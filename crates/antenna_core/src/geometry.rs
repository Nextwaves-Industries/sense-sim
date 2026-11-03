//! Concrete (already evaluated) antenna geometry: the one description every
//! solver and the 3D view consume. Units are metres. Templates and the
//! parameter table in `project` produce it.

use serde::{Deserialize, Serialize};

pub type P3 = [f64; 3];

pub fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: P3, b: P3) -> P3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: P3, k: f64) -> P3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn norm(a: P3) -> f64 {
    dot(a, a).sqrt()
}
pub fn unit(a: P3) -> P3 {
    let n = norm(a).max(1e-300);
    scale(a, 1.0 / n)
}

/// A flat outline point `(u, v)` in the plane normal to `axis` at `at`, in the
/// CSXCAD convention: axis z → (x, y), axis x → (y, z), axis y → (z, x).
pub fn plane_point(axis: usize, at: f64, p: [f64; 2]) -> P3 {
    let mut q = [0.0; 3];
    q[axis] = at;
    q[(axis + 1) % 3] = p[0];
    q[(axis + 2) % 3] = p[1];
    q
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Shape {
    /// Axis-aligned box. A zero extent along one axis makes it a flat sheet.
    Box { min: P3, max: P3 },
    /// Solid cylinder between two axis points.
    Cylinder { a: P3, b: P3, r: f64 },
    Sphere { c: P3, r: f64 },
    /// Outline in the plane normal to `axis` at coordinate `at` (see
    /// [`plane_point`]). `thick` extrudes it along +axis; 0 is a flat sheet.
    Polygon { axis: usize, at: f64, pts: Vec<[f64; 2]>, thick: f64 },
    /// Thin round wire through the points.
    Wire { pts: Vec<P3>, r: f64 },
    /// Triangles: an imported model, or a tilted sheet. `solid` says they close a
    /// volume; an open surface is thickened by `sheet` for FDTD.
    Tris { tris: Vec<[P3; 3]>, solid: bool, sheet: f64 },
}

impl Shape {
    pub fn is_wire(&self) -> bool {
        matches!(self, Shape::Wire { .. })
    }

    /// An open sheet of no thickness, with metal on both faces: not a wire, not a closed body.
    pub fn is_sheet(&self) -> bool {
        match self {
            Shape::Box { min, max } => (0..3).any(|k| max[k] <= min[k]),
            Shape::Polygon { thick, .. } => *thick <= 0.0,
            Shape::Tris { solid, .. } => !*solid,
            _ => false,
        }
    }

    /// Axis-aligned bounds.
    pub fn bounds(&self) -> (P3, P3) {
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        let mut add = |p: P3| {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        };
        match self {
            Shape::Box { min, max } => {
                add(*min);
                add(*max);
            }
            Shape::Cylinder { a, b, r } => {
                for s in [*a, *b] {
                    add(add3(s, -*r));
                    add(add3(s, *r));
                }
            }
            Shape::Sphere { c, r } => {
                add(add3(*c, -*r));
                add(add3(*c, *r));
            }
            Shape::Polygon { axis, at, pts, thick } => {
                for p in pts {
                    add(plane_point(*axis, *at, *p));
                    add(plane_point(*axis, *at + *thick, *p));
                }
            }
            Shape::Wire { pts, r } => {
                for p in pts {
                    add(add3(*p, -*r));
                    add(add3(*p, *r));
                }
            }
            Shape::Tris { tris, .. } => {
                for t in tris {
                    t.iter().for_each(|p| add(*p));
                }
            }
        }
        (lo, hi)
    }
}

fn add3(p: P3, d: f64) -> P3 {
    [p[0] + d, p[1] + d, p[2] + d]
}

/// One named part of the antenna; `material` is a name from [`crate::material`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Solid {
    pub name: String,
    pub material: String,
    pub shape: Shape,
}

/// A lumped feed between the points `a` and `b` (axis-aligned). `width` is its
/// size across the current flow for FDTD; 0 makes it a line. `a → b` is the
/// direction of positive port voltage rise (the port drives current from `a`
/// to `b` inside the gap's outside circuit).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortDef {
    pub name: String,
    pub a: P3,
    pub b: P3,
    pub width: f64,
    pub z0: f64,
}

impl PortDef {
    pub fn center(&self) -> P3 {
        scale(add(self.a, self.b), 0.5)
    }
    /// Axis of the gap (the largest component of `b - a`).
    pub fn axis(&self) -> usize {
        let d = sub(self.b, self.a);
        (0..3).max_by(|i, j| d[*i].abs().total_cmp(&d[*j].abs())).unwrap_or(2)
    }
}

/// Fine-mesh request for FDTD: no cell above `cell` inside the box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshHint {
    pub min: P3,
    pub max: P3,
    pub cell: f64,
}

/// What physical solvers can handle this geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SolverFit {
    pub wire_mom: bool,
    pub surface_mom: bool,
    pub fdtd: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub solids: Vec<Solid>,
    pub ports: Vec<PortDef>,
    /// Infinite perfect ground plane at z = 0 (image theory / PEC boundary).
    pub ground: bool,
    pub hints: Vec<MeshHint>,
    /// Where the main beam is meant to point: +z (true) or broadside to a
    /// vertical axis (false). Used when the antenna is mounted on a device.
    pub axis_forward: bool,
    /// Layers of paper, film or product around flat metal, as wide as the world.
    #[serde(default)]
    pub stack: Option<crate::stack::Stack>,
}

/// Triangulates a simple polygon (concave and keyhole outlines included) by ear
/// clipping. Returns index triples into `pts`.
pub fn triangulate(pts: &[[f64; 2]]) -> Vec<[usize; 3]> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let area: f64 = (0..n).map(|i| { let (a, b) = (pts[i], pts[(i + 1) % n]); a[0] * b[1] - b[0] * a[1] }).sum();
    let orient = if area >= 0.0 { 1.0 } else { -1.0 };
    let cross2 = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) * orient;
    let mut ring: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let mut guard = 0;
    while ring.len() > 3 && guard < n * n {
        guard += 1;
        let m = ring.len();
        // Best ear: convex, empty, and the least sliver-like.
        let mut best: Option<(usize, f64)> = None;
        for i in 0..m {
            let (ia, ib, ic) = (ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            let turn = cross2(a, b, c);
            if turn <= 1e-18 {
                continue;
            }
            let blocked = ring.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = pts[j];
                // Coincident keyhole vertices don't block.
                if p == a || p == b || p == c {
                    return false;
                }
                cross2(a, b, p) >= 0.0 && cross2(b, c, p) >= 0.0 && cross2(c, a, p) >= 0.0
            });
            if blocked {
                continue;
            }
            let l = |u: [f64; 2], v: [f64; 2]| (u[0] - v[0]).powi(2) + (u[1] - v[1]).powi(2);
            let quality = turn / (l(a, b) + l(b, c) + l(c, a)).max(1e-30);
            if best.is_none_or(|b| quality > b.1) {
                best = Some((i, quality));
            }
        }
        let Some((i, _)) = best else {
            // Collinear leftovers: drop the flattest corner.
            let i = (0..m)
                .min_by(|x, y| {
                    let f = |i: usize| cross2(pts[ring[(i + m - 1) % m]], pts[ring[i]], pts[ring[(i + 1) % m]]).abs();
                    f(*x).total_cmp(&f(*y))
                })
                .unwrap_or(0);
            ring.remove(i);
            continue;
        };
        out.push([ring[(i + m - 1) % m], ring[i], ring[(i + 1) % m]]);
        ring.remove(i);
    }
    if ring.len() == 3 && cross2(pts[ring[0]], pts[ring[1]], pts[ring[2]]) > 1e-18 {
        out.push([ring[0], ring[1], ring[2]]);
    }
    out
}

/// A shape as triangles and line segments, for drawing and CAD export.
pub fn tessellate(shape: &Shape) -> (Vec<[P3; 3]>, Vec<[P3; 2]>) {
    let mut tris = Vec::new();
    let mut lines = Vec::new();
    let quad = |t: &mut Vec<[P3; 3]>, a: P3, b: P3, c: P3, d: P3| {
        t.push([a, b, c]);
        t.push([a, c, d]);
    };
    match shape {
        Shape::Box { min, max } => {
            let v = |x: bool, y: bool, z: bool| [if x { max[0] } else { min[0] }, if y { max[1] } else { min[1] }, if z { max[2] } else { min[2] }];
            let faces = [
                (v(false, false, false), v(false, true, false), v(true, true, false), v(true, false, false)),
                (v(false, false, true), v(true, false, true), v(true, true, true), v(false, true, true)),
                (v(false, false, false), v(true, false, false), v(true, false, true), v(false, false, true)),
                (v(false, true, false), v(false, true, true), v(true, true, true), v(true, true, false)),
                (v(false, false, false), v(false, false, true), v(false, true, true), v(false, true, false)),
                (v(true, false, false), v(true, true, false), v(true, true, true), v(true, false, true)),
            ];
            for (i, f) in faces.iter().enumerate() {
                // The faces above come in pairs, normal to z, then y, then x.
                let axis = 2 - i / 2;
                let flat = max[axis] - min[axis] <= 0.0;
                // A flat box is one sheet, not two coincident faces.
                if flat && i % 2 == 1 {
                    continue;
                }
                let extents_ok = (0..3).filter(|k| *k != axis).all(|k| max[k] - min[k] > 0.0);
                if extents_ok {
                    quad(&mut tris, f.0, f.1, f.2, f.3);
                }
            }
        }
        Shape::Polygon { axis, at, pts, thick } => {
            let idx = triangulate(pts);
            let top = *at + *thick;
            for i in &idx {
                tris.push([plane_point(*axis, top, pts[i[0]]), plane_point(*axis, top, pts[i[1]]), plane_point(*axis, top, pts[i[2]])]);
                if *thick > 0.0 {
                    tris.push([plane_point(*axis, *at, pts[i[0]]), plane_point(*axis, *at, pts[i[2]]), plane_point(*axis, *at, pts[i[1]])]);
                }
            }
            if *thick > 0.0 {
                for i in 0..pts.len() {
                    let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
                    quad(&mut tris, plane_point(*axis, *at, p), plane_point(*axis, *at, q), plane_point(*axis, top, q), plane_point(*axis, top, p));
                }
            }
        }
        Shape::Cylinder { a, b, r } => {
            let n = 32;
            let u = unit(sub(*b, *a));
            let seed = if u[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
            let e1 = unit(cross(u, seed));
            let e2 = cross(u, e1);
            let rim = |c: P3, k: usize| {
                let t = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
                add(c, add(scale(e1, r * t.cos()), scale(e2, r * t.sin())))
            };
            for k in 0..n {
                quad(&mut tris, rim(*a, k), rim(*a, k + 1), rim(*b, k + 1), rim(*b, k));
                tris.push([*a, rim(*a, k + 1), rim(*a, k)]);
                tris.push([*b, rim(*b, k), rim(*b, k + 1)]);
            }
        }
        Shape::Sphere { c, r } => {
            let (nt, np) = (16, 32);
            let pt = |i: usize, j: usize| {
                let (st, ct) = (std::f64::consts::PI * i as f64 / nt as f64).sin_cos();
                let (sp, cp) = (2.0 * std::f64::consts::PI * j as f64 / np as f64).sin_cos();
                add(*c, [r * st * cp, r * st * sp, r * ct])
            };
            for i in 0..nt {
                for j in 0..np {
                    quad(&mut tris, pt(i, j), pt(i + 1, j), pt(i + 1, j + 1), pt(i, j + 1));
                }
            }
        }
        Shape::Wire { pts, .. } => {
            for w in pts.windows(2) {
                lines.push([w[0], w[1]]);
            }
        }
        Shape::Tris { tris: t, .. } => tris.extend_from_slice(t),
    }
    (tris, lines)
}

/// Reads an STL file (binary or ASCII); coordinates are multiplied by `scale_to_m`.
pub fn read_stl(bytes: &[u8], scale_to_m: f64) -> anyhow::Result<Vec<[P3; 3]>> {
    let scaled = |v: [f64; 3]| [v[0] * scale_to_m, v[1] * scale_to_m, v[2] * scale_to_m];
    // ASCII starts with "solid" and has the keyword "facet" early on; binary has an exact size.
    let binary = bytes.len() >= 84 && {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        bytes.len() == 84 + n * 50
    };
    if binary {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let r = &bytes[84 + k * 50 + 12..84 + k * 50 + 48];
            let f = |i: usize| f32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]) as f64;
            out.push([scaled([f(0), f(4), f(8)]), scaled([f(12), f(16), f(20)]), scaled([f(24), f(28), f(32)])]);
        }
        return Ok(out);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("not a valid STL file"))?;
    let mut verts = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some("vertex") {
            let v: Vec<f64> = it.filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                verts.push(scaled([v[0], v[1], v[2]]));
            }
        }
    }
    if verts.is_empty() || verts.len() % 3 != 0 {
        anyhow::bail!("no triangles found in the STL file");
    }
    Ok(verts.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

impl Geometry {
    /// With its layers built as parts, to size, for a solver that meshes everything.
    pub fn with_stack_built(&self) -> Self {
        let mut g = self.clone();
        if let Some(stack) = g.stack.take() {
            let metal = g.bounds();
            g.solids.extend(stack.solids(metal));
        }
        g
    }

    /// Metal as a binary STL (millimetres), wires left out.
    pub fn to_stl(&self) -> Vec<u8> {
        let lib = crate::material::library();
        let mut tris: Vec<[P3; 3]> = Vec::new();
        for s in &self.solids {
            if lib.iter().find(|m| m.name == s.material).is_none_or(|m| m.is_conductor()) {
                tris.extend(tessellate(&s.shape).0);
            }
        }
        let mut out = vec![0u8; 80];
        out[..24].copy_from_slice(b"Sense Sim antenna, mm   ");
        out.extend((tris.len() as u32).to_le_bytes());
        for t in &tris {
            let n = unit(cross(sub(t[1], t[0]), sub(t[2], t[0])));
            for v in n {
                out.extend((v as f32).to_le_bytes());
            }
            for p in t {
                for v in p {
                    out.extend(((*v * 1000.0) as f32).to_le_bytes());
                }
            }
            out.extend([0u8, 0u8]);
        }
        out
    }

    /// Flat metal outlines per layer: `(plane axis, position, closed outlines)`,
    /// in-plane coordinates in metres, for fabrication output.
    pub fn flat_layers(&self) -> Vec<(usize, f64, Vec<Vec<[f64; 2]>>)> {
        let lib = crate::material::library();
        let mut layers: Vec<(usize, f64, Vec<Vec<[f64; 2]>>)> = Vec::new();
        let mut add_to = |axis: usize, at: f64, outline: Vec<[f64; 2]>| {
            if let Some(l) = layers.iter_mut().find(|l| l.0 == axis && (l.1 - at).abs() < 1e-9) {
                l.2.push(outline);
            } else {
                layers.push((axis, at, vec![outline]));
            }
        };
        for s in &self.solids {
            if lib.iter().find(|m| m.name == s.material).is_some_and(|m| !m.is_conductor()) {
                continue;
            }
            match &s.shape {
                Shape::Box { min, max } => {
                    let Some(axis) = (0..3).find(|k| max[*k] - min[*k] <= 0.0) else { continue };
                    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                    add_to(axis, min[axis], vec![[min[u], min[v]], [max[u], min[v]], [max[u], max[v]], [min[u], max[v]]]);
                }
                Shape::Polygon { axis, at, pts, thick } if *thick <= 0.0 => add_to(*axis, *at, pts.clone()),
                _ => {}
            }
        }
        layers
    }

    /// One layer as RS-274X Gerber (metric), for PCB fabrication.
    pub fn to_gerber(&self, axis: usize, at: f64) -> String {
        let mut s = String::from("G04 Sense Sim antenna layer*\n%FSLAX46Y46*%\n%MOMM*%\n%LPD*%\n");
        let num = |v: f64| format!("{}", (v * 1e3 * 1e6).round() as i64);
        for (a, h, outlines) in self.flat_layers() {
            if a != axis || (h - at).abs() > 1e-9 {
                continue;
            }
            for o in outlines {
                let Some(first) = o.first() else { continue };
                s.push_str("G36*\n");
                s.push_str(&format!("X{}Y{}D02*\nG01*\n", num(first[0]), num(first[1])));
                for p in o.iter().skip(1).chain(std::iter::once(first)) {
                    s.push_str(&format!("X{}Y{}D01*\n", num(p[0]), num(p[1])));
                }
                s.push_str("G37*\n");
            }
        }
        s.push_str("M02*\n");
        s
    }

    /// Flat metal as a DXF drawing (millimetres), one layer per plane.
    pub fn to_dxf(&self) -> String {
        let mut s = String::from("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
        for (i, (_, at, outlines)) in self.flat_layers().iter().enumerate() {
            for o in outlines {
                s.push_str(&format!("0\nLWPOLYLINE\n8\nLAYER{i}\n90\n{}\n70\n1\n38\n{:.6}\n", o.len(), at * 1e3));
                for p in o {
                    s.push_str(&format!("10\n{:.6}\n20\n{:.6}\n", p[0] * 1e3, p[1] * 1e3));
                }
            }
        }
        s.push_str("0\nENDSEC\n0\nEOF\n");
        s
    }

    /// Bounds of everything, ports included.
    pub fn bounds(&self) -> (P3, P3) {
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for s in &self.solids {
            let (a, b) = s.shape.bounds();
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        for p in &self.ports {
            for q in [p.a, p.b] {
                for k in 0..3 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
            }
        }
        if lo[0] > hi[0] {
            return ([0.0; 3], [0.0; 3]);
        }
        (lo, hi)
    }

    /// Which solvers apply: wires only → wire MoM; conducting sheets and
    /// solids' surfaces → surface MoM; dielectrics force FDTD.
    pub fn solver_fit(&self) -> SolverFit {
        // An unknown name is not a conductor: `csx::plan` then refuses it by name.
        let conductor = |n: &str| crate::material::by_name(n).is_some_and(|m| m.is_conductor());
        let all_conductors = self.solids.iter().all(|s| conductor(&s.material));
        let all_wires = !self.solids.is_empty() && self.solids.iter().all(|s| s.shape.is_wire());
        // With layers, the method of moments needs all the metal in one flat plane: FDTD builds them.
        let in_layers = self.stack.as_ref().is_some_and(|s| !s.is_empty());
        SolverFit {
            wire_mom: all_wires && all_conductors && !in_layers,
            surface_mom: all_conductors && !self.solids.is_empty() && !self.solids.iter().any(|s| s.shape.is_wire()) && (!in_layers || self.flat_height().is_some()),
            fdtd: !self.solids.is_empty(),
        }
    }

    /// The height all the metal lies at, if it's flat metal in one plane of constant z.
    pub fn flat_height(&self) -> Option<f64> {
        let mut height = None;
        for s in &self.solids {
            let z = match &s.shape {
                Shape::Polygon { axis: 2, at, thick, .. } if *thick <= 0.0 => *at,
                Shape::Box { min, max } if max[2] <= min[2] => min[2],
                _ => return None,
            };
            if height.is_some_and(|h: f64| (h - z).abs() > 1e-9) {
                return None;
            }
            height = Some(z);
        }
        height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(t: &[[P3; 3]]) -> f64 {
        t.iter().map(|t| norm(cross(sub(t[1], t[0]), sub(t[2], t[0]))) * 0.5).sum()
    }

    #[test]
    fn a_flat_box_is_one_sheet_whichever_way_it_faces() {
        for (min, max) in [([0.0, 0.0, 0.0], [2.0, 3.0, 0.0]), ([0.0, 0.0, 0.0], [2.0, 0.0, 3.0]), ([0.0, 0.0, 0.0], [0.0, 2.0, 3.0])] {
            let (t, l) = tessellate(&Shape::Box { min, max });
            assert_eq!(t.len(), 2, "{min:?} {max:?}");
            assert!((area(&t) - 6.0).abs() < 1e-12, "{}", area(&t));
            assert!(l.is_empty());
        }
    }

    #[test]
    fn a_solid_box_has_all_its_faces() {
        let (t, _) = tessellate(&Shape::Box { min: [0.0; 3], max: [1.0, 2.0, 3.0] });
        assert_eq!(t.len(), 12);
        assert!((area(&t) - 22.0).abs() < 1e-12);
    }

    #[test]
    fn concave_and_keyhole_outlines_triangulate_to_their_area() {
        let l = [[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]];
        let shoelace = |t: &[usize; 3], p: &[[f64; 2]]| {
            let (a, b, c) = (p[t[0]], p[t[1]], p[t[2]]);
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
        };
        let sum: f64 = triangulate(&l).iter().map(|t| shoelace(t, &l)).sum();
        assert!((sum - 3.0).abs() < 1e-12, "{sum}");
        // A 4 × 4 plate with a 2 × 1 slot cut through a zero-width keyhole.
        let k = [[-2.0, -2.0], [2.0, -2.0], [2.0, 2.0], [-2.0, 2.0], [-2.0, -2.0], [-1.0, -0.5], [-1.0, 0.5], [1.0, 0.5], [1.0, -0.5], [-1.0, -0.5]];
        let sum: f64 = triangulate(&k).iter().map(|t| shoelace(t, &k)).sum();
        assert!((sum - 14.0).abs() < 1e-9, "{sum}");
    }

    #[test]
    fn stl_round_trips_in_millimetres() {
        let g = Geometry { solids: vec![Solid { name: "p".into(), material: "PEC".into(), shape: Shape::Box { min: [0.0; 3], max: [0.01, 0.02, 0.0] } }], ..Default::default() };
        let t = read_stl(&g.to_stl(), 0.001).unwrap();
        assert_eq!(t.len(), 2);
        assert!((area(&t) - 2e-4).abs() < 1e-9, "{}", area(&t));
    }

    #[test]
    fn ascii_stl_is_read_and_garbage_is_refused() {
        let s = "solid x\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid x\n";
        assert_eq!(read_stl(s.as_bytes(), 1.0).unwrap().len(), 1);
        assert!(read_stl(b"not a model", 1.0).is_err());
    }

    #[test]
    fn gerber_and_dxf_cover_each_flat_layer() {
        let p = crate::project::Project::from_template("patch_probe", None).unwrap();
        let g = p.geometry().unwrap();
        let layers = g.flat_layers();
        assert_eq!(layers.len(), 2, "ground and patch");
        for (axis, at, _) in &layers {
            let gbr = g.to_gerber(*axis, *at);
            assert!(gbr.contains("G36*") && gbr.ends_with("M02*\n"), "{gbr}");
        }
        assert_eq!(g.to_dxf().matches("LWPOLYLINE").count(), 2);
    }
}
