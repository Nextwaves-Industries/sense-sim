//! Modelling in 3D, the way a mechanical CAD program does it: a tree of features, sketches with
//! constraints and dimensions, faces and edges picked off the model. All of it writes
//! [`antenna_core::modeler`], so what's drawn solves, sweeps and saves like anything else.

use antenna_core::expr::Vars;
use antenna_core::geometry::{P3, add, dot, norm, scale, sub, unit};
use antenna_core::material;
use antenna_core::modeler::mesh::{FaceId, Frame};
use antenna_core::modeler::sketch::{self as sk, Constraint, Curve, CurveKind, ORIGIN, P2, Sketch, Solved, mm_text};
use antenna_core::modeler::{Anchor, AxisRef, BodyKind, CombineOp, Extent, Feature, Flat, Kind, Layout, Mode, Output, PathRef, PlaneRef, Prim, prims};
use bevy_egui::egui::{self, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind, pos2, vec2};

use super::expr_field::{VarTable, angle_line, expr_line, length_line};
use super::raster::{self, Look};
use super::view3d::{Camera, Projector, ViewMode};
use super::{Page, group, verdict, weak};
use crate::antenna::Studio;
use crate::theme::{self, icon};

/// Who the left mouse button belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pointer {
    /// It orbits the view and selects things. Nothing can be drawn by accident.
    #[default]
    View,
    /// It belongs to the drawing tool.
    Draw,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SkTool {
    #[default]
    Select,
    Line,
    Rect,
    CentreRect,
    Circle,
    Arc,
    Spline,
    Point,
    Dimension,
    Round,
}

/// What a click on the model is for, when it isn't plain selecting.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Picking {
    #[default]
    Nothing,
    /// A plane or flat face to start a sketch on.
    SketchPlane,
    /// Edges for this fillet or chamfer.
    Edges(u32),
    /// Faces to leave open in this shell.
    OpenFaces(u32),
    /// The two ends of a feed.
    Feed(Vec<(P3, Option<(u32, u32)>)>),
    /// A sketch point for a probe.
    Probe,
    Measure(Vec<P3>),
    /// Three points: one end, the corner, the other end.
    Angle(Vec<P3>),
    /// A shape being placed with the mouse.
    Place(super::place::Placing),
}

/// One more thing picked along with the first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picked {
    pub feature: u32,
    pub body: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub feature: Option<u32>,
    pub body: Option<u32>,
    pub face: Option<FaceId>,
    pub points: Vec<u32>,
    pub curves: Vec<u32>,
    /// What else is selected, by Ctrl or Shift, in the order it was picked.
    pub more: Vec<Picked>,
    /// One step of an object is what's picked, in the tree, and not the whole of it.
    pub step: bool,
}

impl Selection {
    /// The bodies selected, first picked first: that's the one a subtraction leaves.
    pub fn bodies(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.body.into_iter().chain(self.more.iter().filter_map(|m| m.body)).collect();
        let mut seen = std::collections::HashSet::new();
        out.retain(|b| seen.insert(*b));
        out
    }

    /// The steps in the tree selected.
    pub fn features(&self) -> Vec<u32> {
        self.feature.into_iter().chain(self.more.iter().map(|m| m.feature)).collect()
    }

    pub fn has_feature(&self, id: u32) -> bool {
        self.feature == Some(id) || self.more.iter().any(|m| m.feature == id)
    }

    /// In if it's out, out if it's in. Leaving the first one out makes the next the first.
    pub fn toggle(&mut self, p: Picked) {
        self.face = None;
        // By body when it is one: a pattern's copies are different bodies from one step.
        let same = |feature: Option<u32>, body: Option<u32>| if p.body.is_some() { body == p.body } else { body.is_none() && feature == Some(p.feature) };
        if same(self.feature, self.body) {
            let next = (!self.more.is_empty()).then(|| self.more.remove(0));
            self.feature = next.map(|n| n.feature);
            self.body = next.and_then(|n| n.body);
        } else if let Some(at) = self.more.iter().position(|m| same(Some(m.feature), m.body)) {
            self.more.remove(at);
        } else if self.feature.is_none() {
            self.feature = Some(p.feature);
            self.body = p.body;
        } else {
            self.more.push(p);
        }
    }
}

/// A dimension being typed, right on the canvas.
#[derive(Clone, Debug)]
pub struct DimEdit {
    /// Left with a name no variable has: it takes effect once the dialog has dealt with that.
    pub waiting: bool,
    pub index: usize,
    pub text: String,
    pub at: Pos2,
    pub fresh: bool,
}

/// What the pointer's over in a sketch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkPick {
    Point(u32),
    Mid(u32, P2),
    On(u32, P2),
    Free(P2),
}

impl SkPick {
    fn at(&self, s: &Solved) -> P2 {
        match *self {
            SkPick::Point(p) => s.at(p),
            SkPick::Mid(_, q) | SkPick::On(_, q) | SkPick::Free(q) => q,
        }
    }
}

#[derive(Clone, Debug)]
enum Drag {
    Point(u32),
    /// A whole curve, by the points it has, from where it started.
    Curve(Vec<(u32, P2)>, P2),
}

pub struct ModelUi {
    /// The older flat tools, for what's in the project's part list.
    pub flat: bool,
    /// Counts the times the work itself called for the modeler's tools, which the ribbon then shows.
    tab_asked: u32,
    /// The world axis the work plane is square to: shapes and new sketches go on it.
    pub work_axis: usize,
    /// The coordinate system new shapes are drawn in. `None` is the global one.
    pub coord: Option<u32>,
    /// What Copy took, for Paste.
    pub clip: Vec<Feature>,
    /// Bodies hidden from view. They're still in the model and still solved.
    pub hidden: std::collections::BTreeSet<u32>,
    /// The same for parts from a template or the flat tools, by name.
    pub hidden_parts: std::collections::BTreeSet<String>,
    pub active: Option<u32>,
    pub tool: SkTool,
    /// Points placed so far with the tool in hand.
    picks: Vec<SkPick>,
    pub sel: Selection,
    /// The tree when several things were selected: see `forget_stale_selection`.
    picked_in: Vec<(u32, String)>,
    hover: Option<SkPick>,
    hover_curve: Option<u32>,
    pub hover_part: Option<usize>,
    pub hover_face: Option<FaceId>,
    pub hover_edge: Option<[FaceId; 2]>,
    drag: Option<Drag>,
    pub picking: Picking,
    pub dim_edit: Option<DimEdit>,
    pub note: Option<String>,
    pub grid_mm: f64,
    pub snap: bool,
    pub auto_dims: bool,
    pub section_on: bool,
    pub section_axis: usize,
    pub section_at: f32,
    pub cam_goal: Option<(f32, f32)>,
    pub frame: Option<(P3, f64)>,
    editing: bool,
    pub round_radius: String,
    seen_revision: u64,
}

impl Default for ModelUi {
    fn default() -> Self {
        Self {
            flat: false,
            tab_asked: 0,
            work_axis: 2,
            coord: None,
            clip: Vec::new(),
            hidden: Default::default(),
            hidden_parts: Default::default(),
            active: None,
            tool: SkTool::Select,
            picks: Vec::new(),
            sel: Selection::default(),
            picked_in: Vec::new(),
            hover: None,
            hover_curve: None,
            hover_part: None,
            hover_face: None,
            hover_edge: None,
            drag: None,
            picking: Picking::Nothing,
            dim_edit: None,
            note: None,
            grid_mm: 0.0,
            snap: true,
            auto_dims: true,
            section_on: false,
            section_axis: 0,
            section_at: 0.0,
            cam_goal: None,
            frame: None,
            editing: false,
            round_radius: "2mm".into(),
            seen_revision: 0,
        }
    }
}

impl ModelUi {
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Lets go of a sketch or a pick on a step that's no longer in the model.
    pub(super) fn forget_gone_steps(&mut self, project: &antenna_core::project::Project) {
        if self.active.is_some_and(|id| project.model.sketch(id).is_none()) {
            self.active = None;
            self.picks.clear();
            self.drag = None;
            self.dim_edit = None;
        }
        if matches!(self.picking, Picking::Edges(id) | Picking::OpenFaces(id) if project.model.feature(id).is_none()) {
            self.picking = Picking::Nothing;
        }
    }

    pub fn toggle_hidden(&mut self, body: u32) {
        if !self.hidden.remove(&body) {
            self.hidden.insert(body);
        }
    }

    /// After an undo or redo: what was being drawn, and what was selected, may no longer be there.
    pub fn forget_what_was_in_hand(&mut self) {
        self.picks.clear();
        self.drag = None;
        self.dim_edit = None;
        self.sel.points.clear();
        self.sel.curves.clear();
        self.sel.face = None;
        self.sel.more.clear();
    }
}

/// Within this many points of a point or line, the pointer is on it.
const SNAP_PX: f32 = 9.0;

fn nice_grid(lambda_mm: f64) -> f64 {
    let raw = lambda_mm / 100.0;
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    (if n < 1.5 { 1.0 } else if n < 3.5 { 2.0 } else if n < 7.5 { 5.0 } else { 10.0 }) * mag
}

fn ensure_grid(st: &mut Studio) {
    if st.ui.m3.grid_mm <= 0.0 {
        st.ui.m3.grid_mm = nice_grid(antenna_core::C0 / st.project.f0().max(1.0) * 1e3);
    }
}

/// On arriving at the page: the model at an angle, framed, with a grid to suit it.
pub fn entered(st: &mut Studio) {
    // Only placing a new part takes the flat tools. Flat parts are in the tree with the rest.
    if st.ui.draw.next_part.is_some() {
        st.ui.m3.flat = true;
    }
    if st.ui.m3.flat {
        super::draw::entered(st);
        st.ui.pointer = Pointer::Draw;
        return;
    }
    // Coming back to the model from a page leaves the view where it was put. A new antenna is framed.
    if st.ui.m3.frame.is_none() {
        show_model(st);
    } else {
        st.ui.view = ViewMode::Model;
        ensure_grid(st);
        st.ui.pointer = Pointer::View;
    }
}

/// The 3D model on the canvas, at an angle and framed.
fn show_model(st: &mut Studio) {
    st.ui.view = ViewMode::Model;
    ensure_grid(st);
    st.ui.pointer = Pointer::View;
    let goal = Camera::default();
    st.ui.m3.cam_goal = Some((goal.yaw, goal.pitch));
    fit(st);
}

/// The user's choice between the 3D model and the flat tools. It holds whatever the project
/// has in it: `entered` only picks where a project opens.
pub(super) fn choose_flat(st: &mut Studio, flat: bool) {
    st.ui.m3.flat = flat;
    st.ui.pointer = Pointer::View;
    if flat {
        if let Some(_id) = st.ui.m3.active.take() {
            st.ui.m3.picks.clear();
        }
        super::draw::entered(st);
        st.ui.pointer = Pointer::Draw;
    } else {
        show_model(st);
    }
}

/// Everything in view, or a wavelength's worth of empty space.
pub fn fit(st: &mut Studio) {
    let lambda = antenna_core::C0 / st.project.f0().max(1.0);
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut any = false;
    for p in st.ui.parts.iter().filter(|p| !p.mesh.is_empty()) {
        let (a, b) = p.mesh.bounds();
        for k in 0..3 {
            lo[k] = lo[k].min(a[k]);
            hi[k] = hi[k].max(b[k]);
        }
        any = true;
    }
    st.ui.m3.frame = Some(if any {
        (std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0), (norm(sub(hi, lo)) / 2.0 * 1.15).max(lambda / 40.0))
    } else {
        ([0.0; 3], lambda / 4.0)
    });
    st.ui.cam.zoom = 1.0;
    st.ui.cam.pan = egui::Vec2::ZERO;
}

/// Frames again if the model has grown out of the view, once per change: an extrusion shouldn't
/// have to be gone looking for.
fn keep_in_view(st: &mut Studio) {
    if st.ui.m3.seen_revision == st.ui.revision {
        return;
    }
    st.ui.m3.seen_revision = st.ui.revision;
    let Some((c, e)) = st.ui.m3.frame else { return };
    let outside = st.ui.parts.iter().filter(|p| !p.mesh.is_empty()).any(|p| {
        let (lo, hi) = p.mesh.bounds();
        norm(sub(lo, c)) > e * 1.2 || norm(sub(hi, c)) > e * 1.2
    });
    if outside && st.ui.m3.drag.is_none() {
        fit(st);
    }
}

/// Whether the left button is the tool's this frame.
pub fn drawing(st: &Studio) -> bool {
    st.ui.page == Page::Draw && matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh) && st.ui.pointer == Pointer::Draw && (st.ui.m3.flat || st.ui.m3.active.is_some() || matches!(st.ui.m3.picking, Picking::Feed(_) | Picking::Probe | Picking::Measure(_) | Picking::Angle(_) | Picking::Place(_)))
}

/// How the canvas should look for what's selected and hovered.
pub fn look(st: &Studio) -> Look {
    let m = &st.ui.m3;
    let on_page = st.ui.page == Page::Draw && !m.flat;
    let mut l = Look { see_through: st.ui.transparent, revision: st.ui.revision, ..Default::default() };
    // What's picked in the tree shows whichever page is up, so a row can be found on the model.
    if !m.flat {
        let chosen = m.sel.bodies();
        let part = if let super::project_tree::Pick::Part(name) = &st.ui.tree.pick { Some(name.as_str()) } else { None };
        if !chosen.is_empty() || part.is_some() {
            l.selected_parts = st.ui.parts.iter().enumerate().filter(|(_, p)| p.body.is_some_and(|b| chosen.contains(&b)) || (part.is_some() && p.solid.as_deref() == part)).map(|(i, _)| i).collect();
        }
    }
    if on_page {
        if let Some(f) = m.sel.face {
            l.selected_faces.push(f);
        }
        if let Picking::OpenFaces(id) = &m.picking
            && let Some(Kind::Shell { open, .. }) = st.project.model.feature(*id).map(|f| &f.kind)
        {
            l.selected_faces.extend(open);
        }
        l.hover_part = m.hover_part;
        l.hover_face = m.hover_face;
        if m.section_on {
            let mut n = [0.0; 3];
            n[m.section_axis] = 1.0;
            l.section = Some((n, m.section_at));
        }
    }
    l
}

/// The sketch plane's grid, while a sketch is open, or the work plane's while a shape is placed.
pub fn canvas_grid(st: &Studio) -> Option<(Frame, f64)> {
    if matches!(st.ui.m3.picking, Picking::Place(_)) {
        return Some((super::place::work_frame(st).0, st.ui.m3.grid_mm * 1e-3));
    }
    let id = st.ui.m3.active?;
    let frame = st.ui.built.sketches.get(&id)?.frame;
    Some((frame, st.ui.m3.grid_mm * 1e-3))
}

fn pressed_view(st: &mut Studio, n: P3) {
    let pitch = (n[2] as f32).clamp(-1.0, 1.0).asin();
    let yaw = if n[0].abs() + n[1].abs() < 1e-6 { 0.0 } else { (n[0] as f32).atan2(-(n[1] as f32)) };
    st.ui.m3.cam_goal = Some((yaw, pitch));
}

/// Turns the camera a little towards where it's going.
pub fn animate(st: &mut Studio, ctx: &egui::Context) {
    let Some((yaw, pitch)) = st.ui.m3.cam_goal else { return };
    let cam = &mut st.ui.cam;
    let pi = std::f32::consts::PI;
    let wrapped = (yaw - cam.yaw + pi).rem_euclid(2.0 * pi) - pi;
    let (dy, dp) = (wrapped, pitch - cam.pitch);
    if dy.abs() < 0.002 && dp.abs() < 0.002 {
        cam.yaw = yaw;
        cam.pitch = pitch;
        st.ui.m3.cam_goal = None;
        return;
    }
    cam.yaw += dy * 0.28;
    cam.pitch += dp * 0.28;
    ctx.request_repaint();
}

pub(super) fn begin_edit(st: &mut Studio) {
    if !st.ui.m3.editing {
        super::draw::remember(st);
        st.ui.m3.editing = true;
    }
}

/// Opens a sketch to draw in, facing it.
pub fn open_sketch(st: &mut Studio, id: u32) {
    st.ui.m3.active = Some(id);
    st.ui.m3.tool = SkTool::Line;
    ask_for_model_tab(st);
    st.ui.m3.picks.clear();
    st.ui.m3.sel = Selection { feature: Some(id), ..Default::default() };
    st.ui.m3.picking = Picking::Nothing;
    st.ui.pointer = Pointer::Draw;
    if let Some(s) = st.ui.built.sketches.get(&id) {
        let n = s.frame.n;
        pressed_view(st, n);
    }
    // The plane it's on may not have been built yet.
    if let Some(Feature { kind: Kind::Sketch { plane: PlaneRef::Base { axis, .. }, .. }, .. }) = st.project.model.feature(id) {
        let n = match axis {
            2 => [0.0, 0.0, 1.0],
            1 => [0.0, -1.0, 0.0],
            _ => [1.0, 0.0, 0.0],
        };
        pressed_view(st, n);
    }
}

/// A new, empty sketch on a plane, opened.
pub fn new_sketch(st: &mut Studio, plane: PlaneRef) -> u32 {
    super::draw::remember(st);
    let id = st.project.model.add(Kind::Sketch { plane, sketch: Sketch::default() });
    // In a coordinate system the camera looks along its axis, which only the build could say.
    if matches!(plane_of(st, id), PlaneRef::InCoord { .. }) {
        let n = super::place::work_frame(st).0.n;
        open_sketch(st, id);
        pressed_view(st, n);
        return id;
    }
    if let PlaneRef::Face { face, .. } = plane_of(st, id) {
        // On a face, the camera looks at it now rather than after the next build.
        let vars = st.project.vars().unwrap_or_default();
        let meshes: Vec<&antenna_core::modeler::mesh::Mesh> = st.ui.built.bodies.iter().map(|b| b.mesh.as_ref()).collect();
        if let Ok(f) = (PlaneRef::Face { face, offset: "0".into() }).frame(&vars, &meshes) {
            let n = f.n;
            pressed_view(st, n);
        }
    }
    open_sketch(st, id);
    id
}

fn plane_of(st: &Studio, id: u32) -> PlaneRef {
    match st.project.model.feature(id).map(|f| &f.kind) {
        Some(Kind::Sketch { plane, .. }) => plane.clone(),
        _ => PlaneRef::default(),
    }
}

pub fn exit_sketch(st: &mut Studio) {
    let id = st.ui.m3.active.take();
    st.ui.m3.picks.clear();
    st.ui.m3.drag = None;
    st.ui.m3.tool = SkTool::Select;
    st.ui.m3.sel = Selection { feature: id, ..Default::default() };
    ask_for_model_tab(st);
    st.ui.m3.dim_edit = None;
    st.ui.pointer = Pointer::View;
    let goal = Camera::default();
    st.ui.m3.cam_goal = Some((goal.yaw, goal.pitch));
}

fn with_sketch(st: &mut Studio, f: impl FnOnce(&mut Sketch, &Vars)) {
    let vars = st.project.vars().unwrap_or_default();
    let Some(id) = st.ui.m3.active else { return };
    if let Some(s) = st.project.model.sketch_mut(id) {
        f(s, &vars);
        let solved = s.solve(&vars);
        s.keep(&solved);
    }
}

/// The point a pick stands for, made if it has to be, tied to whatever it landed on.
fn point_for(s: &mut Sketch, pick: SkPick) -> u32 {
    match pick {
        SkPick::Point(p) => p,
        SkPick::Free(q) => s.add_point(q),
        SkPick::Mid(line, q) => {
            let p = s.add_point(q);
            s.constraints.push(Constraint::Midpoint { p, line });
            p
        }
        SkPick::On(curve, q) => {
            let p = s.add_point(q);
            match s.curve(curve).map(|c| &c.kind) {
                Some(CurveKind::Line { .. }) => s.constraints.push(Constraint::OnLine { p, line: curve }),
                Some(CurveKind::Circle { .. } | CurveKind::Arc { .. }) => s.constraints.push(Constraint::OnCircle { p, curve }),
                _ => {}
            }
            p
        }
    }
}

/// A click with the tool in hand.
pub fn tool_click(st: &mut Studio, pick: SkPick) {
    let Some(id) = st.ui.m3.active else { return };
    let vars = st.project.vars().unwrap_or_default();
    let Some(s) = st.project.model.sketch(id) else { return };
    let solved = s.solve(&vars);
    let tool = st.ui.m3.tool;
    let auto = st.ui.m3.auto_dims;
    let first = st.ui.m3.picks.first().copied();
    let at = pick.at(&solved);
    match tool {
        SkTool::Select | SkTool::Dimension | SkTool::Round => {}
        SkTool::Point => {
            super::draw::remember(st);
            with_sketch(st, |s, _| {
                point_for(s, pick);
            });
        }
        SkTool::Line => {
            let Some(last) = st.ui.m3.picks.last().copied() else {
                st.ui.m3.picks.push(pick);
                return;
            };
            let from = last.at(&solved);
            if (from[0] - at[0]).abs() + (from[1] - at[1]).abs() < 1e-9 {
                return;
            }
            super::draw::remember(st);
            let (d_u, d_v) = (at[0] - from[0], at[1] - from[1]);
            let level = matches!(pick, SkPick::Free(_)) && d_v.abs() <= d_u.abs() * 1e-9;
            let upright = matches!(pick, SkPick::Free(_)) && d_u.abs() <= d_v.abs() * 1e-9;
            let (mut start, mut end) = (0, 0);
            with_sketch(st, |s, _| {
                let a = point_for(s, last);
                let b = point_for(s, pick);
                let line = s.add_line(a, b);
                start = a;
                end = b;
                if level {
                    s.constraints.push(Constraint::Horizontal { line });
                } else if upright {
                    s.constraints.push(Constraint::Vertical { line });
                }
                if auto {
                    s.constraints.push(Constraint::Distance { a, b, value: mm_text((d_u * d_u + d_v * d_v).sqrt()) });
                }
            });
            // The first point of a chain is what it can close on, and closing on it ends the chain.
            let chain_start = match first {
                Some(SkPick::Point(p)) => p,
                _ => start,
            };
            if end == chain_start {
                st.ui.m3.picks.clear();
            } else {
                st.ui.m3.picks = vec![SkPick::Point(chain_start), SkPick::Point(end)];
            }
        }
        SkTool::Rect | SkTool::CentreRect => {
            let Some(first) = first else {
                st.ui.m3.picks.push(pick);
                return;
            };
            st.ui.m3.picks.clear();
            let from = first.at(&solved);
            if (from[0] - at[0]).abs() < 1e-9 || (from[1] - at[1]).abs() < 1e-9 {
                st.ui.m3.note = Some("A rectangle needs two corners that differ both ways.".into());
                return;
            }
            super::draw::remember(st);
            with_sketch(st, |s, _| add_rect_tool(s, tool == SkTool::CentreRect, first, pick, from, at, auto));
        }
        SkTool::Circle => {
            let Some(first) = first else {
                st.ui.m3.picks.push(pick);
                return;
            };
            st.ui.m3.picks.clear();
            let c = first.at(&solved);
            let r = (at[0] - c[0]).hypot(at[1] - c[1]);
            if r < 1e-9 {
                return;
            }
            super::draw::remember(st);
            with_sketch(st, |s, _| {
                let cp = point_for(s, first);
                let curve = s.add_curve(CurveKind::Circle { c: cp, r });
                if auto {
                    s.constraints.push(Constraint::Radius { curve, value: mm_text(r) });
                }
            });
        }
        SkTool::Arc => {
            if st.ui.m3.picks.len() < 2 {
                st.ui.m3.picks.push(pick);
                return;
            }
            let (c, a) = (st.ui.m3.picks[0], st.ui.m3.picks[1]);
            st.ui.m3.picks.clear();
            let (pc, pa) = (c.at(&solved), a.at(&solved));
            let r = (pa[0] - pc[0]).hypot(pa[1] - pc[1]);
            if r < 1e-9 {
                return;
            }
            // The end lands on the circle the start set.
            let t = (at[1] - pc[1]).atan2(at[0] - pc[0]);
            let end = [pc[0] + r * t.cos(), pc[1] + r * t.sin()];
            let end_pick = if matches!(pick, SkPick::Point(_)) { pick } else { SkPick::Free(end) };
            super::draw::remember(st);
            with_sketch(st, |s, _| {
                let (cp, ap, bp) = (point_for(s, c), point_for(s, a), point_for(s, end_pick));
                let curve = s.add_curve(CurveKind::Arc { c: cp, a: ap, b: bp });
                if auto {
                    s.constraints.push(Constraint::Radius { curve, value: mm_text(r) });
                }
            });
        }
        SkTool::Spline => st.ui.m3.picks.push(pick),
    }
    st.ui.m3.note = None;
}

fn add_rect_tool(s: &mut Sketch, centred: bool, first: SkPick, second: SkPick, from: P2, to: P2, auto: bool) {
    let (lo, hi) = if centred {
        let d = [(to[0] - from[0]).abs(), (to[1] - from[1]).abs()];
        ([from[0] - d[0], from[1] - d[1]], [from[0] + d[0], from[1] + d[1]])
    } else {
        ([from[0].min(to[0]), from[1].min(to[1])], [from[0].max(to[0]), from[1].max(to[1])])
    };
    let (p, _) = s.add_rect(lo, hi);
    if centred {
        // A construction diagonal with the centre on its middle, so the size can change around it.
        let centre = point_for(s, first);
        let diag = s.add_line(p[0], p[2]);
        if let Some(c) = s.curves.iter_mut().find(|c| c.id == diag) {
            c.construction = true;
        }
        s.constraints.push(Constraint::Midpoint { p: centre, line: diag });
    } else {
        // A corner placed on something stays on it.
        for (pick, q) in [(first, from), (second, to)] {
            let corner = p.iter().copied().find(|c| s.point(*c).is_some_and(|x| (x[0] - q[0]).abs() + (x[1] - q[1]).abs() < 1e-9));
            if let (Some(corner), true) = (corner, !matches!(pick, SkPick::Free(_))) {
                let other = point_for(s, pick);
                s.constraints.push(Constraint::Coincident { a: corner, b: other });
            }
        }
    }
    if auto {
        s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: mm_text(hi[0] - lo[0]) });
        s.constraints.push(Constraint::DistanceV { a: p[1], b: p[2], value: mm_text(hi[1] - lo[1]) });
    }
}

/// Ends what's being drawn: a chain of lines, or a spline through its points.
pub fn finish_tool(st: &mut Studio) {
    if st.ui.m3.tool == SkTool::Spline && st.ui.m3.picks.len() >= 2 {
        let picks = std::mem::take(&mut st.ui.m3.picks);
        super::draw::remember(st);
        with_sketch(st, |s, _| {
            let pts: Vec<u32> = picks.iter().map(|p| point_for(s, *p)).collect();
            s.add_curve(CurveKind::Spline { pts });
        });
    }
    st.ui.m3.picks.clear();
}

fn plane_to_screen(f: &Frame, pj: &Projector, q: P2) -> Pos2 {
    pj.screen(f.at(q, 0.0))
}

/// Where a screen point lands on a sketch plane, unless the plane's edge-on.
pub(super) fn screen_to_plane(f: &Frame, pj: &Projector, at: Pos2) -> Option<P2> {
    let (o, d) = pj.ray(at);
    let den = dot(d, f.n);
    if den.abs() < 0.04 {
        return None;
    }
    let t = dot(sub(f.o, o), f.n) / den;
    Some(f.local(add(o, scale(d, t))).0)
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let d = b - a;
    let t = ((p - a).dot(d) / d.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + d * t).distance(p)
}

/// What's under the pointer in the sketch, the most specific thing first.
fn sketch_pick(s: &Sketch, solved: &Solved, f: &Frame, pj: &Projector, at: Pos2, q: P2, m: &ModelUi, vars: &Vars) -> (SkPick, Option<u32>) {
    let dist = |p: P2| plane_to_screen(f, pj, p).distance(at);
    let mut best: Option<(f32, u32)> = None;
    let mut ids: Vec<u32> = s.points.iter().map(|p| p.id).collect();
    ids.push(ORIGIN);
    for id in ids {
        let d = dist(solved.at(id));
        if d < SNAP_PX && best.is_none_or(|b| d < b.0) {
            best = Some((d, id));
        }
    }
    if let Some((_, id)) = best {
        return (SkPick::Point(id), None);
    }
    let mut on: Option<(f32, u32, P2)> = None;
    for c in &s.curves {
        let line = prims::curve_polyline(&c.kind, solved, vars, 48, c.id);
        for w in line.windows(2) {
            let (a, b) = (plane_to_screen(f, pj, w[0]), plane_to_screen(f, pj, w[1]));
            let d = seg_dist(at, a, b);
            if d < SNAP_PX && on.is_none_or(|o| d < o.0) {
                let u = [w[1][0] - w[0][0], w[1][1] - w[0][1]];
                let t = (((q[0] - w[0][0]) * u[0] + (q[1] - w[0][1]) * u[1]) / (u[0] * u[0] + u[1] * u[1]).max(1e-30)).clamp(0.0, 1.0);
                on = Some((d, c.id, [w[0][0] + u[0] * t, w[0][1] + u[1] * t]));
            }
        }
        // A line's middle is worth stopping at.
        if let CurveKind::Line { a, b } = c.kind {
            let (pa, pb) = (solved.at(a), solved.at(b));
            let mid = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
            if dist(mid) < SNAP_PX {
                return (SkPick::Mid(c.id, mid), Some(c.id));
            }
        }
    }
    if let Some((_, id, p)) = on {
        return (SkPick::On(id, p), Some(id));
    }
    if !m.snap {
        return (SkPick::Free(q), None);
    }
    let step = (m.grid_mm * 1e-3).max(1e-6);
    (SkPick::Free([(q[0] / step).round() * step, (q[1] / step).round() * step]), None)
}

/// While a line's being drawn, near level or upright snaps to it, the way CAD programs infer it.
fn infer(pick: SkPick, from: Option<P2>, f: &Frame, pj: &Projector) -> (SkPick, Option<[P2; 2]>) {
    let (SkPick::Free(q), Some(a)) = (pick, from) else { return (pick, None) };
    let (sa, sq) = (plane_to_screen(f, pj, a), plane_to_screen(f, pj, q));
    let (level, upright) = (plane_to_screen(f, pj, [a[0], q[1]]), plane_to_screen(f, pj, [q[0], a[1]]));
    let _ = sa;
    if sq.distance(upright) < SNAP_PX {
        let p = [q[0], a[1]];
        return (SkPick::Free(p), Some([a, p]));
    }
    if sq.distance(level) < SNAP_PX {
        let p = [a[0], q[1]];
        return (SkPick::Free(p), Some([a, p]));
    }
    (pick, None)
}

/// Where a dimension's text goes, and the lines that say what it measures, on the screen.
struct DimShape {
    label: Pos2,
    lines: Vec<[Pos2; 2]>,
}

fn dim_shape(c: &Constraint, s: &Sketch, solved: &Solved, f: &Frame, pj: &Projector) -> Option<DimShape> {
    let scr = |q: P2| plane_to_screen(f, pj, q);
    const OFF: f32 = 26.0;
    // The middle of what's drawn, so a dimension can stand clear of it.
    let (mut cx, mut cy, mut count) = (0.0, 0.0, 0.0);
    for p in &s.points {
        let at = scr(solved.at(p.id));
        cx += at.x;
        cy += at.y;
        count += 1.0;
    }
    let middle = pos2(cx / f32::max(count, 1.0), cy / f32::max(count, 1.0));
    let along = |a: Pos2, b: Pos2, off: f32| -> DimShape {
        let dir = (b - a).normalized();
        let halfway = pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let out = dir.rot90();
        let away = if (halfway + out - middle).length() >= (halfway - out - middle).length() { 1.0 } else { -1.0 };
        let n = out * off.abs() * away;
        let (x, y) = (a + n, b + n);
        let tick = dir.rot90() * 4.0;
        DimShape { label: pos2((x.x + y.x) / 2.0, (x.y + y.y) / 2.0), lines: vec![[a, x], [b, y], [x, y], [x - tick, x + tick], [y - tick, y + tick]] }
    };
    match c {
        Constraint::Distance { a, b, .. } => Some(along(scr(solved.at(*a)), scr(solved.at(*b)), OFF)),
        Constraint::DistanceU { a, b, .. } => {
            let (pa, pb) = (solved.at(*a), solved.at(*b));
            if (pa[0] - pb[0]).abs() < 1e-12 {
                return None;
            }
            Some(along(scr(pa), scr([pb[0], pa[1]]), OFF))
        }
        Constraint::DistanceV { a, b, .. } => {
            let (pa, pb) = (solved.at(*a), solved.at(*b));
            if (pa[1] - pb[1]).abs() < 1e-12 {
                return None;
            }
            Some(along(scr(pa), scr([pa[0], pb[1]]), OFF))
        }
        Constraint::PointLine { p, line, .. } => {
            let CurveKind::Line { a, b } = s.curve(*line)?.kind else { return None };
            let (pp, pa, pb) = (solved.at(*p), solved.at(a), solved.at(b));
            let d = [pb[0] - pa[0], pb[1] - pa[1]];
            let t = ((pp[0] - pa[0]) * d[0] + (pp[1] - pa[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1]).max(1e-30);
            let foot = [pa[0] + d[0] * t, pa[1] + d[1] * t];
            Some(along(scr(pp), scr(foot), 0.0))
        }
        Constraint::Radius { curve, .. } | Constraint::Diameter { curve, .. } => {
            let (cen, r) = match s.curve(*curve)?.kind {
                CurveKind::Circle { c, .. } => (solved.at(c), *solved.radii.get(curve)?),
                CurveKind::Arc { c, a, .. } => {
                    let (pc, pa) = (solved.at(c), solved.at(a));
                    (pc, (pa[0] - pc[0]).hypot(pa[1] - pc[1]))
                }
                _ => return None,
            };
            let rim = [cen[0] + r * 0.707, cen[1] + r * 0.707];
            let (a, b) = (scr(cen), scr(rim));
            let label = b + (b - a).normalized() * 16.0;
            Some(DimShape { label, lines: vec![[a, b]] })
        }
        Constraint::Angle { a, b, .. } => {
            let (CurveKind::Line { a: p, .. }, CurveKind::Line { b: q, .. }) = (s.curve(*a)?.kind.clone(), s.curve(*b)?.kind.clone()) else { return None };
            let (pa, pb) = (scr(solved.at(p)), scr(solved.at(q)));
            Some(DimShape { label: pos2((pa.x + pb.x) / 2.0, (pa.y + pb.y) / 2.0), lines: Vec::new() })
        }
        _ => None,
    }
}

/// A dimension's text: its value, and its name if it's tied to something.
fn dim_text(c: &Constraint, vars: &Vars) -> String {
    let Some(v) = c.value() else { return String::new() };
    let free = antenna_core::sketch::is_free(v);
    let number = if matches!(c, Constraint::Angle { .. }) {
        antenna_core::modeler::angle(v, vars).map(|a| format!("{:.2}°", a.to_degrees()))
    } else {
        antenna_core::modeler::length(v, vars).map(|l| {
            let mm = if matches!(c, Constraint::Diameter { .. }) { l * 1e3 } else { l.abs() * 1e3 };
            format!("{}{mm:.3}", match c {
                Constraint::Radius { .. } => "R ",
                Constraint::Diameter { .. } => "Ø ",
                _ => "",
            })
        })
    };
    let n = number.unwrap_or_else(|_| "?".into());
    let n = if n.contains('.') { n.trim_end_matches('0').trim_end_matches('.').to_string() } else { n };
    if free { n } else { format!("{v} = {n}") }
}

/// Everything in the open sketch, over the model. Blue can still move, black is held, red is at odds.
fn paint_sketch(painter: &egui::Painter, s: &Sketch, solved: &Solved, f: &Frame, pj: &Projector, m: &ModelUi, vars: &Vars) {
    let fixed = solved.free == 0;
    let free_col = Color32::from_rgb(40, 90, 210);
    let held = Color32::from_rgb(30, 32, 38);
    let conflict = solved.conflicts.iter().any(|c| *c != usize::MAX);
    for c in &s.curves {
        let line = prims::curve_polyline(&c.kind, solved, vars, 64, c.id);
        let selected = m.sel.curves.contains(&c.id);
        let hovered = m.hover_curve == Some(c.id);
        let loose = curve_is_loose(&c.kind, solved);
        let base = if conflict { theme::DANGER } else if fixed || !loose { held } else { free_col };
        let col = if selected { theme::ACCENT } else if hovered { Color32::from_rgb(240, 130, 20) } else { base };
        let width = if selected || hovered { 2.6 } else { 1.8 };
        for w in line.windows(2) {
            let (a, b) = (plane_to_screen(f, pj, w[0]), plane_to_screen(f, pj, w[1]));
            if c.construction {
                let n = ((a.distance(b) / 7.0) as usize).max(1);
                for k in (0..n).step_by(2) {
                    let (x, y) = (a + (b - a) * (k as f32 / n as f32), a + (b - a) * ((k + 1) as f32 / n as f32));
                    painter.line_segment([x, y], Stroke::new(1.2, col.gamma_multiply(0.75)));
                }
            } else {
                painter.line_segment([a, b], Stroke::new(width, col));
            }
        }
    }
    for p in s.points.iter().map(|p| p.id).chain(std::iter::once(ORIGIN)) {
        let at = plane_to_screen(f, pj, solved.at(p));
        let selected = m.sel.points.contains(&p);
        let hovered = matches!(m.hover, Some(SkPick::Point(q)) if q == p);
        if p == ORIGIN {
            painter.circle_stroke(at, 5.0, Stroke::new(1.4, theme::DANGER));
            painter.line_segment([at - vec2(8.0, 0.0), at + vec2(8.0, 0.0)], Stroke::new(1.2, theme::DANGER));
            painter.line_segment([at - vec2(0.0, 8.0), at + vec2(0.0, 8.0)], Stroke::new(1.2, theme::DANGER));
            continue;
        }
        let loose = solved.loose_points.contains(&p);
        let col = if selected { theme::ACCENT } else if hovered { Color32::from_rgb(240, 130, 20) } else if loose { free_col } else { held };
        let r = if selected || hovered { 4.5 } else { 3.0 };
        painter.circle(at, r, col, Stroke::new(1.0, Color32::WHITE));
    }
    // Small marks for the relations that hold.
    for c in &s.constraints {
        let (line, mark) = match c {
            Constraint::Horizontal { line } => (*line, "H"),
            Constraint::Vertical { line } => (*line, "V"),
            Constraint::Parallel { a, .. } => (*a, "//"),
            Constraint::Perpendicular { a, .. } => (*a, "⊥"),
            Constraint::EqualLength { a, .. } => (*a, "="),
            Constraint::Tangent { line, .. } => (*line, "T"),
            _ => continue,
        };
        let Some(Curve { kind: CurveKind::Line { a, b }, .. }) = s.curve(line) else { continue };
        let (pa, pb) = (solved.at(*a), solved.at(*b));
        let (x, y) = (plane_to_screen(f, pj, pa), plane_to_screen(f, pj, pb));
        if x.distance(y) < 46.0 {
            continue;
        }
        let at = x + (y - x) * 0.32 + (y - x).normalized().rot90() * 9.0;
        let r = Rect::from_center_size(at, vec2(13.0, 13.0));
        painter.rect(r, 2.0, Color32::from_rgb(232, 240, 228), Stroke::new(0.8, Color32::from_rgb(70, 130, 80)), StrokeKind::Inside);
        painter.text(at, Align2::CENTER_CENTER, mark, FontId::proportional(9.5), Color32::from_rgb(40, 100, 55));
    }
    // Dimensions, with room around their text so they read on anything.
    for (i, c) in s.constraints.iter().enumerate() {
        let Some(shape) = dim_shape(c, s, solved, f, pj) else { continue };
        let on = m.dim_edit.as_ref().is_some_and(|d| d.index == i);
        let col = if on { theme::ACCENT } else { Color32::from_rgb(24, 78, 170) };
        for l in &shape.lines {
            painter.line_segment(*l, Stroke::new(1.0, col));
        }
        if !on {
            let g = painter.layout_no_wrap(dim_text(c, vars), FontId::proportional(12.5), col);
            let r = Rect::from_center_size(shape.label, g.size() + vec2(8.0, 4.0));
            painter.rect(r, 3.0, Color32::from_white_alpha(240), Stroke::new(0.8, col.gamma_multiply(0.5)), StrokeKind::Inside);
            painter.galley(r.min + vec2(4.0, 2.0), g, col);
        }
    }
}

fn curve_is_loose(kind: &CurveKind, s: &Solved) -> bool {
    sk::curve_points(kind).iter().any(|p| s.loose_points.contains(p))
}

/// A dimension's text under the pointer.
fn dim_at(s: &Sketch, solved: &Solved, f: &Frame, pj: &Projector, at: Pos2, vars: &Vars, painter: &egui::Painter) -> Option<(usize, Pos2)> {
    for (i, c) in s.constraints.iter().enumerate() {
        let Some(shape) = dim_shape(c, s, solved, f, pj) else { continue };
        let g = painter.layout_no_wrap(dim_text(c, vars), FontId::proportional(12.5), theme::TEXT);
        if Rect::from_center_size(shape.label, g.size() + vec2(10.0, 6.0)).contains(at) {
            return Some((i, shape.label));
        }
    }
    None
}

/// Makes the dimension the tool's picks ask for, and opens it for typing.
fn make_dimension(st: &mut Studio, a: SkPick, b: Option<SkPick>, label_at: Pos2) {
    let Some(id) = st.ui.m3.active else { return };
    let vars = st.project.vars().unwrap_or_default();
    let Some(s) = st.project.model.sketch(id).cloned() else { return };
    let solved = s.solve(&vars);
    let curve_of = |p: SkPick| match p {
        SkPick::On(c, _) | SkPick::Mid(c, _) => Some(c),
        _ => None,
    };
    let line_ends = |c: u32| match s.curve(c).map(|x| &x.kind) {
        Some(CurveKind::Line { a, b }) => Some((*a, *b)),
        _ => None,
    };
    let round = |c: u32| matches!(s.curve(c).map(|x| &x.kind), Some(CurveKind::Circle { .. } | CurveKind::Arc { .. }));
    let dist = |x: u32, y: u32| {
        let (p, q) = (solved.at(x), solved.at(y));
        (q[0] - p[0]).hypot(q[1] - p[1])
    };
    let new = match (a, b) {
        (SkPick::Point(x), Some(SkPick::Point(y))) => Some(Constraint::Distance { a: x, b: y, value: mm_text(dist(x, y)) }),
        (SkPick::Point(p), Some(other)) | (other, Some(SkPick::Point(p))) if curve_of(other).and_then(line_ends).is_some() => {
            let line = curve_of(other).unwrap_or(0);
            let (e0, e1) = line_ends(line).unwrap_or((0, 0));
            let (q, u, v) = (solved.at(p), solved.at(e0), solved.at(e1));
            let d = [v[0] - u[0], v[1] - u[1]];
            let n = d[0].hypot(d[1]).max(1e-30);
            let off = (d[0] * (q[1] - u[1]) - d[1] * (q[0] - u[0])).abs() / n;
            Some(Constraint::PointLine { p, line, value: mm_text(off) })
        }
        (x, Some(y)) if curve_of(x).and_then(line_ends).is_some() && curve_of(y).and_then(line_ends).is_some() => {
            let (l1, l2) = (curve_of(x).unwrap_or(0), curve_of(y).unwrap_or(0));
            let dir = |c: u32| {
                let (p, q) = line_ends(c).unwrap_or((0, 0));
                let (u, v) = (solved.at(p), solved.at(q));
                (v[1] - u[1]).atan2(v[0] - u[0])
            };
            let deg = (dir(l2) - dir(l1)).to_degrees();
            Some(Constraint::Angle { a: l1, b: l2, value: format!("{:.3}deg", deg) })
        }
        (x, None) => match curve_of(x) {
            Some(c) if round(c) => {
                let r = match s.curve(c).map(|v| &v.kind) {
                    Some(CurveKind::Circle { .. }) => solved.radii.get(&c).copied().unwrap_or(0.0),
                    Some(CurveKind::Arc { c: cp, a: ap, .. }) => dist(*cp, *ap),
                    _ => 0.0,
                };
                Some(Constraint::Radius { curve: c, value: mm_text(r) })
            }
            Some(c) => line_ends(c).map(|(p, q)| Constraint::Distance { a: p, b: q, value: mm_text(dist(p, q)) }),
            None => None,
        },
        _ => None,
    };
    let Some(new) = new else {
        st.ui.m3.note = Some("A dimension goes on a line, a circle, two points, a point and a line, or two lines.".into());
        return;
    };
    super::draw::remember(st);
    let text = new.value().unwrap_or("").to_string();
    with_sketch(st, |s, _| s.constraints.push(new));
    let count = st.project.model.sketch(id).map_or(0, |s| s.constraints.len());
    st.ui.m3.dim_edit = Some(DimEdit { waiting: false, index: count - 1, text, at: label_at, fresh: true });
    st.ui.m3.picks.clear();
}

/// Every body selected, or else the one a command falls back on.
fn chosen_bodies(st: &Studio, fallback: u32) -> Vec<u32> {
    let chosen = st.ui.m3.sel.bodies();
    if chosen.is_empty() { vec![fallback] } else { chosen }
}

fn first_metal_or_body(st: &Studio) -> Option<u32> {
    st.ui.m3.sel.body.or_else(|| st.ui.built.bodies.last().map(|b| b.id))
}

/// Which feature a new one should build on: the sketch open or selected, else the latest.
fn sketch_for_feature(st: &Studio) -> Option<u32> {
    if let Some(id) = st.ui.m3.active {
        return Some(id);
    }
    if let Some(id) = st.ui.m3.sel.feature
        && st.project.model.sketch(id).is_some()
    {
        return Some(id);
    }
    st.project.model.last_sketch()
}

/// Adds a feature, selected and ready to have its dimensions typed.
pub fn add_feature(st: &mut Studio, kind: Kind) -> u32 {
    super::draw::remember(st);
    if st.ui.m3.active.is_some() {
        exit_sketch(st);
    }
    let makes_a_body = kind.output().is_some_and(|o| o.mode == Mode::New);
    let id = st.project.model.add(kind);
    st.ui.m3.sel = Selection { feature: Some(id), body: makes_a_body.then_some(id), ..Default::default() };
    st.ui.tree.pick = Default::default();
    st.ui.m3.note = None;
    id
}

fn default_size(st: &Studio) -> String {
    // A depth to start from that looks like something next to what's drawn.
    let sized = st.ui.m3.active.or(st.ui.m3.sel.feature).or_else(|| st.project.model.last_sketch()).and_then(|id| st.ui.built.sketches.get(&id)).and_then(|s| {
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        let mut any = false;
        for r in &s.profiles.regions {
            for q in &r.outer.pts {
                for k in 0..2 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
                any = true;
            }
        }
        any.then(|| ((hi[0] - lo[0]).min(hi[1] - lo[1]) * 0.25).max(1e-4))
    });
    match sized {
        Some(v) => mm_text((v * 1e6).round() / 1e6),
        None => mm_text((antenna_core::C0 / st.project.f0().max(1.0) / 40.0 * 1e4).round() / 1e4),
    }
}

/// What a sketch's feature name is for a list.
fn bodies_list(st: &Studio) -> Vec<(u32, String)> {
    st.ui.built.bodies.iter().map(|b| (b.id, b.name.clone())).collect()
}

fn xyz(x: &str, y: &str, z: &str) -> [String; 3] {
    [x.to_string(), y.to_string(), z.to_string()]
}

/// The body whose face a sketch is on, if it's on one.
fn body_under_sketch(st: &Studio, sketch: u32) -> Option<u32> {
    let Kind::Sketch { plane: PlaneRef::Face { face, .. }, .. } = &st.project.model.feature(sketch)?.kind else { return None };
    st.ui.built.bodies.iter().find(|b| b.mesh.has_face(*face)).map(|b| b.id)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    OnFace,
    Tool(SkTool),
    Construction,
    FullyDefine,
    ExitSketch,
    Extrude,
    Cut,
    Revolve,
    Sweep,
    Loft,
    Sheet,
    SurfaceExtrude,
    SurfaceRevolve,
    SurfaceLoft,
    Wire,
    Helix,
    /// Combines the bodies selected, or on the flat tools, the shapes.
    Boolean(CombineOp),
    Fillet,
    Chamfer,
    Shell,
    Thicken,
    Pattern,
    Circular,
    Mirror,
    Move,
    Feed,
    Probe,
    Section,
    Measure,
    MeshView,
    /// What's selected goes to the clipboard, and with `ClipCut` out of the model.
    Copy,
    ClipCut,
    Paste,
    Delete,
    /// Draws a shape with the mouse.
    Place(super::place::Shape),
    /// A sketch on the work plane.
    SketchHere,
    /// An STL, DXF or Gerber file, by what it is.
    ImportAny,
    Rotate,
    WorkPlane(usize),
    CoordAdd,
    CoordEdit,
    MeasureAngle,
    /// A page of the Studio, by its name.
    Page(&'static str),
    Stop,
    /// A template's or the flat tools' parts become features, so the tools here can work on them.
    BringIn,
}

/// The work calls for the modeler's tools: a sketch opened, say. The ribbon goes there.
fn ask_for_model_tab(st: &mut Studio) {
    st.ui.m3.tab_asked = st.ui.m3.tab_asked.wrapping_add(1);
}

/// Closes an open sketch for a command that picks off the model.
fn leave_sketch(st: &mut Studio) {
    if st.ui.m3.active.is_some() {
        exit_sketch(st);
    }
    ask_for_model_tab(st);
}

/// A new sketch goes on the work plane, in the coordinate system in use.
fn work_plane_ref(st: &Studio) -> PlaneRef {
    let axis = st.ui.m3.work_axis;
    match st.ui.m3.coord.filter(|id| st.project.model.coord(*id).is_some()) {
        Some(coord) => PlaneRef::InCoord { coord, axis, offset: "0".into() },
        None => PlaneRef::Base { axis, offset: "0".into() },
    }
}

/// The steps a command on the selection works on. An object picked counts as all of what made it,
/// by the body's id. A step picked, or anything picked with no body, is that step alone.
fn selected_steps(st: &Studio) -> Vec<u32> {
    let sel = &st.ui.m3.sel;
    let mut ids = if sel.step {
        sel.features()
    } else {
        let mut ids = sel.bodies();
        if let (Some(f), None) = (sel.feature, sel.body) {
            ids.push(f);
        }
        ids.extend(sel.more.iter().filter(|m| m.body.is_none()).map(|m| m.feature));
        ids
    };
    ids.retain(|id| st.project.model.feature(*id).is_some());
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A body picked that's a copy made by a pattern, a mirror or a move, which no step is all of.
fn picked_copy(st: &Studio) -> Option<String> {
    if st.ui.m3.sel.step {
        return None;
    }
    st.ui.m3.sel.bodies().iter().find(|b| st.project.model.feature(**b).is_none()).and_then(|b| st.ui.built.bodies.iter().find(|x| x.id == *b)).map(|b| b.name.clone())
}

/// Takes what's selected out of the model: a whole object if that's what's picked, else the step.
fn delete_selected(st: &mut Studio) {
    if let Some(name) = picked_copy(st) {
        st.ui.m3.note = Some(format!("{name} is a copy made by another step, and goes with the others. To take the copies away, delete that step: expand the object it came from."));
        return;
    }
    let ids = selected_steps(st);
    if ids.is_empty() {
        return;
    }
    super::draw::remember(st);
    let whole = if st.ui.m3.sel.step { Vec::new() } else { st.ui.m3.sel.bodies() };
    for id in ids {
        if whole.contains(&id) {
            st.project.model.remove_object(id);
        } else {
            st.project.model.remove(id);
        }
    }
    st.ui.m3.sel = Selection::default();
    st.ui.m3.picking = Picking::Nothing;
}

/// The next edit is part of the one just remembered.
pub(super) fn one_undo_step(st: &mut Studio) {
    st.ui.m3.editing = true;
}

pub(super) fn run_cmd(st: &mut Studio, c: Cmd) {
    let size = default_size(st);
    match c {
        Cmd::OnFace => {
            st.ui.m3.picking = Picking::SketchPlane;
            st.ui.pointer = Pointer::View;
            st.ui.m3.note = Some("Click a flat face of the model to sketch on it.".into());
        }
        Cmd::Tool(t) => {
            // A tool that draws in a sketch doesn't have to be told to start one first.
            if st.ui.m3.active.is_none() {
                new_sketch(st, work_plane_ref(st));
            }
            st.ui.m3.tool = t;
            st.ui.m3.picks.clear();
            st.ui.pointer = Pointer::Draw;
        }
        Cmd::Construction => {
            let chosen = st.ui.m3.sel.curves.clone();
            if chosen.is_empty() {
                st.ui.m3.note = Some("Select a line or curve to make it construction geometry.".into());
                return;
            }
            super::draw::remember(st);
            with_sketch(st, |s, _| {
                for c in s.curves.iter_mut().filter(|c| chosen.contains(&c.id)) {
                    c.construction = !c.construction;
                }
            });
        }
        Cmd::FullyDefine => {
            super::draw::remember(st);
            with_sketch(st, |s, v| s.fully_define(v));
        }
        Cmd::ExitSketch => exit_sketch(st),
        Cmd::Extrude | Cmd::Cut | Cmd::SurfaceExtrude | Cmd::Sheet | Cmd::Revolve | Cmd::SurfaceRevolve => {
            // A flat shape selected has its own way to get a thickness, with nothing to draw first.
            if c == Cmd::Extrude
                && let Some(id) = st.ui.m3.sel.feature.filter(|id| matches!(st.project.model.feature(*id).map(|f| &f.kind), Some(Kind::Face { .. } | Kind::Sheet { .. })))
            {
                super::draw::remember(st);
                if let Some(out) = st.project.model.feature_mut(id).and_then(|f| f.kind.output_mut()) {
                    out.thick = size;
                }
                return;
            }
            let Some(sketch) = sketch_for_feature(st) else {
                st.ui.m3.note = Some("Draw a sketch first: this builds on one.".into());
                ask_for_model_tab(st);
                return;
            };
            let on_body = body_under_sketch(st, sketch);
            let kind = match c {
                Cmd::Extrude => Kind::Extrude { sketch, depth: size, extent: Extent::One, out: Output { mode: if on_body.is_some() { Mode::Add } else { Mode::New }, target: on_body, ..Default::default() } },
                Cmd::Cut => Kind::Extrude { sketch, depth: size, extent: Extent::Through, out: Output { mode: Mode::Cut, target: on_body.or(st.ui.m3.sel.body), ..Default::default() } },
                Cmd::SurfaceExtrude => Kind::Extrude { sketch, depth: size, extent: Extent::One, out: Output { surface: true, ..Default::default() } },
                Cmd::Sheet => Kind::Sheet { sketch, out: Output::default() },
                Cmd::Revolve => Kind::Revolve { sketch, axis: AxisRef::V, angle: "360deg".into(), out: Output::default() },
                _ => Kind::Revolve { sketch, axis: AxisRef::V, angle: "360deg".into(), out: Output { surface: true, ..Default::default() } },
            };
            add_feature(st, kind);
        }
        Cmd::Sweep => {
            let Some(sketch) = sketch_for_feature(st) else {
                st.ui.m3.note = Some("Draw the cross-section in a sketch first.".into());
                return;
            };
            let path = st.project.model.features.iter().rev().find(|f| matches!(f.kind, Kind::Sketch { .. }) && f.id != sketch).map(|f| f.id);
            let path = path.map_or_else(|| PathRef::Helix { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), radius: size.clone(), radius2: String::new(), pitch: size.clone(), turns: "3".into(), left: false }, |id| PathRef::Sketch { sketch: id });
            add_feature(st, Kind::Sweep { sketch, path, out: Output::default() });
        }
        Cmd::Loft | Cmd::SurfaceLoft => {
            let mut sections: Vec<u32> = st.project.model.features.iter().filter(|f| matches!(f.kind, Kind::Sketch { .. })).map(|f| f.id).collect();
            if sections.len() > 2 {
                sections.drain(..sections.len() - 2);
            }
            if sections.len() < 2 {
                st.ui.m3.note = Some("A loft runs between two or more sketches: draw another on an offset plane.".into());
                return;
            }
            add_feature(st, Kind::Loft { sections, out: Output { surface: c == Cmd::SurfaceLoft, ..Default::default() } });
        }
        Cmd::Wire => {
            let Some(sketch) = sketch_for_feature(st) else {
                st.ui.m3.note = Some("A wire follows a sketch: draw its route first, or use Helix.".into());
                return;
            };
            add_feature(st, Kind::Wire { path: PathRef::Sketch { sketch }, radius: "0.5mm".into(), out: Output::default() });
        }
        Cmd::Helix => {
            add_feature(st, Kind::Wire { path: PathRef::Helix { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), radius: format!("({size})*3"), radius2: String::new(), pitch: format!("({size})*2"), turns: "5".into(), left: false }, radius: "0.5mm".into(), out: Output::default() });
        }
        Cmd::Boolean(op) => {
            forget_stale_selection(st);
            let list = bodies_list(st);
            if list.len() < 2 {
                st.ui.m3.note = Some("Combining takes two bodies, and there's only one so far.".into());
                return;
            }
            let mut chosen: Vec<u32> = st.ui.m3.sel.bodies().into_iter().filter(|b| list.iter().any(|l| l.0 == *b)).collect();
            // With only two there's nothing to choose between, only which comes first.
            if chosen.len() < 2 && list.len() == 2 {
                let first = chosen.first().copied().unwrap_or(list[0].0);
                chosen = vec![first, list.iter().map(|b| b.0).find(|b| *b != first).unwrap_or(first)];
            }
            if chosen.len() < 2 {
                st.ui.m3.note = Some("Ctrl+click at least two bodies first, on the model, in the tree or in the Bodies list. Then choose this again.".into());
                return;
            }
            let wire = st.ui.built.bodies.iter().find(|b| chosen.contains(&b.id) && b.kind == BodyKind::Wire);
            if let Some(b) = wire {
                st.ui.m3.note = Some(format!("{} is a wire, a route with no surface, so it can't be combined. Join metal to it with a feed or by touching it.", b.name));
                return;
            }
            add_feature(st, Kind::Combine { op, target: chosen[0], tools: chosen[1..].to_vec(), keep_tools: false });
        }
        Cmd::Fillet | Cmd::Chamfer => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to round.".into());
                return;
            };
            let id = add_feature(st, Kind::Fillet { body, edges: Vec::new(), radius: size, chamfer: c == Cmd::Chamfer });
            st.ui.m3.picking = Picking::Edges(id);
            st.ui.pointer = Pointer::View;
            st.ui.m3.note = Some("Click the edges to change. Click one again to leave it out.".into());
        }
        Cmd::Shell => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to hollow.".into());
                return;
            };
            let id = add_feature(st, Kind::Shell { body, open: Vec::new(), thick: "1mm".into() });
            st.ui.m3.picking = Picking::OpenFaces(id);
            st.ui.pointer = Pointer::View;
            st.ui.m3.note = Some("Click the faces to leave open. Thickness 0 leaves just the surfaces, for the method of moments.".into());
        }
        Cmd::Thicken => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no surface yet to thicken.".into());
                return;
            };
            add_feature(st, Kind::Thicken { body, thick: "1mm".into() });
        }
        Cmd::Pattern => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to repeat.".into());
                return;
            };
            let bodies = chosen_bodies(st, body);
            add_feature(st, Kind::Pattern { bodies, layout: Layout::Linear { step: xyz(&format!("({size})*8"), "0", "0"), count: "3".into(), step2: None, count2: "1".into() }, merge: false, feeds: Vec::new() });
        }
        Cmd::Circular => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to repeat.".into());
                return;
            };
            let bodies = chosen_bodies(st, body);
            add_feature(st, Kind::Pattern { bodies, layout: Layout::Circular { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), count: "4".into(), total: "360deg".into() }, merge: false, feeds: Vec::new() });
        }
        Cmd::Mirror => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to mirror.".into());
                return;
            };
            let bodies = chosen_bodies(st, body);
            add_feature(st, Kind::Mirror { bodies, plane: PlaneRef::Base { axis: 0, offset: "0".into() }, merge: false });
        }
        Cmd::Move => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to move.".into());
                return;
            };
            let bodies = chosen_bodies(st, body);
            add_feature(st, Kind::Move { bodies, by: xyz("0", "0", "0"), origin: None, axis: None, angle: String::new(), copy: false });
        }
        Cmd::Feed => {
            leave_sketch(st);
            st.ui.m3.picking = Picking::Feed(Vec::new());
            st.ui.pointer = Pointer::Draw;
            st.ui.m3.note = Some("Click one end of the feed, then the other. Sketch points and corners attract the pointer. A feed lies along x, y or z.".into());
        }
        Cmd::Probe => {
            leave_sketch(st);
            st.ui.m3.picking = Picking::Probe;
            st.ui.pointer = Pointer::Draw;
            st.ui.m3.note = Some("Click a point in a sketch: the probe runs through it, from the metal below to the metal above. Its dimensions then move the feed.".into());
        }
        Cmd::Section => {
            st.ui.m3.section_on = !st.ui.m3.section_on;
        }
        Cmd::Measure => {
            leave_sketch(st);
            st.ui.m3.picking = Picking::Measure(Vec::new());
            st.ui.pointer = Pointer::Draw;
        }
        Cmd::MeshView => {
            st.ui.view = if st.ui.view == ViewMode::Mesh { ViewMode::Model } else { ViewMode::Mesh };
        }
        Cmd::Copy | Cmd::ClipCut => {
            if let Some(name) = picked_copy(st) {
                st.ui.m3.note = Some(format!("{name} is a copy made by another step. Copy the object it came from instead."));
                return;
            }
            let ids = selected_steps(st);
            if ids.is_empty() {
                st.ui.m3.note = Some("Select something first, in the tree or on the model.".into());
                return;
            }
            st.ui.m3.clip = st.project.model.copy_steps(&ids);
            if c == Cmd::ClipCut {
                delete_selected(st);
            }
        }
        Cmd::Paste => {
            if st.ui.m3.clip.is_empty() {
                st.ui.m3.note = Some("There's nothing copied yet.".into());
                return;
            }
            super::draw::remember(st);
            let clip = st.ui.m3.clip.clone();
            let new = st.project.model.paste_steps(&clip);
            // The copy sits on the original until it's moved, so it's left selected to be found.
            let body = new.iter().rev().copied().find(|id| st.project.model.feature(*id).is_some_and(|f| f.kind.output().is_some_and(|o| o.mode == Mode::New)));
            st.ui.tree.pick = Default::default();
            st.ui.m3.sel = Selection { feature: body.or(new.last().copied()), body, ..Default::default() };
        }
        Cmd::Delete => delete_selected(st),
        Cmd::Place(shape) => super::place::start(st, shape),
        Cmd::SketchHere => {
            if st.ui.m3.active.is_some() {
                exit_sketch(st);
            }
            new_sketch(st, work_plane_ref(st));
        }
        Cmd::ImportAny => import_any(st),
        Cmd::Rotate => {
            let Some(body) = first_metal_or_body(st) else {
                st.ui.m3.note = Some("There's no body yet to turn.".into());
                return;
            };
            let bodies = chosen_bodies(st, body);
            add_feature(st, Kind::Move { bodies, by: xyz("0", "0", "0"), origin: Some(xyz("0", "0", "0")), axis: Some(xyz("0", "0", "1")), angle: "90deg".into(), copy: false });
        }
        Cmd::WorkPlane(axis) => st.ui.m3.work_axis = axis,
        Cmd::CoordAdd => {
            super::draw::remember(st);
            let id = st.project.model.add_coord();
            st.ui.m3.coord = Some(id);
            st.ui.m3.sel = Selection::default();
            st.ui.tree.pick = super::project_tree::Pick::Coord(Some(id));
        }
        Cmd::CoordEdit => {
            let Some(id) = st.ui.m3.coord else {
                st.ui.m3.note = Some("The global system stays where it is. Add one to move or turn.".into());
                return;
            };
            st.ui.m3.sel = Selection::default();
            st.ui.tree.pick = super::project_tree::Pick::Coord(Some(id));
        }
        Cmd::MeasureAngle => {
            leave_sketch(st);
            st.ui.m3.picking = Picking::Angle(Vec::new());
            st.ui.pointer = Pointer::Draw;
        }
        Cmd::Page(name) => {
            if let Some(p) = Page::from_name(name) {
                st.ui.page = p;
            }
        }
        Cmd::Stop => st.cancel(),
        Cmd::BringIn => match st.project.parts_into_model() {
            Ok(p) => {
                super::draw::remember(st);
                st.project = p;
                st.ui.m3.note = None;
            }
            Err(e) => st.ui.m3.note = Some(format!("{e:#}")),
        },
    }
}

/// A file of any kind the Studio reads: a drawing goes to the page that asks how to take it in.
fn import_any(st: &mut Studio) {
    let Some(paths) = rfd::FileDialog::new().add_filter("Model or drawing", &["stl", "dxf", "gbr", "ger", "gtl", "gbl", "art", "pho"]).add_filter("All files", &["*"]).pick_files() else { return };
    let (models, drawings): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("stl")));
    for p in &models {
        import_stl_at(st, p);
    }
    if !drawings.is_empty() {
        super::import::load(st, &drawings);
        st.ui.page = Page::Parts;
    }
}

fn import_stl_at(st: &mut Studio, path: &std::path::Path) {
    let read = std::fs::read(path).map_err(anyhow::Error::from).and_then(|b| antenna_core::geometry::read_stl(&b, 1e-3));
    match read {
        Ok(tris) => {
            add_feature(st, Kind::Import { file: path.display().to_string(), tris, out: Output::default() });
        }
        Err(e) => st.ui.m3.note = Some(format!("Could not read the file: {e:#}")),
    }
}

/// The drawn symbol on a command, so a row of them reads at a glance.
#[derive(Clone, Copy)]
pub enum Glyph {
    Plane(usize),
    Face,
    Arrow,
    Line,
    Rect,
    CentreRect,
    Circle,
    Arc,
    Spline,
    Point,
    Dim,
    Round,
    Dash,
    Lock,
    Check,
    Extrude,
    Cut,
    Revolve,
    Sweep,
    Loft,
    Box_,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Sheet,
    SurfaceExtrude,
    Wire,
    Helix,
    Join,
    Subtract,
    Intersect,
    Exclude,
    Fillet,
    Chamfer,
    Shell,
    Thicken,
    Pattern,
    Circular,
    Mirror,
    Move,
    Import,
    Feed,
    Probe,
    Section,
    Measure,
    Mesh,
    /// A symbol from the icon font.
    Icon(&'static str),
    Paste,
    Scissors,
    Ellipse,
    Polygon,
    Polyline,
    Angle,
}

/// What each way of combining is called and does, on the ribbon and on a selection's own card.
const COMBINE_WAYS: [(CombineOp, Glyph, &str, &str); 4] = [
    (CombineOp::Add, Glyph::Join, "Join", "Everything selected, as one"),
    (CombineOp::Cut, Glyph::Subtract, "Subtract", "The first selected, less the others"),
    (CombineOp::Common, Glyph::Intersect, "Intersect", "Only where all of them overlap"),
    (CombineOp::Exclude, Glyph::Exclude, "Exclude", "Where they don't overlap"),
];

/// A small isometric box, the shape most of these commands start from.
fn iso(p: &egui::Painter, c: Pos2, s: f32, stroke: Stroke, fill: Color32) {
    let (w, h) = (s * 0.86, s * 0.5);
    let top = [c + vec2(0.0, -s), c + vec2(w, -h), c, c + vec2(-w, -h)];
    let left = [c + vec2(-w, -h), c, c + vec2(0.0, s), c + vec2(-w, h)];
    let right = [c, c + vec2(w, -h), c + vec2(w, h), c + vec2(0.0, s)];
    p.add(egui::Shape::convex_polygon(top.to_vec(), fill.gamma_multiply(0.5), stroke));
    p.add(egui::Shape::convex_polygon(left.to_vec(), fill.gamma_multiply(0.85), stroke));
    p.add(egui::Shape::convex_polygon(right.to_vec(), fill, stroke));
}

/// A command's symbol at another size. They're drawn to a 30-point box, so a small one goes on its
/// own layer, scaled, and every symbol stays one piece of drawing.
pub(super) fn small_glyph(ui: &egui::Ui, center: Pos2, size: f32, g: Glyph, on: bool) {
    let k = size / 30.0;
    let parent = ui.layer_id();
    let layer = egui::LayerId::new(parent.order, parent.id.with(("small_glyphs", (k * 1000.0) as u32)));
    let ctx = ui.ctx();
    ctx.set_transform_layer(layer, egui::emath::TSTransform::from_scaling(k));
    ctx.set_sublayer(parent, layer);
    let clip = ui.clip_rect();
    let painter = egui::Painter::new(ctx.clone(), layer, Rect::from_min_max(clip.min / k, clip.max / k));
    paint_glyph(&painter, Rect::from_center_size(center / k, vec2(30.0, 30.0)), g, on);
}

fn paint_glyph(p: &egui::Painter, r: Rect, g: Glyph, on: bool) {
    let c = r.center();
    let ink = if on { Color32::from_rgb(44, 62, 96) } else { theme::FAINT };
    let st = Stroke::new(1.5, ink);
    let accent = if on { theme::ACCENT } else { theme::FAINT };
    let fill = if on { Color32::from_rgb(190, 208, 240) } else { Color32::from_rgb(232, 234, 238) };
    let metal = if on { Color32::from_rgb(232, 196, 130) } else { Color32::from_rgb(232, 234, 238) };
    let a = Stroke::new(2.0, accent);
    let arrow = |tip: Pos2, from: Pos2| {
        let d = (tip - from).normalized();
        p.line_segment([from, tip], a);
        p.line_segment([tip, tip - d * 5.0 + d.rot90() * 3.5], a);
        p.line_segment([tip, tip - d * 5.0 - d.rot90() * 3.5], a);
    };
    let circle_pts = |at: Pos2, rx: f32, ry: f32, from: f32, to: f32| -> Vec<Pos2> {
        (0..=24).map(|k| { let t = from + (to - from) * k as f32 / 24.0; at + vec2(rx * t.cos(), ry * t.sin()) }).collect()
    };
    let tau = std::f32::consts::TAU;
    match g {
        Glyph::Plane(axis) => {
            let q: [Pos2; 4] = match axis {
                2 => [c + vec2(-11.0, 2.0), c + vec2(0.0, -6.0), c + vec2(11.0, 2.0), c + vec2(0.0, 9.0)],
                1 => [c + vec2(-9.0, -9.0), c + vec2(9.0, -9.0), c + vec2(9.0, 9.0), c + vec2(-9.0, 9.0)],
                _ => [c + vec2(-6.0, -6.0), c + vec2(6.0, -10.0), c + vec2(6.0, 8.0), c + vec2(-6.0, 11.0)],
            };
            p.add(egui::Shape::convex_polygon(q.to_vec(), fill, Stroke::new(1.5, accent)));
        }
        Glyph::Face => {
            iso(p, c, 11.0, st, metal);
            let top = [c + vec2(0.0, -11.0), c + vec2(9.5, -5.5), c, c + vec2(-9.5, -5.5)];
            p.add(egui::Shape::convex_polygon(top.to_vec(), theme::ACCENT.gamma_multiply(0.55), Stroke::new(1.5, accent)));
        }
        Glyph::Arrow => {
            let q = [c + vec2(-7.0, -10.0), c + vec2(7.0, 3.0), c + vec2(1.0, 3.0), c + vec2(4.5, 10.0), c + vec2(1.5, 11.0), c + vec2(-2.0, 4.5), c + vec2(-7.0, 8.0)];
            p.add(egui::Shape::closed_line(q.to_vec(), st));
        }
        Glyph::Line => {
            p.line_segment([c + vec2(-10.0, 8.0), c + vec2(10.0, -8.0)], a);
            p.circle_filled(c + vec2(-10.0, 8.0), 2.5, ink);
            p.circle_filled(c + vec2(10.0, -8.0), 2.5, ink);
        }
        Glyph::Rect => {
            p.rect_stroke(Rect::from_center_size(c, vec2(21.0, 15.0)), 0.0, a, StrokeKind::Middle);
            p.circle_filled(c + vec2(-10.5, 7.5), 2.2, ink);
            p.circle_filled(c + vec2(10.5, -7.5), 2.2, ink);
        }
        Glyph::CentreRect => {
            p.rect_stroke(Rect::from_center_size(c, vec2(21.0, 15.0)), 0.0, a, StrokeKind::Middle);
            p.line_segment([c, c + vec2(10.5, -7.5)], Stroke::new(1.0, ink));
            p.circle_filled(c, 2.2, ink);
        }
        Glyph::Circle => {
            p.circle_stroke(c, 9.5, a);
            p.circle_filled(c, 2.0, ink);
        }
        Glyph::Arc => {
            p.add(egui::Shape::line(circle_pts(c + vec2(0.0, 5.0), 10.0, 10.0, tau * 0.5, tau), a));
            p.circle_filled(c + vec2(-10.0, 5.0), 2.2, ink);
            p.circle_filled(c + vec2(10.0, 5.0), 2.2, ink);
        }
        Glyph::Spline => {
            let pts: Vec<Pos2> = (0..=24).map(|k| { let t = k as f32 / 24.0; c + vec2(-11.0 + 22.0 * t, -7.0 * (t * tau).sin()) }).collect();
            p.add(egui::Shape::line(pts, a));
        }
        Glyph::Point => {
            p.circle_filled(c, 3.5, accent);
            p.line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], Stroke::new(1.0, ink));
            p.line_segment([c - vec2(0.0, 8.0), c + vec2(0.0, 8.0)], Stroke::new(1.0, ink));
        }
        Glyph::Dim => {
            let y = c.y + 2.0;
            p.line_segment([pos2(c.x - 11.0, y), pos2(c.x + 11.0, y)], a);
            p.line_segment([pos2(c.x - 11.0, y - 6.0), pos2(c.x - 11.0, y + 6.0)], st);
            p.line_segment([pos2(c.x + 11.0, y - 6.0), pos2(c.x + 11.0, y + 6.0)], st);
            p.text(c + vec2(0.0, -7.0), Align2::CENTER_CENTER, "12", FontId::proportional(9.5), ink);
        }
        Glyph::Round => {
            p.line_segment([c + vec2(-10.0, 10.0), c + vec2(-10.0, 0.0)], st);
            p.add(egui::Shape::line(circle_pts(c, 10.0, 10.0, tau * 0.5, tau * 0.75), a));
            p.line_segment([c + vec2(0.0, -10.0), c + vec2(10.0, -10.0)], st);
        }
        Glyph::Dash => {
            for k in 0..3 {
                let x = -11.0 + k as f32 * 8.5;
                p.line_segment([c + vec2(x, 6.0 - k as f32 * 6.0), c + vec2(x + 5.0, 3.0 - k as f32 * 6.0)], a);
            }
        }
        Glyph::Lock => {
            p.rect(Rect::from_center_size(c + vec2(0.0, 3.0), vec2(14.0, 11.0)), 1.5, fill, a, StrokeKind::Middle);
            p.add(egui::Shape::line(circle_pts(c + vec2(0.0, -3.0), 4.5, 5.0, tau * 0.5, tau), st));
        }
        Glyph::Check => {
            p.line_segment([c + vec2(-9.0, 1.0), c + vec2(-3.0, 8.0)], Stroke::new(2.6, theme::SUCCESS));
            p.line_segment([c + vec2(-3.0, 8.0), c + vec2(10.0, -8.0)], Stroke::new(2.6, theme::SUCCESS));
        }
        Glyph::Extrude => {
            iso(p, c + vec2(0.0, 5.0), 8.5, st, metal);
            arrow(c + vec2(0.0, -12.0), c + vec2(0.0, -3.0));
        }
        Glyph::Cut => {
            iso(p, c + vec2(0.0, 1.0), 11.0, st, metal);
            let hole = circle_pts(c + vec2(0.0, -4.5), 4.5, 2.5, 0.0, tau);
            p.add(egui::Shape::closed_line(hole, Stroke::new(1.8, theme::DANGER)));
            p.line_segment([c + vec2(0.0, -11.0), c + vec2(0.0, 0.0)], Stroke::new(1.8, theme::DANGER));
        }
        Glyph::Revolve => {
            p.line_segment([c + vec2(-8.0, -12.0), c + vec2(-8.0, 12.0)], Stroke::new(1.0, ink));
            p.add(egui::Shape::closed_line(circle_pts(c + vec2(2.0, 0.0), 9.0, 4.0, 0.0, tau), a));
            p.rect(Rect::from_center_size(c + vec2(5.0, -5.0), vec2(8.0, 8.0)), 0.0, metal, st, StrokeKind::Middle);
        }
        Glyph::Sweep => {
            p.add(egui::Shape::line(circle_pts(c + vec2(-11.0, 11.0), 20.0, 20.0, tau * 0.75, tau), a));
            p.add(egui::Shape::closed_line(circle_pts(c + vec2(9.0, 10.0), 4.5, 2.5, 0.0, tau), st));
            p.add(egui::Shape::closed_line(circle_pts(c + vec2(-11.0, -9.0), 2.5, 4.5, 0.0, tau), st));
        }
        Glyph::Loft => {
            let q = [c + vec2(-4.0, -11.0), c + vec2(4.0, -11.0), c + vec2(11.0, 10.0), c + vec2(-11.0, 10.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), metal, st));
            p.line_segment([c + vec2(-6.0, -3.0), c + vec2(6.0, -3.0)], Stroke::new(1.0, ink));
        }
        Glyph::Box_ => iso(p, c, 11.0, st, metal),
        Glyph::Cylinder => {
            let q = [c + vec2(-8.0, -7.0), c + vec2(8.0, -7.0), c + vec2(8.0, 8.0), c + vec2(-8.0, 8.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), metal, Stroke::NONE));
            p.add(egui::Shape::line(circle_pts(c + vec2(0.0, 8.0), 8.0, 3.0, 0.0, tau * 0.5), st));
            p.add(egui::Shape::closed_line(circle_pts(c + vec2(0.0, -7.0), 8.0, 3.0, 0.0, tau), st));
            p.line_segment([c + vec2(-8.0, -7.0), c + vec2(-8.0, 8.0)], st);
            p.line_segment([c + vec2(8.0, -7.0), c + vec2(8.0, 8.0)], st);
        }
        Glyph::Cone => {
            let q = [c + vec2(0.0, -11.0), c + vec2(9.5, 8.0), c + vec2(-9.5, 8.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), metal, st));
            p.add(egui::Shape::line(circle_pts(c + vec2(0.0, 8.0), 9.5, 3.0, 0.0, tau * 0.5), st));
        }
        Glyph::Sphere => {
            p.circle(c, 10.0, metal, st);
            p.add(egui::Shape::line(circle_pts(c, 10.0, 3.5, 0.0, tau * 0.5), Stroke::new(1.0, ink)));
        }
        Glyph::Torus => {
            p.add(egui::Shape::closed_line(circle_pts(c, 12.0, 7.5, 0.0, tau), Stroke::new(1.6, ink)));
            p.add(egui::Shape::closed_line(circle_pts(c, 5.0, 2.5, 0.0, tau), Stroke::new(1.6, ink)));
        }
        Glyph::Sheet => {
            let q = [c + vec2(-12.0, 3.0), c + vec2(1.0, -6.0), c + vec2(12.0, -1.0), c + vec2(-1.0, 8.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), metal, st));
        }
        Glyph::SurfaceExtrude => {
            let bottom = [c + vec2(-11.0, 9.0), c + vec2(-3.0, 3.0), c + vec2(3.0, 7.0), c + vec2(11.0, 2.0)];
            p.add(egui::Shape::line(bottom.to_vec(), st));
            let q = [c + vec2(-3.0, 3.0), c + vec2(3.0, 7.0), c + vec2(3.0, -7.0), c + vec2(-3.0, -11.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), metal, st));
            let q2 = [c + vec2(3.0, 7.0), c + vec2(11.0, 2.0), c + vec2(11.0, -12.0), c + vec2(3.0, -7.0)];
            p.add(egui::Shape::convex_polygon(q2.to_vec(), metal, st));
        }
        Glyph::Wire => {
            p.add(egui::Shape::line(vec![c + vec2(-11.0, 9.0), c + vec2(-3.0, -8.0), c + vec2(3.0, 8.0), c + vec2(11.0, -9.0)], Stroke::new(2.4, Color32::from_rgb(180, 120, 50))));
        }
        Glyph::Helix => {
            let pts: Vec<Pos2> = (0..=56).map(|k| { let t = k as f32 / 56.0; c + vec2(8.0 * (t * tau * 3.0).cos(), -11.0 + 22.0 * t + 2.5 * (t * tau * 3.0).sin()) }).collect();
            p.add(egui::Shape::line(pts, Stroke::new(1.8, Color32::from_rgb(180, 120, 50))));
        }
        Glyph::Join | Glyph::Subtract | Glyph::Intersect | Glyph::Exclude => {
            let (l, right) = (c + vec2(-4.5, 0.0), c + vec2(4.5, 0.0));
            let (radius, half_angle) = (8.0, (4.5_f32 / 8.0).acos());
            // Where the two circles overlap.
            let mut lens = circle_pts(l, radius, radius, -half_angle, half_angle);
            lens.extend(circle_pts(right, radius, radius, tau / 2.0 - half_angle, tau / 2.0 + half_angle));
            let paper = Color32::WHITE;
            match g {
                Glyph::Join => {
                    p.circle(l, radius, metal, st);
                    p.circle(right, radius, metal, st);
                    p.circle_filled(l, radius - 0.8, metal);
                    p.circle_filled(right, radius - 0.8, metal);
                }
                Glyph::Subtract => {
                    p.circle(l, radius, metal, st);
                    p.circle(right, radius, paper, Stroke::new(1.0, accent));
                }
                Glyph::Intersect => {
                    p.circle_stroke(l, radius, st);
                    p.circle_stroke(right, radius, st);
                    p.add(egui::Shape::convex_polygon(lens, metal, st));
                }
                _ => {
                    p.circle(l, radius, metal, st);
                    p.circle(right, radius, metal, st);
                    p.add(egui::Shape::convex_polygon(lens, paper, st));
                }
            }
        }
        Glyph::Fillet => {
            let mut q = vec![c + vec2(-10.0, 10.0), c + vec2(-10.0, -10.0)];
            q.extend(circle_pts(c + vec2(-2.0, 2.0), 8.0, 8.0, tau * 0.75, tau));
            q.push(c + vec2(6.0, 10.0));
            p.add(egui::Shape::closed_line(q, st));
            p.add(egui::Shape::line(circle_pts(c + vec2(-2.0, 2.0), 8.0, 8.0, tau * 0.75, tau), a));
        }
        Glyph::Chamfer => {
            let q = vec![c + vec2(-10.0, 10.0), c + vec2(-10.0, -10.0), c + vec2(0.0, -10.0), c + vec2(9.0, -1.0), c + vec2(9.0, 10.0)];
            p.add(egui::Shape::closed_line(q, st));
            p.line_segment([c + vec2(0.0, -10.0), c + vec2(9.0, -1.0)], a);
        }
        Glyph::Shell => {
            iso(p, c, 11.0, st, metal);
            let inner = [c + vec2(0.0, -8.0), c + vec2(6.5, -4.0), c + vec2(0.0, -1.0), c + vec2(-6.5, -4.0)];
            p.add(egui::Shape::convex_polygon(inner.to_vec(), Color32::from_rgb(70, 80, 100), Stroke::new(1.0, ink)));
        }
        Glyph::Thicken => {
            p.rect(Rect::from_center_size(c, vec2(22.0, 7.0)), 0.0, metal, st, StrokeKind::Middle);
            arrow(c + vec2(0.0, -11.0), c + vec2(0.0, -4.0));
            arrow(c + vec2(0.0, 11.0), c + vec2(0.0, 4.0));
        }
        Glyph::Pattern => {
            for (x, y) in [(-8.0, -8.0), (8.0, -8.0), (-8.0, 8.0), (8.0, 8.0)] {
                p.rect(Rect::from_center_size(c + vec2(x, y), vec2(9.0, 9.0)), 1.0, metal, st, StrokeKind::Middle);
            }
        }
        Glyph::Circular => {
            for k in 0..6 {
                let t = k as f32 / 6.0 * tau;
                p.circle(c + vec2(9.5 * t.cos(), 9.5 * t.sin()), 3.2, metal, st);
            }
        }
        Glyph::Mirror => {
            p.line_segment([c + vec2(0.0, -12.0), c + vec2(0.0, 12.0)], a);
            p.add(egui::Shape::convex_polygon(vec![c + vec2(-3.0, -7.0), c + vec2(-3.0, 7.0), c + vec2(-11.0, 7.0)], metal, st));
            p.add(egui::Shape::closed_line(vec![c + vec2(3.0, -7.0), c + vec2(11.0, 7.0), c + vec2(3.0, 7.0)], st));
        }
        Glyph::Move => {
            arrow(c + vec2(12.0, 0.0), c);
            arrow(c + vec2(-12.0, 0.0), c);
            arrow(c + vec2(0.0, 12.0), c);
            arrow(c + vec2(0.0, -12.0), c);
        }
        Glyph::Import => {
            p.rect_stroke(Rect::from_center_size(c + vec2(0.0, 4.0), vec2(20.0, 11.0)), 1.5, st, StrokeKind::Middle);
            arrow(c + vec2(0.0, 5.0), c + vec2(0.0, -12.0));
        }
        Glyph::Feed => {
            p.rect(Rect::from_center_size(c + vec2(-8.0, 0.0), vec2(8.0, 9.0)), 0.0, metal, st, StrokeKind::Middle);
            p.rect(Rect::from_center_size(c + vec2(8.0, 0.0), vec2(8.0, 9.0)), 0.0, metal, st, StrokeKind::Middle);
            p.line_segment([c + vec2(-4.0, 0.0), c + vec2(4.0, 0.0)], Stroke::new(2.4, theme::DANGER));
            p.circle_filled(c, 2.8, theme::DANGER);
        }
        Glyph::Probe => {
            p.line_segment([c + vec2(-12.0, -8.0), c + vec2(12.0, -8.0)], Stroke::new(2.4, Color32::from_rgb(190, 140, 70)));
            p.line_segment([c + vec2(-12.0, 9.0), c + vec2(12.0, 9.0)], Stroke::new(2.4, Color32::from_rgb(190, 140, 70)));
            p.line_segment([c + vec2(0.0, -8.0), c + vec2(0.0, 9.0)], Stroke::new(2.2, theme::DANGER));
        }
        Glyph::Section => {
            iso(p, c + vec2(-3.0, 1.0), 10.0, st, metal);
            let q = [c + vec2(1.0, -13.0), c + vec2(1.0, 8.0), c + vec2(10.0, 13.0), c + vec2(10.0, -8.0)];
            p.add(egui::Shape::convex_polygon(q.to_vec(), theme::ACCENT.gamma_multiply(0.35), Stroke::new(1.3, accent)));
        }
        Glyph::Measure => {
            let q = vec![c + vec2(-12.0, 3.0), c + vec2(3.0, -12.0), c + vec2(12.0, -3.0), c + vec2(-3.0, 12.0)];
            p.add(egui::Shape::closed_line(q, a));
            for k in 0..4 {
                let x = -6.0 + k as f32 * 4.5;
                p.line_segment([c + vec2(x, -x), c + vec2(x + 2.5, -x + 2.5)], Stroke::new(1.0, ink));
            }
        }
        Glyph::Icon(text) => {
            p.text(c, Align2::CENTER_CENTER, text, FontId::proportional(25.0), accent);
        }
        Glyph::Paste => {
            let sheet = Rect::from_center_size(c + vec2(0.0, 2.0), vec2(20.0, 24.0));
            p.rect(sheet, 2.0, fill, st, StrokeKind::Inside);
            let clip = Rect::from_center_size(c + vec2(0.0, -10.0), vec2(10.0, 6.0));
            p.rect(clip, 1.5, accent, Stroke::NONE, StrokeKind::Inside);
            for dy in [-1.0, 4.0, 9.0] {
                p.hline((c.x - 6.0)..=(c.x + 6.0), c.y + dy, Stroke::new(1.3, ink));
            }
        }
        Glyph::Scissors => {
            for s in [-1.0, 1.0] {
                p.circle_stroke(c + vec2(s * 6.0, 8.0), 3.8, Stroke::new(1.7, accent));
                p.line_segment([c + vec2(s * 4.0, 5.0), c + vec2(-s * 6.0, -11.0)], Stroke::new(1.7, ink));
            }
        }
        Glyph::Ellipse => {
            p.add(egui::Shape::convex_polygon(circle_pts(c, 12.0, 7.5, 0.0, tau), metal, st));
        }
        Glyph::Polygon => {
            let hex: Vec<Pos2> = (0..6).map(|k| { let t = tau * k as f32 / 6.0 + tau / 12.0; c + vec2(11.0 * t.cos(), 11.0 * t.sin()) }).collect();
            p.add(egui::Shape::convex_polygon(hex, metal, st));
        }
        Glyph::Polyline => {
            let pts = [c + vec2(-11.0, 8.0), c + vec2(-4.0, -7.0), c + vec2(4.0, 5.0), c + vec2(11.0, -9.0)];
            for w in pts.windows(2) {
                p.line_segment([w[0], w[1]], a);
            }
            for q in pts {
                p.circle_filled(q, 2.0, ink);
            }
        }
        Glyph::Angle => {
            let at = c + vec2(-10.0, 8.0);
            p.line_segment([at, c + vec2(12.0, 8.0)], st);
            p.line_segment([at, c + vec2(6.0, -9.0)], st);
            p.add(egui::Shape::line(circle_pts(at, 12.0, 12.0, -0.82, 0.0), a));
        }
        Glyph::Mesh => {
            let (x0, y0, s) = (c.x - 10.0, c.y - 10.0, 10.0);
            for i in 0..3 {
                let v = i as f32 * s;
                p.line_segment([pos2(x0 + v, y0), pos2(x0 + v, y0 + 20.0)], Stroke::new(1.1, accent));
                p.line_segment([pos2(x0, y0 + v), pos2(x0 + 20.0, y0 + v)], Stroke::new(1.1, accent));
            }
            p.line_segment([pos2(x0, y0 + 10.0), pos2(x0 + 10.0, y0)], Stroke::new(1.0, ink));
            p.line_segment([pos2(x0 + 10.0, y0 + 20.0), pos2(x0 + 20.0, y0 + 10.0)], Stroke::new(1.0, ink));
            p.line_segment([pos2(x0 + 10.0, y0 + 10.0), pos2(x0 + 20.0, y0)], Stroke::new(1.0, ink));
            p.line_segment([pos2(x0, y0 + 20.0), pos2(x0 + 10.0, y0 + 10.0)], Stroke::new(1.0, ink));
        }
    }
}

/// One command: its symbol over its name.
pub(super) fn cmd_button(ui: &mut egui::Ui, g: Glyph, text: &str, enabled: bool, selected: bool, tip: &str) -> bool {
    let font = FontId::proportional(11.0);
    let width = ui.painter().layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x + 12.0;
    let (rect, r) = ui.allocate_exact_size(vec2(width.max(46.0), 50.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if selected {
        p.rect(rect, 5.0, theme::ACCENT_SOFT, Stroke::new(1.0, Color32::from_rgb(190, 212, 250)), StrokeKind::Inside);
    } else if enabled && r.hovered() {
        p.rect_filled(rect, 5.0, theme::HOVER);
    }
    paint_glyph(p, Rect::from_center_size(rect.center_top() + vec2(0.0, 18.0), vec2(30.0, 30.0)), g, enabled);
    p.text(rect.center_bottom() - vec2(0.0, 9.0), Align2::CENTER_CENTER, text, font, if enabled { theme::TEXT } else { theme::FAINT });
    let r = if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
    let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
    r.clicked()
}

/// What the app's ribbon shows of the modeler. Published each frame, as the ribbon has no Studio.
#[derive(Clone, Copy, Default)]
pub struct ModelBar {
    /// The 3D model is on screen, so the tool in hand can light up.
    pub showing: bool,
    pub sketching: bool,
    pub has_sketch: bool,
    pub has_body: bool,
    /// There's something Join, Subtract and the rest could work on, in the model or the flat tools.
    pub can_combine: bool,
    /// Parts on the canvas that aren't in the feature tree yet.
    pub loose_parts: usize,
    /// Anything on the canvas at all.
    pub has_parts: bool,
    pub tool: SkTool,
    pub picking_plane: bool,
    pub picking_feed: bool,
    pub picking_probe: bool,
    pub picking_measure: bool,
    pub mesh_view: bool,
    pub section: bool,
    pub tab_asked: u32,
    /// Something's copied, ready to paste.
    pub can_paste: bool,
    pub has_selection: bool,
    /// A flat shape or sheet is selected, which Extrude can give a thickness.
    pub flat_selected: bool,
    pub placing: Option<super::place::Shape>,
    pub picking_angle: bool,
    pub work_axis: usize,
    /// A coordinate system other than the global one is in use.
    pub custom_coord: bool,
    /// A solve is running.
    pub busy: bool,
}

pub fn ribbon_state(st: &Studio) -> ModelBar {
    let m = &st.ui.m3;
    ModelBar {
        showing: st.ui.page == Page::Draw && !m.flat,
        sketching: m.active.is_some(),
        has_sketch: st.project.model.last_sketch().is_some(),
        has_body: !st.ui.built.bodies.is_empty(),
        can_combine: !st.ui.built.bodies.is_empty() || (m.flat && !st.project.extra.is_empty()),
        loose_parts: st.ui.model.as_ref().map_or(0, |g| g.solids.len()),
        has_parts: !st.ui.parts.is_empty() || !st.ui.all_parts.is_empty(),
        tool: m.tool,
        picking_plane: m.picking == Picking::SketchPlane,
        picking_feed: matches!(m.picking, Picking::Feed(_)),
        picking_probe: m.picking == Picking::Probe,
        picking_measure: matches!(m.picking, Picking::Measure(_)),
        mesh_view: st.ui.view == ViewMode::Mesh,
        section: m.section_on,
        tab_asked: m.tab_asked,
        can_paste: !m.clip.is_empty(),
        has_selection: !m.sel.features().is_empty() || !m.sel.bodies().is_empty(),
        flat_selected: m.sel.feature.is_some_and(|id| matches!(st.project.model.feature(id).map(|f| &f.kind), Some(Kind::Face { .. } | Kind::Sheet { .. }))),
        placing: if let Picking::Place(p) = &m.picking { Some(p.shape) } else { None },
        picking_angle: matches!(m.picking, Picking::Angle(_)),
        work_axis: m.work_axis,
        custom_coord: m.coord.is_some(),
        busy: st.busy(),
    }
}

/// A tool chosen on the app's ribbon, which stays up on every page: first the 3D model comes into
/// view, whatever was showing, then the tool runs.
pub fn ribbon_command(st: &mut Studio, c: Cmd) {
    // These have nothing to do with the model being in view.
    if matches!(c, Cmd::Page(_) | Cmd::Stop | Cmd::WorkPlane(_)) {
        run_cmd(st, c);
        return;
    }
    st.ui.page = Page::Draw;
    super::canvas_for_page(st);
    // On the flat tools these work on the shapes selected there, so there's no leaving them.
    if let (Cmd::Boolean(op), true) = (c, st.ui.m3.flat) {
        super::draw::combine_selected(st, op);
        return;
    }
    if st.ui.m3.flat {
        choose_flat(st, false);
    }
    if !matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh) {
        st.ui.view = ViewMode::Model;
    }
    run_cmd(st, c);
}

/// The Draw and View switch, next to the view's own tools. It says who has the left button.
pub fn pointer_switch(ui: &mut egui::Ui, st: &mut Studio) {
    let mut p = st.ui.pointer;
    let tip_v = "Left-drag orbits, right-drag pans, the wheel zooms, and a click selects. Nothing gets drawn. (V)";
    let tip_d = "The left button draws, with the tool in hand. Middle-drag or Alt-drag still orbits. (V)";
    for (mode, text, tip) in [(Pointer::View, "View", tip_v), (Pointer::Draw, "Draw", tip_d)] {
        let sel = p == mode;
        let (fill, color) = if sel { (theme::ACCENT, theme::ON_ACCENT) } else { (theme::HOVER, theme::TEXT) };
        let b = egui::Button::new(RichText::new(text).strong().color(color)).fill(fill).corner_radius(4).min_size(vec2(56.0, 26.0));
        if ui.add(b).on_hover_text(tip).clicked() {
            p = mode;
        }
    }
    if p != st.ui.pointer {
        set_pointer(st, p);
    }
}

/// Changes who has the left button. Drawing with no sketch open starts by choosing where.
pub fn set_pointer(st: &mut Studio, p: Pointer) {
    st.ui.pointer = p;
    if st.ui.m3.flat {
        return;
    }
    if p == Pointer::Draw && st.ui.m3.active.is_none() && !matches!(st.ui.m3.picking, Picking::Feed(_) | Picking::Probe | Picking::Measure(_) | Picking::Angle(_) | Picking::Place(_)) {
        // Back into the sketch last worked on if one's selected; otherwise ask where to draw.
        if let Some(id) = st.ui.m3.sel.feature.filter(|id| st.project.model.sketch(*id).is_some()) {
            open_sketch(st, id);
        } else {
            ask_for_model_tab(st);
            st.ui.m3.picking = Picking::SketchPlane;
            st.ui.pointer = Pointer::View;
            st.ui.m3.note = Some("Where do you want to draw? Click a flat face of the model, or choose Sketch on the ribbon to draw on the work plane.".into());
        }
    }
    st.ui.m3.picks.clear();
}

/// The nearest feature edge of a body to the pointer, as the two faces it lies between.
fn edge_under(p: &raster::Part, pj: &Projector, at: Pos2) -> Option<([FaceId; 2], [P3; 2])> {
    let mut best: Option<(f32, usize)> = None;
    for (i, e) in p.edges.iter().enumerate() {
        let d = seg_dist(at, pj.screen(e[0]), pj.screen(e[1]));
        if d < SNAP_PX && best.is_none_or(|b| d < b.0) {
            best = Some((d, i));
        }
    }
    let i = best?.1;
    let faces = p.edge_faces[i];
    (faces[0] != faces[1]).then_some((faces, p.edges[i]))
}

/// The tools on the canvas, drawn over the model. This takes clicks in both View and Draw: in View
/// a click selects and a drag belongs to the camera.
pub fn canvas(ui: &mut egui::Ui, st: &mut Studio, area: Rect, view: Rect, resp: &Response, pj: &Projector) {
    if !ui.ctx().input(|i| i.pointer.primary_down()) {
        st.ui.m3.drag = None;
    }
    if st.ui.page != Page::Draw || !matches!(st.ui.view, ViewMode::Model | ViewMode::Mesh) {
        return;
    }
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(area);
    ensure_grid(st);
    let pointer = ctx.input(|i| i.pointer.latest_pos()).filter(|_| resp.contains_pointer() || resp.dragged());
    let vars = st.project.vars().unwrap_or_default();

    keep_in_view(st);
    keys(st, &ctx);

    if let Some(id) = st.ui.m3.active {
        sketch_canvas(ui, st, id, &painter, area, view, resp, pj, pointer, &vars);
    } else {
        model_canvas(st, &painter, view, resp, pj, pointer);
    }

    if st.ui.m3.section_on {
        painter.text(pos2(view.left() + 120.0, view.bottom() - 54.0), Align2::LEFT_BOTTOM, "Section view: the model is cut open only on the screen", FontId::proportional(12.5), theme::WEAK);
    }
}

fn keys(st: &mut Studio, ctx: &egui::Context) {
    // A dialog is up: nothing goes through to what's behind it.
    if ctx.egui_wants_keyboard_input() || st.ui.add_var.is_some() {
        return;
    }
    let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    if key(Modifiers::COMMAND, Key::Z) {
        super::draw::undo(st);
    }
    if key(Modifiers::COMMAND, Key::Y) || key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) {
        super::draw::redo(st);
    }
    if key(Modifiers::NONE, Key::V) {
        let next = if st.ui.pointer == Pointer::View { Pointer::Draw } else { Pointer::View };
        set_pointer(st, next);
    }
    if key(Modifiers::NONE, Key::F) {
        fit(st);
    }
    if key(Modifiers::NONE, Key::Escape) {
        // One step back at a time: what's being drawn, the tool, the sketch.
        if st.ui.m3.dim_edit.is_some() {
            st.ui.m3.dim_edit = None;
        } else if !st.ui.m3.picks.is_empty() {
            finish_tool(st);
        } else if st.ui.m3.active.is_some() && st.ui.m3.tool != SkTool::Select {
            st.ui.m3.tool = SkTool::Select;
        } else if st.ui.m3.active.is_some() {
            exit_sketch(st);
        } else if st.ui.m3.picking != Picking::Nothing {
            st.ui.m3.picking = Picking::Nothing;
            st.ui.pointer = Pointer::View;
            st.ui.m3.note = None;
        } else {
            st.ui.m3.sel = Selection::default();
        }
    }
    if st.ui.m3.active.is_some() {
        for (k, t) in [(Key::L, SkTool::Line), (Key::R, SkTool::Rect), (Key::C, SkTool::Circle), (Key::A, SkTool::Arc), (Key::P, SkTool::Point), (Key::D, SkTool::Dimension), (Key::S, SkTool::Select)] {
            if key(Modifiers::NONE, k) {
                st.ui.m3.tool = t;
                st.ui.m3.picks.clear();
                st.ui.pointer = Pointer::Draw;
            }
        }
        if key(Modifiers::NONE, Key::Enter) {
            finish_tool(st);
        }
        if key(Modifiers::NONE, Key::Delete) || key(Modifiers::NONE, Key::Backspace) {
            let (points, curves) = (std::mem::take(&mut st.ui.m3.sel.points), std::mem::take(&mut st.ui.m3.sel.curves));
            if !points.is_empty() || !curves.is_empty() {
                super::draw::remember(st);
                with_sketch(st, |s, _| {
                    for c in curves {
                        s.remove_curve(c);
                    }
                    for p in points {
                        s.remove_point(p);
                    }
                });
            }
        }
    } else if let Picking::Place(_) = st.ui.m3.picking {
        // A line of points ends on Enter. Anything else is finished by its last click.
        if key(Modifiers::NONE, Key::Enter) {
            super::place::finish_polyline(st);
        }
    } else {
        if key(Modifiers::COMMAND, Key::C) {
            run_cmd(st, Cmd::Copy);
        }
        if key(Modifiers::COMMAND, Key::X) {
            run_cmd(st, Cmd::ClipCut);
        }
        if key(Modifiers::COMMAND, Key::V) {
            run_cmd(st, Cmd::Paste);
        }
        if key(Modifiers::COMMAND, Key::A) {
            select_all_bodies(st);
        }
        forget_stale_selection(st);
        let chosen = st.ui.m3.sel.features();
        if !chosen.is_empty() && key(Modifiers::NONE, Key::Delete) {
            delete_selected(st);
        }
    }
}

fn select_all_bodies(st: &mut Studio) {
    let bodies: Vec<Picked> = st.ui.built.bodies.iter().map(|b| Picked { feature: b.last, body: Some(b.id) }).collect();
    st.ui.m3.sel = Selection::default();
    for p in bodies {
        toggle_pick(st, p);
    }
}

/// Every step in the tree by id and name, to tell if this is still the model a selection was made in.
fn steps_now(st: &Studio) -> Vec<(u32, String)> {
    st.project.model.features.iter().map(|f| (f.id, f.name.clone())).collect()
}

pub(super) fn toggle_pick(st: &mut Studio, p: Picked) {
    st.ui.m3.sel.toggle(p);
    st.ui.m3.picked_in = steps_now(st);
}

/// Several things selected are held by id, and another project opened meanwhile can have the same
/// ids for other things. Then all of it is let go, so nothing is combined or deleted that nobody picked.
pub(super) fn forget_stale_selection(st: &mut Studio) {
    if !st.ui.m3.sel.more.is_empty() && st.ui.m3.picked_in != steps_now(st) {
        st.ui.m3.sel = Selection::default();
    }
}

#[allow(clippy::too_many_arguments)]
fn sketch_canvas(ui: &mut egui::Ui, st: &mut Studio, id: u32, painter: &egui::Painter, _area: Rect, view: Rect, resp: &Response, pj: &Projector, pointer: Option<Pos2>, vars: &Vars) {
    let Some(s) = st.project.model.sketch(id).cloned() else {
        st.ui.m3.active = None;
        return;
    };
    let Some(frame) = st.ui.built.sketches.get(&id).map(|b| b.frame).or_else(|| {
        let meshes: Vec<&antenna_core::modeler::mesh::Mesh> = st.ui.built.bodies.iter().map(|b| b.mesh.as_ref()).collect();
        plane_of(st, id).frame(vars, &meshes).ok()
    }) else {
        return;
    };
    let solved = s.solve(vars);
    let drawing = st.ui.pointer == Pointer::Draw;
    let tool = st.ui.m3.tool;
    let alt = ui.input(|i| i.modifiers.alt);
    let raw = pointer.and_then(|p| screen_to_plane(&frame, pj, p));

    // Where the pointer is, snapped to whatever's there.
    let mut guide = None;
    let (mut pick, mut curve) = (None, None);
    if let (Some(q), Some(at)) = (raw, pointer) {
        let (p, c) = sketch_pick(&s, &solved, &frame, pj, at, q, &st.ui.m3, vars);
        let from = st.ui.m3.picks.last().filter(|_| tool == SkTool::Line).map(|x| x.at(&solved));
        let (p, g) = infer(p, from, &frame, pj);
        guide = g;
        pick = Some(p);
        curve = c;
    }
    st.ui.m3.hover = pick;
    st.ui.m3.hover_curve = curve.filter(|_| matches!(pick, Some(SkPick::On(..) | SkPick::Mid(..))));

    // A dimension's text takes the click, whatever the tool.
    let on_dim = pointer.and_then(|at| dim_at(&s, &solved, &frame, pj, at, vars, painter));
    if resp.clicked()
        && let Some((index, at)) = on_dim
        && !alt
    {
        let text = s.constraints.get(index).and_then(|c| c.value()).unwrap_or("").to_string();
        st.ui.m3.dim_edit = Some(DimEdit { waiting: false, index, text, at, fresh: true });
    } else if drawing && !alt {
        if let Some(p) = pick {
            sketch_input(st, &s, &solved, resp, p, tool, pointer, id, vars, pj, &frame);
        }
    }

    let s = st.project.model.sketch(id).cloned().unwrap_or(s);
    let solved = s.solve(vars);
    paint_sketch(painter, &s, &solved, &frame, pj, &st.ui.m3, vars);

    // What's being drawn right now.
    if drawing
        && let Some(p) = pick
    {
        let q = p.at(&solved);
        let scr = |x: P2| plane_to_screen(&frame, pj, x);
        let live = Stroke::new(1.6, theme::ACCENT);
        let picks = st.ui.m3.picks.clone();
        let mut size = None;
        match tool {
            SkTool::Line | SkTool::Spline => {
                if let Some(last) = picks.last() {
                    let a = last.at(&solved);
                    painter.line_segment([scr(a), scr(q)], live);
                    size = Some(format!("{:.2} mm", (q[0] - a[0]).hypot(q[1] - a[1]) * 1e3));
                }
                if tool == SkTool::Spline {
                    for w in picks.windows(2) {
                        painter.line_segment([scr(w[0].at(&solved)), scr(w[1].at(&solved))], live);
                    }
                }
            }
            SkTool::Rect | SkTool::CentreRect => {
                if let Some(first) = picks.first() {
                    let a = first.at(&solved);
                    let (lo, hi) = if tool == SkTool::CentreRect {
                        let d = [(q[0] - a[0]).abs(), (q[1] - a[1]).abs()];
                        ([a[0] - d[0], a[1] - d[1]], [a[0] + d[0], a[1] + d[1]])
                    } else {
                        (a, q)
                    };
                    let c = [lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]];
                    for k in 0..4 {
                        painter.line_segment([scr(c[k]), scr(c[(k + 1) % 4])], live);
                    }
                    size = Some(format!("{:.2} × {:.2} mm", (hi[0] - lo[0]).abs() * 1e3, (hi[1] - lo[1]).abs() * 1e3));
                }
            }
            SkTool::Circle => {
                if let Some(first) = picks.first() {
                    let a = first.at(&solved);
                    let r = (q[0] - a[0]).hypot(q[1] - a[1]);
                    let n = 64;
                    for k in 0..n {
                        let (t0, t1) = (std::f64::consts::TAU * k as f64 / n as f64, std::f64::consts::TAU * (k + 1) as f64 / n as f64);
                        painter.line_segment([scr([a[0] + r * t0.cos(), a[1] + r * t0.sin()]), scr([a[0] + r * t1.cos(), a[1] + r * t1.sin()])], live);
                    }
                    size = Some(format!("radius {:.2} mm", r * 1e3));
                }
            }
            SkTool::Arc => {
                if let [c0, a0] = picks.as_slice() {
                    let (c, a) = (c0.at(&solved), a0.at(&solved));
                    let r = (a[0] - c[0]).hypot(a[1] - c[1]);
                    let (t0, mut t1) = ((a[1] - c[1]).atan2(a[0] - c[0]), (q[1] - c[1]).atan2(q[0] - c[0]));
                    if t1 <= t0 {
                        t1 += std::f64::consts::TAU;
                    }
                    let n = 48;
                    for k in 0..n {
                        let (u, v) = (t0 + (t1 - t0) * k as f64 / n as f64, t0 + (t1 - t0) * (k + 1) as f64 / n as f64);
                        painter.line_segment([scr([c[0] + r * u.cos(), c[1] + r * u.sin()]), scr([c[0] + r * v.cos(), c[1] + r * v.sin()])], live);
                    }
                    size = Some(format!("radius {:.2} mm", r * 1e3));
                } else if let [c0] = picks.as_slice() {
                    painter.line_segment([scr(c0.at(&solved)), scr(q)], Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)));
                }
            }
            _ => {}
        }
        for x in &picks {
            painter.circle_filled(scr(x.at(&solved)), 3.5, theme::ACCENT);
        }
        if let Some(g) = guide {
            let a = scr(g[0]);
            let b = scr(g[1]);
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_rgb(60, 170, 90)));
        }
        let on_thing = !matches!(p, SkPick::Free(_));
        let col = if on_thing { theme::SUCCESS } else { theme::ACCENT };
        let at = scr(q);
        if tool != SkTool::Select {
            painter.line_segment([at - vec2(7.0, 0.0), at + vec2(7.0, 0.0)], Stroke::new(1.0, col));
            painter.line_segment([at - vec2(0.0, 7.0), at + vec2(0.0, 7.0)], Stroke::new(1.0, col));
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if let SkPick::Mid(..) = p {
            painter.circle_stroke(at, 5.0, Stroke::new(1.4, theme::SUCCESS));
        }
        let mut read = format!("{:.2}, {:.2} mm", q[0] * 1e3, q[1] * 1e3);
        if let Some(sz) = size {
            read = format!("{sz}   ·   {read}");
        }
        if let Some(ptr) = pointer {
            let g = painter.layout_no_wrap(read, FontId::proportional(12.0), theme::TEXT);
            let bg = Rect::from_min_size(ptr + vec2(14.0, 14.0), g.size() + vec2(10.0, 6.0));
            painter.rect_filled(bg, 4.0, Color32::from_white_alpha(235));
            painter.galley(bg.min + vec2(5.0, 3.0), g, theme::TEXT);
        }
    }

    // The field for the dimension being typed, right where its text was.
    if let Some(mut edit) = st.ui.m3.dim_edit.take() {
        let table = VarTable::of(&st.project);
        let mut done = false;
        let mut cancel = false;
        egui::Area::new(egui::Id::new("model3d_dim_edit")).order(egui::Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(edit.at).show(ui.ctx(), |ui| {
            theme::floating_frame().inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let r = expr_line(ui, ("model3d_dim", edit.index), &mut edit.text, &table, 118.0);
                    if edit.fresh {
                        r.request_focus();
                        let mut state = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                        let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(edit.text.chars().count()));
                        state.cursor.set_char_range(Some(all));
                        state.store(ui.ctx(), r.id);
                        edit.fresh = false;
                    }
                    if r.lost_focus() {
                        done = true;
                    }
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        cancel = true;
                    }
                });
            });
        });
        if cancel {
            // Left as it was.
        } else if (done || edit.waiting) && super::params::asking(ui.ctx(), st) {
            edit.waiting = true;
            st.ui.m3.dim_edit = Some(edit);
        } else if done || edit.waiting {
            let (index, text) = (edit.index, edit.text.clone());
            let degrees = matches!(s.constraints.get(index), Some(Constraint::Angle { .. }));
            let written = normalised(&text, degrees);
            let works = if degrees { antenna_core::modeler::angle(&written, vars) } else { antenna_core::modeler::length(&written, vars) };
            match works {
                Ok(_) => {
                    super::draw::remember(st);
                    with_sketch(st, |s, _| {
                        if let Some(v) = s.constraints.get_mut(index).and_then(|c| c.value_mut()) {
                            *v = written;
                        }
                    });
                    st.ui.m3.note = None;
                }
                Err(e) => st.ui.m3.note = Some(format!("That dimension stays as it was: {e:#}")),
            }
        } else {
            st.ui.m3.dim_edit = Some(edit);
        }
    }

    let (hint, _) = sketch_hint(st, &solved);
    painter.text(pos2(view.center().x, view.bottom() - 34.0), Align2::CENTER_BOTTOM, hint, FontId::proportional(13.0), theme::WEAK);
}

/// A plain number gets a unit, so it means what it showed.
fn normalised(text: &str, degrees: bool) -> String {
    let t = text.trim();
    if t.parse::<f64>().is_ok() {
        if degrees { format!("{t}deg") } else { format!("{t}mm") }
    } else {
        t.to_string()
    }
}

fn sketch_hint(st: &Studio, solved: &Solved) -> (String, bool) {
    let m = &st.ui.m3;
    if st.ui.pointer == Pointer::View {
        return ("View: left-drag orbits. Press V or choose Draw to carry on drawing.".to_string(), false);
    }
    let begun = !m.picks.is_empty();
    let text = match m.tool {
        SkTool::Select => "Drag points and curves: dimensions and constraints hold. Click a dimension to type it. Del removes what's selected.",
        SkTool::Line if begun => "Click the next point. Near level or upright, it snaps. Click where it started to close, or Enter to stop.",
        SkTool::Line => "Click where the line starts.",
        SkTool::Rect if begun => "Click the opposite corner.",
        SkTool::Rect => "Click one corner, or drag from corner to corner.",
        SkTool::CentreRect if begun => "Click a corner.",
        SkTool::CentreRect => "Click the centre.",
        SkTool::Circle if begun => "Click the rim.",
        SkTool::Circle => "Click the centre.",
        SkTool::Arc if m.picks.len() == 1 => "Click where the arc starts.",
        SkTool::Arc if m.picks.len() == 2 => "Click where it ends, counter-clockwise from the start.",
        SkTool::Arc => "Click the arc's centre.",
        SkTool::Spline if begun => "Click the next point. Enter ends it.",
        SkTool::Spline => "Click the first point.",
        SkTool::Point => "Click where the point goes.",
        SkTool::Dimension if begun => "Click the second thing, or the same one again for its own size.",
        SkTool::Dimension => "Click a line, a circle or a point.",
        SkTool::Round => "Click the corner to round.",
    };
    let drawn = st.ui.m3.active.and_then(|id| st.project.model.sketch(id)).is_some_and(|s| !s.points.is_empty() || !s.curves.is_empty());
    if !drawn {
        return (text.to_string(), false);
    }
    let status = if !solved.ok() { "Constraints at odds".to_string() } else if solved.free == 0 { "Fully defined".to_string() } else { format!("{} ways still free", solved.free) };
    (format!("{text}   ·   {status}"), false)
}

#[allow(clippy::too_many_arguments)]
fn sketch_input(st: &mut Studio, s: &Sketch, solved: &Solved, resp: &Response, pick: SkPick, tool: SkTool, pointer: Option<Pos2>, _id: u32, vars: &Vars, pj: &Projector, frame: &Frame) {
    let primary = egui::PointerButton::Primary;
    match tool {
        SkTool::Select => {
            if resp.drag_started_by(primary) {
                match pick {
                    SkPick::Point(p) if p != ORIGIN => {
                        super::draw::remember(st);
                        st.ui.m3.drag = Some(Drag::Point(p));
                    }
                    SkPick::On(c, q) | SkPick::Mid(c, q) => {
                        if let Some(curve) = s.curve(c) {
                            let held: Vec<(u32, P2)> = sk::curve_points(&curve.kind).into_iter().map(|p| (p, solved.at(p))).collect();
                            super::draw::remember(st);
                            st.ui.m3.drag = Some(Drag::Curve(held, q));
                        }
                    }
                    _ => {}
                }
            }
            if resp.dragged_by(primary)
                && let (Some(d), Some(at)) = (st.ui.m3.drag.clone(), pointer)
                && let Some(q) = screen_to_plane(frame, pj, at)
            {
                let q = if st.ui.m3.snap && !matches!(pick, SkPick::Point(_)) {
                    let step = (st.ui.m3.grid_mm * 1e-3).max(1e-6);
                    [(q[0] / step).round() * step, (q[1] / step).round() * step]
                } else {
                    q
                };
                let held: Vec<(u32, P2)> = match &d {
                    Drag::Point(p) => vec![(*p, q)],
                    Drag::Curve(list, from) => list.iter().map(|(p, was)| (*p, [was[0] + q[0] - from[0], was[1] + q[1] - from[1]])).collect(),
                };
                if let Some(active) = st.ui.m3.active
                    && let Some(sketch) = st.project.model.sketch_mut(active)
                {
                    let got = sketch.solve_with(vars, &held);
                    sketch.keep(&got);
                }
                resp.ctx.request_repaint();
            }
            if resp.drag_stopped_by(primary) {
                st.ui.m3.drag = None;
            }
            if resp.clicked() {
                let extend = resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                if !extend {
                    st.ui.m3.sel.points.clear();
                    st.ui.m3.sel.curves.clear();
                }
                let toggle = |list: &mut Vec<u32>, x: u32| {
                    if let Some(i) = list.iter().position(|v| *v == x) {
                        list.remove(i);
                    } else {
                        list.push(x);
                    }
                };
                match pick {
                    SkPick::Point(p) => toggle(&mut st.ui.m3.sel.points, p),
                    SkPick::On(c, _) | SkPick::Mid(c, _) => toggle(&mut st.ui.m3.sel.curves, c),
                    SkPick::Free(_) => {}
                }
            }
        }
        SkTool::Dimension => {
            if resp.clicked() {
                if matches!(pick, SkPick::Free(_)) {
                    st.ui.m3.picks.clear();
                    return;
                }
                let at = pointer.unwrap_or(resp.rect.center());
                if let Some(first) = st.ui.m3.picks.first().copied() {
                    // The same curve twice is its own size; a point needs its partner.
                    let same = match (first, pick) {
                        (SkPick::On(a, _) | SkPick::Mid(a, _), SkPick::On(b, _) | SkPick::Mid(b, _)) => a == b,
                        _ => false,
                    };
                    make_dimension(st, first, if same { None } else { Some(pick) }, at);
                } else {
                    match pick {
                        SkPick::Point(_) => st.ui.m3.picks.push(pick),
                        _ => {
                            // A line or circle gets its size at once; a second click on another line
                            // would make an angle, so that goes through the panel's Angle.
                            make_dimension(st, pick, None, at);
                        }
                    }
                }
            }
        }
        SkTool::Round => {
            if resp.clicked()
                && let SkPick::Point(p) = pick
            {
                super::draw::remember(st);
                let r = st.ui.m3.round_radius.clone();
                let mut failed = None;
                with_sketch(st, |s, v| {
                    if let Err(e) = s.fillet_corner(p, &r, v) {
                        failed = Some(format!("{e:#}"));
                    }
                });
                st.ui.m3.note = failed;
            }
        }
        _ => {
            // Rectangles and circles can be dragged out as well as clicked.
            let drag_tool = matches!(tool, SkTool::Rect | SkTool::CentreRect | SkTool::Circle);
            if resp.clicked() {
                tool_click(st, pick);
            } else if resp.double_clicked() && matches!(tool, SkTool::Line | SkTool::Spline) {
                finish_tool(st);
            } else if drag_tool && resp.drag_started_by(primary) && st.ui.m3.picks.is_empty() {
                let start = resp.ctx.input(|i| i.pointer.press_origin()).and_then(|o| screen_to_plane(frame, pj, o));
                let first = start.map_or(pick, |q| sketch_pick(s, solved, frame, pj, resp.ctx.input(|i| i.pointer.press_origin()).unwrap_or_default(), q, &st.ui.m3, vars).0);
                st.ui.m3.picks = vec![first];
            } else if drag_tool && resp.drag_stopped_by(primary) && !st.ui.m3.picks.is_empty() {
                tool_click(st, pick);
            }
        }
    }
}

fn model_canvas(st: &mut Studio, painter: &egui::Painter, view: Rect, resp: &Response, pj: &Projector, pointer: Option<Pos2>) {
    // What's under the pointer, from the depth buffer.
    let hit = pointer.filter(|_| !resp.dragged()).and_then(|at| st.ui.raster.at(at).map(|h| (at, h)));
    st.ui.m3.hover_part = hit.map(|(_, h)| h.part);
    st.ui.m3.hover_face = hit.and_then(|(_, h)| st.ui.parts.get(h.part).filter(|p| p.body.is_some()).map(|p| p.mesh.face[h.tri]));
    st.ui.m3.hover_edge = None;
    let picking = st.ui.m3.picking.clone();
    let mut hover_edge_line = None;
    if let (Picking::Edges(_), Some(at)) = (&picking, pointer) {
        let part = hit.map(|(_, h)| h.part);
        if let Some(i) = part
            && let Some(p) = st.ui.parts.get(i)
            && let Some((faces, line)) = edge_under(p, pj, at)
        {
            st.ui.m3.hover_edge = Some(faces);
            hover_edge_line = Some(line);
        }
    }
    if let Some(l) = hover_edge_line {
        painter.line_segment([pj.screen(l[0]), pj.screen(l[1])], Stroke::new(3.0, Color32::from_rgb(240, 130, 20)));
    }
    // Edges already picked for this fillet.
    if let Picking::Edges(id) = &picking
        && let Some(Kind::Fillet { edges, .. }) = st.project.model.feature(*id).map(|f| &f.kind)
    {
        for p in &st.ui.parts {
            for (e, faces) in p.edges.iter().zip(&p.edge_faces) {
                if edges.iter().any(|x| same_edge(*x, *faces)) {
                    painter.line_segment([pj.screen(e[0]), pj.screen(e[1])], Stroke::new(3.0, theme::ACCENT));
                }
            }
        }
    }

    // Before the click: it takes what the numbers say for where the pointer is now.
    super::place::track(st, painter, pj, pointer);

    if resp.clicked() {
        match picking {
            Picking::SketchPlane => {
                if let Some(face) = st.ui.m3.hover_face {
                    let vars = st.project.vars().unwrap_or_default();
                    let meshes: Vec<&antenna_core::modeler::mesh::Mesh> = st.ui.built.bodies.iter().map(|b| b.mesh.as_ref()).collect();
                    match (PlaneRef::Face { face, offset: "0".into() }).frame(&vars, &meshes) {
                        Ok(_) => {
                            st.ui.m3.picking = Picking::Nothing;
                            st.ui.m3.note = None;
                            new_sketch(st, PlaneRef::Face { face, offset: "0".into() });
                        }
                        Err(_) => st.ui.m3.note = Some("That face isn't flat, so it can't hold a sketch. Pick a flat one, or Top, Front or Right.".into()),
                    }
                }
            }
            Picking::Edges(id) => {
                if let Some(faces) = st.ui.m3.hover_edge {
                    super::draw::remember(st);
                    if let Some(Kind::Fillet { edges, .. }) = st.project.model.feature_mut(id).map(|f| &mut f.kind) {
                        if let Some(i) = edges.iter().position(|x| same_edge(*x, faces)) {
                            edges.remove(i);
                        } else {
                            edges.push(faces);
                        }
                    }
                }
            }
            Picking::OpenFaces(id) => {
                if let Some(face) = st.ui.m3.hover_face {
                    super::draw::remember(st);
                    if let Some(Kind::Shell { open, .. }) = st.project.model.feature_mut(id).map(|f| &mut f.kind) {
                        if let Some(i) = open.iter().position(|x| *x == face) {
                            open.remove(i);
                        } else {
                            open.push(face);
                        }
                    }
                }
            }
            Picking::Feed(mut ends) => {
                if let Some(at) = pointer {
                    match feed_snap(st, pj, at, hit.map(|h| h.1)) {
                        Some(end) => {
                            ends.push(end);
                            if ends.len() == 2 {
                                add_feed(st, &ends);
                                st.ui.m3.picking = Picking::Nothing;
                                st.ui.pointer = Pointer::View;
                            } else {
                                st.ui.m3.picking = Picking::Feed(ends);
                            }
                        }
                        None => st.ui.m3.note = Some("A feed goes on the model or on a sketch point, and there's nothing there.".into()),
                    }
                }
            }
            Picking::Probe => {
                if let Some(at) = pointer
                    && let Some((sketch, point, _)) = nearest_sketch_point(st, pj, at)
                {
                    add_feature(st, Kind::Probe { sketch, point, width: String::new(), z0: "50".into() });
                    st.ui.m3.picking = Picking::Nothing;
                    st.ui.pointer = Pointer::View;
                }
            }
            Picking::Measure(mut pts) => {
                if let Some((q, _)) = pointer.and_then(|at| feed_snap(st, pj, at, hit.map(|h| h.1))) {
                    if pts.len() == 2 {
                        pts.clear();
                    }
                    pts.push(q);
                    st.ui.m3.picking = Picking::Measure(pts);
                }
            }
            Picking::Angle(mut pts) => {
                if let Some((q, _)) = pointer.and_then(|at| feed_snap(st, pj, at, hit.map(|h| h.1))) {
                    if pts.len() == 3 {
                        pts.clear();
                    }
                    pts.push(q);
                    st.ui.m3.picking = Picking::Angle(pts);
                }
            }
            Picking::Place(_) => super::place::advance(st),
            Picking::Nothing => {
                // A click selects; a click on nothing clears. Ctrl or Shift adds to what's selected.
                let extend = resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.shift);
                st.ui.m3.sel.step = false;
                match hit {
                    Some((_, h)) => {
                        let body = st.ui.parts.get(h.part).and_then(|p| p.body);
                        let face = st.ui.parts.get(h.part).map(|p| p.mesh.face[h.tri]);
                        let feature = body.and_then(|b| st.ui.built.bodies.iter().find(|x| x.id == b).map(|x| x.last));
                        // A part from a template or the flat tools is in the tree by name.
                        st.ui.tree.pick = match st.ui.parts.get(h.part).and_then(|p| p.solid.clone()) {
                            Some(name) if body.is_none() => {
                                st.ui.draw.selected = st.project.extra.iter().position(|e| e.name == name);
                                super::project_tree::Pick::Part(name)
                            }
                            _ => super::project_tree::Pick::Nothing,
                        };
                        if !extend {
                            st.ui.m3.sel = Selection { feature, body, face: face.filter(|_| body.is_some()), ..Default::default() };
                        } else if let Some(feature) = feature {
                            toggle_pick(st, Picked { feature, body });
                        }
                    }
                    None if extend => {}
                    None => {
                        st.ui.m3.sel = Selection::default();
                        st.ui.tree.pick = super::project_tree::Pick::Nothing;
                    }
                }
            }
        }
    }
    if resp.double_clicked()
        && matches!(st.ui.m3.picking, Picking::Nothing)
        && let Some(face) = st.ui.m3.hover_face
    {
        // Double-clicking a flat face starts a sketch on it, the shortest way to build on the model.
        let vars = st.project.vars().unwrap_or_default();
        let meshes: Vec<&antenna_core::modeler::mesh::Mesh> = st.ui.built.bodies.iter().map(|b| b.mesh.as_ref()).collect();
        if (PlaneRef::Face { face, offset: "0".into() }).frame(&vars, &meshes).is_ok() {
            new_sketch(st, PlaneRef::Face { face, offset: "0".into() });
        }
    }

    // Marks for what's being picked.
    if let Picking::Feed(ends) = &st.ui.m3.picking {
        for (q, _) in ends {
            painter.circle(pj.screen(*q), 4.5, theme::DANGER, Stroke::new(1.0, Color32::WHITE));
        }
    }
    if matches!(st.ui.m3.picking, Picking::Feed(_) | Picking::Probe) {
        for s in st.project.model.features.iter().filter_map(|f| if let Kind::Sketch { sketch, .. } = &f.kind { Some((f.id, sketch)) } else { None }) {
            if let Some(b) = st.ui.built.sketches.get(&s.0) {
                for p in &s.1.points {
                    let at = pj.screen(b.frame.at(b.solved.at(p.id), 0.0));
                    painter.circle(at, 3.0, Color32::from_rgb(240, 130, 20), Stroke::new(1.0, Color32::WHITE));
                }
            }
        }
    }
    if let Picking::Measure(pts) = &st.ui.m3.picking {
        for q in pts {
            painter.circle(pj.screen(*q), 4.0, theme::ACCENT, Stroke::new(1.0, Color32::WHITE));
        }
        if let [a, b] = pts.as_slice() {
            let (x, y) = (pj.screen(*a), pj.screen(*b));
            painter.line_segment([x, y], Stroke::new(1.5, theme::ACCENT));
            let d = sub(*b, *a);
            let text = measure_text(d);
            let g = painter.layout_no_wrap(text, FontId::proportional(12.5), theme::TEXT);
            let at = pos2((x.x + y.x) / 2.0, (x.y + y.y) / 2.0) + vec2(8.0, -22.0);
            let bg = Rect::from_min_size(at, g.size() + vec2(10.0, 6.0));
            painter.rect_filled(bg, 4.0, Color32::from_white_alpha(240));
            painter.galley(bg.min + vec2(5.0, 3.0), g, theme::TEXT);
        }
    }
    if let Picking::Angle(pts) = &st.ui.m3.picking {
        for q in pts {
            painter.circle(pj.screen(*q), 4.0, theme::ACCENT, Stroke::new(1.0, Color32::WHITE));
        }
        for w in pts.windows(2) {
            painter.line_segment([pj.screen(w[0]), pj.screen(w[1])], Stroke::new(1.5, theme::ACCENT));
        }
        if let [a, b, c] = pts.as_slice() {
            let degrees = angle_between(*a, *b, *c);
            let g = painter.layout_no_wrap(format!("{degrees:.2}°"), FontId::proportional(12.5), theme::TEXT);
            let at = pj.screen(*b) + vec2(10.0, -24.0);
            let bg = Rect::from_min_size(at, g.size() + vec2(10.0, 6.0));
            painter.rect_filled(bg, 4.0, Color32::from_white_alpha(240));
            painter.galley(bg.min + vec2(5.0, 3.0), g, theme::TEXT);
        }
    }
    // Where a coordinate system other than the global one stands.
    if let Some(x) = st.ui.m3.coord.and_then(|id| st.project.model.coord(id)).and_then(|c| c.xform(&st.project.vars().unwrap_or_default()).ok()) {
        let o = pj.screen(x.t);
        for (k, colour) in [Color32::from_rgb(214, 48, 49), Color32::from_rgb(39, 174, 96), Color32::from_rgb(45, 110, 240)].iter().enumerate() {
            let mut d = [0.0; 3];
            d[k] = 1.0;
            let along = pj.screen(add(x.t, x.apply_dir(d))) - o;
            let reach = along.normalized() * 26.0 * (along.length() / pj.screen(add(x.t, [0.0, 0.0, 0.0])).distance(o).max(along.length()).max(1.0)).clamp(0.2, 1.0);
            painter.line_segment([o, o + reach], Stroke::new(2.0, *colour));
        }
    }
    let hint = match &st.ui.m3.picking {
        Picking::SketchPlane => "Click a flat face to sketch on it, or choose Sketch on the ribbon to draw on the work plane.",
        Picking::Edges(_) => "Click edges to round them. Press Esc when you're done.",
        Picking::OpenFaces(_) => "Click faces to leave open. Press Esc when you're done.",
        Picking::Feed(e) if e.is_empty() => "Click where the feed starts.",
        Picking::Feed(_) => "Click where it ends. It's pulled onto x, y or z.",
        Picking::Probe => "Click a sketch point.",
        Picking::Measure(_) => "Click two points to measure between them.",
        Picking::Angle(_) => "Click one end, the corner, then the other end.",
        Picking::Place(_) => "",
        Picking::Nothing if st.ui.all_parts.is_empty() && st.project.model.is_empty() => "Pick a shape on the ribbon, such as Box or Rectangle, and click on the grid. Or start from a template under Design.",
        Picking::Nothing => "Drag to orbit, right-drag to pan, scroll to zoom. Ctrl+click selects several. Double-click a flat face to sketch on it.",
    };
    painter.text(pos2(view.center().x, view.bottom() - 34.0), Align2::CENTER_BOTTOM, hint, FontId::proportional(13.0), theme::WEAK);
}

fn same_edge(a: [FaceId; 2], b: [FaceId; 2]) -> bool {
    (a[0] == b[0] && a[1] == b[1]) || (a[0] == b[1] && a[1] == b[0])
}

fn nearest_sketch_point(st: &Studio, pj: &Projector, at: Pos2) -> Option<(u32, u32, P3)> {
    let mut best: Option<(f32, u32, u32, P3)> = None;
    for f in &st.project.model.features {
        let Kind::Sketch { sketch, .. } = &f.kind else { continue };
        let Some(b) = st.ui.built.sketches.get(&f.id) else { continue };
        for p in &sketch.points {
            let q = b.frame.at(b.solved.at(p.id), 0.0);
            let d = pj.screen(q).distance(at);
            if d < SNAP_PX * 1.5 && best.is_none_or(|x| d < x.0) {
                best = Some((d, f.id, p.id, q));
            }
        }
    }
    best.map(|b| (b.1, b.2, b.3))
}

/// Where a click for a feed or a measurement lands: a sketch point, a corner of the model, or the
/// surface under the pointer.
fn feed_snap(st: &Studio, pj: &Projector, at: Pos2, hit: Option<raster::Hit>) -> Option<(P3, Option<(u32, u32)>)> {
    if let Some((s, p, q)) = nearest_sketch_point(st, pj, at) {
        return Some((q, Some((s, p))));
    }
    if let Some(h) = hit
        && let Some(part) = st.ui.parts.get(h.part)
    {
        let c = part.mesh.corners(h.tri);
        let near = c.iter().copied().min_by(|x, y| pj.screen(*x).distance(at).total_cmp(&pj.screen(*y).distance(at)));
        if let Some(q) = near.filter(|q| pj.screen(*q).distance(at) < SNAP_PX * 1.5) {
            return Some((q, None));
        }
        return Some((raster::point_on(part, h.tri, pj, at), None));
    }
    None
}

/// The angle at `corner`, in degrees, between the lines to the other two.
fn angle_between(a: P3, corner: P3, c: P3) -> f64 {
    let (u, v) = (sub(a, corner), sub(c, corner));
    if norm(u) == 0.0 || norm(v) == 0.0 {
        return 0.0;
    }
    dot(unit(u), unit(v)).clamp(-1.0, 1.0).acos().to_degrees()
}

/// A measurement as it's read out, in mm. A difference of nothing has no sign.
fn measure_text(d: P3) -> String {
    let mm = |v: f64| {
        let r = (v * 1e6).round() / 1e3;
        if r == 0.0 { 0.0 } else { r }
    };
    format!("{:.3} mm   (x {:.3}, y {:.3}, z {:.3})", mm(norm(d)), mm(d[0]), mm(d[1]), mm(d[2]))
}

fn add_feed(st: &mut Studio, ends: &[(P3, Option<(u32, u32)>)]) {
    let [(a, anchor_a), (b, anchor_b)] = ends else { return };
    let d = sub(*b, *a);
    if norm(d) < 1e-9 {
        st.ui.m3.note = Some("A feed needs two different points.".into());
        return;
    }
    // Pulled onto its strongest axis: a port lies along x, y or z.
    let k = (0..3).max_by(|x, y| d[*x].abs().total_cmp(&d[*y].abs())).unwrap_or(2);
    let straight = (0..3).filter(|j| *j != k).all(|j| d[j].abs() < 1e-9);
    let tied = |sk: (u32, u32)| Anchor::Sketch { sketch: sk.0, point: sk.1, height: String::new() };
    // A step along that axis from a sketch point: wherever the point goes, the feed stays square.
    let step = |sk: (u32, u32), by: f64| {
        let mut along = ["0".to_string(), "0".to_string(), "0".to_string()];
        along[k] = mm_text(by);
        Anchor::Offset { sketch: sk.0, point: sk.1, by: along }
    };
    let (ka, kb) = match (anchor_a, anchor_b) {
        (Some(x), Some(y)) if straight => (tied(*x), tied(*y)),
        (Some(x), _) => (tied(*x), step(*x, d[k])),
        (_, Some(y)) => (step(*y, -d[k]), tied(*y)),
        (None, None) => {
            let mut end = *a;
            end[k] = b[k];
            (Anchor::Xyz { at: [mm_text(a[0]), mm_text(a[1]), mm_text(a[2])] }, Anchor::Xyz { at: [mm_text(end[0]), mm_text(end[1]), mm_text(end[2])] })
        }
    };
    add_feature(st, Kind::Feed { a: ka, b: kb, width: String::new(), z0: "50".into() });
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Unit {
    Length,
    Angle,
    Plain,
}

/// One dimension: its name, its expression, what that comes to, and a way to name it a variable.
#[allow(clippy::too_many_arguments)]
pub(super) fn field(ui: &mut egui::Ui, st: &mut Studio, salt: &str, label: &str, text: &mut String, unit: Unit, table: &VarTable, vars: &Vars) {
    // The Add variable dialog gives its answer here.
    if let Some(name) = super::params::take_named(st, salt) {
        *text = name;
    }
    let word = match unit {
        Unit::Length => "mm",
        Unit::Angle => "deg",
        Unit::Plain => "",
    };
    let name = ui.add(egui::Label::new(label).sense(Sense::click())).on_hover_text("Right-click to turn this into a variable");
    super::params::field_menu(&name, st, salt, label, text, word);
    let tied = !text.trim().is_empty() && !antenna_core::sketch::is_free(text);
    let room = (st.ui.left_w - 124.0 - if tied { 74.0 } else { 0.0 }).clamp(70.0, 280.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let line = match unit {
            Unit::Length => length_line,
            Unit::Angle => angle_line,
            Unit::Plain => expr_line,
        };
        let r = line(ui, ("model3d_field", salt), text, table, room);
        // A plain number gets its unit once typed, so it can't be taken for metres later on.
        if r.lost_focus() && unit != Unit::Plain {
            *text = normalised(text, unit == Unit::Angle);
        }
        super::params::field_menu(&r, st, salt, label, text, word);
        if tied {
            let read = match unit {
                Unit::Length => antenna_core::modeler::length(text, vars).map(|v| format!("= {} mm", (v * 1e6).round() / 1e3)),
                Unit::Angle => antenna_core::modeler::angle(text, vars).map(|v| format!("= {:.2}°", v.to_degrees())),
                Unit::Plain => antenna_core::expr::eval(text, vars).map(|v| format!("= {v:.4}")),
            };
            match read {
                Ok(t) => ui.label(RichText::new(t).size(12.0).color(theme::WEAK)),
                Err(_) => ui.label(RichText::new("doesn't work out").size(12.0).color(theme::DANGER)),
            };
        }
    });
    ui.end_row();
}

pub(super) fn point3(ui: &mut egui::Ui, st: &mut Studio, salt: &str, label: &str, q: &mut [String; 3], unit: Unit, table: &VarTable, vars: &Vars) {
    // One line that says all three, opening into a row for each.
    let id = ui.make_persistent_id(("point3_open", salt));
    let open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(true);
    let head = ui.add(egui::Label::new(format!("{} {label}", if open { icon::CARET_DOWN } else { icon::CARET_RIGHT })).sense(Sense::click()));
    if head.clicked() {
        ui.data_mut(|d| d.insert_temp(id, !open));
    }
    ui.add(egui::Label::new(RichText::new(format!("{}; {}; {}", q[0], q[1], q[2])).size(12.0).color(theme::WEAK)).truncate());
    ui.end_row();
    if open {
        for (k, axis) in ["X", "Y", "Z"].iter().enumerate() {
            field(ui, st, &format!("{salt}{k}"), &format!("    {axis}"), &mut q[k], unit, table, vars);
        }
    }
}

fn materials_combo(ui: &mut egui::Ui, salt: &str, value: &mut String, metal: &str) {
    let names: Vec<String> = material::library().into_iter().map(|m| m.name).collect();
    let shown = if value.is_empty() { format!("{metal} (project's metal)") } else { value.clone() };
    egui::ComboBox::from_id_salt(("model3d_mat", salt)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
        if ui.selectable_label(value.is_empty(), format!("{metal} (project's metal)")).clicked() {
            value.clear();
        }
        for n in &names {
            if ui.selectable_label(n == value, n).clicked() {
                *value = n.clone();
            }
        }
    });
}

fn body_combo(ui: &mut egui::Ui, salt: &str, value: &mut u32, list: &[(u32, String)]) {
    let shown = list.iter().find(|b| b.0 == *value).map_or("(gone)".to_string(), |b| b.1.clone());
    egui::ComboBox::from_id_salt(("model3d_body", salt)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
        for (id, name) in list {
            ui.selectable_value(value, *id, name);
        }
    });
}

fn bodies_checks(ui: &mut egui::Ui, value: &mut Vec<u32>, list: &[(u32, String)]) {
    for (id, name) in list {
        let mut on = value.contains(id);
        if ui.checkbox(&mut on, name).changed() {
            if on {
                value.push(*id);
            } else {
                value.retain(|x| x != id);
            }
        }
    }
}

fn sketch_combo(ui: &mut egui::Ui, salt: &str, value: &mut u32, features: &[Feature]) {
    let list: Vec<(u32, String)> = features.iter().filter(|f| matches!(f.kind, Kind::Sketch { .. })).map(|f| (f.id, f.name.clone())).collect();
    body_combo(ui, salt, value, &list);
}

/// Where a feature's result goes, what it's made of, and how fine its mesh is.
#[allow(clippy::too_many_arguments)]
fn output_ui(ui: &mut egui::Ui, st: &mut Studio, salt: &str, out: &mut Output, surface: bool, table: &VarTable, vars: &Vars, list: &[(u32, String)]) {
    ui.label("Result");
    theme::segmented(ui, &mut out.mode, &[(Mode::New, "New"), (Mode::Add, "Add"), (Mode::Cut, "Cut"), (Mode::Common, "Common")]);
    ui.end_row();
    if out.mode != Mode::New {
        ui.label("To body");
        let shown = out.target.and_then(|t| list.iter().find(|b| b.0 == t)).map_or_else(|| if out.target.is_some() { "(gone)".to_string() } else { "The latest body".to_string() }, |b| b.1.clone());
        egui::ComboBox::from_id_salt(("model3d_target", salt)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut out.target, None, "The latest body");
            for (id, name) in list {
                ui.selectable_value(&mut out.target, Some(*id), name);
            }
        });
        ui.end_row();
    } else {
        ui.label("Material");
        let metal = st.project.metal.clone();
        materials_combo(ui, salt, &mut out.material, &metal);
        ui.end_row();
    }
    if surface {
        ui.label("Surface");
        ui.checkbox(&mut out.surface, "No thickness, like a thin sheet of metal");
        ui.end_row();
    }
    if matches!(salt, "sheet" | "face") {
        field(ui, st, &format!("{salt}_thick"), "Thickness", &mut out.thick, Unit::Length, table, vars);
        let none = out.thick.trim().is_empty() || antenna_core::modeler::length(&out.thick, vars).is_ok_and(|t| t == 0.0);
        ui.label("");
        // Layered MoM needs all the metal flat in one plane, so a thickness there changes the solver.
        let note = if none {
            "None: a surface"
        } else if st.project.stack.is_empty() {
            "FDTD and FDM cut the real slab. The method of moments keeps a sheet, as it does for any thin metal"
        } else {
            "With layers, the method of moments needs flat metal, so this now goes to a time-domain solver"
        };
        ui.add(egui::Label::new(RichText::new(note).size(11.5).color(theme::WEAK)).wrap());
        ui.end_row();
    }
    ui.label("Coord. system");
    let names: Vec<(u32, String)> = st.project.model.coords.iter().map(|c| (c.id, c.name.clone())).collect();
    let shown = out.coord.map_or("Global system".to_string(), |id| names.iter().find(|c| c.0 == id).map_or("(gone)".to_string(), |c| c.1.clone()));
    egui::ComboBox::from_id_salt(("model3d_coord", salt)).selected_text(shown).width(170.0).show_ui(ui, |ui| {
        ui.selectable_value(&mut out.coord, None, "Global system");
        for (id, name) in &names {
            ui.selectable_value(&mut out.coord, Some(*id), name);
        }
    });
    ui.end_row();
    field(ui, st, &format!("{salt}_mesh"), "Mesh size", &mut out.mesh, Unit::Length, table, vars);
}

/// The dimensions of a sketch, in the grid, with no need to open it first.
pub(super) fn sketch_dims(ui: &mut egui::Ui, st: &mut Studio, id: u32) {
    let Some(s) = st.project.model.sketch(id).cloned() else { return };
    let vars = st.project.vars().unwrap_or_default();
    let table = VarTable::of(&st.project);
    let name = st.project.model.feature(id).map_or(String::new(), |f| f.name.clone());
    let count = s.constraints.iter().filter(|c| c.value().is_some()).count();
    let free = st.ui.built.sketches.get(&id).map_or(0, |b| b.solved.free);
    let mut edited = s.constraints.clone();
    egui::CollapsingHeader::new(RichText::new(format!("Dimensions of {name} ({count})")).strong()).id_salt(("sketch_dims", id)).default_open(true).show(ui, |ui| {
        if count > 0 {
            super::params::grid(ui, ("sketch_dims_grid", id), |ui| {
                // Several of one kind are told apart by number, in the order they were made.
                let mut seen: std::collections::HashMap<&str, usize> = Default::default();
                for (i, c) in edited.iter_mut().enumerate() {
                    let label = c.label();
                    let unit = if label == "Angle" { Unit::Angle } else { Unit::Length };
                    let Some(v) = c.value_mut() else { continue };
                    let n = seen.entry(label).or_insert(0);
                    *n += 1;
                    let short = match label {
                        "Horizontal distance" => "Across",
                        "Vertical distance" => "Up",
                        other => other,
                    };
                    let shown = format!("{short} {n}");
                    field(ui, st, &format!("sk{id}_c{i}"), &shown, v, unit, &table, &vars);
                }
            });
        }
        if free > 0 {
            weak(ui, format!("{free} ways it can still move: nothing holds them, so they'll stay where they are drawn."));
        }
        ui.horizontal(|ui| {
            if st.ui.m3.active != Some(id) && ui.button(theme::label(icon::NOTE_PENCIL, "Edit this sketch")).clicked() {
                open_sketch(st, id);
            }
            if free > 0 && ui.button(theme::label(icon::LOCK, "Fully define")).on_hover_text("Add the dimensions it still needs, from where things are, so every corner has a number").clicked() {
                super::draw::remember(st);
                if let Some(x) = st.project.model.sketch_mut(id) {
                    x.fully_define(&vars);
                }
                return;
            }
        });
        // Where each corner has ended up, from the dimensions.
        if let Some(b) = st.ui.built.sketches.get(&id).cloned() {
            let pts: Vec<(usize, P2)> = s.points.iter().enumerate().filter(|(_, p)| p.id != ORIGIN).map(|(i, p)| (i, b.solved.at(p.id))).collect();
            if !pts.is_empty() {
                egui::CollapsingHeader::new(RichText::new(format!("Corners ({})", pts.len())).color(theme::WEAK)).id_salt(("sketch_corners", id)).show(ui, |ui| {
                    super::params::grid(ui, ("sketch_corners_grid", id), |ui| {
                        for (k, (_, q)) in pts.iter().enumerate() {
                            super::params::read_only(ui, &format!("Point {}", k + 1), &format!("{} mm; {} mm", (q[0] * 1e7).round() / 1e4, (q[1] * 1e7).round() / 1e4));
                        }
                    });
                });
            }
        }
    });
    if edited != s.constraints {
        begin_edit(st);
        if let Some(x) = st.project.model.sketch_mut(id) {
            x.constraints = edited;
        }
    }
}

/// One undo step per field: the next edit after leaving it is a new one.
pub(super) fn done_editing(ui: &egui::Ui, st: &mut Studio) {
    if !ui.ctx().egui_wants_keyboard_input() && !super::expr_field::scrubbing(ui.ctx()) {
        st.ui.m3.editing = false;
    }
}

/// A feature's own dimensions, in a copy put back only if something changed.
pub(super) fn feature_panel(ui: &mut egui::Ui, st: &mut Studio, id: u32) {
    let Some(before) = st.project.model.feature(id).cloned() else { return };
    let mut f = before.clone();
    let table = VarTable::of(&st.project);
    let vars = st.project.vars().unwrap_or_default();
    let list = bodies_list(st);
    let features = st.project.model.features.clone();
    ui.scope(|ui| {
        theme::caption(ui, f.kind.label());
        egui::Grid::new(("model3d_grid", id)).num_columns(2).min_col_width(78.0).spacing(vec2(8.0, 3.0)).striped(true).show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(170.0));
            ui.end_row();
            match &mut f.kind {
                Kind::Sketch { plane, .. } => plane_ui(ui, st, "plane", plane, &table, &vars),
                Kind::Extrude { sketch, depth, extent, out } => {
                    ui.label("Sketch");
                    sketch_combo(ui, "extrude", sketch, &features);
                    ui.end_row();
                    ui.label("Direction");
                    theme::segmented(ui, extent, &[(Extent::One, "Forward"), (Extent::Reverse, "Back"), (Extent::Both, "Both ways"), (Extent::Through, "Through all")]);
                    ui.end_row();
                    if *extent != Extent::Through {
                        field(ui, st, "depth", "Depth", depth, Unit::Length, &table, &vars);
                    }
                    output_ui(ui, st, "extrude", out, true, &table, &vars, &list);
                }
                Kind::Sheet { sketch, out } => {
                    ui.label("Sketch");
                    sketch_combo(ui, "sheet", sketch, &features);
                    ui.end_row();
                    output_ui(ui, st, "sheet", out, false, &table, &vars, &list);
                }
                Kind::Face { plane, shape, out } => {
                    plane_ui(ui, st, "face_plane", plane, &table, &vars);
                    let (u, v) = match plane {
                        PlaneRef::Base { axis: 1, .. } | PlaneRef::InCoord { axis: 1, .. } => ("X", "Z"),
                        PlaneRef::Base { axis: 0, .. } | PlaneRef::InCoord { axis: 0, .. } => ("Y", "Z"),
                        _ => ("X", "Y"),
                    };
                    match shape {
                        Flat::Rect { p0, size } => {
                            field(ui, st, "face_p0", &format!("P0 {u}"), &mut p0[0], Unit::Length, &table, &vars);
                            field(ui, st, "face_p1", &format!("P0 {v}"), &mut p0[1], Unit::Length, &table, &vars);
                            field(ui, st, "face_du", &format!("{u}Dim"), &mut size[0], Unit::Length, &table, &vars);
                            field(ui, st, "face_dv", &format!("{v}Dim"), &mut size[1], Unit::Length, &table, &vars);
                        }
                        Flat::Circle { c, r } => {
                            field(ui, st, "face_c0", &format!("Centre {u}"), &mut c[0], Unit::Length, &table, &vars);
                            field(ui, st, "face_c1", &format!("Centre {v}"), &mut c[1], Unit::Length, &table, &vars);
                            field(ui, st, "face_r", "Radius", r, Unit::Length, &table, &vars);
                        }
                        Flat::Ellipse { c, rx, ry } => {
                            field(ui, st, "face_c0", &format!("Centre {u}"), &mut c[0], Unit::Length, &table, &vars);
                            field(ui, st, "face_c1", &format!("Centre {v}"), &mut c[1], Unit::Length, &table, &vars);
                            field(ui, st, "face_rx", &format!("Radius {u}"), rx, Unit::Length, &table, &vars);
                            field(ui, st, "face_ry", &format!("Radius {v}"), ry, Unit::Length, &table, &vars);
                        }
                        Flat::Polygon { c, r, sides } => {
                            field(ui, st, "face_c0", &format!("Centre {u}"), &mut c[0], Unit::Length, &table, &vars);
                            field(ui, st, "face_c1", &format!("Centre {v}"), &mut c[1], Unit::Length, &table, &vars);
                            field(ui, st, "face_r", "Radius", r, Unit::Length, &table, &vars);
                            field(ui, st, "face_n", "Sides", sides, Unit::Plain, &table, &vars);
                        }
                    }
                    output_ui(ui, st, "face", out, false, &table, &vars, &list);
                }
                Kind::Revolve { sketch, axis, angle, out } => {
                    ui.label("Sketch");
                    sketch_combo(ui, "revolve", sketch, &features);
                    ui.end_row();
                    ui.label("About");
                    let mut which = match axis {
                        AxisRef::U => 0,
                        AxisRef::V => 1,
                        AxisRef::Line { .. } => 2,
                    };
                    theme::segmented(ui, &mut which, &[(0, "u axis"), (1, "v axis"), (2, "A line in the sketch")]);
                    ui.end_row();
                    if which != 2 {
                        *axis = if which == 0 { AxisRef::U } else { AxisRef::V };
                    } else {
                        let lines: Vec<(u32, String)> = features.iter().find(|x| x.id == *sketch).and_then(|x| if let Kind::Sketch { sketch, .. } = &x.kind { Some(sketch) } else { None }).map_or(Vec::new(), |s| s.curves.iter().filter(|c| matches!(c.kind, CurveKind::Line { .. })).enumerate().map(|(i, c)| (c.id, format!("Line {}{}", i + 1, if c.construction { " (construction)" } else { "" }))).collect());
                        let mut chosen = match axis {
                            AxisRef::Line { curve } => *curve,
                            _ => lines.first().map_or(0, |l| l.0),
                        };
                        ui.label("Line");
                        body_combo(ui, "revolve_line", &mut chosen, &lines);
                        ui.end_row();
                        *axis = AxisRef::Line { curve: chosen };
                    }
                    field(ui, st, "angle", "Angle", angle, Unit::Angle, &table, &vars);
                    output_ui(ui, st, "revolve", out, true, &table, &vars, &list);
                }
                Kind::Sweep { sketch, path, out } => {
                    ui.label("Section");
                    sketch_combo(ui, "sweep", sketch, &features);
                    ui.end_row();
                    path_ui(ui, st, "sweep_path", path, &features, &table, &vars);
                    output_ui(ui, st, "sweep", out, true, &table, &vars, &list);
                }
                Kind::Loft { sections, out } => {
                    let sketches: Vec<(u32, String)> = features.iter().filter(|x| matches!(x.kind, Kind::Sketch { .. })).map(|x| (x.id, x.name.clone())).collect();
                    ui.label("Sections");
                    ui.vertical(|ui| bodies_checks(ui, sections, &sketches));
                    ui.end_row();
                    output_ui(ui, st, "loft", out, true, &table, &vars, &list);
                }
                Kind::Solid { shape, out } => {
                    match shape {
                        Prim::Box { min, max } => {
                            point3(ui, st, "box_min", "From", min, Unit::Length, &table, &vars);
                            point3(ui, st, "box_max", "To", max, Unit::Length, &table, &vars);
                        }
                        Prim::Cuboid { p0, size } => {
                            point3(ui, st, "cuboid_p0", "P0", p0, Unit::Length, &table, &vars);
                            field(ui, st, "cuboid_x", "XDim", &mut size[0], Unit::Length, &table, &vars);
                            field(ui, st, "cuboid_y", "YDim", &mut size[1], Unit::Length, &table, &vars);
                            field(ui, st, "cuboid_z", "ZDim", &mut size[2], Unit::Length, &table, &vars);
                        }
                        Prim::Cylinder { a, b, r } => {
                            point3(ui, st, "cyl_a", "From", a, Unit::Length, &table, &vars);
                            point3(ui, st, "cyl_b", "To", b, Unit::Length, &table, &vars);
                            field(ui, st, "cyl_r", "Radius", r, Unit::Length, &table, &vars);
                        }
                        Prim::Cone { a, b, r0, r1 } => {
                            point3(ui, st, "cone_a", "From", a, Unit::Length, &table, &vars);
                            point3(ui, st, "cone_b", "To", b, Unit::Length, &table, &vars);
                            field(ui, st, "cone_r0", "Radius at start", r0, Unit::Length, &table, &vars);
                            field(ui, st, "cone_r1", "Radius at end", r1, Unit::Length, &table, &vars);
                        }
                        Prim::Sphere { c, r } => {
                            point3(ui, st, "sph_c", "Centre", c, Unit::Length, &table, &vars);
                            field(ui, st, "sph_r", "Radius", r, Unit::Length, &table, &vars);
                        }
                        Prim::Torus { c, axis, big, small } => {
                            point3(ui, st, "tor_c", "Centre", c, Unit::Length, &table, &vars);
                            point3(ui, st, "tor_a", "Axis", axis, Unit::Plain, &table, &vars);
                            field(ui, st, "tor_b", "Ring radius", big, Unit::Length, &table, &vars);
                            field(ui, st, "tor_s", "Tube radius", small, Unit::Length, &table, &vars);
                        }
                    }
                    output_ui(ui, st, "solid", out, false, &table, &vars, &list);
                }
                Kind::Wire { path, radius, out } => {
                    path_ui(ui, st, "wire_path", path, &features, &table, &vars);
                    field(ui, st, "wire_r", "Wire radius", radius, Unit::Length, &table, &vars);
                    ui.label("Material");
                    let metal = st.project.metal.clone();
                    materials_combo(ui, "wire", &mut out.material, &metal);
                    ui.end_row();
                }
                Kind::Combine { op, target, tools, keep_tools } => {
                    ui.label("Operation");
                    theme::segmented(ui, op, &[(CombineOp::Add, "Join"), (CombineOp::Cut, "Subtract"), (CombineOp::Common, "Intersect"), (CombineOp::Exclude, "Exclude")]);
                    ui.end_row();
                    // A body this step used up is no longer in the list, but it can still be picked.
                    let named = |id: u32| list.iter().find(|b| b.0 == id).map(|b| b.1.clone()).or_else(|| features.iter().find(|f| f.id == id).map(|f| f.name.clone())).unwrap_or_else(|| "(gone)".to_string());
                    let mut choices = list.clone();
                    for id in tools.iter().chain([&*target]) {
                        if !choices.iter().any(|b| b.0 == *id) {
                            choices.push((*id, named(*id)));
                        }
                    }
                    ui.label("Body");
                    body_combo(ui, "combine", target, &choices);
                    ui.end_row();
                    // A body isn't combined with itself: choosing one as the body takes it off this list.
                    let chosen = *target;
                    tools.retain(|t| *t != chosen);
                    let others: Vec<(u32, String)> = choices.iter().filter(|b| b.0 != *target).cloned().collect();
                    ui.label("With");
                    ui.vertical(|ui| bodies_checks(ui, tools, &others));
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(keep_tools, "Keep the other bodies too");
                    ui.end_row();
                }
                Kind::Pattern { bodies, layout, merge, feeds } => {
                    ui.label("Bodies");
                    ui.vertical(|ui| bodies_checks(ui, bodies, &list));
                    ui.end_row();
                    match layout {
                        Layout::Linear { step, count, step2, count2 } => {
                            point3(ui, st, "pat_s", "Step", step, Unit::Length, &table, &vars);
                            field(ui, st, "pat_n", "Copies", count, Unit::Plain, &table, &vars);
                            let mut two = step2.is_some();
                            ui.label("");
                            ui.checkbox(&mut two, "And a second direction");
                            ui.end_row();
                            if two {
                                let q = step2.get_or_insert_with(|| xyz("0", "20mm", "0"));
                                point3(ui, st, "pat_s2", "Step 2", q, Unit::Length, &table, &vars);
                                if count2.trim().is_empty() {
                                    *count2 = "2".into();
                                }
                                field(ui, st, "pat_n2", "Copies 2", count2, Unit::Plain, &table, &vars);
                            } else {
                                *step2 = None;
                            }
                        }
                        Layout::Circular { origin, axis, count, total } => {
                            point3(ui, st, "cir_o", "Centre", origin, Unit::Length, &table, &vars);
                            point3(ui, st, "cir_a", "Axis", axis, Unit::Plain, &table, &vars);
                            field(ui, st, "cir_n", "Copies", count, Unit::Plain, &table, &vars);
                            field(ui, st, "cir_t", "Over", total, Unit::Angle, &table, &vars);
                        }
                    }
                    ui.label("");
                    ui.checkbox(merge, "Join them into one body");
                    ui.end_row();
                    let feed_list: Vec<(u32, String)> = features.iter().filter(|x| matches!(x.kind, Kind::Feed { .. } | Kind::Probe { .. })).map(|x| (x.id, x.name.clone())).collect();
                    if !feed_list.is_empty() {
                        ui.label("Copy feeds");
                        ui.vertical(|ui| bodies_checks(ui, feeds, &feed_list));
                        ui.end_row();
                    }
                }
                Kind::Mirror { bodies, plane, merge } => {
                    ui.label("Bodies");
                    ui.vertical(|ui| bodies_checks(ui, bodies, &list));
                    ui.end_row();
                    plane_ui(ui, st, "mirror_plane", plane, &table, &vars);
                    ui.label("");
                    ui.checkbox(merge, "Join it to the original");
                    ui.end_row();
                }
                Kind::Move { bodies, by, origin, axis, angle, copy } => {
                    ui.label("Bodies");
                    ui.vertical(|ui| bodies_checks(ui, bodies, &list));
                    ui.end_row();
                    point3(ui, st, "mv_by", "Move", by, Unit::Length, &table, &vars);
                    let mut turn = axis.is_some();
                    ui.label("");
                    ui.checkbox(&mut turn, "And turn it");
                    ui.end_row();
                    if turn {
                        let o = origin.get_or_insert_with(|| xyz("0", "0", "0"));
                        point3(ui, st, "mv_o", "About", o, Unit::Length, &table, &vars);
                        let a = axis.get_or_insert_with(|| xyz("0", "0", "1"));
                        point3(ui, st, "mv_a", "Axis", a, Unit::Plain, &table, &vars);
                        if angle.trim().is_empty() {
                            *angle = "90deg".into();
                        }
                        field(ui, st, "mv_t", "Angle", angle, Unit::Angle, &table, &vars);
                    } else {
                        *axis = None;
                    }
                    ui.label("");
                    ui.checkbox(copy, "Leave the original where it is");
                    ui.end_row();
                }
                Kind::Fillet { body, edges, radius, chamfer } => {
                    ui.label("Body");
                    body_combo(ui, "fillet", body, &list);
                    ui.end_row();
                    field(ui, st, "fillet_r", if *chamfer { "Distance" } else { "Radius" }, radius, Unit::Length, &table, &vars);
                    ui.label("Edges");
                    ui.label(RichText::new(format!("{} picked", edges.len())).color(theme::WEAK));
                    ui.end_row();
                }
                Kind::Shell { body, open, thick } => {
                    ui.label("Body");
                    body_combo(ui, "shell", body, &list);
                    ui.end_row();
                    field(ui, st, "shell_t", "Wall", thick, Unit::Length, &table, &vars);
                    ui.label("Open faces");
                    ui.label(RichText::new(format!("{} picked", open.len())).color(theme::WEAK));
                    ui.end_row();
                }
                Kind::Thicken { body, thick } => {
                    ui.label("Surface");
                    body_combo(ui, "thicken", body, &list);
                    ui.end_row();
                    field(ui, st, "thick_t", "Thickness", thick, Unit::Length, &table, &vars);
                }
                Kind::Import { file, tris, out } => {
                    ui.label("File");
                    ui.label(RichText::new(file.rsplit(['\\', '/']).next().unwrap_or(file)).color(theme::WEAK));
                    ui.end_row();
                    ui.label("Triangles");
                    ui.label(RichText::new(format!("{}", tris.len())).color(theme::WEAK));
                    ui.end_row();
                    output_ui(ui, st, "import", out, false, &table, &vars, &list);
                }
                Kind::Feed { a, b, width, z0 } => {
                    anchor_ui(ui, st, "feed_a", "From", a, &table, &vars);
                    anchor_ui(ui, st, "feed_b", "To", b, &table, &vars);
                    field(ui, st, "feed_w", "Width", width, Unit::Length, &table, &vars);
                    field(ui, st, "feed_z", "Impedance", z0, Unit::Plain, &table, &vars);
                }
                Kind::Probe { width, z0, .. } => {
                    field(ui, st, "probe_w", "Width", width, Unit::Length, &table, &vars);
                    field(ui, st, "probe_z", "Impedance", z0, Unit::Plain, &table, &vars);
                }
            }
        });
        if matches!(f.kind, Kind::Fillet { .. }) && !matches!(st.ui.m3.picking, Picking::Edges(i) if i == id) && ui.button("Pick edges").clicked() {
            st.ui.m3.picking = Picking::Edges(id);
            st.ui.pointer = Pointer::View;
        }
        if matches!(f.kind, Kind::Shell { .. }) && !matches!(st.ui.m3.picking, Picking::OpenFaces(i) if i == id) && ui.button("Pick faces to leave open").clicked() {
            st.ui.m3.picking = Picking::OpenFaces(id);
            st.ui.pointer = Pointer::View;
        }
        if matches!(f.kind, Kind::Sketch { .. }) && st.ui.m3.active != Some(id) && ui.button(theme::label(icon::NOTE_PENCIL, "Edit this sketch")).clicked() {
            open_sketch(st, id);
        }
    });
    if f != before {
        begin_edit(st);
        if let Some(slot) = st.project.model.feature_mut(id) {
            *slot = f;
        }
    }
}

fn plane_ui(ui: &mut egui::Ui, st: &mut Studio, salt: &str, plane: &mut PlaneRef, table: &VarTable, vars: &Vars) {
    match plane {
        PlaneRef::Base { axis, offset } => {
            ui.label("Plane");
            theme::segmented(ui, axis, &[(2, "XY"), (1, "XZ"), (0, "YZ")]);
            ui.end_row();
            field(ui, st, salt, "Offset", offset, Unit::Length, table, vars);
        }
        PlaneRef::Face { offset, .. } => {
            ui.label("Plane");
            ui.label(RichText::new("On a face").color(theme::WEAK));
            ui.end_row();
            field(ui, st, salt, "Offset", offset, Unit::Length, table, vars);
        }
        PlaneRef::Custom { origin, normal } => {
            point3(ui, st, &format!("{salt}_o"), "Through", origin, Unit::Length, table, vars);
            point3(ui, st, &format!("{salt}_n"), "Normal", normal, Unit::Plain, table, vars);
        }
        PlaneRef::InCoord { coord, axis, offset } => {
            ui.label("Plane");
            theme::segmented(ui, axis, &[(2, "XY"), (1, "XZ"), (0, "YZ")]);
            ui.end_row();
            ui.label("In");
            ui.label(RichText::new(st.project.model.coord(*coord).map_or("(gone)".to_string(), |c| c.name.clone())).color(theme::WEAK));
            ui.end_row();
            field(ui, st, salt, "Offset", offset, Unit::Length, table, vars);
        }
    }
}

fn anchor_ui(ui: &mut egui::Ui, st: &mut Studio, salt: &str, label: &str, a: &mut Anchor, table: &VarTable, vars: &Vars) {
    match a {
        Anchor::Xyz { at } => point3(ui, st, salt, label, at, Unit::Length, table, vars),
        Anchor::Sketch { height, .. } => {
            ui.label(label);
            ui.label(RichText::new("A sketch point: its dimensions move it").color(theme::WEAK));
            ui.end_row();
            field(ui, st, salt, "Off the plane", height, Unit::Length, table, vars);
        }
        Anchor::Offset { by, .. } => {
            ui.label(label);
            ui.label(RichText::new("A step from a sketch point, which it follows").color(theme::WEAK));
            ui.end_row();
            point3(ui, st, salt, "Step", by, Unit::Length, table, vars);
        }
    }
}

fn path_ui(ui: &mut egui::Ui, st: &mut Studio, salt: &str, path: &mut PathRef, features: &[Feature], table: &VarTable, vars: &Vars) {
    let mut which = match path {
        PathRef::Sketch { .. } => 0,
        PathRef::Helix { .. } => 1,
        PathRef::Points { .. } => 2,
    };
    ui.label("Path");
    if theme::segmented(ui, &mut which, &[(0, "Sketch"), (1, "Helix"), (2, "Points")]) {
        *path = match which {
            0 => PathRef::Sketch { sketch: features.iter().rev().find(|f| matches!(f.kind, Kind::Sketch { .. })).map_or(0, |f| f.id) },
            1 => PathRef::Helix { origin: xyz("0", "0", "0"), axis: xyz("0", "0", "1"), radius: "10mm".into(), radius2: String::new(), pitch: "10mm".into(), turns: "3".into(), left: false },
            _ => PathRef::Points { pts: vec![xyz("0", "0", "0"), xyz("0", "0", "10mm")] },
        };
    }
    ui.end_row();
    match path {
        PathRef::Sketch { sketch } => {
            ui.label("Sketch");
            sketch_combo(ui, salt, sketch, features);
            ui.end_row();
        }
        PathRef::Helix { origin, axis, radius, radius2, pitch, turns, left } => {
            point3(ui, st, &format!("{salt}o"), "Centre", origin, Unit::Length, table, vars);
            point3(ui, st, &format!("{salt}a"), "Axis", axis, Unit::Plain, table, vars);
            field(ui, st, &format!("{salt}r"), "Radius", radius, Unit::Length, table, vars);
            field(ui, st, &format!("{salt}r2"), "Far radius", radius2, Unit::Length, table, vars);
            field(ui, st, &format!("{salt}p"), "Pitch", pitch, Unit::Length, table, vars);
            field(ui, st, &format!("{salt}t"), "Turns", turns, Unit::Plain, table, vars);
            ui.label("");
            ui.checkbox(left, "Left-handed");
            ui.end_row();
        }
        PathRef::Points { pts } => {
            let mut drop = None;
            let n = pts.len();
            for (k, q) in pts.iter_mut().enumerate() {
                point3(ui, st, &format!("{salt}{k}"), &format!("Point {}", k + 1), q, Unit::Length, table, vars);
                if n > 2 {
                    ui.label("");
                    if ui.small_button("Remove this point").clicked() {
                        drop = Some(k);
                    }
                    ui.end_row();
                }
            }
            if let Some(k) = drop {
                pts.remove(k);
            }
            ui.label("");
            if ui.small_button("Add a point").clicked() {
                let last = pts.last().cloned().unwrap_or_else(|| xyz("0", "0", "0"));
                pts.push(last);
            }
            ui.end_row();
        }
    }
}

/// The sketch being drawn in: how defined it is, what holds it, and relations for what's selected.
pub(super) fn sketch_panel(ui: &mut egui::Ui, st: &mut Studio, id: u32) {
    let vars = st.project.vars().unwrap_or_default();
    let Some(s) = st.project.model.sketch(id).cloned() else { return };
    let solved = s.solve(&vars);
    let name = st.project.model.feature(id).map_or(String::new(), |f| f.name.clone());
    group(ui, |ui| {
        ui.horizontal(|ui| {
            theme::caption(ui, &format!("Drawing in {name}"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(theme::label(icon::CHECK, "Finish sketch")).clicked() {
                    exit_sketch(st);
                }
            });
        });
        if !solved.ok() {
            let why = solved.broken.first().map_or("Some of its constraints can't hold together. Remove one below.".to_string(), |b| b.1.clone());
            verdict(ui, false, &why);
        } else if s.curves.is_empty() && s.points.is_empty() {
            weak(ui, "Nothing drawn yet. Pick a tool above and click on the grid: a dimension appears on each shape, and a click on it lets you type a number or a variable.");
        } else if solved.free == 0 {
            verdict(ui, true, "Fully defined: every point is held by a dimension or a constraint.");
        } else {
            weak(ui, format!("{} ways it can still move, shown in blue. Dimensions and constraints hold it; Fully define does the rest.", solved.free));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Grid");
            ui.add(egui::DragValue::new(&mut st.ui.m3.grid_mm).range(0.01..=100.0).speed(0.05).suffix(" mm").max_decimals(2));
            ui.checkbox(&mut st.ui.m3.snap, "Snap");
            ui.checkbox(&mut st.ui.m3.auto_dims, "Add dimensions as I draw");
        });
        if st.ui.m3.tool == SkTool::Round {
            ui.horizontal(|ui| {
                ui.label("Corner radius");
                let table = VarTable::of(&st.project);
                length_line(ui, "model3d_round", &mut st.ui.m3.round_radius, &table, 100.0);
            });
        }
        relation_buttons(ui, st, &s, &solved);
    });
    if let Some(n) = st.ui.m3.note.clone() {
        verdict(ui, false, &n);
    }
    // Everything holding the sketch, each one removable, dimensions editable.
    egui::CollapsingHeader::new(RichText::new(format!("Dimensions and constraints ({})", s.constraints.len())).color(theme::TEXT)).id_salt("model3d_constraints").default_open(true).show(ui, |ui| {
        let table = VarTable::of(&st.project);
        let mut edited = s.constraints.clone();
        let mut drop = None;
        let bad = |i: usize| solved.conflicts.contains(&i) || solved.broken.iter().any(|b| b.0 == i);
        egui::Grid::new("model3d_list").num_columns(3).spacing(vec2(8.0, 3.0)).show(ui, |ui| {
            for (i, c) in edited.iter_mut().enumerate() {
                let label = c.label();
                let col = if bad(i) { theme::DANGER } else { theme::TEXT };
                ui.label(RichText::new(label).size(12.5).color(col));
                match c.value_mut() {
                    Some(v) => {
                        ui.horizontal(|ui| {
                            let line = if label == "Angle" { angle_line } else { length_line };
                            let r = line(ui, ("model3d_list_v", i), v, &table, 110.0);
                            if r.lost_focus() {
                                *v = normalised(v, label == "Angle");
                            }
                            super::draw::name_it(ui, st, &format!("c{i}"), label, v);
                        });
                    }
                    None => {
                        ui.label("");
                    }
                }
                if theme::icon_button(ui, icon::TRASH, "Remove it").clicked() {
                    drop = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = drop {
            edited.remove(i);
        }
        if edited != s.constraints {
            begin_edit(st);
            with_sketch(st, |x, _| x.constraints = edited);
        }
    });
}

/// Dimensions that fit what's selected, reading their values from where things are now.
fn dimension_buttons(ui: &mut egui::Ui, s: &Sketch, solved: &Solved, points: &[u32], lines: &[u32], rounds: &[u32], add: &mut Vec<Constraint>) {
    let at = |p: u32| solved.at(p);
    let ends = |l: u32| match s.curve(l).map(|c| &c.kind) {
        Some(CurveKind::Line { a, b }) => Some((*a, *b)),
        _ => None,
    };
    let radius_of = |c: u32| match s.curve(c).map(|x| &x.kind) {
        Some(CurveKind::Circle { .. }) => solved.radii.get(&c).copied().unwrap_or(0.0),
        Some(CurveKind::Arc { c: cp, a, .. }) => {
            let (o, q) = (at(*cp), at(*a));
            (q[0] - o[0]).hypot(q[1] - o[1])
        }
        _ => 0.0,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Dimensions").size(12.5).color(theme::WEAK));
        let mut b = |ui: &mut egui::Ui, text: &str, c: Vec<Constraint>| {
            if ui.small_button(text).clicked() {
                add.extend(c);
            }
        };
        match (points, lines, rounds) {
            ([p], [], []) => {
                let q = at(*p);
                b(ui, "Position from the origin", vec![Constraint::DistanceU { a: ORIGIN, b: *p, value: mm_text(q[0]) }, Constraint::DistanceV { a: ORIGIN, b: *p, value: mm_text(q[1]) }]);
            }
            ([x, y], [], []) => {
                let (p, q) = (at(*x), at(*y));
                b(ui, "Distance", vec![Constraint::Distance { a: *x, b: *y, value: mm_text((q[0] - p[0]).hypot(q[1] - p[1])) }]);
                b(ui, "Across", vec![Constraint::DistanceU { a: *x, b: *y, value: mm_text(q[0] - p[0]) }]);
                b(ui, "Up", vec![Constraint::DistanceV { a: *x, b: *y, value: mm_text(q[1] - p[1]) }]);
            }
            ([p], [l], []) => {
                if let Some((u, v)) = ends(*l) {
                    let (q, a, c) = (at(*p), at(u), at(v));
                    let d = [c[0] - a[0], c[1] - a[1]];
                    let off = (d[0] * (q[1] - a[1]) - d[1] * (q[0] - a[0])).abs() / d[0].hypot(d[1]).max(1e-30);
                    b(ui, "Distance to the line", vec![Constraint::PointLine { p: *p, line: *l, value: mm_text(off) }]);
                }
            }
            ([], [l], []) => {
                if let Some((u, v)) = ends(*l) {
                    let (a, c) = (at(u), at(v));
                    b(ui, "Length", vec![Constraint::Distance { a: u, b: v, value: mm_text((c[0] - a[0]).hypot(c[1] - a[1])) }]);
                }
            }
            ([], [x, y], []) => {
                if let (Some((a, c)), Some((d, e))) = (ends(*x), ends(*y)) {
                    let dir = |p: u32, q: u32| (at(q)[1] - at(p)[1]).atan2(at(q)[0] - at(p)[0]);
                    let deg = (dir(d, e) - dir(a, c)).to_degrees();
                    b(ui, "Angle", vec![Constraint::Angle { a: *x, b: *y, value: format!("{deg:.3}deg") }]);
                }
            }
            ([], [], [c]) => {
                let r = radius_of(*c);
                b(ui, "Radius", vec![Constraint::Radius { curve: *c, value: mm_text(r) }]);
                b(ui, "Diameter", vec![Constraint::Diameter { curve: *c, value: mm_text(2.0 * r) }]);
            }
            _ => {
                ui.label(RichText::new("nothing for this combination").size(12.0).color(theme::WEAK));
            }
        }
    });
}

/// Buttons for the relations that make sense for what's selected.
fn relation_buttons(ui: &mut egui::Ui, st: &mut Studio, s: &Sketch, solved: &Solved) {
    let (points, curves) = (st.ui.m3.sel.points.clone(), st.ui.m3.sel.curves.clone());
    if points.is_empty() && curves.is_empty() {
        weak(ui, "Select points and curves with the Select tool, holding Ctrl for more than one, to tie them together.");
        return;
    }
    let is_line = |c: u32| matches!(s.curve(c).map(|x| &x.kind), Some(CurveKind::Line { .. }));
    let is_round = |c: u32| matches!(s.curve(c).map(|x| &x.kind), Some(CurveKind::Circle { .. } | CurveKind::Arc { .. }));
    let lines: Vec<u32> = curves.iter().copied().filter(|c| is_line(*c)).collect();
    let rounds: Vec<u32> = curves.iter().copied().filter(|c| is_round(*c)).collect();
    let mut add: Vec<Constraint> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Relations").size(12.5).color(theme::WEAK));
        let mut b = |ui: &mut egui::Ui, text: &str, c: Constraint| {
            if ui.small_button(text).clicked() {
                add.push(c);
            }
        };
        match (points.as_slice(), lines.as_slice(), rounds.as_slice()) {
            ([p], [], []) => b(ui, "Fix in place", Constraint::Fixed { p: *p }),
            ([a, c], [], []) => {
                b(ui, "Coincident", Constraint::Coincident { a: *a, b: *c });
                b(ui, "Mirror across u = 0", Constraint::SymmetricAxis { a: *a, b: *c, axis: 0 });
                b(ui, "Mirror across v = 0", Constraint::SymmetricAxis { a: *a, b: *c, axis: 1 });
            }
            ([a, c], [l], []) => b(ui, "Symmetric about the line", Constraint::Symmetric { a: *a, b: *c, line: *l }),
            ([p], [l], []) => {
                b(ui, "On the line", Constraint::OnLine { p: *p, line: *l });
                b(ui, "Midpoint", Constraint::Midpoint { p: *p, line: *l });
            }
            ([p], [], [c]) => b(ui, "On the curve", Constraint::OnCircle { p: *p, curve: *c }),
            ([], [l], []) => {
                b(ui, "Horizontal", Constraint::Horizontal { line: *l });
                b(ui, "Vertical", Constraint::Vertical { line: *l });
            }
            ([], [x, y], []) => {
                b(ui, "Parallel", Constraint::Parallel { a: *x, b: *y });
                b(ui, "Perpendicular", Constraint::Perpendicular { a: *x, b: *y });
                b(ui, "Equal length", Constraint::EqualLength { a: *x, b: *y });
            }
            ([], [l], [c]) => b(ui, "Tangent", Constraint::Tangent { line: *l, arc: *c }),
            ([], [], [x, y]) => b(ui, "Equal radius", Constraint::EqualRadius { a: *x, b: *y }),
            _ => {
                ui.label(RichText::new("nothing for this combination").size(12.0).color(theme::WEAK));
            }
        }
    });
    dimension_buttons(ui, s, solved, &points, &lines, &rounds, &mut add);
    if !add.is_empty() {
        super::draw::remember(st);
        with_sketch(st, |x, _| x.constraints.extend(add));
    }
}

pub(super) fn glyph_of(k: &Kind) -> Glyph {
    match k {
        Kind::Sketch { .. } => Glyph::Rect,
        Kind::Extrude { out: Output { mode: Mode::Cut, .. }, .. } => Glyph::Cut,
        Kind::Extrude { out: Output { surface: true, .. }, .. } => Glyph::SurfaceExtrude,
        Kind::Extrude { .. } => Glyph::Extrude,
        Kind::Sheet { .. } => Glyph::Sheet,
        Kind::Face { shape: Flat::Rect { .. }, .. } => Glyph::Rect,
        Kind::Face { shape: Flat::Circle { .. }, .. } => Glyph::Circle,
        Kind::Face { shape: Flat::Ellipse { .. }, .. } => Glyph::Ellipse,
        Kind::Face { shape: Flat::Polygon { .. }, .. } => Glyph::Polygon,
        Kind::Revolve { .. } => Glyph::Revolve,
        Kind::Sweep { .. } => Glyph::Sweep,
        Kind::Loft { .. } => Glyph::Loft,
        Kind::Solid { shape: Prim::Box { .. } | Prim::Cuboid { .. }, .. } => Glyph::Box_,
        Kind::Solid { shape: Prim::Cylinder { .. }, .. } => Glyph::Cylinder,
        Kind::Solid { shape: Prim::Cone { .. }, .. } => Glyph::Cone,
        Kind::Solid { shape: Prim::Sphere { .. }, .. } => Glyph::Sphere,
        Kind::Solid { shape: Prim::Torus { .. }, .. } => Glyph::Torus,
        Kind::Wire { path: PathRef::Helix { .. }, .. } => Glyph::Helix,
        Kind::Wire { .. } => Glyph::Wire,
        Kind::Combine { op, .. } => COMBINE_WAYS.iter().find(|w| w.0 == *op).map_or(Glyph::Join, |w| w.1),
        Kind::Pattern { layout: Layout::Circular { .. }, .. } => Glyph::Circular,
        Kind::Pattern { .. } => Glyph::Pattern,
        Kind::Mirror { .. } => Glyph::Mirror,
        Kind::Move { .. } => Glyph::Move,
        Kind::Fillet { chamfer: true, .. } => Glyph::Chamfer,
        Kind::Fillet { .. } => Glyph::Fillet,
        Kind::Shell { .. } => Glyph::Shell,
        Kind::Thicken { .. } => Glyph::Thicken,
        Kind::Import { .. } => Glyph::Import,
        Kind::Feed { .. } => Glyph::Feed,
        Kind::Probe { .. } => Glyph::Probe,
    }
}

/// Several things selected: what they are, and what can be done with them together.
pub(super) fn selection_panel(ui: &mut egui::Ui, st: &mut Studio) {
    let bodies = st.ui.m3.sel.bodies();
    let names: Vec<String> = bodies.iter().filter_map(|id| st.ui.built.bodies.iter().find(|b| b.id == *id).map(|b| b.name.clone())).collect();
    let steps = st.ui.m3.sel.features().len();
    let mut ask = None;
    group(ui, |ui| {
        if names.len() >= 2 {
            theme::caption(ui, &format!("{} bodies selected", names.len()));
            weak(ui, names.join(", "));
            ui.horizontal(|ui| {
                for (op, glyph, word, what) in COMBINE_WAYS {
                    if cmd_button(ui, glyph, word, true, false, what) {
                        ask = Some(Cmd::Boolean(op));
                    }
                }
            });
            weak(ui, format!("Subtract keeps {} and takes the others from it.", names[0]));
        } else {
            theme::caption(ui, &format!("{steps} steps selected"));
            weak(ui, "Delete removes them. To combine, select two or more bodies: sketches and feeds aren't bodies.");
        }
    });
    if let Some(c) = ask {
        run_cmd(st, c);
    }
}

/// How the model will be meshed, what that comes to, and how to change it.
pub(super) fn mesh_panel(ui: &mut egui::Ui, st: &mut Studio) {
    egui::CollapsingHeader::new(RichText::new("Mesh").color(theme::TEXT)).id_salt("model3d_mesh").default_open(true).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut show = st.ui.view == ViewMode::Mesh;
            if ui.checkbox(&mut show, "Show it on the model").changed() {
                st.ui.view = if show { ViewMode::Mesh } else { ViewMode::Model };
            }
            weak(ui, st.ui.chosen.label().to_string());
        });
        egui::Grid::new("model3d_mesh_grid").num_columns(2).spacing(vec2(10.0, 5.0)).show(ui, |ui| {
            ui.label("Surface mesh");
            ui.add(egui::DragValue::new(&mut st.project.setup.edges_per_lambda).range(4.0..=80.0).speed(0.2).suffix(" edges per wavelength"));
            ui.end_row();
            ui.label("FDTD cells");
            ui.add(egui::DragValue::new(&mut st.project.setup.cells_per_lambda).range(6.0..=200.0).speed(0.2).suffix(" per wavelength"));
            ui.end_row();
            ui.label("Wire segments");
            ui.add(egui::DragValue::new(&mut st.project.setup.segments_per_lambda).range(6.0..=200.0).speed(0.2).suffix(" per wavelength"));
            ui.end_row();
            ui.label("Curves");
            let mut facets = st.project.model.facets as f64;
            if ui.add(egui::DragValue::new(&mut facets).range(8.0..=256.0).speed(0.5).suffix(" sides to a circle")).changed() {
                st.project.model.facets = facets as u32;
            }
            ui.end_row();
        });
        weak(ui, "A single body's mesh size is in its own step, under Mesh size.");
        if let Some((m, n)) = st.ui.mom_mesh.as_ref() {
            let (mut least, mut longest) = (180.0_f64, 0.0_f64);
            for t in &m.tris {
                let c = t.map(|i| m.verts[i as usize]);
                for k in 0..3 {
                    let (u, v) = (sub(c[(k + 1) % 3], c[k]), sub(c[(k + 2) % 3], c[k]));
                    longest = longest.max(norm(u));
                    if norm(u) > 0.0 && norm(v) > 0.0 {
                        least = least.min(dot(unit(u), unit(v)).clamp(-1.0, 1.0).acos().to_degrees());
                    }
                }
            }
            let lambda = antenna_core::C0 / st.project.setup.f_hi.max(1.0);
            let tiles = [
                (format!("{n}"), "unknowns", theme::ACCENT),
                (format!("{}", m.tris.len()), "triangles", theme::ACCENT),
                (format!("{}", m.segments.len()), "wire segments", theme::ACCENT),
                (format!("λ/{:.0}", lambda / longest.max(1e-12)), "longest edge", theme::TEXT),
                (format!("{least:.0}°"), "smallest angle", if least < 10.0 { theme::WARNING } else { theme::TEXT }),
            ];
            super::tiles(ui, &tiles);
            if least < 5.0 {
                verdict(ui, false, "Some triangles are very thin, which can spoil the answer. A finer mesh, or a small feature made bigger, helps.");
            }
        } else if let Some(plan) = st.ui.plan.as_ref() {
            let tiles = [
                (format!("{:.2} M", plan.cells() as f64 / 1e6), "cells", theme::ACCENT),
                (format!("{:.3} mm", plan.min_cell * 1e3), "smallest cell", theme::TEXT),
                (format!("{:.2} mm", plan.max_cell * 1e3), "largest cell", theme::TEXT),
                (format!("{:.1} GB", plan.memory_bytes() as f64 / 1e9), "memory", theme::TEXT),
            ];
            super::tiles(ui, &tiles);
            for w in &plan.warnings {
                weak(ui, w.clone());
            }
        } else {
            weak(ui, "There's a mesh to show once the model has a feed and solves.");
        }
        if let Some(p) = &st.ui.setup_problem {
            verdict(ui, false, p);
        }
    });
}

pub(super) fn inspect_panel(ui: &mut egui::Ui, st: &mut Studio) {
    if !st.ui.m3.section_on {
        return;
    }
    group(ui, |ui| {
        theme::caption(ui, "Section view");
        ui.horizontal(|ui| {
            theme::segmented(ui, &mut st.ui.m3.section_axis, &[(0, "x"), (1, "y"), (2, "z")]);
            let (lo, hi) = st.ui.built.bounds().unwrap_or(([-0.05; 3], [0.05; 3]));
            let k = st.ui.m3.section_axis;
            let (a, b) = (lo[k] as f32, hi[k] as f32);
            ui.add(egui::Slider::new(&mut st.ui.m3.section_at, a..=b).custom_formatter(|v, _| format!("{:.2} mm", v * 1e3)));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use antenna_core::modeler::{BodyKind, Built};
    use antenna_core::project::Project;

    fn studio() -> Studio {
        let mut st = Studio::default();
        st.project = Project::from_template("custom", Some("1GHz")).unwrap();
        st.ui.page = Page::Draw;
        st.ui.m3.grid_mm = 1.0;
        st
    }

    fn built(st: &Studio) -> Built {
        st.project.model.build(&st.project.vars().unwrap())
    }

    fn with_a_flat_part(st: &mut Studio) {
        st.project.extra.push(antenna_core::project::ExtraSolid {
            name: "plate".into(),
            material: "PEC".into(),
            prim: antenna_core::project::ExtraPrim::Box { min: ["0".into(), "0".into(), "0".into()], max: ["10mm".into(), "10mm".into(), "1mm".into()] },
        });
    }

    #[test]
    fn a_project_of_flat_parts_opens_on_them_and_can_still_switch_to_3d() {
        let mut st = studio();
        with_a_flat_part(&mut st);
        entered(&mut st);
        assert!(!st.ui.m3.flat, "its parts are in the tree with their numbers under it, so nothing has to be switched");
        choose_flat(&mut st, true);
        assert!(st.ui.m3.flat, "the flat tools are still there when asked for");
        choose_flat(&mut st, false);
        assert!(!st.ui.m3.flat);
    }

    #[test]
    fn a_ribbon_tool_brings_up_the_3d_model_from_any_page() {
        let mut st = studio();
        with_a_flat_part(&mut st);
        st.ui.page = Page::SParams;
        st.ui.view = ViewMode::Pattern;
        ribbon_command(&mut st, Cmd::SketchHere);
        assert_eq!(st.ui.page, Page::Draw);
        assert!(!st.ui.m3.flat);
        assert!(st.ui.m3.active.is_some(), "the sketch started");
        assert!(drawing(&st), "and the left button draws in it, on a view that shows it");
        // Later in the same frame the page router sees the page it arrived on.
        super::super::canvas_for_page(&mut st);
        assert!(drawing(&st), "arriving at the page must not take the tool back out of the hand");
    }

    #[test]
    fn a_ribbon_tool_that_stays_on_its_tab_does_not_move_the_ribbon() {
        let mut st = studio();
        let asked = st.ui.m3.tab_asked;
        ribbon_command(&mut st, Cmd::Section);
        assert_eq!(st.ui.m3.tab_asked, asked);
        // Finishing a sketch leads on to what can be done with it.
        ribbon_command(&mut st, Cmd::SketchHere);
        let asked = st.ui.m3.tab_asked;
        ribbon_command(&mut st, Cmd::ExitSketch);
        assert_ne!(st.ui.m3.tab_asked, asked);
    }

    #[test]
    fn a_templates_parts_come_in_from_the_ribbon_so_the_body_tools_wake_up() {
        let mut st = studio();
        st.project = Project::from_template("patch_air", Some("2.4GHz")).unwrap();
        st.ui.model = st.project.geometry().ok();
        st.ui.built = built(&st);
        let before = ribbon_state(&st);
        assert!(before.loose_parts > 0 && !before.has_body, "greyed out, with the way in on offer");
        ribbon_command(&mut st, Cmd::BringIn);
        st.ui.model = st.project.geometry().ok();
        st.ui.built = built(&st);
        let after = ribbon_state(&st);
        assert!(after.has_body, "the body tools have something to work on");
    }

    #[test]
    fn view_and_draw_say_who_has_the_left_button() {
        let mut st = studio();
        assert_eq!(st.ui.pointer, Pointer::View, "it opens on viewing, so a click can't draw anything");
        assert!(!drawing(&st));
        // Asking to draw with nowhere to draw asks where, and doesn't take the button yet.
        set_pointer(&mut st, Pointer::Draw);
        assert_eq!(st.ui.m3.picking, Picking::SketchPlane);
        assert_eq!(st.ui.pointer, Pointer::View);
        // With a sketch open, it switches both ways and the sketch stays open.
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        assert!(drawing(&st));
        set_pointer(&mut st, Pointer::View);
        assert!(!drawing(&st));
        assert_eq!(st.ui.m3.active, Some(id));
        set_pointer(&mut st, Pointer::Draw);
        assert!(drawing(&st));
        exit_sketch(&mut st);
        assert_eq!(st.ui.pointer, Pointer::View);
        // Selected, its sketch opens again when drawing's asked for.
        st.ui.m3.sel.feature = Some(id);
        set_pointer(&mut st, Pointer::Draw);
        assert_eq!(st.ui.m3.active, Some(id));
    }

    #[test]
    fn a_rectangle_drawn_and_extruded_is_a_box_with_dimensions() {
        let mut st = studio();
        let sk_id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Rect;
        tool_click(&mut st, SkPick::Free([0.0, 0.0]));
        tool_click(&mut st, SkPick::Free([0.04, 0.02]));
        let s = st.project.model.sketch(sk_id).unwrap();
        assert_eq!(s.curves.len(), 4);
        // What it's drawn at became dimensions, ready to be typed over.
        let dims: Vec<&str> = s.constraints.iter().filter_map(|c| c.value()).collect();
        assert_eq!(dims, vec!["40mm", "20mm"]);
        assert_eq!(s.solve(&st.project.vars().unwrap()).free, 2, "it can still slide about");
        run_cmd(&mut st, Cmd::Extrude);
        assert_eq!(st.ui.m3.active, None, "the sketch closes when it's used");
        let b = built(&st);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.bodies[0].kind, BodyKind::Solid);
        // A variable drives the width.
        sketch_set_first_dim(&mut st, sk_id, "W");
        antenna_core::sketch::add_variable(&mut st.project, "W", "60mm", "").unwrap();
        let b = built(&st);
        let (lo, hi) = b.bodies[0].mesh.bounds();
        assert!(((hi[0] - lo[0]) - 0.06).abs() < 1e-9, "{lo:?} {hi:?}");
    }

    fn sketch_set_first_dim(st: &mut Studio, id: u32, to: &str) {
        let s = st.project.model.sketch_mut(id).unwrap();
        let c = s.constraints.iter_mut().find(|c| c.value().is_some()).unwrap();
        *c.value_mut().unwrap() = to.to_string();
    }

    #[test]
    fn lines_chain_and_close_into_a_loop() {
        let mut st = studio();
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Line;
        tool_click(&mut st, SkPick::Free([0.0, 0.0]));
        tool_click(&mut st, SkPick::Free([0.02, 0.0]));
        tool_click(&mut st, SkPick::Free([0.02, 0.01]));
        let first = st.project.model.sketch(id).unwrap().curves[0].clone();
        let CurveKind::Line { a, .. } = first.kind else { panic!() };
        tool_click(&mut st, SkPick::Point(a));
        assert!(st.ui.m3.picks.is_empty(), "closing it ends the chain");
        let s = st.project.model.sketch(id).unwrap();
        assert_eq!(s.curves.len(), 3);
        // Level and upright lines say so.
        assert!(s.constraints.iter().any(|c| matches!(c, Constraint::Horizontal { .. })));
        assert!(s.constraints.iter().any(|c| matches!(c, Constraint::Vertical { .. })));
        run_cmd(&mut st, Cmd::Sheet);
        let b = built(&st);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert!((b.bodies[0].mesh.area() - 0.5 * 0.02 * 0.01).abs() < 1e-12);
    }

    #[test]
    fn a_circle_and_a_centred_rectangle_stay_around_their_centres() {
        let mut st = studio();
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Circle;
        tool_click(&mut st, SkPick::Point(ORIGIN));
        tool_click(&mut st, SkPick::Free([0.01, 0.0]));
        st.ui.m3.tool = SkTool::CentreRect;
        tool_click(&mut st, SkPick::Point(ORIGIN));
        tool_click(&mut st, SkPick::Free([0.03, 0.02]));
        let vars = st.project.vars().unwrap();
        let s = st.project.model.sketch(id).unwrap();
        let solved = s.solve(&vars);
        assert!(solved.ok(), "{solved:?}");
        // A rectangle around the circle: a plate with a round hole.
        let p = prims::profiles(s, &solved, &vars, 32);
        assert_eq!(p.regions.len(), 1);
        assert_eq!(p.regions[0].holes.len(), 1);
        assert!((p.regions[0].outer.area() - 0.06 * 0.04).abs() < 1e-9);
        // Its size can change and the middle stays on the origin.
        let s = st.project.model.sketch_mut(id).unwrap();
        for c in s.constraints.iter_mut() {
            if let Constraint::DistanceU { value, .. } = c {
                *value = "100mm".into();
            }
        }
        let got = st.project.model.sketch(id).unwrap().solve(&vars);
        let xs: Vec<f64> = st.project.model.sketch(id).unwrap().points.iter().map(|q| got.at(q.id)[0]).collect();
        let (lo, hi) = (xs.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max));
        assert!((lo + 0.05).abs() < 1e-9 && (hi - 0.05).abs() < 1e-9, "{lo} {hi}");
    }

    #[test]
    fn a_dimension_goes_on_what_is_clicked() {
        let mut st = studio();
        st.ui.m3.auto_dims = false;
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Rect;
        tool_click(&mut st, SkPick::Free([0.0, 0.0]));
        tool_click(&mut st, SkPick::Free([0.04, 0.02]));
        let s = st.project.model.sketch(id).unwrap().clone();
        let line = s.curves[0].id;
        make_dimension(&mut st, SkPick::On(line, [0.01, 0.0]), None, Pos2::ZERO);
        let s2 = st.project.model.sketch(id).unwrap();
        assert!(matches!(s2.constraints.last(), Some(Constraint::Distance { value, .. }) if value == "40mm"), "{:?}", s2.constraints.last());
        assert!(st.ui.m3.dim_edit.is_some(), "it opens for typing");
        let CurveKind::Line { a, b } = s.curves[0].kind else { panic!() };
        make_dimension(&mut st, SkPick::Point(a), Some(SkPick::Point(b)), Pos2::ZERO);
        let (c, curve) = (st.project.model.sketch(id).unwrap().points.len(), 0);
        let _ = (c, curve);
        st.ui.m3.tool = SkTool::Circle;
        tool_click(&mut st, SkPick::Free([0.1, 0.1]));
        tool_click(&mut st, SkPick::Free([0.11, 0.1]));
        let circle = st.project.model.sketch(id).unwrap().curves.last().unwrap().id;
        make_dimension(&mut st, SkPick::On(circle, [0.11, 0.1]), None, Pos2::ZERO);
        assert!(matches!(st.project.model.sketch(id).unwrap().constraints.last(), Some(Constraint::Radius { value, .. }) if value == "10mm"));
    }

    #[test]
    fn a_cut_defaults_to_going_right_through() {
        let mut st = studio();
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::CentreRect;
        tool_click(&mut st, SkPick::Point(ORIGIN));
        tool_click(&mut st, SkPick::Free([0.02, 0.02]));
        run_cmd(&mut st, Cmd::Extrude);
        let body = st.ui.m3.sel.feature.unwrap();
        let _ = (id, body);
        let hole = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Circle;
        tool_click(&mut st, SkPick::Point(ORIGIN));
        tool_click(&mut st, SkPick::Free([0.005, 0.0]));
        run_cmd(&mut st, Cmd::Cut);
        let b = built(&st);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.bodies.len(), 1);
        assert!(b.bodies[0].mesh.is_closed());
        let _ = hole;
    }

    #[test]
    fn a_feed_clicked_off_axis_lands_on_one() {
        let mut st = studio();
        add_feed(&mut st, &[([0.0, 0.0, 0.0], None), ([0.001, 0.0, 0.01], None)]);
        let b = built(&st);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        let p = &b.ports[0];
        assert!(p.a[0] == p.b[0] && (p.b[2] - 0.01).abs() < 1e-9, "{p:?}");
        // Between two sketch points it stays tied to them, so it follows their dimensions.
        let mut st = studio();
        let mut s = Sketch::default();
        let (a, c) = (s.add_point([0.0, 0.0]), s.add_point([0.0, 0.01]));
        let id = st.project.model.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "0".into() }, sketch: s });
        add_feed(&mut st, &[([0.0, 0.0, 0.0], Some((id, a))), ([0.0, 0.01, 0.0], Some((id, c)))]);
        let Kind::Feed { a: ka, b: kb, .. } = &st.project.model.features[1].kind else { panic!() };
        assert!(matches!(ka, Anchor::Sketch { .. }) && matches!(kb, Anchor::Sketch { .. }));
    }

    /// The far end of a clicked feed follows the sketch point, so the port stays on an axis at any value.
    #[test]
    fn a_clicked_feed_stays_tied_to_its_sketch_point() {
        let mut st = studio();
        antenna_core::sketch::add_variable(&mut st.project, "Fx", "5mm", "").unwrap();
        let mut s = Sketch::default();
        let p = s.add_point([0.005, 0.0]);
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p, value: "Fx".into() });
        s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p, value: "0mm".into() });
        let id = st.project.model.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "0".into() }, sketch: s });
        for order in [true, false] {
            let (near, far) = (([0.005, 0.0, 0.0], Some((id, p))), ([0.0052, 0.0001, 0.003], None));
            let before = st.project.model.features.len();
            add_feed(&mut st, &if order { [near, far] } else { [far, near] });
            assert_eq!(st.project.model.features.len(), before + 1);
            let mut moved = st.project.clone();
            moved.params.iter_mut().find(|r| r.name == "Fx").unwrap().expr = "9mm".into();
            let b = moved.model.build(&moved.vars().unwrap());
            assert!(b.errors.is_empty(), "{:?}", b.errors);
            let port = b.ports.last().unwrap();
            assert!((port.a[0] - 0.009).abs() < 1e-9 && (port.b[0] - 0.009).abs() < 1e-9, "both ends follow it: {port:?}");
            assert!((port.b[2] - port.a[2]).abs() > 0.0029 && port.a[1] == port.b[1], "still along z: {port:?}");
        }
    }

    /// A sketch, a box from it, and the build the commands look at.
    fn with_a_block(st: &mut Studio) -> (u32, u32) {
        let id = new_sketch(st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Rect;
        tool_click(st, SkPick::Free([0.0, 0.0]));
        tool_click(st, SkPick::Free([0.04, 0.02]));
        run_cmd(st, Cmd::Extrude);
        let body = st.ui.m3.sel.feature.unwrap();
        st.ui.built = built(st);
        (id, body)
    }

    #[test]
    fn delete_takes_what_is_selected_and_no_more() {
        // A step just made goes alone, not the object it's part of.
        let mut st = studio();
        let (sketch, body) = with_a_block(&mut st);
        run_cmd(&mut st, Cmd::Thicken);
        assert_eq!(st.ui.m3.sel.body, None);
        delete_selected(&mut st);
        assert_eq!(st.project.model.features.len(), 2, "the sketch and the block stay");

        // A step picked in the tree goes alone.
        run_cmd(&mut st, Cmd::Thicken);
        let step = st.ui.m3.sel.feature.unwrap();
        st.ui.m3.sel = Selection { feature: Some(step), body: Some(body), step: true, ..Default::default() };
        delete_selected(&mut st);
        assert!(st.project.model.feature(body).is_some());

        // The object picked goes with its sketch.
        st.ui.m3.sel = Selection { feature: Some(body), body: Some(body), ..Default::default() };
        delete_selected(&mut st);
        assert!(st.project.model.is_empty(), "{:?}", st.project.model.features);
        let _ = sketch;
        super::super::draw::undo(&mut st);
        assert_eq!(st.project.model.features.len(), 2, "one undo brings the lot back");
    }

    #[test]
    fn a_copy_made_by_a_pattern_is_not_deleted_or_copied_by_way_of_its_source() {
        let mut st = studio();
        let (_, body) = with_a_block(&mut st);
        st.ui.m3.sel = Selection { feature: Some(body), body: Some(body), ..Default::default() };
        run_cmd(&mut st, Cmd::Pattern);
        st.ui.built = built(&st);
        let copy = st.ui.built.bodies.iter().find(|b| b.copy).unwrap().clone();
        let before = st.project.model.features.len();
        st.ui.m3.sel = Selection { feature: Some(copy.last), body: Some(copy.id), ..Default::default() };
        delete_selected(&mut st);
        assert_eq!(st.project.model.features.len(), before, "the array is still there");
        assert!(st.ui.m3.note.as_deref().is_some_and(|n| n.contains(&copy.name)), "and it says why");
        run_cmd(&mut st, Cmd::Copy);
        assert!(st.ui.m3.clip.is_empty());
    }

    #[test]
    fn what_is_copied_comes_back_as_a_separate_object_selected() {
        let mut st = studio();
        let (_, body) = with_a_block(&mut st);
        st.ui.m3.sel = Selection { feature: Some(body), body: Some(body), ..Default::default() };
        run_cmd(&mut st, Cmd::Copy);
        run_cmd(&mut st, Cmd::Paste);
        let b = built(&st);
        assert!(b.errors.is_empty(), "{:?}", b.errors);
        assert_eq!(b.bodies.len(), 2);
        let new = st.ui.m3.sel.body.unwrap();
        assert_ne!(new, body);
        assert!(b.bodies.iter().any(|x| x.id == new));
        super::super::draw::undo(&mut st);
        assert_eq!(built(&st).bodies.len(), 1, "one undo takes the paste back");
    }

    #[test]
    fn a_tool_that_draws_starts_its_own_sketch_on_the_work_plane() {
        let mut st = studio();
        st.ui.m3.work_axis = 1;
        run_cmd(&mut st, Cmd::Tool(SkTool::Spline));
        assert_eq!(st.ui.m3.tool, SkTool::Spline);
        let id = st.ui.m3.active.expect("it opened one");
        assert!(matches!(plane_of(&st, id), PlaneRef::Base { axis: 1, .. }));
        exit_sketch(&mut st);
        // In a coordinate system, a sketch stays tied to it.
        st.ui.m3.coord = Some(st.project.model.add_coord());
        run_cmd(&mut st, Cmd::SketchHere);
        assert!(matches!(plane_of(&st, st.ui.m3.active.unwrap()), PlaneRef::InCoord { axis: 1, .. }));
    }

    #[test]
    fn an_angle_is_read_at_its_corner() {
        assert!((angle_between([1.0, 0.0, 0.0], [0.0; 3], [0.0, 1.0, 0.0]) - 90.0).abs() < 1e-9);
        assert!((angle_between([1.0, 0.0, 0.0], [0.0; 3], [1.0, 1.0, 0.0]) - 45.0).abs() < 1e-9);
        assert_eq!(angle_between([0.0; 3], [0.0; 3], [1.0, 0.0, 0.0]), 0.0, "no line, no angle");
    }

    #[test]
    fn a_measurement_never_reads_minus_zero() {
        assert_eq!(measure_text([-0.003, 0.004, -1e-12]), "5.000 mm   (x -3.000, y 4.000, z 0.000)");
    }

    #[test]
    fn a_feed_command_closes_the_sketch_so_clicks_don_t_draw() {
        let mut st = studio();
        new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        run_cmd(&mut st, Cmd::Feed);
        assert_eq!(st.ui.m3.active, None);
        assert!(drawing(&st), "the left button is for placing the feed");
    }

    #[test]
    fn undo_takes_back_what_a_command_did() {
        let mut st = studio();
        run_cmd(&mut st, Cmd::Helix);
        assert_eq!(st.project.model.features.len(), 1);
        super::super::draw::undo(&mut st);
        assert!(st.project.model.is_empty());
        // A line half drawn when its first point is undone doesn't go on from a point that's gone.
        let mut st = studio();
        new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Line;
        tool_click(&mut st, SkPick::Free([0.0, 0.0]));
        tool_click(&mut st, SkPick::Free([0.01, 0.0]));
        assert!(!st.ui.m3.picks.is_empty());
        super::super::draw::undo(&mut st);
        assert!(st.ui.m3.picks.is_empty());
    }

    /// Everything a sketch needs to be fully defined can be had from what's selected.
    #[test]
    fn a_rectangle_drawn_without_dimensions_can_be_defined_by_them() {
        let mut st = studio();
        st.ui.m3.auto_dims = false;
        let id = new_sketch(&mut st, PlaneRef::Base { axis: 2, offset: "0".into() });
        st.ui.m3.tool = SkTool::Rect;
        tool_click(&mut st, SkPick::Free([0.0, 0.0]));
        tool_click(&mut st, SkPick::Free([0.04, 0.02]));
        let vars = st.project.vars().unwrap();
        assert!(st.project.model.sketch(id).unwrap().solve(&vars).free > 0);
        with_sketch(&mut st, |s, v| s.fully_define(v));
        let s = st.project.model.sketch(id).unwrap();
        assert_eq!(s.solve(&vars).free, 0);
        let (lo, hi) = (s.points.iter().map(|p| p.at[0]).fold(f64::MAX, f64::min), s.points.iter().map(|p| p.at[0]).fold(f64::MIN, f64::max));
        assert!((hi - lo - 0.04).abs() < 1e-9, "it stays as drawn: {lo} {hi}");
    }

    #[test]
    fn asking_for_too_little_says_so_and_changes_nothing() {
        let mut st = studio();
        run_cmd(&mut st, Cmd::Extrude);
        assert!(st.ui.m3.note.is_some());
        assert!(st.project.model.is_empty());
        run_cmd(&mut st, Cmd::Fillet);
        assert!(st.project.model.is_empty());
        run_cmd(&mut st, Cmd::Boolean(CombineOp::Add));
        assert!(st.project.model.is_empty());
    }

    #[test]
    fn selecting_adds_and_takes_away_and_the_first_stays_first() {
        let (a, b, c) = (Picked { feature: 1, body: Some(10) }, Picked { feature: 2, body: Some(20) }, Picked { feature: 3, body: None });
        let mut s = Selection::default();
        s.toggle(a);
        s.toggle(b);
        s.toggle(c);
        assert_eq!(s.bodies(), vec![10, 20], "the sketch isn't a body");
        assert_eq!(s.features(), vec![1, 2, 3]);
        assert!(s.has_feature(3));
        // The same body by another of its steps is the same thing.
        s.toggle(Picked { feature: 9, body: Some(20) });
        assert_eq!(s.bodies(), vec![10]);
        s.toggle(a);
        assert_eq!((s.feature, s.body), (Some(3), None), "the next one takes over");
        s.toggle(c);
        assert_eq!(s, Selection::default());
    }

    #[test]
    fn a_patterns_copies_are_selected_one_by_one() {
        let mut s = Selection::default();
        for body in [10, 11, 12] {
            s.toggle(Picked { feature: 5, body: Some(body) });
        }
        assert_eq!(s.bodies(), vec![10, 11, 12], "the same step, three bodies");
        s.toggle(Picked { feature: 5, body: Some(11) });
        assert_eq!(s.bodies(), vec![10, 12]);
    }

    fn two_sheets() -> (Studio, u32, u32) {
        let mut st = studio();
        let mut sheet = |lo: [f64; 2], hi: [f64; 2]| {
            let mut s = antenna_core::modeler::sketch::Sketch::default();
            s.add_rect(lo, hi);
            let sk = st.project.model.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "0".into() }, sketch: s });
            st.project.model.add(Kind::Sheet { sketch: sk, out: Output::default() })
        };
        let (a, b) = (sheet([0.0, 0.0], [0.02, 0.02]), sheet([0.01, 0.01], [0.03, 0.03]));
        let vars = st.project.vars().unwrap();
        st.ui.built = st.project.model.build(&vars);
        (st, a, b)
    }

    #[test]
    fn exclude_on_two_selected_sheets_becomes_a_step_that_builds() {
        let (mut st, a, b) = two_sheets();
        toggle_pick(&mut st, Picked { feature: b, body: Some(b) });
        toggle_pick(&mut st, Picked { feature: a, body: Some(a) });
        run_cmd(&mut st, Cmd::Boolean(CombineOp::Exclude));
        let last = st.project.model.features.last().unwrap();
        let Kind::Combine { op, target, tools, keep_tools } = &last.kind else { panic!("{:?}", last.kind) };
        assert_eq!((*op, *target, tools.as_slice(), *keep_tools), (CombineOp::Exclude, b, &[a][..], false), "the first picked is the one kept");
        assert!(st.ui.m3.sel.more.is_empty());
        let built = st.project.model.build(&st.project.vars().unwrap());
        assert!(built.errors.is_empty(), "{:?}", built.errors);
        assert_eq!(built.bodies.len(), 1);
        assert!((built.bodies[0].mesh.area() - 6e-4).abs() < 1e-9, "{}", built.bodies[0].mesh.area());
    }

    #[test]
    fn another_project_lets_go_of_what_was_selected() {
        let (mut st, a, b) = two_sheets();
        toggle_pick(&mut st, Picked { feature: a, body: Some(a) });
        toggle_pick(&mut st, Picked { feature: b, body: Some(b) });
        // The same ids, but other things.
        let steps = st.project.model.features.len();
        st.project.model.features[0].name = "from another project".into();
        forget_stale_selection(&mut st);
        assert_eq!(st.ui.m3.sel, Selection::default());
        run_cmd(&mut st, Cmd::Boolean(CombineOp::Add));
        // With only two bodies there's nothing to choose between, and that's its own rule.
        assert_eq!(st.project.model.features.len(), steps + 1);
    }

    #[test]
    fn with_three_bodies_none_selected_it_says_what_to_do() {
        let (mut st, ..) = two_sheets();
        let mut s = antenna_core::modeler::sketch::Sketch::default();
        s.add_rect([0.1, 0.1], [0.11, 0.11]);
        let sk = st.project.model.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "0".into() }, sketch: s });
        st.project.model.add(Kind::Sheet { sketch: sk, out: Output::default() });
        let vars = st.project.vars().unwrap();
        st.ui.built = st.project.model.build(&vars);
        let steps = st.project.model.features.len();
        run_cmd(&mut st, Cmd::Boolean(CombineOp::Add));
        assert_eq!(st.project.model.features.len(), steps);
        assert!(st.ui.m3.note.as_ref().is_some_and(|n| n.contains("Ctrl+click")));
    }
}
