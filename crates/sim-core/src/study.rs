//! A simulation as the user sets it up: what to solve, on what, with which conditions. Plain data,
//! saved with the project. Backends turn it into solver input.

use serde::{Deserialize, Serialize};

use crate::catalog::{self, AnalysisId, Needs};
use crate::materials::{self, Fluid, Solid};

/// One face of a body. `point` is on the face, in metres: it's how a run notices that an edit to the
/// design has moved or renumbered the face, instead of loading the wrong one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceRef {
    pub face: u32,
    pub point: [f64; 3],
    /// It's on the study's second body, the one this body touches.
    #[serde(default)]
    pub second: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BcKind {
    /// Held still.
    Fixed,
    /// Total force, N.
    Force { force: [f64; 3] },
    /// Pa, pushing into the surface.
    Pressure { pressure: f64 },
    /// Fixed temperature, in deg C.
    Temperature { value: f64 },
    /// W/m2 into the surface.
    HeatFlux { flux: f64 },
    /// W/m2K and deg C.
    Convection { coefficient: f64, ambient: f64 },
    /// Flow in at this speed, m/s, along the face's inward direction.
    VelocityInlet { speed: f64 },
    /// Flow out to this gauge pressure, Pa.
    PressureOutlet { pressure: f64 },
    /// A wall the flow can't pass, no slip.
    Wall,
    /// Moved by this much, in metres. A body that only touches another needs this, not a force: until
    /// contact closes, a force would leave it floating free.
    Displacement { displacement: [f64; 3] },
    /// Faces of the two bodies that press on each other but can't pass through. Zero friction: they slide.
    Contact { friction: f64 },
    /// An electric potential, V.
    Potential { value: f64 },
}

impl BcKind {
    pub fn label(&self) -> &'static str {
        match self {
            BcKind::Fixed => "Fixed support",
            BcKind::Force { .. } => "Force",
            BcKind::Pressure { .. } => "Pressure",
            BcKind::Temperature { .. } => "Temperature",
            BcKind::HeatFlux { .. } => "Heat flux",
            BcKind::Convection { .. } => "Convection",
            BcKind::VelocityInlet { .. } => "Velocity inlet",
            BcKind::PressureOutlet { .. } => "Pressure outlet",
            BcKind::Wall => "Wall",
            BcKind::Potential { .. } => "Electric potential",
            BcKind::Displacement { .. } => "Displacement",
            BcKind::Contact { .. } => "Contact",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub kind: BcKind,
    pub faces: Vec<FaceRef>,
}

/// Knobs of the analyses. Each one uses what it needs. A saved study from before a setting existed
/// reads with that setting at its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Largest element, as a share of the body's diagonal.
    pub element_size: f64,
    pub modes: usize,
    pub f_min: f64,
    pub f_max: f64,
    pub points: usize,
    pub damping: f64,
    /// s
    pub dt: f64,
    /// s
    pub duration: f64,
    pub transient: bool,
    /// deg C
    pub initial_temperature: f64,
    /// deg C
    pub reference_temperature: f64,
    /// Share of the load per increment of a nonlinear run.
    pub load_increment: f64,
    /// m/s, of the wind or free stream of external-flow analyses.
    pub free_stream: f64,
    /// Cells across the body's largest side, for the voxel solvers.
    pub resolution: u32,
    /// Wind directions of a comfort study.
    pub directions: u32,
    /// How high water starts in a multiphase run, as a share of the body's height.
    pub fill_fraction: f64,
    /// Multi-purpose: whether density changes with the flow.
    pub compressible: bool,
    /// Multi-purpose: how fast a zone around the body turns, in revolutions per minute. Zero: it doesn't.
    pub rotor_rpm: f64,
    /// Electromagnetics: a coil's ampere-turns. Zero: electrostatics between picked faces instead.
    pub coil_ampere_turns: f64,
    /// The axis a rotor or a coil turns about, through the body's middle: 0 x, 1 y, 2 z.
    pub axis: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            element_size: 0.04,
            modes: 6,
            f_min: 1.0,
            f_max: 1000.0,
            points: 20,
            damping: 0.01,
            dt: 0.001,
            duration: 0.1,
            transient: false,
            initial_temperature: 20.0,
            reference_temperature: 20.0,
            load_increment: 0.1,
            free_stream: 5.0,
            resolution: 64,
            directions: 8,
            fill_fraction: 0.5,
            compressible: true,
            rotor_rpm: 0.0,
            coil_ampere_turns: 0.0,
            axis: 2,
        }
    }
}

/// A study's `body` starting with this names an object in the scene, not a design body.
pub const SCENE_PREFIX: &str = "scene:";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Study {
    pub name: String,
    pub analysis: AnalysisId,
    /// A design body's name, or [`SCENE_PREFIX`] and an object's name.
    pub body: Option<String>,
    /// A second design body, for contact with the first.
    #[serde(default)]
    pub second_body: Option<String>,
    pub material: Solid,
    /// The user has said what the body is made of. A design body shows its material as it's modelled, but
    /// a scene object says nothing, and the list's first entry isn't an answer.
    #[serde(default)]
    pub material_confirmed: bool,
    pub fluid: Fluid,
    pub conditions: Vec<Condition>,
    pub settings: Settings,
}

impl Study {
    pub fn new(name: String, analysis: AnalysisId) -> Self {
        Self {
            name,
            analysis,
            body: None,
            second_body: None,
            material: materials::solids().remove(0),
            material_confirmed: false,
            fluid: materials::fluids().remove(0),
            conditions: Vec::new(),
            settings: Settings::default(),
        }
    }

    /// The scene object this study is on, if it isn't on a design body.
    pub fn scene_object(&self) -> Option<&str> {
        self.body.as_deref()?.strip_prefix(SCENE_PREFIX)
    }

    pub fn is_fea(&self) -> bool {
        catalog::analysis(self.analysis).needs == Needs::NativeFea
    }

    /// Does the answer depend on what the body is made of?
    pub fn uses_solid(&self) -> bool {
        self.is_fea() || matches!(self.analysis, AnalysisId::ConjugateHeatTransfer | AnalysisId::ConjugateHeatTransferVoxel)
    }

    /// Does the answer depend on the fluid around it? Electromagnetics is in air, whatever's chosen.
    pub fn uses_fluid(&self) -> bool {
        !self.is_fea() && self.analysis != AnalysisId::Electromagnetics
    }

    /// The condition kinds that make sense for this analysis, as starting values.
    pub fn offered_conditions(&self) -> Vec<BcKind> {
        use AnalysisId as A;
        let mechanical = vec![BcKind::Fixed, BcKind::Force { force: [0.0, 0.0, -100.0] }, BcKind::Pressure { pressure: 1e5 }];
        let thermal = vec![BcKind::Temperature { value: 100.0 }, BcKind::HeatFlux { flux: 1000.0 }, BcKind::Convection { coefficient: 10.0, ambient: 20.0 }];
        match self.analysis {
            A::NonlinearMechanical => {
                let mut offered = mechanical;
                offered.push(BcKind::Displacement { displacement: [0.0, 0.0, -1e-3] });
                if self.second_body.is_some() {
                    offered.push(BcKind::Contact { friction: 0.0 });
                }
                offered
            }
            A::Static | A::Dynamic | A::Harmonic => mechanical,
            A::Frequency => vec![BcKind::Fixed],
            A::HeatTransfer => thermal,
            A::Thermomechanical => mechanical.into_iter().chain(thermal).collect(),
            A::Electromagnetics => vec![BcKind::Potential { value: 100.0 }],
            // The body stands in a stream of air, and its heat is what's asked for.
            A::ConjugateHeatTransfer | A::ConjugateHeatTransferVoxel => vec![BcKind::HeatFlux { flux: 1000.0 }],
            A::ConvectiveHeatTransfer => vec![BcKind::VelocityInlet { speed: 1.0 }, BcKind::PressureOutlet { pressure: 0.0 }, BcKind::Wall, BcKind::Temperature { value: 60.0 }, BcKind::HeatFlux { flux: 1000.0 }],
            _ => vec![BcKind::VelocityInlet { speed: 1.0 }, BcKind::PressureOutlet { pressure: 0.0 }, BcKind::Wall],
        }
    }

    /// What's still missing before it can run, in the user's words. Empty: ready.
    pub fn problems(&self) -> Vec<String> {
        use AnalysisId as A;
        let mut out = Vec::new();
        if self.body.is_none() {
            out.push("Choose a body: an object from the scene.".to_string());
        }
        for c in &self.conditions {
            if c.faces.is_empty() {
                out.push(format!("{}: pick the faces it applies to.", c.kind.label()));
            }
        }
        if self.scene_object().is_some() && self.uses_solid() && !self.material_confirmed {
            out.push("Confirm the material: a scene object doesn't say what it's made of.".to_string());
        }
        // A face is one kind of boundary to a flow: on two, the mesher gets the same facets twice.
        if !self.is_fea() {
            for (i, a) in self.conditions.iter().enumerate() {
                for b in &self.conditions[i + 1..] {
                    if a.faces.iter().any(|f| b.faces.iter().any(|g| g.face == f.face && g.second == f.second)) {
                        out.push(format!("{} and {} share a face: a face can only be one of them.", a.kind.label(), b.kind.label()));
                    }
                }
            }
        }
        let has = |f: &dyn Fn(&BcKind) -> bool| self.conditions.iter().any(|c| f(&c.kind));
        match self.analysis {
            A::NonlinearMechanical => {
                if !has(&|k| matches!(k, BcKind::Fixed)) {
                    out.push("Add a fixed support: an unsupported part just flies away.".to_string());
                }
                if !has(&|k| matches!(k, BcKind::Force { .. } | BcKind::Pressure { .. } | BcKind::Displacement { .. })) {
                    out.push("Add a force, a pressure or a displacement.".to_string());
                }
                if self.second_body.is_some() {
                    let on = |second: bool, wanted: &dyn Fn(&BcKind) -> bool| self.conditions.iter().any(|c| wanted(&c.kind) && c.faces.iter().any(|f| f.second == second));
                    let contact = |k: &BcKind| matches!(k, BcKind::Contact { .. });
                    if !on(false, &contact) || !on(true, &contact) {
                        out.push("Add a contact, with faces on both bodies.".to_string());
                    }
                    // Measured: a body held only by a force solved to stresses wrong by ten times and
                    // more, with no error from the solver.
                    let held = |k: &BcKind| matches!(k, BcKind::Fixed | BcKind::Displacement { .. });
                    if !on(false, &held) || !on(true, &held) {
                        out.push("Hold each body with a fixed support or a displacement. A force alone leaves a body floating until contact closes, and the solver gives wrong answers with no error.".to_string());
                    }
                }
            }
            A::Static | A::Dynamic | A::Harmonic | A::Frequency => {
                if !has(&|k| matches!(k, BcKind::Fixed)) {
                    out.push("Add a fixed support: an unsupported part just flies away.".to_string());
                }
                if self.analysis != A::Frequency && !has(&|k| matches!(k, BcKind::Force { .. } | BcKind::Pressure { .. })) {
                    out.push("Add a force or a pressure.".to_string());
                }
            }
            A::HeatTransfer => {
                if !has(&|k| matches!(k, BcKind::Temperature { .. } | BcKind::Convection { .. })) && !self.settings.transient {
                    out.push("Add a temperature or convection: otherwise a steady state has no reference.".to_string());
                }
            }
            A::Thermomechanical => {
                if !has(&|k| matches!(k, BcKind::Fixed)) {
                    out.push("Add a fixed support.".to_string());
                }
                if !has(&|k| matches!(k, BcKind::Temperature { .. } | BcKind::HeatFlux { .. } | BcKind::Convection { .. })) {
                    out.push("Add a thermal condition.".to_string());
                }
            }
            A::ConjugateHeatTransfer | A::ConjugateHeatTransferVoxel => {
                if !has(&|k| matches!(k, BcKind::HeatFlux { .. })) {
                    out.push("Add a heat flux on some faces: with no heat going in, nothing warms up.".to_string());
                }
            }
            A::Electromagnetics => {
                let potentials = self.conditions.iter().filter(|c| matches!(c.kind, BcKind::Potential { .. })).count();
                if self.settings.coil_ampere_turns == 0.0 && potentials < 2 {
                    out.push("Add two potentials for an electric field.".to_string());
                }
            }
            _ => {}
        }
        if self.settings.element_size <= 0.0 {
            out.push("The element size must be above zero.".to_string());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> FaceRef {
        FaceRef { face: 1, point: [0.0; 3], second: false }
    }

    #[test]
    fn a_new_static_study_says_what_it_needs() {
        let mut s = Study::new("t".into(), AnalysisId::Static);
        let p = s.problems();
        assert!(p.iter().any(|m| m.contains("body")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("fixed support")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("force or a pressure")), "{p:?}");
        s.body = Some("B".into());
        s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![face()] });
        s.conditions.push(Condition { kind: BcKind::Force { force: [0.0, 0.0, 1.0] }, faces: vec![] });
        assert_eq!(s.problems(), ["Force: pick the faces it applies to."]);
        s.conditions[1].faces.push(face());
        assert!(s.problems().is_empty());
    }

    #[test]
    fn frequency_only_needs_a_support() {
        let mut s = Study::new("t".into(), AnalysisId::Frequency);
        s.body = Some("B".into());
        s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![face()] });
        assert!(s.problems().is_empty());
    }

    #[test]
    fn studies_survive_saving() {
        let mut s = Study::new("t".into(), AnalysisId::Harmonic);
        s.conditions.push(Condition { kind: BcKind::Force { force: [1.0, 2.0, 3.0] }, faces: vec![face()] });
        let back: Study = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn contact_needs_both_bodies_held_and_a_contact_on_both() {
        let on = |second: bool| FaceRef { face: 0, point: [0.0; 3], second };
        let mut s = Study::new("t".into(), AnalysisId::NonlinearMechanical);
        s.body = Some("A".into());
        s.second_body = Some("B".into());
        assert!(s.offered_conditions().iter().any(|k| matches!(k, BcKind::Contact { .. })));
        s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![on(false)] });
        s.conditions.push(Condition { kind: BcKind::Force { force: [0.0, 0.0, -1.0] }, faces: vec![on(true)] });
        s.conditions.push(Condition { kind: BcKind::Contact { friction: 0.0 }, faces: vec![on(false), on(true)] });
        let p = s.problems();
        assert!(p.iter().any(|m| m.contains("Hold each body")), "a force alone isn't enough: {p:?}");
        s.conditions[1].kind = BcKind::Displacement { displacement: [0.0, 0.0, -1e-4] };
        assert!(s.problems().is_empty(), "{:?}", s.problems());
        s.conditions[2].faces = vec![on(false)];
        assert!(s.problems().iter().any(|m| m.contains("faces on both bodies")), "{:?}", s.problems());
    }

    #[test]
    fn a_face_saved_before_there_were_two_bodies_is_on_the_first() {
        let back: FaceRef = serde_json::from_str("{\"face\": 3, \"point\": [1.0, 2.0, 3.0]}").unwrap();
        assert!(!back.second);
    }

    #[test]
    fn a_study_saved_before_a_setting_existed_still_reads() {
        let mut v = serde_json::to_value(Study::new("t".into(), AnalysisId::Static)).unwrap();
        v["settings"].as_object_mut().unwrap().remove("rotor_rpm");
        let back: Study = serde_json::from_value(v).unwrap();
        assert_eq!(back.settings.rotor_rpm, 0.0);
    }

    #[test]
    fn electromagnetics_needs_two_potentials_or_a_coil() {
        let mut s = Study::new("t".into(), AnalysisId::Electromagnetics);
        s.body = Some("B".into());
        assert!(s.problems().iter().any(|m| m.contains("two potentials")), "{:?}", s.problems());
        s.settings.coil_ampere_turns = 100.0;
        assert!(s.problems().is_empty(), "{:?}", s.problems());
    }

    #[test]
    fn cooling_studies_need_heat() {
        for id in [AnalysisId::ConjugateHeatTransfer, AnalysisId::ConjugateHeatTransferVoxel] {
            let mut s = Study::new("t".into(), id);
            s.body = Some("B".into());
            assert!(s.problems().iter().any(|m| m.contains("heat flux")), "{:?}", s.problems());
            s.conditions.push(Condition { kind: BcKind::HeatFlux { flux: 1.0 }, faces: vec![face()] });
            assert!(s.problems().is_empty(), "{:?}", s.problems());
        }
    }

    #[test]
    fn a_scene_object_needs_its_material_confirmed() {
        let mut s = Study::new("t".into(), AnalysisId::Frequency);
        s.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![face()] });
        s.body = Some("Designed".into());
        assert!(s.problems().is_empty(), "a design body doesn't: {:?}", s.problems());
        s.body = Some(format!("{SCENE_PREFIX}Object"));
        assert!(s.problems().iter().any(|m| m.contains("Confirm the material")), "{:?}", s.problems());
        s.material_confirmed = true;
        assert!(s.problems().is_empty(), "{:?}", s.problems());
        // A flow has no use for what the body's made of.
        let mut flow = Study::new("t".into(), AnalysisId::Incompressible);
        flow.body = Some(format!("{SCENE_PREFIX}Object"));
        assert!(flow.problems().is_empty(), "{:?}", flow.problems());
    }

    #[test]
    fn a_flow_refuses_one_face_under_two_conditions() {
        let mut s = Study::new("t".into(), AnalysisId::Incompressible);
        s.body = Some("B".into());
        s.conditions.push(Condition { kind: BcKind::VelocityInlet { speed: 1.0 }, faces: vec![face()] });
        s.conditions.push(Condition { kind: BcKind::Wall, faces: vec![face()] });
        assert!(s.problems().iter().any(|m| m.contains("share a face")), "{:?}", s.problems());
        s.conditions[1].faces[0].face = 2;
        assert!(s.problems().is_empty(), "{:?}", s.problems());
        // In a structure it's fine: a face can be held and heated at once.
        let mut t = Study::new("t".into(), AnalysisId::Thermomechanical);
        t.body = Some("B".into());
        t.conditions.push(Condition { kind: BcKind::Fixed, faces: vec![face()] });
        t.conditions.push(Condition { kind: BcKind::Temperature { value: 50.0 }, faces: vec![face()] });
        assert!(t.problems().is_empty(), "{:?}", t.problems());
    }

    #[test]
    fn each_analysis_says_what_it_is_made_of() {
        let of = |id| {
            let s = Study::new("t".into(), id);
            (s.uses_solid(), s.uses_fluid())
        };
        assert_eq!(of(AnalysisId::Static), (true, false));
        assert_eq!(of(AnalysisId::Incompressible), (false, true));
        assert_eq!(of(AnalysisId::ConjugateHeatTransfer), (true, true), "the block's conduction and the air both count");
        assert_eq!(of(AnalysisId::ConjugateHeatTransferVoxel), (true, true));
        assert_eq!(of(AnalysisId::Electromagnetics), (false, false));
    }

    #[test]
    fn every_analysis_offers_conditions() {
        for a in catalog::ANALYSES {
            assert!(!Study::new("t".into(), a.id).offered_conditions().is_empty(), "{:?}", a.id);
        }
    }
}
