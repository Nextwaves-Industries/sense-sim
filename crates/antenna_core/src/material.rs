//! Material library: ideal and real conductors, PCB laminates, plastics.

use serde::{Deserialize, Serialize};

use crate::{C64, EPS0, MU0};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatKind {
    /// Perfect electric conductor.
    Pec,
    /// Real metal, conductivity in S/m.
    Metal { sigma: f64 },
    /// Dielectric with loss tangent at `f_tan` (Hz), relative permeability 1 unless set.
    Dielectric { eps_r: f64, tan_d: f64, f_tan: f64, mu_r: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub kind: MatKind,
}

impl Material {
    pub fn pec() -> Self {
        Self { name: "PEC".into(), kind: MatKind::Pec }
    }
    pub fn metal(name: &str, sigma: f64) -> Self {
        Self { name: name.into(), kind: MatKind::Metal { sigma } }
    }
    pub fn dielectric(name: &str, eps_r: f64, tan_d: f64) -> Self {
        Self { name: name.into(), kind: MatKind::Dielectric { eps_r, tan_d, f_tan: 10e9, mu_r: 1.0 } }
    }

    pub fn is_conductor(&self) -> bool {
        matches!(self.kind, MatKind::Pec | MatKind::Metal { .. })
    }

    /// Relative permittivity (1 for conductors).
    pub fn eps_r(&self) -> f64 {
        match self.kind {
            MatKind::Dielectric { eps_r, .. } => eps_r,
            _ => 1.0,
        }
    }

    /// Conductivity (S/m) an FDTD solver uses for a dielectric's loss tangent:
    /// exact at `f`, as ω·ε₀·εr·tanδ.
    pub fn dielectric_kappa(&self, f: f64) -> f64 {
        match self.kind {
            MatKind::Dielectric { .. } if self.name == "Water" => {
                // Debye at 25 °C: εs 78.4, ε∞ 5.2, τ 8.27 ps. No ionic term, so salt water is worse.
                let wt = 2.0 * std::f64::consts::PI * f * 8.27e-12;
                let span = 78.4 - 5.2;
                let lossy = span * wt / (1.0 + wt * wt);
                2.0 * std::f64::consts::PI * f * EPS0 * lossy
            }
            MatKind::Dielectric { eps_r, tan_d, .. } => 2.0 * std::f64::consts::PI * f * EPS0 * eps_r * tan_d,
            _ => 0.0,
        }
    }

    /// `ε′ - jε″` over ε₀ at `f`, with the same loss FDTD uses. 1 for conductors.
    pub fn complex_eps(&self, f: f64) -> C64 {
        C64::new(self.eps_r(), -self.dielectric_kappa(f) / (2.0 * std::f64::consts::PI * f * EPS0))
    }

    /// Skin depth at `f` (m); infinite for PEC or dielectrics.
    pub fn skin_depth(&self, f: f64) -> f64 {
        match self.kind {
            MatKind::Metal { sigma } => (1.0 / (std::f64::consts::PI * f * MU0 * sigma)).sqrt(),
            _ => f64::INFINITY,
        }
    }

    /// Surface impedance of a good conductor at `f`: (1+j)·sqrt(ωμ/2σ). Zero for PEC.
    pub fn surface_impedance(&self, f: f64) -> C64 {
        match self.kind {
            MatKind::Metal { sigma } => {
                let r = (std::f64::consts::PI * f * MU0 / sigma).sqrt();
                C64::new(r, r)
            }
            _ => C64::new(0.0, 0.0),
        }
    }
}

/// Everything the Studio offers by default. The name is the id projects refer to.
pub fn library() -> Vec<Material> {
    let d = Material::dielectric;
    let m = Material::metal;
    vec![
        Material::pec(),
        m("Copper", 5.8e7),
        m("Silver", 6.3e7),
        m("Gold", 4.1e7),
        m("Aluminium", 3.5e7),
        m("Brass", 1.6e7),
        m("Tin", 9.1e6),
        m("Stainless steel", 1.4e6),
        d("Air", 1.0, 0.0),
        d("Foam (Rohacell)", 1.07, 0.0002),
        d("PTFE (Teflon)", 2.1, 0.0002),
        d("FR-4", 4.4, 0.02),
        d("Polyimide (Kapton)", 3.4, 0.008),
        // Rogers' design Dk where it differs from the process Dk (3.38, 3.48, 10.2):
        // that is the value a real resonance lands on.
        d("RO4003C", 3.55, 0.0027),
        d("RO4350B", 3.66, 0.0037),
        d("RT/duroid 5880", 2.2, 0.0009),
        d("RT/duroid 6002", 2.94, 0.0012),
        d("RT/duroid 6010", 10.7, 0.0023),
        d("Taconic TLY-5", 2.2, 0.0009),
        d("Alumina 99.5%", 9.8, 0.0001),
        d("Fused quartz", 3.78, 0.0001),
        d("ABS plastic", 2.8, 0.01),
        d("Polycarbonate", 2.9, 0.01),
        d("Glass", 6.0, 0.01),
        d("Cardboard", 2.0, 0.05),
        // What an RFID label is made of. Stock varies a lot, so these are starting points.
        // Paper: Tentzeris group, 3.2 / 0.053 at 1 GHz for photo paper, and 3.4 / 0.08 used for
        // an inkjet UHF tag.
        d("Paper", 3.3, 0.06),
        // A patent's flexible antenna gives 2.99 / 0.0058 for PET, at no stated frequency.
        d("PET film", 3.0, 0.006),
        // Assumed: no RF measurement of an acrylic label adhesive was found. Thin enough to matter little.
        d("Adhesive (acrylic)", 2.5, 0.02),
        // Assumed: mostly air, so between foam and solid board.
        d("Corrugated board", 1.4, 0.03),
        // Tap water and muscle tissue around 1–6 GHz: lossy, so close to a body or a
        // bottle most of the near field ends up as heat. Water's tan δ here is the value
        // near 1 GHz; `dielectric_kappa` follows its Debye curve.
        d("Water", 78.0, 0.05),
        d("Muscle tissue", 50.0, 0.35),
        // The liquids SAR standards specify (IEEE 1528): εr and σ at these frequencies.
        d("SAR tissue 900 MHz", 41.5, 0.467),
        d("SAR tissue 2450 MHz", 39.2, 0.337),
        d("Ceramic (εr 20)", 20.0, 0.0005),
        d("Ceramic (εr 38)", 38.0, 0.0005),
    ]
}

pub fn by_name(name: &str) -> Option<Material> {
    library().into_iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copper_skin_depth_at_1ghz_is_about_2um() {
        let cu = by_name("copper").unwrap();
        let d = cu.skin_depth(1e9);
        assert!((d - 2.09e-6).abs() < 0.05e-6, "{d}");
        let z = cu.surface_impedance(1e9);
        assert!((z.re - 8.25e-3).abs() < 0.2e-3, "{z}");
        assert_eq!(z.re, z.im);
    }

    #[test]
    fn loss_tangent_maps_to_conductivity() {
        let fr4 = by_name("FR-4").unwrap();
        // ωε₀εr·tanδ at 2.45 GHz.
        let k = fr4.dielectric_kappa(2.45e9);
        assert!((k - 0.012).abs() < 0.001, "{k}");
    }

    /// Rogers' design Dk, the value a resonance lands on, not the process Dk.
    #[test]
    fn laminates_use_the_design_dk() {
        for (name, dk) in [("RO4003C", 3.55), ("RO4350B", 3.66), ("RT/duroid 6010", 10.7)] {
            assert_eq!(by_name(name).unwrap().eps_r(), dk, "{name}");
        }
    }

    /// Debye water: a constant tan δ of 0.05 would be 2.4 times too low at 2.45 GHz.
    /// `ε′` falls with frequency too, so the loss tangent is taken against that.
    #[test]
    fn water_loss_rises_with_frequency() {
        let w = by_name("Water").unwrap();
        for (f, want) in [(915e6, 0.045), (2.45e9, 0.119), (6e9, 0.289)] {
            let omega = 2.0 * std::f64::consts::PI * f;
            let wt = omega * 8.27e-12;
            let real = 5.2 + (78.4 - 5.2) / (1.0 + wt * wt);
            let tan_d = w.dielectric_kappa(f) / (omega * EPS0 * real);
            assert!((tan_d - want).abs() < 0.003, "{f} Hz: {tan_d} vs {want}");
        }
    }

    #[test]
    fn names_are_unique() {
        let lib = library();
        for (i, a) in lib.iter().enumerate() {
            assert!(!lib[i + 1..].iter().any(|b| b.name == a.name), "{}", a.name);
        }
    }
}
