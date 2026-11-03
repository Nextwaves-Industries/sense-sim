//! The Studio's 3D canvas, software-projected and drawn with the egui painter:
//! an antenna is millimetres big and must not share a camera with a warehouse.
//! With no depth buffer, solids are painted back to front, each one whole, and a solid's own
//! triangles sorted back to front, which is enough for these small models.

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

impl Camera {
    /// The views offered by name: (name, tip, yaw, pitch).
    pub const VIEWS: [(&'static str, &'static str, f32, f32); 4] = [
        ("3D", "See the model at an angle", 0.65, 0.42),
        ("Top", "Look straight down", 0.0, std::f32::consts::FRAC_PI_2),
        ("Front", "Look along +y", 0.0, 0.0),
        ("Side", "Look along -x", std::f32::consts::FRAC_PI_2, 0.0),
    ];
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
    // `r()` and friends are already multiplied by alpha: going through them again would turn
    // every see-through solid dark and colourless.
    let [r, g, b, a] = base.to_srgba_unmultiplied();
    translucent([(r as f32 * k) as u8, (g as f32 * k) as u8, (b as f32 * k) as u8], a)
}

/// `rgb` at this opacity, as it should look on the canvas. The window blends in linear light,
/// where a faint tint all but vanishes and a gold plate reads as grey, so this works back from
/// the colour it's meant to end up. Over anything but the canvas it's close enough.
fn translucent(rgb: [u8; 3], alpha: u8) -> Color32 {
    if alpha == 255 {
        return Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    }
    use egui::ecolor::{gamma_u8_from_linear_f32, linear_f32_from_gamma_u8};
    let a = alpha as f32 / 255.0;
    let bg = theme::BG.to_array();
    let channel = |k: usize| {
        let want = (rgb[k] as f32 * a + bg[k] as f32 * (1.0 - a)).round() as u8;
        gamma_u8_from_linear_f32((linear_f32_from_gamma_u8(want) - linear_f32_from_gamma_u8(bg[k]) * (1.0 - a)).max(0.0))
    };
    Color32::from_rgba_premultiplied(channel(0), channel(1), channel(2), alpha)
}

/// The same colour, at most this opaque. Blending in linear light, much less than this lets so
/// much of the bright canvas through that no colour can show: 35 % made metal a pale pink.
fn see_through(base: Color32) -> Color32 {
    let [r, g, b, a] = base.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, a.min(150))
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

/// One solid, for working out what is painted over what. The box is of the triangles drawn, not
/// `Shape::bounds`, which pads a cylinder by its radius along its own axis too.
#[derive(Clone, Copy)]
struct Part {
    lo: P3,
    hi: P3,
    dielectric: bool,
}

impl Part {
    fn of(tris: &[[P3; 3]], dielectric: bool) -> Self {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for p in tris.iter().flatten() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Self { lo, hi, dielectric }
    }

    fn center(&self) -> P3 {
        std::array::from_fn(|k| (self.lo[k] + self.hi[k]) / 2.0)
    }

    fn size(&self) -> f64 {
        antenna_core::geometry::norm(antenna_core::geometry::sub(self.hi, self.lo))
    }

    /// Where it can land on the screen.
    fn on_screen(&self, pj: &Projector) -> Rect {
        let corner = |i: usize| pj.screen(std::array::from_fn(|k| if i & (1 << k) == 0 { self.lo[k] } else { self.hi[k] }));
        Rect::from_points(&std::array::from_fn::<Pos2, 8, _>(corner))
    }
}

/// Whether `a` goes on the canvas before `b`. Sorting triangles can't answer this: a plate is two
/// huge triangles whose centres say little about the part standing on it.
fn painted_before(a: &Part, b: &Part, toward: [f64; 3], tolerance: f64) -> bool {
    // A flat plane between them: the one on the viewer's side can't be hidden by the other. The view
    // has no perspective, so this holds everywhere on the screen, and parts that touch, such as a
    // patch on its substrate, get an answer that doesn't flicker as a depth tie would.
    for k in 0..3 {
        if toward[k] != 0.0 {
            if a.hi[k] <= b.lo[k] + tolerance {
                return toward[k] > 0.0;
            }
            if b.hi[k] <= a.lo[k] + tolerance {
                return toward[k] < 0.0;
            }
        }
    }
    // Their boxes overlap. Usually one is sunk a little into the other, so the shallowest overlap
    // stands in for that plane.
    let overlap = |k: usize| a.hi[k].min(b.hi[k]) - a.lo[k].max(b.lo[k]);
    let k = (0..3).min_by(|x, y| overlap(*x).total_cmp(&overlap(*y))).unwrap_or(0);
    let nearer = (a.center()[k] - b.center()[k]) * toward[k];
    if nearer.abs() > tolerance {
        return nearer < 0.0;
    }
    // One through the middle of the other. A dielectric can be seen through, so it goes first; then
    // the smaller part stays in sight, as a probe through a plate should.
    if a.dielectric != b.dielectric {
        return a.dielectric;
    }
    a.size() >= b.size()
}

/// More solids than this take too long to compare every frame.
const MOST_ORDERED: usize = 800;

/// The order to paint parts in, back to front.
fn paint_order(parts: &[Part], pj: &Projector) -> Vec<usize> {
    let depth: Vec<f32> = parts.iter().map(|p| pj.project(p.center()).1).collect();
    let mut left: Vec<usize> = (0..parts.len()).collect();
    left.sort_by(|a, b| depth[*a].total_cmp(&depth[*b]));
    if parts.len() > MOST_ORDERED {
        left.sort_by_key(|i| !parts[*i].dielectric);
        return left;
    }
    let toward = pj.toward();
    let tolerance = parts.iter().map(Part::size).fold(0.0, f64::max) * 1e-6;
    let screen: Vec<Rect> = parts.iter().map(|p| p.on_screen(pj)).collect();
    let mut waits = vec![0_usize; parts.len()];
    let mut then: Vec<Vec<usize>> = vec![Vec::new(); parts.len()];
    for a in 0..parts.len() {
        for b in a + 1..parts.len() {
            if screen[a].intersects(screen[b]) {
                let (first, second) = if painted_before(&parts[a], &parts[b], toward, tolerance) { (a, b) } else { (b, a) };
                then[first].push(second);
                waits[second] += 1;
            }
        }
    }
    // The farthest part that nothing has to come before. Parts can overlap in a ring, with each in
    // front of the next: then nothing is free, and the farthest one breaks the ring.
    let mut order = Vec::with_capacity(parts.len());
    while !left.is_empty() {
        let i = left.remove(left.iter().position(|i| waits[*i] == 0).unwrap_or(0));
        for j in &then[i] {
            waits[*j] = waits[*j].saturating_sub(1);
        }
        order.push(i);
    }
    order
}

/// Paints solids back to front: each one's triangles, whether it's a dielectric, and its colour.
fn paint_solids<'a>(painter: &egui::Painter, pj: &Projector, solids: impl Iterator<Item = (&'a [[P3; 3]], bool, Color32)>) {
    let solids: Vec<_> = solids.filter(|s| !s.0.is_empty()).collect();
    let parts: Vec<Part> = solids.iter().map(|s| Part::of(s.0, s.1)).collect();
    let toward = pj.toward();
    let mut tris = Vec::new();
    for i in paint_order(&parts, pj) {
        let (ts, _, base) = solids[i];
        for t in ts {
            let color = shade(base, normal(t), toward);
            let (p0, d0) = pj.project(t[0]);
            let (p1, d1) = pj.project(t[1]);
            let (p2, d2) = pj.project(t[2]);
            tris.push(Tri { p: [p0, p1, p2], c: [color; 3], depth: (d0 + d1 + d2) / 3.0 });
        }
        flush(painter, &mut tris);
    }
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
    let o = rect.left_bottom() + vec2(60.0, -60.0);
    painter.circle(o, 50.0, Color32::from_white_alpha(225), Stroke::new(1.0, theme::STROKE));
    for (dir, name, col) in [([1.0, 0.0, 0.0], "X", charts::RED), ([0.0, 1.0, 0.0], "Y", charts::GREEN), ([0.0, 0.0, 1.0], "Z", charts::BLUE)] {
        let s = vec2(pj.r[0][0] * dir[0] + pj.r[0][1] * dir[1] + pj.r[0][2] * dir[2], -(pj.r[1][0] * dir[0] + pj.r[1][1] * dir[1] + pj.r[1][2] * dir[2]));
        painter.line_segment([o, o + s * 32.0], Stroke::new(3.0, col));
        painter.circle_filled(o + s * 32.0, 3.0, col);
        painter.text(o + s * 42.0, Align2::CENTER_CENTER, name, theme::bold(13.0), col);
    }
}

/// How far below a model its floor lies: on the ground plane if it has one, otherwise a little
/// under it, so its shadow shows apart from it.
fn floor_of(g: &Geometry, extent: f64) -> f64 {
    if g.ground { 0.0 } else { g.bounds().0[2] - extent * 0.35 }
}

/// A round grid step for a model this big.
fn floor_step(extent: f64) -> f64 {
    let raw = (extent / 5.0).max(1e-9);
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|k| k * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

/// The model's shadow straight down on the floor: what shows where it stands, in a view
/// with no perspective to tell.
fn shadow(painter: &egui::Painter, pj: &Projector, tess: &[Tessellated], floor: f64) {
    const MOST_TRIANGLES: usize = 20_000;
    let on_floor = |p: P3| pj.project([p[0], p[1], floor]).0;
    let dark = Color32::from_black_alpha(26);
    let mut mesh = Mesh::default();
    if tess.iter().map(|(t, _)| t.len()).sum::<usize>() <= MOST_TRIANGLES {
        // Only one side of a closed solid, or it would count twice.
        for t in tess.iter().flat_map(|(t, _)| t).filter(|t| normal(t)[2] >= 0.0) {
            let n = mesh.vertices.len() as u32;
            for p in t {
                mesh.colored_vertex(on_floor(*p), dark);
            }
            mesh.add_triangle(n, n + 1, n + 2);
        }
    }
    if !mesh.is_empty() {
        painter.add(EShape::mesh(mesh));
    }
    for l in tess.iter().flat_map(|(_, l)| l) {
        painter.line_segment([on_floor(l[0]), on_floor(l[1])], Stroke::new(2.5, Color32::from_black_alpha(55)));
    }
}

/// A wire's colour at a depth: nearer is brighter, `near` from 0 to 1.
fn wire_at(near: f32) -> Color32 {
    let mix = |far: u8, close: u8| (far as f32 + (close as f32 - far as f32) * near) as u8;
    Color32::from_rgb(mix(104, 196), mix(70, 136), mix(26, 52))
}

/// The grid's spacing as the canvas says it.
fn length_label(metres: f64) -> String {
    let mm = metres * 1e3;
    if mm >= 1000.0 {
        format!("{} m", mm / 1000.0)
    } else if mm >= 0.1 {
        format!("{} mm", (mm * 1000.0).round() / 1000.0)
    } else {
        format!("{} µm", (mm * 1e3).round())
    }
}

/// How close a grid's lines may land on the screen, in points: any closer and they read as hatching.
const GRID_MIN_GAP: f32 = 12.0;
/// The sine of the angle under which a plane is too edge-on to show a grid at all.
const GRID_EDGE_ON: f32 = 0.05;
/// A grid line's colour where it shows in full.
const GRID_INK: Color32 = Color32::from_rgb(170, 177, 188);
/// How far a floor reaches from the middle of the model, in model radii.
const FLOOR_REACH: f64 = 1.5;
/// A ground plane's tint: the floor it stands on is metal.
const GROUND_TINT: f32 = 0.24;

/// The opaque colour `t` of the way from `a` to `b`. A grid is the first thing on the canvas, so
/// it can mix with what's under it: blending would happen in linear light, where it barely shows.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// How many points apart a plane's grid lines land per metre of spacing, in whichever of its two
/// directions they're closer. Seen at a low angle a plane squashes, which metres alone can't say.
fn grid_gap(pj: &Projector, axis: usize) -> f32 {
    let along = |k: usize| vec2(pj.r[0][k], pj.r[1][k]).length();
    pj.r[2][axis].abs() / along((axis + 1) % 3).max(along((axis + 2) % 3)).max(1e-6) * pj.scale
}

/// The spacing to draw at: `step` times a power of five, the finest whose lines stay apart.
fn grid_every(step: f64, gap: f32) -> f64 {
    let mut every = step.max(1e-9);
    while every * (gap.max(1e-6) as f64) < GRID_MIN_GAP as f64 {
        every *= 5.0;
    }
    every
}

/// How strongly a line shows, by how far it is from the next line of its own set. Lines fade in as
/// room opens up and out as it closes, so nothing pops when the spacing changes.
fn grid_ink(apart: f32) -> f32 {
    ((apart - GRID_MIN_GAP) / (GRID_MIN_GAP * 20.0)).clamp(0.0, 1.0).sqrt()
}

/// Which coarser set a line also belongs to, as a multiple of the spacing: every fifth line is
/// stronger, every twenty-fifth stronger still. Past that, `grid_ink` has reached its full strength.
fn grid_rank(i: i64) -> f32 {
    if i % 25 == 0 {
        25.0
    } else if i % 5 == 0 {
        5.0
    } else {
        1.0
    }
}

/// How much of a floor shows this fraction of its radius out: all of it around the model, none at the rim.
fn floor_fade(out: f32) -> f32 {
    let t = ((1.0 - out) / 0.6).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The part of a plane a grid covers, in the plane's own two coordinates.
enum Reach {
    /// All of this box: a drawing can go anywhere on the canvas.
    Whole([f64; 2], [f64; 2]),
    /// A disc around the model, fading out to its rim. With edges, a low view reads as a floor
    /// under the model, not as a sheet of lines the size of the canvas.
    Disc([f64; 2], f64),
}

/// One grid line: `at` along coordinate `k`, running from `from` to `to` along the other.
struct GridLine {
    k: usize,
    i: i64,
    at: f64,
    from: f64,
    to: f64,
}

fn grid_lines(every: f64, reach: &Reach) -> Vec<GridLine> {
    let (lo, hi) = match reach {
        Reach::Whole(lo, hi) => (*lo, *hi),
        Reach::Disc(o, r) => ([o[0] - r, o[1] - r], [o[0] + r, o[1] + r]),
    };
    let mut lines = Vec::new();
    for k in 0..2 {
        let first = (lo[k] / every).floor() as i64;
        let last = (hi[k] / every).ceil() as i64;
        if last - first > 600 {
            continue;
        }
        for i in first..=last {
            let at = i as f64 * every;
            let (from, to) = match reach {
                Reach::Whole(..) => (lo[1 - k], hi[1 - k]),
                Reach::Disc(o, r) => {
                    let off = at - o[k];
                    if off.abs() >= *r {
                        continue;
                    }
                    let half = (r * r - off * off).sqrt();
                    (o[1 - k] - half, o[1 - k] + half)
                }
            };
            lines.push(GridLine { k, i, at, from, to });
        }
    }
    lines
}

/// A straight line in a mesh, its colour varying along it as a stroke's can't. `colors_at` gives the
/// line's colour and the opaque colour under it, which its edges feather into. Feathering to
/// transparent would turn any pale line the same grey: the window blends in linear light, and a
/// vertex colour fading to nothing passes through dark on the way.
fn ribbon(mesh: &mut Mesh, a: Pos2, b: Pos2, width: f32, feather: f32, stops: u32, colors_at: impl Fn(f32) -> (Color32, Color32)) {
    let across = (b - a).normalized().rot90();
    let inner = (width.max(feather) - feather) / 2.0;
    let outer = inner + feather;
    let first = mesh.vertices.len() as u32;
    for s in 0..=stops {
        let t = s as f32 / stops as f32;
        let (p, (color, under)) = (a + (b - a) * t, colors_at(t));
        for (off, c) in [(-outer, under), (-inner, color), (inner, color), (outer, under)] {
            mesh.colored_vertex(p + across * off, c);
        }
    }
    for s in 0..stops {
        let row = first + s * 4;
        for q in row..row + 3 {
            mesh.add_triangle(q, q + 1, q + 4);
            mesh.add_triangle(q + 1, q + 5, q + 4);
        }
    }
}

/// A disc of colour on a plane, `color_at` taking the fraction of the radius.
fn disc(mesh: &mut Mesh, center: [f64; 2], radius: f64, on_plane: impl Fn([f64; 2]) -> Pos2, color_at: impl Fn(f32) -> Color32) {
    const RINGS: [f32; 6] = [0.0, 0.4, 0.55, 0.7, 0.85, 1.0];
    const AROUND: u32 = 64;
    let first = mesh.vertices.len() as u32;
    for out in RINGS {
        for a in 0..AROUND {
            let (s, c) = (a as f64 / AROUND as f64 * std::f64::consts::TAU).sin_cos();
            let r = radius * out as f64;
            mesh.colored_vertex(on_plane([center[0] + r * c, center[1] + r * s]), color_at(out));
        }
    }
    for ring in 0..RINGS.len() as u32 - 1 {
        let (inside, outside) = (first + ring * AROUND, first + (ring + 1) * AROUND);
        for a in 0..AROUND {
            let b = (a + 1) % AROUND;
            mesh.add_triangle(inside + a, inside + b, outside + a);
            mesh.add_triangle(inside + b, outside + b, outside + a);
        }
    }
}

/// Paints a grid on the plane `(axis, height, step)`, on a tint if it has one: lines at a spacing
/// that stays readable on the screen, and the plane's axes in their colours. Returns the spacing to
/// tell the user, if there's one to tell. Looking along the plane, nothing is painted.
fn plane_grid(painter: &egui::Painter, pj: &Projector, (axis, at, step): (usize, f64, f64), reach: Reach, tint: Option<Color32>, show_lines: bool) -> Option<f64> {
    let facing = pj.r[2][axis].abs();
    if facing < GRID_EDGE_ON {
        return None;
    }
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let gap = grid_gap(pj, axis);
    let every = grid_every(step, gap);
    // Fades away as the plane turns edge-on, rather than vanishing all at once.
    let shown = ((facing - GRID_EDGE_ON) / 0.2).clamp(0.0, 1.0);
    let on_plane = |c: [f64; 2]| {
        let mut p = [0.0; 3];
        p[axis] = at;
        p[u] = c[0];
        p[v] = c[1];
        pj.screen(p)
    };
    let fade = |c: [f64; 2]| match &reach {
        Reach::Whole(..) => 1.0,
        Reach::Disc(o, r) => floor_fade(((c[0] - o[0]).hypot(c[1] - o[1]) / r) as f32),
    };
    let surface = |f: f32| tint.map_or(theme::BG, |t| mix(theme::BG, t, f * shown));

    let mut mesh = Mesh::default();
    if let (Some(_), Reach::Disc(o, r)) = (tint, &reach) {
        disc(&mut mesh, *o, *r, on_plane, |out| surface(floor_fade(out)));
    }
    let stops = if matches!(reach, Reach::Disc(..)) { 16 } else { 1 };
    let feather = 1.0 / painter.ctx().pixels_per_point();
    let axis_colors = [charts::RED, charts::GREEN, charts::BLUE];
    // Weakest first: they're opaque, so where lines cross the strongest stays on top.
    let mut lines = if show_lines { grid_lines(every, &reach) } else { Vec::new() };
    lines.sort_by(|a, b| (a.i == 0, grid_rank(a.i)).partial_cmp(&(b.i == 0, grid_rank(b.i))).unwrap_or(std::cmp::Ordering::Equal));
    for l in &lines {
        // The line through the origin runs along the plane's other axis.
        let (color, ink, width) = if l.i == 0 { (axis_colors[if l.k == 0 { v } else { u }], 0.7, 1.5) } else { (GRID_INK, grid_ink(grid_rank(l.i) * gap * every as f32), 0.8) };
        if ink * shown < 0.01 {
            continue;
        }
        let point = |along: f64| if l.k == 0 { [l.at, along] } else { [along, l.at] };
        ribbon(&mut mesh, on_plane(point(l.from)), on_plane(point(l.to)), width, feather, stops, |t| {
            let f = fade(point(l.from + (l.to - l.from) * t as f64));
            let under = surface(f);
            (mix(under, color, ink * shown * f), under)
        });
    }
    if !mesh.is_empty() {
        painter.add(EShape::mesh(mesh));
    }
    if show_lines { stated_spacing(every, gap, &reach) } else { None }
}

/// The spacing to tell the user: lines still fading in don't read as the grid yet, so the set around
/// them does. A floor seen from far off or low down may have no such lines on it to measure by.
fn stated_spacing(every: f64, gap: f32, reach: &Reach) -> Option<f64> {
    let spacing = if grid_ink(gap * every as f32) < 0.25 { every * 5.0 } else { every };
    match reach {
        Reach::Disc(_, r) if spacing >= *r => None,
        _ => Some(spacing),
    }
}

/// The drawing plane's grid across the whole view.
fn drawing_grid(painter: &egui::Painter, pj: &Projector, rect: Rect, axis: usize, at: f64, step: f64) -> Option<f64> {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let corners = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for c in corners {
        let (o, d) = pj.ray(c);
        if d[axis].abs() < GRID_EDGE_ON as f64 {
            return None;
        }
        let t = (at - o[axis]) / d[axis];
        for (k, a) in [u, v].into_iter().enumerate() {
            let q = o[a] + d[a] * t;
            lo[k] = lo[k].min(q);
            hi[k] = hi[k].max(q);
        }
    }
    plane_grid(painter, pj, (axis, at, step), Reach::Whole(lo, hi), None, true)
}

/// A floor under the model, its grid finer than `floor_step` so it has lines to show zoomed in.
/// An infinite ground plane is the floor, tinted to say it's metal.
fn floor_grid(painter: &egui::Painter, pj: &Projector, center: P3, radius: f64, (at, step): (f64, f64), ground: bool, show_lines: bool) -> Option<f64> {
    let tint = ground.then(|| mix(theme::BG, METAL, GROUND_TINT));
    plane_grid(painter, pj, (2, at, step / 25.0), Reach::Disc([center[0], center[1]], radius), tint, show_lines)
}

/// Draws the scene over all of `canvas`, framed on `rect`, the part the cards leave
/// free: a zoomed model runs on under the cards. Dragging orbits, right-drag pans,
/// the wheel zooms. Returns the canvas's response and how it projects, for a tool on top.
pub fn show(ui: &mut egui::Ui, canvas: Rect, rect: Rect, cam: &mut Camera, scene: &Scene) -> (egui::Response, Projector) {
    let id = ui.make_persistent_id("antenna_view3d");
    let resp = ui.interact(canvas, id, Sense::click_and_drag());
    // While drawing, the left button is the tool's, so the middle one orbits instead of panning.
    let middle = resp.dragged_by(egui::PointerButton::Middle);
    if (resp.dragged_by(egui::PointerButton::Primary) && (!scene.drawing || ui.input(|i| i.modifiers.alt))) || (scene.drawing && middle) {
        let d = resp.drag_delta();
        cam.yaw -= d.x * 0.008;
        cam.pitch = (cam.pitch + d.y * 0.008).clamp(-1.55, 1.55);
    }
    if resp.dragged_by(egui::PointerButton::Secondary) || (middle && !scene.drawing) {
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
    let mut tris: Vec<Tri> = Vec::new();

    // A drawing plane, or else a floor under the model: what the eye judges it against.
    let drawn_step = if let Some((axis, at, step)) = scene.grid {
        drawing_grid(&painter, &pj, canvas, axis, at, step)
    } else if let Some(g) = scene.geometry.filter(|g| !g.solids.is_empty()) {
        let floor = floor_of(g, extent);
        // Over the FDTD mesh, the solver's own lines are the ones to read.
        let fdtd_mesh = scene.mode == ViewMode::Mesh && scene.mom_mesh.is_none() && scene.plan.is_some();
        let spacing = floor_grid(&painter, &pj, center, view_extent * FLOOR_REACH, (floor, floor_step(extent)), g.ground, !fdtd_mesh);
        if pj.r[2][2].abs() >= GRID_EDGE_ON {
            shadow(&painter, &pj, scene.tess, floor);
        }
        spacing
    } else {
        None
    };
    if let Some(step) = drawn_step {
        painter.text(rect.left_bottom() + vec2(120.0, -16.0), Align2::LEFT_CENTER, format!("Lines every {}", length_label(step)), FontId::proportional(12.5), theme::WEAK);
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
        paint_solids(
            &painter,
            &pj,
            g.solids.iter().zip(scene.tess).map(|(s, (ts, _))| {
                let base = solid_color(&s.material);
                (ts.as_slice(), is_dielectric(&s.material), if scene.transparent { see_through(base) } else { base })
            }),
        );
        for (s, (_, ls)) in g.solids.iter().zip(scene.tess) {
            if let Shape::Wire { r, .. } = &s.shape {
                let w = (*r as f32 * pj.scale * 2.0).clamp(2.5, 10.0);
                for l in ls {
                    let (a, da) = pj.project(l[0]);
                    let (b, db) = pj.project(l[1]);
                    // Nearer is brighter and thicker: the depth this view has no perspective to show.
                    let depth = (da + db) / 2.0;
                    let near = (depth / view_extent.max(1e-9) as f32 * 0.5 + 0.5).clamp(0.0, 1.0);
                    lines.push((a, b, w * (0.8 + 0.5 * near), wire_at(near), depth));
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
                // On a plate of metal a bare label is hard to read.
                let g = painter.layout_no_wrap(label.clone(), FontId::proportional(10.5), *color);
                let at = m + vec2(8.0, -g.size().y / 2.0);
                painter.rect_filled(egui::Rect::from_min_size(at, g.size()).expand(2.0), 3.0, Color32::from_white_alpha(225));
                painter.galley(at, g, *color);
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
    let tess: Vec<Tessellated> = geom.solids.iter().map(|s| tessellate(&s.shape)).collect();
    paint_solids(&painter, &pj, geom.solids.iter().zip(&tess).map(|(s, (ts, _))| (&ts[..ts.len().min(1500)], is_dielectric(&s.material), solid_color(&s.material))));
    for l in tess.iter().flat_map(|(_, ls)| ls) {
        painter.line_segment([pj.project(l[0]).0, pj.project(l[1]).0], Stroke::new(1.6, WIRE));
    }
    for port in &geom.ports {
        let m = pj.project(port.center()).0;
        painter.circle_filled(m, 2.5, theme::DANGER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_grid_takes_a_round_step_near_a_fifth_of_the_model() {
        for (extent, step) in [(0.05, 0.01), (0.024, 0.005), (0.3, 0.1), (1.2, 0.5)] {
            assert!((floor_step(extent) - step).abs() < 1e-12, "{extent}: {}", floor_step(extent));
        }
    }

    #[test]
    fn grid_spacings_read_in_the_unit_that_suits_them() {
        assert_eq!(length_label(0.005), "5 mm");
        assert_eq!(length_label(0.0005), "0.5 mm");
        assert_eq!(length_label(0.00005), "50 µm");
        assert_eq!(length_label(1.5), "1.5 m");
    }

    /// A view from this angle, at this many points to the millimetre.
    fn view_from(yaw: f32, pitch: f32, points_per_mm: f32) -> Projector {
        Projector::new(&Camera { yaw, pitch, zoom: points_per_mm, pan: egui::Vec2::ZERO }, Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 1000.0)), [0.0; 3], 0.42)
    }

    /// How far apart neighbouring lines of the floor land on the screen, measured from where
    /// they're drawn: (lines of constant x, lines of constant y).
    fn measured_gaps(pj: &Projector, every: f64) -> (f32, f32) {
        let apart = |a: P3, b: P3, start: P3| {
            let (a, b, s) = (pj.screen(a), pj.screen(b), pj.screen(start));
            let along = (b - a).normalized();
            let off = s - a;
            (off.x * along.y - off.y * along.x).abs()
        };
        (apart([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [every, 0.0, 0.0]), apart([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, every, 0.0]))
    }

    /// The reported view: orbited to 9° above the floor, 4.9 points to the millimetre. Spacing was
    /// judged in metres, so the 5 mm lines landed under 4 points apart, a hatching over the canvas.
    #[test]
    fn a_low_view_thins_the_grid_by_what_lands_on_the_screen() {
        let pj = view_from(-0.14, 0.154, 4.9);
        let every = grid_every(0.005 / 25.0, grid_gap(&pj, 2));
        let (x, y) = measured_gaps(&pj, every);
        assert!(x.min(y) >= GRID_MIN_GAP, "{every} m lands {x} and {y} points apart");
        let (x, y) = measured_gaps(&pj, 0.005);
        assert!(x.min(y) < GRID_MIN_GAP, "the old spacing was too close here: {x} and {y}");
    }

    #[test]
    fn the_gap_is_what_the_lines_measure_on_the_screen() {
        for (yaw, pitch) in [(0.65, 0.42), (-0.14, 0.154), (0.0, std::f32::consts::FRAC_PI_2), (1.1, -0.8), (2.5, 1.2)] {
            let pj = view_from(yaw, pitch, 10.0);
            let (x, y) = measured_gaps(&pj, 1.0);
            let gap = grid_gap(&pj, 2);
            assert!((x.min(y) - gap).abs() < gap * 1e-3, "yaw {yaw}, pitch {pitch}: {x}, {y} against {gap}");
        }
    }

    /// Looking along the plane, no spacing is far enough apart: it must still return.
    #[test]
    fn looking_along_the_plane_does_not_hang_the_spacing() {
        assert_eq!(grid_gap(&view_from(0.0, 0.0, 1.0), 2), 0.0);
        assert!(grid_every(0.001, 0.0).is_finite());
    }

    /// When the spacing changes, every line that stays must keep its strength, and those that go
    /// must already have faded out.
    #[test]
    fn nothing_pops_when_the_spacing_changes() {
        let gap = GRID_MIN_GAP;
        assert_eq!(grid_ink(gap), 0.0, "the lines about to go");
        for i in -130..130_i64 {
            let (before, after) = (grid_ink(grid_rank(i * 5) * gap), grid_ink(grid_rank(i) * gap * 5.0));
            assert!((before - after).abs() < 1e-5, "line {i}: {before} then {after}");
        }
        assert!(grid_ink(5.0 * gap) < grid_ink(25.0 * gap), "a coarser set is stronger");
    }

    /// The canvas said "Lines every 125 mm" under a floor 44 mm across.
    #[test]
    fn the_canvas_only_states_a_spacing_it_has_lines_for() {
        let floor = Reach::Disc([0.0, 0.0], 0.022);
        assert_eq!(stated_spacing(0.025, GRID_MIN_GAP * 40.0, &floor), None, "wider than the floor");
        assert_eq!(stated_spacing(0.004, GRID_MIN_GAP * 500.0, &floor), Some(0.02), "lines still fading in: the coarser set");
        assert_eq!(stated_spacing(0.004, GRID_MIN_GAP * 2500.0, &floor), Some(0.004), "lines that show");
        let whole = Reach::Whole([-0.01, -0.01], [0.01, 0.01]);
        assert_eq!(stated_spacing(0.025, GRID_MIN_GAP * 40.0, &whole), Some(0.125), "a drawing plane goes on past its box");
    }

    #[test]
    fn a_floor_is_full_around_the_model_and_gone_at_its_rim() {
        assert_eq!(floor_fade(0.0), 1.0);
        assert_eq!(floor_fade(0.4), 1.0);
        assert_eq!(floor_fade(1.0), 0.0);
        assert!(floor_fade(0.6) > floor_fade(0.8));
    }

    #[test]
    fn a_floors_lines_stay_on_it() {
        let (o, r) = ([0.012, -0.004], 0.03);
        let lines = grid_lines(0.005, &Reach::Disc(o, r));
        assert!(!lines.is_empty());
        for l in &lines {
            for along in [l.from, l.to] {
                let p = if l.k == 0 { [l.at, along] } else { [along, l.at] };
                assert!((p[0] - o[0]).hypot(p[1] - o[1]) <= r * (1.0 + 1e-9), "{p:?}");
            }
        }
        // Both directions, and only the lines that cross the disc.
        for k in 0..2 {
            let count = lines.iter().filter(|l| l.k == k).count();
            assert!(count > 0 && count <= (2.0 * r / 0.005) as usize + 1, "{count} lines of direction {k}");
        }
    }

    #[test]
    fn a_drawing_grid_covers_its_whole_box() {
        let (lo, hi) = ([-0.02, -0.01], [0.02, 0.01]);
        let lines = grid_lines(0.005, &Reach::Whole(lo, hi));
        assert!(lines.iter().all(|l| l.from == lo[1 - l.k] && l.to == hi[1 - l.k]));
        assert_eq!(lines.iter().filter(|l| l.k == 0).count(), 9);
        assert_eq!(lines.iter().filter(|l| l.k == 1).count(), 5);
    }

    /// Edges that fade to transparent turned every pale line the same grey in the window, so the
    /// floor's fade didn't show and it ended in a hard, stepped rim.
    #[test]
    fn a_ribbon_feathers_into_the_opaque_colour_under_it() {
        let mut mesh = Mesh::default();
        ribbon(&mut mesh, pos2(0.0, 0.0), pos2(100.0, 0.0), 1.0, 0.5, 4, |_| (Color32::RED, Color32::WHITE));
        ribbon(&mut mesh, pos2(0.0, 5.0), pos2(100.0, 5.0), 1.0, 0.5, 1, |_| (Color32::BLUE, Color32::WHITE));
        assert!(mesh.is_valid());
        assert_eq!(mesh.vertices.len(), 5 * 4 + 2 * 4);
        assert_eq!(mesh.indices.len(), (4 + 1) * 3 * 2 * 3);
        assert_eq!(mesh.vertices[0].color, Color32::WHITE);
        assert_eq!(mesh.vertices[1].color, Color32::RED);
        assert_eq!(mesh.vertices[3].color, Color32::WHITE);
        assert!(mesh.vertices.iter().all(|v| v.color.a() == 255));
    }

    /// A floor under a model, unless it has a ground plane to stand on.
    #[test]
    fn a_model_stands_on_its_ground_plane_or_floats_above_the_floor() {
        let mut g = Geometry::default();
        g.solids.push(antenna_core::geometry::Solid { name: "w".into(), material: "PEC".into(), shape: Shape::Wire { pts: vec![[0.0, 0.0, 0.01], [0.0, 0.0, 0.03]], r: 1e-3 } });
        assert!(floor_of(&g, 0.01) < 0.01, "below its lowest point, so its shadow shows");
        g.ground = true;
        assert_eq!(floor_of(&g, 0.01), 0.0);
    }

    /// What a colour comes out as on the canvas, blended in linear light as the window does.
    fn on_canvas(c: Color32) -> [i32; 3] {
        use egui::ecolor::{gamma_u8_from_linear_f32, linear_f32_from_gamma_u8};
        let (src, bg, keep) = (c.to_array(), theme::BG.to_array(), 1.0 - c.a() as f32 / 255.0);
        std::array::from_fn(|k| gamma_u8_from_linear_f32(linear_f32_from_gamma_u8(src[k]) + linear_f32_from_gamma_u8(bg[k]) * keep) as i32)
    }

    /// See-through solids keep their colour on the canvas: metal stays warm and FR-4 stays
    /// green. They used to come out grey, measured at (207,206,204) for a metal plate.
    #[test]
    fn see_through_solids_keep_their_colour() {
        let facing = [0.0, 0.0, 1.0];

        let [r, g, b] = on_canvas(shade(see_through(METAL), facing, facing));
        assert!(r > g && g > b && r - b > 30, "metal is warm: {r} {g} {b}");

        let [r, g, b] = on_canvas(shade(solid_color("FR-4"), facing, facing));
        assert!(g - r > 12, "FR-4 is green: {r} {g} {b}");
        let [r, g, b] = on_canvas(shade(see_through(solid_color("FR-4")), facing, facing));
        assert!(g - r > 12, "see-through too: {r} {g} {b}");

        // A colour within reach lands where it was aimed, to within rounding.
        let [r, g, b] = on_canvas(translucent([230, 200, 180], 128));
        let bg = theme::BG.to_array();
        let a = 128.0 / 255.0;
        let want = |c: f32, k: usize| (c * a + bg[k] as f32 * (1.0 - a)) as i32;
        assert!((r - want(230.0, 0)).abs() <= 2 && (g - want(200.0, 1)).abs() <= 2 && (b - want(180.0, 2)).abs() <= 2, "{r} {g} {b}");

        // Solid stays solid, and facing away is darker, not another colour.
        assert_eq!(shade(METAL, facing, facing).a(), 255);
        let (away, front) = (shade(METAL, [1.0, 0.0, 0.0], facing), shade(METAL, facing, facing));
        assert!(away.r() < front.r() && away.g() < front.g());
    }

    /// A view at this pitch: positive is from above, negative from below.
    fn looking(pitch: f32) -> Projector {
        Projector::new(&Camera { yaw: 0.65, pitch, zoom: 1.0, pan: egui::Vec2::ZERO }, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 800.0)), [0.0; 3], 0.1)
    }

    fn part(lo: P3, hi: P3, dielectric: bool) -> Part {
        Part { lo, hi, dielectric }
    }

    /// A ceramic puck on its ground plate was always painted first, so the plate hid it. The plate
    /// is listed second in that template, and metal or not must not decide it.
    #[test]
    fn a_part_on_a_plate_is_painted_over_it_only_from_its_own_side() {
        let plate = part([-0.05, -0.05, 0.0], [0.05, 0.05, 0.0], false);
        let puck = part([-0.01, -0.01, 0.0], [0.01, 0.01, 0.02], true);
        assert_eq!(paint_order(&[puck, plate], &looking(0.42)), [1, 0], "from above, the plate first");
        assert_eq!(paint_order(&[plate, puck], &looking(0.42)), [0, 1], "listed the other way round");
        assert_eq!(paint_order(&[puck, plate], &looking(-0.42)), [0, 1], "from below, the puck first");
    }

    /// Faces that touch must get an answer from the geometry: a depth tie would flicker.
    #[test]
    fn a_stack_of_layers_paints_in_stack_order_from_either_side() {
        let ground = part([-0.04, -0.04, 0.0], [0.04, 0.04, 0.0], false);
        let substrate = part([-0.04, -0.04, 0.0], [0.04, 0.04, 0.0016], true);
        let patch = part([-0.02, -0.02, 0.0016], [0.02, 0.02, 0.0016], false);
        let layers = [patch, ground, substrate];
        assert_eq!(paint_order(&layers, &looking(0.42)), [1, 2, 0], "ground, substrate, patch");
        assert_eq!(paint_order(&layers, &looking(-0.42)), [0, 2, 1], "patch, substrate, ground");
    }

    #[test]
    fn a_part_sunk_into_a_thick_plate_still_stands_in_front_of_it() {
        let plate = part([-0.05, -0.05, -0.002], [0.05, 0.05, 0.0], false);
        let puck = part([-0.01, -0.01, -0.001], [0.01, 0.01, 0.02], true);
        assert_eq!(paint_order(&[puck, plate], &looking(0.42)), [1, 0]);
    }

    /// Through the middle of a plate, nothing says which is nearer: the small part stays in sight.
    #[test]
    fn a_probe_through_a_plate_stays_in_sight() {
        let plate = part([-0.05, -0.05, -0.001], [0.05, 0.05, 0.001], false);
        let probe = part([-0.001, -0.001, -0.01], [0.001, 0.001, 0.01], false);
        assert_eq!(paint_order(&[probe, plate], &looking(0.42)), [1, 0]);
    }

    /// Whatever the arrangement, rings included, every part is painted exactly once.
    #[test]
    fn every_part_is_painted_once() {
        let at = |i: usize, m: usize| (i * m % 97) as f64 / 97.0 * 0.05;
        let parts: Vec<Part> = (0..200).map(|i| part([at(i, 13), at(i, 29), at(i, 53)], [at(i, 13) + 0.03, at(i, 29) + 0.002, at(i, 53) + 0.01], i % 3 == 0)).collect();
        let mut order = paint_order(&parts, &looking(0.42));
        order.sort_unstable();
        assert_eq!(order, (0..200).collect::<Vec<_>>());
    }

    /// Too many to compare every frame: it falls back to dielectrics first, and doesn't stall.
    #[test]
    fn a_huge_model_paints_its_dielectrics_first() {
        let parts: Vec<Part> = (0..MOST_ORDERED + 1).map(|i| part([i as f64 * 0.01, 0.0, 0.0], [i as f64 * 0.01 + 0.005, 0.01, 0.0], i % 2 == 1)).collect();
        let order = paint_order(&parts, &looking(0.42));
        let dielectrics = order.iter().take_while(|i| parts[**i].dielectric).count();
        assert_eq!(dielectrics, parts.iter().filter(|p| p.dielectric).count());
        assert_eq!(order.len(), parts.len());
    }

    /// The named views really look along what they say.
    #[test]
    fn named_views_look_where_they_say() {
        let look = |name: &str| {
            let (_, _, yaw, pitch) = Camera::VIEWS.into_iter().find(|v| v.0 == name).unwrap();
            Projector::new(&Camera { yaw, pitch, zoom: 1.0, pan: egui::Vec2::ZERO }, Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0)), [0.0; 3], 1.0).toward()
        };
        let along = |v: [f64; 3], axis: usize, sign: f64| (v[axis] * sign - 1.0).abs() < 1e-6;
        assert!(along(look("Top"), 2, 1.0), "from above");
        assert!(along(look("Front"), 1, -1.0), "from -y");
        assert!(along(look("Side"), 0, 1.0), "from +x");
    }
}
