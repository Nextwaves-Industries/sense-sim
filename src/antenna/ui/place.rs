//! Drawing a shape with the mouse the way a CAD program asks for it: click a corner, click the size,
//! click the height, with the numbers there to type at every step. Whatever's typed is kept as
//! typed, so a variable's name stays a variable. What comes out is an ordinary feature, with all its
//! dimensions in the Parameters panel.

use antenna_core::expr::Vars;
use antenna_core::geometry::{P3, add, scale};
use antenna_core::modeler::mesh::Frame;
use antenna_core::modeler::sketch::{P2, mm_text};
use antenna_core::modeler::{Flat, Kind, Output, PathRef, PlaneRef, Prim, length};
use bevy_egui::egui::{self, Align2, Key, Pos2, Rect, RichText, Stroke, pos2};

use super::expr_field::{VarTable, length_line};
use super::model3d::{self, Picking, Pointer};
use super::view3d::Projector;
use crate::antenna::Studio;
use crate::theme;

/// What can be drawn this way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Box_,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Rect,
    Circle,
    Ellipse,
    Polygon,
    Line,
    Polyline,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Corner,
    Centre,
    Start,
    Next,
    Size,
    Radii,
    Radius,
    Tube,
    Height,
}

impl Shape {
    fn steps(self) -> &'static [Step] {
        match self {
            Shape::Box_ => &[Step::Corner, Step::Size, Step::Height],
            Shape::Rect => &[Step::Corner, Step::Size],
            Shape::Cylinder | Shape::Cone => &[Step::Centre, Step::Radius, Step::Height],
            Shape::Sphere | Shape::Circle | Shape::Polygon => &[Step::Centre, Step::Radius],
            Shape::Ellipse => &[Step::Centre, Step::Radii],
            Shape::Torus => &[Step::Centre, Step::Radius, Step::Tube],
            Shape::Line | Shape::Polyline => &[Step::Start, Step::Next],
        }
    }
}

/// A shape part way through being placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Placing {
    pub shape: Shape,
    step: usize,
    /// Each finished step: its numbers as they'll be stored, and what they come to in metres.
    done: Vec<Vec<(String, f64)>>,
    /// This step's numbers: following the pointer until they're typed in.
    fields: Vec<String>,
    typed: Vec<bool>,
}

impl Placing {
    fn step_now(&self) -> Step {
        let steps = self.shape.steps();
        steps[self.step.min(steps.len() - 1)]
    }

    fn fresh_fields(&mut self) {
        let n = field_count(self.step_now());
        self.fields = vec![String::new(); n];
        self.typed = vec![false; n];
    }
}

fn field_count(s: Step) -> usize {
    match s {
        Step::Radius | Step::Tube | Step::Height => 1,
        _ => 2,
    }
}

/// The world's axes across a plane normal to `axis`, the same way round as its frame.
fn plane_axes(axis: usize) -> (usize, usize) {
    match axis {
        2 => (0, 1),
        1 => (0, 2),
        _ => (1, 2),
    }
}

const AXIS: [&str; 3] = ["X", "Y", "Z"];

fn field_names(s: Step, axis: usize) -> Vec<String> {
    let (u, v) = plane_axes(axis);
    let (u, v, n) = (AXIS[u], AXIS[v], AXIS[axis]);
    match s {
        Step::Corner | Step::Centre | Step::Start | Step::Next => vec![u.to_string(), v.to_string()],
        Step::Size => vec![format!("{u}Dim"), format!("{v}Dim")],
        Step::Radii => vec![format!("Radius {u}"), format!("Radius {v}")],
        Step::Radius => vec!["Radius".to_string()],
        Step::Tube => vec!["Tube radius".to_string()],
        Step::Height => vec![format!("{n}Dim")],
    }
}

fn prompt(shape: Shape, s: Step) -> &'static str {
    match s {
        Step::Corner => "Choose the first corner:",
        Step::Centre => "Choose the centre:",
        Step::Start => "Choose where it starts:",
        Step::Next if shape == Shape::Line => "Choose where it ends:",
        Step::Next => "Choose the next point:",
        Step::Size => "Choose the size:",
        Step::Radii => "Choose its size:",
        Step::Radius => "Choose the radius:",
        Step::Tube => "Choose the tube's radius:",
        Step::Height => "Choose height:",
    }
}

/// Takes up the tool: the left button now places this shape.
pub fn start(st: &mut Studio, shape: Shape) {
    if st.ui.m3.active.is_some() {
        model3d::exit_sketch(st);
    }
    let mut p = Placing { shape, step: 0, done: Vec::new(), fields: Vec::new(), typed: Vec::new() };
    p.fresh_fields();
    st.ui.m3.picking = Picking::Place(p);
    st.ui.pointer = Pointer::Draw;
    st.ui.m3.note = None;
}

/// The plane shapes are placed on, in the coordinate system in use, and which way is up from it.
pub fn work_frame(st: &Studio) -> (Frame, P3) {
    let axis = st.ui.m3.work_axis;
    let vars = st.project.vars().unwrap_or_default();
    let mut f = PlaneRef::Base { axis, offset: "0".into() }.frame(&vars, &[]).unwrap_or(Frame { o: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] });
    let mut up = [0.0; 3];
    up[axis] = 1.0;
    if let Some(x) = st.ui.m3.coord.and_then(|id| st.project.model.coord(id)).and_then(|c| c.xform(&vars).ok()) {
        f = Frame { o: x.apply(f.o), u: x.apply_dir(f.u), v: x.apply_dir(f.v), n: x.apply_dir(f.n) };
        up = x.apply_dir(up);
    }
    (f, up)
}

fn on_grid(st: &Studio, v: f64) -> f64 {
    let g = st.ui.m3.grid_mm * 1e-3;
    if st.ui.m3.snap && g > 0.0 { (v / g).round() * g } else { v }
}

/// How far along `up` from `base` the pointer is, reading the two against each other on the screen.
/// Looking nearly along `up` there's no telling, and a little movement would be a long way.
fn height_at(pj: &Projector, base: P3, up: P3, at: Pos2) -> Option<f64> {
    let (a, b) = (pj.screen(base), pj.screen(add(base, up)));
    let d = b - a;
    let square = d.length_sq();
    if square.sqrt() < 0.15 * pj.points_per_metre() {
        return None;
    }
    Some(f64::from((at - a).dot(d)) / f64::from(square))
}

fn dist(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// What's been settled at step `k`.
fn settled(p: &Placing, k: usize) -> Option<&[(String, f64)]> {
    p.done.get(k).map(Vec::as_slice)
}

fn first_point(p: &Placing) -> Option<P2> {
    let d = settled(p, 0)?;
    Some([d.first()?.1, d.get(1)?.1])
}

/// This step's numbers for where the pointer is.
fn from_pointer(st: &Studio, p: &Placing, f: &Frame, up: P3, pj: &Projector, at: Pos2) -> Option<Vec<f64>> {
    let step = p.step_now();
    let q = model3d::screen_to_plane(f, pj, at);
    let base = first_point(p);
    Some(match step {
        Step::Corner | Step::Centre | Step::Start | Step::Next => {
            let q = q?;
            vec![on_grid(st, q[0]), on_grid(st, q[1])]
        }
        Step::Size | Step::Radii => {
            let (q, b) = (q?, base?);
            let d = [on_grid(st, q[0] - b[0]), on_grid(st, q[1] - b[1])];
            if step == Step::Radii { vec![d[0].abs(), d[1].abs()] } else { d.to_vec() }
        }
        Step::Radius => vec![on_grid(st, dist(q?, base?))],
        Step::Tube => {
            let big = settled(p, 1)?.first()?.1;
            vec![on_grid(st, (dist(q?, base?) - big).abs())]
        }
        Step::Height => {
            let b = base?;
            vec![on_grid(st, height_at(pj, f.at(b, 0.0), up, at)?)]
        }
    })
}

/// Follows the pointer, and draws what it comes to so far.
pub fn track(st: &mut Studio, painter: &egui::Painter, pj: &Projector, pointer: Option<Pos2>) {
    let Picking::Place(mut p) = st.ui.m3.picking.clone() else { return };
    let (f, up) = work_frame(st);
    if let Some(at) = pointer
        && let Some(values) = from_pointer(st, &p, &f, up, pj, at)
    {
        for (k, v) in values.into_iter().enumerate() {
            if !p.typed.get(k).copied().unwrap_or(true) {
                p.fields[k] = mm_text(v);
            }
        }
    }
    let vars = st.project.vars().unwrap_or_default();
    paint(painter, pj, &p, &f, up, &vars);
    st.ui.m3.picking = Picking::Place(p);
}

fn current_values(p: &Placing, vars: &Vars) -> Vec<Option<f64>> {
    p.fields.iter().map(|t| length(t, vars).ok()).collect()
}

fn paint(painter: &egui::Painter, pj: &Projector, p: &Placing, f: &Frame, up: P3, vars: &Vars) {
    let ink = Stroke::new(1.6, theme::ACCENT);
    let now = current_values(p, vars);
    let at = |u: f64, v: f64, h: f64| add(f.at([u, v], 0.0), scale(up, h));
    let line = |pts: &[P3], closed: bool| {
        for w in pts.windows(2) {
            painter.line_segment([pj.screen(w[0]), pj.screen(w[1])], ink);
        }
        if closed && pts.len() > 2 {
            painter.line_segment([pj.screen(pts[pts.len() - 1]), pj.screen(pts[0])], ink);
        }
    };
    let ring = |c: P2, rx: f64, ry: f64, n: usize, h: f64| -> Vec<P3> { (0..n).map(|k| { let t = std::f64::consts::TAU * k as f64 / n as f64; at(c[0] + rx * t.cos(), c[1] + ry * t.sin(), h) }).collect() };
    let step = p.step_now();
    // The first point, settled or still following the pointer.
    let base = first_point(p).or_else(|| match (step, now.as_slice()) {
        (Step::Corner | Step::Centre | Step::Start, [Some(u), Some(v)]) => Some([*u, *v]),
        _ => None,
    });
    let Some(b) = base else { return };
    painter.circle(pj.screen(at(b[0], b[1], 0.0)), 3.5, theme::ACCENT, Stroke::new(1.0, egui::Color32::WHITE));
    let one = |k: usize| -> Option<f64> {
        let in_step = p.shape.steps().iter().position(|s| *s == step).filter(|i| *i == k);
        if in_step.is_some() { now.first().copied().flatten() } else { settled(p, k).and_then(|d| d.first()).map(|x| x.1) }
    };
    let two = |k: usize| -> Option<(f64, f64)> {
        if p.step == k { Some((now.first().copied().flatten()?, now.get(1).copied().flatten()?)) } else { settled(p, k).map(|d| (d[0].1, d[1].1)) }
    };
    match p.shape {
        Shape::Rect | Shape::Box_ => {
            let Some((du, dv)) = two(1) else { return };
            let corners = |h: f64| vec![at(b[0], b[1], h), at(b[0] + du, b[1], h), at(b[0] + du, b[1] + dv, h), at(b[0], b[1] + dv, h)];
            line(&corners(0.0), true);
            if p.shape == Shape::Box_
                && let Some(h) = one(2)
            {
                line(&corners(h), true);
                for (x, y) in corners(0.0).into_iter().zip(corners(h)) {
                    line(&[x, y], false);
                }
            }
        }
        Shape::Circle | Shape::Polygon | Shape::Sphere => {
            let Some(r) = one(1) else { return };
            line(&ring(b, r, r, if p.shape == Shape::Polygon { 6 } else { 64 }, 0.0), true);
            if p.shape == Shape::Sphere {
                let u = ring(b, r, r, 64, 0.0);
                let lift: Vec<P3> = (0..64).map(|k| { let t = std::f64::consts::TAU * k as f64 / 64.0; at(b[0] + r * t.cos(), b[1], r * t.sin()) }).collect();
                line(&u, true);
                line(&lift, true);
            }
        }
        Shape::Ellipse => {
            let Some((rx, ry)) = two(1) else { return };
            line(&ring(b, rx, ry, 64, 0.0), true);
        }
        Shape::Cylinder | Shape::Cone => {
            let Some(r) = one(1) else { return };
            line(&ring(b, r, r, 64, 0.0), true);
            let Some(h) = one(2) else { return };
            if p.shape == Shape::Cylinder {
                line(&ring(b, r, r, 64, h), true);
                for s in [-1.0, 1.0] {
                    line(&[at(b[0] + s * r, b[1], 0.0), at(b[0] + s * r, b[1], h)], false);
                }
            } else {
                for s in [-1.0, 1.0] {
                    line(&[at(b[0] + s * r, b[1], 0.0), at(b[0], b[1], h)], false);
                }
            }
        }
        Shape::Torus => {
            let Some(big) = one(1) else { return };
            let small = one(2).unwrap_or(0.0);
            line(&ring(b, big + small, big + small, 64, 0.0), true);
            if small > 0.0 {
                line(&ring(b, (big - small).max(0.0), (big - small).max(0.0), 64, 0.0), true);
            }
        }
        Shape::Line | Shape::Polyline => {
            let mut pts: Vec<P3> = p.done.iter().filter(|d| d.len() == 2).map(|d| at(d[0].1, d[1].1, 0.0)).collect();
            if let [Some(u), Some(v)] = now.as_slice() {
                pts.push(at(*u, *v, 0.0));
            }
            line(&pts, false);
        }
    }
}

/// The numbers of this step, checked, or what's wrong with them.
fn read_fields(p: &Placing, axis: usize, vars: &Vars) -> Result<Vec<(String, f64)>, String> {
    let mut out = Vec::with_capacity(p.fields.len());
    for (text, name) in p.fields.iter().zip(field_names(p.step_now(), axis)) {
        let v = length(text, vars).map_err(|_| if text.trim().is_empty() { format!("{name} is empty.") } else { format!("{name}: '{text}' doesn't work out to a size.") })?;
        out.push((text.trim().to_string(), v));
    }
    Ok(out)
}

/// Settles this step with what the fields say, then asks for the next or makes the shape.
pub fn advance(st: &mut Studio) {
    let Picking::Place(mut p) = st.ui.m3.picking.clone() else { return };
    let vars = st.project.vars().unwrap_or_default();
    let values = match read_fields(&p, st.ui.m3.work_axis, &vars) {
        Ok(v) => v,
        Err(e) => {
            st.ui.m3.note = Some(e);
            return;
        }
    };
    let step = p.step_now();
    // A click that hasn't moved from the point before would make a shape with no size.
    let nothing = match step {
        Step::Size | Step::Radii => values.iter().any(|v| v.1 == 0.0),
        Step::Radius | Step::Tube | Step::Height => values[0].1 == 0.0,
        Step::Next => p.done.last().is_some_and(|d| d.len() == 2 && d[0].1 == values[0].1 && d[1].1 == values[1].1),
        _ => false,
    };
    if nothing {
        // The same point twice is how a polyline usually ends.
        if p.shape == Shape::Polyline && p.done.len() >= 2 {
            finish(st, &p);
        } else {
            st.ui.m3.note = Some("That has no size yet: move away from the last point, or type a number.".into());
        }
        return;
    }
    p.done.push(values);
    st.ui.m3.note = None;
    if p.shape == Shape::Polyline {
        p.step = 1;
    } else {
        p.step += 1;
        if p.step >= p.shape.steps().len() {
            finish(st, &p);
            return;
        }
    }
    p.fresh_fields();
    st.ui.m3.picking = Picking::Place(p);
}

fn plain(p: &Placing, k: usize, i: usize) -> String {
    p.done.get(k).and_then(|d| d.get(i)).map_or_else(|| "0".to_string(), |x| x.0.clone())
}

/// A point in the world's order, on the plane through the origin.
fn world(axis: usize, u: String, v: String, n: String) -> [String; 3] {
    let (a, b) = plane_axes(axis);
    let mut q = ["0".to_string(), "0".to_string(), "0".to_string()];
    q[a] = u;
    q[b] = v;
    q[axis] = n;
    q
}

/// What the finished steps make.
fn kind_of(p: &Placing, axis: usize, out: Output) -> Kind {
    let plane = PlaneRef::Base { axis, offset: "0".into() };
    let corner = || [plain(p, 0, 0), plain(p, 0, 1)];
    let base = || world(axis, plain(p, 0, 0), plain(p, 0, 1), "0".to_string());
    let tip = |h: String| world(axis, plain(p, 0, 0), plain(p, 0, 1), h);
    let route = || p.done.iter().map(|d| world(axis, d[0].0.clone(), d[1].0.clone(), "0".to_string())).collect::<Vec<_>>();
    match p.shape {
        Shape::Box_ => Kind::Solid { shape: Prim::Cuboid { p0: base(), size: world(axis, plain(p, 1, 0), plain(p, 1, 1), plain(p, 2, 0)) }, out },
        Shape::Cylinder => Kind::Solid { shape: Prim::Cylinder { a: base(), b: tip(plain(p, 2, 0)), r: plain(p, 1, 0) }, out },
        Shape::Cone => Kind::Solid { shape: Prim::Cone { a: base(), b: tip(plain(p, 2, 0)), r0: plain(p, 1, 0), r1: "0".into() }, out },
        Shape::Sphere => Kind::Solid { shape: Prim::Sphere { c: base(), r: plain(p, 1, 0) }, out },
        Shape::Torus => {
            let mut n = ["0".to_string(), "0".to_string(), "0".to_string()];
            n[axis] = "1".into();
            Kind::Solid { shape: Prim::Torus { c: base(), axis: n, big: plain(p, 1, 0), small: plain(p, 2, 0) }, out }
        }
        Shape::Rect => Kind::Face { plane, shape: Flat::Rect { p0: corner(), size: [plain(p, 1, 0), plain(p, 1, 1)] }, out },
        Shape::Circle => Kind::Face { plane, shape: Flat::Circle { c: corner(), r: plain(p, 1, 0) }, out },
        Shape::Ellipse => Kind::Face { plane, shape: Flat::Ellipse { c: corner(), rx: plain(p, 1, 0), ry: plain(p, 1, 1) }, out },
        Shape::Polygon => Kind::Face { plane, shape: Flat::Polygon { c: corner(), r: plain(p, 1, 0), sides: "6".into() }, out },
        Shape::Line | Shape::Polyline => Kind::Wire { path: PathRef::Points { pts: route() }, radius: "0.5mm".into(), out },
    }
}

/// Makes the shape, selected, with its numbers in the Parameters panel.
fn finish(st: &mut Studio, p: &Placing) {
    let out = Output { coord: st.ui.m3.coord, ..Default::default() };
    let kind = kind_of(p, st.ui.m3.work_axis, out);
    st.ui.m3.picking = Picking::Nothing;
    st.ui.pointer = Pointer::View;
    model3d::add_feature(st, kind);
}

/// A polyline ends when it's told to, with the points it has.
pub fn finish_polyline(st: &mut Studio) {
    let Picking::Place(p) = st.ui.m3.picking.clone() else { return };
    if p.shape == Shape::Polyline && p.done.len() >= 2 {
        finish(st, &p);
    }
}

/// The strip under the canvas with the numbers of the step in hand.
pub fn strip(ctx: &egui::Context, st: &mut Studio, view: Rect) {
    let Picking::Place(mut p) = st.ui.m3.picking.clone() else { return };
    let table = VarTable::of(&st.project);
    let axis = st.ui.m3.work_axis;
    let step = p.step_now();
    let names = field_names(step, axis);
    let (mut ok, mut cancel, mut end) = (false, false, false);
    egui::Area::new(egui::Id::new("studio_place_strip")).order(egui::Order::Middle).pivot(Align2::CENTER_BOTTOM).fixed_pos(pos2(view.center().x, view.bottom() - 8.0)).show(ctx, |ui| {
        theme::floating_frame().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(prompt(p.shape, step)).strong());
                for (k, name) in names.iter().enumerate() {
                    ui.add_space(6.0);
                    ui.label(name);
                    let r = length_line(ui, ("place_field", p.step, k), &mut p.fields[k], &table, 86.0);
                    if r.changed() {
                        p.typed[k] = true;
                    }
                    ok |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                }
                ui.add_space(6.0);
                ok |= ui.button("OK").on_hover_text("Take these numbers, as typed. A variable's name stays tied to it. A click on the canvas does the same.").clicked();
                if p.shape == Shape::Polyline && p.done.len() >= 2 {
                    end |= ui.button("Finish").on_hover_text("End the line here (Enter)").clicked();
                }
                cancel |= ui.button("Cancel").on_hover_text("Stop placing it (Esc)").clicked();
            });
        });
    });
    if matches!(st.ui.m3.picking, Picking::Place(_)) {
        st.ui.m3.picking = Picking::Place(p);
    }
    if ok {
        advance(st);
    }
    if end {
        finish_polyline(st);
    }
    if cancel {
        st.ui.m3.picking = Picking::Nothing;
        st.ui.pointer = Pointer::View;
        st.ui.m3.note = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use antenna_core::modeler::BodyKind;
    use antenna_core::project::Project;

    fn studio() -> Studio {
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        st.ui.m3.grid_mm = 1.0;
        st
    }

    /// Types this step's numbers and takes them, as OK does.
    fn typed(st: &mut Studio, values: &[&str]) {
        let Picking::Place(p) = &mut st.ui.m3.picking else { panic!("not placing") };
        for (k, v) in values.iter().enumerate() {
            p.fields[k] = (*v).to_string();
            p.typed[k] = true;
        }
        advance(st);
    }

    fn only_body(st: &Studio) -> std::sync::Arc<antenna_core::modeler::Body> {
        let b = st.project.model.build(&st.project.vars().unwrap());
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.bodies.len(), 1);
        b.bodies[0].clone()
    }

    #[test]
    fn a_box_takes_a_corner_a_size_and_a_height() {
        let mut st = studio();
        start(&mut st, Shape::Box_);
        assert!(model3d::drawing(&st), "the left button places it");
        typed(&mut st, &["1mm", "2mm"]);
        typed(&mut st, &["10mm", "20mm"]);
        assert!(matches!(st.ui.m3.picking, Picking::Place(_)), "it still wants a height");
        typed(&mut st, &["5mm"]);
        assert_eq!(st.ui.m3.picking, Picking::Nothing);
        assert_eq!(st.ui.pointer, Pointer::View, "and hands the mouse back");
        let b = only_body(&st);
        let (lo, hi) = b.mesh.bounds();
        assert_eq!(lo, [0.001, 0.002, 0.0]);
        assert!((hi[0] - 0.011).abs() < 1e-12 && (hi[1] - 0.022).abs() < 1e-12 && (hi[2] - 0.005).abs() < 1e-12, "{hi:?}");
        assert_eq!(st.ui.m3.sel.feature, Some(b.id), "selected, so its numbers show");
    }

    #[test]
    fn a_name_typed_stays_a_variable() {
        let mut st = studio();
        antenna_core::sketch::add_variable(&mut st.project, "L", "40mm", "").unwrap();
        start(&mut st, Shape::Rect);
        typed(&mut st, &["0", "0"]);
        typed(&mut st, &["L", "10mm"]);
        let Some(Kind::Face { shape: Flat::Rect { size, .. }, .. }) = st.project.model.features.last().map(|f| &f.kind) else { panic!() };
        assert_eq!(size[0], "L");
        assert!((only_body(&st).mesh.area() - 0.04 * 0.01).abs() < 1e-12);
    }

    #[test]
    fn shapes_land_on_the_work_plane() {
        let mut st = studio();
        st.ui.m3.work_axis = 1;
        start(&mut st, Shape::Cylinder);
        typed(&mut st, &["3mm", "4mm"]);
        typed(&mut st, &["2mm"]);
        typed(&mut st, &["10mm"]);
        let b = only_body(&st);
        let (lo, hi) = b.mesh.bounds();
        // On the x-z plane it stands along y, from the plane, at x = 3, z = 4.
        assert!((lo[1]).abs() < 1e-9 && (hi[1] - 0.01).abs() < 1e-9, "{lo:?} {hi:?}");
        assert!(((lo[0] + hi[0]) / 2.0 - 0.003).abs() < 1e-9 && ((lo[2] + hi[2]) / 2.0 - 0.004).abs() < 1e-9, "{lo:?} {hi:?}");
    }

    #[test]
    fn it_lands_in_the_coordinate_system_in_use() {
        let mut st = studio();
        let id = st.project.model.add_coord();
        st.project.model.coords[0].origin = ["100mm".into(), "0".into(), "0".into()];
        st.ui.m3.coord = Some(id);
        start(&mut st, Shape::Circle);
        typed(&mut st, &["0", "0"]);
        typed(&mut st, &["5mm"]);
        let (lo, hi) = only_body(&st).mesh.bounds();
        assert!(((lo[0] + hi[0]) / 2.0 - 0.1).abs() < 1e-9, "{lo:?} {hi:?}");
    }

    #[test]
    fn a_polyline_goes_on_until_told_to_stop_and_a_line_stops_by_itself() {
        let mut st = studio();
        start(&mut st, Shape::Polyline);
        for q in [["0", "0"], ["10mm", "0"], ["10mm", "10mm"]] {
            typed(&mut st, &q);
        }
        assert!(matches!(st.ui.m3.picking, Picking::Place(_)));
        finish_polyline(&mut st);
        let b = only_body(&st);
        assert_eq!(b.kind, BodyKind::Wire);
        assert_eq!(b.wires[0].0.len(), 3);

        let mut st = studio();
        start(&mut st, Shape::Line);
        typed(&mut st, &["0", "0"]);
        typed(&mut st, &["10mm", "0"]);
        assert_eq!(st.ui.m3.picking, Picking::Nothing);
        assert_eq!(only_body(&st).wires[0].0.len(), 2);
    }

    #[test]
    fn nothing_is_made_from_no_size_or_from_something_that_does_not_work_out() {
        let mut st = studio();
        start(&mut st, Shape::Rect);
        typed(&mut st, &["0", "0"]);
        typed(&mut st, &["0", "0"]);
        assert!(st.ui.m3.note.is_some(), "it says why");
        assert!(matches!(st.ui.m3.picking, Picking::Place(_)), "and goes on waiting");
        typed(&mut st, &["nope", "1mm"]);
        assert!(st.ui.m3.note.as_deref().unwrap().contains("nope"));
        assert!(st.project.model.is_empty());
    }
}
