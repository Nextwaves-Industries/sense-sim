//! From a solved sketch to loops of points, and from loops to surfaces: extrude, revolve,
//! sweep, loft, and the plain solids. Each side of a shape takes its face name from the
//! curve that made it, so it's the same face however the dimensions change.

use std::collections::HashMap;
use std::f64::consts::TAU;

use anyhow::{Result, bail};
use spade::handles::FixedVertexHandle;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use super::mesh::{FaceId, Frame, Mesh, Xform, face_tag};
use super::sketch::{self, CurveKind, P2, Sketch, Solved};
use crate::expr::Vars;
use crate::geometry::{P3, add, cross, dot, norm, scale, sub, unit};

/// A face's number within its feature: 0 and 1 are the two ends, the rest follow the curves.
pub const END_LOW: u32 = 0;
pub const END_HIGH: u32 = 1;
fn side(curve: u32) -> u32 {
    curve + 2
}

/// A closed loop or open chain: points in order, and which curve each stretch came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Ring {
    pub pts: Vec<P2>,
    /// `tags[i]` is the curve from `pts[i]` to the next point.
    pub tags: Vec<u32>,
}

impl Ring {
    pub fn area(&self) -> f64 {
        signed_area(&self.pts)
    }
    fn reversed(&self) -> Ring {
        let n = self.pts.len();
        let mut pts = self.pts.clone();
        pts.reverse();
        // After reversing, the stretch leaving a point is the one that used to arrive at it.
        let tags = (0..n).map(|i| self.tags[(2 * n - 2 - i) % n]).collect();
        Ring { pts, tags }
    }
    pub fn counter_clockwise(&self) -> Ring {
        if self.area() >= 0.0 { self.clone() } else { self.reversed() }
    }
    pub fn clockwise(&self) -> Ring {
        if self.area() <= 0.0 { self.clone() } else { self.reversed() }
    }
}

pub fn signed_area(pts: &[P2]) -> f64 {
    let n = pts.len();
    0.5 * (0..n).map(|i| { let (a, b) = (pts[i], pts[(i + 1) % n]); a[0] * b[1] - b[0] * a[1] }).sum::<f64>()
}

fn inside(pts: &[P2], p: P2) -> bool {
    let mut hit = false;
    let n = pts.len();
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            hit = !hit;
        }
    }
    hit
}

/// An outer loop, counter-clockwise, and the holes in it, clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub outer: Ring,
    pub holes: Vec<Ring>,
}

impl Region {
    pub fn rings(&self) -> impl Iterator<Item = &Ring> {
        std::iter::once(&self.outer).chain(&self.holes)
    }
    pub fn area(&self) -> f64 {
        self.outer.area().abs() - self.holes.iter().map(|h| h.area().abs()).sum::<f64>()
    }
}

/// What a sketch's curves come to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profiles {
    pub regions: Vec<Region>,
    /// Chains that don't close: what a sheet, a wire or a sweep path is made from.
    pub open: Vec<Ring>,
    /// Why they can't be used as they are: three curves meeting at a point, or loops crossing.
    pub problem: Option<String>,
}

/// Points along a curve from one end to the other, ends included.
pub fn curve_polyline(kind: &CurveKind, s: &Solved, vars: &Vars, facets: usize, curve: u32) -> Vec<P2> {
    match kind {
        CurveKind::Line { a, b } => vec![s.at(*a), s.at(*b)],
        CurveKind::Circle { c, .. } => {
            let (c, r) = (s.at(*c), s.radii.get(&curve).copied().unwrap_or(0.0));
            (0..=facets).map(|k| { let t = TAU * k as f64 / facets as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect()
        }
        CurveKind::Arc { c, a, b } => {
            let (c, pa, pb) = (s.at(*c), s.at(*a), s.at(*b));
            let r = (pa[0] - c[0]).hypot(pa[1] - c[1]);
            let t0 = (pa[1] - c[1]).atan2(pa[0] - c[0]);
            let mut t1 = (pb[1] - c[1]).atan2(pb[0] - c[0]);
            if t1 <= t0 {
                t1 += TAU;
            }
            let n = (((t1 - t0) / TAU * facets as f64).ceil() as usize).max(2);
            let mut out: Vec<P2> = (0..=n).map(|k| { let t = t0 + (t1 - t0) * k as f64 / n as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect();
            // The ends are the points themselves, so what joins them joins exactly.
            out[0] = pa;
            out[n] = pb;
            out
        }
        CurveKind::Spline { pts } => spline(&pts.iter().map(|p| s.at(*p)).collect::<Vec<_>>(), facets),
        CurveKind::Equation { u, v, t0, t1, a, b } => {
            let mut out = sketch::equation_samples(u, v, t0, t1, facets.max(8), vars).unwrap_or_default();
            if let (Some(f), Some(l)) = (out.first_mut(), Some(s.at(*a))) {
                *f = l;
            }
            if let (Some(f), Some(l)) = (out.last_mut(), Some(s.at(*b))) {
                *f = l;
            }
            out
        }
    }
}

/// A Catmull-Rom curve through the points.
fn spline(p: &[P2], facets: usize) -> Vec<P2> {
    if p.len() < 3 {
        return p.to_vec();
    }
    let per = (facets / (p.len() - 1)).clamp(3, 16);
    let mut out = Vec::with_capacity((p.len() - 1) * per + 1);
    let at = |i: isize| p[i.clamp(0, p.len() as isize - 1) as usize];
    for i in 0..p.len() as isize - 1 {
        let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for k in 0..per {
            let t = k as f64 / per as f64;
            let (t2, t3) = (t * t, t * t * t);
            let f = |x: usize| 0.5 * (2.0 * b[x] + (c[x] - a[x]) * t + (2.0 * a[x] - 5.0 * b[x] + 4.0 * c[x] - d[x]) * t2 + (3.0 * b[x] - a[x] - 3.0 * c[x] + d[x]) * t3);
            out.push([f(0), f(1)]);
        }
    }
    out.push(*p.last().unwrap_or(&[0.0, 0.0]));
    out
}

/// Whether two stretches cross, not counting shared ends.
fn segments_cross(a: P2, b: P2, c: P2, d: P2) -> bool {
    let side = |p: P2, q: P2, r: P2| (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
    let (d1, d2, d3, d4) = (side(a, b, c), side(a, b, d), side(c, d, a), side(c, d, b));
    d1 * d2 < -1e-30 && d3 * d4 < -1e-30
}

/// Chains the sketch's curves into loops and open runs, then sorts the loops into regions by
/// what lies inside what.
pub fn profiles(sk: &Sketch, s: &Solved, vars: &Vars, facets: usize) -> Profiles {
    // Ends closer than this are joined, whether or not anyone said Coincident.
    const JOIN: f64 = 1e-9;
    let mut rings: Vec<Ring> = Vec::new();
    let mut edges: Vec<(u32, Vec<P2>)> = Vec::new();
    for c in sk.curves.iter().filter(|c| !c.construction) {
        let line = curve_polyline(&c.kind, s, vars, facets, c.id);
        if line.len() < 2 {
            continue;
        }
        if let CurveKind::Circle { .. } = c.kind {
            let mut pts = line;
            pts.pop();
            if pts.len() >= 3 && signed_area(&pts).abs() > 0.0 {
                rings.push(Ring { tags: vec![c.id; pts.len()], pts });
            }
        } else {
            edges.push((c.id, line));
        }
    }
    let mut nodes: Vec<P2> = Vec::new();
    let mut node_of = |q: P2| -> usize {
        match nodes.iter().position(|n| (n[0] - q[0]).abs() + (n[1] - q[1]).abs() < JOIN) {
            Some(i) => i,
            None => {
                nodes.push(q);
                nodes.len() - 1
            }
        }
    };
    let ends: Vec<(usize, usize)> = edges.iter().map(|(_, l)| (node_of(l[0]), node_of(*l.last().unwrap_or(&l[0])))).collect();
    let mut at_node: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, (a, b)) in ends.iter().enumerate() {
        at_node.entry(*a).or_default().push(i);
        if a != b {
            at_node.entry(*b).or_default().push(i);
        }
    }
    let mut out = Profiles::default();
    if at_node.values().any(|l| l.len() > 2) {
        out.problem = Some("Three or more curves meet at one point. A profile is made of loops, so trim them or make the extras construction lines.".into());
    }
    let mut used = vec![false; edges.len()];
    // Open runs first, from their loose ends; what's left closes.
    let mut starts: Vec<usize> = at_node.iter().filter(|(_, l)| l.len() == 1).map(|(n, _)| *n).collect();
    starts.sort_unstable();
    let walk = |from: usize, first_edge: usize, used: &mut Vec<bool>| -> (Vec<P2>, Vec<u32>, usize) {
        let (mut pts, mut tags) = (Vec::new(), Vec::new());
        let (mut node, mut edge) = (from, first_edge);
        loop {
            used[edge] = true;
            let (a, _) = ends[edge];
            let forward = a == node;
            let line = &edges[edge].1;
            let along: Vec<P2> = if forward { line.clone() } else { line.iter().rev().copied().collect() };
            for q in &along[..along.len() - 1] {
                pts.push(*q);
                tags.push(edges[edge].0);
            }
            node = if forward { ends[edge].1 } else { ends[edge].0 };
            let next = at_node.get(&node).and_then(|l| l.iter().copied().find(|e| !used[*e]));
            match next {
                Some(e) if node != from => edge = e,
                _ => return (pts, tags, node),
            }
        }
    };
    for start in starts {
        let Some(first) = at_node[&start].iter().copied().find(|e| !used[*e]) else { continue };
        let (mut pts, tags, end) = walk(start, first, &mut used);
        pts.push(nodes[end]);
        out.open.push(Ring { pts, tags });
    }
    for e in 0..edges.len() {
        if used[e] {
            continue;
        }
        let (pts, tags, end) = walk(ends[e].0, e, &mut used);
        if end == ends[e].0 && pts.len() >= 3 {
            rings.push(Ring { pts, tags });
        } else {
            let mut pts = pts;
            pts.push(nodes[end]);
            out.open.push(Ring { pts, tags });
        }
    }
    for i in 0..rings.len() {
        for j in i + 1..rings.len() {
            let (a, b) = (&rings[i].pts, &rings[j].pts);
            'pairs: for p in 0..a.len() {
                for q in 0..b.len() {
                    if segments_cross(a[p], a[(p + 1) % a.len()], b[q], b[(q + 1) % b.len()]) {
                        out.problem = Some("Two loops in this sketch cross each other. A profile can have loops inside loops, but they can't overlap.".into());
                        break 'pairs;
                    }
                }
            }
        }
    }
    // A loop inside an odd number of others is a hole in the smallest of them.
    let depth = |i: usize| rings.iter().enumerate().filter(|(j, r)| *j != i && r.area().abs() > rings[i].area().abs() && inside(&r.pts, rings[i].pts[0])).count();
    let mut order: Vec<usize> = (0..rings.len()).collect();
    order.sort_by(|a, b| rings[*b].area().abs().total_cmp(&rings[*a].area().abs()));
    for &i in &order {
        if depth(i) % 2 == 0 {
            out.regions.push(Region { outer: rings[i].counter_clockwise(), holes: Vec::new() });
        }
    }
    for &i in &order {
        if depth(i) % 2 == 1 {
            let host = out.regions.iter_mut().filter(|r| r.outer.area() > rings[i].area().abs() && inside(&r.outer.pts, rings[i].pts[0])).min_by(|a, b| a.outer.area().total_cmp(&b.outer.area()));
            if let Some(h) = host {
                h.holes.push(rings[i].clockwise());
            }
        }
    }
    out
}

/// Triangles filling a region, on its own points: `(points, triangles)`.
pub fn fill_region(rings: &[&[P2]]) -> (Vec<P2>, Vec<[usize; 3]>) {
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    for ring in rings {
        let handles: Vec<FixedVertexHandle> = ring.iter().filter_map(|p| cdt.insert(Point2::new(p[0], p[1])).ok()).collect();
        for k in 0..handles.len() {
            let (a, b) = (handles[k], handles[(k + 1) % handles.len()]);
            if a != b {
                cdt.add_constraint_and_split(a, b, |p| p);
            }
        }
    }
    let mut index: HashMap<FixedVertexHandle, usize> = HashMap::new();
    let mut pts = Vec::with_capacity(cdt.num_vertices());
    for v in cdt.vertices() {
        index.insert(v.fix(), pts.len());
        let q = v.position();
        pts.push([q.x, q.y]);
    }
    let mut tris = Vec::with_capacity(pts.len() * 2);
    for f in cdt.inner_faces() {
        let vs = f.vertices();
        let q = vs.map(|v| v.position());
        let c = [(q[0].x + q[1].x + q[2].x) / 3.0, (q[0].y + q[1].y + q[2].y) / 3.0];
        // Inside the outer loop and outside every hole: an odd number of loops around it.
        if rings.iter().filter(|r| inside(r, c)).count() % 2 == 1 {
            tris.push(vs.map(|v| index[&v.fix()]));
        }
    }
    (pts, tris)
}

fn add_cap(m: &mut Mesh, region: &Region, at: impl Fn(P2) -> P3, face: FaceId, flip: bool) {
    let rings: Vec<&[P2]> = region.rings().map(|r| r.pts.as_slice()).collect();
    let (pts, tris) = fill_region(&rings);
    for t in tris {
        let (a, b, c) = (at(pts[t[0]]), at(pts[t[1]]), at(pts[t[2]]));
        if flip { m.push_tri(a, c, b, face) } else { m.push_tri(a, b, c, face) }
    }
}

/// A region pushed along `n` from height `z0` to `z1`, as a closed solid.
pub fn extrude(region: &Region, f: &Frame, z0: f64, z1: f64, feature: u32) -> Mesh {
    let mut m = Mesh::default();
    add_cap(&mut m, region, |q| f.at(q, z0), face_tag(feature, END_LOW), true);
    add_cap(&mut m, region, |q| f.at(q, z1), face_tag(feature, END_HIGH), false);
    for ring in region.rings() {
        let n = ring.pts.len();
        for i in 0..n {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            m.push_quad(f.at(p, z0), f.at(q, z0), f.at(q, z1), f.at(p, z1), face_tag(feature, side(ring.tags[i])));
        }
    }
    m
}

/// An open or closed chain pushed along `n`: a sheet, such as a bent plate.
pub fn extrude_sheet(ring: &Ring, closed: bool, f: &Frame, z0: f64, z1: f64, feature: u32) -> Mesh {
    let mut m = Mesh::default();
    let n = ring.pts.len();
    let stretches = if closed { n } else { n - 1 };
    for i in 0..stretches {
        let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
        m.push_quad(f.at(p, z0), f.at(q, z0), f.at(q, z1), f.at(p, z1), face_tag(feature, side(ring.tags[i.min(ring.tags.len() - 1)])));
    }
    m
}

/// The flat surface a region fills, facing along the plane's normal.
pub fn sheet(region: &Region, f: &Frame, feature: u32) -> Mesh {
    let mut m = Mesh::default();
    add_cap(&mut m, region, |q| f.at(q, 0.0), face_tag(feature, END_LOW), false);
    m
}

/// A chain turned about a line in its plane. Closed when it was a loop, or when both its ends
/// are on the axis; otherwise a sheet, like a dish.
pub fn revolve(rings: &[(&Ring, bool)], f: &Frame, axis_at: P2, axis_dir: P2, angle: f64, facets: usize, feature: u32) -> Mesh {
    let full = angle >= TAU - 1e-9;
    let steps = ((angle / TAU * facets as f64).ceil() as usize).max(3);
    let o = f.at(axis_at, 0.0);
    let a = unit(f.dir(axis_dir));
    let turn = |k: usize| Xform::rotate(o, a, angle * k as f64 / steps as f64);
    let lift = |q: P2, k: usize| turn(k % if full { steps } else { steps + 1 }).apply(f.at(q, 0.0));
    let mut m = Mesh::default();
    for (ring, closed) in rings {
        let n = ring.pts.len();
        let stretches = if *closed { n } else { n - 1 };
        for i in 0..stretches {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            let face = face_tag(feature, side(ring.tags[i.min(ring.tags.len() - 1)]));
            for k in 0..steps {
                let (p0, q0, q1, p1) = (lift(p, k), lift(q, k), lift(q, k + 1), lift(p, k + 1));
                // Where a point is on the axis the quad comes down to a triangle.
                let tol = 1e-12;
                if norm(sub(p0, p1)) > tol {
                    m.push_tri(p0, p1, q0, face);
                }
                if norm(sub(q0, q1)) > tol {
                    m.push_tri(p1, q1, q0, face);
                }
            }
        }
    }
    m
}

/// The closed ends of a part-turn of a closed region.
pub fn revolve_caps(region: &Region, f: &Frame, axis_at: P2, axis_dir: P2, angle: f64, feature: u32, m: &mut Mesh) {
    let o = f.at(axis_at, 0.0);
    let a = unit(f.dir(axis_dir));
    let end = Xform::rotate(o, a, angle);
    add_cap(m, region, |q| f.at(q, 0.0), face_tag(feature, END_LOW), false);
    add_cap(m, region, |q| end.apply(f.at(q, 0.0)), face_tag(feature, END_HIGH), true);
}

/// Frames along a path that don't twist: each is the last turned as little as it takes.
pub fn path_frames(path: &[P3], closed: bool, start: Option<(P3, P3)>) -> Vec<(P3, P3, P3)> {
    let n = path.len();
    let tangent = |i: usize| -> P3 {
        if closed {
            unit(add(unit(sub(path[(i + 1) % n], path[i])), unit(sub(path[i], path[(i + n - 1) % n]))))
        } else if i == 0 {
            unit(sub(path[1], path[0]))
        } else if i == n - 1 {
            unit(sub(path[n - 1], path[n - 2]))
        } else {
            unit(add(unit(sub(path[i + 1], path[i])), unit(sub(path[i], path[i - 1]))))
        }
    };
    let t0 = tangent(0);
    let u0 = match start {
        Some((u, _)) => unit(sub(u, scale(t0, dot(u, t0)))),
        None => {
            let seed = if t0[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
            unit(cross(seed, t0))
        }
    };
    let mut frames = vec![(t0, u0, cross(t0, u0))];
    for i in 1..n {
        let (tp, up, _) = frames[i - 1];
        let t = tangent(i);
        let axis = cross(tp, t);
        let u = if norm(axis) < 1e-12 {
            up
        } else {
            let ang = dot(tp, t).clamp(-1.0, 1.0).acos();
            Xform::rotate([0.0; 3], axis, ang).apply_dir(up)
        };
        let u = unit(sub(u, scale(t, dot(u, t))));
        frames.push((t, u, cross(t, u)));
    }
    frames
}

/// How far a mitred corner stretches the cross-section, so a tube keeps its width round a bend.
fn miter(path: &[P3], closed: bool, i: usize, t: P3) -> f64 {
    let n = path.len();
    if !closed && (i == 0 || i == n - 1) {
        return 1.0;
    }
    let d = unit(sub(path[(i + 1) % n], path[i]));
    (1.0 / dot(d, t).abs().max(0.25)).min(4.0)
}

/// A cross-section carried along a path. `ring` is given in the plane's `(u, v)`, centred on
/// where the path begins. `closed_ring` and `closed_path` say which are loops, and open ends of
/// a closed section are capped.
pub fn sweep(region: Option<&Region>, ring: &Ring, closed_ring: bool, path: &[P3], closed_path: bool, profile_frame: &Frame, feature: u32) -> Mesh {
    let start = (profile_frame.u, profile_frame.v);
    let frames = path_frames(path, closed_path, Some(start));
    let n = path.len();
    // The profile relative to where the path starts, in the first frame's axes.
    let local = |q: P2| -> [f64; 2] {
        let p = profile_frame.at(q, 0.0);
        let d = sub(p, path[0]);
        [dot(d, frames[0].1), dot(d, frames[0].2)]
    };
    let place = |i: usize, l: [f64; 2]| -> P3 {
        let (t, u, v) = frames[i % n];
        let k = miter(path, closed_path, i % n, t);
        // Stretch along the corner's own direction only.
        let d = add(scale(u, l[0]), scale(v, l[1]));
        let along_bend = if k > 1.0 {
            let next = unit(sub(path[(i % n + 1) % n], path[i % n]));
            let bend = unit(sub(next, scale(t, dot(next, t))));
            let c = dot(d, bend);
            add(d, scale(bend, c * (k - 1.0)))
        } else {
            d
        };
        add(path[i % n], along_bend)
    };
    let mut m = Mesh::default();
    let m_pts = ring.pts.len();
    let stretches = if closed_ring { m_pts } else { m_pts - 1 };
    let rows = if closed_path { n } else { n - 1 };
    for i in 0..stretches {
        let (p, q) = (local(ring.pts[i]), local(ring.pts[(i + 1) % m_pts]));
        let face = face_tag(feature, side(ring.tags[i.min(ring.tags.len() - 1)]));
        for r in 0..rows {
            m.push_quad(place(r, p), place(r, q), place(r + 1, q), place(r + 1, p), face);
        }
    }
    if let Some(region) = region.filter(|_| !closed_path) {
        add_cap(&mut m, region, |q| place(0, local(q)), face_tag(feature, END_LOW), true);
        add_cap(&mut m, region, |q| place(n - 1, local(q)), face_tag(feature, END_HIGH), false);
    }
    m
}

/// A loop resampled to `n` points, keeping as many of its own corners as it can.
fn resampled(ring: &Ring, n: usize) -> Vec<P3Tag> {
    let m = ring.pts.len();
    if m == n {
        return (0..m).map(|i| (ring.pts[i], ring.tags[i])).collect();
    }
    let len = |i: usize| { let (a, b) = (ring.pts[i], ring.pts[(i + 1) % m]); (b[0] - a[0]).hypot(b[1] - a[1]) };
    let total: f64 = (0..m).map(len).sum();
    let mut out = Vec::with_capacity(n);
    let (mut i, mut walked) = (0, 0.0);
    for k in 0..n {
        let want = total * k as f64 / n as f64;
        while i + 1 < m && walked + len(i) < want {
            walked += len(i);
            i += 1;
        }
        let t = if len(i) > 0.0 { ((want - walked) / len(i)).clamp(0.0, 1.0) } else { 0.0 };
        let (a, b) = (ring.pts[i], ring.pts[(i + 1) % m]);
        out.push(([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], ring.tags[i]));
    }
    out
}

type P3Tag = (P2, u32);

/// Surfaces through closed sections in order, like a horn from its throat to its mouth. With
/// `ends` the two end sections are closed over, making a solid.
pub fn loft(sections: &[(Frame, Region)], ends: bool, feature: u32) -> Result<Mesh> {
    if sections.len() < 2 {
        bail!("A loft needs at least two sections.");
    }
    let n = sections.iter().map(|(_, r)| r.outer.pts.len()).max().unwrap_or(0);
    if n < 3 {
        bail!("A loft's sections have to be closed loops.");
    }
    // Sections with the same number of corners keep them, which keeps flat sides flat.
    let rows: Vec<Vec<(P3, u32)>> = sections
        .iter()
        .map(|(f, r)| {
            let pts = resampled(&r.outer, n);
            pts.iter().map(|(q, tag)| (f.at(*q, 0.0), *tag)).collect()
        })
        .collect();
    let mut m = Mesh::default();
    for w in rows.windows(2) {
        // Start each section at the corner nearest the last one's start, so it doesn't twist.
        let shift = (0..n).min_by(|x, y| {
            let cost = |s: usize| (0..n).map(|i| norm(sub(w[0][i].0, w[1][(i + s) % n].0))).sum::<f64>();
            cost(*x).total_cmp(&cost(*y))
        });
        let shift = shift.unwrap_or(0);
        for i in 0..n {
            let (a, b) = (w[0][i].0, w[0][(i + 1) % n].0);
            let (c, d) = (w[1][(i + shift) % n].0, w[1][(i + 1 + shift) % n].0);
            m.push_quad(a, b, d, c, face_tag(feature, side(w[0][i].1)));
        }
    }
    if ends {
        // Over the same points the walls use, resampled or not, or the ends wouldn't meet them.
        let cap = |r: &Region| {
            let pts: Vec<P3Tag> = resampled(&r.outer, n);
            Region { outer: Ring { pts: pts.iter().map(|p| p.0).collect(), tags: pts.iter().map(|p| p.1).collect() }, holes: Vec::new() }
        };
        let (f0, r0) = &sections[0];
        let (f1, r1) = &sections[sections.len() - 1];
        add_cap(&mut m, &cap(r0), |q| f0.at(q, 0.0), face_tag(feature, END_LOW), true);
        add_cap(&mut m, &cap(r1), |q| f1.at(q, 0.0), face_tag(feature, END_HIGH), false);
    }
    Ok(m)
}

/// A closed polygon's loop, for a primitive.
fn ring_of(pts: Vec<P2>, tag: u32) -> Ring {
    Ring { tags: vec![tag; pts.len()], pts }
}

fn circle_points(r: f64, facets: usize) -> Vec<P2> {
    (0..facets).map(|k| { let t = TAU * k as f64 / facets as f64; [r * t.cos(), r * t.sin()] }).collect()
}

/// A box between two corners.
pub fn cuboid(lo: P3, hi: P3, feature: u32) -> Mesh {
    let f = Frame { o: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] };
    let pts = vec![[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]];
    // Tags 0..3 make the side faces, which come after the two ends.
    let ring = Ring { pts, tags: vec![0, 1, 2, 3] };
    extrude(&Region { outer: ring, holes: Vec::new() }, &f, lo[2], hi[2], feature)
}

/// A cylinder or cone between two points, with its two radii.
pub fn cone(a: P3, b: P3, r0: f64, r1: f64, facets: usize, feature: u32) -> Mesh {
    let f = Frame::from_normal(a, sub(b, a));
    let h = norm(sub(b, a));
    let mut m = Mesh::default();
    let at = |r: f64, k: usize, z: f64| { let t = TAU * k as f64 / facets as f64; f.at([r * t.cos(), r * t.sin()], z) };
    for k in 0..facets {
        let wall = face_tag(feature, side(0));
        if r0 > 0.0 {
            m.push_tri(at(r0, k, 0.0), at(r0, k + 1, 0.0), at(r1, k + 1, h), wall);
        }
        if r1 > 0.0 {
            m.push_tri(at(r0, k, 0.0), at(r1, k + 1, h), at(r1, k, h), wall);
        }
    }
    if r0 > 0.0 {
        let region = Region { outer: ring_of(circle_points(r0, facets), 1), holes: Vec::new() };
        add_cap(&mut m, &region, |q| f.at(q, 0.0), face_tag(feature, END_LOW), true);
    }
    if r1 > 0.0 {
        let region = Region { outer: ring_of(circle_points(r1, facets), 2), holes: Vec::new() };
        add_cap(&mut m, &region, |q| f.at(q, h), face_tag(feature, END_HIGH), false);
    }
    m
}

pub fn sphere(c: P3, r: f64, facets: usize, feature: u32) -> Mesh {
    let (nt, np) = ((facets / 2).max(4), facets.max(8));
    let at = |i: usize, j: usize| {
        let (st, ct) = (std::f64::consts::PI * i as f64 / nt as f64).sin_cos();
        let (sp, cp) = (TAU * j as f64 / np as f64).sin_cos();
        add(c, [r * st * cp, r * st * sp, r * ct])
    };
    let face = face_tag(feature, side(0));
    let mut m = Mesh::default();
    for i in 0..nt {
        for j in 0..np {
            let (a, b, d, e) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            if i > 0 {
                m.push_tri(a, b, e, face);
            }
            if i + 1 < nt {
                m.push_tri(b, d, e, face);
            }
        }
    }
    m
}

/// A ring about the line through `c` along `axis`.
pub fn torus(c: P3, axis: P3, big: f64, small: f64, facets: usize, feature: u32) -> Mesh {
    let f = Frame::from_normal(c, axis);
    let (nb, ns) = (facets.max(8), (facets / 2).max(6));
    let at = |i: usize, j: usize| {
        let (sb, cb) = (TAU * i as f64 / nb as f64).sin_cos();
        let (ss, cs) = (TAU * j as f64 / ns as f64).sin_cos();
        f.at([(big + small * cs) * cb, (big + small * cs) * sb], small * ss)
    };
    let face = face_tag(feature, side(0));
    let mut m = Mesh::default();
    for i in 0..nb {
        for j in 0..ns {
            m.push_quad(at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1), face);
        }
    }
    m
}

/// Points along a helix, turned `pitch` per turn, starting at `+u` from `o`. A taper makes it
/// conical: the radius at the far end.
pub fn helix_points(o: P3, axis: P3, r0: f64, r1: f64, pitch: f64, turns: f64, left: bool, facets: usize) -> Vec<P3> {
    let f = Frame::from_normal(o, axis);
    let steps = ((turns.abs() * facets as f64).ceil() as usize).max(4);
    let sign = if left { -1.0 } else { 1.0 };
    (0..=steps)
        .map(|k| {
            let t = turns * k as f64 / steps as f64;
            let r = r0 + (r1 - r0) * k as f64 / steps as f64;
            let a = sign * TAU * t;
            f.at([r * a.cos(), r * a.sin()], pitch * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::boolean::{Op, boolean};
    use crate::modeler::sketch::Sketch;

    fn top() -> Frame {
        Frame { o: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] }
    }

    fn closed(m: &Mesh) -> Mesh {
        let w = m.welded(1e-10);
        assert!(w.is_closed(), "{} open edges", w.open_edges());
        w
    }

    fn profile(s: &Sketch) -> Profiles {
        let v = Vars::new();
        profiles(s, &s.solve(&v), &v, 48)
    }

    #[test]
    fn a_plate_with_a_hole_extrudes_to_its_volume() {
        let mut s = Sketch::default();
        s.add_rect([-0.02, -0.01], [0.02, 0.01]);
        s.add_circle([0.0, 0.0], 0.005);
        let p = profile(&s);
        assert_eq!((p.regions.len(), p.regions[0].holes.len()), (1, 1));
        let m = closed(&extrude(&p.regions[0], &top(), 0.0, 0.003, 7));
        let exact = (0.04 * 0.02 - std::f64::consts::PI * 2.5e-5) * 0.003;
        assert!((m.volume() - exact).abs() / exact < 2e-3, "{} vs {exact}", m.volume());
        // Faces are named after what made them: both ends, four sides, and the hole's wall.
        assert_eq!(m.face_ids().len(), 7, "{:?}", m.face_ids());
    }

    #[test]
    fn loops_inside_loops_are_holes_and_islands() {
        let mut s = Sketch::default();
        s.add_rect([-0.04, -0.04], [0.04, 0.04]);
        s.add_rect([-0.03, -0.03], [0.03, 0.03]);
        s.add_rect([-0.01, -0.01], [0.01, 0.01]);
        s.add_rect([0.1, 0.1], [0.11, 0.11]);
        let p = profile(&s);
        assert_eq!(p.regions.len(), 3, "the frame, the island in it, and the one off to the side");
        assert_eq!(p.regions.iter().map(|r| r.holes.len()).sum::<usize>(), 1);
        assert!(p.problem.is_none());
    }

    #[test]
    fn chains_join_in_any_order_and_problems_are_named() {
        let v = Vars::new();
        // A triangle drawn out of order and with ends only coincident by position.
        let mut s = Sketch::default();
        let (a, b, c, d, e, f) = (s.add_point([0.0, 0.0]), s.add_point([0.01, 0.0]), s.add_point([0.01, 0.0]), s.add_point([0.0, 0.01]), s.add_point([0.0, 0.01]), s.add_point([0.0, 0.0]));
        s.add_line(c, d);
        s.add_line(a, b);
        s.add_line(e, f);
        let p = profile(&s);
        assert_eq!(p.regions.len(), 1);
        assert!((p.regions[0].area() - 5e-5).abs() < 1e-12);
        // An open run stays open.
        let mut s = Sketch::default();
        let (a, b, c) = (s.add_point([0.0, 0.0]), s.add_point([0.01, 0.0]), s.add_point([0.01, 0.01]));
        s.add_line(a, b);
        s.add_line(b, c);
        let p = profile(&s);
        assert!(p.regions.is_empty());
        assert_eq!(p.open[0].pts.len(), 3);
        assert_eq!(p.open[0].tags.len(), 2);
        // A T junction can't be a profile.
        let d = s.add_point([0.01, -0.01]);
        s.add_line(b, d);
        assert!(profiles(&s, &s.solve(&v), &v, 48).problem.is_some());
        // Nor can overlapping loops.
        let mut s = Sketch::default();
        s.add_rect([0.0, 0.0], [0.02, 0.02]);
        s.add_rect([0.01, 0.01], [0.03, 0.03]);
        assert!(profile(&s).problem.unwrap().contains("cross"));
    }

    #[test]
    fn construction_lines_make_no_geometry() {
        let mut s = Sketch::default();
        let (a, b) = (s.add_point([0.0, 0.0]), s.add_point([0.01, 0.0]));
        let l = s.add_line(a, b);
        s.curves.iter_mut().find(|c| c.id == l).unwrap().construction = true;
        let p = profile(&s);
        assert!(p.regions.is_empty() && p.open.is_empty());
    }

    #[test]
    fn revolving_a_half_disc_makes_a_sphere() {
        let mut s = Sketch::default();
        let arc = s.add_arc([0.0, 0.0], [0.0, -0.01], [0.0, 0.01]);
        let CurveKind::Arc { a, b, .. } = s.curve(arc).unwrap().kind else { panic!() };
        s.add_line(b, a);
        let p = profile(&s);
        let r = &p.regions[0];
        let list = [(&r.outer, true)];
        let m = closed(&revolve(&list, &top(), [0.0, 0.0], [0.0, 1.0], TAU, 48, 3));
        let exact = 4.0 / 3.0 * std::f64::consts::PI * 1e-6;
        assert!((m.volume().abs() - exact).abs() / exact < 0.01, "{} vs {exact}", m.volume());
    }

    #[test]
    fn a_full_turn_of_a_loop_off_the_axis_is_a_ring() {
        let mut s = Sketch::default();
        s.add_rect([0.02, -0.005], [0.03, 0.005]);
        let p = profile(&s);
        let r = &p.regions[0];
        let list = [(&r.outer, true)];
        let m = closed(&revolve(&list, &top(), [0.0, 0.0], [0.0, 1.0], TAU, 64, 3));
        let exact = std::f64::consts::PI * (0.03f64.powi(2) - 0.02f64.powi(2)) * 0.01;
        assert!((m.volume().abs() - exact).abs() / exact < 0.01, "{} vs {exact}", m.volume());
    }

    #[test]
    fn part_of_a_turn_gets_its_two_ends() {
        let mut s = Sketch::default();
        s.add_rect([0.02, -0.005], [0.03, 0.005]);
        let p = profile(&s);
        let r = &p.regions[0];
        let list = [(&r.outer, true)];
        let mut m = revolve(&list, &top(), [0.0, 0.0], [0.0, 1.0], TAU / 4.0, 64, 3);
        revolve_caps(r, &top(), [0.0, 0.0], [0.0, 1.0], TAU / 4.0, 3, &mut m);
        let m = closed(&m);
        let exact = std::f64::consts::PI * (0.03f64.powi(2) - 0.02f64.powi(2)) * 0.01 / 4.0;
        assert!((m.volume().abs() - exact).abs() / exact < 0.01, "{} vs {exact}", m.volume());
    }

    #[test]
    fn an_open_chain_turned_is_a_sheet_like_a_dish() {
        let mut s = Sketch::default();
        let (a, b, c) = (s.add_point([0.0, 0.0]), s.add_point([0.02, 0.005]), s.add_point([0.04, 0.02]));
        s.add_line(a, b);
        s.add_line(b, c);
        let p = profile(&s);
        let list = [(&p.open[0], false)];
        let m = revolve(&list, &top(), [0.0, 0.0], [0.0, 1.0], TAU, 32, 3).welded(1e-10);
        assert!(m.open_edges() > 0, "it has a rim");
        assert!(m.area() > std::f64::consts::PI * 0.04 * 0.04 * 0.9);
    }

    #[test]
    fn a_tube_round_a_bend_is_closed_and_keeps_its_size() {
        let mut s = Sketch::default();
        s.add_circle([0.0, 0.0], 0.001);
        let p = profile(&s);
        let r = &p.regions[0];
        let front = Frame { o: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] };
        let path = [[0.0, 0.0, 0.0], [0.0, 0.0, 0.02], [0.02, 0.0, 0.02]];
        let m = closed(&sweep(Some(r), &r.outer, true, &path, false, &front, 4));
        let exact = std::f64::consts::PI * 1e-6 * 0.04;
        assert!(m.volume() > 0.0);
        assert!((m.volume() - exact).abs() / exact < 0.05, "{} vs {exact}", m.volume());
    }

    #[test]
    fn a_strip_along_a_helix_is_a_sheet_along_it() {
        let path = helix_points([0.0; 3], [0.0, 0.0, 1.0], 0.02, 0.02, 0.01, 3.0, false, 32);
        assert_eq!(path.len(), 97);
        assert!((path[96][2] - 0.03).abs() < 1e-12);
        let ring = Ring { pts: vec![[0.0, -0.001], [0.0, 0.001]], tags: vec![0] };
        let f = Frame::from_normal(path[0], sub(path[1], path[0]));
        let f = Frame { o: path[0], ..f };
        let ring2 = Ring { pts: ring.pts.iter().map(|q| { let d = f.at(*q, 0.0); let l = f.local(d).0; l }).collect(), tags: vec![0] };
        let m = sweep(None, &ring2, false, &path, false, &f, 5).welded(1e-10);
        assert!(m.open_edges() > 0);
        let length: f64 = path.windows(2).map(|w| norm(sub(w[1], w[0]))).sum();
        assert!((m.area() - length * 0.002).abs() / m.area() < 0.03, "{} vs {}", m.area(), length * 0.002);
    }

    #[test]
    fn a_loft_of_two_rectangles_is_a_frustum() {
        let mut a = Sketch::default();
        a.add_rect([-0.01, -0.01], [0.01, 0.01]);
        let mut b = Sketch::default();
        b.add_rect([-0.02, -0.02], [0.02, 0.02]);
        let f0 = top();
        let f1 = Frame { o: [0.0, 0.0, 0.03], ..top() };
        let (pa, pb) = (profile(&a), profile(&b));
        let m = closed(&loft(&[(f0, pa.regions[0].clone()), (f1, pb.regions[0].clone())], true, 2).unwrap());
        // (A1 + A2 + sqrt(A1 A2)) h / 3.
        let exact = (4e-4 + 16e-4 + 8e-4) * 0.03 / 3.0;
        assert!((m.volume().abs() - exact).abs() / exact < 1e-6, "{} vs {exact}", m.volume());
        // Open-ended, it's the horn's four walls.
        let horn = loft(&[(f0, pa.regions[0].clone()), (f1, pb.regions[0].clone())], false, 2).unwrap().welded(1e-10);
        assert!(!horn.is_closed());
        assert_eq!(horn.face_ids().len(), 4, "one wall per side");
        assert!(loft(&[(f0, pa.regions[0].clone())], true, 2).is_err());
    }

    #[test]
    fn plain_solids_have_the_right_volumes_and_join_cleanly() {
        let box_ = closed(&cuboid([0.0; 3], [0.02, 0.03, 0.04], 1));
        assert!((box_.volume() - 2.4e-5).abs() < 1e-12);
        assert_eq!(box_.face_ids().len(), 6);
        let pin = closed(&cone([0.01, 0.015, -0.01], [0.01, 0.015, 0.05], 0.002, 0.002, 48, 2));
        assert!((pin.volume() - std::f64::consts::PI * 4e-6 * 0.06).abs() / pin.volume() < 0.01);
        let drill = boolean(&box_, &pin, Op::Difference).unwrap();
        assert!(drill.is_closed());
        let tip = closed(&cone([0.0; 3], [0.0, 0.0, 0.03], 0.01, 0.0, 48, 3));
        assert!((tip.volume() - std::f64::consts::PI * 1e-4 * 0.03 / 3.0).abs() / tip.volume() < 0.01);
        let ball = closed(&sphere([0.0; 3], 0.01, 48, 4));
        assert!((ball.volume() - 4.0 / 3.0 * std::f64::consts::PI * 1e-6).abs() / ball.volume() < 0.02);
        let ring = closed(&torus([0.0; 3], [0.0, 0.0, 1.0], 0.02, 0.005, 48, 5));
        let exact = 2.0 * std::f64::consts::PI.powi(2) * 0.02 * 0.005f64.powi(2);
        assert!((ring.volume() - exact).abs() / exact < 0.02, "{} vs {exact}", ring.volume());
    }

    #[test]
    fn a_rings_reversal_keeps_each_stretch_on_its_curve() {
        let r = Ring { pts: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]], tags: vec![10, 20, 30] };
        assert!(r.area() > 0.0);
        let back = r.clockwise();
        assert!(back.area() < 0.0);
        // From (1,1) to (1,0) is the stretch tagged 20, and so on round.
        assert_eq!(back.pts, vec![[1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        assert_eq!(back.tags, vec![20, 10, 30]);
    }
}
