//! The method of moments' Green's function for flat metal inside a stack of layers, so paper, film,
//! a carton or a bottle cost no unknowns however thin they are. This is the mixed-potential integral
//! equation in layered media (Michalski and Zheng, IEEE TAP 1990, formulation C), for horizontal
//! current in one plane, which leaves two functions of distance alone.
//!
//! Along z each mode is a transmission line, so the fields of a current sheet are the line's voltage,
//! `V`, and both kernels come from its spectrum:
//!
//! `G_A  = (1/2π)∫ V_TE/(jω) J0(κρ) κ dκ`, and
//! `G_φ  = (1/2π)∫ jω/κ² (V_TM - V_TE) J0(κρ) κ dκ`.
//!
//! Normalised to `exp(-jkρ)/ρ` in free space, each is that free-space term, plus static images
//! `Σ a/√(ρ²+b²)` taking what changes quickly close in, plus a smooth rest from the integral. Images
//! are what thin layers come to, and integrating one over a triangle has a closed form, so a 20 µm
//! film needs no fine mesh. Only images deeper than the mesh can see go into the rest.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use rayon::prelude::*;

use crate::farfield::FarField;
use crate::geometry::dot;
use crate::linalg::Lu;
use crate::stack::{Medium, Stack, media};
use crate::{C0, C64, ETA0};

use super::far::{Moment, spherical_axes, wavenumber};
use super::quad::GL6;

const J: C64 = C64::new(0.0, 1.0);

fn j0_series(z: C64) -> C64 {
    let q = -z * z / 4.0;
    let (mut term, mut sum) = (C64::new(1.0, 0.0), C64::new(1.0, 0.0));
    for m in 1..80 {
        term *= q / (m * m) as f64;
        sum += term;
        if term.norm_sqr() < 1e-34 * sum.norm_sqr() {
            break;
        }
    }
    sum
}

/// Hankel's expansion, for `Re z ≥ 0`.
fn j0_hankel(z: C64) -> C64 {
    let w = C64::new(1.0, 0.0) / (z * 8.0);
    let w2 = w * w;
    let w4 = w2 * w2;
    let p = C64::new(1.0, 0.0) - w2 * 4.5 + w4 * (11025.0 / 24.0) - w4 * w2 * (9801.0 * 11025.0 / 720.0) + w4 * w4 * (9801.0 * 11025.0 * 169.0 * 225.0 / 40320.0);
    let q = -w + w2 * w * 37.5 - w4 * w * (893_025.0 / 120.0) + w4 * w2 * w * (893_025.0 * 121.0 * 169.0 / 5040.0);
    let chi = z - PI / 4.0;
    (z * (PI / 2.0)).inv().sqrt() * (p * chi.cos() - q * chi.sin())
}

/// `J0` of a complex argument.
pub(super) fn bessel_j0(z: C64) -> C64 {
    if z.norm_sqr() < 144.0 {
        j0_series(z)
    } else if z.re < 0.0 {
        j0_hankel(-z)
    } else {
        j0_hankel(z)
    }
}

/// `kz` with the wave dying away from the source. Only a half-space cares about the sign, and a
/// layer's line is even in it.
fn kz(k0: f64, er: C64, kr: C64) -> C64 {
    let v = (er * (k0 * k0) - kr * kr).sqrt();
    if v.im > 0.0 || (v.im == 0.0 && v.re < 0.0) { -v } else { v }
}

/// A mode's impedance over η0.
fn mode_z(te: bool, k0: f64, er: C64, kz: C64) -> C64 {
    if te { C64::new(k0, 0.0) / kz } else { kz / (er * k0) }
}

/// The stack at one frequency.
pub(super) struct Spectral {
    k0: f64,
    above: Vec<Medium>,
    below: Vec<Medium>,
    pec: bool,
}

/// Looking out from the metal through `layers`: the impedance over η0, and the outermost face's
/// voltage for 1 V at the metal.
fn look(layers: &[Medium], pec: bool, te: bool, k0: f64, kr: C64) -> (C64, C64) {
    let air = C64::new(1.0, 0.0);
    let mut z_l = if pec { C64::new(0.0, 0.0) } else { mode_z(te, k0, air, kz(k0, air, kr)) };
    let mut out = C64::new(1.0, 0.0);
    for m in layers.iter().rev() {
        let kzi = kz(k0, m.er, kr);
        let zi = mode_z(te, k0, m.er, kzi);
        let g = (z_l - zi) / (z_l + zi);
        let half = (-J * kzi * m.d).exp();
        let p = half * half;
        out *= half * (g + 1.0) / (g * p + 1.0);
        z_l = zi * (g * p + 1.0) / (-(g * p) + 1.0);
    }
    (z_l, out)
}

fn parallel(a: C64, b: C64) -> C64 {
    a * b / (a + b)
}

impl Spectral {
    pub(super) fn new(stack: &Stack, f: f64) -> Self {
        Self { k0: wavenumber(f), above: media(&stack.above, f), below: media(&stack.below, f), pec: stack.metal_behind }
    }

    /// The kernels' spectra, normalised to `κ/(j·kz0)` in free space, as `(A, φ)`.
    fn spectra(&self, kr: C64) -> (C64, C64) {
        let k0 = self.k0;
        let vh = parallel(look(&self.above, false, true, k0, kr).0, look(&self.below, self.pec, true, k0, kr).0);
        let ve = parallel(look(&self.above, false, false, k0, kr).0, look(&self.below, self.pec, false, k0, kr).0);
        (vh * kr * 2.0 / (J * k0), J * (ve - vh) * 2.0 * k0 / kr)
    }

    /// The same two in the static limit, where κ is all that's left of `kz` and they're a sum of
    /// exponentials in κ.
    fn statics(&self, kr: f64) -> (C64, C64) {
        let one = C64::new(1.0, 0.0);
        let walk = |layers: &[Medium], pec: bool, te: bool| {
            let mut z_l = if pec { C64::new(0.0, 0.0) } else { one };
            for m in layers.iter().rev() {
                let zi = if te { one } else { one / m.er };
                let g = (z_l - zi) / (z_l + zi);
                let p = (-2.0 * kr * m.d).exp();
                z_l = zi * (g * p + 1.0) / (-(g * p) + 1.0);
            }
            z_l
        };
        let side = |te: bool| parallel(walk(&self.above, false, te), walk(&self.below, self.pec, te)) * 2.0;
        (side(true), side(false))
    }

    /// The static φ spectrum as κ goes to infinity: only the two media touching the metal count.
    fn phi_limit(&self) -> C64 {
        let touching = |l: &[Medium]| l.first().map_or(C64::new(1.0, 0.0), |m| m.er);
        C64::new(2.0, 0.0) / (touching(&self.above) + touching(&self.below))
    }

    fn thinnest(&self) -> Option<f64> {
        self.above.iter().chain(&self.below).map(|m| m.d).reduce(f64::min)
    }

    fn total(&self) -> f64 {
        self.above.iter().chain(&self.below).map(|m| m.d).sum()
    }

    fn densest(&self) -> f64 {
        self.above.iter().chain(&self.below).map(|m| m.er.norm()).fold(1.0, f64::max)
    }

    /// What the layers do to the wave leaving at angle θ from the z axis on `upper`'s side, against
    /// free space with the same current, referred to the metal's plane: `(TM, TE)`.
    fn radiation(&self, theta_deg: f64, upper: bool) -> (C64, C64) {
        let (layers, other, pec, other_pec) = if upper { (&self.above, &self.below, false, self.pec) } else { (&self.below, &self.above, self.pec, false) };
        if pec {
            return (C64::new(0.0, 0.0), C64::new(0.0, 0.0));
        }
        let k0 = self.k0;
        let kr = C64::new(k0 * theta_deg.to_radians().sin(), 0.0);
        let air = C64::new(1.0, 0.0);
        let kz0 = kz(k0, air, kr);
        let depth: f64 = layers.iter().map(|m| m.d).sum();
        let out_of_stack = (J * kz0 * depth).exp();
        let one = |te: bool| {
            let (z_out, t) = look(layers, false, te, k0, kr);
            let (z_back, _) = look(other, other_pec, te, k0, kr);
            parallel(z_out, z_back) * 2.0 / mode_z(te, k0, air, kz0) * t * out_of_stack
        };
        (one(false), one(true))
    }
}

/// A complex function of distance, on a grid even in `ln ρ`.
struct Table {
    log0: f64,
    step: f64,
    v: Vec<C64>,
}

impl Table {
    fn at(&self, rho: f64) -> C64 {
        let n = self.v.len();
        let x = ((rho.max(1e-30).ln() - self.log0) / self.step).clamp(0.0, (n - 1) as f64);
        let i = (x as usize).min(n - 2);
        let t = x - i as f64;
        let y1 = self.v[i];
        let y2 = self.v[i + 1];
        let y0 = if i > 0 { self.v[i - 1] } else { y1 * 2.0 - y2 };
        let y3 = if i + 2 < n { self.v[i + 2] } else { y2 * 2.0 - y1 };
        let (t2, t3) = (t * t, t * t * t);
        (y1 * 2.0 + (y2 - y0) * t + (y0 * 2.0 - y1 * 5.0 + y2 * 4.0 - y3) * t2 + (y1 * 3.0 - y0 - y2 * 3.0 + y3) * t3) * 0.5
    }
}

/// One kernel, less the free-space `exp(-jkρ)/ρ`.
pub(super) struct Kernel {
    /// `(a, b)` of `a/√(ρ²+b²)`, closed form on a triangle. `b` may be zero.
    pub images: Vec<(C64, f64)>,
    /// The rest: smooth at the mesh's scale.
    smooth: Table,
    /// Images and rest together, times ρ, for pairs too far apart to care which is which.
    whole: Table,
}

impl Kernel {
    #[inline]
    pub(super) fn smooth_at(&self, rho: f64) -> C64 {
        self.smooth.at(rho)
    }
    #[inline]
    pub(super) fn whole_at(&self, rho: f64) -> C64 {
        self.whole.at(rho) / rho.max(1e-30)
    }
}

pub(super) struct Kernels {
    pub a: Kernel,
    pub phi: Kernel,
}

/// Least squares of `target(κ)` on `exp(-κ·b)`, a little regularised: these aren't independent and
/// don't need to be, since what the fit leaves goes to the integral.
fn fit_images(target: &dyn Fn(f64) -> C64, depths: &[f64], lo: f64, hi: f64) -> Vec<(C64, f64)> {
    let n = depths.len();
    if n == 0 {
        return Vec::new();
    }
    const SAMPLES: usize = 240;
    let row = |i: usize| lo * (hi / lo).powf(i as f64 / (SAMPLES - 1) as f64);
    let mut ata = vec![C64::new(0.0, 0.0); n * n];
    let mut atb = vec![C64::new(0.0, 0.0); n];
    for i in 0..SAMPLES {
        let kr = row(i);
        let e: Vec<f64> = depths.iter().map(|b| (-kr * b).exp()).collect();
        let t = target(kr);
        for p in 0..n {
            atb[p] += t * e[p];
            for q in 0..n {
                ata[p * n + q] += C64::new(e[p] * e[q], 0.0);
            }
        }
    }
    let trace: f64 = (0..n).map(|p| ata[p * n + p].re).sum();
    for p in 0..n {
        ata[p * n + p] += C64::new(1e-9 * trace / n as f64, 0.0);
    }
    let Ok(lu) = Lu::factor(ata, n) else { return Vec::new() };
    lu.solve(&mut atb);
    depths.iter().zip(atb).filter(|(_, a)| a.norm() > 1e-7).map(|(b, a)| (a, *b)).collect()
}

/// The depths images may sit at: every layer's and pile's round trip, then enough in between to
/// follow a thin stack's long series. Below `cut`: past that the mesh can follow them by itself.
fn image_depths(s: &Spectral, cut: f64) -> Vec<f64> {
    let Some(thin) = s.thinnest() else { return Vec::new() };
    let mut out = Vec::new();
    for layers in [&s.above, &s.below] {
        let mut pile = 0.0;
        for m in layers.iter() {
            pile += m.d;
            out.push(2.0 * m.d);
            out.push(2.0 * pile);
        }
    }
    let top = cut.min(40.0 * s.total());
    let mut b = 2.0 * thin;
    while b < top {
        out.push(b);
        b *= 10f64.powf(0.2);
    }
    out.retain(|b| *b < cut);
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 0.04 * *b);
    out
}

/// Distances the tables run over.
const RHO_POINTS: usize = 420;
const RHO_MIN: f64 = 1e-6;

/// `∫ T(κ) J0(κρ) dκ` for the rest of both kernels. Around the surface-wave poles and the branch
/// point the path rises off the real axis into the first quadrant, which is where exp(+jωt) leaves
/// it free; then it runs out along the real axis, until the oscillations of `J0` have cancelled what
/// the smooth spectrum still holds.
fn rest_at(s: &Spectral, rho: f64, a_im: &[(C64, f64)], phi_im: &[(C64, f64)], k_max: f64) -> (C64, C64) {
    let k0 = s.k0;
    let k1 = 1.3 * k0 * s.densest().sqrt() + 0.2 * k0;
    let rest = |kr: C64| -> (C64, C64) {
        let (fa, fp) = s.spectra(kr);
        let free = kr / (J * kz(k0, C64::new(1.0, 0.0), kr));
        let sum = |im: &[(C64, f64)]| -> C64 { im.iter().map(|(a, b)| *a * (-kr * *b).exp()).sum() };
        (fa - free - sum(a_im), fp - free - sum(phi_im))
    };
    let (mut ta, mut tp) = (C64::new(0.0, 0.0), C64::new(0.0, 0.0));
    let lift = (0.35 * k0).min(1.0 / rho);
    let panels = (k1 / lift).ceil().clamp(8.0, 600.0) as usize;
    let width = k1 / panels as f64;
    for i in 0..panels {
        let a = i as f64 * width;
        for (x, w) in GL6 {
            let u = a + width * 0.5 * (1.0 + x);
            let phase = PI * u / k1;
            let kr = C64::new(u, lift * phase.sin());
            let jac = C64::new(1.0, lift * PI / k1 * phase.cos()) * (w * width * 0.5);
            let bess = bessel_j0(kr * rho);
            let (fa, fp) = rest(kr);
            ta += fa * bess * jac;
            tp += fp * bess * jac;
        }
    }
    let end = (200.0 / rho).clamp(4.0 * k1, k_max.max(4.0 * k1));
    let mut a = k1;
    while a < end {
        let w = (0.5 * a).min(PI / rho).min(end - a);
        for (x, gw) in GL6 {
            let u = a + w * 0.5 * (1.0 + x);
            let kr = C64::new(u, 0.0);
            let bess = bessel_j0(kr * rho);
            let (fa, fp) = rest(kr);
            let jac = gw * w * 0.5;
            ta += fa * bess * jac;
            tp += fp * bess * jac;
        }
        a += w;
    }
    (ta, tp)
}

impl Kernels {
    pub(super) fn build(stack: &Stack, f: f64, rho_max: f64) -> Self {
        let s = Spectral::new(stack, f);
        let lambda = C0 / f;
        // Deeper than this and the triangles of any mesh this solver would be asked for can see it.
        let cut = lambda / 10.0;
        let depths = image_depths(&s, cut);
        let (lo, hi) = (depths.last().map_or(1.0, |b| 0.2 / b), depths.first().map_or(1.0, |b| 20.0 / b));
        let limit = s.phi_limit();
        let at_zero = limit - 1.0;
        let a_im = fit_images(&|kr| s.statics(kr).0 - 1.0, &depths, lo, hi);
        let mut phi_im = fit_images(&|kr| s.statics(kr).1 - limit, &depths, lo, hi);
        if at_zero.norm() > 0.0 {
            phi_im.push((at_zero, 0.0));
        }
        let k_max = depths.first().map_or(400.0 * s.k0, |b| 30.0 / b).max(400.0 * s.k0);
        let log0 = RHO_MIN.ln();
        let step = ((rho_max * 1.01).ln() - log0) / (RHO_POINTS - 1) as f64;
        let rows: Vec<(f64, C64, C64)> = (0..RHO_POINTS)
            .into_par_iter()
            .map(|i| {
                let rho = (log0 + step * i as f64).exp();
                let (ta, tp) = rest_at(&s, rho, &a_im, &phi_im, k_max);
                (rho, ta, tp)
            })
            .collect();
        let sum = |im: &[(C64, f64)], rho: f64| -> C64 { im.iter().map(|(a, b)| *a / (rho * rho + b * b).sqrt()).sum() };
        let kernel = |im: &[(C64, f64)], pick: fn(&(f64, C64, C64)) -> C64| Kernel {
            images: im.to_vec(),
            smooth: Table { log0, step, v: rows.iter().map(pick).collect() },
            whole: Table { log0, step, v: rows.iter().map(|r| (pick(r) + sum(im, r.0)) * r.0).collect() },
        };
        Self { a: kernel(&a_im, |r| r.1), phi: kernel(&phi_im, |r| r.2) }
    }
}

/// Tables take a moment and an optimiser asks for the same few over and over, for thousands of
/// models: a stack at a frequency out to a distance is the same table.
static CACHE: LazyLock<Mutex<HashMap<u64, Arc<Kernels>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const CACHE_SIZE: usize = 256;

pub(super) fn kernels(stack: &Stack, f: f64, span: f64) -> Arc<Kernels> {
    // To a power of two, so similar models share a table.
    let rho_max = (span.max(0.05) * 1.5).log2().ceil().exp2();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(stack).unwrap_or_default().hash(&mut h);
    f.to_bits().hash(&mut h);
    rho_max.to_bits().hash(&mut h);
    let key = h.finish();
    if let Some(k) = CACHE.lock().unwrap().get(&key) {
        return k.clone();
    }
    let built = Arc::new(Kernels::build(stack, f, rho_max));
    let mut c = CACHE.lock().unwrap();
    if c.len() >= CACHE_SIZE {
        c.clear();
    }
    c.insert(key, built.clone());
    built
}

/// The pattern on the grid: the free-space radiation of the current, with each mode then taken out
/// through the layers. A surface wave along infinite layers doesn't get here, so it reads as loss.
pub(super) fn far_field(moments: &[Moment], f: f64, theta_step: f64, phi_step: f64, stack: &Stack) -> FarField {
    let k = wavenumber(f);
    let s = Spectral::new(stack, f);
    let mut ff = FarField::grid(f, theta_step, phi_step);
    ff.ground = stack.metal_behind;
    let (thetas, phis) = (ff.theta.clone(), ff.phi.clone());
    let np = phis.len();
    let a = C64::new(0.0, -k * ETA0 / (4.0 * PI));
    let rows: Vec<(Vec<C64>, Vec<C64>)> = thetas
        .par_iter()
        .map(|&t| {
            let mut et = vec![C64::new(0.0, 0.0); np];
            let mut ep = vec![C64::new(0.0, 0.0); np];
            if stack.metal_behind && t > 90.0 + 1e-9 {
                return (et, ep);
            }
            // Right at grazing even a thin film's TM wave falls to zero, in a notch far narrower than
            // any grid. On that row it would read as a whole row of power gone, so the row stands for
            // its cell instead: a quarter of a step off the horizon.
            let safe = if (t - 90.0).abs() < 1e-6 { 90.0 - 0.25 * theta_step } else { t };
            let (from_axis, upper) = if safe <= 90.0 { (safe, true) } else { (180.0 - safe, false) };
            let (rm, re) = s.radiation(from_axis, upper);
            for j in 0..np - 1 {
                let (r, th, ph) = spherical_axes(safe, phis[j]);
                let mut n = [C64::new(0.0, 0.0); 2];
                for m in moments {
                    let (sn, c) = (k * dot(r, m.pos)).sin_cos();
                    let phase = C64::new(c, sn);
                    n[0] += m.j[0] * phase;
                    n[1] += m.j[1] * phase;
                }
                et[j] = a * (n[0] * th[0] + n[1] * th[1]) * rm;
                ep[j] = a * (n[0] * ph[0] + n[1] * ph[1]) * re;
            }
            et[np - 1] = et[0];
            ep[np - 1] = ep[0];
            (et, ep)
        })
        .collect();
    for (i, (et, ep)) in rows.into_iter().enumerate() {
        ff.e_theta[i] = et;
        ff.e_phi[i] = ep;
    }
    ff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Geometry, PortDef, Shape, Solid};
    use crate::mom::{MomOptions, MomProblem};
    use crate::stack::Layer;

    fn close(a: C64, re: f64, im: f64) -> bool {
        (a - C64::new(re, im)).norm() < 2e-8
    }

    #[test]
    fn j0_against_published_values() {
        for (x, want) in [(0.0, 1.0), (1.0, 0.765_197_686_6), (5.0, -0.177_596_771_3), (10.0, -0.245_935_764_5), (12.0, 0.047_689_310_8), (13.0, 0.206_926_102_4), (15.0, -0.014_224_472_8), (20.0, 0.167_024_664_3), (100.0, 0.019_985_850_3)] {
            assert!((bessel_j0(C64::new(x, 0.0)) - want).norm() < 3e-9, "J0({x}) = {} vs {want}", bessel_j0(C64::new(x, 0.0)));
        }
        assert!(close(bessel_j0(C64::new(1.0, 1.0)), 0.937_608_476_8, -0.496_529_947_7), "{}", bessel_j0(C64::new(1.0, 1.0)));
    }

    /// Where one form takes over from the other, off the real axis too.
    #[test]
    fn the_two_forms_of_j0_meet() {
        for z in [C64::new(12.0, 0.1), C64::new(11.0, 5.0), C64::new(8.5, 8.5), C64::new(15.0, 2.0)] {
            let (a, b) = (j0_series(z), j0_hankel(z));
            assert!((a - b).norm() < 2e-8 * a.norm().max(1.0), "{z}: {a} vs {b}");
        }
    }

    const F: f64 = 915e6;

    fn free_stack() -> Spectral {
        Spectral::new(&Stack::default(), F)
    }

    #[test]
    fn with_nothing_around_it_the_spectra_are_free_space() {
        let s = free_stack();
        for kr in [C64::new(5.0, 2.0), C64::new(30.0, 0.0), C64::new(4000.0, 0.0)] {
            let want = kr / (J * kz(s.k0, C64::new(1.0, 0.0), kr));
            let (a, p) = s.spectra(kr);
            assert!((a - want).norm() < 1e-9 && (p - want).norm() < 1e-9, "{kr}: {a} {p} vs {want}");
        }
        let (a, p) = s.statics(100.0);
        assert!((a - 1.0).norm() < 1e-12 && (p - 1.0).norm() < 1e-12);
    }

    #[test]
    fn a_layer_of_air_changes_nothing() {
        let stack = Stack { above: vec![Layer::new("a", "Air", 1e-3)], below: vec![Layer::new("b", "Air", 2e-3), Layer::new("c", "Air", 5e-3)], ..Default::default() };
        let s = Spectral::new(&stack, F);
        for kr in [C64::new(5.0, 2.0), C64::new(30.0, 0.0), C64::new(4000.0, 0.0)] {
            let want = kr / (J * kz(s.k0, C64::new(1.0, 0.0), kr));
            let (a, p) = s.spectra(kr);
            assert!((a - want).norm() < 1e-8 * want.norm(), "{a} vs {want}");
            assert!((p - want).norm() < 1e-8 * want.norm(), "{p} vs {want}");
        }
        for angle in [0.0, 45.0, 80.0] {
            for upper in [true, false] {
                let (m, e) = s.radiation(angle, upper);
                assert!((m - 1.0).norm() < 1e-9 && (e - 1.0).norm() < 1e-9, "{angle} {upper}: {m} {e}");
            }
        }
    }

    #[test]
    fn a_spectrum_tends_to_its_static_form() {
        let stack = Stack { above: vec![Layer::new("p", "Paper", 80e-6)], below: vec![Layer::new("f", "PET film", 50e-6)], ..Default::default() };
        let s = Spectral::new(&stack, F);
        for kr in [2e4, 5e4, 2e5] {
            let (a, p) = s.spectra(C64::new(kr, 0.0));
            let (sa, sp) = s.statics(kr);
            let tol = 60.0 * (s.k0 / kr).powi(2);
            assert!((a - sa).norm() < tol, "A at {kr}: {a} vs {sa}");
            assert!((p - sp).norm() < tol, "φ at {kr}: {p} vs {sp}");
        }
        // At the highest: the average of the two media at the metal.
        let (_, p) = s.statics(1e9);
        assert!((p - s.phi_limit()).norm() < 1e-9, "{p} vs {}", s.phi_limit());
    }

    /// A thin series is followed by very few images, and what's left is left small.
    #[test]
    fn images_follow_the_static_spectrum() {
        for stack in [
            Stack { above: vec![Layer::new("p", "Paper", 80e-6), Layer::new("q", "Paper", 300e-6)], below: vec![Layer::new("f", "PET film", 50e-6)], ..Default::default() },
            Stack { below: vec![Layer::new("f", "PET film", 50e-6), Layer::new("g", "Glass", 3e-3)], ..Default::default() },
            Stack { below: vec![Layer::new("s", "Foam (Rohacell)", 3e-3)], metal_behind: true, ..Default::default() },
        ] {
            let s = Spectral::new(&stack, F);
            let depths = image_depths(&s, C0 / F / 10.0);
            let (lo, hi) = (0.2 / depths.last().unwrap(), 20.0 / depths[0]);
            let limit = s.phi_limit();
            let im = fit_images(&|kr| s.statics(kr).1 - limit, &depths, lo, hi);
            println!("{}: {} depths, {} images", stack.describe(), depths.len(), im.len());
            assert!(im.len() <= 26, "{}", im.len());
            let mut worst = 0.0f64;
            for i in 0..50 {
                let kr = lo * (hi / lo).powf(i as f64 / 49.0);
                let fit: C64 = im.iter().map(|(a, b)| *a * (-kr * b).exp()).sum();
                worst = worst.max((fit - (s.statics(kr).1 - limit)).norm());
            }
            assert!(worst < 3e-3, "{}: {worst}", stack.describe());
        }
    }

    fn strip(z: f64, l: f64, w: f64, g: f64) -> Geometry {
        let sheet = |name: &str, x0: f64, x1: f64| Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min: [x0, -w / 2.0, z], max: [x1, w / 2.0, z] } };
        Geometry {
            solids: vec![sheet("left", -l / 2.0, -g / 2.0), sheet("right", g / 2.0, l / 2.0)],
            ports: vec![PortDef { name: "feed".into(), a: [-g / 2.0, 0.0, z], b: [g / 2.0, 0.0, z], width: w, z0: 73.0 }],
            ..Default::default()
        }
    }

    fn opts(edges: f64) -> MomOptions {
        MomOptions { edges_per_lambda: edges, theta_step: 5.0, phi_step: 5.0, ..Default::default() }
    }

    fn solve(g: &Geometry, f: f64, edges: f64) -> C64 {
        MomProblem::new(g, f, opts(edges)).unwrap().solve(f, false).unwrap().z[0][0]
    }

    /// Air all round has to land where the plain solver does.
    #[test]
    fn empty_layers_land_on_the_free_space_solve() {
        let mut g = strip(0.0, 0.15, 0.004, 0.002);
        let plain = solve(&g, F, 24.0);
        g.stack = Some(Stack { above: vec![Layer::new("a", "Air", 1e-3)], below: vec![Layer::new("b", "Air", 1e-4)], ..Default::default() });
        let layered = solve(&g, F, 24.0);
        println!("plain {plain:.3}, in air layers {layered:.3}");
        assert!((plain - layered).norm() < 2e-3 * plain.norm(), "{plain} vs {layered}");
    }

    /// Exact: a metal plate behind air is the image the ground-plane solver already does.
    #[test]
    fn metal_behind_air_lands_on_the_image_solve() {
        for h in [0.005, 0.02, 0.08] {
            let mut image = strip(h, 0.15, 0.004, 0.002);
            image.ground = true;
            let want = solve(&image, F, 24.0);
            let mut g = strip(0.0, 0.15, 0.004, 0.002);
            g.stack = Some(Stack { below: vec![Layer::new("gap", "Air", h)], metal_behind: true, ..Default::default() });
            let got = solve(&g, F, 24.0);
            println!("{} mm: image {want:.3}, layered {got:.3}", h * 1e3);
            assert!((want - got).norm() < 8e-3 * want.norm(), "{h}: {want} vs {got}");
        }
    }

    fn resonance(g: &Geometry, lo: f64, hi: f64, edges: f64) -> f64 {
        let m = MomProblem::new(g, hi, opts(edges)).unwrap();
        let x = |f: f64| m.solve(f, false).unwrap().z[0][0].im;
        let (mut a, mut b) = (lo, hi);
        let (mut xa, mut xb) = (x(a), x(b));
        assert!(xa < 0.0 && xb > 0.0, "no resonance from {lo} to {hi}: {xa} {xb}");
        for _ in 0..8 {
            let c = a - xa * (b - a) / (xb - xa);
            let xc = x(c);
            if xc < 0.0 {
                (a, xa) = (c, xc);
            } else {
                (b, xb) = (c, xc);
            }
        }
        a - xa * (b - a) / (xb - xa)
    }

    /// A strip on a very thick slab is at the interface of two half-spaces, where a line's
    /// quasi-static permittivity is the average of the two: it resonates that much lower. Slab
    /// losses are small and a half-wave strip's end effect is close to the same in both.
    #[test]
    fn a_thick_slab_slows_a_dipole_by_the_average_permittivity() {
        let g = strip(0.0, 0.15, 0.002, 0.002);
        let free = resonance(&g, 800e6, 1100e6, 40.0);
        let mut on = g.clone();
        on.stack = Some(Stack { below: vec![Layer::new("slab", "PTFE (Teflon)", 0.15)], ..Default::default() });
        let slab = resonance(&on, 600e6, 1100e6, 40.0);
        let expected = ((1.0 + 2.1) / 2.0f64).sqrt();
        println!("free {:.1} MHz, on thick PTFE {:.1} MHz, ratio {:.3}, average permittivity says {expected:.3}", free / 1e6, slab / 1e6, free / slab);
        assert!((free / slab - expected).abs() < 0.04, "{} vs {expected}", free / slab);
    }

    /// As a layer thins to nothing, so does its effect, and the shift comes in the right direction.
    #[test]
    fn a_thin_layer_detunes_in_proportion_and_vanishing_layers_vanish() {
        let g = strip(0.0, 0.15, 0.002, 0.002);
        let free = resonance(&g, 800e6, 1100e6, 40.0);
        let mut shifts = Vec::new();
        for t in [1e-7, 80e-6, 300e-6, 1000e-6] {
            let mut on = g.clone();
            on.stack = Some(Stack { above: vec![Layer::new("paper", "Paper", t)], ..Default::default() });
            let f = resonance(&on, 700e6, 1100e6, 40.0);
            println!("{:>6.0} µm of paper: {:.1} MHz, {:+.2} %", t * 1e6, f / 1e6, (f / free - 1.0) * 100.0);
            shifts.push((f / free - 1.0) * 100.0);
        }
        assert!(shifts[0].abs() < 0.5, "{shifts:?}");
        assert!(shifts[1] < shifts[0] && shifts[2] < shifts[1] && shifts[3] < shifts[2], "more paper, lower: {shifts:?}");
    }

    /// Power: with no loss, what's accepted is what radiates, less the surface wave. An ungrounded
    /// slab's lowest mode has no cutoff, and what it carries off grows with the slab: it reads as loss
    /// here because the layers go on for ever.
    #[test]
    fn with_no_loss_in_the_layers_power_is_conserved() {
        let g0 = strip(0.0, 0.15, 0.004, 0.002);
        let eff = |stack: Stack| {
            let mut g = g0.clone();
            g.stack = Some(stack.clone());
            let m = MomProblem::new(&g, F, opts(24.0)).unwrap();
            let e = m.solve(F, true).unwrap().far.remove(0).metrics().rad_efficiency;
            println!("{}: radiated over accepted {e:.4}", stack.describe());
            e
        };
        let air = eff(Stack { below: vec![Layer::new("gap", "Air", 0.02)], metal_behind: true, ..Default::default() });
        assert!((air - 1.0).abs() < 0.01, "{air}");
        let on = |t: f64| eff(Stack { below: vec![Layer::new("y", "PTFE (Teflon)", t)], ..Default::default() });
        let (a, b, c) = (on(0.05e-3), on(0.5e-3), on(2e-3));
        assert!(a > 0.995 && a <= 1.002, "a film hardly launches one: {a}");
        assert!(b < a && c < b, "more slab, more surface wave: {a} {b} {c}");
        assert!(c > 0.85, "{c}");
    }

    /// `cargo test -p antenna_core layered_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn layered_survey() {
        let g0 = strip(0.0, 0.15, 0.004, 0.002);
        let show = |name: &str, stack: Option<Stack>, edges: f64| {
            let mut g = g0.clone();
            g.stack = stack;
            let m = MomProblem::new(&g, F, opts(edges)).unwrap();
            let pt = m.solve(F, true).unwrap();
            let met = pt.far[0].metrics();
            println!("{name:<28} {edges:>4}/λ {:>5} unk Z {:.3} {:+.3}j, eff {:.4}, D {:.3} dBi", m.unknowns(), pt.z[0][0].re, pt.z[0][0].im, met.rad_efficiency, met.peak_directivity_dbi);
        };
        for edges in [24.0, 48.0] {
            show("free", None, edges);
            for (name, mat, t) in [("ptfe 0.1 µm", "PTFE (Teflon)", 1e-7), ("ptfe 1 µm", "PTFE (Teflon)", 1e-6), ("ptfe 10 µm", "PTFE (Teflon)", 1e-5), ("ptfe 50 µm", "PTFE (Teflon)", 5e-5), ("ptfe 500 µm", "PTFE (Teflon)", 5e-4), ("ptfe 2 mm", "PTFE (Teflon)", 2e-3), ("foam 1 mm", "Foam (Rohacell)", 1e-3), ("alumina 50 µm", "Alumina 99.5%", 5e-5)] {
                show(name, Some(Stack { below: vec![Layer::new("l", mat, t)], ..Default::default() }), edges);
            }
        }
        // The two kernels against distance, with the free-space term taken out, as a share of it.
        for (name, mat, t) in [("ptfe 50 µm", "PTFE (Teflon)", 5e-5), ("ptfe 2 mm", "PTFE (Teflon)", 2e-3)] {
            let stack = Stack { below: vec![Layer::new("l", mat, t)], ..Default::default() };
            let kn = Kernels::build(&stack, F, 1.0);
            println!("{name}: A images {:?}\n φ images {:?}", kn.a.images, kn.phi.images);
            for rho in [1e-5, 1e-4, 1e-3, 1e-2, 0.03, 0.1, 0.3, 0.6] {
                println!("  ρ {rho:>8}: smooth A {:.4}, φ {:.4}; whole·ρ A {:.5}, φ {:.5}", kn.a.smooth_at(rho), kn.phi.smooth_at(rho), kn.a.whole_at(rho) * rho, kn.phi.whole_at(rho) * rho);
            }
        }
    }

    #[test]
    fn lossy_layers_take_their_share_and_water_takes_most() {
        let g0 = strip(0.0, 0.15, 0.004, 0.002);
        let eff = |stack: Stack| {
            let mut g = g0.clone();
            g.stack = Some(stack);
            MomProblem::new(&g, F, opts(24.0)).unwrap().solve(F, true).unwrap().far.remove(0).metrics().rad_efficiency
        };
        let paper = eff(Stack { above: vec![Layer::new("paper", "Paper", 80e-6)], ..Default::default() });
        let water = eff(Stack { below: vec![Layer::new("wall", "Polycarbonate", 0.5e-3), Layer::new("water", "Water", 40e-3)], ..Default::default() });
        println!("efficiency under 80 µm paper {paper:.3}, on a bottle of water {water:.3}");
        assert!(paper > 0.9 && paper < 1.001, "{paper}");
        assert!(water < 0.5, "{water}");
    }

    #[test]
    fn what_the_layered_solver_cant_do_it_says() {
        let mut g = strip(0.0, 0.15, 0.004, 0.002);
        g.stack = Some(Stack { above: vec![Layer::new("p", "Paper", 80e-6)], ..Default::default() });
        // Metal on a second height.
        g.solids.push(Solid { name: "up".into(), material: "PEC".into(), shape: Shape::Box { min: [0.0, 0.01, 0.002], max: [0.01, 0.02, 0.002] } });
        let e = MomProblem::new(&g, F, opts(12.0)).err().unwrap().to_string();
        assert!(e.contains("one flat plane"), "{e}");
        let mut h = strip(0.0, 0.15, 0.004, 0.002);
        h.stack = Some(Stack { above: vec![Layer::new("p", "Kryptonite", 80e-6)], ..Default::default() });
        assert!(MomProblem::new(&h, F, opts(12.0)).is_err());
        let mut q = strip(0.0, 0.15, 0.004, 0.002);
        q.stack = Some(Stack { above: vec![Layer::new("p", "Paper", 80e-6)], ..Default::default() });
        let m = MomProblem::new(&q, F, opts(12.0)).unwrap();
        assert!(m.near_field(F, 0, &[[0.0, 0.0, 0.1]]).is_err());
        assert!(m.rcs(F, 0.0, 0.0, C64::new(1.0, 0.0), C64::new(0.0, 0.0), &[(0.0, 0.0)]).is_err());
    }
}
