//! Thin-wire solver: triangle basis on straight segments, reduced kernel
//! `R = sqrt(|r-r'|² + a²)`, Galerkin testing.
//!
//! The matrix comes from segment pairs: `A_ab = ∬λ_a(s)·λ_b(s')·G ds ds'`, where
//! `λ_0` and `λ_1` are the two linear shape functions of a segment. Near pairs
//! integrate `1/R` over the source segment in closed form and grade the outer
//! rule towards the source's ends, where the result has its log singularity.

use std::f64::consts::PI;

use anyhow::{Result, bail};

use crate::geometry::{Geometry, P3, PortDef, Shape, add, cross, dot, norm, scale, sub, unit};
use crate::material::{self, Material};
use crate::{C64, EPS0, MU0};

use super::far::{Moment, PlaneWave, wavenumber};
use super::fill::{Dof, fill_symmetric};
use super::quad::{GL3, GL4, GL6, on_interval};
use super::weld::Welder;
use super::{MeshView, Model};

struct Seg {
    ends: [u32; 2],
    p0: P3,
    t: P3,
    len: f64,
    radius: f64,
    material: Option<Material>,
    /// Which line it came from.
    line: usize,
}

/// Segment length over radius below which the reduced kernel collapses: Z goes wrong by
/// tens of ohms, refining makes it worse, and the matrix shows no symptom.
const REFUSE_RATIO: f64 = 2.0;
/// Below this it drifts from the exact kernel.
const WARN_RATIO: f64 = 4.0;
/// How much of a wire has to be that short for it to be the wire's mesh, not one detail.
/// A lone short segment at a feed or join only moves Z by a few percent.
const SHORT_SHARE: f64 = 0.05;
/// Radius over λ at which a wire stops being thin, and a delta gap has no single answer.
const THICK_WIRE: f64 = 0.01;

impl Seg {
    fn at(&self, s: f64) -> P3 {
        add(self.p0, scale(self.t, s))
    }
    fn mid(&self) -> P3 {
        self.at(0.5 * self.len)
    }
}

/// A polyline from the geometry, or a bridge across a port's gap.
struct Line {
    pts: Vec<P3>,
    radius: f64,
    material: Option<Material>,
    name: String,
    /// Added across a port's gap rather than drawn by the user.
    bridge: bool,
}

pub struct WireModel {
    nodes: Vec<P3>,
    segs: Vec<Seg>,
    dofs: Vec<Vec<Dof>>,
    nbasis: usize,
    /// Per port: its unknown, and the sign from the basis direction to a → b.
    feeds: Vec<(usize, f64)>,
    ground: bool,
    /// What the mesh check found short of refusing.
    notes: Vec<String>,
}

/// Mirror image in z = 0.
fn reflect(p: P3) -> P3 {
    [p[0], p[1], -p[2]]
}

fn closest_on_segment(c: P3, a: P3, b: P3) -> P3 {
    let d = sub(b, a);
    let l2 = dot(d, d);
    if l2 <= 0.0 {
        return a;
    }
    add(a, scale(d, (dot(sub(c, a), d) / l2).clamp(0.0, 1.0)))
}

fn distance(a: P3, b: P3) -> f64 {
    norm(sub(a, b))
}

impl WireModel {
    /// Discretises every wire of `geom` into segments no longer than `delta`.
    pub fn build(geom: &Geometry, delta: f64) -> Result<Self> {
        let (lo, hi) = geom.bounds();
        let tol = 1e-9 * norm(sub(hi, lo)).max(1e-9);
        let mut lines = read_lines(geom, tol)?;
        if geom.ground {
            for s in &geom.solids {
                if let Shape::Wire { pts, .. } = &s.shape
                    && pts.iter().any(|p| p[2] < -tol)
                {
                    bail!("wire '{}' goes below the ground plane: everything has to stay at z ≥ 0", s.name);
                }
            }
        }
        let mut feed_points = Vec::with_capacity(geom.ports.len());
        for port in &geom.ports {
            feed_points.push(place_port(port, &mut lines, geom.ground, delta, tol)?);
        }

        let mut weld = Welder::new(tol);
        let mut segs: Vec<Seg> = Vec::new();
        for li in 0..lines.len() {
            let line = &lines[li];
            for leg in 0..line.pts.len() - 1 {
                let (a, b) = (line.pts[leg], line.pts[leg + 1]);
                let len = distance(a, b);
                if len <= tol {
                    continue;
                }
                let mut stops = vec![0.0, 1.0];
                let d = scale(sub(b, a), 1.0 / len);
                let mut split_at = |p: P3| {
                    let s = dot(sub(p, a), d);
                    if s > tol && s < len - tol && distance(add(a, scale(d, s)), p) <= tol {
                        stops.push(s / len);
                    }
                };
                feed_points.iter().for_each(|p| split_at(*p));
                // A wire ending on this leg's interior joins it there.
                for other in &lines {
                    other.pts.iter().for_each(|p| split_at(*p));
                }
                stops.sort_by(f64::total_cmp);
                stops.dedup_by(|x, y| (*x - *y) * len <= tol);
                let single_leg = line.pts.len() == 2 && stops.len() == 2;
                for w in stops.windows(2) {
                    let part = (w[1] - w[0]) * len;
                    let mut n = ((part / delta - 1e-9).ceil() as usize).max(1);
                    if single_leg {
                        n = n.max(2);
                    }
                    let mut prev = weld.id(add(a, scale(d, w[0] * len)));
                    for k in 1..=n {
                        let u = w[0] + (w[1] - w[0]) * k as f64 / n as f64;
                        let id = weld.id(add(a, scale(d, u * len)));
                        if id != prev {
                            segs.push(make_seg(&weld, prev, id, line, li));
                        }
                        prev = id;
                    }
                }
            }
        }
        let mut nodes = weld.pts.clone();
        if geom.ground {
            for n in &mut nodes {
                if n[2].abs() <= tol {
                    n[2] = 0.0;
                }
            }
            // A segment lying in the plane is shorted by it.
            segs.retain(|s| !(nodes[s.ends[0] as usize][2] == 0.0 && nodes[s.ends[1] as usize][2] == 0.0));
            if segs.is_empty() {
                bail!("every wire lies in the ground plane, where it carries no current");
            }
            for s in &mut segs {
                s.p0 = nodes[s.ends[0] as usize];
            }
        }

        let feed_nodes: Vec<u32> = feed_points
            .iter()
            .zip(&geom.ports)
            .map(|(p, port)| match weld.find(*p) {
                Some(id) => Ok(id),
                None => bail!("port '{}' could not be joined to the wire", port.name),
            })
            .collect::<Result<_>>()?;
        for (i, n) in feed_nodes.iter().enumerate() {
            if let Some(j) = feed_nodes[..i].iter().position(|m| m == n) {
                bail!("ports '{}' and '{}' feed the same point of the wire", geom.ports[j].name, geom.ports[i].name);
            }
        }

        let notes = check_thin_wire(&lines, &segs)?;
        let (dofs, nbasis, feeds) = make_basis(&nodes, &segs, &feed_nodes, &geom.ports, geom.ground)?;
        Ok(Self { nodes, segs, dofs, nbasis, feeds, ground: geom.ground, notes })
    }

    /// Current at both ends of a segment.
    fn end_currents(&self, seg: usize, x: &[C64]) -> [C64; 2] {
        let mut i = [C64::new(0.0, 0.0); 2];
        for d in &self.dofs[seg] {
            i[d.local as usize] += x[d.basis as usize] * d.weight;
        }
        i
    }
}

fn make_seg(weld: &Welder, a: u32, b: u32, line: &Line, index: usize) -> Seg {
    let (pa, pb) = (weld.pts[a as usize], weld.pts[b as usize]);
    Seg { ends: [a, b], p0: pa, t: unit(sub(pb, pa)), len: distance(pa, pb), radius: line.radius, material: line.material.clone(), line: index }
}

/// Refuses a wire meshed finer than the reduced kernel can follow, and says what is
/// marginal. A bridge across a gap is only ever noted: it is a small part of the wire
/// it feeds, and a gap of 2.5 radii still gives Z within 2 Ω.
fn check_thin_wire(lines: &[Line], segs: &[Seg]) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let (mut total, mut under_refuse, mut under_warn, mut shortest) = (0.0, 0.0, 0.0, f64::INFINITY);
        for s in segs.iter().filter(|s| s.line == li) {
            let ratio = s.len / s.radius;
            total += s.len;
            shortest = f64::min(shortest, ratio);
            if ratio < REFUSE_RATIO {
                under_refuse += s.len;
            }
            if ratio < WARN_RATIO {
                under_warn += s.len;
            }
        }
        if total <= 0.0 {
            continue;
        }
        if line.bridge {
            if shortest < REFUSE_RATIO {
                notes.push(format!("The feed gap at {} is only {:.1} wire radii across: the thin-wire kernel is at its limit there, so expect Z to shift by a few percent. Widen the gap, or run the wire through the feed.", line.name, 2.0 * shortest));
            }
        } else if under_refuse >= SHORT_SHARE * total {
            bail!(
                "wire '{}': its segments are only {shortest:.1} times its radius long ({:.3} mm radius). The thin-wire kernel collapses below {REFUSE_RATIO}, and finer meshes make it worse: use fewer segments per wavelength, a thinner wire, or draw it as a surface.",
                line.name,
                line.radius * 1e3
            );
        } else if under_warn >= SHORT_SHARE * total {
            notes.push(format!("Wire '{}': its segments are only {shortest:.1} times its radius long. The thin-wire kernel starts to drift below {WARN_RATIO}, so a finer mesh won't help here.", line.name));
        } else if shortest < REFUSE_RATIO {
            notes.push(format!("Wire '{}' has a short segment {shortest:.1} times its radius long, at a feed or a join: expect Z to shift by a few percent.", line.name));
        }
    }
    Ok(notes)
}

fn read_lines(geom: &Geometry, tol: f64) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for s in &geom.solids {
        let Shape::Wire { pts, r } = &s.shape else { bail!("'{}' is not a wire", s.name) };
        if !(*r > 0.0) {
            bail!("wire '{}' needs a radius above zero", s.name);
        }
        let mut clean: Vec<P3> = Vec::with_capacity(pts.len());
        for p in pts {
            if clean.last().is_none_or(|q| distance(*q, *p) > tol) {
                clean.push(*p);
            }
        }
        if clean.len() < 2 {
            bail!("wire '{}' needs at least two distinct points", s.name);
        }
        lines.push(Line { pts: clean, radius: *r, material: material::by_name(&s.material), name: s.name.clone(), bridge: false });
    }
    Ok(lines)
}

/// Finds where a port feeds, adding a bridge wire if the wires stop at the ends
/// of its gap. Returns the feed point, which `build` turns into a node.
fn place_port(port: &PortDef, lines: &mut Vec<Line>, ground: bool, delta: f64, tol: f64) -> Result<P3> {
    let c = port.center();
    let half_gap = 0.5 * distance(port.a, port.b);

    let nearest = |p: P3, lines: &[Line]| -> Option<(f64, P3, f64)> {
        let mut best: Option<(f64, P3, f64)> = None;
        for line in lines {
            for w in line.pts.windows(2) {
                let q = closest_on_segment(p, w[0], w[1]);
                let d = distance(p, q);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, q, line.radius));
                }
            }
        }
        best
    };
    let Some((d, on_wire, radius)) = nearest(c, lines) else { bail!("port '{}' has no wire to feed", port.name) };

    // Wires stopping at both ends of the gap, or one wire and the ground plane.
    let touches = |p: P3| -> Option<f64> {
        if ground && p[2].abs() <= tol {
            return Some(0.0);
        }
        nearest(p, lines).and_then(|(dist, _, r)| (dist <= tol.max(r)).then_some(r))
    };
    // The gap's middle is off the wires but inside their thickness, next to where one
    // ends: they stop too close to each other to bridge.
    let at_an_end = lines.iter().any(|l| [l.pts[0], l.pts[l.pts.len() - 1]].iter().any(|e| distance(*e, on_wire) <= tol));
    if d > tol && d <= radius && at_an_end && half_gap > tol && touches(port.a).is_some() && touches(port.b).is_some() {
        bail!("port '{}': the wires stop {:.3} mm apart, closer than their {:.3} mm diameter, and a gap that small can't be modelled with a thin wire. Widen the gap, or run the wire through the feed.", port.name, distance(port.a, port.b) * 1e3, 2e3 * radius);
    }
    if d <= tol.max(radius) {
        return Ok(snap_to_vertex(on_wire, lines, half_gap, radius, tol));
    }
    if let (Some(ra), Some(rb)) = (touches(port.a), touches(port.b)) {
        let r = if ra > 0.0 && rb > 0.0 { ra.min(rb) } else { ra.max(rb) };
        if r > 0.0 {
            let material = lines.first().and_then(|l| l.material.clone());
            lines.push(Line { pts: vec![port.a, c, port.b], radius: r, material, name: format!("port '{}'", port.name), bridge: true });
            return Ok(c);
        }
    }
    if d <= half_gap.max(delta) {
        return Ok(snap_to_vertex(on_wire, lines, half_gap, radius, tol));
    }
    bail!("port '{}' is {:.3} mm from the nearest wire: move it onto a wire, or make its ends touch two wires", port.name, d * 1e3)
}

/// A feed point this close to an existing vertex uses that vertex, which spares
/// the mesh a segment far shorter than the wire is thick.
fn snap_to_vertex(p: P3, lines: &[Line], half_gap: f64, radius: f64, tol: f64) -> P3 {
    let reach = tol.max(half_gap.min(0.25 * radius));
    let mut best = (reach, p);
    for line in lines {
        for v in &line.pts {
            let d = distance(*v, p);
            if d <= best.0 {
                best = (d, *v);
            }
        }
    }
    best.1
}

/// Builds the unknowns: `N - 1` at a node with `N` segments, one per segment at
/// a node on the ground plane, none at a free end.
fn make_basis(nodes: &[P3], segs: &[Seg], feed_nodes: &[u32], ports: &[PortDef], ground: bool) -> Result<(Vec<Vec<Dof>>, usize, Vec<(usize, f64)>)> {
    let mut attached: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, s) in segs.iter().enumerate() {
        attached[s.ends[0] as usize].push(i);
        attached[s.ends[1] as usize].push(i);
    }
    // Unit vector out of `node` along a segment, and the end of the segment it is.
    let leaving = |seg: usize, node: usize| -> (P3, u8) {
        let s = &segs[seg];
        if s.ends[0] as usize == node { (s.t, 0) } else { (scale(s.t, -1.0), 1) }
    };
    let mut dofs: Vec<Vec<Dof>> = vec![Vec::new(); segs.len()];
    let mut nbasis = 0usize;
    let mut feeds = vec![(0usize, 0.0); ports.len()];

    for node in 0..nodes.len() {
        let on_ground = ground && nodes[node][2] == 0.0;
        let mut order = attached[node].clone();
        let port_here = feed_nodes.iter().position(|n| *n as usize == node);
        let gap = port_here.map(|p| sub(ports[p].b, ports[p].a)).filter(|d| norm(*d) > 0.0).map(unit);

        // The two segments lined up with the gap carry the port's basis function.
        if let Some(a) = gap {
            if on_ground {
                let best = (0..order.len()).max_by(|i, j| dot(leaving(order[*i], node).0, a).total_cmp(&dot(leaving(order[*j], node).0, a)));
                if let Some(best) = best {
                    order.swap(0, best);
                }
            } else if order.len() > 2 {
                let mut best = (f64::MIN, 0, 1);
                for i in 0..order.len() {
                    for j in 0..order.len() {
                        if i != j {
                            let score = dot(sub(leaving(order[j], node).0, leaving(order[i], node).0), a);
                            if score > best.0 {
                                best = (score, i, j);
                            }
                        }
                    }
                }
                let (bi, bj) = (best.1, best.2);
                order.swap(0, bi);
                let bj = if bj == 0 { bi } else { bj };
                order.swap(1, bj);
            }
        }

        let first = nbasis;
        if on_ground {
            // The image carries the rest of each triangle.
            for &s in &order {
                let (_, end) = leaving(s, node);
                let sign = if end == 0 { 1.0 } else { -1.0 };
                dofs[s].push(Dof { basis: nbasis as u32, local: end, weight: sign });
                nbasis += 1;
            }
        } else if order.len() >= 2 {
            let (_, end0) = leaving(order[0], node);
            for &s in &order[1..] {
                // Current runs along order[0] into the node and out along s.
                let sign_in = if end0 == 1 { 1.0 } else { -1.0 };
                dofs[order[0]].push(Dof { basis: nbasis as u32, local: end0, weight: sign_in });
                let (_, end) = leaving(s, node);
                let sign_out = if end == 0 { 1.0 } else { -1.0 };
                dofs[s].push(Dof { basis: nbasis as u32, local: end, weight: sign_out });
                nbasis += 1;
            }
        }

        if let Some(p) = port_here {
            if nbasis == first {
                bail!("port '{}' sits on the free end of a wire: there is nothing on the far side of its gap", ports[p].name);
            }
            let flow = if on_ground { leaving(order[0], node).0 } else { sub(leaving(order[1], node).0, leaving(order[0], node).0) };
            let sign = match gap {
                Some(a) => {
                    let along = dot(flow, a);
                    if along.abs() < 1e-3 * norm(flow) {
                        bail!("port '{}' runs across its wire: its a → b direction has to follow the wire", ports[p].name);
                    }
                    along.signum()
                }
                None if on_ground => flow[2].signum(),
                None => 1.0,
            };
            feeds[p] = (first, sign);
        }
    }
    Ok((dofs, nbasis, feeds))
}

/// `(exp(-jkR) - 1)/R`, accurate down to R = 0.
#[inline]
pub(super) fn smooth_kernel(k: f64, r: f64) -> C64 {
    let x = k * r;
    if x < 1e-6 {
        return C64::new(-0.5 * k * x, -k);
    }
    let (sh, _) = (0.5 * x).sin_cos();
    C64::new(-2.0 * sh * sh / r, -x.sin() / r)
}

/// Outer-rule panels on a test segment, narrowing with the distance from the
/// source's near-singular points.
fn graded_panels(len: f64, dist: &impl Fn(f64) -> f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut stack = vec![(0.0, len, 0u32)];
    while let Some((a, b, depth)) = stack.pop() {
        let mid = 0.5 * (a + b);
        if b - a > dist(mid) && depth < 14 {
            stack.push((mid, b, depth + 1));
            stack.push((a, mid, depth + 1));
        } else {
            out.push((a, b));
        }
    }
    out
}

/// `A_ab = ∬λ_a(s)·λ_b(s')·G ds ds'` for a test segment against a source running
/// from `q0` along `qt` for `qlen`.
fn segment_pair(test: &Seg, q0: P3, qt: P3, qlen: f64, radius2: f64, k: f64) -> [[C64; 2]; 2] {
    let mut a = [[C64::new(0.0, 0.0); 2]; 2];
    let qmid = add(q0, scale(qt, 0.5 * qlen));
    let gap = distance(test.mid(), qmid) - 0.5 * (test.len + qlen);
    if gap > test.len.max(qlen) {
        for (s, ws) in on_interval(&GL3, 0.0, test.len) {
            let r = test.at(s);
            let l1 = s / test.len;
            for (sq, wq) in on_interval(&GL3, 0.0, qlen) {
                let d = sub(r, add(q0, scale(qt, sq)));
                let rr = (dot(d, d) + radius2).sqrt();
                let (sn, cs) = (k * rr).sin_cos();
                let g = C64::new(cs, -sn) * (ws * wq / rr);
                let m1 = sq / qlen;
                a[0][0] += g * ((1.0 - l1) * (1.0 - m1));
                a[0][1] += g * ((1.0 - l1) * m1);
                a[1][0] += g * (l1 * (1.0 - m1));
                a[1][1] += g * (l1 * m1);
            }
        }
    } else {
        let q1 = add(q0, scale(qt, qlen));
        let aligned = norm(cross(test.t, qt)) < 1e-9 && norm(cross(sub(test.p0, q0), qt)) < 1e-9 * qlen.max(test.len);
        let dist = |s: f64| {
            let r = test.at(s);
            let d = if aligned { distance(r, q0).min(distance(r, q1)) } else { distance(r, closest_on_segment(r, q0, q1)) };
            (d * d + radius2).sqrt()
        };
        for (p0, p1) in graded_panels(test.len, &dist) {
            for (s, ws) in on_interval(&GL6, p0, p1) {
                let r = test.at(s);
                let l1 = s / test.len;
                let inner = source_integrals(r, q0, qt, qlen, radius2, k);
                a[0][0] += inner[0] * (ws * (1.0 - l1));
                a[0][1] += inner[1] * (ws * (1.0 - l1));
                a[1][0] += inner[0] * (ws * l1);
                a[1][1] += inner[1] * (ws * l1);
            }
        }
    }
    let f = 1.0 / (4.0 * PI);
    [[a[0][0] * f, a[0][1] * f], [a[1][0] * f, a[1][1] * f]]
}

/// `∫λ_b(s')·exp(-jkR)/R ds'` from one observation point: the `1/R` part in
/// closed form, the smooth rest by Gauss-Legendre, split where R has its kink.
fn source_integrals(r: P3, q0: P3, qt: P3, qlen: f64, radius2: f64, k: f64) -> [C64; 2] {
    let w = sub(r, q0);
    let s0 = dot(w, qt);
    let d2 = (dot(w, w) - s0 * s0).max(0.0) + radius2;
    let d = d2.sqrt();
    let i0 = ((qlen - s0) / d).asinh() + (s0 / d).asinh();
    let r_end = ((qlen - s0) * (qlen - s0) + d2).sqrt();
    let r_start = (s0 * s0 + d2).sqrt();
    let i1 = r_end - r_start + s0 * i0;
    let mut out = [C64::new(i0 - i1 / qlen, 0.0), C64::new(i1 / qlen, 0.0)];
    let mut add_part = |a: f64, b: f64| {
        for (s, ws) in on_interval(&GL4, a, b) {
            let rr = ((s - s0) * (s - s0) + d2).sqrt();
            let g = smooth_kernel(k, rr) * ws;
            let m1 = s / qlen;
            out[0] += g * (1.0 - m1);
            out[1] += g * m1;
        }
    };
    if s0 > 0.0 && s0 < qlen {
        add_part(0.0, s0);
        add_part(s0, qlen);
    } else {
        add_part(0.0, qlen);
    }
    out
}

impl Model for WireModel {
    fn unknowns(&self) -> usize {
        self.nbasis
    }

    fn matrix(&self, f: f64) -> Vec<C64> {
        let w = 2.0 * PI * f;
        let k = wavenumber(f);
        let jwmu = C64::new(0.0, w * MU0);
        let over_jwe = C64::new(0.0, -1.0 / (w * EPS0));
        let segs = &self.segs;
        let ground = self.ground;
        fill_symmetric::<2>(self.nbasis, &self.dofs, |p, q| {
            let (sp, sq) = (&segs[p], &segs[q]);
            let radius2 = 0.5 * (sp.radius * sp.radius + sq.radius * sq.radius);
            let charge = |a: &[[C64; 2]; 2]| a[0][0] + a[0][1] + a[1][0] + a[1][1];
            let slope = |i: usize| if i == 1 { 1.0 } else { -1.0 };
            let div = |i: usize, j: usize| slope(i) * slope(j) / (sp.len * sq.len);

            let free = segment_pair(sp, sq.p0, sq.t, sq.len, radius2, k);
            let (tt, phi) = (dot(sp.t, sq.t), charge(&free));
            let mut b = [[C64::new(0.0, 0.0); 2]; 2];
            for i in 0..2 {
                for j in 0..2 {
                    b[i][j] = jwmu * free[i][j] * tt + over_jwe * phi * div(i, j);
                }
            }
            if ground {
                // The image of a segment carries minus the reflected current.
                let (q0, qt) = (reflect(sq.p0), reflect(sq.t));
                let img = segment_pair(sp, q0, qt, sq.len, radius2, k);
                let (tt, phi) = (dot(sp.t, qt), charge(&img));
                for i in 0..2 {
                    for j in 0..2 {
                        b[i][j] -= jwmu * img[i][j] * tt + over_jwe * phi * div(i, j);
                    }
                }
            }
            if p == q
                && let Some(m) = &sp.material
            {
                let per_length = m.surface_impedance(f) / (2.0 * PI * sp.radius);
                for i in 0..2 {
                    for j in 0..2 {
                        b[i][j] += per_length * (sp.len * if i == j { 1.0 / 3.0 } else { 1.0 / 6.0 });
                    }
                }
            }
            b
        })
    }

    fn port_rhs(&self, port: usize) -> Vec<C64> {
        let mut v = vec![C64::new(0.0, 0.0); self.nbasis];
        let (basis, sign) = self.feeds[port];
        v[basis] = C64::new(sign, 0.0);
        v
    }

    fn port_current(&self, port: usize, x: &[C64]) -> C64 {
        let (basis, sign) = self.feeds[port];
        x[basis] * sign
    }

    fn plane_wave_rhs(&self, wave: &PlaneWave) -> Vec<C64> {
        let mut v = vec![C64::new(0.0, 0.0); self.nbasis];
        for (seg, dofs) in self.segs.iter().zip(&self.dofs) {
            for (s, ws) in on_interval(&GL4, 0.0, seg.len) {
                let e = wave.at(seg.at(s));
                let along = e[0] * seg.t[0] + e[1] * seg.t[1] + e[2] * seg.t[2];
                let l1 = s / seg.len;
                for d in dofs {
                    let shape = if d.local == 1 { l1 } else { 1.0 - l1 };
                    v[d.basis as usize] += along * (ws * shape * d.weight);
                }
            }
        }
        v
    }

    fn moments(&self, x: &[C64]) -> Vec<Moment> {
        let mut out = Vec::with_capacity(self.segs.len() * 3 * if self.ground { 2 } else { 1 });
        for (i, seg) in self.segs.iter().enumerate() {
            let [i0, i1] = self.end_currents(i, x);
            for (s, ws) in on_interval(&GL3, 0.0, seg.len) {
                let l1 = s / seg.len;
                let current = (i0 * (1.0 - l1) + i1 * l1) * ws;
                let m = Moment { pos: seg.at(s), j: [current * seg.t[0], current * seg.t[1], current * seg.t[2]] };
                out.push(m);
                if self.ground {
                    out.push(m.image());
                }
            }
        }
        out
    }

    fn element_currents(&self, x: &[C64]) -> Vec<f64> {
        (0..self.segs.len())
            .map(|i| {
                let [a, b] = self.end_currents(i, x);
                (a + b).norm() * 0.5
            })
            .collect()
    }

    fn view(&self) -> MeshView {
        MeshView { verts: self.nodes.clone(), tris: Vec::new(), segments: self.segs.iter().map(|s| s.ends).collect() }
    }

    fn warnings(&self, lambda: f64) -> Vec<String> {
        let mut out = self.notes.clone();
        let thickest = self.segs.iter().map(|s| s.radius).fold(0.0, f64::max);
        if thickest > THICK_WIRE * lambda {
            out.push(format!("The thickest wire's radius is {:.3} λ at the top of the sweep, above the {THICK_WIRE} λ where the thin-wire model holds. A delta gap has no single answer at that thickness: draw it as a surface or use FDTD.", thickest / lambda));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MomOptions, MomProblem};
    use super::*;
    use crate::C0;
    use crate::farfield::{Pol, Quantity};
    use crate::geometry::Solid;

    /// λ = 1 m throughout.
    const F: f64 = C0;

    fn wire(name: &str, material: &str, pts: Vec<P3>, r: f64) -> Solid {
        Solid { name: name.into(), material: material.into(), shape: Shape::Wire { pts, r } }
    }
    fn port(a: P3, b: P3, z0: f64) -> PortDef {
        PortDef { name: "feed".into(), a, b, width: 0.0, z0 }
    }
    fn dipole(len: f64, a: f64, material: &str) -> Geometry {
        Geometry {
            solids: vec![wire("dipole", material, vec![[0.0, 0.0, -len / 2.0], [0.0, 0.0, len / 2.0]], a)],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0)],
            ..Default::default()
        }
    }
    fn opts(spl: f64) -> MomOptions {
        MomOptions { segments_per_lambda: spl, ..Default::default() }
    }
    fn solve(g: &Geometry, spl: f64, far: bool) -> super::super::MomPoint {
        MomProblem::new(g, F, opts(spl)).unwrap().solve(F, far).unwrap()
    }

    #[test]
    fn t01_half_wave_dipole() {
        let p = solve(&dipole(0.5, 0.001, "PEC"), 20.0, true);
        let z = p.z[0][0];
        let m = p.far[0].metrics();
        let ti = 0;
        let null = p.far[0].value_db(ti, 0, Quantity::Directivity, Pol::Total);
        println!("T1 dipole Z = {z:.2}, D = {:.3} dBi at θ = {}, axis {:.1} dB", m.peak_directivity_dbi, m.peak_theta, null);
        // This mesh's own value, kept from drifting. The converged value is in the next test:
        // 20 segments per λ sits about 6 Ω low in R and 6 Ω low in X.
        assert!((z - C64::new(82.27, 40.82)).norm() < 0.1, "{z}");
        assert!((m.peak_directivity_dbi - 2.15).abs() < 0.1, "{}", m.peak_directivity_dbi);
        assert!((m.peak_theta - 90.0).abs() < 1e-6);
        assert!(null < -40.0, "field on the axis: {null} dB");
    }

    #[test]
    fn t02_convergence_with_segment_count() {
        let g = dipole(0.5, 0.001, "PEC");
        let (a, b) = (solve(&g, 20.0, false).z[0][0], solve(&g, 40.0, false).z[0][0]);
        // The complex change: |Z| alone can hold still while X moves 8 %.
        let change = (b - a).norm() / a.norm();
        println!("T2 Z(20) = {a:.2}, Z(40) = {b:.2}, Z changes {:.2} %", change * 100.0);
        assert!(change < 0.05, "{change}");
    }

    #[test]
    fn t03_power_is_conserved() {
        let z0: f64 = 73.0;
        let p = solve(&dipole(0.5, 0.001, "PEC"), 20.0, true);
        let z = p.z[0][0];
        let ff = &p.far[0];
        // 1 W available: V and I at the terminals from the source of impedance z0.
        let vs = 2.0 * (2.0 * z0).sqrt();
        let i = vs / (z + z0);
        let v = z * i;
        let terminal = 0.5 * (v * i.conj()).re;
        let ratio = ff.p_rad() / terminal;
        println!("T3 ½Re(VI*) = {terminal:.5} W, p_rad = {:.5} W, ratio {ratio:.5}, rad_eff {:.5}", ff.p_rad(), ff.rad_efficiency());
        assert!((ratio - 1.0).abs() < 0.01, "{ratio}");
        assert!((ff.rad_efficiency() - 1.0).abs() < 0.01, "{}", ff.rad_efficiency());
        // A bent and a junctioned antenna conserve power too.
        let vee = Geometry {
            solids: vec![wire("vee", "PEC", vec![[0.2, 0.0, 0.2], [0.0, 0.0, 0.0], [-0.2, 0.0, 0.2]], 0.002), wire("stub", "PEC", vec![[0.0, 0.0, 0.0], [0.0, 0.0, -0.15]], 0.002)],
            ports: vec![port([0.0, 0.0, -0.1], [0.0, 0.0, -0.09], 50.0)],
            ..Default::default()
        };
        let p = solve(&vee, 30.0, true);
        println!("T3 vee rad_eff {:.5}", p.far[0].rad_efficiency());
        assert!((p.far[0].rad_efficiency() - 1.0).abs() < 0.01, "{}", p.far[0].rad_efficiency());
    }

    fn monopole(h: f64, a: f64) -> Geometry {
        Geometry { solids: vec![wire("m", "PEC", vec![[0.0, 0.0, 0.0], [0.0, 0.0, h]], a)], ports: vec![port([0.0, 0.0, 0.0], [0.0, 0.0, 0.01], 36.0)], ground: true, ..Default::default() }
    }

    #[test]
    fn t04_monopole_over_ground_is_half_a_dipole() {
        let half = solve(&dipole(0.5, 0.001, "PEC"), 20.0, false).z[0][0] * 0.5;
        let fine = MomOptions { segments_per_lambda: 20.0, theta_step: 1.0, phi_step: 3.0, ..Default::default() };
        let p = MomProblem::new(&monopole(0.25, 0.001), F, fine).unwrap().solve(F, true).unwrap();
        let coarse = solve(&monopole(0.25, 0.001), 20.0, true);
        let z = p.z[0][0];
        let m = p.far[0].metrics();
        let mc = coarse.far[0].metrics();
        let delta = (z - half).norm() / half.norm();
        let below = p.far[0].e_theta[p.far[0].theta.len() - 1][0].norm() + p.far[0].e_theta[120][7].norm();
        println!("T4 monopole Z = {z:.2} vs half dipole {half:.2} ({:.2} %), D = {:.3} dBi (1° grid), {:.3} dBi (3° grid), rad_eff {:.4} (1°), {:.4} (3°)", delta * 100.0, m.peak_directivity_dbi, mc.peak_directivity_dbi, m.rad_efficiency, mc.rad_efficiency);
        assert!(delta < 0.01, "{delta}");
        assert!((m.peak_directivity_dbi - 5.15).abs() < 0.05, "{}", m.peak_directivity_dbi);
        assert!((mc.peak_directivity_dbi - 5.15).abs() < 0.05, "3° grid: {}", mc.peak_directivity_dbi);
        assert_eq!(below, 0.0);
        assert!(p.far[0].ground);
    }

    #[test]
    fn t05_mutual_impedance_and_reciprocity() {
        let a = 0.0001;
        let el = |x: f64, name: &str| wire(name, "PEC", vec![[x, 0.0, -0.25], [x, 0.0, 0.25]], a);
        let g = Geometry {
            solids: vec![el(0.0, "one"), el(0.5, "two")],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0), port([0.5, 0.0, -0.005], [0.5, 0.0, 0.005], 73.0)],
            ..Default::default()
        };
        let p = solve(&g, 40.0, false);
        let (z21, z12) = (p.z[1][0], p.z[0][1]);
        // Converged for this radius. The textbook −12.5 − j29.9 is the limit of a → 0.
        let want = C64::new(-16.85, -31.36);
        let err = (z21 - want).norm() / want.norm();
        let rec = (z21 - z12).norm() / z21.norm();
        println!("T5 Z21 = {z21:.3} (target {want}), error {:.1} %, reciprocity {rec:.2e}", err * 100.0);
        assert!((z21 - want).norm() < 0.5, "{z21}");
        assert!(rec < 1e-3, "{rec}");
    }

    /// Converged values from an exact-kernel Galerkin solver and a Hallén solver, which
    /// agree to 0.02 Ω at this density. 20 segments per λ sits about 8 Ω from them.
    #[test]
    fn half_wave_dipole_is_pinned_to_the_converged_impedance() {
        let z = solve(&dipole(0.5, 0.001, "PEC"), 160.0, false).z[0][0];
        let want = C64::new(86.2, 46.2);
        println!("dipole at 160 segments/λ: {z:.3}, reference {want}");
        assert!((z - want).norm() < 1.0, "{z}");
    }

    /// The same reference for mutual impedance at a = 10⁻⁴ λ, not the a → 0 textbook value.
    #[test]
    fn mutual_impedance_is_pinned_to_the_converged_value() {
        let el = |x: f64, name: &str| wire(name, "PEC", vec![[x, 0.0, -0.25], [x, 0.0, 0.25]], 1e-4);
        let g = Geometry {
            solids: vec![el(0.0, "one"), el(0.5, "two")],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0), port([0.5, 0.0, -0.005], [0.5, 0.0, 0.005], 73.0)],
            ..Default::default()
        };
        let z21 = solve(&g, 80.0, false).z[1][0];
        let want = C64::new(-16.85, -31.36);
        println!("Z21 at 80 segments/λ: {z21:.3}, reference {want}");
        assert!((z21 - want).norm() < 0.2, "{z21}");
    }

    /// The reduced kernel keeps R from following the distance once Δ nears a: Z collapses
    /// and refining makes it worse, with no numerical symptom.
    #[test]
    fn a_mesh_finer_than_the_wire_is_thick_is_refused() {
        // Δ/a = 0.83.
        let e = MomProblem::new(&dipole(0.5, 0.05, "PEC"), F, opts(24.0)).err().expect("Δ/a below 2 must be refused").to_string();
        assert!(e.contains("radius"), "{e}");
        // Refining does not get out of it: Δ/a = 1.25 at 80 segments/λ on a = 0.01 λ.
        assert!(MomProblem::new(&dipole(0.5, 0.01, "PEC"), F, opts(80.0)).is_err());
        // Thin against the mesh and against the wavelength: nothing to say.
        let fine = MomProblem::new(&dipole(0.5, 0.001, "PEC"), F, opts(24.0)).unwrap();
        assert!(fine.warnings().is_empty(), "{:?}", fine.warnings());
    }

    #[test]
    fn a_marginal_wire_is_warned_about() {
        let says = |a: f64, spl: f64, word: &str| {
            let w = MomProblem::new(&dipole(0.5, a, "PEC"), F, opts(spl)).unwrap().warnings();
            println!("a = {a} λ at {spl}/λ: {w:?}");
            w.iter().any(|t| t.contains(word))
        };
        assert!(says(0.015, 24.0, "segment"), "Δ/a = 2.8");
        assert!(says(0.015, 24.0, "thin-wire"), "a/λ above 0.01");
        assert!(!says(0.005, 24.0, "thin-wire"));
        assert!(says(0.001, 8.0, "per wavelength"), "too coarse for the resonance");
    }

    /// Wires stopping at a small gap: below four radii it is called out, below the
    /// diameter there is nothing a thin wire can model, and it is refused by name.
    #[test]
    fn a_feed_gap_too_small_for_the_wire_is_called_out() {
        let bridged = |g: f64| {
            let (a, h) = (0.001, g / 2.0);
            let geom = Geometry {
                solids: vec![wire("upper", "PEC", vec![[0.0, 0.0, h], [0.0, 0.0, 0.25]], a), wire("lower", "PEC", vec![[0.0, 0.0, -0.25], [0.0, 0.0, -h]], a)],
                ports: vec![port([0.0, 0.0, -h], [0.0, 0.0, h], 73.0)],
                ..Default::default()
            };
            MomProblem::new(&geom, F, opts(24.0)).map(|p| p.warnings())
        };
        assert!(bridged(0.01).unwrap().is_empty(), "{:?}", bridged(0.01));
        assert!(bridged(0.003).unwrap().iter().any(|w| w.contains("gap")), "{:?}", bridged(0.003));
        for g in [0.0015, 0.0005, 0.0002] {
            let e = bridged(g).err().expect("a gap under the diameter").to_string();
            assert!(e.contains("diameter"), "{e}");
        }
        // A port on a continuous wire is a point feed, whatever gap it names.
        let p = MomProblem::new(&dipole(0.5, 0.001, "PEC"), F, opts(24.0)).unwrap();
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
    }

    #[test]
    fn an_unknown_material_is_refused_not_treated_as_pec() {
        let e = MomProblem::new(&dipole(0.5, 0.001, "Unobtainium"), F, opts(20.0)).err().expect("an unknown material").to_string();
        assert!(e.contains("Unobtainium"), "{e}");
    }

    /// A closed loop has a current with no charge, so far below its resonance the matrix goes
    /// singular with the square of its size in wavelengths. That shows in the warnings.
    #[test]
    fn a_near_singular_matrix_is_warned_about() {
        let s = 0.14;
        let closed = vec![[-s, 0.0, -s], [s, 0.0, -s], [s, 0.0, s], [-s, 0.0, s], [-s, 0.0, -s]];
        let g = Geometry { solids: vec![wire("loop", "PEC", closed, 0.002)], ports: vec![port([-s, 0.0, -0.005], [-s, 0.0, 0.005], 130.0)], ..Default::default() };
        let m = MomProblem::new(&g, F, opts(40.0)).unwrap();
        let _ = m.solve(F, false).unwrap();
        assert!(m.warnings().is_empty(), "{:?}", m.warnings());
        let _ = m.solve(F * 1e-5, false).unwrap();
        let (rcond, _) = m.worst_rcond().unwrap();
        println!("rcond far below the design frequency {rcond:.2e}");
        assert!(rcond < 1e-8, "{rcond}");
        assert!(m.warnings().iter().any(|w| w.contains("near singular")), "{:?}", m.warnings());
    }

    /// A negative feed resistance reads as itself, not as 100 % efficient.
    #[test]
    fn accepted_power_keeps_its_sign() {
        let mut s = solve(&dipole(0.5, 0.001, "PEC"), 20.0, true).far.swap_remove(0);
        assert!(s.p_accepted > 0.0);
        s.p_accepted = -0.1;
        assert!(s.rad_efficiency().is_nan());
    }

    /// A survey behind the thin-wire limits. `cargo test -p antenna_core wire_limit_survey -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn wire_limit_survey() {
        for a in [0.001, 0.005, 0.01, 0.02, 0.03, 0.04, 0.05] {
            let z = solve(&dipole(0.5, a, "PEC"), 24.0, false).z[0][0];
            println!("default mesh, a = {a} λ, Δ/a {:.2}: Z = {z:.1}", 1.0 / 24.0 / a);
        }
        for spl in [24.0, 48.0, 100.0] {
            println!("a = 0.01 λ at {spl}/λ: {:.1}", solve(&dipole(0.5, 0.01, "PEC"), spl, false).z[0][0]);
        }
        // One short leg in an otherwise fine mesh, the way a feed or a junction makes one.
        let a = 0.005;
        for s in [0.0, 4.0, 2.0, 1.5, 1.0, 0.5, 0.25] {
            let leg = s * a;
            let mut pts = vec![[0.0, 0.0, -0.25], [0.0, 0.0, 0.0]];
            if leg > 0.0 {
                pts.push([0.0, 0.0, leg]);
            }
            pts.push([0.0, 0.0, 0.25]);
            let g = Geometry { solids: vec![wire("d", "PEC", pts, a)], ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0)], ..Default::default() };
            let p = solve(&g, 24.0, true);
            println!("one leg of {s} a at the feed: Z = {:.2}, rad_eff {:.4}", p.z[0][0], p.far[0].rad_efficiency());
        }
        // Wires stopping at a gap.
        for g in [0.04, 0.01, 0.004, 0.003, 0.0025, 0.0015, 0.0005, 0.0002] {
            let (a, h) = (0.001, g / 2.0);
            let geom = Geometry {
                solids: vec![wire("upper", "PEC", vec![[0.0, 0.0, h], [0.0, 0.0, 0.25]], a), wire("lower", "PEC", vec![[0.0, 0.0, -0.25], [0.0, 0.0, -h]], a)],
                ports: vec![port([0.0, 0.0, -h], [0.0, 0.0, h], 73.0)],
                ..Default::default()
            };
            match MomProblem::new(&geom, F, opts(24.0)).and_then(|m| m.solve(F, false)) {
                Ok(p) => println!("gap {g}: Z = {:.1}", p.z[0][0]),
                Err(e) => println!("gap {g}: {e}"),
            }
        }
        // Every wire template at its own defaults.
        for t in crate::templates::all() {
            let Ok(p) = crate::project::Project::from_template(t.id, None) else { continue };
            let Ok(g) = p.geometry() else { continue };
            if !g.solver_fit().wire_mom {
                continue;
            }
            let lambda = C0 / p.setup.f_hi;
            let m = WireModel::build(&g, lambda / p.setup.segments_per_lambda).unwrap();
            let mut r: Vec<f64> = m.segs.iter().map(|s| s.len / s.radius).collect();
            r.sort_by(f64::total_cmp);
            let (short2, short4) = (r.iter().filter(|x| **x < 2.0).count(), r.iter().filter(|x| **x < 4.0).count());
            let top = m.segs.iter().map(|s| s.radius / lambda).fold(0.0, f64::max);
            let rc = MomProblem::new(&g, p.setup.f_hi, MomOptions { segments_per_lambda: p.setup.segments_per_lambda, ..Default::default() }).unwrap();
            let z = rc.solve(p.f0(), true).unwrap();
            println!("{:<16} {:>4} segs, Δ/a min {:.2} median {:.2}, below 2: {short2}, below 4: {short4}, a/λ {top:.4}, Z {:.1}, eff {:.4}", t.id, r.len(), r[0], r[r.len() / 2], z.z[0][0], z.far[0].rad_efficiency());
        }
    }

    #[test]
    fn t06_yagi_points_at_its_director() {
        let el = |y: f64, len: f64, name: &str| wire(name, "PEC", vec![[0.0, -len / 2.0, y], [0.0, len / 2.0, y]], 0.002);
        let g = Geometry {
            solids: vec![el(-0.2, 0.5, "reflector"), el(0.0, 0.47, "driven"), el(0.2, 0.42, "director")],
            ports: vec![port([0.0, -0.005, 0.0], [0.0, 0.005, 0.0], 50.0)],
            ..Default::default()
        };
        let p = solve(&g, 30.0, true);
        let m = p.far[0].metrics();
        let fb = m.front_to_back_db.unwrap();
        println!("T6 Yagi Z = {:.2}, gain {:.2} dBi at θ = {}, F/B {fb:.2} dB", p.z[0][0], m.peak_gain_dbi, m.peak_theta);
        assert!(m.peak_gain_dbi > 6.0, "{}", m.peak_gain_dbi);
        assert!(m.peak_theta < 1e-6, "peak at θ = {}", m.peak_theta);
        assert!(fb > 8.0, "{fb}");
    }

    fn helix(hand: f64, turns: f64) -> Geometry {
        let r = 1.0 / (2.0 * PI);
        let pitch = 0.23;
        let z0 = 0.03;
        let mut pts = vec![[r, 0.0, 0.0]];
        let n = (turns * 48.0) as usize;
        for k in 0..=n {
            let t = 2.0 * PI * turns * k as f64 / n as f64;
            pts.push([r * t.cos(), hand * r * t.sin(), z0 + pitch * t / (2.0 * PI)]);
        }
        Geometry { solids: vec![wire("helix", "PEC", pts, 0.005)], ports: vec![port([r, 0.0, 0.0], [r, 0.0, 0.0167], 140.0)], ground: true, ..Default::default() }
    }

    #[test]
    fn t07_axial_mode_helix() {
        let right = solve(&helix(1.0, 8.0), 20.0, true);
        let m = right.far[0].metrics();
        let z = right.z[0][0];
        let on_axis = right.far[0].value_db(0, 0, Quantity::Directivity, Pol::Total);
        let ar = right.far[0].axial_ratio_db(0, 0);
        println!(
            "T7 helix Z = {z:.2}, D on axis {on_axis:.2} dBi (peak {:.2} at θ = {}), AR on axis {ar:.2} dB, rhcp_at_peak {}, unknowns {}",
            m.peak_directivity_dbi,
            m.peak_theta,
            m.rhcp_at_peak,
            MomProblem::new(&helix(1.0, 8.0), F, opts(20.0)).unwrap().unknowns()
        );
        assert!((11.0..15.0).contains(&on_axis), "{on_axis}");
        assert!(ar < 3.0, "{ar}");
        assert!((80.0..250.0).contains(&z.re), "{z}");
        assert!(m.rhcp_at_peak);
        let left = solve(&helix(-1.0, 8.0), 20.0, true);
        let ml = left.far[0].metrics();
        println!("T7 mirrored: D {:.2} dBi, rhcp_at_peak {}", ml.peak_directivity_dbi, ml.rhcp_at_peak);
        assert!(!ml.rhcp_at_peak);
    }

    #[test]
    fn t08_a_lossy_wire_loses_power() {
        // A thin wire, so the difference is big enough to measure.
        let eff = |material: &str| solve(&dipole(0.5, 1e-4, material), 20.0, true).far[0].rad_efficiency();
        let pec = eff("PEC");
        let (cu, bad) = (eff("Copper"), eff("Stainless steel"));
        let brass = eff("Brass");
        println!("T8 rad_eff: PEC {pec:.5}, copper {cu:.5}, brass {brass:.5}, stainless {bad:.5}");
        assert!((pec - 1.0).abs() < 0.01);
        assert!(cu < pec && brass < cu && bad < brass, "{pec} {cu} {brass} {bad}");
    }

    #[test]
    fn t09_port_sign_and_placement() {
        let g = dipole(0.5, 0.001, "PEC");
        let a = solve(&g, 20.0, false).z[0][0];
        let mut flipped = g.clone();
        let (a0, b0) = (flipped.ports[0].a, flipped.ports[0].b);
        flipped.ports[0].a = b0;
        flipped.ports[0].b = a0;
        let b = solve(&flipped, 20.0, false).z[0][0];
        println!("T9 Z = {a:.3}, swapped {b:.3}");
        assert!((a - b).norm() < 1e-9 * a.norm(), "{a} vs {b}");

        let mut far_away = g.clone();
        far_away.ports[0] = port([1.0, 0.0, -0.005], [1.0, 0.0, 0.005], 73.0);
        let e = MomProblem::new(&far_away, F, opts(20.0)).err().expect("a port off the wire must fail");
        println!("T9 message: {e}");
        assert!(e.to_string().contains("feed"));

        // Mutual terms do change sign when one port is reversed.
        let el = |x: f64, name: &str| wire(name, "PEC", vec![[x, 0.0, -0.25], [x, 0.0, 0.25]], 0.001);
        let two = |flip: bool| Geometry {
            solids: vec![el(0.0, "one"), el(0.3, "two")],
            ports: vec![
                port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0),
                if flip { port([0.3, 0.0, 0.005], [0.3, 0.0, -0.005], 73.0) } else { port([0.3, 0.0, -0.005], [0.3, 0.0, 0.005], 73.0) },
            ],
            ..Default::default()
        };
        let (s, t) = (solve(&two(false), 20.0, false).z[1][0], solve(&two(true), 20.0, false).z[1][0]);
        assert!((s + t).norm() < 1e-9 * s.norm(), "{s} {t}");
    }

    #[test]
    fn wires_stopping_at_the_gap_get_a_bridge() {
        let g = Geometry {
            solids: vec![wire("upper", "PEC", vec![[0.0, 0.0, 0.005], [0.0, 0.0, 0.25]], 0.001), wire("lower", "PEC", vec![[0.0, 0.0, -0.25], [0.0, 0.0, -0.005]], 0.001)],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 73.0)],
            ..Default::default()
        };
        let a = solve(&g, 20.0, false).z[0][0];
        let b = solve(&dipole(0.5, 0.001, "PEC"), 20.0, false).z[0][0];
        println!("bridged gap {a:.2}, continuous wire {b:.2}");
        assert!((a - b).norm() / b.norm() < 0.05, "{a} {b}");
    }

    #[test]
    fn a_wire_ending_on_another_wire_joins_it() {
        // A T: the stub's end sits in the middle of the bar.
        let g = Geometry {
            solids: vec![wire("bar", "PEC", vec![[-0.2, 0.0, 0.0], [0.2, 0.0, 0.0]], 0.002), wire("stub", "PEC", vec![[0.03, 0.0, 0.0], [0.03, 0.0, -0.2]], 0.002)],
            ports: vec![port([0.03, 0.0, -0.1], [0.03, 0.0, -0.09], 50.0)],
            ..Default::default()
        };
        let m = MomProblem::new(&g, F, opts(20.0)).unwrap();
        let view = m.view();
        let touching = view.segments.iter().filter(|s| s.iter().any(|&n| distance(view.verts[n as usize], [0.03, 0.0, 0.0]) < 1e-9)).count();
        assert_eq!(touching, 3);
        let p = m.solve(F, true).unwrap();
        assert!((p.far[0].rad_efficiency() - 1.0).abs() < 0.01, "{}", p.far[0].rad_efficiency());
    }

    #[test]
    fn a_short_free_wire_still_has_an_unknown() {
        let g = Geometry { solids: vec![wire("stub", "PEC", vec![[0.0, 0.0, -0.005], [0.0, 0.0, 0.005]], 0.001)], ..Default::default() };
        assert_eq!(MomProblem::new(&g, F, opts(20.0)).unwrap().unknowns(), 1);
    }

    #[test]
    fn a_wire_below_the_ground_plane_is_an_error() {
        let g = Geometry { solids: vec![wire("low", "PEC", vec![[0.0, 0.0, -0.1], [0.0, 0.0, 0.1]], 0.001)], ground: true, ..Default::default() };
        assert!(MomProblem::new(&g, F, opts(20.0)).is_err());
    }

    #[test]
    fn scattering_of_a_short_wire_matches_the_dipole_rule() {
        // Its polarisation and level broadside, and reciprocity of the bistatic value.
        let g = dipole(0.48, 0.001, "PEC");
        let m = MomProblem::new(&Geometry { ports: vec![], ..g }, F, opts(40.0)).unwrap();
        let unit = C64::new(1.0, 0.0);
        let zero = C64::new(0.0, 0.0);
        let broadside = m.rcs(F, 90.0, 0.0, unit, zero, &[(90.0, 0.0)]).unwrap()[0];
        let cross = m.rcs(F, 90.0, 0.0, zero, unit, &[(90.0, 0.0)]).unwrap()[0];
        // A resonant thin dipole: σ ≈ 0.86 λ² broadside.
        println!("wire RCS broadside {broadside:.4} m², cross-polarised {cross:.2e}");
        assert!((0.6..1.1).contains(&broadside), "{broadside}");
        assert!(cross < 1e-6 * broadside);
        let ab = m.rcs(F, 90.0, 0.0, unit, zero, &[(60.0, 40.0)]).unwrap()[0];
        let ba = m.rcs(F, 60.0, 40.0, unit, zero, &[(90.0, 0.0)]).unwrap()[0];
        assert!((ab - ba).abs() < 1e-3 * ab, "{ab} {ba}");
    }

    /// Port 1 driven, port 2 terminated: what is accepted either radiates or ends
    /// up in the termination.
    #[test]
    fn a_terminated_port_absorbs_the_rest_of_the_accepted_power() {
        let el = |x: f64, name: &str| wire(name, "PEC", vec![[x, 0.0, -0.24], [x, 0.0, 0.24]], 0.001);
        let (z1, z2): (f64, f64) = (73.0, 50.0);
        let g = Geometry {
            solids: vec![el(0.0, "one"), el(0.15, "two")],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], z1), port([0.15, 0.0, -0.005], [0.15, 0.0, 0.005], z2)],
            ..Default::default()
        };
        let p = solve(&g, 30.0, true);
        let z = &p.z;
        // (Z + Z0)·I = Vs·e₁, solved by hand for the 2×2 case.
        let vs = 2.0 * (2.0 * z1).sqrt();
        let (a, b, c, d) = (z[0][0] + z1, z[0][1], z[1][0], z[1][1] + z2);
        let det = a * d - b * c;
        let (i1, i2) = (d * vs / det, -c * vs / det);
        let absorbed = 0.5 * i2.norm_sqr() * z2;
        let (rad, acc) = (p.far[0].p_rad(), p.far[0].p_accepted);
        let v1 = z[0][0] * i1 + z[0][1] * i2;
        let terminal = 0.5 * (v1 * i1.conj()).re;
        println!("two ports: accepted {acc:.4} W (terminals {terminal:.4}), radiated {rad:.4} W, absorbed in port 2 {absorbed:.4} W");
        assert!(absorbed > 0.05, "{absorbed}");
        assert!((terminal - acc).abs() < 1e-6, "{terminal} vs {acc}");
        assert!(((rad + absorbed) / acc - 1.0).abs() < 0.01, "{rad} + {absorbed} vs {acc}");
        assert_eq!(p.far.len(), 2);
    }

    /// Over ground the far field only spans the upper hemisphere, and the power it
    /// carries must still equal what the feed accepts.
    #[test]
    fn power_over_ground_balances() {
        for step in [3.0, 1.0] {
            let o = MomOptions { segments_per_lambda: 20.0, theta_step: step, ..Default::default() };
            let p = MomProblem::new(&monopole(0.25, 0.001), F, o).unwrap().solve(F, true).unwrap();
            let eff = p.far[0].rad_efficiency();
            println!("{step}° grid: rad_eff {eff:.4}");
            assert!((eff - 1.0).abs() < 0.01, "{eff}");
        }
    }

    #[test]
    fn scattering_over_ground_follows_image_theory() {
        // At grazing incidence the wave and its reflection add up along a vertical
        // wire, so a monopole sees twice the field of its free-space double.
        let unit = C64::new(1.0, 0.0);
        let zero = C64::new(0.0, 0.0);
        let free = MomProblem::new(&Geometry { ports: vec![], ..dipole(0.48, 0.001, "PEC") }, F, opts(40.0)).unwrap();
        let over = MomProblem::new(&Geometry { ports: vec![], ..monopole(0.24, 0.001) }, F, opts(40.0)).unwrap();
        let a = free.rcs(F, 90.0, 0.0, unit, zero, &[(90.0, 0.0)]).unwrap()[0];
        let b = over.rcs(F, 90.0, 0.0, unit, zero, &[(90.0, 0.0)]).unwrap()[0];
        println!("grazing RCS over ground {b:.4} m², free-space double {a:.4} m², ratio {:.3} (want 4)", b / a);
        assert!((b / a - 4.0).abs() < 0.05, "{}", b / a);
        // Nothing arrives from under the plane, or leaves below it.
        assert_eq!(over.rcs(F, 120.0, 0.0, unit, zero, &[(30.0, 0.0)]).unwrap()[0], 0.0);
        assert_eq!(over.rcs(F, 30.0, 0.0, unit, zero, &[(120.0, 0.0)]).unwrap()[0], 0.0);
    }

    #[test]
    fn each_wire_on_the_ground_plane_gets_its_own_unknown() {
        let g = Geometry {
            solids: vec![wire("a", "PEC", vec![[0.0, 0.0, 0.0], [0.0, 0.0, 0.2]], 0.001), wire("b", "PEC", vec![[0.0, 0.0, 0.0], [0.1, 0.0, 0.2]], 0.001)],
            ports: vec![port([0.0, 0.0, 0.0], [0.0, 0.0, 0.01], 50.0)],
            ground: true,
            ..Default::default()
        };
        let m = MomProblem::new(&g, F, opts(10.0)).unwrap();
        // Each wire: a base unknown, the feed node on `a`, and interior nodes.
        let view = m.view();
        let base = view.verts.iter().position(|p| norm(*p) < 1e-9).unwrap() as u32;
        assert_eq!(view.segments.iter().filter(|s| s.contains(&base)).count(), 2);
        let p = m.solve(F, true).unwrap();
        assert!(p.z[0][0].re > 0.0, "{}", p.z[0][0]);
    }

    #[test]
    fn a_port_at_a_junction_feeds_the_wires_lined_up_with_it() {
        // A stub below and a vee above, meeting at the feed.
        let g = Geometry {
            solids: vec![wire("stub", "PEC", vec![[0.0, 0.0, -0.2], [0.0, 0.0, 0.0]], 0.002), wire("left", "PEC", vec![[0.0, 0.0, 0.0], [-0.15, 0.0, 0.15]], 0.002), wire("right", "PEC", vec![[0.0, 0.0, 0.0], [0.15, 0.0, 0.15]], 0.002)],
            ports: vec![port([0.0, 0.0, -0.005], [0.0, 0.0, 0.005], 50.0)],
            ..Default::default()
        };
        let p = MomProblem::new(&g, F, opts(30.0)).unwrap().solve(F, true).unwrap();
        println!("junction feed Z = {:.2}, rad_eff {:.4}", p.z[0][0], p.far[0].rad_efficiency());
        assert!(p.z[0][0].re > 0.0, "{}", p.z[0][0]);
        assert!((p.far[0].rad_efficiency() - 1.0).abs() < 0.01, "{}", p.far[0].rad_efficiency());
    }

    #[test]
    fn a_port_at_a_free_end_or_across_the_wire_is_an_error() {
        let mut g = dipole(0.5, 0.001, "PEC");
        g.ports[0] = port([0.0, 0.0, 0.25], [0.0, 0.0, 0.25], 50.0);
        assert!(MomProblem::new(&g, F, opts(20.0)).err().unwrap().to_string().contains("free end"));
        g.ports[0] = port([-0.0005, 0.0, 0.0], [0.0005, 0.0, 0.0], 50.0);
        assert!(MomProblem::new(&g, F, opts(20.0)).err().unwrap().to_string().contains("across"));
    }

    /// The closed form for a straight segment's `1/R` integral against brute force.
    #[test]
    fn near_terms_match_a_refined_quadrature() {
        let a = 0.001;
        let seg = Seg { ends: [0, 1], p0: [0.0; 3], t: [0.0, 0.0, 1.0], len: 0.05, radius: a, material: None, line: 0 };
        let k = 2.0 * PI;
        for (q0, qt, name) in [([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], "self"), ([0.0, 0.0, 0.05], [0.0, 0.0, 1.0], "next"), ([0.0, 0.0, 0.05], [1.0, 0.0, 0.0], "bent"), ([0.01, 0.0, 0.0], [0.0, 0.0, 1.0], "parallel")] {
            let got = segment_pair(&seg, q0, qt, 0.05, a * a, k);
            let n = 4000;
            let h = 0.05 / n as f64;
            let mut want = [[C64::new(0.0, 0.0); 2]; 2];
            for i in 0..n {
                let s = (i as f64 + 0.5) * h;
                for j in 0..n {
                    let sq = (j as f64 + 0.5) * h;
                    let d = sub(seg.at(s), add(q0, scale(qt, sq)));
                    let rr = (dot(d, d) + a * a).sqrt();
                    let g = C64::from_polar(1.0 / rr, -k * rr) * (h * h / (4.0 * PI));
                    let (u, v) = (s / 0.05, sq / 0.05);
                    want[0][0] += g * ((1.0 - u) * (1.0 - v));
                    want[0][1] += g * ((1.0 - u) * v);
                    want[1][0] += g * (u * (1.0 - v));
                    want[1][1] += g * (u * v);
                }
            }
            for i in 0..2 {
                for j in 0..2 {
                    let e = (got[i][j] - want[i][j]).norm() / want[i][j].norm();
                    assert!(e < 2e-4, "{name} [{i}][{j}]: {} vs {}, error {e}", got[i][j], want[i][j]);
                }
            }
        }
    }
}
