//! Reads 2D CAD (DXF, Gerber) into flat metal the solvers can take.
//!
//! A drawing holds outlines. It doesn't say which layers are metal, how high they
//! sit, or where the antenna is fed, so reading happens in two steps: [`read`]
//! keeps what the file says, then [`RawDrawing::resolve`] turns it into closed
//! outlines in metres and [`add_to_project`] applies the user's choices.
//!
//! Holes become zero-width keyholes in their parent outline, the convention the
//! `slot` template set: the mesher, the triangulator and CSXCAD all read them
//! by the even-odd rule.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::{PI, TAU};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::geometry::P3;
use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Param, Project};

pub type P2 = [f64; 2];

/// Largest angle of one chord when a curve is flattened; simplification thins it.
const ARC_STEP: f64 = 3.0 * PI / 180.0;
/// Past this many blocks inside blocks a drawing is taken to be circular.
const MAX_BLOCK_DEPTH: usize = 16;

/// What a layer is for. Guessed from its name; the user can change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayerRole {
    Metal,
    /// Lines mark gap feeds, points and small circles mark probes.
    Feed,
    /// The board's outline: gives the substrate its shape.
    Board,
    Ignore,
}

impl LayerRole {
    pub const ALL: [LayerRole; 4] = [LayerRole::Metal, LayerRole::Feed, LayerRole::Board, LayerRole::Ignore];
    pub fn label(self) -> &'static str {
        match self {
            LayerRole::Metal => "Metal",
            LayerRole::Feed => "Feeds",
            LayerRole::Board => "Board outline",
            LayerRole::Ignore => "Ignore",
        }
    }
}

/// Guesses a layer's role from its name, word by word, so "support" isn't a port.
pub fn role_for(name: &str) -> LayerRole {
    let lower = name.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let any = |list: &[&str]| words.iter().any(|w| list.contains(w));
    if any(&["feed", "feeds", "port", "ports", "probe", "probes", "excitation", "source"]) {
        LayerRole::Feed
    } else if any(&["board", "outline", "substrate", "pcb", "dielectric", "fr4", "edge", "cuts"]) {
        LayerRole::Board
    } else if any(&["dim", "dims", "dimension", "dimensions", "text", "note", "notes", "title", "border", "frame", "annotation", "annotations", "defpoints", "silk", "silks", "silkscreen", "mask", "paste", "drill", "hidden", "centerline", "construction", "courtyard", "crtyd", "fab"]) {
        LayerRole::Ignore
    } else {
        LayerRole::Metal
    }
}

/// One path as drawn, in file units.
#[derive(Clone, Debug, PartialEq)]
struct RawPath {
    pts: Vec<P2>,
    closed: bool,
    /// Gerber's clear polarity: cuts instead of adds.
    clear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawLayer {
    pub name: String,
    /// Height from the file, in file units.
    pub z: f64,
    pub role: LayerRole,
    /// Hidden or frozen in the drawing.
    pub off: bool,
    /// Nested closed paths alternate metal and hole (DXF). Gerber adds every dark
    /// shape, overlapping or not, and only its clear shapes cut.
    even_odd: bool,
    paths: Vec<RawPath>,
    points: Vec<P2>,
}

impl RawLayer {
    fn new(name: &str, z: f64, even_odd: bool) -> Self {
        Self { name: name.to_string(), z, role: role_for(name), off: false, even_odd, paths: Vec::new(), points: Vec::new() }
    }
    /// How many shapes the file drew on this layer.
    pub fn shapes(&self) -> usize {
        self.paths.len() + self.points.len()
    }
}

/// A file as read: what it says, before any choice is made.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawDrawing {
    pub format: String,
    pub layers: Vec<RawLayer>,
    /// Metres per file unit, when the file says.
    pub units_to_m: Option<f64>,
    pub warnings: Vec<String>,
    /// Entity kinds that hold no antenna outline, with counts.
    pub skipped: BTreeMap<String, usize>,
}

/// Choices that change how outlines are computed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolve {
    pub scale_to_m: f64,
    /// Curves stay this close to the drawing after thinning, metres.
    pub chord_tol: f64,
}

impl Default for Resolve {
    fn default() -> Self {
        Self { scale_to_m: 1e-3, chord_tol: 50e-6 }
    }
}

/// A closed metal outline and the holes cut from it, in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct Island {
    pub outer: Vec<P2>,
    pub holes: Vec<Vec<P2>>,
}

impl Island {
    pub fn area(&self) -> f64 {
        signed_area(&self.outer).abs() - self.holes.iter().map(|h| signed_area(h).abs()).sum::<f64>()
    }
    /// One outline with each hole joined by a zero-width keyhole.
    pub fn keyholed(&self) -> Vec<P2> {
        keyhole(&self.outer, &self.holes)
    }
}

/// A place to feed the antenna, in the drawing's plane, in metres.
#[derive(Clone, Debug, PartialEq)]
pub enum CadFeed {
    /// Across a gap on one layer. `a` and `b` differ along exactly one axis.
    Gap { a: P2, b: P2, width: f64, layer: usize },
    /// A vertical probe between the lowest and highest metal layers.
    Probe { at: P2 },
}

impl CadFeed {
    pub fn describe(&self) -> String {
        match self {
            CadFeed::Gap { a, b, width, .. } => format!("Gap at ({:.2}, {:.2}) mm, {:.2} mm long, {:.2} mm wide", (a[0] + b[0]) * 500.0, (a[1] + b[1]) * 500.0, dist(*a, *b) * 1e3, width * 1e3),
            CadFeed::Probe { at } => format!("Probe at ({:.2}, {:.2}) mm", at[0] * 1e3, at[1] * 1e3),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CadLayer {
    pub name: String,
    /// Height from the file, metres.
    pub z: f64,
    pub role: LayerRole,
    pub off: bool,
    pub islands: Vec<Island>,
    /// Paths that never closed; they carry no area, so they aren't sheets.
    pub open: Vec<Vec<P2>>,
    /// The narrowest detail in this layer's outlines, metres.
    pub narrowest: f64,
}

impl CadLayer {
    pub fn area(&self) -> f64 {
        self.islands.iter().map(Island::area).sum()
    }
    pub fn is_metal(&self) -> bool {
        self.role == LayerRole::Metal && !self.off
    }
}

/// A drawing ready to be placed: closed outlines in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct CadDrawing {
    pub format: String,
    pub layers: Vec<CadLayer>,
    pub feeds: Vec<CadFeed>,
    /// The feeds came from gaps found in the metal, not from a feed layer.
    pub feeds_guessed: bool,
    pub warnings: Vec<String>,
}

impl CadDrawing {
    /// Bounds of the metal, metres.
    pub fn bounds(&self) -> Option<(P2, P2)> {
        let mut out: Option<(P2, P2)> = None;
        for l in self.layers.iter().filter(|l| l.is_metal()) {
            for i in &l.islands {
                let (a, b) = bbox(&i.outer);
                out = Some(match out {
                    Some((p, q)) => ([p[0].min(a[0]), p[1].min(a[1])], [q[0].max(b[0]), q[1].max(b[1])]),
                    None => (a, b),
                });
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Plane geometry

fn dist(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

pub fn signed_area(pts: &[P2]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| { let (a, b) = (pts[i], pts[(i + 1) % n]); a[0] * b[1] - b[0] * a[1] }).sum::<f64>() / 2.0
}

fn bbox(pts: &[P2]) -> (P2, P2) {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in pts {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn inside(pts: &[P2], p: P2) -> bool {
    let mut c = false;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            c = !c;
        }
    }
    c
}

fn point_segment(p: P2, a: P2, b: P2) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

/// Whether the open segments cross. Touching at an end doesn't count.
fn crosses(a: P2, b: P2, c: P2, d: P2) -> bool {
    let side = |p: P2, q: P2, r: P2| (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
    let (d1, d2, d3, d4) = (side(c, d, a), side(c, d, b), side(a, b, c), side(a, b, d));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// Ramer-Douglas-Peucker on an open run: keeps the ends.
fn thin_run(pts: &[P2], tol: f64, keep: &mut [bool], from: usize, to: usize) {
    if to <= from + 1 {
        return;
    }
    let (mut worst, mut at) = (-1.0, from);
    for i in from + 1..to {
        let d = point_segment(pts[i], pts[from], pts[to]);
        if d > worst {
            (worst, at) = (d, i);
        }
    }
    if worst > tol {
        keep[at] = true;
        thin_run(pts, tol, keep, from, at);
        thin_run(pts, tol, keep, at, to);
    }
}

fn dedup(pts: &[P2], tol: f64) -> Vec<P2> {
    let mut out: Vec<P2> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().is_none_or(|q| dist(*q, *p) > tol) {
            out.push(*p);
        }
    }
    out
}

fn thin_open(pts: &[P2], tol: f64) -> Vec<P2> {
    let pts = dedup(pts, tol * 1e-3);
    if pts.len() < 3 {
        return pts;
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    thin_run(&pts, tol, &mut keep, 0, pts.len() - 1);
    pts.iter().zip(&keep).filter(|x| *x.1).map(|x| *x.0).collect()
}

fn thin_ring(pts: &[P2], tol: f64) -> Vec<P2> {
    let mut pts = dedup(pts, tol * 1e-3);
    while pts.len() > 1 && dist(pts[0], pts[pts.len() - 1]) <= tol * 1e-3 {
        pts.pop();
    }
    if pts.len() < 4 {
        return pts;
    }
    // A ring has no ends: start from the two points farthest apart.
    let far = (1..pts.len()).max_by(|a, b| dist(pts[0], pts[*a]).total_cmp(&dist(pts[0], pts[*b]))).unwrap_or(1);
    let mut lap = pts.clone();
    lap.push(pts[0]);
    let mut keep = vec![false; lap.len()];
    keep[0] = true;
    keep[far] = true;
    keep[lap.len() - 1] = true;
    thin_run(&lap, tol, &mut keep, 0, far);
    thin_run(&lap, tol, &mut keep, far, lap.len() - 1);
    pts.iter().zip(&keep).filter(|x| *x.1).map(|x| *x.0).collect()
}

/// Cuts a hole in an outline that may already have some.
pub fn cut_hole(outline: &[P2], hole: &[P2]) -> Vec<P2> {
    keyhole(outline, &[hole.to_vec()])
}

/// Whether a point is inside an outline, keyholed holes counting as outside.
pub fn point_inside(outline: &[P2], p: P2) -> bool {
    inside(outline, p)
}

/// Joins each hole to the outline with a zero-width bridge to a vertex it can see.
fn keyhole(outer: &[P2], holes: &[Vec<P2>]) -> Vec<P2> {
    let oriented = |pts: &[P2], ccw: bool| {
        let mut v = pts.to_vec();
        if (signed_area(&v) > 0.0) != ccw {
            v.reverse();
        }
        v
    };
    let mut merged = oriented(outer, true);
    let mut todo: Vec<Vec<P2>> = holes.iter().map(|h| oriented(h, false)).collect();
    let rightmost = |h: &Vec<P2>| h.iter().map(|p| p[0]).fold(f64::MIN, f64::max);
    todo.sort_by(|a, b| rightmost(b).total_cmp(&rightmost(a)));
    for i in 0..todo.len() {
        let hole = &todo[i];
        let start = (0..hole.len()).max_by(|a, b| hole[*a][0].total_cmp(&hole[*b][0])).unwrap_or(0);
        let h = hole[start];
        let mut order: Vec<usize> = (0..merged.len()).collect();
        order.sort_by(|a, b| dist(h, merged[*a]).total_cmp(&dist(h, merged[*b])));
        let ring_blocks = |ring: &[P2], m: P2| (0..ring.len()).any(|k| crosses(h, m, ring[k], ring[(k + 1) % ring.len()]));
        let m = order.iter().copied().find(|m| !ring_blocks(&merged, merged[*m]) && !ring_blocks(hole, merged[*m]) && !todo[i + 1..].iter().any(|o| ring_blocks(o, merged[*m]))).unwrap_or(order[0]);
        let mut next = Vec::with_capacity(merged.len() + hole.len() + 2);
        next.extend_from_slice(&merged[..=m]);
        next.extend_from_slice(&hole[start..]);
        next.extend_from_slice(&hole[..=start]);
        next.extend_from_slice(&merged[m..]);
        merged = next;
    }
    merged
}

/// The two sides of a path `w` wide, with mitred joints (bevelled where a mitre
/// would run away), as a closed ring for an open path, or two rings for a closed one.
pub fn stroke(path: &[P2], w: f64, closed: bool) -> Vec<Vec<P2>> {
    let mut pts = dedup(path, w * 1e-6);
    if closed && pts.len() > 1 && dist(pts[0], pts[pts.len() - 1]) <= w * 1e-6 {
        pts.pop();
    }
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    let dir = |a: P2, b: P2| {
        let l = dist(a, b).max(1e-300);
        [(b[0] - a[0]) / l, (b[1] - a[1]) / l]
    };
    let half = w / 2.0;
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    for i in 0..n {
        let before = if i > 0 { Some(pts[i - 1]) } else if closed { Some(pts[n - 1]) } else { None };
        let after = if i + 1 < n { Some(pts[i + 1]) } else if closed { Some(pts[0]) } else { None };
        let d0 = before.map(|p| dir(p, pts[i]));
        let d1 = after.map(|p| dir(pts[i], p));
        let (a, b) = match (d0, d1) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => (a, a),
            (None, Some(b)) => (b, b),
            (None, None) => continue,
        };
        let n0 = [-a[1], a[0]];
        let n1 = [-b[1], b[0]];
        let sum = [n0[0] + n1[0], n0[1] + n1[1]];
        let len = sum[0].hypot(sum[1]);
        // cos of half the turn: mitre length is half/that.
        let cos = len / 2.0;
        if cos > 0.35 {
            let k = half / cos / len.max(1e-300);
            left.push([pts[i][0] + sum[0] * k, pts[i][1] + sum[1] * k]);
            right.push([pts[i][0] - sum[0] * k, pts[i][1] - sum[1] * k]);
        } else {
            left.push([pts[i][0] + n0[0] * half, pts[i][1] + n0[1] * half]);
            left.push([pts[i][0] + n1[0] * half, pts[i][1] + n1[1] * half]);
            right.push([pts[i][0] - n0[0] * half, pts[i][1] - n0[1] * half]);
            right.push([pts[i][0] - n1[0] * half, pts[i][1] - n1[1] * half]);
        }
    }
    if closed {
        return vec![left, right];
    }
    right.reverse();
    left.extend(right);
    vec![left]
}

/// A strip along a path as one outline. A path that comes back to where it started is a ring: stroked
/// as an open path, its two ends cross, leaving an outline that cuts itself with a corner missing, and
/// when that corner was at the feed a real drawing came back with a negative resistance.
pub fn strip_outline(path: &[P2], w: f64) -> Option<Vec<P2>> {
    let closes = path.len() > 3 && dist(path[0], path[path.len() - 1]) <= w * 1e-6;
    if closes {
        if let [a, b] = stroke(path, w, true).as_slice() {
            let (outer, hole) = if signed_area(a).abs() > signed_area(b).abs() { (a, b) } else { (b, a) };
            return Some(cut_hole(outer, hole));
        }
    }
    stroke(path, w, false).into_iter().next()
}

/// A round end: a half circle at `c` facing along `along`, without its two ends.
fn round_cap(c: P2, along: P2, r: f64, out: &mut Vec<P2>) {
    let a0 = along[1].atan2(along[0]) - PI / 2.0;
    let n = 6;
    for k in 1..n {
        let t = a0 + PI * k as f64 / n as f64;
        out.push([c[0] + r * t.cos(), c[1] + r * t.sin()]);
    }
}

fn arc_points(c: P2, r: f64, start: f64, sweep: f64) -> Vec<P2> {
    let n = ((sweep.abs() / ARC_STEP).ceil() as usize).max(1);
    (0..=n).map(|k| { let t = start + sweep * k as f64 / n as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect()
}

/// The points strictly between `p` and `q` on a DXF bulge arc.
fn bulge_points(p: P2, q: P2, b: f64, out: &mut Vec<P2>) {
    if b.abs() < 1e-9 {
        return;
    }
    let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
    let k = (1.0 - b * b) / (4.0 * b);
    let c = [(p[0] + q[0]) / 2.0 - dy * k, (p[1] + q[1]) / 2.0 + dx * k];
    let r = dist(c, p);
    let start = (p[1] - c[1]).atan2(p[0] - c[0]);
    let sweep = 4.0 * b.atan();
    let n = ((sweep.abs() / ARC_STEP).ceil() as usize).max(1);
    for i in 1..n {
        let t = start + sweep * i as f64 / n as f64;
        out.push([c[0] + r * t.cos(), c[1] + r * t.sin()]);
    }
}

/// A point on a NURBS curve by de Boor's algorithm.
fn de_boor(t: f64, degree: usize, knots: &[f64], ctrl: &[P2], weights: &[f64]) -> P2 {
    let n = ctrl.len();
    let mut span = degree;
    while span + 1 < n && t >= knots[span + 1] {
        span += 1;
    }
    let mut d: Vec<[f64; 3]> = (0..=degree)
        .map(|j| {
            let i = span + j - degree;
            let w = weights.get(i).copied().unwrap_or(1.0);
            [ctrl[i][0] * w, ctrl[i][1] * w, w]
        })
        .collect();
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span + j - degree;
            let den = knots[i + degree + 1 - r] - knots[i];
            let a = if den > 0.0 { (t - knots[i]) / den } else { 0.0 };
            for k in 0..3 {
                d[j][k] = (1.0 - a) * d[j - 1][k] + a * d[j][k];
            }
        }
    }
    let e = d[degree];
    [e[0] / e[2], e[1] / e[2]]
}

fn sample_spline(degree: usize, knots: &[f64], ctrl: &[P2], weights: &[f64]) -> Option<Vec<P2>> {
    if degree == 0 || ctrl.len() <= degree || knots.len() != ctrl.len() + degree + 1 {
        return None;
    }
    let (lo, hi) = (knots[degree], knots[ctrl.len()]);
    if !(hi > lo) {
        return None;
    }
    let mut out = Vec::new();
    for w in knots[degree..=ctrl.len()].windows(2) {
        if w[1] > w[0] {
            for k in 0..16 {
                out.push(de_boor(w[0] + (w[1] - w[0]) * k as f64 / 16.0, degree, knots, ctrl, weights));
            }
        }
    }
    out.push(de_boor(hi - (hi - lo) * 1e-12, degree, knots, ctrl, weights));
    Some(out)
}

// ---------------------------------------------------------------------------
// DXF

/// Maps drawing coordinates: a block's placement, or a mirror.
#[derive(Clone, Copy, Debug)]
struct Xf {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    z: f64,
}

impl Xf {
    const ID: Xf = Xf { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0, z: 0.0 };
    fn apply(&self, p: P2) -> P2 {
        [self.a * p[0] + self.b * p[1] + self.e, self.c * p[0] + self.d * p[1] + self.f]
    }
    /// `self` after `o`.
    fn then(&self, o: &Xf) -> Xf {
        Xf {
            a: self.a * o.a + self.b * o.c,
            b: self.a * o.b + self.b * o.d,
            c: self.c * o.a + self.d * o.c,
            d: self.c * o.b + self.d * o.d,
            e: self.a * o.e + self.b * o.f + self.e,
            f: self.c * o.e + self.d * o.f + self.f,
            z: self.z + o.z,
        }
    }
    /// How much lengths grow, for a stroke width.
    fn stretch(&self) -> f64 {
        (self.a * self.d - self.b * self.c).abs().sqrt()
    }
}

type Pair<'a> = (i32, &'a str);

/// One entity: its kind and the group codes up to the next.
#[derive(Clone, Copy)]
struct Ent<'a> {
    kind: &'a str,
    f: &'a [Pair<'a>],
}

impl<'a> Ent<'a> {
    fn num(&self, code: i32) -> Option<f64> {
        self.f.iter().find(|p| p.0 == code).and_then(|p| p.1.trim().parse().ok())
    }
    fn or(&self, code: i32, d: f64) -> f64 {
        self.num(code).unwrap_or(d)
    }
    fn int(&self, code: i32) -> Option<i64> {
        self.num(code).map(|v| v as i64)
    }
    fn text(&self, code: i32) -> Option<&'a str> {
        self.f.iter().find(|p| p.0 == code).map(|p| p.1.trim())
    }
    fn pt(&self, cx: i32) -> P2 {
        [self.or(cx, 0.0), self.or(cx + 10, 0.0)]
    }
}

struct Block<'a> {
    base: P2,
    ents: Vec<Ent<'a>>,
}

fn dxf_pairs(text: &str) -> Result<Vec<Pair<'_>>> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((n, code)) = lines.next() {
        let code = code.trim();
        let Some((_, value)) = lines.next() else { break };
        if code.is_empty() && value.trim().is_empty() {
            continue;
        }
        let c = code.parse::<i32>().map_err(|_| anyhow!("this is not a text DXF file: line {} should hold a group code, not '{}'", n + 1, code.chars().take(30).collect::<String>()))?;
        out.push((c, value.trim_end()));
    }
    Ok(out)
}

/// Metres per unit for `$INSUNITS`; `None` for 0 (unitless) and the rare ones.
fn insunits(code: i64) -> Option<f64> {
    Some(match code {
        1 => 0.0254,
        2 => 0.3048,
        4 => 1e-3,
        5 => 1e-2,
        6 => 1.0,
        8 => 0.0254e-6,
        9 => 0.0254e-3,
        10 => 0.9144,
        13 => 1e-6,
        14 => 1e-1,
        _ => return None,
    })
}

struct DxfReader<'a> {
    blocks: HashMap<&'a str, Block<'a>>,
    layers: BTreeMap<(String, i64), RawLayer>,
    order: Vec<(String, i64)>,
    off: Vec<String>,
    skipped: BTreeMap<String, usize>,
    warnings: Vec<String>,
    non_planar: usize,
    solids_3d: usize,
    tilted: usize,
    bad_splines: usize,
    missing_blocks: usize,
}

impl<'a> DxfReader<'a> {
    fn layer(&mut self, name: &str, z: f64) -> &mut RawLayer {
        // One layer per drawn height, so a stack in one DXF layer stays a stack.
        let key = (name.to_string(), (z * 1e6).round() as i64);
        if !self.layers.contains_key(&key) {
            let mut l = RawLayer::new(name, z, true);
            l.off = self.off.iter().any(|o| o == name);
            if l.off && l.role == LayerRole::Metal {
                l.role = LayerRole::Ignore;
            }
            self.layers.insert(key.clone(), l);
            self.order.push(key.clone());
        }
        self.layers.get_mut(&key).unwrap()
    }

    fn add_path(&mut self, layer: &str, z: f64, pts: Vec<P2>, closed: bool, xf: &Xf) {
        if pts.len() < 2 {
            return;
        }
        let pts = pts.into_iter().map(|p| xf.apply(p)).collect();
        self.layer(layer, z + xf.z).paths.push(RawPath { pts, closed, clear: false });
    }

    fn add_stroked(&mut self, layer: &str, z: f64, pts: Vec<P2>, closed: bool, width: f64, xf: &Xf) {
        let pts: Vec<P2> = pts.into_iter().map(|p| xf.apply(p)).collect();
        for ring in stroke(&pts, width * xf.stretch(), closed) {
            self.layer(layer, z + xf.z).paths.push(RawPath { pts: ring, closed: true, clear: false });
        }
    }

    /// The entity with its own mirror in the placement, when its extrusion points down.
    fn placed(&mut self, e: &Ent, xf: &Xf) -> Option<Xf> {
        let nz = e.or(230, 1.0);
        if nz.abs() < 0.999 {
            self.tilted += 1;
            return None;
        }
        Some(if nz < 0.0 { xf.then(&Xf { a: -1.0, ..Xf::ID }) } else { *xf })
    }

    fn polyline(verts: &[(P2, f64)], closed: bool) -> Vec<P2> {
        let mut pts = Vec::with_capacity(verts.len());
        for (i, (p, b)) in verts.iter().enumerate() {
            pts.push(*p);
            let next = if i + 1 < verts.len() { Some(verts[i + 1].0) } else if closed { Some(verts[0].0) } else { None };
            if let Some(q) = next {
                bulge_points(*p, q, *b, &mut pts);
            }
        }
        pts
    }

    fn entity(&mut self, e: &Ent<'a>, rest: &[Ent<'a>], xf: &Xf, parent_layer: Option<&str>, depth: usize) -> usize {
        // Group code 67 = 1: paper space, where title blocks live.
        if e.int(67) == Some(1) {
            return 0;
        }
        let own = e.text(8).unwrap_or("0");
        // A block's parts on layer 0 take the layer of the block's insert.
        let layer: String = if own == "0" { parent_layer.unwrap_or(own).to_string() } else { own.to_string() };
        match e.kind {
            "LINE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                if (e.or(30, 0.0) - e.or(31, 0.0)).abs() > 1e-9 {
                    self.non_planar += 1;
                    return 0;
                }
                self.add_path(&layer, e.or(30, 0.0), vec![e.pt(10), e.pt(11)], false, &x);
            }
            "ARC" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                let (a0, mut a1) = (e.or(50, 0.0).to_radians(), e.or(51, 360.0).to_radians());
                if a1 <= a0 {
                    a1 += TAU;
                }
                self.add_path(&layer, e.or(30, 0.0), arc_points(e.pt(10), e.or(40, 0.0), a0, a1 - a0), false, &x);
            }
            "CIRCLE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                let mut pts = arc_points(e.pt(10), e.or(40, 0.0), 0.0, TAU);
                pts.pop();
                self.add_path(&layer, e.or(30, 0.0), pts, true, &x);
            }
            "ELLIPSE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                let (c, m, ratio) = (e.pt(10), e.pt(11), e.or(40, 1.0));
                let (t0, mut t1) = (e.or(41, 0.0), e.or(42, TAU));
                if t1 <= t0 {
                    t1 += TAU;
                }
                let full = (t1 - t0 - TAU).abs() < 1e-6;
                let n = (((t1 - t0) / ARC_STEP).ceil() as usize).max(1);
                let mut pts: Vec<P2> = (0..=n)
                    .map(|k| {
                        let t = t0 + (t1 - t0) * k as f64 / n as f64;
                        let (s, co) = t.sin_cos();
                        [c[0] + m[0] * co - m[1] * ratio * s, c[1] + m[1] * co + m[0] * ratio * s]
                    })
                    .collect();
                if full {
                    pts.pop();
                }
                self.add_path(&layer, e.or(30, 0.0), pts, full, &x);
            }
            "LWPOLYLINE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                let mut verts: Vec<(P2, f64)> = Vec::new();
                let mut seg_w: Option<f64> = None;
                for (code, value) in e.f {
                    let v: f64 = value.trim().parse().unwrap_or(0.0);
                    match code {
                        10 => verts.push(([v, 0.0], 0.0)),
                        20 => {
                            if let Some(l) = verts.last_mut() {
                                l.0[1] = v;
                            }
                        }
                        42 => {
                            if let Some(l) = verts.last_mut() {
                                l.1 = v;
                            }
                        }
                        40 | 41 if v > 0.0 => seg_w = Some(seg_w.map_or(v, |w: f64| w.max(v))),
                        _ => {}
                    }
                }
                let closed = e.int(70).unwrap_or(0) & 1 == 1;
                let pts = Self::polyline(&verts, closed);
                let width = e.num(43).filter(|w| *w > 0.0).or(seg_w);
                let z = e.or(38, 0.0);
                match width {
                    Some(w) => self.add_stroked(&layer, z, pts, closed, w, &x),
                    None => self.add_path(&layer, z, pts, closed, &x),
                }
            }
            "POLYLINE" => {
                let flags = e.int(70).unwrap_or(0);
                // Followed by VERTEX entities up to SEQEND.
                let mut verts: Vec<(P2, f64)> = Vec::new();
                let mut used = 0;
                let mut zs = (f64::MAX, f64::MIN);
                for v in rest {
                    used += 1;
                    match v.kind {
                        "VERTEX" => {
                            verts.push((v.pt(10), v.or(42, 0.0)));
                            let z = v.or(30, 0.0);
                            zs = (zs.0.min(z), zs.1.max(z));
                        }
                        _ => break,
                    }
                }
                // Bits 16 and 64 are meshes: faces, not outlines.
                if flags & (16 | 64) != 0 {
                    self.solids_3d += 1;
                    return used;
                }
                if flags & 8 != 0 && zs.1 - zs.0 > 1e-9 {
                    self.non_planar += 1;
                    return used;
                }
                let Some(x) = self.placed(e, xf) else { return used };
                let closed = flags & 1 == 1;
                let pts = Self::polyline(&verts, closed);
                let z = if flags & 8 != 0 && zs.0 != f64::MAX { zs.0 } else { e.or(30, 0.0) };
                match e.num(40).filter(|w| *w > 0.0) {
                    Some(w) => self.add_stroked(&layer, z, pts, closed, w, &x),
                    None => self.add_path(&layer, z, pts, closed, &x),
                }
                return used;
            }
            "SPLINE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                let degree = e.int(71).unwrap_or(3).max(0) as usize;
                let list = |code: i32| -> Vec<f64> { e.f.iter().filter(|p| p.0 == code).filter_map(|p| p.1.trim().parse().ok()).collect() };
                let knots = list(40);
                let weights = list(41);
                let zip2 = |a: Vec<f64>, b: Vec<f64>| -> Vec<P2> { a.into_iter().zip(b).map(|(u, v)| [u, v]).collect() };
                let ctrl = zip2(list(10), list(20));
                let fit = zip2(list(11), list(21));
                let z = list(30).first().copied().unwrap_or(0.0);
                let pts = sample_spline(degree, &knots, &ctrl, &weights).or(if fit.len() >= 2 { Some(fit) } else { None });
                let Some(pts) = pts else {
                    self.bad_splines += 1;
                    return 0;
                };
                let closed = e.int(70).unwrap_or(0) & 1 == 1;
                self.add_path(&layer, z, pts, closed, &x);
            }
            "SOLID" | "TRACE" => {
                let Some(x) = self.placed(e, xf) else { return 0 };
                // The third and fourth corners are stored crossed.
                let pts = vec![e.pt(10), e.pt(11), e.pt(13), e.pt(12)];
                self.add_path(&layer, e.or(30, 0.0), pts, true, &x);
            }
            "3DFACE" => {
                let zs = [e.or(30, 0.0), e.or(31, 0.0), e.or(32, 0.0), e.or(33, 0.0)];
                if zs.iter().any(|z| (z - zs[0]).abs() > 1e-9) {
                    self.non_planar += 1;
                    return 0;
                }
                self.add_path(&layer, zs[0], vec![e.pt(10), e.pt(11), e.pt(12), e.pt(13)], true, xf);
            }
            "HATCH" => {
                if let Some(x) = self.placed(e, xf) {
                    self.hatch(e, &layer, &x);
                }
            }
            "POINT" => {
                if let Some(x) = self.placed(e, xf) {
                    let p = x.apply(e.pt(10));
                    let z = e.or(30, 0.0) + x.z;
                    self.layer(&layer, z).points.push(p);
                }
            }
            "INSERT" => self.insert(e, xf, &layer, depth),
            "ATTRIB" | "ATTDEF" | "SEQEND" | "VERTEX" | "VIEWPORT" => {}
            "3DSOLID" | "BODY" | "REGION" | "SURFACE" | "MESH" | "POLYFACE" | "PLANESURFACE" | "EXTRUDEDSURFACE" | "LOFTEDSURFACE" | "REVOLVEDSURFACE" | "SWEPTSURFACE" => self.solids_3d += 1,
            other => *self.skipped.entry(other.to_string()).or_default() += 1,
        }
        0
    }

    fn insert(&mut self, e: &Ent<'a>, xf: &Xf, layer: &str, depth: usize) {
        if depth >= MAX_BLOCK_DEPTH {
            return;
        }
        let name = e.text(2).unwrap_or("");
        let Some(block) = self.blocks.get(name) else {
            self.missing_blocks += 1;
            return;
        };
        let (base, ents): (P2, Vec<Ent<'a>>) = (block.base, block.ents.clone());
        let Some(x) = self.placed(e, xf) else { return };
        let at = e.pt(10);
        let (sx, sy) = (e.or(41, 1.0), e.or(42, 1.0));
        let (s, c) = e.or(50, 0.0).to_radians().sin_cos();
        let (cols, rows) = (e.int(70).unwrap_or(1).max(1), e.int(71).unwrap_or(1).max(1));
        let (dc, dr) = (e.or(44, 0.0), e.or(45, 0.0));
        for row in 0..rows {
            for col in 0..cols {
                // Position in the block's frame, then scale, rotate and place.
                let (ox, oy) = (dc * col as f64, dr * row as f64);
                let m = Xf {
                    a: c * sx,
                    b: -s * sy,
                    c: s * sx,
                    d: c * sy,
                    e: at[0] + c * ox - s * oy - (c * sx * base[0] - s * sy * base[1]),
                    f: at[1] + s * ox + c * oy - (s * sx * base[0] + c * sy * base[1]),
                    z: e.or(30, 0.0),
                };
                let inner = x.then(&m);
                let mut k = 0;
                while k < ents.len() {
                    let used = self.entity(&ents[k], &ents[k + 1..], &inner, Some(layer), depth + 1);
                    k += 1 + used;
                }
            }
        }
    }

    /// A hatch's boundaries become closed paths; its fill pattern says nothing more.
    fn hatch(&mut self, e: &Ent, layer: &str, xf: &Xf) {
        let f = e.f;
        let z = e.or(30, 0.0);
        let Some(start) = f.iter().position(|p| p.0 == 91) else { return };
        let count: usize = f[start].1.trim().parse().unwrap_or(0);
        let mut i = start + 1;
        let next_num = |i: &mut usize, code: i32| -> Option<f64> {
            while *i < f.len() {
                let (c, v) = f[*i];
                *i += 1;
                if c == code {
                    return v.trim().parse().ok();
                }
            }
            None
        };
        for _ in 0..count {
            let Some(flags) = next_num(&mut i, 92) else { break };
            let flags = flags as i64;
            if flags & 2 != 0 {
                let has_bulge = next_num(&mut i, 72).unwrap_or(0.0) != 0.0;
                let closed = next_num(&mut i, 73).unwrap_or(1.0) != 0.0;
                let n = next_num(&mut i, 93).unwrap_or(0.0) as usize;
                let mut verts = Vec::with_capacity(n);
                for _ in 0..n {
                    let x = next_num(&mut i, 10).unwrap_or(0.0);
                    let y = next_num(&mut i, 20).unwrap_or(0.0);
                    let b = if has_bulge { next_num(&mut i, 42).unwrap_or(0.0) } else { 0.0 };
                    verts.push(([x, y], b));
                }
                self.add_path(layer, z, Self::polyline(&verts, closed), true, xf);
                continue;
            }
            let n = next_num(&mut i, 93).unwrap_or(0.0) as usize;
            let mut ring: Vec<P2> = Vec::new();
            for _ in 0..n {
                match next_num(&mut i, 72).unwrap_or(0.0) as i64 {
                    1 => {
                        let a = [next_num(&mut i, 10).unwrap_or(0.0), next_num(&mut i, 20).unwrap_or(0.0)];
                        let b = [next_num(&mut i, 11).unwrap_or(0.0), next_num(&mut i, 21).unwrap_or(0.0)];
                        ring.push(a);
                        ring.push(b);
                    }
                    2 => {
                        let c = [next_num(&mut i, 10).unwrap_or(0.0), next_num(&mut i, 20).unwrap_or(0.0)];
                        let r = next_num(&mut i, 40).unwrap_or(0.0);
                        let (s, t) = (next_num(&mut i, 50).unwrap_or(0.0), next_num(&mut i, 51).unwrap_or(0.0));
                        let ccw = next_num(&mut i, 73).unwrap_or(1.0) != 0.0;
                        ring.extend(hatch_arc(c, r, s, t, ccw));
                    }
                    3 => {
                        let c = [next_num(&mut i, 10).unwrap_or(0.0), next_num(&mut i, 20).unwrap_or(0.0)];
                        let m = [next_num(&mut i, 11).unwrap_or(0.0), next_num(&mut i, 21).unwrap_or(0.0)];
                        let ratio = next_num(&mut i, 40).unwrap_or(1.0);
                        let (s, t) = (next_num(&mut i, 50).unwrap_or(0.0), next_num(&mut i, 51).unwrap_or(0.0));
                        let ccw = next_num(&mut i, 73).unwrap_or(1.0) != 0.0;
                        // A unit circle's arc, stretched onto the ellipse.
                        for p in hatch_arc([0.0, 0.0], 1.0, s, t, ccw) {
                            ring.push([c[0] + m[0] * p[0] - m[1] * ratio * p[1], c[1] + m[1] * p[0] + m[0] * ratio * p[1]]);
                        }
                    }
                    4 => {
                        let degree = next_num(&mut i, 94).unwrap_or(3.0) as usize;
                        let rational = next_num(&mut i, 73).unwrap_or(0.0) != 0.0;
                        let _ = next_num(&mut i, 74);
                        let nk = next_num(&mut i, 95).unwrap_or(0.0) as usize;
                        let nc = next_num(&mut i, 96).unwrap_or(0.0) as usize;
                        let knots: Vec<f64> = (0..nk).map(|_| next_num(&mut i, 40).unwrap_or(0.0)).collect();
                        let mut ctrl = Vec::with_capacity(nc);
                        let mut weights = Vec::with_capacity(nc);
                        for _ in 0..nc {
                            ctrl.push([next_num(&mut i, 10).unwrap_or(0.0), next_num(&mut i, 20).unwrap_or(0.0)]);
                            if rational {
                                weights.push(next_num(&mut i, 42).unwrap_or(1.0));
                            }
                        }
                        match sample_spline(degree, &knots, &ctrl, &weights) {
                            Some(p) => ring.extend(p),
                            None => self.bad_splines += 1,
                        }
                    }
                    _ => {}
                }
            }
            self.add_path(layer, z, dedup(&ring, 1e-9), true, xf);
        }
    }

    fn finish(mut self, units: Option<f64>) -> RawDrawing {
        let mut warnings = std::mem::take(&mut self.warnings);
        if self.non_planar > 0 {
            warnings.push(format!("{} sloping or 3D shapes were left out: only flat outlines are read from a DXF.", self.non_planar));
        }
        if self.solids_3d > 0 {
            warnings.push(format!("{} 3D solids or meshes were left out: a DXF can't hand those over. Export them as STL and add them under Parts.", self.solids_3d));
        }
        if self.tilted > 0 {
            warnings.push(format!("{} shapes in a tilted plane were left out.", self.tilted));
        }
        if self.bad_splines > 0 {
            warnings.push(format!("{} splines had no usable points and were left out.", self.bad_splines));
        }
        if self.missing_blocks > 0 {
            warnings.push(format!("{} block references point at blocks the file doesn't define.", self.missing_blocks));
        }
        let mut layers = Vec::with_capacity(self.order.len());
        for key in &self.order {
            if let Some(l) = self.layers.remove(key) {
                layers.push(l);
            }
        }
        RawDrawing { format: "DXF".into(), layers, units_to_m: units, warnings, skipped: self.skipped }
    }
}

/// A hatch edge's arc. A clockwise edge stores its angles mirrored.
fn hatch_arc(c: P2, r: f64, start: f64, end: f64, ccw: bool) -> Vec<P2> {
    let (s, t) = if ccw { (start, end) } else { (-start, -end) };
    let (s, t) = (s.to_radians(), t.to_radians());
    let sweep = if ccw {
        let d = t - s;
        if d <= 0.0 { d + TAU } else { d }
    } else {
        let d = t - s;
        if d >= 0.0 { d - TAU } else { d }
    };
    arc_points(c, r, s, sweep)
}

/// Splits a section's group codes into entities.
fn entities<'a>(pairs: &'a [Pair<'a>]) -> Vec<Ent<'a>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pairs.len() {
        if pairs[i].0 != 0 {
            i += 1;
            continue;
        }
        let kind = pairs[i].1.trim();
        let start = i + 1;
        let mut j = start;
        while j < pairs.len() && pairs[j].0 != 0 {
            j += 1;
        }
        out.push(Ent { kind, f: &pairs[start..j] });
        i = j;
    }
    out
}

fn section<'a>(pairs: &'a [Pair<'a>], name: &str) -> Option<&'a [Pair<'a>]> {
    let mut i = 0;
    while i + 1 < pairs.len() {
        if pairs[i].0 == 0 && pairs[i].1.trim() == "SECTION" && pairs[i + 1].0 == 2 {
            let found = pairs[i + 1].1.trim() == name;
            let start = i + 2;
            let mut j = start;
            while j < pairs.len() && !(pairs[j].0 == 0 && pairs[j].1.trim() == "ENDSEC") {
                j += 1;
            }
            if found {
                return Some(&pairs[start..j]);
            }
            i = j;
        }
        i += 1;
    }
    None
}

pub fn read_dxf(bytes: &[u8]) -> Result<RawDrawing> {
    if bytes.starts_with(b"AutoCAD Binary DXF") {
        bail!("this is a binary DXF, which isn't supported: save it as ASCII DXF");
    }
    let text = String::from_utf8_lossy(bytes);
    let pairs = dxf_pairs(&text)?;
    let Some(ent_section) = section(&pairs, "ENTITIES") else {
        bail!("this file has no ENTITIES section, so it holds no drawing");
    };
    let mut units = None;
    let mut warnings = Vec::new();
    if let Some(h) = section(&pairs, "HEADER") {
        if let Some(i) = h.iter().position(|p| p.0 == 9 && p.1.trim() == "$INSUNITS") {
            let code = h.get(i + 1).and_then(|p| p.1.trim().parse::<i64>().ok()).unwrap_or(0);
            units = insunits(code);
        }
    }
    if units.is_none() {
        warnings.push("The file doesn't say what its unit is. Check the size below before you trust it.".to_string());
    }
    let mut off = Vec::new();
    if let Some(t) = section(&pairs, "TABLES") {
        for e in entities(t).iter().filter(|e| e.kind == "LAYER") {
            let hidden = e.int(70).unwrap_or(0) & 1 == 1 || e.int(62).unwrap_or(1) < 0;
            if hidden && let Some(n) = e.text(2) {
                off.push(n.to_string());
            }
        }
    }
    let mut blocks: HashMap<&str, Block> = HashMap::new();
    if let Some(b) = section(&pairs, "BLOCKS") {
        let list = entities(b);
        let mut i = 0;
        while i < list.len() {
            if list[i].kind == "BLOCK" {
                let name = list[i].text(2).unwrap_or("");
                let base = list[i].pt(10);
                let mut ents = Vec::new();
                i += 1;
                while i < list.len() && list[i].kind != "ENDBLK" {
                    ents.push(list[i]);
                    i += 1;
                }
                blocks.insert(name, Block { base, ents });
            }
            i += 1;
        }
    }
    let list = entities(ent_section);
    let mut r = DxfReader { blocks, layers: BTreeMap::new(), order: Vec::new(), off, skipped: BTreeMap::new(), warnings, non_planar: 0, solids_3d: 0, tilted: 0, bad_splines: 0, missing_blocks: 0 };
    let mut i = 0;
    while i < list.len() {
        let used = r.entity(&list[i], &list[i + 1..], &Xf::ID, None, 0);
        i += 1 + used;
    }
    let d = r.finish(units);
    if d.layers.iter().all(|l| l.paths.is_empty() && l.points.is_empty()) {
        bail!("no outlines found in this DXF{}", if d.warnings.is_empty() { String::new() } else { format!(": {}", d.warnings.join(" ")) });
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// Gerber (RS-274X)

#[derive(Clone, Debug)]
enum Aperture {
    Circle(f64),
    Rect(f64, f64),
    Obround(f64, f64),
    Polygon(f64, usize),
    Macro,
}

struct GerberState {
    /// Metres per file unit.
    unit: f64,
    int_digits: usize,
    dec_digits: usize,
    trailing: bool,
    incremental: bool,
    apertures: HashMap<i64, Aperture>,
    current: Option<i64>,
    at: P2,
    mode: i64,
    multi: bool,
    region: bool,
    contour: Vec<P2>,
    clear: bool,
    trace: Vec<P2>,
    op: i64,
    layer: RawLayer,
    macro_pads: usize,
    fmt_seen: bool,
}

impl GerberState {
    fn number(&self, s: &str) -> Option<f64> {
        if s.contains('.') {
            return s.parse::<f64>().ok().map(|v| v * self.unit);
        }
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut raw: f64 = digits.parse().ok()?;
        if self.trailing {
            let total = self.int_digits + self.dec_digits;
            if digits.len() < total {
                raw *= 10f64.powi((total - digits.len()) as i32);
            }
        }
        let v = raw / 10f64.powi(self.dec_digits as i32) * self.unit;
        Some(if neg { -v } else { v })
    }

    /// Ends the trace being drawn, stroking it with its aperture.
    fn flush_trace(&mut self) {
        let trace = std::mem::take(&mut self.trace);
        if trace.len() < 2 {
            return;
        }
        let Some(ap) = self.current.and_then(|c| self.apertures.get(&c).cloned()) else { return };
        let width = match ap {
            Aperture::Circle(d) | Aperture::Polygon(d, _) => d,
            Aperture::Rect(w, h) | Aperture::Obround(w, h) => w.min(h),
            Aperture::Macro => {
                self.macro_pads += 1;
                return;
            }
        };
        if width <= 0.0 {
            return;
        }
        let path = dedup(&trace, width * 1e-6);
        if path.len() < 2 {
            return;
        }
        let closed = dist(path[0], path[path.len() - 1]) <= width * 1e-6 && path.len() > 3;
        for mut ring in stroke(&path, width, closed) {
            if !closed {
                // A round aperture leaves round ends. `stroke` went along the left
                // side and back along the right, so its ends sit midway and at the start.
                let n = ring.len() / 2;
                let last = path[path.len() - 1];
                let before = path[path.len() - 2];
                let l = dist(before, last).max(1e-300);
                let along = [(last[0] - before[0]) / l, (last[1] - before[1]) / l];
                let mut end_cap = Vec::new();
                round_cap(last, [along[0], along[1]], width / 2.0, &mut end_cap);
                end_cap.reverse();
                let (first, second) = (path[0], path[1]);
                let l0 = dist(first, second).max(1e-300);
                let back = [(first[0] - second[0]) / l0, (first[1] - second[1]) / l0];
                let mut start_cap = Vec::new();
                round_cap(first, back, width / 2.0, &mut start_cap);
                start_cap.reverse();
                let mut with_caps = Vec::with_capacity(ring.len() + end_cap.len() + start_cap.len());
                with_caps.extend_from_slice(&ring[..n]);
                with_caps.extend(end_cap);
                with_caps.extend_from_slice(&ring[n..]);
                with_caps.extend(start_cap);
                ring = with_caps;
            }
            self.layer.paths.push(RawPath { pts: ring, closed: true, clear: self.clear });
        }
    }

    fn flash(&mut self, at: P2) {
        let Some(ap) = self.current.and_then(|c| self.apertures.get(&c).cloned()) else { return };
        let ring: Vec<P2> = match ap {
            Aperture::Circle(d) => circle_ring(at, d / 2.0, 24),
            Aperture::Polygon(d, n) => circle_ring(at, d / 2.0, n.max(3)),
            Aperture::Rect(w, h) => {
                let (x, y) = (w / 2.0, h / 2.0);
                vec![[at[0] - x, at[1] - y], [at[0] + x, at[1] - y], [at[0] + x, at[1] + y], [at[0] - x, at[1] + y]]
            }
            Aperture::Obround(w, h) => obround(at, w, h),
            Aperture::Macro => {
                self.macro_pads += 1;
                return;
            }
        };
        self.layer.paths.push(RawPath { pts: ring, closed: true, clear: self.clear });
    }

    fn close_contour(&mut self) {
        let contour = std::mem::take(&mut self.contour);
        if contour.len() >= 3 {
            self.layer.paths.push(RawPath { pts: contour, closed: true, clear: self.clear });
        }
    }

    fn arc(&self, from: P2, to: P2, i: f64, j: f64, clockwise: bool) -> Vec<P2> {
        let centre = if self.multi {
            [from[0] + i, from[1] + j]
        } else {
            // Single quadrant: the signs are left out, the sweep is at most 90°.
            let mut best = [from[0] + i, from[1] + j];
            let mut score = f64::MAX;
            for (a, b) in [(i, j), (-i, j), (i, -j), (-i, -j)] {
                let c = [from[0] + a, from[1] + b];
                let d = (dist(c, from) - dist(c, to)).abs();
                let sweep = sweep_of(c, from, to, clockwise).abs();
                if sweep <= PI / 2.0 + 1e-6 && d < score {
                    (best, score) = (c, d);
                }
            }
            best
        };
        let r = dist(centre, from);
        let start = (from[1] - centre[1]).atan2(from[0] - centre[0]);
        let mut sweep = sweep_of(centre, from, to, clockwise);
        // In multi-quadrant mode the same start and end are a whole circle.
        if self.multi && dist(from, to) < r * 1e-9 {
            sweep = if clockwise { -TAU } else { TAU };
        }
        arc_points(centre, r, start, sweep)
    }
}

fn sweep_of(c: P2, from: P2, to: P2, clockwise: bool) -> f64 {
    let a = (from[1] - c[1]).atan2(from[0] - c[0]);
    let b = (to[1] - c[1]).atan2(to[0] - c[0]);
    let d = b - a;
    if clockwise {
        if d >= 0.0 { d - TAU } else { d }
    } else if d <= 0.0 {
        d + TAU
    } else {
        d
    }
}

fn circle_ring(c: P2, r: f64, n: usize) -> Vec<P2> {
    (0..n).map(|k| { let t = TAU * k as f64 / n as f64; [c[0] + r * t.cos(), c[1] + r * t.sin()] }).collect()
}

fn obround(c: P2, w: f64, h: f64) -> Vec<P2> {
    let (rx, along) = if w >= h { (h / 2.0, [1.0, 0.0]) } else { (w / 2.0, [0.0, 1.0]) };
    let half = (w.max(h) - w.min(h)) / 2.0;
    let (a, b) = ([c[0] + along[0] * half, c[1] + along[1] * half], [c[0] - along[0] * half, c[1] - along[1] * half]);
    let side = [-along[1] * rx, along[0] * rx];
    let mut out = vec![[a[0] - side[0], a[1] - side[1]]];
    round_cap(a, along, rx, &mut out);
    out.push([a[0] + side[0], a[1] + side[1]]);
    out.push([b[0] + side[0], b[1] + side[1]]);
    round_cap(b, [-along[0], -along[1]], rx, &mut out);
    out.push([b[0] - side[0], b[1] - side[1]]);
    out
}

/// Splits `G01X10Y20D01` into its letter and number words.
fn gerber_words(cmd: &str) -> Vec<(char, &str)> {
    let mut out = Vec::new();
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let letter = bytes[i] as char;
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        out.push((letter, &cmd[start..j]));
        i = j;
    }
    out
}

fn gerber_extended(s: &mut GerberState, body: &str) {
    for cmd in body.split('*').map(str::trim).filter(|c| !c.is_empty()) {
        if let Some(rest) = cmd.strip_prefix("FS") {
            // FS, then zero omission (L/T), notation (A/I), then Xnn Ynn.
            s.trailing = rest.starts_with('T');
            s.incremental = rest.get(1..2) == Some("I");
            if let Some(x) = rest.find('X')
                && let Some(d) = rest.get(x + 1..x + 3)
                && d.len() == 2
                && let (Some(a), Some(b)) = (d[..1].parse().ok(), d[1..].parse().ok())
            {
                s.int_digits = a;
                s.dec_digits = b;
                s.fmt_seen = true;
            }
        } else if let Some(rest) = cmd.strip_prefix("MO") {
            s.unit = if rest.starts_with("IN") { 0.0254 } else { 1e-3 };
        } else if let Some(rest) = cmd.strip_prefix("LP") {
            s.flush_trace();
            s.clear = rest.starts_with('C');
        } else if let Some(rest) = cmd.strip_prefix("ADD") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let Ok(code) = digits.parse::<i64>() else { continue };
            let after = &rest[digits.len()..];
            let (name, args) = after.split_once(',').unwrap_or((after, ""));
            let nums: Vec<f64> = args.split('X').filter_map(|v| v.trim().parse::<f64>().ok()).map(|v| v * s.unit).collect();
            let get = |k: usize| nums.get(k).copied().unwrap_or(0.0);
            let ap = match name {
                "C" => Aperture::Circle(get(0)),
                "R" => Aperture::Rect(get(0), get(1)),
                "O" => Aperture::Obround(get(0), get(1)),
                "P" => Aperture::Polygon(get(0), (get(1) / s.unit) as usize),
                _ => Aperture::Macro,
            };
            s.apertures.insert(code, ap);
        }
    }
}

fn gerber_command(s: &mut GerberState, cmd: &str) {
    if cmd.is_empty() || cmd.starts_with("G04") || cmd.starts_with("G4 ") {
        return;
    }
    let (mut x, mut y, mut i, mut j) = (None, None, 0.0, 0.0);
    let mut d: Option<i64> = None;
    for (letter, num) in gerber_words(cmd) {
        match letter {
            'G' => match num.parse::<i64>().unwrap_or(-1) {
                1 => s.mode = 1,
                2 => s.mode = 2,
                3 => s.mode = 3,
                36 => {
                    s.flush_trace();
                    s.region = true;
                    s.contour.clear();
                }
                37 => {
                    s.close_contour();
                    s.region = false;
                }
                70 => s.unit = 0.0254,
                71 => s.unit = 1e-3,
                74 => s.multi = false,
                75 => s.multi = true,
                90 => s.incremental = false,
                91 => s.incremental = true,
                _ => {}
            },
            'X' => x = s.number(num),
            'Y' => y = s.number(num),
            'I' => i = s.number(num).unwrap_or(0.0),
            'J' => j = s.number(num).unwrap_or(0.0),
            'D' => d = num.parse().ok(),
            _ => {}
        }
    }
    if let Some(code) = d
        && code >= 10
    {
        s.flush_trace();
        s.current = Some(code);
        d = None;
    }
    let moved = x.is_some() || y.is_some();
    if let Some(code) = d {
        s.op = code;
    } else if !moved {
        return;
    }
    let to = if s.incremental { [s.at[0] + x.unwrap_or(0.0), s.at[1] + y.unwrap_or(0.0)] } else { [x.unwrap_or(s.at[0]), y.unwrap_or(s.at[1])] };
    match s.op {
        1 => {
            let run: Vec<P2> = if s.mode == 1 { vec![s.at, to] } else { s.arc(s.at, to, i, j, s.mode == 2) };
            if s.region {
                if s.contour.is_empty() {
                    s.contour.extend(run);
                } else {
                    s.contour.extend_from_slice(&run[1..]);
                }
            } else if s.trace.is_empty() {
                s.trace.extend(run);
            } else {
                s.trace.extend_from_slice(&run[1..]);
            }
        }
        2 => {
            if s.region {
                s.close_contour();
            } else {
                s.flush_trace();
            }
        }
        3 => {
            s.flush_trace();
            s.flash(to);
        }
        _ => {}
    }
    s.at = to;
}

/// Reads one Gerber file as one layer.
pub fn read_gerber(bytes: &[u8], layer_name: &str) -> Result<RawDrawing> {
    let text = String::from_utf8_lossy(bytes);
    if !text.contains("%FS") && !text.contains("%MO") {
        bail!("this is not an RS-274X Gerber file: it has no format statement");
    }
    let mut s = GerberState {
        unit: 1e-3,
        int_digits: 4,
        dec_digits: 6,
        trailing: false,
        incremental: false,
        apertures: HashMap::new(),
        current: None,
        at: [0.0, 0.0],
        mode: 1,
        multi: true,
        region: false,
        contour: Vec::new(),
        clear: false,
        trace: Vec::new(),
        op: 1,
        layer: RawLayer::new(layer_name, 0.0, false),
        macro_pads: 0,
        fmt_seen: false,
    };
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut rest = compact.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('%') {
            let end = after.find('%').unwrap_or(after.len());
            gerber_extended(&mut s, &after[..end]);
            rest = after.get(end + 1..).unwrap_or("");
            continue;
        }
        let Some(end) = rest.find(['*', '%']) else { break };
        let at = rest.as_bytes()[end];
        let cmd = &rest[..end];
        if cmd.starts_with("M02") || cmd.starts_with("M00") {
            break;
        }
        gerber_command(&mut s, cmd);
        rest = if at == b'*' { &rest[end + 1..] } else { &rest[end..] };
    }
    s.flush_trace();
    s.close_contour();
    let mut warnings = Vec::new();
    if s.macro_pads > 0 {
        warnings.push(format!("{} pads use aperture macros, which aren't expanded here, and were left out.", s.macro_pads));
    }
    if !s.fmt_seen {
        warnings.push("The file has no coordinate format; assumed 4.6 digits.".to_string());
    }
    if s.layer.paths.is_empty() {
        bail!("no copper found in this Gerber file");
    }
    Ok(RawDrawing { format: "Gerber".into(), layers: vec![s.layer], units_to_m: Some(1.0), warnings, skipped: BTreeMap::new() })
}

/// Reads a file by its name, or by its contents when the name says nothing.
pub fn read(file_name: &str, bytes: &[u8]) -> Result<RawDrawing> {
    let lower = file_name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    let stem = std::path::Path::new(file_name).file_stem().map_or(file_name.to_string(), |s| s.to_string_lossy().to_string());
    match ext {
        "dxf" => read_dxf(bytes),
        "gbr" | "ger" | "gtl" | "gbl" | "gts" | "gbs" | "g1" | "g2" | "pho" | "art" => read_gerber(bytes, &stem),
        "dwg" => bail!("DWG is AutoCAD's closed format and can't be read: save it as DXF"),
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string();
            if head.contains("%FS") || head.contains("%MO") {
                read_gerber(bytes, &stem)
            } else if head.contains("SECTION") || bytes.starts_with(b"AutoCAD Binary DXF") {
                read_dxf(bytes)
            } else {
                bail!("'{file_name}' is neither a DXF nor a Gerber file")
            }
        }
    }
}

impl RawDrawing {
    /// Adds another file's layers, for a stack of Gerber files.
    pub fn merge(&mut self, other: RawDrawing) {
        self.layers.extend(other.layers);
        self.warnings.extend(other.warnings);
        for (k, v) in other.skipped {
            *self.skipped.entry(k).or_default() += v;
        }
        if self.units_to_m.is_none() {
            self.units_to_m = other.units_to_m;
        }
    }

    /// The drawing's extent in file units, for choosing a joining tolerance.
    fn extent(&self) -> f64 {
        let mut all = Vec::new();
        for l in &self.layers {
            for p in &l.paths {
                all.extend_from_slice(&p.pts);
            }
            all.extend_from_slice(&l.points);
        }
        if all.is_empty() {
            return 0.0;
        }
        let (a, b) = bbox(&all);
        dist(a, b)
    }

    /// Closes what it can, finds holes and feeds, and converts to metres.
    pub fn resolve(&self, opt: Resolve) -> Result<CadDrawing> {
        let k = if let (Some(u), true) = (self.units_to_m, self.format == "Gerber") { u } else { opt.scale_to_m };
        if !(k > 0.0) {
            bail!("the file unit must be above zero");
        }
        // Endpoints this close are one point: exported drawings are rounded, not exact.
        let join = (self.extent() * 2e-6).max(1e-7 / k);
        let mut warnings = self.warnings.clone();
        let mut layers = Vec::with_capacity(self.layers.len());
        let mut raw_feeds: Vec<(String, [P2; 2])> = Vec::new();
        let mut probes: Vec<P2> = Vec::new();
        for raw in &self.layers {
            let (rings, clears, open) = close_paths(raw, join);
            if raw.role == LayerRole::Feed {
                for o in &open {
                    if let (Some(a), Some(b)) = (o.first(), o.last()) {
                        raw_feeds.push((raw.name.clone(), [scale2(*a, k), scale2(*b, k)]));
                    }
                }
                for r in &rings {
                    let (lo, hi) = bbox(r);
                    probes.push(scale2([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0], k));
                }
                probes.extend(raw.points.iter().map(|p| scale2(*p, k)));
                layers.push(CadLayer { name: raw.name.clone(), z: raw.z * k, role: raw.role, off: raw.off, islands: Vec::new(), open: Vec::new(), narrowest: f64::INFINITY });
                continue;
            }
            let to_m = |pts: &[P2]| -> Vec<P2> { pts.iter().map(|p| scale2(*p, k)).collect() };
            let rings_m: Vec<Vec<P2>> = rings.iter().map(|r| thin_ring(&to_m(r), opt.chord_tol)).filter(|r| r.len() >= 3).collect();
            let clears_m: Vec<Vec<P2>> = clears.iter().map(|r| thin_ring(&to_m(r), opt.chord_tol)).filter(|r| r.len() >= 3).collect();
            let (rings_m, dropped) = drop_duplicates(rings_m);
            if dropped > 0 {
                warnings.push(format!("Layer '{}': {dropped} outlines were drawn twice; one of each was kept.", raw.name));
            }
            let islands = if raw.even_odd { nest_even_odd(rings_m) } else { nest_clear(rings_m, clears_m) };
            let open_m: Vec<Vec<P2>> = open.iter().map(|o| thin_open(&to_m(o), opt.chord_tol)).collect();
            if raw.role == LayerRole::Metal && !open_m.is_empty() {
                warnings.push(format!("Layer '{}': {} paths don't close, so they carry no area and were left out. Close the outline in the drawing.", raw.name, open_m.len()));
            }
            let narrowest = narrowest_detail(&islands);
            layers.push(CadLayer { name: raw.name.clone(), z: raw.z * k, role: raw.role, off: raw.off, islands, open: open_m, narrowest });
        }
        let total_vertices: usize = layers.iter().flat_map(|l| &l.islands).map(|i| i.outer.len() + i.holes.iter().map(Vec::len).sum::<usize>()).sum();
        if total_vertices > 3000 {
            warnings.push(format!("{total_vertices} outline points is a lot for a mesh: raise the curve tolerance if the unknowns run high."));
        }
        let mut feeds = Vec::new();
        for (layer, line) in raw_feeds {
            match gap_feed(&layers, line) {
                Ok(f) => feeds.push(f),
                Err(e) => warnings.push(format!("Layer '{layer}': {e}")),
            }
        }
        feeds.extend(probes.into_iter().map(|at| CadFeed::Probe { at }));
        let feeds_guessed = feeds.is_empty();
        if feeds_guessed {
            feeds = suggest_feeds(&layers);
        }
        Ok(CadDrawing { format: self.format.clone(), layers, feeds, feeds_guessed, warnings })
    }
}

fn scale2(p: P2, k: f64) -> P2 {
    [p[0] * k, p[1] * k]
}

/// Chains the open paths end to end. Returns closed rings, clear rings, what stayed open.
fn close_paths(raw: &RawLayer, join: f64) -> (Vec<Vec<P2>>, Vec<Vec<P2>>, Vec<Vec<P2>>) {
    let mut rings = Vec::new();
    let mut clears = Vec::new();
    let mut pieces: Vec<Vec<P2>> = Vec::new();
    for p in &raw.paths {
        if p.closed {
            if p.clear {
                clears.push(p.pts.clone());
            } else {
                rings.push(p.pts.clone());
            }
        } else if p.pts.len() >= 2 {
            pieces.push(p.pts.clone());
        }
    }
    let cell = join.max(1e-300);
    let key = |p: P2| ((p[0] / cell).round() as i64, (p[1] / cell).round() as i64);
    // Which piece ends sit near which grid cell.
    let mut grid: HashMap<(i64, i64), Vec<(usize, bool)>> = HashMap::new();
    for (i, piece) in pieces.iter().enumerate() {
        for (end, p) in [(false, piece[0]), (true, piece[piece.len() - 1])] {
            grid.entry(key(p)).or_default().push((i, end));
        }
    }
    let partner = |p: P2, used: &[bool], grid: &HashMap<(i64, i64), Vec<(usize, bool)>>, pieces: &[Vec<P2>]| -> Option<(usize, bool)> {
        let (x, y) = key(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = grid.get(&(x + dx, y + dy)) {
                    for (i, end) in list {
                        if used[*i] {
                            continue;
                        }
                        let q = if *end { pieces[*i][pieces[*i].len() - 1] } else { pieces[*i][0] };
                        if dist(p, q) <= join {
                            return Some((*i, *end));
                        }
                    }
                }
            }
        }
        None
    };
    let mut used = vec![false; pieces.len()];
    let mut open = Vec::new();
    for start in 0..pieces.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut run = pieces[start].clone();
        // Grow from the far end, then from the near end.
        for _ in 0..2 {
            while let Some((i, end)) = partner(run[run.len() - 1], &used, &grid, &pieces) {
                used[i] = true;
                if end {
                    run.extend(pieces[i].iter().rev().skip(1).copied());
                } else {
                    run.extend(pieces[i].iter().skip(1).copied());
                }
            }
            run.reverse();
        }
        if run.len() >= 3 && dist(run[0], run[run.len() - 1]) <= join {
            run.pop();
            rings.push(run);
        } else {
            open.push(run);
        }
    }
    (rings, clears, open)
}

/// Removes outlines that repeat another, which CAD files do a lot: a hatch over
/// its own boundary, or a shape pasted twice. Left in, one would read as a hole.
fn drop_duplicates(rings: Vec<Vec<P2>>) -> (Vec<Vec<P2>>, usize) {
    let mut kept: Vec<(Vec<P2>, f64, (P2, P2))> = Vec::with_capacity(rings.len());
    let mut dropped = 0;
    for r in rings {
        let a = signed_area(&r).abs();
        let b = bbox(&r);
        let size = dist(b.0, b.1);
        let same = kept.iter().any(|(_, ka, kb)| (ka - a).abs() <= a * 1e-3 && dist(kb.0, b.0) <= size * 1e-3 && dist(kb.1, b.1) <= size * 1e-3);
        if same {
            dropped += 1;
        } else {
            kept.push((r, a, b));
        }
    }
    (kept.into_iter().map(|k| k.0).collect(), dropped)
}

/// Nested rings alternate: depth 0 is metal, 1 a hole in it, 2 metal in that hole.
fn nest_even_odd(rings: Vec<Vec<P2>>) -> Vec<Island> {
    let areas: Vec<f64> = rings.iter().map(|r| signed_area(r).abs()).collect();
    let parent_of = |i: usize| -> (usize, Option<usize>) {
        let mut depth = 0;
        let mut parent: Option<usize> = None;
        for j in 0..rings.len() {
            if j != i && areas[j] > areas[i] && inside(&rings[j], rings[i][0]) {
                depth += 1;
                if parent.is_none_or(|p| areas[j] < areas[p]) {
                    parent = Some(j);
                }
            }
        }
        (depth, parent)
    };
    let links: Vec<(usize, Option<usize>)> = (0..rings.len()).map(parent_of).collect();
    let mut order: Vec<usize> = (0..rings.len()).collect();
    order.sort_by_key(|i| links[*i].0);
    let mut islands: Vec<Island> = Vec::new();
    let mut island_of = vec![usize::MAX; rings.len()];
    for i in order {
        let (depth, parent) = links[i];
        if depth % 2 == 0 {
            island_of[i] = islands.len();
            islands.push(Island { outer: rings[i].clone(), holes: Vec::new() });
        } else if let Some(p) = parent {
            islands[island_of[p]].holes.push(rings[i].clone());
        }
    }
    islands
}

/// Every dark ring is metal; clear rings cut the smallest dark ring around them.
fn nest_clear(rings: Vec<Vec<P2>>, clears: Vec<Vec<P2>>) -> Vec<Island> {
    let mut islands: Vec<Island> = rings.into_iter().map(|outer| Island { outer, holes: Vec::new() }).collect();
    for c in clears {
        let mut best: Option<(usize, f64)> = None;
        for (i, isl) in islands.iter().enumerate() {
            if inside(&isl.outer, c[0]) {
                let a = signed_area(&isl.outer).abs();
                if best.is_none_or(|b| a < b.1) {
                    best = Some((i, a));
                }
            }
        }
        if let Some((i, _)) = best {
            islands[i].holes.push(c);
        }
    }
    islands
}

/// The smallest distance across or between outlines on a layer, for sizing the
/// FDTD mesh: from each vertex to every edge that doesn't touch it.
fn narrowest_detail(islands: &[Island]) -> f64 {
    let mut rings: Vec<&[P2]> = Vec::new();
    for i in islands {
        rings.push(&i.outer);
        for h in &i.holes {
            rings.push(h);
        }
    }
    let total: usize = rings.iter().map(|r| r.len()).sum();
    if total == 0 {
        return f64::INFINITY;
    }
    // Thin out on big layers: this runs once per import, on the interface thread.
    let stride = (total / 1500).max(1);
    let mut best = f64::INFINITY;
    for (ra, ring_a) in rings.iter().enumerate() {
        let na = ring_a.len();
        for ia in (0..na).step_by(stride) {
            let p = ring_a[ia];
            for (rb, ring_b) in rings.iter().enumerate() {
                let nb = ring_b.len();
                for ib in 0..nb {
                    let j = (ib + 1) % nb;
                    if ra == rb && (ib == ia || j == ia || (ib + 1) % nb == ia || (ia + 1) % na == ib) {
                        continue;
                    }
                    let d = point_segment(p, ring_b[ib], ring_b[j]);
                    if d > 1e-9 && d < best {
                        best = d;
                    }
                }
            }
        }
    }
    best
}

/// Where a line at `at` across `axis` meets a ring: positions along `axis`.
pub fn line_hits(ring: &[P2], axis: usize, at: f64) -> Vec<f64> {
    let other = 1 - axis;
    let mut out = Vec::new();
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        if (a[other] > at) != (b[other] > at) {
            let t = (at - a[other]) / (b[other] - a[other]);
            out.push(a[axis] + t * (b[axis] - a[axis]));
        }
    }
    out.sort_by(f64::total_cmp);
    out
}

/// The width of metal around `p`, measured across `axis`.
fn metal_width(island: &Island, p: P2, axis: usize) -> f64 {
    let across = 1 - axis;
    let mut hits = line_hits(&island.outer, across, p[axis]);
    for h in &island.holes {
        hits.extend(line_hits(h, across, p[axis]));
    }
    hits.sort_by(f64::total_cmp);
    for w in hits.chunks_exact(2) {
        if p[across] >= w[0] - 1e-12 && p[across] <= w[1] + 1e-12 {
            return w[1] - w[0];
        }
    }
    0.0
}

fn on_island(island: &Island, p: P2, tol: f64) -> bool {
    let on_edge = |r: &[P2]| (0..r.len()).any(|i| point_segment(p, r[i], r[(i + 1) % r.len()]) <= tol);
    if on_edge(&island.outer) || island.holes.iter().any(|h| on_edge(h)) {
        return true;
    }
    inside(&island.outer, p) && !island.holes.iter().any(|h| inside(h, p))
}

/// A single flat sheet of metal, as outlines already in metres, ready to find feeds on.
pub fn sheet_layer(name: &str, outlines: Vec<Vec<P2>>) -> CadLayer {
    let islands: Vec<Island> = outlines.into_iter().filter(|o| o.len() >= 3).map(|outer| Island { outer, holes: Vec::new() }).collect();
    let narrowest = narrowest_detail(&islands);
    CadLayer { name: name.to_string(), z: 0.0, role: LayerRole::Metal, off: false, islands, open: Vec::new(), narrowest }
}

/// The narrowest detail of a set of outlines, metres, for sizing an FDTD mesh.
pub fn narrowest_of(outlines: &[Vec<P2>]) -> f64 {
    let islands: Vec<Island> = outlines.iter().filter(|o| o.len() >= 3).map(|o| Island { outer: o.clone(), holes: Vec::new() }).collect();
    narrowest_detail(&islands)
}

/// Turns a drawn feed line into a port: straight along an axis, ends on metal.
pub fn gap_feed(layers: &[CadLayer], line: [P2; 2]) -> std::result::Result<CadFeed, String> {
    let [a, b] = line;
    let (dx, dy) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return Err("a feed line has no length".into());
    }
    let axis = if dx >= dy { 0 } else { 1 };
    if dx.min(dy) > 0.035 * len {
        return Err(format!("the feed line at ({:.2}, {:.2}) mm runs at an angle. Ports must lie along x or y; redraw it straight.", a[0] * 1e3, a[1] * 1e3));
    }
    let across = 1 - axis;
    let line_at = (a[across] + b[across]) / 2.0;
    let snap = 0.25 * len;
    // The metal layer whose edges sit closest to the ends of the line.
    let mut best: Option<(f64, usize, P2, P2, f64)> = None;
    for (li, l) in layers.iter().enumerate() {
        if !l.is_metal() {
            continue;
        }
        let mut hits = Vec::new();
        for isl in &l.islands {
            hits.extend(line_hits(&isl.outer, axis, line_at));
            for h in &isl.holes {
                hits.extend(line_hits(h, axis, line_at));
            }
        }
        let nearest = |x: f64| hits.iter().copied().min_by(|p, q| (p - x).abs().total_cmp(&(q - x).abs()));
        let (Some(ha), Some(hb)) = (nearest(a[axis]), nearest(b[axis])) else { continue };
        let (da, db) = ((ha - a[axis]).abs(), (hb - b[axis]).abs());
        if da > snap || db > snap || (ha - hb).abs() <= 0.0 {
            continue;
        }
        let mut pa = [0.0; 2];
        pa[axis] = ha;
        pa[across] = line_at;
        let mut pb = pa;
        pb[axis] = hb;
        // Measure each end's width just inside its metal.
        let inward = (hb - ha).signum() * 1e-6;
        let (mut ina, mut inb) = (pa, pb);
        ina[axis] -= inward;
        inb[axis] += inward;
        let width_at = |p: P2| l.islands.iter().filter(|i| on_island(i, p, 1e-9)).map(|i| metal_width(i, p, axis)).fold(0.0, f64::max);
        let (wa, wb) = (width_at(ina), width_at(inb));
        if wa <= 0.0 || wb <= 0.0 {
            continue;
        }
        let score = da + db;
        if best.as_ref().is_none_or(|x| score < x.0) {
            best = Some((score, li, pa, pb, wa.min(wb)));
        }
    }
    let Some((_, layer, pa, pb, width)) = best else {
        return Err(format!("the feed line at ({:.2}, {:.2}) mm doesn't reach metal at both ends.", a[0] * 1e3, a[1] * 1e3));
    };
    Ok(CadFeed::Gap { a: pa, b: pb, width, layer })
}

/// Finds likely feeds when the drawing marks none: the narrowest straight gaps
/// between two pieces of metal (a dipole's arms), or a long thin hole (a slot).
pub fn suggest_feeds(layers: &[CadLayer]) -> Vec<CadFeed> {
    struct Found {
        gap: f64,
        at: f64,
        along: f64,
        feed: CadFeed,
    }
    let mut found: Vec<Found> = Vec::new();
    for (li, l) in layers.iter().enumerate() {
        if !l.is_metal() {
            continue;
        }
        let boxes: Vec<(P2, P2)> = l.islands.iter().map(|i| bbox(&i.outer)).collect();
        for i in 0..l.islands.len() {
            for j in 0..l.islands.len() {
                if i == j {
                    continue;
                }
                for axis in 0..2 {
                    let across = 1 - axis;
                    let (ba, bb) = (boxes[i], boxes[j]);
                    // `i` lies wholly on the lower side of `j` along this axis.
                    if ba.1[axis] > bb.0[axis] + 1e-12 {
                        continue;
                    }
                    let (lo, hi) = (ba.0[across].max(bb.0[across]), ba.1[across].min(bb.1[across]));
                    if !(hi > lo) {
                        continue;
                    }
                    let centre = (lo + hi) / 2.0;
                    let mut best: Option<(f64, f64, f64, f64)> = None;
                    for k in 0..=8 {
                        let s = lo + (hi - lo) * (k as f64 + 0.5) / 9.0;
                        let (ha, hb) = (line_hits(&l.islands[i].outer, axis, s), line_hits(&l.islands[j].outer, axis, s));
                        let (Some(end_a), Some(end_b)) = (ha.last().copied(), hb.first().copied()) else { continue };
                        let gap = end_b - end_a;
                        if gap <= 0.0 {
                            continue;
                        }
                        let tighter = best.is_none_or(|b| gap < b.0 * (1.0 - 1e-6) || ((gap - b.0).abs() <= b.0 * 1e-6 && (s - centre).abs() < (b.1 - centre).abs()));
                        if tighter {
                            best = Some((gap, s, end_a, end_b));
                        }
                    }
                    let Some((gap, s, end_a, end_b)) = best else { continue };
                    let mut a = [0.0; 2];
                    a[axis] = end_a;
                    a[across] = s;
                    let mut b = a;
                    b[axis] = end_b;
                    let (mut ina, mut inb) = (a, b);
                    ina[axis] -= gap * 1e-3;
                    inb[axis] += gap * 1e-3;
                    let width = metal_width(&l.islands[i], ina, axis).min(metal_width(&l.islands[j], inb, axis));
                    if width > 0.0 {
                        found.push(Found { gap, at: a[1], along: a[0], feed: CadFeed::Gap { a, b, width, layer: li } });
                    }
                }
            }
        }
    }
    if let Some(smallest) = found.iter().map(|f| f.gap).reduce(f64::min) {
        // Neighbouring elements have gaps too, much wider than their own feeds.
        found.retain(|f| f.gap <= 3.0 * smallest);
        found.sort_by(|a, b| b.at.total_cmp(&a.at).then(a.along.total_cmp(&b.along)));
        return found.into_iter().map(|f| f.feed).collect();
    }
    // No gaps: a long thin hole fed across its middle.
    let mut out = Vec::new();
    for (li, l) in layers.iter().enumerate() {
        if !l.is_metal() {
            continue;
        }
        for isl in &l.islands {
            for h in &isl.holes {
                let (lo, hi) = bbox(h);
                let (w, ht) = (hi[0] - lo[0], hi[1] - lo[1]);
                let box_area = w * ht;
                if signed_area(h).abs() < 0.85 * box_area || w.max(ht) < 3.0 * w.min(ht) {
                    continue;
                }
                let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
                let (a, b) = if w < ht { ([lo[0], mid[1]], [hi[0], mid[1]]) } else { ([mid[0], lo[1]], [mid[0], hi[1]]) };
                out.push(CadFeed::Gap { a, b, width: w.max(ht) / 25.0, layer: li });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Into a project

/// Which way the drawing stands in the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CadPlane {
    /// Drawing x, y are model x, y; height along z.
    #[default]
    Flat,
    /// Drawing x is model y, drawing y is model z; height along x: a panel facing x.
    FacingX,
    /// Drawing x is model x, drawing y is model z; height along y: a panel facing y.
    FacingY,
}

impl CadPlane {
    pub const ALL: [CadPlane; 3] = [CadPlane::Flat, CadPlane::FacingX, CadPlane::FacingY];
    pub fn label(self) -> &'static str {
        match self {
            CadPlane::Flat => "Flat, facing z",
            CadPlane::FacingX => "Upright, facing x",
            CadPlane::FacingY => "Upright, facing y",
        }
    }
    /// The axis along the drawing's normal.
    pub fn axis(self) -> usize {
        match self {
            CadPlane::Flat => 2,
            CadPlane::FacingX => 0,
            CadPlane::FacingY => 1,
        }
    }
    /// A drawing point as a `Shape::Polygon` point (see `geometry::plane_point`).
    pub fn outline_point(self, p: P2) -> P2 {
        match self {
            CadPlane::Flat | CadPlane::FacingX => p,
            CadPlane::FacingY => [p[1], p[0]],
        }
    }
    /// A drawing point and a height as model coordinates, as expressions.
    fn world(self, p: P2, height: &str) -> [String; 3] {
        let (u, v) = (format!("{:e}", p[0]), format!("{:e}", p[1]));
        match self {
            CadPlane::Flat => [u, v, height.to_string()],
            CadPlane::FacingX => [height.to_string(), u, v],
            CadPlane::FacingY => [u, height.to_string(), v],
        }
    }
    /// A drawing point at a numeric height, as a model point.
    pub fn point(self, p: P2, height: f64) -> P3 {
        match self {
            CadPlane::Flat => [p[0], p[1], height],
            CadPlane::FacingX => [height, p[0], p[1]],
            CadPlane::FacingY => [p[0], height, p[1]],
        }
    }
}

/// How one layer goes into the model.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerPlacement {
    pub include: bool,
    /// Position along the plane's axis, an expression: `0`, `h`, `1.6mm`.
    pub height: String,
    pub material: String,
}

/// Sensible defaults: metal layers stacked by the file's own heights when it has
/// them, otherwise one substrate thickness `h` apart.
pub fn default_placements(cad: &CadDrawing, metal: &str) -> Vec<LayerPlacement> {
    let has_heights = cad.layers.iter().any(|l| l.is_metal() && l.z.abs() > 1e-12);
    let mut level = 0;
    cad.layers
        .iter()
        .map(|l| {
            let height = if has_heights {
                // In millimetres, the way a person would type it.
                if l.z.abs() < 1e-12 { "0".to_string() } else { format!("{}mm", (l.z * 1e6).round() / 1e3) }
            } else if l.is_metal() {
                let h = match level {
                    0 => "0".to_string(),
                    1 => "h".to_string(),
                    k => format!("{k}*h"),
                };
                level += 1;
                h
            } else {
                "0".to_string()
            };
            LayerPlacement { include: l.is_metal(), height, material: metal.to_string() }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub plane: CadPlane,
    /// Moves the metal's centre to the origin.
    pub center: bool,
    pub layers: Vec<LayerPlacement>,
    pub feeds: Vec<CadFeed>,
}

/// What was added, for the message after an import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Added {
    pub parts: usize,
    pub ports: usize,
}

/// Puts the drawing into a project as parts and ports.
pub fn add_to_project(project: &mut Project, cad: &CadDrawing, file: &str, how: &Placement) -> Result<Added> {
    if how.layers.len() != cad.layers.len() {
        bail!("the layer choices don't match the drawing");
    }
    let chosen: Vec<usize> = (0..cad.layers.len()).filter(|k| how.layers[*k].include && !cad.layers[*k].islands.is_empty()).collect();
    if chosen.iter().all(|k| cad.layers[*k].role != LayerRole::Metal) {
        bail!("no metal layer is selected");
    }
    let shift = if how.center {
        let (lo, hi) = cad.bounds().unwrap_or(([0.0; 2], [0.0; 2]));
        [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]
    } else {
        [0.0; 2]
    };
    let moved = |p: P2| [p[0] - shift[0], p[1] - shift[1]];
    let stem = std::path::Path::new(file).file_stem().map_or_else(|| "drawing".to_string(), |s| s.to_string_lossy().to_string());
    let mut added = Added::default();
    let axis = how.plane.axis();
    let taken = |project: &Project, n: &str| project.extra.iter().any(|e| e.name == n);
    // The stack's span, for a probe and for the board.
    let metal_levels: Vec<&String> = chosen.iter().filter(|k| cad.layers[**k].role == LayerRole::Metal).map(|k| &how.layers[*k].height).collect();
    let (low, high) = (metal_levels.first().map_or("0".to_string(), |s| s.to_string()), metal_levels.last().map_or("0".to_string(), |s| s.to_string()));
    for k in &chosen {
        let (layer, place) = (&cad.layers[*k], &how.layers[*k]);
        for (n, island) in layer.islands.iter().enumerate() {
            let pts: Vec<P2> = island.keyholed().into_iter().map(|p| how.plane.outline_point(moved(p))).collect();
            let mut name = if layer.islands.len() == 1 { layer.name.clone() } else { format!("{} {}", layer.name, n + 1) };
            let base = name.clone();
            let mut count = 2;
            while taken(project, &name) {
                name = format!("{base} ({count})");
                count += 1;
            }
            let (material, at, thick, cell) = if layer.role == LayerRole::Board {
                (project.substrate.clone(), low.clone(), format!("({high})-({low})"), f64::INFINITY)
            } else {
                (place.material.clone(), place.height.clone(), "0".to_string(), layer.narrowest)
            };
            project.extra.push(ExtraSolid { name, material, prim: ExtraPrim::Outline { file: stem.clone(), axis, at, pts, thick, cell: if cell.is_finite() { cell / 3.0 } else { 0.0 } } });
            added.parts += 1;
        }
    }
    let first_port = project.extra_ports.len();
    for f in &how.feeds {
        let n = project.extra_ports.len() - first_port + 1;
        let (a, b, width) = match f {
            CadFeed::Gap { a, b, width, layer } => {
                let height = how.layers.get(*layer).map_or("0".to_string(), |l| l.height.clone());
                (how.plane.world(moved(*a), &height), how.plane.world(moved(*b), &height), format!("{width:e}"))
            }
            CadFeed::Probe { at } => {
                if low == high {
                    bail!("a probe feed needs metal at two heights, and the chosen layers sit at one");
                }
                (how.plane.world(moved(*at), &low), how.plane.world(moved(*at), &high), "0".to_string())
            }
        };
        project.extra_ports.push(ExtraPort { name: format!("{stem} {n}"), a, b, width, z0: "Z0".to_string() });
        added.ports += 1;
    }
    let uses_h = how.layers.iter().enumerate().any(|(k, l)| l.include && !cad.layers[k].islands.is_empty() && crate::expr::names(&l.height).is_ok_and(|n| n.iter().any(|x| x == "h")));
    if uses_h && !project.params.iter().any(|p| p.name == "h") {
        project.params.push(Param::new("h", "1.6mm", "Layer spacing"));
    }
    if !project.params.iter().any(|p| p.name == "Z0") {
        project.params.push(Param::new("Z0", "50", "Port impedance"));
    }
    Ok(added)
}

/// A new project at this design frequency holding only the drawing.
pub fn new_project(cad: &CadDrawing, file: &str, f0: &str, how: &Placement) -> Result<(Project, Added)> {
    let mut p = Project::from_template("custom", Some(f0))?;
    p.params.retain(|r| r.name != "L");
    p.name = std::path::Path::new(file).file_stem().map_or_else(|| "Imported antenna".to_string(), |s| s.to_string_lossy().to_string());
    let added = add_to_project(&mut p, cad, file, how)?;
    Ok((p, added))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Geometry, Shape, triangulate};
    use crate::mom::{MomOptions, MomProblem};

    fn mm() -> Resolve {
        Resolve { scale_to_m: 1e-3, chord_tol: 10e-6 }
    }

    /// A DXF file in the shape real writers use, group code and value per line.
    fn dxf(entities: &str) -> String {
        let body: String = entities.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| format!("{l}\n")).collect();
        format!("0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n{body}0\nENDSEC\n0\nEOF\n")
    }

    fn lwpoly(layer: &str, pts: &[(f64, f64)], closed: bool) -> String {
        let mut s = format!("0\nLWPOLYLINE\n8\n{layer}\n90\n{}\n70\n{}\n", pts.len(), i32::from(closed));
        for (x, y) in pts {
            s.push_str(&format!("10\n{x}\n20\n{y}\n"));
        }
        s
    }

    fn area_of(c: &CadDrawing) -> f64 {
        c.layers.iter().map(CadLayer::area).sum()
    }

    #[test]
    fn a_closed_polyline_keeps_its_size_and_unit() {
        let d = read_dxf(dxf(&lwpoly("TOP", &[(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (0.0, 10.0)], true)).as_bytes()).unwrap();
        assert_eq!(d.units_to_m, Some(1e-3));
        let c = d.resolve(mm()).unwrap();
        assert_eq!(c.layers[0].islands.len(), 1);
        assert!((area_of(&c) - 200e-6).abs() < 1e-12, "{}", area_of(&c));
    }

    #[test]
    fn circles_arcs_and_bulges_have_the_right_area() {
        let r = 10.0;
        let circle = format!("0\nCIRCLE\n8\nA\n10\n0\n20\n0\n40\n{r}\n");
        let c = read_dxf(dxf(&circle).as_bytes()).unwrap().resolve(mm()).unwrap();
        let exact = PI * r * r * 1e-6;
        assert!((area_of(&c) - exact).abs() / exact < 1e-3, "{} vs {exact}", area_of(&c));
        // Two half circles as a closed polyline with bulges of 1.
        let bulged = "0\nLWPOLYLINE\n8\nA\n90\n2\n70\n1\n10\n-10\n20\n0\n42\n1\n10\n10\n20\n0\n42\n1\n";
        let c = read_dxf(dxf(bulged).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert!((area_of(&c) - exact).abs() / exact < 1e-3, "{}", area_of(&c));
        // A half disc from a LINE and an ARC, which only close once chained.
        let chained = "0\nLINE\n8\nA\n10\n-10\n20\n0\n11\n10\n21\n0\n0\nARC\n8\nA\n10\n0\n20\n0\n40\n10\n50\n0\n51\n180\n";
        let c = read_dxf(dxf(chained).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert_eq!(c.layers[0].islands.len(), 1, "{:?}", c.warnings);
        assert!((area_of(&c) - exact / 2.0).abs() / exact < 1e-3);
    }

    #[test]
    fn thinning_keeps_a_curve_within_its_tolerance() {
        let circle = "0\nCIRCLE\n8\nA\n10\n0\n20\n0\n40\n50\n";
        let raw = read_dxf(dxf(circle).as_bytes()).unwrap();
        let tight = raw.resolve(Resolve { scale_to_m: 1e-3, chord_tol: 10e-6 }).unwrap();
        let loose = raw.resolve(Resolve { scale_to_m: 1e-3, chord_tol: 500e-6 }).unwrap();
        let n = |c: &CadDrawing| c.layers[0].islands[0].outer.len();
        assert!(n(&loose) < n(&tight), "{} {}", n(&loose), n(&tight));
        assert!(n(&loose) >= 8);
        for p in &loose.layers[0].islands[0].outer {
            assert!((p[0].hypot(p[1]) - 0.05).abs() < 1e-9);
        }
    }

    #[test]
    fn nested_outlines_alternate_metal_and_hole() {
        let mut e = lwpoly("TOP", &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)], true);
        e += &lwpoly("TOP", &[(20.0, 20.0), (80.0, 20.0), (80.0, 80.0), (20.0, 80.0)], true);
        e += &lwpoly("TOP", &[(40.0, 40.0), (60.0, 40.0), (60.0, 60.0), (40.0, 60.0)], true);
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        let l = &c.layers[0];
        assert_eq!(l.islands.len(), 2, "plate with a hole, and an island in that hole");
        assert_eq!(l.islands.iter().filter(|i| i.holes.len() == 1).count(), 1);
        let want = (100.0f64 * 100.0 - 60.0 * 60.0 + 20.0 * 20.0) * 1e-6;
        assert!((l.area() - want).abs() < 1e-12, "{}", l.area());
    }

    #[test]
    fn an_outline_drawn_twice_is_not_read_as_a_hole() {
        let square = lwpoly("TOP", &[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true);
        let c = read_dxf(dxf(&format!("{square}{square}")).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert_eq!(c.layers[0].islands.len(), 1);
        assert!((area_of(&c) - 100e-6).abs() < 1e-12);
        assert!(c.warnings.iter().any(|w| w.contains("twice")), "{:?}", c.warnings);
    }

    #[test]
    fn keyholes_triangulate_to_the_right_area_with_several_holes() {
        let hole = |x: f64, y: f64| vec![[x, y], [x + 2.0, y], [x + 2.0, y + 2.0], [x, y + 2.0]];
        let island = Island { outer: vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]], holes: vec![hole(3.0, 3.0), hole(10.0, 4.0), hole(5.0, 12.0), hole(14.0, 14.0)] };
        let k = island.keyholed();
        // The even-odd rule the mesher uses agrees with the area of the pieces.
        assert!((signed_area(&k).abs() - island.area()).abs() < 1e-9, "{}", signed_area(&k));
        let area: f64 = triangulate(&k).iter().map(|t| {
            let (a, b, c) = (k[t[0]], k[t[1]], k[t[2]]);
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() / 2.0
        }).sum();
        assert!((area - island.area()).abs() < 1e-9, "{area} vs {}", island.area());
        assert!(!inside(&k, [4.0, 4.0]) && inside(&k, [1.0, 1.0]));
    }

    #[test]
    fn a_wide_polyline_becomes_a_strip() {
        let e = "0\nLWPOLYLINE\n8\nCu\n90\n3\n70\n0\n43\n2\n10\n0\n20\n0\n10\n50\n20\n0\n10\n50\n20\n30\n";
        let c = read_dxf(dxf(e).as_bytes()).unwrap().resolve(mm()).unwrap();
        // The centre line's length times the width (the mitre adds and removes equally).
        assert!((area_of(&c) - 80.0 * 2.0 * 1e-6).abs() < 1e-9, "{}", area_of(&c));
    }

    #[test]
    fn blocks_are_placed_scaled_rotated_and_arrayed() {
        let text = "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n8\n0\n2\nELEM\n70\n0\n10\n0\n20\n0\n0\nLWPOLYLINE\n8\n0\n90\n4\n70\n1\n10\n0\n20\n0\n10\n10\n20\n0\n10\n10\n20\n2\n10\n0\n20\n2\n0\nENDBLK\n0\nENDSEC\n";
        let ents = "0\nINSERT\n8\nTOP\n2\nELEM\n10\n100\n20\n0\n41\n2\n42\n2\n50\n90\n70\n3\n44\n50\n71\n1\n";
        let file = format!("{text}0\nSECTION\n2\nENTITIES\n{ents}0\nENDSEC\n0\nEOF\n");
        let raw = read_dxf(file.as_bytes()).unwrap();
        let c = raw.resolve(mm()).unwrap();
        let l = &c.layers[0];
        assert_eq!(l.name, "TOP", "parts on layer 0 take the insert's layer");
        assert_eq!(l.islands.len(), 3);
        assert!((l.area() - 3.0 * 80e-6).abs() < 1e-12, "{}", l.area());
        let (lo, hi) = bbox(&l.islands[0].outer);
        // Rotated 90°: 20 × 4 along y, 4 wide along x, from x = 96.
        assert!((hi[1] - lo[1] - 0.020).abs() < 1e-9 && (hi[0] - lo[0] - 0.004).abs() < 1e-9, "{lo:?} {hi:?}");
    }

    #[test]
    fn a_downward_extrusion_mirrors_x() {
        let e = "0\nARC\n8\nA\n10\n20\n20\n0\n40\n5\n50\n0\n51\n360\n230\n-1\n";
        let c = read_dxf(dxf(e).as_bytes()).unwrap().resolve(mm()).unwrap();
        let (lo, hi) = bbox(&c.layers[0].open.first().cloned().or_else(|| c.layers[0].islands.first().map(|i| i.outer.clone())).unwrap());
        assert!(((lo[0] + hi[0]) / 2.0 + 0.020).abs() < 1e-6, "{lo:?} {hi:?}");
    }

    #[test]
    fn layers_split_by_height_and_paper_space_is_left_out() {
        let mut e = String::new();
        e += "0\nLWPOLYLINE\n8\nCu\n90\n4\n70\n1\n38\n0\n10\n0\n20\n0\n10\n10\n20\n0\n10\n10\n20\n10\n10\n0\n20\n10\n";
        e += "0\nLWPOLYLINE\n8\nCu\n90\n4\n70\n1\n38\n1.6\n10\n1\n20\n1\n10\n9\n20\n1\n10\n9\n20\n9\n10\n1\n20\n9\n";
        e += "0\nLWPOLYLINE\n67\n1\n8\nTitle\n90\n4\n70\n1\n10\n0\n20\n0\n10\n900\n20\n0\n10\n900\n20\n600\n10\n0\n20\n600\n";
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert_eq!(c.layers.len(), 2);
        assert!((c.layers[1].z - 1.6e-3).abs() < 1e-12);
        let places = default_placements(&c, "PEC");
        assert_eq!(places[1].height, "1.6mm");
    }

    /// A file laid out the way AutoCAD R2000 and later write it: handles, owners,
    /// subclass markers, a layer table, and every entity kind the reader takes.
    fn r2000_file() -> String {
        let mut s = String::new();
        let mut put = |code: i32, v: &str| s.push_str(&format!("{code:>3}\r\n{v}\r\n"));
        for (c, v) in [(0, "SECTION"), (2, "HEADER"), (9, "$ACADVER"), (1, "AC1015"), (9, "$INSUNITS"), (70, "4"), (9, "$EXTMIN"), (10, "0"), (20, "0"), (30, "0"), (0, "ENDSEC")] {
            put(c, v);
        }
        for (c, v) in [(0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER"), (5, "2"), (330, "0"), (100, "AcDbSymbolTable"), (70, "2"),
            (0, "LAYER"), (5, "10"), (330, "2"), (100, "AcDbSymbolTableRecord"), (100, "AcDbLayerTableRecord"), (2, "COPPER"), (70, "0"), (62, "7"), (6, "CONTINUOUS"),
            (0, "LAYER"), (5, "11"), (330, "2"), (100, "AcDbSymbolTableRecord"), (100, "AcDbLayerTableRecord"), (2, "OLD"), (70, "1"), (62, "1"), (6, "CONTINUOUS"),
            (0, "ENDTAB"), (0, "ENDSEC")] {
            put(c, v);
        }
        for (c, v) in [(0, "SECTION"), (2, "BLOCKS"), (0, "BLOCK"), (5, "20"), (330, "1F"), (100, "AcDbEntity"), (8, "0"), (100, "AcDbBlockBegin"), (2, "PAD"), (70, "0"), (10, "0"), (20, "0"), (30, "0"), (3, "PAD"), (1, ""),
            (0, "CIRCLE"), (5, "21"), (330, "1F"), (100, "AcDbEntity"), (8, "0"), (100, "AcDbCircle"), (10, "0"), (20, "0"), (30, "0"), (40, "2"),
            (0, "ENDBLK"), (5, "22"), (330, "1F"), (100, "AcDbEntity"), (8, "0"), (100, "AcDbBlockEnd"), (0, "ENDSEC")] {
            put(c, v);
        }
        put(0, "SECTION");
        put(2, "ENTITIES");
        let line = |p: &mut dyn FnMut(i32, &str), a: (f64, f64), b: (f64, f64)| {
            for (c, v) in [(0, "LINE".to_string()), (5, "30".into()), (330, "1F".into()), (100, "AcDbEntity".into()), (8, "COPPER".into()), (100, "AcDbLine".into()), (10, a.0.to_string()), (20, a.1.to_string()), (30, "0".into()), (11, b.0.to_string()), (21, b.1.to_string()), (31, "0".into())] {
                p(c, &v);
            }
        };
        // A 40 × 20 plate from four loose lines.
        line(&mut put, (0.0, 0.0), (40.0, 0.0));
        line(&mut put, (40.0, 20.0), (40.0, 0.0));
        line(&mut put, (40.0, 20.0), (0.0, 20.0));
        line(&mut put, (0.0, 20.0), (0.0, 0.0));
        // A disc of radius 5 as a cubic NURBS: four clamped Bézier quarters, at (100, 0).
        let k = 0.552_284_75 * 5.0;
        let (cx, cy) = (100.0, 0.0);
        let ctrl = [(5.0, 0.0), (5.0, k), (k, 5.0), (0.0, 5.0), (-k, 5.0), (-5.0, k), (-5.0, 0.0), (-5.0, -k), (-k, -5.0), (0.0, -5.0), (k, -5.0), (5.0, -k), (5.0, 0.0)];
        for (c, v) in [(0, "SPLINE"), (5, "31"), (330, "1F"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbSpline"), (210, "0"), (220, "0"), (230, "1"), (70, "9"), (71, "3"), (72, "17"), (73, "13"), (74, "0"), (42, "1e-9"), (43, "1e-10")] {
            put(c, v);
        }
        for t in ["0", "0", "0", "0", "1", "1", "1", "2", "2", "2", "3", "3", "3", "4", "4", "4", "4"] {
            put(40, t);
        }
        for (x, y) in ctrl {
            put(10, &(cx + x).to_string());
            put(20, &(cy + y).to_string());
            put(30, "0");
        }
        // An ellipse 10 × 4 at (200, 0), its major axis along y.
        for (c, v) in [(0, "ELLIPSE"), (5, "32"), (330, "1F"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbEllipse"), (10, "200"), (20, "0"), (30, "0"), (11, "0"), (21, "5"), (31, "0"), (210, "0"), (220, "0"), (230, "1"), (40, "0.4"), (41, "0"), (42, "6.283185307179586")] {
            put(c, v);
        }
        // A half disc hatch: a line, then a clockwise arc over the top, at (300, 0).
        for (c, v) in [(0, "HATCH"), (5, "33"), (330, "1F"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbHatch"), (10, "0"), (20, "0"), (30, "0"), (210, "0"), (220, "0"), (230, "1"), (2, "SOLID"), (70, "1"), (71, "0"), (91, "1"),
            (92, "1"), (93, "2"),
            (72, "1"), (10, "310"), (20, "0"), (11, "290"), (21, "0"),
            (72, "2"), (10, "300"), (20, "0"), (40, "10"), (50, "180"), (51, "360"), (73, "0"),
            (97, "0"), (75, "0"), (76, "1"), (98, "1"), (10, "300"), (20, "1")] {
            put(c, v);
        }
        // A rectangle hatch with a polyline boundary and a bulge, at (400, 0): 20 × 10 with a half disc end.
        for (c, v) in [(0, "HATCH"), (5, "34"), (330, "1F"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbHatch"), (10, "0"), (20, "0"), (30, "0"), (210, "0"), (220, "0"), (230, "1"), (2, "SOLID"), (70, "1"), (71, "0"), (91, "1"),
            (92, "2"), (72, "1"), (73, "1"), (93, "4"),
            (10, "400"), (20, "0"), (42, "0"), (10, "420"), (20, "0"), (42, "1"), (10, "420"), (20, "10"), (42, "0"), (10, "400"), (20, "10"), (42, "0"),
            (97, "0"), (75, "0"), (76, "1"), (98, "0")] {
            put(c, v);
        }
        // A legacy POLYLINE, on a mirrored extrusion: a 10 mm square around x = −500.
        for (c, v) in [(0, "POLYLINE"), (5, "35"), (330, "1F"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDb2dPolyline"), (66, "1"), (10, "0"), (20, "0"), (30, "0"), (70, "1"), (230, "-1")] {
            put(c, v);
        }
        for (x, y) in [(495.0, -5.0), (505.0, -5.0), (505.0, 5.0), (495.0, 5.0)] {
            for (c, v) in [(0, "VERTEX".to_string()), (5, "36".into()), (100, "AcDbEntity".into()), (8, "COPPER".into()), (100, "AcDbVertex".into()), (100, "AcDb2dVertex".into()), (10, x.to_string()), (20, y.to_string()), (30, "0".into()), (70, "0".into())] {
                put(c, &v);
            }
        }
        for (c, v) in [(0, "SEQEND"), (5, "37"), (100, "AcDbEntity"), (8, "COPPER")] {
            put(c, v);
        }
        // A pad from a block, one on a frozen layer, a solid, a note and a title in paper space.
        for (c, v) in [(0, "INSERT"), (5, "38"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbBlockReference"), (2, "PAD"), (10, "600"), (20, "0"), (30, "0")] {
            put(c, v);
        }
        for (c, v) in [(0, "LINE"), (5, "39"), (100, "AcDbEntity"), (8, "OLD"), (100, "AcDbLine"), (10, "700"), (20, "0"), (30, "0"), (11, "710"), (21, "0"), (31, "0")] {
            put(c, v);
        }
        for (c, v) in [(0, "SOLID"), (5, "3A"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbTrace"), (10, "800"), (20, "0"), (30, "0"), (11, "810"), (21, "0"), (31, "0"), (12, "800"), (22, "5"), (32, "0"), (13, "810"), (23, "5"), (33, "0")] {
            put(c, v);
        }
        for (c, v) in [(0, "MTEXT"), (5, "3B"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbMText"), (10, "0"), (20, "0"), (30, "0"), (40, "2.5"), (1, "Rev A")] {
            put(c, v);
        }
        for (c, v) in [(0, "LWPOLYLINE"), (5, "3C"), (67, "1"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbPolyline"), (90, "3"), (70, "1"), (10, "0"), (20, "0"), (10, "900"), (20, "0"), (10, "900"), (20, "600")] {
            put(c, v);
        }
        for (c, v) in [(0, "3DSOLID"), (5, "3D"), (100, "AcDbEntity"), (8, "COPPER"), (100, "AcDbModelerGeometry"), (70, "1")] {
            put(c, v);
        }
        put(0, "ENDSEC");
        put(0, "EOF");
        s
    }

    #[test]
    fn a_file_in_the_r2000_layout_reads_with_every_entity_kind() {
        let raw = read_dxf(r2000_file().as_bytes()).unwrap();
        assert_eq!(raw.units_to_m, Some(1e-3));
        let c = raw.resolve(Resolve { scale_to_m: 1e-3, chord_tol: 5e-6 }).unwrap();
        let copper = c.layers.iter().find(|l| l.name == "COPPER").unwrap();
        // Find each shape by where it sits.
        let at = |x: f64| copper.islands.iter().find(|i| { let (a, b) = bbox(&i.outer); x * 1e-3 > a[0] && x * 1e-3 < b[0] }).unwrap_or_else(|| panic!("nothing at x = {x}: {} islands", copper.islands.len()));
        let close = |got: f64, want_mm2: f64, name: &str| assert!((got * 1e6 - want_mm2).abs() / want_mm2 < 0.005, "{name}: {} mm² vs {want_mm2}", got * 1e6);
        close(at(20.0).area(), 800.0, "loose lines");
        close(at(100.0).area(), PI * 25.0, "spline");
        close(at(200.0).area(), PI * 5.0 * 2.0, "ellipse");
        close(at(300.0).area(), PI * 50.0, "hatch with a clockwise arc");
        close(at(410.0).area(), 200.0 + PI * 12.5, "hatch with a bulge");
        close(at(-500.0).area(), 100.0, "mirrored polyline");
        close(at(600.0).area(), PI * 4.0, "block on layer 0");
        close(at(805.0).area(), 50.0, "solid with crossed corners");
        assert_eq!(copper.islands.len(), 8, "the paper-space title stays out");
        assert!(copper.open.is_empty(), "{:?}", copper.open);
        let old = c.layers.iter().find(|l| l.name == "OLD").unwrap();
        assert!(old.off && !old.is_metal(), "a frozen layer isn't taken as metal");
        assert_eq!(raw.skipped.get("MTEXT"), Some(&1));
        assert!(c.warnings.iter().any(|w| w.contains("3D solids")), "{:?}", c.warnings);
    }

    #[test]
    fn a_slot_cut_after_the_fact_reads_as_a_hole() {
        let plate = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let a = cut_hole(&plate, &[[2.0, 2.0], [4.0, 2.0], [4.0, 4.0], [2.0, 4.0]]);
        let b = cut_hole(&a, &[[6.0, 6.0], [8.0, 6.0], [8.0, 8.0], [6.0, 8.0]]);
        assert!((signed_area(&b).abs() - (100.0 - 4.0 - 4.0)).abs() < 1e-9, "{}", signed_area(&b));
        assert!(!point_inside(&b, [3.0, 3.0]) && !point_inside(&b, [7.0, 7.0]));
        assert!(point_inside(&b, [1.0, 9.0]) && point_inside(&b, [5.0, 5.0]));
        let area: f64 = triangulate(&b).iter().map(|t| {
            let (p, q, r) = (b[t[0]], b[t[1]], b[t[2]]);
            ((q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])).abs() / 2.0
        }).sum();
        assert!((area - 92.0).abs() < 1e-9, "{area}");
    }

    #[test]
    fn layer_names_say_what_they_are_for() {
        assert_eq!(role_for("FEED_1"), LayerRole::Feed);
        assert_eq!(role_for("Port"), LayerRole::Feed);
        assert_eq!(role_for("SUPPORT"), LayerRole::Metal, "not a port");
        assert_eq!(role_for("Edge.Cuts"), LayerRole::Board);
        assert_eq!(role_for("F.Cu"), LayerRole::Metal);
        assert_eq!(role_for("F.SilkS"), LayerRole::Ignore);
        assert_eq!(role_for("DIMENSIONS"), LayerRole::Ignore);
    }

    #[test]
    fn bad_files_are_refused_with_a_reason() {
        let msg = |b: &[u8]| read_dxf(b).unwrap_err().to_string();
        assert!(msg(b"AutoCAD Binary DXF\r\n\x1a\0rest").contains("binary"));
        assert!(msg(b"hello\nworld\n").contains("not a text DXF"));
        assert!(msg(b"0\nSECTION\n2\nHEADER\n0\nENDSEC\n0\nEOF\n").contains("ENTITIES"));
        assert!(msg(dxf("0\nTEXT\n8\nA\n1\nhello\n").as_bytes()).contains("no outlines"));
        assert!(read("a.dwg", b"x").unwrap_err().to_string().contains("DWG"));
        assert!(read("a.txt", b"plain words").is_err());
    }

    #[test]
    fn exported_dxf_reads_back_with_its_stack() {
        let p = Project::from_template("patch_air", None).unwrap();
        let g = p.geometry().unwrap();
        let c = read_dxf(g.to_dxf().as_bytes()).unwrap().resolve(mm()).unwrap();
        let flat = g.flat_layers();
        assert_eq!(c.layers.len(), flat.len());
        for ((_, at, outlines), l) in flat.iter().zip(&c.layers) {
            assert!((l.z - at).abs() < 1e-9, "{} vs {at}", l.z);
            let want: f64 = outlines.iter().map(|o| signed_area(o).abs()).sum();
            assert!((l.area() - want).abs() / want < 1e-6, "{} vs {want}", l.area());
        }
    }

    #[test]
    fn exported_gerber_reads_back() {
        let p = Project::from_template("patch_air", None).unwrap();
        let g = p.geometry().unwrap();
        let (axis, at, outlines) = g.flat_layers()[1].clone();
        let c = read_gerber(g.to_gerber(axis, at).as_bytes(), "top").unwrap().resolve(mm()).unwrap();
        let want: f64 = outlines.iter().map(|o| signed_area(o).abs()).sum();
        assert!((c.layers[0].area() - want).abs() / want < 1e-6, "{} vs {want}", c.layers[0].area());
    }

    #[test]
    fn gerber_flashes_draws_arcs_and_clear_regions() {
        let g = "%FSLAX46Y46*%\n%MOMM*%\n%ADD10C,2.0*%\n%ADD11R,4.0X3.0*%\n\
                 G01*\nD11*\nX0Y0D03*\n\
                 D10*\nX20000000Y0D02*\nX40000000Y0D01*\n\
                 %LPC*%\nG36*\nX1000000Y500000D02*\nX1500000Y500000D01*\nX1500000Y1000000D01*\nX1000000Y1000000D01*\nX1000000Y500000D01*\nG37*\n%LPD*%\nM02*\n";
        let c = read_gerber(g.as_bytes(), "top").unwrap().resolve(mm()).unwrap();
        let l = &c.layers[0];
        assert_eq!(l.islands.len(), 2, "a pad and a trace");
        // 4 × 3 pad less a 0.5 × 0.5 cut; a 20 mm trace 2 mm wide with round ends.
        let trace = 20.0 * 2.0 + PI;
        let want = (12.0 - 0.25 + trace) * 1e-6;
        assert!((l.area() - want).abs() / want < 0.01, "{} vs {want}", l.area());
        // A full circle as a region, in multi-quadrant mode.
        let g = "%FSLAX46Y46*%\n%MOMM*%\nG75*\nG36*\nX5000000Y0D02*\nG03X5000000Y0I-5000000J0D01*\nG37*\nM02*\n";
        let c = read_gerber(g.as_bytes(), "top").unwrap().resolve(mm()).unwrap();
        let exact = PI * 25e-6;
        // Thinning keeps chords, which sit just inside the curve.
        assert!((c.layers[0].area() - exact).abs() / exact < 5e-3, "{}", c.layers[0].area());
    }

    #[test]
    fn gerber_digits_units_and_macros() {
        // Inches, 2.4 digits: 1.5 in square from a region.
        let g = "%FSLAX24Y24*%%MOIN*%G36*X0Y0D02*X15000Y0D01*X15000Y15000D01*X0Y15000D01*X0Y0D01*G37*M02*";
        let c = read_gerber(g.as_bytes(), "t").unwrap().resolve(mm()).unwrap();
        let want = (1.5f64 * 0.0254).powi(2);
        assert!((c.layers[0].area() - want).abs() / want < 1e-9);
        // Macro pads can't be drawn, and the user hears about it.
        let g = "%FSLAX46Y46*%%MOMM*%%AMRR*21,1,1,1,0,0,0*%%ADD12RR,1*%%ADD10C,1*%D12*X0Y0D03*D10*X5000000Y0D03*M02*";
        let c = read_gerber(g.as_bytes(), "t").unwrap().resolve(mm()).unwrap();
        assert_eq!(c.layers[0].islands.len(), 1);
        assert!(c.warnings.iter().any(|w| w.contains("macros")), "{:?}", c.warnings);
    }

    fn strip_pair(cx: f64, len: f64, w: f64, gap: f64) -> String {
        let (x0, x1) = (cx - w / 2.0, cx + w / 2.0);
        let mut e = lwpoly("COPPER", &[(x0, gap / 2.0), (x1, gap / 2.0), (x1, len / 2.0), (x0, len / 2.0)], true);
        e += &lwpoly("COPPER", &[(x0, -len / 2.0), (x1, -len / 2.0), (x1, -gap / 2.0), (x0, -gap / 2.0)], true);
        e
    }

    #[test]
    fn gaps_are_found_as_feeds_and_neighbours_are_not() {
        let mut e = String::new();
        for k in 0..5 {
            e += &strip_pair(100.0 * k as f64, 150.0, 12.0, 8.0);
        }
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert!(c.feeds_guessed);
        assert_eq!(c.feeds.len(), 5, "{:?}", c.feeds);
        for f in &c.feeds {
            let CadFeed::Gap { a, b, width, .. } = f else { panic!() };
            assert!((dist(*a, *b) - 0.008).abs() < 1e-9, "{f:?}");
            assert!((width - 0.012).abs() < 1e-6, "{f:?}");
            assert_eq!(a[0], b[0], "along y");
        }
    }

    #[test]
    fn a_feed_layer_wins_and_snaps_to_the_metal() {
        let mut e = strip_pair(0.0, 150.0, 12.0, 8.0);
        // Slightly short and slightly crooked, as hand-drawn lines are.
        e += "0\nLINE\n8\nFEED\n10\n0.1\n20\n-3.9\n11\n0.1\n21\n3.7\n";
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert!(!c.feeds_guessed);
        assert_eq!(c.feeds.len(), 1, "{:?} {:?}", c.feeds, c.warnings);
        let CadFeed::Gap { a, b, width, .. } = &c.feeds[0] else { panic!() };
        assert!((a[1] + 0.004).abs() < 1e-9 && (b[1] - 0.004).abs() < 1e-9, "{a:?} {b:?}");
        assert!((width - 0.012).abs() < 1e-6);
        // A line at an angle can't be a port, and the file gets told why.
        let mut bad = strip_pair(0.0, 150.0, 12.0, 8.0);
        bad += "0\nLINE\n8\nFEED\n10\n-3\n20\n-4\n11\n3\n21\n4\n";
        let c = read_dxf(dxf(&bad).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert!(c.warnings.iter().any(|w| w.contains("at an angle")), "{:?}", c.warnings);
    }

    #[test]
    fn a_circle_on_a_feed_layer_is_a_probe_between_the_layers() {
        let mut e = String::new();
        e += "0\nLWPOLYLINE\n8\nGND\n90\n4\n70\n1\n38\n0\n10\n-50\n20\n-50\n10\n50\n20\n-50\n10\n50\n20\n50\n10\n-50\n20\n50\n";
        e += "0\nLWPOLYLINE\n8\nPATCH\n90\n4\n70\n1\n38\n5\n10\n-20\n20\n-20\n10\n20\n20\n-20\n10\n20\n20\n20\n10\n-20\n20\n20\n";
        e += "0\nCIRCLE\n8\nFEED\n10\n0\n20\n8\n40\n0.5\n";
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert!(matches!(c.feeds[0], CadFeed::Probe { .. }));
        let how = Placement { plane: CadPlane::Flat, center: false, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
        let (p, added) = new_project(&c, "patch.dxf", "2.45GHz", &how).unwrap();
        assert_eq!(added, Added { parts: 2, ports: 1 });
        let g = p.geometry().unwrap();
        assert_eq!(g.ports[0].axis(), 2);
        assert!((g.ports[0].b[2] - 0.005).abs() < 1e-9);
        assert!(g.solver_fit().surface_mom);
    }

    #[test]
    fn upright_placement_stands_the_drawing_up() {
        let e = strip_pair(0.0, 150.0, 12.0, 8.0);
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        for plane in CadPlane::ALL {
            let how = Placement { plane, center: true, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
            let (p, _) = new_project(&c, "d.dxf", "915MHz", &how).unwrap();
            let g = p.geometry().unwrap();
            let (lo, hi) = g.bounds();
            let size: Vec<f64> = (0..3).map(|k| hi[k] - lo[k]).collect();
            let up = match plane {
                CadPlane::Flat => 1,
                _ => 2,
            };
            assert!((size[up] - 0.15).abs() < 1e-9, "{plane:?} {size:?}");
            assert!(size[plane.axis()].abs() < 1e-9, "{plane:?} {size:?}");
            assert_eq!(g.ports[0].axis(), up, "{plane:?}");
        }
    }

    /// The whole point of an import: the drawing of an antenna solves like the antenna.
    fn solve_z(g: &Geometry, f: f64, edges: f64) -> crate::C64 {
        let opts = MomOptions { edges_per_lambda: edges, ..Default::default() };
        MomProblem::new(g, f, opts).unwrap().solve(f, false).unwrap().z[0][0]
    }

    #[test]
    fn a_drawn_strip_dipole_solves_like_the_template() {
        let t = Project::from_template("strip_dipole", Some("1GHz")).unwrap();
        let v = t.vars().unwrap();
        let (l, w, g) = (v["L"] * 1e3, v["w"] * 1e3, v["g"] * 1e3);
        let c = read_dxf(dxf(&strip_pair(0.0, l, w, g)).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert_eq!(c.feeds.len(), 1, "{:?}", c.feeds);
        let how = Placement { plane: CadPlane::FacingY, center: true, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
        let (mut p, _) = new_project(&c, "strip.dxf", "1GHz", &how).unwrap();
        p.params.iter_mut().find(|r| r.name == "Z0").unwrap().expr = "73".into();
        let (zt, zd) = (solve_z(&t.geometry().unwrap(), 1e9, 14.0), solve_z(&p.geometry().unwrap(), 1e9, 14.0));
        println!("template {zt:.2} Ω, drawn {zd:.2} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.02, "template {zt} vs drawn {zd}");
    }

    #[test]
    fn a_drawn_slot_solves_like_the_template() {
        let t = Project::from_template("slot", Some("2.45GHz")).unwrap();
        let v = t.vars().unwrap();
        let (gx, gy, ls, ws) = (v["Gx"] * 1e3, v["Gy"] * 1e3, v["Ls"] * 1e3, v["ws"] * 1e3);
        let mut e = lwpoly("PLATE", &[(-gx / 2.0, -gy / 2.0), (gx / 2.0, -gy / 2.0), (gx / 2.0, gy / 2.0), (-gx / 2.0, gy / 2.0)], true);
        e += &lwpoly("PLATE", &[(-ws / 2.0, -ls / 2.0), (ws / 2.0, -ls / 2.0), (ws / 2.0, ls / 2.0), (-ws / 2.0, ls / 2.0)], true);
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        assert_eq!(c.layers[0].islands[0].holes.len(), 1);
        assert_eq!(c.feeds.len(), 1, "the slot is found as a feed: {:?}", c.feeds);
        let how = Placement { plane: CadPlane::Flat, center: true, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
        let (mut p, _) = new_project(&c, "slot.dxf", "2.45GHz", &how).unwrap();
        p.params.iter_mut().find(|r| r.name == "Z0").unwrap().expr = "500".into();
        let edges = t.setup.edges_per_lambda;
        let (zt, zd) = (solve_z(&t.geometry().unwrap(), 2.45e9, edges), solve_z(&p.geometry().unwrap(), 2.45e9, edges));
        println!("template {zt:.1} Ω, drawn {zd:.1} Ω");
        assert!((zt - zd).norm() / zt.norm() < 0.08, "template {zt} vs drawn {zd}");
    }

    /// Writes the sample drawing of the catalogue's gate:
    /// `cargo test -p antenna_core write_gate_fixture -- --ignored`
    #[test]
    #[ignore]
    fn write_gate_fixture() {
        let p = Project::from_template("rfid_gate", None).unwrap();
        let v = p.vars().unwrap();
        let mm = |name: &str| v[name] * 1e3;
        let (l, w, g, dz, zc, d, pw, pm) = (mm("L"), mm("w"), mm("g"), mm("dz"), mm("zc"), mm("D"), mm("Pw"), mm("Pm"));
        let mut out = String::new();
        let mut put = |c: i32, s: String| out.push_str(&format!("{c:>3}\r\n{s}\r\n"));
        let header: &[(i32, &str)] = &[
            (999, "Sense Sim sample: a five element RFID gate panel. Drawing x is model y, drawing y is model z, in mm. Import upright, facing x."),
            (0, "SECTION"), (2, "HEADER"), (9, "$ACADVER"), (1, "AC1015"), (9, "$INSUNITS"), (70, "4"), (0, "ENDSEC"),
            (0, "SECTION"), (2, "TABLES"), (0, "TABLE"), (2, "LAYER"), (100, "AcDbSymbolTable"), (70, "4"),
        ];
        for (c, s) in header {
            put(*c, s.to_string());
        }
        for (name, color) in [("PANEL", "8"), ("ELEMENTS", "1"), ("FEED", "3"), ("NOTES", "7")] {
            for (c, s) in [(0, "LAYER"), (100, "AcDbSymbolTableRecord"), (100, "AcDbLayerTableRecord"), (2, name), (70, "0"), (62, color), (6, "CONTINUOUS")] {
                put(c, s.to_string());
            }
        }
        for (c, s) in [(0, "ENDTAB"), (0, "ENDSEC"), (0, "SECTION"), (2, "ENTITIES")] {
            put(c, s.to_string());
        }
        let rect = |put: &mut dyn FnMut(i32, String), layer: &str, z: f64, x0: f64, y0: f64, x1: f64, y1: f64| {
            for (c, s) in [(0, "LWPOLYLINE"), (100, "AcDbEntity"), (8, layer), (100, "AcDbPolyline"), (90, "4"), (70, "1")] {
                put(c, s.to_string());
            }
            put(38, format!("{z:.3}"));
            for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
                put(10, format!("{x:.3}"));
                put(20, format!("{y:.3}"));
            }
        };
        let n = 5;
        let height = |k: usize| zc + (k as f64 - (n as f64 - 1.0) / 2.0) * dz;
        rect(&mut put, "PANEL", 0.0, -pw / 2.0, height(0) - l / 2.0 - pm, pw / 2.0, height(n - 1) + l / 2.0 + pm);
        let mut feeds = Vec::new();
        for k in 0..n {
            let z = height(k);
            if k % 2 == 1 {
                rect(&mut put, "ELEMENTS", d, -l / 2.0, z - w / 2.0, -g / 2.0, z + w / 2.0);
                rect(&mut put, "ELEMENTS", d, g / 2.0, z - w / 2.0, l / 2.0, z + w / 2.0);
                feeds.push(((-g / 2.0, z), (g / 2.0, z)));
            } else {
                rect(&mut put, "ELEMENTS", d, -w / 2.0, z - l / 2.0, w / 2.0, z - g / 2.0);
                rect(&mut put, "ELEMENTS", d, -w / 2.0, z + g / 2.0, w / 2.0, z + l / 2.0);
                feeds.push(((0.0, z - g / 2.0), (0.0, z + g / 2.0)));
            }
        }
        for (a, b) in feeds {
            for (c, s) in [(0, "LINE"), (100, "AcDbEntity"), (8, "FEED"), (100, "AcDbLine")] {
                put(c, s.to_string());
            }
            for (c, s) in [(10, a.0), (20, a.1), (30, d), (11, b.0), (21, b.1), (31, d)] {
                put(c, format!("{s:.3}"));
            }
        }
        for (c, s) in [(0, "MTEXT"), (100, "AcDbEntity"), (8, "NOTES"), (100, "AcDbMText"), (10, "300"), (20, "0"), (30, "0"), (40, "20"), (1, "Five element gate, 915 MHz"), (0, "ENDSEC"), (0, "EOF")] {
            put(c, s.to_string());
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("five_element_gate.dxf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, out).unwrap();
    }

    /// The sample drawing against the template it was drawn from: the import path
    /// from a file on disk to the solver, ports and coupling included.
    #[test]
    fn the_sample_gate_drawing_solves_like_the_template() {
        let bytes = include_bytes!("../fixtures/five_element_gate.dxf");
        let c = read("five_element_gate.dxf", bytes).unwrap().resolve(Resolve::default()).unwrap();
        assert!(!c.feeds_guessed);
        assert_eq!(c.feeds.len(), 5, "{:?} {:?}", c.feeds, c.warnings);
        let how = Placement { plane: CadPlane::FacingX, center: false, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
        let (mut drawn, added) = new_project(&c, "five_element_gate.dxf", "915MHz", &how).unwrap();
        assert_eq!(added, Added { parts: 11, ports: 5 });
        drawn.setup = Project::from_template("rfid_gate", None).unwrap().setup;
        let template = Project::from_template("rfid_gate", None).unwrap();
        let (gt, gd) = (template.geometry().unwrap(), drawn.geometry().unwrap());
        let opts = MomOptions { edges_per_lambda: 8.0, theta_step: 15.0, phi_step: 15.0, ..Default::default() };
        let (pt, pd) = (MomProblem::new(&gt, 915e6, opts).unwrap(), MomProblem::new(&gd, 915e6, opts).unwrap());
        let (zt, zd) = (pt.solve(915e6, false).unwrap().z, pd.solve(915e6, false).unwrap().z);
        for i in 0..5 {
            for j in 0..5 {
                let diff = (zt[i][j] - zd[i][j]).norm() / zt[i][i].norm();
                assert!(diff < 0.03, "Z{}{}: template {} vs drawn {}", i + 1, j + 1, zt[i][j], zd[i][j]);
            }
        }
    }

    #[test]
    fn an_imported_outline_carries_a_mesh_hint_and_exports_again() {
        let e = strip_pair(0.0, 150.0, 2.0, 1.0);
        let c = read_dxf(dxf(&e).as_bytes()).unwrap().resolve(mm()).unwrap();
        let how = Placement { plane: CadPlane::Flat, center: true, layers: default_placements(&c, "PEC"), feeds: c.feeds.clone() };
        let (p, _) = new_project(&c, "fine.dxf", "915MHz", &how).unwrap();
        let g = p.geometry().unwrap();
        assert!(g.hints.iter().all(|h| h.cell > 0.0 && h.cell < 1e-3), "{:?}", g.hints);
        assert!(!g.hints.is_empty());
        assert_eq!(g.to_dxf().matches("LWPOLYLINE").count(), 2);
        assert!(matches!(g.solids[0].shape, Shape::Polygon { axis: 2, .. }));
        let q = Project::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(p, q);
    }
}
