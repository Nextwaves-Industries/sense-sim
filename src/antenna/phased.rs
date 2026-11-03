//! What the array and signal-processing pages calculate, and the assistant's tools for them.
//! The physics is `antenna_core::phased`. The pages recompute off the main thread as settings
//! change, so a drag never waits on a pattern; the slow studies are ordinary Studio jobs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use antenna_core::array::Taper;
use antenna_core::farfield::FarField;
use antenna_core::geometry::{Geometry, P3, Shape, Solid};
use antenna_core::phased::aperture::{Element, Layout, Sparse, angle_between, unit_dir};
use antenna_core::phased::beam::{self, Beam, BeamPlan, Delay, DriveLoad, Hardware, MaskRegion, MultiBeam, ShapedBeam};
use antenna_core::phased::chain::{self, Bf, ChainReport, Stage, TxPoint, Waveform};
use antenna_core::phased::doa::{self, Ambiguity, DfArray, DfOptions, DfResult, FromSolved, Ideal, McPoint, McSetup, Method, Response, Scan, Source, Table};
use antenna_core::phased::pattern::{self, ArrayModel, BeamReport, ElementKind};
use antenna_core::phased::radar::{self, RadarOutput, Target};
use antenna_core::phased::solved::{self, ScanRow, SolvedArray};
use antenna_core::phased::{self, ChainDesign, DfDesign, JammerDesign, PhasedDesign, RadarDesign};
use antenna_core::project::Project;
use antenna_core::{C0, C64, db10};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::Studio;
use crate::agent::llm::ToolDef;
use crate::agent::tools::ToolOutput;

/// A value worked out on another thread whenever its inputs change. The last good one stays
/// up until the new one is ready, and a change made meanwhile waits its turn.
pub struct Lazy<K, V> {
    key: Option<K>,
    value: Option<Arc<V>>,
    /// What's being worked out, and the slot its thread leaves the answer in.
    running: Option<(K, Arc<Mutex<Option<V>>>)>,
}

impl<K, V> Default for Lazy<K, V> {
    fn default() -> Self {
        Self { key: None, value: None, running: None }
    }
}

impl<K: PartialEq + Clone, V: Send + 'static> Lazy<K, V> {
    /// The latest value, and whether a newer one is on its way.
    pub fn get(&mut self, key: &K, job: impl FnOnce() -> V + Send + 'static) -> (Option<Arc<V>>, bool) {
        if let Some((k, slot)) = self.running.take() {
            let answer = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(v) = answer {
                self.key = Some(k);
                self.value = Some(Arc::new(v));
            } else if Arc::strong_count(&slot) == 1 {
                // Its thread is gone with no answer: it panicked. Don't wait for ever.
                self.key = Some(k);
            } else {
                self.running = Some((k, slot));
            }
        }
        if self.key.as_ref() != Some(key) && self.running.is_none() {
            let slot = Arc::new(Mutex::new(None));
            let theirs = slot.clone();
            std::thread::spawn(move || {
                let v = job();
                *theirs.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
            });
            self.running = Some((key.clone(), slot));
        }
        (self.value.clone(), self.key.as_ref() != Some(key))
    }

    pub fn latest(&self) -> Option<Arc<V>> {
        self.value.clone()
    }
}

/// The design frequency a page works at.
pub fn design_f0(project: &Project) -> f64 {
    if project.phased.f0 > 0.0 { project.phased.f0 } else { project.f0() }
}

/// An analytic array's beam, read every way the page shows it.
pub struct BeamView {
    /// The array as this view read it, for the beam table and the codebook.
    pub model: Arc<ArrayModel>,
    pub reports: Vec<BeamReport>,
    /// The beam the pattern views show.
    pub shown: usize,
    pub elements: Vec<Element>,
    pub weights: Vec<C64>,
    pub far: FarField,
    pub cuts: [Vec<(f64, f64)>; 2],
    pub cut_phis: [f64; 2],
    /// `[v][u]` when the array faces one way, `[el][az]` when it sees all round. dB against the peak.
    pub map: Vec<Vec<f64>>,
    pub map_is_uv: bool,
    pub band: Vec<beam::BandPoint>,
    pub band_hz: Option<f64>,
    pub load: DriveLoad,
    /// Total radiated power at the elements' full drive, dBm.
    pub total_dbm: f64,
    pub eirp_dbm: f64,
    pub f0: f64,
}

/// Worked out off the main thread.
pub fn beam_view(d: &PhasedDesign, f0: f64, element: Option<&FarField>) -> Result<BeamView, String> {
    if beam::too_big(&d.layout) {
        return Err(format!("{} elements is more than this page works with, 4096. The Array synthesis page estimates larger uniform lattices.", d.layout.count()));
    }
    let elements = d.layout.elements(f0);
    if elements.is_empty() {
        return Err("The layout has no elements.".into());
    }
    if d.plan.beams.is_empty() {
        return Err("Add a beam.".into());
    }
    let model = Arc::new(ArrayModel::new(elements.clone(), d.element, element));
    let flat = d.layout.is_flat();
    let all = beam::all_weights(&model, &d.plan, f0, f0, flat);
    let shown = d.plan.active.min(d.plan.beams.len() - 1);
    let weights = all.get(if d.plan.multibeam == MultiBeam::Digital { shown } else { 0 }).cloned().unwrap_or_default();
    let dirs: Vec<(f64, f64)> = d.plan.beams.iter().map(|b| (b.theta, b.phi)).collect();
    let reports: Vec<BeamReport> = if d.plan.multibeam == MultiBeam::Digital {
        d.plan.beams.iter().zip(&all).map(|(b, w)| pattern::report(&model, w, f0, (b.theta, b.phi), &[])).collect()
    } else {
        dirs.iter().enumerate().map(|(i, b)| { let others: Vec<(f64, f64)> = dirs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| *x).collect(); pattern::report(&model, &weights, f0, *b, &others) }).collect()
    };
    let step = (model.narrowest_beam_deg(f0) / 2.0).clamp(1.0, 3.0);
    let far = model.far(&weights, f0, step);
    let p_rad = far.p_rad();
    let phi = dirs[shown].1;
    let cut_step = (model.narrowest_beam_deg(f0) / 8.0).clamp(0.1, 1.0);
    let cut_phis = [phi, phi + 90.0];
    let cuts = [pattern::plane_cut(&model, &weights, f0, cut_phis[0], cut_step, p_rad), pattern::plane_cut(&model, &weights, f0, cut_phis[1], cut_step, p_rad)];
    let map_is_uv = flat;
    let map = if map_is_uv { pattern::uv_map(&model, &weights, f0, 141) } else { pattern::az_el_map(&model, &weights, f0, 180, 91) };
    let beam_shown = d.plan.beams[shown];
    let band = beam::band_sweep(&model, &d.plan, &beam_shown, f0, 0.4, 161, flat);
    let band_hz = beam::instantaneous_band(&band, f0);
    let load = beam::drive_load(&all);
    let total: f64 = all.iter().map(|w| w.iter().map(C64::norm_sqr).sum::<f64>()).sum();
    let total_dbm = d.tx_dbm + db10(total.max(1e-30));
    let report = &reports[shown];
    Ok(BeamView { model: model.clone(), eirp_dbm: d.tx_dbm + db10(weights.iter().map(C64::norm_sqr).sum::<f64>().max(1e-30)) + report.at_request_dbi, reports, shown, elements, weights, far, cuts, cut_phis, map, map_is_uv, band, band_hz, load, total_dbm, f0 })
}

/// A small plate per element, to see the layout under the pattern.
pub fn marker_model(elements: &[Element], f0: f64) -> Geometry {
    let size = C0 / f0 / 16.0;
    let solids = elements
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (x, y) = (e.x_axis, e.y_axis());
            let corner = |a: f64, b: f64| -> P3 { std::array::from_fn(|k| e.pos[k] + a * size * x[k] + b * size * y[k]) };
            let q = [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)];
            Solid { name: format!("E{}", i + 1), material: "PEC".into(), shape: Shape::Tris { tris: vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]], solid: false, sheet: 0.0 } }
        })
        .collect();
    Geometry { solids, ..Default::default() }
}

/// The hardware limits the analytic path applies, on solved weights.
fn apply_hardware(w: &mut [C64], hw: &Hardware) {
    let mut rng = proof::Rng::new(hw.seed.wrapping_add(11));
    let lsb = hw.phase_bits.map(|b| std::f64::consts::TAU / 2f64.powi(b as i32));
    for (i, v) in w.iter_mut().enumerate() {
        let (a, b) = (rng.gauss(), rng.gauss());
        let mut phase = v.arg();
        if let Some(l) = lsb {
            phase = (phase / l).round() * l;
        }
        let mut amp = v.norm();
        if let Some(step) = hw.atten_step_db.filter(|s| *s > 0.0)
            && amp > 0.0
        {
            amp = 10f64.powf((20.0 * amp.log10() / step).round() * step / 20.0);
        }
        amp *= 10f64.powf(hw.amp_err_db * a / 20.0);
        *v = if hw.failed.contains(&i) { C64::new(0.0, 0.0) } else { C64::from_polar(amp, phase + hw.phase_err_deg.to_radians() * b) };
    }
}

/// A taper on ports that happen to lie on a lattice, read from where they are. On any other
/// arrangement there's no row or column to taper along, so they're fed equally.
fn lattice_taper(a: &SolvedArray, plan: &BeamPlan) -> Vec<f64> {
    let coordinate = |k: usize| -> Vec<usize> {
        let mut values: Vec<f64> = a.pos.iter().map(|p| p[k]).collect();
        values.sort_by(f64::total_cmp);
        values.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
        a.pos.iter().map(|p| values.iter().position(|v| (*v - p[k]).abs() < 1e-9).unwrap_or(0)).collect()
    };
    let (ix, iy) = (coordinate(0), coordinate(1));
    let (nx, ny) = (ix.iter().max().map_or(1, |v| v + 1), iy.iter().max().map_or(1, |v| v + 1));
    if nx * ny != a.ports() {
        return vec![1.0; a.ports()];
    }
    let (tx, ty) = (antenna_core::array::taper_weights(plan.taper_x, nx), antenna_core::array::taper_weights(plan.taper_y, ny));
    ix.iter().zip(&iy).map(|(i, j)| tx[*i] * ty[*j]).collect()
}

/// Weights for a solved array from a plan: the shown beam's when each beam has its own, and their
/// sum when one network makes them all. Time delay and subarrays don't come in: this is one frequency.
pub fn solved_weights(a: &SolvedArray, plan: &BeamPlan, matched: bool) -> Vec<C64> {
    let taper = lattice_taper(a, plan);
    let one = |b: &Beam| -> Vec<C64> {
        let base = if matched { a.matched_weights(b.theta, b.phi, a.main_pol(b.theta, b.phi)) } else { a.textbook_weights(b.theta, b.phi) };
        let tapered: Vec<C64> = base.iter().zip(&taper).map(|(w, t)| *w * *t).collect();
        a.nulled(&tapered, &plan.nulls)
    };
    let mut w: Vec<C64> = match plan.multibeam {
        MultiBeam::Digital => one(&plan.beams.get(plan.active.min(plan.beams.len().saturating_sub(1))).copied().unwrap_or_default()),
        mode => {
            let mut sum = vec![C64::new(0.0, 0.0); a.ports()];
            let total: f64 = plan.beams.iter().map(|b| b.share.max(0.0)).sum::<f64>().max(1e-12);
            for b in &plan.beams {
                for (s, v) in sum.iter_mut().zip(one(b)) {
                    *s += v * (b.share.max(0.0) / total).sqrt();
                }
            }
            let top = sum.iter().map(|v| v.norm()).fold(0.0, f64::max).max(1e-300);
            sum.iter_mut().for_each(|s| *s = if mode == MultiBeam::SharedPhaseOnly && s.norm() > 0.0 { *s / s.norm() } else { *s / top });
            sum
        }
    };
    apply_hardware(&mut w, &plan.hw);
    w
}

/// What a solved element kind reads: the middle port of a solved array, moved to the origin,
/// or a single antenna's own pattern.
pub fn solved_element(array: Option<&SolvedArray>, latest: Option<&antenna_core::project::SimResult>, far_k: usize) -> Option<FarField> {
    match array {
        Some(a) => Some(a.embedded_element(a.centre_port())),
        None => latest.and_then(|r| r.far.first().and_then(|p| p.get(far_k).cloned())),
    }
}

/// How well a switched single receiver reads the first source, over `trials` draws, with the
/// oscillator's offset taken out and left in: `(rms with, rms without)`, degrees.
pub fn switched_errors(df: &DfDesign, f0: f64, sw: &doa::Switched, trials: usize) -> Result<(f64, f64), String> {
    let source = *df.sources.first().ok_or("Add a source.")?;
    let array = phased::df_array_of(&df.layout, f0);
    if array.pos.len() < 2 {
        return Err("A switched array needs at least 2 elements.".into());
    }
    let resp = Ideal::new(array.pos.clone(), f0);
    let table = Table::new(&resp, df.scan, table_step(df).max(1.0));
    let noise = 10f64.powf(-df.snr_db / 10.0);
    let truth = (source.theta, source.phi);
    let rms = |correct: bool| -> f64 {
        let sum: f64 = (0..trials.max(1))
            .map(|t| {
                let mut rng = proof::Rng::new(df.seed.wrapping_add(31 * t as u64 + 1));
                let x = doa::switched_snapshots(&resp, &source, noise, &doa::Switched { correct, ..*sw }, &mut rng);
                doa::estimate(Method::Beamscan, &x, &array, &table, &resp, &DfOptions { count: Some(1), ..Default::default() }).ok().and_then(|r| r.dirs.first().copied()).map_or(180.0, |d| angle_between(truth, d)).powi(2)
            })
            .sum();
        (sum / trials.max(1) as f64).sqrt()
    };
    Ok((rms(true), rms(false)))
}

/// A solved array under the beam the plan asks for, and scanned through its two planes.
pub struct SolvedView {
    pub row: ScanRow,
    pub weights: Vec<C64>,
    pub far: FarField,
    pub cuts: [Vec<(f64, f64)>; 2],
    pub cut_phis: [f64; 2],
    pub scan_a: Vec<ScanRow>,
    pub scan_b: Vec<ScanRow>,
    pub active_db: Vec<f64>,
    pub active_z: Vec<C64>,
    pub blind: Vec<(f64, f64, f64)>,
    /// Radiated over accepted for these weights: 1 for a lossless, consistent solve.
    pub balance: f64,
    pub names: Vec<String>,
    pub f: f64,
    pub centre_port: usize,
    pub centre_gain_dbi: f64,
    pub ports: usize,
    pub pos: Vec<P3>,
}

pub fn solved_view(a: &SolvedArray, plan: &BeamPlan, matched: bool) -> SolvedView {
    let shown = plan.active.min(plan.beams.len().saturating_sub(1));
    let b = plan.beams.get(shown).copied().unwrap_or_default();
    let weights = solved_weights(a, plan, matched);
    let row = a.scan_row(b.theta, b.phi, &weights);
    let far = a.combined(&weights);
    let cut_phis = [b.phi, b.phi + 90.0];
    let cuts = [a.cut(&weights, cut_phis[0], 1.0), a.cut(&weights, cut_phis[1], 1.0)];
    let scan_a = a.scan_plane(cut_phis[0], 70.0, 5.0);
    let scan_b = a.scan_plane(cut_phis[1], 70.0, 5.0);
    let active = a.active_reflection(&weights);
    let radiation = a.radiation_matrix();
    let (radiated, accepted) = a.power_balance(&weights, &radiation);
    let mut blind = solved::blind_spots(&scan_a);
    blind.extend(solved::blind_spots(&scan_b));
    let c = a.centre_port();
    SolvedView {
        row,
        far,
        cuts,
        cut_phis,
        scan_a,
        scan_b,
        active_db: active.iter().map(|g| antenna_core::db20(g.norm())).collect(),
        active_z: a.active_impedance(&weights),
        blind,
        balance: radiated / accepted.max(1e-30),
        names: a.names.clone(),
        f: a.f,
        centre_port: c,
        centre_gain_dbi: db10(a.embedded_gain(c, 0.0, 0.0, None)),
        ports: a.ports(),
        pos: a.pos.clone(),
        weights,
    }
}

/// One direction-finding run on the page's settings.
pub struct DfView {
    pub result: Result<DfResult, String>,
    pub truth: Vec<Source>,
    pub crb: Vec<f64>,
    pub rayleigh: f64,
    pub ambiguity: Ambiguity,
    pub scan: Scan,
    pub channels: usize,
    /// Which method's unavailable and why, for this array.
    pub unavailable: Vec<(Method, &'static str)>,
}

/// What's needed to evaluate an array's response, given a design and maybe a solve.
struct DfSetup {
    array: DfArray,
    truth: Box<dyn Response>,
    belief: Box<dyn Response>,
}

/// Owned forms of the responses, so a setup can move to a thread.
struct OwnedSolved {
    array: Arc<SolvedArray>,
    scale: f64,
}

impl Response for OwnedSolved {
    fn channels(&self) -> usize {
        self.array.ports()
    }
    fn a(&self, theta: f64, phi: f64) -> Vec<C64> {
        FromSolved { array: &self.array, pol: None, scale: self.scale }.a(theta, phi)
    }
}

fn df_setup(df: &DfDesign, f0: f64, solved: Option<&Arc<SolvedArray>>) -> DfSetup {
    match solved.filter(|_| df.use_ports) {
        Some(a) => {
            let array = phased::df_array_of_ports(a.pos.clone(), a.f);
            let scale = FromSolved::new(a, None).scale;
            let truth: Box<dyn Response> = Box::new(OwnedSolved { array: a.clone(), scale });
            let belief: Box<dyn Response> = if df.calibrated { Box::new(OwnedSolved { array: a.clone(), scale }) } else { Box::new(Ideal::new(a.pos.clone(), a.f)) };
            DfSetup { array, truth, belief }
        }
        None => {
            let array = phased::df_array_of(&df.layout, f0);
            let resp = Ideal::new(array.pos.clone(), f0);
            DfSetup { array, truth: Box::new(resp.clone()), belief: Box::new(resp) }
        }
    }
}

fn df_options(df: &DfDesign) -> DfOptions {
    DfOptions { count: df.count, forward_backward: df.forward_backward, smooth_len: df.smooth_len }
}

/// A scan's grid step: a whole sphere at the page's fine step would take far too long.
fn table_step(df: &DfDesign) -> f64 {
    match df.scan {
        Scan::Hemisphere { .. } => df.step.max(2.0),
        _ => df.step.max(0.1),
    }
}

pub fn df_view(df: &DfDesign, f0: f64, solved: Option<&Arc<SolvedArray>>) -> DfView {
    let s = df_setup(df, f0, solved);
    let table = Table::new(s.belief.as_ref(), df.scan, table_step(df));
    let mut rng = proof::Rng::new(df.seed);
    let noise = 10f64.powf(-df.snr_db / 10.0);
    let x = doa::snapshots(s.truth.as_ref(), &df.sources, df.snapshots.max(1), noise, &df.impair, &mut rng);
    let result = doa::estimate(df.method, &x, &s.array, &table, s.belief.as_ref(), &df_options(df)).map_err(|e| e.to_string());
    let crb = if df.sources.is_empty() { Vec::new() } else { doa::crb_deg(s.truth.as_ref(), &df.sources, noise, df.snapshots.max(1), df.scan).unwrap_or_default() };
    DfView {
        result,
        truth: df.sources.clone(),
        crb,
        rayleigh: s.array.rayleigh_deg(),
        ambiguity: doa::ambiguity(&table, &s.array),
        scan: df.scan,
        channels: s.array.pos.len(),
        unavailable: Method::ALL.iter().filter_map(|m| m.unavailable(&s.array).map(|why| (*m, why))).collect(),
    }
}

/// One curve of an accuracy study.
#[derive(Clone, Debug)]
pub struct DfCurve {
    pub method: Method,
    pub points: Vec<McPoint>,
}

/// Studies, chains and radar runs: what the slower jobs leave behind.
#[derive(Clone, Default)]
pub struct PhasedJobs {
    pub study: Option<Vec<DfCurve>>,
    pub chain: Option<Arc<ChainReport>>,
    pub tx: Option<Vec<TxPoint>>,
    pub radar: Option<Arc<RadarOutput>>,
    pub shaped: Option<Arc<ShapedBeam>>,
}

/// The current project's solved array, if its latest run holds one: every port with a far field.
pub fn solved_array(st: &Studio) -> Option<Arc<SolvedArray>> {
    let r = st.latest()?;
    let g = st.project.geometry().ok()?;
    if r.far.len() < 2 || r.far.len() != g.ports.len() {
        return None;
    }
    // Fields from one set of dimensions and phases from another would agree on nothing.
    let vars = st.project.vars().ok()?;
    if r.vars.iter().any(|(k, v)| vars.get(k).is_none_or(|x| (*x - *v).abs() > 1e-9 * v.abs().max(1e-12))) {
        return None;
    }
    let k = st.ui.far_k.min(r.far_f.len().saturating_sub(1));
    let pos = g.ports.iter().map(|p| p.center()).collect();
    let names = g.ports.iter().map(|p| p.name.clone()).collect();
    SolvedArray::from_result(&r, k, pos, names).ok().map(Arc::new)
}

impl Studio {
    pub fn phased_jobs(&self) -> PhasedJobs {
        self.shared.lock().unwrap().phased.clone()
    }

    /// RMSE against SNR by Monte Carlo, for each method being compared.
    pub fn run_df_study(&mut self) {
        let df = self.project.phased.df.clone();
        let f0 = design_f0(&self.project);
        let solved = solved_array(self);
        self.spawn(move |ctx, shared| {
            let s = df_setup(&df, f0, solved.as_ref());
            let table = Table::new(s.belief.as_ref(), df.scan, table_step(&df).max(1.0));
            let mut snrs = Vec::new();
            let mut v = df.snr_from;
            while v <= df.snr_to + 1e-9 && snrs.len() < 64 {
                snrs.push(v);
                v += df.snr_step.max(0.5);
            }
            let methods: Vec<Method> = df.compare.iter().copied().filter(|m| m.unavailable(&s.array).is_none()).collect();
            if methods.is_empty() {
                anyhow::bail!("None of the chosen methods work on this array.");
            }
            let done = AtomicUsize::new(0);
            let total = methods.len() * snrs.len();
            let setup = McSetup { truth: s.truth.as_ref(), belief: s.belief.as_ref(), table: &table, array: &s.array, sources: &df.sources, snapshots: df.snapshots.max(1), options: df_options(&df), impair: df.impair, trials: df.trials.max(1), seed: df.seed };
            let mut curves = Vec::new();
            for m in &methods {
                let mut points = Vec::new();
                for snr in &snrs {
                    ctx.check_cancel()?;
                    let n = done.fetch_add(1, Ordering::SeqCst);
                    ctx.busy(format!("{} at {snr:.0} dB, {} trials", m.label(), df.trials), n as f32 / total as f32);
                    // A method that can't run at all shows as a gap, not as the job's failure.
                    if let Ok(p) = doa::monte_carlo(&setup, *m, *snr) {
                        points.push(p);
                    }
                }
                curves.push(DfCurve { method: *m, points });
            }
            shared.lock().unwrap().phased.study = Some(curves);
            Ok(())
        });
    }

    /// The receive chain, sample by sample, and the transmitter's back-off sweep.
    pub fn run_chain(&mut self) {
        let design = self.project.phased.chain.clone();
        self.spawn(move |ctx, shared| {
            ctx.busy("Running the chain, sample by sample…", -1.0);
            let sim = design.resolved();
            let report = chain::simulate(&sim)?;
            ctx.check_cancel()?;
            ctx.busy("Sweeping the amplifier's back-off…", -1.0);
            let tx = chain::pa_sweep(&sim.waveform, sim.chain.adc.fs_hz, sim.chain.bw_hz, 30.0, 2.0, &[2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0], sim.seed);
            let mut s = shared.lock().unwrap();
            s.phased.chain = Some(Arc::new(report));
            s.phased.tx = Some(tx);
            Ok(())
        });
    }

    /// The radar from raw beat signal to detections.
    pub fn run_radar(&mut self) {
        let design = self.project.phased.radar.clone();
        self.spawn(move |ctx, shared| {
            ctx.busy("Simulating chirps and processing the cube…", -1.0);
            let r = design.resolved();
            let cube = radar::simulate(&r, &design.targets)?;
            ctx.check_cancel()?;
            let out = radar::process(&r, &cube)?;
            shared.lock().unwrap().phased.radar = Some(Arc::new(out));
            Ok(())
        });
    }

    /// A beam of a set shape, by alternating projection.
    pub fn run_shaped(&mut self, phi: f64, mask: Vec<MaskRegion>, phase_only: bool) {
        let d = self.project.phased.clone();
        let f0 = design_f0(&self.project);
        let array = if matches!(d.element, ElementKind::Solved { .. }) { solved_array(self) } else { None };
        let latest = self.latest();
        let far_k = self.ui.far_k;
        let element = solved_element(array.as_deref(), latest.as_deref(), far_k);
        self.spawn(move |ctx, shared| {
            ctx.busy("Fitting weights to the mask…", -1.0);
            let model = ArrayModel::new(d.layout.elements(f0), d.element, element.as_ref());
            let s = beam::synthesize(&model, f0, phi, &mask, phase_only, 400)?;
            shared.lock().unwrap().phased.shaped = Some(Arc::new(s));
            Ok(())
        });
    }
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn def(name: &str, description: &str, schema: Value) -> ToolDef {
    ToolDef { name: name.into(), description: description.into(), schema }
}

pub fn defs() -> Vec<ToolDef> {
    let empty = json!({ "type": "object", "properties": {} });
    vec![
        def(
            "antenna_phased_set",
            "Set up a phased array study of any layout, multi-beam and multi-panel included. layout: {kind: line|planar|triangular|ring|rings|cylinder|panels|nested|coprime|min_redundancy, n, nx, ny, dx, dy (wavelengths), radius, rings:[[n,radius]], faces, tilt_deg, around, rows, dz, n1, n2, m}. element: isotropic|cosine (q, back_db)|sector|solved|embedded, embedded being the middle port of a solved array, moved to the origin, to size a much larger array of that lattice with its coupling. use_ports: read the latest run's ports with their real coupling (use with an array_ template). beams: [{theta, phi, share}]; butler: true fills them with a line's orthogonal Butler beams. multibeam: digital|shared|shared_phase_only. nulls: [[theta,phi]]. taper: uniform|binomial|chebyshev|taylor|cosine|hamming|blackman with sll_db. phase_bits, atten_step_db, delay: none|element|subarray, subarray: [sx,sy], analog_aim: [theta,phi], amp_err_db, phase_err_deg, failed: [indices], cone_deg, f_ghz, tx_dbm, seed.",
            json!({ "type": "object", "properties": { "layout": { "type": "object" }, "element": { "type": "string" }, "q": { "type": "number" }, "back_db": { "type": "number" }, "use_ports": { "type": "boolean" }, "beams": { "type": "array" }, "multibeam": { "type": "string" }, "nulls": { "type": "array" }, "taper": { "type": "string" }, "sll_db": { "type": "number" }, "phase_bits": { "type": "integer" }, "atten_step_db": { "type": "number" }, "delay": { "type": "string" }, "subarray": { "type": "array" }, "analog_aim": { "type": "array" }, "amp_err_db": { "type": "number" }, "phase_err_deg": { "type": "number" }, "failed": { "type": "array" }, "cone_deg": { "type": "number" }, "f_ghz": { "type": "number" }, "tx_dbm": { "type": "number" }, "seed": { "type": "integer" } } }),
        ),
        def("antenna_phased_results", "What the array does now, per beam: realized gain, directivity, EIRP, beam widths, side lobes, grating lobes, pointing error, taper loss, what the amplifiers see, instantaneous bandwidth. With use_ports and a solved run, also active impedance and reflection per port, total active reflection, scan loss in two planes, blind spots and the gain limit in any direction. Evidence label included.", empty.clone()),
        def("antenna_phased_table", "The beam table to load into a controller, as CSV: for each beam and element, amplitude, phase, delay-line setting, what's left for the shifter and its code. With use_ports, the solved array's.", empty.clone()),
        def("antenna_phased_shape", "Fit weights for a shaped beam in the plane at phi: a flat-topped sector, a cosecant fall-off, or a wide null. mask: [{theta_lo, theta_hi, lo_db, hi_db}], dB against the shape's top; leave lo_db out where only a ceiling applies. Background job; antenna_phased_results reads it.", json!({ "type": "object", "properties": { "phi": { "type": "number" }, "mask": { "type": "array" }, "phase_only": { "type": "boolean" } }, "required": ["mask"] })),
        def(
            "antenna_df_set",
            "Set up a direction-finding study. layout as in antenna_phased_set. use_ports with a solved array_ring or other multi-port run reads the real array; calibrated then says whether the estimator's table comes from the solve (true) or assumes textbook phases (false). sources: [{theta, phi, power_db, coherent_with, phase_deg}]. method: music|capon|beamscan|root_music|esprit|coarray_music|monopulse|interferometer|watson_watt|pseudo_doppler. scan: plane (phi)|azimuth (theta)|hemisphere (theta_max). count: number of sources, or leave out for MDL to count them. forward_backward and smooth_len: for coherent sources such as multipath. Channel errors: gain_err_db, phase_err_deg; converter: adc_bits, backoff_db. compare: methods for the accuracy study, with snr_from, snr_to, snr_step, trials.",
            json!({ "type": "object", "properties": { "layout": { "type": "object" }, "use_ports": { "type": "boolean" }, "calibrated": { "type": "boolean" }, "sources": { "type": "array" }, "snr_db": { "type": "number" }, "snapshots": { "type": "integer" }, "method": { "type": "string" }, "scan": { "type": "string" }, "phi": { "type": "number" }, "theta": { "type": "number" }, "theta_max": { "type": "number" }, "step": { "type": "number" }, "count": { "type": "integer" }, "forward_backward": { "type": "boolean" }, "smooth_len": { "type": "integer" }, "gain_err_db": { "type": "number" }, "phase_err_deg": { "type": "number" }, "adc_bits": { "type": "integer" }, "backoff_db": { "type": "number" }, "compare": { "type": "array" }, "snr_from": { "type": "number" }, "snr_to": { "type": "number" }, "snr_step": { "type": "number" }, "trials": { "type": "integer" }, "seed": { "type": "integer" } } }),
        ),
        def("antenna_df_results", "One run of the direction finder on the current settings: what it found against the truth, sources counted by MDL and AIC, covariance eigenvalues, the Cramér-Rao bound, the Rayleigh limit, ambiguity, which methods this array can't use, and the last accuracy study if one has run.", empty.clone()),
        def("antenna_df_fix", "Where several direction finders' bearings cross: stations [{x, y, z, theta, phi, sigma_deg}], metres and degrees. Gives the fix and its 1σ radius, or says the bearings are too near parallel to mean anything.", json!({ "type": "object", "properties": { "stations": { "type": "array" } }, "required": ["stations"] })),
        def("antenna_df_switched", "How well one receiver switched round the layout reads the first source, as a BLE angle-of-arrival locator does, with the oscillator offset taken out from reference samples and left in. slot_us, cfo_khz, reference (samples), rounds.", json!({ "type": "object", "properties": { "slot_us": { "type": "number" }, "cfo_khz": { "type": "number" }, "reference": { "type": "integer" }, "rounds": { "type": "integer" } } })),
        def("antenna_df_study", "Accuracy against SNR by Monte Carlo for each method in compare, with the Cramér-Rao bound alongside and how often all sources were resolved. Background job: antenna_wait, then antenna_df_results.", empty.clone()),
        def(
            "antenna_chain_set",
            "Set up the receive chain behind an array. stages: [{name, gain_db, nf_db, iip3_dbm}]. adc_bits, fs_mhz, full_scale_dbm, jitter_ps, f_in_mhz, bw_mhz, decim, samples. channels on a half-wave line; signal_dbm per element, signal_deg from broadside; waveform: tone|qpsk|qam16|ofdm|chirp|noise, symbol_mhz. beamformer: steered|mvdr|single. Jammer: jammer_dbm, jammer_deg, jammer_waveform; jammer_dbm null removes it. LO: pn_dbc_hz at pn_offset_khz with loop_khz, lo_shared. iq_gain_db, iq_phase_deg. Channel errors: channel_gain_db, channel_phase_deg, calibrate_snr_db (null: uncalibrated). agc_backoff_db (null: fixed gain).",
            json!({ "type": "object", "properties": { "stages": { "type": "array" }, "adc_bits": { "type": "integer" }, "fs_mhz": { "type": "number" }, "full_scale_dbm": { "type": "number" }, "jitter_ps": { "type": "number" }, "f_in_mhz": { "type": "number" }, "bw_mhz": { "type": "number" }, "decim": { "type": "integer" }, "samples": { "type": "integer" }, "channels": { "type": "integer" }, "signal_dbm": { "type": "number" }, "signal_deg": { "type": "number" }, "waveform": { "type": "string" }, "symbol_mhz": { "type": "number" }, "beamformer": { "type": "string" }, "jammer_dbm": { "type": ["number", "null"] }, "jammer_deg": { "type": "number" }, "jammer_waveform": { "type": "string" }, "pn_dbc_hz": { "type": ["number", "null"] }, "pn_offset_khz": { "type": "number" }, "loop_khz": { "type": "number" }, "lo_shared": { "type": "boolean" }, "iq_gain_db": { "type": "number" }, "iq_phase_deg": { "type": "number" }, "channel_gain_db": { "type": "number" }, "channel_phase_deg": { "type": "number" }, "calibrate_snr_db": { "type": ["number", "null"] }, "agc_backoff_db": { "type": ["number", "null"] }, "seed": { "type": "integer" } } }),
        ),
        def("antenna_chain_budget", "The chain's cascade by formula, at once: per-stage gain, noise figure and intercept, then noise floor, sensitivity, clip level, dynamic range, spur-free range, converter limits, and whether the converter or the front end sets the noise.", empty.clone()),
        def("antenna_chain_run", "Run the chain sample by sample, with compression, LO phase noise, I/Q imbalance, the converter, filter and digital beamformer, plus a power amplifier back-off and predistortion sweep. Background job: antenna_wait, then antenna_chain_results.", empty.clone()),
        def("antenna_chain_results", "The last sample-by-sample run: signal over noise at each point in the chain, array gain, EVM, clipping, and the amplifier sweep.", empty.clone()),
        def(
            "antenna_radar_set",
            "Set up an FMCW MIMO radar: f_ghz, bandwidth_mhz, chirp_us, repeat_us, samples, loops, n_tx, n_rx (laid out as one unbroken virtual half-wave line), tx_dbm, tx_gain_dbi, rx_gain_dbi, nf_db, pfa. targets: [{range_m, speed_mps, theta, phi, rcs_dbsm}], positive speed receding; theta and phi in the array's frame, broadside at theta 0, phi 0 or 180 on the line's plane.",
            json!({ "type": "object", "properties": { "f_ghz": { "type": "number" }, "bandwidth_mhz": { "type": "number" }, "chirp_us": { "type": "number" }, "repeat_us": { "type": "number" }, "samples": { "type": "integer" }, "loops": { "type": "integer" }, "n_tx": { "type": "integer" }, "n_rx": { "type": "integer" }, "tx_dbm": { "type": "number" }, "tx_gain_dbi": { "type": "number" }, "rx_gain_dbi": { "type": "number" }, "nf_db": { "type": "number" }, "pfa": { "type": "number" }, "seed": { "type": "integer" }, "targets": { "type": "array" } } }),
        ),
        def("antenna_radar_run", "Simulate the chirps and process them: range and Doppler FFTs, CFAR, peak fitting, the time-division correction and angle from the virtual array. Background job: antenna_wait, then antenna_radar_results.", empty.clone()),
        def("antenna_radar_results", "What the waveform can tell (range and speed resolution and limits, angle resolution, virtual elements), and the last run's detections against the targets asked for, with the SNR the radar equation predicts for each.", empty),
    ]
}

fn text(v: Value) -> ToolOutput {
    ToolOutput::text(v.to_string())
}

/// A layout as the tools write it.
pub fn layout_from(v: &Value) -> Result<Layout, String> {
    let n = |key: &str, default: usize| v[key].as_u64().map_or(default, |x| x as usize);
    let x = |key: &str, default: f64| v[key].as_f64().unwrap_or(default);
    let spacing = x("spacing", x("dx", 0.5));
    Ok(match v["kind"].as_str().unwrap_or("") {
        "line" => Layout::Line { n: n("n", 8), spacing },
        "planar" | "rectangular" => Layout::Planar { nx: n("nx", 8), ny: n("ny", 8), dx: x("dx", 0.5), dy: x("dy", 0.5), triangular: false },
        "triangular" => Layout::Planar { nx: n("nx", 8), ny: n("ny", 8), dx: x("dx", 0.5), dy: x("dy", 0.433), triangular: true },
        "ring" => Layout::Ring { n: n("n", 8), radius: x("radius", 0.5), radial: v["radial"].as_bool().unwrap_or(false) },
        "rings" => {
            let rings: Vec<(usize, f64)> = v["rings"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|r| Some((r[0].as_u64()? as usize, r[1].as_f64()?))).collect());
            if rings.is_empty() {
                return Err("rings: [[elements, radius in wavelengths], ...]".into());
            }
            Layout::Rings { rings, radial: v["radial"].as_bool().unwrap_or(false) }
        }
        "cylinder" => Layout::Cylinder { around: n("around", 16), rows: n("rows", 4), radius: x("radius", 1.5), dz: x("dz", 0.5) },
        "panels" => Layout::Panels { faces: n("faces", 4), nx: n("nx", 8), ny: n("ny", 8), dx: x("dx", 0.5), dy: x("dy", 0.5), radius: x("radius", 2.0), tilt_deg: x("tilt_deg", 0.0) },
        "nested" => Layout::SparseLine { kind: Sparse::Nested { n1: n("n1", 3), n2: n("n2", 3) }, spacing },
        "coprime" => Layout::SparseLine { kind: Sparse::Coprime { m: n("m", 3), n: n("n", 5) }, spacing },
        "min_redundancy" => Layout::SparseLine { kind: Sparse::MinimumRedundancy { n: n("n", 5) }, spacing },
        other => return Err(format!("no layout '{other}'; use line, planar, triangular, ring, rings, cylinder, panels, nested, coprime or min_redundancy")),
    })
}

fn taper_from(name: &str, sll: f64) -> Option<Taper> {
    Some(match name {
        "uniform" => Taper::Uniform,
        "binomial" => Taper::Binomial,
        "chebyshev" => Taper::DolphChebyshev { sll_db: sll },
        "taylor" => Taper::Taylor { sll_db: sll, nbar: 5 },
        "cosine" => Taper::Cosine { power: 1.0 },
        "hamming" => Taper::Hamming,
        "blackman" => Taper::Blackman,
        _ => return None,
    })
}

fn method_from(name: &str) -> Option<Method> {
    Some(match name {
        "music" => Method::Music,
        "capon" | "mvdr" => Method::Capon,
        "beamscan" | "correlative" => Method::Beamscan,
        "root_music" => Method::RootMusic,
        "esprit" => Method::Esprit,
        "coarray_music" => Method::CoarrayMusic,
        "monopulse" => Method::Monopulse,
        "interferometer" => Method::Interferometer,
        "watson_watt" | "adcock" => Method::WatsonWatt,
        "pseudo_doppler" => Method::PseudoDoppler,
        _ => return None,
    })
}

fn waveform_from(name: &str, symbol_hz: f64, bw_hz: f64) -> Option<Waveform> {
    Some(match name {
        "tone" => Waveform::Tone { offset_hz: 0.13 * bw_hz },
        "qpsk" => Waveform::Qpsk { symbol_rate: symbol_hz, rolloff: 0.35 },
        "qam16" => Waveform::Qam16 { symbol_rate: symbol_hz, rolloff: 0.35 },
        "ofdm" => Waveform::Ofdm { subcarriers: 256, bw_hz },
        "chirp" => Waveform::Chirp { bw_hz, period_s: 100e-6 },
        "noise" => Waveform::Noise { bw_hz },
        _ => return None,
    })
}

/// A line's Butler beams, as directions: `n` orthogonal beams, side by side.
pub fn butler_beams(n: usize, spacing: f64) -> Vec<Beam> {
    beam::butler_directions(n, spacing).into_iter().filter(|u| u.abs() <= 1.0).map(|u| Beam { theta: u.abs().asin().to_degrees(), phi: if u < 0.0 { 180.0 } else { 0.0 }, share: 1.0 }).collect()
}

fn apply_phased(d: &mut PhasedDesign, i: &Value) -> Result<(), String> {
    if !i["layout"].is_null() {
        d.layout = layout_from(&i["layout"])?;
    }
    if let Some(e) = i["element"].as_str() {
        d.element = match e {
            "isotropic" => ElementKind::Isotropic,
            "cosine" => ElementKind::Cosine { q: i["q"].as_f64().unwrap_or(1.5), back_db: i["back_db"].as_f64().unwrap_or(-20.0) },
            "sector" => ElementKind::Sector3gpp,
            "solved" => ElementKind::Solved { embedded: false },
            "embedded" => ElementKind::Solved { embedded: true },
            other => return Err(format!("no element '{other}'")),
        };
    }
    if let Some(v) = i["use_ports"].as_bool() {
        d.use_ports = v;
    }
    if let Some(b) = i["beams"].as_array() {
        let beams: Vec<Beam> = b.iter().map(|x| Beam { theta: x["theta"].as_f64().unwrap_or(0.0), phi: x["phi"].as_f64().unwrap_or(0.0), share: x["share"].as_f64().unwrap_or(1.0) }).collect();
        if beams.is_empty() {
            return Err("give at least one beam".into());
        }
        d.plan.beams = beams;
        d.plan.active = 0;
    }
    if i["butler"].as_bool().unwrap_or(false) {
        let Layout::Line { n, spacing } = d.layout else { return Err("butler needs a line layout".into()) };
        if !n.is_power_of_two() {
            return Err(format!("a Butler matrix needs a power of 2 elements, not {n}"));
        }
        d.plan.beams = butler_beams(n, spacing);
        d.plan.active = 0;
    }
    if let Some(m) = i["multibeam"].as_str() {
        d.plan.multibeam = match m {
            "digital" => MultiBeam::Digital,
            "shared" => MultiBeam::Shared,
            "shared_phase_only" => MultiBeam::SharedPhaseOnly,
            other => return Err(format!("no multibeam mode '{other}'")),
        };
    }
    if let Some(n) = i["nulls"].as_array() {
        d.plan.nulls = n.iter().filter_map(|x| Some((x[0].as_f64()?, x[1].as_f64()?))).collect();
    }
    if let Some(t) = i["taper"].as_str() {
        let t = taper_from(t, i["sll_db"].as_f64().unwrap_or(-30.0)).ok_or_else(|| format!("no taper '{t}'"))?;
        d.plan.taper_x = t;
        d.plan.taper_y = t;
    }
    let hw = &mut d.plan.hw;
    if !i["phase_bits"].is_null() {
        hw.phase_bits = i["phase_bits"].as_u64().map(|b| b.clamp(1, 16) as u8);
    }
    if !i["atten_step_db"].is_null() {
        hw.atten_step_db = i["atten_step_db"].as_f64().filter(|s| *s > 0.0);
    }
    if let Some(s) = i["delay"].as_str() {
        hw.delay = match s {
            "none" => Delay::PhaseOnly,
            "element" => Delay::Element,
            "subarray" => Delay::Subarray,
            other => return Err(format!("no delay '{other}'")),
        };
    }
    if let Some(a) = i["subarray"].as_array() {
        hw.subarray = (a.len() == 2).then(|| (a[0].as_u64().unwrap_or(1) as usize, a[1].as_u64().unwrap_or(1) as usize));
    }
    if hw.delay == Delay::Subarray && hw.subarray.is_none() {
        return Err("delay: subarray needs subarray: [x, y], the elements per subarray".into());
    }
    if let Some(a) = i["analog_aim"].as_array() {
        hw.analog_aim = (a.len() == 2).then(|| (a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)));
    }
    let num = |key: &str, slot: &mut f64| {
        if let Some(v) = i[key].as_f64() {
            *slot = v;
        }
    };
    num("amp_err_db", &mut hw.amp_err_db);
    num("phase_err_deg", &mut hw.phase_err_deg);
    if let Some(f) = i["failed"].as_array() {
        hw.failed = f.iter().filter_map(|x| x.as_u64().map(|v| v as usize)).collect();
    }
    if let Some(s) = i["seed"].as_u64() {
        hw.seed = s;
    }
    num("cone_deg", &mut d.plan.cone_deg);
    num("tx_dbm", &mut d.tx_dbm);
    if let Some(v) = i["f_ghz"].as_f64() {
        d.f0 = v * 1e9;
    }
    Ok(())
}

fn apply_df(df: &mut DfDesign, i: &Value) -> Result<(), String> {
    if !i["layout"].is_null() {
        df.layout = layout_from(&i["layout"])?;
    }
    if let Some(v) = i["use_ports"].as_bool() {
        df.use_ports = v;
    }
    if let Some(v) = i["calibrated"].as_bool() {
        df.calibrated = v;
    }
    if let Some(a) = i["sources"].as_array() {
        df.sources = a.iter().map(|s| Source { theta: s["theta"].as_f64().unwrap_or(0.0), phi: s["phi"].as_f64().unwrap_or(0.0), power_db: s["power_db"].as_f64().unwrap_or(0.0), coherent_with: s["coherent_with"].as_u64().map(|v| v as usize), phase_deg: s["phase_deg"].as_f64().unwrap_or(0.0) }).collect();
    }
    if let Some(m) = i["method"].as_str() {
        df.method = method_from(m).ok_or_else(|| format!("no method '{m}'"))?;
    }
    if let Some(s) = i["scan"].as_str() {
        df.scan = match s {
            "plane" => Scan::Plane { phi: i["phi"].as_f64().unwrap_or(0.0) },
            "azimuth" => Scan::Azimuth { theta: i["theta"].as_f64().unwrap_or(90.0) },
            "hemisphere" => Scan::Hemisphere { theta_max: i["theta_max"].as_f64().unwrap_or(80.0) },
            other => return Err(format!("no scan '{other}'")),
        };
    }
    if i.get("count").is_some() {
        df.count = i["count"].as_u64().map(|c| c as usize);
    }
    if let Some(v) = i["forward_backward"].as_bool() {
        df.forward_backward = v;
    }
    if let Some(v) = i["smooth_len"].as_u64() {
        df.smooth_len = v as usize;
    }
    if let Some(v) = i["snapshots"].as_u64() {
        df.snapshots = (v as usize).clamp(1, 100_000);
    }
    if let Some(v) = i["trials"].as_u64() {
        df.trials = (v as usize).clamp(1, 5000);
    }
    if let Some(v) = i["seed"].as_u64() {
        df.seed = v;
    }
    if !i["adc_bits"].is_null() {
        df.impair.adc_bits = i["adc_bits"].as_u64().map(|b| b.clamp(1, 16) as u8);
    }
    let num = |key: &str, slot: &mut f64| {
        if let Some(v) = i[key].as_f64() {
            *slot = v;
        }
    };
    num("snr_db", &mut df.snr_db);
    num("step", &mut df.step);
    num("gain_err_db", &mut df.impair.gain_db);
    num("phase_err_deg", &mut df.impair.phase_deg);
    num("backoff_db", &mut df.impair.backoff_db);
    num("snr_from", &mut df.snr_from);
    num("snr_to", &mut df.snr_to);
    num("snr_step", &mut df.snr_step);
    if let Some(a) = i["compare"].as_array() {
        df.compare = a.iter().filter_map(|m| m.as_str().and_then(method_from)).collect();
    }
    Ok(())
}

fn apply_chain(d: &mut ChainDesign, i: &Value) -> Result<(), String> {
    let sim = &mut d.sim;
    if let Some(a) = i["stages"].as_array() {
        sim.chain.stages = a.iter().map(|s| Stage { name: s["name"].as_str().unwrap_or("Stage").to_string(), gain_db: s["gain_db"].as_f64().unwrap_or(0.0), nf_db: s["nf_db"].as_f64().unwrap_or(0.0), iip3_dbm: s["iip3_dbm"].as_f64() }).collect();
    }
    if let Some(v) = i["adc_bits"].as_u64() {
        sim.chain.adc.bits = v.clamp(1, 20) as u8;
    }
    if let Some(v) = i["fs_mhz"].as_f64() {
        sim.chain.adc.fs_hz = v * 1e6;
    }
    if let Some(v) = i["full_scale_dbm"].as_f64() {
        sim.chain.adc.full_scale_dbm = v;
    }
    if let Some(v) = i["jitter_ps"].as_f64() {
        sim.chain.adc.jitter_s = v * 1e-12;
    }
    if let Some(v) = i["f_in_mhz"].as_f64() {
        sim.chain.adc.f_in_hz = v * 1e6;
    }
    if let Some(v) = i["bw_mhz"].as_f64() {
        sim.chain.bw_hz = v * 1e6;
    }
    if let Some(v) = i["decim"].as_u64() {
        sim.decim = v.max(1) as usize;
    }
    if let Some(v) = i["samples"].as_u64() {
        sim.samples = (v as usize).clamp(1024, 1 << 20);
    }
    if let Some(v) = i["channels"].as_u64() {
        d.channels = (v as usize).clamp(1, 256);
    }
    if let Some(v) = i["signal_dbm"].as_f64() {
        sim.signal_dbm = v;
    }
    if let Some(v) = i["signal_deg"].as_f64() {
        d.signal_deg = v;
    }
    let symbol = i["symbol_mhz"].as_f64().map_or(sim.chain.bw_hz / 2.0, |v| v * 1e6);
    if let Some(w) = i["waveform"].as_str() {
        sim.waveform = waveform_from(w, symbol, sim.chain.bw_hz).ok_or_else(|| format!("no waveform '{w}'"))?;
    }
    if let Some(b) = i["beamformer"].as_str() {
        sim.bf = match b {
            "steered" => Bf::Steered,
            "mvdr" | "capon" => Bf::Mvdr,
            "single" => Bf::Single,
            other => return Err(format!("no beamformer '{other}'")),
        };
    }
    if i.get("jammer_dbm").is_some() {
        d.jammer = match i["jammer_dbm"].as_f64() {
            Some(dbm) => {
                let bw = sim.chain.bw_hz;
                let name = i["jammer_waveform"].as_str().unwrap_or("tone");
                Some(JammerDesign { dbm, angle_deg: i["jammer_deg"].as_f64().unwrap_or(40.0), waveform: waveform_from(name, symbol, bw).ok_or_else(|| format!("no waveform '{name}'"))? })
            }
            None => None,
        };
    }
    if i.get("pn_dbc_hz").is_some() {
        sim.lo.pn_dbc_hz = i["pn_dbc_hz"].as_f64();
    }
    if let Some(v) = i["pn_offset_khz"].as_f64() {
        sim.lo.pn_offset_hz = v * 1e3;
    }
    if let Some(v) = i["loop_khz"].as_f64() {
        sim.lo.loop_hz = v * 1e3;
    }
    if let Some(v) = i["lo_shared"].as_bool() {
        sim.lo.shared = v;
    }
    if let Some(v) = i["iq_gain_db"].as_f64() {
        sim.iq_gain_db = v;
    }
    if let Some(v) = i["iq_phase_deg"].as_f64() {
        sim.iq_phase_deg = v;
    }
    if let Some(v) = i["channel_gain_db"].as_f64() {
        sim.channel_gain_db = v;
    }
    if let Some(v) = i["channel_phase_deg"].as_f64() {
        sim.channel_phase_deg = v;
    }
    if i.get("calibrate_snr_db").is_some() {
        sim.calibrate_snr_db = i["calibrate_snr_db"].as_f64();
    }
    if i.get("agc_backoff_db").is_some() {
        sim.agc_backoff_db = i["agc_backoff_db"].as_f64();
    }
    if let Some(v) = i["seed"].as_u64() {
        sim.seed = v;
    }
    Ok(())
}

fn apply_radar(d: &mut RadarDesign, i: &Value) {
    let r = &mut d.radar;
    let set = |key: &str, slot: &mut f64, unit: f64| {
        if let Some(v) = i[key].as_f64() {
            *slot = v * unit;
        }
    };
    set("f_ghz", &mut r.f0, 1e9);
    set("bandwidth_mhz", &mut r.bandwidth, 1e6);
    set("chirp_us", &mut r.chirp_s, 1e-6);
    set("repeat_us", &mut r.repeat_s, 1e-6);
    set("tx_dbm", &mut r.tx_dbm, 1.0);
    set("tx_gain_dbi", &mut r.tx_gain_dbi, 1.0);
    set("rx_gain_dbi", &mut r.rx_gain_dbi, 1.0);
    set("nf_db", &mut r.nf_db, 1.0);
    set("pfa", &mut r.pfa, 1.0);
    if let Some(v) = i["samples"].as_u64() {
        r.samples = (v as usize).clamp(16, 4096);
    }
    if let Some(v) = i["loops"].as_u64() {
        r.loops = (v as usize).clamp(4, 1024);
    }
    if let Some(v) = i["seed"].as_u64() {
        r.seed = v;
    }
    if let Some(v) = i["n_tx"].as_u64() {
        d.n_tx = (v as usize).clamp(1, 16);
    }
    if let Some(v) = i["n_rx"].as_u64() {
        d.n_rx = (v as usize).clamp(1, 32);
    }
    if let Some(a) = i["targets"].as_array() {
        d.targets = a.iter().map(|t| Target { range_m: t["range_m"].as_f64().unwrap_or(30.0), speed_mps: t["speed_mps"].as_f64().unwrap_or(0.0), theta: t["theta"].as_f64().unwrap_or(0.0), phi: t["phi"].as_f64().unwrap_or(0.0), rcs_dbsm: t["rcs_dbsm"].as_f64().unwrap_or(10.0) }).collect();
    }
}

fn report_json(r: &BeamReport) -> Value {
    json!({
        "theta": r.theta,
        "phi": r.phi,
        "peak_theta": r3(r.peak_theta),
        "peak_phi": r3(r.peak_phi),
        "pointing_error_deg": r3(r.pointing_error_deg),
        "directivity_dbi": r3(r.directivity_dbi),
        "gain_dbi": r3(r.gain_dbi),
        "realized_dbi": r3(r.realized_dbi),
        "realized_at_request_dbi": r3(r.at_request_dbi),
        "beam_width_deg": [r3(r.hpbw_a), r3(r.hpbw_b)],
        "side_lobes_db": r.sll_db.map(r3),
        "grating_lobes": r.grating.iter().map(|g| json!({ "theta": r3(g.0), "phi": r3(g.1), "db": r3(g.2) })).collect::<Vec<_>>(),
        "taper_loss_db": r3(r.taper_db),
        "elements_driven": r.active,
        "grid_coarser_than_beam_asks": r.estimated,
    })
}

fn scan_json(rows: &[ScanRow]) -> Value {
    json!(rows.iter().map(|r| json!({ "theta": r.theta, "realized_dbi": r3(r.realized_dbi), "limit_dbi": r3(r.limit_dbi), "tarc_db": r3(r.tarc_db), "worst_active_db": r3(r.worst_active_db), "pointing_error_deg": r3(r.pointing_error_deg) })).collect::<Vec<_>>())
}

fn phased_results(world: &World) -> ToolOutput {
    let st = world.resource::<Studio>();
    let d = &st.project.phased;
    let f0 = design_f0(&st.project);
    let latest = st.latest();
    let array = if matches!(d.element, ElementKind::Solved { .. }) { solved_array(st) } else { None };
    let element = if matches!(d.element, ElementKind::Solved { .. }) { solved_element(array.as_deref(), latest.as_deref(), st.ui.far_k) } else { None };
    let shaped = st.phased_jobs().shaped.map(|s| json!({ "worst_miss_db": r3(s.worst_db), "iterations": s.iterations, "taper_loss_db": r3(s.taper_db) }));
    if d.use_ports {
        let Some(a) = solved_array(st) else {
            return ToolOutput::err("use_ports asks for a solved array: open an array_ template, antenna_run, then look again. The latest run has no far fields on 2 or more ports.");
        };
        let v = solved_view(&a, &d.plan, false);
        return text(json!({
            "evidence": proof::Evidence::Simulated.label(),
            "f_ghz": r3(v.f / 1e9),
            "ports": v.ports,
            "beam": { "theta": v.row.theta, "phi": v.row.phi, "realized_dbi": r3(v.row.realized_dbi), "limit_dbi": r3(v.row.limit_dbi), "peak_theta": r3(v.row.peak_theta), "peak_phi": r3(v.row.peak_phi), "pointing_error_deg": r3(v.row.pointing_error_deg) },
            "tarc_db": r3(v.row.tarc_db),
            "mismatch_loss_db": r3(v.row.mismatch_db),
            "active_reflection_db": v.names.iter().zip(&v.active_db).map(|(n, x)| json!({ "port": n, "db": r3(*x) })).collect::<Vec<_>>(),
            "active_impedance_ohm": v.active_z.iter().map(|z| json!([r3(z.re), r3(z.im)])).collect::<Vec<_>>(),
            "scan_plane_a": { "phi": v.cut_phis[0], "rows": scan_json(&v.scan_a) },
            "scan_plane_b": { "phi": v.cut_phis[1], "rows": scan_json(&v.scan_b) },
            "blind_spots": v.blind.iter().map(|b| json!({ "theta": b.0, "gain_drop_db": r3(b.1), "tarc_db": r3(b.2) })).collect::<Vec<_>>(),
            "centre_element_gain_dbi": r3(v.centre_gain_dbi),
            "radiated_over_accepted": r3(v.balance),
            "note": "Coupling, feed mismatch and the model's own surroundings are in these. A feed network's loss and phase error aren't, unless set here as hardware errors.",
            "shaped_beam": shaped,
        }));
    }
    match beam_view(d, f0, element.as_ref()) {
        Ok(v) => text(json!({
            "evidence": proof::Evidence::Estimate.label(),
            "layout": d.layout.describe(),
            "element": d.element.label(),
            "elements": v.elements.len(),
            "f_ghz": r3(f0 / 1e9),
            "beams": v.reports.iter().map(report_json).collect::<Vec<_>>(),
            "eirp_dbm": r3(v.eirp_dbm),
            "total_power_dbm": r3(v.total_dbm),
            "amplifier_load": { "power_use": r3(v.load.power_use), "peak_voltage_when_beams_align": r3(v.load.peak_voltage) },
            "instantaneous_band_mhz": v.band_hz.map(|b| r3(b / 1e6)),
            "shaped_beam": shaped,
            "note": if d.element == (ElementKind::Solved { embedded: true }) { "The middle port's embedded pattern times the array factor: its coupling is in the element, as it would be for one deep inside a large array, so gain follows directly from the power into the ports. The edges of a finite array differ." } else { "Array factor on the element chosen, power conserved. Coupling isn't in this: use_ports reads it from a solved array, or use element: embedded." },
        })),
        Err(e) => ToolOutput::err(e),
    }
}

fn df_results(world: &World) -> ToolOutput {
    let st = world.resource::<Studio>();
    let df = &st.project.phased.df;
    let f0 = design_f0(&st.project);
    let solved = solved_array(st);
    if df.use_ports && solved.is_none() {
        return ToolOutput::err("use_ports asks for a solved array: solve a multi-port model first (antenna_new with array_ring, antenna_run).");
    }
    let v = df_view(df, f0, solved.as_ref());
    let truth: Vec<(f64, f64)> = v.truth.iter().map(|s| (s.theta, s.phi)).collect();
    let found = match &v.result {
        Ok(r) => {
            let errors = doa::pair_up(&truth, &r.dirs);
            json!({ "method": r.method.label(), "found": r.dirs.iter().map(|d| json!({ "theta": r3(d.0), "phi": r3(d.1) })).collect::<Vec<_>>(), "error_deg_per_source": errors.iter().map(|e| e.map(r3)).collect::<Vec<_>>(), "notes": r.notes, "sources_used": r.count, "mdl_counts": r.counted.mdl, "aic_counts": r.counted.aic, "eigenvalues_db": r.eigen_db.iter().map(|x| r3(*x)).collect::<Vec<_>>() })
        }
        Err(e) => json!({ "error": e }),
    };
    let study = st.phased_jobs().study.map(|curves| {
        json!(curves.iter().map(|c| json!({ "method": c.method.label(), "points": c.points.iter().map(|p| json!({ "snr_db": p.snr_db, "rmse_deg": if p.rmse_deg.is_nan() { Value::Null } else { json!(r3(p.rmse_deg)) }, "bound_deg": r3(p.crb_deg), "resolved": r3(p.resolved) })).collect::<Vec<_>>() })).collect::<Vec<_>>())
    });
    text(json!({
        "evidence": if df.use_ports { proof::Evidence::Simulated.label() } else { proof::Evidence::Estimate.label() },
        "channels": v.channels,
        "this_run": found,
        "cramer_rao_bound_deg": v.crb.iter().map(|x| r3(*x)).collect::<Vec<_>>(),
        "rayleigh_limit_deg": r3(v.rayleigh),
        "ambiguity": { "level": r3(v.ambiguity.level), "between": [v.ambiguity.a, v.ambiguity.b], "means": if v.ambiguity.level > 0.9 { "two directions look the same to this array" } else { "no direction looks like another" } },
        "methods_this_array_cant_use": v.unavailable.iter().map(|(m, why)| json!({ "method": m.label(), "why": why })).collect::<Vec<_>>(),
        "study": study,
        "assumes": "Narrowband across the array, far-field sources. With a solved array, one polarisation at a time.",
    }))
}

fn budget_json(b: &chain::Budget) -> Value {
    json!({
        "stages": b.rows.iter().map(|r| json!({ "name": r.name, "gain_db": r3(r.gain_db), "nf_db": r3(r.nf_db), "iip3_dbm": r.iip3_dbm.map(r3), "noise_out_dbm": r3(r.noise_dbm) })).collect::<Vec<_>>(),
        "nf_analog_db": r3(b.nf_analog_db),
        "nf_with_converter_db": r3(b.nf_system_db),
        "gain_db": r3(b.gain_db),
        "iip3_dbm": b.iip3_dbm.map(r3),
        "input_p1db_dbm": b.p1db_input_dbm.map(r3),
        "noise_floor_dbm": r3(b.noise_floor_dbm),
        "clip_input_dbm": r3(b.clip_input_dbm),
        "dynamic_range_db": r3(b.dynamic_range_db),
        "spur_free_range_db": b.sfdr_db.map(r3),
        "converter_sqnr_db": r3(b.sqnr_db),
        "filter_gain_db": r3(b.processing_gain_db),
        "jitter_ceiling_db": if b.jitter_snr_db.is_finite() { json!(r3(b.jitter_snr_db)) } else { Value::Null },
        "thermal_over_quantisation_db": r3(b.thermal_over_quant_db),
    })
}

/// Handles this module's tools; `None` for anything else.
pub fn tool(world: &mut World, name: &str, i: &Value) -> Option<ToolOutput> {
    let page = |st: &mut Studio, p: super::ui::Page| st.ui.page = p;
    Some(match name {
        "antenna_phased_set" => {
            let mut st = world.resource_mut::<Studio>();
            let mut d = st.project.phased.clone();
            if let Err(e) = apply_phased(&mut d, i) {
                return Some(ToolOutput::err(e));
            }
            st.project.phased = d;
            page(&mut st, super::ui::Page::Phased);
            return Some(phased_results(world));
        }
        "antenna_phased_results" => phased_results(world),
        "antenna_phased_table" => {
            let st = world.resource::<Studio>();
            let d = &st.project.phased;
            let f0 = design_f0(&st.project);
            let solved_kind = matches!(d.element, ElementKind::Solved { .. });
            let array = if d.use_ports || solved_kind { solved_array(st) } else { None };
            let csv = match array.as_ref().filter(|_| d.use_ports) {
                Some(a) => {
                    let b = d.plan.beams.get(d.plan.active).copied().unwrap_or_default();
                    beam::solved_csv(&a.pos, (b.theta, b.phi), &solved_weights(a, &d.plan, false), a.f, d.plan.hw.phase_bits)
                }
                None => {
                    let latest = st.latest();
                    let element = if solved_kind { solved_element(array.as_deref(), latest.as_deref(), st.ui.far_k) } else { None };
                    let model = ArrayModel::new(d.layout.elements(f0), d.element, element.as_ref());
                    beam::weights_csv(&model, &d.plan, f0, d.layout.is_flat())
                }
            };
            ToolOutput::text(if csv.len() > 60_000 { format!("{}\n… cut at 60 kB: {} rows in all. Fewer beams or the page's Save will give the rest.", &csv[..csv[..60_000].rfind('\n').unwrap_or(0)], csv.lines().count()) } else { csv })
        }
        "antenna_phased_shape" => {
            let mask: Vec<MaskRegion> = i["mask"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|m| Some(MaskRegion { theta_lo: m["theta_lo"].as_f64()?, theta_hi: m["theta_hi"].as_f64()?, lo_db: m["lo_db"].as_f64(), hi_db: m["hi_db"].as_f64().unwrap_or(0.0) })).collect());
            if mask.is_empty() {
                return Some(ToolOutput::err("give mask: [{theta_lo, theta_hi, lo_db, hi_db}]"));
            }
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            st.run_shaped(i["phi"].as_f64().unwrap_or(0.0), mask, i["phase_only"].as_bool().unwrap_or(false));
            text(json!({ "started": true, "next": "antenna_wait, then antenna_phased_results" }))
        }
        "antenna_df_set" => {
            let mut st = world.resource_mut::<Studio>();
            let mut df = st.project.phased.df.clone();
            if let Err(e) = apply_df(&mut df, i) {
                return Some(ToolOutput::err(e));
            }
            st.project.phased.df = df;
            page(&mut st, super::ui::Page::DirectionFinding);
            return Some(df_results(world));
        }
        "antenna_df_results" => df_results(world),
        "antenna_df_fix" => {
            let stations: Vec<(P3, (f64, f64), f64)> = i["stations"].as_array().map_or(Vec::new(), |a| a.iter().map(|s| ([s["x"].as_f64().unwrap_or(0.0), s["y"].as_f64().unwrap_or(0.0), s["z"].as_f64().unwrap_or(0.0)], (s["theta"].as_f64().unwrap_or(90.0), s["phi"].as_f64().unwrap_or(0.0)), s["sigma_deg"].as_f64().unwrap_or(1.0))).collect());
            match doa::intersect_bearings(&stations) {
                Ok((at, radius)) => text(json!({ "fix_m": [r3(at[0]), r3(at[1]), r3(at[2])], "radius_1sigma_m": r3(radius), "evidence": proof::Evidence::Estimate.label(), "assumes": "Independent errors of the size given, and no bias from a station's own site." })),
                Err(e) => ToolOutput::err(e.to_string()),
            }
        }
        "antenna_df_switched" => {
            let st = world.resource::<Studio>();
            let d = doa::Switched::default();
            let sw = doa::Switched { slot_s: i["slot_us"].as_f64().map_or(d.slot_s, |v| v * 1e-6), cfo_hz: i["cfo_khz"].as_f64().map_or(d.cfo_hz, |v| v * 1e3), reference: i["reference"].as_u64().map_or(d.reference, |v| (v as usize).clamp(2, 256)), rounds: i["rounds"].as_u64().map_or(d.rounds, |v| (v as usize).clamp(1, 256)), correct: true };
            match switched_errors(&st.project.phased.df, design_f0(&st.project), &sw, 100) {
                Ok((with, without)) => text(json!({ "rms_error_deg": { "offset_taken_out": r3(with), "offset_left_in": r3(without) }, "evidence": proof::Evidence::Estimate.label(), "assumes": "One source, a steady offset across the packet, and switching with no settling time." })),
                Err(e) => ToolOutput::err(e),
            }
        }
        "antenna_df_study" => {
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            page(&mut st, super::ui::Page::DirectionFinding);
            st.run_df_study();
            text(json!({ "started": true, "next": "antenna_wait, then antenna_df_results" }))
        }
        "antenna_chain_set" => {
            let mut st = world.resource_mut::<Studio>();
            let mut d = st.project.phased.chain.clone();
            if let Err(e) = apply_chain(&mut d, i) {
                return Some(ToolOutput::err(e));
            }
            let budget = chain::budget(&d.sim.chain);
            st.project.phased.chain = d;
            page(&mut st, super::ui::Page::SignalChain);
            text(json!({ "budget": budget_json(&budget), "next": "antenna_chain_run for the sample-by-sample run" }))
        }
        "antenna_chain_budget" => {
            let st = world.resource::<Studio>();
            text(json!({ "evidence": proof::Evidence::Estimate.label(), "budget": budget_json(&chain::budget(&st.project.phased.chain.sim.chain)) }))
        }
        "antenna_chain_run" => {
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            page(&mut st, super::ui::Page::SignalChain);
            st.run_chain();
            text(json!({ "started": true, "next": "antenna_wait, then antenna_chain_results" }))
        }
        "antenna_chain_results" => {
            let st = world.resource::<Studio>();
            let jobs = st.phased_jobs();
            let Some(r) = jobs.chain else { return Some(ToolOutput::err("nothing has run yet: use antenna_chain_run")) };
            text(json!({
                "evidence": proof::Evidence::Estimate.label(),
                "taps": r.taps.iter().map(|t| json!({ "at": t.name, "signal_over_noise_db": r3(t.sinr_db), "power_dbm": r3(t.power_dbm) })).collect::<Vec<_>>(),
                "channels": r.channels,
                "array_gain_db": r3(r.array_gain_db),
                "evm_percent": r.evm_percent.map(r3),
                "converter_rms_dbfs": r3(r.adc_rms_dbfs),
                "clipped_percent": r3(r.clipped_percent),
                "agc_db": r3(r.agc_db),
                "amplifier": jobs.tx.map(|tx| tx.iter().map(|p| json!({ "backoff_db": p.backoff_db, "out_dbm": r3(p.out_dbm), "evm_percent": r3(p.evm_percent), "acpr_db": r3(p.acpr_db), "with_predistortion": { "evm_percent": r3(p.dpd_evm_percent), "acpr_db": r3(p.dpd_acpr_db) } })).collect::<Vec<_>>()),
                "note": "Signal over noise is read in one filter bandwidth at every point, wider than a narrow signal's own, so a symbol's SNR is higher by the ratio. The amplifier is a memoryless Rapp model, 30 dBm at saturation, with nothing to limit predistortion: don't quote its figures below about 0.01 % EVM or −90 dB as achievable, since real memory effects and feedback noise stop it well before that.",
            }))
        }
        "antenna_radar_set" => {
            let mut st = world.resource_mut::<Studio>();
            let mut d = st.project.phased.radar.clone();
            apply_radar(&mut d, i);
            let limits = d.resolved().limits();
            st.project.phased.radar = d;
            page(&mut st, super::ui::Page::Radar);
            text(json!({ "limits": serde_json::to_value(limits).unwrap_or(Value::Null), "next": "antenna_radar_run" }))
        }
        "antenna_radar_run" => {
            let mut st = world.resource_mut::<Studio>();
            if st.busy() {
                return Some(ToolOutput::err("a job is already running"));
            }
            page(&mut st, super::ui::Page::Radar);
            st.run_radar();
            text(json!({ "started": true, "next": "antenna_wait, then antenna_radar_results" }))
        }
        "antenna_radar_results" => {
            let st = world.resource::<Studio>();
            let d = st.project.phased.radar.clone();
            let r = d.resolved();
            let targets: Vec<Value> = d.targets.iter().map(|t| json!({ "range_m": t.range_m, "speed_mps": t.speed_mps, "theta": t.theta, "phi": t.phi, "rcs_dbsm": t.rcs_dbsm, "predicted_snr_db": r3(r.integrated_snr_db(t)) })).collect();
            let found = st.phased_jobs().radar.map(|o| {
                json!(o.detections.iter().map(|x| json!({ "range_m": r3(x.range_m), "speed_mps": r3(x.speed_mps), "theta": if x.has_angle { json!(r3(x.theta)) } else { Value::Null }, "phi": if x.has_angle { json!(r3(x.phi)) } else { Value::Null }, "snr_db": r3(x.snr_db), "speed_unfolded": x.folds != 0, "matches_target": d.targets.iter().position(|t| (t.range_m - x.range_m).abs() < 2.0 * r.limits().range_resolution_m && angle_between((t.theta, t.phi), (x.theta, x.phi)) < 3.0 * r.limits().angle_resolution_deg) })).collect::<Vec<_>>())
            });
            text(json!({ "evidence": proof::Evidence::Estimate.label(), "limits": serde_json::to_value(r.limits()).unwrap_or(Value::Null), "targets": targets, "detections": found, "note": "Point targets in free space, one bounce: no multipath, clutter or interference from other radars." }))
        }
        _ => return None,
    })
}

/// How the unit vector of a direction reads, for pages that mark beams on a map.
pub fn uv_of(theta: f64, phi: f64) -> (f64, f64) {
    let d = unit_dir(theta, phi);
    (d[0], d[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use antenna_core::phased::aperture::Layout;

    #[test]
    fn antenna_phased_lazy_values_arrive_and_keep_the_last_one_up() {
        let mut lazy: Lazy<u32, u32> = Lazy::default();
        let (first, stale) = lazy.get(&1, || 10);
        assert!(first.is_none() && stale);
        let wait = |lazy: &mut Lazy<u32, u32>, key: u32| {
            for _ in 0..2000 {
                let (v, stale) = lazy.get(&key, move || key * 10);
                if !stale {
                    return v;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            panic!("never finished");
        };
        assert_eq!(*wait(&mut lazy, 1).unwrap(), 10);
        // A new key: the old value stays up while the new one is worked out.
        let (old, stale) = lazy.get(&2, || 20);
        assert_eq!(*old.unwrap(), 10);
        assert!(stale);
        assert_eq!(*wait(&mut lazy, 2).unwrap(), 20);
    }

    #[test]
    fn antenna_phased_the_tools_read_layouts_and_refuse_nonsense() {
        assert_eq!(layout_from(&json!({ "kind": "line", "n": 12, "spacing": 0.6 })).unwrap(), Layout::Line { n: 12, spacing: 0.6 });
        assert!(matches!(layout_from(&json!({ "kind": "panels", "faces": 3 })).unwrap(), Layout::Panels { faces: 3, .. }));
        assert_eq!(layout_from(&json!({ "kind": "rings", "rings": [[4, 0.25], [8, 0.5]] })).unwrap(), Layout::Rings { rings: vec![(4, 0.25), (8, 0.5)], radial: false });
        assert!(layout_from(&json!({ "kind": "rings" })).is_err());
        assert!(layout_from(&json!({ "kind": "spiral" })).is_err());
        let mut d = PhasedDesign::default();
        apply_phased(&mut d, &json!({ "beams": [{ "theta": 30, "phi": 90 }, { "theta": 10, "phi": 0, "share": 2 }], "multibeam": "shared", "taper": "chebyshev", "sll_db": -35, "phase_bits": 6, "delay": "subarray", "subarray": [4, 4], "nulls": [[50, 180]], "failed": [3] })).unwrap();
        assert_eq!(d.plan.beams.len(), 2);
        assert_eq!(d.plan.multibeam, MultiBeam::Shared);
        assert_eq!(d.plan.taper_x, Taper::DolphChebyshev { sll_db: -35.0 });
        assert_eq!(d.plan.hw.phase_bits, Some(6));
        assert_eq!(d.plan.hw.subarray, Some((4, 4)));
        assert_eq!(d.plan.nulls, vec![(50.0, 180.0)]);
        assert!(apply_phased(&mut d, &json!({ "element": "banana" })).is_err());
        assert!(apply_phased(&mut d, &json!({ "beams": [] })).is_err());
        let mut df = DfDesign::default();
        apply_df(&mut df, &json!({ "method": "esprit", "scan": "azimuth", "theta": 80, "count": 2, "adc_bits": 4, "compare": ["music", "capon", "nonsense"], "sources": [{ "theta": 20, "phi": 0 }, { "theta": 30, "phi": 0, "coherent_with": 0 }] })).unwrap();
        assert_eq!(df.method, Method::Esprit);
        assert_eq!(df.scan, Scan::Azimuth { theta: 80.0 });
        assert_eq!(df.count, Some(2));
        assert_eq!(df.impair.adc_bits, Some(4));
        assert_eq!(df.compare, vec![Method::Music, Method::Capon]);
        assert_eq!(df.sources[1].coherent_with, Some(0));
        apply_df(&mut df, &json!({ "count": null })).unwrap();
        assert_eq!(df.count, None, "null goes back to counting by MDL");
        assert!(apply_df(&mut df, &json!({ "method": "tea leaves" })).is_err());
        let mut c = ChainDesign::default();
        apply_chain(&mut c, &json!({ "jammer_dbm": -40, "jammer_deg": 30, "jammer_waveform": "noise", "channels": 16, "beamformer": "mvdr", "stages": [{ "name": "LNA", "gain_db": 20, "nf_db": 1 }] })).unwrap();
        assert_eq!(c.channels, 16);
        assert_eq!(c.sim.bf, Bf::Mvdr);
        assert!(c.jammer.is_some());
        assert_eq!(c.sim.chain.stages.len(), 1);
        apply_chain(&mut c, &json!({ "jammer_dbm": null })).unwrap();
        assert!(c.jammer.is_none());
    }

    /// A little solved array, on the same path the page takes.
    fn small_solved() -> SolvedArray {
        let p = Project::from_template("array_dipole", None).unwrap();
        let g = p.geometry().unwrap();
        let f = p.f0();
        let opts = antenna_core::mom::MomOptions { theta_step: 10.0, phi_step: 10.0, segments_per_lambda: p.setup.segments_per_lambda, ..Default::default() };
        let pt = antenna_core::mom::MomProblem::new(&g, f, opts).unwrap().solve(f, true).unwrap();
        let z0: Vec<f64> = g.ports.iter().map(|q| q.z0).collect();
        let net = antenna_core::sparam::Network::from_z(vec![f], std::slice::from_ref(&pt.z), z0.clone());
        SolvedArray::new(f, pt.far, net.s[0].clone(), z0, g.ports.iter().map(|q| q.center()).collect(), g.ports.iter().map(|q| q.name.clone()).collect()).unwrap()
    }

    #[test]
    fn antenna_phased_a_solved_array_takes_tapers_nulls_and_shared_beams() {
        let a = small_solved();
        let mut plan = BeamPlan { beams: vec![Beam::default()], ..Default::default() };
        let plain = solved_weights(&a, &plan, false);
        // Four by four: a taper reads the lattice from the port positions.
        plan.taper_x = Taper::Hamming;
        plan.taper_y = Taper::Hamming;
        let tapered = solved_weights(&a, &plan, false);
        let (edge, middle) = (tapered[0].norm(), tapered[5].norm());
        assert!(edge < 0.5 * middle, "{edge} {middle}");
        assert!(plain.iter().all(|w| (w.norm() - 1.0).abs() < 1e-9));
        plan.taper_x = Taper::Uniform;
        plan.taper_y = Taper::Uniform;
        plan.nulls = vec![(40.0, 90.0)];
        let nulled = solved_weights(&a, &plan, false);
        let depth = 10.0 * (a.realized_gain(&nulled, 40.0, 90.0) / a.realized_gain(&nulled, 0.0, 0.0)).log10();
        assert!(depth < -60.0, "{depth}");
        plan.nulls.clear();
        plan.multibeam = MultiBeam::SharedPhaseOnly;
        plan.beams = vec![Beam { theta: 25.0, phi: 0.0, share: 1.0 }, Beam { theta: 25.0, phi: 180.0, share: 1.0 }];
        plan.hw.failed = vec![3];
        let shared = solved_weights(&a, &plan, false);
        assert_eq!(shared[3].norm(), 0.0);
        assert!(shared.iter().enumerate().all(|(i, w)| i == 3 || (w.norm() - 1.0).abs() < 1e-9));
        let (left, right) = (a.realized_gain(&shared, 25.0, 0.0), a.realized_gain(&shared, 25.0, 180.0));
        assert!(left > 5.0 && right > 5.0, "both beams are in the one pattern: {left} {right}");
    }

    #[test]
    fn antenna_phased_an_embedded_element_sizes_a_larger_array() {
        let a = small_solved();
        let element = solved_element(Some(&a), None, 0).unwrap();
        let mut d = PhasedDesign::default();
        d.element = ElementKind::Solved { embedded: true };
        d.layout = Layout::Planar { nx: 16, ny: 16, dx: 0.5, dy: 0.5, triangular: false };
        let v = beam_view(&d, a.f, Some(&element)).unwrap();
        let r = &v.reports[0];
        assert!(r.realized_dbi <= r.gain_dbi + 1e-9 && r.gain_dbi <= r.directivity_dbi + 0.5, "{r:?}");
        assert!((r.directivity_dbi - db10(256.0 * std::f64::consts::PI)).abs() < 1.0, "{r:?}");
    }

    #[test]
    fn antenna_phased_a_switched_receiver_reads_better_with_its_offset_out() {
        let df = DfDesign { sources: vec![Source { theta: 20.0, phi: 0.0, ..Default::default() }], snr_db: 25.0, ..Default::default() };
        let (with, without) = switched_errors(&df, 3e9, &doa::Switched::default(), 30).unwrap();
        assert!(with < 1.0 && without > 3.0 * with, "{with} {without}");
        let empty = DfDesign { sources: Vec::new(), ..Default::default() };
        assert!(switched_errors(&empty, 3e9, &doa::Switched::default(), 5).is_err());
    }

    #[test]
    fn antenna_phased_a_page_view_reports_what_it_left_out() {
        let df = DfDesign { layout: Layout::Ring { n: 8, radius: 0.5, radial: false }, scan: Scan::Azimuth { theta: 90.0 }, forward_backward: true, smooth_len: 5, method: Method::Music, ..Default::default() };
        let v = df_view(&df, 3e9, None);
        let r = v.result.unwrap();
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
        assert!(v.unavailable.iter().any(|u| u.0 == Method::Esprit));
    }

    #[test]
    fn antenna_phased_a_butler_fills_a_lines_beams() {
        let mut d = PhasedDesign::default();
        d.layout = Layout::Line { n: 8, spacing: 0.5 };
        apply_phased(&mut d, &json!({ "butler": true })).unwrap();
        assert_eq!(d.plan.beams.len(), 8);
        d.layout = Layout::Line { n: 6, spacing: 0.5 };
        assert!(apply_phased(&mut d, &json!({ "butler": true })).is_err());
        d.layout = Layout::default();
        assert!(apply_phased(&mut d, &json!({ "butler": true })).is_err());
        assert!(apply_phased(&mut PhasedDesign::default(), &json!({ "delay": "subarray" })).is_err(), "a delay per subarray needs subarrays");
    }

    #[test]
    fn antenna_phased_a_beam_view_covers_a_multi_panel_array() {
        let mut d = PhasedDesign::default();
        d.layout = Layout::Panels { faces: 4, nx: 6, ny: 6, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        d.plan.beams = vec![Beam { theta: 90.0, phi: 30.0, share: 1.0 }];
        let f0 = 3e9;
        let v = beam_view(&d, f0, None).unwrap();
        assert!(!v.map_is_uv, "an all-round array gets an azimuth and elevation map");
        assert_eq!(v.elements.len(), 144);
        assert!(v.reports[0].pointing_error_deg < 3.0, "{:?}", v.reports[0]);
        // Too large a design says so in words instead of hanging.
        d.layout = Layout::Planar { nx: 100, ny: 100, dx: 0.5, dy: 0.5, triangular: false };
        assert!(beam_view(&d, f0, None).err().unwrap().contains("4096"));
    }
}
