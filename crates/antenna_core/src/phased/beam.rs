//! Beamforming: from where the beams should go to what each element is fed. Tapers, phase
//! shifters with finite bits, attenuator steps, true time delay, subarrays with one converter
//! each, several beams at once, nulls, adaptive weights, Butler matrices and shaped beams.
//!
//! Weights multiply and add: `y = Σ wₙ·xₙ` on receive, the same numbers on transmit, by
//! reciprocity. An element's response to direction `d` is `aₙ(d) = exp(+jk·d·rₙ)`, so a beam
//! at `d` has `wₙ = exp(−jk·d·rₙ)`. In textbook `wᴴx` notation these are the conjugates.

use std::f64::consts::TAU;

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::aperture::{Layout, unit_dir};
use super::math::{CMat, ZERO, herm, identity, inner, mat_mul, mat_vec, norm_sqr, quadratic, zeros};
use super::pattern::ArrayModel;
use crate::array::{Taper, taper_weights};
use crate::geometry::{P3, dot};
use crate::linalg::invert;
use crate::{C0, C64, db10};

/// One beam: a direction and its share of the power.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beam {
    pub theta: f64,
    pub phi: f64,
    pub share: f64,
}

impl Default for Beam {
    fn default() -> Self {
        Self { theta: 0.0, phi: 0.0, share: 1.0 }
    }
}

/// Where the frequency dependence of the steering goes. Phase shifters hold their phase across
/// the band, so the beam squints; a delay keeps the wavefront straight at every frequency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delay {
    #[default]
    PhaseOnly,
    /// A delay line, or a digital delay, per element.
    Element,
    /// A delay per subarray, phase shifters within it.
    Subarray,
}

impl Delay {
    pub const ALL: [Delay; 3] = [Delay::PhaseOnly, Delay::Subarray, Delay::Element];
    pub fn label(self) -> &'static str {
        match self {
            Delay::PhaseOnly => "Phase shifters only",
            Delay::Subarray => "Delay per subarray",
            Delay::Element => "Delay per element",
        }
    }
}

/// How several beams share the array.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MultiBeam {
    /// A digital array: each beam has its own signal and its own weights, and they
    /// don't load each other on receive.
    #[default]
    Digital,
    /// One network and one signal: the weights add, and one pattern has all the beams.
    Shared,
    /// As `Shared`, keeping only the phases, because amplifiers run at one level.
    SharedPhaseOnly,
}

impl MultiBeam {
    pub const ALL: [MultiBeam; 3] = [MultiBeam::Digital, MultiBeam::Shared, MultiBeam::SharedPhaseOnly];
    pub fn label(self) -> &'static str {
        match self {
            MultiBeam::Digital => "Digital, independent beams",
            MultiBeam::Shared => "One network, weights added",
            MultiBeam::SharedPhaseOnly => "One network, phase only",
        }
    }
}

/// What the hardware between the converters and the elements can do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hardware {
    /// Bits of each phase shifter, `None` for continuous.
    pub phase_bits: Option<u8>,
    /// Attenuator step, dB, `None` for continuous.
    pub atten_step_db: Option<f64>,
    pub delay: Delay,
    /// Elements per converter along x and y. `None`: one per element, or a single analog
    /// feed, which behave alike for one beam.
    pub subarray: Option<(usize, usize)>,
    /// Hybrid: where the phase shifters inside each subarray aim. `None`: the beam itself.
    pub analog_aim: Option<(f64, f64)>,
    /// Random amplitude error, rms dB.
    pub amp_err_db: f64,
    /// Random phase error, rms degrees.
    pub phase_err_deg: f64,
    pub seed: u64,
    /// Elements switched off, by index.
    pub failed: Vec<usize>,
}

/// Everything that decides an array's weights, kept with the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeamPlan {
    pub taper_x: Taper,
    pub taper_y: Taper,
    pub beams: Vec<Beam>,
    /// Which beam the pattern views show, in digital mode.
    pub active: usize,
    pub multibeam: MultiBeam,
    /// Directions to put a null in: `(θ, φ)`.
    pub nulls: Vec<(f64, f64)>,
    pub hw: Hardware,
    /// On an array whose elements face different ways, those further than this off the beam
    /// stay dark, degrees.
    pub cone_deg: f64,
}

impl Default for BeamPlan {
    fn default() -> Self {
        Self { taper_x: Taper::Uniform, taper_y: Taper::Uniform, beams: vec![Beam::default()], active: 0, multibeam: MultiBeam::Digital, nulls: Vec::new(), hw: Hardware::default(), cone_deg: 70.0 }
    }
}

/// Each element's subarray, and the middle of each subarray.
fn subarrays(m: &ArrayModel, size: Option<(usize, usize)>) -> (Vec<usize>, Vec<P3>) {
    let Some((sx, sy)) = size else {
        return ((0..m.len()).collect(), m.elements.iter().map(|e| e.pos).collect());
    };
    let (sx, sy) = (sx.max(1), sy.max(1));
    let mut keys: Vec<(usize, usize, usize)> = Vec::new();
    let mut owner = Vec::with_capacity(m.len());
    for e in &m.elements {
        let key = (e.panel, e.ix / sx, e.iy / sy);
        let at = keys.iter().position(|k| *k == key).unwrap_or_else(|| {
            keys.push(key);
            keys.len() - 1
        });
        owner.push(at);
    }
    let mut centers = vec![[0.0; 3]; keys.len()];
    let mut counts = vec![0.0; keys.len()];
    for (e, o) in m.elements.iter().zip(&owner) {
        for k in 0..3 {
            centers[*o][k] += e.pos[k];
        }
        counts[*o] += 1.0;
    }
    for (c, n) in centers.iter_mut().zip(&counts) {
        for v in c.iter_mut() {
            *v /= *n;
        }
    }
    (owner, centers)
}

/// The tapers, sampled per element on its own panel.
fn amplitudes(m: &ArrayModel, plan: &BeamPlan) -> Vec<f64> {
    let mut cache: Vec<((usize, usize), (Vec<f64>, Vec<f64>))> = Vec::new();
    m.elements
        .iter()
        .map(|e| {
            let key = (e.nx, e.ny);
            let at = cache.iter().position(|c| c.0 == key).unwrap_or_else(|| {
                cache.push((key, (taper_weights(plan.taper_x, e.nx), taper_weights(plan.taper_y, e.ny))));
                cache.len() - 1
            });
            let (tx, ty) = &cache[at].1;
            tx[e.ix.min(e.nx - 1)] * ty[e.iy.min(e.ny - 1)]
        })
        .collect()
}

fn quantise(phase: f64, bits: Option<u8>) -> f64 {
    match bits {
        Some(b) => {
            let lsb = TAU / 2f64.powi(b as i32);
            (phase / lsb).round() * lsb
        }
        None => phase,
    }
}

/// One beam's weights before the hardware's rounding and errors, and whether phase shifters are
/// still to round them. The hybrid path rounds its analog part inside, since the digital part
/// that follows isn't rounded by shifters.
fn steered(m: &ArrayModel, plan: &BeamPlan, beam: &Beam, f0: f64, f: f64, flat: bool) -> (Vec<C64>, bool) {
    let (k0, k) = (TAU * f0 / C0, TAU * f / C0);
    let d = unit_dir(beam.theta, beam.phi);
    let aim = plan.hw.analog_aim.map_or(d, |a| unit_dir(a.0, a.1));
    let amps = amplitudes(m, plan);
    let cone = plan.cone_deg.to_radians().cos();
    // Element phases come from the pattern too, so panels that face different ways, or a
    // solved pattern with a phase of its own, add along one polarisation.
    let pol = m.reference_pol(d);
    let own = m.response(f0, d, pol);
    let hybrid = plan.hw.subarray.is_some();
    let (owner, centers) = subarrays(m, plan.hw.subarray);
    // What's dark or broken can't help make a null, and a null mustn't light it.
    let live: Vec<bool> = m.elements.iter().enumerate().map(|(i, e)| (flat || dot(e.normal, d) >= cone) && !plan.hw.failed.contains(&i)).collect();
    let element_phase = |i: usize| (own[i] * C64::from_polar(1.0, -k0 * dot(d, m.elements[i].pos))).arg();
    let amp = |i: usize| if live[i] { amps[i] } else { 0.0 };
    let dirs: Vec<P3> = plan.nulls.iter().map(|n| unit_dir(n.0, n.1)).collect();
    if hybrid {
        let outer = if plan.hw.delay == Delay::PhaseOnly { k0 } else { k };
        let inner_k = if plan.hw.delay == Delay::Element { k } else { k0 };
        let analog: Vec<C64> = m
            .elements
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let c = centers[owner[i]];
                let inside: P3 = std::array::from_fn(|j| e.pos[j] - c[j]);
                C64::from_polar(amp(i), quantise(-inner_k * dot(aim, inside) - element_phase(i), plan.hw.phase_bits))
            })
            .collect();
        let mut digital: Vec<C64> = centers.iter().map(|c| C64::from_polar(1.0, -outer * dot(d, *c))).collect();
        if !dirs.is_empty() {
            // Only the converters can move: a null here is made from whole subarrays.
            let rows: CMat = null_rows(m, f, &dirs)
                .iter()
                .map(|row| {
                    let mut s = vec![ZERO; centers.len()];
                    for (i, v) in row.iter().enumerate() {
                        s[owner[i]] += *v * analog[i];
                    }
                    s
                })
                .collect();
            digital = project_out(&rows, &digital);
        }
        return ((0..m.len()).map(|i| analog[i] * digital[owner[i]]).collect(), false);
    }
    let kk = if plan.hw.delay == Delay::Element { k } else { k0 };
    let w: Vec<C64> = m.elements.iter().enumerate().map(|(i, e)| C64::from_polar(amp(i), -kk * dot(d, e.pos) - element_phase(i))).collect();
    // A delay per element leaves no shifter to round it.
    let shifters = plan.hw.delay != Delay::Element;
    (if dirs.is_empty() { w } else { null_project(m, &w, f, &dirs, &live) }, shifters)
}

/// The hardware's last word: shifter rounding if it's still due, attenuator steps, random
/// errors, failures. Errors are drawn for every element, so a failure leaves the others' alone.
fn finish(w: &mut [C64], hw: &Hardware, shifters: bool) {
    let mut rng = proof::Rng::new(hw.seed.wrapping_add(11));
    for (i, v) in w.iter_mut().enumerate() {
        let (amp_draw, phase_draw) = (rng.gauss(), rng.gauss());
        let mut amp = v.norm();
        let mut phase = v.arg();
        if shifters {
            phase = quantise(phase, hw.phase_bits);
        }
        if let Some(step) = hw.atten_step_db.filter(|s| *s > 0.0)
            && amp > 0.0
        {
            let db = 20.0 * amp.log10();
            amp = 10f64.powf((db / step).round() * step / 20.0);
        }
        amp *= 10f64.powf(hw.amp_err_db * amp_draw / 20.0);
        phase += hw.phase_err_deg.to_radians() * phase_draw;
        *v = if hw.failed.contains(&i) { ZERO } else { C64::from_polar(amp, phase) };
    }
}

/// The weights of one beam at frequency `f`, on an array designed for `f0`: phase shifters
/// hold what they were set to at `f0`, delays follow `f`.
pub fn beam_weights(m: &ArrayModel, plan: &BeamPlan, beam: &Beam, f0: f64, f: f64, flat: bool) -> Vec<C64> {
    let (mut w, shifters) = steered(m, plan, beam, f0, f, flat);
    finish(&mut w, &plan.hw, shifters);
    w
}

/// Each direction's response as a row: `Σ wₙ·aₙ(d) = 0` is a null. Where elements face different
/// ways a direction takes both polarisations, or the other one would still radiate there.
fn null_rows(m: &ArrayModel, f: f64, dirs: &[P3]) -> CMat {
    let mut rows = Vec::new();
    for d in dirs {
        if m.orientations() > 1 {
            let (theta, phi) = super::aperture::angles_of(*d);
            let (a, b) = (super::aperture::theta_hat(theta, phi), super::aperture::phi_hat(phi));
            rows.push(m.response(f, *d, Some(std::array::from_fn(|k| C64::new(a[k], 0.0)))));
            rows.push(m.response(f, *d, Some(std::array::from_fn(|k| C64::new(b[k], 0.0)))));
        } else {
            rows.push(m.response(f, *d, m.reference_pol(*d)));
        }
    }
    rows
}

/// `w` less its part along `rows`' conjugates: the smallest change that makes each row's sum zero.
/// The Gram matrix is loaded a hair, so a repeated direction or one polarisation in two rows
/// asks nothing impossible.
pub fn project_out(rows: &CMat, w: &[C64]) -> Vec<C64> {
    if rows.is_empty() {
        return w.to_vec();
    }
    let c = herm(rows);
    let mut gram = mat_mul(rows, &c);
    let size = gram.len() as f64;
    let trace: f64 = (0..gram.len()).map(|i| gram[i][i].re).sum();
    for (i, row) in gram.iter_mut().enumerate() {
        row[i] += trace / size * 1e-11;
    }
    let Ok(inverse) = invert(&gram) else { return w.to_vec() };
    let back = mat_vec(&c, &mat_vec(&inverse, &mat_vec(rows, w)));
    w.iter().zip(&back).map(|(a, b)| *a - *b).collect()
}

/// Removes from `w` whatever would radiate towards `dirs`, using only the `live` elements.
pub fn null_project(m: &ArrayModel, w: &[C64], f: f64, dirs: &[P3], live: &[bool]) -> Vec<C64> {
    let mut rows = null_rows(m, f, dirs);
    for row in &mut rows {
        for (v, on) in row.iter_mut().zip(live) {
            if !on {
                *v = ZERO;
            }
        }
    }
    project_out(&rows, w)
}

/// A plan as the hardware can really run it: one set of shifters in each subarray has to serve
/// every digital beam, so with no aim given they point between them.
pub fn effective(plan: &BeamPlan) -> BeamPlan {
    let mut p = plan.clone();
    if p.multibeam == MultiBeam::Digital && p.hw.subarray.is_some() && p.hw.analog_aim.is_none() && p.beams.len() > 1 {
        let mut sum = [0.0; 3];
        for b in &p.beams {
            let d = unit_dir(b.theta, b.phi);
            for k in 0..3 {
                sum[k] += d[k] * b.share.max(0.0);
            }
        }
        p.hw.analog_aim = Some(super::aperture::angles_of(sum));
    }
    p
}

/// A plan's beams as weight vectors, each with its share of the power in it. With one
/// network there's one vector, and every beam is in its pattern.
pub fn all_weights(m: &ArrayModel, plan: &BeamPlan, f0: f64, f: f64, flat: bool) -> Vec<Vec<C64>> {
    let plan = &effective(plan);
    let total: f64 = plan.beams.iter().map(|b| b.share.max(0.0)).sum::<f64>().max(1e-12);
    match plan.multibeam {
        MultiBeam::Digital => plan.beams.iter().map(|b| beam_weights(m, plan, b, f0, f, flat).into_iter().map(|v| v * (b.share.max(0.0) / total).sqrt()).collect()).collect(),
        mode => {
            // One network: the sum is what the shifters have to hold, so they round the sum.
            let mut sum = vec![ZERO; m.len()];
            let mut shifters = true;
            for b in &plan.beams {
                let a = (b.share.max(0.0) / total).sqrt();
                let (w, s) = steered(m, plan, b, f0, f, flat);
                shifters &= s;
                for (x, v) in sum.iter_mut().zip(&w) {
                    *x += *v * a;
                }
            }
            if mode == MultiBeam::SharedPhaseOnly {
                for s in &mut sum {
                    if s.norm() > 0.0 {
                        *s /= s.norm();
                    }
                }
            } else {
                let top = sum.iter().map(|v| v.norm()).fold(0.0, f64::max);
                if top > 0.0 {
                    sum.iter_mut().for_each(|s| *s /= top);
                }
            }
            finish(&mut sum, &plan.hw, shifters);
            vec![sum]
        }
    }
}

/// What the amplifiers see: several digital beams add on each element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DriveLoad {
    /// Fraction of the array's full power the plan uses: taper and phase-only sums cost this.
    pub power_use: f64,
    /// Highest instantaneous voltage on any element when all the beams line up, per unit
    /// of an element at full drive. Above 1 an amplifier has to back off to take it.
    pub peak_voltage: f64,
}

pub fn drive_load(all: &[Vec<C64>]) -> DriveLoad {
    let Some(first) = all.first() else { return DriveLoad::default() };
    let n = first.len().max(1);
    let per_element: Vec<f64> = (0..first.len()).map(|i| all.iter().map(|w| w[i].norm_sqr()).sum()).collect();
    let peak_voltage: f64 = (0..first.len()).map(|i| all.iter().map(|w| w[i].norm()).sum::<f64>()).fold(0.0, f64::max);
    let top = per_element.iter().copied().fold(0.0, f64::max);
    DriveLoad { power_use: if top > 0.0 { per_element.iter().sum::<f64>() / (n as f64 * top) } else { 0.0 }, peak_voltage }
}

/// One frequency of a bandwidth sweep.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BandPoint {
    pub f: f64,
    /// Gain towards the beam's direction against the same at `f0`, dB.
    pub rel_db: f64,
}

/// How the beam holds up off its design frequency, at the direction it was asked for. The
/// element's own frequency response isn't in this: it's the wavefront alone.
pub fn band_sweep(m: &ArrayModel, plan: &BeamPlan, beam: &Beam, f0: f64, span: f64, n: usize, flat: bool) -> Vec<BandPoint> {
    let plan = &effective(plan);
    let d = unit_dir(beam.theta, beam.phi);
    let reference = {
        let w = beam_weights(m, plan, beam, f0, f0, flat);
        m.intensity(&w, f0, d).max(1e-300)
    };
    (0..n.max(2))
        .into_par_iter()
        .map(|i| {
            let f = f0 * (1.0 - span / 2.0 + span * i as f64 / (n.max(2) - 1) as f64);
            let w = beam_weights(m, plan, beam, f0, f, flat);
            BandPoint { f, rel_db: db10(m.intensity(&w, f, d) / reference) }
        })
        .collect()
}

/// The band around `f0` within which `rel_db` stays above −3 dB, Hz. `None`: past the sweep.
pub fn instantaneous_band(sweep: &[BandPoint], f0: f64) -> Option<f64> {
    let mid = sweep.iter().enumerate().min_by(|a, b| (a.1.f - f0).abs().total_cmp(&(b.1.f - f0).abs()))?.0;
    let cross = |range: &mut dyn Iterator<Item = usize>| -> Option<f64> {
        let mut last = sweep[mid];
        for i in range {
            let p = sweep[i];
            if p.rel_db <= -3.0 {
                let t = (last.rel_db + 3.0) / (last.rel_db - p.rel_db).max(1e-9);
                return Some(last.f + (p.f - last.f) * t);
            }
            last = p;
        }
        None
    };
    let hi = cross(&mut (mid + 1..sweep.len()))?;
    let lo = cross(&mut (0..mid).rev())?;
    Some(hi - lo)
}

/// Which way a beam of a phase-shifter array points at another frequency: `sin θ` scales by
/// `f0/f`. `None`: it has left visible space.
pub fn squint_theta(theta0_deg: f64, f0: f64, f: f64) -> Option<f64> {
    let s = theta0_deg.to_radians().sin() * f0 / f;
    (s.abs() <= 1.0).then(|| s.asin().to_degrees())
}

/// Capon's minimum-variance weights for a beam at `a` against covariance `r`: passes `a`
/// whole, and gives up everything else it can. `w = conj(R⁻¹a / aᴴR⁻¹a)`.
pub fn mvdr(r: &CMat, a: &[C64]) -> Result<Vec<C64>> {
    let inv = invert(r)?;
    let ria = mat_vec(&inv, a);
    let denominator = inner(a, &ria);
    Ok(ria.iter().map(|v| (*v / denominator).conj()).collect())
}

/// Linearly constrained minimum variance: the response to each `a_k` is `want_k`.
pub fn lcmv(r: &CMat, constraints: &[(Vec<C64>, C64)]) -> Result<Vec<C64>> {
    if constraints.is_empty() {
        bail!("no constraints");
    }
    let inv = invert(r)?;
    let c: CMat = {
        let n = constraints[0].0.len();
        (0..n).map(|i| constraints.iter().map(|con| con.0[i]).collect()).collect()
    };
    let ric = mat_mul(&inv, &c);
    let gram = mat_mul(&herm(&c), &ric);
    let gram_inv = invert(&gram)?;
    let f: Vec<C64> = constraints.iter().map(|con| con.1.conj()).collect();
    let g = mat_vec(&gram_inv, &f);
    let wh = mat_vec(&ric, &g);
    Ok(wh.into_iter().map(|v| v.conj()).collect())
}

/// A covariance of white noise `noise` and sources `(response, power)`.
pub fn covariance_of(noise: f64, sources: &[(Vec<C64>, f64)]) -> CMat {
    let n = sources.first().map_or(0, |s| s.0.len());
    let mut r = identity(n);
    for (i, row) in r.iter_mut().enumerate() {
        row[i] *= noise;
    }
    for (a, p) in sources {
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v += a[i] * a[j].conj() * *p;
            }
        }
    }
    r
}

/// Output signal over noise plus interference of `y = Σ wₙxₙ`.
pub fn output_sinr(w: &[C64], signal: &[C64], signal_power: f64, r_in: &CMat) -> f64 {
    let wh: Vec<C64> = w.iter().map(|v| v.conj()).collect();
    let s: C64 = w.iter().zip(signal).map(|(a, b)| *a * *b).sum();
    signal_power * s.norm_sqr() / quadratic(r_in, &wh).max(1e-300)
}

/// A Butler matrix: `n = 2^m` orthogonal beams from a line, each with a progressive phase.
/// `beam` runs 0..n; its weights are in the array's element order.
pub fn butler_weights(n: usize, beam: usize) -> Vec<C64> {
    let shift = beam as f64 - (n as f64 - 1.0) / 2.0;
    (0..n).map(|i| C64::from_polar(1.0 / (n as f64).sqrt(), TAU * i as f64 * shift / n as f64)).collect()
}

/// Each Butler beam's direction cosine along the line, on spacing `d_lambda` wavelengths.
pub fn butler_directions(n: usize, d_lambda: f64) -> Vec<f64> {
    (0..n).map(|m| -(m as f64 - (n as f64 - 1.0) / 2.0) / (n as f64 * d_lambda)).collect()
}

/// A limit on a shaped beam over a span of a pattern cut, dB against the shape's top.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskRegion {
    pub theta_lo: f64,
    pub theta_hi: f64,
    pub lo_db: Option<f64>,
    pub hi_db: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapedBeam {
    pub weights: Vec<C64>,
    /// The pattern cut it ended on, `(θ, dB)`, against its top.
    pub cut: Vec<(f64, f64)>,
    /// The worst miss of the mask, dB.
    pub worst_db: f64,
    pub iterations: usize,
    /// Each amplitude's dB against the strongest, as a taper loss would read.
    pub taper_db: f64,
}

/// A beam of a given shape in the plane at `phi`, by alternating projection: clip the pattern
/// to the mask, fit weights to that, repeat. `phase_only` holds every amplitude equal, which
/// costs some of the mask but suits amplifiers run at one level.
pub fn synthesize(m: &ArrayModel, f: f64, phi: f64, mask: &[MaskRegion], phase_only: bool, iterations: usize) -> Result<ShapedBeam> {
    if m.is_empty() {
        bail!("no elements");
    }
    if mask.is_empty() {
        bail!("no mask");
    }
    let lambda = C0 / f;
    let n = m.len();
    // Sampled at a quarter of a Nyquist step for the longest extent.
    let step = (lambda / (m.extent() + lambda) / 4.0).asin().to_degrees().clamp(0.2, 2.0);
    let thetas: Vec<f64> = (0..).map(|i| -90.0 + i as f64 * step).take_while(|t| *t <= 90.0).collect();
    // Along what each direction really radiates: one fixed polarisation would read a false fall-off.
    let response = |theta: f64| {
        let d = unit_dir(theta, phi);
        m.response(f, d, m.reference_pol(d))
    };
    let a: Vec<Vec<C64>> = thetas.par_iter().map(|t| response(*t)).collect();
    let limits: Vec<(f64, f64)> = thetas
        .iter()
        .map(|t| {
            let mut lo = 0.0f64;
            let mut hi = f64::INFINITY;
            for r in mask {
                if *t >= r.theta_lo && *t <= r.theta_hi {
                    if let Some(v) = r.lo_db {
                        lo = lo.max(10f64.powf(v / 20.0));
                    }
                    hi = hi.min(10f64.powf(r.hi_db / 20.0));
                }
            }
            (lo, hi)
        })
        .collect();
    if let Some((t, (lo, hi))) = thetas.iter().zip(&limits).find(|(_, l)| l.0 > l.1) {
        bail!("At {t:.1}° the mask asks for at least {:.1} dB and at most {:.1} dB.", 20.0 * lo.log10(), 20.0 * hi.log10());
    }
    // The normal matrix of the least-squares fit, loaded a little for spacing below λ/2.
    let mut gram = zeros(n, n);
    for row in &a {
        for i in 0..n {
            for j in 0..n {
                gram[i][j] += row[i].conj() * row[j];
            }
        }
    }
    let trace: f64 = (0..n).map(|i| gram[i][i].re).sum();
    for (i, row) in gram.iter_mut().enumerate() {
        row[i] += trace / n as f64 * 1e-6;
    }
    let solve = invert(&gram)?;
    // Start from a beam on the middle of the first spanned region with a lower limit.
    let start = mask.iter().find(|r| r.lo_db.is_some()).map_or(0.0, |r| 0.5 * (r.theta_lo + r.theta_hi));
    let d0 = response(start);
    let mut w: Vec<C64> = d0.iter().map(|v| C64::from_polar(1.0, -v.arg())).collect();
    let (mut best, mut best_worst) = (w.clone(), f64::INFINITY);
    let mut used = 0;
    for it in 0..iterations.max(1) {
        used = it + 1;
        let pattern: Vec<C64> = a.iter().map(|row| row.iter().zip(&w).map(|(x, y)| *x * *y).sum()).collect();
        let top = pattern.iter().zip(&limits).filter(|(_, l)| l.0 > 0.0).map(|(p, _)| p.norm()).fold(0.0, f64::max).max(1e-300);
        // How far outside its limits the pattern is, then the pattern moved inside them.
        let mut worst = 0.0f64;
        let clipped: Vec<C64> = pattern
            .iter()
            .zip(&limits)
            .map(|(p, (lo, hi))| {
                let size = p.norm() / top;
                let target = size.clamp(*lo, *hi);
                let miss = if size > *hi {
                    20.0 * (size / hi).log10()
                } else if size < *lo {
                    20.0 * (lo / size.max(1e-12)).log10()
                } else {
                    0.0
                };
                worst = worst.max(miss);
                if size > 0.0 { *p / p.norm() * (target * top) } else { C64::new(target * top, 0.0) }
            })
            .collect();
        if worst < best_worst {
            best_worst = worst;
            best = w.clone();
        }
        if worst < 0.01 {
            break;
        }
        let mut rhs = vec![ZERO; n];
        for (row, c) in a.iter().zip(&clipped) {
            for (r, x) in rhs.iter_mut().zip(row) {
                *r += x.conj() * *c;
            }
        }
        w = mat_vec(&solve, &rhs);
        if phase_only {
            for v in &mut w {
                *v = if v.norm() > 0.0 { *v / v.norm() } else { C64::new(1.0, 0.0) };
            }
        }
    }
    let w = best;
    let pattern: Vec<f64> = a.iter().map(|row| row.iter().zip(&w).map(|(x, y)| *x * *y).sum::<C64>().norm()).collect();
    let top = pattern.iter().copied().fold(0.0, f64::max).max(1e-300);
    let cut = thetas.iter().zip(&pattern).map(|(t, v)| (*t, 20.0 * (v / top).max(1e-9).log10())).collect();
    let amp_top = w.iter().map(|v| v.norm()).fold(0.0, f64::max).max(1e-300);
    let weights: Vec<C64> = w.iter().map(|v| *v / amp_top).collect();
    let sum: f64 = weights.iter().map(|v| v.norm()).sum();
    Ok(ShapedBeam { taper_db: db10(sum * sum / (n as f64 * norm_sqr(&weights).max(1e-300))), weights, cut, worst_db: best_worst, iterations: used })
}

/// A grid of beams covering a cone off the array's normal, on hexagonal centres so the
/// worst point between them is as high as it can be: what a beam-switched system stores.
pub fn codebook(m: &ArrayModel, f: f64, cone_deg: f64, crossover_db: f64) -> Vec<(f64, f64)> {
    // Spacing in direction cosines: a sinc-like beam falls `crossover_db` at about this far off.
    let lambda = C0 / f;
    let span = m.extent() + lambda / 2.0;
    let width = 0.886 * lambda / span.max(lambda / 2.0);
    let spacing = (width * (crossover_db.abs() / 3.0).sqrt()).max(1e-3);
    let reach = cone_deg.to_radians().sin();
    let mut out = Vec::new();
    let rows = (reach / (spacing * 0.866)).ceil() as i64 + 1;
    let cols = (reach / spacing).ceil() as i64 + 1;
    for r in -rows..=rows {
        for c in -cols..=cols {
            let u = (c as f64 + if r.rem_euclid(2) == 1 { 0.5 } else { 0.0 }) * spacing;
            let v = r as f64 * spacing * 0.866;
            let s = (u * u + v * v).sqrt();
            if s <= reach {
                let theta = s.clamp(0.0, 1.0).asin().to_degrees();
                let phi = if s > 0.0 { v.atan2(u).to_degrees().rem_euclid(360.0) } else { 0.0 };
                out.push((theta, phi));
            }
        }
    }
    out
}

/// What each element's delay line is set to for a beam, seconds, none negative. A delay `τ`
/// multiplies by `e^(−j2πfτ)`, so a beam's `e^(−jk·d·r)` is a delay of `d·r/c`.
fn delay_settings(m: &ArrayModel, plan: &BeamPlan, beam: &Beam) -> Vec<f64> {
    let d = unit_dir(beam.theta, beam.phi);
    let raw: Vec<f64> = match (plan.hw.delay, plan.hw.subarray) {
        (Delay::Element, _) => m.elements.iter().map(|e| dot(d, e.pos) / C0).collect(),
        (Delay::Subarray, Some(size)) => {
            let (owner, centers) = subarrays(m, Some(size));
            owner.iter().map(|o| dot(d, centers[*o]) / C0).collect()
        }
        _ => return vec![0.0; m.len()],
    };
    let least = raw.iter().copied().fold(f64::MAX, f64::min);
    raw.into_iter().map(|t| t - least).collect()
}

/// A beam table to load into a controller: one row per beam per element. `phase_deg` is the
/// weight's own phase at `f0`. It's made up of the delay line and then what's left for the
/// shifter, so `e^(j·phase) = e^(−j2π·f0·delay)·e^(j·shifter)`, and `phase_code` is that shifter's
/// setting, 0 to 2^bits − 1. A plan with one network has one row set, for all its beams.
pub fn weights_csv(m: &ArrayModel, plan: &BeamPlan, f0: f64, flat: bool) -> String {
    let plan = &effective(plan);
    let mut out = String::from(CSV_HEADER);
    let shared = plan.multibeam != MultiBeam::Digital;
    for (b, w) in all_weights(m, plan, f0, f0, flat).iter().enumerate() {
        let beam = plan.beams.get(b).copied().unwrap_or_default();
        let delays = if shared { vec![0.0; m.len()] } else { delay_settings(m, plan, &beam) };
        let asked = (!shared).then_some((beam.theta, beam.phi));
        let places: Vec<(P3, usize)> = m.elements.iter().map(|e| (e.pos, e.panel)).collect();
        csv_rows(&mut out, b, asked, &places, w, &delays, f0, plan.hw.phase_bits);
    }
    out
}

const CSV_HEADER: &str = "beam,theta_deg,phi_deg,element,panel,x_mm,y_mm,z_mm,amp_db,phase_deg,delay_ps,shifter_deg,phase_code\n";

/// The header and rows of a beam table for weights on given positions, as a solved array has them.
pub fn solved_csv(pos: &[P3], asked: (f64, f64), w: &[C64], f0: f64, bits: Option<u8>) -> String {
    let mut out = String::from(CSV_HEADER);
    let places: Vec<(P3, usize)> = pos.iter().map(|p| (*p, 0)).collect();
    csv_rows(&mut out, 0, Some(asked), &places, w, &vec![0.0; pos.len()], f0, bits);
    out
}

#[allow(clippy::too_many_arguments)]
fn csv_rows(out: &mut String, beam: usize, asked: Option<(f64, f64)>, places: &[(P3, usize)], w: &[C64], delays: &[f64], f0: f64, bits: Option<u8>) {
    let label = asked.map_or(",".to_string(), |a| format!("{:.3},{:.3}", a.0, a.1));
    for (i, (v, (p, panel))) in w.iter().zip(places).enumerate() {
        let amp = if v.norm() > 0.0 { 20.0 * v.norm().log10() } else { -999.0 };
        let phase = v.arg().to_degrees().rem_euclid(360.0);
        let shifter = (phase + 360.0 * f0 * delays[i]).rem_euclid(360.0);
        // Rounding near 360° lands on the top code's far side: that's code 0.
        let code = bits.map_or(String::new(), |n| {
            let levels = 2i64.pow(n as u32);
            format!("{}", ((shifter / (360.0 / levels as f64)).round() as i64).rem_euclid(levels))
        });
        out.push_str(&format!("{beam},{label},{i},{panel},{:.4},{:.4},{:.4},{amp:.3},{phase:.3},{:.3},{shifter:.3},{code}\n", p[0] * 1e3, p[1] * 1e3, p[2] * 1e3, delays[i] * 1e12));
    }
}

/// The layout's element count, a guard the pages use before a heavy calculation.
pub fn too_big(layout: &Layout) -> bool {
    layout.count() > 4096
}

/// Peak of the first quantisation lobe for `bits`-bit phase shifters, dB against the beam, at
/// the worst case, when a scan makes the rounding errors periodic: a sawtooth's harmonic falls as
/// `1/(2^bits − 1)`. Two states give two equal beams; the textbook's `−6.02·bits` is its limit.
pub fn quantisation_lobe_db(bits: u8) -> f64 {
    let n = 2f64.powi(bits as i32);
    20.0 * (1.0 / (n - 1.0)).log10()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::super::aperture::{Layout, angle_between};
    use super::super::pattern::{ElementKind, report};
    use super::*;

    const F: f64 = 3e9;

    fn iso(layout: &Layout) -> ArrayModel {
        ArrayModel::new(layout.elements(F), ElementKind::Isotropic, None)
    }

    fn line16() -> (Layout, ArrayModel) {
        let l = Layout::Line { n: 16, spacing: 0.5 };
        let m = iso(&l);
        (l, m)
    }

    fn at(m: &ArrayModel, w: &[C64], f: f64, t: f64, p: f64) -> f64 {
        m.intensity(w, f, unit_dir(t, p)) * 4.0 * PI
    }

    #[test]
    fn a_plain_plan_steers_like_the_formula() {
        let (_, m) = line16();
        let plan = BeamPlan { beams: vec![Beam { theta: 30.0, phi: 0.0, share: 1.0 }], ..Default::default() };
        let w = beam_weights(&m, &plan, &plan.beams[0], F, F, true);
        let k = TAU * F / C0;
        let d = unit_dir(30.0, 0.0);
        for (v, e) in w.iter().zip(&m.elements) {
            let want = C64::from_polar(1.0, -k * dot(d, e.pos));
            assert!((*v - want).norm() < 1e-9, "{v} vs {want}");
        }
        let r = report(&m, &w, F, (30.0, 0.0), &[]);
        assert!(r.pointing_error_deg < 0.05, "{r:?}");
    }

    #[test]
    fn phase_shifters_squint_as_the_formula_says_and_delays_dont() {
        let n = 32;
        let m = iso(&Layout::Line { n, spacing: 0.5 });
        let beam = Beam { theta: 45.0, phi: 0.0, share: 1.0 };
        let f = 1.05 * F;
        let ps = BeamPlan { beams: vec![beam], ..Default::default() };
        let w = beam_weights(&m, &ps, &beam, F, f, true);
        let r = report(&m, &w, f, (45.0, 0.0), &[]);
        let want = squint_theta(45.0, F, f).unwrap();
        assert!((r.peak_theta - want).abs() < 0.05, "peak {} vs formula {want}", r.peak_theta);
        assert!(r.pointing_error_deg > 2.0, "{}", r.pointing_error_deg);
        let delay = BeamPlan { hw: Hardware { delay: Delay::Element, ..Default::default() }, beams: vec![beam], ..Default::default() };
        let w = beam_weights(&m, &delay, &beam, F, f, true);
        let r = report(&m, &w, f, (45.0, 0.0), &[]);
        assert!(r.pointing_error_deg < 0.05, "a delay line doesn't squint: {}", r.pointing_error_deg);
    }

    #[test]
    fn instantaneous_band_matches_the_rule_of_thumb() {
        // A uniform line: Δf/f ≈ 0.886·λ/(L·sin θ₀) between the −3 dB points. At N = 64,
        // d = λ/2, 60°: 3.2 %.
        let n = 64;
        let m = iso(&Layout::Line { n, spacing: 0.5 });
        let beam = Beam { theta: 60.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { beams: vec![beam], ..Default::default() };
        let sweep = band_sweep(&m, &plan, &beam, F, 0.10, 401, true);
        let band = instantaneous_band(&sweep, F).unwrap() / F;
        let want = 0.886 / (n as f64 * 0.5 * 60f64.to_radians().sin());
        assert!((band - want).abs() < 0.08 * want, "{band} vs {want}");
        // Delay per subarray widens it, with element-level delay unlimited over the sweep.
        let sub = BeamPlan { hw: Hardware { delay: Delay::Subarray, subarray: Some((8, 1)), ..Default::default() }, beams: vec![beam], ..Default::default() };
        let wider = instantaneous_band(&band_sweep(&m, &sub, &beam, F, 0.40, 801, true), F).unwrap() / F;
        assert!(wider > 4.0 * band, "{wider} vs {band}");
        let delay = BeamPlan { hw: Hardware { delay: Delay::Element, ..Default::default() }, beams: vec![beam], ..Default::default() };
        let flat = band_sweep(&m, &delay, &beam, F, 0.10, 21, true);
        assert!(flat.iter().all(|p| p.rel_db.abs() < 1e-6), "{flat:?}");
    }

    #[test]
    fn quantised_phase_lowers_gain_as_expected() {
        // Quantisation loss: 10·log10(sinc²(1/2^b)), about 0.23 dB at 3 bits.
        let n = 64;
        let m = iso(&Layout::Line { n, spacing: 0.5 });
        let beam = Beam { theta: 33.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { hw: Hardware { phase_bits: Some(3), ..Default::default() }, beams: vec![beam], ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        let base = n as f64 * n as f64;
        let got = 10.0 * (at(&m, &w, F, 33.0, 0.0) / base).log10();
        let lsb = PI / 8.0;
        let want = 20.0 * (lsb.sin() / lsb).log10();
        assert!((got - want).abs() < 0.1, "{got} vs {want}");
        // Every phase lies on the grid.
        for v in &w {
            let steps = v.arg() / (PI / 4.0);
            assert!((steps - steps.round()).abs() < 1e-9, "{steps}");
        }
    }

    #[test]
    fn quantisation_lobes_match_the_textbook_at_a_periodic_scan() {
        // Step 2π/32 per element and 3 bits: the staircase repeats every 4 elements, so its error
        // is a sawtooth, whose first harmonic lands at −7 times the scan's direction cosine.
        let n = 256;
        let m = iso(&Layout::Line { n, spacing: 0.5 });
        let u0 = 2.0 / 32.0;
        let beam = Beam { theta: f64::asin(u0).to_degrees(), phi: 0.0, share: 1.0 };
        let plan = BeamPlan { hw: Hardware { phase_bits: Some(3), ..Default::default() }, beams: vec![beam], ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        let main = at(&m, &w, F, beam.theta, 0.0);
        let lobe_u = -7.0 * u0;
        let lobe = at(&m, &w, F, lobe_u.abs().asin().to_degrees(), 180.0);
        let got = 10.0 * (lobe / main).log10();
        let want = quantisation_lobe_db(3);
        assert!((got - want).abs() < 1.0, "{got} vs {want}");
    }

    #[test]
    fn hybrid_subarrays_put_quantisation_lobes_where_the_pitch_says() {
        // Subarrays of 4 at λ/2, 2 λ between centres. Analog aim at broadside, digital beam
        // at sin θ = 0.2: the grating lobes of a 2 λ pitch are at u = 0.2 ± m/2.
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let u0 = 0.2f64;
        let beam = Beam { theta: u0.asin().to_degrees(), phi: 0.0, share: 1.0 };
        let plan = BeamPlan { hw: Hardware { subarray: Some((4, 1)), analog_aim: Some((0.0, 0.0)), ..Default::default() }, beams: vec![beam], ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        let main = at(&m, &w, F, beam.theta, 0.0);
        let lobe_u = u0 - 0.5;
        let lobe_t = lobe_u.abs().asin().to_degrees();
        let lobe_p = if lobe_u < 0.0 { 180.0 } else { 0.0 };
        let lobe = at(&m, &w, F, lobe_t, lobe_p);
        // The lobe, weighed by the subarray's own pattern, is well up from a clean array.
        let rel = 10.0 * (lobe / main).log10();
        assert!(rel > -15.0 && rel < 0.0, "{rel}");
        // The same beam aimed by the analog part too has no such lobe.
        let aimed = BeamPlan { hw: Hardware { subarray: Some((4, 1)), analog_aim: None, ..Default::default() }, beams: vec![beam], ..Default::default() };
        let w2 = beam_weights(&m, &aimed, &beam, F, F, true);
        let rel2 = 10.0 * (at(&m, &w2, F, lobe_t, lobe_p) / at(&m, &w2, F, beam.theta, 0.0)).log10();
        assert!(rel2 < rel - 10.0, "{rel2} vs {rel}");
        // And it costs gain, by the subarray pattern at the offset.
        assert!(main < at(&m, &w2, F, beam.theta, 0.0));
    }

    #[test]
    fn a_taper_is_applied_per_panel() {
        let l = Layout::Panels { faces: 2, nx: 8, ny: 1, dx: 0.5, dy: 0.5, radius: 3.0, tilt_deg: 0.0 };
        let m = iso(&l);
        let plan = BeamPlan { taper_x: Taper::Hamming, ..Default::default() };
        let amps = amplitudes(&m, &plan);
        let want = taper_weights(Taper::Hamming, 8);
        assert_eq!(amps.len(), 16);
        for i in 0..8 {
            assert!((amps[i] - want[i]).abs() < 1e-12);
            assert!((amps[8 + i] - want[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn multi_face_arrays_hand_over_between_faces() {
        let l = Layout::Panels { faces: 4, nx: 8, ny: 8, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        let kind = ElementKind::Cosine { q: 1.5, back_db: -30.0 };
        let m = ArrayModel::new(l.elements(F), kind, None);
        let plan = BeamPlan { cone_deg: 60.0, ..Default::default() };
        // Straight at face 0: only face 0 lit.
        let beam = Beam { theta: 90.0, phi: 0.0, share: 1.0 };
        let w = beam_weights(&m, &plan, &beam, F, F, false);
        let lit: Vec<usize> = m.elements.iter().enumerate().filter(|(i, _)| w[*i].norm() > 0.0).map(|(_, e)| e.panel).collect();
        assert_eq!(lit.len(), 64);
        assert!(lit.iter().all(|p| *p == 0), "{lit:?}");
        let r = report(&m, &w, F, (90.0, 0.0), &[]);
        assert!(r.pointing_error_deg < 1.0, "{r:?}");
        // Between two faces both light up, and a beam still forms there, more narrow than one face's.
        let between = Beam { theta: 90.0, phi: 45.0, share: 1.0 };
        let w = beam_weights(&m, &plan, &between, F, F, false);
        let faces: std::collections::BTreeSet<usize> = m.elements.iter().enumerate().filter(|(i, _)| w[*i].norm() > 0.0).map(|(_, e)| e.panel).collect();
        assert_eq!(faces.len(), 2, "{faces:?}");
        let r2 = report(&m, &w, F, (90.0, 45.0), &[]);
        assert!(r2.at_request_dbi > r.realized_dbi - 4.0, "{r2:?} vs {r:?}");
    }

    #[test]
    fn panels_of_turned_elements_add_along_one_polarisation() {
        // Two faces at ±45° see the beam at 45° off normal. Their x axes differ by 90° in space,
        // so with no polarisation term in the phase they don't add. Here they do.
        let l = Layout::Panels { faces: 4, nx: 4, ny: 4, dx: 0.5, dy: 0.5, radius: 1.0, tilt_deg: 0.0 };
        let m = ArrayModel::new(l.elements(F), ElementKind::Isotropic, None);
        let plan = BeamPlan { cone_deg: 90.0, ..Default::default() };
        let beam = Beam { theta: 90.0, phi: 45.0, share: 1.0 };
        let w = beam_weights(&m, &plan, &beam, F, F, false);
        let along: Vec<C64> = m.response(F, unit_dir(90.0, 45.0), m.reference_pol(unit_dir(90.0, 45.0)));
        let sum: C64 = along.iter().zip(&w).map(|(a, b)| *a * *b).sum();
        let bound: f64 = along.iter().zip(&w).map(|(a, b)| a.norm() * b.norm()).sum();
        assert!(sum.norm() > 0.999 * bound, "all the lit elements in phase: {} vs {bound}", sum.norm());
    }

    #[test]
    fn digital_beams_share_power_and_shared_ones_share_one_pattern() {
        let (_, m) = line16();
        let beams = vec![Beam { theta: 20.0, phi: 0.0, share: 1.0 }, Beam { theta: 40.0, phi: 180.0, share: 1.0 }];
        let digital = BeamPlan { beams: beams.clone(), multibeam: MultiBeam::Digital, ..Default::default() };
        let all = all_weights(&m, &digital, F, F, true);
        assert_eq!(all.len(), 2);
        // Each has half the power: 3 dB per beam against a single beam.
        let solo = all_weights(&m, &BeamPlan { beams: vec![beams[0]], ..Default::default() }, F, F, true);
        let d = 10.0 * (at(&m, &all[0], F, 20.0, 0.0) / at(&m, &solo[0], F, 20.0, 0.0)).log10();
        assert!((d + 3.01).abs() < 0.01, "{d}");
        // When they line up they ask for twice the single-beam voltage / √2.
        let load = drive_load(&all);
        assert!((load.peak_voltage - 2f64.sqrt()).abs() < 1e-6, "{load:?}");
        assert!((load.power_use - 1.0).abs() < 1e-9, "each element's power is the same: {load:?}");
        let shared = BeamPlan { beams: beams.clone(), multibeam: MultiBeam::Shared, ..Default::default() };
        let one = all_weights(&m, &shared, F, F, true);
        assert_eq!(one.len(), 1);
        let (a, b) = (at(&m, &one[0], F, 20.0, 0.0), at(&m, &one[0], F, 40.0, 180.0));
        assert!((10.0 * (a / b).log10()).abs() < 0.5, "both beams in one pattern: {a} {b}");
        // Holding amplitudes equal costs little (a pair of beams: about 0.9 dB), and the beams stay.
        let phase = BeamPlan { beams, multibeam: MultiBeam::SharedPhaseOnly, ..Default::default() };
        let po = all_weights(&m, &phase, F, F, true);
        assert!(po[0].iter().all(|v| (v.norm() - 1.0).abs() < 1e-9 || v.norm() == 0.0));
        let loss = 10.0 * ((at(&m, &po[0], F, 20.0, 0.0) + at(&m, &po[0], F, 40.0, 180.0)) / (16.0 * 16.0)).log10();
        assert!((-1.6..0.0).contains(&loss), "{loss}");
    }

    #[test]
    fn nulls_go_where_they_are_asked_at_little_cost() {
        let (_, m) = line16();
        let beam = Beam { theta: 10.0, phi: 0.0, share: 1.0 };
        let bare = BeamPlan { beams: vec![beam], ..Default::default() };
        let plan = BeamPlan { beams: vec![beam], nulls: vec![(35.0, 0.0), (50.0, 180.0)], ..Default::default() };
        let w0 = beam_weights(&m, &bare, &beam, F, F, true);
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        for (t, p) in &plan.nulls {
            let depth = 10.0 * (at(&m, &w, F, *t, *p) / at(&m, &w, F, 10.0, 0.0)).log10();
            assert!(depth < -120.0, "null at {t}: {depth}");
        }
        let loss = 10.0 * (at(&m, &w, F, 10.0, 0.0) / at(&m, &w0, F, 10.0, 0.0)).log10();
        assert!(loss > -0.5, "{loss}");
        // Finite phase shifters limit how deep a null gets: why the bits matter for nulling.
        let rough = BeamPlan { beams: vec![beam], nulls: plan.nulls.clone(), hw: Hardware { phase_bits: Some(4), ..Default::default() }, ..Default::default() };
        let w = beam_weights(&m, &rough, &beam, F, F, true);
        let depth = 10.0 * (at(&m, &w, F, 35.0, 0.0) / at(&m, &w, F, 10.0, 0.0)).log10();
        assert!(depth > -60.0, "{depth}");
    }

    #[test]
    fn mvdr_keeps_the_signal_and_cancels_the_jammer() {
        let (_, m) = line16();
        let resp = |t: f64, p: f64| -> Vec<C64> { let d = unit_dir(t, p); let k = TAU * F / C0; m.elements.iter().map(|e| C64::from_polar(1.0, k * dot(d, e.pos))).collect() };
        let a = resp(5.0, 0.0);
        let j = resp(40.0, 0.0);
        let jammer = 1e4;
        let r = covariance_of(1.0, &[(j.clone(), jammer)]);
        let w = mvdr(&r, &a).unwrap();
        let unity: C64 = w.iter().zip(&a).map(|(x, y)| *x * *y).sum();
        assert!((unity - C64::new(1.0, 0.0)).norm() < 1e-9, "distortionless: {unity}");
        let leak: C64 = w.iter().zip(&j).map(|(x, y)| *x * *y).sum();
        assert!(leak.norm_sqr() * jammer < 1e-2, "jammer {}", leak.norm_sqr() * jammer);
        // Output SINR against the optimum, σ_s²·aᴴR⁻¹a.
        let got = output_sinr(&w, &a, 1.0, &r);
        let inv = invert(&r).unwrap();
        let want = inner(&a, &mat_vec(&inv, &a)).re;
        assert!((got - want).abs() < 1e-6 * want, "{got} vs {want}");
        // With no jammer it reduces to a plain beam: array gain N.
        let quiet = covariance_of(1.0, &[(j, 0.0)]);
        let w = mvdr(&quiet, &a).unwrap();
        assert!((output_sinr(&w, &a, 1.0, &quiet) - 16.0).abs() < 1e-9);
    }

    #[test]
    fn lcmv_holds_every_constraint() {
        let (_, m) = line16();
        let resp = |t: f64| -> Vec<C64> { let d = unit_dir(t, 0.0); let k = TAU * F / C0; m.elements.iter().map(|e| C64::from_polar(1.0, k * dot(d, e.pos))).collect() };
        let r = covariance_of(1.0, &[(resp(50.0), 100.0)]);
        let constraints = vec![(resp(0.0), C64::new(1.0, 0.0)), (resp(20.0), C64::new(0.5, 0.0)), (resp(30.0), ZERO)];
        let w = lcmv(&r, &constraints).unwrap();
        for (a, want) in &constraints {
            let got: C64 = w.iter().zip(a).map(|(x, y)| *x * *y).sum();
            assert!((got - *want).norm() < 1e-9, "{got} vs {want}");
        }
    }

    #[test]
    fn butler_beams_are_orthogonal_and_cross_at_3_9_db() {
        let n = 8;
        let m = iso(&Layout::Line { n, spacing: 0.5 });
        let beams: Vec<Vec<C64>> = (0..n).map(|b| butler_weights(n, b)).collect();
        for a in 0..n {
            for b in 0..n {
                let dot_ab = inner(&beams[a], &beams[b]).norm();
                let want = if a == b { 1.0 } else { 0.0 };
                assert!((dot_ab - want).abs() < 1e-9, "{a} {b}: {dot_ab}");
            }
        }
        let dirs = butler_directions(n, 0.5);
        for (b, u) in dirs.iter().enumerate() {
            let (t, p) = (u.abs().asin().to_degrees(), if *u < 0.0 { 180.0 } else { 0.0 });
            let top = at(&m, &beams[b], F, t, p);
            assert!((top - n as f64).abs() < 1e-6, "beam {b} at u {u}: {top}");
            // Each other beam has a null there.
            for (o, w) in beams.iter().enumerate() {
                if o != b {
                    assert!(at(&m, w, F, t, p) < 1e-9 * top, "{o} on {b}");
                }
            }
        }
        // Crossover midway between adjacent beams: (2/π)² for a sinc, −3.92 dB.
        let u = 0.5 * (dirs[3] + dirs[4]);
        let (t, p) = (u.abs().asin().to_degrees(), if u < 0.0 { 180.0 } else { 0.0 });
        let c = 10.0 * (at(&m, &beams[3], F, t, p) / n as f64).log10();
        assert!((c + 3.92).abs() < 0.25, "{c}");
    }

    #[test]
    fn a_flat_topped_sector_meets_its_mask() {
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let mask = vec![
            MaskRegion { theta_lo: -20.0, theta_hi: 20.0, lo_db: Some(-1.0), hi_db: 0.0 },
            MaskRegion { theta_lo: -90.0, theta_hi: -28.0, lo_db: None, hi_db: -25.0 },
            MaskRegion { theta_lo: 28.0, theta_hi: 90.0, lo_db: None, hi_db: -25.0 },
        ];
        let s = synthesize(&m, F, 0.0, &mask, false, 400).unwrap();
        assert!(s.worst_db < 0.5, "worst {} after {} iterations", s.worst_db, s.iterations);
        let reads = |lo: f64, hi: f64| s.cut.iter().filter(move |c| c.0 >= lo && c.0 <= hi).map(|c| c.1);
        assert!(reads(-20.0, 20.0).fold(0.0, f64::min) > -1.6);
        assert!(reads(28.0, 90.0).fold(f64::MIN, f64::max) < -24.0);
    }

    #[test]
    fn a_phase_only_shape_keeps_every_amplitude_equal() {
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let mask = vec![MaskRegion { theta_lo: -15.0, theta_hi: 15.0, lo_db: Some(-2.0), hi_db: 0.0 }, MaskRegion { theta_lo: -90.0, theta_hi: -25.0, lo_db: None, hi_db: -12.0 }, MaskRegion { theta_lo: 25.0, theta_hi: 90.0, lo_db: None, hi_db: -12.0 }];
        let s = synthesize(&m, F, 0.0, &mask, true, 300).unwrap();
        assert!(s.weights.iter().all(|v| (v.norm() - 1.0).abs() < 1e-9));
        assert!(s.worst_db < 3.0, "{}", s.worst_db);
        assert!(s.taper_db.abs() < 1e-6, "no taper loss: {}", s.taper_db);
    }

    #[test]
    fn a_codebook_covers_its_cone() {
        let m = iso(&Layout::Planar { nx: 8, ny: 8, dx: 0.5, dy: 0.5, triangular: false });
        let beams = codebook(&m, F, 45.0, -3.0);
        assert!(beams.len() > 5, "{beams:?}");
        // Any direction in the cone is within a beam's reach.
        for t in [0.0, 15.0, 30.0, 44.0] {
            for p in [0.0, 37.0, 90.0, 200.0] {
                let near = beams.iter().map(|b| angle_between(*b, (t, p))).fold(f64::MAX, f64::min);
                assert!(near < 16.0, "({t}, {p}) is {near}° from a beam");
            }
        }
    }

    #[test]
    fn failed_elements_stop_radiating_and_errors_repeat() {
        let (_, m) = line16();
        let beam = Beam::default();
        let plan = BeamPlan { hw: Hardware { failed: vec![3, 9], amp_err_db: 0.5, phase_err_deg: 5.0, seed: 3, ..Default::default() }, ..Default::default() };
        let a = beam_weights(&m, &plan, &beam, F, F, true);
        let b = beam_weights(&m, &plan, &beam, F, F, true);
        assert_eq!(a[3], ZERO);
        assert_eq!(a[9], ZERO);
        assert_eq!(a, b, "seeded");
        let c = beam_weights(&m, &BeamPlan { hw: Hardware { failed: vec![3, 9, 11], ..plan.hw.clone() }, ..Default::default() }, &beam, F, F, true);
        assert_eq!(a[5], c[5], "a further failure leaves the others' errors alone");
    }

    #[test]
    fn attenuators_round_to_their_steps() {
        let (_, m) = line16();
        let plan = BeamPlan { taper_x: Taper::Hamming, hw: Hardware { atten_step_db: Some(0.5), ..Default::default() }, ..Default::default() };
        let w = beam_weights(&m, &plan, &Beam::default(), F, F, true);
        for v in &w {
            let db = 20.0 * v.norm().log10();
            assert!((db / 0.5 - (db / 0.5).round()).abs() < 1e-9, "{db}");
        }
    }

    /// Parses the table back: each row's delay and shifter have to rebuild its weight.
    fn table_rows(csv: &str) -> Vec<Vec<String>> {
        csv.lines().skip(1).map(|l| l.split(',').map(str::to_string).collect()).collect()
    }

    #[test]
    fn the_beam_table_rebuilds_its_weights() {
        let m = iso(&Layout::Line { n: 16, spacing: 0.5 });
        // Phase shifters, then delay lines, then delay between subarrays.
        for (delay, subarray) in [(Delay::PhaseOnly, None), (Delay::Element, None), (Delay::Subarray, Some((4, 1)))] {
            let beam = Beam { theta: 25.0, phi: 0.0, share: 1.0 };
            let plan = BeamPlan { beams: vec![beam, Beam::default()], hw: Hardware { delay, subarray, ..Default::default() }, ..Default::default() };
            let all = all_weights(&m, &plan, F, F, true);
            let rows = table_rows(&weights_csv(&m, &plan, F, true));
            assert_eq!(rows.len(), 2 * 16);
            for row in &rows {
                let (b, i): (usize, usize) = (row[0].parse().unwrap(), row[3].parse().unwrap());
                let (phase, delay_s, shifter): (f64, f64, f64) = (row[9].parse().unwrap(), row[10].parse::<f64>().unwrap() * 1e-12, row[11].parse().unwrap());
                let weight = all[b][i];
                // The table's phase is the weight's, and the delay and shifter make it up.
                assert!((C64::from_polar(1.0, phase.to_radians()) - weight / weight.norm()).norm() < 1e-3, "{row:?}");
                let rebuilt = C64::from_polar(1.0, -TAU * F * delay_s) * C64::from_polar(1.0, shifter.to_radians());
                assert!((rebuilt - weight / weight.norm()).norm() < 2e-2, "{delay:?} {row:?}: {rebuilt} vs {weight}");
                assert!(delay_s >= 0.0, "{row:?}");
            }
        }
        // A delay is a real delay, and an unwrapped one: an aperture's worth, many cycles long.
        let plan = BeamPlan { beams: vec![Beam { theta: 60.0, phi: 0.0, share: 1.0 }], hw: Hardware { delay: Delay::Element, ..Default::default() }, ..Default::default() };
        let rows = table_rows(&weights_csv(&m, &plan, F, true));
        let longest = rows.iter().map(|r| r[10].parse::<f64>().unwrap()).fold(0.0, f64::max) * 1e-12;
        let want = 15.0 * 0.5 * (C0 / F) * 60f64.to_radians().sin() / C0;
        assert!((longest - want).abs() < 1e-13, "{longest} vs {want}");
    }

    #[test]
    fn a_shifter_code_stays_in_range() {
        // Steps of exactly a quarter turn land on the wrap of ±180° often.
        let m = iso(&Layout::Line { n: 64, spacing: 0.5 });
        for theta in [0.0, 5.0, 30.0, 14.4775, 48.59] {
            let plan = BeamPlan { beams: vec![Beam { theta, phi: 0.0, share: 1.0 }], hw: Hardware { phase_bits: Some(3), ..Default::default() }, ..Default::default() };
            for row in table_rows(&weights_csv(&m, &plan, F, true)) {
                let code: i64 = row[12].parse().unwrap();
                assert!((0..8).contains(&code), "{row:?}");
            }
        }
    }

    #[test]
    fn a_shared_plan_rounds_the_sum_and_its_table_says_so() {
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let beams = vec![Beam { theta: 11.0, phi: 0.0, share: 1.0 }, Beam { theta: 37.0, phi: 180.0, share: 1.0 }];
        let plan = BeamPlan { beams, multibeam: MultiBeam::SharedPhaseOnly, hw: Hardware { phase_bits: Some(3), ..Default::default() }, ..Default::default() };
        let w = &all_weights(&m, &plan, F, F, true)[0];
        for v in w {
            let steps = v.arg() / (PI / 4.0);
            assert!((steps - steps.round()).abs() < 1e-9, "on the shifter's grid: {steps}");
        }
        let rows = table_rows(&weights_csv(&m, &plan, F, true));
        assert_eq!(rows.len(), 32, "one set of weights");
        assert!(rows[0][1].is_empty(), "it isn't any one beam's: {:?}", rows[0]);
    }

    #[test]
    fn digital_beams_on_subarrays_share_one_analog_aim() {
        // ±40° from one set of shifters: neither is in its subarray's beam, and it costs them gain.
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let beams = vec![Beam { theta: 40.0, phi: 0.0, share: 1.0 }, Beam { theta: 40.0, phi: 180.0, share: 1.0 }];
        let plan = BeamPlan { beams: beams.clone(), hw: Hardware { subarray: Some((8, 1)), ..Default::default() }, ..Default::default() };
        let eff = effective(&plan);
        let aim = eff.hw.analog_aim.expect("an aim was chosen");
        assert!(aim.0 < 1.0, "between them, so broadside: {aim:?}");
        let all = all_weights(&m, &plan, F, F, true);
        let solo = BeamPlan { beams: vec![beams[0]], hw: Hardware { subarray: Some((8, 1)), ..Default::default() }, ..Default::default() };
        let alone = &all_weights(&m, &solo, F, F, true)[0];
        let loss = 10.0 * (at(&m, &all[0], F, 40.0, 0.0) * 2.0 / at(&m, alone, F, 40.0, 0.0)).log10();
        assert!(loss < -3.0, "beyond the share of power: {loss}");
    }

    #[test]
    fn a_null_leaves_dark_faces_dark_and_covers_both_polarisations() {
        let l = Layout::Panels { faces: 4, nx: 6, ny: 6, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        let m = ArrayModel::new(l.elements(F), ElementKind::Isotropic, None);
        let beam = Beam { theta: 90.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { beams: vec![beam], nulls: vec![(70.0, 25.0)], cone_deg: 60.0, ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, false);
        for (v, e) in w.iter().zip(&m.elements) {
            assert_eq!(v.norm() > 0.0, e.panel == 0, "only the front face lights: panel {}", e.panel);
        }
        let depth = 10.0 * (at(&m, &w, F, 70.0, 25.0) / at(&m, &w, F, 90.0, 0.0)).log10();
        assert!(depth < -100.0, "total field, so both polarisations: {depth}");
    }

    #[test]
    fn a_null_works_around_failed_elements() {
        let (_, m) = line16();
        let beam = Beam { theta: 10.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { beams: vec![beam], nulls: vec![(35.0, 0.0)], hw: Hardware { failed: vec![2, 7], ..Default::default() }, ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        assert_eq!((w[2], w[7]), (ZERO, ZERO));
        let depth = 10.0 * (at(&m, &w, F, 35.0, 0.0) / at(&m, &w, F, 10.0, 0.0)).log10();
        assert!(depth < -100.0, "{depth}");
    }

    #[test]
    fn a_null_on_subarrays_is_made_from_the_converters_alone() {
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let beam = Beam { theta: 10.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { beams: vec![beam], nulls: vec![(40.0, 180.0)], hw: Hardware { subarray: Some((4, 1)), phase_bits: Some(4), ..Default::default() }, ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        let depth = 10.0 * (at(&m, &w, F, 40.0, 180.0) / at(&m, &w, F, 10.0, 0.0)).log10();
        assert!(depth < -100.0, "the digital part isn't rounded: {depth}");
    }

    #[test]
    fn asking_for_the_same_null_twice_is_harmless() {
        // Far deeper than any real hardware, though a singular matrix sets some floor.
        let (_, m) = line16();
        let beam = Beam { theta: 10.0, phi: 0.0, share: 1.0 };
        let plan = BeamPlan { beams: vec![beam], nulls: vec![(35.0, 0.0), (35.0, 0.0)], ..Default::default() };
        let w = beam_weights(&m, &plan, &beam, F, F, true);
        let depth = 10.0 * (at(&m, &w, F, 35.0, 0.0) / at(&m, &w, F, 10.0, 0.0)).log10();
        assert!(depth < -80.0, "{depth}");
    }

    #[test]
    fn a_shaped_beam_reads_what_the_field_does_in_any_plane() {
        let m = iso(&Layout::Line { n: 32, spacing: 0.5 });
        let mask = vec![MaskRegion { theta_lo: -15.0, theta_hi: 15.0, lo_db: Some(-1.0), hi_db: 0.0 }, MaskRegion { theta_lo: -90.0, theta_hi: -25.0, lo_db: None, hi_db: -22.0 }, MaskRegion { theta_lo: 25.0, theta_hi: 90.0, lo_db: None, hi_db: -22.0 }];
        let s = synthesize(&m, F, 0.0, &mask, false, 300).unwrap();
        let peak = s.cut.iter().map(|c| at(&m, &s.weights, F, c.0, 0.0)).fold(0.0, f64::max);
        for (t, db) in s.cut.iter().step_by(10) {
            let real = 10.0 * (at(&m, &s.weights, F, *t, 0.0) / peak).log10();
            assert!((real - db).abs() < 0.2 || *db < -60.0, "at {t}°: cut {db}, field {real}");
        }
    }

    #[test]
    fn a_mask_that_contradicts_itself_is_refused_in_words() {
        let m = iso(&Layout::Line { n: 8, spacing: 0.5 });
        let mask = vec![MaskRegion { theta_lo: -20.0, theta_hi: 20.0, lo_db: Some(-1.0), hi_db: 0.0 }, MaskRegion { theta_lo: 15.0, theta_hi: 90.0, lo_db: None, hi_db: -25.0 }];
        let e = synthesize(&m, F, 0.0, &mask, false, 10).unwrap_err().to_string();
        assert!(e.contains("at least"), "{e}");
    }

    #[test]
    fn the_stated_quantisation_lobe_is_about_six_db_per_bit() {
        for b in [3u8, 4, 5, 6] {
            let lobe = quantisation_lobe_db(b);
            assert!((lobe + 6.02 * b as f64).abs() < 1.2, "{b} bits: {lobe}");
        }
        assert_eq!(quantisation_lobe_db(1), 0.0, "one bit gives two equal beams");
    }
}
