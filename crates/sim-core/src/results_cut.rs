//! A cut through a result's volume, added to what's drawn, carrying every node field. Flow results
//! are mostly about what's happening away from the surfaces, so this is how they're looked at.

use std::collections::HashMap;
use std::sync::Arc;

use crate::results::{Location, ResultSet};

/// Adds the plane through `origin` with normal `normal` to `set`'s surface. Node fields are
/// interpolated onto its new nodes; cell fields are left out of the plane (and of the file's drawing).
pub fn add_cut_plane(set: &mut ResultSet, origin: [f64; 3], normal: [f64; 3]) -> usize {
    add_cut_plane_within(set, origin, normal, [f64::NEG_INFINITY; 3], [f64::INFINITY; 3])
}

/// The same, only as far as a box: for a small body in a big domain, where the rest would be empty.
pub fn add_cut_plane_within(set: &mut ResultSet, origin: [f64; 3], normal: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> usize {
    let distance = |p: &[f64; 3]| (p[0] - origin[0]) * normal[0] + (p[1] - origin[1]) * normal[1] + (p[2] - origin[2]) * normal[2];
    let d: Vec<f64> = set.mesh.nodes.iter().map(distance).collect();
    let inside = |n: u32| {
        let p = set.mesh.nodes[n as usize];
        (0..3).all(|k| p[k] >= lo[k] && p[k] <= hi[k])
    };
    // A new node on each cut edge: (lower node, higher node) to its index, with its weight.
    let first_new = set.mesh.nodes.len();
    let mut edge_nodes: HashMap<(u32, u32), u32> = HashMap::new();
    let mut weights: Vec<(u32, u32, f32)> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut new_points: Vec<[f64; 3]> = Vec::new();
    for t in &set.mesh.tets {
        if !t.iter().all(|n| inside(*n)) {
            continue;
        }
        let mut cuts: Vec<u32> = Vec::with_capacity(4);
        // Ordered around the tetrahedron so that four cut edges form a proper quad.
        for (a, b) in [(0, 1), (1, 2), (2, 0), (0, 3), (1, 3), (2, 3)] {
            let (na, nb) = (t[a], t[b]);
            let (da, db) = (d[na as usize], d[nb as usize]);
            if (da < 0.0) == (db < 0.0) {
                continue;
            }
            let key = if na < nb { (na, nb) } else { (nb, na) };
            let id = *edge_nodes.entry(key).or_insert_with(|| {
                let f = (da / (da - db)) as f32;
                let (pa, pb) = (set.mesh.nodes[na as usize], set.mesh.nodes[nb as usize]);
                new_points.push(std::array::from_fn(|k| pa[k] + (pb[k] - pa[k]) * f64::from(f)));
                weights.push((na, nb, f));
                (first_new + new_points.len() - 1) as u32
            });
            cuts.push(id);
        }
        match cuts.len() {
            3 => tris.push([cuts[0], cuts[1], cuts[2]]),
            4 => {
                // The four crossings of a quad cut: order them by angle about their middle.
                let points: Vec<[f64; 3]> = cuts.iter().map(|c| new_points[*c as usize - first_new]).collect();
                let mid: [f64; 3] = std::array::from_fn(|k| points.iter().map(|p| p[k]).sum::<f64>() / 4.0);
                let u = sub(points[0], mid);
                let v = cross(normal, u);
                let mut order: Vec<(f64, u32)> = points.iter().zip(&cuts).map(|(p, c)| {
                    let r = sub(*p, mid);
                    (dot(r, v).atan2(dot(r, u)), *c)
                }).collect();
                order.sort_by(|a, b| a.0.total_cmp(&b.0));
                tris.push([order[0].1, order[1].1, order[2].1]);
                tris.push([order[0].1, order[2].1, order[3].1]);
            }
            _ => {}
        }
    }
    if tris.is_empty() {
        return 0;
    }
    let added = tris.len();
    set.mesh.cut_start.get_or_insert(set.mesh.surface.len());
    set.mesh.nodes.extend(new_points);
    set.mesh.surface.extend(tris);
    for step in &mut set.steps {
        step.fields.retain(|f| f.location == Location::Node);
        for f in &mut step.fields {
            let s = f.stride();
            let mut data = f.data.to_vec();
            data.reserve(weights.len() * s);
            for (a, b, w) in &weights {
                for k in 0..s {
                    let (va, vb) = (f.data[*a as usize * s + k], f.data[*b as usize * s + k]);
                    data.push(va + (vb - va) * w);
                }
            }
            f.data = Arc::from(data);
        }
    }
    added
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Drops surface triangles lying on the box around the nodes: the outside of a wind tunnel's domain
/// would hide everything in it.
pub fn remove_domain_box(set: &mut ResultSet) {
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in &set.mesh.nodes {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let tol = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]) * 1e-6;
    let nodes = &set.mesh.nodes;
    set.mesh.surface.retain(|t| {
        !(0..3).any(|k| t.iter().all(|n| (nodes[*n as usize][k] - lo[k]).abs() < tol) || t.iter().all(|n| (nodes[*n as usize][k] - hi[k]).abs() < tol))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::{Field, ResultMesh, ResultStep, StepKind};

    /// Two tetrahedra along x, with a field equal to x.
    fn sample() -> ResultSet {
        let nodes = vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0], [2.0, 2.0, 2.0]];
        let x: Vec<f32> = nodes.iter().map(|p| p[0] as f32).collect();
        let f = Field { name: "x".into(), unit: "m".into(), location: Location::Node, components: vec!["x".into()], data: Arc::from(x) };
        ResultSet { mesh: ResultMesh { nodes, tets: vec![[0, 1, 2, 3], [1, 2, 3, 4]], surface: vec![], cut_start: None }, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields: vec![f] }], plots: vec![] }
    }

    #[test]
    fn the_plane_interpolates_fields_exactly() {
        let mut s = sample();
        let added = add_cut_plane(&mut s, [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        assert!(added > 0);
        let f = s.steps[0].field("x").unwrap();
        assert_eq!(f.len(), s.mesh.nodes.len(), "every node has a value");
        // Every new node is on the plane, and a field equal to x reads 1 there.
        for i in 5..s.mesh.nodes.len() {
            assert!((s.mesh.nodes[i][0] - 1.0).abs() < 1e-9);
            assert!((f.at(i)[0] - 1.0).abs() < 1e-6);
        }
        // Shared edges give shared nodes.
        let used: std::collections::HashSet<u32> = s.mesh.surface.iter().flatten().copied().collect();
        assert_eq!(used.len(), s.mesh.nodes.len() - 5);
    }

    #[test]
    fn a_plane_can_stop_at_a_box() {
        let mut s = sample();
        let everywhere = add_cut_plane(&mut s, [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        assert!(everywhere > 0);
        // A box around all of it changes nothing; one too small for any tetrahedron leaves nothing.
        let mut wide = sample();
        assert_eq!(add_cut_plane_within(&mut wide, [1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [-1.0; 3], [3.0; 3]), everywhere);
        let mut small = sample();
        assert_eq!(add_cut_plane_within(&mut small, [1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [-1.0; 3], [1.5; 3]), 0);
    }

    #[test]
    fn a_plane_missing_the_mesh_changes_nothing() {
        let mut s = sample();
        assert_eq!(add_cut_plane(&mut s, [9.0, 0.0, 0.0], [1.0, 0.0, 0.0]), 0);
        assert_eq!(s.mesh.nodes.len(), 5);
    }

    #[test]
    fn the_domain_box_goes_and_the_body_stays() {
        let mut s = sample();
        s.mesh.nodes.push([1.0, 1.0, 1.0]);
        // One triangle on the box's x = 0 plane, one in the middle of the domain.
        s.mesh.surface = vec![[0, 2, 3], [1, 5, 4]];
        remove_domain_box(&mut s);
        assert_eq!(s.mesh.surface, vec![[1, 5, 4]]);
    }
}
