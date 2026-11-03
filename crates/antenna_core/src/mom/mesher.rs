//! Turns a geometry's conducting solids into one conformal, welded triangle mesh.
//!
//! Every flat piece of metal is a polygon on a plane: sheets, box faces, cylinder
//! caps, tilted sheets, port bridge strips. Conformity across them works like this:
//!
//! 1. All polygon edges, feed lines and plane/plane intersection lines go into a
//!    pool of 3D feature segments, broken against each other at every endpoint
//!    and crossing, so two overlapping segments end up as the same atomic pieces.
//! 2. Each atomic piece is divided into equal parts, a rule symmetric in its ends,
//!    so every face sharing a piece sees exactly the same points.
//! 3. Each plane gets a constrained Delaunay triangulation of the pieces lying on
//!    its metal, plus a hexagonal lattice of interior points. Triangles whose
//!    centroid is on metal are kept, so coplanar sheets merge and holes stay open.
//! 4. Everything is welded by position.
//!
//! Curved surfaces (sphere, cylinder side) and big imported models are "soup":
//! already final triangles that join the rest where their vertices coincide. A
//! port ending on soup attaches to an existing edge of it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::PI;

use anyhow::{Result, bail};
use spade::{AngleLimit, ConstrainedDelaunayTriangulation, Point2, RefinementParameters, Triangulation};

use crate::geometry::{Geometry, P3, PortDef, Shape, Solid, add, cross, dot, norm, plane_point, scale, sub, unit};

use super::weld::Welder;

/// Boundary pieces are this fraction of the edge limit, which leaves room for the
/// diagonals across a narrow strip.
const FILL: f64 = 0.9;
/// Spacing of the interior lattice as a fraction of the edge limit.
const LATTICE: f64 = 0.98;
/// A `Tris` shape up to this size is treated as flat faces, so it can be imprinted
/// and join sheets along partial edges. Bigger ones are soup.
const FLAT_TRIS: usize = 64;
/// And on no more planes than this.
const FLAT_PLANES: usize = 12;
/// Interior triangles sharper than this (degrees) get refined.
const MIN_ANGLE: f64 = 22.0;
/// The most points one plane's triangulation may reach, outside the metal included. Three unknowns
/// come to about one point, so `MAX_UNKNOWNS` is far below this.
const MAX_VERTICES: usize = 400_000;

/// Whether triangles go as one surface remade to the mesh's size rather than as flat faces imprinted
/// plane by plane. A few flat faces can be imprinted. Many planes means a curved surface, however few
/// triangles it has, and those are better remade as one piece.
pub fn goes_as_soup(tris: &[[P3; 3]], tol: f64) -> bool {
    if tris.len() > FLAT_TRIS {
        return true;
    }
    let mut planes: Vec<(P3, f64)> = Vec::new();
    for t in tris {
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        if norm(n) > 0.0 {
            let n = unit(n);
            let d = dot(n, t[0]);
            if !planes.iter().any(|(m, e)| dot(*m, n) > 1.0 - 1e-9 && (e - d).abs() <= tol) {
                planes.push((n, d));
            }
        }
    }
    planes.len() > FLAT_PLANES
}

/// The finished mesh.
#[derive(Clone, Debug, Default)]
pub struct SurfaceMesh {
    pub verts: Vec<P3>,
    pub tris: Vec<[u32; 3]>,
    /// Which solid each triangle belongs to; `None` is a port bridge strip.
    pub solid: Vec<Option<usize>>,
    /// Per port, the mesh edges of its feed line.
    pub feeds: Vec<Vec<[u32; 2]>>,
    pub tol: f64,
}

/// Shape statistics of a mesh, for reports and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeshQuality {
    pub triangles: usize,
    pub min_angle_deg: f64,
    pub max_edge: f64,
    pub min_area: f64,
    /// Edges by how many triangles share them.
    pub boundary_edges: usize,
    pub interior_edges: usize,
    pub junction_edges: usize,
}

impl SurfaceMesh {
    pub fn quality(&self) -> MeshQuality {
        let mut q = MeshQuality { triangles: self.tris.len(), min_angle_deg: 180.0, min_area: f64::MAX, ..Default::default() };
        let mut edges: HashMap<(u32, u32), usize> = HashMap::new();
        for t in &self.tris {
            let p = t.map(|i| self.verts[i as usize]);
            q.min_area = q.min_area.min(0.5 * norm(cross(sub(p[1], p[0]), sub(p[2], p[0]))));
            for i in 0..3 {
                let (a, b, c) = (p[i], p[(i + 1) % 3], p[(i + 2) % 3]);
                q.max_edge = q.max_edge.max(norm(sub(b, a)));
                let (u, v) = (unit(sub(b, a)), unit(sub(c, a)));
                q.min_angle_deg = q.min_angle_deg.min(dot(u, v).clamp(-1.0, 1.0).acos().to_degrees());
                *edges.entry((t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3]))).or_default() += 1;
            }
        }
        for n in edges.values() {
            match n {
                1 => q.boundary_edges += 1,
                2 => q.interior_edges += 1,
                _ => q.junction_edges += 1,
            }
        }
        q
    }
}

/// Meshes every solid and port of `geom` with edges no longer than `h`.
pub fn mesh_geometry(geom: &Geometry, h: f64, lambda: f64) -> Result<SurfaceMesh> {
    let (lo, hi) = geom.bounds();
    let tol = 1e-8 * norm(sub(hi, lo)).max(1e-9);
    if geom.ground {
        for s in &geom.solids {
            if s.shape.bounds().0[2] < -tol {
                bail!("'{}' goes below the ground plane: everything has to stay at z ≥ 0", s.name);
            }
        }
    }
    let pins: Vec<P3> = geom.ports.iter().flat_map(|p| [p.a, p.b]).collect();
    // Fine enough at a feed to see its gap, without going far past what the mesh is otherwise.
    let pin_size = geom.ports.iter().map(|p| norm(sub(p.b, p.a)).max(p.width) * 1.5).fold(h * 0.3, f64::min).max(h * 0.05);
    let mut m = Mesher { tol, h, lambda, weld: Welder::new(tol), planes: Vec::new(), faces: Vec::new(), soup: Vec::new(), feeds: Vec::new(), extra: Vec::new(), pins, pin_size };
    for (i, s) in geom.solids.iter().enumerate() {
        m.add_solid(i, s)?;
    }
    for port in &geom.ports {
        m.add_port(port, geom.ground)?;
    }
    let chains = m.feature_chains();
    let mut tris = std::mem::take(&mut m.soup);
    for plane in 0..m.planes.len() {
        m.triangulate_plane(plane, &chains, &mut tris)?;
    }
    m.finish(geom, tris)
}

type Triangle = ([u32; 3], Option<usize>);

/// An atomic feature piece, divided. `keep` says it has to be a constraint even
/// with metal to both sides, because something else joins the plane along it.
struct Chain {
    ids: Vec<u32>,
    keep: bool,
}

struct Plane {
    origin: P3,
    n: P3,
    u: P3,
    v: P3,
}

impl Plane {
    fn uv(&self, p: P3) -> [f64; 2] {
        let d = sub(p, self.origin);
        [dot(d, self.u), dot(d, self.v)]
    }
    fn at(&self, q: [f64; 2]) -> P3 {
        add(self.origin, add(scale(self.u, q[0]), scale(self.v, q[1])))
    }
    fn height(&self, p: P3) -> f64 {
        dot(sub(p, self.origin), self.n)
    }
}

struct Face {
    plane: usize,
    pts: Vec<P3>,
    uv: Vec<[f64; 2]>,
    solid: Option<usize>,
    /// A port end this far off the plane still touches the face (a sheet that
    /// stands in for a thin plate).
    snap: f64,
}

fn inside_polygon(uv: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut c = false;
    for i in 0..uv.len() {
        let (a, b) = (uv[i], uv[(i + 1) % uv.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            c = !c;
        }
    }
    c
}

fn distance_to_segment_2d(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
}

fn on_boundary(uv: &[[f64; 2]], p: [f64; 2], tol: f64) -> bool {
    (0..uv.len()).any(|i| distance_to_segment_2d(p, uv[i], uv[(i + 1) % uv.len()]) <= tol)
}

fn lerp(a: P3, b: P3, t: f64) -> P3 {
    add(a, scale(sub(b, a), t))
}

#[derive(Clone, Copy)]
enum Contact {
    Ground(P3),
    Face { plane: usize, at: P3 },
    Soup(u32),
}

struct Mesher {
    tol: f64,
    h: f64,
    lambda: f64,
    weld: Welder,
    planes: Vec<Plane>,
    faces: Vec<Face>,
    soup: Vec<Triangle>,
    feeds: Vec<[P3; 2]>,
    /// Attachment lines of strips built as soup: plates must imprint them.
    extra: Vec<[P3; 2]>,
    /// Where feeds land, which a remade surface keeps as vertices, and how fine it goes there.
    pins: Vec<P3>,
    pin_size: f64,
}

impl Mesher {
    fn plane_for(&mut self, p: P3, n: P3) -> usize {
        for (i, pl) in self.planes.iter().enumerate() {
            if norm(cross(pl.n, n)) < 1e-7 && pl.height(p).abs() <= self.tol {
                return i;
            }
        }
        let axis = (0..3).find(|k| (n[*k].abs() - 1.0).abs() < 1e-12);
        let (n, u, v) = match axis {
            Some(k) => {
                let mut e = [[0.0; 3]; 3];
                e[k][k] = 1.0;
                e[(k + 1) % 3][(k + 1) % 3] = 1.0;
                e[(k + 2) % 3][(k + 2) % 3] = 1.0;
                (e[k], e[(k + 1) % 3], e[(k + 2) % 3])
            }
            None => {
                let seed = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
                let u = unit(cross(n, seed));
                (n, u, cross(n, u))
            }
        };
        self.planes.push(Plane { origin: scale(n, dot(p, n)), n, u, v });
        self.planes.len() - 1
    }

    fn add_face(&mut self, plane: usize, pts: Vec<P3>, solid: Option<usize>, snap: f64, name: &str) -> Result<()> {
        let mut clean: Vec<P3> = Vec::with_capacity(pts.len());
        for p in pts {
            if clean.last().is_none_or(|q| norm(sub(*q, p)) > self.tol) {
                clean.push(p);
            }
        }
        while clean.len() > 1 && norm(sub(clean[0], clean[clean.len() - 1])) <= self.tol {
            clean.pop();
        }
        if clean.len() < 3 {
            bail!("'{name}' needs at least three distinct outline points");
        }
        let uv = clean.iter().map(|p| self.planes[plane].uv(*p)).collect();
        self.faces.push(Face { plane, pts: clean, uv, solid, snap });
        Ok(())
    }

    fn add_solid(&mut self, index: usize, s: &Solid) -> Result<()> {
        let solid = Some(index);
        match &s.shape {
            Shape::Box { min, max } => self.add_box(*min, *max, solid, &s.name),
            Shape::Polygon { axis, at, pts, thick } => {
                let three: Vec<P3> = pts.iter().map(|p| plane_point(*axis, *at, *p)).collect();
                if three.len() < 3 {
                    bail!("'{}' needs at least three outline points", s.name);
                }
                let mut n = [0.0; 3];
                n[*axis] = 1.0;
                let plane = self.plane_for(three[0], n);
                self.add_face(plane, three, solid, thick.abs(), &s.name)
            }
            Shape::Cylinder { a, b, r } => self.add_cylinder(*a, *b, *r, solid, &s.name),
            Shape::Sphere { c, r } => self.add_sphere(*c, *r, solid, &s.name),
            Shape::Tris { tris, .. } => self.add_tris(tris, solid, &s.name),
            Shape::Wire { .. } => bail!("'{}' is a wire, which the surface solver can't mesh", s.name),
        }
    }

    fn add_box(&mut self, min: P3, max: P3, solid: Option<usize>, name: &str) -> Result<()> {
        let e = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        // Two faces closer than this would be nearly coincident: one sheet stands in.
        let thin = (0.05 * self.h).max(self.tol);
        let flat: Vec<usize> = (0..3).filter(|k| e[*k].abs() <= thin).collect();
        if flat.len() > 1 {
            bail!("'{name}' is a box with no area: it needs size along at least two axes");
        }
        let rect = |axis: usize, at: f64| -> Vec<P3> {
            let (a1, a2) = ((axis + 1) % 3, (axis + 2) % 3);
            let p = |a: f64, b: f64| plane_point(axis, at, [a, b]);
            vec![p(min[a1], min[a2]), p(max[a1], min[a2]), p(max[a1], max[a2]), p(min[a1], max[a2])]
        };
        let normal = |axis: usize| {
            let mut n = [0.0; 3];
            n[axis] = 1.0;
            n
        };
        if let Some(&axis) = flat.first() {
            let at = 0.5 * (min[axis] + max[axis]);
            let pts = rect(axis, at);
            let plane = self.plane_for(pts[0], normal(axis));
            return self.add_face(plane, pts, solid, 0.5 * e[axis].abs(), name);
        }
        for axis in 0..3 {
            for at in [min[axis], max[axis]] {
                let pts = rect(axis, at);
                let plane = self.plane_for(pts[0], normal(axis));
                self.add_face(plane, pts, solid, 0.0, name)?;
            }
        }
        Ok(())
    }

    fn add_cylinder(&mut self, a: P3, b: P3, r: f64, solid: Option<usize>, name: &str) -> Result<()> {
        let len = norm(sub(b, a));
        if len <= self.tol || !(r > 0.0) {
            bail!("cylinder '{name}' needs a length and a radius above zero");
        }
        let axis = unit(sub(b, a));
        let seed = if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let e1 = unit(cross(axis, seed));
        let e2 = cross(axis, e1);
        // Chord and row height together keep the quads' diagonals under the limit.
        let half_chord = (0.35 * self.h / r).min(1.0);
        let nt = ((PI / half_chord.asin()).ceil() as usize).max(8);
        let chord = 2.0 * r * (PI / nt as f64).sin();
        let rim = |c: P3, k: usize| {
            let t = 2.0 * PI * (k % nt) as f64 / nt as f64;
            add(c, add(scale(e1, r * t.cos()), scale(e2, r * t.sin())))
        };
        for c in [a, b] {
            let plane = self.plane_for(c, axis);
            self.add_face(plane, (0..nt).map(|k| rim(c, k)).collect(), solid, 0.0, name)?;
            // A plate under the cap must keep these lines for the side to join it.
            self.extra.extend((0..nt).map(|k| [rim(c, k), rim(c, k + 1)]));
        }
        // The side's end rings are the caps' outlines, so it stays conformal with them.
        let rows = ((len / (0.99 * (self.h * self.h - chord * chord).max(0.0).sqrt())).ceil() as usize).max(1);
        let ring = |iz: usize| if iz == 0 { a } else if iz == rows { b } else { lerp(a, b, iz as f64 / rows as f64) };
        for iz in 0..rows {
            for k in 0..nt {
                let q = [self.weld.id(rim(ring(iz), k)), self.weld.id(rim(ring(iz), k + 1)), self.weld.id(rim(ring(iz + 1), k + 1)), self.weld.id(rim(ring(iz + 1), k))];
                if (iz + k) % 2 == 0 {
                    self.soup.push(([q[0], q[1], q[2]], solid));
                    self.soup.push(([q[0], q[2], q[3]], solid));
                } else {
                    self.soup.push(([q[0], q[1], q[3]], solid));
                    self.soup.push(([q[1], q[2], q[3]], solid));
                }
            }
        }
        Ok(())
    }

    fn add_sphere(&mut self, c: P3, r: f64, solid: Option<usize>, name: &str) -> Result<()> {
        if !(r > 0.0) {
            bail!("sphere '{name}' needs a radius above zero");
        }
        let g = (1.0 + 5f64.sqrt()) / 2.0;
        let ico: [P3; 12] = [[-1.0, g, 0.0], [1.0, g, 0.0], [-1.0, -g, 0.0], [1.0, -g, 0.0], [0.0, -1.0, g], [0.0, 1.0, g], [0.0, -1.0, -g], [0.0, 1.0, -g], [g, 0.0, -1.0], [g, 0.0, 1.0], [-g, 0.0, -1.0], [-g, 0.0, 1.0]];
        let faces: [[usize; 3]; 20] = [[0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11], [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8], [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9], [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1]];
        let point = |f: &[usize; 3], i: usize, j: usize, n: usize| {
            let (w0, w1, w2) = ((n - i - j) as f64, i as f64, j as f64);
            let p = add(add(scale(ico[f[0]], w0), scale(ico[f[1]], w1)), scale(ico[f[2]], w2));
            add(c, scale(unit(p), r))
        };
        // The longest chord of a geodesic grid is near the middle of a face.
        let longest = |n: usize| -> f64 {
            let f = &faces[0];
            let mut best = 0.0f64;
            for i in 0..n {
                for j in 0..n - i {
                    let (a, b, d) = (point(f, i, j, n), point(f, i + 1, j, n), point(f, i, j + 1, n));
                    best = best.max(norm(sub(a, b))).max(norm(sub(a, d))).max(norm(sub(b, d)));
                }
            }
            best
        };
        let mut n = 1;
        while longest(n) > self.h && n < 512 {
            n += 1;
        }
        for f in &faces {
            for i in 0..n {
                for j in 0..n - i {
                    let (a, b, d) = (self.weld.id(point(f, i, j, n)), self.weld.id(point(f, i + 1, j, n)), self.weld.id(point(f, i, j + 1, n)));
                    self.soup.push(([a, b, d], solid));
                    if i + j + 1 < n {
                        let e = self.weld.id(point(f, i + 1, j + 1, n));
                        self.soup.push(([b, e, d], solid));
                    }
                }
            }
        }
        Ok(())
    }

    fn add_tris(&mut self, tris: &[[P3; 3]], solid: Option<usize>, name: &str) -> Result<()> {
        if !goes_as_soup(tris, self.tol) {
            for t in tris {
                let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
                if norm(n) <= self.tol * self.tol {
                    continue;
                }
                let plane = self.plane_for(t[0], unit(n));
                self.add_face(plane, t.to_vec(), solid, 0.0, name)?;
            }
            return Ok(());
        }
        // Remade to the mesh's size first: a surface as it was modelled or imported has slivers and
        // tiny facets, and in the method of moments a sliver is a wrong answer.
        let mut raw = crate::modeler::mesh::Mesh::default();
        for t in tris {
            raw.push_tri(t[0], t[1], t[2], 0);
        }
        let options = crate::modeler::remesh::Options { target: 0.75 * self.h, pins: self.pins.clone(), pin_size: self.pin_size, passes: 6 };
        let remade = crate::modeler::remesh::remesh(&raw.welded(self.tol), &options);
        let mut ids: Vec<[u32; 3]> = remade.tris.iter().map(|t| t.map(|v| self.weld.id(remade.verts[v as usize]))).filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect();
        self.refine_soup(&mut ids);
        self.soup.extend(ids.into_iter().map(|t| (t, solid)));
        Ok(())
    }

    /// Splits every edge above the limit at its midpoint until none is left. The
    /// marks are per edge, so neighbours always agree.
    fn refine_soup(&mut self, tris: &mut Vec<[u32; 3]>) {
        loop {
            let mut mids: HashMap<(u32, u32), u32> = HashMap::new();
            let key = |a: u32, b: u32| (a.min(b), a.max(b));
            for t in tris.iter() {
                for i in 0..3 {
                    let (a, b) = (t[i], t[(i + 1) % 3]);
                    let (pa, pb) = (self.weld.pts[a as usize], self.weld.pts[b as usize]);
                    if norm(sub(pa, pb)) > self.h {
                        let m = self.weld.id(lerp(pa, pb, 0.5));
                        mids.insert(key(a, b), m);
                    }
                }
            }
            if mids.is_empty() {
                return;
            }
            let mut out = Vec::with_capacity(tris.len() * 2);
            for t in tris.iter() {
                let m: Vec<Option<u32>> = (0..3).map(|i| mids.get(&key(t[i], t[(i + 1) % 3])).copied()).collect();
                match (m[0], m[1], m[2]) {
                    (None, None, None) => out.push(*t),
                    (Some(a), Some(b), Some(c)) => {
                        out.extend([[t[0], a, c], [a, t[1], b], [c, b, t[2]], [a, b, c]]);
                    }
                    _ => {
                        // Rotate so the first marked edge is 0-1, then fan.
                        let r = (0..3).find(|i| m[*i].is_some()).unwrap_or(0);
                        let v = [t[r], t[(r + 1) % 3], t[(r + 2) % 3]];
                        let (m01, m12, m20) = (m[r], m[(r + 1) % 3], m[(r + 2) % 3]);
                        let m01 = m01.unwrap_or(v[0]);
                        match (m12, m20) {
                            (Some(b), None) => out.extend([[v[0], m01, v[2]], [m01, v[1], b], [m01, b, v[2]]]),
                            (None, Some(c)) => out.extend([[v[0], m01, c], [m01, v[1], v[2]], [m01, v[2], c]]),
                            _ => out.extend([[v[0], m01, v[2]], [m01, v[1], v[2]]]),
                        }
                    }
                }
            }
            *tris = out;
        }
    }

    fn metal_at(&self, plane: usize, q: [f64; 2]) -> bool {
        self.faces.iter().any(|f| f.plane == plane && (inside_polygon(&f.uv, q) || on_boundary(&f.uv, q, self.tol)))
    }

    /// How far metal extends from `at` along the in-plane direction `dir`, as
    /// `(negative, positive)` distances; `at` itself is taken to be on metal.
    fn section(&self, plane: usize, at: P3, dir: P3) -> (f64, f64) {
        let pl = &self.planes[plane];
        let c = pl.uv(at);
        let d = [dot(dir, pl.u), dot(dir, pl.v)];
        let along = |p: [f64; 2]| (p[0] - c[0]) * d[0] + (p[1] - c[1]) * d[1];
        let off = |p: [f64; 2]| d[0] * (p[1] - c[1]) - d[1] * (p[0] - c[0]);
        let mut ts = vec![0.0];
        for f in self.faces.iter().filter(|f| f.plane == plane) {
            for i in 0..f.uv.len() {
                let (a, b) = (f.uv[i], f.uv[(i + 1) % f.uv.len()]);
                let (da, db) = (off(a), off(b));
                if da.abs() <= self.tol {
                    ts.push(along(a));
                }
                if db.abs() <= self.tol {
                    ts.push(along(b));
                }
                if da * db < 0.0 && da.abs() > self.tol && db.abs() > self.tol {
                    let u = da / (da - db);
                    ts.push(along([a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]));
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|x, y| (*x - *y).abs() <= self.tol);
        let start = ts.iter().position(|t| t.abs() <= self.tol).unwrap_or(0);
        let is_metal = |t: f64| self.metal_at(plane, [c[0] + d[0] * t, c[1] + d[1] * t]);
        let mut hi = 0.0;
        for w in ts[start..].windows(2) {
            if !is_metal(0.5 * (w[0] + w[1])) {
                break;
            }
            hi = w[1];
        }
        let mut lo = 0.0;
        for w in ts[..=start].windows(2).rev() {
            if !is_metal(0.5 * (w[0] + w[1])) {
                break;
            }
            lo = w[0];
        }
        (lo, hi)
    }

    /// What a port end rests on, if anything.
    fn contact(&self, e: P3, ground: bool) -> Option<Contact> {
        if ground && e[2].abs() <= self.tol {
            return Some(Contact::Ground([e[0], e[1], 0.0]));
        }
        let mut best: Option<(f64, Contact)> = None;
        for f in &self.faces {
            let pl = &self.planes[f.plane];
            let height = pl.height(e);
            if height.abs() <= self.tol.max(f.snap + self.tol) {
                let q = pl.uv(e);
                if (inside_polygon(&f.uv, q) || on_boundary(&f.uv, q, self.tol)) && best.as_ref().is_none_or(|b| height.abs() < b.0) {
                    best = Some((height.abs(), Contact::Face { plane: f.plane, at: pl.at(q) }));
                }
            }
        }
        if best.is_none() {
            // Facets sag below a curved surface, hence the generous reach.
            let reach = (0.25 * self.h).max(self.tol);
            for (t, _) in &self.soup {
                for &v in t {
                    let d = norm(sub(self.weld.pts[v as usize], e));
                    if d <= self.h && best.as_ref().is_none_or(|b| d < b.0) && distance_to_triangle(e, t.map(|i| self.weld.pts[i as usize])) <= reach {
                        best = Some((d, Contact::Soup(v)));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }

    fn add_port(&mut self, port: &PortDef, ground: bool) -> Result<()> {
        if norm(sub(port.b, port.a)) <= self.tol {
            bail!("port '{}' has zero length: its a and b are the same point", port.name);
        }
        let missing = |end: &str| anyhow::anyhow!("port '{}': its end {end} doesn't touch any conductor", port.name);
        let ca = self.contact(port.a, ground).ok_or_else(|| missing("a"))?;
        let cb = self.contact(port.b, ground).ok_or_else(|| missing("b"))?;
        if matches!(ca, Contact::Soup(_)) || matches!(cb, Contact::Soup(_)) {
            return self.add_soup_strip(port, ca, cb);
        }
        let end = |c: Contact| match c {
            Contact::Ground(p) | Contact::Face { at: p, .. } => p,
            Contact::Soup(_) => unreachable!(),
        };
        let (a, b) = (end(ca), end(cb));
        let dir = sub(b, a);
        if norm(dir) <= self.tol {
            bail!("port '{}' collapses to a point on the sheet it touches", port.name);
        }
        let d = unit(dir);
        let mid = lerp(a, b, 0.5);

        // Both ends on one sheet with metal under the middle: no gap to bridge, so
        // the feed is a line of edges across the metal.
        if let (Contact::Face { plane: pa, .. }, Contact::Face { plane: pb, .. }) = (ca, cb)
            && pa == pb
            && dot(self.planes[pa].n, d).abs() < 1e-6
            && self.metal_at(pa, self.planes[pa].uv(mid))
        {
            let w = unit(cross(self.planes[pa].n, d));
            let (lo, hi) = self.section(pa, mid, w);
            let half = if port.width > 0.0 { 0.5 * port.width } else { f64::INFINITY };
            let (lo, hi) = (lo.max(-half), hi.min(half));
            if hi - lo <= self.tol {
                bail!("port '{}' has no metal to either side of it", port.name);
            }
            self.feeds.push([add(mid, scale(w, lo)), add(mid, scale(w, hi))]);
            return Ok(());
        }

        let w = self.width_direction(d, &[ca, cb]);
        let width = if port.width > 0.0 { port.width } else { self.lambda / 40.0 };
        let (mut lo, mut hi) = (-0.5 * width, 0.5 * width);
        for c in [ca, cb] {
            if let Contact::Face { plane, at } = c
                && dot(self.planes[plane].n, w).abs() < 1e-6
            {
                let (s0, s1) = self.section(plane, at, w);
                lo = lo.max(s0);
                hi = hi.min(s1);
            }
        }
        if hi - lo <= self.tol {
            bail!("port '{}' touches its sheet only at a point: move it away from the corner", port.name);
        }
        let pts = vec![add(a, scale(w, lo)), add(a, scale(w, hi)), add(b, scale(w, hi)), add(b, scale(w, lo))];
        let plane = self.plane_for(a, unit(cross(d, w)));
        self.add_face(plane, pts, None, 0.0, &port.name)?;
        self.feeds.push([add(mid, scale(w, lo)), add(mid, scale(w, hi))]);
        Ok(())
    }

    /// The strip's width axis: perpendicular to the gap, in the plane of a sheet
    /// that contains the gap direction; otherwise x (y for a port along x).
    fn width_direction(&self, d: P3, contacts: &[Contact]) -> P3 {
        for c in contacts {
            if let Contact::Face { plane, .. } = c {
                let n = self.planes[*plane].n;
                if dot(n, d).abs() < 1e-6 {
                    return unit(cross(n, d));
                }
            }
        }
        let e = if d[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        unit(sub(e, scale(d, dot(e, d))))
    }

    /// A bridge strip ending on a curved surface: it attaches to an existing edge
    /// there and is built directly as triangles.
    fn add_soup_strip(&mut self, port: &PortDef, ca: Contact, cb: Contact) -> Result<()> {
        let w_guess = self.width_direction(unit(sub(port.b, port.a)), &[ca, cb]);
        let neighbours = |v: u32, soup: &[Triangle]| -> Vec<u32> {
            let mut out = Vec::new();
            for (t, _) in soup {
                if t.contains(&v) {
                    out.extend(t.iter().copied().filter(|x| *x != v));
                }
            }
            out.sort_unstable();
            out.dedup();
            out
        };
        let soup_edge = |c: Contact| -> Option<[P3; 2]> {
            let Contact::Soup(v) = c else { return None };
            let p = self.weld.pts[v as usize];
            let best = neighbours(v, &self.soup).into_iter().map(|n| self.weld.pts[n as usize]).max_by(|x, y| dot(unit(sub(*x, p)), w_guess).abs().total_cmp(&dot(unit(sub(*y, p)), w_guess).abs()))?;
            Some(if dot(sub(best, p), w_guess) >= 0.0 { [p, best] } else { [best, p] })
        };
        let (ea, eb) = (soup_edge(ca), soup_edge(cb));
        let span = ea.or(eb).map_or(0.0, |e| norm(sub(e[1], e[0])));
        if span <= self.tol {
            bail!("port '{}' can't attach to the curved surface it touches", port.name);
        }
        let flat_end = |c: Contact| -> [P3; 2] {
            let at = match c {
                Contact::Ground(p) | Contact::Face { at: p, .. } => p,
                Contact::Soup(_) => unreachable!(),
            };
            [add(at, scale(w_guess, -0.5 * span)), add(at, scale(w_guess, 0.5 * span))]
        };
        let pa = ea.unwrap_or_else(|| flat_end(ca));
        let pb = eb.unwrap_or_else(|| flat_end(cb));
        for (c, e) in [(ca, pa), (cb, pb)] {
            if matches!(c, Contact::Face { .. }) {
                self.extra.push(e);
            }
        }
        let gap = norm(sub(lerp(pa[0], pa[1], 0.5), lerp(pb[0], pb[1], 0.5)));
        let rows = 2 * ((gap / (2.0 * FILL * self.h)).ceil() as usize).max(1);
        let row = |i: usize, side: usize| lerp(pa[side], pb[side], i as f64 / rows as f64);
        for i in 0..rows {
            let q = [self.weld.id(row(i, 0)), self.weld.id(row(i, 1)), self.weld.id(row(i + 1, 1)), self.weld.id(row(i + 1, 0))];
            self.soup.push(([q[0], q[1], q[2]], None));
            self.soup.push(([q[0], q[2], q[3]], None));
        }
        self.feeds.push([row(rows / 2, 0), row(rows / 2, 1)]);
        Ok(())
    }

    /// Plane/plane intersection lines over the metal of both faces.
    fn plane_lines(&self) -> Vec<[P3; 2]> {
        let bounds: Vec<(P3, P3)> = self
            .faces
            .iter()
            .map(|f| {
                let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                for p in &f.pts {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                (lo, hi)
            })
            .collect();
        let mut out = Vec::new();
        for i in 0..self.faces.len() {
            for j in i + 1..self.faces.len() {
                let (fi, fj) = (&self.faces[i], &self.faces[j]);
                if fi.plane == fj.plane || (0..3).any(|k| bounds[i].1[k] < bounds[j].0[k] - self.tol || bounds[j].1[k] < bounds[i].0[k] - self.tol) {
                    continue;
                }
                let (pi, pj) = (&self.planes[fi.plane], &self.planes[fj.plane]);
                let u = cross(pi.n, pj.n);
                let u2 = dot(u, u);
                if u2 < 1e-14 {
                    continue;
                }
                let (di, dj) = (dot(pi.n, pi.origin), dot(pj.n, pj.origin));
                let start = scale(cross(sub(scale(pj.n, di), scale(pi.n, dj)), u), 1.0 / u2);
                let dir = scale(u, 1.0 / u2.sqrt());
                let mut ts = Vec::new();
                for (f, other) in [(fi, pj), (fj, pi)] {
                    for k in 0..f.pts.len() {
                        let (a, b) = (f.pts[k], f.pts[(k + 1) % f.pts.len()]);
                        let (da, db) = (other.height(a), other.height(b));
                        if da.abs() <= self.tol {
                            ts.push(dot(sub(a, start), dir));
                        }
                        if db.abs() <= self.tol {
                            ts.push(dot(sub(b, start), dir));
                        }
                        if da * db < 0.0 && da.abs() > self.tol && db.abs() > self.tol {
                            ts.push(dot(sub(lerp(a, b, da / (da - db)), start), dir));
                        }
                    }
                }
                ts.sort_by(f64::total_cmp);
                ts.dedup_by(|x, y| (*x - *y).abs() <= self.tol);
                for w in ts.windows(2) {
                    let m = add(start, scale(dir, 0.5 * (w[0] + w[1])));
                    let on = |f: &Face, pl: &Plane| {
                        let q = pl.uv(m);
                        inside_polygon(&f.uv, q) || on_boundary(&f.uv, q, self.tol)
                    };
                    if on(fi, pi) && on(fj, pj) {
                        out.push([add(start, scale(dir, w[0])), add(start, scale(dir, w[1]))]);
                    }
                }
            }
        }
        out
    }

    /// Breaks all features against each other and divides each atomic piece.
    fn feature_chains(&mut self) -> Vec<Chain> {
        let mut segs: BTreeMap<(u32, u32), bool> = BTreeMap::new();
        let sorted = |a: u32, b: u32| (a.min(b), a.max(b));
        for fi in 0..self.faces.len() {
            let mut count: HashMap<(u32, u32), usize> = HashMap::new();
            let n = self.faces[fi].pts.len();
            for i in 0..n {
                let (a, b) = (self.faces[fi].pts[i], self.faces[fi].pts[(i + 1) % n]);
                let (ia, ib) = (self.weld.id(a), self.weld.id(b));
                if ia != ib {
                    *count.entry(sorted(ia, ib)).or_default() += 1;
                }
            }
            // An edge walked twice is a keyhole's bridge: not a real boundary.
            for (e, c) in count {
                if c % 2 == 1 {
                    segs.entry(e).or_insert(false);
                }
            }
        }
        let lines: Vec<[P3; 2]> = self.feeds.iter().chain(&self.extra).copied().chain(self.plane_lines()).collect();
        for [a, b] in lines {
            let (ia, ib) = (self.weld.id(a), self.weld.id(b));
            if ia != ib {
                segs.insert(sorted(ia, ib), true);
            }
        }

        let (list, keeps): (Vec<(u32, u32)>, Vec<bool>) = segs.into_iter().unzip();
        let mut breaks: Vec<Vec<(f64, u32)>> = vec![Vec::new(); list.len()];
        let ends: Vec<[P3; 2]> = list.iter().map(|&(a, b)| [self.weld.pts[a as usize], self.weld.pts[b as usize]]).collect();
        let reach = 2.0 * self.tol;
        let on_segment = |e: P3, s: &[P3; 2]| -> Option<f64> {
            let d = sub(s[1], s[0]);
            let l = norm(d);
            let t = dot(sub(e, s[0]), d) / (l * l);
            (t * l > reach && (1.0 - t) * l > reach && norm(sub(e, add(s[0], scale(d, t)))) <= reach).then_some(t)
        };
        for i in 0..list.len() {
            for j in i + 1..list.len() {
                let (si, sj) = (&ends[i], &ends[j]);
                if (0..3).any(|k| si[0][k].max(si[1][k]) < sj[0][k].min(sj[1][k]) - reach || sj[0][k].max(sj[1][k]) < si[0][k].min(si[1][k]) - reach) {
                    continue;
                }
                for (e, id) in [(sj[0], list[j].0), (sj[1], list[j].1)] {
                    if let Some(t) = on_segment(e, si) {
                        breaks[i].push((t, id));
                    }
                }
                for (e, id) in [(si[0], list[i].0), (si[1], list[i].1)] {
                    if let Some(t) = on_segment(e, sj) {
                        breaks[j].push((t, id));
                    }
                }
                if let Some((t, u, p)) = crossing(si, sj, reach) {
                    let id = self.weld.id(p);
                    breaks[i].push((t, id));
                    breaks[j].push((u, id));
                }
            }
        }

        let mut pieces: BTreeMap<(u32, u32), bool> = BTreeMap::new();
        for (k, &(a, b)) in list.iter().enumerate() {
            let mut along = std::mem::take(&mut breaks[k]);
            along.sort_by(|x, y| x.0.total_cmp(&y.0));
            let mut prev = a;
            for (_, id) in along.into_iter().chain(std::iter::once((1.0, b))) {
                if id != prev {
                    *pieces.entry(sorted(prev, id)).or_insert(false) |= keeps[k];
                    prev = id;
                }
            }
        }

        let limit = FILL * self.h;
        pieces
            .into_iter()
            .map(|((a, b), keep)| {
                let (pa, pb) = (self.weld.pts[a as usize], self.weld.pts[b as usize]);
                let n = ((norm(sub(pb, pa)) / limit - 1e-9).ceil() as usize).max(1);
                let mut ids = Vec::with_capacity(n + 1);
                ids.push(a);
                for i in 1..n {
                    ids.push(self.weld.id(lerp(pa, pb, i as f64 / n as f64)));
                }
                ids.push(b);
                Chain { ids, keep }
            })
            .collect()
    }

    /// Whether a piece on `plane` has metal to both sides of it in that plane.
    fn metal_both_sides(&self, plane: usize, a: [f64; 2], b: [f64; 2]) -> bool {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let l = (dx * dx + dy * dy).sqrt().max(1e-300);
        let eps = (10.0 * self.tol).max(1e-6 * self.h);
        let mid = [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1])];
        let side = |s: f64| {
            let q = [mid[0] - s * eps * dy / l, mid[1] + s * eps * dx / l];
            self.faces.iter().any(|f| f.plane == plane && inside_polygon(&f.uv, q))
        };
        side(1.0) && side(-1.0)
    }

    fn triangulate_plane(&mut self, plane: usize, chains: &[Chain], out: &mut Vec<Triangle>) -> Result<()> {
        let faces: Vec<usize> = (0..self.faces.len()).filter(|f| self.faces[*f].plane == plane).collect();
        if faces.is_empty() {
            return Ok(());
        }
        let pl = &self.planes[plane];
        let reach = 2.0 * self.tol;

        // The atomic pieces lying on this plane's metal, as edges between vertex ids.
        let mut uv_of: HashMap<u32, [f64; 2]> = HashMap::new();
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for chain in chains {
            let ids = &chain.ids;
            let (a, b) = (self.weld.pts[ids[0] as usize], self.weld.pts[ids[ids.len() - 1] as usize]);
            if pl.height(a).abs() > reach || pl.height(b).abs() > reach || !self.metal_at(plane, pl.uv(lerp(a, b, 0.5))) {
                continue;
            }
            // The seam between two coplanar sheets is no constraint on the mesh.
            if !chain.keep && self.metal_both_sides(plane, pl.uv(a), pl.uv(b)) {
                continue;
            }
            for &v in ids {
                uv_of.entry(v).or_insert_with(|| pl.uv(self.weld.pts[v as usize]));
            }
            edges.extend(ids.windows(2).map(|w| (w[0], w[1])));
        }

        let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
        let mut handles: HashMap<u32, spade::handles::FixedVertexHandle> = HashMap::new();
        let mut ids: HashMap<usize, u32> = HashMap::new();
        let mut order: Vec<(u32, [f64; 2])> = uv_of.iter().map(|(k, v)| (*k, *v)).collect();
        order.sort_by_key(|x| x.0);
        for (id, q) in order {
            let h = cdt.insert(Point2::new(q[0], q[1])).map_err(|e| anyhow::anyhow!("meshing failed on a point: {e:?}"))?;
            handles.insert(id, h);
            ids.insert(h.index(), id);
        }
        for &(a, b) in &edges {
            let (ha, hb) = (handles[&a], handles[&b]);
            if ha != hb && !cdt.exists_constraint(ha, hb) && cdt.try_add_constraint(ha, hb).is_empty() {
                bail!("two outlines cross in a way that can't be meshed, near ({:.4}, {:.4}, {:.4}) m", self.weld.pts[a as usize][0], self.weld.pts[a as usize][1], self.weld.pts[a as usize][2]);
            }
        }

        // Interior points on a hexagonal lattice, kept clear of the constraints.
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for &f in &faces {
            for q in &self.faces[f].uv {
                for k in 0..2 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
            }
        }
        let spacing = LATTICE * self.h;
        let clear = 0.55 * spacing;
        let row = spacing * 3f64.sqrt() / 2.0;
        let edge_uv: Vec<([f64; 2], [f64; 2])> = edges.iter().map(|(a, b)| (uv_of[a], uv_of[b])).collect();
        let inside = |q: [f64; 2]| faces.iter().find(|f| inside_polygon(&self.faces[**f].uv, q)).copied();
        let rows = ((hi[1] - lo[1]) / row).floor() as usize;
        for r in 0..=rows {
            let y = lo[1] + 0.5 * (hi[1] - lo[1] - rows as f64 * row) + r as f64 * row;
            let shift = if r % 2 == 0 { 0.0 } else { 0.5 * spacing };
            let cols = ((hi[0] - lo[0]) / spacing).floor() as usize;
            for c in 0..=cols {
                let x = lo[0] + 0.5 * (hi[0] - lo[0] - cols as f64 * spacing) + c as f64 * spacing + shift;
                let q = [x, y];
                if inside(q).is_some() && !edge_uv.iter().any(|(a, b)| distance_to_segment_2d(q, *a, *b) < clear) {
                    cdt.insert(Point2::new(x, y)).map_err(|e| anyhow::anyhow!("meshing failed on a point: {e:?}"))?;
                }
            }
        }

        // Slivers around short features get Steiner points, with constraints kept
        // whole; whatever is still above the limit gets split at its longest edge.
        let limit = self.h * (1.0 + 1e-9);
        for _ in 0..64 {
            // Each pass may double the points, and some outlines keep asking for more: one ordinary
            // fractal reached 118 GB here. Past this many, no solve could use the mesh anyway.
            let extra_allowed = MAX_VERTICES.saturating_sub(cdt.num_vertices());
            if extra_allowed == 0 {
                bail!("This shape's mesh keeps refining without end, past {MAX_VERTICES} points. Traces close together or meeting at sharp angles usually do it: widen the gaps or simplify the outline.");
            }
            let _ = cdt.refine(RefinementParameters::<f64>::new().with_angle_limit(AngleLimit::from_deg(MIN_ANGLE)).keep_constraint_edges().with_min_required_area(1e-3 * self.h * self.h).with_max_additional_vertices(extra_allowed.min(cdt.num_vertices())));
            let mut extra: Vec<Point2<f64>> = Vec::new();
            for f in cdt.inner_faces() {
                let v = f.vertices().map(|x| x.position());
                let centre = [(v[0].x + v[1].x + v[2].x) / 3.0, (v[0].y + v[1].y + v[2].y) / 3.0];
                let (mut longest, mut mid) = (0.0, Point2::new(0.0, 0.0));
                for i in 0..3 {
                    let (a, b) = (v[i], v[(i + 1) % 3]);
                    let l = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                    if l > longest {
                        longest = l;
                        mid = Point2::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y));
                    }
                }
                if longest > limit && inside(centre).is_some() {
                    extra.push(mid);
                }
            }
            if extra.is_empty() {
                break;
            }
            for p in extra {
                cdt.insert(p).map_err(|e| anyhow::anyhow!("meshing failed on a point: {e:?}"))?;
            }
        }

        let mut kept: Vec<([usize; 3], Option<usize>)> = Vec::new();
        for f in cdt.inner_faces() {
            let vs = f.vertices();
            let v = vs.map(|x| x.position());
            let centre = [(v[0].x + v[1].x + v[2].x) / 3.0, (v[0].y + v[1].y + v[2].y) / 3.0];
            let Some(owner) = inside(centre) else { continue };
            let area2 = ((v[1].x - v[0].x) * (v[2].y - v[0].y) - (v[1].y - v[0].y) * (v[2].x - v[0].x)).abs();
            let longest = (0..3).map(|i| ((v[i].x - v[(i + 1) % 3].x).powi(2) + (v[i].y - v[(i + 1) % 3].y).powi(2)).sqrt()).fold(0.0, f64::max);
            // Rounding can leave zero-height triangles along a straight boundary.
            if area2 <= 1e-6 * self.h.min(longest) * longest {
                continue;
            }
            kept.push((vs.map(|x| x.fix().index()), self.faces[owner].solid));
        }
        let positions: Vec<[f64; 2]> = cdt.vertices().map(|x| [x.position().x, x.position().y]).collect();
        let at: Vec<P3> = positions.iter().map(|q| self.planes[plane].at(*q)).collect();
        for (tri, solid) in kept {
            let t = tri.map(|i| match ids.get(&i) {
                Some(id) => *id,
                None => self.weld.id(at[i]),
            });
            out.push((t, solid));
        }
        Ok(())
    }

    fn finish(self, geom: &Geometry, tris: Vec<Triangle>) -> Result<SurfaceMesh> {
        let tol = self.tol;
        let mut verts = self.weld.pts;
        if geom.ground {
            for p in &mut verts {
                if p[2].abs() <= tol {
                    p[2] = 0.0;
                }
            }
        }
        // Metal on the ground plane adds nothing to it, and the same triangle
        // reached twice would make the matrix singular.
        let mut seen: HashSet<[u32; 3]> = HashSet::new();
        let mut kept: Vec<Triangle> = Vec::with_capacity(tris.len());
        for (t, s) in tris {
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                continue;
            }
            if geom.ground && t.iter().all(|i| verts[*i as usize][2] == 0.0) {
                continue;
            }
            let mut key = t;
            key.sort_unstable();
            if seen.insert(key) {
                kept.push((t, s));
            }
        }
        if kept.is_empty() {
            bail!("the model has no surface left to mesh");
        }

        let mut map = vec![u32::MAX; verts.len()];
        let mut out_verts = Vec::new();
        let mut out_tris = Vec::with_capacity(kept.len());
        let mut solid = Vec::with_capacity(kept.len());
        for (t, s) in &kept {
            out_tris.push(t.map(|i| {
                if map[i as usize] == u32::MAX {
                    map[i as usize] = out_verts.len() as u32;
                    out_verts.push(verts[i as usize]);
                }
                map[i as usize]
            }));
            solid.push(*s);
        }

        let mut edge_set: HashSet<(u32, u32)> = HashSet::new();
        for t in &out_tris {
            for i in 0..3 {
                edge_set.insert((t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3])));
            }
        }
        let mut feeds = Vec::with_capacity(self.feeds.len());
        for (line, port) in self.feeds.iter().zip(&geom.ports) {
            let d = sub(line[1], line[0]);
            let l = norm(d);
            let mut along: Vec<(f64, u32)> = Vec::new();
            for (i, p) in out_verts.iter().enumerate() {
                let t = dot(sub(*p, line[0]), d) / (l * l);
                if (-1e-9..=1.0 + 1e-9).contains(&t) && norm(sub(*p, add(line[0], scale(d, t)))) <= 4.0 * tol {
                    along.push((t, i as u32));
                }
            }
            along.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut edges = Vec::new();
            for w in along.windows(2) {
                let e = (w[0].1.min(w[1].1), w[0].1.max(w[1].1));
                if !edge_set.contains(&e) {
                    bail!("the mesh doesn't follow the feed line of port '{}'", port.name);
                }
                edges.push([w[0].1, w[1].1]);
            }
            let covered = along.last().map_or(0.0, |x| x.0) - along.first().map_or(0.0, |x| x.0);
            if edges.is_empty() || (covered - 1.0).abs() > 1e-6 {
                bail!("the mesh doesn't reach across the feed line of port '{}'", port.name);
            }
            feeds.push(edges);
        }
        Ok(SurfaceMesh { verts: out_verts, tris: out_tris, solid, feeds, tol })
    }
}

/// Where two segments cross, away from their ends: `(t, u, point)`.
fn crossing(a: &[P3; 2], b: &[P3; 2], reach: f64) -> Option<(f64, f64, P3)> {
    let (d1, d2) = (sub(a[1], a[0]), sub(b[1], b[0]));
    let r = sub(a[0], b[0]);
    let (aa, bb, ab, ar, br) = (dot(d1, d1), dot(d2, d2), dot(d1, d2), dot(d1, r), dot(d2, r));
    let det = aa * bb - ab * ab;
    if det <= 1e-12 * aa * bb {
        return None;
    }
    let t = (ab * br - bb * ar) / det;
    let u = (aa * br - ab * ar) / det;
    let (la, lb) = (aa.sqrt(), bb.sqrt());
    if t * la <= reach || (1.0 - t) * la <= reach || u * lb <= reach || (1.0 - u) * lb <= reach {
        return None;
    }
    let (p, q) = (add(a[0], scale(d1, t)), add(b[0], scale(d2, u)));
    (norm(sub(p, q)) <= reach).then_some((t, u, lerp(p, q, 0.5)))
}

/// Distance from a point to a triangle.
fn distance_to_triangle(p: P3, t: [P3; 3]) -> f64 {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let n2 = dot(n, n);
    if n2 > 0.0 {
        let h = dot(sub(p, t[0]), n) / n2;
        let q = sub(p, scale(n, h));
        let side = |a: P3, b: P3| dot(cross(sub(b, a), sub(q, a)), n) >= 0.0;
        if side(t[0], t[1]) && side(t[1], t[2]) && side(t[2], t[0]) {
            return norm(sub(p, q));
        }
    }
    let mut best = f64::MAX;
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        let d = sub(b, a);
        let l2 = dot(d, d).max(1e-300);
        let u = (dot(sub(p, a), d) / l2).clamp(0.0, 1.0);
        best = best.min(norm(sub(p, add(a, scale(d, u)))));
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(name: &str, min: P3, max: P3) -> Solid {
        Solid { name: name.into(), material: "PEC".into(), shape: Shape::Box { min, max } }
    }

    fn port(a: P3, b: P3, width: f64) -> PortDef {
        PortDef { name: "feed".into(), a, b, width, z0: 50.0 }
    }

    fn area(m: &SurfaceMesh) -> f64 {
        m.tris.iter().map(|t| { let p = t.map(|i| m.verts[i as usize]); 0.5 * norm(cross(sub(p[1], p[0]), sub(p[2], p[0]))) }).sum()
    }

    fn assert_healthy(m: &SurfaceMesh, h: f64, name: &str) -> MeshQuality {
        let q = m.quality();
        println!("{name}: {} tris, min angle {:.1}°, max edge {:.4} (limit {h:.4}), min area {:.2e}, edges {} boundary / {} interior / {} junction", q.triangles, q.min_angle_deg, q.max_edge, q.min_area, q.boundary_edges, q.interior_edges, q.junction_edges);
        assert!(q.max_edge <= h * (1.0 + 1e-6), "{name}: edge {}", q.max_edge);
        assert!(q.min_area > 0.0, "{name}: degenerate triangle");
        q
    }

    #[test]
    fn t16_a_plain_plate_is_conformal() {
        let g = Geometry { solids: vec![sheet("plate", [-0.5, -0.4, 0.0], [0.5, 0.4, 0.0])], ..Default::default() };
        let h = 1.0 / 12.0;
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "T16 plate");
        assert!((area(&m) - 0.8).abs() < 1e-9, "{}", area(&m));
        assert_eq!(q.junction_edges, 0);
        // A plain plate's boundary edges all lie on its outline.
        let mut count: HashMap<(u32, u32), usize> = HashMap::new();
        for t in &m.tris {
            for i in 0..3 {
                *count.entry((t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3]))).or_default() += 1;
            }
        }
        for ((a, b), n) in &count {
            let (pa, pb) = (m.verts[*a as usize], m.verts[*b as usize]);
            let mid = lerp(pa, pb, 0.5);
            let on_outline = (mid[0].abs() - 0.5).abs() < 1e-9 || (mid[1].abs() - 0.4).abs() < 1e-9;
            assert_eq!(*n, if on_outline { 1 } else { 2 }, "edge {pa:?} {pb:?}");
        }
        assert!(q.min_angle_deg > 25.0, "{}", q.min_angle_deg);
    }

    #[test]
    fn t16_a_keyhole_outline_cuts_a_slot() {
        // The slot template's outline: outer rectangle, a zero-width bridge, the slot.
        let (x, y, sx, sy) = (0.5, 0.6, 0.0125, 0.24);
        let pts = vec![[-x, -y], [x, -y], [x, y], [-x, y], [-x, -y], [-sx, -sy], [-sx, sy], [sx, sy], [sx, -sy], [-sx, -sy]];
        let g = Geometry { solids: vec![Solid { name: "plate".into(), material: "PEC".into(), shape: Shape::Polygon { axis: 2, at: 0.0, pts, thick: 0.0 } }], ..Default::default() };
        let h = 1.0 / 12.0;
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "T16 keyhole");
        let want = 2.0 * x * 2.0 * y - 2.0 * sx * 2.0 * sy;
        assert!((area(&m) - want).abs() < 1e-9, "{} vs {want}", area(&m));
        assert_eq!(q.junction_edges, 0);
        // No triangle in the slot, and no slit along the bridge.
        for t in &m.tris {
            let p = t.map(|i| m.verts[i as usize]);
            let c = scale(add(add(p[0], p[1]), p[2]), 1.0 / 3.0);
            assert!(!(c[0].abs() < sx && c[1].abs() < sy), "triangle inside the slot at {c:?}");
        }
        let outline_len = 2.0 * (2.0 * x + 2.0 * y) + 2.0 * (2.0 * sx + 2.0 * sy);
        let mut boundary = 0.0;
        let mut count: HashMap<(u32, u32), usize> = HashMap::new();
        for t in &m.tris {
            for i in 0..3 {
                *count.entry((t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3]))).or_default() += 1;
            }
        }
        for ((a, b), n) in &count {
            if *n == 1 {
                boundary += norm(sub(m.verts[*a as usize], m.verts[*b as usize]));
            }
        }
        assert!((boundary - outline_len).abs() < 1e-9, "{boundary} vs {outline_len}");
    }

    #[test]
    fn a_concave_outline_works() {
        let pts = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.6, 1.0], [0.6, 0.3], [0.4, 0.3], [0.4, 1.0], [0.0, 1.0]];
        let g = Geometry { solids: vec![Solid { name: "u".into(), material: "PEC".into(), shape: Shape::Polygon { axis: 1, at: 0.2, pts, thick: 0.0 } }], ..Default::default() };
        let h = 0.1;
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        assert_healthy(&m, h, "concave");
        assert!((area(&m) - (1.0 - 0.2 * 0.7)).abs() < 1e-9, "{}", area(&m));
        assert!(m.verts.iter().all(|p| (p[1] - 0.2).abs() < 1e-9));
    }

    #[test]
    fn a_probe_imprints_both_plates() {
        let lambda = 1.0;
        let h = lambda / 12.0;
        let g = Geometry {
            solids: vec![sheet("ground", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), sheet("patch", [-0.24, -0.24, 0.03], [0.24, 0.24, 0.03])],
            ports: vec![port([0.0, 0.1, 0.0], [0.0, 0.1, 0.03], 0.0125)],
            ..Default::default()
        };
        let m = mesh_geometry(&g, h, lambda).unwrap();
        let q = assert_healthy(&m, h, "probe");
        // The strip's two end lines are junctions: plate, plate and strip.
        assert_eq!(q.junction_edges, 2, "{q:?}");
        assert_eq!(m.feeds[0].len(), 1);
        let want = 1.0 + 0.48 * 0.48 + 0.0125 * 0.03;
        assert!((area(&m) - want).abs() < 1e-9, "{}", area(&m));
    }

    #[test]
    fn a_sheet_ending_inside_a_plate_is_a_junction() {
        let h = 0.1;
        let g = Geometry { solids: vec![sheet("plate", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), sheet("wall", [0.11, -0.2, 0.0], [0.11, 0.2, 0.3])], ..Default::default() };
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "wall");
        let n = (0.4f64 / (FILL * h)).ceil() as usize;
        assert_eq!(q.junction_edges, n);
    }

    #[test]
    fn crossing_sheets_are_cut_along_their_intersection() {
        let h = 0.1;
        let g = Geometry { solids: vec![sheet("a", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), sheet("b", [-0.5, 0.0, -0.3], [0.5, 0.0, 0.3])], ..Default::default() };
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "cross");
        assert!(q.junction_edges > 0);
        assert!((area(&m) - (1.0 + 0.6)).abs() < 1e-9, "{}", area(&m));
    }

    #[test]
    fn overlapping_coplanar_sheets_merge() {
        let h = 0.1;
        let g = Geometry { solids: vec![sheet("a", [0.0, 0.0, 0.0], [1.0, 1.0, 0.0]), sheet("b", [0.5, 0.5, 0.0], [1.5, 1.5, 0.0])], ..Default::default() };
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        assert_healthy(&m, h, "overlap");
        assert!((area(&m) - 1.75).abs() < 1e-9, "{}", area(&m));
    }

    #[test]
    fn a_solid_box_is_closed() {
        let h = 0.1;
        let g = Geometry { solids: vec![sheet("box", [0.0, 0.0, 0.0], [0.5, 0.4, 0.3])], ..Default::default() };
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "box");
        assert_eq!(q.boundary_edges, 0);
        assert_eq!(q.junction_edges, 0);
        assert!((area(&m) - 2.0 * (0.2 + 0.15 + 0.12)).abs() < 1e-9);
    }

    #[test]
    fn a_closed_cylinder_and_sphere_have_no_open_edges() {
        let h = 0.1;
        let c = Geometry { solids: vec![Solid { name: "c".into(), material: "PEC".into(), shape: Shape::Cylinder { a: [0.0, 0.0, 0.0], b: [0.2, 0.1, 0.5], r: 0.15 } }], ..Default::default() };
        let m = mesh_geometry(&c, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "cylinder");
        assert_eq!(q.boundary_edges, 0, "{q:?}");
        assert_eq!(q.junction_edges, 0, "{q:?}");
        let s = Geometry { solids: vec![Solid { name: "s".into(), material: "PEC".into(), shape: Shape::Sphere { c: [0.1, 0.0, 0.2], r: 0.3 } }], ..Default::default() };
        let m = mesh_geometry(&s, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "sphere");
        assert_eq!(q.boundary_edges, 0, "{q:?}");
        assert_eq!(q.junction_edges, 0, "{q:?}");
        for p in &m.verts {
            assert!((norm(sub(*p, [0.1, 0.0, 0.2])) - 0.3).abs() < 1e-9);
        }
    }

    #[test]
    fn a_big_imported_model_is_refined_conformally() {
        // Two coarse triangles per cell of a grid: more than the flat-face limit.
        let n = 8;
        let cell = 0.25;
        let mut tris = Vec::new();
        for i in 0..n {
            for j in 0..n {
                let (x, y) = (i as f64 * cell, j as f64 * cell);
                tris.push([[x, y, 0.0], [x + cell, y, 0.0], [x + cell, y + cell, 0.0]]);
                tris.push([[x, y, 0.0], [x + cell, y + cell, 0.0], [x, y + cell, 0.0]]);
            }
        }
        let g = Geometry { solids: vec![Solid { name: "stl".into(), material: "PEC".into(), shape: Shape::Tris { tris, solid: false, sheet: 0.0 } }], ..Default::default() };
        let h = 0.1;
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "imported");
        assert_eq!(q.junction_edges, 0);
        assert!((area(&m) - 4.0).abs() < 1e-9, "{}", area(&m));
    }

    #[test]
    fn a_tilted_sheet_joins_a_box_sheet_edge() {
        // A horn-like flare: its short edge meets the end of a plate.
        let q = [[-0.2, 0.0, 0.5], [0.2, 0.0, 0.5], [0.5, 0.3, 1.5], [-0.5, 0.3, 1.5]];
        let g = Geometry {
            solids: vec![sheet("wall", [-0.2, 0.0, 0.0], [0.2, 0.0, 0.5]), Solid { name: "flare".into(), material: "PEC".into(), shape: Shape::Tris { tris: vec![[q[0], q[1], q[2]], [q[0], q[2], q[3]]], solid: false, sheet: 0.0 } }],
            ..Default::default()
        };
        let h = 0.1;
        let m = mesh_geometry(&g, h, 1.0).unwrap();
        let q = assert_healthy(&m, h, "flare");
        assert_eq!(q.junction_edges, 0, "{q:?}");
        // The shared edge has interior edges, not two open ones.
        let mut open_on_seam = 0;
        let mut count: HashMap<(u32, u32), usize> = HashMap::new();
        for t in &m.tris {
            for i in 0..3 {
                *count.entry((t[i].min(t[(i + 1) % 3]), t[i].max(t[(i + 1) % 3]))).or_default() += 1;
            }
        }
        for ((a, b), n) in &count {
            let (pa, pb) = (m.verts[*a as usize], m.verts[*b as usize]);
            if *n == 1 && (pa[2] - 0.5).abs() < 1e-9 && (pb[2] - 0.5).abs() < 1e-9 {
                open_on_seam += 1;
            }
        }
        assert_eq!(open_on_seam, 0);
    }

    #[test]
    fn a_port_touching_nothing_is_an_error() {
        let g = Geometry { solids: vec![sheet("plate", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0])], ports: vec![port([0.0, 0.0, 0.2], [0.0, 0.0, 0.3], 0.01)], ..Default::default() };
        let e = mesh_geometry(&g, 0.1, 1.0).unwrap_err();
        assert!(e.to_string().contains("doesn't touch"), "{e}");
    }

    #[test]
    fn the_width_axis_follows_the_sheet() {
        // Strip dipole in y = 0, port along z: the strip widens along x.
        let g = Geometry {
            solids: vec![sheet("up", [-0.01, 0.0, 0.01], [0.01, 0.0, 0.25]), sheet("down", [-0.01, 0.0, -0.25], [0.01, 0.0, -0.01])],
            ports: vec![port([0.0, 0.0, -0.01], [0.0, 0.0, 0.01], 0.02)],
            ..Default::default()
        };
        let m = mesh_geometry(&g, 0.05, 1.0).unwrap();
        assert_eq!(m.feeds[0].len(), 1);
        let e = m.feeds[0][0];
        let d = sub(m.verts[e[1] as usize], m.verts[e[0] as usize]);
        assert!((d[0].abs() - 0.02).abs() < 1e-9 && d[1].abs() < 1e-9 && d[2].abs() < 1e-9, "{d:?}");
        assert!(m.verts.iter().all(|p| p[1].abs() < 1e-12));
        // Vertical probe between horizontal plates: x.
        let g = Geometry {
            solids: vec![sheet("a", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), sheet("b", [-0.2, -0.2, 0.1], [0.2, 0.2, 0.1])],
            ports: vec![port([0.0, 0.0, 0.0], [0.0, 0.0, 0.1], 0.02)],
            ..Default::default()
        };
        let m = mesh_geometry(&g, 0.1, 1.0).unwrap();
        let e = m.feeds[0][0];
        let d = sub(m.verts[e[1] as usize], m.verts[e[0] as usize]);
        assert!((d[0].abs() - 0.02).abs() < 1e-9, "{d:?}");
    }

    #[test]
    fn a_port_end_on_a_thick_plate_lands_on_its_sheet() {
        // The plate is meshed at `at`, its thickness ignored, and the port ends on top.
        let patch = Solid { name: "patch".into(), material: "PEC".into(), shape: Shape::Polygon { axis: 2, at: 0.1, pts: vec![[-0.2, -0.2], [0.2, -0.2], [0.2, 0.2], [-0.2, 0.2]], thick: 0.002 } };
        let g = Geometry { solids: vec![sheet("ground", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), patch], ports: vec![port([0.0, 0.05, 0.0], [0.0, 0.05, 0.102], 0.02)], ..Default::default() };
        let m = mesh_geometry(&g, 0.1, 1.0).unwrap();
        assert!(m.verts.iter().all(|p| p[2] <= 0.1 + 1e-9), "the sheet stays at z = 0.1");
        assert_eq!(m.quality().junction_edges, 2);
    }

    #[test]
    fn a_thin_box_is_one_sheet() {
        let g = Geometry { solids: vec![sheet("trace", [0.0, 0.0, 0.0], [0.5, 0.1, 0.0001])], ..Default::default() };
        let m = mesh_geometry(&g, 0.05, 1.0).unwrap();
        assert!(m.verts.iter().all(|p| (p[2] - 0.00005).abs() < 1e-9));
        assert!((area(&m) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn a_very_narrow_gap_still_meshes() {
        for gap in [1e-3, 1e-4] {
            let w = 0.02;
            let g = Geometry {
                solids: vec![sheet("up", [-w / 2.0, 0.0, gap / 2.0], [w / 2.0, 0.0, 0.25]), sheet("down", [-w / 2.0, 0.0, -0.25], [w / 2.0, 0.0, -gap / 2.0])],
                ports: vec![port([0.0, 0.0, -gap / 2.0], [0.0, 0.0, gap / 2.0], w)],
                ..Default::default()
            };
            let m = mesh_geometry(&g, 0.05, 1.0).unwrap();
            assert_eq!(m.feeds[0].len(), 1);
            assert!(m.quality().min_area > 0.0);
        }
    }

    #[test]
    fn a_port_across_a_continuous_sheet_is_a_line_of_edges() {
        let g = Geometry { solids: vec![sheet("strip", [-0.01, 0.0, -0.25], [0.01, 0.0, 0.25])], ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 0.0)], ..Default::default() };
        let m = mesh_geometry(&g, 0.05, 1.0).unwrap();
        let total: f64 = m.feeds[0].iter().map(|e| norm(sub(m.verts[e[0] as usize], m.verts[e[1] as usize]))).sum();
        assert!((total - 0.02).abs() < 1e-9, "{total}");
        assert!((area(&m) - 0.02 * 0.5).abs() < 1e-9);
    }

    #[test]
    fn sheets_on_the_ground_plane_disappear_and_strips_stop_there() {
        let g = Geometry {
            solids: vec![sheet("ignored", [-0.5, -0.5, 0.0], [0.5, 0.5, 0.0]), sheet("patch", [-0.2, -0.2, 0.05], [0.2, 0.2, 0.05])],
            ports: vec![port([0.0, 0.0, 0.0], [0.0, 0.0, 0.05], 0.02)],
            ground: true,
            ..Default::default()
        };
        let m = mesh_geometry(&g, 0.1, 1.0).unwrap();
        assert!((area(&m) - (0.16 + 0.02 * 0.05)).abs() < 1e-9, "{}", area(&m));
        assert!(m.tris.iter().all(|t| t.iter().any(|i| m.verts[*i as usize][2] > 0.0)));
    }
}
