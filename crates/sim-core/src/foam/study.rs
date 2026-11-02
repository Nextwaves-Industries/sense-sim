//! Takes a [`Study`] on a design body through OpenFOAM: a background mesh, snappyHexMesh around or
//! inside the body, the audit of what it made, the solver, and results with a cut plane to look at.
//!
//! With inlet faces picked, the body is the flow domain (an internal flow). With none, it stands in
//! a wind tunnel with a free stream along +x (an external flow).

use std::fmt::Write as _;

use anyhow::{Context, Result, anyhow, bail};

use super::cases::{self, Control, dim};
use super::{Case, mesh_audit};
use crate::catalog::AnalysisId;
use crate::geometry::Partition;
use crate::geometry::partition::tri_area;
use crate::jobs::JobCtx;
use crate::lbm::voxel::{self, Grid};
use crate::results::{Location, ResultSet, ResultStep, StepKind};
use crate::study::{BcKind, Study};

fn bounds(part: &Partition) -> ([f64; 3], [f64; 3]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// A point well inside the body, for an internal mesh: the voxel farthest from any wall.
pub fn interior_point(part: &Partition) -> Option<[f64; 3]> {
    let (lo, hi) = bounds(part);
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    // Fine enough for the body's thinnest side: a slender pipe would otherwise have no cell inside.
    let (thin, long) = (size[0].min(size[1]).min(size[2]), size[0].max(size[1]).max(size[2]));
    let dx = (thin / 12.0).max(long / 400.0);
    let n = |k: usize| ((size[k] / dx).ceil() as u32 + 2).max(3);
    let grid = Grid { origin: [lo[0] - dx, lo[1] - dx, lo[2] - dx], dx, size: [n(0), n(1), n(2)] };
    let solid = voxel::fill(part, &grid);
    // Peel the solid from outside in; the last cells left are the deepest.
    let mut depth = solid.clone();
    // Any solid cell will do if nothing has all its neighbours solid.
    let mut best = (0..grid.size[2]).flat_map(|z| (0..grid.size[1]).flat_map(move |y| (0..grid.size[0]).map(move |x| (x, y, z)))).find(|(x, y, z)| solid[grid.index(*x, *y, *z)]);
    for _ in 0..64 {
        let mut next = depth.clone();
        let mut any = false;
        for z in 1..grid.size[2] - 1 {
            for y in 1..grid.size[1] - 1 {
                for x in 1..grid.size[0] - 1 {
                    let i = grid.index(x, y, z);
                    if depth[i] {
                        let neighbours = [grid.index(x - 1, y, z), grid.index(x + 1, y, z), grid.index(x, y - 1, z), grid.index(x, y + 1, z), grid.index(x, y, z - 1), grid.index(x, y, z + 1)];
                        if neighbours.iter().all(|n| depth[*n]) {
                            any = true;
                            best = Some((x, y, z));
                        } else {
                            next[i] = false;
                        }
                    }
                }
            }
        }
        depth = next;
        if !any {
            break;
        }
    }
    best.map(|(x, y, z)| grid.centre(x, y, z))
}

/// Where a steady solver stops, if its residuals haven't reached their target of 1e-6 first. A run that
/// stops here says so with its result. On air past a cube at Re 100, the target was met at iteration 249,
/// and a target of 1e-4 at iteration 157 gave the same speeds to within 0.5%. So 1e-6 is margin, cheaply
/// bought, and not the cure for anything measured.
const STEADY_ITERATIONS: f64 = 3000.0;

/// A patch of the mesh: name, faces of the design, whether it's a wall.
struct PatchDef {
    name: String,
    faces: Vec<u32>,
    kind: BcKind,
}

fn patches_of(study: &Study, part: &Partition) -> Vec<PatchDef> {
    let mut defs: Vec<PatchDef> = Vec::new();
    let mut used = vec![false; part.faces as usize];
    for (i, c) in study.conditions.iter().enumerate() {
        let name = match c.kind {
            BcKind::VelocityInlet { .. } => format!("inlet{i}"),
            BcKind::PressureOutlet { .. } => format!("outlet{i}"),
            _ => format!("wall{i}"),
        };
        for f in &c.faces {
            if let Some(u) = used.get_mut(f.face as usize) {
                *u = true;
            }
        }
        defs.push(PatchDef { name, faces: c.faces.iter().map(|f| f.face).collect(), kind: c.kind.clone() });
    }
    let rest: Vec<u32> = (0..part.faces).filter(|f| !used[*f as usize]).collect();
    if !rest.is_empty() {
        defs.push(PatchDef { name: "walls".into(), faces: rest, kind: BcKind::Wall });
    }
    defs
}

/// The body as an ASCII multi-solid STL, one solid per patch.
fn stl(part: &Partition, defs: &[PatchDef]) -> String {
    let mut s = String::with_capacity(part.triangles.len() * 200);
    for d in defs {
        let _ = writeln!(s, "solid {}", d.name);
        for (i, f) in part.tri_face.iter().enumerate() {
            if d.faces.contains(f) {
                let [a, b, c] = part.triangle(i);
                let _ = writeln!(s, " facet normal 0 0 0\n  outer loop");
                for p in [a, b, c] {
                    let _ = writeln!(s, "   vertex {} {} {}", p[0], p[1], p[2]);
                }
                let _ = writeln!(s, "  endloop\n endfacet");
            }
        }
        let _ = writeln!(s, "endsolid {}", d.name);
    }
    s
}

fn patch_area(part: &Partition, d: &PatchDef) -> f64 {
    let mut area = 0.0;
    for (i, f) in part.tri_face.iter().enumerate() {
        if d.faces.contains(f) {
            let [a, b, c] = part.triangle(i);
            area += tri_area(a, b, c);
        }
    }
    area
}

/// Which solver and physics a study means.
#[derive(Clone, Copy, PartialEq)]
enum Physics {
    Incompressible,
    Buoyant,
    /// Steady and pressure-based: low to high subsonic speeds.
    Compressible,
    /// Density-based and transient, which holds shocks: high speeds.
    Shocks,
    Multiphase,
}

fn physics_of(study: &Study) -> Result<Physics> {
    Ok(match study.analysis {
        AnalysisId::Incompressible => Physics::Incompressible,
        AnalysisId::ConvectiveHeatTransfer => Physics::Buoyant,
        AnalysisId::MultiPurpose if study.settings.compressible => Physics::Compressible,
        AnalysisId::MultiPurpose => Physics::Incompressible,
        AnalysisId::Compressible => Physics::Shocks,
        AnalysisId::Multiphase => Physics::Multiphase,
        other => bail!("{} isn't run through this path.", crate::catalog::analysis(other).label),
    })
}

/// The rotating zone of a Multi-purpose study: `topoSetDict` and `MRFProperties`. A cylinder around the
/// whole body, a little wider and longer than it. The body's walls turn with it; the tunnel's don't.
fn rotor(case: &mut Case, study: &Study, lo: [f64; 3], hi: [f64; 3], fixed_patches: &str) -> Result<()> {
    let axis = study.settings.axis as usize;
    if axis > 2 {
        bail!("The rotation axis must be x, y or z.");
    }
    let middle: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let radius = 1.2 * 0.5 * ((hi[u] - lo[u]).powi(2) + (hi[v] - lo[v]).powi(2)).sqrt();
    let reach = 0.6 * (hi[axis] - lo[axis]);
    let (mut p1, mut p2) = (middle, middle);
    p1[axis] -= reach;
    p2[axis] += reach;
    let mut direction = [0.0; 3];
    direction[axis] = 1.0;
    let omega = study.settings.rotor_rpm * std::f64::consts::TAU / 60.0;
    case.dict("system", "topoSetDict", "dictionary", &format!("actions ( {{ name rotor; type cellZoneSet; action new; source cylinderToCell; point1 ({} {} {}); point2 ({} {} {}); radius {radius}; }} );\n", p1[0], p1[1], p1[2], p2[0], p2[1], p2[2]));
    case.dict("constant", "MRFProperties", "dictionary", &format!("zone {{ cellZone rotor; active yes; nonRotatingPatches ({fixed_patches}); origin ({} {} {}); axis ({} {} {}); omega {omega}; }}\n", middle[0], middle[1], middle[2], direction[0], direction[1], direction[2]));
    Ok(())
}

/// Builds the whole case.
fn build_case(study: &Study, part: &Partition, defs: &[PatchDef], external: bool, name: &str) -> Result<(Case, f64)> {
    let physics = physics_of(study)?;
    let mut case = Case::new(name);
    let (lo, hi) = bounds(part);
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let length = size[0].max(size[1]).max(size[2]);
    let diagonal = (size[0].powi(2) + size[1].powi(2) + size[2].powi(2)).sqrt();
    let cell = (diagonal * study.settings.element_size * 2.0).max(1e-6);
    let (box_lo, box_hi) = if external {
        ([lo[0] - 2.0 * length, lo[1] - 2.0 * length, lo[2] - 2.0 * length], [hi[0] + 6.0 * length, hi[1] + 2.0 * length, hi[2] + 2.0 * length])
    } else {
        ([lo[0] - 0.1 * length, lo[1] - 0.1 * length, lo[2] - 0.1 * length], [hi[0] + 0.1 * length, hi[1] + 0.1 * length, hi[2] + 0.1 * length])
    };
    let cells: [usize; 3] = std::array::from_fn(|k| (((box_hi[k] - box_lo[k]) / cell).ceil() as usize).max(4));
    let total = cells[0] * cells[1] * cells[2];
    if total > 30_000_000 {
        bail!("The background mesh would have {total} cells. Use a larger element size.");
    }
    let area: f64 = (0..part.triangles.len()).map(|i| {
        let [a, b, c] = part.triangle(i);
        tri_area(a, b, c)
    }).sum();
    let estimate = super::estimated_cells(total, area, cell);
    let sides: [(&str, &str); 6] = if external { [("inletBox", "patch"), ("outletBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch")] } else { [("defaultFaces", "patch"); 6] };
    cases::block_mesh(&mut case, box_lo, box_hi, cells, [1.0, 1.0, 1.0], sides);

    // The mesher: refined to the surface; a point saying which side of it to keep.
    let location = if external { [box_lo[0] + cell * 1.7, box_lo[1] + cell * 1.7, box_lo[2] + cell * 1.7] } else { interior_point(part).ok_or_else(|| anyhow!("No inside found: is this body a closed flow domain?"))? };
    let mut regions = String::new();
    for d in defs {
        let kind = if matches!(d.kind, BcKind::VelocityInlet { .. } | BcKind::PressureOutlet { .. }) { "patch" } else { "wall" };
        let _ = writeln!(regions, "      {} {{ level (2 2); patchInfo {{ type {kind}; }} }}", d.name);
    }
    let mut names = String::new();
    for d in defs {
        let _ = writeln!(names, "      {} {{ name {}; }}", d.name, d.name);
    }
    case.dict(
        "system",
        "snappyHexMeshDict",
        "dictionary",
        &format!(
            "castellatedMesh true;\nsnap true;\naddLayers false;\ngeometry {{ body.stl {{ type triSurfaceMesh; name body; regions {{\n{names}    }} }} }}\ncastellatedMeshControls {{ maxLocalCells 2000000; maxGlobalCells 30000000; minRefinementCells 0; nCellsBetweenLevels 2; resolveFeatureAngle 30; features ({{ file \"body.eMesh\"; level 2; }}); refinementSurfaces {{ body {{ level (2 2); regions {{\n{regions}    }} }} }} refinementRegions {{ }} locationInMesh ({} {} {}); allowFreeStandingZoneFaces true; }}\nsnapControls {{ nSmoothPatch 3; tolerance 2.0; nSolveIter 50; nRelaxIter 5; nFeatureSnapIter 10; implicitFeatureSnap false; explicitFeatureSnap true; multiRegionFeatureSnap false; }}\naddLayersControls {{ relativeSizes true; layers {{ }} expansionRatio 1.2; finalLayerThickness 0.5; minThickness 0.1; nGrow 0; featureAngle 60; nRelaxIter 3; nSmoothSurfaceNormals 1; nSmoothNormals 3; nSmoothThickness 10; maxFaceThicknessRatio 0.5; maxThicknessToMedialRatio 0.3; minMedianAxisAngle 90; nBufferCellsNoExtrude 0; nLayerIter 50; }}\nmeshQualityControls {{ maxNonOrtho 65; maxBoundarySkewness 20; maxInternalSkewness 4; maxConcave 80; minVol 1e-30; minTetQuality 1e-30; minArea -1; minTwist 0.02; minDeterminant 0.001; minFaceWeight 0.02; minVolRatio 0.01; minTriangleTwist -1; nSmoothScale 4; errorReduction 0.75; relaxed {{ maxNonOrtho 75; }} }}\nmergeTolerance 1e-6;\n",
            location[0], location[1], location[2]
        ),
    );
    case.dict("system", "surfaceFeatureExtractDict", "dictionary", "body.stl { extractionMethod extractFromSurface; includedAngle 150; subsetFeatures { nonManifoldEdges no; openEdges yes; } writeObj no; }\n");
    case.files.insert("constant/triSurface/body.stl".into(), stl(part, defs));
    case.dict("system", "decomposeParDict", "dictionary", "numberOfSubdomains 8;\nmethod scotch;\n");

    // The fields.
    let f = &study.fluid;
    let nu = f.viscosity / f.density;
    let speed = if external { study.settings.free_stream } else { study.conditions.iter().find_map(|c| if let BcKind::VelocityInlet { speed } = c.kind { Some(speed) } else { None }).unwrap_or(1.0) };
    // A spinning body drives the flow as much as the free stream does: its tip speed, over its width.
    let (ru, rv) = (((study.settings.axis as usize) + 1) % 3, ((study.settings.axis as usize) + 2) % 3);
    let tip_radius = 0.5 * (size[ru].powi(2) + size[rv].powi(2)).sqrt();
    let tip_speed = study.settings.rotor_rpm.abs() * std::f64::consts::TAU / 60.0 * tip_radius;
    let re = (speed * length).max(tip_speed * 2.0 * tip_radius) / nu;
    let turbulent = re > 2300.0 && physics != Physics::Shocks;
    let k = (1.5 * (0.05 * speed).powi(2)).max(1e-8);
    let omega = k.sqrt() / (0.09f64.powf(0.25) * (0.07 * length).max(1e-6));

    // Boundary entries per patch.
    let mut u_patches: Vec<(String, String)> = Vec::new();
    let mut p_patches: Vec<(String, String)> = Vec::new();
    let mut k_patches: Vec<(String, String)> = Vec::new();
    let mut w_patches: Vec<(String, String)> = Vec::new();
    let mut nut_patches: Vec<(String, String)> = Vec::new();
    let mut t_patches: Vec<(String, String)> = Vec::new();
    let t_ref = 293.15;
    let mut wall = |name: &str, thermal: String, u: &mut Vec<(String, String)>, p: &mut Vec<(String, String)>| {
        u.push((name.into(), "type noSlip;".into()));
        p.push((name.into(), "type zeroGradient;".into()));
        k_patches.push((name.into(), format!("type kqRWallFunction; value uniform {k};")));
        w_patches.push((name.into(), format!("type omegaWallFunction; value uniform {omega};")));
        nut_patches.push((name.into(), "type nutkWallFunction; value uniform 0;".into()));
        t_patches.push((name.into(), thermal));
    };
    let mut open: Vec<(String, String, String, String)> = Vec::new();
    for d in defs {
        match &d.kind {
            BcKind::VelocityInlet { speed } => open.push((d.name.clone(), format!("type flowRateInletVelocity; volumetricFlowRate {}; value uniform (0 0 0);", speed * patch_area(part, d)), "type zeroGradient;".into(), format!("type fixedValue; value uniform {t_ref};"))),
            BcKind::PressureOutlet { pressure } => open.push((d.name.clone(), "type zeroGradient;".into(), format!("type fixedValue; value uniform {};", pressure / f.density), "type zeroGradient;".into())),
            BcKind::Temperature { value } => wall(&d.name, format!("type fixedValue; value uniform {};", value + 273.15), &mut u_patches, &mut p_patches),
            _ => wall(&d.name, "type zeroGradient;".into(), &mut u_patches, &mut p_patches),
        }
    }
    let mut w_open = |name: &str, t: bool, kp: &mut Vec<(String, String)>, wp: &mut Vec<(String, String)>, nt: &mut Vec<(String, String)>, tp: &mut Vec<(String, String)>| {
        kp.push((name.into(), if t { format!("type fixedValue; value uniform {k};") } else { "type zeroGradient;".into() }));
        wp.push((name.into(), if t { format!("type fixedValue; value uniform {omega};") } else { "type zeroGradient;".into() }));
        nt.push((name.into(), "type calculated; value uniform 0;".into()));
        tp.push((name.into(), if t { format!("type fixedValue; value uniform {t_ref};") } else { "type zeroGradient;".into() }));
    };
    for (name, u, p, _t) in &open {
        u_patches.push((name.clone(), u.clone()));
        p_patches.push((name.clone(), p.clone()));
        w_open(name, name.starts_with("inlet"), &mut k_patches, &mut w_patches, &mut nut_patches, &mut t_patches);
    }
    if external {
        u_patches.push(("inletBox".into(), format!("type fixedValue; value uniform ({speed} 0 0);")));
        p_patches.push(("inletBox".into(), "type zeroGradient;".into()));
        w_open("inletBox", true, &mut k_patches, &mut w_patches, &mut nut_patches, &mut t_patches);
        u_patches.push(("outletBox".into(), "type zeroGradient;".into()));
        p_patches.push(("outletBox".into(), "type fixedValue; value uniform 0;".into()));
        w_open("outletBox", false, &mut k_patches, &mut w_patches, &mut nut_patches, &mut t_patches);
        u_patches.push(("sidesBox".into(), "type slip;".into()));
        p_patches.push(("sidesBox".into(), "type slip;".into()));
        for list in [&mut k_patches, &mut w_patches, &mut nut_patches, &mut t_patches] {
            list.push(("sidesBox".into(), "type slip;".into()));
        }
    }
    if !external {
        for list in [&mut u_patches, &mut p_patches, &mut k_patches, &mut w_patches, &mut nut_patches, &mut t_patches] {
            list.push(("defaultFaces".into(), "type zeroGradient;".into()));
        }
    }
    let view = |v: &[(String, String)]| -> Vec<(String, String)> { v.to_vec() };
    let write_field = |case: &mut Case, name: &str, class: &str, d: &str, internal: &str, list: Vec<(String, String)>| {
        let refs: Vec<(&str, &str)> = list.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        cases::field(case, name, class, d, internal, &refs);
    };
    let initial = if external { format!("({speed} 0 0)") } else { "(0 0 0)".to_string() };
    write_field(&mut case, "U", "volVectorField", dim::VELOCITY, &initial, view(&u_patches));
    write_field(&mut case, "k", "volScalarField", "[0 2 -2 0 0 0 0]", &k.to_string(), view(&k_patches));
    write_field(&mut case, "omega", "volScalarField", "[0 0 -1 0 0 0 0]", &omega.to_string(), view(&w_patches));
    write_field(&mut case, "nut", "volScalarField", "[0 2 -1 0 0 0 0]", "0", view(&nut_patches));

    if turbulent {
        cases::rans(&mut case, "kOmegaSST");
    } else {
        cases::laminar(&mut case);
    }
    if study.analysis == AnalysisId::MultiPurpose && study.settings.rotor_rpm != 0.0 {
        if !external {
            bail!("A rotating zone works on a body standing in a flow. With an inlet picked, the body is the flow's passage, and there's nothing in it to turn.");
        }
        rotor(&mut case, study, lo, hi, "inletBox outletBox sidesBox")?;
        // The body's walls turn with the zone, and say so: a plain no-slip wall reads back as still.
        let axis = study.settings.axis as usize;
        let origin: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
        let mut direction = [0.0; 3];
        direction[axis] = 1.0;
        let omega_rad = study.settings.rotor_rpm * std::f64::consts::TAU / 60.0;
        let turning: Vec<(String, String)> = u_patches.iter().map(|(n, t)| (n.clone(), if t.contains("noSlip") { format!("type rotatingWallVelocity; origin ({} {} {}); axis ({} {} {}); omega {omega_rad}; value uniform (0 0 0);", origin[0], origin[1], origin[2], direction[0], direction[1], direction[2]) } else { t.clone() })).collect();
        write_field(&mut case, "U", "volVectorField", dim::VELOCITY, &initial, turning);
    }
    let div_turb = "div(phi,k) bounded Gauss upwind; div(phi,omega) bounded Gauss upwind;";
    match physics {
        Physics::Incompressible => {
            write_field(&mut case, "p", "volScalarField", dim::KINEMATIC_PRESSURE, "0", view(&p_patches));
            cases::transport(&mut case, nu);
            cases::control_dict(&mut case, &Control { application: "simpleFoam", end_time: STEADY_ITERATIONS, dt: 1.0, write_interval: STEADY_ITERATIONS, adjustable: None, extra: "" });
            case.dict("system", "fvSchemes", "dictionary", &format!("ddtSchemes {{ default steadyState; }}\ngradSchemes {{ default Gauss linear; limited cellLimited Gauss linear 1; grad(U) $limited; }}\ndivSchemes {{ default none; div(phi,U) bounded Gauss linearUpwind limited; {div_turb} div((nuEff*dev2(T(grad(U))))) Gauss linear; }}\nlaplacianSchemes {{ default Gauss linear limited 0.5; }}\ninterpolationSchemes {{ default linear; }}\nsnGradSchemes {{ default limited 0.5; }}\nwallDist {{ method meshWave; }}\n"));
            case.dict("system", "fvSolution", "dictionary", "solvers { p { solver GAMG; smoother GaussSeidel; tolerance 1e-7; relTol 0.05; } \"(U|k|omega)\" { solver smoothSolver; smoother symGaussSeidel; tolerance 1e-7; relTol 0.1; } }\nSIMPLE { nNonOrthogonalCorrectors 1; consistent yes; pRefCell 0; pRefValue 0; residualControl { p 1e-6; U 1e-6; \"(k|omega)\" 1e-6; } }\nrelaxationFactors { equations { U 0.7; \"(k|omega)\" 0.7; } }\n");
        }
        Physics::Multiphase => {
            // Water up to a share of the body's height, air above, under gravity along -z.
            let fill = lo[2] + study.settings.fill_fraction.clamp(0.0, 1.0) * size[2];
            let (water, air) = (crate::materials::fluids().remove(1), crate::materials::fluids().remove(0));
            let sigma = 0.072;
            // Courant numbers don't protect a sharp interface: capillary waves limit the step to
            // sqrt((rho1 + rho2) h^3 / (4 pi sigma)). Above it, results go wrong with no other sign.
            let h = cell / 4.0;
            let capillary = ((water.density + air.density) * h.powi(3) / (4.0 * std::f64::consts::PI * sigma)).sqrt();
            let extra = format!("maxDeltaT {capillary};\nfunctions {{ }}\n");
            cases::control_dict(&mut case, &Control { application: "interFoam", end_time: study.settings.duration, dt: capillary.min(1e-4), write_interval: study.settings.duration, adjustable: Some((0.3, 0.3)), extra: &extra });
            case.dict("constant", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 -9.81);\n");
            case.dict("constant", "transportProperties", "dictionary", &format!("phases (water air);\nwater {{ transportModel Newtonian; nu {}; rho {}; }}\nair {{ transportModel Newtonian; nu {}; rho {}; }}\nsigma {sigma};\n", water.viscosity / water.density, water.density, air.viscosity / air.density, air.density));
            case.dict("system", "setFieldsDict", "dictionary", &format!("defaultFieldValues (volScalarFieldValue alpha.water 0);\nregions ( boxToCell {{ box ({} {} {}) ({} {} {fill}); fieldValues (volScalarFieldValue alpha.water 1); }} );\n", box_lo[0], box_lo[1], box_lo[2], box_hi[0], box_hi[1]));
            case.dict("system", "fvSchemes", "dictionary", &format!("ddtSchemes {{ default Euler; }}\ngradSchemes {{ default Gauss linear; }}\ndivSchemes {{ default none; div(rhoPhi,U) Gauss linearUpwind grad(U); div(phi,alpha) Gauss vanLeer; div(phirb,alpha) Gauss linear; {div_turb} div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }}\nlaplacianSchemes {{ default Gauss linear limited 0.5; }}\ninterpolationSchemes {{ default linear; }}\nsnGradSchemes {{ default limited 0.5; }}\nwallDist {{ method meshWave; }}\n"));
            // cAlpha: full interface compression leaves surface tension too few cells to act through, and
            // measured on a droplet its pressure stays 12% low however fine the mesh. At 0.25, the droplet
            // is 0.75% off at 80 cells across and the rising bubble within 2.5%, at the cost of a slightly
            // softer free surface.
            case.dict("system", "fvSolution", "dictionary", "solvers { \"alpha.water.*\" { nAlphaCorr 2; nAlphaSubCycles 1; cAlpha 0.25; MULESCorr yes; nLimiterIter 5; solver smoothSolver; smoother symGaussSeidel; tolerance 1e-8; relTol 0; } \"pcorr.*\" { solver PCG; preconditioner DIC; tolerance 1e-5; relTol 0; } p_rgh { solver PCG; preconditioner DIC; tolerance 1e-7; relTol 0.05; } p_rghFinal { $p_rgh; relTol 0; } \"(U|k|omega).*\" { solver smoothSolver; smoother symGaussSeidel; tolerance 1e-7; relTol 0; } }\nPIMPLE { momentumPredictor no; nOuterCorrectors 1; nCorrectors 3; nNonOrthogonalCorrectors 1; pRefPoint (0 0 0); pRefValue 0; }\nrelaxationFactors { equations { \".*\" 1; } }\n");
            let rgh: Vec<(String, String)> = p_patches.iter().map(|(n, t)| (n.clone(), if t.contains("zeroGradient") { "type fixedFluxPressure; value uniform 0;".to_string() } else { t.clone() })).collect();
            write_field(&mut case, "p_rgh", "volScalarField", dim::PRESSURE, "0", rgh);
            let alpha: Vec<(String, String)> = p_patches.iter().map(|(n, _)| (n.clone(), if n.starts_with("inlet") { "type fixedValue; value uniform 1;".to_string() } else { "type zeroGradient;".to_string() })).collect();
            write_field(&mut case, "alpha.water", "volScalarField", dim::NONE, "0", alpha);
        }
        Physics::Shocks => {
            // As verified against Sod and the oblique shock: inviscid, with walls that slip.
            let (p0, gamma, gas) = (101325.0f64, 1.4f64, 8314.46 / 28.96);
            let sound = (gamma * gas * t_ref).sqrt();
            let supersonic = speed > sound;
            let is_inflow = |n: &str| n.starts_with("inlet");
            let is_outflow = |n: &str| n.starts_with("outlet");
            let slip_or = |n: &str, other: String| if is_inflow(n) || is_outflow(n) || n == "defaultFaces" { other } else { "type slip;".to_string() };
            // Long enough for the flow to cross the domain five times, or for sound to if it's slower,
            // after the inflow has built up.
            let reach = box_hi[0] - box_lo[0];
            let ramp = reach / speed.max(sound);
            let end_time = ramp + 5.0 * reach / speed.max(sound);
            // The inflow builds up from rest. Started at full speed, the air leaving a rear face opens a
            // vacuum at once, its temperature goes below zero, and the solver stops with a square root
            // of a negative number.
            let ramped = |text: &str| -> String {
                if let Some(rest) = text.split("volumetricFlowRate ").nth(1)
                    && let Some(q) = rest.split(';').next()
                {
                    return format!("type flowRateInletVelocity; volumetricFlowRate table ((0 0) ({ramp} {q}) (1e6 {q})); value uniform (0 0 0);");
                }
                format!("type uniformFixedValue; uniformValue table ((0 (0 0 0)) ({ramp} ({speed} 0 0)) (1e6 ({speed} 0 0)));")
            };
            let u: Vec<(String, String)> = u_patches.iter().map(|(n, t)| (n.clone(), if is_inflow(n) { ramped(t) } else { slip_or(n, t.clone()) })).collect();
            let wave = format!("type waveTransmissive; field p; psi thermo:psi; gamma {gamma}; fieldInf {p0}; lInf {length}; value uniform {p0};");
            let p_list: Vec<(String, String)> = p_patches
                .iter()
                .map(|(n, _)| {
                    let text = if is_inflow(n) && supersonic {
                        format!("type fixedValue; value uniform {p0};")
                    } else if is_outflow(n) {
                        // Subsonic while the flow builds up, whatever it ends as.
                        wave.clone()
                    } else if n == "sidesBox" {
                        "type slip;".to_string()
                    } else {
                        "type zeroGradient;".to_string()
                    };
                    (n.clone(), text)
                })
                .collect();
            let temperature: Vec<(String, String)> = t_patches.iter().map(|(n, t)| (n.clone(), if t.contains("fixedValue") && !is_inflow(n) { "type zeroGradient;".to_string() } else { t.clone() })).collect();
            write_field(&mut case, "U", "volVectorField", dim::VELOCITY, "(0 0 0)", u);
            write_field(&mut case, "p", "volScalarField", dim::PRESSURE, &p0.to_string(), p_list);
            write_field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t_ref.to_string(), temperature);
            let cp = gas * gamma / (gamma - 1.0);
            case.dict("constant", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type hePsiThermo; mixture pureMixture; transport const; thermo hConst; equationOfState perfectGas; specie specie; energy sensibleInternalEnergy; }}\nmixture {{ specie {{ molWeight 28.96; }} thermodynamics {{ Cp {cp}; Hf 0; }} transport {{ mu 0; Pr 0.71; }} }}\n"));
            cases::control_dict(&mut case, &Control { application: "rhoCentralFoam", end_time, dt: 1e-8, write_interval: end_time, adjustable: Some((0.2, 1.0)), extra: "" });
            case.dict("system", "fvSchemes", "dictionary", "fluxScheme Kurganov;\nddtSchemes { default Euler; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; div(tauMC) Gauss linear; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; reconstruct(rho) vanLeer; reconstruct(U) vanLeerV; reconstruct(T) vanLeer; }\nsnGradSchemes { default corrected; }\n");
            case.dict("system", "fvSolution", "dictionary", "solvers { \"(rho|rhoU|rhoE)\" { solver diagonal; } U { solver smoothSolver; smoother GaussSeidel; nSweeps 2; tolerance 1e-10; relTol 0; } h { $U; tolerance 1e-10; relTol 0; } e { $U; tolerance 1e-10; relTol 0; } }\n");
        }
        Physics::Buoyant | Physics::Compressible => {
            let p0 = 101325.0;
            let abs_p: Vec<(String, String)> = p_patches.iter().map(|(n, t)| (n.clone(), if t.contains("fixedValue") { format!("type fixedValue; value uniform {p0};") } else if t.contains("slip") { "type slip;".to_string() } else { "type zeroGradient;".to_string() })).collect();
            write_field(&mut case, "T", "volScalarField", dim::TEMPERATURE, &t_ref.to_string(), view(&t_patches));
            let thermo = if physics == Physics::Buoyant { "heRhoThermo" } else { "hePsiThermo" };
            case.dict("constant", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type {thermo}; mixture pureMixture; transport const; thermo hConst; equationOfState perfectGas; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 28.96; }} thermodynamics {{ Cp {}; Hf 0; }} transport {{ mu {}; Pr {}; }} }}\n", f.specific_heat, f.viscosity, f.viscosity * f.specific_heat / f.conductivity));
            let div_energy = "div(phi,h) bounded Gauss upwind; div(phi,K) bounded Gauss upwind; div(phid,p) Gauss upwind; div(phiMRF,p) Gauss upwind; div(phi,Ekp) bounded Gauss upwind;";
            let application = if physics == Physics::Buoyant { "buoyantSimpleFoam" } else { "rhoSimpleFoam" };
            cases::control_dict(&mut case, &Control { application, end_time: STEADY_ITERATIONS, dt: 1.0, write_interval: STEADY_ITERATIONS, adjustable: None, extra: "" });
            case.dict("system", "fvSchemes", "dictionary", &format!("ddtSchemes {{ default steadyState; }}\ngradSchemes {{ default Gauss linear; limited cellLimited Gauss linear 1; grad(U) $limited; }}\ndivSchemes {{ default none; div(phi,U) bounded Gauss linearUpwind limited; {div_turb} {div_energy} div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }}\nlaplacianSchemes {{ default Gauss linear limited 0.5; }}\ninterpolationSchemes {{ default linear; }}\nsnGradSchemes {{ default limited 0.5; }}\nwallDist {{ method meshWave; }}\n"));
            if physics == Physics::Buoyant {
                case.dict("constant", "g", "uniformDimensionedVectorField", "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 -9.81);\n");
                let rgh: Vec<(String, String)> = abs_p.iter().map(|(n, t)| (n.clone(), if t.contains("zeroGradient") { format!("type fixedFluxPressure; value uniform {p0};") } else { t.clone() })).collect();
                write_field(&mut case, "p_rgh", "volScalarField", dim::PRESSURE, &p0.to_string(), rgh);
                let calc: Vec<(String, String)> = abs_p.iter().map(|(n, _)| (n.clone(), format!("type calculated; value uniform {p0};"))).collect();
                write_field(&mut case, "p", "volScalarField", dim::PRESSURE, &p0.to_string(), calc);
                case.dict("system", "fvSolution", "dictionary", "solvers { p_rgh { solver GAMG; smoother DIC; tolerance 1e-7; relTol 0.05; } \"(U|h|k|omega)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-7; relTol 0.1; } }\nSIMPLE { nNonOrthogonalCorrectors 1; pRefCell 0; pRefValue 1e5; residualControl { p_rgh 1e-6; U 1e-6; h 1e-6; } }\nrelaxationFactors { fields { rho 1.0; p_rgh 0.3; } equations { U 0.6; h 0.5; \"(k|omega)\" 0.5; } }\n");
            } else {
                write_field(&mut case, "p", "volScalarField", dim::PRESSURE, &p0.to_string(), abs_p);
                case.dict("system", "fvSolution", "dictionary", "solvers { p { solver GAMG; smoother GaussSeidel; tolerance 1e-7; relTol 0.05; } \"(U|h|k|omega)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-7; relTol 0.1; } }\nSIMPLE { nNonOrthogonalCorrectors 1; pMinFactor 0.5; pMaxFactor 2; residualControl { p 1e-6; U 1e-6; h 1e-6; } }\nrelaxationFactors { fields { p 0.3; rho 0.1; } equations { U 0.6; h 0.5; \"(k|omega)\" 0.5; } }\n");
            }
        }
    }
    Ok((case, estimate))
}

/// The steps that make a study's mesh.
const MESH_STEPS: [&str; 4] = ["blockMesh > log.blockMesh", "surfaceFeatureExtract > log.feature", "snappyHexMesh -overwrite > log.snappy", "checkMesh > log.checkMesh || true"];

/// Compares what the mesher made with the geometry it was given, and stops if they differ.
fn audit_mesh(run: &super::Run, part: &Partition, defs: &[PatchDef]) -> Result<Vec<mesh_audit::Patch>> {
    let audit_input = (run.read_remote("constant/polyMesh/points")?, run.read_remote("constant/polyMesh/faces")?, run.read_remote("constant/polyMesh/boundary")?);
    let found = mesh_audit::patches(&audit_input.0, &audit_input.1, &audit_input.2)?;
    let expected: Vec<(String, f64)> = defs.iter().map(|d| (d.name.clone(), patch_area(part, d))).collect();
    let problems = mesh_audit::audit(&found, &expected, 0.1);
    if !problems.is_empty() {
        bail!("The mesh isn't right for this geometry, so it wasn't solved:\n{}", problems.join("\n"));
    }
    Ok(found)
}

/// The solver a study's physics uses.
fn solver_of(study: &Study) -> Result<&'static str> {
    Ok(match physics_of(study)? {
        Physics::Incompressible => "simpleFoam",
        Physics::Buoyant => "buoyantSimpleFoam",
        Physics::Compressible => "rhoSimpleFoam",
        Physics::Shocks => "rhoCentralFoam",
        Physics::Multiphase => "interFoam",
    })
}

/// Runs `study` on the body's surface `part`.
pub fn run_study(study: &Study, part: &Partition, ctx: &JobCtx) -> Result<ResultSet> {
    crate::check::before_run(study, part)?;
    if study.analysis == AnalysisId::ConjugateHeatTransfer {
        return super::cht::run_study(study, part, ctx);
    }
    let external = !study.conditions.iter().any(|c| matches!(c.kind, BcKind::VelocityInlet { .. }));
    if !external && !part.is_closed() {
        bail!("For an internal flow the body must be a closed solid: the flow fills it.");
    }
    let defs = patches_of(study, part);
    let name = format!("job{}", ctx.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").replace(|c: char| !c.is_ascii_alphanumeric(), ""));
    let (case, cells) = build_case(study, part, &defs, external, &name)?;
    super::check_memory(&crate::provision::distro(), cells)?;
    let solver = solver_of(study)?;

    // Meshing: a step of its own, so its result can be audited before the solver starts.
    ctx.set("Meshing the flow domain", 0.05);
    let local = ctx.dir.join("case");
    let horizon = if solver == "rhoCentralFoam" || solver == "interFoam" { case.files.get("system/controlDict").and_then(|t| t.split("endTime ").nth(1)).and_then(|t| t.split(';').next()).and_then(|t| t.parse().ok()).unwrap_or(1.0) } else { STEADY_ITERATIONS };
    let mut stage = |line: &str| {
        ctx.log(line);
        if let Some(t) = super::parse_time(line) {
            ctx.set("Solving the flow", (0.25 + 0.7 * (t / horizon)).min(0.95) as f32);
        }
    };
    let run = super::run(&case, &local, &MESH_STEPS, &ctx.cancel, &mut stage).context("meshing")?;
    let found = audit_mesh(&run, part, &defs)?;
    ctx.log(&format!("patches: {found:?}"));

    ctx.set("Solving the flow", 0.25);
    let mut set_fields = String::new();
    if solver == "interFoam" {
        set_fields.push_str("setFields > log.setFields\n");
    }
    if study.analysis == AnalysisId::MultiPurpose && study.settings.rotor_rpm != 0.0 {
        set_fields.push_str("topoSet > log.topoSet\n");
    }
    let script = format!("{}\nset -e\ncd {}\nrm -rf processor*\n{set_fields}decomposePar > log.decompose\n({}; mpirun --oversubscribe -np 8 {solver} -parallel)\nreconstructPar -latestTime > log.reconstruct\n", crate::provision::foam_env(), run.remote, crate::jobs::wsl::RANK_LIMIT);
    let mut fatal = None;
    let mut convergence = super::Convergence::default();
    let outcome = run.distro.run_supervised(&run.remote, &[], &["/bin/bash", "-c", &script], &ctx.cancel, |line| {
        super::note_fatal(line, &mut fatal);
        convergence.line(line);
        stage(line);
    });
    if let Some(f) = fatal {
        bail!("OpenFOAM: {f}");
    }
    outcome.context("running the solver")?;

    ctx.set("Reading results", 0.96);
    let (time, grid) = run.latest(None)?;
    let mut fields: Vec<_> = grid.point_fields.iter().filter(|f| f.location == Location::Node).cloned().collect();
    fields.retain(|f| f.len() == grid.points.len());
    let mesh = grid.to_result_mesh();
    // Marching in time, a run ends when its time does; only a steady one has something to settle.
    let steady = matches!(solver, "simpleFoam" | "buoyantSimpleFoam" | "rhoSimpleFoam");
    let plots = if steady { vec![convergence.plot()] } else { Vec::new() };
    let mut set = ResultSet { mesh, steps: vec![ResultStep { kind: StepKind::Time, value: time, fields }], plots };
    if external {
        crate::results_cut::remove_domain_box(&mut set);
    }
    let (lo, hi) = bounds(part);
    crate::results_cut::add_cut_plane(&mut set, [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0], [0.0, 1.0, 0.0]);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Cancel;
    use crate::study::{Condition, FaceRef};

    /// A 1 m cube with faces numbered by side: -x, +x, -y, +y, -z, +z.
    fn cube() -> Partition {
        let positions = vec![[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.], [0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]];
        let quads = [[0u32, 4, 7, 3], [1, 2, 6, 5], [0, 1, 5, 4], [3, 7, 6, 2], [0, 3, 2, 1], [4, 5, 6, 7]];
        let mut triangles = Vec::new();
        let mut tri_face = Vec::new();
        for (face, q) in quads.iter().enumerate() {
            triangles.extend([[q[0], q[1], q[2]], [q[0], q[2], q[3]]]);
            tri_face.extend([face as u32, face as u32]);
        }
        Partition { name: "cube".into(), positions, triangles, tri_face, faces: 6 }
    }

    fn flow_through(part: &Partition) -> Study {
        let mut s = Study::new("t".into(), AnalysisId::Incompressible);
        s.settings.element_size = 0.05;
        s.conditions.push(Condition { kind: BcKind::VelocityInlet { speed: 0.01 }, faces: vec![FaceRef { face: 0, point: [0.0, 0.25, 0.75], second: false }] });
        s.conditions.push(Condition { kind: BcKind::PressureOutlet { pressure: 0.0 }, faces: vec![FaceRef { face: 1, point: [1.0, 0.75, 0.25], second: false }] });
        assert!(crate::check::before_run(&s, part).is_ok());
        s
    }

    #[test]
    fn the_memory_estimate_grows_with_the_mesh() {
        assert!(super::super::estimated_cells(1000, 1.0, 0.1) > 1000.0);
        assert!(super::super::memory_needed(1e7) > super::super::memory_needed(1e5));
    }

    /// The real mesher on a real hole: the closed cube meshes clean, and the same cube with a wall gone
    /// leaks out into the background mesh. The audit must tell them apart.
    #[test]
    #[ignore = "needs the solver environment"]
    fn sim_the_audit_catches_a_real_leak() {
        let closed = cube();
        let study = flow_through(&closed);
        let defs = patches_of(&study, &closed);
        let local = std::env::temp_dir().join("sense-sim-leak-test");

        let (case, _) = build_case(&study, &closed, &defs, false, "leakclosed").unwrap();
        let run = crate::foam::run(&case, &local.join("closed"), &MESH_STEPS, &Cancel::default(), |_| {}).unwrap();
        let found = audit_mesh(&run, &closed, &defs).expect("a closed domain passes the audit");
        println!("closed: {found:?}");

        // The top wall's triangles are dropped from what the mesher sees, and nothing else changes.
        let mut holed = closed.clone();
        let (triangles, tri_face): (Vec<_>, Vec<_>) = closed.triangles.iter().zip(&closed.tri_face).filter(|(_, f)| **f != 5).map(|(t, f)| (*t, *f)).unzip();
        holed.triangles = triangles;
        holed.tri_face = tri_face;
        assert!(!holed.is_closed());
        let (mut case, _) = build_case(&study, &closed, &defs, false, "leakopen").unwrap();
        case.files.insert("constant/triSurface/body.stl".into(), stl(&holed, &defs));
        let run = crate::foam::run(&case, &local.join("open"), &MESH_STEPS, &Cancel::default(), |_| {}).unwrap();
        let caught = audit_mesh(&run, &holed, &defs).expect_err("a leak must not pass the audit").to_string();
        println!("open: {caught}");
        assert!(caught.contains("leaked"), "{caught}");
    }
}
