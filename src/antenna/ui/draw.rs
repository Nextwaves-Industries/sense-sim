//! Drawing an antenna on the canvas: pick a tool, click points on a plane, drag
//! corners, cut slots, click a feed. The shapes are ordinary parts of the project,
//! so what is drawn solves, saves and exports like a template.

use antenna_core::cad::P2;
use antenna_core::geometry::{P3, plane_point};
use antenna_core::material;
use antenna_core::expr::{self, Vars};
use antenna_core::project::{ExtraPrim, ExtraSolid, Form, Project};
use antenna_core::sketch::{self, Sheet};
use bevy_egui::egui::{self, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Response, RichText, Stroke, pos2, vec2};

use super::view3d::{Camera, Projector, ViewMode};
use super::{Page, group, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tool {
    #[default]
    Rect,
    Select,
    Polygon,
    Strip,
    Circle,
    /// A wire through space: each point lands on the plane as it stands at that click.
    Wire,
    Feed,
    Probe,
}

/// A press being held on a part.
#[derive(Clone, Copy, Debug)]
enum Drag {
    Corner { from: P2 },
    Body { last: P2 },
}

pub struct DrawUi {
    pub tool: Tool,
    /// The plane's normal axis and height expression.
    pub axis: usize,
    pub at: String,
    pub material: String,
    pub thick: String,
    pub grid_mm: f64,
    pub snap: bool,
    pub strip_mm: f64,
    /// A shape cuts a hole instead of adding metal.
    pub cut: bool,
    /// The shape in progress, in the plane's own coordinates.
    pub points: Vec<P2>,
    /// The wire in progress, in space: the plane can change between its points.
    pub wire: Vec<P3>,
    pub selected: Option<usize>,
    /// What else is selected, by Ctrl or Shift, in the order it was picked.
    pub also: Vec<usize>,
    /// The names at those places when they were picked, all of them, the first one first.
    also_names: Vec<String>,
    pub(super) editing_vars: bool,
    drag: Option<Drag>,
    undo: Vec<Project>,
    redo: Vec<Project>,
    /// The canvas stays put while the model grows.
    pub frame: Option<(P3, f64)>,
    pub note: Option<String>,
    /// What's in the Exact point fields.
    typed: [String; 2],
    /// The points of the shape in progress that were typed: where each landed, and how it was written.
    typed_points: Vec<(P2, [String; 2])>,
    editing_corner: bool,
    /// The next feed drawn is this switch or varactor instead.
    pub next_part: Option<antenna_core::reconfig::Device>,
    /// What a dimension's new variable is being called.
    naming: String,
    /// A new variable being typed: its name and value.
    new_var: [String; 2],
}

impl Default for DrawUi {
    fn default() -> Self {
        Self {
            tool: Tool::Rect,
            axis: 2,
            at: "0".into(),
            material: String::new(),
            thick: "1.6mm".into(),
            grid_mm: 0.0,
            snap: true,
            strip_mm: 2.0,
            cut: false,
            points: Vec::new(),
            wire: Vec::new(),
            selected: None,
            also: Vec::new(),
            also_names: Vec::new(),
            editing_vars: false,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            frame: None,
            note: None,
            typed: ["0".to_string(), "0".to_string()],
            typed_points: Vec::new(),
            editing_corner: false,
            next_part: None,
            naming: String::new(),
            new_var: [String::new(), String::new()],
        }
    }
}

impl DrawUi {
    /// A corner or a part is being dragged: the mesh can wait until it's let go.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

const AXIS: [&str; 3] = ["x", "y", "z"];
const PLANES: [(usize, &str); 3] = [(2, "x–y"), (0, "y–z"), (1, "z–x")];
/// Points within this of a corner land on it.
const SNAP_PX: f32 = 8.0;

pub fn remember(st: &mut Studio) {
    let copy = st.project.clone();
    let d = &mut st.ui.draw;
    d.undo.push(copy);
    if d.undo.len() > 60 {
        d.undo.remove(0);
    }
    d.redo.clear();
}

pub(super) fn undo(st: &mut Studio) {
    if let Some(p) = st.ui.draw.undo.pop() {
        st.ui.m3.forget_what_was_in_hand();
        let now = std::mem::replace(&mut st.project, p);
        st.ui.draw.redo.push(now);
        st.ui.draw.selected = None;
        st.ui.draw.points.clear();
    }
}

pub(super) fn redo(st: &mut Studio) {
    if let Some(p) = st.ui.draw.redo.pop() {
        st.ui.m3.forget_what_was_in_hand();
        let now = std::mem::replace(&mut st.project, p);
        st.ui.draw.undo.push(now);
        st.ui.draw.selected = None;
    }
}

/// Looks straight at a plane.
fn face_on(cam: &mut Camera, axis: usize) {
    let half_pi = std::f32::consts::FRAC_PI_2;
    (cam.yaw, cam.pitch) = match axis {
        2 => (0.0, half_pi),
        0 => (half_pi, 0.0),
        _ => (0.0, 0.0),
    };
    cam.zoom = 1.0;
    cam.pan = egui::Vec2::ZERO;
}

/// Frames the model, or a wavelength of empty plane.
fn fit(st: &mut Studio) {
    let lambda = antenna_core::C0 / st.project.f0().max(1.0);
    st.ui.draw.frame = Some(match st.ui.model.as_ref().filter(|g| !g.solids.is_empty()) {
        Some(g) => {
            let (lo, hi) = g.bounds();
            let e = antenna_core::geometry::norm(antenna_core::geometry::sub(hi, lo)) / 2.0;
            (std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0), (e * 1.15).max(lambda / 20.0))
        }
        None => ([0.0; 3], lambda / 2.0),
    });
    st.ui.cam.zoom = 1.0;
    st.ui.cam.pan = egui::Vec2::ZERO;
}

/// A round grid for the wavelength: about a hundredth of one.
fn nice_grid(lambda_mm: f64) -> f64 {
    let raw = lambda_mm / 100.0;
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    (if n < 1.5 { 1.0 } else if n < 3.5 { 2.0 } else if n < 7.5 { 5.0 } else { 10.0 }) * mag
}

/// On arriving at the page: straight at the plane, framed, with a sensible grid.
pub fn entered(st: &mut Studio) {
    st.ui.view = ViewMode::Model;
    let axis = st.ui.draw.axis;
    face_on(&mut st.ui.cam, axis);
    fit(st);
    ensure_grid(st);
}

/// A grid to suit the design's wavelength, until there is one. The page needs this too: arriving
/// from a nav button, it draws before `entered` runs, and the Grid field would clamp an unset
/// grid to its 0.01 mm minimum, which then stays.
fn ensure_grid(st: &mut Studio) {
    if st.ui.draw.grid_mm <= 0.0 {
        st.ui.draw.grid_mm = nice_grid(antenna_core::C0 / st.project.f0().max(1.0) * 1e3);
    }
}

/// The grid to draw on the canvas: `(axis, height, step in metres)`.
pub fn canvas_grid(st: &Studio) -> Option<(usize, f64, f64)> {
    let h = sketch::height_of(&st.project, &st.ui.draw.at).ok()?;
    Some((st.ui.draw.axis, h, st.ui.draw.grid_mm * 1e-3))
}

fn uv(axis: usize, q: P3) -> P2 {
    [q[(axis + 1) % 3], q[(axis + 2) % 3]]
}

fn on_plane(pj: &Projector, at: Pos2, axis: usize, h: f64) -> Option<P2> {
    let (o, d) = pj.ray(at);
    if d[axis].abs() < 0.05 {
        return None;
    }
    let t = (h - o[axis]) / d[axis];
    Some(uv(axis, [o[0] + d[0] * t, o[1] + d[1] * t, o[2] + d[2] * t]))
}

/// A corner nearby if there is one, otherwise the grid.
fn snapped(st: &Studio, pj: &Projector, q: P2, h: f64) -> (P2, bool) {
    let d = &st.ui.draw;
    if !d.snap {
        return (q, false);
    }
    let screen = |p: P2| pj.screen(plane_point(d.axis, h, p));
    let here = screen(q);
    let mut best: Option<(f32, P2)> = None;
    let mut consider = |p: P2| {
        let dist = screen(p).distance(here);
        if dist < SNAP_PX && best.is_none_or(|b| dist < b.0) {
            best = Some((dist, p));
        }
    };
    if let Some(g) = st.ui.model.as_ref() {
        for o in sketch::flat_metal(g, d.axis, h) {
            o.iter().for_each(|p| consider(*p));
        }
    }
    d.points.iter().for_each(|p| consider(*p));
    if let Some((_, p)) = best {
        return (p, true);
    }
    let step = (d.grid_mm * 1e-3).max(1e-6);
    ([(q[0] / step).round() * step, (q[1] / step).round() * step], false)
}

fn sheet_of(st: &Studio) -> Sheet {
    let d = &st.ui.draw;
    let material = if d.material.is_empty() { st.project.metal.clone() } else { d.material.clone() };
    let thick = if material::by_name(&material).is_some_and(|m| !m.is_conductor()) { d.thick.clone() } else { "0".to_string() };
    Sheet { axis: d.axis, at: d.at.clone(), material, thick }
}

/// A shape as it comes off the canvas, by what it is rather than by its corners.
enum Finished {
    Rect(P2, P2),
    Circle(P2, f64),
    Poly(Vec<P2>),
    Strip(Vec<P2>, f64),
}

/// Adds a finished shape, keeping its dimensions, or cuts it from the metal under it. What's
/// added is selected, so its dimensions are there to type straight away.
fn finish_shape(st: &mut Studio, base: &str, shape: Finished) {
    remember(st);
    let sheet = sheet_of(st);
    let h = sketch::height_of(&st.project, &sheet.at);
    let cut = st.ui.draw.cut && !matches!(shape, Finished::Strip(..));
    let done = if cut {
        let pts = match &shape {
            Finished::Rect(a, b) => sketch::rectangle(*a, *b),
            Finished::Circle(c, r) => sketch::circle(*c, *r, 50e-6),
            Finished::Poly(pts) | Finished::Strip(pts, _) => pts.clone(),
        };
        h.and_then(|h| sketch::cut_in(&mut st.project, sheet.axis, h, &pts))
    } else {
        match &shape {
            Finished::Rect(a, b) => sketch::add_rect(&mut st.project, base, &sheet, *a, *b),
            Finished::Circle(c, r) => sketch::add_circle(&mut st.project, base, &sheet, *c, *r),
            Finished::Poly(pts) => sketch::add_poly(&mut st.project, base, &sheet, pts),
            Finished::Strip(path, w) => sketch::add_strip(&mut st.project, base, &sheet, path, *w),
        }
    };
    match done {
        Ok(_) => {
            st.ui.draw.note = None;
            if !cut {
                st.ui.draw.selected = st.project.extra.len().checked_sub(1);
                st.ui.draw.also.clear();
                // Points typed as expressions stay expressions, so the shape follows its variables.
                let typed = std::mem::take(&mut st.ui.draw.typed_points);
                if let (Ok(vars), Some(part)) = (st.project.vars(), st.project.extra.last_mut()) {
                    sketch::keep_typed(&mut part.prim, &typed, &vars);
                }
            }
        }
        Err(e) => {
            st.ui.draw.undo.pop();
            st.ui.draw.note = Some(format!("{e:#}"));
        }
    }
    st.ui.draw.points.clear();
    st.ui.draw.typed_points.clear();
}

/// The point in the Exact point fields, for the tool in hand. It says so if they don't work out.
fn add_typed_point(st: &mut Studio) {
    let vars = st.project.vars().unwrap_or_default();
    let read = |t: &str| sketch::typed_length(t, &vars);
    match (read(&st.ui.draw.typed[0]), read(&st.ui.draw.typed[1])) {
        (Ok((u, wu)), Ok((v, wv))) => {
            st.ui.draw.note = None;
            st.ui.draw.typed_points.push(([u, v], [wu, wv]));
            add_point(st, [u, v]);
        }
        (Err(e), _) | (_, Err(e)) => st.ui.draw.note = Some(format!("{e:#}")),
    }
}

/// The wire in progress becomes a part. It has to conduct, whatever the sheet material is.
fn finish_wire(st: &mut Studio) {
    let pts = std::mem::take(&mut st.ui.draw.wire);
    if pts.len() < 2 {
        return;
    }
    remember(st);
    let chosen = &st.ui.draw.material;
    let material = if material::by_name(chosen).is_some_and(|m| m.is_conductor()) { chosen.clone() } else { st.project.metal.clone() };
    let name = (1..).map(|k| format!("Wire{k}")).find(|n| !st.project.extra.iter().any(|e| e.name == *n)).unwrap_or_default();
    let mm = |v: f64| format!("{}mm", (v * 1e6).round() / 1e3);
    let prim = ExtraPrim::Wire { pts: pts.iter().map(|p| p.map(mm)).collect(), r: mm(st.ui.draw.strip_mm * 0.5e-3) };
    st.project.extra.push(ExtraSolid { name, material, prim });
    st.ui.draw.selected = st.project.extra.len().checked_sub(1);
    st.ui.draw.also.clear();
    st.ui.draw.note = None;
}

fn finish_path(st: &mut Studio) {
    if st.ui.draw.tool == Tool::Wire {
        finish_wire(st);
        return;
    }
    let pts = std::mem::take(&mut st.ui.draw.points);
    match st.ui.draw.tool {
        Tool::Polygon if pts.len() >= 3 => finish_shape(st, "Polygon", Finished::Poly(pts)),
        Tool::Strip if pts.len() >= 2 => {
            let w = st.ui.draw.strip_mm * 1e-3;
            finish_shape(st, "Strip", Finished::Strip(pts, w));
        }
        _ => {}
    }
}

/// A click or a typed point, for the tool in hand.
fn add_point(st: &mut Studio, q: P2) {
    let tool = st.ui.draw.tool;
    match tool {
        Tool::Rect | Tool::Circle => {
            let Some(first) = st.ui.draw.points.first().copied() else {
                st.ui.draw.points.push(q);
                return;
            };
            if first == q {
                return;
            }
            st.ui.draw.points.clear();
            if tool == Tool::Rect {
                if first[0] != q[0] && first[1] != q[1] {
                    finish_shape(st, "Rectangle", Finished::Rect(first, q));
                } else {
                    st.ui.draw.note = Some("A rectangle needs two corners that differ both ways.".into());
                }
            } else {
                let r = (q[0] - first[0]).hypot(q[1] - first[1]);
                finish_shape(st, "Circle", Finished::Circle(first, r));
            }
        }
        Tool::Polygon | Tool::Strip => {
            // Clicking the last point again ends the path, however slow the double click.
            if st.ui.draw.points.last() == Some(&q) {
                finish_path(st);
            } else {
                st.ui.draw.points.push(q);
            }
        }
        Tool::Wire => {
            let h = match sketch::height_of(&st.project, &st.ui.draw.at) {
                Ok(h) => h,
                Err(e) => {
                    st.ui.draw.note = Some(format!("{e:#}"));
                    return;
                }
            };
            let p = plane_point(st.ui.draw.axis, h, q);
            // The same point again ends the wire, however slow the double click.
            if st.ui.draw.wire.last() == Some(&p) {
                finish_wire(st);
            } else {
                st.ui.draw.wire.push(p);
            }
        }
        Tool::Feed => {
            let Some(first) = st.ui.draw.points.first().copied() else {
                st.ui.draw.points.push(q);
                return;
            };
            st.ui.draw.points.clear();
            remember(st);
            let (axis, at) = (st.ui.draw.axis, st.ui.draw.at.clone());
            match sketch::add_feed(&mut st.project, axis, &at, first, q) {
                Ok(port) => {
                    st.ui.draw.note = None;
                    if let Some(device) = st.ui.draw.next_part.take() {
                        let rc = &mut st.project.reconfig;
                        let name = (1..).map(|k| format!("D{k}")).find(|n| !rc.elements.iter().any(|e| e.name == *n)).unwrap_or_else(|| "D".to_string());
                        rc.add_element(antenna_core::reconfig::Element { name, port, device, mount: Default::default() });
                        st.ui.page = super::Page::Reconfig;
                    }
                }
                Err(e) => {
                    st.ui.draw.undo.pop();
                    st.ui.draw.note = Some(format!("{e:#}"));
                }
            }
        }
        Tool::Probe => {
            remember(st);
            let axis = st.ui.draw.axis;
            match sketch::add_probe(&mut st.project, axis, q) {
                Ok(_) => st.ui.draw.note = None,
                Err(e) => {
                    st.ui.draw.undo.pop();
                    st.ui.draw.note = Some(format!("{e:#}"));
                }
            }
        }
        Tool::Select => {}
    }
}

/// The selected part's plane and outline, if it's flat and still there.
fn selected(st: &Studio) -> Option<(usize, usize, f64, Vec<P2>)> {
    let i = st.ui.draw.selected?;
    let vars = st.project.vars().ok()?;
    sketch::flat_parts(&st.project, &vars).into_iter().find(|f| f.index == i).map(|f| (i, f.axis, f.at, f.outline.into_owned()))
}

/// The plane a flat part is on.
fn plane_of(prim: &ExtraPrim) -> Option<(usize, &String)> {
    match prim {
        ExtraPrim::Outline { axis, at, .. } | ExtraPrim::Drawn { axis, at, .. } | ExtraPrim::Polygon { axis, at, .. } | ExtraPrim::Plate { axis, at, .. } | ExtraPrim::Strip { axis, at, .. } | ExtraPrim::Disc { axis, at, .. } | ExtraPrim::Arc { axis, at, .. } | ExtraPrim::Meander { axis, at, .. } => Some((*axis, at)),
        _ => None,
    }
}

fn select_part(st: &mut Studio, part: Option<usize>) {
    st.ui.draw.selected = part;
    st.ui.draw.also.clear();
    if let Some((axis, at)) = part.and_then(|i| st.project.extra.get(i)).and_then(|e| plane_of(&e.prim)) {
        // The plane follows, so edits and the grid are on the part you're looking at.
        st.ui.draw.axis = axis;
        st.ui.draw.at = at.clone();
    }
}

/// In if it's out, out if it's in. Leaving the first one out makes the next the first.
fn toggle_part(st: &mut Studio, part: usize) {
    let d = &mut st.ui.draw;
    if d.selected == Some(part) {
        let next = (!d.also.is_empty()).then(|| d.also.remove(0));
        d.selected = next;
    } else if let Some(at) = d.also.iter().position(|i| *i == part) {
        d.also.remove(at);
    } else if d.selected.is_none() {
        select_part(st, Some(part));
    } else {
        d.also.push(part);
    }
    st.ui.draw.also_names = names_chosen(st);
}

fn names_chosen(st: &Studio) -> Vec<String> {
    st.ui.draw.selected.into_iter().chain(st.ui.draw.also.iter().copied()).map(|i| st.project.extra.get(i).map_or(String::new(), |e| e.name.clone())).collect()
}

/// The shapes selected, first picked first. They're held by place in the list, and the list can
/// change under them: another project opened, a part removed on another page, an undo. Then the
/// names no longer match, and what's selected along with the first is let go, so nothing can be
/// joined or deleted that nobody picked.
fn chosen_parts(st: &mut Studio) -> Vec<usize> {
    if !st.ui.draw.also.is_empty() && names_chosen(st) != st.ui.draw.also_names {
        st.ui.draw.also.clear();
    }
    st.ui.draw.selected.into_iter().chain(st.ui.draw.also.iter().copied()).collect()
}

/// Combines the shapes selected, which become what's left of them, selected.
pub fn combine_selected(st: &mut Studio, op: antenna_core::modeler::CombineOp) {
    let parts = chosen_parts(st);
    if parts.len() < 2 {
        st.ui.draw.note = Some("Ctrl+click at least two shapes first, on the canvas or in the Shapes list. Then choose this again.".into());
        return;
    }
    remember(st);
    match sketch::combine_parts(&mut st.project, &parts, op) {
        Ok(made) => {
            st.ui.draw.selected = made.first().copied();
            st.ui.draw.also = made.get(1..).unwrap_or_default().to_vec();
            st.ui.draw.also_names = names_chosen(st);
            st.ui.draw.note = None;
        }
        Err(e) => {
            st.ui.draw.undo.pop();
            st.ui.draw.note = Some(format!("{e:#}"));
        }
    }
}

/// Several shapes selected: what they are, and what can be done with them together.
fn selection_card(ui: &mut egui::Ui, st: &mut Studio) {
    use antenna_core::modeler::CombineOp;
    let parts = chosen_parts(st);
    let names: Vec<String> = parts.iter().filter_map(|i| st.project.extra.get(*i).map(|e| e.name.clone())).collect();
    let mut ask = None;
    group(ui, |ui| {
        theme::caption(ui, &format!("{} shapes selected", names.len()));
        weak(ui, names.join(", "));
        ui.horizontal(|ui| {
            for (op, word, what) in [(CombineOp::Add, "Join", "Everything selected, as one"), (CombineOp::Cut, "Subtract", "The first selected, less the others"), (CombineOp::Common, "Intersect", "Only where all of them overlap"), (CombineOp::Exclude, "Exclude", "Where they don't overlap")] {
                if ui.button(word).on_hover_text(what).clicked() {
                    ask = Some(op);
                }
            }
        });
        weak(ui, format!("Subtract keeps {} and takes the others from it. The result's sizes are numbers: it no longer follows any variable the shapes used.", names.first().map_or("the first", String::as_str)));
    });
    if let Some(op) = ask {
        combine_selected(st, op);
    }
}

/// Says why the mouse stopped short: the coordinate belongs to a variable.
const TIED: &str = "Part of this is tied to a variable, so the mouse leaves it there. Change the variable, or type a number in its place.";

fn edges(pts: &[P2]) -> impl Iterator<Item = (usize, P2, P2)> + '_ {
    let n = pts.len();
    (0..n).map(move |i| (i, pts[i], pts[(i + 1) % n])).filter(move |(_, a, b)| {
        // A keyhole's bridge runs both ways and is no edge of the metal.
        !(0..n).any(|j| pts[j] == *b && pts[(j + 1) % n] == *a)
    })
}

/// The tool on the canvas, once the model has been drawn underneath.
pub fn canvas(ui: &mut egui::Ui, st: &mut Studio, area: Rect, view: Rect, resp: &Response, pj: &Projector) {
    if st.ui.page != Page::Draw || !matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh) {
        return;
    }
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(area);
    let axis = st.ui.draw.axis;
    let h = match sketch::height_of(&st.project, &st.ui.draw.at) {
        Ok(h) => h,
        Err(e) => {
            st.ui.draw.note = Some(format!("{e:#}"));
            return;
        }
    };
    if let Some(p) = st.ui.draw.selected
        && st.project.extra.get(p).is_none()
    {
        st.ui.draw.selected = None;
    }
    chosen_parts(st);
    let pointer = ctx.input(|i| i.pointer.latest_pos()).filter(|_| resp.contains_pointer() || resp.dragged());
    let raw = pointer.and_then(|p| on_plane(pj, p, axis, h));
    let cursor = raw.map(|q| snapped(st, pj, q, h));
    let alt = ctx.input(|i| i.modifiers.alt);
    let to_screen = |q: P2| pj.screen(plane_point(axis, h, q));

    // Keys, unless someone's typing in a field.
    if !ctx.egui_wants_keyboard_input() {
        let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if key(Modifiers::COMMAND, Key::Z) {
            undo(st);
        }
        if key(Modifiers::COMMAND, Key::Y) || key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
            redo(st);
        }
        if key(Modifiers::NONE, Key::Escape) {
            st.ui.draw.points.clear();
            st.ui.draw.wire.clear();
            st.ui.draw.note = None;
        }
        if key(Modifiers::NONE, Key::Enter) {
            finish_path(st);
        }
        if key(Modifiers::NONE, Key::Backspace) || key(Modifiers::NONE, Key::Delete) {
            if !st.ui.draw.points.is_empty() {
                st.ui.draw.points.pop();
            } else if st.ui.draw.wire.pop().is_some() {
                // One point of the wire back.
            } else if st.ui.draw.selected.is_some() {
                remember(st);
                let mut gone = chosen_parts(st);
                gone.sort_unstable();
                for i in gone.into_iter().rev() {
                    if i < st.project.extra.len() {
                        st.project.extra.remove(i);
                    }
                }
                st.ui.draw.selected = None;
                st.ui.draw.also.clear();
            }
        }
    }

    let tool = st.ui.draw.tool;
    if !alt && let Some((q, _)) = cursor {
        if tool == Tool::Select {
            select_tool(st, resp, pj, q, pointer);
        } else {
            let closing = tool == Tool::Polygon && st.ui.draw.points.len() >= 3 && pointer.is_some_and(|p| to_screen(st.ui.draw.points[0]).distance(p) < SNAP_PX);
            if resp.double_clicked() && matches!(tool, Tool::Polygon | Tool::Strip | Tool::Wire) {
                finish_path(st);
            } else if resp.clicked() {
                if closing { finish_path(st) } else { add_point(st, q) }
            } else if resp.drag_started_by(egui::PointerButton::Primary) && matches!(tool, Tool::Rect | Tool::Circle | Tool::Feed) {
                // Press where the drag began, not where it was noticed.
                let start = ctx.input(|i| i.pointer.press_origin()).and_then(|p| on_plane(pj, p, axis, h)).map_or(q, |o| snapped(st, pj, o, h).0);
                st.ui.draw.points = vec![start];
            } else if resp.drag_stopped_by(egui::PointerButton::Primary) && matches!(tool, Tool::Rect | Tool::Circle | Tool::Feed) && !st.ui.draw.points.is_empty() {
                add_point(st, q);
            }
        }
    }

    // ------- What the tool shows
    let outline = Stroke::new(1.0, Color32::from_black_alpha(110));
    let vars = st.project.vars().unwrap_or_default();
    for f in sketch::flat_parts(&st.project, &vars) {
        let (a, part_h) = (f.axis, f.at);
        let sel = st.ui.draw.selected == Some(f.index) || st.ui.draw.also.contains(&f.index);
        let stroke = if sel { Stroke::new(2.0, theme::ACCENT) } else { outline };
        for (_, p, q) in edges(&f.outline) {
            painter.line_segment([pj.screen(plane_point(a, part_h, p)), pj.screen(plane_point(a, part_h, q))], stroke);
        }
    }
    if let Some((i, a, part_h, _)) = selected(st).filter(|_| tool == Tool::Select) {
        for p in &sketch::handles(&st.project, i) {
            let c = pj.screen(plane_point(a, part_h, *p));
            let near = pointer.is_some_and(|m| m.distance(c) < SNAP_PX);
            let r = Rect::from_center_size(c, vec2(7.0, 7.0));
            painter.rect_filled(r, 1.0, if near { theme::ACCENT } else { Color32::WHITE });
            painter.rect_stroke(r, 1.0, Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Outside);
        }
    }
    let live = Stroke::new(1.5, if st.ui.draw.cut { theme::DANGER } else { theme::ACCENT });
    let points = st.ui.draw.points.clone();
    let mut size = None;
    // The wire so far, wherever the pointer is: it may be off the plane the pointer's on.
    for w in st.ui.draw.wire.windows(2) {
        painter.line_segment([pj.screen(w[0]), pj.screen(w[1])], Stroke::new(2.5, live.color));
    }
    for p in &st.ui.draw.wire {
        painter.circle_filled(pj.screen(*p), 3.5, live.color);
    }
    if let (Some((q, on_corner)), false) = (cursor, alt) {
        match tool {
            Tool::Wire => {
                let here = plane_point(axis, h, q);
                if let Some(last) = st.ui.draw.wire.last() {
                    painter.line_segment([pj.screen(*last), pj.screen(here)], Stroke::new(1.5, live.color));
                    size = Some(format!("{:.2} mm", antenna_core::geometry::norm(antenna_core::geometry::sub(here, *last)) * 1e3));
                }
            }
            Tool::Rect | Tool::Circle if !points.is_empty() => {
                let a = points[0];
                if tool == Tool::Rect {
                    let c = sketch::rectangle(a, q);
                    for k in 0..4 {
                        painter.line_segment([to_screen(c[k]), to_screen(c[(k + 1) % 4])], live);
                    }
                    size = Some(format!("{:.2} × {:.2} mm", (q[0] - a[0]).abs() * 1e3, (q[1] - a[1]).abs() * 1e3));
                } else {
                    let r = (q[0] - a[0]).hypot(q[1] - a[1]);
                    let c = sketch::circle(a, r, 50e-6);
                    for k in 0..c.len() {
                        painter.line_segment([to_screen(c[k]), to_screen(c[(k + 1) % c.len()])], live);
                    }
                    size = Some(format!("radius {:.2} mm", r * 1e3));
                }
            }
            Tool::Polygon | Tool::Strip if !points.is_empty() => {
                for w in points.windows(2) {
                    painter.line_segment([to_screen(w[0]), to_screen(w[1])], live);
                }
                if let Some(last) = points.last() {
                    painter.line_segment([to_screen(*last), to_screen(q)], Stroke::new(1.0, live.color));
                    size = Some(format!("{:.2} mm", (q[0] - last[0]).hypot(q[1] - last[1]) * 1e3));
                }
                if tool == Tool::Strip {
                    let mut with = points.clone();
                    with.push(q);
                    if let Ok(ring) = sketch::strip(&with, st.ui.draw.strip_mm * 1e-3) {
                        for k in 0..ring.len() {
                            painter.line_segment([to_screen(ring[k]), to_screen(ring[(k + 1) % ring.len()])], Stroke::new(1.0, live.color.gamma_multiply(0.6)));
                        }
                    }
                }
                for p in &points {
                    painter.circle_filled(to_screen(*p), 3.0, live.color);
                }
            }
            Tool::Feed if !points.is_empty() => {
                let b = sketch::along_axis(points[0], q);
                painter.line_segment([to_screen(points[0]), to_screen(b)], Stroke::new(2.5, theme::DANGER));
                size = Some(format!("{:.2} mm", (b[0] - points[0][0]).hypot(b[1] - points[0][1]) * 1e3));
            }
            Tool::Probe => {
                painter.circle_stroke(to_screen(q), 5.0, Stroke::new(2.0, theme::DANGER));
            }
            _ => {}
        }
        if tool != Tool::Select {
            let c = to_screen(q);
            let col = if on_corner { theme::SUCCESS } else { theme::ACCENT };
            painter.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], Stroke::new(1.0, col));
            painter.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], Stroke::new(1.0, col));
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let (un, vn) = (AXIS[(axis + 1) % 3], AXIS[(axis + 2) % 3]);
        let mut read = format!("{un} {:.2}  {vn} {:.2} mm", q[0] * 1e3, q[1] * 1e3);
        if tool == Tool::Wire {
            read = format!("{read}  {} {:.2} mm", AXIS[axis], h * 1e3);
        }
        if let Some(s) = &size {
            read = format!("{s}   ·   {read}");
        }
        let at = pointer.map_or(view.center(), |p| p + vec2(14.0, 14.0));
        let g = painter.layout_no_wrap(read, FontId::proportional(12.0), theme::TEXT);
        let bg = Rect::from_min_size(at, g.size() + vec2(10.0, 6.0));
        painter.rect_filled(bg, 4.0, Color32::from_white_alpha(235));
        painter.galley(at + vec2(5.0, 3.0), g, theme::TEXT);
    }
    let hint = hint_for(st, raw.is_some());
    painter.text(pos2(view.center().x, view.bottom() - 34.0), Align2::CENTER_BOTTOM, hint, FontId::proportional(13.0), theme::WEAK);
}

fn hint_for(st: &Studio, plane_visible: bool) -> String {
    let d = &st.ui.draw;
    if !plane_visible && st.ui.draw.tool != Tool::Select {
        return "Move over the canvas to draw. If you're looking along the plane, press Face on.".into();
    }
    let begun = !d.points.is_empty();
    let what = if d.cut { "the hole" } else { "the shape" };
    match d.tool {
        Tool::Select => "Click a shape to edit it, Ctrl+click for several. Double-click an edge for a new corner; right-click a corner to remove it.".to_string(),
        Tool::Rect if begun => format!("Click the opposite corner of {what}. Esc cancels."),
        Tool::Rect => "Click one corner, or drag from corner to corner.".to_string(),
        Tool::Circle if begun => "Click the edge. Esc cancels.".to_string(),
        Tool::Circle => "Click the centre, or drag from centre to edge.".to_string(),
        Tool::Polygon if begun => "Keep clicking corners. Click the first or the last again, or press Enter, to close. Backspace takes one back.".to_string(),
        Tool::Polygon => "Click the first corner.".to_string(),
        Tool::Strip if begun => "Keep clicking along the path. Click the last point again or press Enter to finish.".to_string(),
        Tool::Strip => "Click where the strip starts.".to_string(),
        Tool::Wire if !d.wire.is_empty() => "Keep clicking. Change Height or Plane to climb out of this plane. Press Enter or click the last point again to finish.".to_string(),
        Tool::Wire => "Click where the wire starts. Change Height or Plane between clicks to draw in 3D.".to_string(),
        Tool::Feed if begun => "Click the other side of the gap.".to_string(),
        Tool::Feed => "Click one side of the gap, then the other. The feed lands on the metal, along an axis.".to_string(),
        Tool::Probe => "Click a point over two sheets: the probe joins them.".to_string(),
    }
}

fn select_tool(st: &mut Studio, resp: &Response, pj: &Projector, q: P2, pointer: Option<Pos2>) {
    let primary = egui::PointerButton::Primary;
    let sel = selected(st);
    let grips = sel.as_ref().map_or(Vec::new(), |s| sketch::handles(&st.project, s.0));
    let corner_near = |at: Pos2, sel: &Option<(usize, usize, f64, Vec<P2>)>| -> Option<P2> {
        let (_, a, part_h, _) = sel.as_ref()?;
        grips.iter().copied().min_by(|x, y| pj.screen(plane_point(*a, *part_h, *x)).distance(at).total_cmp(&pj.screen(plane_point(*a, *part_h, *y)).distance(at))).filter(|p| pj.screen(plane_point(*a, *part_h, *p)).distance(at) < SNAP_PX)
    };
    if resp.drag_started_by(primary) {
        let origin = resp.ctx.input(|i| i.pointer.press_origin());
        st.ui.draw.note = None;
        if let Some(from) = origin.and_then(|o| corner_near(o, &sel)) {
            remember(st);
            st.ui.draw.drag = Some(Drag::Corner { from });
        } else if let Some((i, a, part_h, pts)) = &sel
            && let Some(o) = origin.and_then(|o| on_plane(pj, o, *a, *part_h))
            && antenna_core::cad::point_inside(pts, o)
            && *i < st.project.extra.len()
        {
            remember(st);
            st.ui.draw.drag = Some(Drag::Body { last: o });
        }
    }
    if resp.dragged_by(primary)
        && let (Some(drag), Some((i, ..))) = (st.ui.draw.drag, sel.as_ref())
    {
        let i = *i;
        match drag {
            Drag::Corner { from } => {
                let reached = sketch::move_corner(&mut st.project, i, from, q);
                st.ui.draw.drag = Some(Drag::Corner { from: reached });
                if (reached[0] - q[0]).abs() + (reached[1] - q[1]).abs() > 1e-9 && sketch::is_tied(&st.project, i) {
                    st.ui.draw.note = Some(TIED.into());
                }
            }
            Drag::Body { last } => {
                let by = [q[0] - last[0], q[1] - last[1]];
                if by != [0.0, 0.0] {
                    let went = sketch::move_part(&mut st.project, i, by);
                    st.ui.draw.drag = Some(Drag::Body { last: q });
                    if went != by {
                        st.ui.draw.note = Some(TIED.into());
                    }
                }
            }
        }
        resp.ctx.request_repaint();
    }
    if resp.drag_stopped_by(primary) {
        st.ui.draw.drag = None;
    }
    if resp.double_clicked()
        && let (Some((i, a, part_h, pts)), Some(at)) = (&sel, pointer)
    {
        // A new corner on the edge nearest the pointer, if it's close.
        let dist = |p: Pos2, u: Pos2, v: Pos2| {
            let d = v - u;
            let t = ((p - u).dot(d) / d.length_sq().max(1e-6)).clamp(0.0, 1.0);
            (u + d * t).distance(p)
        };
        let near = edges(pts).map(|(k, p, r)| (k, dist(at, pj.screen(plane_point(*a, *part_h, p)), pj.screen(plane_point(*a, *part_h, r))))).min_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((k, d)) = near
            && d < SNAP_PX
        {
            let i = *i;
            remember(st);
            sketch::insert_corner(&mut st.project, i, k, q);
        }
    } else if resp.clicked() {
        let (o, d) = pointer.map_or(([0.0; 3], [0.0, 0.0, -1.0]), |p| pj.ray(p));
        let picked = sketch::pick(&st.project, o, d).map(|x| x.0);
        if resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift) {
            if let Some(i) = picked {
                toggle_part(st, i);
            }
        } else {
            select_part(st, picked);
        }
    }
    if resp.secondary_clicked()
        && let (Some((i, ..)), Some(at)) = (&sel, pointer)
        && let Some(c) = corner_near(at, &sel)
    {
        let i = *i;
        remember(st);
        if let Err(e) = sketch::remove_corner(&mut st.project, i, c) {
            st.ui.draw.undo.pop();
            st.ui.draw.note = Some(format!("{e:#}"));
        }
    }
}

/// How wide a dimension's expression field is.
const DIM_W: f32 = 104.0;

/// One dimension: its name, its expression with variables offered as you type, what that comes
/// to if it uses a variable, and a way to turn it into a variable of its own.
fn dim(ui: &mut egui::Ui, st: &mut Studio, salt: &str, label: &str, text: &mut String, table: &super::expr_field::VarTable, vars: &Vars) {
    ui.label(label);
    ui.horizontal(|ui| {
        // An arc's bare number is degrees, where `deg` would convert to radians, and a count has no unit.
        let bare = label == "Periods" || label.ends_with("degrees");
        let line = if bare { super::expr_field::expr_line } else { super::expr_field::length_line };
        line(ui, ("draw_dim", salt), text, table, DIM_W);
        name_it(ui, st, salt, label, text);
        if !sketch::is_free(text) {
            let read = expr::eval(text, vars).map(|v| format!("= {} mm", (v * 1e6).round() / 1e3));
            match read {
                Ok(r) => ui.label(RichText::new(r).size(12.0).color(theme::WEAK)),
                Err(_) => ui.label(RichText::new("doesn't work out").size(12.0).color(theme::DANGER)),
            };
        }
    });
    ui.end_row();
}

/// A small menu that makes a new variable from what's in a field, and puts its name there.
pub(super) fn name_it(ui: &mut egui::Ui, st: &mut Studio, salt: &str, note: &str, text: &mut String) {
    ui.push_id(("name_it", salt), |ui| {
        // A menu shuts on any click by default, even in its own text field.
        let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (button, _) = egui::containers::menu::MenuButton::new("x=").config(stay_open).ui(ui, |ui| {
            ui.label(RichText::new("Make this a variable").size(12.5));
            let field = ui.add(egui::TextEdit::singleline(&mut st.ui.draw.naming).hint_text("Name, like L").desired_width(120.0));
            if ui.ctx().memory(|m| m.focused().is_none()) {
                field.request_focus();
            }
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.button("Create").clicked() || entered {
                let name = st.ui.draw.naming.trim().to_string();
                let copy = st.project.clone();
                match sketch::add_variable(&mut st.project, &name, text, note) {
                    Ok(()) => {
                        let now = std::mem::replace(&mut st.project, copy);
                        remember(st);
                        st.project = now;
                        *text = name;
                        st.ui.draw.naming.clear();
                        st.ui.draw.note = None;
                        ui.close();
                    }
                    Err(e) => st.ui.draw.note = Some(format!("{e:#}")),
                }
            }
        });
        button.on_hover_text("Make this a variable, to tune it by name");
    });
}

/// A list of points, each coordinate an expression.
fn point_rows(ui: &mut egui::Ui, salt: &str, heads: (&str, &str), pts: &mut [[String; 2]], table: &super::expr_field::VarTable) {
    theme::caption(ui, &format!("{salt}, {} and {}", heads.0, heads.1));
    egui::ScrollArea::vertical().id_salt(("draw_pts", salt)).max_height(170.0).show(ui, |ui| {
        egui::Grid::new(("draw_pt_grid", salt)).num_columns(3).spacing(vec2(8.0, 3.0)).show(ui, |ui| {
            for (k, q) in pts.iter_mut().enumerate() {
                ui.label(RichText::new(format!("{}", k + 1)).size(11.5).color(theme::WEAK));
                super::expr_field::length_line(ui, (salt, k, 0), &mut q[0], table, DIM_W);
                super::expr_field::length_line(ui, (salt, k, 1), &mut q[1], table, DIM_W);
                ui.end_row();
            }
        });
    });
}

/// The dimensions of a part, by kind, in a copy that's put back if any of them changed.
fn dimensions(ui: &mut egui::Ui, st: &mut Studio, i: usize, table: &super::expr_field::VarTable, vars: &Vars) {
    let Some(mut prim) = st.project.extra.get(i).map(|e| e.prim.clone()) else { return };
    let before = prim.clone();
    let metal = st.project.extra[i].material.clone();
    let solid = material::by_name(&metal).is_some_and(|m| !m.is_conductor());
    let axis = plane_of(&prim).map(|p| p.0);
    let (un, vn) = axis.map_or(("x", "y"), |a| (AXIS[(a + 1) % 3], AXIS[(a + 2) % 3]));
    let along = |what: &str, n: &str| format!("{what} {n}");
    let mut lists: Vec<(String, &mut Vec<[String; 2]>)> = Vec::new();
    egui::Grid::new(("draw_dims", i)).num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
        match &mut prim {
            ExtraPrim::Drawn { at, thick, form, holes, .. } => {
                dim(ui, st, "height", "Height", at, table, vars);
                if solid {
                    dim(ui, st, "thick", "Thickness", thick, table, vars);
                }
                match form {
                    Form::Rect { c, size } => {
                        let [cu, cv] = c;
                        let [su, sv] = size;
                        dim(ui, st, "cu", &along("Centre", un), cu, table, vars);
                        dim(ui, st, "cv", &along("Centre", vn), cv, table, vars);
                        dim(ui, st, "su", &along("Length along", un), su, table, vars);
                        dim(ui, st, "sv", &along("Length along", vn), sv, table, vars);
                    }
                    Form::Circle { c, r } => {
                        let [cu, cv] = c;
                        dim(ui, st, "cu", &along("Centre", un), cu, table, vars);
                        dim(ui, st, "cv", &along("Centre", vn), cv, table, vars);
                        dim(ui, st, "r", "Radius", r, table, vars);
                    }
                    Form::Poly { pts } => lists.push(("Corners".into(), pts)),
                }
                for (k, h) in holes.iter_mut().enumerate() {
                    lists.push((format!("Hole {}", k + 1), h));
                }
            }
            ExtraPrim::Polygon { at, pts, thick, .. } => {
                dim(ui, st, "height", "Height", at, table, vars);
                if solid {
                    dim(ui, st, "thick", "Thickness", thick, table, vars);
                }
                lists.push(("Corners".into(), pts));
            }
            ExtraPrim::Plate { at, outline, holes, .. } => {
                dim(ui, st, "height", "Height", at, table, vars);
                lists.push(("Corners".into(), outline));
                for (k, h) in holes.iter_mut().enumerate() {
                    lists.push((format!("Hole {}", k + 1), h));
                }
            }
            ExtraPrim::Strip { at, path, w, .. } => {
                dim(ui, st, "height", "Height", at, table, vars);
                dim(ui, st, "w", "Width", w, table, vars);
                lists.push(("Path".into(), path));
            }
            ExtraPrim::Disc { at, c, r, .. } => {
                let [cu, cv] = c;
                dim(ui, st, "height", "Height", at, table, vars);
                dim(ui, st, "cu", &along("Centre", un), cu, table, vars);
                dim(ui, st, "cv", &along("Centre", vn), cv, table, vars);
                dim(ui, st, "r", "Radius", r, table, vars);
            }
            ExtraPrim::Arc { at, c, r, from_deg, to_deg, w, .. } => {
                let [cu, cv] = c;
                dim(ui, st, "height", "Height", at, table, vars);
                dim(ui, st, "cu", &along("Centre", un), cu, table, vars);
                dim(ui, st, "cv", &along("Centre", vn), cv, table, vars);
                dim(ui, st, "r", "Radius", r, table, vars);
                dim(ui, st, "from", "From, degrees", from_deg, table, vars);
                dim(ui, st, "to", "To, degrees", to_deg, table, vars);
                dim(ui, st, "w", "Width", w, table, vars);
            }
            ExtraPrim::Meander { at, start, step, height, n, w, .. } => {
                let [su, sv] = start;
                dim(ui, st, "height", "Height", at, table, vars);
                dim(ui, st, "su", &along("Start", un), su, table, vars);
                dim(ui, st, "sv", &along("Start", vn), sv, table, vars);
                dim(ui, st, "step", "Step", step, table, vars);
                dim(ui, st, "fold", "Fold height", height, table, vars);
                dim(ui, st, "n", "Periods", n, table, vars);
                dim(ui, st, "w", "Width", w, table, vars);
            }
            ExtraPrim::Wire { pts, r } => {
                dim(ui, st, "r", "Radius", r, table, vars);
                for (k, q) in pts.iter_mut().enumerate() {
                    let [x, y, z] = q;
                    dim(ui, st, &format!("p{k}x"), &format!("Point {} x", k + 1), x, table, vars);
                    dim(ui, st, &format!("p{k}y"), &format!("Point {} y", k + 1), y, table, vars);
                    dim(ui, st, &format!("p{k}z"), &format!("Point {} z", k + 1), z, table, vars);
                }
            }
            ExtraPrim::Box { min, max } => {
                for (k, (lo, hi)) in min.iter_mut().zip(max.iter_mut()).enumerate() {
                    dim(ui, st, &format!("lo{k}"), &format!("From {}", AXIS[k]), lo, table, vars);
                    dim(ui, st, &format!("hi{k}"), &format!("To {}", AXIS[k]), hi, table, vars);
                }
            }
            ExtraPrim::Cylinder { a, b, r } => {
                dim(ui, st, "r", "Radius", r, table, vars);
                for (k, (p, q)) in a.iter_mut().zip(b.iter_mut()).enumerate() {
                    dim(ui, st, &format!("a{k}"), &format!("From {}", AXIS[k]), p, table, vars);
                    dim(ui, st, &format!("b{k}"), &format!("To {}", AXIS[k]), q, table, vars);
                }
            }
            ExtraPrim::Sphere { c, r } => {
                dim(ui, st, "r", "Radius", r, table, vars);
                for (k, p) in c.iter_mut().enumerate() {
                    dim(ui, st, &format!("c{k}"), &format!("Centre {}", AXIS[k]), p, table, vars);
                }
            }
            ExtraPrim::Outline { at, thick, .. } => {
                dim(ui, st, "height", "Height", at, table, vars);
                dim(ui, st, "thick", "Thickness", thick, table, vars);
            }
            ExtraPrim::Imported { .. } => {}
        }
    });
    for (title, pts) in lists {
        point_rows(ui, &title, (un, vn), pts, table);
    }
    if prim != before {
        if !st.ui.draw.editing_corner {
            remember(st);
            st.ui.draw.editing_corner = true;
        }
        st.project.extra[i].prim = prim;
    }
}

pub(super) fn properties(ui: &mut egui::Ui, st: &mut Studio) {
    if chosen_parts(st).len() > 1 {
        selection_card(ui, st);
        return;
    }
    let Some(i) = st.ui.draw.selected.filter(|i| *i < st.project.extra.len()) else { return };
    let names: Vec<String> = material::library().into_iter().map(|m| m.name).collect();
    let table = super::expr_field::VarTable::of(&st.project);
    let vars = st.project.vars().unwrap_or_default();
    group(ui, |ui| {
        theme::caption(ui, "Selected shape");
        let e = &mut st.project.extra[i];
        egui::Grid::new("draw_props").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(160.0));
            ui.end_row();
            ui.label("Material");
            egui::ComboBox::from_id_salt("draw_part_mat").selected_text(e.material.clone()).width(150.0).show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut e.material, n.clone(), n);
                }
            });
            ui.end_row();
        });
        dimensions(ui, st, i, &table, &vars);
        // An outline from a file or a template stays in numbers until it's asked for.
        if !matches!(st.project.extra.get(i).map(|e| &e.prim), Some(ExtraPrim::Outline { .. })) {
            return;
        }
        if ui.button(theme::label(icon::NOTE_PENCIL, "Give it dimensions")).on_hover_text("Lets its corners, or a rectangle's centre and sides, use expressions and variables").clicked() {
            remember(st);
            if let Err(e) = sketch::make_drawn(&mut st.project, i) {
                st.ui.draw.undo.pop();
                st.ui.draw.note = Some(format!("{e:#}"));
            }
            return;
        }
        let Some((_, axis, _, pts)) = selected(st) else { return };
        let mut corners: Vec<P2> = Vec::with_capacity(pts.len());
        for p in &pts {
            if !corners.contains(p) {
                corners.push(*p);
            }
        }
        let (un, vn) = (AXIS[(axis + 1) % 3], AXIS[(axis + 2) % 3]);
        theme::caption(ui, &format!("Corners, {un} and {vn} in mm"));
        let mut change = None;
        let mut released = false;
        egui::ScrollArea::vertical().id_salt("draw_corners").max_height(170.0).show(ui, |ui| {
            egui::Grid::new("draw_corner_grid").num_columns(3).spacing(vec2(8.0, 3.0)).show(ui, |ui| {
                for (k, c) in corners.iter().enumerate() {
                    ui.label(RichText::new(format!("{}", k + 1)).size(11.5).color(theme::WEAK));
                    let mut mm = [c[0] * 1e3, c[1] * 1e3];
                    for (slot, v) in mm.iter_mut().enumerate() {
                        let r = ui.add(egui::DragValue::new(v).speed(st.ui.draw.grid_mm.max(0.01)).max_decimals(3));
                        if r.changed() {
                            change = Some((*c, slot, *v * 1e-3));
                        }
                        released |= r.drag_stopped() || r.lost_focus();
                    }
                    ui.end_row();
                }
            });
        });
        if let Some((from, slot, value)) = change {
            if !st.ui.draw.editing_corner {
                remember(st);
                st.ui.draw.editing_corner = true;
            }
            let mut to = from;
            to[slot] = value;
            sketch::move_corner(&mut st.project, i, from, to);
        }
        if released {
            st.ui.draw.editing_corner = false;
        }
    });
}

/// The Draw page: tools, the plane, and everything drawn so far.
pub fn page(ui: &mut egui::Ui, st: &mut Studio) {
    ensure_grid(st);
    theme::caption(ui, "Draw");
    if let Some(d) = &st.ui.draw.next_part {
        let part = d.part.clone();
        super::verdict(ui, false, &format!("Click one side of a gap, then the other: {part} lands there. To cancel, choose another tool."));
        if st.ui.draw.tool != Tool::Feed {
            st.ui.draw.next_part = None;
        }
    }
    weak(ui, "Draw your own antenna on the canvas: metal first, then a feed. Right-drag pans, the wheel zooms, middle-drag or Alt-drag orbits. Wire draws in 3D.");
    if st.project.template != "custom" {
        group(ui, |ui| {
            weak(ui, "This model's own parts come from its template, which ties them to the variable table. Make them editable to change their shapes: they then take their own dimensions, which you can name again. What you draw here is added either way.");
            if ui.button(theme::label(icon::NOTE_PENCIL, "Make every part editable")).clicked() {
                let copy = st.project.editable_copy();
                match copy {
                    Ok(p) => {
                        remember(st);
                        st.project = p;
                        st.ui.draw.note = None;
                    }
                    Err(e) => st.ui.draw.note = Some(format!("{e:#}")),
                }
            }
        });
    }
    group(ui, |ui| {
        let mut tool = st.ui.draw.tool;
        ui.horizontal_wrapped(|ui| {
            theme::segmented(ui, &mut tool, &[(Tool::Select, "Select"), (Tool::Rect, "Rectangle"), (Tool::Polygon, "Polygon"), (Tool::Strip, "Strip"), (Tool::Circle, "Circle"), (Tool::Wire, "Wire")]);
            // On a line of their own: side by side the two groups run past the card.
            ui.end_row();
            theme::segmented(ui, &mut tool, &[(Tool::Feed, "Feed"), (Tool::Probe, "Probe")]);
        });
        if tool != st.ui.draw.tool {
            st.ui.draw.tool = tool;
            st.ui.draw.points.clear();
            st.ui.draw.wire.clear();
            st.ui.draw.note = None;
            // A wire through space is drawn looking at space, not square on to one plane.
            if tool == Tool::Wire {
                let view = Camera::default();
                (st.ui.cam.yaw, st.ui.cam.pitch) = (view.yaw, view.pitch);
            }
        }
        let names: Vec<(String, bool)> = material::library().into_iter().map(|m| { let c = m.is_conductor(); (m.name, c) }).collect();
        let table = super::expr_field::VarTable::of(&st.project);
        egui::Grid::new("draw_opts").num_columns(2).spacing(vec2(10.0, 5.0)).show(ui, |ui| {
            ui.label("Plane");
            ui.horizontal(|ui| {
                let mut axis = st.ui.draw.axis;
                theme::segmented(ui, &mut axis, &PLANES);
                if axis != st.ui.draw.axis {
                    st.ui.draw.axis = axis;
                    st.ui.draw.points.clear();
                    // A wire carries on from the new plane, seen as it was.
                    if st.ui.draw.tool != Tool::Wire {
                        face_on(&mut st.ui.cam, axis);
                    }
                }
                if ui.small_button("Face on").clicked() {
                    face_on(&mut st.ui.cam, st.ui.draw.axis);
                }
            });
            ui.end_row();
            ui.label("Height");
            super::expr_field::length_line(ui, "draw height", &mut st.ui.draw.at, &table, 100.0).on_hover_text("Where the plane sits. An expression: 0, h, 1.6mm");
            ui.end_row();
            ui.label("Material");
            let now = if st.ui.draw.material.is_empty() { st.project.metal.clone() } else { st.ui.draw.material.clone() };
            egui::ComboBox::from_id_salt("draw_mat").selected_text(now.clone()).width(150.0).show_ui(ui, |ui| {
                for (n, _) in &names {
                    if ui.selectable_label(*n == now, n).clicked() {
                        st.ui.draw.material = n.clone();
                    }
                }
            });
            ui.end_row();
            if names.iter().any(|(n, metal)| *n == now && !*metal) {
                ui.label("Thickness");
                super::expr_field::length_line(ui, "draw thickness", &mut st.ui.draw.thick, &table, 100.0);
                ui.end_row();
            }
            ui.label("Grid");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut st.ui.draw.grid_mm).range(0.01..=100.0).speed(0.05).suffix(" mm").max_decimals(2));
                ui.checkbox(&mut st.ui.draw.snap, "Snap");
            });
            ui.end_row();
            if matches!(st.ui.draw.tool, Tool::Strip | Tool::Wire) {
                ui.label(if st.ui.draw.tool == Tool::Wire { "Wire diameter" } else { "Strip width" });
                ui.add(egui::DragValue::new(&mut st.ui.draw.strip_mm).range(0.01..=500.0).speed(0.05).suffix(" mm").max_decimals(2));
                ui.end_row();
            }
        });
        if matches!(st.ui.draw.tool, Tool::Rect | Tool::Polygon | Tool::Circle) {
            ui.checkbox(&mut st.ui.draw.cut, "Cut a hole instead of adding metal");
        }
        if st.ui.draw.tool != Tool::Select {
            let axis = st.ui.draw.axis;
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Exact point, {} and {}", AXIS[(axis + 1) % 3], AXIS[(axis + 2) % 3])).size(12.5).color(theme::WEAK));
                let table = super::expr_field::VarTable::of(&st.project);
                let tip = "A number in mm, or an expression over your variables, like a1+2mm. A strip or polygon keeps it, so the point follows the variable.";
                super::expr_field::length_line(ui, "draw typed u", &mut st.ui.draw.typed[0], &table, 72.0).on_hover_text(tip);
                super::expr_field::length_line(ui, "draw typed v", &mut st.ui.draw.typed[1], &table, 72.0).on_hover_text(tip);
                if ui.small_button("Add").clicked() {
                    add_typed_point(st);
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!st.ui.draw.undo.is_empty(), egui::Button::new(theme::label(icon::ARROW_U_UP_LEFT, "Undo"))).on_hover_text("Ctrl+Z").clicked() {
                undo(st);
            }
            if ui.add_enabled(!st.ui.draw.redo.is_empty(), egui::Button::new(theme::label(icon::ARROW_U_UP_RIGHT, "Redo"))).on_hover_text("Ctrl+Y").clicked() {
                redo(st);
            }
            if ui.button(theme::label(icon::FRAME_CORNERS, "Fit")).clicked() {
                fit(st);
            }
        });
    });
    if let Some(n) = st.ui.draw.note.clone() {
        verdict(ui, false, &n);
    }
    properties(ui, st);

    egui::CollapsingHeader::new(RichText::new("Variables").color(theme::TEXT)).id_salt("draw_vars").default_open(true).show(ui, |ui| {
        weak(ui, "Write a shape's dimensions in these, and one change moves everything that uses it. Sweeps and the optimiser work on them by name.");
        let table = super::expr_field::VarTable::of(&st.project);
        super::design::variable_rows(ui, st, &table);
        ui.horizontal(|ui| {
            let [name, value] = &mut st.ui.draw.new_var;
            ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(70.0));
            super::expr_field::expr_line(ui, "draw new var", value, &table, DIM_W);
            if ui.button(theme::label(icon::PLUS, "Add")).clicked() {
                let [name, value] = st.ui.draw.new_var.clone();
                let value = if value.trim().is_empty() { "0".to_string() } else { value };
                remember(st);
                match sketch::add_variable(&mut st.project, &name, &value, "Your own variable") {
                    Ok(()) => {
                        st.ui.draw.new_var = [String::new(), String::new()];
                        st.ui.draw.note = None;
                    }
                    Err(e) => {
                        st.ui.draw.undo.pop();
                        st.ui.draw.note = Some(format!("{e:#}"));
                    }
                }
            }
        });
    });

    theme::caption(ui, "Shapes");
    let shapes: Vec<(usize, String, String)> = st.project.extra.iter().enumerate().map(|(i, e)| (i, e.name.clone(), e.material.clone())).collect();
    if shapes.is_empty() {
        weak(ui, "Nothing drawn yet.");
    }
    let mut pick_part = None;
    let mut drop_part = None;
    egui::Grid::new("draw_shapes").num_columns(3).spacing(vec2(10.0, 2.0)).striped(true).show(ui, |ui| {
        for (i, name, mat) in &shapes {
            if ui.selectable_label(st.ui.draw.selected == Some(*i) || st.ui.draw.also.contains(i), name).clicked() {
                pick_part = Some(*i);
            }
            ui.label(RichText::new(mat).size(12.0).color(theme::WEAK));
            if theme::icon_button(ui, icon::TRASH, "Remove this shape").clicked() {
                drop_part = Some(*i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = pick_part {
        if ui.input(|i| i.modifiers.ctrl || i.modifiers.shift) {
            toggle_part(st, i);
        } else {
            select_part(st, Some(i));
        }
        st.ui.draw.tool = Tool::Select;
    }
    if let Some(i) = drop_part {
        remember(st);
        st.project.extra.remove(i);
        st.ui.draw.selected = None;
        st.ui.draw.also.clear();
    }

    theme::caption(ui, "Feeds");
    let ports: Vec<String> = st.project.extra_ports.iter().map(|p| p.name.clone()).collect();
    let template_ports = st.ui.geometry.as_ref().map_or(0, |g| g.ports.len()).saturating_sub(ports.len());
    if ports.is_empty() && template_ports == 0 {
        weak(ui, "None yet. An antenna needs one before it can be solved: choose Feed and click across a gap, or Probe to join two sheets.");
    }
    if template_ports > 0 {
        weak(ui, format!("{template_ports} from the template."));
    }
    let mut drop_port = None;
    let table = super::expr_field::VarTable::of(&st.project);
    let vars = st.project.vars().unwrap_or_default();
    let mut edited = st.project.extra_ports.clone();
    let mut wave_feeds = st.project.wave_feeds.clone();
    for (i, port) in edited.iter_mut().enumerate() {
        egui::CollapsingHeader::new(RichText::new(port.name.clone()).size(12.5)).id_salt(("draw_feed", i)).show(ui, |ui| {
            egui::Grid::new(("draw_feed_grid", i)).num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut port.name).desired_width(120.0));
                ui.end_row();
                let [ax, ay, az] = &mut port.a;
                dim(ui, st, &format!("f{i}ax"), "From x", ax, &table, &vars);
                dim(ui, st, &format!("f{i}ay"), "From y", ay, &table, &vars);
                dim(ui, st, &format!("f{i}az"), "From z", az, &table, &vars);
                let [bx, by, bz] = &mut port.b;
                dim(ui, st, &format!("f{i}bx"), "To x", bx, &table, &vars);
                dim(ui, st, &format!("f{i}by"), "To y", by, &table, &vars);
                dim(ui, st, &format!("f{i}bz"), "To z", bz, &table, &vars);
                dim(ui, st, &format!("f{i}w"), "Width", &mut port.width, &table, &vars);
                ui.label("Impedance");
                super::expr_field::expr_line(ui, ("draw_feed_z0", i), &mut port.z0, &table, DIM_W);
                ui.end_row();
            });
            let mut wave = wave_feeds.contains(&port.name);
            let hint = "Launches the line's own mode instead of driving a gap, and reads what comes back, as a connector would see it. The feed has to sit on the outer edge of the model, across a line that runs straight into it. It sets the impedance from the line, and only the built-in FDM solver has it: others run it as a gap.";
            if ui.checkbox(&mut wave, "Wave port").on_hover_text(hint).changed() {
                if wave {
                    wave_feeds.push(port.name.clone());
                } else {
                    wave_feeds.retain(|f| f != &port.name);
                }
            }
            if ui.button(theme::label(icon::TRASH, "Remove this feed")).clicked() {
                drop_port = Some(i);
            }
        });
    }
    // A wave port stays one when its feed is renamed.
    for (old, new) in st.project.extra_ports.iter().zip(&edited) {
        if old.name != new.name {
            wave_feeds.iter_mut().filter(|f| **f == old.name).for_each(|f| *f = new.name.clone());
        }
    }
    if wave_feeds != st.project.wave_feeds {
        remember(st);
        st.project.wave_feeds = wave_feeds;
    }
    if edited != st.project.extra_ports {
        if !st.ui.draw.editing_corner {
            remember(st);
            st.ui.draw.editing_corner = true;
        }
        st.project.extra_ports = edited;
    }
    // One undo step per field: the next edit after leaving it is a new one.
    if !ui.ctx().egui_wants_keyboard_input() && !super::expr_field::scrubbing(ui.ctx()) {
        st.ui.draw.editing_corner = false;
    }
    if let Some(i) = drop_port {
        remember(st);
        st.project.extra_ports.remove(i);
    }
    match &st.ui.geometry {
        Ok(_) => {
            if super::run_button(ui, &theme::label(icon::PLAY, "Solve this antenna"), !st.busy()) {
                st.ui.page = Page::Setup;
                st.run();
            }
        }
        // Having no feed yet is already said just above.
        Err(e) if !e.is_empty() && !shapes.is_empty() && !ports.is_empty() => weak(ui, e.clone()),
        _ => {}
    }
    super::import::panel(ui, st);
}

#[cfg(test)]
mod tests {
    use super::*;
    use antenna_core::geometry::Shape;

    fn close(a: P3, b: P3) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9)
    }

    /// The plane moves between clicks and the wire goes with it, which is how it leaves a plane.
    #[test]
    fn a_wire_climbs_when_the_plane_moves_between_clicks() {
        let mut st = Studio::default();
        let parts = st.project.extra.len();
        st.ui.draw.tool = Tool::Wire;
        add_point(&mut st, [0.0, 0.0]);
        st.ui.draw.at = "10mm".into();
        add_point(&mut st, [0.0, 0.0]);
        // Over to the y-z plane at x = 5 mm: its own coordinates are y and z.
        st.ui.draw.axis = 0;
        st.ui.draw.at = "5mm".into();
        add_point(&mut st, [0.0, 0.01]);
        finish_path(&mut st);

        assert!(st.ui.draw.wire.is_empty());
        assert_eq!(st.project.extra.len(), parts + 1);
        let geometry = st.project.geometry().unwrap();
        let Some(Shape::Wire { pts, r }) = geometry.solids.last().map(|s| &s.shape) else { panic!("no wire") };
        assert_eq!(pts.len(), 3);
        assert!(close(pts[0], [0.0, 0.0, 0.0]) && close(pts[1], [0.0, 0.0, 0.01]) && close(pts[2], [0.005, 0.0, 0.01]), "{pts:?}");
        assert!((r - 1e-3).abs() < 1e-9, "half the 2 mm diameter: {r}");

        undo(&mut st);
        assert_eq!(st.project.extra.len(), parts, "one undo step");
    }

    #[test]
    fn the_last_point_again_finishes_a_wire_and_one_point_makes_nothing() {
        let mut st = Studio::default();
        let parts = st.project.extra.len();
        st.ui.draw.tool = Tool::Wire;
        add_point(&mut st, [0.0, 0.0]);
        add_point(&mut st, [0.0, 0.0]);
        assert_eq!(st.project.extra.len(), parts, "a single point isn't a wire");
        assert!(st.ui.draw.wire.is_empty());

        add_point(&mut st, [0.0, 0.0]);
        add_point(&mut st, [0.01, 0.0]);
        add_point(&mut st, [0.01, 0.0]);
        assert_eq!(st.project.extra.len(), parts + 1);
    }

    fn two_shapes() -> Studio {
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        st.ui.draw.axis = 2;
        st.ui.draw.tool = Tool::Rect;
        for (a, b) in [([0.0, 0.0], [0.04, 0.04]), ([0.02, 0.02], [0.06, 0.06]), ([0.1, 0.1], [0.12, 0.12])] {
            add_point(&mut st, a);
            add_point(&mut st, b);
        }
        select_part(&mut st, None);
        st
    }

    #[test]
    fn shapes_are_selected_with_ctrl_and_combined_into_what_is_left() {
        let mut st = two_shapes();
        toggle_part(&mut st, 0);
        toggle_part(&mut st, 1);
        assert_eq!(chosen_parts(&mut st), vec![0, 1]);
        combine_selected(&mut st, antenna_core::modeler::CombineOp::Add);
        assert_eq!(st.project.extra.len(), 2, "two became one, and the third is untouched");
        assert_eq!(chosen_parts(&mut st), vec![0], "what's left is selected");
        undo(&mut st);
        assert_eq!(st.project.extra.len(), 3, "one undo takes it back");
    }

    #[test]
    fn asking_to_combine_one_shape_says_so_and_changes_nothing() {
        let mut st = two_shapes();
        let steps = st.ui.draw.undo.len();
        toggle_part(&mut st, 0);
        combine_selected(&mut st, antenna_core::modeler::CombineOp::Add);
        assert_eq!(st.project.extra.len(), 3);
        assert!(st.ui.draw.note.is_some());
        assert_eq!(st.ui.draw.undo.len(), steps, "nothing to undo was added");
    }

    #[test]
    fn a_refused_combination_leaves_no_undo_step() {
        let mut st = two_shapes();
        let steps = st.ui.draw.undo.len();
        // Subtracting a shape from itself's twin leaves nothing, which is refused.
        st.project.extra[1] = st.project.extra[0].clone();
        st.project.extra[1].name = "twin".into();
        toggle_part(&mut st, 0);
        toggle_part(&mut st, 1);
        combine_selected(&mut st, antenna_core::modeler::CombineOp::Cut);
        assert_eq!(st.project.extra.len(), 3);
        assert_eq!(st.ui.draw.undo.len(), steps);
        assert!(st.ui.draw.note.as_ref().is_some_and(|n| n.contains("Nothing would be left")));
    }

    /// Nothing may be joined or deleted that nobody picked: the list can change under a selection.
    #[test]
    fn shapes_selected_together_are_let_go_when_the_list_changes() {
        let mut st = two_shapes();
        toggle_part(&mut st, 1);
        toggle_part(&mut st, 2);
        // A part removed on another page moves everything along one.
        st.project.extra.remove(0);
        assert_eq!(chosen_parts(&mut st).len(), 1);
        assert!(st.ui.draw.also.is_empty());

        // Another project, whose first parts happen to sit at the same places.
        let mut st = two_shapes();
        toggle_part(&mut st, 0);
        toggle_part(&mut st, 1);
        let mut other = Project::from_template("custom", Some("1GHz")).unwrap();
        for name in ["x", "y", "z"] {
            let mut part = st.project.extra[0].clone();
            part.name = name.into();
            other.extra.push(part);
        }
        st.project = other;
        combine_selected(&mut st, antenna_core::modeler::CombineOp::Add);
        assert_eq!(st.project.extra.len(), 3, "nothing happened to the new project's parts");
    }

    /// What the tools draw keeps its dimensions, and is selected ready to type them.
    #[test]
    fn a_drawn_shape_keeps_its_dimensions_and_is_selected() {
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        st.ui.draw.axis = 2;
        st.ui.draw.tool = Tool::Rect;
        add_point(&mut st, [0.0, 0.0]);
        add_point(&mut st, [0.04, 0.02]);
        assert_eq!(st.ui.draw.selected, Some(0));
        let ExtraPrim::Drawn { form: Form::Rect { c, size }, .. } = &st.project.extra[0].prim else { panic!("{:?}", st.project.extra[0].prim) };
        assert_eq!((c[0].as_str(), c[1].as_str(), size[0].as_str(), size[1].as_str()), ("20mm", "10mm", "40mm", "20mm"));

        st.ui.draw.tool = Tool::Circle;
        add_point(&mut st, [0.1, 0.0]);
        add_point(&mut st, [0.11, 0.0]);
        let ExtraPrim::Drawn { form: Form::Circle { r, .. }, .. } = &st.project.extra[1].prim else { panic!() };
        assert_eq!(r, "10mm");

        st.ui.draw.tool = Tool::Strip;
        st.ui.draw.strip_mm = 1.5;
        add_point(&mut st, [0.2, 0.0]);
        add_point(&mut st, [0.22, 0.0]);
        finish_path(&mut st);
        let ExtraPrim::Strip { w, path, .. } = &st.project.extra[2].prim else { panic!("{:?}", st.project.extra[2].prim) };
        assert_eq!((w.as_str(), path.len()), ("1.5mm", 2));

        // Points typed as expressions stay written that way, so the strip follows the variable, and a
        // plain number still means millimetres. A click between them stays a number.
        sketch::add_variable(&mut st.project, "a1", "5mm", "Test").unwrap();
        st.ui.draw.typed = ["a1".to_string(), "0".to_string()];
        add_typed_point(&mut st);
        st.ui.draw.typed = ["a1+2mm".to_string(), "3".to_string()];
        add_typed_point(&mut st);
        add_point(&mut st, [0.02, 0.003]);
        finish_path(&mut st);
        let ExtraPrim::Strip { path, .. } = st.project.extra.remove(3).prim else { panic!() };
        assert_eq!(path[0], ["a1".to_string(), "0mm".to_string()]);
        assert_eq!(path[1], ["a1+2mm".to_string(), "3mm".to_string()]);
        assert_eq!(path[2], ["20mm".to_string(), "3mm".to_string()]);
        assert!(st.ui.draw.typed_points.is_empty(), "nothing carries over to the next shape");
        // Something that doesn't work out says so, and adds nothing.
        st.ui.draw.typed = ["nope".to_string(), "0".to_string()];
        add_typed_point(&mut st);
        assert!(st.ui.draw.note.as_deref().is_some_and(|n| n.contains("nope")), "{:?}", st.ui.draw.note);
        assert!(st.ui.draw.points.is_empty());
        st.ui.draw.note = None;

        st.ui.draw.tool = Tool::Polygon;
        for q in [[0.3, 0.0], [0.34, 0.0], [0.3, 0.03]] {
            add_point(&mut st, q);
        }
        finish_path(&mut st);
        assert!(matches!(&st.project.extra[3].prim, ExtraPrim::Drawn { form: Form::Poly { .. }, .. }));
        assert_eq!(st.ui.draw.selected, Some(3));

        // A cut goes in the shape under it, as a hole, and doesn't select anything new.
        st.ui.draw.tool = Tool::Rect;
        st.ui.draw.cut = true;
        add_point(&mut st, [0.01, 0.005]);
        add_point(&mut st, [0.03, 0.015]);
        assert_eq!(st.ui.draw.note, None);
        let ExtraPrim::Drawn { holes, .. } = &st.project.extra[0].prim else { panic!() };
        assert_eq!(holes.len(), 1);
        assert_eq!(st.ui.draw.selected, Some(3));

        // Every one of them can be picked out of the canvas again.
        let (o, down) = ([0.02, 0.002, 1.0], [0.0, 0.0, -1.0]);
        assert_eq!(sketch::pick(&st.project, o, down).map(|x| x.0), Some(0));
        select_part(&mut st, Some(1));
        assert_eq!(st.ui.draw.at, "0");
        assert!(selected(&st).is_some_and(|s| s.0 == 1));
    }

    /// A sheet can be a dielectric, but a wire has to conduct.
    #[test]
    fn a_wire_is_metal_whatever_the_sheet_material_is() {
        let mut st = Studio::default();
        st.ui.draw.material = "FR-4".into();
        st.ui.draw.tool = Tool::Wire;
        add_point(&mut st, [0.0, 0.0]);
        add_point(&mut st, [0.01, 0.0]);
        finish_path(&mut st);
        assert_eq!(st.project.extra.last().unwrap().material, st.project.metal);
    }

    /// A part asked for on the Reconfigurable page lands on the next gap clicked, and the
    /// feed drawn before it stays a feed.
    #[test]
    fn a_part_lands_on_the_next_gap_clicked() {
        use antenna_core::reconfig::{Device, Setting};
        use antenna_core::sketch::{self, Sheet};
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        let sheet = Sheet { axis: 2, at: "0".into(), material: "PEC".into(), thick: "0".into() };
        for (a, b) in [([-0.07, -0.005], [-0.001, 0.005]), ([0.001, -0.005], [0.03, 0.005]), ([0.033, -0.005], [0.07, 0.005])] {
            sketch::add_shape(&mut st.project, "Arm", &sheet, sketch::rectangle(a, b)).unwrap();
        }
        st.ui.draw.axis = 2;
        st.ui.draw.at = "0".into();
        st.ui.draw.tool = Tool::Feed;
        add_point(&mut st, [-0.002, 0.0]);
        add_point(&mut st, [0.002, 0.0]);
        assert!(!st.project.reconfig.is_on(), "a plain feed is a feed");

        st.ui.draw.next_part = Device::by_part("BAR64-02V PIN");
        add_point(&mut st, [0.0295, 0.0]);
        add_point(&mut st, [0.0335, 0.0]);
        assert_eq!(st.ui.draw.note, None);
        assert!(st.ui.draw.next_part.is_none(), "one part, one gap");
        assert_eq!(st.ui.page, Page::Reconfig, "back to where it was asked for");

        let rc = &st.project.reconfig;
        assert_eq!(rc.elements.len(), 1);
        assert_eq!(rc.elements[0].name, "D1");
        assert_eq!(rc.states.len(), 2, "off and on to start from");
        assert_eq!(rc.states[1].settings, vec![Setting::On]);
        let g = st.project.geometry().unwrap();
        let names: Vec<String> = g.ports.iter().map(|p| p.name.clone()).collect();
        let bound = rc.bind(&names).unwrap();
        assert_eq!((bound.feeds.len(), bound.loads.len()), (1, 1));
        assert_eq!(names[bound.loads[0]], rc.elements[0].port);
    }
}
