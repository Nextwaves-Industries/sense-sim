//! Array synthesis on an element pattern, as CST's Farfield Array and the HFSS array
//! factor do: tapers, lattices, beam steering, phase-shifter quantisation, random
//! errors and failed elements. Mutual coupling is not modelled: each element is
//! assumed to radiate the supplied pattern and to accept power in proportion to |w|².

use std::f64::consts::PI;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farfield::FarField;
use crate::optimize::Rng;
use crate::{C0, C64, db10};

/// Amplitude taper along one axis. A side-lobe level may be given with either sign.
/// Dolph-Chebyshev assumes a spacing of at least λ/2, below which its lobes rise.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Taper {
    Uniform,
    Binomial,
    DolphChebyshev { sll_db: f64 },
    Taylor { sll_db: f64, nbar: usize },
    Cosine { power: f64 },
    Hamming,
    Blackman,
}

/// `Triangular` shifts every other row by `dx/2`; a single column stays straight.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Lattice {
    Rectangular,
    Triangular,
}

/// `dx` and `dy` are in metres; the steering direction and error are in degrees.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArrayConfig {
    pub nx: usize,
    pub ny: usize,
    pub dx: f64,
    pub dy: f64,
    pub lattice: Lattice,
    pub taper_x: Taper,
    pub taper_y: Taper,
    pub steer_theta: f64,
    pub steer_phi: f64,
    /// Quantise phase shifters.
    pub phase_bits: Option<u8>,
    /// Random amplitude error, rms dB.
    pub amp_err_db: f64,
    /// Random phase error, rms degrees.
    pub phase_err_deg: f64,
    pub seed: u64,
    /// Elements switched off: `(ix, iy)`.
    pub failed: Vec<(usize, usize)>,
}

impl Default for ArrayConfig {
    /// Eight uniform elements along x at λ/2 for 2.4 GHz.
    fn default() -> Self {
        Self {
            nx: 8,
            ny: 1,
            dx: 0.0625,
            dy: 0.0625,
            lattice: Lattice::Rectangular,
            taper_x: Taper::Uniform,
            taper_y: Taper::Uniform,
            steer_theta: 0.0,
            steer_phi: 0.0,
            phase_bits: None,
            amp_err_db: 0.0,
            phase_err_deg: 0.0,
            seed: 1,
            failed: Vec::new(),
        }
    }
}

/// Weights normalised to a maximum magnitude of 1. Continuous windows (Taylor,
/// cosine, Hamming, Blackman) are sampled at element centres of an aperture `n`
/// element pitches long, so no edge element is wasted on a zero.
pub fn taper_weights(t: Taper, n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let raw: Vec<f64> = match t {
        Taper::Uniform => vec![1.0; n],
        Taper::Binomial => binomial(n),
        Taper::DolphChebyshev { sll_db } => dolph_chebyshev(sll_db, n),
        Taper::Taylor { sll_db, nbar } => taylor(sll_db, nbar, n),
        Taper::Cosine { power } => (0..n).map(|i| (PI * aperture(i, n)).cos().powf(power)).collect(),
        Taper::Hamming => (0..n).map(|i| 0.54 + 0.46 * (2.0 * PI * aperture(i, n)).cos()).collect(),
        Taper::Blackman => {
            (0..n)
                .map(|i| {
                    let x = aperture(i, n);
                    0.42 + 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos()
                })
                .collect()
        }
    };
    let top = raw.iter().fold(0.0f64, |a, b| a.max(b.abs()));
    raw.iter().map(|v| if top > 0.0 { v / top } else { 0.0 }).collect()
}

/// Element centre in units of the aperture, in (−½, ½).
fn aperture(i: usize, n: usize) -> f64 {
    (i as f64 - (n - 1) as f64 / 2.0) / n as f64
}

fn binomial(n: usize) -> Vec<f64> {
    let mut log_c = vec![0.0f64; n];
    for k in 1..n {
        log_c[k] = log_c[k - 1] + ((n - k) as f64 / k as f64).ln();
    }
    let top = log_c.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    log_c.iter().map(|v| (v - top).exp()).collect()
}

fn chebyshev(order: usize, x: f64) -> f64 {
    let m = order as f64;
    if x.abs() <= 1.0 {
        (m * x.acos()).cos()
    } else if x > 0.0 {
        (m * x.acosh()).cosh()
    } else {
        let sign = if order.is_multiple_of(2) { 1.0 } else { -1.0 };
        sign * (m * (-x).acosh()).cosh()
    }
}

/// Dolph's pattern `T_{n−1}(x₀·cos(ψ/2))` with `T_{n−1}(x₀) = R`, turned into
/// element weights by an inverse DFT over the `n` samples that determine it.
fn dolph_chebyshev(sll_db: f64, n: usize) -> Vec<f64> {
    if n < 3 {
        return vec![1.0; n];
    }
    let order = n - 1;
    let ratio = 10f64.powf(sll_db.abs() / 20.0).max(1.0);
    let x0 = (ratio.acosh() / order as f64).cosh();
    (0..n)
        .map(|i| {
            let offset = i as f64 - order as f64 / 2.0;
            (0..n)
                .map(|k| {
                    let psi = 2.0 * PI * k as f64 / n as f64;
                    chebyshev(order, x0 * (psi / 2.0).cos()) * (psi * offset).cos()
                })
                .sum::<f64>()
                / n as f64
        })
        .collect()
}

/// Taylor's n̄ line-source distribution from its space-factor samples, sampled at
/// element centres (the same weights as MATLAB's `taylorwin`). The design level holds
/// once the array has a few more than 2·n̄ elements; below that the sampling costs
/// side-lobe level (−30 dB, n̄ = 5 reads −28.0 dB at N = 8, −29.6 dB at N = 12), and
/// Dolph-Chebyshev is the exact choice.
fn taylor(sll_db: f64, nbar: usize, n: usize) -> Vec<f64> {
    let nbar = nbar.max(1);
    let a = 10f64.powf(sll_db.abs() / 20.0).max(1.0).acosh() / PI;
    let nb = nbar as f64;
    let sigma2 = nb * nb / (a * a + (nb - 0.5) * (nb - 0.5));
    let space_factor: Vec<f64> = (1..nbar)
        .map(|m| {
            let mf = (m * m) as f64;
            let (mut top, mut bottom) = (1.0, 1.0);
            for i in 1..nbar {
                let shifted = i as f64 - 0.5;
                top *= 1.0 - mf / (sigma2 * (a * a + shifted * shifted));
                if i != m {
                    bottom *= 1.0 - mf / (i * i) as f64;
                }
            }
            let sign = if m % 2 == 1 { 1.0 } else { -1.0 };
            sign * top / (2.0 * bottom)
        })
        .collect();
    (0..n)
        .map(|i| {
            let x = aperture(i, n);
            1.0 + 2.0 * space_factor.iter().enumerate().map(|(m, s)| s * (2.0 * PI * (m + 1) as f64 * x).cos()).sum::<f64>()
        })
        .collect()
}

/// Element centres in the xy plane, centred on the origin. Rows along x, `iy`
/// outermost: index = `iy·nx + ix`.
pub fn element_positions(c: &ArrayConfig) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(c.nx * c.ny);
    for iy in 0..c.ny {
        for ix in 0..c.nx {
            let shift = if c.lattice == Lattice::Triangular && c.nx > 1 && iy % 2 == 1 { 0.5 * c.dx } else { 0.0 };
            out.push([ix as f64 * c.dx + shift, iy as f64 * c.dy, 0.0]);
        }
    }
    let n = out.len().max(1) as f64;
    let mean_x = out.iter().map(|p| p[0]).sum::<f64>() / n;
    let mean_y = out.iter().map(|p| p[1]).sum::<f64>() / n;
    for p in &mut out {
        p[0] -= mean_x;
        p[1] -= mean_y;
    }
    out
}

fn direction_cosines(theta_deg: f64, phi_deg: f64) -> (f64, f64) {
    let st = theta_deg.to_radians().sin();
    let (sp, cp) = phi_deg.to_radians().sin_cos();
    (st * cp, st * sp)
}

/// Complex weights in [`element_positions`] order at frequency `f`: taper, steering
/// phase, phase-shifter quantisation, then random errors, then failures.
pub fn excitations(c: &ArrayConfig, f: f64) -> Vec<C64> {
    let k = 2.0 * PI * f / C0;
    let (u0, v0) = direction_cosines(c.steer_theta, c.steer_phi);
    let (tx, ty) = (taper_weights(c.taper_x, c.nx), taper_weights(c.taper_y, c.ny));
    let mut rng = Rng::new(c.seed);
    let lsb = c.phase_bits.map(|bits| 2.0 * PI / 2f64.powi(bits as i32));
    element_positions(c)
        .iter()
        .enumerate()
        .map(|(index, p)| {
            let (ix, iy) = (index % c.nx, index / c.nx);
            let mut phase = -k * (u0 * p[0] + v0 * p[1]);
            if let Some(lsb) = lsb {
                phase = (phase / lsb).round() * lsb;
            }
            // Drawn for every element, so a failure leaves the others' errors alone.
            let (amp_draw, phase_draw) = (rng.gauss(), rng.gauss());
            let amp = tx[ix] * ty[iy] * 10f64.powf(c.amp_err_db * amp_draw / 20.0);
            let phase = phase + c.phase_err_deg.to_radians() * phase_draw;
            if c.failed.contains(&(ix, iy)) { C64::new(0.0, 0.0) } else { C64::from_polar(amp, phase) }
        })
        .collect()
}

/// `AF = Σ wₙ·exp(+jk·r̂·rₙ)`; a uniform broadside array peaks at the element count. The
/// weights are rebuilt on each call, so use [`apply`] for a whole pattern.
pub fn array_factor(c: &ArrayConfig, f: f64, theta_deg: f64, phi_deg: f64) -> C64 {
    let (u, v) = direction_cosines(theta_deg, phi_deg);
    Elements::new(c, f).factor(u, v)
}

/// Element pattern × array factor on the element's grid. Accepted and incident power
/// follow from what really radiates, with the element's own efficiencies: see [`gain`].
pub fn apply(element: &FarField, c: &ArrayConfig) -> FarField {
    apply_with_gain(element, c).0
}

/// [`apply`] and [`gain`] from the one power integral.
pub fn apply_with_gain(element: &FarField, c: &ArrayConfig) -> (FarField, ArrayGain) {
    let elements = Elements::new(c, element.f);
    let mut out = element.clone();
    let phi: Vec<(f64, f64)> = element.phi.iter().map(|p| p.to_radians().sin_cos()).collect();
    let theta = &element.theta;
    out.e_theta.par_iter_mut().zip(out.e_phi.par_iter_mut()).enumerate().for_each(|(ti, (row_t, row_p))| {
        let st = theta[ti].to_radians().sin();
        for (pi, (sp, cp)) in phi.iter().enumerate() {
            let af = elements.factor(st * cp, st * sp);
            row_t[pi] *= af;
            row_p[pi] *= af;
        }
    });
    let g = gain(element, c);
    let per_element = element.p_rad();
    // Uncoupled elements would each accept what one does alone, Σ|w|² in all, and that
    // lets close elements read above their directivity. Scale from the radiated power instead.
    let scale = if per_element > 0.0 && g.p_rad > 0.0 { g.p_rad / per_element } else { elements.w.iter().map(C64::norm_sqr).sum() };
    out.p_accepted *= scale;
    out.p_incident *= scale;
    (out, g)
}

/// What an array reads out with its element's pattern and losses. Mutual coupling is not
/// modelled, but power is conserved: gain is the directivity of the real pattern times
/// the element's efficiency, never `N·G` above that directivity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrayGain {
    pub directivity_dbi: f64,
    /// Directivity times the element's radiation efficiency.
    pub gain_dbi: f64,
    /// Directivity times the element's total efficiency.
    pub realized_dbi: f64,
    pub peak_theta: f64,
    pub peak_phi: f64,
    /// Radiated power (W) with the element's normalisation, the reference of [`cut_db`].
    pub p_rad: f64,
    /// Grid step of the power integral, degrees.
    pub step_deg: f64,
    /// Too many elements for a full integral: directivity comes from an estimate.
    pub estimated: bool,
    /// Elements closer than λ/2 along some axis, where coupling is strong and not modelled.
    pub close_spacing: bool,
}

/// Complex terms one power integral may cost before a planar array is estimated instead.
const INTEGRAL_BUDGET: f64 = 5e8;
/// Pattern samples a line array's integral may cost, with its factor read from a table.
const LINE_SAMPLES: f64 = 4e7;
/// Grid points of a sphere at a step of one degree: `2·(90/s)·4·(90/s)` at step `s`.
const SPHERE_POINTS: f64 = 64800.0;

/// Bilinear lookup of an element's radiation intensity.
struct ElementPower {
    u: Vec<Vec<f64>>,
    dt: f64,
    dp: f64,
    ground: bool,
}

impl ElementPower {
    fn new(ff: &FarField) -> Self {
        let u = (0..ff.theta.len()).map(|ti| (0..ff.phi.len()).map(|pi| ff.intensity(ti, pi)).collect()).collect();
        Self { u, dt: ff.theta_step(), dp: ff.phi_step(), ground: ff.ground }
    }

    fn at(&self, theta: f64, phi: f64) -> f64 {
        if self.ground && theta > 90.0 + 1e-9 {
            return 0.0;
        }
        let (nt, np) = (self.u.len(), self.u[0].len());
        let (x, y) = ((theta / self.dt).clamp(0.0, (nt - 1) as f64), phi.rem_euclid(360.0) / self.dp);
        let (i, j) = ((x as usize).min(nt - 2), (y as usize).min(np - 2));
        let (s, t) = (x - i as f64, y - j as f64);
        let row = |a: usize| self.u[a][j] * (1.0 - t) + self.u[a][j + 1] * t;
        row(i) * (1.0 - s) + row(i + 1) * s
    }
}

/// The half-power width of a broadside beam along the array's longest side, degrees: the
/// narrowest any scan of it gets.
pub fn narrowest_beam_deg(c: &ArrayConfig, f: f64) -> f64 {
    let long = f64::max(if c.nx > 1 { c.nx as f64 * c.dx } else { 0.0 }, if c.ny > 1 { c.ny as f64 * c.dy } else { 0.0 });
    if long > 0.0 { (0.886 * C0 / f / long).min(2.0).to_degrees() } else { 360.0 }
}

/// Radiated power and strongest grid direction of `U_el·|AF|²` on a grid of about `step`
/// degrees, with the weights of [`FarField::p_rad`].
fn integrate(u: &ElementPower, af2: &(dyn Fn(f64, f64) -> f64 + Sync), step: f64) -> (f64, (f64, f64)) {
    let cells = (90.0 / step).round().max(1.0) as usize;
    let d = (90.0 / cells as f64).to_radians();
    let rows = if u.ground { cells + 1 } else { 2 * cells + 1 };
    let (sum, best) = (0..rows)
        .into_par_iter()
        .map(|ti| {
            let th = 90.0 * ti as f64 / cells as f64;
            let st = th.to_radians().sin();
            let mut w = if ti == 0 || ti == 2 * cells { (1.0 - (d * 0.5).cos()) * d } else { st * d * d };
            if u.ground && ti == cells {
                w *= 0.5;
            }
            let (mut sum, mut best) = (0.0, (f64::MIN, 0.0, 0.0));
            for pi in 0..4 * cells {
                let ph = 90.0 * pi as f64 / cells as f64;
                let (sp, cp) = ph.to_radians().sin_cos();
                let v = u.at(th, ph) * af2(st * cp, st * sp);
                sum += v * w;
                if v > best.0 {
                    best = (v, th, ph);
                }
            }
            (sum, best)
        })
        .reduce(|| (0.0, (f64::MIN, 0.0, 0.0)), |a, b| (a.0 + b.0, if b.1.0 > a.1.0 { b.1 } else { a.1 }));
    (sum, (best.1, best.2))
}

/// Peak, gain and directivity of the array on this element. The peak is found on a grid
/// that resolves the beam and then refined, so a narrow beam between grid lines is not lost.
pub fn gain(element: &FarField, c: &ArrayConfig) -> ArrayGain {
    gain_within_budget(element, c, INTEGRAL_BUDGET)
}

fn gain_within_budget(element: &FarField, c: &ArrayConfig, budget: f64) -> ArrayGain {
    let elements = Elements::new(c, element.f);
    let lambda = C0 / element.f;
    let u = ElementPower::new(element);
    let finest = (narrowest_beam_deg(c, element.f) / 5.0).min(element.theta_step()).min(element.phi_step());
    let total = |th: f64, ph: f64| {
        let (x, y) = direction_cosines(th, ph);
        u.at(th, ph) * elements.factor(x, y).norm_sqr()
    };
    let n = elements.w.len() as f64;
    let (p_rad, start, step, estimated) = if c.nx <= 1 || c.ny <= 1 {
        // A line's factor depends on one direction cosine, so a table stands in for the sum.
        let span = f64::max(if c.nx > 1 { c.nx as f64 * c.dx } else { 0.0 }, if c.ny > 1 { c.ny as f64 * c.dy } else { 0.0 });
        let count = ((64.0 * span / lambda).ceil() as usize).max(1024) + 1;
        let along_x = c.nx > 1;
        let table: Vec<f64> = (0..count)
            .into_par_iter()
            .map(|i| {
                let s = -1.0 + 2.0 * i as f64 / (count - 1) as f64;
                let w = if along_x { elements.factor(s, 0.0) } else { elements.factor(0.0, s) };
                w.norm_sqr()
            })
            .collect();
        let af2 = |a: f64, b: f64| {
            let at = ((if along_x { a } else { b } + 1.0) * 0.5 * (count - 1) as f64).clamp(0.0, (count - 1) as f64);
            let i = (at as usize).min(count - 2);
            table[i] + (table[i + 1] - table[i]) * (at - i as f64)
        };
        let step = finest.max((SPHERE_POINTS / LINE_SAMPLES).sqrt());
        let (p, best) = integrate(&u, &af2, step);
        (p, best, step, step > finest)
    } else if SPHERE_POINTS / (finest * finest) * n <= budget {
        let af2 = |a: f64, b: f64| elements.factor(a, b).norm_sqr();
        let (p, best) = integrate(&u, &af2, finest);
        (p, best, finest, false)
    } else {
        // Too many elements to integrate: nearly all the power is in the lobes the lattice
        // allows, each in both half-spaces, and each weighted by the element's pattern there.
        let (u0, v0) = direction_cosines(c.steer_theta, c.steer_phi);
        let mut lobes = vec![(u0, v0)];
        lobes.extend(lattice_vectors(c, lambda).iter().map(|g| (u0 + g.0, v0 + g.1)).filter(|d| d.0 * d.0 + d.1 * d.1 <= 1.0));
        let dirs: Vec<(f64, f64)> = lobes
            .iter()
            .flat_map(|d| {
                let th = (d.0 * d.0 + d.1 * d.1).sqrt().min(1.0).asin().to_degrees();
                let ph = d.1.atan2(d.0).to_degrees();
                [(th, ph), (180.0 - th, ph)]
            })
            .collect();
        let mean = dirs.iter().map(|d| u.at(d.0, d.1)).sum::<f64>() / dirs.len() as f64;
        let best = dirs.iter().copied().max_by(|a, b| total(a.0, a.1).total_cmp(&total(b.0, b.1))).unwrap_or((0.0, 0.0));
        (4.0 * PI * elements.radiated_power() * mean, best, finest, true)
    };

    let top = if element.ground { 90.0 } else { 180.0 };
    let (mut th, mut ph) = start;
    let mut best = total(th, ph);
    let mut s = step;
    for _ in 0..64 {
        let mut moved = false;
        for (a, b) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
            let (t, p) = ((th + a * s).clamp(0.0, top), (ph + b * s).rem_euclid(360.0));
            let v = total(t, p);
            if v > best {
                (th, ph, best, moved) = (t, p, v, true);
            }
        }
        if !moved {
            s *= 0.5;
            if s < step * 1e-3 {
                break;
            }
        }
    }

    let directivity = 4.0 * PI * best / p_rad.max(1e-300);
    let close = |n: usize, d: f64| n > 1 && d < 0.5 * lambda * (1.0 - 1e-9);
    ArrayGain {
        directivity_dbi: db10(directivity),
        gain_dbi: db10(directivity * element.rad_efficiency()),
        realized_dbi: db10(directivity * element.total_efficiency()),
        peak_theta: th,
        peak_phi: ph,
        p_rad,
        step_deg: step,
        estimated,
        close_spacing: close(c.nx, c.dx) || close(c.ny, c.dy),
    }
}

/// A directivity cut through the array in the plane at `phi`, as `(θ°, dBi)` from −180° to
/// 180°, fine enough to draw the beam whatever the element's grid. Negative θ reads the
/// half-plane at φ + 180°.
pub fn cut_db(element: &FarField, c: &ArrayConfig, g: &ArrayGain, phi: f64) -> Vec<(f64, f64)> {
    let elements = Elements::new(c, element.f);
    let u = ElementPower::new(element);
    let step = (narrowest_beam_deg(c, element.f) / 5.0).clamp(360.0 / 4000.0, 2.0);
    let count = (180.0 / step).round() as usize;
    (0..=2 * count)
        .into_par_iter()
        .map(|i| {
            let angle = -180.0 + 180.0 * i as f64 / count as f64;
            let (th, ph) = if angle < 0.0 { (-angle, phi + 180.0) } else { (angle, phi) };
            let (x, y) = direction_cosines(th, ph);
            (angle, db10(4.0 * PI * u.at(th, ph) * elements.factor(x, y).norm_sqr() / g.p_rad.max(1e-300)))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArraySummary {
    /// Of an isotropic-element array at its peak, from the exact integral of |AF|² over
    /// the sphere, a pair-wise sum costing O(N²).
    pub directivity_dbi: f64,
    /// `|Σ|w||² / (N·Σ|w|²)`, N counting failed elements.
    pub taper_efficiency: f64,
    pub taper_loss_db: f64,
    /// `(θ, φ)` in degrees of the visible grating lobe closest to the beam. For a
    /// line array the lobe is a cone; this is its direction nearest the steering plane.
    pub grating_lobe: Option<(f64, f64)>,
    /// Largest θ in the steering plane φ = `steer_phi` free of grating lobes.
    pub max_scan_no_grating_deg: f64,
    /// Half-power width along x through the beam at constant v, or 360 if the
    /// pattern never falls 3 dB on that line.
    pub hpbw_x_deg: f64,
    /// Likewise along y at constant u.
    pub hpbw_y_deg: f64,
    /// Highest lobe outside the main beam relative to the peak (negative dB), grating
    /// lobes included; −∞ if there is none. Both principal lines are searched
    /// finely. Planar arrays also get a 2-D search of the visible disc while that costs
    /// below 3·10⁸ complex terms (about 46 × 46 elements at λ/2); larger arrays are
    /// searched on the principal lines only, where a separable taper has its highest lobes.
    pub sll_db: f64,
}

/// Beam, side-lobe and grating-lobe figures of an isotropic-element array.
pub fn summarize(c: &ArrayConfig, f: f64) -> ArraySummary {
    let elements = Elements::new(c, f);
    let lambda = C0 / f;
    let (guess_u, guess_v) = direction_cosines(c.steer_theta, c.steer_phi);
    let (lx, ly) = elements.extents();

    let first = elements.cut(Axis::X, guess_v, lx);
    let u = first.beam(guess_u).peak;
    let along_y = elements.cut(Axis::Y, u, ly);
    let beam_y = along_y.beam(guess_v);
    let along_x = elements.cut(Axis::X, beam_y.peak, lx);
    let beam_x = along_x.beam(u);
    let (peak_u, peak_v) = (beam_x.peak, beam_y.peak);

    let peak_power = elements.factor(peak_u, peak_v).norm_sqr();
    let peak_db = db10(peak_power);
    let radiated = elements.radiated_power();
    let directivity_dbi = db10(peak_power / radiated.max(1e-300));

    let amplitude: f64 = elements.w.iter().map(|w| w.norm()).sum();
    let power: f64 = elements.w.iter().map(C64::norm_sqr).sum();
    let count = (c.nx * c.ny).max(1) as f64;
    let taper_efficiency = if power > 0.0 { amplitude * amplitude / (count * power) } else { 0.0 };

    let mut sll = f64::NEG_INFINITY;
    for lobe in [along_x.side_lobe_db(&beam_x, peak_db), along_y.side_lobe_db(&beam_y, peak_db)].into_iter().flatten() {
        sll = sll.max(lobe);
    }
    if c.nx > 1 && c.ny > 1 {
        let half_x = along_x.main_half_width(&beam_x);
        let half_y = along_y.main_half_width(&beam_y);
        if let Some(lobe) = elements.scan_side_lobes((peak_u, peak_v), (half_x, half_y), peak_db, (lx, ly)) {
            sll = sll.max(lobe);
        }
    }

    ArraySummary {
        directivity_dbi,
        taper_efficiency,
        taper_loss_db: 0.0 - db10(taper_efficiency),
        grating_lobe: grating_lobe(c, lambda, guess_u, guess_v),
        max_scan_no_grating_deg: max_scan(c, lambda),
        hpbw_x_deg: along_x.half_power_width(&beam_x),
        hpbw_y_deg: along_y.half_power_width(&beam_y),
        sll_db: sll,
    }
}

/// Non-zero reciprocal-lattice vectors in direction-cosine units, far enough out to
/// reach the visible disc from anywhere inside it.
fn lattice_vectors(c: &ArrayConfig, lambda: f64) -> Vec<(f64, f64)> {
    let along_x = c.nx > 1 && c.dx > 0.0;
    let along_y = c.ny > 1 && c.dy > 0.0;
    let reach = |d: f64| (2.0 * d / lambda).ceil() as i64 + 1;
    let mut out = Vec::new();
    match (along_x, along_y) {
        (true, true) => {
            let (px, qy) = (reach(c.dx), reach(c.dy));
            // A shifted row adds half a period of y to every odd multiple of x.
            let qy = if c.lattice == Lattice::Triangular { qy + px } else { qy };
            for p in -px..=px {
                for q in -qy..=qy {
                    if p == 0 && q == 0 {
                        continue;
                    }
                    let skew = if c.lattice == Lattice::Triangular { p as f64 / 2.0 } else { 0.0 };
                    out.push((p as f64 * lambda / c.dx, (q as f64 - skew) * lambda / c.dy));
                }
            }
        }
        (true, false) => {
            let px = reach(c.dx);
            out.extend((-px..=px).filter(|p| *p != 0).map(|p| (p as f64 * lambda / c.dx, 0.0)));
        }
        (false, true) => {
            let qy = reach(c.dy);
            out.extend((-qy..=qy).filter(|q| *q != 0).map(|q| (0.0, q as f64 * lambda / c.dy)));
        }
        (false, false) => {}
    }
    out
}

fn grating_lobe(c: &ArrayConfig, lambda: f64, u0: f64, v0: f64) -> Option<(f64, f64)> {
    let line_x = c.nx > 1 && c.ny <= 1;
    let line_y = c.ny > 1 && c.nx <= 1;
    let mut best: Option<(f64, f64, f64)> = None;
    for (gu, gv) in lattice_vectors(c, lambda) {
        let (mut u, mut v) = (u0 + gu, v0 + gv);
        let visible = if line_x {
            u.abs() <= 1.0 + 1e-12
        } else if line_y {
            v.abs() <= 1.0 + 1e-12
        } else {
            u * u + v * v <= 1.0 + 1e-12
        };
        if !visible {
            continue;
        }
        if line_x {
            let room = (1.0 - u * u).max(0.0).sqrt();
            v = v.clamp(-room, room);
        } else if line_y {
            let room = (1.0 - v * v).max(0.0).sqrt();
            u = u.clamp(-room, room);
        }
        let reach = gu * gu + gv * gv;
        if best.is_none_or(|b| reach < b.2) {
            best = Some((u, v, reach));
        }
    }
    best.map(|(u, v, _)| {
        let theta = (u * u + v * v).sqrt().min(1.0).asin().to_degrees();
        (theta, v.atan2(u).to_degrees().rem_euclid(360.0))
    })
}

/// First `s = sin θ` in the steering plane at which `(s·cos φ, s·sin φ) + G` enters the
/// visible region, over all lattice vectors G. A line array's lobes are planes in
/// (u, v) rather than points, so only the axis along the line matters.
fn max_scan(c: &ArrayConfig, lambda: f64) -> f64 {
    let (sp, cp) = c.steer_phi.to_radians().sin_cos();
    let line_x = c.nx > 1 && c.ny <= 1;
    let line_y = c.ny > 1 && c.nx <= 1;
    let mut limit = 1.0f64;
    for (gu, gv) in lattice_vectors(c, lambda) {
        let entry = if line_x || line_y {
            let (g, along) = if line_x { (gu, cp) } else { (gv, sp) };
            if g.abs() <= 1.0 {
                Some(0.0)
            } else if g * along < 0.0 {
                Some((g.abs() - 1.0) / along.abs())
            } else {
                None
            }
        } else {
            let g2 = gu * gu + gv * gv;
            let a = cp * gu + sp * gv;
            let disc = a * a - g2 + 1.0;
            if g2 <= 1.0 {
                Some(0.0)
            } else if a < 0.0 && disc >= 0.0 {
                Some(-a - disc.sqrt())
            } else {
                None
            }
        };
        if let Some(s) = entry {
            limit = limit.min(s);
        }
    }
    limit.asin().to_degrees()
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

/// Positions and weights at one frequency, for repeated pattern evaluation.
struct Elements {
    x: Vec<f64>,
    y: Vec<f64>,
    w: Vec<C64>,
    k: f64,
}

impl Elements {
    fn new(c: &ArrayConfig, f: f64) -> Self {
        let positions = element_positions(c);
        Self {
            x: positions.iter().map(|p| p[0]).collect(),
            y: positions.iter().map(|p| p[1]).collect(),
            w: excitations(c, f),
            k: 2.0 * PI * f / C0,
        }
    }

    fn factor(&self, u: f64, v: f64) -> C64 {
        let mut sum = C64::new(0.0, 0.0);
        for i in 0..self.w.len() {
            sum += self.w[i] * C64::cis(self.k * (u * self.x[i] + v * self.y[i]));
        }
        sum
    }

    fn extents(&self) -> (f64, f64) {
        let span = |v: &[f64]| {
            let (lo, hi) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
            if hi >= lo { hi - lo } else { 0.0 }
        };
        (span(&self.x), span(&self.y))
    }

    /// `(1/4π)∮|AF|²dΩ = Σₘₙ wₘwₙ*·sinc(k·|rₘ − rₙ|)`.
    fn radiated_power(&self) -> f64 {
        let n = self.w.len();
        (0..n)
            .into_par_iter()
            .map(|i| {
                let mut sum = self.w[i].norm_sqr();
                for j in i + 1..n {
                    let r = ((self.x[i] - self.x[j]).powi(2) + (self.y[i] - self.y[j]).powi(2)).sqrt() * self.k;
                    let sinc = if r < 1e-9 { 1.0 } else { r.sin() / r };
                    sum += 2.0 * (self.w[i] * self.w[j].conj()).re * sinc;
                }
                sum
            })
            .sum()
    }

    /// `|AF|²` along a straight line across the visible disc, about 32 samples per
    /// lobe spacing `λ/aperture`.
    fn cut(&self, axis: Axis, fixed: f64, aperture: f64) -> Cut {
        let half = (1.0 - fixed * fixed).max(0.0).sqrt();
        let lambda = 2.0 * PI / self.k;
        let wanted = if aperture > 0.0 { 2.0 * half * 32.0 * aperture / lambda } else { 0.0 };
        let n = (wanted.ceil() as usize).clamp(512, 1 << 18) + 1;
        let coords: Vec<f64> = (0..n).map(|i| -half + 2.0 * half * i as f64 / (n - 1) as f64).collect();
        let power = coords
            .par_iter()
            .map(|s| match axis {
                Axis::X => self.factor(*s, fixed).norm_sqr(),
                Axis::Y => self.factor(fixed, *s).norm_sqr(),
            })
            .collect();
        Cut { coords, power, axis, fixed }
    }

    /// Strongest lobe outside the main-beam ellipse on a grid over the visible disc.
    /// `None` when the grid would cost more than the budget.
    fn scan_side_lobes(&self, peak: (f64, f64), half_width: (f64, f64), peak_db: f64, aperture: (f64, f64)) -> Option<f64> {
        const BUDGET: f64 = 3e8;
        let lambda = 2.0 * PI / self.k;
        let count = |a: f64| ((16.0 * a / lambda).ceil() as usize).max(32) + 1;
        let (nu, nv) = (count(aperture.0), count(aperture.1));
        if (nu * nv * self.w.len()) as f64 > BUDGET {
            return None;
        }
        let coord = |i: usize, n: usize| -1.0 + 2.0 * i as f64 / (n - 1) as f64;
        let grid: Vec<Vec<f64>> = (0..nu)
            .into_par_iter()
            .map(|i| {
                let u = coord(i, nu);
                (0..nv)
                    .map(|j| {
                        let v = coord(j, nv);
                        if u * u + v * v <= 1.0 { self.factor(u, v).norm_sqr() } else { -1.0 }
                    })
                    .collect()
            })
            .collect();
        let mut best = None;
        for i in 0..nu {
            for j in 0..nv {
                let p = grid[i][j];
                if p <= 0.0 {
                    continue;
                }
                let (du, dv) = ((coord(i, nu) - peak.0) / half_width.0.max(1e-9), (coord(j, nv) - peak.1) / half_width.1.max(1e-9));
                if du * du + dv * dv <= 1.0 {
                    continue;
                }
                let is_peak = (i.saturating_sub(1)..=(i + 1).min(nu - 1))
                    .all(|a| (j.saturating_sub(1)..=(j + 1).min(nv - 1)).all(|b| grid[a][b] <= p));
                if is_peak {
                    let rel = db10(p) - peak_db;
                    if best.is_none_or(|v| rel > v) {
                        best = Some(rel);
                    }
                }
            }
        }
        best
    }
}

/// One line of a pattern in direction-cosine space.
struct Cut {
    coords: Vec<f64>,
    power: Vec<f64>,
    axis: Axis,
    fixed: f64,
}

/// A located main beam: refined coordinate and level, its sample, and the samples of
/// the first minima either side.
struct Beam {
    peak: f64,
    peak_db: f64,
    index: usize,
    low: usize,
    high: usize,
}

impl Cut {
    fn db(&self, k: usize) -> f64 {
        db10(self.power[k])
    }

    /// Climbs from the sample nearest `guess` to the top of that lobe.
    fn beam(&self, guess: f64) -> Beam {
        let n = self.coords.len();
        let spacing = (self.coords[n - 1] - self.coords[0]) / (n - 1) as f64;
        let start = if spacing > 0.0 { ((guess - self.coords[0]) / spacing).round() } else { 0.0 };
        let mut i = (start.max(0.0) as usize).min(n - 1);
        loop {
            let left = if i > 0 { self.power[i - 1] } else { f64::NEG_INFINITY };
            let right = if i + 1 < n { self.power[i + 1] } else { f64::NEG_INFINITY };
            if left > self.power[i] && left >= right {
                i -= 1;
            } else if right > self.power[i] {
                i += 1;
            } else {
                break;
            }
        }
        let (mut low, mut high) = (i, i);
        while low > 0 && self.power[low - 1] <= self.power[low] {
            low -= 1;
        }
        while high + 1 < n && self.power[high + 1] <= self.power[high] {
            high += 1;
        }
        let (peak, peak_db) = self.vertex(i);
        Beam { peak, peak_db, index: i, low, high }
    }

    /// Parabola through the dB values at samples `k − 1, k, k + 1`.
    fn vertex(&self, k: usize) -> (f64, f64) {
        if k == 0 || k + 1 >= self.coords.len() {
            return (self.coords[k], self.db(k));
        }
        let (a, b, c) = (self.db(k - 1), self.db(k), self.db(k + 1));
        let curve = a - 2.0 * b + c;
        if curve.abs() < 1e-12 {
            return (self.coords[k], b);
        }
        let shift = (0.5 * (a - c) / curve).clamp(-1.0, 1.0);
        (self.coords[k] + shift * (self.coords[k + 1] - self.coords[k]), b - 0.25 * (a - c) * shift)
    }

    fn direction(&self, s: f64) -> [f64; 3] {
        let (u, v) = match self.axis {
            Axis::X => (s, self.fixed),
            Axis::Y => (self.fixed, s),
        };
        [u, v, (1.0 - u * u - v * v).max(0.0).sqrt()]
    }

    fn angle_between(&self, a: f64, b: f64) -> f64 {
        let (p, q) = (self.direction(a), self.direction(b));
        (p[0] * q[0] + p[1] * q[1] + p[2] * q[2]).clamp(-1.0, 1.0).acos().to_degrees()
    }

    /// The pattern of a planar array mirrors in z = 0, so a beam reaching the edge of the
    /// visible disc continues through it and comes back to the same crossing.
    fn half_power_width(&self, beam: &Beam) -> f64 {
        let level = beam.peak_db - 10.0 * 2f64.log10();
        let crossing = |a: usize, b: usize| {
            let (x, y) = (self.db(a), self.db(b));
            self.coords[a] + (level - x) / (y - x) * (self.coords[b] - self.coords[a])
        };
        let left = (0..beam.index).rev().find(|k| self.db(*k) < level).map(|k| crossing(k + 1, k));
        let right = (beam.index + 1..self.coords.len()).find(|k| self.db(*k) < level).map(|k| crossing(k - 1, k));
        let last = self.coords.len() - 1;
        match (left, right) {
            (Some(a), Some(b)) => self.angle_between(a, b),
            (Some(a), None) => 2.0 * self.angle_between(a, self.coords[last]),
            (None, Some(b)) => 2.0 * self.angle_between(self.coords[0], b),
            (None, None) => 360.0,
        }
    }

    fn side_lobe_db(&self, beam: &Beam, peak_db: f64) -> Option<f64> {
        let n = self.coords.len();
        let mut best = None;
        for k in (0..beam.low).chain(beam.high + 1..n) {
            let rising = k == 0 || self.power[k - 1] <= self.power[k];
            let falling = k + 1 == n || self.power[k + 1] <= self.power[k];
            if rising && falling && self.power[k] > 0.0 {
                let rel = self.vertex(k).1 - peak_db;
                if best.is_none_or(|v| rel > v) {
                    best = Some(rel);
                }
            }
        }
        best
    }

    fn main_half_width(&self, beam: &Beam) -> f64 {
        (beam.peak - self.coords[beam.low]).max(self.coords[beam.high] - beam.peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::farfield::{Pol, Quantity};

    const F: f64 = 1e9;

    fn lambda() -> f64 {
        C0 / F
    }

    fn line(n: usize, spacing_wavelengths: f64, taper: Taper) -> ArrayConfig {
        let d = spacing_wavelengths * lambda();
        ArrayConfig { nx: n, ny: 1, dx: d, dy: d, taper_x: taper, ..ArrayConfig::default() }
    }

    fn isotropic(step: f64) -> FarField {
        let mut ff = FarField::grid(F, step, step);
        for row in &mut ff.e_theta {
            row.iter_mut().for_each(|v| *v = C64::new(1.0, 0.0));
        }
        ff.p_accepted = ff.p_rad();
        ff.p_incident = ff.p_accepted / 0.9;
        ff
    }

    /// Scans φ = `phi` over θ, returning the strongest θ, in steps of 0.05°.
    fn scan_peak(c: &ArrayConfig, phi: f64, from: f64, to: f64) -> (f64, f64) {
        let mut best = (from, f64::NEG_INFINITY);
        let count = ((to - from) / 0.05) as usize;
        for i in 0..=count {
            let theta = from + i as f64 * 0.05;
            let v = array_factor(c, F, theta, phi).norm();
            if v > best.1 {
                best = (theta, v);
            }
        }
        best
    }

    /// First side lobe of `sin(Nψ/2) / (N·sin(ψ/2))` by a fine scan of the closed form.
    fn uniform_side_lobe_db(n: usize) -> f64 {
        let nf = n as f64;
        let first_null = 2.0 * PI / nf;
        (0..2_000_000)
            .map(|i| {
                let psi = first_null + (PI - first_null) * i as f64 / 2e6;
                ((nf * psi / 2.0).sin() / (nf * (psi / 2.0).sin())).abs()
            })
            .fold(0.0, f64::max)
            .log10()
            * 20.0
    }

    #[test]
    fn uniform_eight_element_line() {
        let s = summarize(&line(8, 0.5, Taper::Uniform), F);
        println!("uniform 8: {s:?}");
        // The exact level is −12.797 dB, which is −12.80 to the two decimals of the −12.8
        // limit, so the upper limit here allows for that rounding and no more.
        let exact = uniform_side_lobe_db(8);
        println!("uniform 8: closed-form side lobe {exact:.4} dB");
        assert!((exact + 12.797).abs() < 0.001, "{exact}");
        assert!((s.sll_db - exact).abs() < 0.001, "{} vs {exact}", s.sll_db);
        assert!((-13.3..=-12.795).contains(&s.sll_db), "{}", s.sll_db);
        assert!((s.directivity_dbi - 9.03).abs() < 0.1, "{}", s.directivity_dbi);
        assert!((s.taper_efficiency - 1.0).abs() < 1e-12);
        assert!(s.taper_loss_db.abs() < 1e-9);
        assert!((s.hpbw_x_deg - 12.8).abs() < 0.3, "{}", s.hpbw_x_deg);
        assert_eq!(s.hpbw_y_deg, 360.0, "no directivity across a line");
        assert!(s.grating_lobe.is_none());
        assert_eq!(s.max_scan_no_grating_deg, 90.0);
    }

    #[test]
    fn peak_factor_is_the_element_count() {
        let c = line(8, 0.5, Taper::Uniform);
        assert!((array_factor(&c, F, 0.0, 0.0).norm() - 8.0).abs() < 1e-12);
        assert_eq!(element_positions(&c).len(), 8);
        assert_eq!(excitations(&c, F).len(), 8);
    }

    #[test]
    fn dolph_chebyshev_hits_its_side_lobe_level() {
        for (n, spacing, target) in [(8, 0.5, -30.0), (16, 0.5, -30.0), (9, 0.5, -25.0), (32, 0.5, -40.0), (20, 0.7, -30.0)] {
            let c = line(n, spacing, Taper::DolphChebyshev { sll_db: target });
            let s = summarize(&c, F);
            println!("Dolph N={n} d={spacing}λ {target} dB: sll {:.3}, eff {:.4}", s.sll_db, s.taper_efficiency);
            assert!((s.sll_db - target).abs() <= 0.5, "N={n}: {}", s.sll_db);
        }
    }

    #[test]
    fn dolph_chebyshev_is_the_chebyshev_polynomial() {
        for (n, sll) in [(8usize, 30.0), (9, 25.0), (10, 26.0), (17, 40.0)] {
            let w = taper_weights(Taper::DolphChebyshev { sll_db: -sll }, n);
            let ratio = 10f64.powf(sll / 20.0);
            let x0 = (ratio.acosh() / (n - 1) as f64).cosh();
            let at_peak: f64 = w.iter().sum();
            for k in 0..200 {
                let psi = PI * k as f64 / 199.0;
                let af: f64 = w.iter().enumerate().map(|(i, a)| a * (psi * (i as f64 - (n - 1) as f64 / 2.0)).cos()).sum();
                let want = chebyshev(n - 1, x0 * (psi / 2.0).cos()) / ratio;
                assert!((af / at_peak - want).abs() < 1e-9, "N={n} ψ={psi}: {} vs {want}", af / at_peak);
            }
        }
    }

    #[test]
    fn dolph_chebyshev_ten_element_coefficients() {
        // N = 10, R = 20 (26.02 dB), from T₉(x₀·c) = Σ aₙ·cos((2n − 1)u) by hand.
        let x0 = (20f64.acosh() / 9.0).cosh();
        let a5 = x0.powi(9);
        let a4 = 9.0 * (a5 - x0.powi(7));
        let a3 = (432.0 * (x0.powi(5) - a5) + 112.0 * a4) / 16.0;
        let a2 = (-120.0 * (x0.powi(3) - a5) + 20.0 * a3 - 56.0 * a4) / 4.0;
        let a1 = 9.0 * (x0 - a5) + 3.0 * a2 - 5.0 * a3 + 7.0 * a4;
        let w = taper_weights(Taper::DolphChebyshev { sll_db: -20.0 * 20f64.log10() }, 10);
        for (got, want) in w.iter().zip([a5, a4, a3, a2, a1]) {
            assert!((got - want / a1).abs() < 1e-9, "{w:?}: {}", want / a1);
        }
    }

    #[test]
    fn taylor_hits_its_side_lobe_level() {
        for n in [12usize, 16, 32, 64, 128] {
            let c = line(n, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 5 });
            let s = summarize(&c, F);
            println!("Taylor N={n}: sll {:.3}, eff {:.4}", s.sll_db, s.taper_efficiency);
            assert!((s.sll_db + 30.0).abs() <= 1.0, "N={n}: {}", s.sll_db);
        }
    }

    /// The −30 dB, n̄ = 5 target is not met at N = 8: with n̄ above N/2, sampling has too few
    /// elements to place the zeros. Pinned here so the limitation stays visible.
    #[test]
    fn taylor_on_a_short_array_misses_by_the_known_amount() {
        let s = summarize(&line(8, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 5 }), F);
        println!("Taylor N=8: sll {:.3}, eff {:.4}", s.sll_db, s.taper_efficiency);
        assert!((s.sll_db + 27.99).abs() < 0.02, "{}", s.sll_db);
    }

    #[test]
    fn taylor_efficiency_lies_between_uniform_and_dolph() {
        let efficiencies = |n: usize| {
            let taylor = summarize(&line(n, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 5 }), F).taper_efficiency;
            let dolph = summarize(&line(n, 0.5, Taper::DolphChebyshev { sll_db: -30.0 }), F).taper_efficiency;
            println!("N={n}: Taylor {taylor:.4}, Dolph {dolph:.4}");
            (taylor, dolph)
        };
        // Short arrays, and long ones where Dolph's edge spike takes over.
        for n in [8usize, 12, 256] {
            let (taylor, dolph) = efficiencies(n);
            assert!(dolph < taylor && taylor < 1.0, "N={n}: Taylor {taylor}, Dolph {dolph}");
        }
        // In between, Dolph at this level is really the more efficient of the two.
        for n in [16usize, 32, 64, 128] {
            let (taylor, dolph) = efficiencies(n);
            assert!(taylor < dolph && dolph < 1.0, "N={n}: Taylor {taylor}, Dolph {dolph}");
        }
    }

    #[test]
    fn taylor_large_array_tends_to_the_continuous_distribution() {
        // Continuous n̄ Taylor: first side lobe at the design level, power efficiency from
        // Parseval on the space-factor samples, independent of N.
        let s = summarize(&line(512, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 8 }), F);
        assert!((s.sll_db + 30.0).abs() < 0.3, "{}", s.sll_db);
        let a = taper_weights(Taper::Taylor { sll_db: -30.0, nbar: 8 }, 512);
        let b = taper_weights(Taper::Taylor { sll_db: -30.0, nbar: 8 }, 1024);
        assert!((a[256] - b[512]).abs() < 1e-3);
        assert!((a[0] - b[0]).abs() < 0.01);
    }

    #[test]
    fn taper_shapes() {
        let b = taper_weights(Taper::Binomial, 5);
        let want = [1.0 / 6.0, 4.0 / 6.0, 1.0, 4.0 / 6.0, 1.0 / 6.0];
        for (x, y) in b.iter().zip(want) {
            assert!((x - y).abs() < 1e-12, "{b:?}");
        }
        assert!(taper_weights(Taper::Binomial, 3000).iter().all(|v| v.is_finite()));
        let tapers = [
            Taper::Uniform,
            Taper::Binomial,
            Taper::DolphChebyshev { sll_db: 30.0 },
            Taper::Taylor { sll_db: -25.0, nbar: 4 },
            Taper::Cosine { power: 2.0 },
            Taper::Hamming,
            Taper::Blackman,
        ];
        for t in tapers {
            for n in [1usize, 2, 3, 8, 11] {
                let w = taper_weights(t, n);
                assert_eq!(w.len(), n);
                let top = w.iter().fold(0.0f64, |a, v| a.max(*v));
                assert!((top - 1.0).abs() < 1e-12, "{t:?} N={n}: {w:?}");
                for i in 0..n {
                    assert!((w[i] - w[n - 1 - i]).abs() < 1e-9, "{t:?} N={n} is not symmetric: {w:?}");
                    assert!(w[i] > 0.0, "{t:?} N={n} has a dead element: {w:?}");
                }
            }
        }
        assert!(taper_weights(Taper::Uniform, 0).is_empty());
        let hamming = summarize(&line(32, 0.5, Taper::Hamming), F);
        assert!((hamming.sll_db + 42.0).abs() < 2.0, "Hamming: {}", hamming.sll_db);
        let cosine = summarize(&line(32, 0.5, Taper::Cosine { power: 1.0 }), F);
        assert!((cosine.sll_db + 23.0).abs() < 1.0, "cosine: {}", cosine.sll_db);
        let blackman = summarize(&line(32, 0.5, Taper::Blackman), F);
        assert!((blackman.sll_db + 58.0).abs() < 3.0, "Blackman: {}", blackman.sll_db);
    }

    #[test]
    fn grating_lobe_is_predicted_exactly() {
        let mut c = line(16, 0.7, Taper::Uniform);
        c.steer_theta = 30.0;
        let s = summarize(&c, F);
        let (theta, phi) = s.grating_lobe.expect("a lobe at d = 0.7λ scanned 30°");
        let want = (1.0f64 / 0.7 - 0.5).asin().to_degrees();
        println!("grating lobe predicted at θ = {theta:.3}°, φ = {phi}, closed form {want:.3}°");
        assert!((theta - want).abs() < 1e-9, "{theta} vs {want}");
        assert_eq!(phi, 180.0);
        let (scanned, level) = scan_peak(&c, 180.0, 50.0, 90.0);
        println!("brute-force peak at θ = {scanned:.2}°, {level:.3} of 16");
        assert!((scanned - want).abs() < 0.1, "{scanned}");
        assert!(level > 15.9, "as strong as the main beam: {level}");
        assert!(s.sll_db > -0.1, "a grating lobe counts as a side lobe: {}", s.sll_db);
        let scan_limit = (1.0f64 / 0.7 - 1.0).asin().to_degrees();
        assert!((s.max_scan_no_grating_deg - scan_limit).abs() < 1e-9, "{}", s.max_scan_no_grating_deg);

        let mut clean = line(16, 0.5, Taper::Uniform);
        clean.steer_theta = 30.0;
        let s = summarize(&clean, F);
        assert!(s.grating_lobe.is_none());
        assert!(s.sll_db < -12.0, "{}", s.sll_db);
    }

    #[test]
    fn grating_lobes_of_planar_lattices() {
        let d = 0.7 * lambda();
        let mut rect = ArrayConfig { nx: 12, ny: 12, dx: d, dy: d, ..ArrayConfig::default() };
        rect.steer_theta = 30.0;
        let (theta, phi) = summarize(&rect, F).grating_lobe.unwrap();
        assert!((theta - (1.0f64 / 0.7 - 0.5).asin().to_degrees()).abs() < 1e-9 && phi == 180.0, "{theta}, {phi}");

        let broadside = ArrayConfig { nx: 12, ny: 12, dx: d, dy: d, ..ArrayConfig::default() };
        assert!(summarize(&broadside, F).grating_lobe.is_none());
        assert!((summarize(&broadside, F).max_scan_no_grating_deg - (1.0f64 / 0.7 - 1.0).asin().to_degrees()).abs() < 1e-9);

        let wide = ArrayConfig { nx: 12, ny: 12, dx: 1.2 * lambda(), dy: 0.5 * lambda(), ..ArrayConfig::default() };
        let s = summarize(&wide, F);
        assert!(s.grating_lobe.is_some(), "d > λ has a lobe at broadside");
        assert_eq!(s.max_scan_no_grating_deg, 0.0);

        // An equilateral lattice of pitch 0.9λ: its nearest lobes sit at sin θ = 1.283, outside
        // the visible disc at broadside, where a square lattice of 1.05λ already has one.
        let (pitch, row) = (0.9 * lambda(), 0.9 * 3f64.sqrt() / 2.0 * lambda());
        let tri = ArrayConfig { nx: 15, ny: 15, dx: pitch, dy: row, lattice: Lattice::Triangular, ..ArrayConfig::default() };
        let tri_summary = summarize(&tri, F);
        println!("triangular: {tri_summary:?}");
        assert!(tri_summary.grating_lobe.is_none(), "{:?}", tri_summary.grating_lobe);
        let square = ArrayConfig { nx: 15, ny: 15, dx: 1.05 * lambda(), dy: 1.05 * lambda(), ..ArrayConfig::default() };
        assert!(summarize(&square, F).grating_lobe.is_some());

        // The same triangular lattice scanned past its limit, checked against the pattern.
        let mut scanned = tri.clone();
        scanned.steer_theta = 30.0;
        scanned.steer_phi = 0.0;
        let s = summarize(&scanned, F);
        let (t, p) = s.grating_lobe.expect("scanned triangular lattice has a lobe");
        let want = array_factor(&scanned, F, 30.0, 0.0).norm();
        let got = array_factor(&scanned, F, t, p).norm();
        println!("triangular lobe at ({t:.2}, {p:.2}): {got:.2} of {want:.2}, sll {:.2}", s.sll_db);
        assert!(got > 0.95 * want, "{got} vs {want}");
        assert!(s.sll_db > -1.0, "the 2-D search must see it: {}", s.sll_db);
    }

    #[test]
    fn steering_puts_the_peak_where_requested() {
        for theta in [0.0, 10.0, 25.0, 45.0, 60.0] {
            let mut c = line(16, 0.5, Taper::Uniform);
            c.steer_theta = theta;
            let (found, level) = scan_peak(&c, 0.0, 0.0, 89.0);
            assert!((found - theta).abs() < 1.0, "{found} vs {theta}");
            assert!((level - 16.0).abs() < 0.05);
        }
        let d = 0.5 * lambda();
        let planar =
            ArrayConfig { nx: 10, ny: 10, dx: d, dy: d, steer_theta: 35.0, steer_phi: 40.0, ..ArrayConfig::default() };
        let (found, level) = scan_peak(&planar, 40.0, 0.0, 89.0);
        assert!((found - 35.0).abs() < 1.0, "{found}");
        assert!((level - 100.0).abs() < 0.5, "{level}");
        assert!((array_factor(&planar, F, 35.0, 40.0).norm() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn scanning_broadens_the_beam_by_one_over_cos_theta() {
        let base = summarize(&line(32, 0.5, Taper::Uniform), F).hpbw_x_deg;
        let mut c = line(32, 0.5, Taper::Uniform);
        c.steer_theta = 45.0;
        let scanned = summarize(&c, F).hpbw_x_deg;
        let ratio = scanned / base;
        assert!((ratio - 2f64.sqrt()).abs() < 0.03, "{ratio}");
    }

    #[test]
    fn endfire_beam_width_mirrors_through_the_horizon() {
        // Ordinary endfire, d = λ/4, N = 16: HPBW ≈ 2·acos(1 − 0.443·λ/(N·d)).
        let mut c = line(16, 0.25, Taper::Uniform);
        c.steer_theta = 90.0;
        let s = summarize(&c, F);
        let want = 2.0 * (1.0f64 - 0.443 / 4.0).acos().to_degrees();
        assert!((s.hpbw_x_deg - want).abs() < 2.0, "{} vs {want}", s.hpbw_x_deg);
    }

    fn apply_matches_summary(c: &ArrayConfig, step: f64) -> f64 {
        let out = apply(&isotropic(step), c);
        let from_pattern = out.metrics().peak_directivity_dbi;
        let from_summary = summarize(c, F).directivity_dbi;
        from_pattern - from_summary
    }

    #[test]
    fn apply_on_an_isotropic_element_matches_summarize() {
        let d = lambda();
        let cases = [
            line(8, 0.5, Taper::Uniform),
            line(8, 0.25, Taper::Uniform),
            line(12, 0.7, Taper::DolphChebyshev { sll_db: -25.0 }),
            ArrayConfig {
                nx: 4,
                ny: 6,
                dx: 0.6 * d,
                dy: 0.45 * d,
                steer_theta: 25.0,
                steer_phi: 30.0,
                taper_x: Taper::Taylor { sll_db: -25.0, nbar: 3 },
                taper_y: Taper::Cosine { power: 1.0 },
                ..ArrayConfig::default()
            },
            ArrayConfig {
                nx: 5,
                ny: 5,
                dx: 0.6 * d,
                dy: 0.52 * d,
                lattice: Lattice::Triangular,
                steer_theta: 15.0,
                steer_phi: 200.0,
                ..ArrayConfig::default()
            },
        ];
        for c in &cases {
            let difference = apply_matches_summary(c, 1.0);
            println!("apply vs summarize: {difference:+.3} dB ({}×{})", c.nx, c.ny);
            assert!(difference.abs() < 0.2, "{difference} dB off for {c:?}");
        }
    }

    /// A beam between grid lines must still read its true peak.
    #[test]
    fn array_gain_finds_an_off_grid_beam() {
        for n in [32usize, 64, 128] {
            let mut c = line(n, 0.5, Taper::Uniform);
            c.steer_theta = 31.0;
            let element = isotropic(3.0);
            let g = gain(&element, &c);
            let want = summarize(&c, F).directivity_dbi;
            println!("N = {n}: gain() directivity {:.3}, array factor {want:.3}, grid read {:.3}", g.directivity_dbi, apply(&element, &c).metrics().peak_directivity_dbi);
            assert!((g.directivity_dbi - want).abs() < 0.05, "N = {n}: {} vs {want}", g.directivity_dbi);
            // Any direction on the cone is the peak of a line of isotropic elements.
            let cosine = direction_cosines(g.peak_theta, g.peak_phi).0;
            assert!((cosine - 31f64.to_radians().sin()).abs() < 0.2 / n as f64, "N = {n}: u = {cosine}");
        }
    }

    /// z-directed dipoles along x, the audit's case: the array's real directivity at each spacing.
    fn z_dipole_element(step: f64) -> FarField {
        let mut ff = FarField::grid(F, step, step);
        for ti in 0..ff.theta.len() {
            let s = ff.theta[ti].to_radians().sin();
            for pi in 0..ff.phi.len() {
                ff.e_theta[ti][pi] = C64::new(s, 0.0);
            }
        }
        ff.p_accepted = ff.p_rad();
        ff.p_incident = ff.p_accepted;
        ff
    }

    /// Eight z-dipoles along x, from the pair sum for short dipoles.
    fn z_dipole_line_directivity(n: usize, d: f64) -> f64 {
        // ∫sin²θ·exp(jkΔ·sinθcosφ)dΩ with kΔ = x: 8π/3·3·[sin x/x·... ] in closed form.
        let pair = |x: f64| if x < 1e-9 { 1.0 } else { 1.5 * (x.sin() / x - (x.sin() / x - x.cos()) / (x * x)) };
        let k = 2.0 * PI;
        let power: f64 = (0..n).flat_map(|i| (0..n).map(move |j| (i, j))).map(|(i, j)| pair(k * d * i.abs_diff(j) as f64)).sum();
        // Broadside of the line and of the dipole: U = N²·sin²θ, average sin²θ·... = 2/3.
        (n * n) as f64 * 1.5 / power
    }

    #[test]
    fn array_gain_never_exceeds_directivity_of_the_real_pattern() {
        let element = z_dipole_element(3.0);
        for d in [0.05, 0.1, 0.25, 0.5, 0.7] {
            let c = line(8, d, Taper::Uniform);
            let g = gain(&element, &c);
            let want = db10(z_dipole_line_directivity(8, d));
            println!("d = {d}λ: directivity {:.3}, closed form {want:.3}, gain {:.3}, realized {:.3}, close {}", g.directivity_dbi, g.gain_dbi, g.realized_dbi, g.close_spacing);
            assert!((g.directivity_dbi - want).abs() < 0.03, "d = {d}: {} vs {want}", g.directivity_dbi);
            // A lossless, matched element: gain is the directivity.
            assert!((g.gain_dbi - g.directivity_dbi).abs() < 1e-6, "{g:?}");
            assert_eq!(g.close_spacing, d < 0.5, "d = {d}");
        }
    }

    /// Loss and mismatch of the element carry through as they are, at any spacing.
    #[test]
    fn array_gain_carries_the_element_efficiency() {
        let mut element = z_dipole_element(3.0);
        let rad = element.p_rad();
        element.p_accepted = rad / 0.5;
        element.p_incident = rad / 0.25;
        let g = gain(&element, &line(8, 0.25, Taper::Uniform));
        assert!((g.directivity_dbi - g.gain_dbi - 3.0103).abs() < 1e-3, "{g:?}");
        assert!((g.gain_dbi - g.realized_dbi - 3.0103).abs() < 1e-3, "{g:?}");
    }

    /// Too large for the grid: the estimate has to land near the integral it stands in for.
    #[test]
    fn the_large_array_estimate_agrees_with_the_integral() {
        let d = 0.5 * lambda();
        let element = {
            // Half-space cos θ element, like a patch.
            let mut ff = FarField::grid(F, 3.0, 3.0);
            for ti in 0..ff.theta.len() {
                let v = ff.theta[ti].to_radians().cos().max(0.0);
                for pi in 0..ff.phi.len() {
                    ff.e_theta[ti][pi] = C64::new(v, 0.0);
                }
            }
            ff.p_accepted = ff.p_rad();
            ff.p_incident = ff.p_accepted;
            ff
        };
        for steer in [0.0, 30.0] {
            let c = ArrayConfig { nx: 24, ny: 24, dx: d, dy: d, steer_theta: steer, ..ArrayConfig::default() };
            let exact = gain(&element, &c);
            let rough = gain_within_budget(&element, &c, 1.0);
            println!("steer {steer}: integral {:.3} dBi, estimate {:.3} dBi", exact.directivity_dbi, rough.directivity_dbi);
            assert!(!exact.estimated && rough.estimated);
            assert!((exact.directivity_dbi - rough.directivity_dbi).abs() < 0.3, "{exact:?} {rough:?}");
        }
    }

    #[test]
    fn apply_scales_power_with_the_weights() {
        let element = isotropic(2.0);
        let c = line(8, 0.5, Taper::Hamming);
        let out = apply(&element, &c);
        let sum: f64 = excitations(&c, F).iter().map(C64::norm_sqr).sum();
        // At λ/2 on a line, isotropic elements are orthogonal: what radiates is Σ|w|², as
        // an integral on the grid can tell. Closer than that it is not, see below.
        assert!((out.p_accepted / element.p_accepted / sum - 1.0).abs() < 2e-3, "{}", out.p_accepted / element.p_accepted / sum);
        assert!((out.p_incident / element.p_incident / sum - 1.0).abs() < 2e-3);
        let tight = apply(&element, &line(8, 0.25, Taper::Hamming));
        let m = tight.metrics();
        assert!(m.peak_gain_dbi <= m.peak_directivity_dbi + 0.02, "{m:?}");
        let (a, b) = (out.p_accepted / out.p_incident, element.p_accepted / element.p_incident);
        assert!((a - b).abs() < 1e-12, "the mismatch ratio is conserved");
        assert_eq!(out.theta, element.theta);
        assert_eq!(out.phi, element.phi);

        // At λ/2 the elements are orthogonal on the sphere: gain equals directivity.
        let m = apply(&element, &line(8, 0.5, Taper::Uniform)).metrics();
        assert!((m.peak_gain_dbi - m.peak_directivity_dbi).abs() < 0.05, "{m:?}");
        let realized = m.peak_gain_dbi + db10(0.9);
        assert!((m.peak_realized_dbi - realized).abs() < 1e-9);
    }

    #[test]
    fn apply_multiplies_a_real_element_pattern() {
        let mut element = FarField::grid(F, 2.0, 2.0);
        for ti in 0..element.theta.len() {
            let v = element.theta[ti].to_radians().cos();
            for pi in 0..element.phi.len() {
                element.e_theta[ti][pi] = C64::new(v, 0.0);
                element.e_phi[ti][pi] = C64::new(0.0, 0.5 * v);
            }
        }
        element.p_accepted = element.p_rad();
        element.p_incident = element.p_accepted;
        let c = line(4, 0.5, Taper::Uniform);
        let out = apply(&element, &c);
        let af = array_factor(&c, F, 30.0, 90.0);
        let (ti, pi) = (15, 45);
        assert_eq!(element.theta[ti], 30.0);
        assert_eq!(element.phi[pi], 90.0);
        assert!((out.e_theta[ti][pi] - element.e_theta[ti][pi] * af).norm() < 1e-12);
        assert!((out.e_phi[ti][pi] - element.e_phi[ti][pi] * af).norm() < 1e-12);
        let db = out.value_db(0, 0, Quantity::Directivity, Pol::Theta);
        assert!(db.is_finite());
    }

    #[test]
    fn phase_quantisation_raises_side_lobes() {
        let mut ideal = line(32, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 5 });
        ideal.steer_theta = 21.0;
        let mut coarse = ideal.clone();
        coarse.phase_bits = Some(3);
        let (a, b) = (summarize(&ideal, F), summarize(&coarse, F));
        println!("3-bit: ideal {:.2} dB, quantised {:.2} dB", a.sll_db, b.sll_db);
        assert!(b.sll_db > a.sll_db + 1.0, "{} vs {}", b.sll_db, a.sll_db);
        let lsb = 2.0 * PI / 8.0;
        for w in excitations(&coarse, F) {
            let steps = w.arg() / lsb;
            assert!((steps - steps.round()).abs() < 1e-9, "{}", w.arg());
        }
        let mut fine = ideal.clone();
        fine.phase_bits = Some(8);
        let f = summarize(&fine, F);
        assert!((f.sll_db - a.sll_db).abs() < 1.0, "8 bits are nearly ideal: {} vs {}", f.sll_db, a.sll_db);
    }

    #[test]
    fn a_failed_element_lowers_the_gain_as_expected() {
        let healthy = line(8, 0.5, Taper::Uniform);
        let mut broken = healthy.clone();
        broken.failed = vec![(3, 0)];
        let (a, b) = (summarize(&healthy, F), summarize(&broken, F));
        // Orthogonal at λ/2, so D = N: from 8 to 7, and the peak field from 8 to 7.
        let expected = db10(7.0 / 8.0);
        println!("failed element: directivity {:.3} -> {:.3} dBi, expected change {expected:.3}", a.directivity_dbi, b.directivity_dbi);
        assert!((b.directivity_dbi - a.directivity_dbi - expected).abs() < 0.02);
        assert_eq!(excitations(&broken, F)[3].norm(), 0.0);
        assert!((array_factor(&broken, F, 0.0, 0.0).norm() - 7.0).abs() < 1e-12);
        assert!(b.sll_db > a.sll_db, "{} vs {}", b.sll_db, a.sll_db);

        let element = isotropic(2.0);
        let (ga, gb) = (apply(&element, &healthy).metrics(), apply(&element, &broken).metrics());
        assert!((gb.peak_gain_dbi - ga.peak_gain_dbi - expected).abs() < 0.05, "{} {}", gb.peak_gain_dbi, ga.peak_gain_dbi);
        let eirp = 20.0 * (7.0f64 / 8.0).log10();
        let (fa, fb) = (apply(&element, &healthy), apply(&element, &broken));
        let (ia, ib) = (fa.intensity(0, 0), fb.intensity(0, 0));
        assert!((db10(ib / ia) - eirp).abs() < 1e-9, "the same drive per element loses 1.16 dB of EIRP");
        assert!((summarize(&broken, F).taper_efficiency - 7.0 / 8.0).abs() < 1e-12);
    }

    #[test]
    fn random_errors_are_seeded_and_have_the_requested_size() {
        let mut c = ArrayConfig { nx: 200, ny: 200, ..ArrayConfig::default() };
        c.amp_err_db = 1.0;
        c.phase_err_deg = 5.0;
        c.seed = 9;
        let a = excitations(&c, F);
        assert_eq!(a, excitations(&c, F), "same seed, same errors");
        c.seed = 10;
        assert_ne!(a, excitations(&c, F));

        let n = a.len() as f64;
        let rms_db = (a.iter().map(|w| (20.0 * w.norm().log10()).powi(2)).sum::<f64>() / n).sqrt();
        assert!((rms_db - 1.0).abs() < 0.03, "{rms_db}");
        c.seed = 9;
        let steer_free = ArrayConfig { amp_err_db: 0.0, ..c.clone() };
        let rms_phase = (excitations(&steer_free, F).iter().map(|w| w.arg().to_degrees().powi(2)).sum::<f64>() / n).sqrt();
        assert!((rms_phase - 5.0).abs() < 0.15, "{rms_phase}");

        let mut noisy = line(32, 0.5, Taper::Taylor { sll_db: -35.0, nbar: 5 });
        let clean = summarize(&noisy, F).sll_db;
        noisy.amp_err_db = 1.5;
        noisy.phase_err_deg = 15.0;
        noisy.seed = 3;
        assert!(summarize(&noisy, F).sll_db > clean, "errors fill the nulls");
    }

    #[test]
    fn positions_are_centred_and_ordered() {
        let d = 0.01;
        let c = ArrayConfig { nx: 3, ny: 2, dx: d, dy: 2.0 * d, ..ArrayConfig::default() };
        let p = element_positions(&c);
        assert_eq!(p.len(), 6);
        let sum: [f64; 3] = p.iter().fold([0.0; 3], |a, q| [a[0] + q[0], a[1] + q[1], a[2] + q[2]]);
        assert!(sum.iter().all(|v| v.abs() < 1e-12));
        assert!((p[1][0] - p[0][0] - d).abs() < 1e-12, "x runs inside a row");
        assert!((p[3][1] - p[0][1] - 2.0 * d).abs() < 1e-12, "rows step in y");
        assert!((p[3][0] - p[0][0]).abs() < 1e-12);

        let tri = ArrayConfig { lattice: Lattice::Triangular, ..c.clone() };
        let q = element_positions(&tri);
        assert!((q[3][0] - q[0][0] - 0.5 * d).abs() < 1e-12, "odd rows shift by dx/2");
        let sum_x: f64 = q.iter().map(|r| r[0]).sum();
        assert!(sum_x.abs() < 1e-12);

        let column = ArrayConfig { nx: 1, ny: 4, lattice: Lattice::Triangular, ..c.clone() };
        assert!(element_positions(&column).iter().all(|r| r[0].abs() < 1e-12));
    }

    #[test]
    fn planar_taper_and_directivity() {
        let d = 0.5 * lambda();
        let c = ArrayConfig {
            nx: 8,
            ny: 8,
            dx: d,
            dy: d,
            taper_x: Taper::Taylor { sll_db: -30.0, nbar: 5 },
            taper_y: Taper::Taylor { sll_db: -30.0, nbar: 5 },
            ..ArrayConfig::default()
        };
        let s = summarize(&c, F);
        let single = summarize(&line(8, 0.5, Taper::Taylor { sll_db: -30.0, nbar: 5 }), F);
        assert!((s.taper_efficiency - single.taper_efficiency * single.taper_efficiency).abs() < 1e-9);
        assert!((s.taper_loss_db - 2.0 * single.taper_loss_db).abs() < 1e-9);
        assert!((s.hpbw_x_deg - s.hpbw_y_deg).abs() < 1e-6);
        assert!((s.sll_db - single.sll_db).abs() < 0.05, "{} vs {}", s.sll_db, single.sll_db);
        // Broadside aperture: 4π·A·η/λ², with the isotropic element's doubled lobe split.
        let area = (8.0 * d) * (8.0 * d);
        let aperture = db10(4.0 * PI * area * s.taper_efficiency / (lambda() * lambda()) / 2.0);
        assert!((s.directivity_dbi - aperture).abs() < 0.5, "{} vs {aperture}", s.directivity_dbi);
    }

    #[test]
    fn degenerate_arrays_stay_finite() {
        let one = ArrayConfig { nx: 1, ny: 1, ..ArrayConfig::default() };
        let s = summarize(&one, F);
        assert!(s.directivity_dbi.abs() < 1e-9);
        assert_eq!(s.hpbw_x_deg, 360.0);
        assert_eq!(s.sll_db, f64::NEG_INFINITY);
        assert!(s.grating_lobe.is_none());
        let empty = ArrayConfig { nx: 0, ny: 0, ..ArrayConfig::default() };
        assert!(element_positions(&empty).is_empty());
        assert_eq!(array_factor(&empty, F, 0.0, 0.0), C64::new(0.0, 0.0));
        let nothing = summarize(&empty, F);
        assert!(nothing.directivity_dbi.is_finite() && nothing.taper_efficiency == 0.0);
        let dead = ArrayConfig { nx: 2, ny: 1, failed: vec![(0, 0), (1, 0)], ..ArrayConfig::default() };
        assert!(summarize(&dead, F).directivity_dbi.is_finite());
    }
}
