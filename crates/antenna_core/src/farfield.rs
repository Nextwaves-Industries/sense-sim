//! Far-field results and everything derived from them: directivity, gain,
//! realised gain, efficiencies, polarisation (θ/φ, RHCP/LHCP, Ludwig-3, axial
//! ratio), pattern cuts, beam width, side lobes, front-to-back, and the export
//! formats other tools read. Every solver produces a [`FarField`].

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::{C64, ETA0, db10};

/// `r·E` in volts on a regular θ/φ grid. θ covers 0..=180°, φ covers 0..=360°
/// (the last column repeats the first), both evenly spaced.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FarField {
    pub f: f64,
    pub theta: Vec<f64>,
    pub phi: Vec<f64>,
    pub e_theta: Vec<Vec<C64>>,
    pub e_phi: Vec<Vec<C64>>,
    /// Power into the antenna terminals (W), same normalisation as the field.
    pub p_accepted: f64,
    /// Power the source offered (W): `p_accepted / (1 − |S11|²)`.
    pub p_incident: f64,
    /// An infinite ground plane at z = 0: only the upper hemisphere radiates.
    pub ground: bool,
}

/// What a pattern plot or the 3D view shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Quantity {
    Directivity,
    Gain,
    #[default]
    RealizedGain,
}

impl Quantity {
    pub const ALL: [Quantity; 3] = [Quantity::RealizedGain, Quantity::Gain, Quantity::Directivity];
    pub fn label(self) -> &'static str {
        match self {
            Quantity::Directivity => "Directivity",
            Quantity::Gain => "Gain (IEEE)",
            Quantity::RealizedGain => "Realized gain",
        }
    }
}

/// Which field component a pattern shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Pol {
    #[default]
    Total,
    Theta,
    Phi,
    Rhcp,
    Lhcp,
    /// Ludwig-3 co-polar for an x-polarised reference.
    Co,
    /// Ludwig-3 cross-polar for an x-polarised reference.
    Cross,
}

impl Pol {
    pub const ALL: [Pol; 7] = [Pol::Total, Pol::Theta, Pol::Phi, Pol::Rhcp, Pol::Lhcp, Pol::Co, Pol::Cross];
    pub fn label(self) -> &'static str {
        match self {
            Pol::Total => "Total",
            Pol::Theta => "Eθ",
            Pol::Phi => "Eφ",
            Pol::Rhcp => "RHCP",
            Pol::Lhcp => "LHCP",
            Pol::Co => "Co-pol (Ludwig 3, x)",
            Pol::Cross => "Cross-pol (Ludwig 3, x)",
        }
    }
}

/// A principal plane of a pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cut {
    /// θ sweeps −180..180° in the plane at this φ.
    Phi(f64),
    /// φ sweeps 0..360° on the cone at this θ.
    Theta(f64),
}

/// Single-number summary of a pattern.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub f: f64,
    pub peak_directivity_dbi: f64,
    pub peak_gain_dbi: f64,
    pub peak_realized_dbi: f64,
    pub peak_theta: f64,
    pub peak_phi: f64,
    pub p_rad: f64,
    pub rad_efficiency: f64,
    pub total_efficiency: f64,
    /// Half-power beam width in the φ-plane through the peak, degrees.
    pub hpbw_e: Option<f64>,
    /// Half-power beam width in the plane perpendicular to it, degrees.
    pub hpbw_h: Option<f64>,
    pub front_to_back_db: Option<f64>,
    /// Side-lobe level relative to the peak, in the φ-plane through the peak (dB, negative).
    pub side_lobe_db: Option<f64>,
    pub axial_ratio_db: f64,
    /// `true` when right-hand circular dominates at the peak.
    pub rhcp_at_peak: bool,
}

impl FarField {
    /// Empty grid of about `theta_step`/`phi_step` degrees, ready to be filled. A step
    /// snaps to the nearest that closes θ at 180° and φ at 360° and lands on θ = 90° and
    /// φ = 90°, so the principal planes are real samples; 7° becomes 6.92°.
    pub fn grid(f: f64, theta_step: f64, phi_step: f64) -> Self {
        let (half, quarter) = ((90.0 / theta_step).round().max(1.0) as usize, (90.0 / phi_step).round().max(1.0) as usize);
        let (nt, np) = (2 * half + 1, 4 * quarter + 1);
        Self {
            f,
            theta: (0..nt).map(|i| 90.0 * i as f64 / half as f64).collect(),
            phi: (0..np).map(|i| 90.0 * i as f64 / quarter as f64).collect(),
            e_theta: vec![vec![C64::new(0.0, 0.0); np]; nt],
            e_phi: vec![vec![C64::new(0.0, 0.0); np]; nt],
            p_accepted: 0.0,
            p_incident: 0.0,
            ground: false,
        }
    }

    pub fn theta_step(&self) -> f64 {
        self.theta.get(1).map_or(1.0, |t| t - self.theta[0])
    }
    pub fn phi_step(&self) -> f64 {
        self.phi.get(1).map_or(1.0, |p| p - self.phi[0])
    }

    /// Radiation intensity (W/sr).
    pub fn intensity(&self, ti: usize, pi: usize) -> f64 {
        (self.e_theta[ti][pi].norm_sqr() + self.e_phi[ti][pi].norm_sqr()) / (2.0 * ETA0)
    }

    /// Total radiated power (W), trapezoid in θ and φ.
    pub fn p_rad(&self) -> f64 {
        let (dt, dp) = (self.theta_step().to_radians(), self.phi_step().to_radians());
        let mut sum = 0.0;
        let last_t = self.theta.len() - 1;
        for (ti, th) in self.theta.iter().enumerate() {
            let mut w = th.to_radians().sin() * dt * dp;
            if ti == 0 || ti == last_t {
                // The pole has no area in the sine rule; keep the small cap it stands for.
                w = (1.0 - (dt * 0.5).cos()) * dp;
            }
            // Over a ground plane only the upper half of the horizon's ring radiates.
            if self.ground && (*th - 90.0).abs() < 1e-9 {
                w *= 0.5;
            }
            // The φ = 360° column repeats φ = 0°.
            for pi in 0..self.phi.len() - 1 {
                sum += self.intensity(ti, pi) * w;
            }
        }
        sum
    }

    /// Radiated / accepted power (1 for a lossless antenna). NaN when accepted power is
    /// negative or NaN, as from a negative feed resistance; 1 when none was recorded.
    pub fn rad_efficiency(&self) -> f64 {
        if self.p_accepted > 0.0 {
            self.p_rad() / self.p_accepted
        } else if self.p_accepted == 0.0 {
            1.0
        } else {
            f64::NAN
        }
    }

    /// Radiated / incident power (matching loss included).
    pub fn total_efficiency(&self) -> f64 {
        if self.p_incident > 0.0 { self.p_rad() / self.p_incident } else { self.rad_efficiency() }
    }

    /// Components in one direction: `(Eθ, Eφ)`.
    pub fn at(&self, ti: usize, pi: usize) -> (C64, C64) {
        (self.e_theta[ti][pi], self.e_phi[ti][pi])
    }

    /// Right- and left-hand circular components of `r·E` (IEEE sense with
    /// `exp(+jωt)`, looking along the outgoing wave).
    pub fn circular(&self, ti: usize, pi: usize) -> (C64, C64) {
        let (et, ep) = self.at(ti, pi);
        let j = C64::new(0.0, 1.0);
        let s = std::f64::consts::FRAC_1_SQRT_2;
        // Outgoing along r̂, (θ̂, φ̂, r̂) is right-handed. RHCP: E ∝ θ̂ − jφ̂.
        ((et + j * ep) * s, (et - j * ep) * s)
    }

    /// Ludwig-3 co- and cross-polar components for an x-polarised reference.
    pub fn ludwig3(&self, ti: usize, pi: usize) -> (C64, C64) {
        let (et, ep) = self.at(ti, pi);
        let (sp, cp) = self.phi[pi].to_radians().sin_cos();
        (et * cp - ep * sp, et * sp + ep * cp)
    }

    /// Axial ratio (dB, 0 = circular, large = linear).
    pub fn axial_ratio_db(&self, ti: usize, pi: usize) -> f64 {
        let (r, l) = self.circular(ti, pi);
        let (r, l) = (r.norm(), l.norm());
        let (a, b) = (r + l, (r - l).abs());
        (20.0 * (a / b.max(1e-12)).log10()).min(60.0)
    }

    /// Normalisation (W) that turns 4π·U into the chosen quantity.
    fn reference_power(&self, q: Quantity) -> f64 {
        let p = match q {
            Quantity::Directivity => self.p_rad(),
            Quantity::Gain => {
                if self.p_accepted > 0.0 {
                    self.p_accepted
                } else if self.p_accepted == 0.0 {
                    self.p_rad()
                } else {
                    // Negative or NaN: gain has no meaning here, and must not read as directivity.
                    f64::NAN
                }
            }
            Quantity::RealizedGain => {
                if self.p_incident > 0.0 {
                    self.p_incident
                } else if self.p_accepted > 0.0 {
                    self.p_accepted
                } else if self.p_incident == 0.0 && self.p_accepted == 0.0 {
                    self.p_rad()
                } else {
                    f64::NAN
                }
            }
        };
        if p.is_nan() { p } else { p.max(1e-300) }
    }

    /// Pattern value in dBi (dBiC for circular components).
    pub fn value_db(&self, ti: usize, pi: usize, q: Quantity, pol: Pol) -> f64 {
        let reference = self.reference_power(q);
        self.value_db_ref(ti, pi, reference, pol)
    }

    fn value_db_ref(&self, ti: usize, pi: usize, reference: f64, pol: Pol) -> f64 {
        let (et, ep) = self.at(ti, pi);
        let e2 = match pol {
            Pol::Total => et.norm_sqr() + ep.norm_sqr(),
            Pol::Theta => et.norm_sqr(),
            Pol::Phi => ep.norm_sqr(),
            Pol::Rhcp => self.circular(ti, pi).0.norm_sqr(),
            Pol::Lhcp => self.circular(ti, pi).1.norm_sqr(),
            Pol::Co => self.ludwig3(ti, pi).0.norm_sqr(),
            Pol::Cross => self.ludwig3(ti, pi).1.norm_sqr(),
        };
        db10(4.0 * PI * e2 / (2.0 * ETA0) / reference)
    }

    /// The whole grid in dB, `[θ][φ]`.
    pub fn grid_db(&self, q: Quantity, pol: Pol) -> Vec<Vec<f64>> {
        let reference = self.reference_power(q);
        (0..self.theta.len()).map(|ti| (0..self.phi.len()).map(|pi| self.value_db_ref(ti, pi, reference, pol)).collect()).collect()
    }

    /// Grid indices of the strongest direction.
    pub fn peak_index(&self) -> (usize, usize) {
        let mut best = (0, 0);
        let mut v = f64::MIN;
        for ti in 0..self.theta.len() {
            for pi in 0..self.phi.len() {
                let u = self.intensity(ti, pi);
                if u > v {
                    v = u;
                    best = (ti, pi);
                }
            }
        }
        best
    }

    fn nearest_phi(&self, phi: f64) -> usize {
        let p = phi.rem_euclid(360.0);
        ((p / self.phi_step()).round() as usize).min(self.phi.len() - 1)
    }

    fn nearest_theta(&self, theta: f64) -> usize {
        ((theta.clamp(0.0, 180.0) / self.theta_step()).round() as usize).min(self.theta.len() - 1)
    }

    /// A cut through the pattern as `(angle°, dB)`. A φ cut sweeps θ from −180°
    /// to 180°: negative θ reads the half-plane at φ + 180°.
    pub fn cut(&self, cut: Cut, q: Quantity, pol: Pol) -> Vec<(f64, f64)> {
        let reference = self.reference_power(q);
        match cut {
            Cut::Phi(phi) => {
                let (a, b) = (self.nearest_phi(phi), self.nearest_phi(phi + 180.0));
                let mut v: Vec<(f64, f64)> = Vec::with_capacity(2 * self.theta.len());
                for ti in (1..self.theta.len()).rev() {
                    v.push((-self.theta[ti], self.value_db_ref(ti, b, reference, pol)));
                }
                for ti in 0..self.theta.len() {
                    v.push((self.theta[ti], self.value_db_ref(ti, a, reference, pol)));
                }
                v
            }
            Cut::Theta(theta) => {
                let ti = self.nearest_theta(theta);
                (0..self.phi.len()).map(|pi| (self.phi[pi], self.value_db_ref(ti, pi, reference, pol))).collect()
            }
        }
    }

    /// Fractional-angle crossing of `level` to either side of index `k` of a
    /// `(angle, dB)` curve. `wrap` treats the curve as periodic.
    fn width_at(curve: &[(f64, f64)], k: usize, level: f64, wrap: bool) -> Option<f64> {
        let n = curve.len();
        let crossing = |dir: i64| -> Option<f64> {
            let mut i = k as i64;
            let mut steps = 0;
            loop {
                let j = i + dir;
                let (ia, ja) = if wrap {
                    (i.rem_euclid(n as i64) as usize, j.rem_euclid(n as i64) as usize)
                } else {
                    if j < 0 || j >= n as i64 {
                        return None;
                    }
                    (i as usize, j as usize)
                };
                steps += 1;
                if steps > n {
                    return None;
                }
                let (va, vb) = (curve[ia].1, curve[ja].1);
                if vb < level {
                    let t = (va - level) / (va - vb).max(1e-12);
                    let step = (i as f64 + t * dir as f64) - k as f64;
                    return Some(step.abs() * (curve[1].0 - curve[0].0).abs());
                }
                i = j;
            }
        };
        Some(crossing(-1)? + crossing(1)?)
    }

    /// The angle between two directions on the cone at `theta0`, `span` degrees of φ apart.
    /// Past 180° the great-circle angle would shrink again, so it carries on along the cone.
    fn cone_width(theta0: f64, span: f64) -> f64 {
        let s = theta0.to_radians().sin();
        if span <= 180.0 {
            2.0 * (s * (0.5 * span).to_radians().sin()).min(1.0).asin().to_degrees()
        } else {
            2.0 * theta0.min(180.0 - theta0) + (span - 180.0) * s
        }
    }

    /// Everything a datasheet states.
    pub fn metrics(&self) -> Metrics {
        let (ti, pi) = self.peak_index();
        let (theta0, phi0) = (self.theta[ti], self.phi[pi]);
        let d = self.value_db(ti, pi, Quantity::Directivity, Pol::Total);
        let g = self.value_db(ti, pi, Quantity::Gain, Pol::Total);
        let rg = self.value_db(ti, pi, Quantity::RealizedGain, Pol::Total);

        // The plane through the beam: at the peak's φ, except for a beam on the
        // z axis, whose φ is arbitrary: then the planes φ = 0° and 90°.
        let on_axis = theta0 < 1e-6 || theta0 > 180.0 - 1e-6;
        let phi_e = if on_axis { 0.0 } else { phi0 };
        let q = Quantity::Directivity;
        let e_cut = self.cut(Cut::Phi(phi_e), q, Pol::Total);
        let k_e = e_cut.iter().enumerate().max_by(|a, b| a.1.1.total_cmp(&b.1.1)).map_or(0, |x| x.0);
        let hpbw_e = Self::width_at(&e_cut, k_e, e_cut[k_e].1 - 3.0, false);
        // Second plane: φ ⟂ for an axial beam; the φ cone for a broadside beam.
        let hpbw_h = if on_axis {
            let c = self.cut(Cut::Phi(phi_e + 90.0), q, Pol::Total);
            let k = c.iter().enumerate().max_by(|a, b| a.1.1.total_cmp(&b.1.1)).map_or(0, |x| x.0);
            Self::width_at(&c, k, c[k].1 - 3.0, false)
        } else {
            let c = self.cut(Cut::Theta(theta0), q, Pol::Total);
            let c = &c[..c.len() - 1];
            let k = c.iter().enumerate().max_by(|a, b| a.1.1.total_cmp(&b.1.1)).map_or(0, |x| x.0);
            Self::width_at(c, k, c[k].1 - 3.0, true).map(|span| Self::cone_width(theta0, span))
        };

        // Front to back: the strongest direction against its exact opposite.
        let back = self.value_db(self.nearest_theta(180.0 - theta0), self.nearest_phi(phi0 + 180.0), q, Pol::Total);
        let front_to_back_db = if self.ground { None } else { Some(d - back) };

        // Side lobes: the highest local maximum outside the main lobe of the E cut.
        let peak = e_cut[k_e].1;
        let n = e_cut.len();
        let mut lo = k_e;
        while lo > 0 && e_cut[lo - 1].1 <= e_cut[lo].1 {
            lo -= 1;
        }
        let mut hi = k_e;
        while hi + 1 < n && e_cut[hi + 1].1 <= e_cut[hi].1 {
            hi += 1;
        }
        let mut sll: Option<f64> = None;
        for i in 1..n.saturating_sub(1) {
            if (i < lo || i > hi) && e_cut[i].1 >= e_cut[i - 1].1 && e_cut[i].1 >= e_cut[i + 1].1 {
                let rel = e_cut[i].1 - peak;
                if sll.is_none_or(|s| rel > s) && rel < -0.5 {
                    sll = Some(rel);
                }
            }
        }

        let (r, l) = self.circular(ti, pi);
        Metrics {
            f: self.f,
            peak_directivity_dbi: d,
            peak_gain_dbi: g,
            peak_realized_dbi: rg,
            peak_theta: theta0,
            peak_phi: phi0,
            p_rad: self.p_rad(),
            rad_efficiency: self.rad_efficiency(),
            total_efficiency: self.total_efficiency(),
            hpbw_e,
            hpbw_h,
            front_to_back_db,
            side_lobe_db: sll,
            axial_ratio_db: self.axial_ratio_db(ti, pi),
            rhcp_at_peak: r.norm() >= l.norm(),
        }
    }

    /// CSV: `theta,phi,re_Etheta,im_Etheta,re_Ephi,im_Ephi,realized_dbi`.
    pub fn to_csv(&self) -> String {
        let mut s = String::from("theta_deg,phi_deg,re_Etheta_V,im_Etheta_V,re_Ephi_V,im_Ephi_V,realized_gain_dbi\n");
        for ti in 0..self.theta.len() {
            for pi in 0..self.phi.len() {
                let (a, b) = self.at(ti, pi);
                s.push_str(&format!(
                    "{},{},{:.8e},{:.8e},{:.8e},{:.8e},{:.3}\n",
                    self.theta[ti],
                    self.phi[pi],
                    a.re,
                    a.im,
                    b.re,
                    b.im,
                    self.value_db(ti, pi, Quantity::RealizedGain, Pol::Total)
                ));
            }
        }
        s
    }

    /// Far-field in the `.ffe` table layout FEKO writes (one frequency).
    pub fn to_ffe(&self) -> String {
        let mut s = String::new();
        s.push_str("##File Type: Far field\n##File Format: 8\n##Source: Sense Sim\n");
        s.push_str(&format!("#Request Name: FarField\n#Frequency: {:e}\n#Coordinate System: Spherical\n", self.f));
        s.push_str(&format!("#No. of Theta Samples: {}\n#No. of Phi Samples: {}\n", self.theta.len(), self.phi.len()));
        s.push_str("#Result Type: Directivity\n#No. of Header Lines: 1\n");
        s.push_str("#  \"Theta\" \"Phi\" \"Re(Etheta)\" \"Im(Etheta)\" \"Re(Ephi)\" \"Im(Ephi)\" \"Directivity(Theta)\" \"Directivity(Phi)\" \"Directivity(Total)\"\n");
        let prad = self.p_rad().max(1e-300);
        for pi in 0..self.phi.len() {
            for ti in 0..self.theta.len() {
                let (a, b) = self.at(ti, pi);
                let dt = db10(4.0 * PI * a.norm_sqr() / (2.0 * ETA0) / prad);
                let dp = db10(4.0 * PI * b.norm_sqr() / (2.0 * ETA0) / prad);
                let d = db10(4.0 * PI * (a.norm_sqr() + b.norm_sqr()) / (2.0 * ETA0) / prad);
                s.push_str(&format!(
                    "{:.2} {:.2} {:.8e} {:.8e} {:.8e} {:.8e} {:.4} {:.4} {:.4}\n",
                    self.theta[ti], self.phi[pi], a.re, a.im, b.re, b.im, dt, dp, d
                ));
            }
        }
        s
    }

    /// Far-field in the `.ffd` layout HFSS and CST import.
    pub fn to_ffd(&self) -> String {
        let mut s = String::new();
        let (nt, np) = (self.theta.len(), self.phi.len());
        s.push_str(&format!("{} {} {}\n", self.theta[0], self.theta[nt - 1], nt));
        s.push_str(&format!("{} {} {}\n", self.phi[0], self.phi[np - 1], np));
        s.push_str(&format!("Freq {}\n", self.f));
        for ti in 0..nt {
            for pi in 0..np {
                let (a, b) = self.at(ti, pi);
                s.push_str(&format!("{:.8e} {:.8e} {:.8e} {:.8e}\n", a.re, a.im, b.re, b.im));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short z-directed current element: E_θ ∝ sin θ, 1.76 dBi.
    fn hertzian(p_acc: f64) -> FarField {
        let mut ff = FarField::grid(1e9, 2.0, 2.0);
        for ti in 0..ff.theta.len() {
            let s = ff.theta[ti].to_radians().sin();
            for pi in 0..ff.phi.len() {
                ff.e_theta[ti][pi] = C64::new(s, 0.0);
            }
        }
        // ∫sin²θ dΩ = 8π/3, so U₀ = 1/(2η) gives P = 8π/(3·2η).
        ff.p_accepted = p_acc;
        ff.p_incident = p_acc;
        ff
    }

    #[test]
    fn short_dipole_directivity_and_power() {
        let mut ff = hertzian(0.0);
        let want = 8.0 * PI / 3.0 / (2.0 * ETA0);
        assert!((ff.p_rad() / want - 1.0).abs() < 0.002, "{} vs {want}", ff.p_rad());
        ff.p_accepted = want;
        ff.p_incident = want;
        let m = ff.metrics();
        assert!((m.peak_directivity_dbi - 1.76).abs() < 0.02, "{}", m.peak_directivity_dbi);
        assert!((m.peak_theta - 90.0).abs() < 1e-9);
        assert!((m.hpbw_e.unwrap() - 90.0).abs() < 1.5, "{:?}", m.hpbw_e);
        assert!((m.rad_efficiency - 1.0).abs() < 0.002);
    }

    /// A short monopole on ground: half the free-space power, 3 dB more directivity,
    /// whichever grid it is sampled on.
    #[test]
    fn ground_plane_power_is_the_upper_hemisphere() {
        for step in [1.0, 3.0, 5.0] {
            let mut ff = FarField::grid(1e9, step, step);
            ff.ground = true;
            for ti in 0..ff.theta.len() {
                if ff.theta[ti] > 90.0 + 1e-9 {
                    continue;
                }
                let s = ff.theta[ti].to_radians().sin();
                for pi in 0..ff.phi.len() {
                    ff.e_theta[ti][pi] = C64::new(s, 0.0);
                }
            }
            let want = 4.0 * PI / 3.0 / (2.0 * ETA0);
            assert!((ff.p_rad() / want - 1.0).abs() < 0.002, "step {step}: {} vs {want}", ff.p_rad());
            let d = ff.metrics().peak_directivity_dbi;
            assert!((d - 4.77).abs() < 0.02, "step {step}: {d}");
        }
    }

    #[test]
    fn gain_follows_efficiency_and_mismatch() {
        let want = 8.0 * PI / 3.0 / (2.0 * ETA0);
        let mut ff = hertzian(want * 2.0); // 50% radiation efficiency
        ff.p_incident = want * 4.0; // 25% total
        let m = ff.metrics();
        assert!((m.peak_directivity_dbi - m.peak_gain_dbi - 3.01).abs() < 0.03);
        assert!((m.peak_gain_dbi - m.peak_realized_dbi - 3.01).abs() < 0.03);
        assert!((m.total_efficiency - 0.25).abs() < 0.002);
    }

    #[test]
    fn circular_polarisation_is_decomposed() {
        let mut ff = FarField::grid(1e9, 5.0, 5.0);
        let s = std::f64::consts::FRAC_1_SQRT_2;
        for ti in 0..ff.theta.len() {
            for pi in 0..ff.phi.len() {
                // E ∝ θ̂ − jφ̂: RHCP.
                ff.e_theta[ti][pi] = C64::new(s, 0.0);
                ff.e_phi[ti][pi] = C64::new(0.0, -s);
            }
        }
        let (r, l) = ff.circular(5, 5);
        assert!((r.norm() - 1.0).abs() < 1e-12, "{r}");
        assert!(l.norm() < 1e-12, "{l}");
        assert!(ff.axial_ratio_db(5, 5) < 0.01);
        assert!(ff.metrics().rhcp_at_peak);
    }

    #[test]
    fn linear_polarisation_has_large_axial_ratio() {
        let ff = hertzian(1.0);
        assert!(ff.axial_ratio_db(45, 0) > 40.0);
    }

    /// A pencil beam of the same angular width in every plane, at θ0 off the axis.
    fn pencil(theta0: f64, width: f64) -> FarField {
        let mut ff = FarField::grid(1e9, 1.0, 1.0);
        let axis = [theta0.to_radians().sin(), 0.0, theta0.to_radians().cos()];
        for ti in 0..ff.theta.len() {
            let (st, ct) = ff.theta[ti].to_radians().sin_cos();
            for pi in 0..ff.phi.len() {
                let cp = ff.phi[pi].to_radians().cos();
                let cos = (axis[0] * st * cp + axis[2] * ct).clamp(-1.0, 1.0);
                let off = cos.acos().to_degrees();
                ff.e_theta[ti][pi] = C64::new((-(2f64.ln()) * (off / width).powi(2) * 2.0).exp(), 0.0);
            }
        }
        ff.p_accepted = 1.0;
        ff.p_incident = 1.0;
        ff
    }

    #[test]
    fn cross_plane_beam_width_is_an_angle_not_a_phi_span() {
        // At θ0 = 30° a 20° beam spans about 40° of φ, and it is still a 20° beam.
        for theta0 in [90.0, 60.0, 30.0] {
            let m = pencil(theta0, 20.0).metrics();
            let (e, h) = (m.hpbw_e.unwrap(), m.hpbw_h.unwrap());
            println!("θ0 = {theta0}: main {e:.2}°, cross {h:.2}°");
            assert!((e - 20.0).abs() < 1.0, "main plane at θ0 = {theta0}: {e}");
            assert!((h - 20.0).abs() < 1.0, "cross plane at θ0 = {theta0}: {h}");
        }
    }

    #[test]
    fn ffe_header_names_what_its_columns_hold() {
        let ffe = hertzian(1.0).to_ffe();
        assert!(ffe.contains("#Result Type: Directivity"), "{}", &ffe[..300]);
        assert!(!ffe.contains("#Result Type: Gain"));
    }

    #[test]
    fn a_step_that_does_not_divide_the_circle_still_closes_it() {
        for step in [7.0, 4.0, 11.0, 2.5] {
            let ff = FarField::grid(1e9, step, step);
            let (t, p) = (ff.theta[ff.theta.len() - 1], ff.phi[ff.phi.len() - 1]);
            assert!((t - 180.0).abs() < 1e-9, "step {step}: θ ends at {t}");
            assert!((p - 360.0).abs() < 1e-9, "step {step}: φ ends at {p}");
            // The principal planes are real samples, so a cut at 90° reads 90°.
            assert!(ff.theta.iter().any(|x| (x - 90.0).abs() < 1e-9), "step {step}: no θ = 90°");
            assert!(ff.phi.iter().any(|x| (x - 90.0).abs() < 1e-9), "step {step}: no φ = 90°");
        }
        // Steps that already divide stay exactly as asked.
        for step in [1.0, 2.0, 3.0, 5.0, 15.0] {
            assert!((FarField::grid(1e9, step, step).theta_step() - step).abs() < 1e-12);
        }
    }

    #[test]
    fn power_that_was_not_accepted_is_not_a_full_efficiency() {
        let mut ff = hertzian(-0.01);
        ff.p_incident = 1.0;
        assert!(ff.rad_efficiency().is_nan(), "{}", ff.rad_efficiency());
        assert!(ff.metrics().peak_gain_dbi.is_nan(), "gain must not fall back to directivity");
        ff.p_accepted = f64::NAN;
        assert!(ff.rad_efficiency().is_nan(), "{}", ff.rad_efficiency());
        // Nothing recorded is not the same as a bad value: a bare pattern keeps working.
        assert_eq!(hertzian(0.0).rad_efficiency(), 1.0);
    }

    #[test]
    fn exports_have_the_right_size() {
        let ff = hertzian(1.0);
        let n = ff.theta.len() * ff.phi.len();
        assert_eq!(ff.to_csv().lines().count(), n + 1);
        assert_eq!(ff.to_ffd().lines().count(), n + 3);
        assert!(ff.to_ffe().lines().count() > n);
    }
}
