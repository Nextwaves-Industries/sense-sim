//! A surface that comes with no faces, such as a USD asset's mesh, turned into a [`Partition`]: its
//! points welded, its triangles made to agree on which way is out, and its smooth regions grown into
//! faces that can be picked. A CAD body's faces are exact; these follow the mesh's creases, and are
//! only as meaningful as they are.

use std::collections::{HashMap, VecDeque};

use anyhow::{Result, bail};

use super::partition::{Partition, cross, dot, sub, tri_area};

/// The weld's snapping size, as a share of the mesh's diagonal.
const WELD: f64 = 1e-7;
/// How sharp a bend starts a new face, for meshes that come without any. Gmsh is told the same when it
/// splits such a surface, so that its surfaces are these faces.
pub const CREASE_DEGREES: f64 = 30.0;

/// What came of making a partition from a bare mesh.
#[derive(Debug, Clone)]
pub struct Regions {
    pub partition: Partition,
    /// Separate connected pieces of surface. FEA needs exactly one closed one.
    pub shells: usize,
    /// Triangles dropped for having no area.
    pub degenerate: usize,
}

impl Regions {
    /// Why this surface can't be meshed as a solid, if it can't. In the user's words.
    pub fn solid_problem(&self) -> Option<String> {
        if !self.partition.is_closed() {
            return Some(format!("{} isn't a closed surface, so it has no inside to mesh. It can still run on the voxel solvers.", self.partition.name));
        }
        if self.shells != 1 {
            return Some(format!("{} is made of {} separate shells, and a structural mesh needs one closed solid. It can still run on the voxel solvers.", self.partition.name, self.shells));
        }
        None
    }
}

/// Builds a partition from triangles, starting a new face wherever neighbours meet at more than
/// `crease_degrees`.
pub fn from_triangles(name: &str, positions: &[[f64; 3]], triangles: &[[u32; 3]], crease_degrees: f64) -> Result<Regions> {
    if triangles.is_empty() || positions.is_empty() {
        bail!("{name} has no triangles");
    }
    // Weld, by snapping to a fine lattice and looking in the neighbouring lattice cells.
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let diagonal = dot(sub(hi, lo), sub(hi, lo)).sqrt();
    if diagonal <= 0.0 || !diagonal.is_finite() {
        bail!("{name} has no size");
    }
    let tolerance = diagonal * WELD;
    let mut lattice: HashMap<[i64; 3], u32> = HashMap::with_capacity(positions.len());
    let mut welded: Vec<[f64; 3]> = Vec::with_capacity(positions.len());
    let mut remap: Vec<u32> = Vec::with_capacity(positions.len());
    for p in positions {
        let cell: [i64; 3] = std::array::from_fn(|k| ((p[k] - lo[k]) / tolerance).round() as i64);
        let mut found = None;
        'search: for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(i) = lattice.get(&[cell[0] + dx, cell[1] + dy, cell[2] + dz]) {
                        found = Some(*i);
                        break 'search;
                    }
                }
            }
        }
        let id = found.unwrap_or_else(|| {
            welded.push(*p);
            let id = (welded.len() - 1) as u32;
            lattice.insert(cell, id);
            id
        });
        remap.push(id);
    }

    // Triangles that still have an area.
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(triangles.len());
    let mut degenerate = 0;
    for t in triangles {
        let Some(m) = t.iter().map(|i| remap.get(*i as usize).copied()).collect::<Option<Vec<u32>>>() else { bail!("{name}: a triangle points past the end of the mesh") };
        let [a, b, c] = [m[0], m[1], m[2]];
        if a == b || b == c || a == c || tri_area(welded[a as usize], welded[b as usize], welded[c as usize]) <= diagonal * diagonal * 1e-16 {
            degenerate += 1;
            continue;
        }
        tris.push([a, b, c]);
    }
    if tris.is_empty() {
        bail!("{name} has no triangles with any area");
    }

    // Who shares each edge.
    let mut edges: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(tris.len() * 3 / 2);
    for (i, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            edges.entry(if a < b { (a, b) } else { (b, a) }).or_default().push(i as u32);
        }
    }

    // USD meshes often disagree about winding: go through each shell making neighbours consistent.
    let mut shell = vec![u32::MAX; tris.len()];
    let mut shells = 0u32;
    let mut queue = VecDeque::new();
    for start in 0..tris.len() {
        if shell[start] != u32::MAX {
            continue;
        }
        shell[start] = shells;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            let t = tris[i];
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let key = if a < b { (a, b) } else { (b, a) };
                let shared = &edges[&key];
                if shared.len() != 2 {
                    continue;
                }
                let other = if shared[0] as usize == i { shared[1] } else { shared[0] } as usize;
                if shell[other] != u32::MAX {
                    continue;
                }
                shell[other] = shells;
                // Neighbours in agreement run along their shared edge in opposite directions.
                let n = tris[other];
                if [(n[0], n[1]), (n[1], n[2]), (n[2], n[0])].contains(&(a, b)) {
                    tris[other] = [n[0], n[2], n[1]];
                }
                queue.push_back(other);
            }
        }
        shells += 1;
    }
    // Each shell's outside out, by the sign of the volume it encloses.
    let mut volume = vec![0.0; shells as usize];
    for (i, t) in tris.iter().enumerate() {
        let (a, b, c) = (sub(welded[t[0] as usize], lo), sub(welded[t[1] as usize], lo), sub(welded[t[2] as usize], lo));
        volume[shell[i] as usize] += dot(a, cross(b, c)) / 6.0;
    }
    for (i, t) in tris.iter_mut().enumerate() {
        if volume[shell[i] as usize] < 0.0 {
            *t = [t[0], t[2], t[1]];
        }
    }

    // Faces, grown across edges with a small enough bend.
    let normal = |t: &[u32; 3]| {
        let n = cross(sub(welded[t[1] as usize], welded[t[0] as usize]), sub(welded[t[2] as usize], welded[t[0] as usize]));
        let l = dot(n, n).sqrt().max(1e-300);
        [n[0] / l, n[1] / l, n[2] / l]
    };
    let limit = crease_degrees.to_radians().cos();
    let mut tri_face = vec![u32::MAX; tris.len()];
    let mut faces = 0u32;
    for start in 0..tris.len() {
        if tri_face[start] != u32::MAX {
            continue;
        }
        tri_face[start] = faces;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            let t = tris[i];
            let here = normal(&t);
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let shared = &edges[&(if a < b { (a, b) } else { (b, a) })];
                if shared.len() != 2 {
                    continue;
                }
                let other = if shared[0] as usize == i { shared[1] } else { shared[0] } as usize;
                if tri_face[other] == u32::MAX && dot(here, normal(&tris[other])) >= limit {
                    tri_face[other] = faces;
                    queue.push_back(other);
                }
            }
        }
        faces += 1;
    }

    Ok(Regions { partition: Partition { name: name.to_string(), positions: welded, triangles: tris, tri_face, faces }, shells: shells as usize, degenerate })
}

/// A surface as binary STL, for Gmsh. `scale` multiplies the coordinates.
pub fn to_stl(part: &Partition, scale: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(84 + part.triangles.len() * 50);
    // Gmsh refuses a header of all zeros: "Error loading". Any text but "solid" will do.
    let mut header = [b' '; 80];
    header[..15].copy_from_slice(b"from Sense Sim ");
    out.extend_from_slice(&header);
    out.extend_from_slice(&(part.triangles.len() as u32).to_le_bytes());
    for i in 0..part.triangles.len() {
        let [a, b, c] = part.triangle(i);
        let n = cross(sub(b, a), sub(c, a));
        let l = dot(n, n).sqrt().max(1e-300);
        for v in [n[0] / l, n[1] / l, n[2] / l] {
            out.extend_from_slice(&(v as f32).to_le_bytes());
        }
        for p in [a, b, c] {
            for v in p {
                out.extend_from_slice(&((v * scale) as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box as a bare triangle soup: every triangle with its own three points, the way some USD
    /// assets come, in the winding `flip` says.
    fn soup(size: [f64; 3], flip: impl Fn(usize) -> bool) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let c = |x: usize, y: usize, z: usize| [x as f64 * size[0], y as f64 * size[1], z as f64 * size[2]];
        let quads = [
            [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)],
            [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
            [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)],
            [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
            [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)],
            [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
        ];
        let (mut p, mut t) = (Vec::new(), Vec::new());
        for (i, q) in quads.iter().enumerate() {
            for (k, tri) in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]].iter().enumerate() {
                let start = p.len() as u32;
                p.extend_from_slice(tri);
                t.push(if flip(i * 2 + k) { [start, start + 2, start + 1] } else { [start, start + 1, start + 2] });
            }
        }
        (p, t)
    }

    /// The signed volume a closed partition encloses.
    fn volume(part: &Partition) -> f64 {
        (0..part.triangles.len()).map(|i| {
            let [a, b, c] = part.triangle(i);
            dot(a, cross(b, c)) / 6.0
        }).sum()
    }

    #[test]
    fn a_triangle_soup_becomes_a_closed_box_with_six_faces() {
        let (p, t) = soup([2.0, 1.0, 0.5], |_| false);
        let r = from_triangles("box", &p, &t, 30.0).unwrap();
        assert_eq!(r.partition.positions.len(), 8, "welded");
        assert_eq!(r.partition.faces, 6);
        assert_eq!(r.shells, 1);
        assert!(r.partition.is_closed());
        assert!(r.solid_problem().is_none());
        let mut areas = r.partition.face_areas();
        areas.sort_by(f64::total_cmp);
        for (got, want) in areas.iter().zip([0.5, 0.5, 1.0, 1.0, 2.0, 2.0]) {
            assert!((got - want).abs() < 1e-12, "{areas:?}");
        }
    }

    #[test]
    fn mixed_windings_and_an_inside_out_mesh_come_out_outward() {
        let (p, t) = soup([1.0, 1.0, 1.0], |i| i % 3 == 0);
        let r = from_triangles("box", &p, &t, 30.0).unwrap();
        assert!(r.partition.is_closed(), "windings were made to agree");
        assert!((volume(&r.partition) - 1.0).abs() < 1e-12, "{}", volume(&r.partition));
        let (p, t) = soup([1.0, 1.0, 1.0], |_| true);
        let r = from_triangles("box", &p, &t, 30.0).unwrap();
        assert!((volume(&r.partition) - 1.0).abs() < 1e-12, "{}", volume(&r.partition));
    }

    #[test]
    fn a_smooth_surface_stays_one_face_and_its_rims_stay_apart() {
        // A cylinder of 48 sides: 7.5 degrees between them, under the crease limit.
        let sides = 48u32;
        let (mut p, mut t) = (vec![[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]], Vec::new());
        for i in 0..sides {
            let a = f64::from(i) * std::f64::consts::TAU / f64::from(sides);
            p.push([a.cos(), a.sin(), 0.0]);
            p.push([a.cos(), a.sin(), 1.0]);
        }
        for i in 0..sides {
            let (a, b) = (2 + 2 * i, 2 + 2 * ((i + 1) % sides));
            t.push([a, b, b + 1]);
            t.push([a, b + 1, a + 1]);
            t.push([0, b, a]);
            t.push([1, a + 1, b + 1]);
        }
        let r = from_triangles("cyl", &p, &t, 30.0).unwrap();
        assert_eq!(r.partition.faces, 3, "the side and two ends");
        assert!(r.partition.is_closed());
    }

    #[test]
    fn open_and_separate_surfaces_are_told_apart_with_a_reason() {
        let (mut p, mut t) = soup([1.0, 1.0, 1.0], |_| false);
        t.pop();
        let open = from_triangles("open", &p, &t, 30.0).unwrap();
        assert!(!open.partition.is_closed());
        assert!(open.solid_problem().unwrap().contains("isn't a closed surface"));

        // Two boxes apart.
        let (mut p2, t2) = soup([1.0, 1.0, 1.0], |_| false);
        for q in &mut p2 {
            q[0] += 5.0;
        }
        let (p1, t1) = soup([1.0, 1.0, 1.0], |_| false);
        p = p1;
        let shift = p.len() as u32;
        p.extend(p2);
        let mut both = t1;
        both.extend(t2.iter().map(|x| [x[0] + shift, x[1] + shift, x[2] + shift]));
        let two = from_triangles("two", &p, &both, 30.0).unwrap();
        assert_eq!(two.shells, 2);
        assert_eq!(two.partition.faces, 12);
        assert!(two.solid_problem().unwrap().contains("2 separate shells"));
    }

    #[test]
    fn bad_input_is_refused_and_slivers_are_counted() {
        assert!(from_triangles("none", &[], &[], 30.0).is_err());
        assert!(from_triangles("flat", &[[0.0; 3], [0.0; 3], [0.0; 3]], &[[0, 1, 2]], 30.0).is_err());
        assert!(from_triangles("past", &[[0.0; 3], [1.0, 0.0, 0.0]], &[[0, 1, 9]], 30.0).is_err());
        let r = from_triangles("t", &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [2.0, 0.0, 0.0]], &[[0, 1, 2], [0, 1, 3]], 30.0).unwrap();
        assert_eq!(r.degenerate, 1);
        assert_eq!(r.partition.triangles.len(), 1);
    }

    #[test]
    fn stl_has_the_size_its_header_says() {
        let (p, t) = soup([1.0, 1.0, 1.0], |_| false);
        let r = from_triangles("box", &p, &t, 30.0).unwrap();
        let bytes = to_stl(&r.partition, 1000.0);
        assert_eq!(bytes.len(), 84 + 12 * 50);
        assert_eq!(u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]), 12);
        assert_ne!(bytes[0], 0, "Gmsh can't read a header that starts with a zero");
        assert_ne!(&bytes[..5], b"solid", "that would say it's a text file");
    }
}
