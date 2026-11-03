//! Assigns a solver mesh's surface to the B-rep faces of a [`Partition`], and checks the result.
//!
//! This is the correctness-critical step of FEA here: a wrong assignment gives a load on the wrong
//! face, and results that look fine. So nothing is trusted one way: every mesh facet is matched to
//! the nearest tagged triangle (with its normal compatible), then every tagged triangle is matched
//! back to the mesh; the two must agree, and each face's area must come out right. Any failure
//! becomes an [`Issue`] that stops the run. There is no fallback that guesses.

use std::collections::HashMap;

use super::partition::{Partition, cross, dot, sub, tri_area};
use crate::ccx::deck::tri6_weights;
use crate::mesh::msh::TetMesh;

/// The point at a second-order triangle's middle, on its curved surface: the shape functions there
/// are -1/9 at the corners and 4/9 at the mid-side nodes.
fn tri6_centre(nodes: &[[f64; 3]], t: &[u32; 6]) -> [f64; 3] {
    let mut c = [0.0; 3];
    for (i, n) in t.iter().enumerate() {
        let w = if i < 3 { -1.0 / 9.0 } else { 4.0 / 9.0 };
        for (k, v) in c.iter_mut().enumerate() {
            *v += w * nodes[*n as usize][k];
        }
    }
    c
}

/// A uniform grid over a partition's triangles, for the "nearest within a tolerance" queries here.
struct TriangleGrid<'a> {
    part: &'a Partition,
    lo: [f64; 3],
    cell: f64,
    dims: [usize; 3],
    cells: HashMap<usize, Vec<u32>>,
}

impl<'a> TriangleGrid<'a> {
    fn new(part: &'a Partition, reach: f64) -> Self {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &part.positions {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let volume = size[0].max(1e-9) * size[1].max(1e-9) * size[2].max(1e-9);
        let cell = (volume / part.triangles.len().max(1) as f64).cbrt().max(reach).max(size[0].max(size[1]).max(size[2]) / 200.0).max(1e-9);
        let dims = [(size[0] / cell) as usize + 1, (size[1] / cell) as usize + 1, (size[2] / cell) as usize + 1];
        let mut grid = Self { part, lo, cell, dims, cells: HashMap::new() };
        for i in 0..part.triangles.len() {
            let [a, b, c] = part.triangle(i);
            let (mut tl, mut th) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for p in [a, b, c] {
                for k in 0..3 {
                    tl[k] = tl[k].min(p[k] - reach);
                    th[k] = th[k].max(p[k] + reach);
                }
            }
            let (l, h) = (grid.coords(tl), grid.coords(th));
            for z in l[2]..=h[2] {
                for y in l[1]..=h[1] {
                    for x in l[0]..=h[0] {
                        grid.cells.entry(x + grid.dims[0] * (y + grid.dims[1] * z)).or_default().push(i as u32);
                    }
                }
            }
        }
        grid
    }

    fn coords(&self, p: [f64; 3]) -> [usize; 3] {
        std::array::from_fn(|a| (((p[a] - self.lo[a]) / self.cell).max(0.0) as usize).min(self.dims[a] - 1))
    }

    /// The closest triangle within `reach` whose normal isn't opposed to `normal`.
    fn nearest(&self, p: [f64; 3], normal: Option<[f64; 3]>, reach: f64) -> Option<(u32, f64)> {
        let c = self.coords(p);
        let list = self.cells.get(&(c[0] + self.dims[0] * (c[1] + self.dims[1] * c[2])))?;
        let mut best: Option<(u32, f64)> = None;
        for &i in list {
            let [a, b, tc] = self.part.triangle(i as usize);
            let d = distance_to_triangle(p, a, b, tc);
            if d > reach || best.is_some_and(|(_, bd)| d >= bd) {
                continue;
            }
            if let Some(n) = normal {
                let tn = cross(sub(b, a), sub(tc, a));
                // Across a sharp edge the neighbouring face can be just as near, but faces another
                // way. Gmsh's orientation isn't the body's, so only the line of the normal counts.
                if dot(n, tn).abs() < 0.5 * (dot(n, n) * dot(tn, tn)).sqrt() {
                    continue;
                }
            }
            best = Some((i, d));
        }
        best
    }
}

/// Distance from a point to a triangle (Ericson, *Real-Time Collision Detection*).
pub fn distance_to_triangle(p: [f64; 3], a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
    let (d1, d2) = (dot(ab, ap), dot(ac, ap));
    let at = |q: [f64; 3]| {
        let v = sub(p, q);
        dot(v, v).sqrt()
    };
    if d1 <= 0.0 && d2 <= 0.0 {
        return at(a);
    }
    let bp = sub(p, b);
    let (d3, d4) = (dot(ab, bp), dot(ac, bp));
    if d3 >= 0.0 && d4 <= d3 {
        return at(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return at([a[0] + v * ab[0], a[1] + v * ab[1], a[2] + v * ab[2]]);
    }
    let cp = sub(p, c);
    let (d5, d6) = (dot(ab, cp), dot(ac, cp));
    if d6 >= 0.0 && d5 <= d6 {
        return at(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return at([a[0] + w * ac[0], a[1] + w * ac[1], a[2] + w * ac[2]]);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let bc = sub(c, b);
        return at([b[0] + w * bc[0], b[1] + w * bc[1], b[2] + w * bc[2]]);
    }
    let denom = 1.0 / (va + vb + vc);
    let (v, w) = (vb * denom, vc * denom);
    at([a[0] + ab[0] * v + ac[0] * w, a[1] + ab[1] * v + ac[1] * w, a[2] + ab[2] * v + ac[2] * w])
}

/// Something wrong with an assignment: it must stop a run.
#[derive(Debug, Clone, PartialEq)]
pub enum Issue {
    /// Mesh facets near no face, as a share of the mesh's area.
    Unassigned { entity: i32, share: f64 },
    /// A face's area in the mesh differs from its true area.
    AreaMismatch { face: u32, tagged: f64, meshed: f64 },
    /// Looking from the tagged side, a face's area lands on other faces' mesh.
    Disagreement { face: u32, agreement: f64 },
    /// A face got no mesh at all.
    MissingFace { face: u32, area: f64 },
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Issue::Unassigned { entity, share } => write!(f, "{:.1}% of mesh surface {entity} is near no face of the body", share * 100.0),
            Issue::AreaMismatch { face, tagged, meshed } => write!(f, "face {face}: the mesh covers {meshed:.6e} m² of its {tagged:.6e} m²"),
            Issue::Disagreement { face, agreement } => write!(f, "face {face}: only {:.1}% of it maps back to itself", agreement * 100.0),
            Issue::MissingFace { face, area } => write!(f, "face {face} ({area:.6e} m²) has no mesh; a finer mesh may be needed"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Assignment {
    /// For each mesh surface entity (by index in `mesh.surfaces`), each facet's face.
    pub facets: Vec<Vec<Option<u32>>>,
    /// Entities lying entirely on one face: entity tag to face.
    pub whole: HashMap<i32, u32>,
    /// Entities spanning several faces (STEP export merged them), so that facets decide, which
    /// makes the boundary between those faces follow the mesh.
    pub jagged: Vec<i32>,
    /// Faces in those entities: usable, but their boundary is only as good as the mesh.
    pub approximate: Vec<u32>,
    /// Each face's area in the mesh.
    pub meshed_area: Vec<f64>,
    pub issues: Vec<Issue>,
}

/// Thresholds of [`classify`]. The defaults are the plan's.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// A face's meshed area may differ from its true area by this share.
    pub area: f64,
    /// A share of each face that must map back to itself.
    pub agreement: f64,
    /// The same two for faces whose boundary follows the mesh (a surface STEP export merged).
    pub approximate_area: f64,
    pub approximate_agreement: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { area: 0.01, agreement: 0.99, approximate_area: 0.05, approximate_agreement: 0.9 }
    }
}

/// Assigns `mesh`'s surface facets to `part`'s faces, in the same units. `tolerance` is the largest
/// distance between a mesh facet and the surface it belongs to.
pub fn classify(mesh: &TetMesh, part: &Partition, tolerance: f64, limits: Limits) -> Assignment {
    let grid = TriangleGrid::new(part, tolerance);
    let mut out = Assignment { meshed_area: vec![0.0; part.faces as usize], ..Default::default() };

    for s in &mesh.surfaces {
        let mut by_face: HashMap<u32, f64> = HashMap::new();
        let mut lost = 0.0;
        let mut total = 0.0;
        let mut facets = Vec::with_capacity(s.tris.len());
        for t in &s.tris {
            // The true curved area and middle, not the chords between the corners: those fall short
            // of a curved face by more than the audit allows.
            let (_, area) = tri6_weights(&mesh.nodes, t);
            total += area;
            let centre = tri6_centre(&mesh.nodes, t);
            let normal = {
                let (a, b, c) = (mesh.nodes[t[0] as usize], mesh.nodes[t[1] as usize], mesh.nodes[t[2] as usize]);
                cross(sub(b, a), sub(c, a))
            };
            match grid.nearest(centre, Some(normal), tolerance) {
                Some((tri, _)) => {
                    let face = part.tri_face[tri as usize];
                    *by_face.entry(face).or_default() += area;
                    facets.push(Some(face));
                }
                None => {
                    lost += area;
                    facets.push(None);
                }
            }
        }
        if total > 0.0 && lost / total > 1e-3 {
            out.issues.push(Issue::Unassigned { entity: s.tag, share: lost / total });
        }
        if let Some((&face, &area)) = by_face.iter().max_by(|a, b| a.1.total_cmp(b.1)) {
            if total > 0.0 && area / total >= limits.agreement {
                out.whole.insert(s.tag, face);
                // The whole entity is this face: a stray facet near an edge is the nearest-point
                // search's noise, not a second face.
                for f in &mut facets {
                    *f = Some(face);
                }
            } else {
                out.jagged.push(s.tag);
            }
        }
        for (t, f) in s.tris.iter().zip(&facets) {
            if let Some(f) = f
                && let Some(slot) = out.meshed_area.get_mut(*f as usize)
            {
                *slot += tri6_weights(&mesh.nodes, t).1;
            }
        }
        out.facets.push(facets);
    }

    // Faces sharing a merged surface: their boundary follows the mesh, so their areas can only be
    // right to about a facet. They are usable with that said, and held to a looser limit.
    for (s, facets) in mesh.surfaces.iter().zip(&out.facets) {
        if out.jagged.contains(&s.tag) {
            for f in facets.iter().flatten() {
                if !out.approximate.contains(f) {
                    out.approximate.push(*f);
                }
            }
        }
    }
    let area_limit = |face: u32, out: &Assignment| if out.approximate.contains(&face) { limits.approximate_area } else { limits.area };
    let agreement_limit = |face: u32, out: &Assignment| if out.approximate.contains(&face) { limits.approximate_agreement } else { limits.agreement };

    // Each face's area must be there.
    let tagged = part.face_areas();
    for (face, (&truth, &meshed)) in tagged.iter().zip(&out.meshed_area).enumerate() {
        let face = face as u32;
        if truth <= 0.0 {
            continue;
        }
        if meshed <= 0.0 {
            out.issues.push(Issue::MissingFace { face, area: truth });
        } else if (meshed - truth).abs() / truth > area_limit(face, &out) {
            out.issues.push(Issue::AreaMismatch { face, tagged: truth, meshed });
        }
    }

    // The other way round: a grid over the mesh's own facets, relabelled as a partition.
    let back = mesh_as_partition(mesh, &out);
    let back_grid = TriangleGrid::new(&back, tolerance * 2.0);
    let mut hit = vec![0.0; part.faces as usize];
    let mut area = vec![0.0; part.faces as usize];
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        let tri_a = tri_area(a, b, c);
        let face = part.tri_face[i] as usize;
        let Some(slot) = area.get_mut(face) else { continue };
        *slot += tri_a;
        let centre = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0];
        let normal = cross(sub(b, a), sub(c, a));
        if let Some((tri, _)) = back_grid.nearest(centre, Some(normal), tolerance * 2.0)
            && back.tri_face[tri as usize] as usize == face
        {
            hit[face] += tri_a;
        }
    }
    for face in 0..area.len() {
        if area[face] > 0.0 && hit[face] / area[face] < agreement_limit(face as u32, &out) {
            out.issues.push(Issue::Disagreement { face: face as u32, agreement: hit[face] / area[face] });
        }
    }
    out
}

/// The mesh's surface facets with their assigned faces, for the backward check. Each second-order
/// triangle becomes its four straight sub-triangles, which follow a curved face closely.
fn mesh_as_partition(mesh: &TetMesh, a: &Assignment) -> Partition {
    let mut p = Partition { name: "mesh".into(), positions: mesh.nodes.clone(), faces: a.meshed_area.len() as u32, ..Default::default() };
    for (s, facets) in mesh.surfaces.iter().zip(&a.facets) {
        for (t, f) in s.tris.iter().zip(facets) {
            if let Some(f) = f {
                for sub_tri in [[t[0], t[3], t[5]], [t[3], t[1], t[4]], [t[5], t[4], t[2]], [t[3], t[4], t[5]]] {
                    p.triangles.push(sub_tri);
                    p.tri_face.push(*f);
                }
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::msh::SurfaceEntity;

    /// A 1 x 1 square in z = 0 as two faces: x < 0.5 is face 0, x > 0.5 is face 1.
    fn two_faces() -> Partition {
        Partition {
            name: "t".into(),
            positions: vec![[0., 0., 0.], [0.5, 0., 0.], [1., 0., 0.], [0., 1., 0.], [0.5, 1., 0.], [1., 1., 0.]],
            triangles: vec![[0, 1, 4], [0, 4, 3], [1, 2, 5], [1, 5, 4]],
            tri_face: vec![0, 0, 1, 1],
            faces: 2,
        }
    }

    /// A surface with real mid-side nodes on the corners' straight edges, as Gmsh's are.
    fn with_midsides(nodes: &mut Vec<[f64; 3]>, a: u32, b: u32, c: u32) -> [u32; 6] {
        let mut mid = |p: u32, q: u32| {
            let (u, v) = (nodes[p as usize], nodes[q as usize]);
            nodes.push([(u[0] + v[0]) / 2.0, (u[1] + v[1]) / 2.0, (u[2] + v[2]) / 2.0]);
            nodes.len() as u32 - 1
        };
        let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
        [a, b, c, ab, bc, ca]
    }

    /// A mesh of the same square: one surface entity per face, each two triangles.
    fn matching_mesh() -> TetMesh {
        let mut nodes = two_faces().positions;
        let (t1, t2) = (with_midsides(&mut nodes, 0, 1, 4), with_midsides(&mut nodes, 0, 4, 3));
        let (t3, t4) = (with_midsides(&mut nodes, 1, 2, 5), with_midsides(&mut nodes, 1, 5, 4));
        TetMesh { nodes, surfaces: vec![SurfaceEntity { tag: 10, tris: vec![t1, t2] }, SurfaceEntity { tag: 20, tris: vec![t3, t4] }], ..Default::default() }
    }

    #[test]
    fn matching_surfaces_classify_cleanly() {
        let a = classify(&matching_mesh(), &two_faces(), 1e-6, Limits::default());
        assert_eq!(a.issues, vec![]);
        assert_eq!(a.whole[&10], 0);
        assert_eq!(a.whole[&20], 1);
        assert!(a.jagged.is_empty());
    }

    #[test]
    fn a_merged_entity_is_jagged_but_keeps_its_faces() {
        let m = matching_mesh();
        let tris = m.surfaces.iter().flat_map(|s| s.tris.iter().copied()).collect();
        let mesh = TetMesh { nodes: m.nodes, surfaces: vec![SurfaceEntity { tag: 1, tris }], ..Default::default() };
        let a = classify(&mesh, &two_faces(), 1e-6, Limits::default());
        assert_eq!(a.jagged, [1]);
        assert_eq!(a.facets[0], [Some(0), Some(0), Some(1), Some(1)]);
        assert_eq!(a.issues, vec![]);
    }

    #[test]
    fn a_mesh_of_something_else_stops_the_run() {
        let mut mesh = matching_mesh();
        for n in &mut mesh.nodes {
            n[2] += 1.0;
        }
        let a = classify(&mesh, &two_faces(), 1e-3, Limits::default());
        assert!(a.issues.iter().any(|i| matches!(i, Issue::Unassigned { .. })), "{:?}", a.issues);
        assert!(a.issues.iter().any(|i| matches!(i, Issue::MissingFace { .. })), "{:?}", a.issues);
    }

    #[test]
    fn a_missing_part_of_a_face_is_an_area_mismatch() {
        let mut mesh = matching_mesh();
        mesh.surfaces[1].tris.pop();
        let a = classify(&mesh, &two_faces(), 1e-6, Limits::default());
        assert!(a.issues.iter().any(|i| matches!(i, Issue::AreaMismatch { face: 1, .. })), "{:?}", a.issues);
    }

    #[test]
    fn distance_to_triangle_regions() {
        let (a, b, c) = ([0., 0., 0.], [1., 0., 0.], [0., 1., 0.]);
        let d = |p| distance_to_triangle(p, a, b, c);
        assert!((d([0.25, 0.25, 2.0]) - 2.0).abs() < 1e-12, "above the face");
        assert!((d([-1.0, 0.0, 0.0]) - 1.0).abs() < 1e-12, "past a corner");
        assert!((d([0.5, -2.0, 0.0]) - 2.0).abs() < 1e-12, "past an edge");
        assert!((d([1.0, 1.0, 0.0]) - (0.5f64).sqrt()).abs() < 1e-12, "past the long edge");
    }
}
