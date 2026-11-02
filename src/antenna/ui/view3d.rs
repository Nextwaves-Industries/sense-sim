//! The Studio's 3D canvas, software-projected and drawn with the egui painter:
//! an antenna is millimetres big and must not share a camera with a warehouse.
//! Triangles are sorted back to front, which is enough for these small models.

use antenna_core::csx::Plan;
use antenna_core::farfield::{FarField, Pol, Quantity};
use antenna_core::geometry::{Geometry, P3, Shape, tessellate};
use antenna_core::material;
use antenna_core::project::{CurrentView, FieldPlane};
use bevy_egui::egui::{self, Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape as EShape, Stroke, pos2, vec2};

use super::charts;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ViewMode {
    #[default]
    Model,
    Mesh,
    Pattern,
    Currents,
    Field,
}

impl ViewMode {
    pub const ALL: [(ViewMode, &'static str); 5] = [(ViewMode::Model, "Model"), (ViewMode::Mesh, "Mesh"), (ViewMode::Pattern, "3D pattern"), (ViewMode::Currents, "Currents"), (ViewMode::Field, "Field plane")];
}

pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan: egui::Vec2,
}

impl Default for Camera {
    fn default() -> Self {
        Self { yaw: 0.65, pitch: 0.42, zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

pub struct Projector {
    center: P3,
    scale: f32,
    origin: Pos2,
    r: [[f32; 3]; 3],
}

impl Projector {
    fn new(cam: &Camera, rect: Rect, center: P3, extent: f64) -> Self {
        let (sy, cy) = cam.yaw.sin_cos();
        let (sp, cp) = cam.pitch.sin_cos();
        // Screen right, screen up, towards the viewer. Antenna z is up on screen.
        let right = [cy, sy, 0.0];
        let toward = [sy * cp, -cy * cp, sp];
        let up = [-sy * sp, cy * sp, cp];
        let scale = rect.width().min(rect.height()) * 0.42 / (extent.max(1e-9) as f32) * cam.zoom;
        Self { center, scale, origin: rect.center() + cam.pan, r: [right, up, toward] }
    }
    /// Screen position and depth (larger is nearer).
    fn project(&self, p: P3) -> (Pos2, f32) {
        let d = [(p[0] - self.center[0]) as f32, (p[1] - self.center[1]) as f32, (p[2] - self.center[2]) as f32];
        let dot = |a: [f32; 3]| a[0] * d[0] + a[1] * d[1] + a[2] * d[2];
        (self.origin + vec2(dot(self.r[0]) * self.scale, -dot(self.r[1]) * self.scale), dot(self.r[2]))
    }
    fn toward(&self) -> [f64; 3] {
        let t = self.r[2];
        [t[0] as f64, t[1] as f64, t[2] as f64]
    }
    pub fn screen(&self, p: P3) -> Pos2 {
        self.project(p).0
    }
    /// The line of sight through a screen point: a point well in front of the model
    /// and the direction into the screen.
    pub fn ray(&self, at: Pos2) -> (P3, P3) {
        let (sx, sy) = (((at.x - self.origin.x) / self.scale) as f64, (-(at.y - self.origin.y) / self.scale) as f64);
        let (right, up, toward) = (self.r[0], self.r[1], self.r[2]);
        let o = std::array::from_fn(|k| self.center[k] + right[k] as f64 * sx + up[k] as f64 * sy + toward[k] as f64 * 1000.0);
        (o, std::array::from_fn(|k| -toward[k] as f64))
    }
}

fn normal(t: &[P3; 3]) -> P3 {
    antenna_core::geometry::unit(antenna_core::geometry::cross(antenna_core::geometry::sub(t[1], t[0]), antenna_core::geometry::sub(t[2], t[0])))
}

fn shade(base: Color32, n: P3, toward: [f64; 3]) -> Color32 {
    let d = antenna_core::geometry::dot(n, toward).abs();
    let k = (0.55 + 0.45 * d) as f32;
    Color32::from_rgba_unmultiplied((base.r() as f32 * k) as u8, (base.g() as f32 * k) as u8, (base.b() as f32 * k) as u8, base.a())
}

/// One queued triangle.
struct Tri {
    p: [Pos2; 3],
    c: [Color32; 3],
    depth: f32,
}

/// Paints triangles back to front as one mesh.
fn flush(painter: &egui::Painter, tris: &mut Vec<Tri>) {
    tris.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    let mut mesh = Mesh::default();
    for t in tris.iter() {
        let n = mesh.vertices.len() as u32;
        for k in 0..3 {
            mesh.colored_vertex(t.p[k], t.c[k]);
        }
        mesh.add_triangle(n, n + 1, n + 2);
        // egui indexes with u32, but a very large mesh stays cheaper in pieces.
        if mesh.vertices.len() > 60_000 {
            painter.add(EShape::mesh(std::mem::take(&mut mesh)));
        }
    }
    if !mesh.is_empty() {
        painter.add(EShape::mesh(mesh));
    }
    tris.clear();
}

/// One solid, as `tessellate` returns it.
pub type Tessellated = (Vec<[P3; 3]>, Vec<[P3; 2]>);

/// What the canvas shows this frame.
pub struct Scene<'a> {
    /// Wire boxes drawn with the model, and framed with it.
    pub boxes: &'a [(P3, P3, Color32)],
    /// The left button belongs to the drawing tool: Alt-drag orbits instead.
    pub drawing: bool,
    /// Fixes the framing, so the view holds still while the model grows.
    pub frame: Option<(P3, f64)>,
    /// A drawing grid under the model: `(plane axis, height, step in metres)`.
    pub grid: Option<(usize, f64, f64)>,
    pub geometry: Option<&'a Geometry>,
    /// The geometry's solids, tessellated, in order.
    pub tess: &'a [Tessellated],
    /// The MoM discretisation and its unknowns, when MoM is the solver.
    pub mom_mesh: Option<&'a (antenna_core::mom::MeshView, usize)>,
    pub mode: ViewMode,
    pub plan: Option<&'a Plan>,
    pub far: Option<&'a FarField>,
    pub quantity: Quantity,
    pub pol: Pol,
    pub range_db: f64,
    pub currents: Option<&'a CurrentView>,
    pub plane: Option<&'a FieldPlane>,
    pub plane_tex: Option<egui::TextureId>,
    pub transparent: bool,
    pub show_ports: bool,
    /// Reconfigurable parts: a port by name, its colour and its label.
    pub part_marks: &'a [(String, Color32, String)],
}

const METAL: Color32 = Color32::from_rgb(214, 168, 92);
const WIRE: Color32 = Color32::from_rgb(160, 110, 40);

fn solid_color(name: &str) -> Color32 {
    match material::by_name(name) {
        Some(m) if !m.is_conductor() => {
            let er = m.eps_r();
            if er > 15.0 { Color32::from_rgba_unmultiplied(214, 150, 120, 160) } else { Color32::from_rgba_unmultiplied(58, 150, 96, 120) }
        }
        _ => METAL,
    }
}

fn is_dielectric(name: &str) -> bool {
    material::by_name(name).is_some_and(|m| !m.is_conductor())
}

/// A bounding sphere around what is drawn.
fn frame(geom: Option<&Geometry>, cur: Option<&CurrentView>) -> (P3, f64) {
    let (lo, hi) = match (geom, cur) {
        (Some(g), _) if !g.solids.is_empty() => g.bounds(),
        (_, Some(c)) if !c.verts.is_empty() => {
            let mut lo = [f64::MAX; 3];
            let mut hi = [f64::MIN; 3];
            for v in &c.verts {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
            (lo, hi)
        }
        _ => ([-0.05; 3], [0.05; 3]),
    };
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let e = antenna_core::geometry::norm(antenna_core::geometry::sub(hi, lo)) / 2.0;
    (c, e.max(1e-4))
}

/// Pattern value to radius (0..1) and colour value.
fn pattern_mesh(far: &FarField, q: Quantity, pol: Pol, range: f64, pj: &Projector, center: P3, radius: f64, tris: &mut Vec<Tri>) -> f64 {
    let db = far.grid_db(q, pol);
    let peak = db.iter().flatten().copied().fold(f64::MIN, f64::max);
    let floor = peak - range;
    let (nt, np) = (far.theta.len(), far.phi.len());
    let pos = |ti: usize, pi: usize| -> (P3, f32) {
        let v = ((db[ti][pi] - floor) / range).clamp(0.0, 1.0);
        let r = radius * v;
        let (st, ct) = far.theta[ti].to_radians().sin_cos();
        let (sp, cp) = far.phi[pi].to_radians().sin_cos();
        ([center[0] + r * st * cp, center[1] + r * st * sp, center[2] + r * ct], v as f32)
    };
    let toward = pj.toward();
    for ti in 0..nt - 1 {
        for pi in 0..np - 1 {
            let (a, va) = pos(ti, pi);
            let (b, vb) = pos(ti + 1, pi);
            let (c, vc) = pos(ti + 1, pi + 1);
            let (d, vd) = pos(ti, pi + 1);
            for (t, cs) in [([a, b, c], [va, vb, vc]), ([a, c, d], [va, vc, vd])] {
                let n = normal(&t);
                let k = (0.72 + 0.28 * antenna_core::geometry::dot(n, toward).abs()) as f32;
                let col = |v: f32| {
                    let h = charts::heat(v);
                    Color32::from_rgba_unmultiplied((h.r() as f32 * k) as u8, (h.g() as f32 * k) as u8, (h.b() as f32 * k) as u8, 190)
                };
                let (p0, d0) = pj.project(t[0]);
                let (p1, d1) = pj.project(t[1]);
                let (p2, d2) = pj.project(t[2]);
                tris.push(Tri { p: [p0, p1, p2], c: [col(cs[0]), col(cs[1]), col(cs[2])], depth: (d0 + d1 + d2) / 3.0 });
            }
        }
    }
    peak
}

fn axes(painter: &egui::Painter, pj: &Projector, rect: Rect) {
    let o = rect.left_bottom() + vec2(44.0, -44.0);
    for (dir, name, col) in [([1.0, 0.0, 0.0], "x", charts::RED), ([0.0, 1.0, 0.0], "y", charts::GREEN), ([0.0, 0.0, 1.0], "z", charts::BLUE)] {
        let s = vec2(pj.r[0][0] * dir[0] + pj.r[0][1] * dir[1] + pj.r[0][2] * dir[2], -(pj.r[1][0] * dir[0] + pj.r[1][1] * dir[1] + pj.r[1][2] * dir[2]));
        painter.line_segment([o, o + s * 26.0], Stroke::new(2.0, col));
        painter.text(o + s * 33.0, Align2::CENTER_CENTER, name, FontId::proportional(12.0), col);
    }
}

/// The drawing plane's lines across the view, thinned until they're far enough apart
/// to read, every fifth one heavier, and the plane's axes heaviest.
fn drawing_grid(painter: &egui::Painter, pj: &Projector, rect: Rect, axis: usize, at: f64, step: f64) {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for c in corners {
        let (o, d) = pj.ray(c);
        if d[axis].abs() < 0.05 {
            return;
        }
        let t = (at - o[axis]) / d[axis];
        for (k, a) in [u, v].into_iter().enumerate() {
            let q = o[a] + d[a] * t;
            lo[k] = lo[k].min(q);
            hi[k] = hi[k].max(q);
        }
    }
    let mut every = step.max(1e-9);
    while every * (pj.scale as f64) < 8.0 {
        every *= 5.0;
    }
    let point = |a: f64, b: f64| {
        let mut p = [0.0; 3];
        p[axis] = at;
        p[u] = a;
        p[v] = b;
        pj.screen(p)
    };
    let minor = Stroke::new(1.0, Color32::from_rgb(232, 234, 237));
    let major = Stroke::new(1.0, Color32::from_rgb(212, 216, 222));
    let zero = Stroke::new(1.2, Color32::from_rgb(170, 176, 186));
    for k in 0..2 {
        let first = (lo[k] / every).floor() as i64;
        let last = (hi[k] / every).ceil() as i64;
        if last - first > 600 {
            continue;
        }
        for i in first..=last {
            let x = i as f64 * every;
            let line = if k == 0 { [point(x, lo[1]), point(x, hi[1])] } else { [point(lo[0], x), point(hi[0], x)] };
            let stroke = if i == 0 { zero } else if i % 5 == 0 { major } else { minor };
            painter.line_segment(line, stroke);
        }
    }
}

/// Draws the scene over all of `canvas`, framed on `rect`, the part the cards leave
/// free: a zoomed model runs on under the cards. Dragging orbits, right-drag pans,
/// the wheel zooms. Returns the canvas's response and how it projects, for a tool on top.
pub fn show(ui: &mut egui::Ui, canvas: Rect, rect: Rect, cam: &mut Camera, scene: &Scene) -> (egui::Response, Projector) {
    let id = ui.make_persistent_id("antenna_view3d");
    let resp = ui.interact(canvas, id, Sense::click_and_drag());
    if resp.dragged_by(egui::PointerButton::Primary) && (!scene.drawing || ui.input(|i| i.modifiers.alt)) {
        let d = resp.drag_delta();
        cam.yaw -= d.x * 0.008;
        cam.pitch = (cam.pitch + d.y * 0.008).clamp(-1.55, 1.55);
    }
    if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
        cam.pan += resp.drag_delta();
    }
    if resp.hovered() {
        let z = ui.input(|i| i.smooth_scroll_delta.y);
        if z != 0.0 {
            cam.zoom = (cam.zoom * (z * 0.0018).exp()).clamp(0.1, 40.0);
        }
    }
    if resp.double_clicked() && !scene.drawing {
        *cam = Camera::default();
    }

    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, 0.0, theme::BG);
    let (mut center, mut extent) = frame(scene.geometry, scene.currents.filter(|_| scene.mode == ViewMode::Currents));
    if !scene.boxes.is_empty() {
        let (mut lo, mut hi): (P3, P3) = (std::array::from_fn(|k| center[k] - extent), std::array::from_fn(|k| center[k] + extent));
        for (a, b, _) in scene.boxes {
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        center = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
        extent = antenna_core::geometry::norm(antenna_core::geometry::sub(hi, lo)) / 2.0;
    }
    if let Some((c, e)) = scene.frame {
        (center, extent) = (c, e);
    }
    // A far-field solid is drawn around the model and sized to frame both.
    let pattern = scene.far.filter(|_| scene.mode == ViewMode::Pattern);
    let view_extent = if pattern.is_some() { extent * 1.6 } else { extent };
    let pj = Projector::new(cam, rect, center, view_extent);
    let toward = pj.toward();
    let mut tris: Vec<Tri> = Vec::new();

    if let Some((axis, at, step)) = scene.grid {
        drawing_grid(&painter, &pj, canvas, axis, at, step);
    }

    // Ground plane: a tinted grid at z = 0.
    if let Some(g) = scene.geometry.filter(|g| g.ground) {
        let r = extent * 1.4;
        let (cx, cy) = (center[0], center[1]);
        let n = 10;
        let grid = Stroke::new(1.0, Color32::from_rgb(208, 212, 218));
        let _ = g;
        for k in 0..=n {
            let t = -r + 2.0 * r * k as f64 / n as f64;
            let (a, _) = pj.project([cx + t, cy - r, 0.0]);
            let (b, _) = pj.project([cx + t, cy + r, 0.0]);
            painter.line_segment([a, b], grid);
            let (a, _) = pj.project([cx - r, cy + t, 0.0]);
            let (b, _) = pj.project([cx + r, cy + t, 0.0]);
            painter.line_segment([a, b], grid);
        }
    }

    // Field plane, under the model.
    if scene.mode == ViewMode::Field
        && let (Some(fp), Some(tex)) = (scene.plane, scene.plane_tex)
    {
        let add = antenna_core::geometry::add;
        let c = [fp.corner, add(fp.corner, fp.du), add(add(fp.corner, fp.du), fp.dv), add(fp.corner, fp.dv)];
        let p: Vec<Pos2> = c.iter().map(|x| pj.project(*x).0).collect();
        let mut m = Mesh::with_texture(tex);
        m.vertices.push(egui::epaint::Vertex { pos: p[0], uv: pos2(0.0, 1.0), color: Color32::WHITE });
        m.vertices.push(egui::epaint::Vertex { pos: p[1], uv: pos2(1.0, 1.0), color: Color32::WHITE });
        m.vertices.push(egui::epaint::Vertex { pos: p[2], uv: pos2(1.0, 0.0), color: Color32::WHITE });
        m.vertices.push(egui::epaint::Vertex { pos: p[3], uv: pos2(0.0, 0.0), color: Color32::WHITE });
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        painter.add(EShape::mesh(m));
    }

    let mut lines: Vec<(Pos2, Pos2, f32, Color32, f32)> = Vec::new();

    // Currents replace the plain model where a solver gave them.
    let cur = scene.currents.filter(|_| scene.mode == ViewMode::Currents);
    if let Some(c) = cur {
        // The feed is singular and would push everything else off a peak-based scale.
        let mut sorted: Vec<f64> = c.current.iter().copied().filter(|v| v.is_finite()).collect();
        sorted.sort_by(f64::total_cmp);
        let peak = sorted.get(sorted.len().saturating_sub(1) * 98 / 100).copied().unwrap_or(1.0).max(1e-30);
        let nseg = c.segments.len();
        let col = |v: f64| charts::heat(((20.0 * (v / peak).max(1e-6).log10() + 30.0) / 30.0) as f32);
        for (i, s) in c.segments.iter().enumerate() {
            let v = c.current.get(i).copied().unwrap_or(0.0);
            let (a, da) = pj.project(c.verts[s[0] as usize]);
            let (b, db) = pj.project(c.verts[s[1] as usize]);
            lines.push((a, b, 3.5, col(v), (da + db) / 2.0));
        }
        for (i, t) in c.tris.iter().enumerate() {
            let v = c.current.get(nseg + i).copied().unwrap_or(0.0);
            let tri = [c.verts[t[0] as usize], c.verts[t[1] as usize], c.verts[t[2] as usize]];
            let (p0, d0) = pj.project(tri[0]);
            let (p1, d1) = pj.project(tri[1]);
            let (p2, d2) = pj.project(tri[2]);
            let color = col(v);
            tris.push(Tri { p: [p0, p1, p2], c: [color; 3], depth: (d0 + d1 + d2) / 3.0 });
        }
    }

    // The model.
    if let Some(g) = scene.geometry.filter(|_| cur.is_none()) {
        // Dielectrics first: a patch lies exactly on its substrate, and a depth
        // sort of coplanar faces would flicker between them.
        for pass_dielectric in [true, false] {
            for (s, (ts, _)) in g.solids.iter().zip(scene.tess) {
                if is_dielectric(&s.material) != pass_dielectric {
                    continue;
                }
                let mut base = solid_color(&s.material);
                if scene.transparent {
                    base = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), base.a().min(90));
                }
                for t in ts {
                    let color = shade(base, normal(t), toward);
                    let (p0, d0) = pj.project(t[0]);
                    let (p1, d1) = pj.project(t[1]);
                    let (p2, d2) = pj.project(t[2]);
                    tris.push(Tri { p: [p0, p1, p2], c: [color; 3], depth: (d0 + d1 + d2) / 3.0 });
                }
            }
            flush(&painter, &mut tris);
        }
        for (s, (_, ls)) in g.solids.iter().zip(scene.tess) {
            if let Shape::Wire { r, .. } = &s.shape {
                let w = ((*r as f32 * pj.scale * 2.0).clamp(1.5, 10.0)).max(2.0);
                for l in ls {
                    let (a, da) = pj.project(l[0]);
                    let (b, db) = pj.project(l[1]);
                    lines.push((a, b, w, WIRE, (da + db) / 2.0));
                }
            }
            // Outline of sheets, so a flat plate still reads as a shape.
            if let Shape::Box { min, max } = &s.shape
                && let Some(axis) = (0..3).find(|k| max[*k] - min[*k] <= 0.0)
            {
                let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                let mk = |a: f64, b: f64| {
                    let mut p = [0.0; 3];
                    p[axis] = min[axis];
                    p[u] = a;
                    p[v] = b;
                    p
                };
                let c = [mk(min[u], min[v]), mk(max[u], min[v]), mk(max[u], max[v]), mk(min[u], max[v])];
                for k in 0..4 {
                    let (a, da) = pj.project(c[k]);
                    let (b, db) = pj.project(c[(k + 1) % 4]);
                    lines.push((a, b, 1.0, Color32::from_black_alpha(90), (da + db) / 2.0));
                }
            }
        }
    }

    for (a, b, color) in scene.boxes {
        let corner = |i: usize| [if i & 1 == 0 { a[0] } else { b[0] }, if i & 2 == 0 { a[1] } else { b[1] }, if i & 4 == 0 { a[2] } else { b[2] }];
        for i in 0..8 {
            for bit in [1, 2, 4] {
                if i & bit == 0 {
                    let (p, dp) = pj.project(corner(i));
                    let (q, dq) = pj.project(corner(i | bit));
                    lines.push((p, q, 1.2, *color, (dp + dq) / 2.0));
                }
            }
        }
    }

    // The MoM mesh: segments and triangle edges on the metal.
    let mom = scene.mom_mesh.filter(|_| scene.mode == ViewMode::Mesh);
    if let Some((m, _)) = mom {
        flush(&painter, &mut tris);
        let line = Stroke::new(0.8, Color32::from_rgba_unmultiplied(30, 60, 150, 170));
        for t in &m.tris {
            let p: Vec<Pos2> = t.iter().map(|i| pj.project(m.verts[*i as usize]).0).collect();
            painter.line_segment([p[0], p[1]], line);
            painter.line_segment([p[1], p[2]], line);
            painter.line_segment([p[2], p[0]], line);
        }
        for s in &m.segments {
            let (a, _) = pj.project(m.verts[s[0] as usize]);
            let (b, _) = pj.project(m.verts[s[1] as usize]);
            painter.circle_filled(a, 2.0, line.color);
            painter.circle_filled(b, 2.0, line.color);
        }
    }

    // FDTD: mesh lines on the faces of the simulation domain and through the model.
    if scene.mode == ViewMode::Mesh
        && mom.is_none()
        && let Some(plan) = scene.plan
    {
        let [mx, my, mz] = &plan.mesh;
        let z = mz.iter().copied().min_by(|a, b| (a - center[2]).abs().total_cmp(&(b - center[2]).abs())).unwrap_or(0.0);
        let thin = Stroke::new(0.5, Color32::from_rgba_unmultiplied(38, 90, 200, 110));
        let (x0, x1, y0, y1) = (mx[0], mx[mx.len() - 1], my[0], my[my.len() - 1]);
        for x in drawn_lines(mx) {
            let (a, _) = pj.project([*x, y0, z]);
            let (b, _) = pj.project([*x, y1, z]);
            painter.line_segment([a, b], thin);
        }
        for y in drawn_lines(my) {
            let (a, _) = pj.project([x0, *y, z]);
            let (b, _) = pj.project([x1, *y, z]);
            painter.line_segment([a, b], thin);
        }
        let (z0, z1) = (mz[0], mz[mz.len() - 1]);
        let y = my.iter().copied().min_by(|a, b| (a - center[1]).abs().total_cmp(&(b - center[1]).abs())).unwrap_or(0.0);
        let thin2 = Stroke::new(0.5, Color32::from_rgba_unmultiplied(200, 90, 38, 90));
        for x in drawn_lines(mx) {
            let (a, _) = pj.project([*x, y, z0]);
            let (b, _) = pj.project([*x, y, z1]);
            painter.line_segment([a, b], thin2);
        }
        for zz in drawn_lines(mz) {
            let (a, _) = pj.project([x0, y, *zz]);
            let (b, _) = pj.project([x1, y, *zz]);
            painter.line_segment([a, b], thin2);
        }
    }

    flush(&painter, &mut tris);
    lines.sort_by(|a, b| a.4.total_cmp(&b.4));
    for (a, b, w, c, _) in lines {
        painter.line_segment([a, b], Stroke::new(w, c));
    }

    // The far-field solid, see-through, around the model.
    let mut peak_label = None;
    if let Some(far) = pattern {
        peak_label = Some(pattern_mesh(far, scene.quantity, scene.pol, scene.range_db, &pj, center, extent * 1.45, &mut tris));
        flush(&painter, &mut tris);
    }

    if scene.show_ports
        && let Some(g) = scene.geometry
    {
        for (i, port) in g.ports.iter().enumerate() {
            let (a, _) = pj.project(port.a);
            let (b, _) = pj.project(port.b);
            // A part on the gap, in its state's colour, instead of a feed's number.
            if let Some((_, color, label)) = scene.part_marks.iter().find(|m| m.0 == port.name) {
                painter.line_segment([a, b], Stroke::new(4.0, *color));
                let m = pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                painter.circle(m, 4.0, Color32::WHITE, Stroke::new(2.0, *color));
                painter.text(m + vec2(7.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(10.5), *color);
                continue;
            }
            painter.line_segment([a, b], Stroke::new(3.0, theme::DANGER));
            let m = pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
            painter.circle_filled(m, 4.5, theme::DANGER);
            let label = format!("{}", i + 1);
            painter.text(m, Align2::CENTER_CENTER, label, FontId::proportional(9.0), Color32::WHITE);
        }
    }
    axes(&painter, &pj, rect);

    if let Some(peak) = peak_label {
        let text = format!("{} {}: {peak:.2} dBi peak, {:.0} dB range", scene.quantity.label(), scene.pol.label(), scene.range_db);
        painter.text(rect.center_bottom() - vec2(0.0, 12.0), Align2::CENTER_BOTTOM, text, FontId::proportional(13.0), theme::TEXT);
    }
    if let Some((m, n)) = mom {
        let t = format!("{n} unknowns · {} segments, {} triangles", m.segments.len(), m.tris.len());
        painter.text(rect.center_bottom() - vec2(0.0, 12.0), Align2::CENTER_BOTTOM, t, FontId::proportional(13.0), theme::TEXT);
    } else if scene.mode == ViewMode::Mesh
        && let Some(plan) = scene.plan
    {
        let t = format!("{}×{}×{} = {:.2} M cells · cell {:.3} to {:.2} mm", plan.mesh[0].len() - 1, plan.mesh[1].len() - 1, plan.mesh[2].len() - 1, plan.cells() as f64 / 1e6, plan.min_cell * 1e3, plan.max_cell * 1e3);
        painter.text(rect.center_bottom() - vec2(0.0, 12.0), Align2::CENTER_BOTTOM, t, FontId::proportional(13.0), theme::TEXT);
    }
    (resp, pj)
}

/// The mesh lines of one axis worth drawing. Each is painted every frame, and a big
/// mesh has far more than a screen can show apart.
fn drawn_lines(lines: &[f64]) -> impl Iterator<Item = &f64> {
    const MOST: usize = 600;
    lines.iter().step_by((lines.len() / MOST).max(1))
}

/// A small static picture of a template for the gallery.
pub fn thumbnail(ui: &mut egui::Ui, rect: Rect, geom: &Geometry) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::RAISED);
    let cam = Camera { yaw: 0.7, pitch: 0.45, zoom: 1.0, pan: egui::Vec2::ZERO };
    let (c, e) = frame(Some(geom), None);
    let pj = Projector::new(&cam, rect, c, e);
    let toward = pj.toward();
    let mut tris = Vec::new();
    let mut lines: Vec<(Pos2, Pos2, f32)> = Vec::new();
    for s in &geom.solids {
        let (ts, ls) = tessellate(&s.shape);
        let base = solid_color(&s.material);
        for t in ts.iter().take(1500) {
            let color = shade(base, normal(t), toward);
            let (p0, d0) = pj.project(t[0]);
            let (p1, d1) = pj.project(t[1]);
            let (p2, d2) = pj.project(t[2]);
            tris.push(Tri { p: [p0, p1, p2], c: [color; 3], depth: (d0 + d1 + d2) / 3.0 });
        }
        for l in &ls {
            lines.push((pj.project(l[0]).0, pj.project(l[1]).0, 1.6));
        }
    }
    flush(&painter, &mut tris);
    for (a, b, w) in lines {
        painter.line_segment([a, b], Stroke::new(w, WIRE));
    }
    for port in &geom.ports {
        let m = pj.project(port.center()).0;
        painter.circle_filled(m, 2.5, theme::DANGER);
    }
}
