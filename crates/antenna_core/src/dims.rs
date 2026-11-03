//! Which distance on the model each variable is, so the canvas can write `L = 70 mm` on it.
//!
//! A template's formulas are code and a drawing's are text, and neither says "this is the
//! length". So it's measured: each variable is nudged, the model is built again, and a variable
//! gets a line only between two points that are exactly its value apart and whose distance
//! follows it one for one. A label can't sit on a distance that only happens to have the same
//! size, and a frequency, a count or a variable nothing uses gets no line at all.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::expr::Vars;
use crate::geometry::{P3, PortDef, Shape, Solid, add, cross, norm, plane_point, scale, sub};
use crate::project::Project;

/// How a dimension is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimKind {
    /// A distance between two points, its line standing clear of the model.
    Between,
    /// A round part's size. The line stays on the part.
    Inside,
}

/// One variable, where it can be seen on the model.
#[derive(Clone, Debug, PartialEq)]
pub struct Dim {
    pub name: String,
    /// Metres.
    pub value: f64,
    pub kind: DimKind,
    /// Where its line runs: from the first point it measures, along the axis it measures on.
    pub line: [P3; 2],
    /// The second point it measures, which the line's end may be level with and not on.
    pub to: P3,
    /// The part it was measured on.
    pub part: String,
    /// Shown all the time. The rest wait until someone points at their row in the table.
    pub main: bool,
}

impl Dim {
    pub fn text(&self) -> String {
        format!("{} = {}", self.name, length_text(self.value))
    }
}

/// A length as a dimension reads: 70 mm, 0.5 mm, 1.2 m, 50 µm.
pub fn length_text(metres: f64) -> String {
    let trim = |s: String| if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    let a = metres.abs();
    if a >= 1.0 {
        format!("{} m", trim(format!("{metres:.3}")))
    } else if a >= 0.01e-3 {
        format!("{} mm", trim(format!("{:.3}", metres * 1e3)))
    } else {
        format!("{} µm", trim(format!("{:.2}", metres * 1e6)))
    }
}

/// How far each variable is nudged, of its value. Big enough to stand out from what a sketch
/// solver leaves behind, small enough not to change the model's shape.
const NUDGE: f64 = 0.01;
/// A distance within this much of the value, of the value, is that value.
const SAME_SIZE: f64 = 1e-5;
/// Nothing on a model is nearer than this to anything without being the same place.
const SAME_PLACE: f64 = 2e-9;
/// How far from one for one a distance may be and still be said to follow the variable.
const FOLLOWS: f64 = 0.02;
/// Above this a value is a frequency or the like, not a length. 10 km.
const LONGEST: f64 = 1e4;
/// A part with more points than this is judged by its outer box: the corners of a smooth curve
/// are nothing anyone would dimension.
const MOST_POINTS: usize = 400;
/// With more than this in the model, distances between parts are no longer looked at.
const MOST_ACROSS: usize = 6000;
/// A model too slow to rebuild once per variable keeps what was found by then.
const TIME_LIMIT: Duration = Duration::from_secs(6);

/// A round part's size, which no two points on it state.
#[derive(Clone, Copy, Debug)]
struct Round {
    center: P3,
    /// Square to its axis, length 1.
    out: P3,
    r: f64,
}

/// What can be measured on one solid or feed.
#[derive(Clone, Debug)]
struct Part {
    name: String,
    /// The first this many points are the outer box and its middle, which still line up with
    /// the nudged model when the outline's point count changes.
    stable: usize,
    /// Points before this one are the box around the part, not on it.
    on_part: usize,
    points: Vec<P3>,
    /// Pairs of points that are really edges, for lengths that aren't along an axis.
    edges: Vec<(usize, usize)>,
    round: Option<Round>,
}

/// How many points [`outer_box`] gives, and how many of them are corners.
const BOX_POINTS: usize = 9;
const BOX_CORNERS: usize = 8;

/// The corners of a box, then its middle: where a strip's centre line runs.
fn outer_box(lo: P3, hi: P3) -> Vec<P3> {
    let mut points: Vec<P3> = (0..BOX_CORNERS).map(|i| [if i & 1 == 0 { lo[0] } else { hi[0] }, if i & 2 == 0 { lo[1] } else { hi[1] }, if i & 4 == 0 { lo[2] } else { hi[2] }]).collect();
    points.push(scale(add(lo, hi), 0.5));
    points
}

fn bounds_of(points: &[P3]) -> (P3, P3) {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in points {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// A direction square to `d`, for the side of something round.
fn square_to(d: P3) -> P3 {
    let along = (0..3).min_by(|a, b| d[*a].abs().total_cmp(&d[*b].abs())).unwrap_or(0);
    let mut e = [0.0; 3];
    e[along] = 1.0;
    let c = cross(d, e);
    let n = norm(c);
    if n > 0.0 { scale(c, 1.0 / n) } else { [1.0, 0.0, 0.0] }
}

/// A flat outline that is a circle: a disc comes as a polygon, whose width is a little short of
/// its diameter when it has an odd number of sides.
fn circle_in(axis: usize, at: f64, pts: &[[f64; 2]]) -> Option<Round> {
    if pts.len() < 12 {
        return None;
    }
    let n = pts.len() as f64;
    let c = [pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n];
    let r = (pts[0][0] - c[0]).hypot(pts[0][1] - c[1]);
    if !(r > 0.0) || pts.iter().any(|p| ((p[0] - c[0]).hypot(p[1] - c[1]) - r).abs() > r * 1e-6) {
        return None;
    }
    Some(Round { center: plane_point(axis, at, c), out: plane_point(axis, 0.0, [(pts[0][0] - c[0]) / r, (pts[0][1] - c[1]) / r]), r })
}

fn part_of(s: &Solid) -> Part {
    let name = s.name.clone();
    match &s.shape {
        Shape::Box { min, max } => Part { name, stable: BOX_POINTS, on_part: 0, points: outer_box(*min, *max), edges: Vec::new(), round: None },
        Shape::Cylinder { a, b, r } => {
            let (lo, hi) = bounds_of(&[*a, *b]);
            let mut points = outer_box(lo, hi);
            points.extend([*a, *b]);
            let center = scale(add(*a, *b), 0.5);
            Part { name, stable: BOX_POINTS, on_part: BOX_CORNERS, points, edges: vec![(BOX_POINTS, BOX_POINTS + 1)], round: Some(Round { center, out: square_to(sub(*b, *a)), r: *r }) }
        }
        Shape::Sphere { c, r } => Part { name, stable: BOX_POINTS, on_part: BOX_CORNERS, points: outer_box(*c, *c), edges: Vec::new(), round: Some(Round { center: *c, out: [1.0, 0.0, 0.0], r: *r }) },
        Shape::Polygon { axis, at, pts, thick } => {
            let (lo, hi) = s.shape.bounds();
            let mut points = outer_box(lo, hi);
            let mut edges = Vec::new();
            let n = pts.len();
            if n <= MOST_POINTS {
                let first = points.len();
                points.extend(pts.iter().map(|p| plane_point(*axis, *at, *p)));
                edges.extend((0..n).map(|i| (first + i, first + (i + 1) % n)));
                if *thick > 0.0 {
                    points.extend(pts.iter().map(|p| plane_point(*axis, *at + *thick, *p)));
                }
                // A trace is its two sides, the second coming back: the middle of each opposite
                // pair is the path it was drawn along, which is what its variables set.
                if n % 2 == 0 {
                    points.extend((0..n / 2).map(|i| plane_point(*axis, *at, [(pts[i][0] + pts[n - 1 - i][0]) / 2.0, (pts[i][1] + pts[n - 1 - i][1]) / 2.0])));
                }
            }
            Part { name, stable: BOX_POINTS, on_part: BOX_CORNERS, points, edges, round: circle_in(*axis, *at, pts) }
        }
        Shape::Wire { pts, r } => {
            let (lo, hi) = bounds_of(pts);
            let mut points = outer_box(lo, hi);
            let first = points.len();
            points.extend(pts.iter().copied());
            let edges: Vec<(usize, usize)> = (1..pts.len()).map(|i| (first + i - 1, first + i)).collect();
            // On its longest straight run, where there's most room for the label.
            let longest = (1..pts.len()).max_by(|a, b| norm(sub(pts[*a], pts[*a - 1])).total_cmp(&norm(sub(pts[*b], pts[*b - 1]))));
            let round = longest.map(|i| Round { center: scale(add(pts[i - 1], pts[i]), 0.5), out: square_to(sub(pts[i], pts[i - 1])), r: *r });
            Part { name, stable: BOX_POINTS, on_part: BOX_CORNERS, points, edges, round }
        }
        Shape::Tris { tris, .. } => {
            let (lo, hi) = s.shape.bounds();
            let mut points = outer_box(lo, hi);
            let mut seen = BTreeSet::new();
            let mut corners: Vec<P3> = Vec::new();
            for q in tris.iter().flatten() {
                // A femtometre grid: only what's the same corner of two triangles falls together.
                if seen.insert(q.map(|c| (c * 1e15).round() as i64)) {
                    corners.push(*q);
                    if corners.len() > MOST_POINTS {
                        corners.clear();
                        break;
                    }
                }
            }
            points.extend(corners);
            Part { name, stable: BOX_POINTS, on_part: BOX_CORNERS, points, edges: Vec::new(), round: None }
        }
    }
}

fn port_part(p: &PortDef) -> Part {
    Part { name: p.name.clone(), stable: 0, on_part: 0, points: vec![p.a, p.b], edges: vec![(0, 1)], round: None }
}

/// What's modelled in 3D, or `None` if a step of it fails. Besides the bodies, the sketches and
/// the primitive solids: what a cut took away has left no part to measure, and they still say
/// how big it was.
fn model_parts(p: &Project, vars: &Vars) -> Option<Vec<Part>> {
    use crate::modeler::sketch::CurveKind;
    use crate::modeler::{Kind, Prim, length};
    if p.model.is_empty() {
        return Some(Vec::new());
    }
    let built = p.model.build_unremembered(vars);
    if !built.errors.is_empty() {
        return None;
    }
    let mut parts: Vec<Part> = built.solids(&p.metal).iter().map(part_of).collect();
    parts.extend(built.ports.iter().map(port_part));
    let three = |q: &[String; 3]| -> Option<P3> { Some([length(&q[0], vars).ok()?, length(&q[1], vars).ok()?, length(&q[2], vars).ok()?]) };
    let bare = |name: &str, points: Vec<P3>, round: Option<Round>| Part { name: name.to_string(), stable: 0, on_part: 0, points, edges: Vec::new(), round };
    let count = p.model.rollback.unwrap_or(p.model.features.len()).min(p.model.features.len());
    for f in p.model.features[..count].iter().filter(|f| !f.off) {
        match &f.kind {
            Kind::Sketch { sketch, .. } => {
                let Some(s) = built.sketches.get(&f.id) else { continue };
                let mut ids: Vec<u32> = s.solved.points.keys().copied().collect();
                ids.sort_unstable();
                let index = |id: u32| ids.binary_search(&id).ok();
                let mut part = bare(&f.name, ids.iter().map(|id| s.frame.at(s.solved.at(*id), 0.0)).collect(), None);
                for c in &sketch.curves {
                    match c.kind {
                        CurveKind::Line { a, b } => {
                            if let (Some(x), Some(y)) = (index(a), index(b)) {
                                part.edges.push((x, y));
                            }
                        }
                        CurveKind::Circle { c: center, .. } => {
                            let Some(r) = s.solved.radii.get(&c.id) else { continue };
                            parts.push(bare(&f.name, Vec::new(), Some(Round { center: s.frame.at(s.solved.at(center), 0.0), out: s.frame.u, r: *r })));
                        }
                        _ => {}
                    }
                }
                parts.push(part);
            }
            Kind::Solid { shape: Prim::Cylinder { a, b, r }, .. } => {
                let (Some(a), Some(b), Ok(r)) = (three(a), three(b), length(r, vars)) else { continue };
                parts.push(bare(&f.name, vec![a, b], Some(Round { center: scale(add(a, b), 0.5), out: square_to(sub(b, a)), r })));
            }
            Kind::Solid { shape: Prim::Sphere { c, r }, .. } => {
                let (Some(c), Ok(r)) = (three(c), length(r, vars)) else { continue };
                parts.push(bare(&f.name, vec![c], Some(Round { center: c, out: [1.0, 0.0, 0.0], r })));
            }
            _ => {}
        }
    }
    Some(parts)
}

/// The model's parts at these values, or `None` if it doesn't build with them. `bodies`, if
/// given, is what's modelled in 3D, when these values can't have changed it.
fn parts_at(p: &Project, vars: &Vars, bodies: Option<&[Part]>) -> Option<Vec<Part>> {
    let g = p.parts_geometry(vars).ok()?;
    let mut parts: Vec<Part> = g.solids.iter().map(part_of).collect();
    parts.extend(g.ports.iter().map(port_part));
    match bodies {
        Some(b) => parts.extend_from_slice(b),
        None => parts.extend(model_parts(p, vars)?),
    }
    Some(parts)
}

/// How the model moved when a variable did, per metre of it.
struct Moved {
    /// Per part, per point.
    rate: Vec<Vec<P3>>,
    /// Per part: how fast its roundness grows.
    round_rate: Vec<f64>,
    /// The nudged points, for edges that aren't along an axis.
    nudged: Vec<Vec<P3>>,
    step: f64,
}

/// How the model moved, or `None` if it came out as a different set of parts.
fn moved_between(base: &[Part], nudged: &[Part], step: f64) -> Option<Moved> {
    if base.len() != nudged.len() {
        return None;
    }
    let mut m = Moved { rate: Vec::with_capacity(base.len()), round_rate: Vec::with_capacity(base.len()), nudged: Vec::with_capacity(base.len()), step };
    for (a, b) in base.iter().zip(nudged) {
        // A changed outline still has its outer box to go by.
        let same_points = a.points.len() == b.points.len();
        let rate: Vec<P3> = a.points.iter().enumerate().map(|(i, p)| {
            if same_points || i < a.stable { std::array::from_fn(|k| (b.points[i][k] - p[k]) / step) } else { [f64::NAN; 3] }
        }).collect();
        m.rate.push(rate);
        m.round_rate.push(match (a.round, b.round) {
            (Some(x), Some(y)) => (y.r - x.r) / step,
            _ => 0.0,
        });
        m.nudged.push(if same_points { b.points.clone() } else { Vec::new() });
    }
    Some(m)
}

/// One point of the model, and how fast it moves along the axis being looked at.
#[derive(Clone, Copy)]
struct Row {
    at: f64,
    rate: f64,
    part: usize,
    point: usize,
}

/// Better first: what a reader would take for that dimension.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct Rank(u8, f64);

struct Found {
    rank: Rank,
    kind: DimKind,
    to: P3,
    line: [P3; 2],
    part: usize,
}

fn keep(best: &mut Option<Found>, f: Found) {
    if best.as_ref().is_none_or(|b| f.rank.partial_cmp(&b.rank) == Some(std::cmp::Ordering::Less)) {
        *best = Some(f);
    }
}

/// How well a line's place suits it, smaller being better: far from the model's middle, so it
/// stands clear of everything, and on the low side when both sides are as good.
fn out_from(middle: P3, axis: usize, a: P3) -> f64 {
    let mut far = 0.0;
    let mut low = 0.0;
    for k in (0..3).filter(|k| *k != axis) {
        far += (a[k] - middle[k]).abs();
        low += a[k] - middle[k];
    }
    -(far - low * 1e-6)
}

fn rows_of(parts: &[Part], moved: &Moved, axis: usize, only: Option<usize>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        for (j, p) in part.points.iter().enumerate() {
            let rate = moved.rate[i][j][axis];
            if rate.is_finite() {
                rows.push(Row { at: p[axis], rate, part: i, point: j });
            }
        }
    }
    rows.sort_by(|a, b| a.at.total_cmp(&b.at));
    rows
}

/// Pairs along one axis `times` the value apart whose distance follows the variable at that rate.
fn along_axis(rows: &[Row], value: f64, times: f64, mut each: impl FnMut(Row, Row)) {
    let want = value * times;
    let near = (want * SAME_SIZE).max(SAME_PLACE);
    for low in rows {
        let start = rows.partition_point(|r| r.at < low.at + want - near);
        for high in rows[start..].iter().take_while(|r| r.at <= low.at + want + near) {
            if ((high.rate - low.rate) - times).abs() < FOLLOWS * times {
                each(*low, *high);
            }
        }
    }
}

fn is_real(parts: &[Part], r: Row) -> bool {
    r.point >= parts[r.part].on_part
}

fn lined_up(a: P3, b: P3, axis: usize, near: f64) -> bool {
    (0..3).filter(|k| *k != axis).all(|k| (a[k] - b[k]).abs() <= near)
}

/// A line along `axis` from the outer of two points, to level with the other.
fn foot_of(middle: P3, axis: usize, a: P3, b: P3) -> (P3, P3, P3) {
    let (start, end) = if out_from(middle, axis, a) <= out_from(middle, axis, b) { (a, b) } else { (b, a) };
    let mut foot = start;
    foot[axis] = end[axis];
    (start, end, foot)
}

/// The one line that best shows this variable. In order: what a round part says outright, a
/// distance on one part, one between parts, half of something mirrored, a place from the origin.
fn find(parts: &[Part], moved: &Moved, value: f64, middle: P3, negative: bool) -> Option<Found> {
    let mut best: Option<Found> = None;
    let near = (value * SAME_SIZE).max(SAME_PLACE);

    for (i, part) in parts.iter().enumerate() {
        let Some(round) = part.round else { continue };
        let rate = moved.round_rate[i];
        let rim = |r: f64, side: f64| -> P3 { add(round.center, scale(round.out, r * side)) };
        let ends = if (round.r - value).abs() <= near && (rate - 1.0).abs() < FOLLOWS {
            Some((round.center, rim(round.r, 1.0)))
        } else if (2.0 * round.r - value).abs() <= near && (2.0 * rate - 1.0).abs() < FOLLOWS {
            Some((rim(round.r, -1.0), rim(round.r, 1.0)))
        } else {
            None
        };
        if let Some((from, to)) = ends {
            keep(&mut best, Found { rank: Rank(0, i as f64), kind: DimKind::Inside, to, line: [from, to], part: i });
        }
    }
    if best.is_some() {
        return best;
    }

    let moves = |i: usize| moved.rate[i].iter().any(|r| r.iter().any(|x| x.is_finite() && x.abs() > 1e-6));
    let changed: Vec<usize> = (0..parts.len()).filter(|i| moves(*i)).collect();

    for &i in &changed {
        for axis in 0..3 {
            let rows = rows_of(parts, moved, axis, Some(i));
            along_axis(&rows, value, 1.0, |low, high| {
                let (a, b) = (parts[i].points[low.point], parts[i].points[high.point]);
                let class = match (lined_up(a, b, axis, near), is_real(parts, low) && is_real(parts, high)) {
                    (true, true) => 1,
                    (true, false) => 4,
                    (false, true) => 5,
                    (false, false) => 7,
                };
                let (from, to, foot) = foot_of(middle, axis, a, b);
                keep(&mut best, Found { rank: Rank(class, out_from(middle, axis, from)), kind: DimKind::Between, to, line: [from, foot], part: i });
            });
        }
        // A slanted edge, which no axis measures.
        if !moved.nudged[i].is_empty() {
            for &(x, y) in &parts[i].edges {
                let (a, b) = (parts[i].points[x], parts[i].points[y]);
                let (na, nb) = (moved.nudged[i][x], moved.nudged[i][y]);
                let (length, longer) = (norm(sub(b, a)), norm(sub(nb, na)));
                if (length - value).abs() <= near && ((longer - length) / moved.step - 1.0).abs() < FOLLOWS && (0..3).all(|axis| !lined_up(a, b, axis, near)) {
                    keep(&mut best, Found { rank: Rank(2, 0.0), kind: DimKind::Between, to: b, line: [a, b], part: i });
                }
            }
        }
    }

    // Between parts: a gap, a height over the ground, how far apart two elements are.
    let total: usize = parts.iter().map(|p| p.points.len()).sum();
    let few = total <= MOST_ACROSS;
    if best.as_ref().is_none_or(|b| b.rank.0 > 3) && few {
        for axis in 0..3 {
            let rows = rows_of(parts, moved, axis, None);
            along_axis(&rows, value, 1.0, |low, high| {
                if low.part == high.part {
                    return;
                }
                let (a, b) = (parts[low.part].points[low.point], parts[high.part].points[high.point]);
                let class = match (lined_up(a, b, axis, near), is_real(parts, low) && is_real(parts, high)) {
                    (true, true) => 3,
                    (true, false) | (false, true) => 6,
                    (false, false) => 8,
                };
                let (from, to, foot) = foot_of(middle, axis, a, b);
                // The points that face each other, not the far corners of the two parts.
                keep(&mut best, Found { rank: Rank(class, norm(sub(a, b)) - value), kind: DimKind::Between, to, line: [from, foot], part: low.part });
            });
        }
    }
    if best.is_some() {
        return best;
    }

    // Set from the middle, with the same again on the other side: arms at ±x.
    if few {
        for axis in 0..3 {
            let rows = rows_of(parts, moved, axis, None);
            along_axis(&rows, value, 2.0, |low, high| {
                let (a, b) = (parts[low.part].points[low.point], parts[high.part].points[high.point]);
                // On the side the variable says: x1 = -17.5 mm goes left of the middle.
                let (end, part) = if negative { (a, low.part) } else { (b, high.part) };
                let mut foot = end;
                foot[axis] = (a[axis] + b[axis]) / 2.0;
                let class = if is_real(parts, low) && is_real(parts, high) { 9 } else { 10 };
                keep(&mut best, Found { rank: Rank(class, out_from(middle, axis, end)), kind: DimKind::Between, to: foot, line: [end, foot], part });
            });
        }
    }
    if best.is_some() {
        return best;
    }

    // A place given from the origin, measured from the plane through it.
    for &i in &changed {
        for axis in 0..3 {
            for (j, p) in parts[i].points.iter().enumerate() {
                let rate = moved.rate[i][j][axis];
                if (p[axis].abs() - value).abs() <= near && (rate * p[axis].signum() - 1.0).abs() < FOLLOWS {
                    let mut foot = *p;
                    foot[axis] = 0.0;
                    let class = if j >= parts[i].on_part { 11 } else { 12 };
                    keep(&mut best, Found { rank: Rank(class, out_from(middle, axis, *p)), kind: DimKind::Between, to: foot, line: [*p, foot], part: i });
                }
            }
        }
    }
    best
}

fn same_line(a: &Dim, f: &Found, near: f64) -> bool {
    let close = |p: P3, q: P3| norm(sub(p, q)) <= near;
    (close(a.line[0], f.line[0]) && close(a.line[1], f.line[1])) || (close(a.line[0], f.line[1]) && close(a.line[1], f.line[0]))
}

/// The variable's name as a whole word in `text`.
fn mentions(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word) && !text[at + name.len()..].chars().next().is_some_and(word)
    })
}

/// Past this many labels a small model is hidden behind them, and past this many rows the table
/// stops saying what the design is.
const MOST_SHOWN: usize = 12;

/// Whether a coordinate is this name by itself, or its mirror image.
fn is_bare(coordinate: &str, name: &str) -> bool {
    let c = coordinate.trim();
    c.strip_prefix(['-', '+']).unwrap_or(c).trim() == name
}

/// The rows that only say where a corner of a drawn path is, when there are too many rows to read.
/// A zig-zag drawn with every corner named has `xl1` to `xl6` and so on, which are coordinates
/// and not what anyone would dimension; what shows is the sizes they were worked out from. A row
/// also written as a width or radius stays, as does one that other rows are written over: that
/// makes it a dimension, whatever else it does. Read from the text, so a row doesn't jump between
/// sections when a measurement comes back. All of them can still be set by name.
pub fn corner_rows(p: &Project) -> BTreeSet<String> {
    use crate::project::{ExtraPrim, Form};
    let rows = || p.params.iter().filter(|r| !r.derived && !matches!(r.name.as_str(), "f0" | "Z0"));
    if rows().count() <= MOST_SHOWN {
        return BTreeSet::new();
    }
    let mut corners: Vec<&str> = Vec::new();
    let mut sizes: Vec<&str> = Vec::new();
    for part in &p.extra {
        match &part.prim {
            ExtraPrim::Strip { path, w, .. } => {
                corners.extend(path.iter().flatten().map(String::as_str));
                sizes.push(w);
            }
            ExtraPrim::Polygon { pts, thick, .. } => {
                corners.extend(pts.iter().flatten().map(String::as_str));
                sizes.push(thick);
            }
            ExtraPrim::Plate { outline, holes, .. } => corners.extend(outline.iter().chain(holes.iter().flatten()).flatten().map(String::as_str)),
            ExtraPrim::Wire { pts, r } => {
                corners.extend(pts.iter().flatten().map(String::as_str));
                sizes.push(r);
            }
            ExtraPrim::Drawn { thick, form, holes, .. } => {
                sizes.push(thick);
                corners.extend(holes.iter().flatten().flatten().map(String::as_str));
                match form {
                    Form::Poly { pts } => corners.extend(pts.iter().flatten().map(String::as_str)),
                    Form::Rect { size, .. } => sizes.extend(size.iter().map(String::as_str)),
                    Form::Circle { r, .. } => sizes.push(r),
                }
            }
            ExtraPrim::Cylinder { r, .. } | ExtraPrim::Sphere { r, .. } | ExtraPrim::Disc { r, .. } => sizes.push(r),
            ExtraPrim::Arc { r, w, .. } => sizes.extend([r.as_str(), w.as_str()]),
            ExtraPrim::Meander { step, height, n, w, .. } => sizes.extend([step.as_str(), height.as_str(), n.as_str(), w.as_str()]),
            ExtraPrim::Box { .. } | ExtraPrim::Imported { .. } | ExtraPrim::Outline { .. } => {}
        }
    }
    rows()
        .filter(|r| {
            corners.iter().any(|c| is_bare(c, &r.name))
                && !sizes.iter().any(|s| mentions(s, &r.name))
                && !p.params.iter().any(|o| o.name != r.name && mentions(&o.expr, &r.name))
        })
        .map(|r| r.name.clone())
        .collect()
}

/// Every variable in the table that is a distance on the model, each once, the main ones first
/// so that they're the ones found if a slow model runs out of time. Helper rows are left out:
/// `lambda0` is two spacings of an array, which is true and of no use to anyone.
pub fn of_project(p: &Project) -> Vec<Dim> {
    let Ok(vars) = p.vars() else { return Vec::new() };
    let started = Instant::now();
    // A 3D model is the slow part: it's kept for the variables that can't touch it.
    let model_text = if p.model.is_empty() { String::new() } else { serde_json::to_string(&p.model).unwrap_or_default() };
    let Some(bodies) = model_parts(p, &vars) else { return Vec::new() };
    let Some(base) = parts_at(p, &vars, Some(&bodies)) else { return Vec::new() };
    let (lo, hi) = bounds_of(&base.iter().flat_map(|q| q.points.iter().copied()).collect::<Vec<_>>());
    if lo[0] > hi[0] {
        return Vec::new();
    }
    let middle = scale(add(lo, hi), 0.5);
    let corners = corner_rows(p);
    let (main, placed): (Vec<_>, Vec<_>) = p.params.iter().filter(|r| !r.derived).partition(|r| !corners.contains(&r.name));
    let mut out: Vec<Dim> = Vec::new();
    for row in main.into_iter().chain(placed) {
        if started.elapsed() > TIME_LIMIT {
            break;
        }
        let Some(&value) = vars.get(&row.name) else { continue };
        // A coordinate can be negative, and it's as far from the origin as its size says.
        let size = value.abs();
        if !(size > 0.0 && size < LONGEST) {
            continue;
        }
        let moved = [NUDGE, -NUDGE].iter().find_map(|f| {
            let q = p.with_values(&[(row.name.clone(), value * (1.0 + f))]);
            let v = q.vars().ok()?;
            // Whatever follows this row in the table moves with it, so its users count too.
            let touches_model = !p.model.is_empty() && v.iter().any(|(name, x)| vars.get(name) != Some(x) && mentions(&model_text, name));
            let nudged = parts_at(&q, &v, (!touches_model).then_some(&bodies[..]))?;
            moved_between(&base, &nudged, size * f)
        });
        let Some(moved) = moved else { continue };
        let Some(found) = find(&base, &moved, size, middle, value < 0.0) else { continue };
        let near = (size * SAME_SIZE).max(SAME_PLACE);
        if out.iter().any(|d| same_line(d, &found, near)) {
            continue;
        }
        out.push(Dim { name: row.name.clone(), value, kind: found.kind, to: found.to, line: found.line, part: base[found.part].name.clone(), main: !corners.contains(&row.name) });
    }
    // The table's first rows are what its author thought of first.
    for d in out.iter_mut().filter(|d| d.main).skip(MOST_SHOWN) {
        d.main = false;
    }
    out
}

/// Which variables change each part, by the part's name, found the same way: nudge each one, build
/// the model again and see what moved. A template's formulas are code, so nothing else can say.
/// The design frequency is left out: on a template it would be listed on everything.
pub fn drivers(p: &Project) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    let Ok(vars) = p.vars() else { return out };
    let started = Instant::now();
    let model_text = if p.model.is_empty() { String::new() } else { serde_json::to_string(&p.model).unwrap_or_default() };
    let Some(bodies) = model_parts(p, &vars) else { return out };
    let Some(base) = parts_at(p, &vars, Some(&bodies)) else { return out };
    for part in &base {
        out.entry(part.name.clone()).or_default();
    }
    for row in p.params.iter().filter(|r| !r.derived && r.name != "f0") {
        if started.elapsed() > TIME_LIMIT {
            break;
        }
        let Some(&value) = vars.get(&row.name) else { continue };
        let nudged = if value == 0.0 { 1e-4 } else { value * (1.0 + NUDGE) };
        let q = p.with_values(&[(row.name.clone(), nudged)]);
        let Ok(v) = q.vars() else { continue };
        let touches_model = !p.model.is_empty() && v.iter().any(|(name, x)| vars.get(name) != Some(x) && mentions(&model_text, name));
        let Some(moved) = parts_at(&q, &v, (!touches_model).then_some(&bodies[..])) else { continue };
        // By name and place among those of that name: a variable can change how many parts there are.
        let nth = |list: &'_ [Part], name: &str, k: usize| list.iter().filter(|x| x.name == name).nth(k).cloned();
        let mut seen: std::collections::HashMap<&str, usize> = Default::default();
        for a in &base {
            let k = seen.entry(a.name.as_str()).or_insert(0);
            let found = nth(&moved, &a.name, *k);
            *k += 1;
            let Some(b) = found else {
                if let Some(list) = out.get_mut(&a.name)
                    && !list.contains(&row.name)
                {
                    list.push(row.name.clone());
                }
                continue;
            };
            let b = &b;
            // A round part's size is in no point of it.
            let radius = |p: &Part| p.round.map(|r| r.r);
            let same_round = match (radius(a), radius(b)) {
                (Some(x), Some(y)) => (x - y).abs() <= SAME_PLACE,
                (None, None) => true,
                _ => false,
            };
            let same = a.name == b.name && same_round && a.points.len() == b.points.len() && a.points.iter().zip(&b.points).all(|(x, y)| norm(sub(*x, *y)) <= SAME_PLACE);
            // A body can be measured as more than one part under the one name.
            if !same
                && let Some(list) = out.get_mut(&a.name)
                && !list.contains(&row.name)
            {
                list.push(row.name.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ExtraPort, ExtraPrim, ExtraSolid, Form, Param};

    #[test]
    fn the_variables_that_change_a_part_are_found_by_moving_them() {
        let mut p = Project::from_template("custom", Some("1GHz")).unwrap();
        for (name, expr) in [("W", "20mm"), ("H", "5mm"), ("R", "2mm"), ("unused", "7mm")] {
            p.params.push(row(name, expr));
        }
        let xyz = |a: &str, b: &str, c: &str| [a.to_string(), b.to_string(), c.to_string()];
        p.model.add_named(1, "plate".into(), crate::modeler::Kind::Solid { shape: crate::modeler::Prim::Cuboid { p0: xyz("0", "0", "0"), size: xyz("W", "10mm", "H") }, out: Default::default() });
        p.model.add_named(2, "ball".into(), crate::modeler::Kind::Solid { shape: crate::modeler::Prim::Sphere { c: xyz("50mm", "0", "0"), r: "R".into() }, out: Default::default() });
        let d = drivers(&p);
        assert_eq!(d["plate"], ["W", "H"]);
        assert_eq!(d["ball"], ["R"]);
        assert!(!d.values().flatten().any(|v| v == "unused" || v == "f0"), "{d:?}");
    }

    fn row(name: &str, expr: &str) -> Param {
        Param { name: name.into(), expr: expr.into(), note: String::new(), range: None, derived: false }
    }

    fn blank(rows: &[(&str, &str)]) -> Project {
        let mut p = Project::from_template("custom", None).unwrap();
        p.params = rows.iter().map(|(n, e)| row(n, e)).collect();
        p
    }

    fn plate(name: &str, c: [&str; 2], size: [&str; 2]) -> ExtraSolid {
        ExtraSolid {
            name: name.into(),
            material: "PEC".into(),
            prim: ExtraPrim::Drawn { axis: 2, at: "0".into(), thick: "0".into(), form: Form::Rect { c: [c[0].into(), c[1].into()], size: [size[0].into(), size[1].into()] }, holes: Vec::new() },
        }
    }

    fn poly(name: &str, pts: &[[&str; 2]]) -> ExtraSolid {
        ExtraSolid {
            name: name.into(),
            material: "PEC".into(),
            prim: ExtraPrim::Drawn { axis: 2, at: "0".into(), thick: "0".into(), form: Form::Poly { pts: pts.iter().map(|q| [q[0].to_string(), q[1].to_string()]).collect() }, holes: Vec::new() },
        }
    }

    fn strip(name: &str, path: &[[&str; 2]], w: &str) -> ExtraSolid {
        ExtraSolid { name: name.into(), material: "PEC".into(), prim: ExtraPrim::Strip { axis: 2, at: "0".into(), path: path.iter().map(|q| [q[0].to_string(), q[1].to_string()]).collect(), w: w.into() } }
    }

    fn get<'a>(d: &'a [Dim], name: &str) -> &'a Dim {
        d.iter().find(|x| x.name == name).unwrap_or_else(|| panic!("no line for {name} in {:?}", d.iter().map(|x| x.name.as_str()).collect::<Vec<_>>()))
    }

    fn line_length(d: &Dim) -> f64 {
        norm(sub(d.line[1], d.line[0]))
    }

    #[test]
    fn a_plates_length_and_width_are_its_sides() {
        let mut p = blank(&[("L", "70mm"), ("W", "20mm")]);
        p.extra.push(plate("arm", ["0", "0"], ["L", "W"]));
        let d = of_project(&p);
        let (l, w) = (get(&d, "L"), get(&d, "W"));
        assert!((line_length(l) - 0.070).abs() < 1e-9, "{l:?}");
        assert!((line_length(w) - 0.020).abs() < 1e-9, "{w:?}");
        assert!((l.line[1][0] - l.line[0][0]).abs() > 0.06, "L runs along x: {l:?}");
        assert!((w.line[1][1] - w.line[0][1]).abs() > 0.01, "W runs along y: {w:?}");
        assert_eq!(l.text(), "L = 70 mm");
        assert_eq!(w.text(), "W = 20 mm");
    }

    #[test]
    fn the_same_size_elsewhere_doesnt_take_the_label() {
        // A fixed side of 20 mm and a variable of 20 mm that sets another part's side.
        let mut p = blank(&[("t", "20mm")]);
        p.extra.push(plate("fixed", ["0", "0"], ["20mm", "5mm"]));
        p.extra.push(plate("set", ["0", "50mm"], ["t", "5mm"]));
        let d = of_project(&p);
        assert_eq!(get(&d, "t").part, "set");
    }

    #[test]
    fn a_variable_nothing_uses_gets_no_line() {
        let mut p = blank(&[("L", "70mm"), ("unused", "70mm")]);
        p.extra.push(plate("arm", ["0", "0"], ["L", "5mm"]));
        let d = of_project(&p);
        assert!(d.iter().all(|x| x.name != "unused"), "{d:?}");
    }

    #[test]
    fn a_frequency_or_a_count_is_not_a_length() {
        let mut p = blank(&[("f0", "915MHz"), ("L", "c0/f0/2"), ("n", "3")]);
        p.extra.push(plate("arm", ["0", "0"], ["L", "5mm"]));
        let d = of_project(&p);
        assert!(d.iter().all(|x| x.name != "f0" && x.name != "n"), "{d:?}");
        assert!(d.iter().any(|x| x.name == "L"), "{d:?}");
    }

    #[test]
    fn a_helper_row_gets_no_line() {
        let mut p = blank(&[("L", "70mm")]);
        p.params.push(Param { name: "half".into(), expr: "L/2".into(), note: String::new(), range: None, derived: true });
        p.extra.push(poly("arm", &[["0", "0"], ["half", "0"], ["half", "5mm"], ["0", "5mm"]]));
        let d = of_project(&p);
        assert!(d.iter().all(|x| x.name != "half"), "{d:?}");
    }

    #[test]
    fn a_dimension_tied_to_an_offset_is_measured_from_what_it_offsets() {
        // The sketch's a1: a notch whose top is a1 above a line that's 10 mm up.
        let mut p = blank(&[("a1", "4mm")]);
        p.extra.push(poly("notched", &[["0", "0"], ["30mm", "0"], ["30mm", "10mm"], ["20mm", "10mm"], ["20mm", "a1+10mm"], ["0", "a1+10mm"]]));
        let d = of_project(&p);
        let a = get(&d, "a1");
        assert!((line_length(a) - 0.004).abs() < 1e-9, "{a:?}");
    }

    #[test]
    fn a_gap_between_two_parts_is_a_dimension() {
        let mut p = blank(&[("g", "2mm")]);
        p.extra.push(poly("left", &[["-20mm", "0"], ["-g/2", "0"], ["-g/2", "5mm"], ["-20mm", "5mm"]]));
        p.extra.push(poly("right", &[["g/2", "0"], ["20mm", "0"], ["20mm", "5mm"], ["g/2", "5mm"]]));
        let d = of_project(&p);
        let g = get(&d, "g");
        assert!((line_length(g) - 0.002).abs() < 1e-9, "{g:?}");
        assert_eq!(g.kind, DimKind::Between);
    }

    #[test]
    fn a_feeds_gap_is_measured_on_the_feed() {
        let mut p = blank(&[("gp", "1mm")]);
        p.extra.push(plate("left", ["-10mm", "0"], ["18mm", "4mm"]));
        p.extra_ports.push(ExtraPort { name: "feed".into(), a: ["-gp/2".into(), "0".into(), "0".into()], b: ["gp/2".into(), "0".into(), "0".into()], width: "0".into(), z0: "50".into() });
        let d = of_project(&p);
        let g = get(&d, "gp");
        assert_eq!(g.part, "feed");
        assert!((line_length(g) - 0.001).abs() < 1e-9, "{g:?}");
    }

    #[test]
    fn a_discs_radius_runs_from_its_middle() {
        // This one comes out with an odd number of sides, so no two points are a diameter apart.
        let mut p = blank(&[("R", "10mm")]);
        p.extra.push(ExtraSolid { name: "disc".into(), material: "PEC".into(), prim: ExtraPrim::Disc { axis: 2, at: "0".into(), c: ["5mm".into(), "0".into()], r: "R".into() } });
        let d = of_project(&p);
        let r = get(&d, "R");
        assert_eq!(r.kind, DimKind::Inside);
        assert!((line_length(r) - 0.010).abs() < 1e-8, "{r:?}");
        assert!((r.line[0][0] - 0.005).abs() < 1e-8, "starts at the middle: {r:?}");
    }

    #[test]
    fn a_wires_radius_and_length_are_both_shown() {
        let p = Project::from_template("dipole", None).unwrap();
        let d = of_project(&p);
        let vars = p.vars().unwrap();
        for x in &d {
            assert!((x.value - vars[&x.name]).abs() < 1e-12, "{x:?}");
        }
        assert!((line_length(get(&d, "L")) - vars["L"]).abs() < 1e-9, "{d:?}");
        assert_eq!(get(&d, "a").kind, DimKind::Inside, "{d:?}");
    }

    #[test]
    fn a_place_given_from_the_origin_is_measured_from_there() {
        let mut p = blank(&[("x1", "12mm")]);
        p.extra.push(poly("tab", &[["x1", "0"], ["40mm", "0"], ["40mm", "7mm"], ["x1", "7mm"]]));
        let d = of_project(&p);
        let x = get(&d, "x1");
        assert!((line_length(x) - 0.012).abs() < 1e-9, "{x:?}");
    }

    #[test]
    fn a_negative_coordinate_is_measured_by_its_size() {
        // How the Assistant wrote the sketch's corners: x1 = -17.5 mm, and the arm from there.
        let mut p = blank(&[("x1", "-17.5mm")]);
        p.extra.push(poly("arm", &[["x1", "0"], ["-5mm", "0"], ["-5mm", "3mm"], ["x1", "3mm"]]));
        let d = of_project(&p);
        let x = get(&d, "x1");
        assert!((line_length(x) - 0.0175).abs() < 1e-9, "{x:?}");
        assert_eq!(x.text(), "x1 = -17.5 mm");
    }

    #[test]
    fn a_negative_value_on_mirrored_arms_goes_on_its_own_side() {
        let mut p = blank(&[("x1", "-17.5mm")]);
        p.extra.push(poly("left", &[["x1", "0"], ["-30mm", "0"], ["-30mm", "3mm"], ["x1", "3mm"]]));
        p.extra.push(poly("right", &[["-x1", "0"], ["30mm", "0"], ["30mm", "3mm"], ["-x1", "3mm"]]));
        let d = of_project(&p);
        let x = get(&d, "x1");
        assert!((line_length(x) - 0.0175).abs() < 1e-9, "{x:?}");
        assert!(x.line[0][0] < 0.0 || x.line[1][0] < 0.0, "{x:?}");
        assert!(x.line[0][0] <= 1e-9 && x.line[1][0] <= 1e-9, "{x:?}");
    }

    #[test]
    fn a_traces_centre_line_is_what_its_variables_set() {
        // Corners and ends lie half a width off the path, so no two points on the outline are L
        // or H apart, and an edge's own length includes half a width.
        let mut p = blank(&[("L", "40mm"), ("H", "10mm"), ("w", "1mm")]);
        p.extra.push(strip("arm", &[["0", "0"], ["L", "0"], ["L", "H"]], "w"));
        let d = of_project(&p);
        assert!((line_length(get(&d, "L")) - 0.040).abs() < 1e-9, "{d:?}");
        assert!((line_length(get(&d, "H")) - 0.010).abs() < 1e-9, "{d:?}");
        assert!((line_length(get(&d, "w")) - 0.001).abs() < 1e-9, "{d:?}");
    }

    #[test]
    fn a_printed_antennas_arm_and_height_are_found_on_their_centre_lines() {
        let p = Project::from_template("ifa_pcb", None).unwrap();
        let d = of_project(&p);
        let vars = p.vars().unwrap();
        for name in ["H", "La", "x0", "S", "wt"] {
            assert!((line_length(get(&d, name)) - vars[name]).abs() < vars[name] * 1e-4, "{name}: {d:?}");
        }
    }

    #[test]
    fn mirrored_arms_are_measured_from_the_middle() {
        let p = Project::from_template("tag_bent", None).unwrap();
        let d = of_project(&p);
        let vars = p.vars().unwrap();
        assert!((line_length(get(&d, "Xa")) - vars["Xa"]).abs() < vars["Xa"] * 1e-4, "{d:?}");
    }

    #[test]
    fn a_body_modelled_in_3d_shows_its_variables_too() {
        use crate::modeler::{Kind, Output, Prim};
        let mut p = blank(&[("W", "30mm"), ("H", "12mm"), ("spare", "5mm")]);
        let xyz = |a: &str, b: &str, c: &str| [a.to_string(), b.to_string(), c.to_string()];
        p.model.add(Kind::Solid { shape: Prim::Box { min: xyz("0", "0", "0"), max: xyz("W", "40mm", "H") }, out: Output::default() });
        let d = of_project(&p);
        let (w, h) = (get(&d, "W"), get(&d, "H"));
        assert!((line_length(w) - 0.030).abs() < 1e-8, "{w:?}");
        assert!((line_length(h) - 0.012).abs() < 1e-8, "{h:?}");
        assert!(d.iter().all(|x| x.name != "spare"), "{d:?}");
    }

    #[test]
    fn what_a_cut_took_away_still_shows_its_size() {
        use crate::modeler::{Kind, Mode, Output, Prim};
        let mut p = blank(&[("R", "6mm"), ("t", "3mm")]);
        let xyz = |a: &str, b: &str, c: &str| [a.to_string(), b.to_string(), c.to_string()];
        let block = p.model.add(Kind::Solid { shape: Prim::Box { min: xyz("-15mm", "-20mm", "0"), max: xyz("15mm", "20mm", "t") }, out: Output::default() });
        p.model.add(Kind::Solid { shape: Prim::Cylinder { a: xyz("0", "0", "-1mm"), b: xyz("0", "0", "5mm"), r: "R".into() }, out: Output { mode: Mode::Cut, target: Some(block), ..Default::default() } });
        let d = of_project(&p);
        let r = get(&d, "R");
        assert_eq!(r.kind, DimKind::Inside);
        assert!((line_length(r) - 0.006).abs() < 1e-8, "{r:?}");
    }

    #[test]
    fn a_sketchs_dimensions_show_on_the_body_made_from_it() {
        use crate::modeler::spec;
        let mut p = blank(&[("W", "30mm"), ("Ls", "40mm"), ("t", "3mm"), ("Rh", "4mm")]);
        let steps = vec![
            serde_json::json!({ "kind": "sketch", "name": "plate", "plane": "top", "shapes": [ { "rect": { "c": ["0", "0"], "size": ["W", "Ls"] } }, { "circle": { "c": ["0", "0"], "r": "Rh" } } ] }),
            serde_json::json!({ "kind": "extrude", "name": "block", "sketch": "plate", "depth": "t" }),
        ];
        let vars = p.vars().unwrap();
        let mut model = std::mem::take(&mut p.model);
        spec::add_steps(&mut model, &steps, &vars).unwrap();
        p.model = model;
        let d = of_project(&p);
        for (name, size) in [("W", 0.030), ("Ls", 0.040), ("t", 0.003), ("Rh", 0.004)] {
            assert!((line_length(get(&d, name)) - size).abs() < 1e-7, "{name}: {d:?}");
        }
    }

    /// Whatever a template does, a line says what the model really measures: its length is the
    /// value, and it stays so when the variable is really changed.
    #[test]
    fn every_line_on_every_template_measures_what_it_says() {
        let mut checked = 0;
        for t in crate::templates::all() {
            let Ok(p) = Project::from_template(t.id, None) else { continue };
            for d in of_project(&p) {
                let size = d.value.abs();
                assert!((line_length(&d) - size).abs() <= (size * 1e-4).max(1e-8), "{}: {d:?}", t.id);
                // A different value, from scratch: the line has to follow it.
                let new = d.value * 1.03;
                let q = p.with_values(&[(d.name.clone(), new)]);
                if q.parts_geometry(&q.vars().unwrap()).is_err() {
                    continue;
                }
                if let Some(a) = of_project(&q).iter().find(|x| x.name == d.name) {
                    assert!((line_length(a) - new.abs()).abs() <= (new.abs() * 1e-3).max(1e-8), "{}: {a:?}", t.id);
                }
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} lines over all the templates");
    }

    /// Run with `--nocapture` to see what each template shows and what it leaves out.
    #[test]
    fn how_much_of_each_template_shows() {
        let (mut shown, mut rows) = (0, 0);
        for t in crate::templates::all() {
            let Ok(p) = Project::from_template(t.id, None) else { continue };
            let d = of_project(&p);
            let vars = p.vars().unwrap();
            let names: Vec<String> = d.iter().map(|x| x.name.clone()).collect();
            let left: Vec<String> = p.params.iter().filter(|r| !r.derived && !names.contains(&r.name)).map(|r| format!("{}={:.4e}", r.name, vars.get(&r.name).copied().unwrap_or(f64::NAN))).collect();
            println!("{:<22} shown {:?}  left {:?}", t.id, names, left);
            shown += names.len();
            rows += p.params.iter().filter(|r| !r.derived).count();
        }
        println!("shown {shown} of {rows} rows");
        assert!(shown * 10 >= rows * 6, "only {shown} of {rows} rows");
    }

    /// The shape of the drawing the user called too many: a few sizes, then a row per corner, some
    /// of them worked out from the sizes.
    fn zig_zag() -> Project {
        let mut rows: Vec<(String, String)> = [("f0", "915MHz"), ("Z0", "50"), ("L", "70mm"), ("W", "20mm"), ("g", "1mm"), ("pd", "1.5mm"), ("w", "1mm"), ("xp", "6mm")].iter().map(|(n, e)| (n.to_string(), e.to_string())).collect();
        rows.extend([("xt", "L/2-w/2"), ("xb", "-L/2+w/2"), ("ye", "W/2-w/2"), ("yg", "g/2+w/2"), ("xl3", "xp+pd"), ("xl4", "xp-pd")].map(|(n, e)| (n.to_string(), e.to_string())));
        let free = [("x1", "23.1mm"), ("x2", "17.3mm"), ("x3", "-3.3mm"), ("x4", "-15.7mm"), ("y1", "-1.9mm"), ("y2", "2.7mm"), ("y3", "-4.3mm"), ("y4", "-6.1mm"), ("y5", "1.3mm")];
        rows.extend(free.map(|(n, e)| (n.to_string(), e.to_string())));
        let table: Vec<(&str, &str)> = rows.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let mut p = blank(&table);
        p.extra.push(strip("line", &[["xt", "y1"], ["x1", "y1"], ["x1", "y2"], ["x2", "y2"], ["x2", "y3"], ["xl3", "y3"], ["xl3", "-yg"], ["xl4", "-yg"], ["xl4", "y4"], ["x3", "y4"], ["x3", "y5"], ["x4", "y5"], ["x4", "-ye"], ["xb", "-ye"]], "w"));
        p.extra.push(strip("over", &[["xl3", "yg"], ["xl4", "yg"]], "w"));
        p.extra.push(strip("end", &[["xb", "-ye"], ["xb", "ye"]], "w"));
        p.extra.push(strip("back", &[["xb", "ye"], ["xt", "ye"], ["xt", "y1"]], "w"));
        p.extra_ports.push(ExtraPort { name: "port".into(), a: ["xp".into(), "-yg".into(), "0".into()], b: ["xp".into(), "yg".into(), "0".into()], width: "w".into(), z0: "Z0".into() });
        p
    }

    #[test]
    fn a_drawing_with_every_corner_named_shows_its_sizes_and_not_its_corners() {
        let p = zig_zag();
        let corners = corner_rows(&p);
        for name in ["L", "W", "g", "pd", "w", "xp", "f0", "Z0"] {
            assert!(!corners.contains(name), "{name} is a size or a setting: {corners:?}");
        }
        for name in ["xt", "xb", "ye", "yg", "xl3", "xl4", "x1", "y5"] {
            assert!(corners.contains(name), "{name} only says where a corner is: {corners:?}");
        }
        let d = of_project(&p);
        let shown: BTreeSet<&str> = d.iter().filter(|x| x.main).map(|x| x.name.as_str()).collect();
        assert_eq!(shown, BTreeSet::from(["L", "W", "g", "pd", "w", "xp"]), "{d:?}");
        // Still found, for pointing at its row.
        let x1 = get(&d, "x1");
        assert!(!x1.main, "{x1:?}");
        assert!((line_length(x1) - 0.0231).abs() < 1e-9, "{x1:?}");
    }

    #[test]
    fn a_small_drawing_keeps_every_row_it_has() {
        // Here the corners are the design: L and H are the only sizes it has.
        let mut p = blank(&[("L", "40mm"), ("H", "10mm"), ("w", "1mm")]);
        p.extra.push(strip("arm", &[["0", "0"], ["L", "0"], ["L", "H"]], "w"));
        assert!(corner_rows(&p).is_empty());
        assert!(of_project(&p).iter().all(|d| d.main));
    }

    #[test]
    fn a_corner_that_sets_something_else_is_a_dimension() {
        let mut rows: Vec<(String, String)> = (0..12).map(|i| (format!("x{i}"), format!("{}mm", i + 1))).collect();
        rows.push(("s".into(), "2mm".into()));
        rows.push(("t".into(), "x3*2".into()));
        let table: Vec<(&str, &str)> = rows.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let mut p = blank(&table);
        let path: Vec<[String; 2]> = (0..12).map(|i| [format!("x{i}"), "0".to_string()]).chain([["s".to_string(), "t".to_string()]]).collect();
        let path: Vec<[&str; 2]> = path.iter().map(|q| [q[0].as_str(), q[1].as_str()]).collect();
        p.extra.push(strip("arm", &path, "x5"));
        let corners = corner_rows(&p);
        assert!(!corners.contains("x3"), "t is written over it: {corners:?}");
        assert!(!corners.contains("x5"), "it's the trace width too: {corners:?}");
        assert!(corners.contains("x4"), "{corners:?}");
        assert!(corners.contains("s"), "{corners:?}");
    }

    #[test]
    fn no_more_than_a_readable_number_of_labels_show() {
        // Plate sizes, not corners: every one is a real dimension, and there are still too many.
        let rows: Vec<(String, String)> = (0..16).map(|i| (format!("a{i}"), format!("{}mm", 3 + i))).collect();
        let table: Vec<(&str, &str)> = rows.iter().map(|(n, e)| (n.as_str(), e.as_str())).collect();
        let mut p = blank(&table);
        for (i, (name, _)) in rows.iter().enumerate() {
            let y = format!("{}mm", i * 40);
            p.extra.push(plate(&format!("p{i}"), ["0", &y], [name, "1.7mm"]));
        }
        let d = of_project(&p);
        assert_eq!(d.len(), 16, "{d:?}");
        let shown: Vec<&str> = d.iter().filter(|x| x.main).map(|x| x.name.as_str()).collect();
        assert_eq!(shown.len(), MOST_SHOWN, "{shown:?}");
        assert_eq!(shown[0], "a0", "the table's order: {shown:?}");
    }

    #[test]
    fn lengths_read_in_the_unit_that_suits_them() {
        assert_eq!(length_text(0.07), "70 mm");
        assert_eq!(length_text(0.0005), "0.5 mm");
        assert_eq!(length_text(0.00125), "1.25 mm");
        assert_eq!(length_text(1.5), "1.5 m");
        assert_eq!(length_text(0.000005), "5 µm");
    }

    #[test]
    fn names_are_found_whole() {
        assert!(mentions("{\"a\":\"a1+2mm\"}", "a1"));
        assert!(!mentions("{\"a\":\"a10+2mm\"}", "a1"));
        assert!(!mentions("{\"a\":\"xa1\"}", "a1"));
    }
}
