//! What lies over and under a flat antenna: the label paper, the PET it's printed on, the glue, the
//! carton, the bottle, the metal shelf. Layers are as wide as the world, which is what lets the
//! method of moments treat them in its Green's function (see `mom::layered`) instead of meshing them:
//! a 50 µm film costs nothing. How wide a real label is doesn't enter, so a layer whose edge is close
//! to the metal needs a check by FDTD, which does build it to size (see [`Stack::solids`]).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::geometry::{P3, Shape, Solid};
use crate::{C64, material};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    /// A name from the material library.
    pub material: String,
    /// Metres.
    pub thickness: f64,
}

impl Layer {
    pub fn new(name: &str, material: &str, thickness: f64) -> Self {
        Self { name: name.into(), material: material.into(), thickness }
    }
}

/// Layers around a sheet of metal lying flat in a plane of constant z.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    /// Going up from the metal, nearest first.
    #[serde(default)]
    pub above: Vec<Layer>,
    /// Going down from the metal, nearest first.
    #[serde(default)]
    pub below: Vec<Layer>,
    /// A metal surface behind the last layer below, as wide as the world.
    #[serde(default)]
    pub metal_behind: bool,
    /// How far past the antenna each layer runs when FDTD builds it, metres. 0: 10 mm.
    #[serde(default)]
    pub margin: f64,
}

impl Stack {
    pub fn is_empty(&self) -> bool {
        self.above.is_empty() && self.below.is_empty() && !self.metal_behind
    }

    pub fn layers(&self) -> impl Iterator<Item = &Layer> {
        self.above.iter().chain(&self.below)
    }

    /// Says what can't be solved, in words the user can act on.
    pub fn check(&self) -> Result<()> {
        for l in self.layers() {
            let Some(m) = material::by_name(&l.material) else {
                bail!("Layer '{}' is made of '{}', which isn't in the material library.", l.name, l.material);
            };
            if m.is_conductor() {
                bail!("Layer '{}' is {}, a conductor. Metal behind the antenna is set with metal_behind, and metal in the antenna's plane is part of the antenna.", l.name, m.name);
            }
            if !(l.thickness > 0.0 && l.thickness.is_finite()) {
                bail!("Layer '{}' needs a thickness above zero.", l.name);
            }
        }
        if self.metal_behind && self.below.is_empty() {
            bail!("Metal directly under the antenna shorts it out: put a spacer layer between them.");
        }
        Ok(())
    }

    /// A line of words for reports: `paper 80 µm / antenna / PET 50 µm / carton 3 mm`.
    pub fn describe(&self) -> String {
        let word = |l: &Layer| {
            let um = l.thickness * 1e6;
            if um < 1000.0 { format!("{} {um:.0} µm", l.name) } else { format!("{} {:.1} mm", l.name, um / 1e3) }
        };
        let mut parts: Vec<String> = self.above.iter().rev().map(word).collect();
        parts.push("antenna".into());
        parts.extend(self.below.iter().map(word));
        if self.metal_behind {
            parts.push("metal".into());
        }
        parts.join(" / ")
    }

    /// The layers as dielectric boxes and a metal plate, for FDTD: `metal` is the antenna's bounds.
    pub fn solids(&self, metal: (P3, P3)) -> Vec<Solid> {
        let margin = if self.margin > 0.0 { self.margin } else { 0.01 };
        let (lo, hi) = metal;
        let (x0, x1, y0, y1) = (lo[0] - margin, hi[0] + margin, lo[1] - margin, hi[1] + margin);
        let z = lo[2];
        let block = |l: &Layer, a: f64, b: f64| Solid { name: l.name.clone(), material: l.material.clone(), shape: Shape::Box { min: [x0, y0, a.min(b)], max: [x1, y1, a.max(b)] } };
        let mut out = Vec::new();
        let mut at = z;
        for l in &self.above {
            out.push(block(l, at, at + l.thickness));
            at += l.thickness;
        }
        at = z;
        for l in &self.below {
            out.push(block(l, at - l.thickness, at));
            at -= l.thickness;
        }
        if self.metal_behind {
            let reach = margin.max(0.05);
            out.push(Solid { name: "metal behind".into(), material: "PEC".into(), shape: Shape::Box { min: [lo[0] - reach, lo[1] - reach, at], max: [hi[0] + reach, hi[1] + reach, at] } });
        }
        out
    }
}

/// One layer as the solver sees it at one frequency.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Medium {
    pub er: C64,
    pub d: f64,
}

pub(crate) fn media(layers: &[Layer], f: f64) -> Vec<Medium> {
    layers.iter().map(|l| Medium { er: material::by_name(&l.material).map_or(C64::new(1.0, 0.0), |m| m.complex_eps(f)), d: l.thickness }).collect()
}

/// A named build-up, for asking in words. Figures are typical: measure the real stock for a design
/// that has to be right. See the material library for where they come from.
pub struct Preset {
    pub id: &'static str,
    /// A few words, for a list.
    pub label: &'static str,
    pub about: &'static str,
    pub stack: fn() -> Stack,
}

fn l(name: &str, material: &str, um: f64) -> Layer {
    Layer::new(name, material, um * 1e-6)
}

/// PET under the metal: how a wet or dry inlay comes.
fn pet() -> Layer {
    l("PET", "PET film", 50.0)
}

fn glue() -> Layer {
    l("adhesive", "Adhesive (acrylic)", 20.0)
}

pub const PRESETS: [Preset; 8] = [
    Preset { id: "free_space", label: "Bare on its PET, in air", about: "A bare inlay on its PET in air: how inlays are specified, not how they're used", stack: || Stack { below: vec![pet()], ..Default::default() } },
    Preset { id: "paper_label", label: "Under a paper label", about: "Inlay laminated under 80 g/m² paper face stock, with its PET and glue, in air", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet()], ..Default::default() } },
    Preset { id: "thick_paper_label", label: "Under thick card", about: "Inlay under 250 g/m² card or a swing tag, in air", stack: || Stack { above: vec![glue(), l("paper", "Paper", 300.0)], below: vec![pet()], ..Default::default() } },
    Preset { id: "paper_on_carton", label: "Paper label on a carton", about: "A paper label stuck on a corrugated carton wall", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet(), glue(), l("carton", "Corrugated board", 3000.0)], ..Default::default() } },
    Preset { id: "paper_on_glass", label: "Paper label on glass", about: "A paper label on a glass bottle or window, 3 mm of glass", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet(), glue(), l("glass", "Glass", 3000.0)], ..Default::default() } },
    Preset { id: "paper_on_plastic", label: "Paper label on plastic", about: "A paper label on a 2 mm polycarbonate case", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet(), glue(), l("plastic", "Polycarbonate", 2000.0)], ..Default::default() } },
    Preset { id: "on_water", label: "Label on a bottle of water", about: "A label on a thin plastic bottle full of water, 40 mm of water behind 0.5 mm of plastic", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet(), glue(), l("bottle", "Polycarbonate", 500.0), l("water", "Water", 40000.0)], ..Default::default() } },
    Preset { id: "foam_on_metal", label: "On foam over metal", about: "An inlay on 3 mm of foam on a metal surface", stack: || Stack { above: vec![glue(), l("paper", "Paper", 80.0)], below: vec![pet(), glue(), l("foam", "Foam (Rohacell)", 3000.0)], metal_behind: true, ..Default::default() } },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_can_be_solved_and_reads_in_order() {
        for p in &PRESETS {
            (p.stack)().check().unwrap_or_else(|e| panic!("{}: {e}", p.id));
        }
        let paper = (preset("paper_on_carton").unwrap().stack)();
        assert_eq!(paper.describe(), "paper 80 µm / adhesive 20 µm / antenna / PET 50 µm / adhesive 20 µm / carton 3.0 mm");
    }

    #[test]
    fn what_cant_be_solved_is_refused_with_a_reason() {
        let bad = Stack { above: vec![Layer::new("x", "Kryptonite", 1e-3)], ..Default::default() };
        assert!(bad.check().unwrap_err().to_string().contains("Kryptonite"));
        let thin = Stack { above: vec![Layer::new("x", "Paper", 0.0)], ..Default::default() };
        assert!(thin.check().is_err());
        let shorted = Stack { metal_behind: true, ..Default::default() };
        assert!(shorted.check().unwrap_err().to_string().contains("spacer"));
        let foil = Stack { below: vec![Layer::new("x", "Aluminium", 1e-3)], ..Default::default() };
        assert!(foil.check().unwrap_err().to_string().contains("metal_behind"));
    }

    #[test]
    fn fdtd_gets_the_same_layers_built_to_size() {
        let s = Stack { above: vec![Layer::new("top", "Paper", 1e-3)], below: vec![Layer::new("under", "PET film", 2e-3)], metal_behind: true, margin: 0.005 };
        let out = s.solids(([-0.05, -0.01, 0.0], [0.05, 0.01, 0.0]));
        assert_eq!(out.len(), 3);
        let Shape::Box { min, max } = &out[0].shape else { panic!() };
        assert_eq!((min[2], max[2]), (0.0, 1e-3));
        assert!((min[0] + 0.055).abs() < 1e-12 && (max[1] - 0.015).abs() < 1e-12);
        let Shape::Box { min, max } = &out[1].shape else { panic!() };
        assert_eq!((min[2], max[2]), (-2e-3, 0.0));
        let Shape::Box { min, max } = &out[2].shape else { panic!() };
        assert!(min[2] == -2e-3 && max[2] == -2e-3, "a plate behind it");
    }
}
