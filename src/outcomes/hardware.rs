//! The antenna and device recipes, as jobs that run off the frame loop. Each job
//! returns what happened in plain JSON and the claims to put on record.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use antenna_core::farfield::FarField;
use antenna_core::mom::{MomOptions, MomProblem};
use antenna_core::project::{Project, SimResult};
use antenna_core::recipes::{aoa, coexist, fit, integrate, medical, mom_network, sar, shelf, tagging, tunnel, uwb_bias};
use antenna_core::sparam::Network;
use bevy::prelude::{ResMut, Resource};
use proof::{Claim, Evidence, Prediction, Target, Verdict};
use serde_json::{Value, json};

use super::Predictions;
use crate::rtls::antenna::AntennaLibrary;
use crate::rtls::planning::{FdtdAntenna, Tech};

#[derive(Default)]
pub struct JobResult {
    pub value: Value,
    pub claims: Vec<Claim>,
    /// A pattern to add to the antenna library.
    pub antenna: Option<FdtdAntenna>,
}

pub enum JobState {
    Running,
    Done(Value),
    Failed(String),
}

pub struct Job {
    pub id: u64,
    pub title: String,
    pub started: std::time::Instant,
    pub seconds: Option<f64>,
    pub state: JobState,
    outbox: Arc<Mutex<Option<Result<JobResult, String>>>>,
}

#[derive(Resource, Default)]
pub struct Jobs {
    next: u64,
    pub list: Vec<Job>,
}

impl Jobs {
    /// Runs `work` on its own thread. Solvers are parallel inside, so one job at a
    /// time gets the machine, and the frame loop stays free.
    pub fn start(&mut self, title: &str, work: impl FnOnce() -> Result<JobResult> + Send + 'static) -> u64 {
        self.next += 1;
        let id = self.next;
        let outbox: Arc<Mutex<Option<Result<JobResult, String>>>> = Default::default();
        let out = outbox.clone();
        let spawned = std::thread::Builder::new().name(format!("recipe-{id}")).stack_size(16 << 20).spawn(move || {
            // A panic in a solver must end the job, not leave it running forever.
            let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(p) => Err(format!("the solver stopped unexpectedly: {}", p.downcast_ref::<String>().map(String::as_str).or_else(|| p.downcast_ref::<&str>().copied()).unwrap_or("no details"))),
            };
            *out.lock().unwrap() = Some(r);
        });
        let state = match spawned {
            Ok(_) => JobState::Running,
            Err(e) => JobState::Failed(format!("couldn't start a thread: {e}")),
        };
        self.list.push(Job { id, title: title.into(), started: std::time::Instant::now(), seconds: None, state, outbox });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Job> {
        self.list.iter().find(|j| j.id == id)
    }
}

/// Files finished work: claims into the ledger, patterns into the library.
pub fn collect_finished(mut jobs: ResMut<Jobs>, mut ledger: ResMut<Predictions>, mut library: ResMut<AntennaLibrary>) {
    for j in jobs.list.iter_mut().filter(|j| matches!(j.state, JobState::Running)) {
        let Some(r) = j.outbox.lock().unwrap().take() else { continue };
        j.seconds = Some(j.started.elapsed().as_secs_f64());
        match r {
            Ok(res) => {
                for c in res.claims {
                    ledger.put(c);
                }
                if let Some(a) = res.antenna
                    && let Err(e) = library.save(a)
                {
                    j.state = JobState::Failed(format!("the result is fine, but saving the antenna failed: {e}"));
                    continue;
                }
                j.state = JobState::Done(res.value);
            }
            Err(e) => j.state = JobState::Failed(e),
        }
    }
}

fn project_from(i: &Value) -> Result<Project> {
    let template = i["template"].as_str().context("give a template, e.g. strip_dipole, bowtie, patch_air, monopole")?;
    let mut p = Project::from_template(template, i["f0"].as_str())?;
    if let Some(vars) = i["vars"].as_object() {
        for (k, v) in vars {
            let expr = v.as_str().map_or_else(|| v.to_string(), str::to_string);
            match p.params.iter_mut().find(|r| &r.name == k) {
                Some(r) => r.expr = expr,
                None => bail!("{template} has no variable '{k}'; it has {}", p.params.iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", ")),
            }
        }
    }
    Ok(p)
}

fn num(i: &Value, key: &str, default: f64) -> f64 {
    i[key].as_f64().unwrap_or(default)
}

/// Round to something a person can read. A failed number stays failed, which JSON
/// writes as null: never as a 0 that reads like a result.
fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// What a recipe's numbers rest on, for its result.
fn evidence_note(e: Evidence) -> String {
    format!("{}: not validated on hardware", e.label())
}

/// A short key for what was asked, so another design gets its own prediction and the
/// same request replaces its earlier result.
fn key_of(i: &Value) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in i.to_string().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// One solve is one number, with no range and so no chance to quote: it either meets
/// the target in that solve or it doesn't. A number that failed has met nothing.
pub(super) fn exact_claim(id: &str, recipe: &str, subject: &str, p: Prediction, target: Option<Target>, assumptions: Vec<String>) -> Claim {
    let verdict = target.map_or(Verdict::NoTarget, |t| if t.met_by(p.median) { Verdict::Likely } else { Verdict::Unlikely }).capped(p.evidence, p.is_point());
    Claim { id: id.into(), recipe: recipe.into(), subject: subject.into(), prediction: p, target, pass_probability: None, verdict, drivers: Vec::new(), assumptions, checks: Vec::new(), created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) }
}

// ---------------------------------------------------------------------------
// UWB antenna bias

fn metrics_json(m: &uwb_bias::BiasMetrics) -> Value {
    json!({
        "rms_cm": r3(m.rms_m * 100.0),
        "p95_cm": r3(m.p95_abs_m * 100.0),
        "peak_to_peak_cm": r3(m.peak_to_peak_m * 100.0),
        "pulse_fidelity_min": r3(m.fidelity_min),
        "gain_mean_dbi": r3(m.gain_mean_dbi),
        "gain_min_dbi": r3(m.gain_min_dbi),
        "directions_usable": r3(m.usable_share),
    })
}

pub fn uwb_bias_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let channel = if i["channel"].as_u64() == Some(9) { uwb_bias::Channel::CH9 } else { uwb_bias::Channel::CH5 };
    let mut project = project_from(i)?;
    if i["f0"].is_null() {
        let f = format!("{}", channel.f0);
        if let Some(r) = project.params.iter_mut().find(|r| r.name == "f0") {
            r.expr = f;
        }
    }
    project.geometry()?;
    // A site's own cone, in degrees from the antenna's axis, outranks the stock regions.
    let region = match (i["theta_min"].as_f64(), i["theta_max"].as_f64(), i["region"].as_str()) {
        (Some(lo), Some(hi), _) => {
            if !(0.0..=180.0).contains(&lo) || !(0.0..=180.0).contains(&hi) || lo > hi {
                bail!("theta_min and theta_max must be degrees from 0 to 180, the smaller first");
            }
            uwb_bias::Region { theta_min: lo, theta_max: hi }
        }
        (_, _, Some("all")) => uwb_bias::Region::ALL,
        _ => uwb_bias::Region::HORIZON,
    };
    let (n, step, edges) = (num(i, "frequencies", 5.0) as usize, num(i, "angle_step", 15.0), num(i, "edges_per_lambda", 16.0));
    let vars: Vec<(String, f64, f64)> = i["optimize"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| Some((v["name"].as_str()?.to_string(), v["min"].as_f64()?, v["max"].as_f64()?))).collect());
    if let Some(v) = vars.iter().find(|v| !(v.1 < v.2)) {
        bail!("'{}' needs min below max", v.0);
    }
    let key = key_of(i);
    let max_evals = num(i, "max_evals", 24.0) as usize;
    let save_as = i["save_as"].as_str().map(str::to_string);
    let target_cm = i["target_cm"].as_f64();
    Ok(move || {
        let opts = uwb_bias::BiasOptions { channel, ..Default::default() };
        let goal = uwb_bias::BiasGoal { region, ..Default::default() };
        let mut value = json!({});
        if !vars.is_empty() {
            let d = uwb_bias::optimise(&project, &vars, opts, goal, n, step, edges, max_evals)?;
            let pairs: Vec<(String, f64)> = d.names.iter().cloned().zip(d.best.iter().copied()).collect();
            value["optimised"] = json!({ "variables": pairs, "before": metrics_json(&d.before), "after": metrics_json(&d.after), "solves": d.evals });
            project = project.with_values(&pairs);
        }
        let geom = project.geometry()?;
        let dielectric = geom.solids.iter().any(|s| antenna_core::material::by_name(&s.material).is_some_and(|m| !m.is_conductor()));
        let map = if dielectric {
            uwb_bias::bias_map(&fdtd_far_fields(&project, &channel.solve_freqs(n), step)?, opts)?
        } else {
            uwb_bias::analyse_mom(&geom, 0, n, step, opts, edges)?
        };
        value["solver"] = json!(if dielectric { "FDTD (openEMS)" } else { "Method of moments" });
        let m = map.metrics(region);
        value["bias"] = metrics_json(&m);
        value["removed_by_delay_calibration_cm"] = json!(r3(map.mean_delay_m * 100.0));
        value["meaning"] = json!("Range error that remains after a normal antenna-delay calibration, from the antenna's shape alone. Compare it with the site's multipath and walls before spending on it.");
        value["evidence"] = json!(evidence_note(Evidence::Simulated));
        let claim = uwb_bias_claim(&format!("uwb_bias_{key}"), &project.name, &m, dielectric, target_cm);
        let antenna = save_as.and_then(|label| {
            let bias = map.for_simulator();
            if bias.is_none() {
                value["not_saved"] = json!("No direction in this antenna can carry a link, so there's no shift to give the simulator.");
                return None;
            }
            value["saved_as"] = json!(label);
            value["saved_nulls_as"] = json!("The worst usable shift: the simulator can't be told there's no link in those directions");
            Some(FdtdAntenna {
                tech: Tech::Uwb,
                label,
                source: Default::default(),
                theta_step: (map.theta.get(1).copied().unwrap_or(15.0) - map.theta[0]) as f32,
                phi_step: (map.phi.get(1).copied().unwrap_or(15.0) - map.phi[0]) as f32,
                d: map.gain_dbi.iter().map(|r| r.iter().map(|v| *v as f32).collect()).collect(),
                mismatch_db: 0.0,
                axis_forward: geom.axis_forward,
                bias_m: bias,
            })
        });
        Ok(JobResult { value, claims: vec![claim], antenna })
    })
}

/// The bias as a claim, saying how many directions it really speaks for. The 95 % is of
/// the usable ones, so at 50 % usable it covers 47.5 %, and no answer isn't 0 cm.
fn uwb_bias_claim(id: &str, name: &str, m: &uwb_bias::BiasMetrics, dielectric: bool, target_cm: Option<f64>) -> Claim {
    let (cm, covered) = (m.p95_abs_m * 100.0, 95.0 * m.usable_share);
    let prediction = Prediction { metric: format!("{covered:.0} % of directions within"), unit: "cm".into(), median: cm, low: cm, high: cm, evidence: Evidence::Simulated };
    let mut assumptions = vec![
        if dielectric { "Solved with FDTD as modelled, in free space: no enclosure or body beyond what's in the model".to_string() } else { "Metal only, solved in free space: no substrate, enclosure or body".to_string() },
        format!("{:.0} % of the directions in the region can carry a link. The rest are in nulls and aren't scored", m.usable_share * 100.0),
        "After calibration means the best delay for this whole region. A delay calibrated on the bench in one direction can leave up to about twice this".into(),
        "A leading-edge detector at 25 % of the peak stands in for the chip's first-path algorithm".into(),
        "Receiver polarisation matched in each direction".into(),
    ];
    if m.p95_abs_m.is_nan() {
        assumptions.insert(0, "No direction in the region can carry a link, so there is no number to give".into());
    }
    let mut claim = exact_claim(id, "uwb_antenna_bias", &format!("Range shift from the antenna: {name}"), prediction, target_cm.map(Target::at_most), assumptions);
    // A target met over only part of the directions hasn't been met.
    if claim.verdict == Verdict::MeetsOnce && m.usable_share < 0.9 {
        claim.verdict = Verdict::Uncertain;
    }
    claim
}

// ---------------------------------------------------------------------------
// RFID shelf

pub fn shelf_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let d = shelf::ShelfSpec::default();
    let spec = shelf::ShelfSpec {
        width: num(i, "width_m", d.width),
        depth: num(i, "depth_m", d.depth),
        gap: num(i, "gap_m", d.gap),
        above: num(i, "levels_above", d.above as f64) as usize,
        below: num(i, "levels_below", d.below as f64) as usize,
        f: num(i, "f_mhz", 915.0) * 1e6,
        product_loss_db: (num(i, "product_loss_db", d.product_loss_db.0), num(i, "product_loss_spread_db", d.product_loss_db.1)),
        min_read: num(i, "min_read", d.min_read),
        max_stray: num(i, "max_stray", d.max_stray),
        tx_range_dbm: (num(i, "min_dbm", d.tx_range_dbm.0), num(i, "max_dbm", d.tx_range_dbm.1)),
        ..d
    };
    if !(spec.width > 0.1 && spec.depth > 0.1 && spec.gap > 0.05) {
        bail!("a shelf needs a width and depth above 10 cm and a level gap above 5 cm");
    }
    let optimize = i["optimize"].as_bool().unwrap_or(false);
    let key = key_of(i);
    let (edges, max_evals) = (num(i, "edges_per_lambda", 8.0), num(i, "max_evals", 24.0) as usize);
    Ok(move || {
        let start = shelf::ShelfAntenna::start(&spec);
        let mut value = json!({});
        let best = if optimize {
            let r = shelf::design(&spec, &start, edges, max_evals)?;
            value["optimised"] = json!({
                "before": { "read": r3(r.before.read), "stray": r3(r.before.stray), "reader_dbm": r.before.tx_dbm },
                "after": { "read": r3(r.after.read), "stray": r3(r.after.stray), "reader_dbm": r.after.tx_dbm },
                "solves": r.evals,
            });
            r.best
        } else {
            start
        };
        let field = shelf::solve(&spec, &best, edges)?;
        let nominal = shelf::best_power(&field, &spec);
        let (read, stray) = shelf::predict(&field, &spec, nominal.tx_dbm, 400);
        value["antenna_mm"] = json!({ "length": r3(best.l * 1e3), "width": r3(best.w * 1e3), "height": r3(best.h * 1e3), "x": r3(best.x * 1e3), "y": r3(best.y * 1e3), "rim": r3(best.lip * 1e3) });
        value["reader_dbm"] = json!(nominal.tx_dbm);
        value["s11_db"] = json!(r3(field.s11_db));
        value["read"] = json!({ "median": r3(read.prediction.median), "low": r3(read.prediction.low), "high": r3(read.prediction.high), "runs": read.runs(), "main_driver": read.main_driver().map(|s| s.factor.clone()) });
        value["stray"] = json!({ "median": r3(stray.prediction.median), "low": r3(stray.prediction.low), "high": r3(stray.prediction.high), "runs": stray.runs() });
        value["unknowns"] = json!(field.unknowns);
        value["evidence"] = json!(evidence_note(Evidence::Estimate));
        let claims = vec![
            Claim::from_report(&format!("shelf_read_{key}"), "rfid_shelf", "Read rate on the shelf level", &read, Some(Target::at_least(spec.min_read)), shelf::assumptions()),
            Claim::from_report(&format!("shelf_stray_{key}"), "rfid_shelf", "Stray reads on the levels around it", &stray, Some(Target::at_most(spec.max_stray)), shelf::assumptions()),
        ];
        Ok(JobResult { value, claims, antenna: None })
    })
}

// ---------------------------------------------------------------------------
// Angle of arrival

pub fn aoa_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let f = num(i, "f_ghz", 2.44) * 1e9;
    let (nx, ny) = (num(i, "nx", 4.0) as usize, num(i, "ny", 4.0) as usize);
    if nx * ny < 2 {
        bail!("an array needs at least 2 elements");
    }
    let d = num(i, "spacing_m", 0.5 * antenna_core::C0 / f);
    let theta_max = num(i, "theta_max", 70.0);
    let field = aoa::Perturbation { phase_sigma_deg: num(i, "phase_sigma_deg", 5.0), amp_sigma_db: num(i, "amp_sigma_db", 0.5), snr_db: num(i, "snr_db", 20.0), seed: 1 };
    let name = i["save_as"].as_str().unwrap_or("array").to_string();
    let key = key_of(i);
    // BLE's channels run about 1.6 % either side of 2.44 GHz.
    let band = num(i, "band_share", 0.016);
    Ok(move || {
        let geom = aoa::monopole_array(nx, ny, d, d, f);
        let (step, pol, draws) = (5.0, (1.0, 0.0), 16);
        let freqs = [f * (1.0 - band), f, f * (1.0 + band)];
        // Solved at half the table's step, so it can be tested between its own points.
        let problem = MomProblem::new(&geom, freqs[2], MomOptions { theta_step: step / 2.0, phi_step: step / 2.0, segments_per_lambda: 20.0, ..Default::default() })?;
        let by_freq: Vec<Vec<FarField>> = freqs.iter().map(|g| problem.solve(*g, true).map(|p| p.far)).collect::<Result<_>>()?;
        let fields: &[FarField] = &by_freq[1];
        let r = aoa::analyse(&geom, fields, theta_max, step, pol, field)?;
        let table = aoa::solved_manifold(fields, theta_max, step, pol)?;
        let textbook = aoa::ideal_manifold(&aoa::positions(&geom), f, step, theta_max);
        let test = aoa::held_out(&by_freq, &table, theta_max, step, pol, field, draws)?;
        let test_textbook = aoa::held_out(&by_freq, &textbook, theta_max, step, pol, field, draws)?;
        let dir = crate::rtls::antenna::AntennaLibrary::dir().join("aoa");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", name.replace(|c: char| !c.is_ascii_alphanumeric(), "_")));
        std::fs::write(&path, serde_json::to_string(&table)?)?;
        let value = json!({
            "textbook_model_rms_deg": r3(r.ideal.rms_deg),
            "textbook_with_field_conditions_rms_deg": r3(test_textbook.rms_deg),
            "with_calibration_table_rms_deg": r3(test.rms_deg),
            "table_p95_deg": r3(test.p95_deg),
            "table_worst_draw_rms_deg": r3(test.worst_run_rms_deg),
            "table_on_its_own_grid_rms_deg": r3(r.table.rms_deg),
            "tested": format!("Between the table's grid points, at {:.3}, {:.3} and {:.3} GHz, over {draws} perturbed arrays. The table is from the middle frequency.", freqs[0] / 1e9, freqs[1] / 1e9, freqs[2] / 1e9),
            "elements": r.elements,
            "table_file": path.to_string_lossy(),
            // These are total angle errors; the simulator applies its figure per axis.
            "for_the_site": format!("Set the locator profile's aoa_sigma_deg to about {:.1} with this table, or {:.1} without.", test.rms_deg / std::f64::consts::SQRT_2, test_textbook.rms_deg / std::f64::consts::SQRT_2),
            "evidence": evidence_note(Evidence::Simulated),
        });
        let spread = |v: f64| Prediction { metric: "Angle error, RMS".into(), unit: "°".into(), median: v, low: v, high: v, evidence: Evidence::Simulated };
        let claims = vec![exact_claim(&format!("aoa_table_{key}"), "aoa_array", &format!("{nx}×{ny} array with its calibration table"), spread(test.rms_deg), None, vec![
            "Monopoles on an infinite ground plane stand in for the real array and its mount".into(),
            format!("Field conditions assumed: {:.0}° phase and {:.1} dB level error per element, {:.0} dB SNR", field.phase_sigma_deg, field.amp_sigma_db, field.snr_db),
            format!("Tested between the table's grid points, across ±{:.1} % of frequency and {draws} perturbed arrays, on the same solved model; the worst array gave {:.2}°", band * 100.0, test.worst_run_rms_deg),
            "One wave at a time: reflections aren't in this number".into(),
        ])];
        Ok(JobResult { value, claims, antenna: None })
    })
}

// ---------------------------------------------------------------------------
// Integration

/// Whether any of these needs FDTD. Before and after must come from one solver, or the
/// difference between solvers reads as what the enclosure did.
fn needs_fdtd(projects: &[&Project]) -> Result<bool> {
    for p in projects {
        if p.geometry()?.solids.iter().any(|s| antenna_core::material::by_name(&s.material).is_some_and(|m| !m.is_conductor())) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn solve_for_integration(p: &Project, f_lo: f64, f_hi: f64, fdtd: bool) -> Result<Network> {
    if fdtd {
        return solve_fdtd(p, f_lo, f_hi);
    }
    mom_network(&p.geometry()?, f_lo, f_hi, 15, 201, MomOptions { edges_per_lambda: 20.0, segments_per_lambda: 24.0, ..Default::default() })
}

/// A recipe's result can't show a solver's warning, so a run that was cut off fails the
/// job rather than feeding the prediction a wrong number. The other warnings are logged.
#[cfg(feature = "antenna-studio")]
fn usable(r: SimResult) -> Result<SimResult> {
    if r.stopped_short() {
        bail!("the FDTD run hit its step limit before its energy decayed, so its result can't be used: raise the step limit and run it again");
    }
    for w in &r.warnings {
        bevy::log::warn!("{}: {w}", r.label);
    }
    Ok(r)
}

#[cfg(feature = "antenna-studio")]
fn solve_fdtd(p: &Project, f_lo: f64, f_hi: f64) -> Result<Network> {
    use crate::antenna::run::{JobCtx, RunOptions, simulate};
    let mut q = p.clone();
    q.setup.f_lo = f_lo;
    q.setup.f_hi = f_hi;
    q.setup.solver = antenna_core::project::SolverKind::Fdtd;
    q.setup.far_freqs = Vec::new();
    q.setup.field_planes = false;
    let ctx = JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
    let r = usable(simulate(&q, &ctx, &RunOptions { keep: false, ..Default::default() })?)?;
    Ok(r.network)
}

#[cfg(not(feature = "antenna-studio"))]
fn solve_fdtd(_: &Project, _: f64, _: f64) -> Result<Network> {
    let _: Option<SimResult> = None;
    bail!("this model has plastic or another dielectric, which needs the FDTD solver from Antenna Studio, left out of this build. Metal-only parts work here.")
}

/// Far fields across a band from one FDTD run: a time-domain solve covers it all.
#[cfg(feature = "antenna-studio")]
fn fdtd_far_fields(p: &Project, freqs: &[f64], step: f64) -> Result<Vec<FarField>> {
    use crate::antenna::run::{JobCtx, RunOptions, simulate};
    let mut q = p.clone();
    q.setup.solver = antenna_core::project::SolverKind::Fdtd;
    q.setup.far_freqs = freqs.to_vec();
    q.setup.far_step = step;
    q.setup.field_planes = false;
    let ctx = JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
    let mut r = usable(simulate(&q, &ctx, &RunOptions { keep: false, ..Default::default() })?)?;
    if r.far.is_empty() {
        bail!("the FDTD run returned no far fields");
    }
    Ok(r.far.swap_remove(0))
}

#[cfg(not(feature = "antenna-studio"))]
fn fdtd_far_fields(_: &Project, _: &[f64], _: f64) -> Result<Vec<FarField>> {
    bail!("this antenna has a substrate, which needs the FDTD solver from Antenna Studio, left out of this build. Metal-only antennas work here.")
}

pub fn integrate_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let project = project_from(i)?;
    let f0 = project.f0();
    let (f_lo, f_hi) = (num(i, "f_lo_ghz", f0 * 0.98 / 1e9) * 1e9, num(i, "f_hi_ghz", f0 * 1.02 / 1e9) * 1e9);
    let (sweep_lo, sweep_hi) = (f0 * 0.65, f0 * 1.35);
    let mut parts: Vec<integrate::Part> = match i.get("parts") {
        Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| anyhow!("parts: {e}"))?,
        _ => Vec::new(),
    };
    if let Some(s) = i.get("shell").filter(|s| !s.is_null()) {
        parts.extend(integrate::shell_around(&project, s["material"].as_str().unwrap_or("ABS plastic"), num(s, "gap_m", 0.002), num(s, "wall_m", 0.002))?);
    }
    if parts.is_empty() {
        bail!("give parts ([{{name, material, min:[x,y,z], max:[x,y,z]}}]) or a shell ({{material, gap_m, wall_m}})");
    }
    integrate::check_parts(&project, &parts)?;
    let retune = i["retune"].as_str().map(str::to_string);
    let key = key_of(i);
    let max_loss = i["max_loss_db"].as_f64();
    Ok(move || {
        let with = integrate::with_parts(&project, &parts);
        let fdtd = needs_fdtd(&[&project, &with])?;
        let solve = |p: &Project| solve_for_integration(p, sweep_lo, sweep_hi, fdtd);
        let before = solve(&project)?;
        let after = solve(&with)?;
        let d = integrate::compare(&before, &after, 0, f_lo, f_hi);
        let mut fixes = Vec::new();
        if let Some(m) = integrate::match_fix(&after, 0, f_lo, f_hi) {
            fixes.push(m);
        }
        if let Some(var) = &retune {
            fixes.push(integrate::retune(&with, var, (f_lo + f_hi) / 2.0, f_lo, f_hi, 0, 6, solve)?);
        }
        let solver = if fdtd { "FDTD (openEMS), for both sides" } else { "Method of moments, for both sides" };
        let evidence = Evidence::Simulated;
        let value = json!({ "detuning": d, "fixes": fixes, "summary": d.summary, "solver": solver, "evidence": evidence_note(evidence) });
        let limit = max_loss.unwrap_or(1.0);
        let mut assumptions = vec!["Only the parts listed are modelled".to_string(), "Part sizes and material constants as entered".to_string()];
        if max_loss.is_none() {
            assumptions.push("The 1 dB target is this recipe's default, not your link budget: give max_loss_db".into());
        }
        let claim = exact_claim(&format!("integration_loss_{key}"), "antenna_integration", &format!("{} inside the product", project.name), Prediction { metric: "Power lost to mismatch at the band's worst point".into(), unit: "dB".into(), median: d.band_loss_after_db, low: d.band_loss_after_db, high: d.band_loss_after_db, evidence }, Some(Target::at_most(limit)), assumptions);
        Ok(JobResult { value, claims: vec![claim], antenna: None })
    })
}

// ---------------------------------------------------------------------------
// Quick recipes

pub fn tag_advice(i: &Value) -> Result<JobResult> {
    let id = i["contents"].as_str().context("give contents: one of dry_goods, cardboard, plastic, water_based, oily, frozen, metal_can, foil_lined")?;
    let c = tagging::contents(id).with_context(|| format!("unknown contents '{id}'"))?;
    let (f, eirp) = if i["region"].as_str() == Some("etsi") { (866.5e6, 35.0) } else { (915e6, 36.0) };
    let target = num(i, "target_range_m", 3.0);
    let list = tagging::recommend(c, f, eirp, target, 200);
    // Ranked, but with no chance of reaching the target quoted: the formula behind it is
    // too rough to carry a percentage, however many samples go through it.
    let value = json!({
        "contents": c.label,
        "options": list.iter().take(6).map(|o| json!({ "tag": o.tag, "placement": o.placement, "range_m": r3(o.report.prediction.median), "low": r3(o.report.prediction.low), "high": r3(o.report.prediction.high), "main_driver": o.report.main_driver().map(|s| s.factor.clone()), "outside_the_fit": o.warnings })).collect::<Vec<_>>(),
        "evidence": "Estimate: use it to rank options, then confirm on the real product",
    });
    let claims = list.first().map(|o| {
        let mut assumptions = tagging::assumptions();
        assumptions.extend(o.warnings.iter().cloned());
        Claim::from_report(&format!("tag_{id}_{}", key_of(i)), "tag_on_product", &format!("{}: {} ({})", c.label, o.tag, o.placement), &o.report, Some(Target::at_least(target)), assumptions).without_pass_chance()
    }).into_iter().collect();
    Ok(JobResult { value, claims, antenna: None })
}

pub fn coexistence(i: &Value) -> Result<JobResult> {
    let radios: Vec<coexist::Radio> = i["radios"].as_array().context("give radios: names from the presets, or full radio objects")?.iter().map(|v| match v.as_str() {
        Some(n) => coexist::Radio::preset(n).with_context(|| format!("no preset '{n}'; there are {}", coexist::Radio::presets().iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", "))),
        None => serde_json::from_value(v.clone()).map_err(|e| anyhow!("radio: {e}")),
    }).collect::<Result<_>>()?;
    if radios.len() < 2 {
        bail!("give at least two radios");
    }
    let flat = num(i, "isolation_db", 20.0);
    let net = match i["touchstone"].as_str() {
        Some(p) => Some(Network::from_touchstone(&std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?)?),
        None => None,
    };
    let has_net = net.as_ref().is_some_and(|n| n.ports() >= radios.len());
    let list = coexist::matrix(&radios, |a, v| if has_net { coexist::Isolation::Network { i: v, j: a } } else { coexist::Isolation::Flat(flat) }, net.as_ref());
    let on_air = |name: &str| radios.iter().find(|r| r.name == name).map(|r| r3(r.duty));
    let value = json!({
        "isolation": match (&net, has_net) {
            (_, true) => "from the S-parameter file, port order = radio order".to_string(),
            (Some(n), false) => format!("{flat} dB everywhere: the file has {} ports for {} radios, so it wasn't used", n.ports(), radios.len()),
            _ => format!("{flat} dB everywhere, as a first guess"),
        },
        "pairs": list.iter().map(|p| json!({ "transmitting": p.aggressor, "hurt": p.victim, "desense_db": r3(p.desense_db), "blocked": p.blocked, "range_left": r3(p.range_while_on), "range_left_is_while": "the other radio is on the air", "other_radio_on_air_share": on_air(&p.aggressor), "isolation_needed_db": r3(p.needed_isolation_db), "harmonics": p.harmonics, "isolation_not_in_the_data_at_ghz": p.isolation_unknown_at_hz.iter().map(|f| r3(f / 1e9)).collect::<Vec<_>>(), "note": p.note })).collect::<Vec<_>>(),
        "evidence": "Estimate from typical radio figures unless you gave full radio objects: not validated on hardware",
        "us_restricted_harmonics": radios.iter().filter_map(|r| {
            let list: Vec<Value> = coexist::restricted_harmonics(r).iter().map(|h| json!({ "order": h.order, "band_mhz": [r3(h.band_mhz.0), r3(h.band_mhz.1)], "assumed_level_dbm": r3(h.level_dbm), "required_dbc": r3(h.required_dbc), "over_limit_db": r3(h.over_limit_db) })).collect();
            (!list.is_empty()).then(|| json!({ "radio": r.name, "harmonics": list }))
        }).collect::<Vec<_>>(),
        "us_restricted_harmonics_note": format!("FCC 15.205 and 15.209: a harmonic in a restricted band must stay under {} dBm EIRP. tx_dbm stands for EIRP here, and the level is an assumption until you give the radio's real harmonic_dbc, so this says what to test for, not that a part fails. Other regions have their own limits.", coexist::FCC_RESTRICTED_LIMIT_DBM_EIRP),
    });
    let mut claims = Vec::new();
    if let Some(w) = list.first()
        && let (Some(a), Some(v)) = (radios.iter().find(|r| r.name == w.aggressor), radios.iter().find(|r| r.name == w.victim))
    {
        let rep = coexist::predict(a, v, w.isolation_db, 0.9, 300);
        claims.push(Claim::from_report(&format!("coexistence_{}", key_of(i)), "radio_coexistence", &format!("{} while {} transmits", v.name, a.name), &rep, Some(Target::at_least(0.9)), coexist::assumptions()));
    }
    Ok(JobResult { value, claims, antenna: None })
}

pub fn fit_measured_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let path = i["measured"].as_str().context("give measured: the path of a Touchstone file from the VNA")?.to_string();
    let measured = Network::from_touchstone(&std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?)?;
    let simulated = match i["simulated"].as_str() {
        Some(p) => Some(Network::from_touchstone(&std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?)?),
        None => None,
    };
    let project = if simulated.is_none() { Some(project_from(i).context("give simulated (a Touchstone file) or a template to solve")?) } else { None };
    let er = i["er"].as_f64();
    let key = key_of(i);
    Ok(move || {
        let sim = match (simulated, project) {
            (Some(n), _) => n,
            (None, Some(p)) => {
                let g = p.geometry()?;
                // Wider than the measurement, or a scale above 1 runs off the model's end.
                mom_network(&g, measured.f[0] * 0.8, *measured.f.last().unwrap() * 1.25, 31, 301, MomOptions { edges_per_lambda: 20.0, segments_per_lambda: 24.0, ..Default::default() })?
            }
            _ => unreachable!(),
        };
        let r = fit::fit_measured(&measured, &sim, 0, er)?;
        let f_res = r.f_res_simulated.unwrap_or(f64::NAN) / 1e9;
        let mut assumptions = vec!["Model and antenna differ by a frequency scale and a series resistance, nothing stranger".to_string()];
        assumptions.extend(r.notes.iter().cloned());
        let mut claim = exact_claim(&format!("vna_resonance_{key}"), "vna_fit", &format!("Resonance against {path}"), Prediction::exact("Resonance", "GHz", f_res, Evidence::Simulated), None, assumptions);
        if let Some(m) = r.f_res_measured {
            claim.check_from(m / 1e9, "From the VNA sweep", &path);
        }
        let value = json!({
            "frequency_scale": r3(r.f_scale),
            "extra_series_resistance_ohm": r3(r.r_extra),
            "resistance_also_fits_ohm": r.r_extra_alternatives.iter().map(|v| r3(*v)).collect::<Vec<_>>(),
            "ambiguous": !r.r_extra_alternatives.is_empty(),
            "resistance_means": "Loss or radiation: S11 alone can't tell them apart",
            "simulated_resonance_ghz": r.f_res_simulated.map(|v| r3(v / 1e9)),
            "measured_resonance_ghz": r.f_res_measured.map(|v| r3(v / 1e9)),
            "rms_before_db": r3(r.rms_before_db),
            "rms_after_db": r3(r.rms_after_db),
            "implied_er": r.implied_er.map(r3),
            "implied_er_means": "If all of the field were in the substrate. Part of it is in the air, so the real change is larger",
            "converged": r.calibration.converged,
            "notes": r.notes,
            "evidence": "Simulated, fitted to the measured sweep you gave: one antenna, not validated more widely",
        });
        Ok(JobResult { value, claims: vec![claim], antenna: None })
    })
}

/// The tolerances asked for. A missing or misspelt `sigma_m` used to mean "this doesn't
/// vary", which gives 100 % yield for any design that's good at nominal.
fn parse_tolerances(i: &Value) -> Result<(Vec<String>, Vec<f64>)> {
    let list = i["tolerances"].as_array().context("give tolerances: [{name, sigma_m}], the dimensions the factory varies")?;
    let (mut names, mut tol) = (Vec::new(), Vec::new());
    for v in list {
        let name = v["name"].as_str().context("each tolerance needs a name and sigma_m")?;
        let Some(sigma) = v["sigma_m"].as_f64() else {
            bail!("tolerance '{name}': sigma_m must be a number in metres, got {}", v["sigma_m"]);
        };
        if !sigma.is_finite() || sigma < 0.0 {
            bail!("tolerance '{name}': sigma_m can't be {sigma}");
        }
        names.push(name.to_string());
        tol.push(sigma);
    }
    if names.is_empty() {
        bail!("give at least one tolerance");
    }
    if tol.iter().all(|s| *s == 0.0) {
        bail!("every sigma_m is 0, so nothing varies and a yield would mean nothing");
    }
    Ok((names, tol))
}

pub fn field_yield_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let project = project_from(i)?;
    let (names, tol) = parse_tolerances(i)?;
    let vars = project.vars()?;
    let x0: Vec<f64> = names.iter().map(|n| vars.get(n).copied().with_context(|| format!("no variable '{n}'"))).collect::<Result<_>>()?;
    let f0 = project.f0();
    let (f_lo, f_hi) = (num(i, "f_lo_ghz", f0 * 0.98 / 1e9) * 1e9, num(i, "f_hi_ghz", f0 * 1.02 / 1e9) * 1e9);
    let exponent = num(i, "path_loss_exp", 2.2);
    let target = num(i, "min_range_share", 0.9);
    let key = key_of(i);
    Ok(move || {
        let steps: Vec<f64> = tol.clone();
        let solves = names.clone();
        let metric = |x: &[f64]| -> Result<f64> {
            let pairs: Vec<(String, f64)> = solves.iter().cloned().zip(x.iter().copied()).collect();
            let g = project.with_values(&pairs).geometry()?;
            let n = mom_network(&g, f_lo, f_hi, 5, 41, MomOptions { edges_per_lambda: 20.0, segments_per_lambda: 24.0, ..Default::default() })?;
            let worst = antenna_core::project::band_worst(&n, 0, 0, f_lo, f_hi).unwrap_or(0.0);
            Ok(fit::range_share_from_mismatch(worst, exponent))
        };
        let response = fit::response(&names, &x0, &steps, metric)?;
        let rep = fit::field_yield("Link range at the band's worst point", "share of a perfect match", &response, &tol, Target::at_least(target), Some(1.0), 2000);
        let value = json!({
            "yield_about": rep.pass_probability.map(|p| r3(fit::quoted_yield(p))),
            "yield_could_be_off_by": fit::YIELD_TOLERANCE,
            "range_share": { "median": r3(rep.prediction.median), "low": r3(rep.prediction.low), "high": r3(rep.prediction.high) },
            "biggest_cause": rep.main_driver().map(|s| s.factor.clone()),
            "solves": response.solves,
            "evidence": evidence_note(Evidence::Estimate),
        });
        let tolerance = format!("A quadratic per dimension through ±1σ, with no cross terms, from {} solves. Its own test allows about ±{:.0} points of yield, which the {} draws through it don't reduce", response.solves, fit::YIELD_TOLERANCE * 100.0, rep.samples);
        let claim = Claim::from_report(&format!("field_yield_{key}"), "field_yield", &format!("Units of {} meeting the range target", project.name), &rep, Some(Target::at_least(target)), vec![tolerance, "Only the antenna's match is judged".into()]);
        Ok(JobResult { value, claims: vec![claim], antenna: None })
    })
}


// ---------------------------------------------------------------------------
// What things are made of, from a photo

/// Hand-checked share of top guesses that were right, on photos not used to tune
/// the prompts. Part of every answer, because a guess shown bare reads as a fact.
const SITE_ACCURACY: &str = "About 60 % of top guesses were right on held-out photos, so treat these as a starting prior and check them.";
const PRODUCT_ACCURACY: &str = "About 30 % of top guesses were right on held-out photos, and water-based contents were never recognised. Confirm what the product is before relying on this.";
/// Below this the answer is "don't know": no class is a catch-all for what nothing fits.
/// Chance over seven or eight classes is about 13 %; this is a little over twice that.
const UNKNOWN_BELOW: f32 = 0.3;

pub fn guess_materials_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    use crate::agent::vision::{materials, models};
    let path = i["path"].as_str().context("give path: a photo")?.to_string();
    let product = i["kind"].as_str() == Some("product");
    let use_sam = i["use_sam"].as_bool().unwrap_or(false);
    let labels: Vec<String> = i["labels"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
    let mut boxes: Vec<(String, [f32; 4])> = Vec::new();
    for b in i["boxes"].as_array().map_or(&[][..], |a| a.as_slice()) {
        let v: Vec<f32> = b.as_array().map_or(Vec::new(), |a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect());
        if v.len() != 4 {
            bail!("each box is [x0, y0, x1, y1], from 0 to 1 across the photo");
        }
        boxes.push(("box".to_string(), [v[0], v[1], v[2], v[3]]));
    }
    if boxes.is_empty() && labels.is_empty() {
        bail!("give labels to look for (e.g. [\"pallet rack\", \"column\"]) or boxes");
    }
    let threshold = num(i, "threshold", 0.3) as f32;
    Ok(move || {
        let img = image::open(&path).with_context(|| format!("reading {path}"))?.to_rgb8();
        let m = models::models();
        // One pass per label: labels sharing a prompt compete, and most then fall under the threshold.
        for label in &labels {
            let found = models::with(&m.detector, models::Detector::load, |d| d.run(&img, std::slice::from_ref(label), threshold, threshold * 0.8))?;
            boxes.extend(found.into_iter().map(|d| (d.label, d.bbox)));
        }
        let prompts = if product { materials::product_contents() } else { materials::site_materials() };
        let just_boxes: Vec<[f32; 4]> = boxes.iter().map(|b| b.1).collect();
        let guesses = materials::classify(&img, &just_boxes, &prompts, use_sam)?;
        let list: Vec<Value> = boxes
            .iter()
            .zip(&guesses)
            .map(|((label, b), g)| {
                let sure = g.confidence >= UNKNOWN_BELOW;
                let mut top = g.scores.clone();
                top.sort_by(|a, b| b.1.total_cmp(&a.1));
                top.truncate(3);
                json!({ "label": label, "box": b, "guess": if sure { g.class.as_str() } else { "unknown" }, "confidence": r3(g.confidence as f64), "best_candidates": top, "outlined_with_sam": g.masked })
            })
            .collect();
        let value = json!({ "found": list, "evidence": evidence_note(Evidence::Estimate), "how_far_to_trust_this": if product { PRODUCT_ACCURACY } else { SITE_ACCURACY }, "next": if product { "Pick the contents yourself and pass them to design_tag." } else { "If a guess looks right, apply it with site_set_material. Nothing was changed." } });
        Ok(JobResult { value, claims: Vec::new(), antenna: None })
    })
}

// ---------------------------------------------------------------------------
// RFID tunnel

pub fn tunnel_advice(i: &Value, belt_mps: Option<f64>) -> Result<JobResult> {
    let id = i["contents"].as_str().context("give contents: one of dry_goods, cardboard, plastic, water_based, oily, frozen, metal_can, foil_lined")?;
    let c = tagging::contents(id).with_context(|| format!("unknown contents '{id}'"))?;
    let d = tunnel::TunnelSpec::default();
    let count = |k: &str, v: usize| i[k].as_u64().map_or(v, |x| x as usize);
    let etsi = i["region"].as_str() == Some("etsi");
    let spec = tunnel::TunnelSpec {
        belt_mps: belt_mps.unwrap_or_else(|| num(i, "belt_mps", d.belt_mps)),
        carton_length_m: num(i, "carton_length_m", d.carton_length_m),
        gap_m: num(i, "gap_m", d.gap_m),
        zone_m: num(i, "zone_m", d.zone_m),
        tags_per_carton: count("tags_per_carton", d.tags_per_carton),
        reader_reads_per_s: num(i, "reads_per_s", d.reader_reads_per_s),
        antennas: count("antennas", d.antennas),
        antenna_distance_m: num(i, "antenna_distance_m", d.antenna_distance_m),
        eirp_dbm: num(i, "eirp_dbm", if etsi { 35.0 } else { d.eirp_dbm }),
        f: if etsi { 866.5e6 } else { d.f },
        wall_m: d.wall_m,
    };
    let min_read = num(i, "min_read", 0.99);
    let (er, tan_d) = ((c.er.0 + c.er.1) / 2.0, (c.tan_d.0 + c.tan_d.1) / 2.0);
    let mut rows: Vec<_> = tagging::TAGS
        .iter()
        .map(|t| (t, tunnel::evaluate(&spec, t, er, tan_d, c.metal), tunnel::max_belt_speed(&spec, t, er, tan_d, c.metal, min_read), tunnel::predict(&spec, t, c, min_read, 200)))
        .collect();
    rows.sort_by(|a, b| b.3.prediction.median.total_cmp(&a.3.prediction.median));
    let claims = rows.first().map(|r| Claim::from_report(&format!("tunnel_{id}_{}", key_of(i)), "rfid_tunnel", &format!("{} through a tunnel, {} at {:.2} m/s", c.label, r.0.label, spec.belt_mps), &r.3, Some(Target::at_least(min_read)), tunnel::assumptions().into_iter().chain(tunnel::warnings(r.0, &spec, c.metal)).collect()).without_pass_chance()).into_iter().collect();
    let value = json!({
        "contents": c.label,
        "belt_mps": r3(spec.belt_mps),
        "belt_from_conveyor": belt_mps.is_some(),
        "cartons_per_min": rows.first().map(|r| r3(r.1.cartons_per_min)),
        "cartons_can_mix_in_the_field_share": rows.first().map(|r| r3(r.1.overlap)),
        "options": rows.iter().map(|(t, s, max, r)| json!({
            "tag": t.label,
            "tags_read": r3(r.prediction.median),
            "low": r3(r.prediction.low),
            "high": r3(r.prediction.high),
            "whole_carton_read": r3(s.carton_complete),
            "fastest_belt_mps": max.map(r3),
            "limited_by": match max {
                None => "the link: a slower belt won't help",
                Some(v) if *v >= 9.99 => "nothing up to 10 m/s, as far as this looks",
                Some(_) => "time in the field",
            },
            "main_driver": r.clear_drivers().first().map(|m| m.factor.clone()),
            "outside_the_fit": tunnel::warnings(t, &spec, c.metal),
        })).collect::<Vec<_>>(),
        "evidence": "Estimate: not checked against a real tunnel, so test the real carton mix before committing a speed",
    });
    Ok(JobResult { value, claims, antenna: None })
}

// ---------------------------------------------------------------------------
// A medical device among the hospital's radios

pub fn medical_advice(i: &Value, from_scene: Vec<medical::Neighbour>) -> Result<JobResult> {
    let preset = |n: &str| coexist::Radio::preset(n).with_context(|| format!("no preset '{n}'; there are {}", coexist::Radio::presets().iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", ")));
    let link = medical::DeviceLink { radio: preset(i["radio"].as_str().unwrap_or("BLE"))?, margin_db: num(i, "margin_db", 15.0), packets_per_s: num(i, "packets_per_s", 10.0), packet_s: num(i, "packet_ms", 2.0) / 1000.0, max_gap_s: num(i, "max_gap_s", 1.0) };
    let mut neighbours = Vec::new();
    if let Some(list) = i["neighbours"].as_array() {
        for n in list {
            let radio = preset(n["radio"].as_str().context("each neighbour needs a radio, by preset name")?)?;
            neighbours.push(medical::Neighbour { name: n["name"].as_str().unwrap_or(&radio.name).to_string(), distance_m: num(n, "distance_m", 3.0), count: n["count"].as_u64().map_or(1, |v| v as usize), extra_loss_db: num(n, "extra_loss_db", 0.0), burst_s: num(n, "burst_ms", 2.0) / 1000.0, radio });
        }
    }
    if i["typical"].as_bool().unwrap_or(neighbours.is_empty() && from_scene.is_empty()) {
        neighbours.extend(medical::typical_hospital());
    }
    neighbours.extend(from_scene);
    if neighbours.is_empty() {
        bail!("no networks to test against: give neighbours, a position in a scene with devices, or leave typical on");
    }
    let max_gaps = num(i, "max_gaps_per_hour", 0.1);
    let mut findings: Vec<medical::Finding> = neighbours.iter().map(|n| medical::assess(&link, n)).collect();
    let chance = medical::failure_chance(&link, &findings);
    let gaps = medical::gaps_per_hour(&link, &findings);
    findings.sort_by(|a, b| b.packet_loss.total_cmp(&a.packet_loss).then(b.desense_db.total_cmp(&a.desense_db)));
    let report = medical::predict(&link, &neighbours, max_gaps, i["samples"].as_u64().map_or(200, |v| v as usize));
    let claim = Claim::from_report(&format!("medical_{}", key_of(i)), "medical_coexistence", &format!("{} device among {} networks", link.radio.name, neighbours.len()), &report, Some(Target::at_most(max_gaps)), medical::assumptions()).without_pass_chance();
    let value = json!({
        "device": { "radio": link.radio.name, "margin_db": link.margin_db, "packets_per_s": link.packets_per_s, "packet_ms": link.packet_s * 1000.0, "max_gap_s": link.max_gap_s, "given": i["margin_db"].is_number() && i["packets_per_s"].is_number() && i["max_gap_s"].is_number() },
        "as_placed": { "too_long_gaps_per_hour": gaps, "chance_per_window": chance },
        "target_gaps_per_hour": max_gaps,
        "target_given": i["max_gaps_per_hour"].is_number(),
        "range": { "median": report.prediction.median, "low": report.prediction.low, "high": report.prediction.high },
        "main_driver": report.clear_drivers().first().map(|m| m.factor.clone()),
        "test_first": findings.iter().take(5).map(|f| json!({ "network": f.neighbour, "isolation_db": r3(f.isolation_db), "sensitivity_lost_db": r3(f.desense_db), "blocked": f.blocked, "on_air_during_a_packet": r3(f.overlap), "packets_lost": r3(f.packet_loss) })).collect::<Vec<_>>(),
        "networks": findings.len(),
        "evidence": "Estimate for planning tests. It doesn't show coexistence: that takes ANSI C63.27 testing and a risk analysis under AAMI TIR69",
    });
    Ok(JobResult { value, claims: vec![claim], antenna: None })
}

// ---------------------------------------------------------------------------
// SAR pre-check

/// A slab of tissue beside the antenna, on the + side of `axis`.
#[derive(Clone, Debug)]
pub struct SarAsk {
    pub axis: usize,
    /// From the antenna's outer surface to the tissue, metres.
    pub gap_m: f64,
    pub tissue: String,
    /// Along the next axis, the one after, and into the tissue. `None`: sized from the antenna.
    pub size: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct SarSolved {
    /// 1 g and 10 g peak averages and the highest point, W/kg per watt reaching the antenna.
    pub g1: f64,
    pub g10: f64,
    pub point: f64,
    pub accepted_share: f64,
    pub s11_db: f64,
    /// Where the match is best with the body there, Hz.
    pub best_f: f64,
    pub seconds: f64,
    pub warnings: Vec<String>,
}

/// |E|² in the tissue in one plane holding the tissue's normal `n` and another axis `u`,
/// per watt incident, with the surface itself added by continuing the decay inwards: the
/// sample right on it mixes in the field outside.
#[cfg(feature = "antenna-studio")]
fn tissue_plane(dir: &std::path::Path, f: f64, per_watt: f64, n: usize, u_axis: usize, min: [f64; 3], max: [f64; 3]) -> Result<(sar::PlaneField, f64)> {
    let fixed = 3 - n - u_axis;
    let name = ["E_yz", "E_xz", "E_xy"][fixed];
    let g = crate::rtls::fdtd::post::read_vtr(&crate::rtls::fdtd::post::fd_file(dir, name, f, "abs"))?;
    let axes = [&g.x, &g.y, &g.z];
    let len = [g.x.len(), g.y.len(), g.z.len()];
    if len[fixed] != 1 {
        bail!("{name} is not a plane");
    }
    let stride = [1, len[0], len[0] * len[1]];
    let value = |iu: usize, iv: usize| -> f64 { g.at(iu * stride[u_axis] + iv * stride[n]).iter().map(|v| (*v as f64).powi(2)).sum::<f64>() * per_watt };
    let within = |k: usize| -> Vec<usize> { (0..len[k]).filter(|i| axes[k][*i] > min[k] + 1e-9 && axes[k][*i] < max[k] - 1e-9).collect() };
    let (us, vs) = (within(u_axis), within(n));
    if us.len() < 2 || vs.len() < 2 {
        bail!("{name}: the mesh has too few lines inside the tissue");
    }
    let face = min[n];
    let (v1, v2) = (axes[n][vs[0]], axes[n][vs[1]]);
    let mut v = vec![face];
    v.extend(vs.iter().map(|k| axes[n][*k]));
    let e2 = us
        .iter()
        .map(|iu| {
            let (a, b) = (value(*iu, vs[0]), value(*iu, vs[1]));
            let surface = if a > 0.0 && b > 0.0 { (a * ((a / b).ln() * (v1 - face) / (v2 - v1)).exp()).min(4.0 * a) } else { a };
            let mut col = vec![surface];
            col.extend(vs.iter().map(|iv| value(*iu, *iv)));
            col
        })
        .collect();
    let u: Vec<f64> = us.iter().map(|k| axes[u_axis][*k]).collect();
    let inside = vec![vec![true; v.len()]; u.len()];
    Ok((sar::PlaneField { u, v, e2, inside }, axes[fixed][0]))
}

#[cfg(feature = "antenna-studio")]
pub fn sar_solve(p: &Project, ask: &SarAsk) -> Result<SarSolved> {
    use crate::antenna::run::{JobCtx, RunOptions, simulate};
    let f = p.f0();
    let (lo, hi) = p.geometry()?.bounds();
    let tissue = antenna_core::material::by_name(&ask.tissue).with_context(|| format!("no material '{}'", ask.tissue))?;
    let sigma = tissue.dielectric_kappa(f);
    let lambda = antenna_core::C0 / f;
    let n = ask.axis;
    let (a, b) = ((n + 1) % 3, (n + 2) % 3);
    let (mut min, mut max) = ([0.0; 3], [0.0; 3]);
    for (j, k) in [a, b].into_iter().enumerate() {
        let centre = (lo[k] + hi[k]) / 2.0;
        let half = ask.size.map_or((hi[k] - lo[k]) / 2.0 + lambda / 3.0, |s| s[j] / 2.0);
        min[k] = centre - half;
        max[k] = centre + half;
    }
    min[n] = hi[n] + ask.gap_m;
    max[n] = min[n] + ask.size.map_or((lambda * 0.45).min(0.15), |s| s[2]);
    let t = |v: [f64; 3]| [format!("{:e}", v[0]), format!("{:e}", v[1]), format!("{:e}", v[2])];
    let mut q = integrate::with_parts(p, &[integrate::Part { name: "phantom".into(), material: ask.tissue.clone(), min: t(min), max: t(max) }]);
    q.setup.solver = antenna_core::project::SolverKind::Fdtd;
    q.setup.f_lo = f * 0.8;
    q.setup.f_hi = f * 1.2;
    q.setup.far_freqs = Vec::new();
    q.setup.field_planes = true;
    let z0 = q.geometry()?.ports.first().context("the antenna has no port")?.z0;
    let ctx = JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
    let r: SimResult = simulate(&q, &ctx, &RunOptions { keep: true, ..Default::default() })?;
    let run = std::path::PathBuf::from(&r.dir);
    let read = || -> Result<SarSolved> {
        let dir = run.join("p1");
        let inc = crate::rtls::fdtd::post::port_fd(&dir, 1, z0, &[f])?.inc[0];
        let per_watt = 1.0 / (0.5 * inc.norm2() / z0).max(1e-300);
        let (pa, fixed_a) = tissue_plane(&dir, f, per_watt, n, a, min, max)?;
        let (pb, fixed_b) = tissue_plane(&dir, f, per_watt, n, b, min, max)?;
        let k = r.network.f.iter().enumerate().min_by(|x, y| (x.1 - f).abs().total_cmp(&(y.1 - f).abs())).map_or(0, |e| e.0);
        let sweep = r.network.sij_db(0, 0);
        let s11_db = sweep[k];
        let best_f = sweep.iter().enumerate().min_by(|x, y| x.1.total_cmp(y.1)).map_or(f, |e| r.network.f[e.0]);
        let accepted_share = (1.0 - 10f64.powf(s11_db / 10.0)).max(1e-3);
        let _ = fixed_a;
        let cube = |side: f64| -> Result<(f64, f64)> {
            let step = side / 20.0;
            let (ra, rb) = (sar::resampled(&pa, step), sar::resampled(&pb, step));
            let (sa, point_a) = sar::peak_average(&ra, sigma, side).context("the tissue is too small for the averaging mass")?;
            let (sb, point_b) = sar::peak_average(&rb, sigma, side).context("the tissue is too small for the averaging mass")?;
            let line = sar::peak_line(&ra, sigma, fixed_b, side).unwrap_or(0.0);
            Ok((sar::cube_from_planes(sa, sb, line), point_a.max(point_b)))
        };
        let (g1, point) = cube(sar::cube_side_m(1.0))?;
        let (g10, _) = cube(sar::cube_side_m(10.0))?;
        Ok(SarSolved { g1: g1 / accepted_share, g10: g10 / accepted_share, point: point / accepted_share, accepted_share, s11_db, best_f, seconds: r.seconds, warnings: r.warnings.clone() })
    };
    let out = read();
    let _ = std::fs::remove_dir_all(&run);
    out
}

#[cfg(not(feature = "antenna-studio"))]
pub fn sar_solve(_: &Project, _: &SarAsk) -> Result<SarSolved> {
    bail!("this needs the FDTD solver from Antenna Studio, left out of this build")
}

pub fn sar_job(i: &Value) -> Result<impl FnOnce() -> Result<JobResult> + Send + 'static> {
    let p = project_from(i)?;
    let f = p.f0();
    let (lo, hi) = p.geometry()?.bounds();
    // The body lies against the antenna's flat side; a wire has no flat side, so +y.
    let axis = match i["axis"].as_str() {
        Some("x") => 0,
        Some("y") => 1,
        Some("z") => 2,
        Some(o) => bail!("axis '{o}': give x, y or z"),
        None => [1usize, 0, 2].into_iter().min_by(|a, b| (hi[*a] - lo[*a]).total_cmp(&(hi[*b] - lo[*b]))).unwrap_or(1),
    };
    let tissue = i["tissue"].as_str().map_or_else(|| if f < 1.5e9 { "SAR tissue 900 MHz".to_string() } else { "SAR tissue 2450 MHz".to_string() }, str::to_string);
    let ask = SarAsk { axis, gap_m: num(i, "gap_mm", 5.0) / 1000.0, tissue: tissue.clone(), size: None };
    let power = num(i, "power_mw", 100.0) / 1000.0;
    let key = key_of(i);
    let template = i["template"].as_str().unwrap_or("antenna").to_string();
    let from_band = (f < 0.6e9 || (f > 1.2e9 && f < 1.8e9) || f > 3.2e9) && i["tissue"].is_null();
    Ok(move || {
        let s = sar_solve(&p, &ask)?;
        if !(s.g1.is_finite() && s.g10.is_finite() && s.g1 > 0.0 && s.g10 > 0.0) {
            bail!("the run gave no usable field in the tissue, so there's no SAR to report");
        }
        let checks = [sar::check(s.g1, power, sar::FCC_1G), sar::check(s.g10, power, sar::ICNIRP_10G)];
        let mut assumptions = sar::assumptions();
        assumptions.push(format!("{} mW average reaching the antenna, {:.0} mm from {tissue}", power * 1000.0, ask.gap_m * 1000.0));
        if from_band {
            assumptions.push(format!("The tissue values are for another band than {:.2} GHz: give a tissue material for this frequency", f / 1e9));
        }
        let claim = |grams: &str, c: &sar::SarCheck| exact_claim(&format!("sar_{grams}_{key}"), "sar_precheck", &format!("{template}, SAR over {grams}, {:.0} mW at {:.0} mm", power * 1000.0, ask.gap_m * 1000.0), Prediction::exact(&format!("SAR over {grams}"), "W/kg", c.sar_w_per_kg, Evidence::Simulated), Some(Target::at_most(c.limit.w_per_kg)), assumptions.clone());
        let claims = vec![claim("1 g", &checks[0]), claim("10 g", &checks[1])];
        let value = json!({
            "per_watt_at_the_antenna": { "g1_w_per_kg": r3(s.g1), "g10_w_per_kg": r3(s.g10), "highest_point": r3(s.point) },
            "checks": checks.iter().map(|c| json!({ "limit": c.limit.name, "limit_w_per_kg": c.limit.w_per_kg, "sar_w_per_kg": r3(c.sar_w_per_kg), "share_of_limit": r3(c.share_of_limit), "most_power_mw": r3(c.max_power_w * 1000.0) })).collect::<Vec<_>>(),
            "antenna_with_the_body_there": { "s11_db": r3(s.s11_db), "power_accepted_share": r3(s.accepted_share), "best_match_ghz": r3(s.best_f / 1e9) },
            "seconds": r3(s.seconds),
            "warnings": s.warnings,
            "evidence": "Simulated: a design-stage check. Only an accredited lab's measurement can show compliance",
            "assumptions": assumptions,
        });
        Ok(JobResult { value, claims, antenna: None })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "antenna-studio")]
    #[test]
    fn a_recipe_refuses_a_run_that_was_cut_off() {
        let cut = SimResult { warnings: vec![format!("{} of 1500 before the energy decayed", antenna_core::project::STOPPED_SHORT)], ..Default::default() };
        assert!(usable(cut).unwrap_err().to_string().contains("step limit"));
        let advisory = SimResult { warnings: vec!["Copper is lossless on this wire".into()], ..Default::default() };
        assert!(usable(advisory).is_ok());
    }

    // OUT-M3: NaN and infinity came out as 0, which reads as 0 cm, 0 dB or 0 % yield.
    #[test]
    fn a_failed_number_prints_as_null_not_zero() {
        assert_eq!(json!(r3(f64::NAN)), Value::Null);
        assert_eq!(json!(r3(f64::INFINITY)), Value::Null);
        assert_eq!(json!(r3(1.23456)), json!(1.235));
        assert_eq!(metrics_json(&uwb_bias::BiasMetrics { p95_abs_m: f64::NAN, ..Default::default() })["p95_cm"], Value::Null);
    }

    // OUT-B12: these all became "doesn't vary", and so 100 % yield.
    #[test]
    fn a_missing_or_wrong_tolerance_is_refused() {
        let with = |t: Value| parse_tolerances(&json!({ "tolerances": t }));
        let (names, tol) = with(json!([{ "name": "L", "sigma_m": 1e-4 }, { "name": "W", "sigma_m": 0.0 }])).unwrap();
        assert_eq!((names, tol), (vec!["L".to_string(), "W".to_string()], vec![1e-4, 0.0]));
        for (bad, says) in [
            (json!([{ "name": "L", "sigma": 1e-4 }]), "sigma_m must be a number"),
            (json!([{ "name": "L", "sigma_m": "0.1mm" }]), "sigma_m must be a number"),
            (json!([{ "name": "L", "sigma_m": -1e-4 }]), "can't be"),
            (json!([{ "name": "L", "sigma_m": 0.0 }]), "nothing varies"),
            (json!([{ "sigma_m": 1e-4 }]), "needs a name"),
            (json!([]), "at least one"),
        ] {
            let e = with(bad).unwrap_err().to_string();
            assert!(e.contains(says), "{e}");
        }
    }

    // OUT-B10: one solve meeting its target read "Likely", though the enum says "across
    // nearly the whole range".
    #[test]
    fn one_solve_that_meets_its_target_says_only_that() {
        let of = |e: Evidence, v: f64| exact_claim("a", "r", "s", Prediction::exact("m", "dB", v, e), Some(Target::at_most(1.0)), vec![]).verdict;
        assert_eq!(of(Evidence::Simulated, 0.5), Verdict::MeetsOnce);
        assert_eq!(of(Evidence::Simulated, 2.0), Verdict::Unlikely);
        assert_eq!(of(Evidence::Estimate, 0.5), Verdict::WorthTesting);
        assert_eq!(of(Evidence::Measured, 0.5), Verdict::Likely);
        assert_eq!(of(Evidence::Simulated, f64::NAN), Verdict::Unlikely, "no number has met nothing");
    }

    // OUT-B10, OUT-M7: design_tag could show a green "Likely (100 %)".
    #[test]
    fn a_tag_estimate_is_only_worth_testing_and_quotes_no_chance() {
        let r = tag_advice(&json!({ "contents": "cardboard", "target_range_m": 1.0 })).unwrap();
        let c = &r.claims[0];
        assert_eq!(c.verdict, Verdict::WorthTesting);
        assert_eq!(c.pass_probability, None);
        assert_eq!(c.verdict_text(), "Worth testing");
        assert!(r.value["options"][0].get("chance_of_target").is_none(), "{}", r.value);
        assert!(r.value["options"][0]["outside_the_fit"].as_array().is_some_and(|w| !w.is_empty()), "{}", r.value);
        assert!(c.assumptions.iter().any(|a| a.contains("assumed model error")), "{:?}", c.assumptions);
        assert!(r.value["evidence"].as_str().unwrap().starts_with("Estimate"));
    }

    // OUT-M2
    #[test]
    fn the_bias_claim_says_how_many_directions_it_speaks_for() {
        let half = uwb_bias::BiasMetrics { p95_abs_m: 0.02, usable_share: 0.5, ..Default::default() };
        let c = uwb_bias_claim("a", "x", &half, false, Some(5.0));
        assert_eq!(c.prediction.metric, "48 % of directions within");
        assert_eq!(c.verdict, Verdict::Uncertain, "met over half the directions isn't met");
        assert!(c.assumptions.iter().any(|a| a.contains("50 %") && a.contains("nulls")), "{:?}", c.assumptions);
        assert!(c.assumptions.iter().any(|a| a.contains("twice")), "{:?}", c.assumptions);
        assert!(c.assumptions[0].starts_with("Metal only"));
        assert!(!uwb_bias_claim("a", "x", &half, true, None).assumptions[0].contains("Metal only"), "the FDTD path said it too");
        let whole = uwb_bias::BiasMetrics { p95_abs_m: 0.02, usable_share: 1.0, ..Default::default() };
        assert_eq!(uwb_bias_claim("a", "x", &whole, false, Some(5.0)).verdict, Verdict::MeetsOnce);
        let none = uwb_bias::BiasMetrics { p95_abs_m: f64::NAN, usable_share: 0.0, ..Default::default() };
        let c = uwb_bias_claim("a", "x", &none, false, Some(5.0));
        assert_eq!(c.verdict, Verdict::Unlikely, "it used to be 0 cm and Likely");
        assert!(c.prediction.text().contains("n/a"), "{}", c.prediction.text());
    }

    // OUT-M5: with a plastic shell, "before" was MoM and "after" FDTD.
    #[test]
    fn both_sides_of_an_integration_use_one_solver() {
        let bare = Project::from_template("strip_dipole", Some("2.45GHz")).unwrap();
        let metal = integrate::with_parts(&bare, &[integrate::Part { name: "plate".into(), material: "PEC".into(), min: ["0.01".into(), "-0.01".into(), "-0.01".into()], max: ["0.01".into(), "0.01".into(), "0.01".into()] }]);
        let shell = integrate::with_parts(&bare, &integrate::shell_around(&bare, "ABS plastic", 0.002, 0.002).unwrap());
        assert!(!needs_fdtd(&[&bare, &metal]).unwrap(), "all metal: the method of moments for both");
        assert!(!needs_fdtd(&[&bare]).unwrap(), "on its own the bare antenna would pick the other solver");
        assert!(needs_fdtd(&[&bare, &shell]).unwrap());
    }

    #[test]
    fn a_coexistence_result_labels_itself_and_says_what_it_leaves_out() {
        let r = coexistence(&json!({ "radios": ["UHF RFID reader", "UWB channel 5"], "isolation_db": 40.0 })).unwrap();
        assert!(r.value["evidence"].as_str().unwrap().contains("not validated"), "{}", r.value);
        let pair = r.value["pairs"].as_array().unwrap().iter().find(|p| p["hurt"] == "UWB channel 5").unwrap();
        assert!(pair["desense_db"].as_f64().unwrap() > 10.0, "the harmonic is in the number: {pair}");
        assert!(pair["other_radio_on_air_share"].as_f64().unwrap() > 0.0);
        assert!(r.claims[0].assumptions.iter().any(|a| a.contains("no source")), "{:?}", r.claims[0].assumptions);
    }
}

/// Real openEMS runs that check what the quick models claim:
/// `cargo test --features antenna-studio --bin SenseSim fdtd_checks -- --ignored --nocapture`
#[cfg(all(test, feature = "antenna-studio"))]
mod fdtd_checks {
    use super::*;
    use antenna_core::recipes::integrate::Part;

    fn slab(name: &str, material: &str, min: [f64; 3], max: [f64; 3]) -> Part {
        let t = |v: [f64; 3]| [format!("{:e}", v[0]), format!("{:e}", v[1]), format!("{:e}", v[2])];
        Part { name: name.into(), material: material.into(), min: t(min), max: t(max) }
    }

    #[test]
    #[ignore = "real FDTD runs, a few minutes"]
    fn fdtd_plastic_enclosure_detunes_a_dipole() {
        let p = Project::from_template("strip_dipole", Some("2.45GHz")).unwrap();
        let (lo, hi) = (1.6e9, 3.2e9);
        let free = solve_fdtd(&p, lo, hi).unwrap();
        let shell = integrate::shell_around(&p, "ABS plastic", 0.002, 0.003).unwrap();
        let cased = solve_fdtd(&integrate::with_parts(&p, &shell), lo, hi).unwrap();
        let d = integrate::compare(&free, &cased, 0, 2.4e9, 2.5e9);
        println!("{d:#?}");
        assert!(d.shift_pct.unwrap() < -0.5, "plastic must pull the resonance down: {d:?}");
    }

    /// A reference dipole on a flat block of tissue, tuned to resonance as the real ones are
    /// (better than -20 dB). FDTD runs a thin wire whatever radius is set, so the real
    /// dipole's length doesn't resonate here: the first run finds where it does, and the
    /// second is the one compared. Against the real one's 1 W forward power, which with
    /// that match is about 1 W accepted.
    fn reference_dipole(f0: &str, length_m: f64, gap_m: f64, tissue: &str, size: [f64; 3]) -> SarSolved {
        let solve = |length: f64| {
            let mut p = Project::from_template("dipole", Some(f0)).unwrap();
            for (k, v) in [("L", format!("{length:e}")), ("a", "1.8mm".into()), ("g", "1mm".into()), ("Z0", "50".into())] {
                p.params.iter_mut().find(|r| r.name == k).unwrap().expr = v;
            }
            let (_, hi) = p.geometry().unwrap().bounds();
            sar_solve(&p, &SarAsk { axis: 1, gap_m: gap_m - hi[1], tissue: tissue.into(), size: Some(size) }).unwrap()
        };
        let f = Project::from_template("dipole", Some(f0)).unwrap().f0();
        let first = solve(length_m);
        println!("as built, {:.1} mm: S11 {:.1} dB, best at {:.3} GHz", length_m * 1e3, first.s11_db, first.best_f / 1e9);
        let tuned = length_m * first.best_f / f;
        let s = solve(tuned);
        println!("tuned, {:.1} mm: S11 {:.1} dB, best at {:.3} GHz", tuned * 1e3, s.s11_db, s.best_f / 1e9);
        s
    }

    /// The check that matters most. IEEE 1528's targets, as far as they could be checked
    /// without the standard to hand, are about 10.8 W/kg over 1 g and 6.9 over 10 g at
    /// 900 MHz, and real dipole certificates list about 11 and 7. The band was set before
    /// the first run: 20 %, about a SAR lab's own measurement uncertainty.
    #[test]
    #[ignore = "real FDTD runs, tens of minutes"]
    fn fdtd_sar_reference_dipole() {
        let s = reference_dipole("900MHz", 0.149, 0.015, "SAR tissue 900 MHz", [0.36, 0.30, 0.15]);
        println!("{s:#?}\n1 g {:.2} W/kg against 10.8, 10 g {:.2} W/kg against 6.9", s.g1, s.g10);
        assert!(s.s11_db < -10.0, "not tuned: {}", s.s11_db);
        assert!((s.g1 / 10.8 - 1.0).abs() <= 0.2, "1 g: {}", s.g1);
        assert!((s.g10 / 6.9 - 1.0).abs() <= 0.2, "10 g: {}", s.g10);
    }

    /// The same at 2450 MHz, as an independent second point: 51.5 mm, 10 mm from the tissue,
    /// targets about 52.4 W/kg over 1 g and 24 over 10 g, with the same caveat and band.
    #[test]
    #[ignore = "real FDTD runs, tens of minutes"]
    fn fdtd_sar_reference_dipole_2450() {
        let s = reference_dipole("2450MHz", 0.0515, 0.010, "SAR tissue 2450 MHz", [0.20, 0.16, 0.08]);
        println!("{s:#?}\n1 g {:.2} W/kg against 52.4, 10 g {:.2} W/kg against 24", s.g1, s.g10);
        assert!(s.s11_db < -10.0, "not tuned: {}", s.s11_db);
        assert!((s.g1 / 52.4 - 1.0).abs() <= 0.2, "1 g: {}", s.g1);
        assert!((s.g10 / 24.0 - 1.0).abs() <= 0.2, "10 g: {}", s.g10);
    }

    /// What a block behind a strip dipole does: impedance, efficiency and gain.
    fn report(label: &str, p: &Project, f_lo: f64, f_hi: f64) {
        use crate::antenna::run::{JobCtx, RunOptions, simulate};
        let mut q = p.clone();
        q.setup.f_lo = f_lo;
        q.setup.f_hi = f_hi;
        q.setup.solver = antenna_core::project::SolverKind::Fdtd;
        q.setup.far_freqs = vec![915e6];
        q.setup.far_step = 15.0;
        q.setup.field_planes = false;
        let ctx = JobCtx { live: Default::default(), cancel: Default::default(), children: Default::default() };
        let r = simulate(&q, &ctx, &RunOptions { keep: false, ..Default::default() }).unwrap();
        let z = r.network.zin(0);
        let at = |f: f64| r.network.f.iter().enumerate().min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs())).map_or(0, |e| e.0);
        let zero: Vec<f64> = (1..z.len()).filter(|&k| z[k - 1].im < 0.0 && z[k].im >= 0.0).map(|k| r.network.f[k] / 1e6).collect();
        let m = r.far[0][0].metrics();
        println!("{label}: X=0 rising at {zero:.0?} MHz | Z(915) {:.1} | R at X=0 {:?} | eff {:.3} | peak gain {:.2} dBi | {:.0} s", z[at(915e6)], zero.iter().map(|f| z[at(f * 1e6)].re.round()).collect::<Vec<_>>(), m.rad_efficiency, m.peak_gain_dbi, r.seconds);
    }

    #[test]
    #[ignore = "real FDTD runs, tens of minutes"]
    fn fdtd_water_behind_a_tag_against_the_quick_estimate() {
        // A half-wave strip at 915 MHz with a block of water behind it at several gaps.
        let p = Project::from_template("strip_dipole", Some("915MHz")).unwrap();
        let (lo0, hi0) = p.geometry().unwrap().bounds();
        let (f_lo, f_hi) = (300e6, 1.3e9);
        report("free    ", &p, f_lo, f_hi);
        for gap in [0.002, 0.005, 0.01, 0.02] {
            let (mut a, mut b) = ([0.0; 3], [0.0; 3]);
            for k in 0..3 {
                a[k] = lo0[k] - 0.02;
                b[k] = hi0[k] + 0.02;
            }
            a[1] = gap;
            b[1] = gap + 0.04;
            report(&format!("water {:>2.0}mm", gap * 1e3), &integrate::with_parts(&p, &[slab("water", "Water", a, b)]), f_lo, f_hi);
        }
    }
    #[test]
    #[ignore = "real FDTD runs, tens of minutes"]
    fn fdtd_other_contents_behind_a_tag() {
        // Water alone can't separate reflection from loss: one very lossy, one nearly lossless.
        let p = Project::from_template("strip_dipole", Some("915MHz")).unwrap();
        let (lo0, hi0) = p.geometry().unwrap().bounds();
        let (f_lo, f_hi) = (300e6, 1.3e9);
        for material in ["Muscle tissue", "Polycarbonate"] {
            for gap in [0.005, 0.02] {
                let (mut a, mut b) = ([0.0; 3], [0.0; 3]);
                for k in 0..3 {
                    a[k] = lo0[k] - 0.02;
                    b[k] = hi0[k] + 0.02;
                }
                a[1] = gap;
                b[1] = gap + 0.04;
                report(&format!("{material} {:>2.0}mm", gap * 1e3), &integrate::with_parts(&p, &[slab("block", material, a, b)]), f_lo, f_hi);
            }
        }
    }
}
