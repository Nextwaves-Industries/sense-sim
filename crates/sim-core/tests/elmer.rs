//! The verification benchmarks of Electromagnetics: real Elmer, against closed-form answers.
//!
//!     cargo test --release --test elmer -- --ignored --nocapture --test-threads=1

use std::path::PathBuf;

use sim_core::elmer::{self, EPSILON_0, MU_0};
use sim_core::jobs::Cancel;

fn work(name: &str) -> PathBuf {
    std::env::temp_dir().join("sense-sim-verify-elmer").join(name)
}

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    let err = (got - want).abs() / want.abs();
    println!("{what}: got {got:.6e}, expected {want:.6e}, error {:.3}% (allowed {:.1}%)", err * 100.0, tol * 100.0);
    assert!(err <= tol, "{what}: {got} is {:.2}% from {want}, allowed {:.1}%", err * 100.0, tol * 100.0);
}

const DIRECT: &str = "  Linear System Solver = Direct\n  Linear System Direct Method = UMFPack\n";

/// A coaxial capacitor, b/a = 2. C' = 2 pi eps0 / ln(b/a) = 80.26 pF/m. Electrostatics.
#[test]
#[ignore = "needs the solver environment"]
fn sim_em_coax_capacitance() {
    let (a, b) = (0.01f64, 0.02f64);
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nMesh.ElementOrder = 2;\nMesh.SecondOrderLinear = 0;\nMesh.CharacteristicLengthMax = 0.0006;\nDisk(1) = {{0, 0, 0, {b}}};\nDisk(2) = {{0, 0, 0, {a}}};\nr[] = BooleanDifference{{ Surface{{1}}; Delete; }}{{ Surface{{2}}; Delete; }};\nb[] = Boundary{{ Surface{{r[0]}}; }};\nPhysical Curve(1) = {{b[1]}};\nPhysical Curve(2) = {{b[0]}};\nPhysical Surface(1) = {{r[0]}};\n"
    );
    let sif = format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Cartesian 2D\n  Simulation Type = Steady State\n  Steady State Max Iterations = 1\nEnd\nBody 1\n  Equation = 1\n  Material = 1\nEnd\nMaterial 1\n  Relative Permittivity = 1\nEnd\nEquation 1\n  Active Solvers(2) = 1 2\nEnd\nSolver 1\n  Equation = Stat Elec Solver\n  Variable = Potential\n  Variable DOFs = 1\n  Procedure = \"StatElecSolve\" \"StatElecSolver\"\n  Calculate Electric Field = False\n  Calculate Electric Energy = True\n{DIRECT}End\nSolver 2\n  Exec Solver = After All\n  Equation = SaveScalars\n  Procedure = \"SaveData\" \"SaveScalars\"\n  Filename = scalars.dat\nEnd\nBoundary Condition 1\n  Target Boundaries(1) = 1\n  Potential = 0\nEnd\nBoundary Condition 2\n  Target Boundaries(1) = 2\n  Potential = 1\nEnd\n",
        elmer::constants()
    );
    let mut tail = Vec::new();
    let run = elmer::run("coax", &work("coax"), &geo, &sif, &[], 2, &Cancel::default(), |l| {
        tail.push(l.to_string());
        if tail.len() > 12 {
            tail.remove(0);
        }
    })
    .unwrap_or_else(|e| panic!("{e:#}\n{}", tail.join("\n")));
    let scalars = run.scalars("scalars.dat").unwrap();
    println!("{scalars:?}");
    let energy = scalars.iter().find(|(n, _)| n.contains("energy")).expect("no energy").1;
    // W = C V^2 / 2 with V = 1, per metre of depth.
    let exact = 2.0 * std::f64::consts::PI * EPSILON_0 / (b / a).ln();
    assert_close("capacitance per metre", 2.0 * energy, exact, 0.01);
}

/// A long solenoid, axisymmetric: the field inside is mu0 n I, here mu0 J (thickness).
#[test]
#[ignore = "needs the solver environment"]
fn sim_em_solenoid_field() {
    // Radii r1 to r2, half length half; a large air region around it.
    let (r1, r2, half, far) = (0.05f64, 0.06f64, 1.0f64, 5.0f64);
    let j = 1e6f64;
    // Everything is selected by where it is: the order of a boolean's results isn't dependable, and
    // a loose box around the outside would also catch the coil's edges and pin the field there.
    let e = 1e-6;
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nMesh.ElementOrder = 2;\nMesh.SecondOrderLinear = 0;\nRectangle(1) = {{0, -{far}, 0, {far}, {}}};\nRectangle(2) = {{{r1}, -{half}, 0, {}, {}}};\nBooleanFragments{{ Surface{{1}}; Delete; }}{{ Surface{{2}}; Delete; }}\nField[1] = Box; Field[1].VIn = 0.006; Field[1].VOut = 0.15; Field[1].XMin = 0; Field[1].XMax = 0.1; Field[1].YMin = -1.1; Field[1].YMax = 1.1; Field[1].Thickness = 0.6;\nBackground Field = 1;\nMesh.MeshSizeFromPoints = 0; Mesh.MeshSizeFromCurvature = 0; Mesh.MeshSizeExtendFromBoundary = 0;\ncoil[] = Surface In BoundingBox{{{} , {}, -1, {}, {}, 1}};\nall[] = Surface{{:}};\nair[] = all[]; air[] -= coil[];\nPhysical Surface(1) = {{coil[]}};\nPhysical Surface(2) = {{air[]}};\ntop[] = Curve In BoundingBox{{-{e}, {far}-{e}, -1, {far}+{e}, {far}+{e}, 1}};\nbottom[] = Curve In BoundingBox{{-{e}, -{far}-{e}, -1, {far}+{e}, -{far}+{e}, 1}};\nright[] = Curve In BoundingBox{{{far}-{e}, -{far}-{e}, -1, {far}+{e}, {far}+{e}, 1}};\nPhysical Curve(1) = {{top[], bottom[], right[]}};\n",
        2.0 * far,
        r2 - r1,
        2.0 * half,
        r1 - e,
        -half - e,
        r2 + e,
        half + e
    );
    let sif = format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Axi Symmetric\n  Simulation Type = Steady State\n  Steady State Max Iterations = 1\nEnd\nBody 1\n  Equation = 1\n  Material = 1\n  Body Force = 1\nEnd\nBody 2\n  Equation = 1\n  Material = 1\nEnd\nBody Force 1\n  Current Density = {j}\nEnd\nMaterial 1\n  Relative Permeability = 1\nEnd\nEquation 1\n  Active Solvers(2) = 1 2\nEnd\nSolver 1\n  Equation = MgDyn2D\n  Variable = Potential\n  Procedure = \"MagnetoDynamics2D\" \"MagnetoDynamics2D\"\n{DIRECT}  Nonlinear System Max Iterations = 1\nEnd\nSolver 2\n  Exec Solver = After All\n  Equation = SaveLine\n  Procedure = \"SaveData\" \"SaveLine\"\n  Filename = axis.dat\n  Variable 1 = Potential\n  Polyline Coordinates(2,2) = 0.001 0 0.03 0\n  Polyline Divisions(1) = 30\nEnd\nBoundary Condition 1\n  Target Boundaries(1) = 1\n  Potential = 0\nEnd\n",
        elmer::constants()
    );
    let mut tail = Vec::new();
    let run = elmer::run("solenoid", &work("solenoid"), &geo, &sif, &[], 2, &Cancel::default(), |l| {
        tail.push(l.to_string());
        if tail.len() > 12 {
            tail.remove(0);
        }
    })
    .unwrap_or_else(|e| panic!("{e:#}\n{}", tail.join("\n")));
    let (names, rows) = run.line("axis.dat").unwrap();
    println!("columns {names:?}, {} rows", rows.len());
    let x = names.iter().position(|n| n == "coordinate 1").expect("no x column");
    let a = names.iter().position(|n| n == "potential").expect("no potential column");
    // On the axis A = B r / 2: fit B over the points well inside.
    let (mut num, mut den) = (0.0, 0.0);
    for r in rows.iter().filter(|r| r[x] > 0.002 && r[x] < 0.03) {
        num += r[a] * r[x];
        den += r[x] * r[x];
    }
    assert!(den > 0.0, "no points on the axis");
    // Sign depends on the current's direction.
    let b = (2.0 * num / den).abs();
    let thick = MU_0 * j * (r2 - r1);
    // A finite solenoid is slightly below the infinite one: L/(2 r) = 18, about 0.3%.
    assert_close("field at the centre", b, thick, 0.01);
}

/// A copper half-space at 50 Hz: the field falls as exp(-x / delta), delta = sqrt(2 / (w mu sigma)).
#[test]
#[ignore = "needs the solver environment"]
fn sim_em_skin_depth() {
    let sigma = 5.96e7f64;
    let omega = 2.0 * std::f64::consts::PI * 50.0;
    let delta = (2.0 / (omega * MU_0 * sigma)).sqrt();
    let (l, w) = (0.12f64, 0.01f64);
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nMesh.ElementOrder = 2;\nMesh.SecondOrderLinear = 0;\nMesh.CharacteristicLengthMax = 0.0015;\nRectangle(1) = {{0, 0, 0, {l}, {w}}};\nPhysical Surface(1) = {{1}};\nPhysical Curve(1) = {{Curve In BoundingBox{{-1e-6, -1e-6, -1, 1e-6, {w}+1e-6, 1}}}};\nPhysical Curve(2) = {{Curve In BoundingBox{{{l}-1e-6, -1e-6, -1, {l}+1e-6, {w}+1e-6, 1}}}};\n"
    );
    let sif = format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Cartesian 2D\n  Simulation Type = Steady State\n  Steady State Max Iterations = 1\n  Frequency = 50\nEnd\nBody 1\n  Equation = 1\n  Material = 1\nEnd\nMaterial 1\n  Relative Permeability = 1\n  Electric Conductivity = {sigma}\nEnd\nEquation 1\n  Active Solvers(2) = 1 2\nEnd\nSolver 1\n  Equation = MgDyn2DHarmonic\n  Variable = Potential[Potential re:1 Potential im:1]\n  Variable DOFs = 2\n  Procedure = \"MagnetoDynamics2D\" \"MagnetoDynamics2DHarmonic\"\n{DIRECT}  Nonlinear System Max Iterations = 1\nEnd\nSolver 2\n  Exec Solver = After All\n  Equation = SaveLine\n  Procedure = \"SaveData\" \"SaveLine\"\n  Filename = line.dat\n  Variable 1 = Potential re\n  Variable 2 = Potential im\n  Polyline Coordinates(2,2) = 0 0.005 {l} 0.005\n  Polyline Divisions(1) = 120\nEnd\nBoundary Condition 1\n  Target Boundaries(1) = 1\n  Potential re = 1\n  Potential im = 0\nEnd\nBoundary Condition 2\n  Target Boundaries(1) = 2\n  Potential re = 0\n  Potential im = 0\nEnd\n",
        elmer::constants()
    );
    let mut tail = Vec::new();
    let run = elmer::run("skin", &work("skin"), &geo, &sif, &[], 2, &Cancel::default(), |l| {
        tail.push(l.to_string());
        if tail.len() > 12 {
            tail.remove(0);
        }
    })
    .unwrap_or_else(|e| panic!("{e:#}\n{}", tail.join("\n")));
    let (names, rows) = run.line("line.dat").unwrap();
    println!("columns {names:?}, {} rows", rows.len());
    let x = names.iter().position(|n| n == "coordinate 1").expect("no x column");
    let re = names.iter().position(|n| n == "potential re").expect("no re column");
    let im = names.iter().position(|n| n == "potential im").expect("no im column");
    // Fit the decay over the depths where it's clean: log of the magnitude against depth.
    let (mut sx, mut sy, mut sxx, mut sxy, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for r in rows.iter().filter(|r| r[x] > 0.25 * delta && r[x] < 4.0 * delta) {
        let m = (r[re] * r[re] + r[im] * r[im]).sqrt().ln();
        sx += r[x];
        sy += m;
        sxx += r[x] * r[x];
        sxy += r[x] * m;
        n += 1.0;
    }
    assert!(n > 5.0, "too few points in the fit");
    let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    assert_close("skin depth", -1.0 / slope, delta, 0.03);
}

/// erfc, by the rational approximation of Numerical Recipes: good to about 1.2e-7 everywhere.
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t * (-z * z - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))))).exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// A magnetic potential switched on at the surface of copper diffuses in over time, as
/// A = erfc(x / (2 sqrt(t / (mu sigma)))). An exact check of the transient magnetics.
#[test]
#[ignore = "needs the solver environment"]
fn sim_em_transient_diffusion() {
    let sigma = 5.96e7f64;
    let diffusivity = 1.0 / (MU_0 * sigma);
    let (l, w, dt, steps) = (0.3f64, 0.01f64, 5e-4f64, 200u32);
    let time = dt * f64::from(steps);
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\nMesh.ElementOrder = 2;\nMesh.SecondOrderLinear = 0;\nMesh.CharacteristicLengthMax = 0.002;\nRectangle(1) = {{0, 0, 0, {l}, {w}}};\nPhysical Surface(1) = {{1}};\nPhysical Curve(1) = {{Curve In BoundingBox{{-1e-6, -1e-6, -1, 1e-6, {w}+1e-6, 1}}}};\nPhysical Curve(2) = {{Curve In BoundingBox{{{l}-1e-6, -1e-6, -1, {l}+1e-6, {w}+1e-6, 1}}}};\n"
    );
    let sif = format!(
        "Header\n  Mesh DB \".\" \"model\"\nEnd\n{}Simulation\n  Coordinate System = Cartesian 2D\n  Simulation Type = Transient\n  Timestepping Method = BDF\n  BDF Order = 2\n  Timestep Intervals(1) = {steps}\n  Timestep Sizes(1) = {dt}\n  Steady State Max Iterations = 1\nEnd\nBody 1\n  Equation = 1\n  Material = 1\nEnd\nMaterial 1\n  Relative Permeability = 1\n  Electric Conductivity = {sigma}\nEnd\nEquation 1\n  Active Solvers(2) = 1 2\nEnd\nSolver 1\n  Equation = MgDyn2D\n  Variable = Potential\n  Procedure = \"MagnetoDynamics2D\" \"MagnetoDynamics2D\"\n{DIRECT}  Nonlinear System Max Iterations = 1\nEnd\nSolver 2\n  Exec Solver = After All\n  Equation = SaveLine\n  Procedure = \"SaveData\" \"SaveLine\"\n  Filename = line.dat\n  Variable 1 = Potential\n  Polyline Coordinates(2,2) = 0 0.005 {l} 0.005\n  Polyline Divisions(1) = 150\nEnd\nBoundary Condition 1\n  Target Boundaries(1) = 1\n  Potential = 1\nEnd\nBoundary Condition 2\n  Target Boundaries(1) = 2\n  Potential = 0\nEnd\n",
        elmer::constants()
    );
    let mut tail = Vec::new();
    let run = elmer::run("diffusion", &work("diffusion"), &geo, &sif, &[], 2, &Cancel::default(), |l| {
        tail.push(l.to_string());
        if tail.len() > 12 {
            tail.remove(0);
        }
    })
    .unwrap_or_else(|e| panic!("{e:#}\n{}", tail.join("\n")));
    let (names, rows) = run.line("line.dat").unwrap();
    println!("columns {names:?}, {} rows", rows.len());
    let x = names.iter().position(|n| n == "coordinate 1").expect("no x column");
    let a = names.iter().position(|n| n == "potential").expect("no potential column");
    let length = 2.0 * (diffusivity * time).sqrt();
    println!("the front has moved {length:.4} m");
    let (mut worst, mut n) = (0.0f64, 0);
    let mut mid = None;
    for r in rows.iter().filter(|r| r[x] > 0.0 && r[x] < 2.0 * length) {
        let exact = erfc(r[x] / length);
        // Written out, because max() would quietly skip a value that isn't a number.
        assert!(r[a].is_finite(), "the solver wrote {} at x = {}", r[a], r[x]);
        let difference = (r[a] - exact).abs();
        if difference > worst {
            worst = difference;
        }
        if mid.is_none() && r[x] >= 0.5 * length {
            mid = Some((r[x], r[a], exact));
        }
        n += 1;
    }
    assert!(n > 10, "too few points: {n}");
    let (at, got, exact) = mid.expect("a point half way to the front");
    println!("{n} points; at x = {at:.4}: {got:.6} against {exact:.6}");
    println!("worst difference from the exact profile, as a share of the surface value: {:.5}%", worst * 100.0);
    // The profile really has moved in: half way to the front it's nearer 0.48 than 0 or 1.
    assert!(got > 0.3 && got < 0.7, "{got}");
    assert!(worst < 0.01, "{worst}");
}