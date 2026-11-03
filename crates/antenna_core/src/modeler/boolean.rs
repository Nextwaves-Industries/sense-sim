//! Union, difference and intersection of triangle surfaces.
//!
//! How it works, and why it stays watertight:
//!
//! 1. All vertices go into one pool that merges points closer than a tolerance, so a point
//!    computed twice from two triangles is one point.
//! 2. Every pair of triangles that can touch is intersected, giving a segment. Coplanar
//!    pairs imprint each other's edges instead.
//! 3. A segment's end on a triangle's edge is noted against the edge, not the triangle, so
//!    both triangles sharing that edge split it at exactly the same points.
//! 4. Each triangle touched is re-triangulated with its segments as constraints. It keeps
//!    its face's name, so what a face is called survives the operation.
//! 5. The pieces are grouped into patches across edges that aren't cuts. Everything in a
//!    patch is on one side of the other body, so one winding-number test settles it.
//! 6. The op picks patches. The result is checked: a solid has to come out closed, with a
//!    volume in the range the op allows. If not, it tries again at a coarser tolerance, and
//!    fails with a reason rather than handing back a broken surface.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use spade::handles::FixedVertexHandle;
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

use super::mesh::{Bvh, FaceId, Frame, Mesh, Pool, closest_on_triangle, winding_number};
use crate::geometry::{P3, add, cross, dot, norm, scale, sub, unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Union,
    Difference,
    Intersection,
    /// What's in one or the other, but not both.
    Exclusion,
}

/// The tolerance two surfaces start at, in proportion to the space they take up.
fn base_tolerance(a: &Mesh, b: &Mesh) -> f64 {
    let (lo_a, hi_a) = a.bounds();
    let (lo_b, hi_b) = b.bounds();
    let size = (0..3).map(|k| hi_a[k].max(hi_b[k]) - lo_a[k].min(lo_b[k])).fold(0.0, f64::max);
    (size * 1e-8).max(1e-10)
}

/// What's asked for with both surfaces as they stand: retried at coarser tolerances, since
/// nearly touching surfaces can come out cleanly at one and not another.
pub fn boolean(a: &Mesh, b: &Mesh, op: Op) -> Result<Mesh> {
    if a.is_empty() || b.is_empty() {
        return match op {
            Op::Union | Op::Exclusion => Ok(if a.is_empty() { b.clone() } else { a.clone() }),
            Op::Difference if !a.is_empty() => Ok(a.clone()),
            _ => bail!("One of the bodies is empty, so nothing is left."),
        };
    }
    let (a_closed, b_closed) = (a.is_closed(), b.is_closed());
    let base = base_tolerance(a, b);
    // Two sheets on one plane have an inside after all: each other's area.
    if !a_closed && !b_closed && on_one_plane(a, b, base * COARSEST) {
        return with_tolerances(base, |tol| flat_attempt(a, b, op, tol));
    }
    if op == Op::Exclusion {
        if !a_closed || !b_closed {
            bail!("Exclude takes two solids, or two sheets on one plane.");
        }
        let mut both = boolean(a, b, Op::Difference)?;
        both.append(&boolean(b, a, Op::Difference)?);
        return Ok(both);
    }
    // A sheet is cut by a solid; a solid can't be cut by a sheet, which has no inside.
    if !b_closed && matches!(op, Op::Difference) {
        if a_closed {
            bail!("A cut needs a closed solid as its tool, and this one is an open sheet.");
        }
        bail!("These two sheets aren't on one plane, so neither has any area of the other to cut away.");
    }
    if !a_closed && !b_closed && matches!(op, Op::Intersection) {
        bail!("These two sheets aren't on one plane, so all they can share is a line.");
    }
    if !b_closed && matches!(op, Op::Intersection) {
        return boolean(b, a, op);
    }
    with_tolerances(base, |tol| attempt(a, b, op, tol, a_closed, b_closed))
}

const COARSEST: f64 = 256.0;

fn with_tolerances(base: f64, attempt: impl Fn(f64) -> Result<Mesh>) -> Result<Mesh> {
    let mut why = String::new();
    for factor in [1.0, 16.0, COARSEST] {
        let tol = base * factor;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attempt(tol))) {
            Ok(Ok(m)) => return Ok(m),
            Ok(Err(e)) => why = format!("{e}"),
            Err(_) => why = "the triangulation gave up".to_string(),
        }
    }
    bail!("{why}")
}

/// A point on a surface and its normal, from its largest triangle, if all of it lies in that plane.
fn flat_plane(m: &Mesh, tol: f64) -> Option<(P3, P3)> {
    let mut best: Option<(f64, P3, P3)> = None;
    for t in 0..m.tris.len() {
        let c = m.corners(t);
        let n = cross(sub(c[1], c[0]), sub(c[2], c[0]));
        let area = norm(n);
        if area > 0.0 && best.is_none_or(|b| area > b.0) {
            best = Some((area, c[0], unit(n)));
        }
    }
    let (_, o, n) = best?;
    m.tris.iter().flatten().all(|v| dot(sub(m.verts[*v as usize], o), n).abs() <= tol).then_some((o, n))
}

fn on_one_plane(a: &Mesh, b: &Mesh, tol: f64) -> bool {
    let (Some((o, n)), Some(_)) = (flat_plane(a, tol), flat_plane(b, tol)) else { return false };
    b.tris.iter().flatten().all(|v| dot(sub(b.verts[*v as usize], o), n).abs() <= tol)
}

/// Whether the middle of each triangle lies on the other surface.
fn covered(m: &Mesh, other: &Mesh, tol: f64) -> Vec<bool> {
    let boxes: Vec<(P3, P3)> = (0..other.tris.len())
        .map(|t| {
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for p in other.corners(t) {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k] - tol);
                    hi[k] = hi[k].max(p[k] + tol);
                }
            }
            (lo, hi)
        })
        .collect();
    let tree = Bvh::new(&boxes);
    (0..m.tris.len())
        .map(|t| {
            let c = m.corners(t);
            let centre = scale(add(add(c[0], c[1]), c[2]), 1.0 / 3.0);
            let mut on = false;
            tree.query(centre, centre, |o| {
                on = on || norm(sub(closest_on_triangle(centre, other.corners(o as usize)), centre)) <= tol;
            });
            on
        })
        .collect()
}

/// Two sheets on one plane. Once each is imprinted with the other's edges, every piece is wholly
/// on the other sheet or wholly off it, so what stays is a matter of asking each piece.
fn flat_attempt(a: &Mesh, b: &Mesh, op: Op, tol: f64) -> Result<Mesh> {
    let Some((o, n)) = flat_plane(a, f64::INFINITY) else { bail!("One of the sheets has no area.") };
    // Close enough to be called one plane, so made exactly one: otherwise a sheet a hair off the
    // other's plane would meet nothing at this tolerance, and a cut would quietly do nothing.
    let onto_plane = |m: &Mesh| {
        let mut flat = m.clone();
        for v in &mut flat.verts {
            *v = sub(*v, scale(n, dot(sub(*v, o), n)));
        }
        flat
    };
    let cut = imprinted(&onto_plane(a), &onto_plane(b), tol);
    if !cut.touched {
        return Ok(match op {
            Op::Union | Op::Exclusion => {
                let mut both = a.clone();
                both.append(b);
                both
            }
            Op::Difference => a.clone(),
            Op::Intersection => Mesh::default(),
        });
    }
    let on_b = covered(&cut.a, &cut.b, 2.0 * tol);
    let on_a = covered(&cut.b, &cut.a, 2.0 * tol);
    let area_of = |m: &Mesh, on: &[bool]| -> f64 {
        (0..m.tris.len())
            .filter(|t| on[*t])
            .map(|t| {
                let c = m.corners(t);
                norm(cross(sub(c[1], c[0]), sub(c[2], c[0]))) / 2.0
            })
            .sum()
    };
    // The overlap, measured from each side, is the same area or something has gone wrong.
    let (from_a, from_b) = (area_of(&cut.a, &on_b), area_of(&cut.b, &on_a));
    if (from_a - from_b).abs() > 1e-6 * a.area().max(b.area()) {
        bail!("The overlap of these two sheets doesn't add up ({from_a:.4e} m² from one, {from_b:.4e} m² from the other), so the result can't be trusted.");
    }
    let (off_a, on_a_stays, off_b) = match op {
        Op::Union => (true, true, true),
        Op::Difference => (true, false, false),
        Op::Intersection => (false, true, false),
        Op::Exclusion => (true, false, true),
    };
    // Joined, it's one face, so a solver gets one outline for the current to cross.
    let merged = matches!(op, Op::Union | Op::Exclusion);
    let one_face = a.face[0];
    let mut out = Mesh { verts: cut.a.verts.clone(), ..Default::default() };
    let mut keep = |m: &Mesh, t: usize| {
        let v = m.tris[t];
        let faces_on = m.normal(t).is_none_or(|d| dot(d, n) >= 0.0);
        out.tris.push(if faces_on { v } else { [v[0], v[2], v[1]] });
        out.face.push(if merged { one_face } else { m.face[t] });
    };
    for (t, on) in on_b.iter().enumerate() {
        if if *on { on_a_stays } else { off_a } {
            keep(&cut.a, t);
        }
    }
    for (t, on) in on_a.iter().enumerate() {
        if !*on && off_b {
            keep(&cut.b, t);
        }
    }
    let out = out.compacted();
    if out.is_empty() {
        return Ok(out);
    }
    Ok(tidied(&out, &Frame::from_normal(o, n), tol))
}

/// A point on a straight run does nothing but make a solver's mesh finer there.
fn without_straight_points(mut ring: Vec<[f64; 2]>, tol: f64) -> Vec<[f64; 2]> {
    let mut i = 0;
    while ring.len() > 3 && i < ring.len() {
        let n = ring.len();
        let lift = |q: [f64; 2]| [q[0], q[1], 0.0];
        if distance_to_segment(lift(ring[i]), lift(ring[(i + n - 1) % n]), lift(ring[(i + 1) % n])) <= tol {
            ring.remove(i);
        } else {
            i += 1;
        }
    }
    ring
}

/// The same surface made again from its outlines. Imprinting leaves points along straight edges
/// wherever the other sheet's triangles happened to cross them, and those would reach the solver.
fn tidied(m: &Mesh, f: &Frame, tol: f64) -> Mesh {
    let mut out = Mesh::default();
    for face in m.face_ids() {
        let rings: Vec<Vec<[f64; 2]>> = m.boundary_loops(face).iter().map(|l| without_straight_points(l.iter().map(|v| f.local(m.verts[*v as usize]).0).collect(), tol)).filter(|r| r.len() >= 3).collect();
        let refs: Vec<&[[f64; 2]]> = rings.iter().map(Vec::as_slice).collect();
        let (pts, tris) = super::prims::fill_region(&refs);
        for t in tris {
            out.push_tri(f.at(pts[t[0]], 0.0), f.at(pts[t[1]], 0.0), f.at(pts[t[2]], 0.0), face);
        }
    }
    let out = out.welded(tol);
    // Remade wrongly it's no use, and the untidy one is still right.
    if (out.area() - m.area()).abs() <= 1e-6 * m.area() { out } else { m.clone() }
}

/// Whether two surfaces meet along a line or over an area: a shared face, a sheet standing on a plate.
pub fn touches(a: &Mesh, b: &Mesh) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (lo_a, hi_a) = a.bounds();
    let (lo_b, hi_b) = b.bounds();
    let size = (0..3).map(|k| hi_a[k].max(hi_b[k]) - lo_a[k].min(lo_b[k])).fold(0.0, f64::max);
    let tol = (size * 1e-8).max(1e-10);
    let mut pool = Pool::new(tol);
    let ta = tris_of(a, &mut pool);
    let tb = tris_of(b, &mut pool);
    let boxes_b: Vec<(P3, P3)> = tb.iter().map(|t| tri_box(t, &pool.pts, 2.0 * tol)).collect();
    let tree = Bvh::new(&boxes_b);
    for t in &ta {
        let (lo, hi) = tri_box(t, &pool.pts, 2.0 * tol);
        let mut near = Vec::new();
        tree.query(lo, hi, |i| near.push(i as usize));
        if near.into_iter().any(|i| !intersect(t, &tb[i], &mut pool, tol).is_empty()) {
            return true;
        }
    }
    false
}

/// One triangle of one body, on pool vertices.
#[derive(Clone, Copy)]
struct Tri {
    v: [u32; 3],
    face: FaceId,
    /// Unit normal; zero if it has no area.
    n: P3,
}

/// Two surfaces on one pool of points, each split along where it meets the other.
struct Imprint {
    a: Mesh,
    b: Mesh,
    /// The edges no patch crosses.
    cut_edges: HashSet<(u32, u32)>,
    /// Whether they met anywhere at all, more than at a point.
    touched: bool,
}

fn imprinted(a: &Mesh, b: &Mesh, tol: f64) -> Imprint {
    let mut pool = Pool::new(tol);
    let ta = tris_of(a, &mut pool);
    let tb = tris_of(b, &mut pool);

    // Which triangles the other body touches, and how.
    let mut cuts_a: Vec<Vec<[u32; 2]>> = vec![Vec::new(); ta.len()];
    let mut cuts_b: Vec<Vec<[u32; 2]>> = vec![Vec::new(); tb.len()];
    let mut on_edge: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let boxes_b: Vec<(P3, P3)> = tb.iter().map(|t| tri_box(t, &pool.pts, 2.0 * tol)).collect();
    let tree_b = Bvh::new(&boxes_b);
    for (ia, t) in ta.iter().enumerate() {
        let (lo, hi) = tri_box(t, &pool.pts, 2.0 * tol);
        let mut near = Vec::new();
        tree_b.query(lo, hi, |i| near.push(i as usize));
        for ib in near {
            let segments = intersect(t, &tb[ib], &mut pool, tol);
            for s in segments {
                for tri in [t, &tb[ib]] {
                    for end in s {
                        note_on_edges(tri, end, &pool.pts, tol, &mut on_edge);
                    }
                }
                cuts_a[ia].push(s);
                cuts_b[ib].push(s);
            }
        }
    }

    // Everything re-triangulated, the cuts noted as edges no patch crosses.
    let mut cut_edges: HashSet<(u32, u32)> = HashSet::new();
    let pieces_a = split_all(&ta, &cuts_a, &on_edge, &mut pool, tol, &mut cut_edges);
    let pieces_b = split_all(&tb, &cuts_b, &on_edge, &mut pool, tol, &mut cut_edges);

    let verts = pool.pts;
    let mesh_a = Mesh { verts: verts.clone(), tris: pieces_a.iter().map(|p| p.0).collect(), face: pieces_a.iter().map(|p| p.1).collect() };
    let mesh_b = Mesh { verts, tris: pieces_b.iter().map(|p| p.0).collect(), face: pieces_b.iter().map(|p| p.1).collect() };
    Imprint { a: mesh_a, b: mesh_b, cut_edges, touched: cuts_a.iter().chain(&cuts_b).any(|c| !c.is_empty()) }
}

fn attempt(a: &Mesh, b: &Mesh, op: Op, tol: f64, a_closed: bool, b_closed: bool) -> Result<Mesh> {
    let Imprint { a: mesh_a, b: mesh_b, cut_edges, .. } = imprinted(a, b, tol);
    let side_a = classify(&mesh_a, &mesh_b, b_closed, &cut_edges, 2.0 * tol);
    let side_b = classify(&mesh_b, &mesh_a, a_closed, &cut_edges, 2.0 * tol);

    let mut out = Mesh { verts: mesh_a.verts.clone(), ..Default::default() };
    for (i, s) in side_a.iter().enumerate() {
        if keep_first(op, *s, a_closed) {
            out.tris.push(mesh_a.tris[i]);
            out.face.push(mesh_a.face[i]);
        }
    }
    // A sheet cut by a solid leaves nothing of the solid.
    let second_counts = a_closed || op == Op::Union;
    if second_counts {
        for (i, s) in side_b.iter().enumerate() {
            match keep_second(op, *s, a_closed) {
                Keep::No => {}
                Keep::Yes => {
                    out.tris.push(mesh_b.tris[i]);
                    out.face.push(mesh_b.face[i]);
                }
                Keep::Flipped => {
                    let t = mesh_b.tris[i];
                    out.tris.push([t[0], t[2], t[1]]);
                    out.face.push(mesh_b.face[i]);
                }
            }
        }
    }
    let out = out.compacted();
    if a_closed && b_closed {
        check_solid(&out, a, b, op)?;
    }
    Ok(out)
}

fn check_solid(out: &Mesh, a: &Mesh, b: &Mesh, op: Op) -> Result<()> {
    if out.is_empty() {
        return Ok(());
    }
    let open = out.open_edges();
    if open > 0 {
        bail!("The result would have {open} open edges. Move the bodies slightly apart or together: they may be nearly but not quite touching.");
    }
    let (va, vb, v) = (a.volume(), b.volume(), out.volume());
    let slack = 1e-6 * (va.abs() + vb.abs()).max(1e-30);
    let fits = match op {
        Op::Union => v >= va.max(vb) - slack && v <= va + vb + slack,
        Op::Difference => v >= -slack && v <= va + slack,
        Op::Intersection => v >= -slack && v <= va.min(vb) + slack,
        // Made from two differences, each of which has already been through this.
        Op::Exclusion => true,
    };
    if !fits {
        bail!("The result's volume doesn't add up ({:.4e} m³ from {:.4e} and {:.4e}), so it can't be trusted.", v, va, vb);
    }
    Ok(())
}

fn tris_of(m: &Mesh, pool: &mut Pool) -> Vec<Tri> {
    let mut out = Vec::with_capacity(m.tris.len());
    for (i, t) in m.tris.iter().enumerate() {
        let v = t.map(|k| pool.id(m.verts[k as usize]));
        if v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
            continue;
        }
        let p = v.map(|k| pool.pts[k as usize]);
        let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
        let n = if norm(n) > 0.0 { unit(n) } else { [0.0; 3] };
        out.push(Tri { v, face: m.face[i], n });
    }
    out
}

fn tri_box(t: &Tri, pts: &[P3], pad: f64) -> (P3, P3) {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for &v in &t.v {
        let p = pts[v as usize];
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] - pad);
            hi[k] = hi[k].max(p[k] + pad);
        }
    }
    (lo, hi)
}

/// Whether `p`, known to be on the triangle's plane, is on the triangle.
fn on_triangle(p: P3, t: [P3; 3], n: P3, tol: f64) -> bool {
    (0..3).all(|k| {
        let (a, b) = (t[k], t[(k + 1) % 3]);
        let edge = sub(b, a);
        let len = norm(edge);
        len > 0.0 && dot(cross(edge, sub(p, a)), n) / len >= -tol
    })
}

/// Where two triangles meet: nothing, one segment, or for coplanar ones each other's edges.
fn intersect(a: &Tri, b: &Tri, pool: &mut Pool, tol: f64) -> Vec<[u32; 2]> {
    if norm(a.n) == 0.0 || norm(b.n) == 0.0 {
        return Vec::new();
    }
    let pa = a.v.map(|k| pool.pts[k as usize]);
    let pb = b.v.map(|k| pool.pts[k as usize]);
    let snap = |d: f64| if d.abs() <= tol { 0.0 } else { d };
    let db = pb.map(|p| snap(dot(a.n, sub(p, pa[0]))));
    let da = pa.map(|p| snap(dot(b.n, sub(p, pb[0]))));
    if db.iter().all(|d| *d > 0.0) || db.iter().all(|d| *d < 0.0) || da.iter().all(|d| *d > 0.0) || da.iter().all(|d| *d < 0.0) {
        return Vec::new();
    }
    if db.iter().all(|d| *d == 0.0) || da.iter().all(|d| *d == 0.0) {
        return coplanar(a, b, pool, tol);
    }
    // Each point where one triangle's edge goes through the other.
    let mut found: Vec<u32> = Vec::with_capacity(4);
    let mut crossings = |edges_of: [P3; 3], d: [f64; 3], other: [P3; 3], n: P3, pool: &mut Pool| {
        for k in 0..3 {
            let (s, t) = (edges_of[k], edges_of[(k + 1) % 3]);
            let (ds, dt) = (d[k], d[(k + 1) % 3]);
            if ds == 0.0 && on_triangle(s, other, n, tol) {
                found.push(pool.id(s));
            }
            if ds * dt < 0.0 {
                let q = add(s, scale(sub(t, s), ds / (ds - dt)));
                if on_triangle(q, other, n, tol) {
                    found.push(pool.id(q));
                }
            }
        }
    };
    crossings(pa, da, pb, b.n, pool);
    crossings(pb, db, pa, a.n, pool);
    found.sort_unstable();
    found.dedup();
    if found.len() < 2 {
        return Vec::new();
    }
    // They're all on one line, so the two farthest apart are the segment's ends.
    let mut best = (0.0, found[0], found[1]);
    for i in 0..found.len() {
        for j in i + 1..found.len() {
            let d = norm(sub(pool.pts[found[i] as usize], pool.pts[found[j] as usize]));
            if d > best.0 {
                best = (d, found[i], found[j]);
            }
        }
    }
    vec![[best.1, best.2]]
}

/// Coplanar triangles imprint each other's edges, as far as they overlap.
fn coplanar(a: &Tri, b: &Tri, pool: &mut Pool, tol: f64) -> Vec<[u32; 2]> {
    let mut out = Vec::new();
    for (from, into) in [(a, b), (b, a)] {
        let pf = from.v.map(|k| pool.pts[k as usize]);
        let pi = into.v.map(|k| pool.pts[k as usize]);
        for k in 0..3 {
            let (s, e) = (pf[k], pf[(k + 1) % 3]);
            let d = sub(e, s);
            // The part of this edge inside the other triangle, clipped by each of its sides.
            let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
            let mut inside = true;
            for j in 0..3 {
                let (u, v) = (pi[j], pi[(j + 1) % 3]);
                let edge = sub(v, u);
                let len = norm(edge).max(1e-300);
                let inward = unit(cross(into.n, edge));
                let at = dot(sub(s, u), inward);
                let rate = dot(d, inward);
                if rate.abs() * 1.0 < 1e-300 || rate.abs() <= tol * 1e-3 {
                    if at < -tol {
                        inside = false;
                    }
                    continue;
                }
                let t = -at / rate;
                if rate > 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
                let _ = len;
            }
            let span = norm(d);
            if inside && (t1 - t0) * span > tol {
                let (p, q) = (pool.id(add(s, scale(d, t0))), pool.id(add(s, scale(d, t1))));
                if p != q {
                    out.push([p, q]);
                }
            }
        }
    }
    out
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// Notes a point on any edge of the triangle it lies on, unless it's one of the edge's ends.
fn note_on_edges(t: &Tri, id: u32, pts: &[P3], tol: f64, on_edge: &mut HashMap<(u32, u32), Vec<u32>>) {
    if t.v.contains(&id) {
        return;
    }
    let p = pts[id as usize];
    for k in 0..3 {
        let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
        if distance_to_segment(p, pts[a as usize], pts[b as usize]) <= tol {
            let list = on_edge.entry(edge_key(a, b)).or_default();
            if !list.contains(&id) {
                list.push(id);
            }
        }
    }
}

fn distance_to_segment(p: P3, a: P3, b: P3) -> f64 {
    let d = sub(b, a);
    let t = (dot(sub(p, a), d) / dot(d, d).max(1e-300)).clamp(0.0, 1.0);
    norm(sub(p, add(a, scale(d, t))))
}

type Piece = ([u32; 3], FaceId);

/// Every triangle with its cuts and split edges re-triangulated; the others as they were.
fn split_all(tris: &[Tri], cuts: &[Vec<[u32; 2]>], on_edge: &HashMap<(u32, u32), Vec<u32>>, pool: &mut Pool, tol: f64, cut_edges: &mut HashSet<(u32, u32)>) -> Vec<Piece> {
    let mut out = Vec::with_capacity(tris.len());
    for (i, t) in tris.iter().enumerate() {
        let touched = !cuts[i].is_empty() || (0..3).any(|k| on_edge.contains_key(&edge_key(t.v[k], t.v[(k + 1) % 3])));
        if touched && norm(t.n) > 0.0 {
            split_one(t, &cuts[i], on_edge, pool, tol, cut_edges, &mut out);
        } else {
            out.push((t.v, t.face));
        }
    }
    out
}

/// A triangle's corners and the points on its edges, in order round it.
fn boundary_chain(t: &Tri, on_edge: &HashMap<(u32, u32), Vec<u32>>, pts: &[P3]) -> Vec<(u32, usize)> {
    let mut chain = Vec::new();
    for k in 0..3 {
        let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
        chain.push((a, k));
        if let Some(list) = on_edge.get(&edge_key(a, b)) {
            let (pa, d) = (pts[a as usize], sub(pts[b as usize], pts[a as usize]));
            let mut sorted: Vec<(f64, u32)> = list.iter().map(|id| (dot(sub(pts[*id as usize], pa), d), *id)).collect();
            sorted.sort_by(|x, y| x.0.total_cmp(&y.0));
            chain.extend(sorted.into_iter().map(|(_, id)| (id, k)));
        }
    }
    chain
}

fn split_one(t: &Tri, cuts: &[[u32; 2]], on_edge: &HashMap<(u32, u32), Vec<u32>>, pool: &mut Pool, tol: f64, cut_edges: &mut HashSet<(u32, u32)>, out: &mut Vec<Piece>) {
    let corners = t.v.map(|k| pool.pts[k as usize]);
    let o = corners[0];
    let e1 = unit(sub(corners[1], o));
    let e2 = cross(t.n, e1);
    let flat = |p: P3| Point2::new(dot(sub(p, o), e1), dot(sub(p, o), e2));
    let chain = boundary_chain(t, on_edge, &pool.pts);

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> = ConstrainedDelaunayTriangulation::new();
    let mut ids: HashMap<FixedVertexHandle, u32> = HashMap::new();
    let mut add_point = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>, id: u32, p: P3| -> Option<FixedVertexHandle> {
        let h = cdt.insert(flat(p)).ok()?;
        ids.entry(h).or_insert(id);
        Some(h)
    };
    let mut handles: Vec<FixedVertexHandle> = Vec::with_capacity(chain.len());
    let mut chain_ok = Vec::with_capacity(chain.len());
    for (id, _) in &chain {
        if let Some(h) = add_point(&mut cdt, *id, pool.pts[*id as usize]) {
            handles.push(h);
            chain_ok.push(*id);
        }
    }
    // The outline, then the cuts. A cut crossing something makes a new point there.
    let n = handles.len();
    for k in 0..n {
        let (x, y) = (handles[k], handles[(k + 1) % n]);
        if x != y {
            cdt.add_constraint_and_split(x, y, |p| p);
        }
    }
    let mut cut_handles = Vec::with_capacity(cuts.len());
    for c in cuts {
        let (px, py) = (pool.pts[c[0] as usize], pool.pts[c[1] as usize]);
        let (Some(x), Some(y)) = (add_point(&mut cdt, c[0], px), add_point(&mut cdt, c[1], py)) else { continue };
        if x != y {
            cut_handles.push((x, y));
            cdt.add_constraint_and_split(x, y, |p| p);
        }
    }

    // A vertex the splitting made has no id yet: where it is on the triangle gives it one.
    let at_3d = |q: Point2<f64>| add(o, add(scale(e1, q.x), scale(e2, q.y)));
    let mut vertex_id = |h: FixedVertexHandle, q: Point2<f64>, pool: &mut Pool| -> u32 { *ids.entry(h).or_insert_with(|| pool.id(at_3d(q))) };

    // Which edge of the triangle each boundary point is on, for spotting slivers along an edge.
    let mut edges_of_point: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, (id, k)) in chain.iter().enumerate() {
        edges_of_point.entry(*id).or_default().push(*k);
        // A corner is on the edge before it too.
        if i > 0 && chain[i].0 == t.v[*k] {
            edges_of_point.entry(*id).or_default().push((*k + 2) % 3);
        }
    }
    edges_of_point.entry(t.v[0]).or_default().push(2);
    let on_same_edge = |v: &[u32]| -> bool { (0..3).any(|e| v.iter().all(|id| edges_of_point.get(id).is_some_and(|l| l.contains(&e)))) };

    let ring: Vec<Point2<f64>> = chain_ok.iter().map(|id| flat(pool.pts[*id as usize])).collect();
    for f in cdt.inner_faces() {
        let vs = f.vertices();
        let q = vs.map(|v| v.position());
        let c = Point2::new((q[0].x + q[1].x + q[2].x) / 3.0, (q[0].y + q[1].y + q[2].y) / 3.0);
        if !inside_ring(&ring, c) {
            continue;
        }
        let id = [vertex_id(vs[0].fix(), q[0], pool), vertex_id(vs[1].fix(), q[1], pool), vertex_id(vs[2].fix(), q[2], pool)];
        if id[0] == id[1] || id[1] == id[2] || id[0] == id[2] || on_same_edge(&id) {
            continue;
        }
        out.push((id, t.face));
    }
    for e in cdt.undirected_edges() {
        if !e.is_constraint_edge() {
            continue;
        }
        let [x, y] = e.vertices();
        let (idx, idy) = (vertex_id(x.fix(), x.position(), pool), vertex_id(y.fix(), y.position(), pool));
        if idx == idy {
            continue;
        }
        if on_same_edge(&[idx, idy]) {
            // On the triangle's own edge it's only a cut where a cut runs along it.
            let mid = scale(add(pool.pts[idx as usize], pool.pts[idy as usize]), 0.5);
            if !cuts.iter().any(|c| distance_to_segment(mid, pool.pts[c[0] as usize], pool.pts[c[1] as usize]) <= tol) {
                continue;
            }
        }
        cut_edges.insert(edge_key(idx, idy));
    }
}

fn inside_ring(ring: &[Point2<f64>], p: Point2<f64>) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            inside = !inside;
        }
    }
    inside
}

/// Where a patch of one body lies relative to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    In,
    Out,
    OnSame,
    OnOpposite,
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    while parent[x as usize] != x {
        parent[x as usize] = parent[parent[x as usize] as usize];
        x = parent[x as usize];
    }
    x
}

/// Each triangle's side, worked out once per patch.
fn classify(m: &Mesh, other: &Mesh, other_closed: bool, cut_edges: &HashSet<(u32, u32)>, on_tol: f64) -> Vec<Side> {
    let mut parent: Vec<u32> = (0..m.tris.len() as u32).collect();
    let mut first: HashMap<(u32, u32), u32> = HashMap::with_capacity(m.tris.len() * 2);
    for (i, t) in m.tris.iter().enumerate() {
        for k in 0..3 {
            let key = edge_key(t[k], t[(k + 1) % 3]);
            if cut_edges.contains(&key) {
                continue;
            }
            match first.get(&key) {
                Some(&j) => {
                    let (x, y) = (find(&mut parent, i as u32), find(&mut parent, j));
                    parent[x as usize] = y;
                }
                None => {
                    first.insert(key, i as u32);
                }
            }
        }
    }
    // The biggest triangle of each patch speaks for it: it's the farthest from any doubt.
    let mut rep: HashMap<u32, (usize, f64)> = HashMap::new();
    for t in 0..m.tris.len() {
        let root = find(&mut parent, t as u32);
        let c = m.corners(t);
        let area = norm(cross(sub(c[1], c[0]), sub(c[2], c[0])));
        let e = rep.entry(root).or_insert((t, area));
        if area > e.1 {
            *e = (t, area);
        }
    }
    let boxes: Vec<(P3, P3)> = (0..other.tris.len())
        .map(|t| {
            let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
            for p in other.corners(t) {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            (lo, hi)
        })
        .collect();
    let tree = Bvh::new(&boxes);
    let mut side_of_root: HashMap<u32, Side> = HashMap::with_capacity(rep.len());
    for (root, (t, _)) in &rep {
        let c = m.corners(*t);
        let centre = scale(add(add(c[0], c[1]), c[2]), 1.0 / 3.0);
        let normal = m.normal(*t).unwrap_or([0.0; 3]);
        let pad = [on_tol; 3];
        let (lo, hi) = (sub(centre, pad), add(centre, pad));
        let mut on: Option<f64> = None;
        tree.query(lo, hi, |o| {
            let o = o as usize;
            let q = other.corners(o);
            if norm(sub(closest_on_triangle(centre, q), centre)) <= on_tol
                && let Some(n) = other.normal(o)
            {
                let d = dot(n, normal);
                // Coplanar is what's meant: a surface merely passing through here isn't "on".
                if d.abs() > 0.99 && on.is_none_or(|b| d.abs() > b.abs()) {
                    on = Some(d);
                }
            }
        });
        let side = match on {
            Some(d) if d > 0.0 => Side::OnSame,
            Some(_) => Side::OnOpposite,
            None if !other_closed => Side::Out,
            None => {
                if winding_number(other, centre).abs() > 0.5 {
                    Side::In
                } else {
                    Side::Out
                }
            }
        };
        side_of_root.insert(*root, side);
    }
    (0..m.tris.len())
        .map(|t| {
            let root = find(&mut parent, t as u32);
            side_of_root[&root]
        })
        .collect()
}

/// Whether a piece of the first body stays.
fn keep_first(op: Op, s: Side, closed: bool) -> bool {
    match (op, s) {
        (Op::Union, Side::Out | Side::OnSame) => true,
        // An open sheet lying on the other body's surface is still there.
        (Op::Union, Side::OnOpposite) => !closed,
        (Op::Union, Side::In) => !closed,
        (Op::Difference, Side::Out | Side::OnOpposite) => true,
        (Op::Difference, Side::OnSame) => !closed,
        (Op::Difference, Side::In) => false,
        (Op::Intersection, Side::In | Side::OnSame) => true,
        (Op::Intersection, Side::OnOpposite) => !closed,
        (Op::Intersection, Side::Out) => false,
        // Never reaches here: it's made from two differences, or by the flat route.
        (Op::Exclusion, _) => false,
    }
}

enum Keep {
    No,
    Yes,
    Flipped,
}

/// Whether a piece of the second body stays, and which way it faces.
fn keep_second(op: Op, s: Side, first_closed: bool) -> Keep {
    match (op, s) {
        (Op::Union, Side::Out) => Keep::Yes,
        // Inside a sheet there's no inside: it keeps all of itself but what's already there.
        (Op::Union, Side::In) if !first_closed => Keep::Yes,
        (Op::Difference, Side::In) => Keep::Flipped,
        (Op::Intersection, Side::In) => Keep::Yes,
        _ => Keep::No,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::mesh::Xform;

    fn cuboid(lo: P3, hi: P3, base: u32) -> Mesh {
        let v = |x: usize, y: usize, z: usize| [if x == 0 { lo[0] } else { hi[0] }, if y == 0 { lo[1] } else { hi[1] }, if z == 0 { lo[2] } else { hi[2] }];
        let mut m = Mesh::default();
        let f = |k: u32| base + k;
        m.push_quad(v(0, 0, 0), v(0, 1, 0), v(1, 1, 0), v(1, 0, 0), f(0));
        m.push_quad(v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1), f(1));
        m.push_quad(v(0, 0, 0), v(1, 0, 0), v(1, 0, 1), v(0, 0, 1), f(2));
        m.push_quad(v(0, 1, 0), v(0, 1, 1), v(1, 1, 1), v(1, 1, 0), f(3));
        m.push_quad(v(0, 0, 0), v(0, 0, 1), v(0, 1, 1), v(0, 1, 0), f(4));
        m.push_quad(v(1, 0, 0), v(1, 1, 0), v(1, 1, 1), v(1, 0, 1), f(5));
        m.welded(1e-12)
    }

    fn tube(c: [f64; 2], r: f64, z0: f64, z1: f64, n: usize, base: u32) -> Mesh {
        let mut m = Mesh::default();
        let at = |k: usize, z: f64| {
            let t = std::f64::consts::TAU * k as f64 / n as f64;
            [c[0] + r * t.cos(), c[1] + r * t.sin(), z]
        };
        for k in 0..n {
            m.push_quad(at(k, z0), at(k + 1, z0), at(k + 1, z1), at(k, z1), base);
            m.push_tri([c[0], c[1], z0], at(k + 1, z0), at(k, z0), base + 1);
            m.push_tri([c[0], c[1], z1], at(k, z1), at(k + 1, z1), base + 2);
        }
        m.welded(1e-12)
    }

    /// The identity every op has to obey, and each result closed.
    fn check_all(a: &Mesh, b: &Mesh, label: &str) {
        let u = boolean(a, b, Op::Union).unwrap_or_else(|e| panic!("{label} union: {e}"));
        let i = boolean(a, b, Op::Intersection).unwrap_or_else(|e| panic!("{label} intersection: {e}"));
        let ab = boolean(a, b, Op::Difference).unwrap_or_else(|e| panic!("{label} a-b: {e}"));
        let ba = boolean(b, a, Op::Difference).unwrap_or_else(|e| panic!("{label} b-a: {e}"));
        let (va, vb) = (a.volume(), b.volume());
        let scale = va.abs() + vb.abs();
        let close = |x: f64, y: f64, what: &str| assert!((x - y).abs() <= 1e-5 * scale, "{label} {what}: {x} vs {y}");
        close(u.volume() + i.volume(), va + vb, "union + intersection");
        close(ab.volume() + i.volume(), va, "a-b + intersection");
        close(ba.volume() + i.volume(), vb, "b-a + intersection");
        for (m, what) in [(&u, "union"), (&i, "intersection"), (&ab, "a-b"), (&ba, "b-a")] {
            assert!(m.is_empty() || m.open_edges() == 0, "{label} {what} has {} open edges", m.open_edges());
        }
    }

    #[test]
    fn overlapping_boxes_obey_the_volume_identity() {
        let a = cuboid([0.0; 3], [2.0, 2.0, 2.0], 0);
        let b = cuboid([1.0, 1.0, 1.0], [3.0, 3.0, 3.0], 100);
        check_all(&a, &b, "corner overlap");
        let i = boolean(&a, &b, Op::Intersection).unwrap();
        assert!((i.volume() - 1.0).abs() < 1e-9, "{}", i.volume());
    }

    #[test]
    fn faces_that_coincide_are_handled() {
        let a = cuboid([0.0; 3], [2.0, 2.0, 2.0], 0);
        // Same top, so the cut comes out flush; a pocket with its wall on a's wall; side by side.
        check_all(&a, &cuboid([0.5, 0.5, 1.0], [1.5, 1.5, 2.0], 100), "flush pocket");
        check_all(&a, &cuboid([0.0, 0.5, 1.0], [1.0, 1.5, 2.0], 100), "notch");
        check_all(&a, &cuboid([2.0, 0.0, 0.0], [3.0, 2.0, 2.0], 100), "side by side");
        check_all(&a, &cuboid([2.0, 0.5, 0.5], [3.0, 1.5, 1.5], 100), "smaller on a face");
        check_all(&a, &a.clone(), "the same box");
        check_all(&a, &cuboid([0.0, 0.0, 0.0], [2.0, 2.0, 1.0], 100), "half of it");
        // Through the whole of it, flush both ends.
        check_all(&a, &cuboid([0.5, 0.5, 0.0], [1.5, 1.5, 2.0], 100), "flush through");
    }

    #[test]
    fn a_hole_through_a_plate_leaves_the_right_volume() {
        let plate = cuboid([-1.0, -1.0, 0.0], [1.0, 1.0, 0.1], 0);
        let pin = tube([0.2, 0.1], 0.3, -1.0, 1.0, 48, 100);
        check_all(&plate, &pin, "hole");
        let out = boolean(&plate, &pin, Op::Difference).unwrap();
        let exact = 4.0 * 0.1 - 0.1 * pin.volume() / 2.0;
        assert!((out.volume() - exact).abs() < 1e-6, "{} vs {exact}", out.volume());
        // A cut keeps the names of the faces it cuts, and the wall of the hole has the pin's.
        for face in [0, 1, 2, 3, 4, 5, 100] {
            assert!(out.has_face(face), "face {face}");
        }
        assert!(!out.has_face(101), "the pin's ends are gone");
    }

    #[test]
    fn a_body_wholly_inside_another_makes_a_cavity() {
        let a = cuboid([0.0; 3], [4.0, 4.0, 4.0], 0);
        let b = cuboid([1.0, 1.0, 1.0], [2.0, 2.0, 2.0], 100);
        let cut = boolean(&a, &b, Op::Difference).unwrap();
        assert!((cut.volume() - 63.0).abs() < 1e-9);
        assert!(boolean(&a, &b, Op::Union).unwrap().volume() > 63.9);
        assert!((boolean(&b, &a, Op::Difference).unwrap().volume()).abs() < 1e-9);
        assert!(boolean(&b, &a, Op::Difference).unwrap().is_empty(), "nothing is left");
    }

    #[test]
    fn apart_bodies_stay_apart() {
        let a = cuboid([0.0; 3], [1.0, 1.0, 1.0], 0);
        let b = cuboid([3.0, 0.0, 0.0], [4.0, 1.0, 1.0], 100);
        assert!((boolean(&a, &b, Op::Union).unwrap().volume() - 2.0).abs() < 1e-12);
        assert!(boolean(&a, &b, Op::Intersection).unwrap().is_empty());
        assert!((boolean(&a, &b, Op::Difference).unwrap().volume() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn turned_and_curved_bodies_obey_it_too() {
        let mut seed = 12345_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64) / (1u64 << 31) as f64
        };
        for k in 0..120 {
            let a = cuboid([0.0; 3], [1.0 + next(), 1.0 + next(), 1.0 + next()], 0);
            let axis = [next() - 0.5, next() - 0.5, next() - 0.5 + 0.01];
            let x = Xform::rotate([0.5, 0.5, 0.5], axis, next() * 6.0);
            let mut b = if k % 3 == 0 {
                tube([next(), next()], 0.2 + next() * 0.5, -0.5, 0.3 + next(), 24, 100)
            } else {
                cuboid([next() * 0.5, next() * 0.5, next() * 0.5], [1.0 + next(), 1.0 + next(), 1.0 + next()], 100)
            };
            b = b.transformed(&x);
            check_all(&a, &b, &format!("case {k}"));
        }
    }

    #[test]
    fn boxes_on_a_shared_grid_obey_it() {
        // Every coincidence a grid can make: shared faces, edges, corners.
        let mut seed = 99_u64;
        let mut next = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % n) as f64
        };
        for k in 0..150 {
            let lo = [next(3), next(3), next(3)];
            let hi = [lo[0] + 1.0 + next(3), lo[1] + 1.0 + next(3), lo[2] + 1.0 + next(3)];
            let a = cuboid([0.0; 3], [3.0, 3.0, 3.0], 0);
            check_all(&a, &cuboid(lo, hi, 100), &format!("grid {k} {lo:?} {hi:?}"));
        }
    }

    #[test]
    fn a_sheet_is_cut_by_a_solid_and_not_the_other_way() {
        let mut sheet = Mesh::default();
        sheet.push_quad([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], 7);
        let sheet = sheet.welded(1e-12);
        assert!(!sheet.is_closed());
        let tool = cuboid([-0.5, -0.5, -1.0], [0.5, 0.5, 1.0], 100);
        let cut = boolean(&sheet, &tool, Op::Difference).unwrap();
        assert!((cut.area() - 3.0).abs() < 1e-9, "{}", cut.area());
        assert_eq!(cut.face_ids(), vec![7], "none of the tool is left");
        let inside = boolean(&sheet, &tool, Op::Intersection).unwrap();
        assert!((inside.area() - 1.0).abs() < 1e-9, "{}", inside.area());
        assert!(boolean(&tool, &sheet, Op::Difference).is_err(), "a sheet has no inside to cut with");
        // The other order of an intersection means the same.
        assert!((boolean(&tool, &sheet, Op::Intersection).unwrap().area() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sheets_joined_keep_both_and_share_their_edges() {
        let mut a = Mesh::default();
        a.push_quad([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], 1);
        let mut b = Mesh::default();
        b.push_quad([0.0, -0.5, 0.0], [0.0, 0.5, 0.0], [0.0, 0.5, 1.0], [0.0, -0.5, 1.0], 2);
        let joined = boolean(&a.welded(1e-12), &b.welded(1e-12), Op::Union).unwrap();
        assert!((joined.area() - 5.0).abs() < 1e-9, "{}", joined.area());
        assert!(joined.pinched_edges() > 0, "they meet along a junction");
    }

    fn flat(lo: [f64; 2], hi: [f64; 2], face: u32) -> Mesh {
        let mut m = Mesh::default();
        m.push_quad([lo[0], lo[1], 0.0], [hi[0], lo[1], 0.0], [hi[0], hi[1], 0.0], [lo[0], hi[1], 0.0], face);
        m.welded(1e-12)
    }

    /// Everything the four ways of combining two sheets have to agree on.
    fn check_flat(a: &Mesh, b: &Mesh, what: &str) {
        let (aa, ab) = (a.area(), b.area());
        let join = boolean(a, b, Op::Union).unwrap();
        let common = boolean(a, b, Op::Intersection).unwrap();
        let minus = boolean(a, b, Op::Difference).unwrap();
        let either = boolean(a, b, Op::Exclusion).unwrap();
        let close = |x: f64, y: f64, name: &str| assert!((x - y).abs() < 1e-9, "{what}, {name}: {x} against {y}");
        close(join.area() + common.area(), aa + ab, "join and common");
        close(minus.area(), aa - common.area(), "minus");
        close(either.area(), aa + ab - 2.0 * common.area(), "exclude");
        for m in [&join, &common, &minus, &either] {
            for t in 0..m.tris.len() {
                assert!(m.normal(t).is_none_or(|n| n[2] > 0.0), "{what}: every piece faces the first sheet's way");
            }
        }
    }

    #[test]
    fn sheets_on_one_plane_combine_as_areas() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        check_flat(&a, &flat([2.0, 2.0], [6.0, 6.0], 2), "overlapping corners");
        check_flat(&a, &flat([1.0, 1.0], [2.0, 2.0], 2), "one inside the other");
        check_flat(&flat([1.0, 1.0], [2.0, 2.0], 2), &a, "the other inside the one");
        check_flat(&a, &flat([4.0, 1.0], [6.0, 3.0], 2), "side by side");
        check_flat(&a, &flat([-1.0, 1.0], [5.0, 3.0], 2), "a bar across");
        check_flat(&a, &flat([0.0, 0.0], [4.0, 4.0], 2), "the very same");
        check_flat(&a, &flat([9.0, 9.0], [10.0, 10.0], 2), "well apart");
        check_flat(&a, &flat([4.0, 4.0], [5.0, 5.0], 2), "touching at a corner");
    }

    #[test]
    fn a_sheet_facing_the_other_way_still_combines() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        check_flat(&a, &flat([2.0, 2.0], [6.0, 6.0], 2).flipped(), "flipped");
    }

    #[test]
    fn sheets_joined_on_one_plane_are_one_outline_with_no_stray_points() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        let b = flat([4.0, 1.0], [6.0, 3.0], 2);
        let join = boolean(&a, &b, Op::Union).unwrap();
        assert_eq!(join.face_ids(), vec![1], "one face, for one outline");
        let loops = join.boundary_loops(1);
        assert_eq!(loops.len(), 1, "{loops:?}");
        assert_eq!(loops[0].len(), 8, "the corners and nothing else: {loops:?}");
    }

    #[test]
    fn a_sheet_cut_by_one_inside_it_has_a_hole() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        let minus = boolean(&a, &flat([1.0, 1.0], [2.0, 2.0], 2), Op::Difference).unwrap();
        assert_eq!(minus.boundary_loops(1).len(), 2, "the outside and the hole");
        assert_eq!(minus.face_ids(), vec![1]);
    }

    #[test]
    fn a_sheet_cut_in_two_comes_apart() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        let minus = boolean(&a, &flat([1.0, -1.0], [2.0, 5.0], 2), Op::Difference).unwrap();
        assert_eq!(minus.boundary_loops(1).len(), 2);
        assert!((minus.area() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn where_exclude_leaves_parts_touching_at_points_no_loop_crosses_itself() {
        let build = || boolean(&flat([0.0, 0.0], [4.0, 4.0], 1), &flat([2.0, 2.0], [6.0, 6.0], 2), Op::Exclusion).unwrap();
        let either = build();
        let loops = either.boundary_loops(1);
        // Two L shapes meeting at two points: not an outline with a hole that touches it.
        assert_eq!(loops.len(), 2, "{loops:?}");
        let area = |l: &Vec<u32>| {
            let q: Vec<[f64; 2]> = l.iter().map(|v| [either.verts[*v as usize][0], either.verts[*v as usize][1]]).collect();
            crate::modeler::prims::signed_area(&q)
        };
        for l in &loops {
            assert!((area(l) - 12.0).abs() < 1e-9, "each is an L of 12: {} {l:?}", area(l));
        }
        for l in &loops {
            let mut once = l.clone();
            once.sort_unstable();
            once.dedup();
            assert_eq!(once.len(), l.len(), "a figure of eight: {l:?}");
        }
        for _ in 0..8 {
            assert_eq!(build().boundary_loops(1), loops, "the same every time");
        }
    }

    #[test]
    fn a_hole_touching_the_edge_at_a_point_is_one_loop_through_it_twice() {
        // A diamond inside the square, its left point on the square's left side.
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        let mut d = Mesh::default();
        d.push_quad([0.0, 2.0, 0.0], [1.0, 1.0, 0.0], [2.0, 2.0, 0.0], [1.0, 3.0, 0.0], 2);
        let minus = boolean(&a, &d.welded(1e-12), Op::Difference).unwrap();
        assert!((minus.area() - 14.0).abs() < 1e-9, "{}", minus.area());
        let loops = minus.boundary_loops(1);
        assert_eq!(loops.len(), 1, "{loops:?}");
        // The square's four corners, the diamond's other three, and the shared point both times.
        assert_eq!(loops[0].len(), 9, "{loops:?}");
    }

    #[test]
    fn sheets_that_only_touch_at_a_corner_keep_their_own_faces() {
        let join = boolean(&flat([0.0, 0.0], [1.0, 1.0], 1), &flat([1.0, 1.0], [2.0, 2.0], 2), Op::Union).unwrap();
        assert_eq!(join.face_ids(), vec![1, 2]);
    }

    #[test]
    fn a_sheet_a_hair_off_the_others_plane_still_cuts_it() {
        // 100 mm sheets, 100 nm apart: within what's called one plane, not within what touches.
        let (s, gap) = (0.1, 1e-7);
        let a = flat([0.0, 0.0], [s, s], 1);
        let mut b = Mesh::default();
        b.push_quad([s / 4.0, s / 4.0, gap], [s / 2.0, s / 4.0, gap], [s / 2.0, s / 2.0, gap], [s / 4.0, s / 2.0, gap], 2);
        let minus = boolean(&a, &b.welded(1e-12), Op::Difference).unwrap();
        let hole = (s / 4.0) * (s / 4.0);
        assert!((minus.area() - (s * s - hole)).abs() < 1e-9, "{}", minus.area());
    }

    #[test]
    fn sheets_off_each_others_plane_say_why_they_cannot_cut() {
        let a = flat([0.0, 0.0], [4.0, 4.0], 1);
        let mut b = Mesh::default();
        b.push_quad([0.0, 0.0, 1.0], [4.0, 0.0, 1.0], [4.0, 4.0, 1.0], [0.0, 4.0, 1.0], 2);
        let b = b.welded(1e-12);
        for op in [Op::Difference, Op::Intersection, Op::Exclusion] {
            assert!(boolean(&a, &b, op).unwrap_err().to_string().contains("one plane"), "{op:?}");
        }
        assert!((boolean(&a, &b, Op::Union).unwrap().area() - 32.0).abs() < 1e-9, "joining them still works");
    }

    #[test]
    fn solids_exclude_each_other() {
        let a = cuboid([0.0, 0.0, 0.0], [2.0, 2.0, 2.0], 0);
        let b = cuboid([1.0, 1.0, 1.0], [3.0, 3.0, 3.0], 100);
        let either = boolean(&a, &b, Op::Exclusion).unwrap();
        assert!((either.volume() - (8.0 + 8.0 - 2.0)).abs() < 1e-9, "{}", either.volume());
        assert!(either.is_closed());
        assert!(boolean(&a, &flat([0.0, 0.0], [1.0, 1.0], 7), Op::Exclusion).is_err(), "a sheet has no inside");
        // Whichever way round, and apart they're simply both.
        assert!((boolean(&b, &a, Op::Exclusion).unwrap().volume() - 14.0).abs() < 1e-9);
        let far = cuboid([9.0, 9.0, 9.0], [10.0, 10.0, 10.0], 200);
        assert!((boolean(&a, &far, Op::Exclusion).unwrap().volume() - 9.0).abs() < 1e-9);
    }
}
