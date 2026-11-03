//! Direction finding: from what the channels receive to where the sources are. Subspace methods
//! (MUSIC, Root-MUSIC, ESPRIT), Capon and plain beamscan, sparse-coarray MUSIC, and the classic
//! hardware methods (amplitude-free monopulse, phase interferometer, Watson-Watt, pseudo-Doppler),
//! with source counting, the Cramér-Rao bound as the yardstick, and Monte Carlo to read
//! them against it.
//!
//! The signal model: `x(t) = Σ a(dᵢ)·sᵢ(t) + n(t)`, with `aₙ(d) ∝ exp(+jk·d·rₙ)` and white noise
//! of variance σ². What's assumed: narrowband across the array, sources in the far field,
//! a known polarisation when the manifold comes from solved patterns.

use std::f64::consts::TAU;

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::aperture::{LineInfo, angle_between, angles_of, phi_hat, theta_hat, unit_dir};
use super::math::{CMat, ONE, ZERO, eigenvalues, hermitian_eig, herm, identity, mat_mul, mat_vec, norm_sqr, poly_roots, quadratic, unit, zeros};
use super::pattern::{ArrayModel, refine_peak};
use super::solved::SolvedArray;
use crate::geometry::{P3, dot};
use crate::linalg::invert;
use crate::{C0, C64};

/// An array's response to a direction. Its size carries the element's gain, if it has one.
pub trait Response: Sync {
    fn channels(&self) -> usize;
    fn a(&self, theta: f64, phi: f64) -> Vec<C64>;
}

/// The textbook manifold: isotropic elements at their positions, nothing else.
#[derive(Clone, Debug)]
pub struct Ideal {
    pub pos: Vec<P3>,
    pub k: f64,
}

impl Ideal {
    pub fn new(pos: Vec<P3>, f: f64) -> Self {
        Self { pos, k: TAU * f / C0 }
    }
}

impl Response for Ideal {
    fn channels(&self) -> usize {
        self.pos.len()
    }
    fn a(&self, theta: f64, phi: f64) -> Vec<C64> {
        let d = unit_dir(theta, phi);
        self.pos.iter().map(|r| C64::from_polar(1.0, self.k * dot(d, *r))).collect()
    }
}

/// An analytic array with its element pattern, scaled so the strongest direction has unit
/// mean amplitude: a source away from the beam of the element arrives weaker.
pub struct FromModel<'a> {
    pub model: &'a ArrayModel,
    pub f: f64,
    pub scale: f64,
}

impl<'a> FromModel<'a> {
    pub fn new(model: &'a ArrayModel, f: f64) -> Self {
        let mut top = 0.0f64;
        for i in 0..=18 {
            for j in 0..36 {
                let d = unit_dir(i as f64 * 10.0, j as f64 * 10.0);
                let v = model.response(f, d, model.reference_pol(d));
                top = top.max(norm_sqr(&v) / v.len().max(1) as f64);
            }
        }
        Self { model, f, scale: if top > 0.0 { 1.0 / top.sqrt() } else { 1.0 } }
    }
}

impl Response for FromModel<'_> {
    fn channels(&self) -> usize {
        self.model.len()
    }
    fn a(&self, theta: f64, phi: f64) -> Vec<C64> {
        let d = unit_dir(theta, phi);
        self.model.response(self.f, d, self.model.reference_pol(d)).into_iter().map(|v| v * self.scale).collect()
    }
}

/// A solved array's own response along a polarisation: coupling, mount and all. Scaled like [`FromModel`].
pub struct FromSolved<'a> {
    pub array: &'a SolvedArray,
    pub pol: Option<(C64, C64)>,
    pub scale: f64,
}

impl<'a> FromSolved<'a> {
    pub fn new(array: &'a SolvedArray, pol: Option<(C64, C64)>) -> Self {
        let mut top = 0.0f64;
        let first = &array.far[0];
        for ti in (0..first.theta.len()).step_by(4) {
            for pi in (0..first.phi.len() - 1).step_by(4) {
                let (t, p) = (first.theta[ti], first.phi[pi]);
                let use_pol = pol.unwrap_or_else(|| array.main_pol(t, p));
                let v = array.response(t, p, use_pol);
                top = top.max(norm_sqr(&v) / v.len() as f64);
            }
        }
        Self { array, pol, scale: if top > 0.0 { 1.0 / top.sqrt() } else { 1.0 } }
    }
}

impl Response for FromSolved<'_> {
    fn channels(&self) -> usize {
        self.array.ports()
    }
    fn a(&self, theta: f64, phi: f64) -> Vec<C64> {
        let (theta, phi) = if theta < 0.0 { (-theta, phi + 180.0) } else { (theta, phi) };
        let pol = self.pol.unwrap_or_else(|| self.array.main_pol(theta, phi));
        self.array.response(theta, phi, pol).into_iter().map(|v| v * self.scale).collect()
    }
}

/// What directions an estimator looks through.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Scan {
    /// θ from −90° to 90° in the plane at `phi`, which is all a line along that plane can tell.
    Plane { phi: f64 },
    /// φ all the way round at one θ: the usual HF to UHF bearing.
    Azimuth { theta: f64 },
    /// Everything out to `theta_max` off z.
    Hemisphere { theta_max: f64 },
}

impl Default for Scan {
    fn default() -> Self {
        Scan::Plane { phi: 0.0 }
    }
}

/// A manifold on a grid of directions, each vector unit norm: the "calibration table".
#[derive(Clone, Debug)]
pub struct Table {
    pub scan: Scan,
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    /// `a[θ][φ][channel]`.
    pub a: Vec<Vec<Vec<C64>>>,
    pub step: f64,
}

impl Table {
    pub fn new(resp: &dyn Response, scan: Scan, step: f64) -> Self {
        let step = step.max(0.05);
        let span = |from: f64, to: f64| -> Vec<f64> {
            let n = ((to - from) / step).round() as usize;
            (0..=n).map(|i| from + i as f64 * (to - from) / n.max(1) as f64).collect()
        };
        let round = || -> Vec<f64> {
            let n = (360.0 / step).round() as usize;
            (0..n).map(|i| i as f64 * 360.0 / n.max(1) as f64).collect()
        };
        let (theta, phi) = match scan {
            Scan::Plane { phi } => (span(-90.0, 90.0), vec![phi]),
            Scan::Azimuth { theta } => (vec![theta], round()),
            Scan::Hemisphere { theta_max } => (span(0.0, theta_max), round()),
        };
        let a: Vec<Vec<Vec<C64>>> = theta.par_iter().map(|t| phi.iter().map(|p| unit(resp.a(*t, *p))).collect()).collect();
        Self { scan, theta, phi, a, step }
    }

    pub fn is_map(&self) -> bool {
        matches!(self.scan, Scan::Hemisphere { .. })
    }

    /// A rough 1 dB beam's reach of one direction into its neighbours on this grid.
    pub fn directions(&self) -> usize {
        self.theta.len() * self.phi.len()
    }
}

/// A source: where it is, and how strong against 0 dB, the level the SNR is quoted at.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub theta: f64,
    pub phi: f64,
    pub power_db: f64,
    /// Coherent with this other source: a reflection of it, say, whatever its delay. Such a pair
    /// breaks subspace methods unless the array is smoothed first.
    pub coherent_with: Option<usize>,
    /// Phase against the source it copies, degrees.
    pub phase_deg: f64,
}

impl Default for Source {
    fn default() -> Self {
        Self { theta: 20.0, phi: 0.0, power_db: 0.0, coherent_with: None, phase_deg: 0.0 }
    }
}

/// What the channels do to it on the way in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Impair {
    /// Channel gain error, rms dB.
    pub gain_db: f64,
    /// Channel phase error, rms degrees.
    pub phase_deg: f64,
    /// Bits of each I and Q converter.
    pub adc_bits: Option<u8>,
    /// How far the rail's rms sits below full scale, dB.
    pub backoff_db: f64,
}

/// `snapshots[t][channel]`. Sources are complex Gaussian, independent unless coherent. Noise
/// has variance `noise` per channel, complex.
pub fn snapshots(resp: &dyn Response, sources: &[Source], k: usize, noise: f64, imp: &Impair, rng: &mut proof::Rng) -> Vec<Vec<C64>> {
    let n = resp.channels();
    let steering: Vec<Vec<C64>> = sources.iter().map(|s| resp.a(s.theta, s.phi)).collect();
    let gains: Vec<C64> = (0..n)
        .map(|_| {
            let g = 10f64.powf(imp.gain_db * rng.gauss() / 20.0);
            C64::from_polar(g, (imp.phase_deg * rng.gauss()).to_radians())
        })
        .collect();
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let mut out = Vec::with_capacity(k);
    for _ in 0..k {
        let mut s = vec![ZERO; sources.len()];
        for (i, src) in sources.iter().enumerate() {
            let amp = 10f64.powf(src.power_db / 20.0);
            s[i] = match src.coherent_with.filter(|c| *c < i) {
                Some(c) => {
                    let parent = 10f64.powf(sources[c].power_db / 20.0);
                    s[c] / parent.max(1e-12) * C64::from_polar(amp, src.phase_deg.to_radians())
                }
                None => C64::new(rng.gauss(), rng.gauss()) * (amp * half),
            };
        }
        let x: Vec<C64> = (0..n)
            .map(|c| {
                let signal: C64 = s.iter().zip(&steering).map(|(v, a)| *v * a[c]).sum();
                (signal * gains[c]) + C64::new(rng.gauss(), rng.gauss()) * (noise.max(0.0).sqrt() * half)
            })
            .collect();
        out.push(x);
    }
    if let Some(bits) = imp.adc_bits {
        quantise_channels(&mut out, bits, imp.backoff_db);
    }
    out
}

/// A mid-rise quantiser on each rail. Full scale on each channel is `backoff_db` above its rail rms.
pub fn quantise_channels(x: &mut [Vec<C64>], bits: u8, backoff_db: f64) {
    let Some(n) = x.first().map(Vec::len) else { return };
    for c in 0..n {
        let power: f64 = x.iter().map(|row| row[c].norm_sqr()).sum::<f64>() / x.len() as f64;
        let full = (power / 2.0).sqrt() * 10f64.powf(backoff_db / 20.0);
        if full <= 0.0 {
            continue;
        }
        let delta = 2.0 * full / 2f64.powi(bits as i32);
        let q = |v: f64| ((v / delta).floor() + 0.5).clamp(-(2f64.powi(bits as i32 - 1)) + 0.5, 2f64.powi(bits as i32 - 1) - 0.5) * delta;
        for row in x.iter_mut() {
            row[c] = C64::new(q(row[c].re), q(row[c].im));
        }
    }
}

pub fn covariance(x: &[Vec<C64>]) -> CMat {
    let n = x.first().map_or(0, Vec::len);
    let mut r = zeros(n, n);
    for row in x {
        for i in 0..n {
            for j in 0..n {
                r[i][j] += row[i] * row[j].conj();
            }
        }
    }
    let k = x.len().max(1) as f64;
    r.iter_mut().flatten().for_each(|v| *v /= k);
    r
}

/// `(R + J·R*·J)/2`: a symmetric array has a mirror image to average with, which decorrelates
/// two coherent sources unless they sit in a particular phase.
pub fn forward_backward(r: &CMat) -> CMat {
    let n = r.len();
    let mut out = zeros(n, n);
    for i in 0..n {
        for j in 0..n {
            out[i][j] = (r[i][j] + r[n - 1 - i][n - 1 - j].conj()) * 0.5;
        }
    }
    out
}

/// Spatial smoothing: averages the covariance of each overlapping subarray of `len` channels of
/// a uniform line, which restores the rank coherent sources take away (Shan, Wax and Kailath, 1985).
pub fn smooth(r: &CMat, len: usize) -> CMat {
    let n = r.len();
    let len = len.clamp(1, n);
    let count = n - len + 1;
    let mut out = zeros(len, len);
    for p in 0..count {
        for i in 0..len {
            for j in 0..len {
                out[i][j] += r[p + i][p + j];
            }
        }
    }
    out.iter_mut().flatten().for_each(|v| *v /= count as f64);
    out
}

/// How many sources the eigenvalues say are there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCount {
    pub mdl: usize,
    pub aic: usize,
}

/// Wax and Kailath's (1985) information criteria on eigenvalues given ascending, from `k`
/// snapshots: the MDL is consistent, the AIC tends to overestimate.
pub fn count_sources(eigen: &[f64], k: usize) -> SourceCount {
    let n = eigen.len();
    if n < 2 {
        return SourceCount::default();
    }
    let k = k.max(1) as f64;
    let (mut best_mdl, mut best_aic) = ((f64::INFINITY, 0), (f64::INFINITY, 0));
    for m in 0..n {
        let tail = &eigen[..n - m];
        let count = tail.len() as f64;
        let floor = 1e-12 * eigen[n - 1].abs().max(1e-300);
        let mean = tail.iter().map(|v| v.max(floor)).sum::<f64>() / count;
        let log_geo = tail.iter().map(|v| v.max(floor).ln()).sum::<f64>() / count;
        let l = k * count * (mean.ln() - log_geo);
        let free = (m * (2 * n - m)) as f64;
        let aic = 2.0 * l + 2.0 * free;
        let mdl = l + 0.5 * free * k.ln();
        if mdl < best_mdl.0 {
            best_mdl = (mdl, m);
        }
        if aic < best_aic.0 {
            best_aic = (aic, m);
        }
    }
    SourceCount { mdl: best_mdl.1, aic: best_aic.1 }
}

/// A method and whether an array can use it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    /// Delay-and-sum scanned over the table: the correlative interferometer when the table is measured.
    Beamscan,
    /// Minimum variance: sharper than a beam, needing no source count.
    Capon,
    #[default]
    Music,
    RootMusic,
    Esprit,
    /// MUSIC on a sparse line's difference coarray: more sources than sensors.
    CoarrayMusic,
    /// Sum and difference beams, one snapshot's worth of hardware.
    Monopulse,
    /// Phase comparison across baselines, the short one unwrapping the long.
    Interferometer,
    /// Adcock: four elements' pair differences give the bearing's sine and cosine.
    WatsonWatt,
    /// One receiver switched round a ring.
    PseudoDoppler,
}

impl Method {
    pub const ALL: [Method; 10] = [Method::Music, Method::Capon, Method::Beamscan, Method::RootMusic, Method::Esprit, Method::CoarrayMusic, Method::Monopulse, Method::Interferometer, Method::WatsonWatt, Method::PseudoDoppler];

    pub fn label(self) -> &'static str {
        match self {
            Method::Beamscan => "Beamscan",
            Method::Capon => "Capon",
            Method::Music => "MUSIC",
            Method::RootMusic => "Root-MUSIC",
            Method::Esprit => "ESPRIT",
            Method::CoarrayMusic => "Coarray MUSIC",
            Method::Monopulse => "Monopulse",
            Method::Interferometer => "Interferometer",
            Method::WatsonWatt => "Watson-Watt",
            Method::PseudoDoppler => "Pseudo-Doppler",
        }
    }

    /// Why this array can't use it, if it can't.
    pub fn unavailable(self, a: &DfArray) -> Option<&'static str> {
        match self {
            Method::RootMusic | Method::Esprit => (!a.line.as_ref().is_some_and(LineInfo::is_uniform)).then_some("needs a uniform line"),
            Method::CoarrayMusic => a.line.is_none().then_some("needs a line, best a sparse one"),
            Method::Interferometer => a.line.is_none().then_some("needs a line"),
            Method::WatsonWatt => (!a.ring || a.pos.len() != 4).then_some("needs a ring of 4"),
            Method::PseudoDoppler => (!a.ring || a.pos.len() < 3).then_some("needs a ring"),
            _ => None,
        }
    }

    /// Whether it has a spatial spectrum to plot, rather than just an answer.
    pub fn has_spectrum(self) -> bool {
        matches!(self, Method::Beamscan | Method::Capon | Method::Music | Method::CoarrayMusic)
    }

    /// Whether it can only report one source.
    pub fn single_source(self) -> bool {
        matches!(self, Method::Monopulse | Method::Interferometer | Method::WatsonWatt | Method::PseudoDoppler)
    }
}

/// The array as the estimators need to know it.
#[derive(Clone, Debug)]
pub struct DfArray {
    pub pos: Vec<P3>,
    pub f: f64,
    /// A line along x, with its base spacing.
    pub line: Option<LineInfo>,
    /// Elements evenly round a circle at z = 0, starting on +x.
    pub ring: bool,
}

impl DfArray {
    pub fn k(&self) -> f64 {
        TAU * self.f / C0
    }

    /// Channel `i` and channel `n − 1 − i` mirror each other through one centre, for every `i`.
    pub fn is_centro_symmetric(&self) -> bool {
        let n = self.pos.len();
        if n == 0 {
            return false;
        }
        let mid: P3 = std::array::from_fn(|k| self.pos[0][k] + self.pos[n - 1][k]);
        let size = self.pos.iter().map(|p| dot(*p, *p)).fold(0.0, f64::max).sqrt().max(1e-12);
        (0..n).all(|i| (0..3).all(|k| (self.pos[i][k] + self.pos[n - 1 - i][k] - mid[k]).abs() < 1e-9 * size))
    }

    /// How far apart two equal sources can sit and still be told apart by a beam, degrees.
    pub fn rayleigh_deg(&self) -> f64 {
        let n = self.pos.len().max(1) as f64;
        let c: P3 = std::array::from_fn(|k| self.pos.iter().map(|p| p[k]).sum::<f64>() / n);
        let mut r2 = 0.0f64;
        for p in &self.pos {
            let v: P3 = std::array::from_fn(|k| p[k] - c[k]);
            r2 = r2.max(dot(v, v));
        }
        let lambda = C0 / self.f;
        let span = 2.0 * r2.sqrt() + lambda / 2.0;
        (lambda / span).min(1.0).asin().to_degrees()
    }
}

/// Knobs every estimator shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DfOptions {
    /// Number of sources, `None` to count them by MDL.
    pub count: Option<usize>,
    pub forward_backward: bool,
    /// Subarray length for spatial smoothing, 0 for none.
    pub smooth_len: usize,
}

/// A spatial spectrum on a table's grid, peak 1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    pub p: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DfResult {
    pub method: Method,
    pub dirs: Vec<(f64, f64)>,
    pub spectrum: Option<Spectrum>,
    pub counted: SourceCount,
    /// Used for the subspace, however it was arrived at.
    pub count: usize,
    /// Covariance eigenvalues, descending, dB against the largest.
    pub eigen_db: Vec<f64>,
    /// Options that were asked for and don't apply to this method or array, in words.
    pub notes: Vec<String>,
}

/// Local maxima of a spectrum, strongest first, up to `want`.
fn find_peaks(s: &Spectrum, scan: Scan, want: usize) -> Vec<(usize, usize)> {
    let (nt, np) = (s.theta.len(), s.phi.len());
    let wrap_phi = !matches!(scan, Scan::Plane { .. });
    let mut found: Vec<(f64, usize, usize)> = Vec::new();
    for ti in 0..nt {
        for pi in 0..np {
            let v = s.p[ti][pi];
            let polar = matches!(scan, Scan::Hemisphere { .. }) && s.theta[ti].abs() < 1e-9;
            if polar && pi > 0 {
                continue;
            }
            let mut top = true;
            for dt in [-1i64, 0, 1] {
                for dp in [-1i64, 0, 1] {
                    if dt == 0 && dp == 0 {
                        continue;
                    }
                    let tj = ti as i64 + dt;
                    if tj < 0 || tj as usize >= nt {
                        continue;
                    }
                    let pj = pi as i64 + dp;
                    let pj = if wrap_phi {
                        pj.rem_euclid(np as i64) as usize
                    } else if pj < 0 || pj as usize >= np {
                        continue;
                    } else {
                        pj as usize
                    };
                    if s.p[tj as usize][pj] > v {
                        top = false;
                    }
                }
            }
            if top {
                found.push((v, ti, pi));
            }
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    // A flat top counts once, across a grid step and no further: subspace peaks can be close.
    let step_of = |v: &[f64]| if v.len() > 1 { (v[1] - v[0]).abs() } else { 0.0 };
    let same_top = 1.5 * step_of(&s.theta).max(step_of(&s.phi));
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (_, ti, pi) in found {
        let here = (s.theta[ti], s.phi[pi]);
        if !out.iter().any(|(a, b)| angle_between((s.theta[*a], s.phi[*b]), here) < same_top) {
            out.push((ti, pi));
        }
        if out.len() >= want {
            break;
        }
    }
    out
}

/// Polishes a peak off the grid with the spectrum as a function.
fn polish(scan: Scan, start: (f64, f64), step: f64, value: &(dyn Fn(f64, f64) -> f64 + Sync)) -> (f64, f64) {
    match scan {
        Scan::Plane { phi } => {
            let (mut t, mut best, mut h) = (start.0, value(start.0, phi), step);
            while h > 1e-3 {
                let mut moved = false;
                for sign in [1.0, -1.0] {
                    let v = value(t + sign * h, phi);
                    if v > best {
                        best = v;
                        t += sign * h;
                        moved = true;
                        break;
                    }
                }
                if !moved {
                    h *= 0.5;
                }
            }
            (t, phi)
        }
        Scan::Azimuth { theta } => {
            let (mut p, mut best, mut h) = (start.1, value(theta, start.1), step);
            while h > 1e-3 {
                let mut moved = false;
                for sign in [1.0, -1.0] {
                    let v = value(theta, p + sign * h);
                    if v > best {
                        best = v;
                        p += sign * h;
                        moved = true;
                        break;
                    }
                }
                if !moved {
                    h *= 0.5;
                }
            }
            (theta, p.rem_euclid(360.0))
        }
        Scan::Hemisphere { .. } => {
            let v = |d: P3| {
                let (t, p) = angles_of(d);
                value(t, p)
            };
            let (t, p, _) = refine_peak(&v, start, step);
            (t, p)
        }
    }
}

/// A direction as a signed plane angle, the way a line's plots read it.
pub fn signed_plane_angle(dir: (f64, f64), plane_phi: f64) -> f64 {
    let d = unit_dir(dir.0, dir.1);
    let n = unit_dir(90.0, plane_phi);
    dot(d, n).clamp(-1.0, 1.0).asin().to_degrees()
}

/// What MUSIC's noise subspace makes of a steering vector.
fn music_value(projector: &CMat, a: &[C64]) -> f64 {
    1.0 / quadratic(projector, a).max(1e-14)
}

/// Runs one method on the snapshots. `resp` is the manifold the method believes in; it's only
/// read off the grid to polish a peak.
pub fn estimate(method: Method, x: &[Vec<C64>], array: &DfArray, table: &Table, resp: &dyn Response, opts: &DfOptions) -> Result<DfResult> {
    if let Some(why) = method.unavailable(array) {
        bail!("{} {why}", method.label());
    }
    let n = x.first().map_or(0, Vec::len);
    if n < 2 {
        bail!("direction finding needs at least 2 channels");
    }
    let k = x.len();
    let mut r = covariance(x);
    let mut notes = Vec::new();
    // Mirroring the channels only keeps a steering vector what it was if they mirror in space.
    if opts.forward_backward {
        if array.is_centro_symmetric() {
            r = forward_backward(&r);
        } else {
            notes.push("Forward-backward averaging was left out: it needs channels symmetric about the array's middle, in order, and these aren't. It would invent mirror-image sources.".to_string());
        }
    }
    let raw_eig = hermitian_eig(&r).0;
    let eigen_db: Vec<f64> = {
        let top = raw_eig.last().copied().unwrap_or(1.0).max(1e-300);
        raw_eig.iter().rev().map(|v| crate::db10(v.max(1e-30) / top)).collect()
    };
    let counted = count_sources(&raw_eig, k);
    // Only methods that read their channels as one uniform subarray can take a shorter one.
    let can_smooth = matches!(method, Method::Beamscan | Method::Capon | Method::Music | Method::RootMusic | Method::Esprit);
    let smoothing = opts.smooth_len > 1 && opts.smooth_len < n && can_smooth && array.line.as_ref().is_some_and(LineInfo::is_uniform);
    if opts.smooth_len > 1 && !smoothing {
        notes.push(format!("Spatial smoothing was left out: {} on this array can't use it. It needs a uniform line and a subarray shorter than it.", method.label()));
    }
    if smoothing {
        r = smooth(&r, opts.smooth_len);
    }
    let size = r.len();
    let (values, vectors) = hermitian_eig(&r);
    let counted_here = if smoothing { count_sources(&values, k) } else { counted };
    // A coarray's subspace is much larger than the physical array's, which is its whole point.
    let count = match (method, opts.count) {
        (Method::CoarrayMusic, Some(c)) => c.max(1),
        (_, Some(c)) => c.min(size.saturating_sub(1)).max(1),
        (_, None) => counted_here.mdl.min(size.saturating_sub(1)),
    };
    let mut result = DfResult { method, counted, count, eigen_db, notes, ..Default::default() };
    // Search counts the peaks even if MDL said none: a spectrum can still be read.
    let peaks_wanted = count.max(1);
    let shorten = |a: &[C64]| -> Vec<C64> { if size == a.len() { a.to_vec() } else { unit(a[..size].to_vec()) } };
    match method {
        Method::Beamscan | Method::Capon | Method::Music => {
            let inverse = if method == Method::Capon {
                let load = (0..size).map(|i| r[i][i].re).sum::<f64>() / size as f64 * 1e-6;
                let mut loaded = r.clone();
                for (i, row) in loaded.iter_mut().enumerate() {
                    row[i] += load;
                }
                Some(invert(&loaded)?)
            } else {
                None
            };
            let projector = (method == Method::Music).then(|| {
                let noise = size.saturating_sub(count.max(1)).max(1);
                let mut c = zeros(size, size);
                for row in 0..size {
                    for col in 0..size {
                        c[row][col] = (0..noise).map(|m| vectors[row][m] * vectors[col][m].conj()).sum();
                    }
                }
                c
            });
            let value = |a: &[C64]| -> f64 {
                match (method, &inverse, &projector) {
                    (Method::Capon, Some(inv), _) => 1.0 / quadratic(inv, a).max(1e-300),
                    (Method::Music, _, Some(c)) => music_value(c, a),
                    _ => quadratic(&r, a).max(1e-300),
                }
            };
            let mut p: Vec<Vec<f64>> = table.a.par_iter().map(|row| row.iter().map(|a| value(&shorten(a))).collect()).collect();
            let top = p.iter().flatten().copied().fold(0.0, f64::max).max(1e-300);
            p.iter_mut().flatten().for_each(|v| *v /= top);
            let s = Spectrum { theta: table.theta.clone(), phi: table.phi.clone(), p };
            let polished = |t: f64, ph: f64| -> f64 { value(&shorten(&unit(resp.a(t, ph)))) };
            result.dirs = find_peaks(&s, table.scan, peaks_wanted).into_iter().map(|(ti, pi)| polish(table.scan, (s.theta[ti], s.phi[pi]), table.step, &polished)).collect();
            result.spectrum = Some(s);
        }
        Method::CoarrayMusic => {
            let line = array.line.as_ref().expect("checked by unavailable");
            let (spectrum, dirs) = coarray_music(&r, line, array.k(), table, count.max(1))?;
            result.spectrum = Some(spectrum);
            result.dirs = dirs;
        }
        Method::RootMusic => {
            let line = array.line.as_ref().expect("checked by unavailable");
            let want = count.max(1);
            let noise = size.saturating_sub(want).max(1);
            let mut coefficients = vec![ZERO; 2 * size - 1];
            for row in 0..size {
                for col in 0..size {
                    let c: C64 = (0..noise).map(|m| vectors[row][m] * vectors[col][m].conj()).sum();
                    // aᴴCa = Σ z^(col − row)·C[row][col], shifted up to make a polynomial.
                    coefficients[col + size - 1 - row] += c;
                }
            }
            let mut roots: Vec<C64> = poly_roots(&coefficients).into_iter().filter(|z| z.norm() <= 1.0 + 1e-9).collect();
            roots.sort_by(|a, b| (1.0 - a.norm()).abs().total_cmp(&(1.0 - b.norm()).abs()));
            let kd = array.k() * line.spacing;
            result.dirs = roots.iter().take(want).map(|z| plane_direction(z.arg() / kd, table.scan)).collect();
        }
        Method::Esprit => {
            let line = array.line.as_ref().expect("checked by unavailable");
            let want = count.max(1).min(size.saturating_sub(1));
            let signal: CMat = vectors.iter().map(|row| row[size - want..].to_vec()).collect();
            let upper: CMat = signal[..size - 1].to_vec();
            let lower: CMat = signal[1..].to_vec();
            let gram = invert(&mat_mul(&herm(&upper), &upper))?;
            let phi = mat_mul(&gram, &mat_mul(&herm(&upper), &lower));
            let kd = array.k() * line.spacing;
            result.dirs = eigenvalues(&phi).iter().map(|z| plane_direction(z.arg() / kd, table.scan)).collect();
        }
        Method::Monopulse => {
            result.dirs = vec![monopulse(&r, array, table)?];
        }
        Method::Interferometer => {
            let line = array.line.as_ref().expect("checked by unavailable");
            result.dirs = vec![plane_direction(interferometer(&r, line, array.k()), table.scan)];
        }
        Method::WatsonWatt => {
            let theta = if let Scan::Azimuth { theta } = table.scan { theta } else { 90.0 };
            result.dirs = vec![(theta, watson_watt(&r))];
        }
        Method::PseudoDoppler => {
            let theta = if let Scan::Azimuth { theta } = table.scan { theta } else { 90.0 };
            result.dirs = vec![(theta, pseudo_doppler(&r))];
        }
    }
    Ok(result)
}

/// A direction cosine along x as a direction the scan allows: a line only knows the cone, and
/// the scan says where on it. Off the scan's reach it clamps to the nearest it can reach.
fn plane_direction(u: f64, scan: Scan) -> (f64, f64) {
    match scan {
        Scan::Plane { phi } => {
            let along = phi.to_radians().cos();
            let s = if along.abs() > 1e-6 { (u / along).clamp(-1.0, 1.0) } else { 0.0 };
            (s.asin().to_degrees(), phi)
        }
        Scan::Azimuth { theta } => {
            let across = theta.to_radians().sin();
            let c = if across.abs() > 1e-6 { (u / across).clamp(-1.0, 1.0) } else { 0.0 };
            (theta, c.acos().to_degrees())
        }
        Scan::Hemisphere { .. } => (u.clamp(-1.0, 1.0).asin().to_degrees(), 0.0),
    }
}

/// MUSIC on the difference coarray of a line (Pal and Vaidyanathan, 2010; Liu and Vaidyanathan,
/// 2015): each lag's covariance is averaged into a Toeplitz matrix that looks like a much
/// longer uniform line's.
pub fn coarray_music(r: &CMat, line: &LineInfo, k: f64, table: &Table, count: usize) -> Result<(Spectrum, Vec<(f64, f64)>)> {
    let (lags, hole_free) = super::aperture::coarray(&line.index);
    let hole_free = hole_free.max(1) as usize;
    let mut value = vec![ZERO; hole_free + 1];
    for (lag, _) in &lags {
        if *lag < 0 || *lag as usize > hole_free {
            continue;
        }
        let (mut sum, mut pairs) = (ZERO, 0.0);
        for (i, a) in line.index.iter().enumerate() {
            for (j, b) in line.index.iter().enumerate() {
                if a - b == *lag {
                    sum += r[i][j];
                    pairs += 1.0;
                }
            }
        }
        value[*lag as usize] = sum / pairs;
    }
    let size = hole_free + 1;
    let mut t = zeros(size, size);
    for i in 0..size {
        for j in 0..size {
            t[i][j] = if i >= j { value[i - j] } else { value[j - i].conj() };
        }
    }
    let (_, vectors) = hermitian_eig(&t);
    let noise = size.saturating_sub(count).max(1);
    let mut c = zeros(size, size);
    for row in 0..size {
        for col in 0..size {
            c[row][col] = (0..noise).map(|m| vectors[row][m] * vectors[col][m].conj()).sum();
        }
    }
    let kd = k * line.spacing;
    let phase_of = |t: f64, p: f64| dot(unit_dir(t, p), [1.0, 0.0, 0.0]) * kd;
    let at = |t: f64, p: f64| -> f64 {
        let step = phase_of(t, p);
        let a: Vec<C64> = (0..size).map(|m| C64::from_polar(1.0 / (size as f64).sqrt(), m as f64 * step)).collect();
        music_value(&c, &a)
    };
    let mut p: Vec<Vec<f64>> = table.theta.par_iter().map(|th| table.phi.iter().map(|ph| at(*th, *ph)).collect()).collect();
    let top = p.iter().flatten().copied().fold(0.0, f64::max).max(1e-300);
    p.iter_mut().flatten().for_each(|v| *v /= top);
    let s = Spectrum { theta: table.theta.clone(), phi: table.phi.clone(), p };
    let dirs = find_peaks(&s, table.scan, count).into_iter().map(|(ti, pi)| polish(table.scan, (s.theta[ti], s.phi[pi]), table.step, &at)).collect();
    Ok((s, dirs))
}

/// Sum and difference beams, steered at the strongest beamscan direction: the ratio reads the
/// offset within the beam directly (`Δ/Σ = j·tan(k·D·Δu/2)` for two halves `D` apart).
fn monopulse(r: &CMat, array: &DfArray, table: &Table) -> Result<(f64, f64)> {
    let k = array.k();
    let value = |a: &[C64]| quadratic(r, a);
    let mut best = (f64::MIN, (0.0, 0.0));
    for (ti, row) in table.a.iter().enumerate() {
        for (pi, a) in row.iter().enumerate() {
            let v = value(a);
            if v > best.0 {
                best = (v, (table.theta[ti], table.phi[pi]));
            }
        }
    }
    let steer = best.1;
    let d0 = unit_dir(steer.0, steer.1);
    let n = array.pos.len() as f64;
    let c: P3 = std::array::from_fn(|i| array.pos.iter().map(|p| p[i]).sum::<f64>() / n);
    let weights: Vec<C64> = array.pos.iter().map(|p| C64::from_polar(1.0, -k * dot(d0, *p))).collect();
    // E[(wᵀx)(vᵀx)*] = wᵀ·R·v*.
    let cross = |w: &[C64], v: &[C64]| -> C64 {
        let conj: Vec<C64> = v.iter().map(|x| x.conj()).collect();
        w.iter().zip(&mat_vec(r, &conj)).map(|(a, b)| *a * *b).sum()
    };
    let axis_offset = |axis: usize| -> Option<f64> {
        let side = |sign: f64| -> (Vec<C64>, f64, f64) {
            let (mut w, mut sum, mut count) = (Vec::with_capacity(array.pos.len()), 0.0, 0.0);
            for (p, v) in array.pos.iter().zip(&weights) {
                let x = p[axis] - c[axis];
                if x * sign > 1e-12 {
                    w.push(*v);
                    sum += x;
                    count += 1.0;
                } else if x.abs() <= 1e-12 {
                    // The middle column lies on the seam and takes half in each.
                    w.push(*v * 0.5);
                    count += 0.5;
                } else {
                    w.push(ZERO);
                }
            }
            (w, sum, count)
        };
        let (a, sa, ca) = side(1.0);
        let (b, sb, cb) = side(-1.0);
        if ca == 0.0 || cb == 0.0 || sa == 0.0 {
            return None;
        }
        let spread = sa / ca - sb / cb;
        let sum: Vec<C64> = a.iter().zip(&b).map(|(x, y)| *x + *y).collect();
        let diff: Vec<C64> = a.iter().zip(&b).map(|(x, y)| *x - *y).collect();
        let ratio = cross(&diff, &sum) / cross(&sum, &sum);
        Some(2.0 / (k * spread) * ratio.im.atan())
    };
    let mut d = d0;
    if let Some(du) = axis_offset(0) {
        d[0] += du;
    }
    if let Some(dv) = axis_offset(1) {
        d[1] += dv;
    }
    let side = d[0] * d[0] + d[1] * d[1];
    if side >= 1.0 {
        return Ok(steer);
    }
    d[2] = (1.0 - side).sqrt().copysign(d0[2]);
    Ok(angles_of(d))
}

/// A line's phase differences, shortest baseline first, each resolving the next's ambiguity,
/// then a least-squares slope across the lot.
fn interferometer(r: &CMat, line: &LineInfo, k: f64) -> f64 {
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for (i, a) in line.index.iter().enumerate().skip(1) {
        pairs.push(((*a - line.index[0]) as f64 * line.spacing, r[i][0].arg()));
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut u = 0.0;
    let (mut top, mut bottom) = (0.0, 0.0);
    for (i, (b, psi)) in pairs.iter().enumerate() {
        let unwrapped = if i == 0 {
            *psi
        } else {
            let turns = ((k * b * u - psi) / TAU).round();
            psi + turns * TAU
        };
        top += b * unwrapped;
        bottom += b * b;
        u = top / bottom / k;
    }
    u
}

/// Bearing from four elements on E, N, W, S: `tan φ = (N − S)/(E − W)`, each referred to their sum.
pub fn watson_watt(r: &CMat) -> f64 {
    // Channels in ring order: E, N, W, S.
    let sum = [ONE, ONE, ONE, ONE];
    let cross = |pair: [C64; 4]| -> C64 {
        let conj: Vec<C64> = sum.iter().map(|v| v.conj()).collect();
        pair.iter().zip(&mat_vec(r, &conj)).map(|(a, b)| *a * *b).sum()
    };
    let ew = cross([ONE, ZERO, -ONE, ZERO]);
    let ns = cross([ZERO, ONE, ZERO, -ONE]);
    ns.im.atan2(ew.im).to_degrees().rem_euclid(360.0)
}

/// Bearing from phase steps round a ring, as one receiver sees them being switched in turn:
/// the steps' first harmonic is the bearing, a quarter turn back.
pub fn pseudo_doppler(r: &CMat) -> f64 {
    let n = r.len();
    let step = TAU / n as f64;
    let mut c = ZERO;
    for i in 0..n {
        let next = (i + 1) % n;
        let delta = r[next][i].arg();
        c += C64::from_polar(delta, (i as f64 + 0.5) * step);
    }
    (c.arg().to_degrees() + 90.0).rem_euclid(360.0)
}

/// A one-receiver array that switches between elements in slots, as a BLE AoA locator does. The
/// reference slots before switching measure the oscillator's offset, which would otherwise read as angle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Switched {
    /// Time from one switched sample to the next, seconds.
    pub slot_s: f64,
    /// Transmitter against receiver, Hz. BLE allows up to ±150 kHz.
    pub cfo_hz: f64,
    /// Samples on the first element to measure it with.
    pub reference: usize,
    /// How many times round the array.
    pub rounds: usize,
    /// Take the offset out. Off, it shows what that costs.
    pub correct: bool,
}

impl Default for Switched {
    /// As a BLE locator: 2 µs slots, a 40 kHz offset, eight reference samples, four rounds.
    fn default() -> Self {
        Self { slot_s: 2e-6, cfo_hz: 40e3, reference: 8, rounds: 4, correct: true }
    }
}

/// Builds one snapshot per round from a single switched receiver.
pub fn switched_snapshots(resp: &dyn Response, source: &Source, noise: f64, sw: &Switched, rng: &mut proof::Rng) -> Vec<Vec<C64>> {
    let n = resp.channels();
    let a = resp.a(source.theta, source.phi);
    let carrier = C64::from_polar(10f64.powf(source.power_db / 20.0), rng.uniform() * TAU);
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let noisy = |t: f64, c: usize, rng: &mut proof::Rng| -> C64 { carrier * a[c] * C64::from_polar(1.0, TAU * sw.cfo_hz * t) + C64::new(rng.gauss(), rng.gauss()) * (noise.max(0.0).sqrt() * half) };
    let reference: Vec<C64> = (0..sw.reference).map(|i| noisy(i as f64 * sw.slot_s, 0, rng)).collect();
    // The average phase step per slot, with its wrap kept straight by a running product.
    let step: C64 = reference.windows(2).map(|w| w[1] * w[0].conj()).sum();
    let measured = if sw.correct && reference.len() > 1 { step.arg() / (TAU * sw.slot_s) } else { 0.0 };
    let mut time = sw.reference as f64 * sw.slot_s;
    let mut out = Vec::with_capacity(sw.rounds);
    for _ in 0..sw.rounds.max(1) {
        let mut row = Vec::with_capacity(n);
        for c in 0..n {
            let v = noisy(time, c, rng);
            row.push(v * C64::from_polar(1.0, -TAU * measured * time));
            time += sw.slot_s;
        }
        out.push(row);
    }
    out
}

/// The stochastic Cramér-Rao bound on each source's direction error, degrees: the angle on
/// the sphere, in the directions `scan` leaves free. `CRB = σ²/2K · {Re[(DᴴP⊥D) ⊙ (S Aᴴ R⁻¹ A S)ᵀ]}⁻¹`
/// (Stoica and Nehorai, 1990; Stoica, Larsson and Gershman, 2001).
pub fn crb_deg(resp: &dyn Response, sources: &[Source], noise: f64, k: usize, scan: Scan) -> Result<Vec<f64>> {
    let m = sources.len();
    let n = resp.channels();
    if m == 0 || n == 0 {
        bail!("nothing to bound");
    }
    let a_mat: Vec<Vec<C64>> = sources.iter().map(|s| resp.a(s.theta, s.phi)).collect();
    let a: CMat = (0..n).map(|r| (0..m).map(|c| a_mat[c][r]).collect()).collect();
    // Coherent sources share a waveform.
    let mut s_mat = zeros(m, m);
    let root = |i: usize| sources[i].coherent_with.filter(|c| *c < i).unwrap_or(i);
    for i in 0..m {
        for j in 0..m {
            if root(i) == root(j) {
                let (pi, pj) = (10f64.powf(sources[i].power_db / 20.0), 10f64.powf(sources[j].power_db / 20.0));
                let phase = |x: usize| if sources[x].coherent_with.is_some() { sources[x].phase_deg.to_radians() } else { 0.0 };
                s_mat[i][j] = C64::from_polar(pi * pj, phase(i) - phase(j));
            }
        }
    }
    let r_in = {
        let mid = mat_mul(&a, &mat_mul(&s_mat, &herm(&a)));
        let mut r = mid;
        for (i, row) in r.iter_mut().enumerate() {
            row[i] += noise;
        }
        r
    };
    let inv = invert(&r_in)?;
    // Derivatives along a direction's free tangents: moving along θ̂ or φ̂ by one radian.
    let h = 0.01f64;
    let tangent_derivative = |s: &Source, t: P3| -> Vec<C64> {
        let d = unit_dir(s.theta, s.phi);
        let moved = |sign: f64| -> Vec<C64> {
            let v: P3 = std::array::from_fn(|i| d[i] + sign * h.to_radians() * t[i]);
            let (th, ph) = angles_of(v);
            resp.a(th, ph)
        };
        let (plus, minus) = (moved(1.0), moved(-1.0));
        plus.iter().zip(&minus).map(|(x, y)| (*x - *y) / (2.0 * h.to_radians())).collect()
    };
    let mut d_cols: Vec<Vec<C64>> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let (ta, pa) = (theta_hat(s.theta, s.phi), phi_hat(s.phi));
        match scan {
            Scan::Plane { .. } => {
                d_cols.push(tangent_derivative(s, ta));
                owner.push(i);
            }
            Scan::Azimuth { .. } => {
                d_cols.push(tangent_derivative(s, pa));
                owner.push(i);
            }
            Scan::Hemisphere { .. } => {
                d_cols.push(tangent_derivative(s, ta));
                owner.push(i);
                d_cols.push(tangent_derivative(s, pa));
                owner.push(i);
            }
        }
    }
    // P⊥ = I − A(AᴴA)⁻¹Aᴴ.
    let gram_inv = invert(&mat_mul(&herm(&a), &a))?;
    let proj = mat_mul(&a, &mat_mul(&gram_inv, &herm(&a)));
    let mut perp = identity(n);
    for r in 0..n {
        for c in 0..n {
            perp[r][c] -= proj[r][c];
        }
    }
    let p = d_cols.len();
    let d_row: CMat = (0..n).map(|r| (0..p).map(|c| d_cols[c][r]).collect()).collect();
    let dpd = mat_mul(&herm(&d_row), &mat_mul(&perp, &d_row));
    let core = mat_mul(&s_mat, &mat_mul(&herm(&a), &mat_mul(&inv, &mat_mul(&a, &s_mat))));
    let mut fisher = zeros(p, p);
    for i in 0..p {
        for j in 0..p {
            fisher[i][j] = C64::new((dpd[i][j] * core[owner[j]][owner[i]]).re, 0.0);
        }
    }
    let inverse = invert(&fisher)?;
    let scale = noise / (2.0 * k.max(1) as f64);
    let mut out = vec![0.0; m];
    for i in 0..p {
        out[owner[i]] += inverse[i][i].re * scale;
    }
    Ok(out.into_iter().map(|v| v.max(0.0).sqrt().to_degrees()).collect())
}

/// Each true direction paired with an estimate so the total squared error is least, by
/// trying the orderings for a few sources and nearest-first beyond. `None` where one was missed.
pub fn pair_up(truth: &[(f64, f64)], found: &[(f64, f64)]) -> Vec<Option<f64>> {
    let n = truth.len();
    if n == 0 {
        return Vec::new();
    }
    if n <= 6 && found.len() <= 8 {
        fn search(truth: &[(f64, f64)], found: &[(f64, f64)], i: usize, used: &mut Vec<bool>, cost: f64, current: &mut Vec<Option<f64>>, best: &mut (f64, Vec<Option<f64>>)) {
            if cost >= best.0 {
                return;
            }
            if i == truth.len() {
                *best = (cost, current.clone());
                return;
            }
            let free = used.iter().filter(|u| !**u).count();
            for j in 0..found.len() {
                if !used[j] {
                    used[j] = true;
                    let e = angle_between(truth[i], found[j]);
                    current[i] = Some(e);
                    search(truth, found, i + 1, used, cost + e * e, current, best);
                    used[j] = false;
                }
            }
            // With fewer found than true, any of them can be the one missed: a miss costs
            // more than any real pairing, so only as many as there must be.
            if free < truth.len() - i {
                current[i] = None;
                search(truth, found, i + 1, used, cost + 1e6, current, best);
            }
        }
        let mut best = (f64::INFINITY, vec![None; n]);
        search(truth, found, 0, &mut vec![false; found.len()], 0.0, &mut vec![None; n], &mut best);
        return best.1;
    }
    let mut used = vec![false; found.len()];
    truth
        .iter()
        .map(|t| {
            let pick = found.iter().enumerate().filter(|(j, _)| !used[*j]).min_by(|a, b| angle_between(*t, *a.1).total_cmp(&angle_between(*t, *b.1)));
            pick.map(|(j, f)| {
                used[j] = true;
                angle_between(*t, *f)
            })
        })
        .collect()
}

/// One SNR's worth of trials.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct McPoint {
    pub snr_db: f64,
    /// Root mean square error over sources found in resolved trials, degrees. `NaN` if none was.
    pub rmse_deg: f64,
    pub p95_deg: f64,
    /// How often every source came out within half their separation.
    pub resolved: f64,
    /// The bound at this SNR, degrees.
    pub crb_deg: f64,
}

/// What a Monte Carlo study is run on.
pub struct McSetup<'a> {
    /// What's really out there: can be a solved array.
    pub truth: &'a dyn Response,
    /// What the estimator believes.
    pub belief: &'a dyn Response,
    pub table: &'a Table,
    pub array: &'a DfArray,
    pub sources: &'a [Source],
    pub snapshots: usize,
    pub options: DfOptions,
    pub impair: Impair,
    pub trials: usize,
    pub seed: u64,
}

/// `trials` draws at one SNR. Noise variance is `10^(−snr/10)`, so a 0 dB source of unit
/// element gain reads that SNR per channel.
pub fn monte_carlo(setup: &McSetup, method: Method, snr_db: f64) -> Result<McPoint> {
    let truth: Vec<(f64, f64)> = setup.sources.iter().map(|s| (s.theta, s.phi)).collect();
    let noise = 10f64.powf(-snr_db / 10.0);
    let separation = {
        let mut sep = f64::INFINITY;
        for i in 0..truth.len() {
            for j in 0..i {
                sep = sep.min(angle_between(truth[i], truth[j]));
            }
        }
        sep
    };
    // A wrong peak well outside a beam is a failure to resolve, not a large error.
    let tolerance = if truth.len() > 1 { (separation * 0.5).min(setup.array.rayleigh_deg()) } else { setup.array.rayleigh_deg() };
    let rows: Vec<Result<(Vec<Option<f64>>, bool)>> = (0..setup.trials.max(1))
        .into_par_iter()
        .map(|t| {
            let mut rng = proof::Rng::new(setup.seed.wrapping_add(7919 * t as u64 + 1));
            let x = snapshots(setup.truth, setup.sources, setup.snapshots, noise, &setup.impair, &mut rng);
            let r = estimate(method, &x, setup.array, setup.table, setup.belief, &setup.options)?;
            let pairs = pair_up(&truth, &r.dirs);
            let ok = pairs.iter().all(|e| e.is_some_and(|v| v <= tolerance));
            Ok((pairs, ok))
        })
        .collect();
    let (mut errors, mut good) = (Vec::new(), 0usize);
    let total = rows.len();
    for row in rows {
        let (pairs, ok) = row?;
        if ok {
            good += 1;
            errors.extend(pairs.into_iter().flatten());
        }
    }
    errors.sort_by(f64::total_cmp);
    let rmse = if errors.is_empty() { f64::NAN } else { (errors.iter().map(|e| e * e).sum::<f64>() / errors.len() as f64).sqrt() };
    let p95 = errors.get(((errors.len() as f64 * 0.95) as usize).min(errors.len().saturating_sub(1))).copied().unwrap_or(f64::NAN);
    let crb = crb_deg(setup.truth, setup.sources, noise, setup.snapshots, setup.table.scan).ok().map_or(f64::NAN, |v| (v.iter().map(|x| x * x).sum::<f64>() / v.len().max(1) as f64).sqrt());
    Ok(McPoint { snr_db, rmse_deg: rmse, p95_deg: p95, resolved: good as f64 / total.max(1) as f64, crb_deg: crb })
}

/// Where else the array could believe a source is. A level near 1 means two directions look
/// the same to it: a grating lobe, or a line's cone. `(level, one direction, the other)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ambiguity {
    pub level: f64,
    pub a: (f64, f64),
    pub b: (f64, f64),
}

pub fn ambiguity(table: &Table, array: &DfArray) -> Ambiguity {
    let apart = (2.0 * array.rayleigh_deg()).max(2.0 * table.step);
    // A plane scan is judged within ±60°: at λ/2, +90° and −90° are one phase by nature, and the
    // beams that wrap across end-fire reach well inside it, on every array, however good.
    let plane = matches!(table.scan, Scan::Plane { .. });
    let dirs: Vec<(f64, f64, &Vec<C64>)> = table.theta.iter().enumerate().filter(|(_, t)| !plane || t.abs() <= 60.0).flat_map(|(ti, t)| table.phi.iter().enumerate().map(move |(pi, p)| (*t, *p, &table.a[ti][pi]))).collect();
    let step = (dirs.len() / 1500).max(1);
    let sampled: Vec<&(f64, f64, &Vec<C64>)> = dirs.iter().step_by(step).collect();
    sampled
        .par_iter()
        .enumerate()
        .map(|(i, x)| {
            let mut best = Ambiguity::default();
            for y in sampled.iter().skip(i + 1) {
                if angle_between((x.0, x.1), (y.0, y.1)) < apart {
                    continue;
                }
                let v: C64 = x.2.iter().zip(y.2.iter()).map(|(p, q)| p.conj() * *q).sum();
                let level = v.norm_sqr();
                if level > best.level {
                    best = Ambiguity { level, a: (x.0, x.1), b: (y.0, y.1) };
                }
            }
            best
        })
        .reduce(Ambiguity::default, |a, b| if b.level > a.level { b } else { a })
}

/// The point nearest a set of bearing lines from separated stations, weighting each by its
/// accuracy. `stations`: `(position, direction (θ, φ), rms error degrees)`. Also the error
/// radius, 1σ, from those accuracies and the geometry.
pub fn intersect_bearings(stations: &[(P3, (f64, f64), f64)]) -> Result<(P3, f64)> {
    if stations.len() < 2 {
        bail!("a fix needs at least 2 bearings");
    }
    // Each line contributes (I − d·dᵀ)/σ²; the point solves the sum.
    let mut a = [[0.0f64; 3]; 3];
    let mut b = [0.0f64; 3];
    let mut per_range = 0.0f64;
    for (p, dir, sigma) in stations {
        let d = unit_dir(dir.0, dir.1);
        let w = 1.0 / sigma.max(1e-3).to_radians().powi(2);
        for r in 0..3 {
            for c in 0..3 {
                let v = (if r == c { 1.0 } else { 0.0 } - d[r] * d[c]) * w;
                a[r][c] += v;
                b[r] += v * p[c];
            }
        }
        per_range += w;
    }
    // Parallel lines have no one nearest point along them, so a hair of pull to their mean keeps
    // the solve defined; the spread below is what says whether the fix means anything.
    let n = stations.len() as f64;
    let mean: P3 = std::array::from_fn(|k| stations.iter().map(|s| s.0[k]).sum::<f64>() / n);
    let ridge = per_range * 1e-9;
    for r in 0..3 {
        a[r][r] += ridge;
        b[r] += ridge * mean[r];
    }
    let m: CMat = a.iter().map(|row| row.iter().map(|v| C64::new(*v, 0.0)).collect()).collect();
    let inv = invert(&m)?;
    let rhs: Vec<C64> = b.iter().map(|v| C64::new(*v, 0.0)).collect();
    let x = mat_vec(&inv, &rhs);
    let at: P3 = std::array::from_fn(|k| x[k].re);
    // The fix's spread: each σ is an angle, so σ·range is metres across that line, and the
    // inverse of the summed information is its covariance. Every axis counts, the weakest most.
    let mut info = zeros(3, 3);
    for (p, dir, sigma) in stations {
        let d = unit_dir(dir.0, dir.1);
        let v: P3 = std::array::from_fn(|k| at[k] - p[k]);
        let w = 1.0 / (sigma.max(1e-3).to_radians().powi(2) * dot(v, v).max(1e-6));
        for r in 0..3 {
            for c in 0..3 {
                info[r][c] += C64::new((if r == c { 1.0 } else { 0.0 } - d[r] * d[c]) * w, 0.0);
            }
        }
    }
    let (values, _) = hermitian_eig(&info);
    if values[0] < 1e-5 * values[2] {
        bail!("These bearings are nearly parallel, so they don't cross anywhere that means anything.");
    }
    Ok((at, values.iter().map(|v| 1.0 / v).sum::<f64>().sqrt()))
}

/// The bearing Watson-Watt reports for a source, by the formula rather than by simulation:
/// the octantal error of a finite Adcock against its true bearing.
pub fn adcock_bearing(true_phi_deg: f64, radius_lambda: f64, theta_deg: f64) -> f64 {
    let k_r = TAU * radius_lambda * theta_deg.to_radians().sin();
    let (s, c) = true_phi_deg.to_radians().sin_cos();
    (k_r * s).sin().atan2((k_r * c).sin()).to_degrees().rem_euclid(360.0)
}

/// An angle in degrees as −180..180.
pub fn wrap_deg(v: f64) -> f64 {
    let w = v.rem_euclid(360.0);
    if w > 180.0 { w - 360.0 } else { w }
}

#[cfg(test)]
mod tests {
    use super::super::aperture::{Layout, Sparse};
    use super::*;

    const F: f64 = 3e9;

    fn lambda() -> f64 {
        C0 / F
    }

    fn line_array(n: usize) -> (DfArray, Ideal) {
        let l = Layout::Line { n, spacing: 0.5 };
        let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
        (DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false }, Ideal::new(pos, F))
    }

    fn ring_array(n: usize, radius: f64) -> (DfArray, Ideal) {
        let pos: Vec<P3> = Layout::Ring { n, radius, radial: false }.elements(F).iter().map(|e| e.pos).collect();
        (DfArray { pos: pos.clone(), f: F, line: None, ring: true }, Ideal::new(pos, F))
    }

    fn source(theta: f64) -> Source {
        Source { theta, ..Default::default() }
    }

    fn plane_sources(list: &[f64]) -> Vec<Source> {
        // A negative plane angle is the other half of the plane.
        list.iter().map(|t| if *t < 0.0 { Source { theta: -*t, phi: 180.0, ..Default::default() } } else { source(*t) }).collect()
    }

    fn take(method: Method, sources: &[Source], array: &DfArray, resp: &Ideal, snr: f64, k: usize, opts: DfOptions, seed: u64) -> DfResult {
        let mut rng = proof::Rng::new(seed);
        let x = snapshots(resp, sources, k, 10f64.powf(-snr / 10.0), &Impair::default(), &mut rng);
        let table = Table::new(resp, Scan::Plane { phi: 0.0 }, 1.0);
        estimate(method, &x, array, &table, resp, &opts).unwrap()
    }

    fn assert_found(found: &[(f64, f64)], truth: &[Source], tol: f64) {
        let t: Vec<(f64, f64)> = truth.iter().map(|s| (s.theta, s.phi)).collect();
        let pairs = pair_up(&t, found);
        for (i, e) in pairs.iter().enumerate() {
            assert!(e.is_some_and(|v| v < tol), "source {i}: {pairs:?} in {found:?} of {t:?}");
        }
    }

    #[test]
    fn every_method_finds_one_source() {
        let (array, resp) = line_array(16);
        let truth = [source(23.0)];
        for m in [Method::Beamscan, Method::Capon, Method::Music, Method::RootMusic, Method::Esprit, Method::Monopulse, Method::Interferometer] {
            let r = take(m, &truth, &array, &resp, 20.0, 200, DfOptions { count: Some(1), ..Default::default() }, 1);
            assert_found(&r.dirs, &truth, 0.25);
        }
    }

    #[test]
    fn the_covariance_has_the_expected_signal_and_noise_levels() {
        let (_, resp) = line_array(8);
        let mut rng = proof::Rng::new(3);
        let snr = 10.0;
        let x = snapshots(&resp, &[source(20.0)], 20000, 10f64.powf(-snr / 10.0), &Impair::default(), &mut rng);
        let r = covariance(&x);
        let (values, _) = hermitian_eig(&r);
        // Top eigenvalue: N·P + σ² = 8.1; the rest σ² = 0.1.
        assert!((values[7] - 8.1).abs() < 0.2, "{values:?}");
        for v in &values[..7] {
            assert!((v - 0.1).abs() < 0.01, "{values:?}");
        }
    }

    #[test]
    fn two_close_sources_beat_the_beam_with_subspace_methods() {
        // 12 elements at λ/2 have a 9.5° Rayleigh limit. Sources 5° apart stay one lump to a
        // beam, and come apart with MUSIC, Root-MUSIC and ESPRIT.
        let (array, resp) = line_array(12);
        let truth = [source(10.0), source(15.0)];
        let opts = DfOptions { count: Some(2), ..Default::default() };
        let beam = take(Method::Beamscan, &truth, &array, &resp, 25.0, 400, opts, 2);
        let found = pair_up(&[(10.0, 0.0), (15.0, 0.0)], &beam.dirs);
        assert!(found.iter().any(|e| e.is_none_or(|v| v > 1.0)), "a beam alone shouldn't split these: {:?}", beam.dirs);
        for m in [Method::Music, Method::RootMusic, Method::Esprit] {
            let r = take(m, &truth, &array, &resp, 25.0, 400, opts, 2);
            assert_found(&r.dirs, &truth, 0.5);
        }
    }

    #[test]
    fn mdl_counts_the_sources() {
        let (_, resp) = line_array(10);
        for (list, want) in [(vec![10.0], 1), (vec![10.0, 40.0], 2), (vec![-30.0, 5.0, 45.0], 3)] {
            let mut rng = proof::Rng::new(want as u64);
            let x = snapshots(&resp, &plane_sources(&list), 400, 0.05, &Impair::default(), &mut rng);
            let (values, _) = hermitian_eig(&covariance(&x));
            assert_eq!(count_sources(&values, 400).mdl, want, "{list:?}: {values:?}");
        }
    }

    #[test]
    fn music_tells_nothing_apart_without_smoothing_when_sources_are_coherent() {
        // A direct path and its reflection: the same waveform twice, so the signal subspace has
        // rank 1. Smoothing restores it.
        let (array, resp) = line_array(12);
        let truth = [source(5.0), Source { theta: 14.0, phi: 0.0, coherent_with: Some(0), phase_deg: 70.0, ..Default::default() }];
        let plain = take(Method::Music, &truth, &array, &resp, 25.0, 400, DfOptions { count: Some(2), ..Default::default() }, 5);
        let pairs = pair_up(&[(5.0, 0.0), (14.0, 0.0)], &plain.dirs);
        let plain_worst = pairs.iter().map(|e| e.unwrap_or(90.0)).fold(0.0, f64::max);
        assert!(plain_worst > 1.0, "{:?}", plain.dirs);
        let fixed = take(Method::Music, &truth, &array, &resp, 25.0, 400, DfOptions { count: Some(2), forward_backward: true, smooth_len: 8 }, 5);
        assert_found(&fixed.dirs, &truth, 0.6);
    }

    #[test]
    fn more_sources_than_sensors_with_a_nested_line() {
        // 6 sensors, nested 3+3: 11 consecutive lags, so up to 11 uncorrelated sources in
        // principle. 7 here, more than an ordinary 6-element array's limit of 5.
        let l = Layout::SparseLine { kind: Sparse::Nested { n1: 3, n2: 3 }, spacing: 0.5 };
        let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
        let array = DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false };
        let resp = Ideal::new(pos, F);
        let truth = plane_sources(&[-45.0, -30.0, -15.0, 0.0, 15.0, 30.0, 45.0]);
        let mut rng = proof::Rng::new(6);
        let x = snapshots(&resp, &truth, 4000, 0.03, &Impair::default(), &mut rng);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 0.5);
        let r = estimate(Method::CoarrayMusic, &x, &array, &table, &resp, &DfOptions { count: Some(7), ..Default::default() }).unwrap();
        assert_found(&r.dirs, &truth, 1.5);
    }

    #[test]
    fn minimum_redundancy_has_no_coarray_holes_and_finds_sources() {
        let l = Layout::SparseLine { kind: Sparse::MinimumRedundancy { n: 5 }, spacing: 0.5 };
        let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
        let array = DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false };
        let resp = Ideal::new(pos, F);
        let truth = plane_sources(&[-40.0, -15.0, 8.0, 35.0, 60.0, -60.0]);
        let mut rng = proof::Rng::new(16);
        let x = snapshots(&resp, &truth, 8000, 0.02, &Impair::default(), &mut rng);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 0.5);
        let r = estimate(Method::CoarrayMusic, &x, &array, &table, &resp, &DfOptions { count: Some(6), ..Default::default() }).unwrap();
        assert_found(&r.dirs, &truth, 2.0);
    }

    #[test]
    fn a_ring_scans_all_the_way_round() {
        let (array, resp) = ring_array(8, 0.6);
        let table = Table::new(&resp, Scan::Azimuth { theta: 90.0 }, 1.0);
        for phi in [0.0, 37.0, 91.0, 200.0, 359.0] {
            let truth = [Source { theta: 90.0, phi, ..Default::default() }];
            let mut rng = proof::Rng::new(phi as u64 + 1);
            let x = snapshots(&resp, &truth, 200, 0.03, &Impair::default(), &mut rng);
            let r = estimate(Method::Music, &x, &array, &table, &resp, &DfOptions { count: Some(1), ..Default::default() }).unwrap();
            assert_found(&r.dirs, &truth, 0.4);
        }
    }

    #[test]
    fn a_planar_array_finds_elevation_and_azimuth_too() {
        let l = Layout::Planar { nx: 6, ny: 6, dx: 0.5, dy: 0.5, triangular: false };
        let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
        let array = DfArray { pos: pos.clone(), f: F, line: None, ring: false };
        let resp = Ideal::new(pos, F);
        let table = Table::new(&resp, Scan::Hemisphere { theta_max: 80.0 }, 2.0);
        let truth = [Source { theta: 35.0, phi: 60.0, ..Default::default() }, Source { theta: 50.0, phi: 200.0, ..Default::default() }];
        let mut rng = proof::Rng::new(8);
        let x = snapshots(&resp, &truth, 200, 0.03, &Impair::default(), &mut rng);
        let r = estimate(Method::Music, &x, &array, &table, &resp, &DfOptions { count: Some(2), ..Default::default() }).unwrap();
        assert_found(&r.dirs, &truth, 0.5);
        // Monopulse off a planar array, one source.
        let one = [Source { theta: 20.0, phi: 30.0, ..Default::default() }];
        let mut rng = proof::Rng::new(9);
        let x = snapshots(&resp, &one, 200, 0.03, &Impair::default(), &mut rng);
        let r = estimate(Method::Monopulse, &x, &array, &table, &resp, &DfOptions::default()).unwrap();
        assert_found(&r.dirs, &one, 0.6);
    }

    #[test]
    fn watson_watt_reads_the_formula_and_shows_its_octantal_error() {
        // Radius λ/8, spacing λ/4 across: a small Adcock, whose error is small.
        for radius in [0.05, 0.125, 0.2] {
            let (array, resp) = ring_array(4, radius);
            let table = Table::new(&resp, Scan::Azimuth { theta: 90.0 }, 2.0);
            for phi in [10.0, 22.5, 40.0, 100.0, 170.0, 250.0, 340.0] {
                let truth = [Source { theta: 90.0, phi, ..Default::default() }];
                let mut rng = proof::Rng::new(phi as u64 + 3);
                let x = snapshots(&resp, &truth, 400, 1e-3, &Impair::default(), &mut rng);
                let r = estimate(Method::WatsonWatt, &x, &array, &table, &resp, &DfOptions::default()).unwrap();
                let want = adcock_bearing(phi, radius, 90.0);
                assert!(wrap_deg(r.dirs[0].1 - want).abs() < 0.5, "r {radius}, φ {phi}: {} vs {want}", r.dirs[0].1);
                assert!(wrap_deg(want - phi).abs() < 3.5 + 22.0 * radius, "octantal error at r {radius}: {want} for {phi}");
            }
        }
        // On an axis and on a diagonal the error is zero, by symmetry.
        assert!(wrap_deg(adcock_bearing(45.0, 0.3, 90.0) - 45.0).abs() < 1e-9);
        assert!(wrap_deg(adcock_bearing(90.0, 0.3, 90.0) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn pseudo_doppler_reads_the_bearing_off_the_phase_steps() {
        let (array, resp) = ring_array(8, 0.25);
        let table = Table::new(&resp, Scan::Azimuth { theta: 90.0 }, 2.0);
        for phi in [0.0, 44.0, 120.0, 233.0, 301.0] {
            let truth = [Source { theta: 90.0, phi, ..Default::default() }];
            let mut rng = proof::Rng::new(phi as u64 + 5);
            let x = snapshots(&resp, &truth, 300, 0.01, &Impair::default(), &mut rng);
            let r = estimate(Method::PseudoDoppler, &x, &array, &table, &resp, &DfOptions::default()).unwrap();
            assert!(wrap_deg(r.dirs[0].1 - phi).abs() < 4.0, "φ {phi}: {}", r.dirs[0].1);
        }
    }

    #[test]
    fn an_interferometer_unwraps_a_long_baseline_with_a_short_one() {
        // Baselines λ/2 and 4 λ: the long one alone has 8 turns' ambiguity, and is much more
        // accurate once resolved.
        let pos = vec![[0.0, 0.0, 0.0], [0.5 * lambda(), 0.0, 0.0], [4.0 * lambda(), 0.0, 0.0]];
        let array = DfArray { pos: pos.clone(), f: F, line: Some(LineInfo { spacing: 0.5 * lambda(), index: vec![0, 1, 8] }), ring: false };
        let resp = Ideal::new(pos, F);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        for theta in [5.0, 30.0, 50.0, 70.0] {
            let truth = [source(theta)];
            let mut rng = proof::Rng::new(theta as u64);
            let x = snapshots(&resp, &truth, 400, 0.01, &Impair::default(), &mut rng);
            let r = estimate(Method::Interferometer, &x, &array, &table, &resp, &DfOptions::default()).unwrap();
            assert_found(&r.dirs, &truth, 0.1);
        }
    }

    #[test]
    fn the_crb_matches_its_closed_form_for_one_source() {
        // Var(u) = 6/(K·SNR·(kd)²·N(N²−1))·(1 + 1/(N·SNR)), at u = sin θ.
        let (n, k, snr) = (8usize, 100usize, 10f64);
        let (_, resp) = line_array(n);
        let bound = crb_deg(&resp, &[source(0.0)], 1.0 / snr, k, Scan::Plane { phi: 0.0 }).unwrap()[0];
        let kd = std::f64::consts::PI;
        let nn = n as f64;
        let var = 6.0 / (k as f64 * snr * kd * kd * nn * (nn * nn - 1.0)) * (1.0 + 1.0 / (nn * snr));
        let want = var.sqrt().to_degrees();
        assert!((bound - want).abs() < 1e-3 * want, "{bound} vs {want}");
    }

    #[test]
    fn music_approaches_the_bound_at_high_snr() {
        let (array, resp) = line_array(8);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        let sources = [source(12.0)];
        let setup = McSetup { truth: &resp, belief: &resp, table: &table, array: &array, sources: &sources, snapshots: 100, options: DfOptions { count: Some(1), ..Default::default() }, impair: Impair::default(), trials: 300, seed: 1 };
        for method in [Method::Music, Method::RootMusic, Method::Esprit] {
            let p = monte_carlo(&setup, method, 10.0).unwrap();
            let ratio = p.rmse_deg / p.crb_deg;
            let top = if method == Method::Esprit { 1.5 } else { 1.25 };
            assert!(ratio > 0.8 && ratio < top, "{}: {} vs {}, ratio {ratio}", method.label(), p.rmse_deg, p.crb_deg);
            assert_eq!(p.resolved, 1.0);
        }
    }

    #[test]
    fn error_falls_with_snr_and_with_snapshots() {
        let (array, resp) = line_array(8);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        let sources = [source(12.0)];
        let build = |k: usize| McSetup { truth: &resp, belief: &resp, table: &table, array: &array, sources: &sources, snapshots: k, options: DfOptions { count: Some(1), ..Default::default() }, impair: Impair::default(), trials: 200, seed: 2 };
        let low = monte_carlo(&build(50), Method::Music, 0.0).unwrap();
        let high = monte_carlo(&build(50), Method::Music, 20.0).unwrap();
        let more = monte_carlo(&build(500), Method::Music, 0.0).unwrap();
        assert!(high.rmse_deg < low.rmse_deg / 5.0, "{low:?} {high:?}");
        assert!(more.rmse_deg < low.rmse_deg / 2.0, "{low:?} {more:?}");
    }

    #[test]
    fn the_two_source_bound_rises_as_they_close_up() {
        let (_, resp) = line_array(8);
        let at = |sep: f64| crb_deg(&resp, &[source(10.0), source(10.0 + sep)], 0.1, 100, Scan::Plane { phi: 0.0 }).unwrap()[0];
        assert!(at(5.0) > 2.0 * at(20.0), "{} vs {}", at(5.0), at(20.0));
    }

    #[test]
    fn channel_errors_hurt_until_the_table_is_right_about_them() {
        let (array, resp) = line_array(8);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        let sources = [source(12.0)];
        let build = |imp: Impair| McSetup { truth: &resp, belief: &resp, table: &table, array: &array, sources: &sources, snapshots: 100, options: DfOptions { count: Some(1), ..Default::default() }, impair: imp, trials: 200, seed: 7 };
        let clean = monte_carlo(&build(Impair::default()), Method::Music, 20.0).unwrap();
        let rough = monte_carlo(&build(Impair { gain_db: 1.0, phase_deg: 15.0, ..Default::default() }), Method::Music, 20.0).unwrap();
        assert!(rough.rmse_deg > 5.0 * clean.rmse_deg, "{rough:?} vs {clean:?}");
    }

    #[test]
    fn even_one_bit_converters_find_a_source() {
        let (array, resp) = line_array(16);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        let sources = [source(25.0)];
        let build = |bits: Option<u8>| McSetup { truth: &resp, belief: &resp, table: &table, array: &array, sources: &sources, snapshots: 200, options: DfOptions { count: Some(1), ..Default::default() }, impair: Impair { adc_bits: bits, backoff_db: 6.0, ..Default::default() }, trials: 100, seed: 11 };
        let one = monte_carlo(&build(Some(1)), Method::Music, 0.0).unwrap();
        let eight = monte_carlo(&build(Some(8)), Method::Music, 0.0).unwrap();
        let ideal = monte_carlo(&build(None), Method::Music, 0.0).unwrap();
        assert_eq!(one.resolved, 1.0, "{one:?}");
        assert!(one.rmse_deg < 0.5, "{one:?}");
        // About π/2 in variance at low SNR, so about 1.25 in rms.
        assert!(one.rmse_deg / ideal.rmse_deg < 1.6, "{one:?} vs {ideal:?}");
        assert!((eight.rmse_deg / ideal.rmse_deg - 1.0).abs() < 0.1, "{eight:?} vs {ideal:?}");
    }

    #[test]
    fn a_wide_spacing_is_flagged_as_ambiguous() {
        let build = |spacing: f64| {
            let l = Layout::Line { n: 8, spacing };
            let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
            let a = DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false };
            let resp = Ideal::new(pos, F);
            let t = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
            ambiguity(&t, &a)
        };
        let good = build(0.5);
        let bad = build(1.0);
        assert!(good.level < 0.5, "{good:?}");
        assert!(bad.level > 0.95, "{bad:?}");
    }

    #[test]
    fn a_line_cant_tell_where_on_its_cone() {
        let l = Layout::Line { n: 8, spacing: 0.5 };
        let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect();
        let a = DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false };
        let resp = Ideal::new(pos, F);
        let t = Table::new(&resp, Scan::Hemisphere { theta_max: 90.0 }, 4.0);
        let amb = ambiguity(&t, &a);
        assert!(amb.level > 0.99, "{amb:?}");
    }

    #[test]
    fn switching_one_receiver_needs_its_frequency_offset_taken_out() {
        let (_, resp) = line_array(8);
        let pos = resp.pos.clone();
        let array = DfArray { pos, f: F, line: Layout::Line { n: 8, spacing: 0.5 }.line(F), ring: false };
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 1.0);
        let source = source(20.0);
        let err = |correct: bool, seed: u64| {
            let sw = Switched { slot_s: 2e-6, cfo_hz: 40e3, reference: 8, rounds: 6, correct };
            let mut rng = proof::Rng::new(seed);
            let x = switched_snapshots(&resp, &source, 0.01, &sw, &mut rng);
            let r = estimate(Method::Beamscan, &x, &array, &table, &resp, &DfOptions { count: Some(1), ..Default::default() }).unwrap();
            angle_between((20.0, 0.0), r.dirs[0])
        };
        let wrong: f64 = (0..10).map(|s| err(false, s)).sum::<f64>() / 10.0;
        let fixed: f64 = (0..10).map(|s| err(true, s)).sum::<f64>() / 10.0;
        // The offset reads as a phase ramp: a large bias towards the wrong angle.
        assert!(wrong > 5.0, "{wrong}");
        assert!(fixed < 1.5, "{fixed}");
    }

    #[test]
    fn bearings_intersect_at_the_transmitter() {
        let target = [120.0, 80.0, 0.0];
        let stations: Vec<(P3, (f64, f64), f64)> = [[0.0, 0.0, 0.0], [300.0, 0.0, 0.0], [100.0, 300.0, 0.0]]
            .iter()
            .map(|p| {
                let v: P3 = std::array::from_fn(|k| target[k] - p[k]);
                (*p, angles_of(v), 1.0)
            })
            .collect();
        let (at, radius) = intersect_bearings(&stations).unwrap();
        for k in 0..3 {
            assert!((at[k] - target[k]).abs() < 1e-4, "{at:?}");
        }
        assert!(radius > 1.0 && radius < 8.0, "{radius}");
        assert!(intersect_bearings(&stations[..1]).is_err());
    }

    #[test]
    fn a_fix_is_as_sharp_as_its_geometry_says() {
        // Two stations at right angles, R away, σ each: σ·R along each axis across the plane, and
        // both lines hold the height, σ·R/√2. So √(1 + 1 + ½)·σ·R in all.
        let r = 1000.0;
        let sigma = 1.0;
        let stations = [([r, 0.0, 0.0], angles_of([-1.0, 0.0, 0.0]), sigma), ([0.0, r, 0.0], angles_of([0.0, -1.0, 0.0]), sigma)];
        let (at, radius) = intersect_bearings(&stations).unwrap();
        assert!(at.iter().all(|v| v.abs() < 1e-3), "{at:?}");
        let want = 2.5f64.sqrt() * sigma.to_radians() * r;
        assert!((radius - want).abs() < 1e-3 * want, "{radius} vs {want}");
        // A narrow crossing stretches it along the lines: stations 1 km apart, 5 km out, 11.4°.
        let target = [500.0, 5000.0, 0.0];
        let narrow: Vec<(P3, (f64, f64), f64)> = [[0.0, 0.0, 0.0], [1000.0, 0.0, 0.0]].iter().map(|p| { let v: P3 = std::array::from_fn(|k| target[k] - p[k]); (*p, angles_of(v), 1.0) }).collect();
        let (_, wide) = intersect_bearings(&narrow).unwrap();
        assert!(wide > 500.0 && wide < 750.0, "about σ·R/sin(crossing/2)/√2: {wide}");
        // Parallel bearings don't make a fix at all.
        let parallel = [([0.0, 0.0, 0.0], (90.0, 90.0), 1.0), ([100.0, 0.0, 0.0], (90.0, 90.0), 1.0)];
        assert!(intersect_bearings(&parallel).unwrap_err().to_string().contains("parallel"));
    }

    #[test]
    fn every_method_copes_with_every_option_it_cant_use() {
        // Nothing here may panic, whatever a page or a tool asks for.
        let cases: Vec<(DfArray, Ideal, Scan)> = vec![
            { let (a, r) = line_array(8); (a, r, Scan::Plane { phi: 0.0 }) },
            { let (a, r) = ring_array(4, 0.15); (a, r, Scan::Azimuth { theta: 90.0 }) },
            { let (a, r) = ring_array(8, 0.5); (a, r, Scan::Azimuth { theta: 90.0 }) },
            { let l = Layout::SparseLine { kind: Sparse::Nested { n1: 3, n2: 3 }, spacing: 0.5 }; let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect(); (DfArray { pos: pos.clone(), f: F, line: l.line(F), ring: false }, Ideal::new(pos, F), Scan::Plane { phi: 0.0 }) },
            { let l = Layout::Planar { nx: 4, ny: 4, dx: 0.5, dy: 0.5, triangular: false }; let pos: Vec<P3> = l.elements(F).iter().map(|e| e.pos).collect(); (DfArray { pos: pos.clone(), f: F, line: None, ring: false }, Ideal::new(pos, F), Scan::Hemisphere { theta_max: 80.0 }) },
        ];
        for (array, resp, scan) in &cases {
            let table = Table::new(resp, *scan, 4.0);
            let mut rng = proof::Rng::new(3);
            let x = snapshots(resp, &[Source { theta: 30.0, phi: 0.0, ..Default::default() }], 50, 0.1, &Impair::default(), &mut rng);
            for method in Method::ALL {
                for opts in [DfOptions { count: Some(40), ..Default::default() }, DfOptions { forward_backward: true, smooth_len: 5, ..Default::default() }, DfOptions { count: None, smooth_len: 100, ..Default::default() }] {
                    // Either an answer or a refusal in words.
                    let _ = estimate(method, &x, array, &table, resp, &opts);
                }
            }
        }
    }

    #[test]
    fn forward_backward_only_runs_where_it_doesnt_invent_sources() {
        // A ring's channels don't mirror in order: it would have added a ghost at 135° − φ.
        let (array, resp) = ring_array(8, 0.5);
        assert!(!array.is_centro_symmetric());
        assert!(line_array(8).0.is_centro_symmetric());
        let table = Table::new(&resp, Scan::Azimuth { theta: 90.0 }, 1.0);
        let truth = [Source { theta: 90.0, phi: 30.0, ..Default::default() }];
        let mut rng = proof::Rng::new(4);
        let x = snapshots(&resp, &truth, 300, 0.01, &Impair::default(), &mut rng);
        let r = estimate(Method::Beamscan, &x, &array, &table, &resp, &DfOptions { forward_backward: true, ..Default::default() }).unwrap();
        assert_eq!(r.counted.mdl, 1, "{r:?}");
        assert!(r.notes.iter().any(|n| n.contains("Forward-backward")), "{:?}", r.notes);
        let ghost = r.spectrum.as_ref().unwrap().p[0][105];
        assert!(ghost < 0.5, "nothing at 105°: {ghost}");
    }

    #[test]
    fn close_subspace_peaks_stay_two_peaks() {
        // 16 elements, sources 2.5° apart at high SNR on a 0.5° grid: two peaks, however close.
        let (array, resp) = line_array(16);
        let truth = [source(20.0), source(22.5)];
        let mut rng = proof::Rng::new(5);
        let x = snapshots(&resp, &truth, 2000, 1e-4, &Impair::default(), &mut rng);
        let table = Table::new(&resp, Scan::Plane { phi: 0.0 }, 0.5);
        let r = estimate(Method::Music, &x, &array, &table, &resp, &DfOptions { count: Some(2), ..Default::default() }).unwrap();
        assert_found(&r.dirs, &truth, 0.3);
    }

    #[test]
    fn a_missed_source_can_be_any_of_them() {
        // One estimate, right on the last source: that's the one found, not the first.
        let pairs = pair_up(&[(10.0, 0.0), (50.0, 0.0)], &[(50.0, 0.0)]);
        assert_eq!(pairs[0], None, "{pairs:?}");
        assert!(pairs[1].unwrap() < 1e-9, "{pairs:?}");
    }

    #[test]
    fn direct_methods_answer_in_the_scans_own_plane() {
        // A line along x, scanned in the plane φ = 60°: u = sin θ·cos 60°.
        let (array, resp) = line_array(12);
        let scan = Scan::Plane { phi: 60.0 };
        let table = Table::new(&resp, scan, 1.0);
        let truth = [Source { theta: 40.0, phi: 60.0, ..Default::default() }];
        let mut rng = proof::Rng::new(6);
        let x = snapshots(&resp, &truth, 300, 0.01, &Impair::default(), &mut rng);
        for m in [Method::Music, Method::RootMusic, Method::Esprit, Method::Interferometer] {
            let r = estimate(m, &x, &array, &table, &resp, &DfOptions { count: Some(1), ..Default::default() }).unwrap();
            assert_found(&r.dirs, &truth, 0.5);
        }
    }

    #[test]
    fn a_solved_array_can_stand_in_as_the_truth() {
        // The solved response of ideal isotropic fields equals the ideal response.
        use crate::farfield::FarField;
        let (array, resp) = ring_array(4, 0.5);
        let far: Vec<FarField> = resp
            .pos
            .iter()
            .map(|r| {
                let mut ff = FarField::grid(F, 2.0, 2.0);
                let k = resp.k;
                for (ti, t) in ff.theta.clone().iter().enumerate() {
                    for (pi, p) in ff.phi.clone().iter().enumerate() {
                        ff.e_theta[ti][pi] = C64::from_polar(1.0, k * dot(unit_dir(*t, *p), *r));
                    }
                }
                ff.p_accepted = 1.0;
                ff.p_incident = 1.0;
                ff
            })
            .collect();
        let solved = SolvedArray::new(F, far, identity(4), vec![50.0; 4], resp.pos.clone(), vec![String::new(); 4]).unwrap();
        let from = FromSolved::new(&solved, Some((C64::new(1.0, 0.0), ZERO)));
        for (t, p) in [(90.0, 13.0), (50.0, 200.0), (30.0, 301.0)] {
            let (a, b) = (from.a(t, p), resp.a(t, p));
            for (x, y) in a.iter().zip(&b) {
                assert!((*x - *y).norm() < 1e-3, "({t}, {p}): {x} vs {y}");
            }
        }
        let table = Table::new(&from, Scan::Azimuth { theta: 90.0 }, 2.0);
        let truth = [Source { theta: 90.0, phi: 77.0, ..Default::default() }];
        let mut rng = proof::Rng::new(1);
        let x = snapshots(&from, &truth, 200, 0.01, &Impair::default(), &mut rng);
        let r = estimate(Method::Music, &x, &array, &table, &from, &DfOptions { count: Some(1), ..Default::default() }).unwrap();
        assert_found(&r.dirs, &truth, 0.5);
    }
}
