//! The verification benchmarks of the structural analyses: real Gmsh, real CalculiX, and an
//! analytic answer to compare with. They need the solvers installed, so they are `#[ignore]`:
//!
//!     cargo test --release --test structural -- --ignored --nocapture --test-threads=1
//!
//! The shared beam: E 210 GPa, nu 0.3, rho 7850, L 1 m, 50 x 50 mm section, clamped at x = 0.

use std::path::PathBuf;

use sim_core::ccx::deck::{self, Condition, Groups, Material, Model, Procedure};
use sim_core::ccx::run::{self, Run};
use sim_core::jobs::Cancel;
use sim_core::mesh::gmsh::{self, Options};
use sim_core::mesh::msh::TetMesh;
use sim_core::results::{ResultStep, StepKind};

const THREADS: usize = 16;

fn work(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("sense-sim-verify").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn steel() -> Material {
    Material { name: "steel".into(), young: 210e9, poisson: 0.3, density: 7850.0, conductivity: Some(50.0), specific_heat: Some(480.0), expansion: Some(12e-6), plastic: vec![] }
}

fn beam(dir: &std::path::Path, size: f64) -> TetMesh {
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\n{}Box(1) = {{0, 0, 0, 1, 0.05, 0.05}};\n",
        gmsh::settings(&Options { size, threads: THREADS })
    );
    gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), dir, &geo, &Cancel::default(), |_| {}).unwrap()
}

/// OpenCASCADE's box faces: 1 x-min, 2 x-max, 3 y-min, 4 y-max, 5 z-min, 6 z-max.
fn beam_groups() -> Groups {
    Groups(vec![("FIXED".into(), vec![1]), ("TIP".into(), vec![2]), ("YMIN".into(), vec![3]), ("YMAX".into(), vec![4]), ("ZMIN".into(), vec![5]), ("ZMAX".into(), vec![6])])
}

fn solve(dir: &std::path::Path, model: &Model, kind: StepKind) -> Run {
    let text = deck::write(model).unwrap();
    run::solve(&sim_core::tools::ccx().expect("CalculiX is not installed"), dir, &text, kind, THREADS, &Cancel::default(), |_| {}).unwrap()
}

/// Mean of displacement component `axis` over the nodes at x = 1.
fn tip_mean(run: &Run, step: &ResultStep, axis: usize) -> f64 {
    let disp = step.field("DISP").expect("no DISP");
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in run.frd.nodes.iter().enumerate() {
        if (p[0] - 1.0).abs() < 1e-6 {
            sum += disp.at(i)[axis] as f64;
            n += 1.0;
        }
    }
    assert!(n > 0.0, "no tip nodes");
    sum / n
}

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    let err = (got - want).abs() / want.abs();
    println!("{what}: got {got:.6e}, expected {want:.6e}, error {:.3}% (allowed {:.1}%)", err * 100.0, tol * 100.0);
    assert!(err <= tol, "{what}: {got} is {:.2}% from {want}, allowed {:.1}%", err * 100.0, tol * 100.0);
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_static_cantilever() {
    let dir = work("static");
    let mesh = beam(&dir, 0.0125);
    println!("{} nodes, {} tets", mesh.nodes.len(), mesh.tets.len());
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }, Condition::Force { group: "TIP".into(), force: [0.0, 0.0, -1000.0] }],
        procedure: Procedure::Static { nlgeom: false, increment: 1.0 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    // PL^3/3EI + PL/kGA = 3.048 + 0.006 mm.
    assert_close("tip deflection", -tip_mean(&r, r.frd.steps.last().unwrap(), 2), 3.054e-3, 0.01);
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_frequency_cantilever() {
    let dir = work("frequency");
    let mesh = beam(&dir, 0.0125);
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }],
        procedure: Procedure::Frequency { modes: 6 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Mode);
    let f: Vec<f64> = r.frd.steps.iter().map(|s| s.value).collect();
    println!("modes (Hz): {f:?}");
    // A square section: two equal bending modes, then two more.
    assert_close("first bending", f[0], 41.8, 0.01);
    assert_close("first bending, other axis", f[1], 41.8, 0.01);
    assert_close("second bending", f[2], 262.0, 0.02);
    assert_close("second bending, other axis", f[3], 262.0, 0.02);
}

/// The last step's DISP field holds the time history only with one frd block per output; these
/// pick out each block's mean tip deflection.
fn tip_history(run: &Run, axis: usize) -> Vec<(f64, f64)> {
    run.frd.steps.iter().filter(|s| s.field("DISP").is_some()).map(|s| (s.value, tip_mean(run, s, axis))).collect()
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_dynamic_step_load() {
    let dir = work("dynamic");
    let mesh = beam(&dir, 0.025);
    // A 1 kN step at the tip. Undamped: the tip swings about its static position.
    let period = 0.0239;
    let dt = period / 80.0;
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }, Condition::Force { group: "TIP".into(), force: [0.0, 0.0, -1000.0] }],
        procedure: Procedure::Dynamic { dt, duration: 2.5 * period },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let h = tip_history(&r, 2);
    println!("{} outputs", h.len());
    let peak = h.iter().map(|(_, u)| -*u).fold(0.0, f64::max);
    assert_close("first peak (twice the static deflection)", peak, 6.1e-3, 0.05);
    // The period, from upward crossings of the static position.
    let mid = 3.054e-3;
    let mut crossings = Vec::new();
    for w in h.windows(2) {
        let (a, b) = (-w[0].1, -w[1].1);
        if a < mid && b >= mid {
            crossings.push(w[0].0 + (w[1].0 - w[0].0) * (mid - a) / (b - a));
        }
    }
    println!("upward crossings: {crossings:?}");
    assert!(crossings.len() >= 2, "too few crossings to time a period");
    assert_close("period", crossings[1] - crossings[0], period, 0.01);
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_harmonic_resonance() {
    let dir = work("harmonic");
    let mesh = beam(&dir, 0.025);
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }, Condition::Force { group: "TIP".into(), force: [0.0, 0.0, -1.0] }],
        procedure: Procedure::Harmonic { modes: 6, f_min: 38.0, f_max: 46.0, points: 41, damping: 0.01 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Frequency);
    // Amplitude at the tip from the real and imaginary parts, per excitation frequency.
    let mut curve: Vec<(f64, f64)> = Vec::new();
    for s in &r.frd.steps {
        let (Some(re), im) = (s.field("DISP"), s.field("DISP_IM").or_else(|| s.field("DISPI"))) else { continue };
        let (mut sum, mut n) = (0.0, 0.0);
        for (i, p) in r.frd.nodes.iter().enumerate() {
            if (p[0] - 1.0).abs() < 1e-6 {
                let a = f64::from(re.at(i)[2]);
                let b = im.map_or(0.0, |f| f64::from(f.at(i)[2]));
                sum += (a * a + b * b).sqrt();
                n += 1.0;
            }
        }
        curve.push((s.value, sum / n));
    }
    println!("{} frequencies, steps: {:?}", curve.len(), r.frd.steps.iter().take(3).map(|s| (s.value, s.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>())).collect::<Vec<_>>());
    let (f_peak, a_peak) = curve.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)).expect("no curve");
    println!("peak: {a_peak:.4e} m at {f_peak:.3} Hz");
    assert_close("resonant frequency", f_peak, 41.8, 0.01);
    assert_close("resonant amplitude", a_peak, 148e-6, 0.03);
}

/// A plate between two faces: x from 0 to 0.1 m.
fn slab(dir: &std::path::Path, size: f64) -> TetMesh {
    let geo = format!("SetFactory(\"OpenCASCADE\");\n{}Box(1) = {{0, 0, 0, 0.1, 0.02, 0.02}};\n", gmsh::settings(&Options { size, threads: THREADS }));
    gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), dir, &geo, &Cancel::default(), |_| {}).unwrap()
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_heat_transfer_transient_slab() {
    let dir = work("heat");
    let mesh = slab(&dir, 0.006);
    let mat = steel();
    let alpha = mat.conductivity.unwrap() / (mat.density * mat.specific_heat.unwrap());
    // Fourier number 0.5 on the half thickness.
    let duration = 0.5 * 0.05 * 0.05 / alpha;
    let model = Model {
        mesh: &mesh,
        material: mat,
        groups: beam_groups(),
        conditions: vec![Condition::Temperature { group: "FIXED".into(), value: 100.0 }, Condition::Temperature { group: "TIP".into(), value: 100.0 }],
        procedure: Procedure::Heat { transient: Some((duration / 100.0, duration)) },
        initial_temperature: Some(0.0),
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let last = r.frd.steps.last().unwrap();
    let t = last.field("NDTEMP").expect("no NDTEMP");
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in r.frd.nodes.iter().enumerate() {
        if (p[0] - 0.05).abs() < 1e-3 {
            sum += f64::from(t.at(i)[0]);
            n += 1.0;
        }
    }
    println!("time {:.2} s", last.value);
    // Series solution: 100 (1 - (4/pi) e^-(pi^2 Fo / 4)), with more terms negligible at Fo = 0.5.
    assert_close("centre temperature", sum / n, 62.9, 0.005);
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_thermomechanical_constrained_bar() {
    let dir = work("thermomechanical");
    let mesh = slab(&dir, 0.006);
    // Uniformly 100 K hotter than its stress-free state, with its length held: -E alpha dT.
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![
            Condition::Displacement { group: "FIXED".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "TIP".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "YMIN".into(), axis: 1, value: 0.0 },
            Condition::Displacement { group: "ZMIN".into(), axis: 2, value: 0.0 },
            Condition::Temperature { group: "FIXED".into(), value: 120.0 },
            Condition::Temperature { group: "TIP".into(), value: 120.0 },
            Condition::Temperature { group: "YMAX".into(), value: 120.0 },
            Condition::Temperature { group: "ZMAX".into(), value: 120.0 },
            Condition::Temperature { group: "YMIN".into(), value: 120.0 },
            Condition::Temperature { group: "ZMIN".into(), value: 120.0 },
        ],
        procedure: Procedure::Thermomechanical { transient: None, reference: 20.0 },
        initial_temperature: Some(20.0),
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let last = r.frd.steps.last().unwrap();
    let s = last.field("STRESS").expect("no STRESS");
    let (mut sum, mut n) = (0.0, 0.0);
    for i in 0..s.len() {
        let v = f64::from(s.at(i)[0]);
        if v.is_finite() {
            sum += v;
            n += 1.0;
        }
    }
    assert_close("axial stress", sum / n, -252e6, 0.01);
}

#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_nonlinear_large_deflection() {
    let dir = work("nonlinear");
    let mesh = beam(&dir, 0.025);
    // PL^2/EI = 1: the elastica. The tip moves 0.3017 L sideways and 0.0566 L back.
    let ei = 210e9 * 0.05f64.powi(4) / 12.0;
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }, Condition::Force { group: "TIP".into(), force: [0.0, 0.0, -ei] }],
        procedure: Procedure::Static { nlgeom: true, increment: 0.1 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let last = r.frd.steps.last().unwrap();
    assert_close("sideways, as a share of length", -tip_mean(&r, last, 2), 0.3017, 0.02);
    assert_close("back, as a share of length", -tip_mean(&r, last, 0), 0.0566, 0.02);
}

/// NAFEMS LE1: a quarter elliptic membrane under outward pressure. Curved faces and pressure on
/// them, and a stress concentration to get right.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_static_nafems_le1() {
    let dir = work("le1");
    // Inner ellipse x^2/4 + y^2 = 1; outer x^2/3.25^2 + y^2/2.75^2 = 1; 0.1 thick.
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\n{}Disk(1) = {{0, 0, 0, 3.25, 2.75}};\nDisk(2) = {{0, 0, 0, 2, 1}};\nRectangle(3) = {{0, 0, 0, 4, 4}};\nout[] = BooleanIntersection{{ Surface{{1}}; Delete; }}{{ Surface{{3}}; Delete; }};\nres[] = BooleanDifference{{ Surface{{out[0]}}; Delete; }}{{ Surface{{2}}; Delete; }};\nExtrude{{0, 0, 0.1}} {{ Surface{{res[0]}}; }}\nMesh.MeshSizeFromCurvature = 40;\n",
        gmsh::settings(&Options { size: 0.1, threads: THREADS })
    );
    let mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), &dir, &geo, &Cancel::default(), |_| {}).unwrap();
    // Which surface is which, by where its nodes are.
    let on = |tag: i32, f: &dyn Fn(&[f64; 3]) -> bool| mesh.surfaces.iter().find(|s| s.tag == tag).unwrap().tris.iter().flatten().all(|n| f(&mesh.nodes[*n as usize]));
    let (mut outer, mut x0, mut y0, mut z0) = (vec![], vec![], vec![], vec![]);
    for s in &mesh.surfaces {
        let t = s.tag;
        if on(t, &|p| (p[0] / 3.25).powi(2) + (p[1] / 2.75).powi(2) > 0.99) {
            outer.push(t);
        } else if on(t, &|p| p[0].abs() < 1e-6) {
            x0.push(t);
        } else if on(t, &|p| p[1].abs() < 1e-6) {
            y0.push(t);
        } else if on(t, &|p| p[2].abs() < 1e-6) {
            z0.push(t);
        }
    }
    let groups = Groups(vec![("OUTER".into(), outer), ("X0".into(), x0), ("Y0".into(), y0), ("Z0".into(), z0)]);
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups,
        conditions: vec![
            Condition::Displacement { group: "X0".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "Y0".into(), axis: 1, value: 0.0 },
            Condition::Displacement { group: "Z0".into(), axis: 2, value: 0.0 },
            // Outward pressure of 10 MPa: positive pushes in.
            Condition::Pressure { group: "OUTER".into(), pressure: -10e6 },
        ],
        procedure: Procedure::Static { nlgeom: false, increment: 1.0 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let s = r.frd.steps.last().unwrap().field("STRESS").expect("no STRESS");
    // sigma_yy at point D, (2, 0), mid-thickness: the nearest node on y = 0 on the inner edge.
    let (mut best, mut at) = (f64::INFINITY, 0);
    for (i, p) in r.frd.nodes.iter().enumerate() {
        let d = (p[0] - 2.0).abs() + p[1].abs() + (p[2] - 0.05).abs();
        if d < best {
            best = d;
            at = i;
        }
    }
    println!("point D found {best:.3} away, at {:?}", r.frd.nodes[at]);
    assert_close("sigma_yy at D", f64::from(s.at(at)[1]), 92.7e6, 0.02);
}
/// A bar pulled past yield, free to contract sideways: it can hold exactly sigma_y A, whatever the
/// mesh. The plainest check that plasticity does what it should.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_nonlinear_bar_yields_at_sigma_y() {
    let dir = work("bar");
    let mesh = slab(&dir, 0.006);
    let mut mat = steel();
    mat.plastic = vec![(250e6, 0.0)];
    let model = Model {
        mesh: &mesh,
        material: mat,
        groups: beam_groups(),
        conditions: vec![
            Condition::Displacement { group: "FIXED".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "YMIN".into(), axis: 1, value: 0.0 },
            Condition::Displacement { group: "ZMIN".into(), axis: 2, value: 0.0 },
            Condition::Displacement { group: "TIP".into(), axis: 0, value: 0.0005 },
        ],
        procedure: Procedure::Static { nlgeom: false, increment: 0.1 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let curve = r.totals.iter().find(|p| p.name == "FIXED.fx").expect("no reaction printed");
    assert_close("force held", curve.points.last().unwrap().1.abs(), 250e6 * 0.02 * 0.02, 0.01);
}

/// A cantilever bent past yield. Elastic to first yield, sigma_y I / (c L) = 5208 N; then it levels
/// off. Beam theory's limit is 7.81 kN, but a clamped root stops the section contracting sideways, a
/// plane-strain constraint that can raise the limit moment by up to 2/sqrt(3). So the limit must
/// lie in that band, not on the lower edge: halving the element size moved it by only 0.4%
/// (8324 N to 8292 N), which rules out element locking as the reason it's above the edge.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_nonlinear_plastic_limit() {
    let dir = work("plastic");
    let mesh = beam(&dir, 0.0125);
    let mut mat = steel();
    mat.plastic = vec![(250e6, 0.0)];
    let model = Model {
        mesh: &mesh,
        material: mat,
        groups: beam_groups(),
        conditions: vec![Condition::Fixed { group: "FIXED".into() }, Condition::Displacement { group: "TIP".into(), axis: 2, value: -0.08 }],
        procedure: Procedure::Static { nlgeom: false, increment: 0.05 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let curve = r.totals.iter().find(|p| p.name == "FIXED.fz").expect("no reaction printed");
    // First yield: where the curve leaves its elastic line, measured from its first point.
    let stiffness = curve.points[0].1.abs() / curve.points[0].0;
    let knee = curve.points.iter().find(|(t, f)| f.abs() < 0.995 * stiffness * t).map_or(0.0, |p| p.1.abs());
    println!("leaves the elastic line near {knee:.0} N");
    assert!(knee > 5208.0 * 0.95 && knee < 5208.0 * 1.3, "first yield at {knee} N, expected just above 5208 N");
    let last = curve.points.last().unwrap().1.abs();
    let earlier = curve.points[curve.points.len() * 3 / 4].1.abs();
    assert!((last - earlier) / last < 0.01, "it hasn't levelled off");
    let (low, high) = (7812.5, 7812.5 * 2.0 / 3f64.sqrt());
    println!("limit {last:.0} N, in the band {low:.0} to {high:.0} N");
    assert!(last >= low * 0.99 && last <= high, "limit force {last} is outside {low} to {high}");
}

/// NAFEMS LE10: a thick quarter elliptic plate under 1 MPa, held on its outer edge. sigma_yy at D,
/// on the loaded face at the inner ellipse's end, is -5.38 MPa. Only the outer face's mid-plane line
/// is held in z, so the plate is made of two layers to have nodes along that line.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_static_nafems_le10() {
    let dir = work("le10");
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\n{}Disk(1) = {{0, 0, -0.3, 3.25, 2.75}};\nDisk(2) = {{0, 0, -0.3, 2, 1}};\nRectangle(3) = {{0, 0, -0.3, 4, 4}};\nout[] = BooleanIntersection{{ Surface{{1}}; Delete; }}{{ Surface{{3}}; Delete; }};\nres[] = BooleanDifference{{ Surface{{out[0]}}; Delete; }}{{ Surface{{2}}; Delete; }};\na[] = Extrude{{0, 0, 0.3}} {{ Surface{{res[0]}}; }};\nb[] = Extrude{{0, 0, 0.3}} {{ Surface{{a[0]}}; }};\nField[1] = Ball; Field[1].Radius = 0.5; Field[1].Thickness = 1.0; Field[1].VIn = 0.03; Field[1].VOut = 0.12; Field[1].XCenter = 2; Field[1].YCenter = 0; Field[1].ZCenter = 0.3;\nBackground Field = 1;\nMesh.MeshSizeFromPoints = 0; Mesh.MeshSizeFromCurvature = 0; Mesh.MeshSizeExtendFromBoundary = 0;\n",
        gmsh::settings(&Options { size: 0.12, threads: THREADS })
    );
    let mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), &dir, &geo, &Cancel::default(), |_| {}).unwrap();
    println!("{} nodes, {} tets", mesh.nodes.len(), mesh.tets.len());
    let on_outer = |p: &[f64; 3]| ((p[0] / 3.25).powi(2) + (p[1] / 2.75).powi(2) - 1.0).abs() < 1e-3;
    let all = |tag: i32, f: &dyn Fn(&[f64; 3]) -> bool| mesh.surfaces.iter().find(|s| s.tag == tag).unwrap().tris.iter().flatten().all(|n| f(&mesh.nodes[*n as usize]));
    let (mut outer, mut x0, mut y0, mut upper) = (vec![], vec![], vec![], vec![]);
    for s in &mesh.surfaces {
        let t = s.tag;
        if all(t, &on_outer) {
            outer.push(t);
        } else if all(t, &|p| p[0].abs() < 1e-6) {
            x0.push(t);
        } else if all(t, &|p| p[1].abs() < 1e-6) {
            y0.push(t);
        } else if all(t, &|p| (p[2] - 0.3).abs() < 1e-6) {
            upper.push(t);
        }
    }
    assert!(!outer.is_empty() && !x0.is_empty() && !y0.is_empty() && !upper.is_empty(), "every face was found");
    let midline: Vec<u32> = mesh.nodes.iter().enumerate().filter(|(_, p)| p[2].abs() < 1e-6 && on_outer(p)).map(|(i, _)| i as u32).collect();
    println!("{} nodes on the held mid-plane line", midline.len());
    let groups = Groups(vec![("OUTER".into(), outer), ("X0".into(), x0), ("Y0".into(), y0), ("UPPER".into(), upper)]);
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups,
        conditions: vec![
            Condition::Displacement { group: "X0".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "Y0".into(), axis: 1, value: 0.0 },
            Condition::Displacement { group: "OUTER".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "OUTER".into(), axis: 1, value: 0.0 },
            Condition::DisplacementAt { nodes: midline, axis: 2, value: 0.0 },
            Condition::Pressure { group: "UPPER".into(), pressure: 1e6 },
        ],
        procedure: Procedure::Static { nlgeom: false, increment: 1.0 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let s = r.frd.steps.last().unwrap().field("STRESS").expect("no STRESS");
    let (mut best, mut at) = (f64::INFINITY, 0);
    for (i, p) in r.frd.nodes.iter().enumerate() {
        let d = (p[0] - 2.0).abs() + p[1].abs() + (p[2] - 0.3).abs();
        if d < best {
            best = d;
            at = i;
        }
    }
    println!("point D found {best:.3} away, at {:?}", r.frd.nodes[at]);
    assert_close("sigma_yy at D", f64::from(s.at(at)[1]), -5.38e6, 0.02);
}

/// NAFEMS LE11: a solid of revolution, cylinder, taper and sphere, under a temperature that grows with
/// radius and height. sigma_zz at A, on the inside at the plane of symmetry, is -105 MPa.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_thermomechanical_nafems_le11() {
    let dir = work("le11");
    let s45 = std::f64::consts::FRAC_1_SQRT_2;
    // In the xz plane: A (1, 0), B (1.4, 0), then round and up, as the benchmark's figure gives them.
    let (r45, top, knee, outer) = (s45, s45 + 0.69 + 0.4, s45 + 0.69, (1.96f64 - 0.5).sqrt());
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\n{}Point(1) = {{0, 0, 0}};\nPoint(2) = {{1, 0, 0}};\nPoint(3) = {{1.4, 0, 0}};\nPoint(4) = {{{r45}, 0, {r45}}};\nPoint(5) = {{{outer}, 0, {r45}}};\nPoint(6) = {{{r45}, 0, {top}}};\nPoint(7) = {{1, 0, {top}}};\nPoint(8) = {{1, 0, {knee}}};\nCircle(1) = {{2, 1, 4}};\nLine(2) = {{4, 6}};\nLine(3) = {{6, 7}};\nLine(4) = {{7, 8}};\nLine(5) = {{8, 5}};\nCircle(6) = {{5, 1, 3}};\nLine(7) = {{3, 2}};\nCurve Loop(1) = {{1, 2, 3, 4, 5, 6, 7}};\nPlane Surface(1) = {{1}};\nRecursive Delete {{ Point{{1}}; }}\nExtrude {{{{0, 0, 1}}, {{0, 0, 0}}, Pi / 2}} {{ Surface{{1}}; }}\nField[1] = Ball; Field[1].Radius = 0.25; Field[1].Thickness = 0.4; Field[1].VIn = 0.012; Field[1].VOut = 0.05; Field[1].XCenter = 1; Field[1].YCenter = 0; Field[1].ZCenter = 0;\nBackground Field = 1;\nMesh.MeshSizeFromPoints = 0; Mesh.MeshSizeFromCurvature = 0; Mesh.MeshSizeExtendFromBoundary = 0;\n",
        gmsh::settings(&Options { size: 0.05, threads: THREADS })
    );
    let mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), &dir, &geo, &Cancel::default(), |_| {}).unwrap();
    println!("{} nodes, {} tets", mesh.nodes.len(), mesh.tets.len());
    let all = |tag: i32, f: &dyn Fn(&[f64; 3]) -> bool| mesh.surfaces.iter().find(|s| s.tag == tag).unwrap().tris.iter().flatten().all(|n| f(&mesh.nodes[*n as usize]));
    let (mut x0, mut y0, mut z0, mut lid) = (vec![], vec![], vec![], vec![]);
    for s in &mesh.surfaces {
        let t = s.tag;
        if all(t, &|p| p[0].abs() < 1e-6) {
            x0.push(t);
        } else if all(t, &|p| p[1].abs() < 1e-6) {
            y0.push(t);
        } else if all(t, &|p| p[2].abs() < 1e-6) {
            z0.push(t);
        } else if all(t, &|p| (p[2] - top).abs() < 1e-6) {
            lid.push(t);
        }
    }
    assert!(!x0.is_empty() && !y0.is_empty() && !z0.is_empty() && !lid.is_empty(), "every face was found");
    let values: Vec<f64> = mesh.nodes.iter().map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt() + p[2]).collect();
    let mut material = steel();
    material.expansion = Some(2.3e-4);
    let model = Model {
        mesh: &mesh,
        material,
        groups: Groups(vec![("X0".into(), x0), ("Y0".into(), y0), ("Z0".into(), z0), ("LID".into(), lid)]),
        conditions: vec![
            Condition::Displacement { group: "X0".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "Y0".into(), axis: 1, value: 0.0 },
            Condition::Displacement { group: "Z0".into(), axis: 2, value: 0.0 },
            Condition::Displacement { group: "LID".into(), axis: 2, value: 0.0 },
            Condition::TemperatureField { values },
        ],
        procedure: Procedure::Static { nlgeom: false, increment: 1.0 },
        initial_temperature: None,
        prescribed_temperature: Some((0.0, 0.0)),
    };
    let r = solve(&dir, &model, StepKind::Time);
    let s = r.frd.steps.last().unwrap().field("STRESS").expect("no STRESS");
    let (mut best, mut at) = (f64::INFINITY, 0);
    for (i, p) in r.frd.nodes.iter().enumerate() {
        let d = (p[0] - 1.0).abs() + p[1].abs() + p[2].abs();
        if d < best {
            best = d;
            at = i;
        }
    }
    println!("point A found {best:.3} away, at {:?}", r.frd.nodes[at]);
    assert_close("sigma_zz at A", f64::from(s.at(at)[2]), -105e6, 0.03);
}

/// NAFEMS T4: a plate 0.6 by 1 m, one edge at 100 C, one insulated and two losing heat by convection
/// to 0 C. The temperature at E, (0.6, 0.2), is 18.25 C. A thin slice of it stands for the plane.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_heat_transfer_nafems_t4() {
    let dir = work("t4");
    let geo = format!("SetFactory(\"OpenCASCADE\");\n{}Box(1) = {{0, 0, 0, 0.6, 1, 0.02}};\n", gmsh::settings(&Options { size: 0.012, threads: THREADS }));
    let mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), &dir, &geo, &Cancel::default(), |_| {}).unwrap();
    println!("{} nodes, {} tets", mesh.nodes.len(), mesh.tets.len());
    let mut material = steel();
    material.conductivity = Some(52.0);
    let model = Model {
        mesh: &mesh,
        material,
        groups: beam_groups(),
        conditions: vec![
            Condition::Temperature { group: "YMIN".into(), value: 100.0 },
            Condition::Convection { group: "TIP".into(), coefficient: 750.0, ambient: 0.0 },
            Condition::Convection { group: "YMAX".into(), coefficient: 750.0, ambient: 0.0 },
        ],
        procedure: Procedure::Heat { transient: None },
        initial_temperature: Some(0.0),
        prescribed_temperature: None,
    };
    let r = solve(&dir, &model, StepKind::Time);
    let t = r.frd.steps.last().unwrap().field("NDTEMP").expect("no NDTEMP");
    // The mean through the slice's thickness, at the nodes nearest E.
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in r.frd.nodes.iter().enumerate() {
        if (p[0] - 0.6).abs() < 1e-6 && (p[1] - 0.2).abs() < 0.008 {
            sum += f64::from(t.at(i)[0]);
            n += 1.0;
        }
    }
    assert!(n > 0.0, "no nodes near E");
    assert_close("temperature at E", sum / n, 18.25, 0.02);
}

/// The cantilever again, but given as nothing but a triangle soup, as a USD asset would be: the box's
/// 12 triangles, each with its own corners. Faces come from creases, Gmsh remeshes the surface, and the
/// audited face matching and the whole study path run on it. The same answer proves the whole route.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_static_on_a_bare_mesh() {
    use sim_core::catalog::AnalysisId;
    use sim_core::ccx::study::{FeaGeometry, run_study};
    use sim_core::jobs::{JobManager, Kind, State};
    use sim_core::study::{BcKind, Condition as Bc, FaceRef, Study};
    let (lx, ly, lz) = (1.0, 0.05, 0.05);
    let c = |x: f64, y: f64, z: f64| [x * lx, y * ly, z * lz];
    let quads = [
        [c(0., 0., 0.), c(0., 1., 0.), c(1., 1., 0.), c(1., 0., 0.)],
        [c(0., 0., 1.), c(1., 0., 1.), c(1., 1., 1.), c(0., 1., 1.)],
        [c(0., 0., 0.), c(1., 0., 0.), c(1., 0., 1.), c(0., 0., 1.)],
        [c(0., 1., 0.), c(0., 1., 1.), c(1., 1., 1.), c(1., 1., 0.)],
        [c(0., 0., 0.), c(0., 0., 1.), c(0., 1., 1.), c(0., 1., 0.)],
        [c(1., 0., 0.), c(1., 1., 0.), c(1., 1., 1.), c(1., 0., 1.)],
    ];
    let (mut points, mut tris) = (Vec::new(), Vec::new());
    for q in &quads {
        for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            let at = points.len() as u32;
            points.extend_from_slice(&t);
            tris.push([at, at + 1, at + 2]);
        }
    }
    let regions = sim_core::geometry::regions::from_triangles("Bar", &points, &tris, sim_core::geometry::regions::CREASE_DEGREES).unwrap();
    assert_eq!(regions.solid_problem(), None);
    let part = regions.partition;
    assert_eq!(part.faces, 6);
    let face_at = |x: f64| -> FaceRef {
        let i = (0..part.triangles.len()).find(|i| part.triangle(*i).iter().all(|p| (p[0] - x).abs() < 1e-9)).expect("an end face");
        let [a, b, d] = part.triangle(i);
        FaceRef { face: part.tri_face[i], point: std::array::from_fn(|k| (a[k] + b[k] + d[k]) / 3.0), second: false }
    };
    let mut study = Study::new("bar".into(), AnalysisId::Static);
    study.body = Some("Bar".into());
    study.settings.element_size = 0.012;
    study.conditions.push(Bc { kind: BcKind::Fixed, faces: vec![face_at(0.0)] });
    study.conditions.push(Bc { kind: BcKind::Force { force: [0.0, 0.0, -1000.0] }, faces: vec![face_at(1.0)] });
    assert!(study.problems().is_empty(), "{:?}", study.problems());
    let root = work("bare");
    let jobs = JobManager::new(root);
    let geometry = FeaGeometry { partition: part, step: Vec::new(), second: None };
    let id = jobs.submit("bar", "bar", Kind::Cpu, move |ctx| run_study(&study, &geometry, ctx));
    loop {
        match jobs.list().into_iter().find(|j| j.id == id).unwrap().state {
            State::Done => break,
            State::Failed { error } => panic!("{error}\n{}", jobs.log(id, 30).join("\n")),
            State::Cancelled | State::Interrupted => panic!("stopped"),
            _ => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
    let set = jobs.result(id).unwrap();
    let disp = set.steps.last().unwrap().field("DISP").unwrap();
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, p) in set.mesh.nodes.iter().enumerate() {
        if (p[0] - 1.0).abs() < 1e-6 {
            sum += f64::from(disp.at(i)[2]);
            n += 1.0;
        }
    }
    println!("{} nodes in all, {n} at the tip", set.mesh.nodes.len());
    assert_close("tip deflection, from a bare mesh", -sum / n, 3.054e-3, 0.01);
}

/// Hertz: a steel sphere of radius 10 mm pressed on a steel plate with 100 N. The contact is a circle of
/// radius 0.187 mm, with a peak pressure of 1371 MPa at its middle and pressure falling across it as
/// sqrt(1 - r^2 / a^2). A quarter of each body, on planes of symmetry.
#[test]
#[ignore = "needs Gmsh and CalculiX"]
fn sim_nonlinear_hertz_contact() {
    let dir = work("hertz");
    let (radius, force) = (0.01f64, 100.0f64);
    let e_star = 210e9 / (2.0 * (1.0 - 0.3f64 * 0.3));
    let a_exact = (3.0 * force * radius / (4.0 * e_star)).cbrt();
    let p_exact = 3.0 * force / (2.0 * std::f64::consts::PI * a_exact * a_exact);
    println!("expected a = {:.4} mm, p0 = {:.1} MPa", a_exact * 1e3, p_exact / 1e6);
    let fine: f64 = std::env::var("HERTZ_CELL").ok().and_then(|v| v.parse().ok()).unwrap_or(4e-5);
    // The plate's top is z = 0 and the sphere stands on it, touching at the origin.
    let geo = format!(
        "SetFactory(\"OpenCASCADE\");\n{}Sphere(1) = {{0, 0, {radius}, {radius}}};\nBox(2) = {{0, 0, 0, {w}, {w}, {radius}}};\nq[] = BooleanIntersection{{ Volume{{1}}; Delete; }}{{ Volume{{2}}; Delete; }};\nBox(3) = {{0, 0, -{radius}, {radius}, {radius}, {radius}}};\nField[1] = Ball; Field[1].Radius = {core}; Field[1].Thickness = {thick}; Field[1].VIn = {fine}; Field[1].VOut = {coarse}; Field[1].XCenter = 0; Field[1].YCenter = 0; Field[1].ZCenter = 0;\nBackground Field = 1;\nMesh.MeshSizeFromPoints = 0; Mesh.MeshSizeFromCurvature = 0; Mesh.MeshSizeExtendFromBoundary = 0;\n",
        gmsh::settings(&Options { size: 2e-3, threads: THREADS }),
        w = radius * 1.0001,
        core = 2.5 * a_exact,
        thick = 3e-3,
        coarse = 2e-3,
    );
    let mesh = gmsh::mesh_geo(&sim_core::tools::gmsh().expect("Gmsh is not installed"), &dir, &geo, &Cancel::default(), |_| {}).unwrap();
    println!("{} nodes, {} tets", mesh.nodes.len(), mesh.tets.len());
    let all = |tag: i32, f: &dyn Fn(&[f64; 3]) -> bool| mesh.surfaces.iter().find(|s| s.tag == tag).unwrap().tris.iter().flatten().all(|n| f(&mesh.nodes[*n as usize]));
    let e = 1e-9;
    let (mut x0, mut y0, mut bottom, mut top, mut plate_top, mut ball) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for s in &mesh.surfaces {
        let t = s.tag;
        if all(t, &|p| p[0].abs() < e) {
            x0.push(t);
        } else if all(t, &|p| p[1].abs() < e) {
            y0.push(t);
        } else if all(t, &|p| (p[2] + radius).abs() < e) {
            bottom.push(t);
        } else if all(t, &|p| (p[2] - radius).abs() < e) {
            top.push(t);
        } else if all(t, &|p| p[2].abs() < e) {
            plate_top.push(t);
        } else if all(t, &|p| ((p[0] * p[0] + p[1] * p[1] + (p[2] - radius).powi(2)).sqrt() - radius).abs() < 1e-6) {
            ball.push(t);
        }
    }
    assert!(!x0.is_empty() && !y0.is_empty() && !bottom.is_empty() && !top.is_empty() && !plate_top.is_empty() && !ball.is_empty(), "every face was found: {:?}", (x0.len(), y0.len(), bottom.len(), top.len(), plate_top.len(), ball.len()));
    let slope = std::env::var("HERTZ_SLOPE").ok().and_then(|v| v.parse().ok()).unwrap_or(210e9 / fine * 10.0);
    let model = Model {
        mesh: &mesh,
        material: steel(),
        groups: Groups(vec![("X0".into(), x0), ("Y0".into(), y0), ("BOTTOM".into(), bottom), ("TOP".into(), top), ("PLATE".into(), plate_top), ("BALL".into(), ball)]),
        conditions: vec![
            Condition::Displacement { group: "X0".into(), axis: 0, value: 0.0 },
            Condition::Displacement { group: "Y0".into(), axis: 1, value: 0.0 },
            Condition::Fixed { group: "BOTTOM".into() },
            // The top of the quarter sphere is pushed down by the amount that gives about this force.
            // Held by a displacement, not a force: until contact closes, a force would leave the
            // sphere floating free, and the first solve singular.
            Condition::Displacement { group: "TOP".into(), axis: 2, value: -a_exact * a_exact / radius },
            Condition::Contact { slave: "BALL".into(), master: "PLATE".into(), slope, friction: None },
        ],
        procedure: Procedure::Static { nlgeom: false, increment: 0.25 },
        initial_temperature: None,
        prescribed_temperature: None,
    };
    let started = std::time::Instant::now();
    let r = solve(&dir, &model, StepKind::Time);
    println!("solved in {:.0} s", started.elapsed().as_secs_f64());
    println!("{} steps at {:?}", r.frd.steps.len(), r.frd.steps.iter().map(|s| s.value).collect::<Vec<_>>());
    println!("fields on the last: {:?}", r.frd.steps.last().unwrap().fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>());
    for step in &r.frd.steps {
        let (st, u) = (step.field("STRESS").unwrap(), step.field("DISP").unwrap());
        let at = |x: f64, z: f64| r.frd.nodes.iter().enumerate().min_by(|(_, a), (_, b)| ((a[0] - x).abs() + a[1].abs() + (a[2] - z).abs()).total_cmp(&((b[0] - x).abs() + b[1].abs() + (b[2] - z).abs()))).map(|(i, _)| i).unwrap();
        let row: Vec<String> = [(0.0, -1e-4), (0.0, -1e-3), (5e-4, 0.0), (1e-3, 0.0), (3e-3, 0.0)].iter().map(|(x, z)| { let i = at(*x, *z); format!("({:.1}mm,{:.1}mm) szz {:.0} MPa uz {:.2} um", r.frd.nodes[i][0] * 1e3, r.frd.nodes[i][2] * 1e3, st.at(i)[2] / 1e6, u.at(i)[2] * 1e6) }).collect();
        println!("t {}: {}", step.value, row.join("; "));
    }
    // Hertz at the force it took, from the four quarters.
    let reaction = r.totals.iter().find(|p| p.name == "TOP.fz").and_then(|p| p.points.last()).map(|p| p.1).expect("no reaction on the top");
    let measured = reaction.abs() * 4.0;
    let a_exact = (3.0 * measured * radius / (4.0 * e_star)).cbrt();
    let p_exact = 3.0 * measured / (2.0 * std::f64::consts::PI * a_exact * a_exact);
    println!("{measured:.2} N it took: expected a = {:.4} mm, p0 = {:.1} MPa", a_exact * 1e3, p_exact / 1e6);
    let s = r.frd.steps.last().unwrap().field("STRESS").expect("no STRESS");
    // Pressure on the plate's top along the plane y = 0: the normal stress there.
    let mut line: Vec<(f64, f64)> = r.frd.nodes.iter().enumerate().filter(|(_, p)| p[2].abs() < e && p[1].abs() < e && p[0] < 1.5 * a_exact).map(|(i, p)| (p[0], -f64::from(s.at(i)[2]))).collect();
    line.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("pressure along the plate: {:?}", line.iter().take(14).map(|(x, p)| format!("{:.3}mm {:.0}MPa", x * 1e3, p / 1e6)).collect::<Vec<_>>());
    // p^2 is a straight line in r^2 for Hertz. Fit it well inside the contact.
    let inside: Vec<(f64, f64)> = line.iter().filter(|(x, _)| *x < 0.75 * a_exact).map(|(x, p)| (x * x, p * p)).collect();
    assert!(inside.len() >= 3, "too few nodes across the contact: {}", inside.len());
    let n = inside.len() as f64;
    let (sx, sy, sxx, sxy) = inside.iter().fold((0.0, 0.0, 0.0, 0.0), |(a, b, c, d), (x, y)| (a + x, b + y, c + x * x, d + x * y));
    let slope_fit = (n * sxy - sx * sy) / (n * sxx - sx * sx);
    let intercept = (sy - slope_fit * sx) / n;
    let (p0, a) = (intercept.max(0.0).sqrt(), (-intercept / slope_fit).max(0.0).sqrt());
    assert_close("peak pressure", p0, p_exact, 0.05);
    assert_close("contact radius", a, a_exact, 0.05);
}