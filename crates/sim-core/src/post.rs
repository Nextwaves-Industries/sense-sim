//! Looking inside a result's volume: cut planes and isosurfaces by marching tetrahedra, and
//! streamlines by integrating the velocity through the cells. Works on any result with tetrahedra.

use std::collections::HashMap;

use crate::results::{Field, ResultMesh};

/// A surface through a volume. Each point lies on a mesh edge; `weights` say where, so that any node
/// field can be read on it without another search.
#[derive(Debug, Clone, Default)]
pub struct Slice {
    pub points: Vec<[f64; 3]>,
    /// (lower node, higher node, how far along), one per point.
    pub weights: Vec<(u32, u32, f32)>,
    /// Indices into `points`.
    pub triangles: Vec<[u32; 3]>,
}

impl Slice {
    /// A node field's values at the slice's points. `values` holds one value per mesh node.
    pub fn sample(&self, values: &[f32]) -> Vec<f32> {
        self.weights
            .iter()
            .map(|(a, b, f)| match (values.get(*a as usize), values.get(*b as usize)) {
                (Some(x), Some(y)) => x + (y - x) * f,
                _ => f32::NAN,
            })
            .collect()
    }

    pub fn area(&self) -> f64 {
        self.triangles.iter().map(|t| crate::geometry::partition::tri_area(self.points[t[0] as usize], self.points[t[1] as usize], self.points[t[2] as usize])).sum()
    }
}

/// Where `scalar`, one value per node, crosses `level`. Tetrahedra with a value that isn't finite are
/// left out instead of drawing nonsense.
pub fn slice(mesh: &ResultMesh, scalar: &[f64], level: f64) -> Slice {
    let mut out = Slice::default();
    let mut on_edge: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &mesh.tets {
        let Some(values) = t.iter().map(|n| scalar.get(*n as usize).copied().filter(|v| v.is_finite())).collect::<Option<Vec<f64>>>() else { continue };
        let below: Vec<usize> = (0..4).filter(|i| values[*i] < level).collect();
        let above: Vec<usize> = (0..4).filter(|i| values[*i] >= level).collect();
        let mut point = |a: usize, b: usize| -> u32 {
            let (mut na, mut nb, mut va, mut vb) = (t[a], t[b], values[a], values[b]);
            if na > nb {
                std::mem::swap(&mut na, &mut nb);
                std::mem::swap(&mut va, &mut vb);
            }
            *on_edge.entry((na, nb)).or_insert_with(|| {
                let f = (level - va) / (vb - va);
                let (pa, pb) = (mesh.nodes[na as usize], mesh.nodes[nb as usize]);
                out.points.push(std::array::from_fn(|k| pa[k] + (pb[k] - pa[k]) * f));
                out.weights.push((na, nb, f as f32));
                (out.points.len() - 1) as u32
            })
        };
        let mut quad: Option<[u32; 4]> = None;
        let mut tri: Option<[u32; 3]> = None;
        match (below.len(), above.len()) {
            (1, 3) => tri = Some([point(below[0], above[0]), point(below[0], above[1]), point(below[0], above[2])]),
            (3, 1) => tri = Some([point(above[0], below[0]), point(above[0], below[1]), point(above[0], below[2])]),
            // Going round the four crossed edges: a-c, a-d, b-d, b-c.
            (2, 2) => quad = Some([point(below[0], above[0]), point(below[0], above[1]), point(below[1], above[1]), point(below[1], above[0])]),
            _ => {}
        }
        if let Some(t) = tri {
            out.triangles.push(t);
        }
        if let Some(q) = quad {
            out.triangles.push([q[0], q[1], q[2]]);
            out.triangles.push([q[0], q[2], q[3]]);
        }
    }
    out
}

/// Each node's signed distance from a plane: the scalar whose zero level is that plane.
pub fn plane_distance(mesh: &ResultMesh, origin: [f64; 3], normal: [f64; 3]) -> Vec<f64> {
    mesh.nodes.iter().map(|p| (p[0] - origin[0]) * normal[0] + (p[1] - origin[1]) * normal[1] + (p[2] - origin[2]) * normal[2]).collect()
}

/// Tetrahedra filling a grid of `size` nodes. All six in a cube share its main diagonal, so
/// neighbouring cubes agree on the diagonals of their shared faces. `keep(x, y, z)` says whether the
/// cube starting at that node is in the volume.
pub fn grid_tets(size: [u32; 3], keep: impl Fn(u32, u32, u32) -> bool) -> Vec<[u32; 4]> {
    let [nx, ny, nz] = size;
    let index = |x: u32, y: u32, z: u32| x + nx * (y + ny * z);
    let mut tets = Vec::new();
    for z in 0..nz.saturating_sub(1) {
        for y in 0..ny.saturating_sub(1) {
            for x in 0..nx.saturating_sub(1) {
                if !keep(x, y, z) {
                    continue;
                }
                let c = |bits: u32| index(x + (bits & 1), y + ((bits >> 1) & 1), z + ((bits >> 2) & 1));
                for [a, b] in [[1, 3], [1, 5], [2, 3], [2, 6], [4, 5], [4, 6]] {
                    tets.push([c(0), c(a), c(b), c(7)]);
                }
            }
        }
    }
    tets
}

/// Finds the tetrahedron holding a point, from bins laid over the mesh.
pub struct Locator<'a> {
    mesh: &'a ResultMesh,
    lo: [f64; 3],
    size: [f64; 3],
    bins: [usize; 3],
    cells: Vec<Vec<u32>>,
}

impl<'a> Locator<'a> {
    pub fn new(mesh: &'a ResultMesh) -> Self {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for t in &mesh.tets {
            for n in t {
                let p = mesh.nodes[*n as usize];
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        let size: [f64; 3] = std::array::from_fn(|k| (hi[k] - lo[k]).max(1e-12));
        // About one tetrahedron per bin, with the bins near enough to cubes.
        let longest = size[0].max(size[1]).max(size[2]);
        let per_long_side = (mesh.tets.len() as f64 * longest * longest * longest / (size[0] * size[1] * size[2])).cbrt();
        let bins: [usize; 3] = std::array::from_fn(|k| ((per_long_side * size[k] / longest).ceil() as usize).clamp(1, 160));
        let mut cells = vec![Vec::new(); bins[0] * bins[1] * bins[2]];
        for (i, t) in mesh.tets.iter().enumerate() {
            let (mut a, mut b) = ([usize::MAX; 3], [0usize; 3]);
            for n in t {
                let p = mesh.nodes[*n as usize];
                for k in 0..3 {
                    let bin = (((p[k] - lo[k]) / size[k] * bins[k] as f64) as usize).min(bins[k] - 1);
                    a[k] = a[k].min(bin);
                    b[k] = b[k].max(bin);
                }
            }
            for z in a[2]..=b[2] {
                for y in a[1]..=b[1] {
                    for x in a[0]..=b[0] {
                        cells[x + bins[0] * (y + bins[1] * z)].push(i as u32);
                    }
                }
            }
        }
        Self { mesh, lo, size, bins, cells }
    }

    /// The tetrahedron holding `p`, and `p`'s weights on its four nodes.
    pub fn locate(&self, p: [f64; 3]) -> Option<(usize, [f64; 4])> {
        let mut at = [0usize; 3];
        for k in 0..3 {
            let f = (p[k] - self.lo[k]) / self.size[k];
            if !(-1e-9..=1.0 + 1e-9).contains(&f) {
                return None;
            }
            at[k] = ((f * self.bins[k] as f64) as usize).min(self.bins[k] - 1);
        }
        for t in &self.cells[at[0] + self.bins[0] * (at[1] + self.bins[1] * at[2])] {
            if let Some(w) = self.weights(*t as usize, p)
                && w.iter().all(|v| *v >= -1e-9)
            {
                return Some((*t as usize, w));
            }
        }
        None
    }

    fn weights(&self, tet: usize, p: [f64; 3]) -> Option<[f64; 4]> {
        let t = self.mesh.tets[tet];
        let node = |i: usize| self.mesh.nodes[t[i] as usize];
        let a = node(0);
        let (u, v, w) = (sub(node(1), a), sub(node(2), a), sub(node(3), a));
        let det = dot(u, cross(v, w));
        if det.abs() < 1e-300 {
            return None;
        }
        let r = sub(p, a);
        let (l1, l2, l3) = (dot(r, cross(v, w)) / det, dot(u, cross(r, w)) / det, dot(u, cross(v, r)) / det);
        Some([1.0 - l1 - l2 - l3, l1, l2, l3])
    }

    fn edge(&self, tet: usize) -> f64 {
        let t = self.mesh.tets[tet];
        let (a, b) = (self.mesh.nodes[t[0] as usize], self.mesh.nodes[t[1] as usize]);
        dot(sub(a, b), sub(a, b)).sqrt()
    }
}

/// A streamline: where it goes, and the speed at each point.
#[derive(Debug, Clone, Default)]
pub struct Streamline {
    pub points: Vec<[f64; 3]>,
    pub speeds: Vec<f32>,
}

/// Follows `velocity`, a three-component node field, from each seed both ways, by fourth-order
/// Runge-Kutta. A line ends at the edge of the volume, where the flow stops, or at `max_points`.
pub fn streamlines(mesh: &ResultMesh, velocity: &Field, seeds: &[[f64; 3]], max_points: usize) -> Vec<Streamline> {
    if velocity.stride() != 3 || mesh.tets.is_empty() {
        return Vec::new();
    }
    let locator = Locator::new(mesh);
    let peak = velocity.range().map_or(0.0, |r| f64::from(r.1));
    if peak <= 0.0 {
        return Vec::new();
    }
    let at = |p: [f64; 3]| -> Option<([f64; 3], usize)> {
        let (tet, w) = locator.locate(p)?;
        let mut u = [0.0; 3];
        for (node, weight) in mesh.tets[tet].iter().zip(w) {
            let v = velocity.at(*node as usize);
            for k in 0..3 {
                u[k] += weight * f64::from(v[k]);
            }
        }
        Some((u, tet))
    };
    let follow = |seed: [f64; 3], sign: f64| -> Vec<([f64; 3], f32)> {
        let mut out = Vec::new();
        let mut p = seed;
        for _ in 0..max_points / 2 {
            let Some((u1, tet)) = at(p) else { break };
            let speed = dot(u1, u1).sqrt();
            out.push((p, speed as f32));
            if speed < peak * 1e-3 {
                break;
            }
            // A step of about a quarter of the cell.
            let h = sign * 0.25 * locator.edge(tet) / speed;
            let step = |from: [f64; 3], u: [f64; 3], by: f64| -> [f64; 3] { std::array::from_fn(|k| from[k] + by * u[k]) };
            let Some((u2, _)) = at(step(p, u1, 0.5 * h)) else { break };
            let Some((u3, _)) = at(step(p, u2, 0.5 * h)) else { break };
            let Some((u4, _)) = at(step(p, u3, h)) else { break };
            p = std::array::from_fn(|k| p[k] + h / 6.0 * (u1[k] + 2.0 * u2[k] + 2.0 * u3[k] + u4[k]));
        }
        out
    };
    let mut lines = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let back = follow(*seed, -1.0);
        let ahead = follow(*seed, 1.0);
        let mut line = Streamline::default();
        for (p, s) in back.iter().rev().chain(ahead.iter().skip(1)) {
            line.points.push(*p);
            line.speeds.push(*s);
        }
        if line.points.len() >= 2 {
            lines.push(line);
        }
    }
    lines
}

/// About `count` places to start streamlines: spread over a plane near where the flow comes in, and
/// where that finds too few (a bent pipe, say), among the fast-moving nodes.
pub fn default_seeds(mesh: &ResultMesh, velocity: &Field, count: usize) -> Vec<[f64; 3]> {
    if velocity.stride() != 3 || mesh.tets.is_empty() || count == 0 {
        return Vec::new();
    }
    let used: std::collections::BTreeSet<u32> = mesh.tets.iter().flatten().copied().collect();
    let peak = velocity.range().map_or(0.0, |r| f64::from(r.1));
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    let mut mean = [0.0; 3];
    for n in &used {
        let p = mesh.nodes[*n as usize];
        let v = velocity.at(*n as usize);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
            mean[k] += f64::from(v[k]);
        }
    }
    let axis = (0..3).max_by(|a, b| mean[*a].abs().total_cmp(&mean[*b].abs())).unwrap_or(0);
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let along = if mean[axis] >= 0.0 { lo[axis] + 0.08 * (hi[axis] - lo[axis]) } else { hi[axis] - 0.08 * (hi[axis] - lo[axis]) };
    let locator = Locator::new(mesh);
    let fast = |p: [f64; 3]| -> bool {
        locator.locate(p).is_some_and(|(tet, w)| {
            let mut s = [0.0; 3];
            for (node, weight) in mesh.tets[tet].iter().zip(w) {
                let c = velocity.at(*node as usize);
                for k in 0..3 {
                    s[k] += weight * f64::from(c[k]);
                }
            }
            dot(s, s).sqrt() > 0.1 * peak
        })
    };
    let side = ((count as f64 * 2.0).sqrt().ceil() as usize).max(2);
    let mut seeds = Vec::new();
    for i in 0..side {
        for j in 0..side {
            let mut p = [0.0; 3];
            p[axis] = along;
            p[u] = lo[u] + (hi[u] - lo[u]) * (i as f64 + 0.5) / side as f64;
            p[v] = lo[v] + (hi[v] - lo[v]) * (j as f64 + 0.5) / side as f64;
            if fast(p) {
                seeds.push(p);
            }
        }
    }
    if seeds.len() < count / 2 {
        let quick: Vec<[f64; 3]> = used
            .iter()
            .filter(|n| {
                let c = velocity.at(**n as usize);
                f64::from(c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt() > 0.3 * peak
            })
            .map(|n| mesh.nodes[*n as usize])
            .collect();
        let stride = (quick.len() / count.max(1)).max(1);
        seeds.extend(quick.iter().step_by(stride).copied());
    }
    if seeds.len() > count {
        let stride = seeds.len() as f64 / count as f64;
        seeds = (0..count).map(|i| seeds[(i as f64 * stride) as usize]).collect();
    }
    seeds
}

/// Adds a surface inside a volume to what's drawn, with every node field read on it. For a body whose
/// outline isn't part of the mesh's outside: it's as walls, before any cut. A point that's outside every
/// tetrahedron takes the nearest node's values.
pub fn add_surface(set: &mut crate::results::ResultSet, positions: &[[f64; 3]], triangles: &[[u32; 3]]) {
    let first = set.mesh.nodes.len() as u32;
    let weights: Vec<Vec<(u32, f64)>> = {
        let locator = Locator::new(&set.mesh);
        positions
            .iter()
            .map(|p| match locator.locate(*p) {
                Some((tet, w)) => set.mesh.tets[tet].iter().copied().zip(w).collect(),
                None => {
                    let nearest = set.mesh.nodes.iter().enumerate().min_by(|(_, a), (_, b)| dot(sub(**a, *p), sub(**a, *p)).total_cmp(&dot(sub(**b, *p), sub(**b, *p))));
                    nearest.map_or_else(Vec::new, |(i, _)| vec![(i as u32, 1.0)])
                }
            })
            .collect()
    };
    for step in &mut set.steps {
        step.fields.retain(|f| f.location == crate::results::Location::Node);
        for f in &mut step.fields {
            let s = f.stride();
            let mut data = f.data.to_vec();
            for w in &weights {
                for k in 0..s {
                    data.push(w.iter().map(|(n, weight)| f.data[*n as usize * s + k] * *weight as f32).sum());
                }
            }
            f.data = std::sync::Arc::from(data);
        }
    }
    set.mesh.nodes.extend_from_slice(positions);
    set.mesh.surface.extend(triangles.iter().map(|t| t.map(|n| n + first)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::Location;
    use std::sync::Arc;

    /// A grid of tetrahedra on `[-1, 1]^3`, `n` nodes a side.
    fn volume(n: u32) -> ResultMesh {
        let mut nodes = Vec::new();
        let at = |i: u32| -1.0 + 2.0 * f64::from(i) / f64::from(n - 1);
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    nodes.push([at(x), at(y), at(z)]);
                }
            }
        }
        ResultMesh { nodes, tets: grid_tets([n, n, n], |_, _, _| true), surface: Vec::new(), cut_start: None }
    }

    fn vector(mesh: &ResultMesh, f: impl Fn([f64; 3]) -> [f64; 3]) -> Field {
        let data: Vec<f32> = mesh.nodes.iter().flat_map(|p| f(*p).map(|v| v as f32)).collect();
        Field { name: "U".into(), unit: "m/s".into(), location: Location::Node, components: vec!["x".into(), "y".into(), "z".into()], data: Arc::from(data) }
    }

    #[test]
    fn grid_tetrahedra_fill_the_grid_exactly_and_all_one_way_up() {
        let m = volume(5);
        assert_eq!(m.tets.len(), 4 * 4 * 4 * 6);
        let mut total = 0.0;
        for t in &m.tets {
            let n = |i: usize| m.nodes[t[i] as usize];
            let v = dot(sub(n(1), n(0)), cross(sub(n(2), n(0)), sub(n(3), n(0)))) / 6.0;
            total += v.abs();
        }
        assert!((total - 8.0).abs() < 1e-9, "{total}");
    }

    #[test]
    fn a_plane_cut_has_the_area_of_the_section_at_any_angle() {
        let m = volume(9);
        let axis = slice(&m, &plane_distance(&m, [0.13, 0.0, 0.0], [1.0, 0.0, 0.0]), 0.0);
        assert!((axis.area() - 4.0).abs() < 1e-9, "{}", axis.area());
        // A plane at 45 degrees through the middle: its section is 2 by 2 times the square root of 2.
        let r = 0.5f64.sqrt();
        let tilted = slice(&m, &plane_distance(&m, [0.0; 3], [r, r, 0.0]), 0.0);
        assert!((tilted.area() - 4.0 * 2.0f64.sqrt()).abs() < 1e-9, "{}", tilted.area());
    }

    #[test]
    fn an_isosurface_of_a_sphere_has_a_spheres_area_with_fields_read_on_it() {
        let m = volume(41);
        let r: Vec<f64> = m.nodes.iter().map(|p| dot(*p, *p).sqrt()).collect();
        let s = slice(&m, &r, 0.7);
        let exact = 4.0 * std::f64::consts::PI * 0.49;
        assert!((s.area() - exact).abs() / exact < 0.01, "{} against {exact}", s.area());
        let values: Vec<f32> = r.iter().map(|v| *v as f32).collect();
        let read = s.sample(&values);
        assert_eq!(read.len(), s.points.len());
        // Linear interpolation of a curved field, so close and not exact, but the surface is at 0.7.
        assert!(read.iter().all(|v| (v - 0.7).abs() < 1e-5), "the field reads its own level");
        for p in &s.points {
            assert!((dot(*p, *p).sqrt() - 0.7).abs() < 0.01);
        }
    }

    #[test]
    fn bad_values_are_left_out() {
        let m = volume(3);
        let mut d = plane_distance(&m, [0.1, 0.0, 0.0], [1.0, 0.0, 0.0]);
        let whole = slice(&m, &d, 0.0).triangles.len();
        d[13] = f64::NAN;
        let holed = slice(&m, &d, 0.0);
        assert!(holed.triangles.len() < whole);
        assert!(holed.points.iter().all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn the_locator_finds_a_point_and_its_weights_rebuild_it() {
        let m = volume(7);
        let l = Locator::new(&m);
        let p = [0.311, -0.417, 0.726];
        let (t, w) = l.locate(p).unwrap();
        let mut back = [0.0; 3];
        for (n, weight) in m.tets[t].iter().zip(w) {
            for k in 0..3 {
                back[k] += weight * m.nodes[*n as usize][k];
            }
        }
        assert!(p.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-12));
        assert!(l.locate([1.5, 0.0, 0.0]).is_none());
    }

    #[test]
    fn a_uniform_flow_gives_a_straight_line_right_through() {
        let m = volume(9);
        let u = vector(&m, |_| [2.0, 0.0, 0.0]);
        let lines = streamlines(&m, &u, &[[0.1, 0.2, -0.3]], 4000);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert!(l.points.iter().all(|p| (p[1] - 0.2).abs() < 1e-5 && (p[2] + 0.3).abs() < 1e-5));
        let (a, b) = (l.points[0][0], l.points.last().unwrap()[0]);
        assert!(a < -0.9 && b > 0.9, "from {a} to {b}");
        assert!((l.speeds[0] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn a_rigid_rotation_stays_on_its_circle() {
        let m = volume(21);
        let u = vector(&m, |p| [-p[1], p[0], 0.0]);
        let lines = streamlines(&m, &u, &[[0.6, 0.0, 0.1]], 3000);
        assert_eq!(lines.len(), 1);
        for p in &lines[0].points {
            let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
            assert!((r - 0.6).abs() < 0.006, "radius {r}");
            assert!((p[2] - 0.1).abs() < 1e-6);
        }
    }

    #[test]
    fn seeds_start_upstream_and_in_the_flow() {
        let m = volume(11);
        let u = vector(&m, |_| [0.0, 0.0, -1.0]);
        let seeds = default_seeds(&m, &u, 16);
        assert!(!seeds.is_empty() && seeds.len() <= 16, "{}", seeds.len());
        assert!(seeds.iter().all(|p| p[2] > 0.8), "the flow comes down from high z: {seeds:?}");
    }

    #[test]
    fn a_surface_inside_reads_the_fields_at_its_points() {
        use crate::results::{ResultSet, ResultStep, StepKind};
        let m = volume(5);
        let x: Vec<f32> = m.nodes.iter().map(|p| p[0] as f32).collect();
        let f = Field { name: "x".into(), unit: String::new(), location: Location::Node, components: vec!["x".into()], data: Arc::from(x) };
        let mut set = ResultSet { mesh: m, steps: vec![ResultStep { kind: StepKind::Time, value: 0.0, fields: vec![f] }], plots: vec![] };
        let before = set.mesh.nodes.len();
        add_surface(&mut set, &[[0.25, 0.1, 0.1], [0.5, 0.1, 0.1], [0.5, 0.3, 0.1], [9.0, 0.0, 0.0]], &[[0, 1, 2]]);
        assert_eq!(set.mesh.surface, [[before as u32, before as u32 + 1, before as u32 + 2]]);
        let got = set.steps[0].field("x").unwrap();
        assert_eq!(got.len(), set.mesh.nodes.len());
        assert!((got.at(before)[0] - 0.25).abs() < 1e-5 && (got.at(before + 1)[0] - 0.5).abs() < 1e-5, "interpolated");
        assert!((got.at(before + 3)[0] - 1.0).abs() < 1e-5, "outside: the nearest node's value");
    }

    #[test]
    fn a_still_field_has_no_streamlines() {
        let m = volume(5);
        let u = vector(&m, |_| [0.0; 3]);
        assert!(streamlines(&m, &u, &[[0.0; 3]], 100).is_empty());
        assert!(default_seeds(&m, &u, 8).is_empty());
    }
}
