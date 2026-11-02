//! Conjugate heat transfer on a body-fitted mesh: one closed body standing in a stream of air. The mesh
//! is cut along the body's surface into a solid region and a fluid region, and the two are solved
//! together. The power of the study's heat-flux conditions is released evenly through the body, as the
//! voxel solver does it, and the run checks that the air carries that power away.

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use super::cases::{self, Control, dim};
use super::{Case, Run};
use crate::geometry::Partition;
use crate::jobs::JobCtx;
use crate::results::{Field, Location, Plot, ResultMesh, ResultSet, ResultStep, StepKind};
use crate::results_vtk::Grid;
use crate::study::{BcKind, Study};

/// Temperature of the incoming air and the body at the start, in K, when the study's isn't used.
const KELVIN: f64 = 273.15;

/// The heat going in: each heat flux times the area of the faces it's on.
pub fn power_of(study: &Study, part: &Partition) -> f64 {
    let areas = part.face_areas();
    let mut power = 0.0;
    for c in &study.conditions {
        if let BcKind::HeatFlux { flux } = c.kind {
            power += c.faces.iter().map(|f| flux * areas.get(f.face as usize).copied().unwrap_or(0.0)).sum::<f64>();
        }
    }
    power
}

fn one_solid_stl(part: &Partition) -> String {
    let mut s = String::with_capacity(part.triangles.len() * 200);
    s.push_str("solid body\n");
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        s.push_str(" facet normal 0 0 0\n  outer loop\n");
        for p in [a, b, c] {
            let _ = writeln!(s, "   vertex {} {} {}", p[0], p[1], p[2]);
        }
        s.push_str("  endloop\n endfacet\n");
    }
    s.push_str("endsolid body\n");
    s
}

/// One region's field file.
fn region_field(case: &mut Case, region: &str, name: &str, class: &str, dimensions: &str, internal: &str, patches: &[(&str, String)]) {
    let mut body = format!("dimensions {dimensions};\ninternalField uniform {internal};\nboundaryField\n{{\n");
    for (p, text) in patches {
        let _ = writeln!(body, " {p} {{ {text} }}");
    }
    body.push_str("}\n");
    case.dict(&format!("0/{region}"), name, class, &body);
}

/// Builds the case. Returns it with the iterations it runs for, and about how many cells it will have.
fn build(study: &Study, part: &Partition, power: f64, name: &str) -> Result<(Case, f64, f64)> {
    let mut case = Case::new(name);
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let length = size[0].max(size[1]).max(size[2]);
    let diagonal = (size[0].powi(2) + size[1].powi(2) + size[2].powi(2)).sqrt();
    let cell = (diagonal * study.settings.element_size * 2.0).max(1e-6);
    let (box_lo, box_hi) = ([lo[0] - 2.0 * length, lo[1] - 2.0 * length, lo[2] - 2.0 * length], [hi[0] + 6.0 * length, hi[1] + 2.0 * length, hi[2] + 2.0 * length]);
    let cells: [usize; 3] = std::array::from_fn(|k| (((box_hi[k] - box_lo[k]) / cell).ceil() as usize).max(4));
    let total = cells[0] * cells[1] * cells[2];
    if total > 8_000_000 {
        bail!("The background mesh would have {total} cells. Use a larger element size.");
    }
    cases::block_mesh(&mut case, box_lo, box_hi, cells, [1.0, 1.0, 1.0], [("inletBox", "patch"), ("outletBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch"), ("sidesBox", "patch")]);
    let area: f64 = part.face_areas().iter().sum();
    let estimate = super::estimated_cells(total, area, cell);
    let air_point = [box_lo[0] + cell * 1.7, box_lo[1] + cell * 1.7, box_lo[2] + cell * 1.7];
    case.dict(
        "system",
        "snappyHexMeshDict",
        "dictionary",
        &format!(
            "castellatedMesh true;\nsnap true;\naddLayers false;\ngeometry {{ body.stl {{ type triSurfaceMesh; name body; }} }}\ncastellatedMeshControls {{ maxLocalCells 4000000; maxGlobalCells 20000000; minRefinementCells 0; nCellsBetweenLevels 2; resolveFeatureAngle 30; features ({{ file \"body.eMesh\"; level 2; }}); refinementSurfaces {{ body {{ level (2 2); faceZone solidFaces; cellZone solid; cellZoneInside inside; }} }} refinementRegions {{ }} locationInMesh ({} {} {}); allowFreeStandingZoneFaces false; }}\nsnapControls {{ nSmoothPatch 3; tolerance 2.0; nSolveIter 50; nRelaxIter 5; nFeatureSnapIter 10; implicitFeatureSnap false; explicitFeatureSnap true; multiRegionFeatureSnap false; }}\naddLayersControls {{ relativeSizes true; layers {{ }} expansionRatio 1.2; finalLayerThickness 0.5; minThickness 0.1; nGrow 0; featureAngle 60; nRelaxIter 3; nSmoothSurfaceNormals 1; nSmoothNormals 3; nSmoothThickness 10; maxFaceThicknessRatio 0.5; maxThicknessToMedialRatio 0.3; minMedianAxisAngle 90; nBufferCellsNoExtrude 0; nLayerIter 50; }}\nmeshQualityControls {{ maxNonOrtho 65; maxBoundarySkewness 20; maxInternalSkewness 4; maxConcave 80; minVol 1e-30; minTetQuality 1e-30; minArea -1; minTwist 0.02; minDeterminant 0.001; minFaceWeight 0.02; minVolRatio 0.01; minTriangleTwist -1; nSmoothScale 4; errorReduction 0.75; relaxed {{ maxNonOrtho 75; }} }}\nmergeTolerance 1e-6;\n",
            air_point[0], air_point[1], air_point[2]
        ),
    );
    case.dict("system", "surfaceFeatureExtractDict", "dictionary", "body.stl { extractionMethod extractFromSurface; includedAngle 150; subsetFeatures { nonManifoldEdges no; openEdges yes; } writeObj no; }\n");
    case.files.insert("constant/triSurface/body.stl".into(), one_solid_stl(part));
    case.dict("system", "decomposeParDict", "dictionary", "numberOfSubdomains 8;\nmethod scotch;\n");
    for region in ["air", "solid"] {
        case.dict(&format!("system/{region}"), "decomposeParDict", "dictionary", "numberOfSubdomains 8;\nmethod scotch;\n");
    }
    // splitMeshRegions names the part in no zone after the zones it isn't: renamed below to "air".
    case.dict("constant", "regionProperties", "dictionary", "regions ( fluid (air) solid (solid) );\n");

    let f = &study.fluid;
    let m = &study.material;
    let speed = study.settings.free_stream;
    if speed <= 0.0 {
        bail!("The air speed must be above zero: this run has no buoyancy to move the air by itself.");
    }
    let ambient = study.settings.initial_temperature + KELVIN;
    let re = speed * length * f.density / f.viscosity;
    let turbulent = re > 2300.0;
    let iterations = 12000.0;
    // The balance is read at the outlet, from the very flux the solver uses.
    let extra = "functions { outflowHeat { type surfaceFieldValue; libs (fieldFunctionObjects); region air; regionType patch; name outletBox; operation weightedSum; weightField phi; fields (T); writeControl timeStep; writeInterval 50; writeFields false; log false; } outflowMass { type surfaceFieldValue; libs (fieldFunctionObjects); region air; regionType patch; name outletBox; operation sum; fields (phi); writeControl timeStep; writeInterval 50; writeFields false; log false; } }\n";
    cases::control_dict(&mut case, &Control { application: "chtMultiRegionSimpleFoam", end_time: iterations, dt: 1.0, write_interval: iterations, adjustable: None, extra });
    case.dict("system", "fvSchemes", "dictionary", "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; }\nlaplacianSchemes { default Gauss linear corrected; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default corrected; }\n");
    case.dict("system", "fvSolution", "dictionary", "");
    let no_gravity = "dimensions [0 1 -2 0 0 0 0];\nvalue (0 0 0);\n";
    case.dict("constant", "g", "uniformDimensionedVectorField", no_gravity);
    case.dict("constant/air", "g", "uniformDimensionedVectorField", no_gravity);
    case.dict("constant/air", "turbulenceProperties", "dictionary", if turbulent { "simulationType RAS;\nRAS { RASModel kOmegaSST; turbulence on; printCoeffs off; }\n" } else { "simulationType laminar;\n" });
    let pr = f.viscosity * f.specific_heat / f.conductivity;
    case.dict("constant/air", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type heRhoThermo; mixture pureMixture; transport const; thermo hConst; equationOfState rhoConst; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 28.96; }} equationOfState {{ rho {}; }} thermodynamics {{ Cp {}; Hf 0; }} transport {{ mu {}; Pr {pr}; }} }}\n", f.density, f.specific_heat, f.viscosity));
    case.dict("constant/solid", "thermophysicalProperties", "dictionary", &format!("thermoType {{ type heSolidThermo; mixture pureMixture; transport constIso; thermo hConst; equationOfState rhoConst; specie specie; energy sensibleEnthalpy; }}\nmixture {{ specie {{ molWeight 50; }} transport {{ kappa {}; }} thermodynamics {{ Hf 0; Cp {}; }} equationOfState {{ rho {}; }} }}\n", m.conductivity, m.specific_heat, m.density));
    // The heat: evenly through the solid.
    case.dict("constant/solid", "fvOptions", "dictionary", &format!("heat {{ type scalarSemiImplicitSource; active yes; selectionMode all; volumeMode absolute; injectionRateSuSp {{ h ({power} 0); }} }}\n"));
    let turbulence_div = if turbulent { " div(phi,k) bounded Gauss upwind; div(phi,omega) bounded Gauss upwind;" } else { "" };
    case.dict("system/air", "fvSchemes", "dictionary", &format!("ddtSchemes {{ default steadyState; }}\ngradSchemes {{ default Gauss linear; limited cellLimited Gauss linear 1; }}\ndivSchemes {{ default none; div(phi,U) bounded Gauss linearUpwind limited; div(phi,h) bounded Gauss upwind; div(phi,K) bounded Gauss upwind;{turbulence_div} div(((rho*nuEff)*dev2(T(grad(U))))) Gauss linear; }}\nlaplacianSchemes {{ default Gauss linear limited 0.5; }}\ninterpolationSchemes {{ default linear; }}\nsnGradSchemes {{ default limited 0.5; }}\nwallDist {{ method meshWave; }}\n"));
    case.dict("system/air", "fvSolution", "dictionary", "solvers { rho { solver diagonal; } rhoFinal { $rho; } p_rgh { solver GAMG; smoother DIC; tolerance 1e-7; relTol 0.05; } \"(U|h|k|omega)\" { solver PBiCGStab; preconditioner DILU; tolerance 1e-7; relTol 0.1; } }\nSIMPLE { nNonOrthogonalCorrectors 1; pRefCell 0; pRefValue 1e5; rhoMin 0.1; rhoMax 100; residualControl { p_rgh 1e-5; U 1e-5; h 1e-7; } }\nrelaxationFactors { fields { rho 1.0; p_rgh 0.3; } equations { U 0.6; h 0.9; \"(k|omega)\" 0.6; } }\n");
    case.dict("system/solid", "fvSchemes", "dictionary", "ddtSchemes { default steadyState; }\ngradSchemes { default Gauss linear; }\ndivSchemes { default none; }\nlaplacianSchemes { default Gauss linear limited 0.5; }\ninterpolationSchemes { default linear; }\nsnGradSchemes { default limited 0.5; }\n");
    case.dict("system/solid", "fvSolution", "dictionary", "solvers { h { solver PCG; preconditioner DIC; tolerance 1e-8; relTol 0.01; } }\nSIMPLE { nNonOrthogonalCorrectors 1; residualControl { h 1e-7; } }\nrelaxationFactors { equations { h 1.0; } }\n");

    let coupled = |kappa: &str| format!("type compressible::turbulentTemperatureCoupledBaffleMixed; Tnbr T; kappaMethod {kappa}; value uniform {ambient};");
    let inlet_u = format!("type fixedValue; value uniform ({speed} 0 0);");
    let (fb, calc) = ("type fixedFluxPressure; value uniform 1e5;".to_string(), "type calculated; value uniform 1e5;".to_string());
    let slip = "type slip;".to_string();
    let zero = "type zeroGradient;".to_string();
    region_field(&mut case, "air", "U", "volVectorField", dim::VELOCITY, &format!("({speed} 0 0)"), &[("inletBox", inlet_u), ("outletBox", "type inletOutlet; inletValue uniform (0 0 0); value uniform (0 0 0);".to_string()), ("sidesBox", slip.clone()), ("air_to_solid", "type noSlip;".to_string())]);
    region_field(&mut case, "air", "T", "volScalarField", dim::TEMPERATURE, &ambient.to_string(), &[("inletBox", format!("type fixedValue; value uniform {ambient};")), ("outletBox", format!("type inletOutlet; inletValue uniform {ambient}; value uniform {ambient};")), ("sidesBox", slip.clone()), ("air_to_solid", coupled("fluidThermo"))]);
    region_field(&mut case, "air", "p_rgh", "volScalarField", dim::PRESSURE, "1e5", &[("inletBox", fb.clone()), ("outletBox", "type fixedValue; value uniform 1e5;".to_string()), ("sidesBox", slip.clone()), ("air_to_solid", fb.clone())]);
    region_field(&mut case, "air", "p", "volScalarField", dim::PRESSURE, "1e5", &[("inletBox", calc.clone()), ("outletBox", calc.clone()), ("sidesBox", slip.clone()), ("air_to_solid", calc.clone())]);
    if turbulent {
        let k = (1.5 * (0.05 * speed).powi(2)).max(1e-8);
        let omega = k.sqrt() / (0.09f64.powf(0.25) * (0.07 * length).max(1e-6));
        region_field(&mut case, "air", "k", "volScalarField", "[0 2 -2 0 0 0 0]", &k.to_string(), &[("inletBox", format!("type fixedValue; value uniform {k};")), ("outletBox", zero.clone()), ("sidesBox", slip.clone()), ("air_to_solid", format!("type kqRWallFunction; value uniform {k};"))]);
        region_field(&mut case, "air", "omega", "volScalarField", "[0 0 -1 0 0 0 0]", &omega.to_string(), &[("inletBox", format!("type fixedValue; value uniform {omega};")), ("outletBox", zero.clone()), ("sidesBox", slip.clone()), ("air_to_solid", format!("type omegaWallFunction; value uniform {omega};"))]);
        region_field(&mut case, "air", "nut", "volScalarField", "[0 2 -1 0 0 0 0]", "0", &[("inletBox", "type calculated; value uniform 0;".to_string()), ("outletBox", "type calculated; value uniform 0;".to_string()), ("sidesBox", slip.clone()), ("air_to_solid", "type nutkWallFunction; value uniform 0;".to_string())]);
        region_field(&mut case, "air", "alphat", "volScalarField", "[1 -1 -1 0 0 0 0]", "0", &[("inletBox", "type calculated; value uniform 0;".to_string()), ("outletBox", "type calculated; value uniform 0;".to_string()), ("sidesBox", slip.clone()), ("air_to_solid", "type compressible::alphatWallFunction; Prt 0.85; value uniform 0;".to_string())]);
    }
    region_field(&mut case, "solid", "T", "volScalarField", dim::TEMPERATURE, &ambient.to_string(), &[("solid_to_air", coupled("solidThermo"))]);
    region_field(&mut case, "solid", "p", "volScalarField", dim::PRESSURE, "1e5", &[("solid_to_air", calc)]);
    Ok((case, iterations, estimate))
}

/// The two regions as one result: temperatures everywhere in degrees C, velocity in the air, with the
/// body's surface to look at and a cut through both.
pub fn merge(solid: &Grid, air: &Grid) -> Result<ResultSet> {
    let (solid_mesh, air_mesh) = (solid.to_result_mesh(), air.to_result_mesh());
    let offset = solid_mesh.nodes.len() as u32;
    let mut nodes = solid_mesh.nodes;
    nodes.extend(air_mesh.nodes);
    let mut tets = solid_mesh.tets;
    tets.extend(air_mesh.tets.iter().map(|t| t.map(|n| n + offset)));
    let total = nodes.len();
    let temperature_of = |g: &Grid, what: &str| -> Result<Vec<f32>> {
        let f = g.point_field("T").ok_or_else(|| anyhow!("the {what} has no temperature"))?;
        Ok(f.data.iter().map(|v| v - KELVIN as f32).collect())
    };
    let mut t = temperature_of(solid, "solid")?;
    t.extend(temperature_of(air, "air")?);
    let mut u = vec![0.0f32; offset as usize * 3];
    match air.point_field("U") {
        Some(f) if f.stride() == 3 => u.extend_from_slice(&f.data),
        _ => bail!("the air has no velocity"),
    }
    if t.len() != total || u.len() != total * 3 {
        bail!("the two regions' fields don't match their meshes");
    }
    let field = |name: &str, unit: &str, components: Vec<String>, data: Vec<f32>| Field { name: name.into(), unit: unit.into(), location: Location::Node, components, data: Arc::from(data) };
    let fields = vec![field("T", "°C", vec!["T".into()], t), field("U", "m/s", vec!["U x".into(), "U y".into(), "U z".into()], u)];
    Ok(ResultSet { mesh: ResultMesh { nodes, tets, surface: solid_mesh.surface, cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields }], plots: Vec::new() })
}

/// The second column of a function object's file, row by row.
pub fn column(text: &str) -> Vec<f64> {
    text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split_ascii_whitespace().nth(1)?.parse().ok()).collect()
}

/// The heat the air carries away, in W, from the outlet's flow-weighted temperature and mass flow on the
/// same iterations, over the end of the run. Those two are large and nearly cancel: the difference
/// between them is the heat. So they must come from one iteration, or noise of one part in 10,000 in the
/// mass flow swings the answer by nearly half of it, and they're averaged as well, since a wake can go on
/// wandering. `None` with too little history.
pub fn carried_heat(weighted: &[f64], mass: &[f64], ambient: f64, specific_heat: f64) -> Option<f64> {
    let n = weighted.len().min(mass.len());
    if n < 4 {
        return None;
    }
    let from = n - n.max(10) * 3 / 10;
    let from = from.min(n - 1);
    let rise: Vec<f64> = (from..n).map(|i| weighted[i] - ambient * mass[i]).collect();
    Some(specific_heat * rise.iter().sum::<f64>() / rise.len() as f64)
}

/// A function object's history, by its name.
fn history(run: &Run, name: &str) -> Result<Vec<f64>> {
    let mut paths = String::new();
    run.distro.exec(false, &["/usr/bin/find", &format!("{}/postProcessing", run.remote), "-name", "surfaceFieldValue.dat"], &crate::jobs::Cancel::default(), |l| {
        paths.push_str(l);
        paths.push('\n');
    })?;
    let path = paths.lines().find(|p| p.contains(name)).ok_or_else(|| anyhow!("the run wrote no {name} history"))?;
    let rel = path.strip_prefix(&format!("{}/", run.remote)).unwrap_or(path);
    Ok(column(&run.read_remote(rel)?))
}

pub fn run_study(study: &Study, part: &Partition, ctx: &JobCtx) -> Result<ResultSet> {
    if !part.is_closed() {
        bail!("Conjugate Heat Transfer needs a closed solid, since the heat is solved inside it. Conjugate Heat Transfer (voxel) can take an open surface.");
    }
    let power = power_of(study, part);
    if power <= 0.0 {
        bail!("Add a heat flux on some faces: with no heat going in, nothing warms up.");
    }
    let name = format!("job{}", ctx.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").replace(|c: char| !c.is_ascii_alphanumeric(), ""));
    let (case, iterations, cells) = build(study, part, power, &name)?;
    super::check_memory(&crate::provision::distro(), cells)?;

    ctx.set("Meshing the air and the body", 0.05);
    let mut stage = |line: &str| {
        ctx.log(line);
        if let Some(t) = super::parse_time(line) {
            ctx.set("Solving flow and heat", (0.25 + 0.7 * (t / iterations)).min(0.95) as f32);
        }
    };
    // The fields wait aside while the mesh is made: its tools would otherwise read them as a mesh's.
    let steps = [
        "mv 0 0.keep",
        "blockMesh > log.blockMesh",
        "surfaceFeatureExtract > log.feature",
        "snappyHexMesh -overwrite > log.snappy",
        "checkMesh > log.checkMesh || true",
        "splitMeshRegions -cellZones -overwrite > log.split",
        // What's in no zone comes out as domain0: it's the air.
        "mv constant/domain0/polyMesh constant/air/polyMesh",
        "sed -i 's/domain0/air/g' constant/air/polyMesh/boundary constant/solid/polyMesh/boundary",
        "rm -rf constant/domain0 system/domain0 0",
        "mv 0.keep 0",
    ];
    let run = super::run(&case, &ctx.dir.join("case"), &steps, &ctx.cancel, &mut stage).context("meshing")?;
    // Both sides of the interface have to be there, or the body wasn't cut out.
    let boundary = run.read_remote("constant/solid/polyMesh/boundary").context("the solid region wasn't made")?;
    if !boundary.contains("solid_to_air") {
        bail!("The mesh didn't separate the body from the air, so there's no interface for heat to cross. A smaller element size usually cures this.");
    }

    ctx.set("Solving flow and heat", 0.25);
    let script = format!("{}\nset -e\ncd {}\nrm -rf processor*\ndecomposePar -allRegions > log.decompose\n({}; mpirun --oversubscribe -np 8 chtMultiRegionSimpleFoam -parallel)\nreconstructPar -allRegions -latestTime > log.reconstruct\n", crate::provision::foam_env(), run.remote, crate::jobs::wsl::RANK_LIMIT);
    let mut fatal = None;
    let outcome = run.distro.run_supervised(&run.remote, &[], &["/bin/bash", "-c", &script], &ctx.cancel, |line| {
        super::note_fatal(line, &mut fatal);
        stage(line);
    });
    if let Some(f) = fatal {
        bail!("OpenFOAM: {f}");
    }
    outcome.context("running the solver")?;

    ctx.set("Reading results", 0.96);
    // Carried away: the outlet's flow-weighted temperature, above the incoming air's.
    let ambient = study.settings.initial_temperature + KELVIN;
    let f = &study.fluid;
    let carried = carried_heat(&history(&run, "outflowHeat")?, &history(&run, "outflowMass")?, ambient, f.specific_heat).ok_or_else(|| anyhow!("the outflow history is too short"))?;
    let (_, air) = run.latest(Some("air"))?;
    let (_, solid) = run.latest(Some("solid"))?;
    let imbalance = (carried - power).abs() / power;
    ctx.log(&format!("power in {power:.3} W, carried out {carried:.3} W, {:.1}% apart", imbalance * 100.0));
    if imbalance > 0.1 {
        bail!("The heat balance doesn't close: {power:.2} W went in and {carried:.2} W came out, so this hasn't settled and isn't shown. A larger element size settles sooner.");
    }

    let mut set = merge(&solid, &air)?;
    set.plots.push(Plot { name: "heat balance".into(), unit: "W".into(), points: vec![(0.0, power), (1.0, carried)] });
    let (body_lo, body_hi) = {
        let (mut a, mut b) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &part.positions {
            for k in 0..3 {
                a[k] = a[k].min(p[k]);
                b[k] = b[k].max(p[k]);
            }
        }
        (a, b)
    };
    let middle: [f64; 3] = std::array::from_fn(|k| (body_lo[k] + body_hi[k]) / 2.0);
    crate::results_cut::add_cut_plane(&mut set, middle, [0.0, 1.0, 0.0]);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AnalysisId;
    use crate::study::{Condition, FaceRef};

    fn cube() -> Partition {
        let c = |x: f64, y: f64, z: f64| [x, y, z];
        let quads = [
            [c(0., 0., 0.), c(0., 1., 0.), c(1., 1., 0.), c(1., 0., 0.)],
            [c(0., 0., 1.), c(1., 0., 1.), c(1., 1., 1.), c(0., 1., 1.)],
            [c(0., 0., 0.), c(1., 0., 0.), c(1., 0., 1.), c(0., 0., 1.)],
            [c(0., 1., 0.), c(0., 1., 1.), c(1., 1., 1.), c(1., 1., 0.)],
            [c(0., 0., 0.), c(0., 0., 1.), c(0., 1., 1.), c(0., 1., 0.)],
            [c(1., 0., 0.), c(1., 1., 0.), c(1., 1., 1.), c(1., 0., 1.)],
        ];
        let (mut p, mut t) = (Vec::new(), Vec::new());
        for q in &quads {
            for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                let at = p.len() as u32;
                p.extend_from_slice(&tri);
                t.push([at, at + 1, at + 2]);
            }
        }
        crate::geometry::regions::from_triangles("cube", &p, &t, 30.0).unwrap().partition
    }

    fn study() -> Study {
        let mut s = Study::new("t".into(), AnalysisId::ConjugateHeatTransfer);
        s.settings.element_size = 0.1;
        s.settings.free_stream = 1.0;
        s.conditions.push(Condition { kind: BcKind::HeatFlux { flux: 100.0 }, faces: vec![FaceRef { face: 0, point: [0.0; 3], second: false }, FaceRef { face: 1, point: [0.0; 3], second: false }] });
        s
    }

    #[test]
    fn the_power_is_flux_times_area() {
        assert!((power_of(&study(), &cube()) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn the_case_has_both_regions_with_the_heat_in_the_solid() {
        let (case, _, _) = build(&study(), &cube(), 200.0, "t").unwrap();
        for f in ["0/air/U", "0/air/T", "0/air/p_rgh", "0/air/p", "0/solid/T", "constant/solid/fvOptions", "constant/air/thermophysicalProperties", "constant/solid/thermophysicalProperties", "constant/regionProperties", "system/air/fvSolution", "system/solid/fvSolution", "system/snappyHexMeshDict"] {
            assert!(case.files.contains_key(f), "{f}");
        }
        assert!(case.files["constant/solid/fvOptions"].contains("h (200 0)"));
        assert!(case.files["0/air/T"].contains("air_to_solid"));
        assert!(case.files["0/solid/T"].contains("solid_to_air"));
        assert!(case.files["system/snappyHexMeshDict"].contains("cellZone solid"));
    }

    #[test]
    fn fast_air_gets_a_turbulence_model_and_slow_air_doesnt() {
        let mut s = study();
        // Air at 1 cm/s over 1 m: Re about 700.
        s.settings.free_stream = 0.01;
        let (slow, _, _) = build(&s, &cube(), 1.0, "t").unwrap();
        assert!(!slow.files.contains_key("0/air/k"), "{:?}", slow.files["constant/air/turbulenceProperties"]);
        s.settings.free_stream = 5.0;
        // Air at 5 m/s over 1 m: Re about 3e5.
        let (fast, _, _) = build(&s, &cube(), 1.0, "t").unwrap();
        assert!(fast.files.contains_key("0/air/k") && fast.files.contains_key("0/air/alphat"));
    }

    #[test]
    fn still_air_is_refused() {
        let mut s = study();
        s.settings.free_stream = 0.0;
        assert!(build(&s, &cube(), 1.0, "t").unwrap_err().to_string().contains("air speed"));
    }

    #[test]
    fn the_regions_merge_into_one_result() {
        let grid = |x: f32, t: f32, u: Option<f32>| {
            let points: Vec<[f64; 3]> = vec![[f64::from(x), 0.0, 0.0], [f64::from(x) + 1.0, 0.0, 0.0], [f64::from(x), 1.0, 0.0], [f64::from(x), 0.0, 1.0]];
            let mut g = Grid { points, cells: vec![(10, vec![0, 1, 2, 3])], ..Default::default() };
            g.point_fields.push(Field { name: "T".into(), unit: String::new(), location: Location::Node, components: vec!["T".into()], data: Arc::from(vec![t; 4]) });
            if let Some(v) = u {
                g.point_fields.push(Field { name: "U".into(), unit: String::new(), location: Location::Node, components: vec!["x".into(), "y".into(), "z".into()], data: Arc::from(vec![v; 12]) });
            }
            g
        };
        let set = merge(&grid(0.0, 373.15, None), &grid(5.0, 293.15, Some(2.0))).unwrap();
        assert_eq!(set.mesh.nodes.len(), 8);
        assert_eq!(set.mesh.tets, vec![[0, 1, 2, 3], [4, 5, 6, 7]]);
        assert_eq!(set.mesh.surface.len(), 4, "only the solid's surface");
        let t = set.steps[0].field("T").unwrap();
        assert!((t.at(0)[0] - 100.0).abs() < 1e-3 && (t.at(7)[0] - 20.0).abs() < 1e-3, "degrees C");
        let u = set.steps[0].field("U").unwrap();
        assert_eq!(u.at(0), &[0.0, 0.0, 0.0], "still in the solid");
        assert_eq!(u.at(7), &[2.0, 2.0, 2.0]);
    }

    #[test]
    fn a_files_column_is_read() {
        assert_eq!(column("# Time weightedSum\n50 1.5\n100 2.5\n"), [1.5, 2.5]);
        assert!(column("# nothing yet\n").is_empty());
    }

    /// The measured case: 2 W carried away at 0.06 kg/s, in air at 293 K, with the mass flow wandering by
    /// one part in 10,000. Read on its own, each of those two numbers is useless for this.
    #[test]
    fn mass_flow_noise_cancels_out_of_the_heat() {
        let (cp, ambient, power) = (1005.0, 293.15, 2.0);
        let rise = power / cp;
        let mut weighted = Vec::new();
        let mut mass = Vec::new();
        for i in 0..240 {
            let m = 0.06 * (1.0 + 1e-4 * (f64::from(i) * 0.7).sin());
            mass.push(m);
            weighted.push(m * ambient + rise);
        }
        let got = carried_heat(&weighted, &mass, ambient, cp).unwrap();
        assert!((got - power).abs() < 1e-9, "{got}");
        // What a fixed mass flow would have made of the last row of it.
        let naive = cp * (weighted[239] - 0.06 * ambient);
        assert!((naive - power).abs() > 0.2, "the noise is big enough to matter: {naive}");
    }

    #[test]
    fn the_end_of_the_run_is_averaged() {
        let mass = vec![0.0; 100];
        // Still settling for most of the run, then going up and down around 1.
        let weighted: Vec<f64> = (0..100).map(|i| if i < 70 { 0.0 } else if i % 2 == 0 { 0.5 } else { 1.5 }).collect();
        assert!((carried_heat(&weighted, &mass, 300.0, 1.0).unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(carried_heat(&[1.0], &[1.0], 300.0, 1.0), None, "too little to go on");
    }
}
