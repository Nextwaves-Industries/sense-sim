//! Radiation from sampled currents, and the plane wave used for scattering.

use std::f64::consts::PI;

use rayon::prelude::*;

use crate::farfield::FarField;
use crate::geometry::{P3, dot};
use crate::{C0, C64, ETA0};

/// A current sample: position and current moment (A·m).
#[derive(Clone, Copy, Debug)]
pub struct Moment {
    pub pos: P3,
    pub j: [C64; 3],
}

impl Moment {
    /// The mirror image in a perfect plane at z = 0.
    pub fn image(&self) -> Self {
        Self { pos: [self.pos[0], self.pos[1], -self.pos[2]], j: [-self.j[0], -self.j[1], self.j[2]] }
    }
}

pub fn wavenumber(f: f64) -> f64 {
    2.0 * PI * f / C0
}

/// `r̂`, `θ̂` and `φ̂` at angles in degrees.
pub fn spherical_axes(theta: f64, phi: f64) -> (P3, P3, P3) {
    let (st, ct) = theta.to_radians().sin_cos();
    let (sp, cp) = phi.to_radians().sin_cos();
    ([st * cp, st * sp, ct], [ct * cp, ct * sp, -st], [-sp, cp, 0.0])
}

/// `r·E` as `(Eθ, Eφ)` in one direction, the factor `exp(-jkr)` left out.
pub fn field_at(moments: &[Moment], k: f64, theta: f64, phi: f64) -> (C64, C64) {
    let (r, t, p) = spherical_axes(theta, phi);
    let mut n = [C64::new(0.0, 0.0); 3];
    for m in moments {
        let (s, c) = (k * dot(r, m.pos)).sin_cos();
        let phase = C64::new(c, s);
        n[0] += m.j[0] * phase;
        n[1] += m.j[1] * phase;
        n[2] += m.j[2] * phase;
    }
    let a = C64::new(0.0, -k * ETA0 / (4.0 * PI));
    let along = |u: P3| n[0] * u[0] + n[1] * u[1] + n[2] * u[2];
    (a * along(t), a * along(p))
}

/// The complete electric field of the current samples at the point `at`: near,
/// intermediate and far terms of each sample's dipole field. Accurate from about
/// one mesh edge away from the metal; closer than that the samples show through.
pub fn field_near(moments: &[Moment], k: f64, at: P3) -> [C64; 3] {
    let mut e = [C64::new(0.0, 0.0); 3];
    let a = C64::new(0.0, -k * ETA0 / (4.0 * PI));
    for m in moments {
        let d = [at[0] - m.pos[0], at[1] - m.pos[1], at[2] - m.pos[2]];
        let r = dot(d, d).sqrt().max(1e-9);
        let u = [d[0] / r, d[1] / r, d[2] / r];
        let kr = k * r;
        let (s, c) = kr.sin_cos();
        let g = C64::new(c, -s) / r;
        // With exp(+jωt): 1/(jkr) = -j/(kr).
        let t = C64::new(1.0 - 1.0 / (kr * kr), -1.0 / kr);
        let l = C64::new(1.0 - 3.0 / (kr * kr), -3.0 / kr);
        let along = m.j[0] * u[0] + m.j[1] * u[1] + m.j[2] * u[2];
        for i in 0..3 {
            e[i] += a * g * (m.j[i] * t - along * l * u[i]);
        }
    }
    e
}

/// A sample of the equivalent currents on a closed surface around an antenna.
/// `j` is n×H and `m` is −n×E, both per metre; `area` is the patch's.
#[derive(Clone, Copy, Debug)]
pub struct Huygens {
    pub pos: P3,
    pub j: [C64; 3],
    pub m: [C64; 3],
    pub area: f64,
}

/// The electric field outside a closed surface from the currents on it. This is how
/// FDTD's near-field box answers for points beyond its grid. Only free space may lie
/// between the surface and the point. Good from about one sample spacing away.
pub fn field_outside(samples: &[Huygens], k: f64, at: P3) -> [C64; 3] {
    let mut e = [C64::new(0.0, 0.0); 3];
    let electric = C64::new(0.0, -k * ETA0 / (4.0 * PI));
    let magnetic = C64::new(0.0, k / (4.0 * PI));
    for h in samples {
        let d = [at[0] - h.pos[0], at[1] - h.pos[1], at[2] - h.pos[2]];
        let r = dot(d, d).sqrt().max(1e-9);
        let u = [d[0] / r, d[1] / r, d[2] / r];
        let kr = k * r;
        let (s, c) = kr.sin_cos();
        let g = C64::new(c, -s) / r * h.area;
        let t = C64::new(1.0 - 1.0 / (kr * kr), -1.0 / kr);
        let l = C64::new(1.0 - 3.0 / (kr * kr), -3.0 / kr);
        let along = h.j[0] * u[0] + h.j[1] * u[1] + h.j[2] * u[2];
        // A magnetic current's field is the electric dipole's dual: jk/4π · g · (1 + 1/jkr) · u×M.
        let dual = C64::new(1.0, -1.0 / kr);
        let cross = [h.m[2] * u[1] - h.m[1] * u[2], h.m[0] * u[2] - h.m[2] * u[0], h.m[1] * u[0] - h.m[0] * u[1]];
        for i in 0..3 {
            e[i] += g * (electric * (h.j[i] * t - along * l * u[i]) + magnetic * dual * cross[i]);
        }
    }
    e
}

/// The whole pattern on the grid. With `ground` the field below z = 0 is zero.
pub fn far_field(moments: &[Moment], f: f64, theta_step: f64, phi_step: f64, ground: bool) -> FarField {
    let k = wavenumber(f);
    let mut ff = FarField::grid(f, theta_step, phi_step);
    ff.ground = ground;
    let (thetas, phis) = (ff.theta.clone(), ff.phi.clone());
    let np = phis.len();
    let rows: Vec<(Vec<C64>, Vec<C64>)> = thetas
        .par_iter()
        .map(|&t| {
            let mut et = vec![C64::new(0.0, 0.0); np];
            let mut ep = vec![C64::new(0.0, 0.0); np];
            if !(ground && t > 90.0 + 1e-9) {
                for j in 0..np - 1 {
                    (et[j], ep[j]) = field_at(moments, k, t, phis[j]);
                }
                et[np - 1] = et[0];
                ep[np - 1] = ep[0];
            }
            (et, ep)
        })
        .collect();
    for (i, (et, ep)) in rows.into_iter().enumerate() {
        ff.e_theta[i] = et;
        ff.e_phi[i] = ep;
    }
    ff
}

/// A unit plane wave arriving from `(theta, phi)`, with its reflection in the
/// ground plane when there is one.
pub struct PlaneWave {
    k_vec: P3,
    e0: [C64; 3],
    ground: bool,
}

impl PlaneWave {
    pub fn new(f: f64, theta: f64, phi: f64, pol_theta: C64, pol_phi: C64, ground: bool) -> Self {
        let (r, t, p) = spherical_axes(theta, phi);
        let k = wavenumber(f);
        Self {
            // It travels towards -r̂.
            k_vec: [-k * r[0], -k * r[1], -k * r[2]],
            e0: [pol_theta * t[0] + pol_phi * p[0], pol_theta * t[1] + pol_phi * p[1], pol_theta * t[2] + pol_phi * p[2]],
            ground,
        }
    }

    /// The incident field at `r`.
    pub fn at(&self, r: P3) -> [C64; 3] {
        let (s, c) = dot(self.k_vec, r).sin_cos();
        let phase = C64::new(c, -s);
        let mut e = [self.e0[0] * phase, self.e0[1] * phase, self.e0[2] * phase];
        if self.ground {
            let (s, c) = (self.k_vec[0] * r[0] + self.k_vec[1] * r[1] - self.k_vec[2] * r[2]).sin_cos();
            let phase = C64::new(c, -s);
            e[0] -= self.e0[0] * phase;
            e[1] -= self.e0[1] * phase;
            e[2] += self.e0[2] * phase;
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_current_element_gives_the_textbook_field() {
        // r·E_θ = jηkIL·sinθ/(4π) with exp(-jkr); we use -jk because of the sign of θ̂·ẑ.
        let f = 1e9;
        let k = wavenumber(f);
        let m = [Moment { pos: [0.0; 3], j: [C64::new(0.0, 0.0), C64::new(0.0, 0.0), C64::new(1e-3, 0.0)] }];
        let (et, ep) = field_at(&m, k, 90.0, 0.0);
        let want = C64::new(0.0, ETA0 * k * 1e-3 / (4.0 * PI));
        assert!((et - want).norm() < 1e-9 * want.norm(), "{et} vs {want}");
        assert!(ep.norm() < 1e-12);
        let (et, _) = field_at(&m, k, 30.0, 45.0);
        assert!((et.norm() / want.norm() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn near_field_becomes_the_far_field_far_away() {
        let f = 1e9;
        let k = wavenumber(f);
        let m = [Moment { pos: [0.01, -0.02, 0.03], j: [C64::new(1e-3, 0.0), C64::new(0.0, 0.0), C64::new(0.0, 2e-3)] }];
        let (theta, phi) = (70.0, 40.0);
        let (r, t, p) = spherical_axes(theta, phi);
        let dist = 3000.0;
        let e = field_near(&m, k, [r[0] * dist, r[1] * dist, r[2] * dist]);
        let (et, ep) = field_at(&m, k, theta, phi);
        let (s, c) = (k * dist).sin_cos();
        let phase = C64::new(c, -s) / dist;
        let got_t = e[0] * t[0] + e[1] * t[1] + e[2] * t[2];
        let got_p = e[0] * p[0] + e[1] * p[1] + e[2] * p[2];
        assert!((got_t - et * phase).norm() < 2e-3 * (et * phase).norm(), "{got_t} vs {}", et * phase);
        assert!((got_p - ep * phase).norm() < 2e-3 * (et * phase).norm(), "{got_p} vs {}", ep * phase);
    }

    #[test]
    fn a_closed_surface_of_currents_reproduces_the_dipole_inside_it() {
        // A short dipole's exact E and H on a box around it, then the field beyond the
        // box from those currents alone, against the dipole's own field there.
        let k = wavenumber(915e6);
        let lambda = 2.0 * PI / k;
        let source = [Moment { pos: [0.01, -0.02, 0.03], j: [C64::new(0.2, 0.1), C64::new(0.0, 0.0), C64::new(1.0, 0.0)] }];
        let h_field = |at: P3| -> [C64; 3] {
            let m = &source[0];
            let d = [at[0] - m.pos[0], at[1] - m.pos[1], at[2] - m.pos[2]];
            let r = dot(d, d).sqrt();
            let u = [d[0] / r, d[1] / r, d[2] / r];
            let kr = k * r;
            let f = C64::new(0.0, k / (4.0 * PI)) * C64::new(kr.cos(), -kr.sin()) / r * C64::new(1.0, -1.0 / kr);
            [f * (m.j[1] * u[2] - m.j[2] * u[1]), f * (m.j[2] * u[0] - m.j[0] * u[2]), f * (m.j[0] * u[1] - m.j[1] * u[0])]
        };
        let half = 0.3;
        let n = (2.0 * half / (lambda / 20.0)).ceil() as usize;
        let h = 2.0 * half / n as f64;
        let mut samples = Vec::new();
        for axis in 0..3 {
            for sign in [-1.0, 1.0] {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                for i in 0..n {
                    for j in 0..n {
                        let mut p = [0.0; 3];
                        p[axis] = sign * half;
                        p[u] = -half + (i as f64 + 0.5) * h;
                        p[v] = -half + (j as f64 + 0.5) * h;
                        let (e, hv) = (field_near(&source, k, p), h_field(p));
                        let mut nrm = [0.0; 3];
                        nrm[axis] = sign;
                        let cross = |a: P3, b: [C64; 3]| [b[2] * a[1] - b[1] * a[2], b[0] * a[2] - b[2] * a[0], b[1] * a[0] - b[0] * a[1]];
                        let j_s = cross(nrm, hv);
                        let m_s = cross(nrm, e).map(|c| -c);
                        samples.push(Huygens { pos: p, j: j_s, m: m_s, area: h * h });
                    }
                }
            }
        }
        for at in [[0.9, 0.2, 0.1], [-0.1, 1.3, 0.7], [0.45, -0.35, -0.5]] {
            let got = field_outside(&samples, k, at);
            let want = field_near(&source, k, at);
            let size = (want[0].norm_sqr() + want[1].norm_sqr() + want[2].norm_sqr()).sqrt();
            let diff = ((got[0] - want[0]).norm_sqr() + (got[1] - want[1]).norm_sqr() + (got[2] - want[2]).norm_sqr()).sqrt();
            assert!(diff / size < 0.02, "at {at:?}: {:.1} % off", 100.0 * diff / size);
        }
        // Inside the surface the same currents cancel: that's what makes them equivalent.
        let inner = field_outside(&samples, k, [0.15, 0.1, -0.1]);
        let want = field_near(&source, k, [0.15, 0.1, -0.1]);
        let (a, b) = ((inner[0].norm_sqr() + inner[1].norm_sqr() + inner[2].norm_sqr()).sqrt(), (want[0].norm_sqr() + want[1].norm_sqr() + want[2].norm_sqr()).sqrt());
        assert!(a < 0.05 * b, "{a} vs {b}");
    }

    #[test]
    fn near_field_has_the_static_dipole_form_up_close() {
        // kR ≪ 1: E = (1/(jω·4πεR³)) · (3R̂(R̂·p) − p), so on the axis it is twice the
        // field at the same distance off to the side, with the opposite sign.
        let k = wavenumber(1e8);
        let m = [Moment { pos: [0.0; 3], j: [C64::new(0.0, 0.0), C64::new(0.0, 0.0), C64::new(1e-3, 0.0)] }];
        let r = 0.01;
        let axial = field_near(&m, k, [0.0, 0.0, r])[2];
        let side = field_near(&m, k, [r, 0.0, 0.0])[2];
        assert!((axial / side + C64::new(2.0, 0.0)).norm() < 0.01, "{}", axial / side);
        let want = 1e-3 * ETA0 / (4.0 * PI * k * r * r * r);
        assert!((side.norm() - want).abs() < 0.01 * want, "{} vs {want}", side.norm());
    }

    #[test]
    fn a_plane_wave_vanishes_tangentially_on_the_ground_plane() {
        let w = PlaneWave::new(1e9, 40.0, 25.0, C64::new(1.0, 0.0), C64::new(0.0, 0.5), true);
        let e = w.at([0.13, -0.27, 0.0]);
        assert!(e[0].norm() < 1e-12 && e[1].norm() < 1e-12, "{e:?}");
        assert!(e[2].norm() > 0.1);
    }
}
