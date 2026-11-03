//! Remakes a triangulated surface with triangles of a chosen size and a decent shape, which is
//! what the method of moments needs: a sliver in its mesh is a wrong answer, not a slow one.
//!
//! Each pass splits what's too long, collapses what's too short, flips edges towards better
//! angles and slides vertices along the surface. Creases, rims and the borders between faces
//! stay where they are, and the original surface is what vertices are pulled back onto, so the
//! shape doesn't drift. Everything runs in a fixed order, so the same input gives the same mesh.

use std::collections::{BTreeMap, BTreeSet};

use super::mesh::{Bvh, FaceId, Mesh, closest_on_triangle};
use crate::geometry::{P3, add, cross, dot, norm, scale, sub, unit};

#[derive(Clone, Debug)]
pub struct Options {
    /// The edge length wanted, metres.
    pub target: f64,
    /// Points that must be vertices when it's done: where a feed lands.
    pub pins: Vec<P3>,
    /// The edge length wanted right at a pin. The mesh grades up from there.
    pub pin_size: f64,
    pub passes: usize,
}

/// What a vertex may do. In order: anything can collapse into something higher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Free,
    Crease,
    Corner,
}

/// Dihedral past which an edge is a crease.
const CREASE_COS: f64 = 0.866;
/// How far a collapse may turn a triangle.
const TURN_COS: f64 = 0.5;

struct Work {
    verts: Vec<P3>,
    tris: Vec<[u32; 3]>,
    face: Vec<FaceId>,
    pinned: BTreeSet<u32>,
}

fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn tri_normal(p: [P3; 3]) -> P3 {
    cross(sub(p[1], p[0]), sub(p[2], p[0]))
}

impl Work {
    fn corners(&self, t: usize) -> [P3; 3] {
        self.tris[t].map(|v| self.verts[v as usize])
    }

    /// Which triangles share each edge, in a fixed order.
    fn edges(&self) -> BTreeMap<(u32, u32), Vec<u32>> {
        let mut map: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
        for (i, t) in self.tris.iter().enumerate() {
            for k in 0..3 {
                map.entry(key(t[k], t[(k + 1) % 3])).or_default().push(i as u32);
            }
        }
        map
    }

    fn is_feature(&self, tris: &[u32], normals: &[P3]) -> bool {
        if tris.len() != 2 {
            return true;
        }
        let (x, y) = (tris[0] as usize, tris[1] as usize);
        self.face[x] != self.face[y] || dot(normals[x], normals[y]) < CREASE_COS
    }

    fn normals(&self) -> Vec<P3> {
        (0..self.tris.len())
            .map(|t| {
                let n = tri_normal(self.corners(t));
                if norm(n) > 0.0 { unit(n) } else { [0.0; 3] }
            })
            .collect()
    }

    /// Each vertex's rank, and which vertices its feature edges lead to.
    fn ranks(&self, edges: &BTreeMap<(u32, u32), Vec<u32>>, normals: &[P3]) -> (Vec<Rank>, Vec<Vec<u32>>) {
        let mut along: Vec<Vec<u32>> = vec![Vec::new(); self.verts.len()];
        for (&(a, b), list) in edges {
            if self.is_feature(list, normals) {
                along[a as usize].push(b);
                along[b as usize].push(a);
            }
        }
        let rank = (0..self.verts.len())
            .map(|v| {
                if self.pinned.contains(&(v as u32)) {
                    return Rank::Corner;
                }
                match along[v].len() {
                    0 => Rank::Free,
                    2 => {
                        let p = self.verts[v];
                        let (a, b) = (unit(sub(self.verts[along[v][0] as usize], p)), unit(sub(p, self.verts[along[v][1] as usize])));
                        if dot(a, b) < CREASE_COS { Rank::Corner } else { Rank::Crease }
                    }
                    _ => Rank::Corner,
                }
            })
            .collect();
        (rank, along)
    }

    /// Makes `p` a vertex, splitting what it lands on. Gives its index.
    fn insert(&mut self, p: P3, tol: f64) -> u32 {
        let mut best: Option<(f64, usize)> = None;
        for t in 0..self.tris.len() {
            let d = norm(sub(closest_on_triangle(p, self.corners(t)), p));
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, t));
            }
        }
        let Some((_, t)) = best else { return 0 };
        let tri = self.tris[t];
        let q = closest_on_triangle(p, self.corners(t));
        for &v in &tri {
            if norm(sub(self.verts[v as usize], q)) <= tol {
                return v;
            }
        }
        let new = self.verts.len() as u32;
        self.verts.push(q);
        // On an edge it splits both triangles sharing it, so the surface stays whole.
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let (pa, pb) = (self.verts[a as usize], self.verts[b as usize]);
            let d = sub(pb, pa);
            let s = (dot(sub(q, pa), d) / dot(d, d).max(1e-300)).clamp(0.0, 1.0);
            if norm(sub(q, add(pa, scale(d, s)))) <= tol {
                for i in 0..self.tris.len() {
                    let cur = self.tris[i];
                    for j in 0..3 {
                        let (x, y, z) = (cur[j], cur[(j + 1) % 3], cur[(j + 2) % 3]);
                        if key(x, y) == key(a, b) {
                            self.tris[i] = [x, new, z];
                            self.tris.push([new, y, z]);
                            self.face.push(self.face[i]);
                            break;
                        }
                    }
                }
                return new;
            }
        }
        self.tris[t] = [tri[0], tri[1], new];
        let f = self.face[t];
        self.tris.push([tri[1], tri[2], new]);
        self.tris.push([tri[2], tri[0], new]);
        self.face.push(f);
        self.face.push(f);
        new
    }

    fn size_at(&self, p: P3, o: &Options) -> f64 {
        let mut s = o.target;
        for pin in &o.pins {
            s = s.min(o.pin_size + 0.6 * norm(sub(p, *pin)));
        }
        s
    }

    /// Splits every edge over 4/3 of its target at the middle. Marks are per edge, so the two
    /// triangles either side always agree.
    fn split_long(&mut self, o: &Options, most: usize) {
        for _ in 0..24 {
            if self.tris.len() > most {
                return;
            }
            let mut mids: BTreeMap<(u32, u32), u32> = BTreeMap::new();
            let wanted: Vec<(u32, u32)> = self
                .edges()
                .into_keys()
                .filter(|&(a, b)| {
                    let (pa, pb) = (self.verts[a as usize], self.verts[b as usize]);
                    norm(sub(pa, pb)) > 4.0 / 3.0 * self.size_at(scale(add(pa, pb), 0.5), o)
                })
                .collect();
            if wanted.is_empty() {
                return;
            }
            for (a, b) in wanted {
                let m = scale(add(self.verts[a as usize], self.verts[b as usize]), 0.5);
                mids.insert((a, b), self.verts.len() as u32);
                self.verts.push(m);
            }
            let mut tris = Vec::with_capacity(self.tris.len() * 2);
            let mut face = Vec::with_capacity(self.tris.len() * 2);
            for (i, t) in self.tris.iter().enumerate() {
                let m: [Option<u32>; 3] = std::array::from_fn(|k| mids.get(&key(t[k], t[(k + 1) % 3])).copied());
                let f = self.face[i];
                let mut put = |x: [u32; 3]| {
                    tris.push(x);
                    face.push(f);
                };
                match m {
                    [None, None, None] => put(*t),
                    [Some(a), Some(b), Some(c)] => {
                        put([t[0], a, c]);
                        put([a, t[1], b]);
                        put([c, b, t[2]]);
                        put([a, b, c]);
                    }
                    _ => {
                        let r = (0..3).find(|i| m[*i].is_some()).unwrap_or(0);
                        let v = [t[r], t[(r + 1) % 3], t[(r + 2) % 3]];
                        let (m0, m1, m2) = (m[r].unwrap_or(v[0]), m[(r + 1) % 3], m[(r + 2) % 3]);
                        match (m1, m2) {
                            (Some(b), None) => {
                                put([v[0], m0, v[2]]);
                                put([m0, v[1], b]);
                                put([m0, b, v[2]]);
                            }
                            (None, Some(c)) => {
                                put([v[0], m0, c]);
                                put([m0, v[1], v[2]]);
                                put([m0, v[2], c]);
                            }
                            _ => {
                                put([v[0], m0, v[2]]);
                                put([m0, v[1], v[2]]);
                            }
                        }
                    }
                }
            }
            self.tris = tris;
            self.face = face;
        }
    }

    /// Takes out edges under 4/5 of their target, where that doesn't spoil what's around them.
    fn collapse_short(&mut self, o: &Options) {
        let edges = self.edges();
        let normals = self.normals();
        let (rank, _) = self.ranks(&edges, &normals);
        let mut around: Vec<Vec<u32>> = vec![Vec::new(); self.verts.len()];
        for (i, t) in self.tris.iter().enumerate() {
            for &v in t {
                around[v as usize].push(i as u32);
            }
        }
        // Shortest first, in a fixed order among equals.
        let mut list: Vec<(f64, (u32, u32), bool, usize)> = edges
            .iter()
            .filter_map(|(&(a, b), tris)| {
                let (pa, pb) = (self.verts[a as usize], self.verts[b as usize]);
                let len = norm(sub(pa, pb));
                (len < 0.8 * self.size_at(scale(add(pa, pb), 0.5), o)).then(|| (len, (a, b), self.is_feature(tris, &normals), tris.len()))
            })
            .collect();
        list.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        let mut dead = vec![false; self.tris.len()];
        let mut touched = vec![false; self.verts.len()];
        for (_, (a, b), feature, shared) in list {
            if touched[a as usize] || touched[b as usize] || shared > 2 {
                continue;
            }
            let (ra, rb) = (rank[a as usize], rank[b as usize]);
            // What stays is what ranks higher. Two creases only join along their own line, so a
            // crease never gets pulled off itself.
            let (keep, gone) = match ra.cmp(&rb) {
                std::cmp::Ordering::Greater => (a, b),
                std::cmp::Ordering::Less => (b, a),
                std::cmp::Ordering::Equal => {
                    if ra == Rank::Corner || (ra == Rank::Crease && !feature) {
                        continue;
                    }
                    (a, b)
                }
            };
            if rank[gone as usize] == Rank::Crease && !feature {
                continue;
            }
            // The edge's two triangles go; the rest of `gone`'s swing over to `keep`.
            let shared_tris: Vec<u32> = around[gone as usize].iter().copied().filter(|t| !dead[*t as usize] && self.tris[*t as usize].contains(&keep)).collect();
            let neighbours = |v: u32, tris: &Vec<[u32; 3]>, dead: &Vec<bool>| -> BTreeSet<u32> {
                let mut s = BTreeSet::new();
                for &t in &around[v as usize] {
                    if !dead[t as usize] {
                        for &x in &tris[t as usize] {
                            if x != v {
                                s.insert(x);
                            }
                        }
                    }
                }
                s
            };
            // Only if the two ends share exactly the vertices across this edge: otherwise it would
            // pinch the surface together.
            let (na, nb) = (neighbours(keep, &self.tris, &dead), neighbours(gone, &self.tris, &dead));
            if na.intersection(&nb).count() != shared_tris.len() {
                continue;
            }
            let (pk, target) = (self.verts[keep as usize], self.size_at(self.verts[keep as usize], o));
            let mut fine = true;
            for &t in &around[gone as usize] {
                let t = t as usize;
                if dead[t] || self.tris[t].contains(&keep) {
                    continue;
                }
                let before = self.corners(t);
                let mut after = before;
                for (k, v) in self.tris[t].iter().enumerate() {
                    if *v == gone {
                        after[k] = pk;
                    }
                }
                let (n0, n1) = (tri_normal(before), tri_normal(after));
                if norm(n1) <= 1e-30 || dot(unit(n0), unit(n1)) < TURN_COS {
                    fine = false;
                    break;
                }
                // No edge it makes may be long enough to want splitting again.
                for k in 0..3 {
                    if norm(sub(after[k], pk)) > 4.0 / 3.0 * target {
                        fine = false;
                    }
                }
            }
            if !fine {
                continue;
            }
            for &t in &shared_tris {
                dead[t as usize] = true;
            }
            for &t in &around[gone as usize].clone() {
                let t = t as usize;
                if !dead[t] {
                    for v in self.tris[t].iter_mut() {
                        if *v == gone {
                            *v = keep;
                        }
                    }
                    around[keep as usize].push(t as u32);
                    for &x in &self.tris[t] {
                        touched[x as usize] = true;
                    }
                }
            }
            touched[keep as usize] = true;
            touched[gone as usize] = true;
        }
        let mut i = 0;
        self.tris.retain(|_| {
            i += 1;
            !dead[i - 1]
        });
        let mut i = 0;
        self.face.retain(|_| {
            i += 1;
            !dead[i - 1]
        });
    }

    /// Flips edges inside a flat or gently curved patch to the Delaunay choice, which gives the
    /// largest smallest angle it can.
    fn flip_edges(&mut self) {
        let edges = self.edges();
        let normals = self.normals();
        let existing: BTreeSet<(u32, u32)> = edges.keys().copied().collect();
        let mut made: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut used = vec![false; self.tris.len()];
        for (&(a, b), list) in &edges {
            if list.len() != 2 || self.is_feature(list, &normals) {
                continue;
            }
            let (x, y) = (list[0] as usize, list[1] as usize);
            if used[x] || used[y] || dot(normals[x], normals[y]) < 0.985 {
                continue;
            }
            let opposite = |t: [u32; 3]| t.iter().copied().find(|v| *v != a && *v != b);
            let (Some(c), Some(d)) = (opposite(self.tris[x]), opposite(self.tris[y])) else { continue };
            if c == d || existing.contains(&key(c, d)) || made.contains(&key(c, d)) {
                continue;
            }
            let at = |o: u32, p: u32, q: u32| {
                let (u, v) = (unit(sub(self.verts[p as usize], self.verts[o as usize])), unit(sub(self.verts[q as usize], self.verts[o as usize])));
                dot(u, v).clamp(-1.0, 1.0).acos()
            };
            if at(c, a, b) + at(d, a, b) <= std::f64::consts::PI + 1e-6 {
                continue;
            }
            // In the order that keeps each triangle facing as the old pair did.
            let forward = (0..3).any(|k| self.tris[x][k] == a && self.tris[x][(k + 1) % 3] == b);
            let (from, to) = if forward { (a, b) } else { (b, a) };
            let (t1, t2) = ([c, from, d], [c, d, to]);
            let n = normals[x];
            let tri_at = |t: [u32; 3]| t.map(|v| self.verts[v as usize]);
            let (n1, n2) = (tri_normal(tri_at(t1)), tri_normal(tri_at(t2)));
            if norm(n1) <= 1e-30 || norm(n2) <= 1e-30 || dot(unit(n1), n) < 0.9 || dot(unit(n2), n) < 0.9 {
                continue;
            }
            self.tris[x] = t1;
            self.tris[y] = t2;
            used[x] = true;
            used[y] = true;
            made.insert(key(c, d));
        }
    }

    /// Slides each free vertex towards the middle of its neighbours, along the surface, and pulls it
    /// back onto the surface it started on.
    fn smooth(&mut self, reference: &Mesh, tree: &Bvh, strength: f64) {
        let edges = self.edges();
        let normals = self.normals();
        let (rank, _) = self.ranks(&edges, &normals);
        let mut sum = vec![[0.0; 3]; self.verts.len()];
        let mut count = vec![0_u32; self.verts.len()];
        for &(a, b) in edges.keys() {
            sum[a as usize] = add(sum[a as usize], self.verts[b as usize]);
            sum[b as usize] = add(sum[b as usize], self.verts[a as usize]);
            count[a as usize] += 1;
            count[b as usize] += 1;
        }
        let mut vert_normal = vec![[0.0; 3]; self.verts.len()];
        let mut around: Vec<Vec<u32>> = vec![Vec::new(); self.verts.len()];
        for (i, t) in self.tris.iter().enumerate() {
            for &v in t {
                vert_normal[v as usize] = add(vert_normal[v as usize], normals[i]);
                around[v as usize].push(i as u32);
            }
        }
        for v in 0..self.verts.len() {
            if rank[v] != Rank::Free || count[v] == 0 {
                continue;
            }
            let p = self.verts[v];
            let middle = scale(sum[v], 1.0 / count[v] as f64);
            let n = if norm(vert_normal[v]) > 0.0 { unit(vert_normal[v]) } else { continue };
            let raw = sub(middle, p);
            let slide = scale(sub(raw, scale(n, dot(raw, n))), strength);
            let moved = add(p, slide);
            let onto = project(reference, tree, moved, norm(slide) * 2.0 + 1e-12).unwrap_or(moved);
            // Refused if it would turn over anything around it.
            let ok = around[v].iter().all(|t| {
                let t = *t as usize;
                let before = self.corners(t);
                let after = self.tris[t].map(|x| if x as usize == v { onto } else { self.verts[x as usize] });
                let (n0, n1) = (tri_normal(before), tri_normal(after));
                norm(n1) > 0.0 && dot(unit(n0), unit(n1)) > 0.7
            });
            if ok {
                self.verts[v] = onto;
            }
        }
    }
}

/// The nearest point on the reference surface within `reach` of `p`.
fn project(reference: &Mesh, tree: &Bvh, p: P3, reach: f64) -> Option<P3> {
    let pad = [reach; 3];
    let mut best: Option<(f64, P3)> = None;
    tree.query(sub(p, pad), add(p, pad), |t| {
        let q = closest_on_triangle(p, reference.corners(t as usize));
        let d = norm(sub(q, p));
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, q));
        }
    });
    best.map(|b| b.1)
}

// How many triangles to stop at. Room to refine what's there, however big it starts: a model already
/// over the usual ceiling is left room to grow, rather than refused.
fn triangle_budget(wanted: f64, have: usize) -> usize {
    let floor = have.saturating_mul(4);
    (wanted as usize).max(floor).min(floor.max(2_000_000))
}

/// The surface remade to the size asked for. Open or closed, it comes back the same way.
pub fn remesh(m: &Mesh, o: &Options) -> Mesh {
    if m.is_empty() || !(o.target > 0.0) {
        return m.clone();
    }
    let (lo, hi) = m.bounds();
    let size = norm(sub(hi, lo)).max(1e-12);
    let tol = (size * 1e-9).max(1e-10);
    let boxes: Vec<(P3, P3)> = (0..m.tris.len())
        .map(|t| {
            let c = m.corners(t);
            let l = std::array::from_fn(|k| c[0][k].min(c[1][k]).min(c[2][k]));
            let h = std::array::from_fn(|k| c[0][k].max(c[1][k]).max(c[2][k]));
            (l, h)
        })
        .collect();
    let tree = Bvh::new(&boxes);
    let mut w = Work { verts: m.verts.clone(), tris: m.tris.clone(), face: m.face.clone(), pinned: BTreeSet::new() };
    // Pins only count where they land on this surface: a feed's other end may be on another body.
    for pin in &o.pins {
        if project(m, &tree, *pin, o.target.max(size * 1e-3)).is_some_and(|q| norm(sub(q, *pin)) <= (o.target * 0.25).max(tol)) {
            let v = w.insert(*pin, tol);
            w.pinned.insert(v);
        }
    }
    // Far more triangles than the area and size ask for means something's wrong: stop short.
    let most = triangle_budget(m.area() / (o.pin_size.min(o.target).max(1e-9).powi(2) * 0.2), m.tris.len());
    for pass in 0..o.passes.max(1) {
        w.split_long(o, most);
        w.collapse_short(o);
        w.flip_edges();
        let strength = if pass + 1 == o.passes { 0.3 } else { 0.5 };
        w.smooth(m, &tree, strength);
        w.flip_edges();
    }
    Mesh { verts: w.verts, tris: w.tris, face: w.face }.compacted()
}

/// How good a mesh is: the smallest angle in it, degrees, and the longest edge.
pub fn quality(m: &Mesh) -> (f64, f64) {
    let mut least = 180.0_f64;
    let mut longest = 0.0_f64;
    for t in 0..m.tris.len() {
        let c = m.corners(t);
        for k in 0..3 {
            let (a, b, d) = (c[k], c[(k + 1) % 3], c[(k + 2) % 3]);
            let (u, v) = (sub(b, a), sub(d, a));
            longest = longest.max(norm(u));
            if norm(u) > 0.0 && norm(v) > 0.0 {
                least = least.min(dot(unit(u), unit(v)).clamp(-1.0, 1.0).acos().to_degrees());
            }
        }
    }
    (least, longest)
}

/// The share of triangles with a smallest angle under `deg`.
pub fn share_below(m: &Mesh, deg: f64) -> f64 {
    let mut bad = 0;
    for t in 0..m.tris.len() {
        let c = m.corners(t);
        let worst = (0..3)
            .map(|k| {
                let (u, v) = (sub(c[(k + 1) % 3], c[k]), sub(c[(k + 2) % 3], c[k]));
                dot(unit(u), unit(v)).clamp(-1.0, 1.0).acos().to_degrees()
            })
            .fold(180.0, f64::min);
        if worst < deg {
            bad += 1;
        }
    }
    bad as f64 / m.tris.len().max(1) as f64
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::prims;

    fn opts(target: f64) -> Options {
        Options { target, pins: Vec::new(), pin_size: target, passes: 8 }
    }

    #[test]
    fn a_long_thin_box_comes_back_closed_and_the_right_size() {
        let m = prims::cuboid([0.0; 3], [0.2, 0.02, 0.01], 1).welded(1e-10);
        let out = remesh(&m, &opts(0.01));
        assert!(out.is_closed(), "{} open", out.open_edges());
        assert!((out.volume() - m.volume()).abs() / m.volume() < 1e-9, "{}", out.volume());
        assert!((out.area() - m.area()).abs() / m.area() < 1e-9);
        let (least, longest) = quality(&out);
        assert!(longest <= 0.0134, "{longest}");
        assert!(least > 15.0, "{least}");
        assert_eq!(out.face_ids().len(), 6, "every face keeps its name");
    }

    #[test]
    fn a_cone_with_a_tiny_tip_loses_its_slivers() {
        // The shape that broke the plain refinement: 32 long thin triangles meeting at a small ring.
        let m = prims::cone([0.0; 3], [0.0, 0.0, 0.06], 0.0005, 0.04, 32, 1).welded(1e-10);
        let raw = share_below(&m, 10.0);
        assert!(raw > 0.3, "it starts out bad: {raw}");
        let out = remesh(&m, &opts(0.012));
        assert!(out.is_closed(), "{} open", out.open_edges());
        let (least, longest) = quality(&out);
        assert!(share_below(&out, 15.0) < 0.05, "{} of it under 15°, the least {least}", share_below(&out, 15.0));
        assert!(longest <= 0.012 * 1.4, "{longest}");
        assert!((out.area() - m.area()).abs() / m.area() < 0.03, "{} vs {}", out.area(), m.area());
    }

    #[test]
    fn a_sphere_stays_a_sphere() {
        let m = prims::sphere([0.0; 3], 0.05, 64, 1).welded(1e-10);
        let out = remesh(&m, &opts(0.012));
        assert!(out.is_closed());
        for p in &out.verts {
            assert!((norm(*p) - 0.05).abs() < 0.001, "{}", norm(*p));
        }
        assert!(share_below(&out, 20.0) < 0.1, "{}", share_below(&out, 20.0));
    }

    #[test]
    fn an_open_sheet_keeps_its_rim() {
        let mut s = Mesh::default();
        s.push_quad([0.0, 0.0, 0.0], [0.1, 0.0, 0.0], [0.1, 0.05, 0.0], [0.0, 0.05, 0.0], 1);
        let s = s.welded(1e-10);
        let out = remesh(&s, &opts(0.01));
        assert!((out.area() - 5e-3).abs() < 1e-9, "{}", out.area());
        assert!(out.open_edges() > 0);
        let corners: Vec<P3> = vec![[0.0, 0.0, 0.0], [0.1, 0.0, 0.0], [0.1, 0.05, 0.0], [0.0, 0.05, 0.0]];
        for c in corners {
            assert!(out.verts.iter().any(|p| norm(sub(*p, c)) < 1e-12), "{c:?}");
        }
    }

    #[test]
    fn a_pin_becomes_a_vertex_and_the_mesh_grades_up_from_it() {
        let m = prims::cuboid([0.0; 3], [0.1, 0.1, 0.01], 1).welded(1e-10);
        let pin = [0.03, 0.04, 0.01];
        let out = remesh(&m, &Options { target: 0.02, pins: vec![pin], pin_size: 0.002, passes: 8 });
        assert!(out.verts.iter().any(|p| norm(sub(*p, pin)) < 1e-9), "the pin is there");
        assert!(out.is_closed());
        let near_edge = (0..out.tris.len())
            .flat_map(|t| {
                let c = out.corners(t);
                (0..3).map(move |k| (c[k], c[(k + 1) % 3]))
            })
            .filter(|(a, _)| norm(sub(*a, pin)) < 1e-9)
            .map(|(a, b)| norm(sub(a, b)))
            .fold(0.0_f64, f64::max);
        assert!(near_edge < 0.006, "fine at the pin: {near_edge}");
        // A pin out in space is nothing to do with this surface.
        let other = remesh(&m, &Options { target: 0.02, pins: vec![[1.0, 1.0, 1.0]], pin_size: 0.002, passes: 3 });
        assert!(other.verts.iter().all(|p| norm(*p) < 1.0));
    }

    #[test]
    fn a_very_big_model_has_a_budget_too() {
        assert_eq!(triangle_budget(1e9, 600_000), 2_400_000);
        assert_eq!(triangle_budget(100.0, 600_000), 2_400_000);
        assert_eq!(triangle_budget(1e9, 1000), 2_000_000);
        assert_eq!(triangle_budget(100.0, 1000), 4000);
    }

    #[test]
    fn the_same_input_gives_the_same_mesh() {
        let m = prims::cone([0.0; 3], [0.0, 0.0, 0.05], 0.002, 0.03, 32, 1).welded(1e-10);
        let (a, b) = (remesh(&m, &opts(0.008)), remesh(&m, &opts(0.008)));
        assert_eq!(a, b);
    }
}
