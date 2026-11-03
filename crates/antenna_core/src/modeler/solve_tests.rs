//! Models built with features against the same antennas from templates: what's drawn has to
//! solve like what's already been checked, and follow its variables when it does.

use super::mesh::face_tag;
use super::sketch::{Constraint, CurveKind, ORIGIN, Sketch};
use super::*;
use crate::C64;
use crate::geometry::{Geometry, Shape};
use crate::mom::{MomOptions, MomProblem};
use crate::project::{Param, Project};

fn custom(f: &str, vars: &[(&str, String)]) -> Project {
    let mut p = Project::from_template("custom", Some(f)).unwrap();
    for (name, value) in vars {
        match p.params.iter_mut().find(|r| r.name == *name) {
            Some(r) => r.expr = value.clone(),
            None => p.params.push(Param::new(name, value, "")),
        }
    }
    p
}

fn z_of(g: &Geometry, f: f64, edges: f64) -> C64 {
    let opts = MomOptions { edges_per_lambda: edges, ..Default::default() };
    MomProblem::new(g, f, opts).unwrap().solve(f, false).unwrap().z[0][0]
}

fn mm(v: f64) -> String {
    format!("{}mm", v * 1e3)
}

/// A rectangle by its centre and sides, every one a variable or expression.
fn rect(cu: &str, cv: &str, su: &str, sv: &str) -> (Sketch, [u32; 4]) {
    let mut s = Sketch::default();
    let (p, _) = s.add_rect([-0.01, -0.01], [0.01, 0.01]);
    s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: su.into() });
    s.constraints.push(Constraint::DistanceV { a: p[1], b: p[2], value: sv.into() });
    s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p[0], value: format!("({cu})-({su})/2") });
    s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p[0], value: format!("({cv})-({sv})/2") });
    (s, p)
}

fn front(offset: &str) -> PlaneRef {
    PlaneRef::Base { axis: 1, offset: offset.into() }
}

fn top(offset: &str) -> PlaneRef {
    PlaneRef::Base { axis: 2, offset: offset.into() }
}

fn xyz(x: &str, y: &str, z: &str) -> [String; 3] {
    [x.to_string(), y.to_string(), z.to_string()]
}

#[test]
fn a_dipole_from_two_sheets_solves_like_the_template() {
    let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
    let v = t.vars().unwrap();
    let mut p = custom("1GHz", &[("Z0", "73".into()), ("L", mm(v["L"])), ("w", mm(v["w"])), ("g", mm(v["g"]))]);
    // The two arms along z on the y = 0 plane, whose u is x and whose v is z.
    let (upper, _) = rect("0", "(L+g)/4", "w", "(L-g)/2");
    let (lower, _) = rect("0", "-(L+g)/4", "w", "(L-g)/2");
    let a = p.model.add(Kind::Sketch { plane: front("0"), sketch: upper });
    let b = p.model.add(Kind::Sketch { plane: front("0"), sketch: lower });
    p.model.add(Kind::Sheet { sketch: a, out: Output::default() });
    p.model.add(Kind::Sheet { sketch: b, out: Output::default() });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("0", "0", "-g/2") }, b: Anchor::Xyz { at: xyz("0", "0", "g/2") }, width: "w".into(), z0: "Z0".into() });
    let g = p.geometry().unwrap();
    assert_eq!(g.ports.len(), 1);
    assert!(g.solids.iter().all(|s| matches!(s.shape, Shape::Polygon { thick, .. } if thick == 0.0)), "flat sheets stay exact outlines");
    let (zt, zm) = (z_of(&t.geometry().unwrap(), 1e9, 14.0), z_of(&g, 1e9, 14.0));
    println!("template {zt:.2} Ω, modelled {zm:.2} Ω");
    assert!((zt - zm).norm() / zt.norm() < 0.02, "template {zt} vs modelled {zm}");
    // A variable moves it, and the feed stays on the gap.
    let longer = p.with_values(&[("L".to_string(), v["L"] * 1.1)]);
    let (lo, hi) = longer.geometry().unwrap().bounds();
    assert!(((hi[2] - lo[2]) - v["L"] * 1.1).abs() < 1e-9, "{}", hi[2] - lo[2]);
    let z2 = z_of(&longer.geometry().unwrap(), 1e9, 14.0);
    assert!((z2 - zm).norm() / zm.norm() > 0.05, "a longer dipole has another impedance: {zm} {z2}");
}

/// The same dipole as above with no sketches behind it, then with the metal given a thickness. The
/// method of moments takes a thin sheet either way, and FDTD gets the slab.
#[test]
fn flat_shapes_solve_like_sheets_and_a_thickness_reaches_the_solvers_that_can_use_it() {
    let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
    let v = t.vars().unwrap();
    let mut p = custom("1GHz", &[("Z0", "73".into()), ("L", mm(v["L"])), ("w", mm(v["w"])), ("g", mm(v["g"])), ("tm", "35um".into())]);
    let arm = |p0v: &str| Flat::Rect { p0: ["-w/2".into(), p0v.into()], size: ["w".into(), "(L-g)/2".into()] };
    let up = p.model.add(Kind::Face { plane: front("0"), shape: arm("g/2"), out: Output::default() });
    let down = p.model.add(Kind::Face { plane: front("0"), shape: arm("-L/2"), out: Output::default() });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("0", "0", "-g/2") }, b: Anchor::Xyz { at: xyz("0", "0", "g/2") }, width: "w".into(), z0: "Z0".into() });
    let g = p.geometry().unwrap();
    assert!(g.solids.iter().all(|s| matches!(s.shape, Shape::Polygon { thick, .. } if thick == 0.0)), "flat shapes stay exact outlines");
    let (zt, zm) = (z_of(&t.geometry().unwrap(), 1e9, 14.0), z_of(&g, 1e9, 14.0));
    assert!((zt - zm).norm() / zt.norm() < 0.02, "template {zt} vs modelled {zm}");

    for id in [up, down] {
        p.model.feature_mut(id).unwrap().kind.output_mut().unwrap().thick = "tm".into();
    }
    let thick = p.geometry().unwrap();
    assert!(thick.solids.iter().all(|s| matches!(s.shape, Shape::Polygon { thick, .. } if (thick - 35e-6).abs() < 1e-12)), "{:?}", thick.solids);
    assert!(thick.solver_fit().surface_mom, "it can still go to the method of moments");
    let zs = z_of(&thick, 1e9, 14.0);
    assert!((zs - zm).norm() / zm.norm() < 1e-3, "a thin slab is the sheet it was: {zm} {zs}");
    // The port still meets the metal, and FDTD cuts a real slab of that thickness.
    let plan = crate::csx::plan(&thick, &p.setup, p.f0()).unwrap();
    let xml = crate::csx::to_xml(&thick, &plan, 0);
    assert!(xml.contains("LinPoly"), "{}", &xml[..xml.len().min(400)]);
    assert!(xml.contains("Length=\"0.000035"), "it has the thickness asked for");

    // Everything new comes back from a saved file.
    let mut q = p.clone();
    let cs = q.model.add_coord();
    q.model.coords[0].origin = xyz("1mm", "2mm", "3mm");
    q.model.add(Kind::Solid { shape: Prim::Cuboid { p0: xyz("0", "0", "0"), size: xyz("-1mm", "2mm", "tm") }, out: Output { coord: Some(cs), ..Default::default() } });
    q.model.add(Kind::Face { plane: top("tm"), shape: Flat::Ellipse { c: ["0".into(), "0".into()], rx: "1mm".into(), ry: "2mm".into() }, out: Output::default() });
    q.model.add(Kind::Face { plane: top("0"), shape: Flat::Polygon { c: ["0".into(), "0".into()], r: "1mm".into(), sides: "5".into() }, out: Output::default() });
    assert_eq!(Project::from_json(&q.to_json().unwrap()).unwrap(), q);
}

#[test]
fn a_slot_in_a_sketch_and_a_slot_cut_by_a_solid_solve_like_the_template() {
    let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
    let v = t.vars().unwrap();
    let (gx, gy, ls, ws, wp) = (v["Gx"], v["Gy"], v["Ls"], v["ws"], v["wp"]);
    let vars = [("Z0", "500".to_string()), ("Gx", mm(gx)), ("Gy", mm(gy)), ("Ls", mm(ls)), ("ws", mm(ws)), ("wp", mm(wp))];
    let edges = t.setup.edges_per_lambda;
    let zt = z_of(&t.geometry().unwrap(), 2.45e9, edges);
    let feed = || Kind::Feed { a: Anchor::Xyz { at: xyz("-ws/2", "0", "0") }, b: Anchor::Xyz { at: xyz("ws/2", "0", "0") }, width: "wp".into(), z0: "Z0".into() };

    // A hole drawn in the plate's own sketch.
    let mut drawn = custom("2.45GHz", &vars);
    let (mut s, _) = rect("0", "0", "Gx", "Gy");
    let (hole, _) = rect("0", "0", "ws", "Ls");
    // The hole's points come in after the plate's, under their own ids.
    let base = s.next_id;
    let shift = |i: u32| if i == ORIGIN { ORIGIN } else { i + base };
    for q in &hole.points {
        s.points.push(sketch::SkPoint { id: shift(q.id), at: q.at });
    }
    for c in &hole.curves {
        let CurveKind::Line { a, b } = c.kind else { panic!() };
        s.curves.push(sketch::Curve { id: shift(c.id), kind: CurveKind::Line { a: shift(a), b: shift(b) }, construction: false });
    }
    for c in &hole.constraints {
        s.constraints.push(match c.clone() {
            Constraint::Horizontal { line } => Constraint::Horizontal { line: shift(line) },
            Constraint::Vertical { line } => Constraint::Vertical { line: shift(line) },
            Constraint::DistanceU { a, b, value } => Constraint::DistanceU { a: shift(a), b: shift(b), value },
            Constraint::DistanceV { a, b, value } => Constraint::DistanceV { a: shift(a), b: shift(b), value },
            other => panic!("{other:?}"),
        });
    }
    let sk = drawn.model.add(Kind::Sketch { plane: top("0"), sketch: s });
    drawn.model.add(Kind::Sheet { sketch: sk, out: Output::default() });
    drawn.model.add(feed());
    let g = drawn.geometry().unwrap();
    let zd = z_of(&g, 2.45e9, edges);
    println!("template {zt:.1} Ω, hole in the sketch {zd:.1} Ω");
    assert!((zt - zd).norm() / zt.norm() < 0.08, "template {zt} vs drawn {zd}");

    // The same slot cut from a plain plate by a solid. It comes back to the mesher as an outline.
    let mut cut = custom("2.45GHz", &vars);
    let (s, _) = rect("0", "0", "Gx", "Gy");
    let sk = cut.model.add(Kind::Sketch { plane: top("0"), sketch: s });
    cut.model.add(Kind::Sheet { sketch: sk, out: Output::default() });
    cut.model.add(Kind::Solid { shape: Prim::Box { min: xyz("-ws/2", "-Ls/2", "-1mm"), max: xyz("ws/2", "Ls/2", "1mm") }, out: Output { mode: Mode::Cut, ..Default::default() } });
    cut.model.add(feed());
    let g = cut.geometry().unwrap();
    assert!(g.solids.iter().all(|s| matches!(s.shape, Shape::Polygon { .. })), "{:?}", g.solids.iter().map(|s| &s.name).collect::<Vec<_>>());
    let zc = z_of(&g, 2.45e9, edges);
    println!("cut by a solid {zc:.1} Ω");
    assert!((zt - zc).norm() / zt.norm() < 0.08, "template {zt} vs cut {zc}");
}

#[test]
fn a_slot_cut_by_another_sheet_solves_like_the_template() {
    let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
    let v = t.vars().unwrap();
    let vars = [("Z0", "500".to_string()), ("Gx", mm(v["Gx"])), ("Gy", mm(v["Gy"])), ("Ls", mm(v["Ls"])), ("ws", mm(v["ws"])), ("wp", mm(v["wp"]))];
    let edges = t.setup.edges_per_lambda;
    let zt = z_of(&t.geometry().unwrap(), 2.45e9, edges);
    let mut p = custom("2.45GHz", &vars);
    let (plate, _) = rect("0", "0", "Gx", "Gy");
    let (hole, _) = rect("0", "0", "ws", "Ls");
    let (a, b) = (p.model.add(Kind::Sketch { plane: top("0"), sketch: plate }), p.model.add(Kind::Sketch { plane: top("0"), sketch: hole }));
    let (plate, hole) = (p.model.add(Kind::Sheet { sketch: a, out: Output::default() }), p.model.add(Kind::Sheet { sketch: b, out: Output::default() }));
    p.model.add(Kind::Combine { op: CombineOp::Cut, target: plate, tools: vec![hole], keep_tools: false });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("-ws/2", "0", "0") }, b: Anchor::Xyz { at: xyz("ws/2", "0", "0") }, width: "wp".into(), z0: "Z0".into() });
    let g = p.geometry().unwrap();
    assert_eq!(g.solids.len(), 1, "{:?}", g.solids.iter().map(|s| &s.name).collect::<Vec<_>>());
    assert!(matches!(g.solids[0].shape, Shape::Polygon { .. }), "it reaches the solver as an exact outline");
    let z = z_of(&g, 2.45e9, edges);
    println!("template {zt:.1} Ω, cut by a sheet {z:.1} Ω");
    assert!((zt - z).norm() / zt.norm() < 0.08, "template {zt} vs cut by a sheet {z}");
}

#[test]
fn an_arm_joined_from_two_sheets_solves_like_one() {
    let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
    let v = t.vars().unwrap();
    let vars = [("Z0", "73".to_string()), ("L", mm(v["L"])), ("w", mm(v["w"])), ("g", mm(v["g"]))];
    let feed = || Kind::Feed { a: Anchor::Xyz { at: xyz("0", "0", "-g/2") }, b: Anchor::Xyz { at: xyz("0", "0", "g/2") }, width: "w".into(), z0: "Z0".into() };

    let mut whole = custom("1GHz", &vars);
    let (upper, _) = rect("0", "(L+g)/4", "w", "(L-g)/2");
    let (lower, _) = rect("0", "-(L+g)/4", "w", "(L-g)/2");
    let (a, b) = (whole.model.add(Kind::Sketch { plane: front("0"), sketch: upper }), whole.model.add(Kind::Sketch { plane: front("0"), sketch: lower }));
    whole.model.add(Kind::Sheet { sketch: a, out: Output::default() });
    whole.model.add(Kind::Sheet { sketch: b, out: Output::default() });
    whole.model.add(feed());

    // The upper arm in two lengths, end to end, joined.
    let mut joined = custom("1GHz", &vars);
    let (near, _) = rect("0", "g/2+(L-g)/8", "w", "(L-g)/4");
    let (far, _) = rect("0", "g/2+3*(L-g)/8", "w", "(L-g)/4");
    let (lower, _) = rect("0", "-(L+g)/4", "w", "(L-g)/2");
    let (a, b, c) = (joined.model.add(Kind::Sketch { plane: front("0"), sketch: near }), joined.model.add(Kind::Sketch { plane: front("0"), sketch: far }), joined.model.add(Kind::Sketch { plane: front("0"), sketch: lower }));
    let (near, far) = (joined.model.add(Kind::Sheet { sketch: a, out: Output::default() }), joined.model.add(Kind::Sheet { sketch: b, out: Output::default() }));
    joined.model.add(Kind::Sheet { sketch: c, out: Output::default() });
    joined.model.add(Kind::Combine { op: CombineOp::Add, target: near, tools: vec![far], keep_tools: false });
    joined.model.add(feed());
    let g = joined.geometry().unwrap();
    assert_eq!(g.solids.len(), 2, "one outline to an arm: {:?}", g.solids.iter().map(|s| &s.name).collect::<Vec<_>>());
    let (zw, zj) = (z_of(&whole.geometry().unwrap(), 1e9, 14.0), z_of(&g, 1e9, 14.0));
    println!("one sheet {zw:.2} Ω, joined {zj:.2} Ω");
    assert!((zw - zj).norm() / zw.norm() < 0.01, "one sheet {zw} vs joined {zj}");
    // It still follows the variables it was drawn with.
    let longer = joined.with_values(&[("L".to_string(), v["L"] * 1.1)]);
    let (lo, hi) = longer.geometry().unwrap().bounds();
    assert!(((hi[2] - lo[2]) - v["L"] * 1.1).abs() < 1e-9, "{}", hi[2] - lo[2]);
}

#[test]
fn a_patch_with_a_probe_on_a_sketch_point_solves_like_the_template() {
    let t = Project::from_template("patch_air", Some("2.45GHz")).unwrap();
    let g = t.geometry().unwrap();
    let port = &g.ports[0];
    let boxes: Vec<(crate::geometry::P3, crate::geometry::P3)> = g.solids.iter().filter_map(|s| if let Shape::Box { min, max } = s.shape { Some((min, max)) } else { None }).collect();
    assert_eq!(boxes.len(), 2);
    let (ground, patch) = if boxes[0].0[2] < boxes[1].0[2] { (boxes[0], boxes[1]) } else { (boxes[1], boxes[0]) };
    let mut p = custom("2.45GHz", &[("Z0", format!("{}", port.z0)), ("h", mm(patch.0[2])), ("Fx", mm(port.a[0])), ("Fy", mm(port.a[1])), ("Fw", mm(port.width))]);
    let side = |lo: crate::geometry::P3, hi: crate::geometry::P3| rect(&mm((lo[0] + hi[0]) / 2.0), &mm((lo[1] + hi[1]) / 2.0), &mm(hi[0] - lo[0]), &mm(hi[1] - lo[1])).0;
    let gs = p.model.add(Kind::Sketch { plane: top("0"), sketch: side(ground.0, ground.1) });
    p.model.add(Kind::Sheet { sketch: gs, out: Output::default() });
    let mut up = side(patch.0, patch.1);
    let pin = up.add_point([port.a[0], port.a[1]]);
    up.constraints.push(Constraint::DistanceU { a: ORIGIN, b: pin, value: "Fx".into() });
    up.constraints.push(Constraint::DistanceV { a: ORIGIN, b: pin, value: "Fy".into() });
    let ps = p.model.add(Kind::Sketch { plane: top("h"), sketch: up });
    p.model.add(Kind::Sheet { sketch: ps, out: Output::default() });
    p.model.add(Kind::Probe { sketch: ps, point: pin, width: "Fw".into(), z0: "Z0".into() });
    let gm = p.geometry().unwrap();
    assert!((gm.ports[0].b[2] - patch.0[2]).abs() < 1e-9, "{:?}", gm.ports[0]);
    let (zt, zm) = (z_of(&g, 2.45e9, 12.0), z_of(&gm, 2.45e9, 12.0));
    println!("template {zt:.1} Ω, modelled {zm:.1} Ω");
    assert!((zt - zm).norm() / zt.norm() < 0.05, "template {zt} vs modelled {zm}");
    // Moving the feed by name moves the impedance, which is what an optimiser needs.
    let k = if port.a[0].abs() > port.a[1].abs() { 0 } else { 1 };
    let moved = p.with_values(&[(["Fx", "Fy"][k].to_string(), port.a[k] * 0.5)]);
    let gm2 = moved.geometry().unwrap();
    assert!((gm2.ports[0].a[k] - port.a[k] * 0.5).abs() < 1e-9, "{:?} from {:?}", gm2.ports[0], port);
    let z2 = z_of(&gm2, 2.45e9, 12.0);
    assert!((z2 - zm).norm() / zm.norm() > 0.05, "{zm} {z2}");
}

#[test]
fn a_wire_along_a_path_solves_like_the_template_dipole() {
    let t = Project::from_template("dipole", Some("1GHz")).unwrap();
    let g = t.geometry().unwrap();
    let Shape::Wire { pts, r } = &g.solids[0].shape else { panic!("{:?}", g.solids[0].shape) };
    let port = &g.ports[0];
    let mut p = custom("1GHz", &[("Z0", format!("{}", port.z0))]);
    p.model.add(Kind::Wire { path: PathRef::Points { pts: pts.iter().map(|q| [mm(q[0]), mm(q[1]), mm(q[2])]).collect() }, radius: mm(*r), out: Output::default() });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: [mm(port.a[0]), mm(port.a[1]), mm(port.a[2])] }, b: Anchor::Xyz { at: [mm(port.b[0]), mm(port.b[1]), mm(port.b[2])] }, width: String::new(), z0: "Z0".into() });
    let gm = p.geometry().unwrap();
    assert!(gm.solver_fit().wire_mom, "wires go to the wire solver");
    let (zt, zm) = (z_of(&g, 1e9, 12.0), z_of(&gm, 1e9, 12.0));
    println!("template {zt:.2} Ω, modelled {zm:.2} Ω");
    assert!((zt - zm).norm() / zt.norm() < 0.005, "template {zt} vs modelled {zm}");
}

/// A bent plate over ground, like a PIFA: a shape no template's flat sheets could be cut from.
#[test]
fn a_bent_sheet_solves_and_its_faces_join() {
    let mut p = custom("2.45GHz", &[("Z0", "50".into()), ("Lp", "20mm".into()), ("Wp", "10mm".into()), ("hp", "5mm".into())]);
    let mut s = Sketch::default();
    let (a, b, c, d) = (s.add_point([-0.01, 0.0]), s.add_point([-0.01, 0.005]), s.add_point([0.01, 0.005]), s.add_point([0.01, 0.0]));
    s.add_line(a, b);
    s.add_line(b, c);
    s.add_line(c, d);
    s.constraints.push(Constraint::DistanceV { a, b, value: "hp".into() });
    s.constraints.push(Constraint::DistanceU { a: b, b: c, value: "Lp".into() });
    s.constraints.push(Constraint::DistanceV { a: d, b: c, value: "hp".into() });
    s.constraints.push(Constraint::SymmetricAxis { a: a, b: d, axis: 0 });
    s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: a, value: "0".into() });
    let sk = p.model.add(Kind::Sketch { plane: front("0"), sketch: s });
    p.model.add(Kind::Extrude { sketch: sk, depth: "Wp".into(), extent: Extent::Both, out: Output { surface: true, ..Default::default() } });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("-Lp/2", "0", "0") }, b: Anchor::Xyz { at: xyz("-Lp/2", "0", "hp/2") }, width: "2mm".into(), z0: "Z0".into() });
    // A port across unbroken metal needs both its ends on the sheet, and this one has them.
    let g = p.geometry().unwrap();
    assert_eq!(g.solids.len(), 3, "three flat faces");
    let opts = MomOptions { edges_per_lambda: 16.0, ..Default::default() };
    let problem = MomProblem::new(&g, 2.45e9, opts).unwrap();
    let q = crate::mom::mesher::mesh_geometry(&g, crate::C0 / 2.45e9 / 16.0, crate::C0 / 2.45e9).unwrap().quality();
    assert_eq!(q.junction_edges, 0, "{q:?}");
    let area = 2.0 * 0.005 * 0.01 + 0.02 * 0.01;
    let solved = problem.solve(2.45e9, false).unwrap().z[0][0];
    println!("bent plate: {solved:.1} Ω, {} unknowns, {area} m²", problem.unknowns());
    assert!(solved.re.is_finite() && solved.re > 0.0, "{solved}");
}

/// A cone and a disc from revolves, curved metal the mesher takes as triangles. Two cones form a
/// biconical dipole, whose input resistance has a known size: Z = 120 ln cot(θ/2) for an infinite
/// one, and a finite one sits in the same range at its first resonance.
#[test]
fn a_bicone_from_revolves_solves() {
    let mut p = custom("1GHz", &[("Z0", "50".into()), ("len", "65mm".into()), ("gap", "2mm".into()), ("flare", "40mm".into())]);
    for (name, sign) in [("Upper", ""), ("Lower", "-")] {
        let mut s = Sketch::default();
        let (a, b) = (s.add_point([0.0, 0.0]), s.add_point([0.04, 0.0]));
        s.add_line(a, b);
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: a, value: "0.5".into() });
        s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: a, value: format!("{sign}gap/2") });
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b, value: "flare".into() });
        s.constraints.push(Constraint::DistanceV { a: ORIGIN, b, value: format!("{sign}(gap/2+len)") });
        let sk = p.model.add(Kind::Sketch { plane: front("0"), sketch: s });
        let id = p.model.fresh_id();
        p.model.add_named(id, name.to_string(), Kind::Revolve { sketch: sk, axis: AxisRef::V, angle: "360".into(), out: Output { surface: true, ..Default::default() } });
    }
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("0.5mm", "0", "-gap/2") }, b: Anchor::Xyz { at: xyz("0.5mm", "0", "gap/2") }, width: "1mm".into(), z0: "Z0".into() });
    let g = p.geometry().unwrap();
    assert!(g.solids.iter().all(|s| matches!(s.shape, Shape::Tris { solid: false, .. })));
    let (z12, z20) = (z_of(&g, 1e9, 12.0), z_of(&g, 1e9, 20.0));
    println!("bicone: {z12:.1} Ω at 12 edges per wavelength, {z20:.1} Ω at 20");
    // Zc = 120 ln cot(θ/2) is about 150 Ω for this flare, and the length is near a quarter wave.
    assert!(z20.re > 40.0 && z20.re < 350.0 && z20.im.abs() < 200.0, "{z20}");
    assert!((z12 - z20).norm() / z20.norm() < 0.2, "a finer mesh doesn't move it far: {z12} {z20}");
}

/// A thick round dipole: closed cylinders the mesher knows exactly, against the same cylinders made
/// through a cut, so they arrive as triangles. Same antenna, so the same answer.
#[test]
fn curved_metal_as_triangles_solves_like_the_exact_shape() {
    let build = |via_cut: bool| {
        let mut p = custom("1GHz", &[("Z0", "73".into()), ("a", "3mm".into()), ("len", "68mm".into()), ("g", "4mm".into())]);
        for (top, bottom) in [("g/2+len", "g/2"), ("-g/2", "-g/2-len")] {
            let id = p.model.add(Kind::Solid { shape: Prim::Cylinder { a: xyz("0", "0", bottom), b: xyz("0", "0", top), r: "a".into() }, out: Output::default() });
            if via_cut {
                // Cuts nothing away, but leaves it as triangles.
                p.model.add(Kind::Solid { shape: Prim::Box { min: xyz("50mm", "50mm", "50mm"), max: xyz("60mm", "60mm", "60mm") }, out: Output { mode: Mode::Cut, target: Some(id), ..Default::default() } });
            }
        }
        p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("a", "0", "-g/2") }, b: Anchor::Xyz { at: xyz("a", "0", "g/2") }, width: "2mm".into(), z0: "Z0".into() });
        p.geometry().unwrap()
    };
    let (exact, soup) = (build(false), build(true));
    assert!(exact.solids.iter().all(|s| matches!(s.shape, Shape::Cylinder { .. })));
    assert!(soup.solids.iter().all(|s| matches!(s.shape, Shape::Tris { solid: true, .. })));
    let (ze, zs) = (z_of(&exact, 1e9, 20.0), z_of(&soup, 1e9, 20.0));
    println!("thick dipole: exact shapes {ze:.1} Ω, as triangles {zs:.1} Ω");
    assert!((ze - zs).norm() / ze.norm() < 0.12, "exact {ze} vs triangles {zs}");
}

/// A PIFA drawn as a bent plate over a ground, against the template of the same dimensions.
#[test]
fn a_pifa_from_a_bent_sheet_solves_like_the_template() {
    let t = Project::from_template("pifa", Some("2.45GHz")).unwrap();
    let v = t.vars().unwrap();
    let names = ["L", "W", "h", "xf", "Gx", "Gy", "wp", "Z0"];
    let vars: Vec<(&str, String)> = names.iter().map(|n| (*n, if *n == "Z0" { "50".to_string() } else { mm(v[*n]) })).collect();
    let mut p = custom("2.45GHz", &vars);
    // The plate and its short in one sketch on the front plane, pushed out across y.
    let mut s = Sketch::default();
    let (a, b, c) = (s.add_point([0.0, 0.0]), s.add_point([0.0, 0.01]), s.add_point([0.03, 0.01]));
    s.add_line(a, b);
    s.add_line(b, c);
    s.constraints.push(Constraint::Coincident { a, b: ORIGIN });
    s.constraints.push(Constraint::DistanceU { a, b, value: "0".into() });
    s.constraints.push(Constraint::DistanceV { a, b, value: "h".into() });
    s.constraints.push(Constraint::DistanceU { a: b, b: c, value: "L".into() });
    s.constraints.push(Constraint::DistanceV { a: b, b: c, value: "0".into() });
    let sk = p.model.add(Kind::Sketch { plane: front("0"), sketch: s });
    p.model.add(Kind::Extrude { sketch: sk, depth: "W".into(), extent: Extent::Both, out: Output { surface: true, ..Default::default() } });
    let (ground, _) = rect("Gx/4", "0", "Gx", "Gy");
    let gs = p.model.add(Kind::Sketch { plane: top("0"), sketch: ground });
    p.model.add(Kind::Sheet { sketch: gs, out: Output::default() });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("xf", "0", "0") }, b: Anchor::Xyz { at: xyz("xf", "0", "h") }, width: "wp".into(), z0: "Z0".into() });
    let gm = p.geometry().unwrap();
    assert!(gm.solids.iter().all(|s| matches!(s.shape, Shape::Polygon { .. })), "flat, square to the world: exact outlines");
    let (zt, zm) = (z_of(&t.geometry().unwrap(), 2.45e9, 16.0), z_of(&gm, 2.45e9, 16.0));
    println!("PIFA: template {zt:.1} Ω, modelled {zm:.1} Ω");
    assert!((zt - zm).norm() / zt.norm() < 0.05, "template {zt} vs modelled {zm}");
}

/// A horn: a loft between two rectangles, with its end faces taken away for the method of moments.
#[test]
fn a_horn_from_a_loft_is_open_walls() {
    let mut p = custom("10GHz", &[("Z0", "50".into())]);
    let (throat, _) = rect("0", "0", "22.86mm", "10.16mm");
    let (mouth, _) = rect("0", "0", "80mm", "60mm");
    let a = p.model.add(Kind::Sketch { plane: top("0"), sketch: throat });
    let b = p.model.add(Kind::Sketch { plane: top("60mm"), sketch: mouth });
    let body = p.model.add(Kind::Loft { sections: vec![a, b], out: Output::default() });
    let vars = p.vars().unwrap();
    let solid = p.model.build(&vars);
    assert!(solid.errors.is_empty(), "{:?}", solid.errors);
    assert!(solid.bodies[0].mesh.is_closed());
    let exact = (22.86e-3 * 10.16e-3 + 80e-3 * 60e-3 + (22.86e-3 * 10.16e-3 * 80e-3 * 60e-3_f64).sqrt()) * 0.06 / 3.0;
    assert!((solid.bodies[0].mesh.volume() - exact).abs() / exact < 0.02, "{} vs {exact}", solid.bodies[0].mesh.volume());
    p.model.add(Kind::Shell { body, open: vec![face_tag(body, prims::END_LOW), face_tag(body, prims::END_HIGH)], thick: "0".into() });
    let built = p.model.build(&vars);
    assert!(built.errors.is_empty(), "{:?}", built.errors);
    assert_eq!(built.bodies[0].kind, BodyKind::Sheet);
    // Four flat tilted walls, ready for a solver, not two coincident solids.
    assert_eq!(built.bodies[0].mesh.face_ids().len(), 4);
    assert_eq!(built.bodies[0].shapes().len(), 4);
    assert!(built.bodies[0].shapes().iter().all(|s| matches!(s, Shape::Tris { solid: false, .. })));
}

/// A template brought into the tree is the same antenna, and can then be built on.
#[test]
fn a_template_brought_into_the_tree_solves_the_same() {
    for (id, f, edges) in [("strip_dipole", 1e9, 14.0), ("slot", 2.45e9, 30.0), ("patch_air", 2.45e9, 12.0), ("pifa", 2.45e9, 16.0), ("dipole", 1e9, 12.0), ("rfid_tag", 915e6, 40.0)] {
        let t = Project::from_template(id, None).unwrap();
        let m = t.parts_into_model().unwrap();
        assert_eq!(m.template, "custom");
        assert!(!m.model.is_empty() && m.extra.is_empty(), "{id}");
        let vars = m.vars().unwrap();
        let b = m.model.build(&vars);
        assert!(b.errors.is_empty(), "{id}: {:?}", b.errors);
        let (gt, gm) = (t.geometry().unwrap(), m.geometry().unwrap());
        assert_eq!(gt.ports.len(), gm.ports.len(), "{id}");
        let (a, c) = (gt.bounds(), gm.bounds());
        assert!((0..3).all(|k| (a.0[k] - c.0[k]).abs() < 1e-8 && (a.1[k] - c.1[k]).abs() < 1e-8), "{id}: {a:?} {c:?}");
        let (zt, zm) = (z_of(&gt, f, edges), z_of(&gm, f, edges));
        println!("{id}: template {zt:.2} Ω, in the tree {zm:.2} Ω");
        assert!((zt - zm).norm() / zt.norm() < 0.03, "{id}: template {zt} vs tree {zm}");
    }
    // A substrate stays a dielectric body.
    let p = Project::from_template("patch_probe", None).unwrap().parts_into_model().unwrap();
    assert!(p.geometry().unwrap().solids.iter().any(|s| s.material == "FR-4"));
    // What needs its template's infinite ground says so.
    let ground = crate::templates::all().iter().map(|t| t.id).filter(|id| *id != "custom").find(|id| Project::from_template(id, None).unwrap().geometry().unwrap().ground);
    if let Some(id) = ground {
        assert!(Project::from_template(id, None).unwrap().parts_into_model().unwrap_err().to_string().contains("ground"), "{id}");
    }
}

#[test]
fn fdtd_gets_closed_solids_and_lines_on_their_faces() {
    let mut p = custom("2.45GHz", &[("Z0", "50".into()), ("Lb", "30mm".into()), ("hb", "5mm".into())]);
    let (s, _) = rect("0", "0", "Lb", "Lb");
    let sk = p.model.add(Kind::Sketch { plane: top("0"), sketch: s });
    let block = p.model.add(Kind::Extrude { sketch: sk, depth: "hb".into(), extent: Extent::One, out: Output { material: "FR-4".into(), ..Default::default() } });
    p.model.add(Kind::Solid { shape: Prim::Cylinder { a: xyz("0", "0", "-1mm"), b: xyz("0", "0", "6mm"), r: "3mm".into() }, out: Output { mode: Mode::Cut, target: Some(block), ..Default::default() } });
    let (m, _) = rect("0", "0", "20mm", "20mm");
    let ms = p.model.add(Kind::Sketch { plane: top("hb"), sketch: m });
    p.model.add(Kind::Sheet { sketch: ms, out: Output::default() });
    let (m, _) = rect("0", "0", "Lb", "Lb");
    let ms = p.model.add(Kind::Sketch { plane: top("0"), sketch: m });
    p.model.add(Kind::Sheet { sketch: ms, out: Output::default() });
    p.model.add(Kind::Feed { a: Anchor::Xyz { at: xyz("5mm", "0", "0") }, b: Anchor::Xyz { at: xyz("5mm", "0", "hb") }, width: String::new(), z0: "Z0".into() });
    let g = p.geometry().unwrap();
    let plan = crate::csx::plan(&g, &p.setup, p.f0()).unwrap();
    let on_line = |k: usize, x: f64, tol: f64| plan.mesh[k].iter().any(|l| (l - x).abs() < tol);
    assert!(on_line(2, 0.005, 1e-6), "the block's top has a mesh line: {:?}", &plan.mesh[2]);
    let xml = crate::csx::to_xml(&g, &plan, 0);
    assert!(xml.contains("<Polyhedron"), "the cut block goes as a polyhedron");
    // Sharing its vertices is what makes it a closed solid to openEMS.
    let chunk = xml.split("<Polyhedron").nth(1).unwrap().split("</Polyhedron>").next().unwrap();
    let verts = chunk.matches("<Vertex>").count();
    let faces = chunk.matches("<Face>").count();
    assert!(verts < faces, "{verts} vertices for {faces} faces");
    let solid = g.solids.iter().find(|s| matches!(s.shape, Shape::Tris { .. })).unwrap();
    let Shape::Tris { solid: closed, .. } = solid.shape else { panic!() };
    assert!(closed);
    assert_eq!(solid.material, "FR-4");
}

#[test]
fn a_model_with_a_broken_step_wont_solve_and_says_why() {
    let mut p = custom("1GHz", &[("Lb", "30mm".into())]);
    let (s, _) = rect("0", "0", "Lb", "Lb");
    let sk = p.model.add(Kind::Sketch { plane: top("0"), sketch: s });
    p.model.add(Kind::Sheet { sketch: sk, out: Output::default() });
    let id = p.model.add(Kind::Extrude { sketch: sk, depth: "nope".into(), extent: Extent::One, out: Output::default() });
    let name = p.model.feature(id).unwrap().name.clone();
    let e = format!("{:#}", p.scatterer().unwrap_err());
    assert!(e.contains(&name) && e.contains("nope"), "{e}");
    // The canvas's own list is still there.
    let vars = p.vars().unwrap();
    assert_eq!(p.model.build(&vars).bodies.len(), 1);
    // A file with a model comes back with it, and an old file without one opens as before.
    let back = Project::from_json(&p.to_json().unwrap()).unwrap();
    assert_eq!(back.model, p.model);
    let old = Project::from_template("dipole", None).unwrap();
    assert!(!old.to_json().unwrap().contains("\"model\""));
    assert_eq!(Project::from_json(&old.to_json().unwrap()).unwrap(), old);
}

#[test]
fn metal_bodies_touching_without_joining_are_reported() {
    let mut p = custom("1GHz", &[]);
    let a = p.model.add(Kind::Solid { shape: Prim::Cone { a: xyz("0", "0", "0"), b: xyz("0", "0", "10mm"), r0: "5mm".into(), r1: "2mm".into() }, out: Output::default() });
    p.model.add(Kind::Solid { shape: Prim::Cone { a: xyz("0", "0", "5mm"), b: xyz("20mm", "0", "5mm"), r0: "2mm".into(), r1: "1mm".into() }, out: Output::default() });
    let vars = p.vars().unwrap();
    assert_eq!(p.model.build(&vars).touching_unjoined().len(), 1);
    let other = p.model.features[1].id;
    p.model.add(Kind::Combine { op: CombineOp::Add, target: a, tools: vec![other], keep_tools: false });
    let joined = p.model.build(&vars);
    assert!(joined.errors.is_empty(), "{:?}", joined.errors);
    assert_eq!(joined.bodies.len(), 1);
    assert!(joined.touching_unjoined().is_empty());
    assert!(joined.bodies[0].mesh.is_closed());
}

#[test]
fn every_feature_kind_survives_a_save_and_a_load() {
    let mut p = custom("1GHz", &[]);
    let mut s = Sketch::default();
    s.add_rect([0.0, 0.0], [0.01, 0.01]);
    s.add_circle([0.03, 0.03], 0.002);
    s.add_arc([0.0, 0.05], [0.01, 0.05], [0.0, 0.06]);
    let sk = p.model.add(Kind::Sketch { plane: PlaneRef::Custom { origin: xyz("0", "0", "1mm"), normal: xyz("1", "1", "1") }, sketch: s });
    let o = Output::default;
    let body = p.model.add(Kind::Extrude { sketch: sk, depth: "1mm".into(), extent: Extent::Through, out: o() });
    p.model.add(Kind::Sheet { sketch: sk, out: o() });
    p.model.add(Kind::Revolve { sketch: sk, axis: AxisRef::Line { curve: 1 }, angle: "90".into(), out: o() });
    p.model.add(Kind::Sweep { sketch: sk, path: PathRef::Sketch { sketch: sk }, out: o() });
    p.model.add(Kind::Loft { sections: vec![sk, sk], out: o() });
    p.model.add(Kind::Solid { shape: Prim::Torus { c: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), big: "9mm".into(), small: "1mm".into() }, out: o() });
    p.model.add(Kind::Wire { path: PathRef::Helix { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), radius: "1mm".into(), radius2: String::new(), pitch: "1mm".into(), turns: "2".into(), left: true }, radius: "0.1mm".into(), out: o() });
    p.model.add(Kind::Combine { op: CombineOp::Cut, target: body, tools: vec![body], keep_tools: true });
    p.model.add(Kind::Pattern { bodies: vec![body], layout: Layout::Circular { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), count: "4".into(), total: "360".into() }, merge: true, feeds: Vec::new() });
    p.model.add(Kind::Mirror { bodies: vec![body], plane: PlaneRef::Face { face: 4096, offset: "0".into() }, merge: false });
    p.model.add(Kind::Move { bodies: vec![body], by: xyz("0", "0", "0"), origin: Some(xyz("0", "0", "0")), axis: Some(xyz("0", "0", "1")), angle: "30".into(), copy: true });
    p.model.add(Kind::Fillet { body, edges: vec![[1, 2]], radius: "1mm".into(), chamfer: true });
    p.model.add(Kind::Shell { body, open: vec![1], thick: "1mm".into() });
    p.model.add(Kind::Thicken { body, thick: "1mm".into() });
    p.model.add(Kind::Import { file: "a.stl".into(), tris: vec![[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]], out: o() });
    p.model.add(Kind::Feed { a: Anchor::Sketch { sketch: sk, point: 1, height: "1mm".into() }, b: Anchor::Xyz { at: xyz("0", "0", "0") }, width: String::new(), z0: "50".into() });
    p.model.add(Kind::Probe { sketch: sk, point: 1, width: String::new(), z0: "50".into() });
    let back = Project::from_json(&p.to_json().unwrap()).unwrap();
    assert_eq!(back.model, p.model);
    // None of it panics, whatever it comes to.
    let vars = p.vars().unwrap();
    let _ = p.model.build(&vars);
}
