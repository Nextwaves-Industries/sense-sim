//! A body's surface: welded triangles, each belonging to a named face. Faces keep their
//! names through booleans, patterns and moves, which is what lets a later feature say
//! "this face" or "the edge between these two" and still find it after a variable changes.

use std::collections::{HashMap, HashSet};

use crate::geometry::{P3, add, cross, dot, norm, scale, sub, unit};

/// A face's name: the feature that made it, and which face of that feature.
pub type FaceId = u32;

/// How many faces one feature can name. A side face per sketch curve, so it's plenty.
const FACES_PER_FEATURE: u32 = 1 << 12;

pub fn face_tag(feature: u32, local: u32) -> FaceId {
    feature.wrapping_mul(FACES_PER_FEATURE) | local.min(FACES_PER_FEATURE - 1)
}

/// The feature a face came from.
pub fn feature_of(face: FaceId) -> u32 {
    face / FACES_PER_FEATURE
}

/// Which face of that feature it is.
pub fn local_of(face: FaceId) -> u32 {
    face % FACES_PER_FEATURE
}

/// A plane with its own axes: `n = u × v`, right-handed, all unit length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub o: P3,
    pub u: P3,
    pub v: P3,
    pub n: P3,
}

impl Frame {
    /// A point `h` off the plane, at `q` in it.
    pub fn at(&self, q: [f64; 2], h: f64) -> P3 {
        add(add(add(self.o, scale(self.u, q[0])), scale(self.v, q[1])), scale(self.n, h))
    }

    pub fn dir(&self, q: [f64; 2]) -> P3 {
        add(scale(self.u, q[0]), scale(self.v, q[1]))
    }

    /// Where a point is on the plane, and how far off it.
    pub fn local(&self, p: P3) -> ([f64; 2], f64) {
        let d = sub(p, self.o);
        ([dot(d, self.u), dot(d, self.v)], dot(d, self.n))
    }

    /// Axes for a plane given only by its normal. The same normal always gets the same
    /// axes, so a sketch on a face stays where it was when the face moves along itself.
    pub fn from_normal(o: P3, n: P3) -> Self {
        let n = unit(n);
        let u = if n[2].abs() >= 0.9 {
            unit(sub([1.0, 0.0, 0.0], scale(n, n[0])))
        } else {
            unit(cross([0.0, 0.0, 1.0], n))
        };
        Self { o, u, v: cross(n, u), n }
    }
}

/// A rigid move, or a reflection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform {
    pub m: [[f64; 3]; 3],
    pub t: P3,
}

impl Xform {
    pub const IDENTITY: Self = Self { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    pub fn translate(d: P3) -> Self {
        Self { t: d, ..Self::IDENTITY }
    }

    /// A turn of `angle` radians about the line through `p` along `axis`, right-handed.
    pub fn rotate(p: P3, axis: P3, angle: f64) -> Self {
        let a = unit(axis);
        let (s, c) = angle.sin_cos();
        let k = 1.0 - c;
        let m = [
            [c + a[0] * a[0] * k, a[0] * a[1] * k - a[2] * s, a[0] * a[2] * k + a[1] * s],
            [a[1] * a[0] * k + a[2] * s, c + a[1] * a[1] * k, a[1] * a[2] * k - a[0] * s],
            [a[2] * a[0] * k - a[1] * s, a[2] * a[1] * k + a[0] * s, c + a[2] * a[2] * k],
        ];
        let turned = Self { m, t: [0.0; 3] }.apply(p);
        Self { m, t: sub(p, turned) }
    }

    /// A reflection in the plane through `p` with normal `n`.
    pub fn mirror(p: P3, n: P3) -> Self {
        let n = unit(n);
        let m = std::array::from_fn(|i| std::array::from_fn(|j| (if i == j { 1.0 } else { 0.0 }) - 2.0 * n[i] * n[j]));
        let moved = Self { m, t: [0.0; 3] }.apply(p);
        Self { m, t: sub(p, moved) }
    }

    /// This one done after `first`.
    pub fn after(&self, first: &Xform) -> Xform {
        let m = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| self.m[i][k] * first.m[k][j]).sum()));
        Xform { m, t: self.apply(first.t) }
    }

    pub fn apply(&self, p: P3) -> P3 {
        std::array::from_fn(|i| self.m[i][0] * p[0] + self.m[i][1] * p[1] + self.m[i][2] * p[2] + self.t[i])
    }

    /// A direction, which only turns.
    pub fn apply_dir(&self, d: P3) -> P3 {
        std::array::from_fn(|i| self.m[i][0] * d[0] + self.m[i][1] * d[1] + self.m[i][2] * d[2])
    }

    pub fn flips(&self) -> bool {
        let m = &self.m;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        det < 0.0
    }
}

/// Points closer than a tolerance are one point. Looks in the neighbouring cells too, so two
/// points either side of a cell wall still merge.
pub struct Pool {
    grid: HashMap<(i64, i64, i64), Vec<u32>>,
    pub pts: Vec<P3>,
    tol: f64,
}

impl Pool {
    pub fn new(tol: f64) -> Self {
        Self { grid: HashMap::new(), pts: Vec::new(), tol }
    }

    fn cell(&self, p: P3) -> (i64, i64, i64) {
        let c = |x: f64| (x / (2.0 * self.tol)).floor() as i64;
        (c(p[0]), c(p[1]), c(p[2]))
    }

    pub fn id(&mut self, p: P3) -> u32 {
        let (cx, cy, cz) = self.cell(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(list) = self.grid.get(&(cx + dx, cy + dy, cz + dz)) {
                        for &i in list {
                            if norm(sub(self.pts[i as usize], p)) <= self.tol {
                                return i;
                            }
                        }
                    }
                }
            }
        }
        let i = self.pts.len() as u32;
        self.pts.push(p);
        self.grid.entry((cx, cy, cz)).or_default().push(i);
        i
    }
}

/// Triangles over shared vertices, each with the face it belongs to. A closed solid's
/// triangles wind counter-clockwise seen from outside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub verts: Vec<P3>,
    pub tris: Vec<[u32; 3]>,
    pub face: Vec<FaceId>,
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// A triangle by its corners, sharing nothing until [`Self::welded`].
    pub fn push_tri(&mut self, a: P3, b: P3, c: P3, face: FaceId) {
        let n = self.verts.len() as u32;
        self.verts.extend([a, b, c]);
        self.tris.push([n, n + 1, n + 2]);
        self.face.push(face);
    }

    pub fn push_quad(&mut self, a: P3, b: P3, c: P3, d: P3, face: FaceId) {
        self.push_tri(a, b, c, face);
        self.push_tri(a, c, d, face);
    }

    pub fn append(&mut self, other: &Mesh) {
        let n = self.verts.len() as u32;
        self.verts.extend_from_slice(&other.verts);
        self.tris.extend(other.tris.iter().map(|t| t.map(|v| v + n)));
        self.face.extend_from_slice(&other.face);
    }

    pub fn corners(&self, t: usize) -> [P3; 3] {
        self.tris[t].map(|v| self.verts[v as usize])
    }

    /// One vertex where corners coincide, with what that collapses dropped.
    pub fn welded(&self, tol: f64) -> Mesh {
        let mut pool = Pool::new(tol);
        let mut out = Mesh::default();
        for (i, t) in self.tris.iter().enumerate() {
            let v = t.map(|k| pool.id(self.verts[k as usize]));
            if v[0] != v[1] && v[1] != v[2] && v[0] != v[2] {
                out.tris.push(v);
                out.face.push(self.face[i]);
            }
        }
        out.verts = pool.pts;
        out.compacted()
    }

    /// Without vertices nothing uses.
    pub fn compacted(&self) -> Mesh {
        let mut map = vec![u32::MAX; self.verts.len()];
        let mut out = Mesh { verts: Vec::new(), tris: Vec::with_capacity(self.tris.len()), face: self.face.clone() };
        for t in &self.tris {
            out.tris.push(t.map(|v| {
                let slot = &mut map[v as usize];
                if *slot == u32::MAX {
                    *slot = out.verts.len() as u32;
                    out.verts.push(self.verts[v as usize]);
                }
                *slot
            }));
        }
        out
    }

    pub fn transformed(&self, x: &Xform) -> Mesh {
        let mut out = self.clone();
        for p in &mut out.verts {
            *p = x.apply(*p);
        }
        if x.flips() {
            for t in &mut out.tris {
                t.swap(1, 2);
            }
        }
        out
    }

    /// The same surface facing the other way.
    pub fn flipped(&self) -> Mesh {
        let mut out = self.clone();
        for t in &mut out.tris {
            t.swap(1, 2);
        }
        out
    }

    /// Edges without a partner running the other way: 0 for a closed surface. Where four
    /// triangles meet on an edge, two pairs of them still balance.
    pub fn open_edges(&self) -> usize {
        let mut count: HashMap<(u32, u32), i32> = HashMap::with_capacity(self.tris.len() * 3 / 2);
        for t in &self.tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let (key, sign) = if a < b { ((a, b), 1) } else { ((b, a), -1) };
                *count.entry(key).or_default() += sign;
            }
        }
        count.values().map(|n| n.unsigned_abs() as usize).sum()
    }

    pub fn is_closed(&self) -> bool {
        !self.tris.is_empty() && self.open_edges() == 0
    }

    /// Edges more than two triangles share. They meet at a line, which a solver may refuse.
    pub fn pinched_edges(&self) -> usize {
        let mut count: HashMap<(u32, u32), u32> = HashMap::with_capacity(self.tris.len() * 3 / 2);
        for t in &self.tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *count.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        count.values().filter(|n| **n > 2).count()
    }

    pub fn area(&self) -> f64 {
        (0..self.tris.len())
            .map(|t| {
                let c = self.corners(t);
                0.5 * norm(cross(sub(c[1], c[0]), sub(c[2], c[0])))
            })
            .sum()
    }

    /// Enclosed volume, positive when the surface faces outward. Meaningful only when closed.
    pub fn volume(&self) -> f64 {
        (0..self.tris.len())
            .map(|t| {
                let c = self.corners(t);
                dot(c[0], cross(c[1], c[2])) / 6.0
            })
            .sum()
    }

    pub fn bounds(&self) -> (P3, P3) {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for t in &self.tris {
            for &v in t {
                let p = self.verts[v as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        (lo, hi)
    }

    /// A triangle's unit normal, or `None` if it has no area.
    pub fn normal(&self, t: usize) -> Option<P3> {
        let c = self.corners(t);
        let n = cross(sub(c[1], c[0]), sub(c[2], c[0]));
        (norm(n) > 0.0).then(|| unit(n))
    }

    /// The plane of a face, if its triangles all lie on one. The normal comes from the
    /// biggest triangle, which is the least upset by rounding.
    pub fn plane_of(&self, face: FaceId, tol: f64) -> Option<(P3, P3)> {
        let list: Vec<usize> = (0..self.tris.len()).filter(|t| self.face[*t] == face).collect();
        let best = list.iter().copied().max_by(|a, b| {
            let area = |t: usize| {
                let c = self.corners(t);
                norm(cross(sub(c[1], c[0]), sub(c[2], c[0])))
            };
            area(*a).total_cmp(&area(*b))
        })?;
        let n = self.normal(best)?;
        let o = self.corners(best)[0];
        for t in &list {
            for p in self.corners(*t) {
                if dot(sub(p, o), n).abs() > tol {
                    return None;
                }
            }
        }
        Some((o, n))
    }

    /// Whether any triangle of a face has this name.
    pub fn has_face(&self, face: FaceId) -> bool {
        self.face.contains(&face)
    }

    /// Faces in the order they're first met.
    pub fn face_ids(&self) -> Vec<FaceId> {
        let mut seen = HashSet::new();
        self.face.iter().copied().filter(|f| seen.insert(*f)).collect()
    }

    /// The lines worth drawing: an open edge, an edge between two named faces, or a crease.
    /// Inside one curved face only a real crease counts, so a cylinder shows its rims and not
    /// its facets.
    pub fn feature_edges(&self, crease_deg: f64) -> Vec<[u32; 2]> {
        let cos = crease_deg.to_radians().cos();
        let normals: Vec<P3> = (0..self.tris.len()).map(|t| self.normal(t).unwrap_or([0.0; 3])).collect();
        let mut map: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(self.tris.len() * 3 / 2);
        for (i, t) in self.tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                map.entry((a.min(b), a.max(b))).or_default().push(i as u32);
            }
        }
        let mut out: Vec<[u32; 2]> = map
            .into_iter()
            .filter(|(_, list)| {
                if list.len() != 2 {
                    return true;
                }
                let (x, y) = (list[0] as usize, list[1] as usize);
                self.face[x] != self.face[y] || dot(normals[x], normals[y]) < cos
            })
            .map(|((a, b), _)| [a, b])
            .collect();
        out.sort_unstable();
        out
    }

    /// A normal for each corner of each triangle: flat across a crease, averaged inside a
    /// curved face, so it shades smooth without blurring its rims.
    pub fn corner_normals(&self, crease_deg: f64) -> Vec<[P3; 3]> {
        let cos = crease_deg.to_radians().cos();
        let flat: Vec<P3> = (0..self.tris.len()).map(|t| self.normal(t).unwrap_or([0.0, 0.0, 1.0])).collect();
        let weight: Vec<f64> = (0..self.tris.len())
            .map(|t| {
                let c = self.corners(t);
                norm(cross(sub(c[1], c[0]), sub(c[2], c[0])))
            })
            .collect();
        let mut at_vertex: Vec<Vec<u32>> = vec![Vec::new(); self.verts.len()];
        for (i, t) in self.tris.iter().enumerate() {
            for &v in t {
                at_vertex[v as usize].push(i as u32);
            }
        }
        (0..self.tris.len())
            .map(|t| {
                self.tris[t].map(|v| {
                    let mut sum = [0.0; 3];
                    for &o in &at_vertex[v as usize] {
                        let o = o as usize;
                        if self.face[o] == self.face[t] && dot(flat[o], flat[t]) >= cos {
                            sum = add(sum, scale(flat[o], weight[o]));
                        }
                    }
                    if norm(sum) > 0.0 { unit(sum) } else { flat[t] }
                })
            })
            .collect()
    }

    /// The loops bounding a face, in order, each as vertex indices. Where parts of a face touch at
    /// only a point, the walk stays on the part it came in on, turning through that part's own
    /// triangles: so two parts are two loops, and a hole touching the edge at one point is one loop
    /// that passes through that point twice, the way a keyhole does.
    pub fn boundary_loops(&self, face: FaceId) -> Vec<Vec<u32>> {
        // Each edge one way round its triangle, and that triangle's third corner.
        let mut third: HashMap<(u32, u32), u32> = HashMap::new();
        for (i, t) in self.tris.iter().enumerate() {
            if self.face[i] == face {
                for k in 0..3 {
                    third.insert((t[k], t[(k + 1) % 3]), t[(k + 2) % 3]);
                }
            }
        }
        let on_to = |(a, b): (u32, u32)| -> Option<(u32, u32)> {
            let mut to = *third.get(&(a, b))?;
            for _ in 0..third.len() {
                if !third.contains_key(&(to, b)) {
                    return Some((b, to));
                }
                to = *third.get(&(to, b))?;
            }
            None
        };
        let mut edges: Vec<(u32, u32)> = third.keys().copied().filter(|(a, b)| !third.contains_key(&(*b, *a))).collect();
        edges.sort_unstable();
        let mut walked: HashSet<(u32, u32)> = HashSet::with_capacity(edges.len());
        let mut loops = Vec::new();
        for first in edges {
            let mut ring = Vec::new();
            let mut edge = first;
            while walked.insert(edge) {
                ring.push(edge.0);
                match on_to(edge) {
                    Some(next) => edge = next,
                    None => break,
                }
            }
            if ring.len() >= 3 {
                loops.push(ring);
            }
        }
        loops
    }
}

/// A box tree over triangles, for finding the few a query can touch.
pub struct Bvh {
    nodes: Vec<BvhNode>,
    order: Vec<u32>,
}

struct BvhNode {
    lo: P3,
    hi: P3,
    /// A leaf holds `order[start..start + count]`; otherwise the children are `left` and `left + 1`.
    start: u32,
    count: u32,
    left: u32,
}

fn boxes_touch(a_lo: P3, a_hi: P3, b_lo: P3, b_hi: P3) -> bool {
    (0..3).all(|k| a_lo[k] <= b_hi[k] && b_lo[k] <= a_hi[k])
}

impl Bvh {
    pub fn new(boxes: &[(P3, P3)]) -> Self {
        let mut bvh = Self { nodes: Vec::with_capacity(boxes.len().max(1)), order: (0..boxes.len() as u32).collect() };
        bvh.nodes.push(BvhNode { lo: [0.0; 3], hi: [0.0; 3], start: 0, count: 0, left: 0 });
        bvh.split(0, 0, boxes.len(), boxes);
        bvh
    }

    fn split(&mut self, node: usize, start: usize, count: usize, boxes: &[(P3, P3)]) {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for &i in &self.order[start..start + count] {
            let (a, b) = boxes[i as usize];
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
        self.nodes[node].lo = lo;
        self.nodes[node].hi = hi;
        if count <= 6 {
            self.nodes[node].start = start as u32;
            self.nodes[node].count = count as u32;
            return;
        }
        let axis = (0..3).max_by(|x, y| (hi[*x] - lo[*x]).total_cmp(&(hi[*y] - lo[*y]))).unwrap_or(0);
        let mid = count / 2;
        self.order[start..start + count].select_nth_unstable_by(mid, |x, y| {
            let c = |i: u32| boxes[i as usize].0[axis] + boxes[i as usize].1[axis];
            c(*x).total_cmp(&c(*y))
        });
        let left = self.nodes.len();
        self.nodes[node].left = left as u32;
        self.nodes.push(BvhNode { lo: [0.0; 3], hi: [0.0; 3], start: 0, count: 0, left: 0 });
        self.nodes.push(BvhNode { lo: [0.0; 3], hi: [0.0; 3], start: 0, count: 0, left: 0 });
        self.split(left, start, mid, boxes);
        self.split(left + 1, start + mid, count - mid, boxes);
    }

    /// Calls back with every box that may touch this one: a leaf is reported whole, so the
    /// caller still looks at each.
    pub fn query(&self, lo: P3, hi: P3, mut hit: impl FnMut(u32)) {
        if self.order.is_empty() {
            return;
        }
        let mut stack = vec![0_usize];
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n];
            if !boxes_touch(lo, hi, node.lo, node.hi) {
                continue;
            }
            if node.count > 0 {
                for &i in &self.order[node.start as usize..(node.start + node.count) as usize] {
                    hit(i);
                }
            } else {
                stack.push(node.left as usize);
                stack.push(node.left as usize + 1);
            }
        }
    }
}

/// The nearest point of a triangle to `p`.
pub fn closest_on_triangle(p: P3, t: [P3; 3]) -> P3 {
    let (a, b, c) = (t[0], t[1], t[2]);
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return add(a, scale(ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return add(a, scale(ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return add(b, scale(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let k = 1.0 / (va + vb + vc);
    add(a, add(scale(ab, vb * k), scale(ac, vc * k)))
}

/// How many times a closed surface wraps around `p`: 1 inside, 0 outside, whatever its size.
pub fn winding_number(mesh: &Mesh, p: P3) -> f64 {
    let mut sum = 0.0;
    for t in 0..mesh.tris.len() {
        let c = mesh.corners(t);
        let (a, b, d) = (sub(c[0], p), sub(c[1], p), sub(c[2], p));
        let (la, lb, ld) = (norm(a), norm(b), norm(d));
        let det = dot(a, cross(b, d));
        let denom = la * lb * ld + dot(a, b) * ld + dot(b, d) * la + dot(d, a) * lb;
        sum += 2.0 * det.atan2(denom);
    }
    sum / (4.0 * std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetra() -> Mesh {
        let p = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut m = Mesh::default();
        for (k, f) in [[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]].iter().enumerate() {
            m.push_tri(p[f[0]], p[f[1]], p[f[2]], k as u32);
        }
        m.welded(1e-9)
    }

    #[test]
    fn welding_closes_a_surface_and_keeps_its_faces() {
        let m = tetra();
        assert_eq!(m.verts.len(), 4);
        assert!(m.is_closed());
        assert!((m.volume() - 1.0 / 6.0).abs() < 1e-12, "{}", m.volume());
        assert_eq!(m.face_ids().len(), 4);
        assert_eq!(m.feature_edges(30.0).len(), 6);
    }

    #[test]
    fn a_reflection_keeps_a_solid_facing_out() {
        let m = tetra().transformed(&Xform::mirror([0.0; 3], [1.0, 0.0, 0.0]));
        assert!(m.is_closed());
        assert!(m.volume() > 0.0);
        assert!(m.bounds().0[0] < -0.9);
    }

    #[test]
    fn a_turn_moves_a_point_about_its_own_line() {
        let x = Xform::rotate([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], std::f64::consts::FRAC_PI_2);
        let q = x.apply([2.0, 0.0, 0.0]);
        assert!(norm(sub(q, [1.0, 1.0, 0.0])) < 1e-12, "{q:?}");
    }

    #[test]
    fn a_surface_winds_once_around_what_it_encloses() {
        let m = tetra();
        assert!((winding_number(&m, [0.1, 0.1, 0.1]) - 1.0).abs() < 1e-9);
        assert!(winding_number(&m, [2.0, 2.0, 2.0]).abs() < 1e-9);
    }

    #[test]
    fn the_same_normal_always_gets_the_same_axes() {
        for n in [[0.0, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.6, 0.0, 0.8]] {
            let f = Frame::from_normal([0.0; 3], n);
            assert!(norm(sub(cross(f.u, f.v), f.n)) < 1e-12, "{n:?}");
            assert!(norm(f.u) > 0.99 && dot(f.u, f.n).abs() < 1e-12);
        }
        let f = Frame { o: [1.0, 2.0, 3.0], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] };
        let (q, h) = f.local(f.at([4.0, 5.0], 6.0));
        assert_eq!((q, h), ([4.0, 5.0], 6.0));
    }

    #[test]
    fn a_box_tree_finds_what_touches() {
        let boxes: Vec<(P3, P3)> = (0..100).map(|i| { let x = i as f64; ([x, 0.0, 0.0], [x + 0.5, 1.0, 1.0]) }).collect();
        let tree = Bvh::new(&boxes);
        let mut hits = Vec::new();
        tree.query([10.2, 0.0, 0.0], [12.1, 1.0, 1.0], |i| hits.push(i));
        hits.sort_unstable();
        for want in [10, 11, 12] {
            assert!(hits.contains(&want), "{hits:?}");
        }
        assert!(hits.len() < 30, "it looked at only part of the tree: {hits:?}");
    }

    #[test]
    fn the_nearest_point_is_on_the_right_part_of_the_triangle() {
        let t = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(closest_on_triangle([-1.0, -1.0, 0.0], t), t[0]);
        assert!(norm(sub(closest_on_triangle([0.2, 0.2, 5.0], t), [0.2, 0.2, 0.0])) < 1e-12);
        assert!(norm(sub(closest_on_triangle([1.0, 1.0, 0.0], t), [0.5, 0.5, 0.0])) < 1e-12);
    }
}
