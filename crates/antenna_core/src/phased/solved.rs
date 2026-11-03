//! A solved array: every port's far field with the others terminated, and the S-matrix. That
//! holds what the array factor leaves out, so this is where coupling shows: active impedance,
//! scan loss and blindness, and the best any weights can do.
//!
//! All of it is superposition on one solve, exact for a linear antenna, so scanning needs
//! no new solve.

use std::f64::consts::{PI, TAU};

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::aperture::{angle_between, phi_hat, theta_hat, unit_dir};
use super::math::{CMat, ZERO, mat_vec, norm_sqr, zeros};
use super::pattern::{refine_peak, sample_far};
use crate::farfield::FarField;
use crate::geometry::{P3, dot};
use crate::project::SimResult;
use crate::sparam::Network;
use crate::{C0, C64, ETA0, db10, db20};

/// One frequency of a solved array.
#[derive(Clone, Debug)]
pub struct SolvedArray {
    pub f: f64,
    /// Per port, for that port's incident power (see [`FarField::p_incident`]).
    pub far: Vec<FarField>,
    pub s: CMat,
    pub z0: Vec<f64>,
    /// Port centres, the phase centres of the textbook model.
    pub pos: Vec<P3>,
    /// Port names, for reporting which is in trouble.
    pub names: Vec<String>,
}

/// What a beam does on the solved array.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanRow {
    pub theta: f64,
    pub phi: f64,
    /// Realized gain towards the asked direction with textbook phases, dBi.
    pub realized_dbi: f64,
    /// The most any weights can realize there, along the same polarisation: the sum of the
    /// ports' embedded realized gains.
    pub limit_dbi: f64,
    /// Where the beam actually is.
    pub peak_theta: f64,
    pub peak_phi: f64,
    pub pointing_error_deg: f64,
    /// Total active reflection, dB: all that comes back over all that went in.
    pub tarc_db: f64,
    /// The worst port's active reflection, dB, and which it is.
    pub worst_active_db: f64,
    pub worst_port: usize,
    /// Power lost to reflection across the array, dB.
    pub mismatch_db: f64,
}

/// `S` at `f`, or nothing when no sweep covers it.
fn s_at(net: &Network, f: f64) -> Result<CMat> {
    if net.s.is_empty() {
        bail!("this run has no S-parameters");
    }
    let (lo, hi) = (*net.f.first().unwrap_or(&0.0), *net.f.last().unwrap_or(&0.0));
    if f < lo * (1.0 - 1e-9) || f > hi * (1.0 + 1e-9) {
        bail!("{:.4} GHz is outside the S-parameters of this run ({:.4} to {:.4} GHz)", f / 1e9, lo / 1e9, hi / 1e9);
    }
    Ok(net.interpolate(&[f]).s.swap_remove(0))
}

impl SolvedArray {
    pub fn new(f: f64, far: Vec<FarField>, s: CMat, z0: Vec<f64>, pos: Vec<P3>, names: Vec<String>) -> Result<Self> {
        let n = far.len();
        if n == 0 || s.len() != n || pos.len() != n || z0.len() != n {
            bail!("{n} far fields, {} S rows, {} positions and {} impedances don't describe one array", s.len(), pos.len(), z0.len());
        }
        let first = &far[0];
        if far.iter().any(|ff| ff.theta.len() != first.theta.len() || ff.phi.len() != first.phi.len()) {
            bail!("the ports' far fields are on different grids");
        }
        Ok(Self { f, far, s, z0, pos, names })
    }

    /// From an ordinary run with several ports, at far-field index `k`.
    pub fn from_result(r: &SimResult, k: usize, pos: Vec<P3>, names: Vec<String>) -> Result<Self> {
        let Some(f) = r.far_f.get(k).copied() else { bail!("the run has no far field at index {k}") };
        let far: Vec<FarField> = r.far.iter().filter_map(|p| p.get(k).cloned()).collect();
        if far.len() < 2 {
            bail!("a solved array needs at least 2 ports with far fields, and this run has {}", far.len());
        }
        let s = s_at(&r.network, f)?;
        Self::new(f, far, s, r.network.z0.clone(), pos, names)
    }

    pub fn ports(&self) -> usize {
        self.far.len()
    }

    fn k(&self) -> f64 {
        TAU * self.f / C0
    }

    /// One port's field in a direction. The port's phase centre is taken out while
    /// interpolating, or a port far from the origin spins too fast between grid points.
    pub fn port_field(&self, port: usize, theta: f64, phi: f64) -> (C64, C64) {
        let k = self.k();
        let at = |t: f64, p: f64| -> (C64, C64) {
            let spin = C64::from_polar(1.0, -k * dot(unit_dir(t, p), self.pos[port]));
            let (a, b) = sample_far(&self.far[port], t, p);
            (a * spin, b * spin)
        };
        let ff = &self.far[port];
        let (dt, dp) = (ff.theta_step(), ff.phi_step());
        let (nt, np) = (ff.theta.len(), ff.phi.len());
        let (theta, phi) = (theta.clamp(0.0, 180.0), phi.rem_euclid(360.0));
        let x = (theta / dt).clamp(0.0, (nt - 1) as f64);
        let y = (phi / dp).clamp(0.0, (np - 1) as f64);
        let (i, j) = ((x as usize).min(nt - 2), (y as usize).min(np - 2));
        let (s, t) = (x - i as f64, y - j as f64);
        let g = |ii: usize, jj: usize| at(ff.theta[ii], ff.phi[jj]);
        let (a00, a01, a10, a11) = (g(i, j), g(i, j + 1), g(i + 1, j), g(i + 1, j + 1));
        let mix = |p: C64, q: C64, r: C64, u: C64| (p * (1.0 - t) + q * t) * (1.0 - s) + (r * (1.0 - t) + u * t) * s;
        let restore = C64::from_polar(1.0, k * dot(unit_dir(theta, phi), self.pos[port]));
        (mix(a00.0, a01.0, a10.0, a11.0) * restore, mix(a00.1, a01.1, a10.1, a11.1) * restore)
    }

    /// A port's incident power, W.
    fn incident(&self, port: usize) -> f64 {
        let p = self.far[port].p_incident;
        if p > 0.0 { p } else { 1.0 }
    }

    /// All the ports' fields along `pol = (along θ̂, along φ̂)`, conjugated as a projection.
    /// By reciprocity this is also the receive vector of a wave of that polarisation.
    pub fn response(&self, theta: f64, phi: f64, pol: (C64, C64)) -> Vec<C64> {
        (0..self.ports())
            .map(|p| {
                let (a, b) = self.port_field(p, theta, phi);
                a * pol.0.conj() + b * pol.1.conj()
            })
            .collect()
    }

    /// The field of the array, `(Eθ, Eφ)`, for weights.
    pub fn field(&self, w: &[C64], theta: f64, phi: f64) -> (C64, C64) {
        let mut out = (ZERO, ZERO);
        for (p, v) in w.iter().enumerate() {
            if v.norm_sqr() == 0.0 {
                continue;
            }
            let (a, b) = self.port_field(p, theta, phi);
            out.0 += a * *v;
            out.1 += b * *v;
        }
        out
    }

    /// Weights in the units the fields use: `a = w·√p_incident` is what enters each port.
    fn waves(&self, w: &[C64]) -> Vec<C64> {
        w.iter().enumerate().map(|(p, v)| *v * self.incident(p).sqrt()).collect()
    }

    /// Power in through all the ports, W.
    pub fn incident_power(&self, w: &[C64]) -> f64 {
        norm_sqr(&self.waves(w))
    }

    /// Power back out of all the ports, W.
    pub fn reflected_power(&self, w: &[C64]) -> f64 {
        norm_sqr(&mat_vec(&self.s, &self.waves(w)))
    }

    /// `Γₘ = Σ Sₘₙ aₙ / aₘ`, the reflection each port really sees with its neighbours driven.
    pub fn active_reflection(&self, w: &[C64]) -> Vec<C64> {
        let a = self.waves(w);
        let b = mat_vec(&self.s, &a);
        a.iter().zip(&b).map(|(x, y)| if x.norm_sqr() > 0.0 { *y / *x } else { ZERO }).collect()
    }

    /// The impedance each port sees with its neighbours driven.
    pub fn active_impedance(&self, w: &[C64]) -> Vec<C64> {
        self.active_reflection(w).iter().zip(&self.z0).map(|(g, z)| (C64::new(1.0, 0.0) + *g) / (C64::new(1.0, 0.0) - *g) * *z).collect()
    }

    /// Total active reflection coefficient, as a magnitude.
    pub fn tarc(&self, w: &[C64]) -> f64 {
        (self.reflected_power(w) / self.incident_power(w).max(1e-300)).sqrt()
    }

    /// Realized gain towards a direction, in linear terms: everything that went in counts.
    pub fn realized_gain(&self, w: &[C64], theta: f64, phi: f64) -> f64 {
        let (a, b) = self.field(w, theta, phi);
        4.0 * PI * ((a.norm_sqr() + b.norm_sqr()) / (2.0 * ETA0)) / self.incident_power(w).max(1e-300)
    }

    /// One port's embedded realized gain towards a direction, along a polarisation.
    pub fn embedded_gain(&self, port: usize, theta: f64, phi: f64, pol: Option<(C64, C64)>) -> f64 {
        let (a, b) = self.port_field(port, theta, phi);
        let u = match pol {
            Some(p) => (a * p.0.conj() + b * p.1.conj()).norm_sqr(),
            None => a.norm_sqr() + b.norm_sqr(),
        };
        4.0 * PI * (u / (2.0 * ETA0)) / self.incident(port)
    }

    /// Textbook steering: each port's phase from its position alone.
    pub fn textbook_weights(&self, theta: f64, phi: f64) -> Vec<C64> {
        let k = self.k();
        let d = unit_dir(theta, phi);
        self.pos.iter().map(|r| C64::from_polar(1.0, -k * dot(d, *r))).collect()
    }

    /// The polarisation a direction is mostly in, across all the ports.
    pub fn main_pol(&self, theta: f64, phi: f64) -> (C64, C64) {
        let (mut tt, mut pp, mut tp) = (0.0f64, 0.0f64, ZERO);
        for p in 0..self.ports() {
            let (a, b) = self.port_field(p, theta, phi);
            tt += a.norm_sqr();
            pp += b.norm_sqr();
            tp += a * b.conj();
        }
        // The top eigenvector of [[tt, tp], [tp*, pp]].
        let gap = ((tt - pp).powi(2) + 4.0 * tp.norm_sqr()).sqrt();
        let top = 0.5 * (tt + pp + gap);
        let (x, y) = if tp.norm() > 1e-30 { (tp, C64::new(top - tt, 0.0)) } else if tt >= pp { (C64::new(1.0, 0.0), ZERO) } else { (ZERO, C64::new(1.0, 0.0)) };
        let n = (x.norm_sqr() + y.norm_sqr()).sqrt().max(1e-300);
        (x / n, y / n)
    }

    /// Weights that realize the most gain in a direction. What enters port `n` is `aₙ = wₙ·√pₙ`, and
    /// its field per unit of that is `eₙ/√pₙ`, so by Cauchy-Schwarz the best `aₙ` is that field's
    /// conjugate: `wₙ ∝ conj(eₙ)/pₙ`. Nothing can beat them, and they reach the sum of the embedded gains.
    pub fn matched_weights(&self, theta: f64, phi: f64, pol: (C64, C64)) -> Vec<C64> {
        let e = self.response(theta, phi, pol);
        let raw: Vec<C64> = e.iter().enumerate().map(|(p, v)| v.conj() / self.incident(p)).collect();
        let size = raw.iter().map(|v| v.norm()).fold(0.0, f64::max).max(1e-300);
        raw.into_iter().map(|v| v / size).collect()
    }

    /// `w` less whatever would radiate towards `nulls`, in both polarisations, by the least change.
    pub fn nulled(&self, w: &[C64], nulls: &[(f64, f64)]) -> Vec<C64> {
        let mut rows = Vec::with_capacity(2 * nulls.len());
        for (theta, phi) in nulls {
            rows.push(self.response(*theta, *phi, (C64::new(1.0, 0.0), ZERO)));
            rows.push(self.response(*theta, *phi, (ZERO, C64::new(1.0, 0.0))));
        }
        super::beam::project_out(&rows, w)
    }

    /// The most gain any weights can realize towards a direction along a polarisation.
    pub fn gain_limit(&self, theta: f64, phi: f64, pol: (C64, C64)) -> f64 {
        (0..self.ports()).map(|p| self.embedded_gain(p, theta, phi, Some(pol))).sum()
    }

    /// `Bₘₙ = ∮ Eₘ*·Eₙ dΩ / 2η₀`: `wᴴBw` is the power radiated by weights `w`. For a lossless
    /// antenna that must equal `aᴴ(I − SᴴS)a`, which checks the solve.
    pub fn radiation_matrix(&self) -> CMat {
        let n = self.ports();
        let first = &self.far[0];
        let (dt, dp) = (first.theta_step().to_radians(), first.phi_step().to_radians());
        let (nt, np) = (first.theta.len(), first.phi.len());
        let rows: Vec<CMat> = (0..nt)
            .into_par_iter()
            .map(|ti| {
                let th = first.theta[ti].to_radians();
                let mut area = if ti == 0 || ti == nt - 1 { (1.0 - (dt * 0.5).cos()) * dp } else { th.sin() * dt * dp };
                if first.ground && (first.theta[ti] - 90.0).abs() < 1e-9 {
                    area *= 0.5;
                }
                let mut b = zeros(n, n);
                for pi in 0..np - 1 {
                    let et: Vec<C64> = self.far.iter().map(|ff| ff.e_theta[ti][pi]).collect();
                    let ep: Vec<C64> = self.far.iter().map(|ff| ff.e_phi[ti][pi]).collect();
                    for r in 0..n {
                        for c in 0..n {
                            b[r][c] += (et[r].conj() * et[c] + ep[r].conj() * ep[c]) * (area / (2.0 * ETA0));
                        }
                    }
                }
                b
            })
            .collect();
        let mut out = zeros(n, n);
        for b in rows {
            for r in 0..n {
                for c in 0..n {
                    out[r][c] += b[r][c];
                }
            }
        }
        out
    }

    /// `(radiated, accepted)` for weights: their difference is loss in the antenna, or error.
    pub fn power_balance(&self, w: &[C64], b: &CMat) -> (f64, f64) {
        let wh: Vec<C64> = w.iter().map(|v| v.conj()).collect();
        let radiated: f64 = wh.iter().zip(&mat_vec(b, w)).map(|(x, y)| (*x * *y).re).sum();
        (radiated, self.incident_power(w) - self.reflected_power(w))
    }

    /// The full pattern for weights, as the rest of the app reads a pattern.
    pub fn combined(&self, w: &[C64]) -> FarField {
        let first = &self.far[0];
        let mut out = FarField { f: self.f, theta: first.theta.clone(), phi: first.phi.clone(), ground: first.ground, ..Default::default() };
        out.e_theta = zeros(first.theta.len(), first.phi.len());
        out.e_phi = out.e_theta.clone();
        for (ff, v) in self.far.iter().zip(w) {
            for ti in 0..ff.theta.len() {
                for pi in 0..ff.phi.len() {
                    out.e_theta[ti][pi] += ff.e_theta[ti][pi] * *v;
                    out.e_phi[ti][pi] += ff.e_phi[ti][pi] * *v;
                }
            }
        }
        out.p_incident = self.incident_power(w);
        out.p_accepted = out.p_incident - self.reflected_power(w);
        out
    }

    /// The port nearest the middle of the array, whose surroundings look most like a large array's.
    pub fn centre_port(&self) -> usize {
        let n = self.ports() as f64;
        let c: P3 = std::array::from_fn(|k| self.pos.iter().map(|p| p[k]).sum::<f64>() / n);
        let dist = |p: &P3| { let v: P3 = std::array::from_fn(|k| p[k] - c[k]); dot(v, v) };
        self.pos.iter().enumerate().min_by(|a, b| dist(a.1).total_cmp(&dist(b.1))).map_or(0, |x| x.0)
    }

    /// The centre port's embedded pattern with its phase centre moved to the origin, as the
    /// element of a much larger array of the same lattice.
    pub fn embedded_element(&self, port: usize) -> FarField {
        let k = self.k();
        let mut out = self.far[port].clone();
        for (ti, t) in out.theta.clone().iter().enumerate() {
            for (pi, p) in out.phi.clone().iter().enumerate() {
                let spin = C64::from_polar(1.0, -k * dot(unit_dir(*t, *p), self.pos[port]));
                out.e_theta[ti][pi] *= spin;
                out.e_phi[ti][pi] *= spin;
            }
        }
        out
    }

    /// One scan direction: weights from `weights`, judged at that direction.
    pub fn scan_row(&self, theta: f64, phi: f64, w: &[C64]) -> ScanRow {
        let pol = self.main_pol(theta, phi);
        let realized = self.realized_gain(w, theta, phi);
        let limit = self.gain_limit(theta, phi, pol);
        let value = |d: P3| {
            let (t, p) = super::aperture::angles_of(d);
            let (a, b) = self.field(w, t, p);
            a.norm_sqr() + b.norm_sqr()
        };
        // From the strongest point of the grid, since a scanned beam needn't be where it was asked.
        let start = (theta, phi);
        let first = &self.far[0];
        let (mut best, mut at) = (f64::MIN, start);
        for ti in (0..first.theta.len()).step_by(2) {
            for pi in (0..first.phi.len() - 1).step_by(2) {
                let (a, b) = self.field(w, first.theta[ti], first.phi[pi]);
                let v = a.norm_sqr() + b.norm_sqr();
                if v > best {
                    best = v;
                    at = (first.theta[ti], first.phi[pi]);
                }
            }
        }
        let (peak_theta, peak_phi, _) = refine_peak(&value, at, first.theta_step().max(1.0));
        let active = self.active_reflection(w);
        let (worst_port, worst) = active.iter().enumerate().fold((0, 0.0f64), |acc, (i, g)| if g.norm() > acc.1 { (i, g.norm()) } else { acc });
        let incident = self.incident_power(w);
        let reflected = self.reflected_power(w);
        ScanRow {
            theta,
            phi,
            realized_dbi: db10(realized),
            limit_dbi: db10(limit),
            peak_theta,
            peak_phi,
            pointing_error_deg: angle_between((theta, phi), (peak_theta, peak_phi)),
            tarc_db: db20(self.tarc(w)),
            worst_active_db: db20(worst),
            worst_port,
            mismatch_db: db10((1.0 - reflected / incident.max(1e-300)).max(1e-9)),
        }
    }

    /// A scan over θ in one plane, textbook phases at every step.
    pub fn scan_plane(&self, phi: f64, theta_max: f64, step: f64) -> Vec<ScanRow> {
        let n = (theta_max / step).round() as usize;
        (0..=n)
            .into_par_iter()
            .map(|i| {
                let t = i as f64 * step;
                self.scan_row(t, phi, &self.textbook_weights(t, phi))
            })
            .collect()
    }

    /// A pattern cut of realized gain, dBi, for one set of weights.
    pub fn cut(&self, w: &[C64], phi: f64, step: f64) -> Vec<(f64, f64)> {
        let n = (180.0 / step).round() as i64;
        (-n..=n)
            .into_par_iter()
            .map(|i| {
                let t = i as f64 * step;
                let (ta, pa) = if t >= 0.0 { (t, phi) } else { (-t, phi + 180.0) };
                (t, db10(self.realized_gain(w, ta, pa)))
            })
            .collect()
    }
}

/// Where a scan table finds the array in trouble: in a dip of realized gain against its own
/// trend with nearly everything reflected. `(θ, gain drop dB, TARC dB)`.
pub fn blind_spots(rows: &[ScanRow]) -> Vec<(f64, f64, f64)> {
    let mut out = Vec::new();
    for i in 1..rows.len().saturating_sub(1) {
        let (a, b, c) = (&rows[i - 1], &rows[i], &rows[i + 1]);
        let local_min = b.realized_dbi < a.realized_dbi && b.realized_dbi <= c.realized_dbi;
        let drop = rows[0].realized_dbi - b.realized_dbi;
        if local_min && b.tarc_db > -3.0 && drop > 6.0 {
            out.push((b.theta, drop, b.tarc_db));
        }
    }
    out
}

/// The polarisation vector along `(θ̂, φ̂)` a direction's main polarisation has as a 3-vector.
pub fn pol_vector(theta: f64, phi: f64, pol: (C64, C64)) -> [C64; 3] {
    let (a, b) = (theta_hat(theta, phi), phi_hat(phi));
    std::array::from_fn(|k| pol.0 * a[k] + pol.1 * b[k])
}

#[cfg(test)]
mod tests {
    use super::super::aperture::Layout;
    use super::super::pattern::{ArrayModel, ElementKind, report};
    use super::*;
    use crate::geometry::{Geometry, PortDef, Shape, Solid};
    use crate::mom::{MomOptions, MomProblem};

    fn wire_array(nx: usize, ny: usize, dx: f64, dy: f64, f: f64, height: f64, along_z: bool) -> Geometry {
        let lambda = C0 / f;
        let (l, a, g) = (0.47 * lambda, 0.002 * lambda, lambda / 100.0);
        let (mut solids, mut ports) = (Vec::new(), Vec::new());
        for iy in 0..ny {
            for ix in 0..nx {
                let x = (ix as f64 - (nx as f64 - 1.0) / 2.0) * dx;
                let y = (iy as f64 - (ny as f64 - 1.0) / 2.0) * dy;
                let name = format!("element {}", ports.len() + 1);
                let (p0, p1, f0, f1) = if along_z { ([x, y, height - l / 2.0], [x, y, height + l / 2.0], [x, y, height - g / 2.0], [x, y, height + g / 2.0]) } else { ([x, y - l / 2.0, height], [x, y + l / 2.0, height], [x, y - g / 2.0, height], [x, y + g / 2.0, height]) };
                solids.push(Solid { name: name.clone(), material: "PEC".into(), shape: Shape::Wire { pts: vec![p0, p1], r: a } });
                ports.push(PortDef { name, a: f0, b: f1, width: 0.0, z0: 50.0 });
            }
        }
        Geometry { solids, ports, ground: height > 0.0 && !along_z, ..Default::default() }
    }

    fn solve(g: &Geometry, f: f64, step: f64) -> (SolvedArray, Vec<Vec<C64>>) {
        let problem = MomProblem::new(g, f, MomOptions { theta_step: step, phi_step: step, segments_per_lambda: 24.0, ..Default::default() }).unwrap();
        let pt = problem.solve(f, true).unwrap();
        let z0 = vec![50.0; g.ports.len()];
        let net = Network::from_z(vec![f], std::slice::from_ref(&pt.z), z0.clone());
        let pos: Vec<P3> = g.ports.iter().map(PortDef::center).collect();
        let names = g.ports.iter().map(|p| p.name.clone()).collect();
        let array = SolvedArray::new(f, pt.far, net.s[0].clone(), z0, pos, names).unwrap();
        (array, pt.z)
    }

    /// Parallel side-by-side half-wave dipoles are the textbook case (Kraus; Balanis, ch. 8): by
    /// the induced-EMF method Z11 = 73.1 + j42.5 Ω and Z21 at λ/2 = −12.5 − j29.9 Ω. That method
    /// assumes a sinusoidal current, which only a wire of no thickness has, so a real wire
    /// sits off it and has to close in as it gets thinner. The self impedance of a thin wire
    /// has its own known answer from the integral equation, about 80 to 90 + j45 (King and Middleton).
    #[test]
    fn mutual_impedance_closes_in_on_the_textbook_as_the_wire_thins() {
        let f = 3e8;
        let lambda = C0 / f;
        let pair = |radius: f64, per_lambda: f64| -> (C64, C64) {
            let (l, a, gap) = (0.5 * lambda, radius * lambda, lambda / 100.0);
            let dipole = |x: f64, name: &str| (Solid { name: name.into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[x, 0.0, -l / 2.0], [x, 0.0, l / 2.0]], r: a } }, PortDef { name: name.into(), a: [x, 0.0, -gap / 2.0], b: [x, 0.0, gap / 2.0], width: 0.0, z0: 50.0 });
            let (s1, p1) = dipole(0.0, "a");
            let (s2, p2) = dipole(0.5 * lambda, "b");
            let g = Geometry { solids: vec![s1, s2], ports: vec![p1, p2], ..Default::default() };
            let m = MomProblem::new(&g, f, MomOptions { theta_step: 30.0, phi_step: 30.0, segments_per_lambda: per_lambda, ..Default::default() }).unwrap();
            let z = m.solve(f, false).unwrap().z;
            assert!((z[0][1] - z[1][0]).norm() < 1e-6 * z[1][0].norm().max(1.0), "reciprocal: {} vs {}", z[0][1], z[1][0]);
            (z[0][0], z[1][0])
        };
        let textbook = C64::new(-12.5, -29.9);
        let miss = |r: f64| (pair(r, 48.0).1 - textbook).norm();
        let (thick, mid, thin) = (miss(0.002), miss(0.0005), miss(0.0001));
        println!("|Z21 − textbook|: {thick:.2} at a/λ 0.002, {mid:.2} at 0.0005, {thin:.2} at 0.0001");
        assert!(thin < mid && mid < thick, "{thick} {mid} {thin}");
        assert!(thin < 5.0, "{thin}");
        // Settled on the mesh: twice the segments moves it by little against that gap.
        let (coarse, fine) = (pair(0.0005, 48.0).1, pair(0.0005, 96.0).1);
        assert!((coarse - fine).norm() < 0.5, "{coarse} vs {fine}");
        let (z11, _) = pair(0.0005, 96.0);
        assert!((78.0..92.0).contains(&z11.re) && (40.0..50.0).contains(&z11.im), "{z11}");
    }

    #[test]
    fn a_lossless_solve_balances_power_for_any_weights() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(4, 3, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 5.0);
        let b = a.radiation_matrix();
        let mut rng = proof::Rng::new(5);
        for trial in 0..5 {
            let w: Vec<C64> = (0..a.ports()).map(|_| C64::new(rng.gauss(), rng.gauss())).collect();
            let (radiated, accepted) = a.power_balance(&w, &b);
            let e = (radiated - accepted).abs() / accepted.abs().max(1e-12);
            assert!(e < 0.04, "trial {trial}: radiated {radiated} vs accepted {accepted}");
        }
    }

    #[test]
    fn a_port_scaled_to_another_incident_power_is_the_same_array() {
        // The same solve with one port's field recorded for 4 W: gain limit and best weights' gain
        // can't change, nor can what its realized gain reads once the weights make up for it.
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(3, 2, 0.45 * lambda, 0.6 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 5.0);
        let mut b = a.clone();
        for ti in 0..b.far[1].theta.len() {
            for pi in 0..b.far[1].phi.len() {
                b.far[1].e_theta[ti][pi] *= 2.0;
                b.far[1].e_phi[ti][pi] *= 2.0;
            }
        }
        b.far[1].p_incident *= 4.0;
        b.far[1].p_accepted *= 4.0;
        let (t, p) = (25.0, 30.0);
        let pol = a.main_pol(t, p);
        let along = |x: &SolvedArray, w: &[C64]| {
            let (u, v) = x.field(w, t, p);
            let e = u * pol.0.conj() + v * pol.1.conj();
            4.0 * PI * (e.norm_sqr() / (2.0 * ETA0)) / x.incident_power(w)
        };
        let limit = a.gain_limit(t, p, pol);
        assert!((b.gain_limit(t, p, pol) - limit).abs() < 1e-9 * limit);
        let m = b.matched_weights(t, p, pol);
        assert!((along(&b, &m) - limit).abs() < 1e-6 * limit, "{} vs {limit}", along(&b, &m));
    }

    #[test]
    fn a_null_on_a_solved_array_holds_both_polarisations() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(4, 3, 0.45 * lambda, 0.6 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 5.0);
        let w = a.textbook_weights(0.0, 0.0);
        let n = a.nulled(&w, &[(40.0, 0.0)]);
        let depth = 10.0 * (a.realized_gain(&n, 40.0, 0.0) / a.realized_gain(&n, 0.0, 0.0)).log10();
        assert!(depth < -80.0, "{depth}");
        let loss = 10.0 * (a.realized_gain(&n, 0.0, 0.0) / a.realized_gain(&w, 0.0, 0.0)).log10();
        assert!(loss > -2.0, "{loss}");
    }

    #[test]
    fn s_is_passive_and_reciprocal() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(3, 3, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 15.0);
        for r in 0..a.ports() {
            let power: f64 = a.s.iter().map(|row| row[r].norm_sqr()).sum();
            assert!(power <= 1.0 + 1e-9, "column {r}: {power}");
            for c in 0..a.ports() {
                assert!((a.s[r][c] - a.s[c][r]).norm() < 1e-9, "({r},{c})");
            }
        }
    }

    #[test]
    fn nothing_beats_the_matched_weights_and_they_reach_the_stated_limit() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(4, 4, 0.45 * lambda, 0.6 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 5.0);
        let dir = (25.0, 30.0);
        let pol = a.main_pol(dir.0, dir.1);
        let along = |w: &[C64]| {
            let (x, y) = a.field(w, dir.0, dir.1);
            let v = x * pol.0.conj() + y * pol.1.conj();
            4.0 * PI * (v.norm_sqr() / (2.0 * ETA0)) / a.incident_power(w)
        };
        let m = a.matched_weights(dir.0, dir.1, pol);
        let limit = a.gain_limit(dir.0, dir.1, pol);
        assert!((along(&m) - limit).abs() < 1e-6 * limit, "{} vs {limit}", along(&m));
        let textbook = a.textbook_weights(dir.0, dir.1);
        assert!(along(&textbook) <= limit * (1.0 + 1e-9));
        let mut rng = proof::Rng::new(9);
        for _ in 0..500 {
            let mut w = m.clone();
            for v in &mut w {
                *v += C64::new(rng.gauss(), rng.gauss()) * 0.3 * v.norm();
            }
            assert!(along(&w) <= limit * (1.0 + 1e-9));
        }
        println!("textbook {:.2} dB, matched {:.2} dB", db10(along(&textbook)), db10(limit));
    }

    /// The element-gain paradox (Hannan, 1964): a matched element in a large array has gain
    /// 4π·dx·dy/λ² at broadside, however gainy it is alone. Half-wave lattice: 4π/4 = 4.97 dBi.
    #[test]
    fn a_large_array_element_has_the_gain_of_its_cell() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(9, 9, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 10.0);
        let c = a.centre_port();
        let got = a.embedded_gain(c, 0.0, 0.0, None);
        let reflect = {
            // What the middle port sees with the array scanned to broadside: its mismatch.
            let w = vec![C64::new(1.0, 0.0); a.ports()];
            a.active_reflection(&w)[c].norm_sqr()
        };
        let cell = 4.0 * PI * (0.5 * 0.5);
        println!("embedded {:.2} dBi, cell {:.2} dBi, active |Γ|² {reflect:.3}", db10(got), db10(cell));
        // The cell's gain less the middle port's own mismatch with the array driven.
        let want = db10(cell) + db10(1.0 - reflect);
        assert!((db10(got) - want).abs() < 0.5, "{} vs {want}", db10(got));
    }

    /// Superposition: the array's realized gain is what the sum of the embedded gains allow.
    #[test]
    fn scanned_gain_stays_under_the_limit_and_falls_off_with_scan() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(7, 7, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 5.0);
        let rows = a.scan_plane(0.0, 60.0, 15.0);
        for r in &rows {
            assert!(r.realized_dbi <= r.limit_dbi + 1e-6, "{r:?}");
            assert!(r.pointing_error_deg < 6.0, "{r:?}");
        }
        assert!(rows[0].realized_dbi > rows[4].realized_dbi + 1.0, "{rows:#?}");
        println!("{rows:#?}");
    }

    /// Wheeler's current sheet: the active resistance goes as cos θ scanned in the E plane
    /// and 1/cos θ in the H plane. Dipoles along y over ground: E plane is φ = 90°, H plane φ = 0°.
    #[test]
    fn active_resistance_moves_the_way_a_current_sheet_says() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(9, 9, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 15.0);
        let c = a.centre_port();
        let r_at = |t: f64, p: f64| a.active_impedance(&a.textbook_weights(t, p))[c].re;
        let (bore, e, h) = (r_at(0.0, 0.0), r_at(45.0, 90.0), r_at(45.0, 0.0));
        println!("R broadside {bore:.1}, E plane 45° {e:.1}, H plane 45° {h:.1}");
        assert!(e < bore, "E-plane scan lowers it: {e} vs {bore}");
        assert!(h > bore, "H-plane scan raises it: {h} vs {bore}");
    }

    /// A small solved array's centre element predicts a bigger one, coupling included.
    #[test]
    fn an_embedded_pattern_scales_up_to_a_larger_array() {
        let f = 1e9;
        let lambda = C0 / f;
        let (d, h) = (0.5 * lambda, 0.25 * lambda);
        let (small, _) = solve(&wire_array(7, 7, d, d, f, h, false), f, 5.0);
        let (big, _) = solve(&wire_array(11, 11, d, d, f, h, false), f, 5.0);
        let element = small.embedded_element(small.centre_port());
        let layout = Layout::Planar { nx: 11, ny: 11, dx: 0.5, dy: 0.5, triangular: false };
        let m = ArrayModel::new(layout.elements(f), ElementKind::Solved { embedded: true }, Some(&element));
        for (t, p) in [(0.0, 0.0), (30.0, 0.0), (30.0, 90.0)] {
            let dir = unit_dir(t, p);
            let k = TAU * f / C0;
            let w: Vec<C64> = m.elements.iter().map(|e| C64::from_polar(1.0, -k * dot(dir, e.pos))).collect();
            let rep = report(&m, &w, f, (t, p), &[]);
            let direct = db10(big.realized_gain(&big.textbook_weights(t, p), t, p));
            println!("scan ({t}, {p}): from the 7×7's element {:.2} dBi, full 11×11 {direct:.2} dBi", rep.at_request_dbi);
            // What's coupled away isn't loss: realized can't read above gain, nor gain above directivity.
            assert!(rep.realized_dbi <= rep.gain_dbi + 1e-9, "{rep:?}");
            assert!(rep.gain_dbi <= rep.directivity_dbi + 0.5, "{rep:?}");
            // A large array's directivity is its aperture's, N·π at λ/2, times cos θ scanned.
            let aperture = db10(121.0 * PI * t.to_radians().cos());
            assert!((rep.directivity_dbi - aperture).abs() < 0.6, "{} vs {aperture}", rep.directivity_dbi);
            // The worst is the E plane, where collinear dipoles a sliver apart couple hardest.
            assert!((rep.at_request_dbi - direct).abs() < 1.25, "({t}, {p}): {} vs {direct}", rep.at_request_dbi);
        }
    }

    #[test]
    fn active_reflection_of_one_driven_port_is_its_own_s() {
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(2, 2, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (a, _) = solve(&g, f, 15.0);
        let mut w = vec![ZERO; 4];
        w[1] = C64::new(1.0, 0.0);
        let gamma = a.active_reflection(&w);
        assert!((gamma[1] - a.s[1][1]).norm() < 1e-9, "{} vs {}", gamma[1], a.s[1][1]);
        // Uniform drive on a symmetric square: the same active Γ on every port.
        let uniform = vec![C64::new(1.0, 0.0); 4];
        let g2 = a.active_reflection(&uniform);
        for x in &g2 {
            assert!((*x - g2[0]).norm() < 1e-6, "{g2:?}");
        }
        assert!(a.tarc(&uniform) >= 0.0);
    }

    #[test]
    fn interpolation_through_the_phase_centre_holds_off_the_grid() {
        // Solved on 10°, read halfway between grid points, against a solve on 5°.
        let f = 1e9;
        let lambda = C0 / f;
        let g = wire_array(6, 1, 0.5 * lambda, 0.5 * lambda, f, 0.25 * lambda, false);
        let (coarse, _) = solve(&g, f, 10.0);
        let (fine, _) = solve(&g, f, 5.0);
        let port = 5;
        for (t, p) in [(35.0, 35.0), (55.0, 125.0), (65.0, 15.0)] {
            let (a, b) = coarse.port_field(port, t, p);
            let (x, y) = fine.port_field(port, t, p);
            let (ti, pi) = (fine.far[port].theta.iter().position(|v| (*v - t).abs() < 1e-9).unwrap(), fine.far[port].phi.iter().position(|v| (*v - p).abs() < 1e-9).unwrap());
            let (ex, ey) = fine.far[port].at(ti, pi);
            assert!((x - ex).norm() < 1e-9 * ex.norm().max(1.0), "on the grid, it's the grid's value");
            assert!((y - ey).norm() < 1e-9 * ey.norm().max(1.0));
            let size = (ex.norm_sqr() + ey.norm_sqr()).sqrt();
            let miss = ((a - ex).norm_sqr() + (b - ey).norm_sqr()).sqrt() / size;
            assert!(miss < 0.08, "({t}, {p}): {miss}");
        }
    }
}
