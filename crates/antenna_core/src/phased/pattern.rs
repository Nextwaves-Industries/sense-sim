//! An array's pattern from its elements: each with its own orientation, positions anywhere,
//! fields added as vectors so panels facing different ways combine with the right
//! polarisation. Gain conserves power, as [`crate::array`] does: with no coupling known, it's
//! the directivity of the real pattern times the element's efficiency. An embedded pattern
//! already holds the coupling, so there realized gain comes straight from the incident power.

use std::f64::consts::PI;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::aperture::{Element, angle_between, angles_of, phi_hat, theta_hat, unit_dir};
use crate::farfield::FarField;
use crate::geometry::{P3, dot};
use crate::{C0, C64, ETA0, db10};

/// What one element radiates, in its own frame.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ElementKind {
    /// Equal in every direction, the textbook array factor. Polarised along its x axis.
    Isotropic,
    /// Power ∝ cos^q of the angle off its normal, down to `back_db` behind and to the side.
    /// q about 1.5 is a typical patch's in-array scan loss.
    Cosine { q: f64, back_db: f64 },
    /// The sector element of 3GPP TR 38.901: 65° beam both ways, a 30 dB floor.
    Sector3gpp,
    /// The latest solved pattern of this project's antenna, in its own axes. With `embedded`,
    /// it was solved inside an array, so coupling is already in it: gain adds up from it
    /// directly, as N × embedded gain (Hannan's element-gain argument).
    Solved { embedded: bool },
}

impl Default for ElementKind {
    fn default() -> Self {
        ElementKind::Cosine { q: 1.5, back_db: -20.0 }
    }
}

impl ElementKind {
    pub fn label(&self) -> &'static str {
        match self {
            ElementKind::Isotropic => "Isotropic",
            ElementKind::Cosine { .. } => "Cosine (patch-like)",
            ElementKind::Sector3gpp => "3GPP sector",
            ElementKind::Solved { embedded: false } => "This antenna's solved pattern",
            ElementKind::Solved { embedded: true } => "Embedded pattern, centre port",
        }
    }
}

/// The element model made ready to evaluate: analytic shapes normalised to 1 W.
#[derive(Clone, Debug)]
enum Shape {
    Isotropic,
    Cosine { q: f64, floor: f64 },
    Sector,
    Far(Box<FarField>),
}

/// Elements that face the same way share one element pattern evaluation per direction.
#[derive(Clone, Debug)]
struct Group {
    x: P3,
    y: P3,
    n: P3,
    members: Vec<usize>,
}

/// An array's elements and what each radiates.
#[derive(Clone, Debug)]
pub struct ArrayModel {
    pub elements: Vec<Element>,
    pub kind: ElementKind,
    shape: Shape,
    /// Peak field for 1 W, volts: `√(2·η₀·G/4π)` with `G` the normalised shape.
    scale: f64,
    groups: Vec<Group>,
    /// Radiated over accepted, and over incident, for one element.
    pub eff_rad: f64,
    pub eff_total: f64,
    /// What a solved pattern's field is for, watts into its port. Weights scale it.
    pub unit_incident: f64,
    pub embedded: bool,
    pub ground: bool,
}

/// `(mean over the sphere)` of a shape, which fixes its gain.
fn mean_gain(shape: &dyn Fn(f64, f64, f64) -> f64) -> f64 {
    let n = 180;
    let m = 360;
    let mut sum = 0.0;
    for i in 0..n {
        let t = (i as f64 + 0.5) / n as f64 * PI;
        let (st, ct) = t.sin_cos();
        for j in 0..m {
            let p = (j as f64 + 0.5) / m as f64 * 2.0 * PI;
            sum += shape(st * p.cos(), st * p.sin(), ct) * st;
        }
    }
    sum * (PI / n as f64) * (2.0 * PI / m as f64) / (4.0 * PI)
}

fn cosine_shape(q: f64, floor: f64, z: f64) -> f64 {
    if z > 0.0 { z.powf(q).max(floor) } else { floor }
}

fn sector_shape(x: f64, y: f64, z: f64) -> f64 {
    // Vertical angle off the horizon of the element's own y, and azimuth round it.
    let elevation = y.clamp(-1.0, 1.0).asin().to_degrees();
    let azimuth = x.atan2(z).to_degrees();
    let vertical = (12.0 * (elevation / 65.0).powi(2)).min(30.0);
    let horizontal = (12.0 * (azimuth / 65.0).powi(2)).min(30.0);
    10f64.powf(-(vertical + horizontal).min(30.0) / 10.0)
}

/// Bilinear in θ and φ, on a pattern whose phase centre is near its origin.
pub fn sample_far(ff: &FarField, theta: f64, phi: f64) -> (C64, C64) {
    let (dt, dp) = (ff.theta_step(), ff.phi_step());
    let (nt, np) = (ff.theta.len(), ff.phi.len());
    if nt < 2 || np < 2 {
        return (C64::new(0.0, 0.0), C64::new(0.0, 0.0));
    }
    let x = (theta / dt).clamp(0.0, (nt - 1) as f64);
    let y = (phi.rem_euclid(360.0) / dp).clamp(0.0, (np - 1) as f64);
    let (i, j) = ((x as usize).min(nt - 2), (y as usize).min(np - 2));
    let (s, t) = (x - i as f64, y - j as f64);
    let mix = |g: &[Vec<C64>]| {
        let lo = g[i][j] * (1.0 - t) + g[i][j + 1] * t;
        let hi = g[i + 1][j] * (1.0 - t) + g[i + 1][j + 1] * t;
        lo * (1.0 - s) + hi * s
    };
    (mix(&ff.e_theta), mix(&ff.e_phi))
}

impl ArrayModel {
    /// `solved` is the pattern behind [`ElementKind::Solved`].
    pub fn new(elements: Vec<Element>, kind: ElementKind, solved: Option<&FarField>) -> Self {
        let unit_incident = solved.map_or(1.0, |ff| if ff.p_incident > 0.0 { ff.p_incident } else { 1.0 });
        let (shape, scale, eff_rad, eff_total, embedded, ground) = match (kind, solved) {
            (ElementKind::Isotropic, _) => (Shape::Isotropic, (2.0 * ETA0 / (4.0 * PI)).sqrt(), 1.0, 1.0, false, false),
            (ElementKind::Cosine { q, back_db }, _) => {
                let floor = 10f64.powf(back_db.min(0.0) / 10.0);
                let mean = mean_gain(&|_, _, z| cosine_shape(q, floor, z));
                (Shape::Cosine { q, floor }, (2.0 * ETA0 / (4.0 * PI * mean)).sqrt(), 1.0, 1.0, false, false)
            }
            (ElementKind::Sector3gpp, _) => {
                let mean = mean_gain(&sector_shape);
                // 65° by 65° is about 9.8 dBi of directivity, and the standard specifies 8 dBi: the rest is loss.
                let eff = (10f64.powf(0.8) * mean).min(1.0);
                (Shape::Sector, (2.0 * ETA0 / (4.0 * PI * mean)).sqrt(), eff, eff, false, false)
            }
            (ElementKind::Solved { embedded }, Some(ff)) => {
                let (rad, total) = (ff.rad_efficiency(), ff.total_efficiency());
                let rad = if rad.is_finite() && rad > 0.0 { rad.min(1.0) } else { 1.0 };
                let total = if total.is_finite() && total > 0.0 { total.min(1.0) } else { 1.0 };
                (Shape::Far(Box::new(ff.clone())), 1.0, rad, total, embedded, ff.ground)
            }
            // Nothing solved to use yet: a plain element rather than silence.
            (ElementKind::Solved { .. }, None) => (Shape::Isotropic, (2.0 * ETA0 / (4.0 * PI)).sqrt(), 1.0, 1.0, false, false),
        };
        let mut groups: Vec<Group> = Vec::new();
        for (i, e) in elements.iter().enumerate() {
            let same = |g: &&mut Group| (0..3).all(|k| (g.n[k] - e.normal[k]).abs() < 1e-9 && (g.x[k] - e.x_axis[k]).abs() < 1e-9);
            match groups.iter_mut().find(|g| same(g)) {
                Some(g) => g.members.push(i),
                None => groups.push(Group { x: e.x_axis, y: e.y_axis(), n: e.normal, members: vec![i] }),
            }
        }
        // A ground plane is a global z = 0 only for elements that all face up. On a turned element
        // the solved pattern's own empty half does the job, in that element's frame.
        let ground = ground && elements.iter().all(|e| (e.normal[2] - 1.0).abs() < 1e-9);
        Self { elements, kind, shape, scale, groups, eff_rad, eff_total, unit_incident, embedded, ground }
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The field of one element for 1 W in direction `d`, as a vector, ignoring where it is.
    fn local_field(&self, g: &Group, d: P3) -> [C64; 3] {
        let (x, y, z) = (dot(d, g.x), dot(d, g.y), dot(d, g.n));
        let local: [C64; 3] = match &self.shape {
            Shape::Far(ff) => {
                let (theta, phi) = angles_of([x, y, z]);
                let (et, ep) = sample_far(ff, theta, phi);
                let (a, b) = (theta_hat(theta, phi), phi_hat(phi));
                std::array::from_fn(|k| et * a[k] + ep * b[k])
            }
            shape => {
                let g2 = match shape {
                    Shape::Cosine { q, floor } => cosine_shape(*q, *floor, z),
                    Shape::Sector => sector_shape(x, y, z),
                    _ => 1.0,
                };
                // Ludwig-3 from the local x axis: polarisation stays put as the direction moves.
                let (theta, phi) = angles_of([x, y, z]);
                let (a, b) = (theta_hat(theta, phi), phi_hat(phi));
                let (sp, cp) = phi.to_radians().sin_cos();
                let amp = self.scale * g2.max(0.0).sqrt();
                std::array::from_fn(|k| C64::new(amp * (cp * a[k] - sp * b[k]), 0.0))
            }
        };
        std::array::from_fn(|k| local[0] * g.x[k] + local[1] * g.y[k] + local[2] * g.n[k])
    }

    /// Each element's complex field in direction `d`, position included, along polarisation
    /// `pol` (a unit vector; conjugated as a projection).
    pub fn response(&self, f: f64, d: P3, pol: Option<[C64; 3]>) -> Vec<C64> {
        let k = 2.0 * PI * f / C0;
        let mut out = vec![C64::new(0.0, 0.0); self.len()];
        for g in &self.groups {
            let e = self.local_field(g, d);
            let v: C64 = match pol {
                Some(p) => (0..3).map(|i| p[i].conj() * e[i]).sum(),
                None => {
                    let size = (0..3).map(|i| e[i].norm_sqr()).sum::<f64>().sqrt();
                    C64::new(size, 0.0)
                }
            };
            for i in &g.members {
                out[*i] = v * C64::from_polar(1.0, k * dot(d, self.elements[*i].pos));
            }
        }
        out
    }

    /// The field of the whole array in direction `d`, as a vector.
    pub fn field(&self, w: &[C64], f: f64, d: P3) -> [C64; 3] {
        let k = 2.0 * PI * f / C0;
        let mut out = [C64::new(0.0, 0.0); 3];
        if self.ground && d[2] < -1e-9 {
            return out;
        }
        for g in &self.groups {
            let af: C64 = g.members.iter().map(|i| w[*i] * C64::from_polar(1.0, k * dot(d, self.elements[*i].pos))).sum();
            if af.norm_sqr() == 0.0 {
                continue;
            }
            let e = self.local_field(g, d);
            for i in 0..3 {
                out[i] += e[i] * af;
            }
        }
        out
    }

    /// Radiation intensity (W/sr) in direction `d`, for these weights.
    pub fn intensity(&self, w: &[C64], f: f64, d: P3) -> f64 {
        self.field(w, f, d).iter().map(C64::norm_sqr).sum::<f64>() / (2.0 * ETA0)
    }

    /// The polarisation an array of these elements radiates in direction `d`: the strongest
    /// element's, as a complex unit vector. Steering in phase along it makes panels add.
    pub fn reference_pol(&self, d: P3) -> Option<[C64; 3]> {
        let best = self.groups.iter().map(|g| self.local_field(g, d)).max_by(|a, b| {
            let size = |e: &[C64; 3]| e.iter().map(C64::norm_sqr).sum::<f64>();
            size(a).total_cmp(&size(b))
        })?;
        let n = best.iter().map(C64::norm_sqr).sum::<f64>().sqrt();
        (n > 0.0).then(|| std::array::from_fn(|i| best[i] / n))
    }

    /// How many different ways the elements face.
    pub fn orientations(&self) -> usize {
        self.groups.len()
    }

    /// The normal of the plane all the elements lie in, when they do and all face along it.
    pub fn flat_normal(&self) -> Option<P3> {
        let first = self.elements.first()?;
        let n = first.normal;
        let same_way = self.groups.len() == 1;
        let level = dot(first.pos, n);
        (same_way && self.elements.iter().all(|e| (dot(e.pos, n) - level).abs() < 1e-9)).then_some(n)
    }

    /// The longest extent of the array, metres.
    pub fn extent(&self) -> f64 {
        let mut d2 = 0.0f64;
        // Against the centroid, not all pairs: thousands of elements come through here per drag.
        let n = self.len().max(1) as f64;
        let c: P3 = std::array::from_fn(|k| self.elements.iter().map(|e| e.pos[k]).sum::<f64>() / n);
        for e in &self.elements {
            let v: P3 = std::array::from_fn(|k| e.pos[k] - c[k]);
            d2 = d2.max(dot(v, v));
        }
        2.0 * d2.sqrt()
    }

    /// Half-power width of a uniform beam at broadside across the longest extent, degrees.
    pub fn narrowest_beam_deg(&self, f: f64) -> f64 {
        let lambda = C0 / f;
        let long = self.extent() + lambda / 2.0;
        (0.886 * lambda / long).min(2.0).to_degrees()
    }

    /// The pattern on a grid, in the crate's far-field form. Accepted and incident power follow
    /// the module's rule, so metrics, the 3D view and exports read it correctly.
    pub fn far(&self, w: &[C64], f: f64, step: f64) -> FarField {
        let mut out = FarField::grid(f, step, step);
        out.ground = self.ground;
        let phi = out.phi.clone();
        let theta = out.theta.clone();
        let rows: Vec<(Vec<C64>, Vec<C64>)> = theta
            .par_iter()
            .map(|t| {
                let (mut et, mut ep) = (Vec::with_capacity(phi.len()), Vec::with_capacity(phi.len()));
                for p in &phi {
                    let e = self.field(w, f, unit_dir(*t, *p));
                    let (a, b) = (theta_hat(*t, *p), phi_hat(*p));
                    et.push((0..3).map(|i| e[i] * a[i]).sum::<C64>());
                    ep.push((0..3).map(|i| e[i] * b[i]).sum::<C64>());
                }
                (et, ep)
            })
            .collect();
        for (ti, (a, b)) in rows.into_iter().enumerate() {
            out.e_theta[ti] = a;
            out.e_phi[ti] = b;
        }
        let (acc, inc) = self.input_power(w, out.p_rad());
        out.p_accepted = acc;
        out.p_incident = inc;
        out
    }

    /// `(accepted, incident)`. An embedded pattern's field is per watt into its port, and what
    /// that port's neighbours soak up is already out of it, so all it can say is the incident
    /// power: its gain and realized gain are one figure. With no coupling known they follow from
    /// what really radiates, or close elements would read above their directivity.
    fn input_power(&self, w: &[C64], p_rad: f64) -> (f64, f64) {
        if self.embedded {
            let sum: f64 = w.iter().map(C64::norm_sqr).sum();
            let incident = (sum * self.unit_incident).max(1e-30);
            (incident, incident)
        } else {
            (p_rad / self.eff_rad.max(1e-9), p_rad / self.eff_total.max(1e-9))
        }
    }
}

/// What a beam does, as a page or a report reads it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BeamReport {
    pub theta: f64,
    pub phi: f64,
    pub peak_theta: f64,
    pub peak_phi: f64,
    pub pointing_error_deg: f64,
    pub directivity_dbi: f64,
    pub gain_dbi: f64,
    pub realized_dbi: f64,
    /// Realized gain in the direction asked for, which a squinted beam misses.
    pub at_request_dbi: f64,
    /// Width through the peak along the plane of scan, and across it, degrees.
    pub hpbw_a: f64,
    pub hpbw_b: f64,
    pub sll_db: Option<f64>,
    pub sll_at: Option<(f64, f64)>,
    /// Lobes as strong as the main beam to within 3 dB, away from it and from `other_beams`.
    pub grating: Vec<(f64, f64, f64)>,
    /// Taper loss against the same elements fed equally, dB (negative).
    pub taper_db: f64,
    /// Share of the elements with any drive.
    pub active: usize,
    pub step_deg: f64,
    /// Too many elements for the fine grid its beam calls for: figures are coarser.
    pub estimated: bool,
}

/// Complex terms one report's power integral may cost. A narrower beam than that resolves
/// gets a coarser grid, and says so.
const REPORT_BUDGET: f64 = 4e9;

/// A shrinking search over direction for the local maximum nearest `start`. `value` takes
/// the direction as a vector so the pole holds no surprises.
pub fn refine_peak(value: &(dyn Fn(P3) -> f64 + Sync), start: (f64, f64), first_step_deg: f64) -> (f64, f64, f64) {
    let (mut t, mut p) = start;
    let mut best = value(unit_dir(t, p));
    let mut step = first_step_deg;
    while step > 1e-3 {
        let mut moved = false;
        // Sideways steps measured on the sphere, so φ's scale doesn't collapse near the pole.
        let side = step / t.to_radians().sin().abs().max(0.02);
        for (dt, dp) in [(step, 0.0), (-step, 0.0), (0.0, side), (0.0, -side), (step, side), (step, -side), (-step, side), (-step, -side)] {
            let (a, b) = (t + dt, p + dp);
            let v = value(unit_dir(a, b));
            // A hair's margin: along the top of a cone-shaped beam, rounding would otherwise wander.
            if v > best * (1.0 + 1e-12) {
                best = v;
                t = a;
                p = b;
                moved = true;
                break;
            }
        }
        if !moved {
            step *= 0.5;
        }
    }
    let (theta, phi) = angles_of(unit_dir(t, p));
    (theta, phi, best)
}

/// Half-power width along a great circle through `peak`, heading `heading_deg` from θ̂ towards φ̂.
fn width_through(value: &dyn Fn(P3) -> f64, peak: (f64, f64), heading_deg: f64, guess: f64) -> f64 {
    let center = unit_dir(peak.0, peak.1);
    let (a, b) = (theta_hat(peak.0, peak.1), phi_hat(peak.1));
    let (s, c) = heading_deg.to_radians().sin_cos();
    let tangent: P3 = std::array::from_fn(|k| c * a[k] + s * b[k]);
    let top = value(center);
    let half = top * 0.5;
    let along = |deg: f64| {
        let (sa, ca) = deg.to_radians().sin_cos();
        value(std::array::from_fn(|k| ca * center[k] + sa * tangent[k]))
    };
    let side = |sign: f64| -> Option<f64> {
        let mut x0 = 0.0;
        let step = (guess / 12.0).max(0.02);
        let mut x1 = step;
        while x1 < 180.0 {
            if along(sign * x1) <= half {
                let (mut lo, mut hi) = (x0, x1);
                for _ in 0..30 {
                    let mid = 0.5 * (lo + hi);
                    if along(sign * mid) > half {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                return Some(0.5 * (lo + hi));
            }
            x0 = x1;
            x1 += step;
        }
        None
    };
    match (side(1.0), side(-1.0)) {
        (Some(p), Some(q)) => p + q,
        _ => 360.0,
    }
}

/// The eight cells round a cell of a θ/φ grid, φ wrapping round.
fn neighbours(ti: usize, pi: usize, nt: usize, np: usize) -> impl Iterator<Item = (usize, usize)> {
    [-1i64, 0, 1].into_iter().flat_map(move |a| {
        [np - 1, 0, 1].into_iter().filter_map(move |b| {
            let t = ti as i64 + a;
            (t >= 0 && (t as usize) < nt && !(a == 0 && b == 0)).then_some((t as usize, (pi + b) % np))
        })
    })
}

/// Marks everything joined to `start` at or above `floor` as no longer outside it.
fn fill_lobe(u: &[Vec<f64>], start: (usize, usize), outside: &mut [Vec<bool>], floor: f64) {
    let (nt, np) = (u.len(), u[0].len());
    let mut stack = vec![start];
    outside[start.0][start.1] = false;
    while let Some((ti, pi)) = stack.pop() {
        for (a, b) in neighbours(ti, pi, nt, np) {
            if outside[a][b] && u[a][b] >= floor {
                outside[a][b] = false;
                stack.push((a, b));
            }
        }
    }
}

/// A lobe that may be a whole cone: everything joined to its top above half power, then all that
/// runs downhill from any of that. A ridge sampled on a grid goes up and down by rounding, so
/// the walk downhill alone would stop along it.
fn fill_main_lobe(u: &[Vec<f64>], start: (usize, usize), outside: &mut [Vec<bool>]) {
    let (nt, np) = (u.len(), u[0].len());
    // Nothing to mark from a cell with no field: half of nothing would take in the whole sphere.
    if u[start.0][start.1] <= 0.0 {
        return;
    }
    let half = u[start.0][start.1] * 0.5;
    let mut ridge = vec![start];
    let mut stack = vec![start];
    outside[start.0][start.1] = false;
    while let Some((ti, pi)) = stack.pop() {
        for (a, b) in neighbours(ti, pi, nt, np) {
            if outside[a][b] && u[a][b] >= half {
                outside[a][b] = false;
                stack.push((a, b));
                ridge.push((a, b));
            }
        }
    }
    while let Some((ti, pi)) = ridge.pop() {
        let here = u[ti][pi];
        for (a, b) in neighbours(ti, pi, nt, np) {
            if outside[a][b] && u[a][b] <= here * (1.0 + 1e-9) {
                outside[a][b] = false;
                ridge.push((a, b));
            }
        }
    }
}

/// From a cell uphill to the top of its lobe.
fn climb(u: &[Vec<f64>], mut at: (usize, usize)) -> (usize, usize) {
    let (nt, np) = (u.len(), u[0].len());
    loop {
        let best = neighbours(at.0, at.1, nt, np).max_by(|a, b| u[a.0][a.1].total_cmp(&u[b.0][b.1]));
        match best {
            Some(n) if u[n.0][n.1] > u[at.0][at.1] => at = n,
            _ => return at,
        }
    }
}

/// Reads a beam out of its weights. `other_beams` aren't grating lobes when found.
pub fn report(m: &ArrayModel, w: &[C64], f: f64, asked: (f64, f64), other_beams: &[(f64, f64)]) -> BeamReport {
    let beam = m.narrowest_beam_deg(f);
    let finest = (beam / 4.0).clamp(0.2, 3.0);
    let terms = |s: f64| (180.0 / s) * (360.0 / s) * m.len() as f64;
    let mut step = finest;
    while terms(step) > REPORT_BUDGET && step < 6.0 {
        step *= 1.25;
    }
    let estimated = step > finest * 1.01;
    let (half, quarter) = ((90.0 / step).round().max(2.0) as usize, (90.0 / step).round().max(2.0) as usize);
    let (nt, np) = (2 * half + 1, 4 * quarter);
    let (dt, dp) = (90.0 / half as f64, 90.0 / quarter as f64);
    let u: Vec<Vec<f64>> = (0..nt).into_par_iter().map(|ti| (0..np).map(|pi| m.intensity(w, f, unit_dir(ti as f64 * dt, pi as f64 * dp))).collect()).collect();
    let mut p_rad = 0.0;
    for (ti, row) in u.iter().enumerate() {
        let th = (ti as f64 * dt).to_radians();
        let (dtr, dpr) = (dt.to_radians(), dp.to_radians());
        let mut area = if ti == 0 || ti == nt - 1 { (1.0 - (dtr * 0.5).cos()) * dpr } else { th.sin() * dtr * dpr };
        if m.ground && ti == half {
            area *= 0.5;
        }
        p_rad += row.iter().sum::<f64>() * area;
    }
    let intensity = |d: P3| m.intensity(w, f, d);
    let top = u.iter().flatten().copied().fold(f64::MIN, f64::max);
    let wanted = unit_dir(asked.0, asked.1);
    let cell_of = |dir: (f64, f64)| {
        let (theta, phi) = angles_of(unit_dir(dir.0, dir.1));
        (((theta / dt).round() as usize).min(nt - 1), ((phi / dp).round() as usize) % np)
    };
    let grid_peak = if other_beams.is_empty() {
        // A line's beam is a cone, every point of it as strong as any other, so of the cells
        // about as strong as the best take the one nearest the request.
        let mut best = (0, 0);
        let mut nearest = f64::MIN;
        for (ti, row) in u.iter().enumerate() {
            for (pi, v) in row.iter().enumerate() {
                if *v >= top * (1.0 - 1e-3) {
                    let closeness = dot(unit_dir(ti as f64 * dt, pi as f64 * dp), wanted);
                    if closeness > nearest {
                        nearest = closeness;
                        best = (ti, pi);
                    }
                }
            }
        }
        best
    } else {
        // Several beams in one pattern: each is read at its own lobe, not at the strongest.
        climb(&u, cell_of(asked))
    };
    let refined = refine_peak(&intensity, (grid_peak.0 as f64 * dt, grid_peak.1 as f64 * dp), step);
    // Already on the ridge of a cone-shaped beam, the request is as good a peak as any point of it.
    let at_request = intensity(wanted);
    let (peak_theta, peak_phi, peak_u) = if at_request >= refined.2 * (1.0 - 1e-9) { (asked.0, asked.1, at_request) } else { refined };
    // Width in the plane through the array's normal and the beam, and across it.
    let along = width_through(&intensity, (peak_theta, peak_phi), 0.0, beam);
    let across = width_through(&intensity, (peak_theta, peak_phi), 90.0, beam);
    // The main lobe is all that can be reached from its peak going only downhill, out to its
    // first nulls in every direction. Anything still standing outside that is another lobe.
    let mut outside = vec![vec![true; np]; nt];
    fill_main_lobe(&u, grid_peak, &mut outside);
    // A flat array can't tell front from back: each beam has a mirror image through its plane,
    // which the element's pattern may weaken but which isn't a lobe of the lattice. Over a
    // ground plane nothing radiates there at all, so there's no such lobe to set aside.
    let mirror = m.flat_normal().filter(|_| !m.ground);
    let mut known: Vec<(f64, f64)> = other_beams.to_vec();
    known.push((peak_theta, peak_phi));
    if let Some(n) = mirror {
        for i in 0..known.len() {
            let d = unit_dir(known[i].0, known[i].1);
            let along = dot(d, n);
            known.push(angles_of(std::array::from_fn(|k| d[k] - 2.0 * along * n[k])));
        }
    }
    for b in &known {
        let summit = climb(&u, cell_of(*b));
        fill_main_lobe(&u, summit, &mut outside);
    }
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for (ti, row) in u.iter().enumerate() {
        for (pi, v) in row.iter().enumerate() {
            if *v <= 0.0 || !outside[ti][pi] || (ti == 0 || ti == nt - 1) && pi > 0 {
                continue;
            }
            if neighbours(ti, pi, nt, np).all(|(a, b)| u[a][b] <= *v) {
                candidates.push((*v, ti, pi));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut side: Option<(f64, f64, f64)> = None;
    let mut grating = Vec::new();
    // A cone of a lobe is many cells at one height: each lobe counts once.
    for (v, ti, pi) in candidates {
        if !outside[ti][pi] {
            continue;
        }
        let (t, p, val) = refine_peak(&intensity, (ti as f64 * dt, pi as f64 * dp), step / 2.0);
        if side.is_none_or(|s| val > s.2) {
            side = Some((t, p, val));
        }
        let rel = db10(val / peak_u);
        if rel > -3.0 {
            grating.push((t, p, rel));
        } else {
            break;
        }
        fill_lobe(&u, (ti, pi), &mut outside, v * 0.5);
    }
    let directivity = 4.0 * PI * peak_u / p_rad.max(1e-300);
    let (accepted, incident) = m.input_power(w, p_rad);
    let gain = 4.0 * PI * peak_u / accepted.max(1e-300);
    let realized = 4.0 * PI * peak_u / incident.max(1e-300);
    let at_request = 4.0 * PI * intensity(unit_dir(asked.0, asked.1)) / incident.max(1e-300);
    let sum: f64 = w.iter().map(|v| v.norm()).sum();
    let power: f64 = w.iter().map(C64::norm_sqr).sum();
    let active = w.iter().filter(|v| v.norm_sqr() > 0.0).count();
    BeamReport {
        theta: asked.0,
        phi: asked.1,
        peak_theta,
        peak_phi,
        pointing_error_deg: angle_between(asked, (peak_theta, peak_phi)),
        directivity_dbi: db10(directivity),
        gain_dbi: db10(gain),
        realized_dbi: db10(realized),
        at_request_dbi: db10(at_request),
        hpbw_a: along,
        hpbw_b: across,
        sll_db: side.map(|s| db10(s.2 / peak_u)),
        sll_at: side.map(|s| (s.0, s.1)),
        grating,
        taper_db: if active > 0 && power > 0.0 { db10(sum * sum / (active as f64 * power)) } else { 0.0 },
        active,
        step_deg: step,
        estimated,
    }
}

/// A pattern cut through the plane at `phi`: `(θ from −180 to 180, dB against the pattern's
/// isotropic level)`. A negative θ is the plane's other half.
pub fn plane_cut(m: &ArrayModel, w: &[C64], f: f64, phi: f64, step: f64, p_rad: f64) -> Vec<(f64, f64)> {
    let n = (180.0 / step).round() as i64;
    (-n..=n)
        .into_par_iter()
        .map(|i| {
            let t = i as f64 * step;
            (t, db10(4.0 * PI * m.intensity(w, f, unit_dir(t, phi)) / p_rad.max(1e-300)))
        })
        .collect()
}

/// A map of the upper hemisphere against direction cosines: `[v][u]` in dB against the peak, NaN
/// outside the visible circle.
pub fn uv_map(m: &ArrayModel, w: &[C64], f: f64, size: usize) -> Vec<Vec<f64>> {
    let at = |i: usize| -1.0 + 2.0 * i as f64 / (size.max(2) - 1) as f64;
    let rows: Vec<Vec<f64>> = (0..size)
        .into_par_iter()
        .map(|iv| {
            let v = at(iv);
            (0..size)
                .map(|iu| {
                    let u = at(iu);
                    let r = u * u + v * v;
                    if r > 1.0 { f64::NAN } else { m.intensity(w, f, [u, v, (1.0 - r).sqrt()]) }
                })
                .collect()
        })
        .collect();
    let top = rows.iter().flatten().filter(|x| x.is_finite()).fold(0.0f64, |a, b| a.max(*b));
    rows.into_iter().map(|r| r.into_iter().map(|x| if x.is_finite() { db10(x / top.max(1e-300)) } else { f64::NAN }).collect()).collect()
}

/// A map against azimuth and elevation, for arrays that see all round: `[el][az]` in dB
/// against the peak. Azimuth 0..360°, elevation −90..90°.
pub fn az_el_map(m: &ArrayModel, w: &[C64], f: f64, width: usize, height: usize) -> Vec<Vec<f64>> {
    let rows: Vec<Vec<f64>> = (0..height)
        .into_par_iter()
        .map(|ie| {
            let el = -90.0 + 180.0 * ie as f64 / (height.max(2) - 1) as f64;
            (0..width).map(|ia| m.intensity(w, f, unit_dir(90.0 - el, 360.0 * ia as f64 / width as f64))).collect()
        })
        .collect();
    let top = rows.iter().flatten().fold(0.0f64, |a, b| a.max(*b));
    rows.into_iter().map(|r: Vec<f64>| r.into_iter().map(|x| db10(x / top.max(1e-300))).collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::super::aperture::Layout;
    use super::*;

    const F: f64 = 3e9;

    fn uniform(n: usize) -> Vec<C64> {
        vec![C64::new(1.0, 0.0); n]
    }

    fn steer(m: &ArrayModel, f: f64, theta: f64, phi: f64) -> Vec<C64> {
        let d = unit_dir(theta, phi);
        let k = 2.0 * PI * f / C0;
        m.elements.iter().map(|e| C64::from_polar(1.0, -k * dot(d, e.pos))).collect()
    }

    fn line(n: usize, kind: ElementKind) -> ArrayModel {
        ArrayModel::new(Layout::Line { n, spacing: 0.5 }.elements(F), kind, None)
    }

    /// A ground-plane pattern: a beam up, nothing below the horizon.
    fn ground_element() -> FarField {
        let mut ff = FarField::grid(F, 2.0, 2.0);
        for (ti, t) in ff.theta.clone().iter().enumerate() {
            for (pi, p) in ff.phi.clone().iter().enumerate() {
                let a = if *t < 90.0 { t.to_radians().cos() } else { 0.0 };
                let (sp, cp) = p.to_radians().sin_cos();
                ff.e_theta[ti][pi] = C64::new(a * cp, 0.0);
                ff.e_phi[ti][pi] = C64::new(-a * sp, 0.0);
            }
        }
        ff.ground = true;
        ff.p_accepted = 1.0;
        ff.p_incident = 1.0;
        ff
    }

    #[test]
    fn a_ground_plane_element_still_shows_its_lobes() {
        // 1 λ spacing scanned to 30°: a grating lobe at u = −0.5, and a real side-lobe structure.
        let m = ArrayModel::new(Layout::Planar { nx: 8, ny: 8, dx: 1.0, dy: 1.0, triangular: false }.elements(F), ElementKind::Solved { embedded: false }, Some(&ground_element()));
        assert!(m.ground);
        let w = steer(&m, F, 30.0, 0.0);
        let r = report(&m, &w, F, (30.0, 0.0), &[]);
        assert!(r.sll_db.is_some(), "{r:?}");
        assert!(r.grating.iter().any(|g| (g.0.to_radians().sin() * g.1.to_radians().cos() + 0.5).abs() < 0.05), "{r:?}");
    }

    #[test]
    fn a_turned_ground_plane_element_isnt_cut_by_the_global_horizon() {
        // A face looking along +x: half its beam is below z = 0, and all of that radiates.
        let l = Layout::Panels { faces: 4, nx: 6, ny: 6, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        let m = ArrayModel::new(l.elements(F), ElementKind::Solved { embedded: false }, Some(&ground_element()));
        assert!(!m.ground, "not all facing up");
        let w: Vec<C64> = steer(&m, F, 90.0, 0.0).iter().zip(&m.elements).map(|(v, e)| if e.panel == 0 { *v } else { C64::new(0.0, 0.0) }).collect();
        let (above, below) = (m.intensity(&w, F, unit_dir(80.0, 0.0)), m.intensity(&w, F, unit_dir(100.0, 0.0)));
        assert!((above / below - 1.0).abs() < 1e-6, "{above} vs {below}");
        let r = report(&m, &w, F, (90.0, 0.0), &[]);
        assert!(r.hpbw_a < 40.0, "{r:?}");
        // A flat 6 × 6 at λ/2 on a cos² element, about N·π: 20 dBi at most.
        assert!(r.directivity_dbi < 21.0, "{r:?}");
    }

    #[test]
    fn beams_in_one_pattern_are_each_read_at_their_own_lobe() {
        // Unequal beams in a pattern: the weaker reads its own peak, width and directivity. The
        // stronger one's side lobes do pull it a little, which is real: well within its own width.
        let m = ArrayModel::new(Layout::Planar { nx: 16, ny: 16, dx: 0.5, dy: 0.5, triangular: false }.elements(F), ElementKind::Isotropic, None);
        let (a, b) = (steer(&m, F, 20.0, 0.0), steer(&m, F, 40.0, 180.0));
        let w: Vec<C64> = a.iter().zip(&b).map(|(x, y)| *x + *y * 0.5).collect();
        let strong = report(&m, &w, F, (20.0, 0.0), &[(40.0, 180.0)]);
        let weak = report(&m, &w, F, (40.0, 180.0), &[(20.0, 0.0)]);
        assert!(strong.pointing_error_deg < strong.hpbw_a / 4.0, "{strong:?}");
        assert!(weak.pointing_error_deg < weak.hpbw_a / 4.0, "the weaker beam's own: {weak:?}");
        let gap = strong.directivity_dbi - weak.directivity_dbi;
        assert!((gap - 6.02).abs() < 1.5, "amplitude 0.5 is about 6 dB down: {gap}");
        assert!(strong.grating.is_empty() && weak.grating.is_empty(), "{strong:?} {weak:?}");
    }

    #[test]
    fn analytic_elements_radiate_one_watt() {
        for kind in [ElementKind::Isotropic, ElementKind::Cosine { q: 1.5, back_db: -20.0 }, ElementKind::Cosine { q: 4.0, back_db: -30.0 }, ElementKind::Sector3gpp] {
            let m = ArrayModel::new(Layout::Line { n: 1, spacing: 0.5 }.elements(F), kind, None);
            let far = m.far(&uniform(1), F, 2.0);
            assert!((far.p_rad() - 1.0).abs() < 0.01, "{kind:?}: {}", far.p_rad());
        }
    }

    #[test]
    fn cosine_element_has_the_closed_form_gain() {
        // ∫cos^q dΩ over the front half is 2π/(q+1), so G = 2(q+1) with nothing behind.
        let m = ArrayModel::new(Layout::Line { n: 1, spacing: 0.5 }.elements(F), ElementKind::Cosine { q: 2.0, back_db: -120.0 }, None);
        let r = report(&m, &uniform(1), F, (0.0, 0.0), &[]);
        assert!((r.directivity_dbi - db10(6.0)).abs() < 0.03, "{}", r.directivity_dbi);
        // The 3GPP element is specified as 8 dBi, though 65° by 65° directs about 9.8: the rest is loss.
        let s = ArrayModel::new(Layout::Line { n: 1, spacing: 0.5 }.elements(F), ElementKind::Sector3gpp, None);
        let g = report(&s, &uniform(1), F, (0.0, 0.0), &[]);
        assert!((g.directivity_dbi - 9.8).abs() < 0.3, "{}", g.directivity_dbi);
        assert!((g.gain_dbi - 8.0).abs() < 0.01, "{}", g.gain_dbi);
    }

    #[test]
    fn the_uniform_line_has_its_textbook_numbers() {
        // N = 16 at λ/2: directivity 12.04 dB, first side lobe −13.1 dB, width 6.4°.
        let m = line(16, ElementKind::Isotropic);
        let r = report(&m, &uniform(16), F, (0.0, 0.0), &[]);
        assert!((r.directivity_dbi - 12.04).abs() < 0.03, "{}", r.directivity_dbi);
        assert!((r.sll_db.unwrap() + 13.15).abs() < 0.2, "{:?}", r.sll_db);
        assert!((r.hpbw_a.min(r.hpbw_b) - 6.35).abs() < 0.15, "{} {}", r.hpbw_a, r.hpbw_b);
        assert_eq!(r.taper_db, 0.0);
        assert!(r.grating.is_empty(), "{:?}", r.grating);
    }

    #[test]
    fn agrees_with_the_existing_array_factor() {
        use crate::array::{self, ArrayConfig};
        let c = ArrayConfig { nx: 6, ny: 4, dx: 0.5 * C0 / F, dy: 0.6 * C0 / F, steer_theta: 25.0, steer_phi: 30.0, ..Default::default() };
        let m = ArrayModel::new(Layout::Planar { nx: 6, ny: 4, dx: 0.5, dy: 0.6, triangular: false }.elements(F), ElementKind::Isotropic, None);
        let w = steer(&m, F, 25.0, 30.0);
        for (t, p) in [(0.0, 0.0), (25.0, 30.0), (40.0, 100.0), (70.0, 200.0)] {
            let a = array::array_factor(&c, F, t, p).norm_sqr();
            // |AF|² from this module's intensity, which carries the isotropic element's U = 1/4π.
            let b = m.intensity(&w, F, unit_dir(t, p)) * 4.0 * PI;
            assert!((a - b).abs() < 1e-9 * a.max(1.0), "({t}, {p}): {a} vs {b}");
        }
    }

    #[test]
    fn steering_puts_the_beam_where_it_was_asked() {
        let m = ArrayModel::new(Layout::Planar { nx: 12, ny: 12, dx: 0.5, dy: 0.5, triangular: false }.elements(F), ElementKind::Isotropic, None);
        let w = steer(&m, F, 40.0, 60.0);
        let r = report(&m, &w, F, (40.0, 60.0), &[]);
        assert!(r.pointing_error_deg < 0.05, "{r:?}");
        assert!(r.grating.is_empty());
        // The beam widens as 1/cos θ along the scan plane.
        let bore = report(&m, &uniform(144), F, (0.0, 0.0), &[]);
        let want = bore.hpbw_a / 40f64.to_radians().cos();
        assert!((r.hpbw_a - want).abs() < 0.3, "{} vs {want}", r.hpbw_a);
    }

    #[test]
    fn a_wide_spacing_shows_its_grating_lobe() {
        // 1 λ spacing scanned to sin θ = 0.5 puts a grating lobe at sin θ = −0.5.
        let m = ArrayModel::new(Layout::Line { n: 16, spacing: 1.0 }.elements(F), ElementKind::Isotropic, None);
        let w = steer(&m, F, 30.0, 0.0);
        let r = report(&m, &w, F, (30.0, 0.0), &[]);
        // On a line the lobe is a whole cone, so it's read by its direction cosine along the line.
        let cosine = |g: &(f64, f64, f64)| g.0.to_radians().sin() * g.1.to_radians().cos();
        assert!(r.grating.iter().any(|g| (cosine(g) + 0.5).abs() < 0.02), "{:?}", r.grating);
        assert_eq!(r.grating.len(), 1, "one lobe, however many cells its cone spans: {:?}", r.grating);
    }

    #[test]
    fn a_taper_trades_directivity_for_side_lobes() {
        use crate::array::{Taper, taper_weights};
        let m = line(16, ElementKind::Isotropic);
        let w: Vec<C64> = taper_weights(Taper::DolphChebyshev { sll_db: -30.0 }, 16).into_iter().map(|v| C64::new(v, 0.0)).collect();
        let r = report(&m, &w, F, (0.0, 0.0), &[]);
        assert!((r.sll_db.unwrap() + 30.0).abs() < 0.3, "{:?}", r.sll_db);
        assert!(r.taper_db < -0.3 && r.taper_db > -1.5, "{}", r.taper_db);
    }

    #[test]
    fn panels_of_different_facing_use_their_own_element_pattern() {
        // A 4-face array with a beam on face 0's normal: elements on face 2 face away and
        // barely radiate that way, whatever their phase.
        let l = Layout::Panels { faces: 4, nx: 4, ny: 4, dx: 0.5, dy: 0.5, radius: 2.0, tilt_deg: 0.0 };
        let kind = ElementKind::Cosine { q: 2.0, back_db: -30.0 };
        let m = ArrayModel::new(l.elements(F), kind, None);
        let d = (90.0, 0.0);
        let r = m.response(F, unit_dir(d.0, d.1), None);
        let of = |p: usize| m.elements.iter().enumerate().filter(move |(_, e)| e.panel == p).map(|(i, _)| i).collect::<Vec<_>>();
        let front = of(0).into_iter().map(|i| r[i].norm()).fold(0.0, f64::max);
        let back = of(2).into_iter().map(|i| r[i].norm()).fold(0.0, f64::max);
        assert!(20.0 * (front / back).log10() > 29.0, "{front} vs {back}");
        // On the normal, an element's gain is the same on every face, turned or not.
        let on_face1 = m.response(F, unit_dir(90.0, 90.0), None)[of(1)[0]].norm();
        assert!((on_face1 - front).abs() < 1e-9 * front, "{on_face1} vs {front}");
    }

    #[test]
    fn a_turned_element_keeps_its_polarisation_with_it() {
        // An element facing +x, x axis +y: towards +x it radiates along +y, not along its old axis.
        let e = Element { pos: [0.0; 3], normal: [1.0, 0.0, 0.0], x_axis: [0.0, 1.0, 0.0], panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 };
        let m = ArrayModel::new(vec![e], ElementKind::Cosine { q: 1.0, back_db: -40.0 }, None);
        let field = m.field(&uniform(1), F, [1.0, 0.0, 0.0]);
        let size = |c: C64| c.norm();
        assert!(size(field[1]) > 1e3 * size(field[0]).max(size(field[2])).max(1e-9), "{field:?}");
    }

    #[test]
    fn a_solved_pattern_turns_with_its_element() {
        // A far field that is a beam along its own +z, used by an element facing +x.
        let mut ff = FarField::grid(F, 2.0, 2.0);
        for (ti, t) in ff.theta.clone().iter().enumerate() {
            for (pi, p) in ff.phi.clone().iter().enumerate() {
                let a = t.to_radians().cos().max(0.0);
                let (sp, cp) = p.to_radians().sin_cos();
                // Ludwig 3, x-polarised.
                ff.e_theta[ti][pi] = C64::new(a * cp, 0.0);
                ff.e_phi[ti][pi] = C64::new(-a * sp, 0.0);
            }
        }
        ff.p_accepted = 1.0;
        ff.p_incident = 1.0;
        let e = Element { pos: [0.0; 3], normal: [1.0, 0.0, 0.0], x_axis: [0.0, 1.0, 0.0], panel: 0, ix: 0, iy: 0, nx: 1, ny: 1 };
        let m = ArrayModel::new(vec![e], ElementKind::Solved { embedded: false }, Some(&ff));
        let along_x = m.intensity(&uniform(1), F, [1.0, 0.0, 0.0]);
        let along_z = m.intensity(&uniform(1), F, [0.0, 0.0, 1.0]);
        assert!(along_x > 1e3 * along_z.max(1e-12), "{along_x} vs {along_z}");
        let field = m.field(&uniform(1), F, [1.0, 0.0, 0.0]);
        assert!((field[1].norm() - 1.0).abs() < 1e-9, "x-polarised in its own frame is +y here: {field:?}");
    }

    #[test]
    fn close_elements_cannot_read_above_their_directivity() {
        let m = ArrayModel::new(Layout::Line { n: 8, spacing: 0.15 }.elements(F), ElementKind::Isotropic, None);
        let r = report(&m, &uniform(8), F, (0.0, 0.0), &[]);
        assert!((r.gain_dbi - r.directivity_dbi).abs() < 1e-9, "{r:?}");
        assert!((r.realized_dbi - r.directivity_dbi).abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn peak_refinement_recovers_an_off_grid_beam() {
        let m = line(32, ElementKind::Isotropic);
        let w = steer(&m, F, 17.37, 0.0);
        let r = report(&m, &w, F, (17.37, 0.0), &[]);
        assert!(r.pointing_error_deg < 0.02, "{}", r.pointing_error_deg);
    }

    #[test]
    fn maps_peak_at_the_beam() {
        let m = ArrayModel::new(Layout::Planar { nx: 8, ny: 8, dx: 0.5, dy: 0.5, triangular: false }.elements(F), ElementKind::Isotropic, None);
        let w = steer(&m, F, 30.0, 0.0);
        let map = uv_map(&m, &w, F, 101);
        // Peak at u = sin 30° = 0.5, v = 0, indices (50, 75).
        assert!(map[50][75] > -0.1, "{}", map[50][75]);
        assert!(map[0][0].is_nan(), "the corner is outside the visible circle");
        let az = az_el_map(&m, &w, F, 180, 91);
        // Elevation 60° is θ = 30°, at azimuth 0.
        assert!(az[75][0] > -0.1, "{}", az[75][0]);
    }
}
