//! What's done to a body's edges and walls: fillet, chamfer, shell, thicken. A fillet is a
//! shaped tool run along the edge and then cut away or added, so it leans on the same
//! boolean as everything else and comes out just as closed.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::boolean::{self, Op};
use super::mesh::{FaceId, Mesh};
use super::prims::fill_region;
use crate::geometry::{P3, add, cross, dot, norm, scale, sub, unit};

/// One run of mesh edges between two faces, in order.
struct Chain {
    pts: Vec<P3>,
    closed: bool,
    /// For each point: the two faces' normals there, and which way each face runs from the edge.
    normals: Vec<(P3, P3)>,
    away: Vec<(P3, P3)>,
}

/// The chains of edges between two named faces.
fn edge_chains(m: &Mesh, faces: [FaceId; 2]) -> Vec<Chain> {
    // Each edge between them, with the triangle on each side.
    let mut by_edge: BTreeMap<(u32, u32), (Option<usize>, Option<usize>)> = BTreeMap::new();
    for (t, tri) in m.tris.iter().enumerate() {
        let f = m.face[t];
        if f != faces[0] && f != faces[1] {
            continue;
        }
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let e = by_edge.entry((a.min(b), a.max(b))).or_default();
            if f == faces[0] {
                e.0 = Some(t);
            } else {
                e.1 = Some(t);
            }
        }
    }
    let shared: Vec<((u32, u32), usize, usize)> = by_edge.into_iter().filter_map(|(k, (a, b))| Some((k, a?, b?))).collect();
    // Normals and directions averaged over the edges at each vertex.
    let mut at: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, (k, _, _)) in shared.iter().enumerate() {
        at.entry(k.0).or_default().push(i);
        at.entry(k.1).or_default().push(i);
    }
    let mut used = vec![false; shared.len()];
    let mut out = Vec::new();
    let walk = |start: u32, first: usize, used: &mut Vec<bool>| -> (Vec<u32>, Vec<usize>, bool) {
        let (mut verts, mut edges, mut v, mut e) = (vec![start], Vec::new(), start, first);
        loop {
            used[e] = true;
            edges.push(e);
            let (a, b) = shared[e].0;
            v = if a == v { b } else { a };
            if v == start {
                return (verts, edges, true);
            }
            verts.push(v);
            match at[&v].iter().copied().find(|x| !used[*x]) {
                Some(next) => e = next,
                None => return (verts, edges, false),
            }
        }
    };
    let mut starts: Vec<u32> = at.iter().filter(|(_, l)| l.len() == 1).map(|(v, _)| *v).collect();
    starts.sort_unstable();
    let mut runs: Vec<(Vec<u32>, Vec<usize>, bool)> = Vec::new();
    for s in starts {
        let first = at[&s][0];
        if !used[first] {
            runs.push(walk(s, first, &mut used));
        }
    }
    for i in 0..shared.len() {
        if !used[i] {
            runs.push(walk(shared[i].0.0, i, &mut used));
        }
    }
    for (verts, edges, closed) in runs {
        let pts: Vec<P3> = verts.iter().map(|v| m.verts[*v as usize]).collect();
        let n = pts.len();
        let info = |e: usize, tri: usize| -> (P3, P3) {
            let normal = m.normal(tri).unwrap_or([0.0, 0.0, 1.0]);
            let (a, b) = shared[e].0;
            let (pa, pb) = (m.verts[a as usize], m.verts[b as usize]);
            let along = unit(sub(pb, pa));
            let third = m.tris[tri].iter().copied().find(|v| *v != a && *v != b).unwrap_or(a);
            let raw = sub(m.verts[third as usize], pa);
            let away = unit(sub(raw, scale(along, dot(raw, along))));
            (normal, away)
        };
        let per_edge: Vec<((P3, P3), (P3, P3))> = edges.iter().map(|e| (info(*e, shared[*e].1), info(*e, shared[*e].2))).collect();
        let mut normals = Vec::with_capacity(n);
        let mut away = Vec::with_capacity(n);
        for i in 0..n {
            let (before, after) = if closed { (per_edge[(i + per_edge.len() - 1) % per_edge.len()], per_edge[i % per_edge.len()]) } else { (per_edge[i.saturating_sub(1).min(per_edge.len() - 1)], per_edge[i.min(per_edge.len() - 1)]) };
            normals.push((unit(add(before.0.0, after.0.0)), unit(add(before.1.0, after.1.0))));
            away.push((unit(add(before.0.1, after.0.1)), unit(add(before.1.1, after.1.1))));
        }
        out.push(Chain { pts, closed, normals, away });
    }
    out
}

/// Rounds (or with `chamfer`, bevels) the edges between two faces. `tag` names the new surface.
pub fn blend(m: &Mesh, faces: [FaceId; 2], r: f64, chamfer: bool, facets: usize, tag: FaceId) -> Result<Mesh> {
    let chains = edge_chains(m, faces);
    if chains.is_empty() {
        bail!("Those two faces don't share an edge any more.");
    }
    let mut out = m.clone();
    for c in &chains {
        // Convex where one face's direction runs below the other's surface.
        let convex = dot(c.normals[0].0, c.away[0].1) < 0.0;
        let tool = edge_tool(c, r, chamfer, convex, facets, tag)?;
        out = boolean::boolean(&out, &tool, if convex { Op::Difference } else { Op::Union })?;
    }
    if out.is_empty() {
        bail!("That radius takes the whole body away.");
    }
    Ok(out)
}

/// The shape to remove from a convex edge or add to a concave one: the corner past the rounding.
fn edge_tool(c: &Chain, r: f64, chamfer: bool, convex: bool, facets: usize, tag: FaceId) -> Result<Mesh> {
    let n = c.pts.len();
    // A little past each surface, so the tool cuts through it cleanly rather than grazing it.
    let over = (r * 0.5).max(1e-6);
    let sign = if convex { 1.0 } else { -1.0 };
    let rim: Vec<Vec<P3>> = (0..n)
        .map(|i| -> Result<Vec<P3>> {
            let (d1, d2) = c.away[i];
            let (n1, n2) = c.normals[i];
            let p = c.pts[i];
            let cos = dot(d1, d2).clamp(-1.0, 1.0);
            let open = cos.acos();
            if open < 0.02 || open > std::f64::consts::PI - 0.02 {
                bail!("These two faces meet too nearly flat, or too sharply, to round.");
            }
            let half = open / 2.0;
            let reach = r / half.tan();
            let (a, b) = (add(p, scale(d1, reach)), add(p, scale(d2, reach)));
            let mut ring = Vec::new();
            // Starting outside the first face, along the rounding, out through the second, and
            // round the far side of the corner.
            ring.push(add(a, scale(n1, sign * over)));
            ring.push(a);
            if chamfer {
                ring.push(b);
            } else {
                let bis = unit(add(d1, d2));
                let centre = add(p, scale(bis, r / half.sin()));
                let (u, v) = (sub(a, centre), sub(b, centre));
                let steps = (((std::f64::consts::PI - open) / std::f64::consts::TAU * facets as f64).ceil() as usize).max(2);
                let total = dot(unit(u), unit(v)).clamp(-1.0, 1.0).acos();
                let axis = unit(cross(u, v));
                for k in 1..steps {
                    let t = total * k as f64 / steps as f64;
                    let turned = add(add(scale(u, t.cos()), scale(cross(axis, u), t.sin())), scale(axis, dot(axis, u) * (1.0 - t.cos())));
                    ring.push(add(centre, turned));
                }
                ring.push(b);
            }
            ring.push(add(b, scale(n2, sign * over)));
            ring.push(add(p, scale(add(n1, n2), sign * over)));
            Ok(ring)
        })
        .collect::<Result<_>>()?;
    // An open run being cut away overshoots its ends, so it doesn't leave a sliver at either. Being
    // added, it stops flush with them: anything past them would be new metal hanging in space.
    let mut rows = rim;
    let reach = (r * 0.6).max(1e-6);
    if !c.closed && convex {
        let t0 = unit(sub(c.pts[0], c.pts[1]));
        let t1 = unit(sub(c.pts[n - 1], c.pts[n - 2]));
        let first: Vec<P3> = rows[0].iter().map(|q| add(*q, scale(t0, reach))).collect();
        let last: Vec<P3> = rows[n - 1].iter().map(|q| add(*q, scale(t1, reach))).collect();
        rows.insert(0, first);
        rows.push(last);
    }
    let width = rows[0].len();
    let count = rows.len();
    let spans = if c.closed { count } else { count - 1 };
    let mut tool = Mesh::default();
    for s in 0..spans {
        let (x, y) = (&rows[s], &rows[(s + 1) % count]);
        for k in 0..width {
            let (a, b) = (x[k], x[(k + 1) % width]);
            let (d, e) = (y[k], y[(k + 1) % width]);
            tool.push_quad(a, b, e, d, tag);
        }
    }
    if !c.closed {
        // The first row's cap faces back along the run and the last row's forward, whichever way
        // round the ring happens to go: the walls come out one way or the other, and these match.
        for (row, flip) in [(&rows[0], true), (&rows[count - 1], false)] {
            cap_row(&mut tool, row, flip, tag);
        }
    }
    let w = tool.welded(1e-10);
    if !w.is_closed() {
        bail!("The rounding tool for this edge didn't close.");
    }
    Ok(if w.volume() < 0.0 { w.flipped() } else { w })
}

/// Closes the end of a tool: a flat ring of points in space.
fn cap_row(m: &mut Mesh, row: &[P3], flip: bool, face: FaceId) {
    let o = row[0];
    let mut n = [0.0; 3];
    for i in 1..row.len() - 1 {
        n = add(n, cross(sub(row[i], o), sub(row[i + 1], o)));
    }
    let n = unit(n);
    let u = unit(sub(row[1], o));
    let v = cross(n, u);
    let flat: Vec<[f64; 2]> = row.iter().map(|p| [dot(sub(*p, o), u), dot(sub(*p, o), v)]).collect();
    let (pts, tris) = fill_region(&[flat.as_slice()]);
    let lift = |q: [f64; 2]| add(o, add(scale(u, q[0]), scale(v, q[1])));
    for t in tris {
        let (a, b, c) = (lift(pts[t[0]]), lift(pts[t[1]]), lift(pts[t[2]]));
        if flip { m.push_tri(a, c, b, face) } else { m.push_tri(a, b, c, face) }
    }
}

/// Each vertex moved so the planes of its faces each move by what `offset` says for that face.
fn offset_verts(m: &Mesh, offset: impl Fn(FaceId) -> f64) -> Vec<P3> {
    // Each vertex's planes, with duplicates merged: three planes place it exactly.
    let mut planes: Vec<Vec<(P3, f64)>> = vec![Vec::new(); m.verts.len()];
    for t in 0..m.tris.len() {
        let Some(n) = m.normal(t) else { continue };
        let d = offset(m.face[t]);
        for &v in &m.tris[t] {
            let list = &mut planes[v as usize];
            if !list.iter().any(|(o, od)| dot(*o, n) > 0.99999 && (od - d).abs() < 1e-15) {
                list.push((n, d));
            }
        }
    }
    (0..m.verts.len())
        .map(|i| {
            let p = m.verts[i];
            let list = &planes[i];
            if list.is_empty() {
                return p;
            }
            // Least squares, slightly damped: with fewer than three planes the smallest move does it.
            let mut a = [[0.0; 3]; 3];
            let mut b = [0.0; 3];
            for (n, d) in list {
                for r in 0..3 {
                    for c in 0..3 {
                        a[r][c] += n[r] * n[c];
                    }
                    b[r] += n[r] * d;
                }
            }
            for (k, row) in a.iter_mut().enumerate() {
                row[k] += 1e-9;
            }
            add(p, solve3(a, b))
        })
        .collect()
}

/// Solves by elimination with the largest pivot each time. The system is nearly singular whenever a
/// vertex has fewer than three planes, which is the usual case and why it's damped, and a formula
/// through determinants loses every digit there: it slid a vertex on a tilted sheet eight hundred
/// times the offset it was asked for.
fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> P3 {
    for i in 0..3 {
        let p = (i..3).max_by(|x, y| a[*x][i].abs().total_cmp(&a[*y][i].abs())).unwrap_or(i);
        a.swap(i, p);
        b.swap(i, p);
        for r in i + 1..3 {
            let k = a[r][i] / a[i][i];
            for c in i..3 {
                a[r][c] -= k * a[i][c];
            }
            b[r] -= k * b[i];
        }
    }
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let mut v = b[i];
        for c in i + 1..3 {
            v -= a[i][c] * x[c];
        }
        x[i] = v / a[i][i];
    }
    x
}

/// Hollows a solid: everything inside walls `t` thick is cut away. The open faces' planes move
/// outward instead of inward, so the cut goes clean through them.
pub fn shell(m: &Mesh, open: &[FaceId], t: f64) -> Result<Mesh> {
    let (lo, hi) = m.bounds();
    let size = norm(sub(hi, lo));
    if t * 2.0 >= (0..3).map(|k| hi[k] - lo[k]).fold(f64::MAX, f64::min) && open.is_empty() {
        bail!("Walls that thick leave no room inside.");
    }
    let through = (size * 0.05).max(t);
    let inner_verts = offset_verts(m, |f| if open.contains(&f) { through } else { -t });
    let inner = Mesh { verts: inner_verts, tris: m.tris.clone(), face: m.face.clone() };
    if inner.volume() <= 0.0 {
        bail!("Walls that thick leave no room inside.");
    }
    let out = boolean::boolean(m, &inner, Op::Difference).map_err(|e| anyhow::anyhow!("Hollowing it didn't work: the walls may be too thick for its smallest parts. ({e})"))?;
    if out.is_empty() {
        bail!("Walls that thick leave nothing.");
    }
    Ok(out)
}

/// A surface given a thickness: a copy each side, joined round the rim.
pub fn thicken(m: &Mesh, t: f64, feature: u32) -> Mesh {
    use super::mesh::face_tag;
    let up = offset_verts(m, |_| t / 2.0);
    let down = offset_verts(m, |_| -t / 2.0);
    let (top, bottom, wall) = (face_tag(feature, 0), face_tag(feature, 1), face_tag(feature, 2));
    let mut out = Mesh::default();
    let mut directed: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for tri in &m.tris {
        let c = tri.map(|v| up[v as usize]);
        out.push_tri(c[0], c[1], c[2], top);
        let c = tri.map(|v| down[v as usize]);
        out.push_tri(c[0], c[2], c[1], bottom);
        for k in 0..3 {
            *directed.entry((tri[k], tri[(k + 1) % 3])).or_default() += 1;
        }
    }
    for (&(a, b), _) in directed.iter().filter(|(k, _)| !directed.contains_key(&(k.1, k.0))) {
        let (a, b) = (a as usize, b as usize);
        out.push_quad(down[a], down[b], up[b], up[a], wall);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::mesh::face_tag;
    use crate::modeler::prims::{self, END_HIGH};

    /// Which face of a box: the two ends, then its four sides.
    fn side(feature: u32, k: u32) -> FaceId {
        face_tag(feature, 2 + k)
    }

    fn block() -> Mesh {
        prims::cuboid([0.0; 3], [0.02, 0.03, 0.04], 1).welded(1e-10)
    }

    #[test]
    fn a_fillet_takes_off_what_it_should() {
        let m = block();
        let out = blend(&m, [face_tag(1, END_HIGH), side(1, 1)], 0.005, false, 64, 999).unwrap();
        assert!(out.is_closed(), "{} open", out.open_edges());
        // A square corner of 25 mm² becomes a quarter circle: (1 - π/4) r² gone, along 30 mm.
        let gone = (1.0 - std::f64::consts::FRAC_PI_4) * 25e-6 * 0.03;
        let v = 0.02 * 0.03 * 0.04 - gone;
        assert!((out.volume() - v).abs() / gone < 0.03, "{} vs {v}", out.volume());
        assert!(out.has_face(999), "the rounding has a name");
        assert!(out.has_face(face_tag(1, END_HIGH)), "the faces it ran between are still there");
    }

    #[test]
    fn a_chamfer_takes_off_a_triangle() {
        let m = block();
        let out = blend(&m, [face_tag(1, END_HIGH), side(1, 1)], 0.005, true, 64, 999).unwrap();
        assert!(out.is_closed());
        let gone = 0.5 * 25e-6 * 0.03;
        let v = 0.02 * 0.03 * 0.04 - gone;
        assert!((out.volume() - v).abs() / gone < 1e-4, "{} vs {v}", out.volume());
    }

    #[test]
    fn the_rim_of_a_cylinder_rounds_all_the_way_round() {
        let m = prims::cone([0.0; 3], [0.0, 0.0, 0.03], 0.01, 0.01, 64, 1).welded(1e-10);
        let out = blend(&m, [face_tag(1, END_HIGH), side(1, 0)], 0.002, false, 64, 999).unwrap();
        assert!(out.is_closed(), "{} open", out.open_edges());
        // Pappus: the corner's area times how far its centroid goes.
        let area = (1.0 - std::f64::consts::FRAC_PI_4) * 4e-6;
        let at = 0.01 - 0.002 * (1.0 - 2.0 / (3.0 * (4.0 - std::f64::consts::PI)) * 1.0);
        let gone = area * std::f64::consts::TAU * at;
        assert!((m.volume() - out.volume() - gone).abs() / gone < 0.08, "{} vs {gone}", m.volume() - out.volume());
    }

    #[test]
    fn an_inside_corner_fills_in() {
        // A step: a block on a block, whose inside corner the fillet adds to.
        let base = prims::cuboid([0.0; 3], [0.04, 0.02, 0.01], 1).welded(1e-10);
        let top = prims::cuboid([0.0, 0.0, 0.01], [0.02, 0.02, 0.03], 2).welded(1e-10);
        let m = boolean::boolean(&base, &top, Op::Union).unwrap();
        // The base's top end meets the upper block's side at x = 20 mm.
        let out = blend(&m, [face_tag(1, END_HIGH), side(2, 1)], 0.004, false, 64, 999).unwrap();
        assert!(out.is_closed(), "{} open", out.open_edges());
        let added = (1.0 - std::f64::consts::FRAC_PI_4) * 16e-6 * 0.02;
        assert!((out.volume() - m.volume() - added).abs() / added < 0.05, "{} vs {added}", out.volume() - m.volume());
    }

    #[test]
    fn faces_that_dont_meet_are_refused() {
        let m = block();
        let e = blend(&m, [face_tag(1, 0), face_tag(1, END_HIGH)], 0.001, false, 32, 9).err().unwrap();
        assert!(e.to_string().contains("share an edge"), "{e}");
    }

    #[test]
    fn a_shell_of_a_box_has_the_walls_it_should() {
        let m = block();
        let out = shell(&m, &[face_tag(1, END_HIGH)], 0.002).unwrap();
        assert!(out.is_closed());
        let inside = 0.016 * 0.026 * 0.038;
        assert!((out.volume() - (m.volume() - inside)).abs() / inside < 1e-6, "{}", out.volume());
        // With nothing open it's a sealed cavity.
        let sealed = shell(&m, &[], 0.002).unwrap();
        let cavity = 0.016 * 0.026 * 0.036;
        assert!((sealed.volume() - (m.volume() - cavity)).abs() / cavity < 1e-6);
        assert!(shell(&m, &[], 0.02).is_err(), "too thick to have an inside");
    }

    #[test]
    fn a_cylinder_shells_too() {
        let m = prims::cone([0.0; 3], [0.0, 0.0, 0.04], 0.01, 0.01, 48, 1).welded(1e-10);
        let out = shell(&m, &[face_tag(1, END_HIGH)], 0.001).unwrap();
        assert!(out.is_closed());
        assert!(out.volume() > 0.0 && out.volume() < m.volume() * 0.5);
    }

    /// At any tilt, a vertex moves straight off its surface by the offset and no further.
    #[test]
    fn a_sheet_tilted_any_way_thickens_straight_off_itself() {
        use crate::modeler::mesh::Xform;
        let mut flat = Mesh::default();
        flat.push_quad([0.0, 0.0, 0.0], [0.02, 0.0, 0.0], [0.02, 0.01, 0.0], [0.0, 0.01, 0.0], 1);
        let flat = flat.welded(1e-10);
        let mut seed = 7_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64) / (1u64 << 31) as f64
        };
        for _ in 0..200 {
            let turn = Xform::rotate([0.0; 3], [next() - 0.5, next() - 0.5, next() - 0.5 + 1e-3], next() * 6.0);
            let sheet = flat.transformed(&turn);
            let out = thicken(&sheet, 0.001, 7).welded(1e-10);
            assert!(out.is_closed(), "{} open", out.open_edges());
            let exact = sheet.area() * 0.001;
            assert!((out.volume().abs() - exact).abs() / exact < 1e-4, "{} vs {exact}", out.volume().abs());
            let n = sheet.normal(0).unwrap();
            for p in &sheet.verts {
                assert!(out.verts.iter().any(|q| norm(sub(*q, add(*p, scale(n, 0.0005)))) < 1e-8), "a corner slid sideways");
            }
        }
    }

    #[test]
    fn a_thickened_sheet_is_a_closed_plate() {
        let mut s = Mesh::default();
        s.push_quad([0.0, 0.0, 0.0], [0.02, 0.0, 0.0], [0.02, 0.01, 0.0], [0.0, 0.01, 0.0], 1);
        s.push_quad([0.02, 0.0, 0.0], [0.02, 0.0, 0.01], [0.02, 0.01, 0.01], [0.02, 0.01, 0.0], 2);
        let s = s.welded(1e-10);
        let out = thicken(&s, 0.001, 7).welded(1e-10);
        assert!(out.is_closed(), "{} open", out.open_edges());
        assert!((out.volume().abs() - s.area() * 0.001).abs() / (s.area() * 0.001) < 0.03, "{}", out.volume());
    }
}
