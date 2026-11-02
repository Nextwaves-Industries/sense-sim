//! Pictures for the report, drawn from the data rather than captured: the model in three
//! views and at an angle, the 3D pattern, currents on the metal, and field planes.
//!
//! SVG paints in order, so nearer things have to come later. Parts are ordered by which
//! side of each other they lie on, which is right for any two parts whose boxes don't
//! overlap, and depth decides within a part.

use std::collections::HashMap;
use std::fmt::Write;

use super::chart::{hex, ramp, scale_key};
use super::esc;
use crate::farfield::{FarField, Pol, Quantity};
use crate::geometry::{Geometry, P3, Shape, add, cross, dot, norm, scale, sub, tessellate, unit};
use crate::material;
use crate::project::{CurrentView, FieldPlane};

const METAL: [u8; 3] = [214, 168, 92];
const WIRE: [u8; 3] = [160, 110, 40];
const LOW_DK: [u8; 3] = [58, 150, 96];
const HIGH_DK: [u8; 3] = [214, 150, 120];
const PORT: &str = "#6250d6";
/// Past this, a part is drawn as its box: an imported mesh could be a million triangles.
const MAX_TRIANGLES: usize = 6000;
/// Past this, the currents would make the page too heavy to open.
const MAX_ELEMENTS: usize = 20_000;

#[derive(Clone, Copy)]
struct View {
    right: P3,
    up: P3,
    toward: P3,
}

const TOP: View = View { right: [1.0, 0.0, 0.0], up: [0.0, 1.0, 0.0], toward: [0.0, 0.0, 1.0] };
const FRONT: View = View { right: [1.0, 0.0, 0.0], up: [0.0, 0.0, 1.0], toward: [0.0, -1.0, 0.0] };
const SIDE: View = View { right: [0.0, 1.0, 0.0], up: [0.0, 0.0, 1.0], toward: [1.0, 0.0, 0.0] };

/// From above, in front and to the right, with z up.
fn angled() -> View {
    let toward = unit([1.0, -1.0, 1.0]);
    let right = unit(cross([0.0, 0.0, 1.0], toward));
    View { right, up: cross(toward, right), toward }
}

impl View {
    fn uv(&self, p: P3) -> (f64, f64) {
        (dot(p, self.right), dot(p, self.up))
    }
    fn depth(&self, p: P3) -> f64 {
        dot(p, self.toward)
    }
}

/// From a view's plane to the page.
#[derive(Clone, Copy)]
struct Place {
    s: f64,
    /// Page position of (u0, v0).
    x: f64,
    y: f64,
    u0: f64,
    v0: f64,
}

impl Place {
    fn at(&self, v: &View, p: P3) -> (f64, f64) {
        let (a, b) = v.uv(p);
        (self.x + (a - self.u0) * self.s, self.y - (b - self.v0) * self.s)
    }
}

/// Coplanar triangles of a part, which draw as one shape with no seams inside.
struct Face {
    tris: Vec<[P3; 3]>,
    normal: P3,
}

struct Part {
    name: String,
    material: String,
    color: [u8; 3],
    opacity: f64,
    faces: Vec<Face>,
    /// Where a line is worth drawing: the rim of a sheet, or a real corner.
    edges: Vec<(P3, P3, Vec<usize>)>,
    wires: Vec<[P3; 2]>,
    radius: f64,
    bounds: (P3, P3),
    boxed: bool,
}

fn build_part(s: &crate::geometry::Solid) -> Part {
    let (mut tris, wires) = tessellate(&s.shape);
    let bounds = s.shape.bounds();
    let boxed = tris.len() > MAX_TRIANGLES;
    if boxed {
        tris = tessellate(&Shape::Box { min: bounds.0, max: bounds.1 }).0;
    }
    let size = norm(sub(bounds.1, bounds.0)).max(1e-12);
    let mut groups: HashMap<[i64; 4], usize> = HashMap::new();
    let mut faces: Vec<Face> = Vec::new();
    for t in tris {
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        if norm(n) <= size * size * 1e-14 {
            continue;
        }
        let n = unit(n);
        let q = |v: f64| (v * 1000.0).round() as i64;
        let key = [q(n[0]), q(n[1]), q(n[2]), q(dot(n, t[0]) / size)];
        let k = *groups.entry(key).or_insert_with(|| {
            faces.push(Face { tris: Vec::new(), normal: n });
            faces.len() - 1
        });
        faces[k].tris.push(t);
    }
    // An edge inside one face is no edge. Between faces at a shallow angle it's a facet of
    // something round, and lines there would make it look like a wireframe.
    let q = |v: f64| (v / (size * 1e-6)).round() as i64;
    let key = |p: P3| [q(p[0]), q(p[1]), q(p[2])];
    let mut seen: HashMap<([i64; 3], [i64; 3]), (P3, P3, Vec<usize>)> = HashMap::new();
    for (k, f) in faces.iter().enumerate() {
        for t in &f.tris {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let (ka, kb) = (key(a), key(b));
                let id = if ka < kb { (ka, kb) } else { (kb, ka) };
                seen.entry(id).or_insert((a, b, Vec::new())).2.push(k);
            }
        }
    }
    let mut edges: Vec<(P3, P3, Vec<usize>)> = seen
        .into_values()
        .filter_map(|(a, b, mut users)| {
            let inner = users.len() == 2 && users[0] == users[1];
            users.sort_unstable();
            users.dedup();
            let corner = users.len() == 1 || users.iter().any(|i| users.iter().any(|j| dot(faces[*i].normal, faces[*j].normal) < 0.85));
            (corner && !inner).then_some((a, b, users))
        })
        .collect();
    // HashMap order would make every export differ.
    edges.sort_by(|x, y| (x.0, x.1).partial_cmp(&(y.0, y.1)).unwrap_or(std::cmp::Ordering::Equal));
    let (color, opacity) = match material::by_name(&s.material) {
        Some(m) if !m.is_conductor() => {
            if m.eps_r() > 15.0 {
                (HIGH_DK, 0.63)
            } else {
                (LOW_DK, 0.47)
            }
        }
        _ if s.shape.is_wire() => (WIRE, 1.0),
        _ => (METAL, 1.0),
    };
    let radius = if let Shape::Wire { r, .. } = s.shape { r } else { 0.0 };
    Part { name: s.name.clone(), material: s.material.clone(), color, opacity, faces, edges, wires, radius, bounds, boxed }
}

fn center(b: &(P3, P3)) -> P3 {
    scale(add(b.0, b.1), 0.5)
}

/// Far parts first. For two parts with space between their boxes along an axis the view
/// isn't side-on to, the one on the far side comes first, whatever their centres say.
fn paint_order(parts: &[Part], v: &View) -> Vec<usize> {
    let n = parts.len();
    let mut order: Vec<usize> = (0..n).collect();
    let depth = |k: usize| v.depth(center(&parts[k].bounds));
    order.sort_by(|a, b| depth(*a).total_cmp(&depth(*b)));
    if n > 300 {
        return order;
    }
    let before = |a: usize, b: usize| {
        let (ba, bb) = (&parts[a].bounds, &parts[b].bounds);
        (0..3).any(|k| {
            let t = v.toward[k];
            (t > 1e-9 && ba.1[k] <= bb.0[k]) || (t < -1e-9 && ba.0[k] >= bb.1[k])
        })
    };
    // Depth order, then each part moved after anything that has to be behind it. A cycle
    // can't be painted right by any order, so the passes are limited.
    for _ in 0..n {
        let mut moved = false;
        for i in 0..n {
            for j in (i + 1)..n {
                if before(order[j], order[i]) && !before(order[i], order[j]) {
                    let x = order.remove(j);
                    order.insert(i, x);
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    order
}

fn path_of(tris: &[[P3; 3]], v: &View, pl: &Place) -> String {
    let mut d = String::new();
    for t in tris {
        for (k, p) in t.iter().enumerate() {
            let (x, y) = pl.at(v, *p);
            let _ = write!(d, "{}{x:.1} {y:.1}", if k == 0 { "M" } else { "L" });
        }
        d.push('Z');
    }
    d
}

fn shade(c: [u8; 3], k: f64) -> String {
    hex([(c[0] as f64 * k) as u8, (c[1] as f64 * k) as u8, (c[2] as f64 * k) as u8])
}

fn paint_parts(out: &mut String, parts: &[Part], v: &View, pl: &Place) {
    let light = unit(add(add(v.toward, scale(v.up, 0.6)), scale(v.right, -0.3)));
    for &index in &paint_order(parts, v) {
        let part = &parts[index];
        let _ = write!(out, "<g><title>{} ({})</title>", esc(&part.name), esc(&part.material));
        // Faces far to near; an edge goes with the nearest face that has it.
        let depth: Vec<f64> = part.faces.iter().map(|f| f.tris.iter().flatten().map(|p| v.depth(*p)).sum::<f64>() / (f.tris.len() * 3).max(1) as f64).collect();
        let mut faces: Vec<usize> = (0..part.faces.len()).collect();
        faces.sort_by(|a, b| depth[*a].total_cmp(&depth[*b]));
        let mut edges_of: Vec<Vec<usize>> = vec![Vec::new(); part.faces.len()];
        for (e, edge) in part.edges.iter().enumerate() {
            if let Some(k) = edge.2.iter().copied().max_by(|a, b| depth[*a].total_cmp(&depth[*b])) {
                edges_of[k].push(e);
            }
        }
        let line = shade(part.color, 0.5);
        for k in faces {
            let f = &part.faces[k];
            let lit = 0.62 + 0.38 * dot(f.normal, light).abs();
            let fill = shade(part.color, lit);
            if part.opacity < 1.0 {
                let _ = write!(out, r#"<path d="{}" fill="{fill}" fill-opacity="{}"/>"#, path_of(&f.tris, v, pl), part.opacity);
            } else {
                let _ = write!(out, r#"<path d="{}" fill="{fill}" stroke="{fill}" stroke-width=".4" stroke-linejoin="round"/>"#, path_of(&f.tris, v, pl));
            }
            if !edges_of[k].is_empty() {
                let mut d = String::new();
                for e in &edges_of[k] {
                    let (ax, ay) = pl.at(v, part.edges[*e].0);
                    let (bx, by) = pl.at(v, part.edges[*e].1);
                    let _ = write!(d, "M{ax:.1} {ay:.1}L{bx:.1} {by:.1}");
                }
                let _ = write!(out, r#"<path d="{d}" fill="none" stroke="{line}" stroke-width="1" stroke-linecap="round"/>"#);
            }
        }
        if !part.wires.is_empty() {
            let width = (2.0 * part.radius * pl.s).max(1.8);
            let mut d = String::new();
            for w in &part.wires {
                let (ax, ay) = pl.at(v, w[0]);
                let (bx, by) = pl.at(v, w[1]);
                let _ = write!(d, "M{ax:.1} {ay:.1}L{bx:.1} {by:.1}");
            }
            let _ = write!(out, r#"<path d="{d}" fill="none" stroke="{}" stroke-width="{width:.1}" stroke-linecap="round" stroke-linejoin="round"/>"#, hex(part.color));
        }
        out.push_str("</g>");
    }
}

fn paint_ports(out: &mut String, g: &Geometry, v: &View, pl: &Place) {
    for (k, port) in g.ports.iter().enumerate() {
        let (ax, ay) = pl.at(v, port.a);
        let (bx, by) = pl.at(v, port.b);
        let (cx, cy) = pl.at(v, port.center());
        let (mx, my) = (cx + 12.0, cy - 12.0);
        let _ = write!(out, r##"<g><title>Port {} ({})</title><line x1="{ax:.1}" y1="{ay:.1}" x2="{bx:.1}" y2="{by:.1}" stroke="{PORT}" stroke-width="2.5"/><line x1="{cx:.1}" y1="{cy:.1}" x2="{mx:.1}" y2="{my:.1}" stroke="{PORT}"/><circle cx="{mx:.1}" cy="{my:.1}" r="8" fill="{PORT}" stroke="#fff" stroke-width="2"/><text x="{mx:.1}" y="{:.1}" class="pn">{}</text></g>"##, k + 1, esc(&port.name), my + 3.5, k + 1);
    }
}

/// A length as a drawing would label it.
pub fn mm_text(metres: f64) -> String {
    let mm = metres * 1e3;
    if mm >= 1000.0 {
        format!("{:.3} m", metres)
    } else if mm >= 100.0 {
        format!("{mm:.0} mm")
    } else if mm >= 10.0 {
        format!("{mm:.1} mm")
    } else {
        format!("{mm:.2} mm")
    }
}

fn three_views(g: &Geometry, parts: &[Part]) -> String {
    let (lo, hi) = g.bounds();
    let size = sub(hi, lo);
    let (lx, ly, lz) = (size[0].max(0.0), size[1].max(0.0), size[2].max(0.0));
    let s = (540.0 / (lx + ly).max(1e-12)).min(420.0 / (ly + lz).max(1e-12));
    let (wx, wy, wz) = (lx * s, ly * s, lz * s);
    // Each view keeps room for its heading, even when it's only a line.
    let col1 = 66.0;
    let cell1 = wx.max(150.0);
    let col2 = col1 + cell1 + 84.0;
    let row1 = 32.0;
    let row2 = row1 + wy + 74.0;
    let width = col2 + wy.max(150.0) + 16.0;
    let height = row2 + wz + 40.0;
    let mut out = format!(r#"<svg class="drawing" viewBox="0 0 {width:.0} {height:.0}" role="img" aria-label="The model from above, the front and the side, with its overall size">"#);
    let views = [
        (TOP, "Top: x across, y up", col1, row1, wx, wy, lx, ly),
        (FRONT, "Front: x across, z up", col1, row2, wx, wz, lx, lz),
        (SIDE, "Side: y across, z up", col2, row2, wy, wz, ly, lz),
    ];
    for (view, title, left, top, vw, vh, across, up) in views {
        let (u0, v0) = view.uv(lo);
        let pl = Place { s, x: left, y: top + vh, u0, v0 };
        let _ = write!(out, r#"<text x="{left}" y="{:.1}" class="vt">{title}</text>"#, top - 12.0);
        if g.ground && view.up[2] > 0.5 {
            // The ground plane seen edge on, with the hatch that means "continues".
            let y = pl.at(&view, [0.0, 0.0, 0.0]).1;
            let (a, b) = (left - 8.0, left + vw + 8.0);
            let _ = write!(out, r#"<line x1="{a:.1}" x2="{b:.1}" y1="{y:.1}" y2="{y:.1}" class="base"/>"#);
            let mut x = a;
            while x < b {
                let _ = write!(out, r#"<line x1="{x:.1}" y1="{y:.1}" x2="{:.1}" y2="{:.1}" class="base"/>"#, x - 5.0, y + 5.0);
                x += 9.0;
            }
        }
        paint_parts(&mut out, parts, &view, &pl);
        paint_ports(&mut out, g, &view, &pl);
        let (xb, xr) = (top + vh + 12.0, left + vw);
        let _ = write!(out, r#"<path d="M{left} {:.1}V{:.1}M{left} {xb:.1}H{xr:.1}M{xr:.1} {:.1}V{:.1}" class="dim"/><text x="{:.1}" y="{:.1}" class="a" text-anchor="middle">{}</text>"#, xb - 3.0, xb + 3.0, xb - 3.0, xb + 3.0, left + vw / 2.0, xb + 15.0, mm_text(across));
        let yl = left - 12.0;
        let _ = write!(out, r#"<path d="M{:.1} {top:.1}H{:.1}M{yl:.1} {top:.1}V{:.1}M{:.1} {:.1}H{:.1}" class="dim"/><text x="{:.1}" y="{:.1}" class="a" text-anchor="middle" transform="rotate(-90 {:.1} {:.1})">{}</text>"#, yl - 3.0, yl + 3.0, top + vh, yl - 3.0, top + vh, yl + 3.0, yl - 6.0, top + vh / 2.0, yl - 6.0, top + vh / 2.0, mm_text(up));
    }
    out.push_str("</svg>");
    out
}

/// The x, y and z directions in a corner, so a pattern's θ and φ can be read against them.
fn axes_key(out: &mut String, v: &View, x: f64, y: f64) {
    let len = 26.0;
    out.push_str("<g>");
    for (axis, name) in [([1.0, 0.0, 0.0], "x"), ([0.0, 1.0, 0.0], "y"), ([0.0, 0.0, 1.0], "z")] {
        let (a, b) = v.uv(axis);
        let (ex, ey) = (x + a * len, y - b * len);
        let _ = write!(out, r#"<line x1="{x}" y1="{y}" x2="{ex:.1}" y2="{ey:.1}" class="dim"/><text x="{:.1}" y="{:.1}" class="a" text-anchor="middle">{name}</text>"#, x + a * (len + 9.0), y - b * (len + 9.0) + 4.0);
    }
    out.push_str("</g>");
}

fn angled_view(g: &Geometry, parts: &[Part]) -> String {
    let v = angled();
    let (w, h) = (400.0, 360.0);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let (lo, hi) = g.bounds();
    for x in [lo[0], hi[0]] {
        for y in [lo[1], hi[1]] {
            for z in [lo[2], hi[2]] {
                pts.push(v.uv([x, y, z]));
            }
        }
    }
    let (u0, u1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (v0, v1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let s = ((w - 70.0) / (u1 - u0).max(1e-12)).min((h - 90.0) / (v1 - v0).max(1e-12));
    let pl = Place { s, x: (w - (u1 - u0) * s) / 2.0, y: h - 46.0 - (h - 90.0 - (v1 - v0) * s) / 2.0, u0, v0 };
    let mut out = format!(r#"<svg class="angled" viewBox="0 0 {w} {h}" role="img" aria-label="The model at an angle">"#);
    let _ = write!(out, r#"<text x="12" y="20" class="vt">At an angle</text>"#);
    if g.ground {
        let m = 0.18 * norm(sub(hi, lo)).max(1e-12);
        let corners = [[lo[0] - m, lo[1] - m, 0.0], [hi[0] + m, lo[1] - m, 0.0], [hi[0] + m, hi[1] + m, 0.0], [lo[0] - m, hi[1] + m, 0.0]];
        let d: Vec<String> = corners.iter().map(|p| {
            let (x, y) = pl.at(&v, *p);
            format!("{x:.1},{y:.1}")
        }).collect();
        let _ = write!(out, r##"<polygon points="{}" fill="#ecebe6" stroke="#c3c2b7"/>"##, d.join(" "));
    }
    paint_parts(&mut out, parts, &v, &pl);
    paint_ports(&mut out, g, &v, &pl);
    axes_key(&mut out, &v, 40.0, h - 40.0);
    out.push_str("</svg>");
    out
}

/// The model's whole figure: the views and what the colours mean.
pub fn model_figure(g: &Geometry) -> String {
    let parts: Vec<Part> = g.solids.iter().map(build_part).collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut out = format!(r#"<div class="views">{}{}</div>"#, angled_view(g, &parts), three_views(g, &parts));
    // What each colour is, in the order the parts first use it.
    let mut key: Vec<(String, String)> = Vec::new();
    for (p, s) in parts.iter().zip(&g.solids) {
        let swatch = hex(p.color);
        let word = match material::by_name(&s.material) {
            Some(m) if m.is_conductor() => if s.shape.is_wire() { "Wire".to_string() } else { "Metal".to_string() },
            Some(m) => m.name,
            None => format!("{} (not in the library)", s.material),
        };
        if !key.iter().any(|k| k.0 == word) {
            key.push((word, swatch));
        }
    }
    let mut legend: Vec<String> = key.iter().map(|(name, color)| format!(r#"<span><i style="background:{color}"></i>{}</span>"#, esc(name))).collect();
    if !g.ports.is_empty() {
        legend.push(format!(r#"<span><i style="background:{PORT}"></i>Feed, numbered by port</span>"#));
    }
    if g.ground {
        legend.push(r#"<span><i style="background:#c3c2b7"></i>Infinite ground plane at z = 0</span>"#.to_string());
    }
    let _ = write!(out, r#"<p class="legend">{}</p>"#, legend.join(""));
    let boxed: Vec<&str> = parts.iter().filter(|p| p.boxed).map(|p| p.name.as_str()).collect();
    if !boxed.is_empty() {
        let _ = write!(out, r#"<p class="note">Drawn as a box because its mesh is too large to draw: {}.</p>"#, esc(&boxed.join(", ")));
    }
    out
}

/// The realized gain as a surface: each direction's distance from the centre is its level,
/// from the floor 30 dB under the peak out to the peak.
pub fn pattern_3d(ff: &FarField) -> String {
    if ff.theta.len() < 3 || ff.phi.len() < 3 {
        return String::new();
    }
    let grid = ff.grid_db(Quantity::RealizedGain, Pol::Total);
    let peak = grid.iter().flatten().copied().filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    if !peak.is_finite() {
        return String::new();
    }
    let floor = peak - 30.0;
    let level = |db: f64| if db.is_nan() { 0.0 } else { ((db - floor) / 30.0).clamp(0.0, 1.0) };
    // 6° is plenty for a picture, and keeps a pattern to about a hundred kilobytes.
    let dt = ((6.0 / ff.theta_step().max(1e-9)).round() as usize).max(1);
    let dp = ((6.0 / ff.phi_step().max(1e-9)).round() as usize).max(1);
    let point = |ti: usize, pi: usize| -> P3 {
        let (st, ct) = ff.theta[ti].to_radians().sin_cos();
        let (sp, cp) = ff.phi[pi].to_radians().sin_cos();
        scale([st * cp, st * sp, ct], level(grid[ti][pi]))
    };
    let v = angled();
    let (w, h, c, r) = (400.0, 400.0, 200.0, 168.0);
    let pl = Place { s: r, x: c, y: c + 4.0, u0: 0.0, v0: 0.0 };
    let mut quads: Vec<(f64, f64, String)> = Vec::new();
    let mut ti = 0;
    while ti + 1 < ff.theta.len() {
        let t2 = (ti + dt).min(ff.theta.len() - 1);
        let mut pi = 0;
        while pi + 1 < ff.phi.len() {
            let p2 = (pi + dp).min(ff.phi.len() - 1);
            let corners = [point(ti, pi), point(t2, pi), point(t2, p2), point(ti, p2)];
            let lev = (level(grid[ti][pi]) + level(grid[t2][pi]) + level(grid[t2][p2]) + level(grid[ti][p2])) / 4.0;
            if lev > 0.005 {
                let mid = scale(add(add(corners[0], corners[1]), add(corners[2], corners[3])), 0.25);
                let d: Vec<String> = corners.iter().map(|q| {
                    let (x, y) = pl.at(&v, *q);
                    format!("{x:.1},{y:.1}")
                }).collect();
                quads.push((v.depth(mid), lev, d.join(" ")));
            }
            pi = p2;
        }
        ti = t2;
    }
    quads.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = format!(r#"<svg class="pattern3d" viewBox="0 0 {w} {h}" role="img" aria-label="Realized gain over all directions as a surface">"#);
    let axis = |out: &mut String, dir: P3, name: &str| {
        let (x, y) = pl.at(&v, scale(dir, 1.12));
        let (tx, ty) = pl.at(&v, scale(dir, 1.18));
        let _ = write!(out, r#"<line x1="{c}" y1="{}" x2="{x:.1}" y2="{y:.1}" class="dim"/><text x="{tx:.1}" y="{:.1}" class="a" text-anchor="middle">{name}</text>"#, c + 4.0, ty + 4.0);
    };
    axis(&mut out, [1.0, 0.0, 0.0], "x");
    axis(&mut out, [0.0, 1.0, 0.0], "y");
    axis(&mut out, [0.0, 0.0, 1.0], "z");
    for (_, lev, d) in &quads {
        let color = hex(ramp(*lev));
        let _ = write!(out, r#"<polygon points="{d}" fill="{color}" stroke="{color}" stroke-width=".5" stroke-linejoin="round"/>"#);
    }
    out.push_str("</svg>");
    out
}

/// Currents on the model, on a dB scale under its 98th percentile, as the app draws them.
/// Flat models are seen face on, since that shows every part of the surface.
pub fn currents_figure(c: &CurrentView) -> Option<String> {
    let elements = c.tris.len() + c.segments.len();
    if elements == 0 || elements > MAX_ELEMENTS || c.current.len() < elements {
        return None;
    }
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in &c.verts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size = sub(hi, lo);
    let big = size[0].max(size[1]).max(size[2]);
    let v = if size[2] < 0.15 * big {
        TOP
    } else if size[1] < 0.15 * big {
        FRONT
    } else if size[0] < 0.15 * big {
        SIDE
    } else {
        angled()
    };
    let mut sorted: Vec<f64> = c.current.iter().copied().filter(|x| x.is_finite()).collect();
    sorted.sort_by(f64::total_cmp);
    let top = sorted.get(((sorted.len() as f64 * 0.98) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0);
    if !(top > 0.0) {
        return None;
    }
    let level = |x: f64| ((20.0 * (x / top).max(1e-9).log10() + 30.0) / 30.0).clamp(0.0, 1.0);
    let pts: Vec<(f64, f64)> = c.verts.iter().map(|p| v.uv(*p)).collect();
    let (u0, u1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (v0, v1) = pts.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let w = 620.0;
    let s = ((w - 40.0) / (u1 - u0).max(1e-12)).min(420.0 / (v1 - v0).max(1e-12));
    let h = (v1 - v0) * s + 40.0;
    let pl = Place { s, x: (w - (u1 - u0) * s) / 2.0, y: h - 20.0, u0, v0 };
    let vert = |i: u32| c.verts.get(i as usize).copied();
    let mut items: Vec<(f64, String)> = Vec::with_capacity(elements);
    for (k, seg) in c.segments.iter().enumerate() {
        let (Some(a), Some(b)) = (vert(seg[0]), vert(seg[1])) else { continue };
        let (ax, ay) = pl.at(&v, a);
        let (bx, by) = pl.at(&v, b);
        let color = hex(ramp(level(c.current[k])));
        items.push((v.depth(scale(add(a, b), 0.5)), format!(r#"<line x1="{ax:.1}" y1="{ay:.1}" x2="{bx:.1}" y2="{by:.1}" stroke="{color}" stroke-width="3.5" stroke-linecap="round"/>"#)));
    }
    for (k, t) in c.tris.iter().enumerate() {
        let (Some(a), Some(b), Some(d)) = (vert(t[0]), vert(t[1]), vert(t[2])) else { continue };
        let (ax, ay) = pl.at(&v, a);
        let (bx, by) = pl.at(&v, b);
        let (dx, dy) = pl.at(&v, d);
        let color = hex(ramp(level(c.current[c.segments.len() + k])));
        items.push((v.depth(scale(add(add(a, b), d), 1.0 / 3.0)), format!(r#"<polygon points="{ax:.1},{ay:.1} {bx:.1},{by:.1} {dx:.1},{dy:.1}" fill="{color}" stroke="{color}" stroke-width=".4" stroke-linejoin="round"/>"#)));
    }
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = format!(r#"<svg class="currents" viewBox="0 0 {w} {h:.0}" role="img" aria-label="Current magnitude on the model">"#);
    for (_, item) in &items {
        out.push_str(item);
    }
    out.push_str("</svg>");
    out.push_str(&scale_key("−30 dB", "0 dB (98th percentile)"));
    Some(out)
}

const CRC_POLY: u32 = 0xedb8_8320;

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for chunk in chunks {
        for b in *chunk {
            crc ^= *b as u32;
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ CRC_POLY } else { crc >> 1 };
            }
        }
    }
    !crc
}

/// A paletted PNG, uncompressed. One byte a pixel is small enough for a field plane, and a
/// picture this size as SVG rectangles would be many times larger.
pub fn png_indexed(w: usize, h: usize, palette: &[[u8; 3]], pixels: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"PLTE", &palette.iter().flatten().copied().collect::<Vec<u8>>());
    let mut raw = Vec::with_capacity((w + 1) * h);
    for row in pixels.chunks(w.max(1)) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    // A zlib stream of stored blocks.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

pub fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A plane's name as a reader would say it.
pub fn plane_title(name: &str) -> String {
    match name {
        "E_xy" => "|E| in the xy plane".into(),
        "E_xz" => "|E| in the xz plane".into(),
        "E_yz" => "|E| in the yz plane".into(),
        "H_top" => "|H| just above the metal (surface current)".into(),
        other => other.to_string(),
    }
}

/// A field plane as an image, in dB under its 99th percentile. Rows run `u + nu·v`, with v
/// upward on the page. `None` if it holds nothing to show.
pub fn plane_figure(p: &FieldPlane) -> Option<String> {
    if p.nu < 2 || p.nv < 2 || p.mag.len() < p.nu * p.nv {
        return None;
    }
    let mut sorted: Vec<f32> = p.mag.iter().copied().filter(|x| x.is_finite()).collect();
    sorted.sort_by(f32::total_cmp);
    let top = sorted.get(((sorted.len() as f64 * 0.99) as usize).min(sorted.len().saturating_sub(1))).copied().unwrap_or(0.0) as f64;
    if !(top > 0.0) {
        return None;
    }
    let palette: Vec<[u8; 3]> = (0..=255).map(|k| ramp(k as f64 / 255.0)).collect();
    let mut px = Vec::with_capacity(p.nu * p.nv);
    for v in (0..p.nv).rev() {
        for u in 0..p.nu {
            let m = p.mag[u + p.nu * v] as f64;
            let t = ((20.0 * (m / top).max(1e-9).log10() + 40.0) / 40.0).clamp(0.0, 1.0);
            px.push((t * 255.0).round() as u8);
        }
    }
    let png = base64(&png_indexed(p.nu, p.nv, &palette, &px));
    let (du, dv) = (norm(p.du), norm(p.dv));
    let aspect = if du > 0.0 && dv > 0.0 { du / dv } else { p.nu as f64 / p.nv as f64 };
    let axis = |d: P3| ["x", "y", "z"][(0..3).max_by(|a, b| d[*a].abs().total_cmp(&d[*b].abs())).unwrap_or(0)];
    let title = plane_title(&p.name);
    Some(format!(
        r#"<figure class="plane"><figcaption>{}</figcaption><img alt="{}" src="data:image/png;base64,{png}" style="aspect-ratio:{aspect:.3}"><p class="note">{} across {}, {} up {}. {} × {} samples.</p></figure>"#,
        esc(&title),
        esc(&title),
        axis(p.du),
        mm_text(du),
        axis(p.dv),
        mm_text(dv),
        p.nu,
        p.nv
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PortDef, Solid};

    fn sheet(name: &str, z: f64, half: f64, material: &str) -> Solid {
        Solid { name: name.into(), material: material.into(), shape: Shape::Box { min: [-half, -half, z], max: [half, half, z] } }
    }

    /// The patch must not disappear behind the much larger ground under it, wherever it sits.
    #[test]
    fn a_small_part_above_a_large_one_paints_after_it_from_every_side() {
        let mut patch = sheet("patch", 0.01, 0.01, "PEC");
        // Off to the far corner, where its centre is further from the viewer than the ground's.
        if let Shape::Box { min, max } = &mut patch.shape {
            *min = [-0.05, 0.03, 0.01];
            *max = [-0.03, 0.05, 0.01];
        }
        let g = Geometry { solids: vec![patch, sheet("ground", 0.0, 0.05, "PEC")], ports: vec![], ..Default::default() };
        let parts: Vec<Part> = g.solids.iter().map(build_part).collect();
        for v in [angled(), TOP] {
            let order = paint_order(&parts, &v);
            assert_eq!(parts[order[0]].name, "ground");
            assert_eq!(parts[order[1]].name, "patch");
        }
    }

    #[test]
    fn a_box_shows_its_corners_and_a_cylinder_is_not_a_wireframe() {
        let b = build_part(&Solid { name: "b".into(), material: "PEC".into(), shape: Shape::Box { min: [0.0; 3], max: [1.0, 1.0, 1.0] } });
        assert_eq!(b.faces.len(), 6);
        assert_eq!(b.edges.len(), 12, "a cube has 12 edges, and no diagonals");
        let c = build_part(&Solid { name: "c".into(), material: "PEC".into(), shape: Shape::Cylinder { a: [0.0; 3], b: [0.0, 0.0, 1.0], r: 0.1 } });
        assert_eq!(c.edges.len(), 64, "two rims of 32, and no line along each facet");
    }

    #[test]
    fn an_edge_on_sheet_still_shows() {
        let g = Geometry { solids: vec![sheet("patch", 0.01, 0.02, "PEC")], ports: vec![PortDef { name: "feed".into(), a: [0.0, 0.0, 0.0], b: [0.0, 0.0, 0.01], width: 0.0, z0: 50.0 }], ..Default::default() };
        let s = model_figure(&g);
        // Seen from the front it has no area, so only its rim draws it.
        assert!(s.contains(r#"fill="none" stroke="#), "{s}");
        assert!(s.contains("Feed, numbered by port") && s.contains("40.0 mm"), "{s}");
        assert!(!s.contains("NaN") && !s.contains("inf"), "{s}");
    }

    #[test]
    fn a_huge_mesh_draws_as_its_box_and_says_so() {
        let tris = vec![[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]; MAX_TRIANGLES + 1];
        let g = Geometry { solids: vec![Solid { name: "scan".into(), material: "PEC".into(), shape: Shape::Tris { tris, solid: true, sheet: 0.0 } }], ..Default::default() };
        let s = model_figure(&g);
        assert!(s.contains("too large to draw: scan"), "{}", &s[..200.min(s.len())]);
        assert!(s.len() < 200_000, "{}", s.len());
    }

    #[test]
    fn the_same_model_draws_the_same_every_time() {
        let g = Geometry { solids: vec![sheet("a", 0.0, 0.05, "PEC"), sheet("b", 0.01, 0.02, "FR-4")], ..Default::default() };
        assert_eq!(model_figure(&g), model_figure(&g));
    }

    #[test]
    fn a_png_has_the_structure_a_viewer_checks() {
        let px: Vec<u8> = (0..12).collect();
        let palette: Vec<[u8; 3]> = (0..12).map(|k| [k, k, k]).collect();
        let png = png_indexed(4, 3, &palette, &px);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 4);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 3);
        // The IEND chunk's CRC is fixed by the format.
        assert_eq!(&png[png.len() - 4..], &[0xae, 0x42, 0x60, 0x82]);
        assert_eq!(crc32(&[b"IEND"]), 0xae42_6082);
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn pictures_with_nothing_in_them_are_left_out() {
        assert!(plane_figure(&FieldPlane { nu: 2, nv: 2, mag: vec![0.0; 4], ..Default::default() }).is_none());
        assert!(currents_figure(&CurrentView::default()).is_none());
        assert!(pattern_3d(&FarField::default()).is_empty());
    }
}
