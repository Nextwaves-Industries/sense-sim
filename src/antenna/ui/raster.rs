//! A depth buffer for the Studio's canvas. Painting whole solids back to front can't get an
//! intersection or a part through a hole right, and a CAD view has plenty of both, so the model
//! is rasterised here, band by band across the cores, into a texture the canvas shows.
//!
//! The same pass gives what the tools need: which triangle is under the pointer, how deep it is,
//! and whether an edge is in sight. Edges are then drawn as crisp lines over the picture.

use std::sync::Arc;

use antenna_core::geometry::{P3, add, cross, dot, scale, sub};
use antenna_core::modeler::mesh::Mesh;
use bevy_egui::egui::{self, Color32, Pos2, Rect};
use rayon::prelude::*;

use super::view3d::Projector;

/// One thing on the canvas: a body, or a part from a template.
#[derive(Clone, Debug, Default)]
pub struct Part {
    pub mesh: Arc<Mesh>,
    /// A normal per triangle corner.
    pub normals: Vec<[P3; 3]>,
    /// Lines to draw: rims, creases, borders between faces.
    pub edges: Vec<[P3; 2]>,
    /// The two faces each of those lies between. The same one twice for an open rim.
    pub edge_faces: Vec<[u32; 2]>,
    /// Edges inside a smooth surface, with the normals either side: a silhouette when they face
    /// opposite ways.
    pub soft: Vec<([P3; 2], P3, P3)>,
    pub rgb: [u8; 3],
    /// Below 255 it's see-through, and drawn after everything solid.
    pub alpha: u8,
    /// The modelled body it is, if any.
    pub body: Option<u32>,
    /// The part it is, by name, when it comes from a template or the flat tools.
    pub solid: Option<String>,
    /// A surface with no inside shows the same both ways.
    pub two_sided: bool,
}

impl Part {
    pub fn new(mesh: Arc<Mesh>, rgb: [u8; 3], alpha: u8, body: Option<u32>) -> Self {
        let normals = mesh.corner_normals(CREASE_DEG);
        let lines = mesh.feature_edges(CREASE_DEG);
        let mut hard: std::collections::HashSet<(u32, u32)> = lines.iter().map(|e| (e[0].min(e[1]), e[0].max(e[1]))).collect();
        let edges = lines.iter().map(|e| [mesh.verts[e[0] as usize], mesh.verts[e[1] as usize]]).collect();
        let mut first: std::collections::HashMap<(u32, u32), P3> = std::collections::HashMap::new();
        let mut soft = Vec::new();
        for t in 0..mesh.tris.len() {
            let Some(n) = mesh.normal(t) else { continue };
            let tri = mesh.tris[t];
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let key = (a.min(b), a.max(b));
                if hard.contains(&key) {
                    continue;
                }
                if let Some(m) = first.get(&key) {
                    soft.push(([mesh.verts[a as usize], mesh.verts[b as usize]], *m, n));
                    hard.insert(key);
                } else {
                    first.insert(key, n);
                }
            }
        }
        let mut sides: std::collections::HashMap<(u32, u32), [u32; 2]> = std::collections::HashMap::new();
        for (t, tri) in mesh.tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let f = mesh.face[t];
                sides.entry((a.min(b), a.max(b))).and_modify(|s| s[1] = f).or_insert([f, f]);
            }
        }
        let edge_faces = lines.iter().map(|e| sides.get(&(e[0].min(e[1]), e[0].max(e[1]))).copied().unwrap_or([0, 0])).collect();
        let two_sided = !mesh.is_closed();
        Self { mesh, normals, edges, edge_faces, soft, rgb, alpha, body, solid: None, two_sided }
    }

    fn tri_count(&self) -> usize {
        self.mesh.tris.len()
    }
}

/// How the canvas is to look: what's lit up, and what's cut away.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    /// Parts to tint, and faces, by part index and face name.
    pub selected_parts: Vec<usize>,
    pub selected_faces: Vec<u32>,
    pub hover_part: Option<usize>,
    pub hover_face: Option<u32>,
    pub see_through: bool,
    /// Everything on the positive side of this plane is left out: `(normal, offset)`.
    pub section: Option<([f32; 3], f32)>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Key {
    size: [usize; 2],
    view: [f32; 13],
    center: P3,
    look: Look,
}

/// What's under a point of the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub part: usize,
    pub tri: usize,
    /// In the projector's depth, larger is nearer.
    pub depth: f32,
}

#[derive(Default)]
pub struct Canvas {
    /// The picture's textures, and which pixels each holds: left, top, right, bottom.
    tiles: Vec<(egui::TextureHandle, [usize; 4])>,
    key: Option<Key>,
    size: [usize; 2],
    scale: f32,
    origin: Pos2,
    depth: Vec<f32>,
    /// The same with only what's opaque: a line behind glass is still in sight.
    solid: Vec<f32>,
    /// One more than the triangle's running number; 0 is nothing.
    ids: Vec<u32>,
    /// Each part's first running number.
    starts: Vec<u32>,
}

/// One triangle in pixels, with what varies over it.
#[derive(Clone, Copy)]
struct Prim {
    x: [f32; 3],
    y: [f32; 3],
    z: [f32; 3],
    c: [[f32; 3]; 3],
    s: [f32; 3],
    id: u32,
    alpha: u8,
}

const BAND: usize = 48;
/// Faces meeting at more than this show as a crease, with a line along it.
const CREASE_DEG: f64 = 38.0;

/// The eye's axes, for lighting that stays where it is while the model turns.
#[derive(Clone, Copy)]
struct Eye {
    right: P3,
    up: P3,
    toward: P3,
}

/// Studio light: a soft key from up and to the left, a weak fill from the other side, a little sky,
/// and for metal a highlight. Comes back as how much of the base colour shows, and how much white.
fn shade(n: P3, eye: &Eye, front_only: bool, gloss: f32) -> (f32, f32) {
    let facing = dot(n, eye.toward);
    // A surface with no inside is lit the same from either side.
    let flip = if !front_only && facing < 0.0 { -1.0 } else { 1.0 };
    let (x, y, z) = (dot(n, eye.right) * flip, dot(n, eye.up) * flip, facing * flip);
    let lit = |l: [f64; 3]| (x * l[0] + y * l[1] + z * l[2]).max(0.0);
    let key = lit([-0.42, 0.58, 0.70]);
    let fill = lit([0.62, -0.18, 0.76]);
    let sky = 0.5 + 0.5 * y;
    let body = 0.34 + 0.56 * key + 0.20 * fill + 0.10 * sky;
    // Where the key light's reflection comes back to the eye: the half-way direction.
    let half = lit([-0.24, 0.33, 0.91]);
    let shine = half.powi(36) as f32 * gloss;
    // The inside of a body, seen through a section, reads as being in shade.
    let dim = if front_only && facing < 0.0 { 0.55 } else { 1.0 };
    ((body * dim) as f32, shine * dim as f32)
}

/// A few threads of its own. Waking every thread of a big machine's shared pool costs far more than
/// a picture is worth: about 21 ms a frame on 112 threads, whatever the picture's size, which was most of
/// the time a picture took while orbiting (27 ms against 6).
fn workers() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|k| format!("canvas-{k}")).build().expect("the canvas's threads start")
    })
}

impl Canvas {
    /// Pixel of a canvas point.
    fn pixel(&self, p: Pos2) -> Option<usize> {
        let x = ((p.x - self.origin.x) * self.scale).floor();
        let y = ((p.y - self.origin.y) * self.scale).floor();
        if x < 0.0 || y < 0.0 || x >= self.size[0] as f32 || y >= self.size[1] as f32 {
            return None;
        }
        Some(y as usize * self.size[0] + x as usize)
    }

    /// The nearest thing under a canvas point.
    pub fn at(&self, p: Pos2) -> Option<Hit> {
        let i = self.pixel(p)?;
        let id = *self.ids.get(i)?;
        if id == 0 {
            return None;
        }
        let n = id - 1;
        let part = self.starts.partition_point(|s| *s <= n).checked_sub(1)?;
        Some(Hit { part, tri: (n - self.starts[part]) as usize, depth: self.depth[i] })
    }

    /// Whether a point at this depth can be seen, or something's in front of it. A line on a
    /// silhouette is on a surface so steep that the pixel under it can read as in front, so the
    /// pixels right around it get a say too.
    #[cfg(test)]
    pub fn sees(&self, p: Pos2, depth: f32, bias: f32) -> bool {
        self.clear_in(&self.depth, p, depth, bias, 0.0)
    }

    fn clear_in(&self, buffer: &[f32], p: Pos2, depth: f32, bias: f32, around: f32) -> bool {
        let step = around / self.scale.max(1e-3);
        [egui::vec2(0.0, 0.0), egui::vec2(step, 0.0), egui::vec2(-step, 0.0), egui::vec2(0.0, step), egui::vec2(0.0, -step)].iter().any(|d| match self.pixel(p + *d).and_then(|i| buffer.get(i)) {
            Some(v) => *v <= depth + bias,
            None => true,
        })
    }

    /// How a line at this point shows: in the clear, only behind something see-through, or hidden.
    fn sight(&self, p: Pos2, depth: f32, bias: f32, around: f32) -> u8 {
        if self.clear_in(&self.depth, p, depth, bias, around) {
            0
        } else if self.clear_in(&self.solid, p, depth, bias, around) {
            1
        } else {
            2
        }
    }

    /// Renders if anything changed, and paints the picture over `rect`.
    pub fn paint(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, parts: &[Part], pj: &Projector, look: &Look, hurry: bool) {
        let ppp = ctx.pixels_per_point();
        // While the view is moving, half the pixels: it's what makes a big model keep up.
        let scale = if hurry { (ppp * 0.5).max(1.0) } else if ppp < 1.5 { ppp * 2.0 } else { ppp };
        let size = [((rect.width() * scale).ceil() as usize).clamp(1, 8192), ((rect.height() * scale).ceil() as usize).clamp(1, 8192)];
        let key = Key { size, view: pj.signature(), center: pj.center(), look: look.clone() };
        if self.key.as_ref() != Some(&key) || self.tiles.is_empty() {
            self.origin = rect.min;
            self.scale = scale;
            self.size = size;
            let began = std::time::Instant::now();
            let pixels = workers().install(|| self.render(parts, pj, look));
            let rendered = began.elapsed();
            // A texture has a largest side, and a big canvas on a high-density screen passes it, so
            // the picture goes up in tiles that meet pixel for pixel. Nearest sampling is what keeps
            // their joins invisible; only the half-size picture of a moving view gets smoothed.
            let side = ctx.input(|i| i.max_texture_side).max(256);
            let spans = |n: usize| (0..n.div_ceil(side)).map(move |k| (k * side, ((k + 1) * side).min(n)));
            let options = if scale != ppp { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST };
            let mut used = 0;
            for (y0, y1) in spans(size[1]) {
                for (x0, x1) in spans(size[0]) {
                    let mut tile = Vec::with_capacity((x1 - x0) * (y1 - y0));
                    for row in y0..y1 {
                        tile.extend_from_slice(&pixels[row * size[0] + x0..row * size[0] + x1]);
                    }
                    let image = egui::ColorImage::new([x1 - x0, y1 - y0], tile);
                    let px = [x0, y0, x1, y1];
                    match self.tiles.get_mut(used) {
                        Some(t) => {
                            t.0.set(image, options);
                            t.1 = px;
                        }
                        None => self.tiles.push((ctx.load_texture(format!("antenna_canvas_{used}"), image, options), px)),
                    }
                    used += 1;
                }
            }
            self.tiles.truncate(used);
            self.key = Some(key);
            if std::env::var_os("SENSESIM_CANVAS_TIMING").is_some() {
                eprintln!("canvas {}x{} {} triangles: render {:.1} ms, handed over {:.1} ms", size[0], size[1], parts.iter().map(|p| p.mesh.tris.len()).sum::<usize>(), rendered.as_secs_f64() * 1e3, (began.elapsed() - rendered).as_secs_f64() * 1e3);
            }
        }
        // The next quiet frame comes back at full resolution.
        if hurry {
            ctx.request_repaint();
        }
        let whole = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        for (t, px) in &self.tiles {
            let at = |x: usize, y: usize| self.origin + egui::vec2(x as f32, y as f32) / self.scale;
            painter.image(t.id(), Rect::from_min_max(at(px[0], px[1]), at(px[2], px[3])), whole, Color32::WHITE);
        }
    }

    fn render(&mut self, parts: &[Part], pj: &Projector, look: &Look) -> Vec<Color32> {
        let [w, h] = self.size;
        let count = w * h;
        self.depth.clear();
        self.depth.resize(count, f32::NEG_INFINITY);
        self.ids.clear();
        self.ids.resize(count, 0);
        let mut pixels = vec![Color32::TRANSPARENT; count];

        self.starts.clear();
        let mut running = 0_u32;
        for p in parts {
            self.starts.push(running);
            running += p.tri_count() as u32;
        }
        let eye = Eye { right: pj.right(), up: pj.up(), toward: pj.toward() };
        let (origin, scale) = (self.origin, self.scale);
        let solid_prims = |want_solid: bool| -> Vec<Prim> {
            parts
                .par_iter()
                .enumerate()
                .filter(|(_, p)| (p.alpha == 255 && !look.see_through) == want_solid)
                .flat_map_iter(|(pi, p)| {
                    let base = tint_for(p, pi, look);
                    let start = self.starts[pi];
                    let mesh = &p.mesh;
                    (0..mesh.tris.len()).map(move |t| {
                        let tri = mesh.tris[t];
                        let (mut x, mut y, mut z, mut c, mut s) = ([0.0; 3], [0.0; 3], [0.0; 3], [[0.0; 3]; 3], [0.0; 3]);
                        let face = mesh.face[t];
                        let col = face_tint(base, p, pi, face, look);
                        // With a closed body cut open, any of its inside that can be seen is seen through the
                        // cut, so painting it all one flat colour makes it read as the cut face.
                        let cut_face = look.section.is_some() && !p.two_sided && {
                            let [a, b, c] = [mesh.verts[tri[0] as usize], mesh.verts[tri[1] as usize], mesh.verts[tri[2] as usize]];
                            dot(cross(sub(b, a), sub(c, a)), eye.toward) < 0.0
                        };
                        for k in 0..3 {
                            let q = mesh.verts[tri[k] as usize];
                            let (at, depth) = pj.project(q);
                            x[k] = (at.x - origin.x) * scale;
                            y[k] = (at.y - origin.y) * scale;
                            z[k] = depth;
                            let n = p.normals[t][k];
                            let (b, white) = shade(n, &eye, !p.two_sided, if p.alpha == 255 { 0.34 } else { 0.12 });
                            c[k] = if cut_face { [col[0] * CUT_FACE, col[1] * CUT_FACE, col[2] * CUT_FACE] } else { [col[0] * b + 255.0 * white, col[1] * b + 255.0 * white, col[2] * b + 255.0 * white] };
                            if let Some((m, d)) = look.section {
                                s[k] = m[0] * q[0] as f32 + m[1] * q[1] as f32 + m[2] * q[2] as f32 - d;
                            }
                        }
                        let alpha = if look.see_through { p.alpha.min(150) } else { p.alpha };
                        Prim { x, y, z, c, s, id: start + t as u32 + 1, alpha }
                    })
                })
                .collect()
        };
        let solid = solid_prims(true);
        let mut glass = solid_prims(false);

        // Opaque first, one band of rows per job, with only the triangles that touch it.
        let bands = h.div_ceil(BAND).max(1);
        let mut bins: Vec<Vec<u32>> = vec![Vec::new(); bands];
        for (i, t) in solid.iter().enumerate() {
            let (lo, hi) = (t.y[0].min(t.y[1]).min(t.y[2]), t.y[0].max(t.y[1]).max(t.y[2]));
            if hi < 0.0 || lo >= h as f32 {
                continue;
            }
            let (a, b) = ((lo.max(0.0) as usize) / BAND, (hi.min(h as f32 - 1.0) as usize) / BAND);
            for bin in &mut bins[a..=b.min(bands - 1)] {
                bin.push(i as u32);
            }
        }
        pixels
            .par_chunks_mut(w * BAND)
            .zip(self.depth.par_chunks_mut(w * BAND))
            .zip(self.ids.par_chunks_mut(w * BAND))
            .enumerate()
            .for_each(|(band, ((colors, depths), ids))| {
                let first = band * BAND;
                let rows = colors.len() / w;
                for &i in &bins[band] {
                    fill(&solid[i as usize], w, first, rows, colors, depths, ids);
                }
            });

        // See-through parts last, far to near, over whatever's behind them. They're laid on the
        // canvas's own colour, since blending with a window in linear light loses any faint tint.
        let bias = pj.depth_bias();
        self.solid.clear();
        self.solid.extend_from_slice(&self.depth);
        glass.sort_by(|a, b| (a.z[0] + a.z[1] + a.z[2]).total_cmp(&(b.z[0] + b.z[1] + b.z[2])));
        for t in &glass {
            blend(t, w, h, &mut pixels, &self.solid, &mut self.depth, &mut self.ids, bias);
        }
        pixels
    }
}

fn tint_for(p: &Part, index: usize, look: &Look) -> [f32; 3] {
    let base = p.rgb.map(|v| v as f32);
    let mix = |to: [f32; 3], t: f32| [base[0] + (to[0] - base[0]) * t, base[1] + (to[1] - base[1]) * t, base[2] + (to[2] - base[2]) * t];
    if look.selected_parts.contains(&index) {
        mix(SELECT, 0.62)
    } else if look.hover_part == Some(index) && look.hover_face.is_none() {
        mix([255.0, 255.0, 255.0], 0.2)
    } else {
        base
    }
}

const SELECT: [f32; 3] = [45.0, 110.0, 240.0];
/// How much of a body's colour its cut face shows.
const CUT_FACE: f32 = 0.72;

fn face_tint(base: [f32; 3], _p: &Part, index: usize, face: u32, look: &Look) -> [f32; 3] {
    let mix = |t: f32| [base[0] + (SELECT[0] - base[0]) * t, base[1] + (SELECT[1] - base[1]) * t, base[2] + (SELECT[2] - base[2]) * t];
    if look.selected_faces.contains(&face) {
        mix(0.6)
    } else if look.hover_part == Some(index) && look.hover_face == Some(face) {
        [base[0] + (255.0 - base[0]) * 0.22, base[1] + (255.0 - base[1]) * 0.22, base[2] + (255.0 - base[2]) * 0.22]
    } else {
        base
    }
}

/// Steps along a row: where a triangle covers it, and how its values change across it.
struct Plane {
    z: [f32; 3],
    x0: f32,
    y0: f32,
}

impl Plane {
    fn new(t: &Prim) -> Option<(Plane, f32)> {
        let (x, y) = (t.x, t.y);
        let area = (x[1] - x[0]) * (y[2] - y[0]) - (x[2] - x[0]) * (y[1] - y[0]);
        if area.abs() < 1e-6 {
            return None;
        }
        Some((Plane { z: t.z, x0: x[0], y0: y[0] }, 1.0 / area))
    }

    fn slopes(v: [f32; 3], t: &Prim, inv: f32) -> (f32, f32) {
        let (x, y) = (t.x, t.y);
        let dx = ((v[1] - v[0]) * (y[2] - y[0]) - (v[2] - v[0]) * (y[1] - y[0])) * inv;
        let dy = ((v[2] - v[0]) * (x[1] - x[0]) - (v[1] - v[0]) * (x[2] - x[0])) * inv;
        (dx, dy)
    }
}

/// Where a triangle crosses a row of pixels, in whole pixels.
fn span(t: &Prim, row: f32, w: usize) -> Option<(usize, usize)> {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for k in 0..3 {
        let (ya, yb, xa, xb) = (t.y[k], t.y[(k + 1) % 3], t.x[k], t.x[(k + 1) % 3]);
        if (ya <= row) != (yb <= row) {
            let x = xa + (row - ya) / (yb - ya) * (xb - xa);
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    if lo > hi {
        return None;
    }
    let a = (lo - 0.5).ceil().max(0.0) as usize;
    let b = ((hi - 0.5).floor().min(w as f32 - 1.0)).max(-1.0);
    if b < 0.0 || a as f32 > b {
        return None;
    }
    Some((a, b as usize))
}

fn pack(c: [f32; 3]) -> Color32 {
    Color32::from_rgb(c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8)
}

fn fill(t: &Prim, w: usize, first: usize, rows: usize, colors: &mut [Color32], depths: &mut [f32], ids: &mut [u32]) {
    let Some((plane, inv)) = Plane::new(t) else { return };
    let (lo, hi) = (t.y[0].min(t.y[1]).min(t.y[2]), t.y[0].max(t.y[1]).max(t.y[2]));
    let (a, b) = (((lo - 0.5).ceil().max(first as f32)) as usize, (hi - 0.5).floor().min((first + rows) as f32 - 1.0));
    if b < a as f32 {
        return;
    }
    let (zx, zy) = Plane::slopes(plane.z, t, inv);
    let slope = |k: usize| Plane::slopes([t.c[0][k], t.c[1][k], t.c[2][k]], t, inv);
    let (rx, ry) = slope(0);
    let (gx, gy) = slope(1);
    let (bx, by) = slope(2);
    let (sx, sy) = Plane::slopes(t.s, t, inv);
    let cut = t.s != [0.0; 3];
    for row in a..=b as usize {
        let yc = row as f32 + 0.5;
        let Some((x0, x1)) = span(t, yc, w) else { continue };
        let at = (row - first) * w;
        let dy = yc - plane.y0;
        let start = x0 as f32 + 0.5 - plane.x0;
        let mut z = t.z[0] + zx * start + zy * dy;
        let (mut r, mut g, mut bl) = (t.c[0][0] + rx * start + ry * dy, t.c[0][1] + gx * start + gy * dy, t.c[0][2] + bx * start + by * dy);
        let mut s = t.s[0] + sx * start + sy * dy;
        for x in x0..=x1 {
            let i = at + x;
            if z > depths[i] && (!cut || s <= 0.0) {
                depths[i] = z;
                ids[i] = t.id;
                colors[i] = pack([r, g, bl]);
            }
            z += zx;
            r += rx;
            g += gx;
            bl += bx;
            s += sx;
        }
    }
}

/// A see-through triangle over what's already there. Only what's solid can hide it, so other glass
/// shows through it whatever order they come in. The nearest of it is noted, for picking.
#[allow(clippy::too_many_arguments)]
fn blend(t: &Prim, w: usize, h: usize, colors: &mut [Color32], solid: &[f32], nearest: &mut [f32], ids: &mut [u32], bias: f32) {
    let Some((plane, inv)) = Plane::new(t) else { return };
    let (lo, hi) = (t.y[0].min(t.y[1]).min(t.y[2]), t.y[0].max(t.y[1]).max(t.y[2]));
    let (a, b) = (((lo - 0.5).ceil().max(0.0)) as usize, (hi - 0.5).floor().min(h as f32 - 1.0));
    if b < a as f32 {
        return;
    }
    let (zx, zy) = Plane::slopes(plane.z, t, inv);
    let slope = |k: usize| Plane::slopes([t.c[0][k], t.c[1][k], t.c[2][k]], t, inv);
    let (rx, ry) = slope(0);
    let (gx, gy) = slope(1);
    let (bx, by) = slope(2);
    let (sx, sy) = Plane::slopes(t.s, t, inv);
    let cut = t.s != [0.0; 3];
    let k = t.alpha as f32 / 255.0;
    let canvas = [crate::theme::BG.r() as f32, crate::theme::BG.g() as f32, crate::theme::BG.b() as f32];
    for row in a..=b as usize {
        let yc = row as f32 + 0.5;
        let Some((x0, x1)) = span(t, yc, w) else { continue };
        let at = row * w;
        let dy = yc - plane.y0;
        let start = x0 as f32 + 0.5 - plane.x0;
        let mut z = t.z[0] + zx * start + zy * dy;
        let (mut r, mut g, mut bl) = (t.c[0][0] + rx * start + ry * dy, t.c[0][1] + gx * start + gy * dy, t.c[0][2] + bx * start + by * dy);
        let mut s = t.s[0] + sx * start + sy * dy;
        for x in x0..=x1 {
            let i = at + x;
            // Metal lying in the same plane stays on top of it.
            if z > solid[i] + bias && (!cut || s <= 0.0) {
                let under = colors[i];
                let (ur, ug, ub) = if under.a() == 0 { (canvas[0], canvas[1], canvas[2]) } else { (under.r() as f32, under.g() as f32, under.b() as f32) };
                colors[i] = pack([ur + (r - ur) * k, ug + (g - ug) * k, ub + (bl - ub) * k]);
                if z > nearest[i] {
                    nearest[i] = z;
                    ids[i] = t.id;
                }
            }
            z += zx;
            r += rx;
            g += gx;
            bl += bx;
            s += sx;
        }
    }
}

/// The part of a line a section leaves. What's beyond it is gone, lines and all.
pub fn clip_to_section(line: [P3; 2], section: Option<([f32; 3], f32)>) -> Option<[P3; 2]> {
    let Some((m, d)) = section else { return Some(line) };
    let side = |q: P3| m[0] as f64 * q[0] + m[1] as f64 * q[1] + m[2] as f64 * q[2] - d as f64;
    let (a, b) = (side(line[0]), side(line[1]));
    if a > 0.0 && b > 0.0 {
        return None;
    }
    if a <= 0.0 && b <= 0.0 {
        return Some(line);
    }
    let on = add(line[0], scale(sub(line[1], line[0]), a / (a - b)));
    Some(if a > 0.0 { [on, line[1]] } else { [line[0], on] })
}

/// Where a section crosses the closed bodies: the outline of the cut face.
pub fn section_outline<'a>(parts: &'a [Part], section: Option<([f32; 3], f32)>) -> impl Iterator<Item = [P3; 2]> + 'a {
    parts.iter().filter(move |p| section.is_some() && !p.two_sided).flat_map(move |p| {
        let (m, d) = section.unwrap_or(([0.0; 3], 0.0));
        let side = move |q: P3| m[0] as f64 * q[0] + m[1] as f64 * q[1] + m[2] as f64 * q[2] - d as f64;
        p.mesh.tris.iter().filter_map(move |t| {
            let q = [p.mesh.verts[t[0] as usize], p.mesh.verts[t[1] as usize], p.mesh.verts[t[2] as usize]];
            let v = [side(q[0]), side(q[1]), side(q[2])];
            let mut ends = (0..3).filter_map(|k| {
                let (i, j) = (k, (k + 1) % 3);
                ((v[i] > 0.0) != (v[j] > 0.0)).then(|| add(q[i], scale(sub(q[j], q[i]), v[i] / (v[i] - v[j]))))
            });
            Some([ends.next()?, ends.next()?])
        })
    })
}

/// Lines over the picture, each broken where something hides it, so edges stay crisp at any zoom.
/// Behind something see-through they show fainter; hidden ones use `hidden`, which can be nothing.
pub fn paint_lines(painter: &egui::Painter, canvas: &Canvas, pj: &Projector, lines: impl Iterator<Item = [P3; 2]>, visible: egui::Stroke, hidden: egui::Stroke) {
    paint_lines_around(painter, canvas, pj, lines, visible, hidden, 0.0)
}

/// The same for a silhouette, which lies on a surface so steep that the pixels right beside it get
/// a say in whether it shows.
pub fn paint_silhouettes(painter: &egui::Painter, canvas: &Canvas, pj: &Projector, lines: impl Iterator<Item = [P3; 2]>, visible: egui::Stroke) {
    paint_lines_around(painter, canvas, pj, lines, visible, egui::Stroke::NONE, 1.5)
}

fn paint_lines_around(painter: &egui::Painter, canvas: &Canvas, pj: &Projector, lines: impl Iterator<Item = [P3; 2]>, visible: egui::Stroke, hidden: egui::Stroke, around: f32) {
    let coarse = canvas.scale < painter.ctx().pixels_per_point();
    let bias = pj.depth_bias() * if coarse { 2.5 } else { 1.0 };
    let around = if coarse { around.max(1.0) } else { around };
    let mut runs: Vec<(Pos2, Pos2, u8)> = Vec::new();
    for [a, b] in lines {
        let ((pa, da), (pb, db)) = (pj.project(a), pj.project(b));
        let steps = ((pa.distance(pb) / 5.0).ceil() as usize).clamp(1, 96);
        let mut run: Option<(Pos2, u8)> = None;
        let mut last = pa;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let at = pa + (pb - pa) * t;
            let seen = canvas.sight(at, da + (db - da) * t, bias, around);
            match run {
                None => run = Some((at, seen)),
                Some((from, was)) if was != seen => {
                    runs.push((from, at, was));
                    run = Some((at, seen));
                }
                _ => {}
            }
            last = at;
        }
        if let Some((from, was)) = run
            && from != last
        {
            runs.push((from, last, was));
        }
    }
    let through = egui::Stroke::new(visible.width * 0.8, visible.color.gamma_multiply(0.45));
    for (a, b, seen) in runs {
        let s = match seen {
            0 => visible,
            1 => through,
            _ => hidden,
        };
        if s.width > 0.0 && s.color.a() > 0 {
            painter.line_segment([a, b], s);
        }
    }
}

/// Silhouettes: inside edges of smooth surfaces where they turn away from the eye.
pub fn silhouettes<'a>(parts: &'a [Part], pj: &'a Projector) -> impl Iterator<Item = [P3; 2]> + 'a {
    let toward = pj.toward();
    parts.iter().flat_map(move |p| {
        p.soft.iter().filter(move |(_, n1, n2)| dot(*n1, toward) * dot(*n2, toward) < 0.0).map(|(e, _, _)| *e)
    })
}

/// The point of a triangle a screen position falls on, using its plane.
pub fn point_on(part: &Part, tri: usize, pj: &Projector, at: Pos2) -> P3 {
    let c = part.mesh.corners(tri);
    let (o, d) = pj.ray(at);
    let n = antenna_core::geometry::cross(sub(c[1], c[0]), sub(c[2], c[0]));
    let denom = dot(n, d);
    if denom.abs() < 1e-30 {
        return c[0];
    }
    let t = dot(sub(c[0], o), n) / denom;
    antenna_core::geometry::add(o, antenna_core::geometry::scale(d, t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::view3d::Camera;
    use antenna_core::geometry::norm;
    use antenna_core::modeler::prims;

    fn view(rect: Rect, yaw: f32, pitch: f32) -> Projector {
        Projector::for_test(&Camera { yaw, pitch, zoom: 1.0, pan: egui::Vec2::ZERO }, rect, [0.0; 3], 0.05)
    }

    fn part(lo: P3, hi: P3, feature: u32, rgb: [u8; 3], alpha: u8) -> Part {
        Part::new(Arc::new(prims::cuboid(lo, hi, feature).welded(1e-10)), rgb, alpha, Some(feature))
    }

    fn rendered(parts: &[Part], pj: &Projector, rect: Rect, look: &Look) -> (Canvas, Vec<Color32>) {
        let mut c = Canvas { size: [rect.width() as usize, rect.height() as usize], scale: 1.0, origin: rect.min, ..Default::default() };
        let px = c.render(parts, pj, look);
        (c, px)
    }

    #[test]
    fn what_is_nearer_covers_what_is_behind() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
        // Looking down: the small box stands on the big one.
        let pj = view(rect, 0.0, std::f32::consts::FRAC_PI_2);
        let parts = vec![part([-0.03, -0.03, 0.0], [0.03, 0.03, 0.01], 1, [200, 0, 0], 255), part([-0.01, -0.01, 0.01], [0.01, 0.01, 0.03], 2, [0, 200, 0], 255)];
        let (c, px) = rendered(&parts, &pj, rect, &Look::default());
        let mid = egui::pos2(100.0, 100.0);
        let hit = c.at(mid).expect("something's there");
        assert_eq!(hit.part, 1, "the top box");
        let i = 100 * 200 + 100;
        assert!(px[i].g() > px[i].r(), "{:?}", px[i]);
        // Off to the side it's the big one, and past it nothing at all.
        assert_eq!(c.at(pj.screen([0.02, 0.0, 0.01])).unwrap().part, 0);
        assert!(c.at(egui::pos2(2.0, 2.0)).is_none());
        // The order they're listed in makes no difference.
        let flipped = vec![parts[1].clone(), parts[0].clone()];
        let (c2, _) = rendered(&flipped, &pj, rect, &Look::default());
        assert_eq!(c2.at(mid).unwrap().part, 0);
    }

    #[test]
    fn a_body_through_another_shows_where_they_really_cross() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 300.0));
        // Side on, a post passing through a plate: seen from the front, in front of the plate's
        // far half and behind nothing, at a tilt where sorting whole parts can't get it right.
        let pj = view(rect, 0.6, 0.5);
        let parts = vec![part([-0.04, -0.04, -0.002], [0.04, 0.04, 0.002], 1, [200, 0, 0], 255), part([-0.005, -0.005, -0.04], [0.005, 0.005, 0.04], 2, [0, 200, 0], 255)];
        let (c, _) = rendered(&parts, &pj, rect, &Look::default());
        // The post's top end is above the plate, its bottom end below it, and the plate hides the
        // bottom where they overlap on the screen.
        let top = pj.screen([0.0, 0.0, 0.03]);
        assert_eq!(c.at(top).unwrap().part, 1);
        let hidden = pj.screen([0.0, 0.0, -0.01]);
        assert_eq!(c.at(hidden).unwrap().part, 0, "the plate is in front of the post's lower part");
    }

    #[test]
    fn a_see_through_part_tints_what_is_behind_without_hiding_it() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
        let pj = view(rect, 0.0, std::f32::consts::FRAC_PI_2);
        let parts = vec![part([-0.01, -0.01, 0.0], [0.01, 0.01, 0.002], 1, [220, 170, 90], 255), part([-0.03, -0.03, 0.002], [0.03, 0.03, 0.01], 2, [58, 150, 96], 120)];
        let (c, px) = rendered(&parts, &pj, rect, &Look::default());
        let i = 100 * 200 + 100;
        // Under the glass the metal still shows, tinted.
        assert!(px[i].a() == 255 && px[i].r() > px[i].b(), "{:?}", px[i]);
        // Where there's only glass it's laid on the canvas, opaque, so the window can't darken it.
        let j = 100 * 200 + 60;
        assert_eq!(px[j].a(), 255);
        assert!(px[j].g() > px[j].r(), "{:?}", px[j]);
        // It can be picked, as the nearest thing there.
        assert_eq!(c.at(egui::pos2(100.5, 100.5)).unwrap().part, 1);
    }

    #[test]
    fn an_edge_behind_something_is_found_hidden() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
        let pj = view(rect, 0.0, std::f32::consts::FRAC_PI_2);
        let parts = vec![part([-0.02, -0.02, 0.0], [0.02, 0.02, 0.01], 1, [200, 0, 0], 255)];
        let (c, _) = rendered(&parts, &pj, rect, &Look::default());
        let bias = pj.depth_bias();
        let (on_top, d_top) = pj.project([0.0, 0.0, 0.01]);
        assert!(c.sees(on_top, d_top, bias), "on the surface");
        let (under, d_under) = pj.project([0.0, 0.0, 0.0]);
        assert!(!c.sees(under, d_under, bias), "under it");
    }

    #[test]
    fn a_section_opens_a_body_up() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
        let pj = view(rect, 0.0, std::f32::consts::FRAC_PI_2);
        let parts = vec![part([-0.02, -0.02, 0.0], [0.02, 0.02, 0.01], 1, [200, 0, 0], 255)];
        let look = Look { section: Some(([1.0, 0.0, 0.0], 0.0)), ..Default::default() };
        let (c, _) = rendered(&parts, &pj, rect, &look);
        assert!(c.at(pj.screen([-0.01, 0.0, 0.01])).is_some(), "the kept side");
        assert!(c.at(pj.screen([0.01, 0.0, 0.01])).is_none(), "the side cut away");
    }

    #[test]
    fn a_section_shows_a_flat_cut_face_and_a_closed_outline() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 300.0));
        let pj = view(rect, 0.9, 0.5);
        let parts = vec![part([-0.02, -0.02, 0.0], [0.02, 0.02, 0.01], 1, [200, 0, 0], 255)];
        let section = Some(([1.0, 0.0, 0.0], 0.0));
        let look = Look { section, ..Default::default() };
        let (c, px) = rendered(&parts, &pj, rect, &look);
        let colour = |p: P3| {
            let q = pj.screen(p);
            let i = c.pixel(q).unwrap();
            px[i]
        };
        // Two points on the cut, whichever inner wall lies behind each: all one colour.
        let cut = (200.0 * CUT_FACE) as u8;
        assert_eq!(colour([0.0, -0.012, 0.005]).r(), cut);
        assert_eq!(colour([0.0, 0.012, 0.005]).r(), cut);
        // Its outline goes all the way round: 2 × (40 + 10) mm.
        let round: f64 = section_outline(&parts, section).map(|[a, b]| norm(sub(b, a))).sum();
        assert!((round - 0.1).abs() < 1e-9, "{round}");
        assert_eq!(section_outline(&parts, None).count(), 0);
    }

    #[test]
    fn lines_stop_at_the_section() {
        let cut = Some(([1.0, 0.0, 0.0], 0.0));
        let (a, b) = ([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        assert_eq!(clip_to_section([a, b], cut), Some([a, [0.0, 0.0, 0.0]]));
        assert_eq!(clip_to_section([b, a], cut), Some([[0.0, 0.0, 0.0], a]));
        assert_eq!(clip_to_section([b, [2.0, 0.0, 0.0]], cut), None, "entirely beyond it");
        assert_eq!(clip_to_section([a, [-2.0, 0.0, 0.0]], cut), Some([a, [-2.0, 0.0, 0.0]]));
        assert_eq!(clip_to_section([a, b], None), Some([a, b]));
    }

    #[test]
    fn silhouettes_are_only_where_a_surface_turns_away() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(200.0, 200.0));
        let pj = view(rect, 0.0, 0.0);
        let m = prims::cone([0.0, 0.0, -0.02], [0.0, 0.0, 0.02], 0.01, 0.01, 48, 1).welded(1e-10);
        let parts = vec![Part::new(Arc::new(m), [200, 0, 0], 255, None)];
        let n = silhouettes(&parts, &pj).count();
        assert!(n >= 2 && n <= 8, "{n} of {}", parts[0].soft.len());
    }

    #[test]
    fn a_fair_sized_model_renders_quickly() {
        let rect = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1600.0, 950.0));
        let pj = view(rect, 0.6, 0.4);
        let m = prims::sphere([0.0; 3], 0.04, 256, 1).welded(1e-10);
        let tris = m.tris.len();
        let parts = vec![Part::new(Arc::new(m), [200, 170, 90], 255, None)];
        let t = std::time::Instant::now();
        let (_c, px) = rendered(&parts, &pj, rect, &Look::default());
        let took = t.elapsed();
        println!("{tris} triangles at 1600x950: {took:?}");
        assert!(px.iter().any(|p| p.a() == 255));
    }
}
