//! Angle-of-arrival arrays as they really are: elements couple, the mount scatters,
//! and the textbook phase model drifts from what the array actually does.
//!
//! Each port's far field, solved with the other ports terminated, already holds the
//! coupling and the mount. Feeding a wave through those fields and estimating its
//! direction with the textbook model shows the angle error. Using the solved fields
//! as the manifold gives a calibration table, usable before any hardware exists,
//! and testing that table against a perturbed array shows how well it would hold up.

use std::f64::consts::TAU;

use anyhow::{Result, bail};
use proof::Rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::farfield::FarField;
use crate::geometry::{Geometry, P3, PortDef, Shape, Solid, dot};
use crate::{C0, C64};

/// A receive vector per direction, with which grid point it belongs to.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifold {
    pub f: f64,
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    /// `a[θ][φ][element]`, unit norm over the elements.
    pub a: Vec<Vec<Vec<C64>>>,
}

fn unit_dir(theta: f64, phi: f64) -> P3 {
    let (st, ct) = theta.to_radians().sin_cos();
    let (sp, cp) = phi.to_radians().sin_cos();
    [st * cp, st * sp, ct]
}

fn normalised(mut v: Vec<C64>) -> Vec<C64> {
    let n = v.iter().map(|x| x.norm_sqr()).sum::<f64>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// Directions a locator's estimator searches: the half space in front of it.
fn grid(step: f64, theta_max: f64) -> (Vec<f64>, Vec<f64>) {
    let nt = (theta_max / step).round() as usize + 1;
    let np = (360.0 / step).round() as usize;
    ((0..nt).map(|i| i as f64 * step).collect(), (0..np).map(|i| i as f64 * step).collect())
}

/// The textbook model: isotropic elements at their positions, nothing else.
pub fn ideal_manifold(positions: &[P3], f: f64, step: f64, theta_max: f64) -> Manifold {
    let (theta, phi) = grid(step, theta_max);
    let k = TAU * f / C0;
    let a = theta.iter().map(|t| phi.iter().map(|p| {
        let r = unit_dir(*t, *p);
        normalised(positions.iter().map(|x| C64::from_polar(1.0, k * dot(r, *x))).collect())
    }).collect()).collect();
    Manifold { f, theta, phi, a }
}

/// What a wave from (`theta`, `phi`), polarised along `pol = (θ̂, φ̂)`, puts on each
/// port. By reciprocity that's the port's transmit far field in that direction.
fn snapshot_at(fields: &[FarField], ti: usize, pi: usize, pol: (f64, f64)) -> Vec<C64> {
    fields.iter().map(|f| { let (t, p) = f.at(ti, pi); t * pol.0 + p * pol.1 }).collect()
}

/// A manifold from the solved far fields: this is the calibration table. All
/// fields must share a grid. Directions past `theta_max` aren't included.
pub fn solved_manifold(fields: &[FarField], theta_max: f64, step: f64, pol: (f64, f64)) -> Result<Manifold> {
    let Some(first) = fields.first() else { bail!("no far fields") };
    let (theta, phi) = grid(step, theta_max);
    let near = |v: &[f64], x: f64| v.iter().enumerate().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |e| e.0);
    let a = theta.iter().map(|t| phi.iter().map(|p| normalised(snapshot_at(fields, near(&first.theta, *t), near(&first.phi, *p), pol))).collect()).collect();
    Ok(Manifold { f: first.f, theta, phi, a })
}

impl Manifold {
    /// The grid direction that best explains `x` (single source: conventional
    /// beamformer, which here is the same as MUSIC with one source).
    pub fn estimate(&self, x: &[C64]) -> (f64, f64) {
        let power = |ti: usize, pi: usize| -> f64 { self.a[ti][pi].iter().zip(x).map(|(u, v)| u.conj() * *v).sum::<C64>().norm_sqr() };
        let (mut peak, mut at) = (0.0, (0, 0));
        for ti in 0..self.a.len() {
            for pi in 0..self.a[ti].len() {
                let v = power(ti, pi);
                if v > peak {
                    peak = v;
                    at = (ti, pi);
                }
            }
        }
        let (ti, pi) = at;
        // A parabola through the peak and its neighbours: a table can't be finer
        // than its grid otherwise, and that would hide any error smaller than a step.
        let vertex = |a: f64, b: f64, c: f64| -> f64 {
            let d = a - 2.0 * b + c;
            if d < 0.0 { (0.5 * (a - c) / d).clamp(-0.5, 0.5) } else { 0.0 }
        };
        let step = |v: &[f64]| if v.len() > 1 { v[1] - v[0] } else { 0.0 };
        let mut theta = self.theta[ti];
        if ti > 0 && ti + 1 < self.a.len() {
            theta += vertex(power(ti - 1, pi), peak, power(ti + 1, pi)) * step(&self.theta);
        }
        let np = self.phi.len();
        let phi = if np > 2 && ti > 0 { self.phi[pi] + vertex(power(ti, (pi + np - 1) % np), peak, power(ti, (pi + 1) % np)) * step(&self.phi) } else { self.phi[pi] };
        (theta, phi)
    }
}

/// The angle between two directions, degrees.
pub fn angle_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    dot(unit_dir(a.0, a.1), unit_dir(b.0, b.1)).clamp(-1.0, 1.0).acos().to_degrees()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AoaMetrics {
    pub rms_deg: f64,
    pub p95_deg: f64,
    pub max_deg: f64,
    pub directions: usize,
}

/// What differs between the array that made the table and the array in the field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Perturbation {
    /// Phase error per element (RF path tolerance), degrees (1σ).
    pub phase_sigma_deg: f64,
    /// Amplitude error per element, dB (1σ).
    pub amp_sigma_db: f64,
    /// Noise on each port relative to the signal, dB below (0 = none).
    pub snr_db: f64,
    pub seed: u64,
}

/// Runs every direction of `fields` within `theta_max` through `manifold` and scores
/// the angle errors. Directions with almost no signal (nulls) are skipped.
pub fn evaluate(fields: &[FarField], manifold: &Manifold, theta_max: f64, step: f64, pol: (f64, f64), p: Perturbation) -> Result<AoaMetrics> {
    let (theta, phi) = grid(step, theta_max);
    let dirs: Vec<(f64, f64)> = theta.iter().flat_map(|t| phi.iter().map(move |q| (*t, *q))).collect();
    evaluate_at(fields, manifold, &dirs, theta_max, pol, p)
}

/// The directions halfway between the grid points of a table with this step.
pub fn between_grid_points(step: f64, theta_max: f64) -> Vec<(f64, f64)> {
    let (theta, phi) = grid(step, theta_max);
    theta.iter().filter(|t| **t + step / 2.0 <= theta_max).flat_map(|t| phi.iter().map(move |q| (*t + step / 2.0, *q + step / 2.0))).collect()
}

/// How a table does when it isn't marking its own homework.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HeldOut {
    /// RMS over every direction, frequency and draw.
    pub rms_deg: f64,
    /// 95th percentile, averaged over the runs.
    pub p95_deg: f64,
    /// The worst single array and frequency.
    pub worst_run_rms_deg: f64,
    pub frequencies: usize,
    pub draws: usize,
}

/// A table tested on what didn't build it: at directions between its grid points, at
/// each set of far fields in `by_freq` (the table is from one frequency), and against
/// `draws` differently perturbed arrays. The fields must be solved at half `step` or finer.
pub fn held_out(by_freq: &[Vec<FarField>], table: &Manifold, theta_max: f64, step: f64, pol: (f64, f64), p: Perturbation, draws: usize) -> Result<HeldOut> {
    if by_freq.is_empty() || draws == 0 {
        bail!("nothing to test the table against");
    }
    let dirs = between_grid_points(step, theta_max);
    let (mut sum, mut p95, mut worst, mut runs) = (0.0, 0.0, 0.0f64, 0.0);
    for fields in by_freq {
        for d in 0..draws {
            let m = evaluate_at(fields, table, &dirs, theta_max, pol, Perturbation { seed: p.seed.wrapping_add(d as u64 * 7919), ..p })?;
            sum += m.rms_deg * m.rms_deg;
            p95 += m.p95_deg;
            worst = worst.max(m.rms_deg);
            runs += 1.0;
        }
    }
    Ok(HeldOut { rms_deg: (sum / runs).sqrt(), p95_deg: p95 / runs, worst_run_rms_deg: worst, frequencies: by_freq.len(), draws })
}

/// [`evaluate`] over given directions.
pub fn evaluate_at(fields: &[FarField], manifold: &Manifold, dirs: &[(f64, f64)], theta_max: f64, pol: (f64, f64), p: Perturbation) -> Result<AoaMetrics> {
    let Some(first) = fields.first() else { bail!("no far fields") };
    let near = |v: &[f64], x: f64| v.iter().enumerate().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map_or(0, |e| e.0);
    let mut rng = Rng::new(p.seed.wrapping_add(1));
    let per_element: Vec<C64> = fields
        .iter()
        .map(|_| {
            let a = 10f64.powf(rng.gauss() * p.amp_sigma_db / 20.0);
            C64::from_polar(a, (rng.gauss() * p.phase_sigma_deg).to_radians())
        })
        .collect();
    let peak = dirs.iter().map(|(t, q)| snapshot_at(fields, near(&first.theta, *t), near(&first.phi, *q), pol).iter().map(|c| c.norm_sqr()).sum::<f64>()).fold(0.0, f64::max);
    let errs: Vec<(f64, f64)> = dirs
        .par_iter()
        .enumerate()
        .filter_map(|(i, (t, q))| {
            let mut x = snapshot_at(fields, near(&first.theta, *t), near(&first.phi, *q), pol);
            let power: f64 = x.iter().map(|c| c.norm_sqr()).sum();
            // Below −20 dB of the strongest direction no real link would be here.
            if power < peak * 1e-2 {
                return None;
            }
            for (c, g) in x.iter_mut().zip(&per_element) {
                *c *= *g;
            }
            if p.snr_db > 0.0 {
                let mut r = Rng::new(p.seed.wrapping_add(1000 + i as u64));
                let s = (power / x.len() as f64 / 10f64.powf(p.snr_db / 10.0) / 2.0).sqrt();
                for c in x.iter_mut() {
                    *c += C64::new(r.gauss(), r.gauss()) * s;
                }
            }
            let got = manifold.estimate(&x);
            // At the zenith every φ is the same direction, which `angle_between` handles.
            Some((angle_between(got, (*t, *q)), t.to_radians().sin().max(0.02)))
        })
        .collect();
    if errs.is_empty() {
        bail!("no direction within {theta_max}° has any signal");
    }
    let total: f64 = errs.iter().map(|e| e.1).sum();
    let rms = (errs.iter().map(|(e, w)| e * e * w).sum::<f64>() / total).sqrt();
    let mut sorted = errs.clone();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut acc = 0.0;
    let mut p95 = sorted.last().map_or(0.0, |e| e.0);
    for (e, w) in &sorted {
        acc += w;
        if acc >= 0.95 * total {
            p95 = *e;
            break;
        }
    }
    Ok(AoaMetrics { rms_deg: rms, p95_deg: p95, max_deg: sorted.last().map_or(0.0, |e| e.0), directions: errs.len() })
}

/// The report: the textbook model against the table made from the solved array.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AoaReport {
    pub f: f64,
    pub elements: usize,
    /// The textbook phase model on the solved array.
    pub ideal: AoaMetrics,
    /// The table on the same array, with noise and path tolerances: what it should
    /// do on hardware that matches the model.
    pub table: AoaMetrics,
    /// The textbook model with the same noise and tolerances, as a fair comparison.
    pub ideal_noisy: AoaMetrics,
}

/// Port centres, the phase centres the textbook model assumes.
pub fn positions(geom: &Geometry) -> Vec<P3> {
    geom.ports.iter().map(PortDef::center).collect()
}

pub fn analyse(geom: &Geometry, fields: &[FarField], theta_max: f64, step: f64, pol: (f64, f64), field_conditions: Perturbation) -> Result<AoaReport> {
    if fields.len() < 2 {
        bail!("angle of arrival needs at least 2 ports");
    }
    let f = fields[0].f;
    let pos = positions(geom);
    if pos.len() != fields.len() {
        bail!("{} ports but {} far fields", pos.len(), fields.len());
    }
    let ideal_m = ideal_manifold(&pos, f, step, theta_max);
    let table_m = solved_manifold(fields, theta_max, step, pol)?;
    let clean = Perturbation::default();
    Ok(AoaReport {
        f,
        elements: fields.len(),
        ideal: evaluate(fields, &ideal_m, theta_max, step, pol, clean)?,
        table: evaluate(fields, &table_m, theta_max, step, pol, field_conditions)?,
        ideal_noisy: evaluate(fields, &ideal_m, theta_max, step, pol, field_conditions)?,
    })
}

/// A rectangular monopole array on an infinite ground: the quick stand-in for a
/// locator when no custom array exists. `dx`, `dy` are the spacings in metres.
pub fn monopole_array(nx: usize, ny: usize, dx: f64, dy: f64, f: f64) -> Geometry {
    let lambda = C0 / f;
    let (h, a, g) = (0.235 * lambda, 0.002 * lambda, lambda / 100.0);
    let (mut solids, mut ports) = (Vec::new(), Vec::new());
    for iy in 0..ny {
        for ix in 0..nx {
            let x = (ix as f64 - (nx as f64 - 1.0) / 2.0) * dx;
            let y = (iy as f64 - (ny as f64 - 1.0) / 2.0) * dy;
            let name = format!("element {}", ports.len() + 1);
            solids.push(Solid { name: name.clone(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[x, y, 0.0], [x, y, h]], r: a } });
            ports.push(PortDef { name, a: [x, y, 0.0], b: [x, y, g], width: 0.0, z0: 50.0 });
        }
    }
    Geometry { solids, ports, ground: true, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mom::{MomOptions, MomProblem};

    /// Far fields of ideal isotropic elements: no coupling, no mount.
    fn ideal_fields(pos: &[P3], f: f64, step: f64) -> Vec<FarField> {
        let k = TAU * f / C0;
        pos.iter()
            .map(|x| {
                let mut ff = FarField::grid(f, step, step);
                for (ti, t) in ff.theta.clone().iter().enumerate() {
                    for (pi, p) in ff.phi.clone().iter().enumerate() {
                        let r = unit_dir(*t, *p);
                        ff.e_theta[ti][pi] = C64::from_polar(1.0, k * dot(r, *x));
                    }
                }
                ff
            })
            .collect()
    }

    fn square(n: usize, d: f64) -> Vec<P3> {
        let mut v = Vec::new();
        for y in 0..n {
            for x in 0..n {
                v.push([(x as f64 - (n as f64 - 1.0) / 2.0) * d, (y as f64 - (n as f64 - 1.0) / 2.0) * d, 0.0]);
            }
        }
        v
    }

    #[test]
    fn an_ideal_array_has_no_angle_error() {
        let f = 2.44e9;
        let pos = square(4, 0.5 * C0 / f);
        let fields = ideal_fields(&pos, f, 5.0);
        let m = ideal_manifold(&pos, f, 5.0, 60.0);
        let got = evaluate(&fields, &m, 60.0, 5.0, (1.0, 0.0), Perturbation::default()).unwrap();
        // The floor of interpolating a 5° table, not an array error.
        assert!(got.max_deg < 0.5, "{got:?}");
    }

    // OUT-M6: truth and table were the same fields at the same grid points.
    #[test]
    fn a_table_tested_on_what_built_it_looks_far_better_than_it_is() {
        let f = 2.44e9;
        let pos = square(4, 0.5 * C0 / f);
        let m = ideal_manifold(&pos, f, 5.0, 60.0);
        let fine = |g: f64| ideal_fields(&pos, g, 2.5);
        let clean = Perturbation::default();
        let on_grid = evaluate(&fine(f), &m, 60.0, 5.0, (1.0, 0.0), clean).unwrap();
        // Off the grid, same frequency: only interpolating the table.
        let off = held_out(&[fine(f)], &m, 60.0, 5.0, (1.0, 0.0), clean, 1).unwrap();
        assert!(off.rms_deg > on_grid.rms_deg, "{off:?} vs {on_grid:?}");
        // At a BLE band edge: the error at 60° is about tan 60° × 1.6 % = 1.6°.
        let edge = evaluate(&fine(f * 1.016), &m, 60.0, 5.0, (1.0, 0.0), clean).unwrap();
        assert!(edge.max_deg > 1.2, "{edge:?}");
        assert!(edge.rms_deg > 3.0 * on_grid.rms_deg.max(0.05), "{edge:?} vs {on_grid:?}");
        let band = held_out(&[fine(f * 0.984), fine(f), fine(f * 1.016)], &m, 60.0, 5.0, (1.0, 0.0), clean, 1).unwrap();
        assert!(band.rms_deg > on_grid.rms_deg + 0.2, "{band:?} vs {on_grid:?}");
        // A perturbed array, one draw then many: one lucky draw isn't the figure.
        let field = Perturbation { phase_sigma_deg: 10.0, amp_sigma_db: 1.0, snr_db: 20.0, seed: 1 };
        let many = held_out(&[fine(f)], &m, 60.0, 5.0, (1.0, 0.0), field, 12).unwrap();
        let first = held_out(&[fine(f)], &m, 60.0, 5.0, (1.0, 0.0), field, 1).unwrap();
        assert_eq!((many.draws, many.frequencies), (12, 1));
        assert!(many.worst_run_rms_deg > first.rms_deg, "{many:?} vs {first:?}");
        assert!(many.rms_deg > on_grid.rms_deg, "{many:?}");
    }

    #[test]
    fn phase_errors_cost_angle_and_noise_too() {
        let f = 2.44e9;
        let pos = square(4, 0.5 * C0 / f);
        let fields = ideal_fields(&pos, f, 5.0);
        let m = ideal_manifold(&pos, f, 5.0, 60.0);
        let phase = evaluate(&fields, &m, 60.0, 5.0, (1.0, 0.0), Perturbation { phase_sigma_deg: 20.0, seed: 3, ..Default::default() }).unwrap();
        let noisy = evaluate(&fields, &m, 60.0, 5.0, (1.0, 0.0), Perturbation { snr_db: 5.0, seed: 3, ..Default::default() }).unwrap();
        assert!(phase.rms_deg > 0.3, "{phase:?}");
        assert!(noisy.rms_deg > 0.3, "{noisy:?}");
    }

    #[test]
    fn a_real_array_differs_from_the_textbook_and_the_table_fixes_it() {
        // 4×4 monopoles half a wavelength apart: coupled, as real arrays are. The
        // edge elements see different neighbours than the middle ones.
        let f = 2.44e9;
        let d = 0.5 * C0 / f;
        let geom = monopole_array(4, 4, d, d, f);
        let problem = MomProblem::new(&geom, f, MomOptions { theta_step: 5.0, phi_step: 5.0, segments_per_lambda: 20.0, ..Default::default() }).unwrap();
        let t = std::time::Instant::now();
        let pt = problem.solve(f, true).unwrap();
        println!("solved in {:.1} s", t.elapsed().as_secs_f64());
        let r = analyse(&geom, &pt.far, 80.0, 5.0, (1.0, 0.0), Perturbation::default()).unwrap();
        println!("{r:#?}");
        // Without coupling a monopole's phase centre is its own position; with it the
        // ideal model goes wrong by a measurable amount, and the table doesn't.
        assert!(r.ideal.rms_deg > 1.0, "ideal {:?}", r.ideal);
        assert!(r.table.rms_deg < 0.5, "table {:?}", r.table);
        // Coupled ports: the matrix isn't diagonal.
        let z = &pt.z;
        assert!(z[0][1].norm() > 1.0, "{}", z[0][1]);
    }

    #[test]
    fn the_table_survives_field_tolerances_better_than_the_textbook() {
        let f = 2.44e9;
        let d = 0.5 * C0 / f;
        let geom = monopole_array(4, 4, d, d, f);
        let problem = MomProblem::new(&geom, f, MomOptions { theta_step: 5.0, phi_step: 5.0, segments_per_lambda: 20.0, ..Default::default() }).unwrap();
        let pt = problem.solve(f, true).unwrap();
        let field = Perturbation { phase_sigma_deg: 5.0, amp_sigma_db: 0.5, snr_db: 20.0, seed: 9 };
        let r = analyse(&geom, &pt.far, 60.0, 5.0, (1.0, 0.0), field).unwrap();
        println!("{r:#?}");
        assert!(r.table.rms_deg < r.ideal_noisy.rms_deg, "table {:?} vs textbook {:?}", r.table, r.ideal_noisy);
    }
}
