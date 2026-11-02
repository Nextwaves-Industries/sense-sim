//! Takes an electrostatics [`Study`] on a design body through Elmer: a tetrahedral mesh by Gmsh, the
//! picked faces as boundaries through the same audited face matching FEA uses, and the potential
//! and field as results. In this build, magnetics runs are set up by script, not from the panel.

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use crate::ccx::study::{FeaGeometry, diagonal_of};
use crate::elmer;
use crate::geometry::classify::{Limits, classify};
use crate::jobs::JobCtx;
use crate::mesh::gmsh::{self, Options};
use crate::results::{Field, Location, ResultMesh, ResultSet, ResultStep, StepKind};
use crate::results_vtk;
use crate::study::{BcKind, Study};

const DIRECT: &str = "  Linear System Solver = Iterative\n  Linear System Iterative Method = BiCGStab\n  Linear System Max Iterations = 2000\n  Linear System Convergence Tolerance = 1e-9\n  Linear System Preconditioning = ILU1\n";

/// The `.sif` file: `surfaces` holds, for each potential condition, its mesh surface numbers.
fn sif(study: &Study, bcs: &[(i32, f64)]) -> String {
    let mut s = format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Cartesian\n  Simulation Type = Steady State\n  Steady State Max Iterations = 1\nEnd\nBody 1\n  Target Bodies(1) = 1\n  Equation = 1\n  Material = 1\nEnd\nMaterial 1\n  Relative Permittivity = {}\nEnd\nEquation 1\n  Active Solvers(3) = 1 2 3\nEnd\nSolver 1\n  Equation = Stat Elec Solver\n  Variable = Potential\n  Variable DOFs = 1\n  Procedure = \"StatElecSolve\" \"StatElecSolver\"\n  Calculate Electric Field = True\n  Calculate Electric Energy = True\n{DIRECT}End\nSolver 2\n  Exec Solver = After All\n  Equation = SaveScalars\n  Procedure = \"SaveData\" \"SaveScalars\"\n  Filename = scalars.dat\nEnd\nSolver 3\n  Exec Solver = After All\n  Equation = \"result output\"\n  Procedure = \"ResultOutputSolve\" \"ResultOutputSolver\"\n  Output File Name = result\n  Vtk Format = True\n  Ascii Output = True\nEnd\n",
        elmer::constants(),
        1.0f64.max(study.material.relative_permeability.min(1.0))
    );
    for (i, (boundary, value)) in bcs.iter().enumerate() {
        let _ = writeln!(s, "Boundary Condition {}\n  Target Boundaries(1) = {boundary}\n  Potential = {value}\nEnd", i + 1);
    }
    s
}

/// The area of a coil's winding, seen in a cut through its axis, from its closed surface. The winding
/// carries the same current all the way round, so this is what turns ampere-turns into a current
/// density. It's (1 / 2 pi) times the volume integral of 1 / r, which the divergence theorem turns into
/// a surface integral: the unit vector pointing away from the axis has a divergence of exactly 1 / r.
pub fn winding_section(part: &crate::geometry::Partition, axis: usize, centre: [f64; 3]) -> f64 {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let mut sum = 0.0;
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        let n = crate::geometry::partition::cross(crate::geometry::partition::sub(b, a), crate::geometry::partition::sub(c, a));
        let mid: [f64; 3] = std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0);
        let (ru, rv) = (mid[u] - centre[u], mid[v] - centre[v]);
        let r = (ru * ru + rv * rv).sqrt();
        if r > 0.0 {
            // n is twice the area along the normal.
            sum += 0.5 * (n[u] * ru + n[v] * rv) / r;
        }
    }
    sum / std::f64::consts::TAU
}

/// The Elmer deck of a coil: the body carries `density` A/m2 round `axis`, in air, and the field is
/// held to the outer box.
fn coil_sif(density: f64, axis: usize, centre: [f64; 3]) -> String {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let dist = format!("sqrt((tx({u})-({}))^2+(tx({v})-({}))^2)", centre[u], centre[v]);
    let mut comps = [String::from("Real 0"), String::from("Real 0"), String::from("Real 0")];
    comps[u] = format!("Variable Coordinate 1, Coordinate 2, Coordinate 3\n    Real MATC \"-({density})*(tx({v})-({}))/{dist}\"", centre[v]);
    comps[v] = format!("Variable Coordinate 1, Coordinate 2, Coordinate 3\n    Real MATC \"({density})*(tx({u})-({}))/{dist}\"", centre[u]);
    format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Cartesian\n  Simulation Type = Steady State\n  Steady State Max Iterations = 1\nEnd\nBody 1\n  Target Bodies(1) = 1\n  Equation = 1\n  Material = 1\n  Body Force = 1\nEnd\nBody 2\n  Target Bodies(1) = 2\n  Equation = 1\n  Material = 1\nEnd\nBody Force 1\n  Current Density 1 = {}\n  Current Density 2 = {}\n  Current Density 3 = {}\nEnd\nMaterial 1\n  Relative Permeability = 1\n  Electric Conductivity = 0\nEnd\nEquation 1\n  Active Solvers(3) = 1 2 3\nEnd\nSolver 1\n  Equation = MGDynamics\n  Procedure = \"MagnetoDynamics\" \"WhitneyAVSolver\"\n  Variable = AV\n  Fix Input Current Density = True\n  Linear System Solver = Iterative\n  Linear System Iterative Method = BiCGStabL\n  BiCGStabL Polynomial Degree = 6\n  Linear System Max Iterations = 6000\n  Linear System Convergence Tolerance = 1e-7\n  Linear System Preconditioning = None\n  Linear System Abort Not Converged = True\n  Nonlinear System Max Iterations = 1\nEnd\nSolver 2\n  Equation = MGDynamicsCalc\n  Procedure = \"MagnetoDynamics\" \"MagnetoDynamicsCalcFields\"\n  Potential Variable = String \"AV\"\n  Calculate Current Density = Logical True\n  Linear System Solver = Iterative\n  Linear System Iterative Method = CG\n  Linear System Max Iterations = 5000\n  Linear System Convergence Tolerance = 1e-8\n  Linear System Preconditioning = None\n  Nonlinear System Max Iterations = 1\nEnd\nSolver 3\n  Exec Solver = After All\n  Equation = \"result output\"\n  Procedure = \"ResultOutputSolve\" \"ResultOutputSolver\"\n  Output File Name = result\n  Vtk Format = True\n  Ascii Output = True\nEnd\nBoundary Condition 1\n  Target Boundaries(1) = 1\n  AV {{e}} = Real 0\nEnd\n",
        elmer::constants(),
        comps[0],
        comps[1],
        comps[2]
    )
}

/// The magnetic field of a coil: a body from the design carrying a steady current round an axis, in a
/// box of air that its region is cut out of.
fn run_coil(study: &Study, geometry: &FeaGeometry, ctx: &JobCtx) -> Result<ResultSet> {
    let part = &geometry.partition;
    if geometry.step.is_empty() {
        bail!("A coil needs a body from the design: its region is cut out of the air around it, which takes CAD geometry, not a mesh.");
    }
    if !part.is_closed() {
        bail!("A coil's body must be a closed solid.");
    }
    let axis = study.settings.axis as usize;
    if axis > 2 {
        bail!("The coil's axis must be x, y or z.");
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &part.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let centre: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    let section = winding_section(part, axis, centre);
    if section <= 0.0 {
        bail!("This body has no winding around that axis to carry current.");
    }
    let density = study.settings.coil_ampere_turns / section;
    ctx.log(&format!("winding section {:.4e} m2, current density {density:.4e} A/m2", section));

    // Gmsh in millimetres, as the STEP is; the deck in metres.
    let mm = |v: f64| v * 1000.0;
    let diagonal = diagonal_of(part);
    let margin = 4.0 * diagonal;
    let near = (diagonal * study.settings.element_size).max(1e-6) * 1000.0;
    let (b_lo, b_hi): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| mm(lo[k] - margin)), std::array::from_fn(|k| mm(hi[k] + margin)));
    let e = mm(diagonal) * 1e-4;
    let side = |lo_c: [f64; 3], hi_c: [f64; 3]| format!("Surface In BoundingBox{{{}, {}, {}, {}, {}, {}}}", lo_c[0] - e, lo_c[1] - e, lo_c[2] - e, hi_c[0] + e, hi_c[1] + e, hi_c[2] + e);
    let mut outer = Vec::new();
    for k in 0..3 {
        let (mut a_hi, mut b_lo2) = (b_hi, b_lo);
        a_hi[k] = b_lo[k];
        b_lo2[k] = b_hi[k];
        outer.push(side(b_lo, a_hi));
        outer.push(side(b_lo2, b_hi));
    }
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nGeometry.OCCTargetUnit = \"MM\";\nMerge \"body.step\";\nbody[] = Volume{{:}};\nBox(9000) = {{{}, {}, {}, {}, {}, {}}};\nBooleanFragments{{ Volume{{9000}}; Delete; }}{{ Volume{{body[]}}; Delete; }}\ncoil[] = Volume In BoundingBox{{{}, {}, {}, {}, {}, {}}};\nall[] = Volume{{:}};\nair[] = all[]; air[] -= coil[];\nPhysical Volume(1) = {{coil[]}};\nPhysical Volume(2) = {{air[]}};\nouter[] = {{}};\n{}Physical Surface(1) = {{outer[]}};\nField[1] = Box; Field[1].VIn = {near}; Field[1].VOut = {far}; Field[1].XMin = {}; Field[1].XMax = {}; Field[1].YMin = {}; Field[1].YMax = {}; Field[1].ZMin = {}; Field[1].ZMax = {}; Field[1].Thickness = {thick};\nBackground Field = 1;\nMesh.MeshSizeFromPoints = 0; Mesh.MeshSizeFromCurvature = 0; Mesh.MeshSizeExtendFromBoundary = 0;\nMesh.ElementOrder = 1;\nMesh.Optimize = 1;\nMesh.MshFileVersion = 2.2;\nMesh.SaveAll = 0;\nMesh.ScalingFactor = 0.001;\n",
        b_lo[0], b_lo[1], b_lo[2], b_hi[0] - b_lo[0], b_hi[1] - b_lo[1], b_hi[2] - b_lo[2],
        mm(lo[0]) - e, mm(lo[1]) - e, mm(lo[2]) - e, mm(hi[0]) + e, mm(hi[1]) + e, mm(hi[2]) + e,
        outer.iter().map(|s| format!("outer[] += {s};\n")).collect::<String>(),
        mm(lo[0]) - near, mm(hi[0]) + near, mm(lo[1]) - near, mm(hi[1]) + near, mm(lo[2]) - near, mm(hi[2]) + near,
        far = near * 8.0,
        thick = mm(diagonal)
    );
    let name = format!("job{}", ctx.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").replace(|c: char| !c.is_ascii_alphanumeric(), ""));
    ctx.set("Meshing the coil and the air", 0.05);
    std::fs::create_dir_all(&ctx.dir)?;
    let sif_text = coil_sif(density, axis, centre);
    ctx.set("Solving", 0.3);
    let run = elmer::run(&name, &ctx.dir.join("case"), &geo, &sif_text, &[("body.step", geometry.step.as_slice())], 3, &ctx.cancel, |l| ctx.log(l))?;

    ctx.set("Reading results", 0.95);
    let mut names = String::new();
    run.distro.exec(false, &["/usr/bin/find", &run.remote, "-name", "*.vtk"], &ctx.cancel, |l| {
        names.push_str(l);
        names.push('\n');
    })?;
    let file = names.lines().filter(|l| l.contains("result")).max().ok_or_else(|| anyhow!("Elmer wrote no results"))?;
    let rel = file.strip_prefix(&format!("{}/", run.remote)).unwrap_or(file);
    let grid = results_vtk::parse(&run.read(rel)?).context("reading Elmer's results")?;
    let mesh = grid.to_result_mesh();
    let mut fields: Vec<Field> = Vec::new();
    for f in results_vtk::join_components(&grid.point_fields) {
        let unit = if f.name.contains("flux density") {
            "T"
        } else if f.name.contains("current density") {
            "A/m²"
        } else {
            ""
        };
        fields.push(Field { unit: unit.into(), location: Location::Node, ..f });
    }
    fields.retain(|f| f.name != "jfix" && f.name != "av");
    if !fields.iter().any(|f| f.name.contains("flux density")) {
        bail!("Elmer's results have no magnetic flux density");
    }
    let mut set = ResultSet { mesh: ResultMesh { nodes: mesh.nodes, tets: mesh.tets, surface: Vec::new(), cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields }], plots: Vec::new() };
    crate::post::add_surface(&mut set, &part.positions, &part.triangles);
    let mut normal = [0.0; 3];
    normal[(axis + 1) % 3] = 1.0;
    // As far as the field is worth looking at: the air box goes on for many times the coil's size.
    let (near_lo, near_hi): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| lo[k] - diagonal), std::array::from_fn(|k| hi[k] + diagonal));
    crate::results_cut::add_cut_plane_within(&mut set, centre, normal, near_lo, near_hi);
    Ok(set)
}

pub fn run_study(study: &Study, geometry: &FeaGeometry, ctx: &JobCtx) -> Result<ResultSet> {
    let part = &geometry.partition;
    crate::check::before_run(study, part)?;
    if study.settings.coil_ampere_turns != 0.0 {
        return run_coil(study, geometry, ctx);
    }
    let gmsh_exe = crate::tools::gmsh().ok_or_else(|| anyhow!("Gmsh is not installed"))?;
    let diagonal = diagonal_of(part);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 32);

    let values: Vec<(Vec<u32>, f64)> = study.conditions.iter().filter_map(|c| match c.kind {
        BcKind::Potential { value } => Some((c.faces.iter().map(|f| f.face).collect(), value)),
        _ => None,
    }).collect();
    if values.len() < 2 {
        bail!("Add at least two potentials: a field needs a difference.");
    }

    // A first mesh to learn which CAD surface is which face, through the same audit as FEA.
    ctx.set("Meshing", 0.05);
    std::fs::create_dir_all(&ctx.dir)?;
    let opts = Options { size: (diagonal * study.settings.element_size * 1000.0).max(1e-6), threads };
    let (base, file, bytes) = geometry.mesh_script(&opts);
    std::fs::write(ctx.dir.join(file), &bytes).context("writing the geometry")?;
    let mut mesh = gmsh::mesh_geo(&gmsh_exe, &ctx.dir, &base, &ctx.cancel, |l| ctx.log(l))?;
    for n in &mut mesh.nodes {
        for v in n.iter_mut() {
            *v *= 1e-3;
        }
    }
    ctx.set("Matching faces to the mesh", 0.2);
    let a = classify(&mesh, part, diagonal * 2e-3, Limits::default());
    if !a.issues.is_empty() {
        let list: Vec<String> = a.issues.iter().take(5).map(|i| i.to_string()).collect();
        bail!("The mesh doesn't match the design's faces, so potentials can't be placed reliably:\n{}", list.join("\n"));
    }
    // Physical groups: one volume, and one surface per condition. All other faces stay insulating.
    let mut physical = String::from("Physical Volume(1) = {1};\n");
    let mut bcs = Vec::new();
    for (i, (faces, value)) in values.iter().enumerate() {
        let mut entities = Vec::new();
        for f in faces {
            let found: Vec<i32> = a.whole.iter().filter(|(_, face)| *face == f).map(|(e, _)| *e).collect();
            if found.is_empty() {
                bail!("A picked face shares a surface with its neighbour in the mesh, which this analysis can't separate yet.");
            }
            entities.extend(found);
        }
        let list: Vec<String> = entities.iter().map(|e| e.to_string()).collect();
        let _ = writeln!(physical, "Physical Surface({}) = {{{}}};", i + 1, list.join(", "));
        bcs.push(((i + 1) as i32, *value));
    }
    // Only what's in a physical group is saved: those are the bodies and boundaries the deck names.
    let geo = format!("{base}{physical}Mesh.SaveAll = 0;\nMesh.ScalingFactor = 0.001;\n");
    let name = format!("job{}", ctx.dir.file_name().and_then(|n| n.to_str()).unwrap_or("x").replace(|c: char| !c.is_ascii_alphanumeric(), ""));
    ctx.set("Solving", 0.3);
    let run = elmer::run(&name, &ctx.dir.join("case"), &geo, &sif(study, &bcs), &[(file, bytes.as_slice())], 3, &ctx.cancel, |l| ctx.log(l))?;
    if let Ok(scalars) = run.scalars("scalars.dat") {
        for (n, v) in scalars {
            ctx.log(&format!("{n}: {v:.6e}"));
        }
    }

    ctx.set("Reading results", 0.95);
    let listing = {
        let mut names = String::new();
        run.distro.exec(false, &["/usr/bin/find", &run.remote, "-name", "*.vtk"], &ctx.cancel, |l| {
            names.push_str(l);
            names.push('\n');
        })?;
        names
    };
    let file = listing.lines().filter(|l| l.contains("result")).max().ok_or_else(|| anyhow!("Elmer wrote no results"))?;
    let rel = file.strip_prefix(&format!("{}/", run.remote)).unwrap_or(file);
    let text = run.read(rel)?;
    let grid = results_vtk::parse(&text).context("reading Elmer's results")?;
    let mesh = grid.to_result_mesh();
    let mut fields: Vec<Field> = Vec::new();
    for f in results_vtk::join_components(&grid.point_fields) {
        let unit = match f.name.as_str() {
            "potential" => "V",
            "electric field" => "V/m",
            _ => "",
        };
        fields.push(Field { unit: unit.into(), location: Location::Node, ..f });
    }
    if fields.is_empty() {
        bail!("Elmer's results have no fields");
    }
    Ok(ResultSet { mesh: ResultMesh { nodes: mesh.nodes, tets: mesh.tets, surface: mesh.surface, cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields }], plots: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::AnalysisId;

    /// A ring of radii `r1` to `r2` and height `h` about z, as many-sided prisms.
    fn ring(r1: f64, r2: f64, h: f64) -> crate::geometry::Partition {
        let n = 720u32;
        let at = |r: f64, i: u32, z: f64| {
            let a = f64::from(i % n) * std::f64::consts::TAU / f64::from(n);
            [r * a.cos(), r * a.sin(), z]
        };
        let (mut p, mut t) = (Vec::new(), Vec::new());
        let mut quad = |a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]| {
            for tri in [[a, b, c], [a, c, d]] {
                let first = p.len() as u32;
                p.extend_from_slice(&tri);
                t.push([first, first + 1, first + 2]);
            }
        };
        for i in 0..n {
            quad(at(r2, i, 0.0), at(r2, i + 1, 0.0), at(r2, i + 1, h), at(r2, i, h));
            quad(at(r1, i + 1, 0.0), at(r1, i, 0.0), at(r1, i, h), at(r1, i + 1, h));
            quad(at(r1, i, h), at(r2, i, h), at(r2, i + 1, h), at(r1, i + 1, h));
            quad(at(r2, i, 0.0), at(r1, i, 0.0), at(r1, i + 1, 0.0), at(r2, i + 1, 0.0));
        }
        crate::geometry::regions::from_triangles("ring", &p, &t, 30.0).unwrap().partition
    }

    #[test]
    fn the_winding_section_of_a_ring_is_its_width_times_its_height() {
        let part = ring(0.05, 0.06, 0.1);
        let got = winding_section(&part, 2, [0.0, 0.0, 0.05]);
        assert!((got - 0.01 * 0.1).abs() / (0.01 * 0.1) < 1e-4, "{got}");
    }

    #[test]
    fn the_coils_deck_turns_current_round_its_axis() {
        let s = coil_sif(1e6, 2, [1.0, 2.0, 3.0]);
        assert!(s.contains("WhitneyAVSolver"));
        assert!(s.contains("Current Density 1 = Variable Coordinate 1, Coordinate 2, Coordinate 3\n    Real MATC \"-(1000000)*(tx(1)-(2))/sqrt((tx(0)-(1))^2+(tx(1)-(2))^2)\""), "{s}");
        assert!(s.contains("Current Density 2 = Variable Coordinate 1, Coordinate 2, Coordinate 3\n    Real MATC \"(1000000)*(tx(0)-(1))/"), "{s}");
        assert!(s.contains("Current Density 3 = Real 0"), "no current along the axis");
        assert!(s.contains("AV {e} = Real 0"));
    }

    #[test]
    fn each_potential_is_a_boundary_of_the_deck() {
        let s = Study::new("t".into(), AnalysisId::Electromagnetics);
        let text = sif(&s, &[(1, 0.0), (2, 100.0)]);
        assert!(text.contains("Boundary Condition 2\n  Target Boundaries(1) = 2\n  Potential = 100"));
        assert!(text.contains("Coordinate System = Cartesian"));
        assert!(!text.contains('\r'));
    }
}
