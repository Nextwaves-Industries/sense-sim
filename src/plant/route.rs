//! Orthogonal pipe routing between two pipe ports: the run leaves each
//! port straight along its direction, turns only with 90° elbows (bend
//! radius 1.5 d), avoids other objects, and becomes a chain of generated
//! straight pipes and elbows whose ports all mate exactly.

use bevy::prelude::*;

use super::connect::{Link, LinkKind, PortRef};
use super::parts::{Spec, elbow_radius};

/// An obstacle's world bounds and name.
pub struct Obstacle {
    pub name: String,
    pub min: Vec3,
    pub max: Vec3,
}

pub struct Piece {
    pub name: String,
    pub spec: Spec,
    pub transform: Transform,
}

pub struct Route {
    pub pieces: Vec<Piece>,
    pub links: Vec<Link>,
    pub points: Vec<Vec3>,
    pub length: f32,
    pub elbows: usize,
}

/// Snaps a direction to the nearest axis if it is within ~3°.
pub fn axis_of(d: Vec3) -> Option<Vec3> {
    let d = d.normalize_or_zero();
    let a = d.abs();
    let (i, m) = [a.x, a.y, a.z].into_iter().enumerate().max_by(|x, y| x.1.total_cmp(&y.1))?;
    if m < 0.9986 {
        return None;
    }
    let mut v = Vec3::ZERO;
    v[i] = d[i].signum();
    Some(v)
}

fn simplify(pts: Vec<Vec3>) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::new();
    for p in pts {
        if out.last().is_some_and(|q| q.distance(p) < 1e-4) {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (u, v) = ((b - a).normalize(), (p - b).normalize());
            if u.dot(v) > 0.9999 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Checks bends (90° only, no reversals) and room for the elbows.
fn valid(pts: &[Vec3], bend: f32) -> bool {
    if pts.len() < 2 {
        return false;
    }
    for i in 0..pts.len() - 1 {
        let seg = pts[i + 1] - pts[i];
        if axis_of(seg).is_none() {
            return false;
        }
        let need = if i > 0 { bend } else { 0.0 } + if i + 2 < pts.len() { bend } else { 0.0 };
        if seg.length() + 1e-4 < need {
            return false;
        }
        if i + 2 < pts.len() {
            let next = pts[i + 2] - pts[i + 1];
            if seg.normalize().dot(next.normalize()).abs() > 1e-3 {
                return false;
            }
        }
    }
    true
}

fn seg_hits(a: Vec3, b: Vec3, r: f32, o: &Obstacle) -> bool {
    let lo = a.min(b) - Vec3::splat(r);
    let hi = a.max(b) + Vec3::splat(r);
    lo.x < o.max.x && hi.x > o.min.x && lo.y < o.max.y && hi.y > o.min.y && lo.z < o.max.z && hi.z > o.min.z
}

/// Names of obstacles the path runs through. The first and last segments
/// may touch the two objects being connected (they leave from their ports).
fn collisions(pts: &[Vec3], r: f32, obstacles: &[Obstacle], ends: [&str; 2]) -> Vec<String> {
    let mut hit = Vec::new();
    let n = pts.len() - 1;
    for i in 0..n {
        // Shrink each segment a little so neighbours sharing a corner don't count.
        let (a, b) = (pts[i], pts[i + 1]);
        let d = (b - a).normalize_or_zero();
        let (a, b) = (a + d * r * 0.5, b - d * r * 0.5);
        for o in obstacles {
            let at_end = (i == 0 && o.name == ends[0]) || (i == n - 1 && o.name == ends[1]) || (n == 1 && ends.contains(&o.name.as_str()));
            if at_end {
                continue;
            }
            if seg_hits(a, b, r, o) && !hit.contains(&o.name) {
                hit.push(o.name.clone());
            }
        }
    }
    hit
}

fn cost(pts: &[Vec3]) -> f32 {
    pts.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>() + 0.4 * (pts.len().saturating_sub(2)) as f32
}

/// Candidate polylines from s to e (both already outside their objects):
/// every axis order, a detour along one axis to just past an obstacle face
/// (round the side, over the top or under), and lifts over everything.
fn candidates(s: Vec3, e: Vec3, lifts: &[f32], obstacles: &[Obstacle], clearance: f32) -> Vec<Vec<Vec3>> {
    let mut out = Vec::new();
    let orders = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    let reach = |from: Vec3, ord: &[usize; 3], pts: &mut Vec<Vec3>| {
        let mut p = from;
        for &ax in ord {
            p[ax] = e[ax];
            pts.push(p);
        }
    };
    for ord in &orders {
        let mut pts = vec![s];
        reach(s, ord, &mut pts);
        out.push(pts);
    }
    // Detours: the faces of nearby obstacles, pushed out by the clearance.
    let lo = s.min(e) - Vec3::splat(4.0);
    let hi = s.max(e) + Vec3::splat(4.0);
    for ax in 0..3 {
        let mut cs: Vec<f32> = obstacles
            .iter()
            .filter(|o| o.max.x > lo.x && o.min.x < hi.x && o.max.z > lo.z && o.min.z < hi.z)
            .flat_map(|o| [o.min[ax] - clearance, o.max[ax] + clearance])
            .collect();
        if ax == 1 {
            cs.retain(|c| *c > clearance);
            cs.extend_from_slice(lifts);
        }
        cs.sort_by(f32::total_cmp);
        cs.dedup_by(|a, b| (*a - *b).abs() < 0.02);
        for c in cs {
            let mut p = s;
            p[ax] = c;
            for ord in &orders {
                let mut pts = vec![s, p];
                reach(p, ord, &mut pts);
                out.push(pts);
            }
        }
    }
    // Over the top: rise to a clearance height, cross, come down.
    for &y in lifts {
        for first in [0usize, 2] {
            let second = 2 - first;
            let mut p = s;
            let mut pts = vec![s];
            p.y = y;
            pts.push(p);
            p[first] = e[first];
            pts.push(p);
            p[second] = e[second];
            pts.push(p);
            pts.push(e);
            out.push(pts);
        }
    }
    out
}

/// Plans a route between two ports given by world position and outward
/// direction. `via` points are visited in order. `names` gives each new
/// piece a unique object name.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    from: (&PortRef, Vec3, Vec3),
    to: (&PortRef, Vec3, Vec3),
    d: f32,
    via: &[Vec3],
    obstacles: &[Obstacle],
    names: &mut dyn FnMut(&str) -> String,
) -> Result<Route, String> {
    let d0 = axis_of(from.2).ok_or_else(|| format!("{} does not point along an axis: rotate its object to a multiple of 90°", from.0.label()))?;
    let d1 = axis_of(to.2).ok_or_else(|| format!("{} does not point along an axis: rotate its object to a multiple of 90°", to.0.label()))?;
    let bend = elbow_radius(d);
    let stub = bend + (d * 1.5).max(0.08);
    let (p0, p1) = (from.1, to.1);
    let s = p0 + d0 * stub;
    let e = p1 + d1 * stub;
    let top = obstacles.iter().map(|o| o.max.y).fold(s.y.max(e.y), f32::max);
    let r = d * 0.8 + 0.02;

    // Route leg by leg through the via points; each leg picks its best candidate.
    let mut stops = vec![s];
    stops.extend_from_slice(via);
    stops.push(e);
    let mut pts = vec![p0];
    let mut blocked: Vec<String> = Vec::new();
    for (k, w) in stops.windows(2).enumerate() {
        let lifts = [w[0].y.max(w[1].y) + 0.3, top + r + 0.1];
        let mut best: Option<(f32, Vec<Vec3>)> = None;
        for cand in candidates(w[0], w[1], &lifts, obstacles, r + 0.05) {
            let mut trial = pts.clone();
            trial.extend(cand);
            if k + 2 == stops.len() {
                trial.push(p1);
            }
            let trial = simplify(trial);
            // Stay above the floor, with room for the flanges.
            if !valid(&trial, bend) || trial.iter().any(|p| p.y < d * 0.8 + 0.01) {
                continue;
            }
            let hits = collisions(&trial, r, obstacles, [&from.0.object, &to.0.object]);
            if !hits.is_empty() {
                for h in hits {
                    if !blocked.contains(&h) {
                        blocked.push(h);
                    }
                }
                continue;
            }
            let c = cost(&trial);
            if best.as_ref().is_none_or(|b| c < b.0) {
                best = Some((c, trial));
            }
        }
        match best {
            Some((_, t)) => {
                pts = t;
                if k + 2 == stops.len() {
                    pts.pop();
                }
            }
            None => {
                return Err(if blocked.is_empty() {
                    "no orthogonal route fits (ports too close for the elbows): move the objects further apart, or give via points".into()
                } else {
                    format!("every route is blocked by {}: give via points around them, or move them", blocked.join(", "))
                });
            }
        }
    }
    pts.push(p1);
    let pts = simplify(pts);
    if !valid(&pts, bend) {
        return Err("route has no room for its elbows: move the objects further apart".into());
    }
    Ok(build(&pts, d, from.0, to.0, names))
}

/// Turns a valid polyline into pipe and elbow pieces with their links.
pub fn build(pts: &[Vec3], d: f32, from: &PortRef, to: &PortRef, names: &mut dyn FnMut(&str) -> String) -> Route {
    let bend = elbow_radius(d);
    let mut pieces = Vec::new();
    let mut links = Vec::new();
    let mut prev = from.clone();
    let chain = |pieces: &mut Vec<Piece>, links: &mut Vec<Link>, prev: &mut PortRef, name: String, spec: Spec, t: Transform, entry: &str, exit: &str| {
        links.push(Link { a: prev.clone(), b: PortRef::new(&name, entry), kind: LinkKind::Pipe });
        *prev = PortRef::new(&name, exit);
        pieces.push(Piece { name, spec, transform: t });
    };
    let n = pts.len() - 1;
    let mut length = 0.0;
    for i in 0..n {
        let (a, b) = (pts[i], pts[i + 1]);
        let u = (b - a).normalize();
        let start = a + u * if i > 0 { bend } else { 0.0 };
        let end = b - u * if i + 1 < n { bend } else { 0.0 };
        let len = start.distance(end);
        length += a.distance(b);
        if len > 0.005 {
            let spec = Spec::Pipe { d, len }.normalized();
            let ports = spec.ports();
            let pa = &ports[0];
            let rot = Quat::from_rotation_arc(Vec3::X, u);
            let t = Transform::from_translation(start - rot * pa.pos).with_rotation(rot);
            chain(&mut pieces, &mut links, &mut prev, names("pipe"), spec, t, "a", "b");
        }
        if i + 1 < n {
            let u2 = (pts[i + 2] - b).normalize();
            let spec = Spec::Elbow { d, angle: 90.0 }.normalized();
            let ports = spec.ports();
            let rot = Quat::from_mat3(&Mat3::from_cols(u, u2, u.cross(u2)));
            let entry = b - u * bend;
            let t = Transform::from_translation(entry - rot * ports[0].pos).with_rotation(rot);
            chain(&mut pieces, &mut links, &mut prev, names("elbow"), spec, t, "a", "b");
        }
    }
    links.push(Link { a: prev, b: to.clone(), kind: LinkKind::Pipe });
    let elbows = n.saturating_sub(1);
    Route { pieces, links, points: pts.to_vec(), length, elbows }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namer() -> impl FnMut(&str) -> String {
        let mut i = 0;
        move |k: &str| {
            i += 1;
            format!("{k}_{i}")
        }
    }

    /// World pose of a piece's port.
    fn port_world(p: &Piece, id: &str) -> (Vec3, Vec3) {
        let ports = p.spec.ports();
        let port = ports.iter().find(|q| q.id == id).unwrap();
        port.world(&p.transform)
    }

    #[test]
    fn l_shaped_route_chains_exactly() {
        let a = PortRef::new("tank", "outlet");
        let b = PortRef::new("pump", "inlet");
        let mut names = namer();
        let r = plan((&a, Vec3::new(0.0, 0.5, 0.0), Vec3::X), (&b, Vec3::new(3.0, 0.5, 2.0), -Vec3::Z), 0.1, &[], &[], &mut names).unwrap();
        assert_eq!(r.elbows, 1);
        // Every interior link joins two piece ports face to face.
        for l in &r.links {
            let find = |p: &PortRef| r.pieces.iter().find(|x| x.name == p.object).map(|x| port_world(x, &p.port));
            if let (Some(pa), Some(pb)) = (find(&l.a), find(&l.b)) {
                assert!(pa.0.distance(pb.0) < 2e-3, "gap {}", pa.0.distance(pb.0));
                assert!(pa.1.dot(pb.1) < -0.999);
            }
        }
        // The first piece starts at the tank port, the last ends at the pump port.
        let first = port_world(&r.pieces[0], "a");
        assert!(first.0.distance(Vec3::new(0.0, 0.5, 0.0)) < 1e-3);
        let last = port_world(r.pieces.last().unwrap(), "b");
        assert!(last.0.distance(Vec3::new(3.0, 0.5, 2.0)) < 2e-3);
    }

    #[test]
    fn routes_around_obstacles() {
        let a = PortRef::new("a", "p");
        let b = PortRef::new("b", "p");
        let wall = Obstacle { name: "box".into(), min: Vec3::new(1.0, 0.0, -1.0), max: Vec3::new(2.0, 1.5, 1.0) };
        let mut names = namer();
        let r = plan((&a, Vec3::new(0.0, 0.5, 0.0), Vec3::X), (&b, Vec3::new(3.0, 0.5, 0.0), -Vec3::X), 0.05, &[], &[wall], &mut names).unwrap();
        assert!(r.points.iter().any(|p| p.y > 1.5 || p.z.abs() > 1.0), "{:?}", r.points);
        assert!(r.elbows >= 2);
    }
}
