//! Triangle BVH for fast ray casts against placed objects. Bevy's mesh ray cast
//! tests every triangle of a mesh whose bounds the ray touches, which is far too
//! slow for racks with ~50k triangles in one mesh when placement, overlap checks
//! and gravity cast dozens of rays per frame.

use bevy::math::Vec3;

const LEAF: usize = 4;

struct Node {
    min: Vec3,
    max: Vec3,
    /// Leaf: first triangle. Inner: index of the left child (right is `left + 1`).
    first: u32,
    /// Triangles in a leaf, 0 for inner nodes.
    count: u32,
}

pub struct Bvh {
    nodes: Vec<Node>,
    tris: Vec<[Vec3; 3]>,
}

impl Bvh {
    pub fn new(positions: &[[f32; 3]], indices: &[u32]) -> Self {
        let mut tris: Vec<[Vec3; 3]> = indices
            .chunks_exact(3)
            .filter_map(|c| {
                let p = |i: u32| positions.get(i as usize).map(|v| Vec3::from_array(*v));
                Some([p(c[0])?, p(c[1])?, p(c[2])?])
            })
            .collect();
        let mut nodes = Vec::with_capacity((tris.len() / LEAF).max(1) * 2);
        nodes.push(Node { min: Vec3::ZERO, max: Vec3::ZERO, first: 0, count: 0 });
        if !tris.is_empty() {
            let n = tris.len();
            build(&mut nodes, &mut tris, 0, 0, n);
        }
        Self { nodes, tris }
    }

    /// Nearest hit along `origin + dir * t` with `0 <= t < max_t`, from either side of
    /// a triangle: `(t, unnormalized face normal)`. `dir` need not be unit length.
    pub fn cast(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Option<(f32, Vec3)> {
        if self.tris.is_empty() {
            return None;
        }
        let inv = dir.recip();
        let mut best: Option<(f32, Vec3)> = None;
        let mut limit = max_t;
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if !slab(origin, inv, node.min, node.max, limit) {
                continue;
            }
            if node.count > 0 {
                let start = node.first as usize;
                for tri in &self.tris[start..start + node.count as usize] {
                    if let Some(t) = hit_triangle(origin, dir, tri)
                        && t < limit
                    {
                        limit = t;
                        best = Some((t, (tri[1] - tri[0]).cross(tri[2] - tri[0])));
                    }
                }
            } else if sp + 2 <= stack.len() {
                stack[sp] = node.first;
                stack[sp + 1] = node.first + 1;
                sp += 2;
            }
        }
        best
    }
}

impl Bvh {
    /// Every hit along `origin + dir * t`, `0 <= t < max_t`, sorted (for inside /
    /// outside parity tests).
    pub fn hits(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Vec<f32> {
        let mut out = Vec::new();
        if self.tris.is_empty() {
            return out;
        }
        let inv = dir.recip();
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if !slab(origin, inv, node.min, node.max, max_t) {
                continue;
            }
            if node.count > 0 {
                let start = node.first as usize;
                for tri in &self.tris[start..start + node.count as usize] {
                    if let Some(t) = hit_triangle(origin, dir, tri)
                        && t < max_t
                    {
                        out.push(t);
                    }
                }
            } else if sp + 2 <= stack.len() {
                stack[sp] = node.first;
                stack[sp + 1] = node.first + 1;
                sp += 2;
            }
        }
        out.sort_by(f32::total_cmp);
        out
    }

    /// The triangles in mesh space.
    pub fn triangles(&self) -> &[[Vec3; 3]] {
        &self.tris
    }
}

fn build(nodes: &mut Vec<Node>, tris: &mut [[Vec3; 3]], index: usize, start: usize, end: usize) {
    let slice = &tris[start..end];
    let (mut min, mut max) = (Vec3::MAX, Vec3::MIN);
    let (mut cmin, mut cmax) = (Vec3::MAX, Vec3::MIN);
    for t in slice {
        for v in t {
            min = min.min(*v);
            max = max.max(*v);
        }
        let c = (t[0] + t[1] + t[2]) / 3.0;
        cmin = cmin.min(c);
        cmax = cmax.max(c);
    }
    nodes[index].min = min;
    nodes[index].max = max;
    let extent = cmax - cmin;
    if end - start <= LEAF || extent.max_element() <= 0.0 {
        nodes[index].first = start as u32;
        nodes[index].count = (end - start) as u32;
        return;
    }
    // Median split along the widest centroid axis.
    let axis = if extent.x >= extent.y && extent.x >= extent.z { 0 } else if extent.y >= extent.z { 1 } else { 2 };
    let mid = (end - start) / 2;
    let key = |t: &[Vec3; 3]| t[0][axis] + t[1][axis] + t[2][axis];
    tris[start..end].select_nth_unstable_by(mid, |a, b| key(a).total_cmp(&key(b)));
    let left = nodes.len();
    nodes.push(Node { min: Vec3::ZERO, max: Vec3::ZERO, first: 0, count: 0 });
    nodes.push(Node { min: Vec3::ZERO, max: Vec3::ZERO, first: 0, count: 0 });
    nodes[index].first = left as u32;
    build(nodes, tris, left, start, start + mid);
    build(nodes, tris, left + 1, start + mid, end);
}

fn slab(o: Vec3, inv: Vec3, min: Vec3, max: Vec3, limit: f32) -> bool {
    let a = (min - o) * inv;
    let b = (max - o) * inv;
    let t0 = a.min(b).max_element().max(0.0);
    let t1 = a.max(b).min_element().min(limit);
    // NaN (ray parallel to a slab, starting on its plane) compares false: treat as a hit.
    !(t0 > t1)
}

/// Möller–Trumbore, two-sided.
fn hit_triangle(o: Vec3, d: Vec3, [a, b, c]: &[Vec3; 3]) -> Option<f32> {
    let e1 = *b - *a;
    let e2 = *c - *a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - *a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t >= 0.0).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_nearest_of_two_planes() {
        // Two horizontal quads at y = 1 and y = 2.
        let pos = [
            [-1.0, 1.0, -1.0], [1.0, 1.0, -1.0], [1.0, 1.0, 1.0], [-1.0, 1.0, 1.0],
            [-1.0, 2.0, -1.0], [1.0, 2.0, -1.0], [1.0, 2.0, 1.0], [-1.0, 2.0, 1.0],
        ];
        let idx = [0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7];
        let bvh = Bvh::new(&pos, &idx);
        let (t, n) = bvh.cast(Vec3::new(0.2, 5.0, 0.3), Vec3::NEG_Y, f32::MAX).unwrap();
        assert!((t - 3.0).abs() < 1e-5);
        assert!(n.normalize().y.abs() > 0.99);
        // From between the planes, going up: hits the top one from below.
        let (t, _) = bvh.cast(Vec3::new(0.0, 1.5, 0.0), Vec3::Y, f32::MAX).unwrap();
        assert!((t - 0.5).abs() < 1e-5);
        // Limited range misses.
        assert!(bvh.cast(Vec3::new(0.0, 1.5, 0.0), Vec3::Y, 0.4).is_none());
        // Off to the side misses.
        assert!(bvh.cast(Vec3::new(3.0, 5.0, 0.0), Vec3::NEG_Y, f32::MAX).is_none());
    }

    #[test]
    fn many_triangles_match_brute_force() {
        // A 60x60 grid of small quads with a bump; compare against testing every triangle.
        let n = 60;
        let mut pos = Vec::new();
        for i in 0..=n {
            for j in 0..=n {
                let (x, z) = (i as f32 / n as f32, j as f32 / n as f32);
                pos.push([x, ((x * 9.0).sin() * (z * 7.0).cos()) * 0.1, z]);
            }
        }
        let mut idx = Vec::new();
        for i in 0..n {
            for j in 0..n {
                let a = (i * (n + 1) + j) as u32;
                let b = a + (n + 1) as u32;
                idx.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        let bvh = Bvh::new(&pos, &idx);
        for k in 0..200 {
            let o = Vec3::new((k as f32 * 0.37).fract(), 1.0, (k as f32 * 0.61).fract());
            let d = Vec3::new(0.1, -1.0, -0.05);
            let brute = idx
                .chunks(3)
                .filter_map(|c| hit_triangle(o, d, &[0, 1, 2].map(|q| Vec3::from_array(pos[c[q] as usize]))))
                .min_by(|a, b| a.total_cmp(b));
            let fast = bvh.cast(o, d, f32::MAX).map(|h| h.0);
            match (brute, fast) {
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-5, "{a} vs {b}"),
                (None, None) => {}
                other => panic!("mismatch {other:?}"),
            }
        }
    }
}
