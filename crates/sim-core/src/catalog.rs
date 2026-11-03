//! Every analysis type on offer. The Create Simulation dialog is drawn from this, and `readiness`
//! is earned: an analysis is `Verified` only when its benchmarks in `tests/` pass against known
//! answers. The descriptions say plainly what each one is and is not.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnalysisId {
    Incompressible,
    IncompressibleLbm,
    MultiPurpose,
    PedestrianWindComfort,
    Compressible,
    ConvectiveHeatTransfer,
    ConjugateHeatTransfer,
    ConjugateHeatTransferVoxel,
    Multiphase,
    Static,
    Dynamic,
    HeatTransfer,
    Thermomechanical,
    Harmonic,
    Frequency,
    NonlinearMechanical,
    Electromagnetics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Flow,
    Structural,
    Electromagnetic,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Flow, Group::Structural, Group::Electromagnetic];
    pub fn label(self) -> &'static str {
        match self {
            Group::Flow => "Flow",
            Group::Structural => "Structural",
            Group::Electromagnetic => "Electromagnetic",
        }
    }
}

/// What must be installed before an analysis can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Needs {
    /// Gmsh and CalculiX: native Windows programs.
    NativeFea,
    /// The WSL distro, with OpenFOAM.
    Foam,
    /// The WSL distro, with Elmer, and Gmsh.
    Elmer,
    /// Only the GPU, with the built-in solver.
    Gpu,
}

/// How far an analysis can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Its benchmarks pass.
    Verified,
    /// It runs, but its benchmarks aren't all met: results are shown with a warning.
    Experimental,
}

#[derive(Debug, Clone, Copy)]
pub struct Analysis {
    pub id: AnalysisId,
    pub group: Group,
    pub label: &'static str,
    pub summary: &'static str,
    pub engine: &'static str,
    pub tags: &'static [&'static str],
    /// What it can't do, shown beside what it can.
    pub limits: &'static str,
    pub needs: Needs,
    pub readiness: Readiness,
}

pub const ANALYSES: &[Analysis] = &[
    Analysis {
        id: AnalysisId::Incompressible,
        group: Group::Flow,
        label: "Incompressible",
        summary: "Air or water at low speed, where density stays constant: pressure drop, flow distribution, forces on a part. Steady or transient, with a RANS turbulence model on a body-fitted mesh.",
        engine: "OpenFOAM simpleFoam / pimpleFoam, k-omega SST",
        tags: &["External flow", "Internal flow", "Turbulence", "Pressure drop"],
        limits: "Below about Mach 0.3. Needs a watertight flow domain.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::IncompressibleLbm,
        group: Group::Flow,
        label: "Incompressible (LBM)",
        summary: "Fast transient flow on the GPU with the lattice Boltzmann method. Geometry is voxelised, so it needs no meshing or clean-up, and copes with messy or open surfaces.",
        engine: "Built-in GPU solver (D3Q19, TRT, Smagorinsky)",
        tags: &["GPU", "Transient", "No meshing", "External flow"],
        limits: "Walls follow the voxels, so drag converges slowly with cell size. Transient only; low Mach only.",
        needs: Needs::Gpu,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::MultiPurpose,
        group: Group::Flow,
        label: "Multi-purpose",
        summary: "One setup from low to high subsonic speeds, with a compressible switch and a rotating zone around the body for fans and rotors. An open-source equivalent built on OpenFOAM's tools, not any commercial solver.",
        engine: "OpenFOAM simpleFoam / rhoSimpleFoam, k-omega SST, MRF",
        tags: &["Compressible switch", "Rotating zones", "Fans", "Pumps"],
        limits: "Rotating parts by frozen rotor (MRF), with the zone around the whole body; sliding meshes are not offered. A rotor needs a fine mesh: with about 3 cells across its radius at a tip Reynolds number of 1000, the surface moved correctly but the air beside it had a tenth of the swirl it should.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::PedestrianWindComfort,
        group: Group::Flow,
        label: "Pedestrian Wind Comfort",
        summary: "Wind over buildings from up to 36 directions on the GPU, with a log-law wind profile, giving a map of speed-up at pedestrian height.",
        engine: "Built-in GPU solver with a log-law inlet",
        tags: &["GPU", "Wind rose", "Buildings", "AEC"],
        limits: "For comparing layouts: not validated against AIJ benchmarks, so don't read its values as compliance with a wind standard. Its sheltered zones run long: behind a tall building, the wake measured 1.37 widths in a wind tunnel and comes out at 3.4 here. The incoming wind has no gusts.",
        needs: Needs::Gpu,
        readiness: Readiness::Experimental,
    },
    Analysis {
        id: AnalysisId::Compressible,
        group: Group::Flow,
        label: "Compressible",
        summary: "High-speed gas flow where density changes and shocks matter: nozzles, valves, supersonic parts.",
        engine: "OpenFOAM rhoCentralFoam",
        tags: &["High speed", "Shocks", "Nozzles"],
        limits: "Inviscid: walls slip, so it gives shocks and pressure forces, not boundary layers or skin friction. Single-species ideal gas.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::ConvectiveHeatTransfer,
        group: Group::Flow,
        label: "Convective Heat Transfer",
        summary: "Flow and temperature in a fluid, with buoyancy: natural and forced convection, where solids only appear as boundaries.",
        engine: "OpenFOAM buoyantSimpleFoam / buoyantPimpleFoam",
        tags: &["Natural convection", "Forced convection", "HVAC"],
        limits: "Heat conduction inside solids is not solved; use Conjugate Heat Transfer for that.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::ConjugateHeatTransfer,
        group: Group::Flow,
        label: "Conjugate Heat Transfer",
        summary: "Heat moving between a solid and the air flowing around it, with both resolved on a body-fitted mesh: a heat sink or a hot part in a stream of air.",
        engine: "OpenFOAM chtMultiRegionSimpleFoam",
        tags: &["Solids and fluids", "Electronics cooling", "Heat exchanger"],
        limits: "Steady state, one closed body in forced air, with its heat released evenly through it. Slow: the solid and the air settle at very different rates.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::ConjugateHeatTransferVoxel,
        group: Group::Flow,
        label: "Conjugate Heat Transfer (voxel)",
        summary: "Early-stage electronics and HVAC cooling without cleaning up CAD: everything is voxelised onto a Cartesian grid, with flow on the GPU and conduction in solids on the CPU, iterated to a steady state.",
        engine: "Built-in GPU flow solver coupled to a CPU conduction solver",
        tags: &["GPU", "Complex assemblies", "Electronics cooling", "No meshing"],
        limits: "Steady state only. Not an immersed-boundary method: walls follow the voxels.",
        needs: Needs::Gpu,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Multiphase,
        group: Group::Flow,
        label: "Multiphase",
        summary: "Two immiscible fluids with a sharp interface between them, such as air and water: sloshing, filling, free surfaces.",
        engine: "OpenFOAM interFoam (volume of fluid)",
        tags: &["Free surface", "Sloshing", "Filling"],
        limits: "Two incompressible phases. Transient, so runs are long. The interface is kept slightly soft, because that's what makes surface tension accurate.",
        needs: Needs::Foam,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Static,
        group: Group::Structural,
        label: "Static",
        summary: "Displacements and stresses under steady loads, ignoring inertia. Pick faces to hold and load; second-order elements follow curved geometry.",
        engine: "CalculiX *STATIC on Gmsh second-order tetrahedra",
        tags: &["Stress", "Deflection", "Linear"],
        limits: "Small deflections and linear materials: use Nonlinear Mechanical beyond that.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Dynamic,
        group: Group::Structural,
        label: "Dynamic",
        summary: "Displacements and stresses over time, with inertia, for loads that change quickly: impacts, step loads, vibration start-up.",
        engine: "CalculiX *DYNAMIC, direct integration",
        tags: &["Transient", "Inertia", "Impact loads"],
        limits: "Implicit time stepping: not for fast crash or explosion events.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::HeatTransfer,
        group: Group::Structural,
        label: "Heat Transfer",
        summary: "Temperature and heat flow inside solids, steady or over time, with fixed temperatures, heat fluxes, convection and internal heat sources.",
        engine: "CalculiX *HEAT TRANSFER",
        tags: &["Conduction", "Transient", "Solids only"],
        limits: "The surrounding fluid is only a film coefficient. Use a CHT type to solve it.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Thermomechanical,
        group: Group::Structural,
        label: "Thermomechanical",
        summary: "Stresses and deformation caused by temperature, together with mechanical loads: thermal expansion, mismatch, constrained heating.",
        engine: "CalculiX *COUPLED TEMPERATURE-DISPLACEMENT",
        tags: &["Thermal stress", "Expansion", "Coupled"],
        limits: "Temperature-independent material properties.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Harmonic,
        group: Group::Structural,
        label: "Harmonic",
        summary: "The steady response to a periodic load across a frequency range, including damping: the height and place of resonance peaks.",
        engine: "CalculiX *STEADY STATE DYNAMICS, modal",
        tags: &["Frequency response", "Resonance", "Damping"],
        limits: "Linear. Accuracy depends on extracting enough modes above the range of interest.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Frequency,
        group: Group::Structural,
        label: "Frequency Analysis",
        summary: "Natural frequencies and mode shapes, animated: what a part would ring at, and how.",
        engine: "CalculiX *FREQUENCY",
        tags: &["Modes", "Natural frequencies"],
        limits: "No load effects on stiffness.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::NonlinearMechanical,
        group: Group::Structural,
        label: "Nonlinear Mechanical",
        summary: "Large deflections, plasticity and contact between two bodies, loaded in increments. An open-source equivalent on CalculiX, not a commercial solver.",
        engine: "CalculiX *STATIC, NLGEOM, *PLASTIC, *CONTACT PAIR",
        tags: &["Large deformation", "Plasticity", "Contact"],
        limits: "Elastic-perfectly-plastic materials only. Both bodies share one material, and each must be held by a support or a displacement, not a force alone. A small contact patch needs a fine element size.",
        needs: Needs::NativeFea,
        readiness: Readiness::Verified,
    },
    Analysis {
        id: AnalysisId::Electromagnetics,
        group: Group::Electromagnetic,
        label: "Electromagnetics",
        summary: "Low-frequency fields on a body: the electric field between faces at set potentials. Radio frequencies stay with the RF solver (openEMS).",
        engine: "Elmer StatElecSolver and MagnetoDynamics (Whitney elements)",
        tags: &["Electrostatics", "Magnetostatics", "Coils"],
        limits: "Coils carry a uniform current round one axis, in air, with no magnetic materials. Eddy currents and time-varying fields are checked at solver level but aren't offered in the panel yet.",
        needs: Needs::Elmer,
        readiness: Readiness::Verified,
    },
];

pub fn analysis(id: AnalysisId) -> &'static Analysis {
    ANALYSES.iter().find(|a| a.id == id).expect("every id has an entry")
}

/// A way in by what the user is doing, not by physics.
#[derive(Debug, Clone, Copy)]
pub struct Workflow {
    pub label: &'static str,
    pub summary: &'static str,
    /// What it starts as. Sensible defaults for it are set in `study::Study::for_workflow`.
    pub analysis: AnalysisId,
}

pub const WORKFLOWS: &[Workflow] = &[
    Workflow { label: "Electronics cooling", summary: "Will this enclosure stay cool? Heat sources in a voxelised assembly, with fans or natural convection.", analysis: AnalysisId::ConjugateHeatTransferVoxel },
    Workflow { label: "HVAC and rooms", summary: "Air and temperature distribution in a building space.", analysis: AnalysisId::ConvectiveHeatTransfer },
    Workflow { label: "Valves and pipes", summary: "Pressure drop and flow rate through an internal passage.", analysis: AnalysisId::Incompressible },
    Workflow { label: "Fans and pumps", summary: "Rotating machinery with an MRF zone.", analysis: AnalysisId::MultiPurpose },
    Workflow { label: "Wind around buildings", summary: "Comfort at pedestrian height across wind directions.", analysis: AnalysisId::PedestrianWindComfort },
    Workflow { label: "External aerodynamics", summary: "Forces and wake around a body, with no meshing.", analysis: AnalysisId::IncompressibleLbm },
    Workflow { label: "Part strength", summary: "Will it hold? Stresses and deflection under load.", analysis: AnalysisId::Static },
    Workflow { label: "Vibration check", summary: "What frequencies will this ring at?", analysis: AnalysisId::Frequency },
    Workflow { label: "Thermal stress", summary: "What happens to this part when it heats up?", analysis: AnalysisId::Thermomechanical },
    Workflow { label: "Coils and magnets", summary: "Flux density and inductance of a low-frequency device.", analysis: AnalysisId::Electromagnetics },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_seventeen_types_are_offered_once_each() {
        assert_eq!(ANALYSES.len(), 17);
        let mut seen = std::collections::HashSet::new();
        for a in ANALYSES {
            assert!(seen.insert(a.id), "{:?} twice", a.id);
            assert_eq!(analysis(a.id).label, a.label);
            assert!(!a.summary.is_empty() && !a.limits.is_empty() && !a.tags.is_empty(), "{:?} is incomplete", a.id);
        }
    }

    #[test]
    fn the_groups_hold_what_the_dialog_shows() {
        let count = |g| ANALYSES.iter().filter(|a| a.group == g).count();
        assert_eq!(count(Group::Flow), 9);
        assert_eq!(count(Group::Structural), 7);
        assert_eq!(count(Group::Electromagnetic), 1);
    }

    #[test]
    fn nothing_claims_to_be_a_proprietary_solver() {
        for a in ANALYSES {
            assert!(!a.label.contains("Marc"), "{}", a.label);
            assert!(!a.label.contains("IBM"), "{}: that's a method this doesn't use", a.label);
        }
    }

    #[test]
    fn each_workflow_starts_somewhere_real() {
        for w in WORKFLOWS {
            assert!(!analysis(w.analysis).label.is_empty());
        }
    }
}
