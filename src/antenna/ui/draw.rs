//! Drawing an antenna on the canvas: pick a tool, click points on a plane, drag
//! corners, cut slots, click a feed. The shapes are ordinary parts of the project,
//! so what is drawn solves, saves and exports like a template.

use antenna_core::cad::P2;
use antenna_core::geometry::{P3, plane_point};
use antenna_core::material;
use antenna_core::project::{ExtraPrim, Project};
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
    pub selected: Option<usize>,
    drag: Option<Drag>,
    undo: Vec<Project>,
    redo: Vec<Project>,
    /// The canvas stays put while the model grows.
    pub frame: Option<(P3, f64)>,
    pub note: Option<String>,
    typed: [f64; 2],
    editing_corner: bool,
    /// The next feed drawn is this switch or varactor instead.
    pub next_part: Option<antenna_core::reconfig::Device>,
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
            selected: None,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            frame: None,
            note: None,
            typed: [0.0; 2],
            editing_corner: false,
            next_part: None,
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

pub(super) fn remember(st: &mut Studio) {
    let copy = st.project.clone();
    let d = &mut st.ui.draw;
    d.undo.push(copy);
    if d.undo.len() > 60 {
        d.undo.remove(0);
    }
    d.redo.clear();
}

fn undo(st: &mut Studio) {
    if let Some(p) = st.ui.draw.undo.pop() {
        let now = std::mem::replace(&mut st.project, p);
        st.ui.draw.redo.push(now);
        st.ui.draw.selected = None;
        st.ui.draw.points.clear();
    }
}

fn redo(st: &mut Studio) {
    if let Some(p) = st.ui.draw.redo.pop() {
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

/// Adds a finished shape, or cuts it from the metal under it.
fn finish_shape(st: &mut Studio, base: &str, pts: Vec<P2>) {
    remember(st);
    let sheet = sheet_of(st);
    let h = sketch::height_of(&st.project, &sheet.at);
    let done = if st.ui.draw.cut {
        h.and_then(|h| sketch::cut_in(&mut st.project, sheet.axis, h, &pts))
    } else {
        sketch::add_shape(&mut st.project, base, &sheet, pts)
    };
    match done {
        Ok(_) => st.ui.draw.note = None,
        Err(e) => {
            st.ui.draw.undo.pop();
            st.ui.draw.note = Some(format!("{e:#}"));
        }
    }
    st.ui.draw.points.clear();
}

fn finish_path(st: &mut Studio) {
    let pts = std::mem::take(&mut st.ui.draw.points);
    match st.ui.draw.tool {
        Tool::Polygon if pts.len() >= 3 => finish_shape(st, "Polygon", pts),
        Tool::Strip if pts.len() >= 2 => {
            let w = st.ui.draw.strip_mm * 1e-3;
            match sketch::strip(&pts, w) {
                Ok(ring) => finish_shape(st, "Strip", ring),
                Err(e) => st.ui.draw.note = Some(format!("{e:#}")),
            }
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
                    finish_shape(st, "Rectangle", sketch::rectangle(first, q));
                } else {
                    st.ui.draw.note = Some("A rectangle needs two corners that differ both ways.".into());
                }
            } else {
                let r = (q[0] - first[0]).hypot(q[1] - first[1]);
                finish_shape(st, "Circle", sketch::circle(first, r, 50e-6));
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

/// The selected part's plane and corners, if it's still there.
fn selected(st: &Studio) -> Option<(usize, usize, f64, Vec<P2>)> {
    let i = st.ui.draw.selected?;
    match &st.project.extra.get(i)?.prim {
        ExtraPrim::Outline { axis, at, pts, .. } => Some((i, *axis, sketch::height_of(&st.project, at).ok()?, pts.clone())),
        _ => None,
    }
}

fn select_part(st: &mut Studio, part: Option<usize>) {
    st.ui.draw.selected = part;
    if let Some(ExtraPrim::Outline { axis, at, .. }) = part.and_then(|i| st.project.extra.get(i)).map(|e| &e.prim) {
        // The plane follows, so edits and the grid are on the part you're looking at.
        st.ui.draw.axis = *axis;
        st.ui.draw.at = at.clone();
    }
}

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
            st.ui.draw.note = None;
        }
        if key(Modifiers::NONE, Key::Enter) {
            finish_path(st);
        }
        if key(Modifiers::NONE, Key::Backspace) || key(Modifiers::NONE, Key::Delete) {
            if !st.ui.draw.points.is_empty() {
                st.ui.draw.points.pop();
            } else if let Some(i) = st.ui.draw.selected.take() {
                remember(st);
                st.project.extra.remove(i);
            }
        }
    }

    let tool = st.ui.draw.tool;
    if !alt && let Some((q, _)) = cursor {
        if tool == Tool::Select {
            select_tool(st, resp, pj, q, pointer);
        } else {
            let closing = tool == Tool::Polygon && st.ui.draw.points.len() >= 3 && pointer.is_some_and(|p| to_screen(st.ui.draw.points[0]).distance(p) < SNAP_PX);
            if resp.double_clicked() && matches!(tool, Tool::Polygon | Tool::Strip) {
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
    for (i, a, at, pts) in sketch::outlines(&st.project) {
        let Ok(part_h) = sketch::height_of(&st.project, at) else { continue };
        let sel = st.ui.draw.selected == Some(i);
        let stroke = if sel { Stroke::new(2.0, theme::ACCENT) } else { outline };
        for (_, p, q) in edges(pts) {
            painter.line_segment([pj.screen(plane_point(a, part_h, p)), pj.screen(plane_point(a, part_h, q))], stroke);
        }
    }
    if let Some((_, a, part_h, pts)) = selected(st).filter(|_| tool == Tool::Select) {
        for p in &pts {
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
    if let (Some((q, on_corner)), false) = (cursor, alt) {
        match tool {
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
        Tool::Select => "Click a shape. Drag it or its corners; double-click an edge for a new corner; right-click a corner to remove it.".to_string(),
        Tool::Rect if begun => format!("Click the opposite corner of {what}. Esc cancels."),
        Tool::Rect => "Click one corner, or drag from corner to corner.".to_string(),
        Tool::Circle if begun => "Click the edge. Esc cancels.".to_string(),
        Tool::Circle => "Click the centre, or drag from centre to edge.".to_string(),
        Tool::Polygon if begun => "Keep clicking corners. Click the first or the last again, or press Enter, to close. Backspace takes one back.".to_string(),
        Tool::Polygon => "Click the first corner.".to_string(),
        Tool::Strip if begun => "Keep clicking along the path. Click the last point again or press Enter to finish.".to_string(),
        Tool::Strip => "Click where the strip starts.".to_string(),
        Tool::Feed if begun => "Click the other side of the gap.".to_string(),
        Tool::Feed => "Click one side of the gap, then the other. The feed lands on the metal, along an axis.".to_string(),
        Tool::Probe => "Click a point over two sheets: the probe joins them.".to_string(),
    }
}

fn select_tool(st: &mut Studio, resp: &Response, pj: &Projector, q: P2, pointer: Option<Pos2>) {
    let primary = egui::PointerButton::Primary;
    let sel = selected(st);
    let corner_near = |at: Pos2, sel: &Option<(usize, usize, f64, Vec<P2>)>| -> Option<P2> {
        let (_, a, part_h, pts) = sel.as_ref()?;
        pts.iter().copied().min_by(|x, y| pj.screen(plane_point(*a, *part_h, *x)).distance(at).total_cmp(&pj.screen(plane_point(*a, *part_h, *y)).distance(at))).filter(|p| pj.screen(plane_point(*a, *part_h, *p)).distance(at) < SNAP_PX)
    };
    if resp.drag_started_by(primary) {
        let origin = resp.ctx.input(|i| i.pointer.press_origin());
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
                sketch::move_corner(&mut st.project, i, from, q);
                st.ui.draw.drag = Some(Drag::Corner { from: q });
            }
            Drag::Body { last } => {
                let by = [q[0] - last[0], q[1] - last[1]];
                if by != [0.0, 0.0] {
                    sketch::move_part(&mut st.project, i, by);
                    st.ui.draw.drag = Some(Drag::Body { last: q });
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
        select_part(st, picked);
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

fn properties(ui: &mut egui::Ui, st: &mut Studio) {
    let Some(i) = st.ui.draw.selected.filter(|i| *i < st.project.extra.len()) else { return };
    let names: Vec<String> = material::library().into_iter().map(|m| m.name).collect();
    let table = super::expr_field::VarTable::of(&st.project);
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
            if let ExtraPrim::Outline { at, thick, .. } = &mut e.prim {
                ui.label("Height");
                super::expr_field::expr_line(ui, "part height", at, &table, 100.0);
                ui.end_row();
                ui.label("Thickness");
                super::expr_field::expr_line(ui, "part thickness", thick, &table, 100.0);
                ui.end_row();
            }
        });
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
    theme::caption(ui, "Draw");
    if let Some(d) = &st.ui.draw.next_part {
        let part = d.part.clone();
        super::verdict(ui, false, &format!("Click one side of a gap, then the other: {part} lands there. To cancel, choose another tool."));
        if st.ui.draw.tool != Tool::Feed {
            st.ui.draw.next_part = None;
        }
    }
    weak(ui, "Draw your own antenna on the canvas: metal first, then a feed. Right-drag pans, the wheel zooms, Alt-drag orbits.");
    if st.project.template != "custom" {
        group(ui, |ui| {
            weak(ui, "This model's own parts come from its template, which ties them to the variable table. Make them editable to change their shapes. What you draw here is added either way.");
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
            theme::segmented(ui, &mut tool, &[(Tool::Select, "Select"), (Tool::Rect, "Rectangle"), (Tool::Polygon, "Polygon"), (Tool::Strip, "Strip"), (Tool::Circle, "Circle")]);
            theme::segmented(ui, &mut tool, &[(Tool::Feed, "Feed"), (Tool::Probe, "Probe")]);
        });
        if tool != st.ui.draw.tool {
            st.ui.draw.tool = tool;
            st.ui.draw.points.clear();
            st.ui.draw.note = None;
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
                    face_on(&mut st.ui.cam, axis);
                }
                if ui.small_button("Face on").clicked() {
                    face_on(&mut st.ui.cam, st.ui.draw.axis);
                }
            });
            ui.end_row();
            ui.label("Height");
            super::expr_field::expr_line(ui, "draw height", &mut st.ui.draw.at, &table, 100.0).on_hover_text("Where the plane sits. An expression: 0, h, 1.6mm");
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
                super::expr_field::expr_line(ui, "draw thickness", &mut st.ui.draw.thick, &table, 100.0);
                ui.end_row();
            }
            ui.label("Grid");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut st.ui.draw.grid_mm).range(0.01..=100.0).speed(0.05).suffix(" mm").max_decimals(2));
                ui.checkbox(&mut st.ui.draw.snap, "Snap");
            });
            ui.end_row();
            if st.ui.draw.tool == Tool::Strip {
                ui.label("Strip width");
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
                ui.add(egui::DragValue::new(&mut st.ui.draw.typed[0]).speed(0.1).suffix(" mm").max_decimals(3));
                ui.add(egui::DragValue::new(&mut st.ui.draw.typed[1]).speed(0.1).suffix(" mm").max_decimals(3));
                if ui.small_button("Add").clicked() {
                    let q = [st.ui.draw.typed[0] * 1e-3, st.ui.draw.typed[1] * 1e-3];
                    add_point(st, q);
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

    theme::caption(ui, "Shapes");
    let shapes: Vec<(usize, String, String)> = st.project.extra.iter().enumerate().map(|(i, e)| (i, e.name.clone(), e.material.clone())).collect();
    if shapes.is_empty() {
        weak(ui, "Nothing drawn yet.");
    }
    let mut pick_part = None;
    let mut drop_part = None;
    egui::Grid::new("draw_shapes").num_columns(3).spacing(vec2(10.0, 2.0)).striped(true).show(ui, |ui| {
        for (i, name, mat) in &shapes {
            if ui.selectable_label(st.ui.draw.selected == Some(*i), name).clicked() {
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
        select_part(st, Some(i));
        st.ui.draw.tool = Tool::Select;
    }
    if let Some(i) = drop_part {
        remember(st);
        st.project.extra.remove(i);
        st.ui.draw.selected = None;
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
    for (i, name) in ports.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).size(12.5));
            if theme::icon_button(ui, icon::TRASH, "Remove this feed").clicked() {
                drop_port = Some(i);
            }
        });
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
