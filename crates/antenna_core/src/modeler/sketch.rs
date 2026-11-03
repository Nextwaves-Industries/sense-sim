//! A sketch: points and curves on a plane, held by constraints and driven by dimensions.
//! Dimensions are expressions, so a variable moves everything that depends on it, and the
//! solver finds where the points go from where they last were.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{angle, length};
use crate::expr::{self, Vars};

pub type P2 = [f64; 2];

/// The origin: a point every sketch has, fixed at (0, 0).
pub const ORIGIN: u32 = 0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkPoint {
    pub id: u32,
    /// Where it last solved to, metres.
    pub at: P2,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CurveKind {
    Line { a: u32, b: u32 },
    Circle { c: u32, r: f64 },
    /// Counter-clockwise from `a` to `b` about `c`.
    Arc { c: u32, a: u32, b: u32 },
    /// A smooth curve through its points.
    Spline { pts: Vec<u32> },
    /// `u(t)`, `v(t)` as expressions: an exponential taper, a parabola. Its two ends are points
    /// other curves can join, and they go where the formula puts them.
    Equation { a: u32, b: u32, u: String, v: String, t0: String, t1: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    pub id: u32,
    pub kind: CurveKind,
    /// A reference line: helps define the sketch and makes no geometry.
    #[serde(default)]
    pub construction: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    Coincident { a: u32, b: u32 },
    Horizontal { line: u32 },
    Vertical { line: u32 },
    Parallel { a: u32, b: u32 },
    Perpendicular { a: u32, b: u32 },
    EqualLength { a: u32, b: u32 },
    EqualRadius { a: u32, b: u32 },
    Tangent { line: u32, arc: u32 },
    OnLine { p: u32, line: u32 },
    OnCircle { p: u32, curve: u32 },
    Midpoint { p: u32, line: u32 },
    /// Mirror images across a line in the sketch.
    Symmetric { a: u32, b: u32, line: u32 },
    /// Mirror images across a sketch axis: 0 is the line u = 0's mirror (v kept), 1 the line v = 0's.
    SymmetricAxis { a: u32, b: u32, axis: u32 },
    Fixed { p: u32 },
    /// From `a` to `b`, however it's turned.
    Distance { a: u32, b: u32, value: String },
    /// `b` minus `a` along u. Negative means `b` lies on the other side.
    DistanceU { a: u32, b: u32, value: String },
    DistanceV { a: u32, b: u32, value: String },
    /// The perpendicular distance from a point to a line.
    PointLine { p: u32, line: u32, value: String },
    Radius { curve: u32, value: String },
    Diameter { curve: u32, value: String },
    /// Between two lines, from the first to the second counter-clockwise. A plain number is degrees.
    Angle { a: u32, b: u32, value: String },
}

impl Constraint {
    /// The expression a dimension is driven by.
    pub fn value_mut(&mut self) -> Option<&mut String> {
        match self {
            Constraint::Distance { value, .. } | Constraint::DistanceU { value, .. } | Constraint::DistanceV { value, .. } | Constraint::PointLine { value, .. } | Constraint::Radius { value, .. } | Constraint::Diameter { value, .. } | Constraint::Angle { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn value(&self) -> Option<&str> {
        match self {
            Constraint::Distance { value, .. } | Constraint::DistanceU { value, .. } | Constraint::DistanceV { value, .. } | Constraint::PointLine { value, .. } | Constraint::Radius { value, .. } | Constraint::Diameter { value, .. } | Constraint::Angle { value, .. } => Some(value),
            _ => None,
        }
    }

    /// What it says, for a list.
    pub fn label(&self) -> &'static str {
        match self {
            Constraint::Coincident { .. } => "Coincident",
            Constraint::Horizontal { .. } => "Horizontal",
            Constraint::Vertical { .. } => "Vertical",
            Constraint::Parallel { .. } => "Parallel",
            Constraint::Perpendicular { .. } => "Perpendicular",
            Constraint::EqualLength { .. } => "Equal length",
            Constraint::EqualRadius { .. } => "Equal radius",
            Constraint::Tangent { .. } => "Tangent",
            Constraint::OnLine { .. } => "Point on line",
            Constraint::OnCircle { .. } => "Point on circle",
            Constraint::Midpoint { .. } => "Midpoint",
            Constraint::Symmetric { .. } => "Symmetric",
            Constraint::SymmetricAxis { .. } => "Symmetric about an axis",
            Constraint::Fixed { .. } => "Fixed",
            Constraint::Distance { .. } => "Distance",
            Constraint::DistanceU { .. } => "Horizontal distance",
            Constraint::DistanceV { .. } => "Vertical distance",
            Constraint::PointLine { .. } => "Distance to line",
            Constraint::Radius { .. } => "Radius",
            Constraint::Diameter { .. } => "Diameter",
            Constraint::Angle { .. } => "Angle",
        }
    }
}

/// The geometry of a sketch, without where it lies: that's the feature's plane.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sketch {
    pub points: Vec<SkPoint>,
    pub curves: Vec<Curve>,
    pub constraints: Vec<Constraint>,
    /// The next id for a point or curve. Ids are never reused: a face takes its name from its
    /// curve's, so a name stays with the same curve.
    #[serde(default)]
    pub next_id: u32,
}

/// How far along a solve got.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Solved {
    pub points: HashMap<u32, P2>,
    pub radii: HashMap<u32, f64>,
    /// How many ways the sketch can still move. 0 is fully defined.
    pub free: usize,
    /// Points the constraints don't pin down yet.
    pub loose_points: Vec<u32>,
    /// Constraints that can't all hold, by index. Empty when it solved.
    pub conflicts: Vec<usize>,
    /// Dimensions whose expression doesn't work out, with why.
    pub broken: Vec<(usize, String)>,
}

impl Solved {
    pub fn ok(&self) -> bool {
        self.conflicts.is_empty() && self.broken.is_empty()
    }
    pub fn at(&self, id: u32) -> P2 {
        self.points.get(&id).copied().unwrap_or([0.0, 0.0])
    }
}

/// Internally the solver works in millimetres, which keeps lengths and angles comparable.
const SCALE: f64 = 1e3;
/// Residual that counts as met, millimetres: a picometre.
const MET: f64 = 1e-9;

/// A constraint with its dimension worked out, in solver units.
#[derive(Clone, Copy, Debug)]
enum Row {
    Coincident(usize, usize),
    Horizontal(usize, usize),
    Vertical(usize, usize),
    Parallel([usize; 4]),
    Perpendicular([usize; 4]),
    EqualLength([usize; 4]),
    EqualRadius(usize, usize),
    /// A radius held equal to the distance between two points.
    RadiusToPoint { r: usize, c: usize, p: usize },
    /// Two arms of an arc held the same length.
    ArcRadius { c: usize, a: usize, b: usize },
    Tangent { a: usize, b: usize, c: usize, r: Reach },
    OnLine { p: usize, a: usize, b: usize },
    OnCircle { p: usize, c: usize, r: Reach },
    Midpoint { p: usize, a: usize, b: usize },
    Symmetric { p: usize, q: usize, a: usize, b: usize },
    SymmetricAxis { p: usize, q: usize, axis: usize },
    Fixed { p: usize, at: P2 },
    Distance { a: usize, b: usize, d: f64 },
    DistanceU { a: usize, b: usize, d: f64 },
    DistanceV { a: usize, b: usize, d: f64 },
    PointLine { p: usize, a: usize, b: usize, d: f64 },
    RadiusIs { r: Reach, d: f64 },
    Angle { a: [usize; 4], rad: f64 },
}

/// A circle's radius is an unknown of its own; an arc's is how far its start is from its centre.
#[derive(Clone, Copy, Debug)]
enum Reach {
    Own(usize),
    Arc { c: usize, a: usize },
}

impl Reach {
    fn get(&self, x: &[f64]) -> f64 {
        match *self {
            Reach::Own(r) => x[r],
            Reach::Arc { c, a } => (x[a] - x[c]).hypot(x[a + 1] - x[c + 1]),
        }
    }
    fn uses(&self, out: &mut Vec<usize>) {
        match *self {
            Reach::Own(r) => out.push(r),
            Reach::Arc { c, a } => out.extend([c, c + 1, a, a + 1]),
        }
    }
}

fn dir(x: &[f64], a: usize, b: usize) -> P2 {
    [x[b] - x[a], x[b + 1] - x[a + 1]]
}

fn unit2(d: P2) -> P2 {
    let n = d[0].hypot(d[1]).max(1e-30);
    [d[0] / n, d[1] / n]
}

impl Row {
    /// How many equations it stands for.
    fn count(&self) -> usize {
        match self {
            Row::Coincident(..) | Row::Midpoint { .. } | Row::Fixed { .. } | Row::Symmetric { .. } | Row::SymmetricAxis { .. } => 2,
            _ => 1,
        }
    }

    /// The unknowns it depends on.
    fn uses(&self, out: &mut Vec<usize>) {
        let p = |out: &mut Vec<usize>, a: usize| out.extend([a, a + 1]);
        match self {
            Row::Coincident(a, b) | Row::Horizontal(a, b) | Row::Vertical(a, b) => {
                p(out, *a);
                p(out, *b);
            }
            Row::Parallel(q) | Row::Perpendicular(q) | Row::EqualLength(q) => q.iter().for_each(|a| p(out, *a)),
            Row::EqualRadius(a, b) => out.extend([*a, *b]),
            Row::RadiusToPoint { r, c, p: q } => {
                out.push(*r);
                p(out, *c);
                p(out, *q);
            }
            Row::ArcRadius { c, a, b } => [c, a, b].iter().for_each(|i| p(out, **i)),
            Row::Tangent { a, b, c, r } => {
                [a, b, c].iter().for_each(|i| p(out, **i));
                r.uses(out);
            }
            Row::OnLine { p: q, a, b } | Row::Midpoint { p: q, a, b } | Row::PointLine { p: q, a, b, .. } => [q, a, b].iter().for_each(|i| p(out, **i)),
            Row::OnCircle { p: q, c, r } => {
                [q, c].iter().for_each(|i| p(out, **i));
                r.uses(out);
            }
            Row::Symmetric { p: q, q: w, a, b } => [q, w, a, b].iter().for_each(|i| p(out, **i)),
            Row::SymmetricAxis { p: q, q: w, .. } => [q, w].iter().for_each(|i| p(out, **i)),
            Row::Fixed { p: q, .. } => p(out, *q),
            Row::Distance { a, b, .. } | Row::DistanceU { a, b, .. } | Row::DistanceV { a, b, .. } => [a, b].iter().for_each(|i| p(out, **i)),
            Row::RadiusIs { r, .. } => r.uses(out),
            Row::Angle { a, .. } => a.iter().for_each(|i| p(out, *i)),
        }
    }

    /// Its residuals at `x`, zero where it holds.
    fn eval(&self, x: &[f64], out: &mut Vec<f64>) {
        match *self {
            Row::Coincident(a, b) => out.extend([x[a] - x[b], x[a + 1] - x[b + 1]]),
            Row::Horizontal(a, b) => out.push(x[a + 1] - x[b + 1]),
            Row::Vertical(a, b) => out.push(x[a] - x[b]),
            Row::Parallel([a, b, c, d]) => {
                let (u, v) = (unit2(dir(x, a, b)), unit2(dir(x, c, d)));
                out.push(u[0] * v[1] - u[1] * v[0]);
            }
            Row::Perpendicular([a, b, c, d]) => {
                let (u, v) = (unit2(dir(x, a, b)), unit2(dir(x, c, d)));
                out.push(u[0] * v[0] + u[1] * v[1]);
            }
            Row::EqualLength([a, b, c, d]) => {
                let (u, v) = (dir(x, a, b), dir(x, c, d));
                out.push(u[0].hypot(u[1]) - v[0].hypot(v[1]));
            }
            Row::EqualRadius(a, b) => out.push(x[a] - x[b]),
            Row::RadiusToPoint { r, c, p } => {
                let d = dir(x, c, p);
                out.push(d[0].hypot(d[1]) - x[r]);
            }
            Row::ArcRadius { c, a, b } => {
                let (u, v) = (dir(x, c, a), dir(x, c, b));
                out.push(u[0].hypot(u[1]) - v[0].hypot(v[1]));
            }
            Row::Tangent { a, b, c, r } => {
                // The centre's distance from the line is the radius. Which side stays as drawn: the
                // solver starts on it and has no reason to cross.
                let u = unit2(dir(x, a, b));
                let w = dir(x, a, c);
                out.push((u[0] * w[1] - u[1] * w[0]).abs() - r.get(x));
            }
            Row::OnLine { p, a, b } => {
                let u = unit2(dir(x, a, b));
                let w = dir(x, a, p);
                out.push(u[0] * w[1] - u[1] * w[0]);
            }
            Row::OnCircle { p, c, r } => {
                let d = dir(x, c, p);
                out.push(d[0].hypot(d[1]) - r.get(x));
            }
            Row::Midpoint { p, a, b } => out.extend([x[p] - (x[a] + x[b]) / 2.0, x[p + 1] - (x[a + 1] + x[b + 1]) / 2.0]),
            Row::Symmetric { p, q, a, b } => {
                let u = unit2(dir(x, a, b));
                let (mp, pq) = ([(x[p] + x[q]) / 2.0 - x[a], (x[p + 1] + x[q + 1]) / 2.0 - x[a + 1]], dir(x, p, q));
                // The middle is on the line, and the join is square to it.
                out.extend([u[0] * mp[1] - u[1] * mp[0], u[0] * pq[0] + u[1] * pq[1]]);
            }
            Row::SymmetricAxis { p, q, axis } => {
                if axis == 0 {
                    out.extend([x[p] + x[q], x[p + 1] - x[q + 1]]);
                } else {
                    out.extend([x[p] - x[q], x[p + 1] + x[q + 1]]);
                }
            }
            Row::Fixed { p, at } => out.extend([x[p] - at[0], x[p + 1] - at[1]]),
            Row::Distance { a, b, d } => {
                let v = dir(x, a, b);
                out.push(v[0].hypot(v[1]) - d);
            }
            Row::DistanceU { a, b, d } => out.push(x[b] - x[a] - d),
            Row::DistanceV { a, b, d } => out.push(x[b + 1] - x[a + 1] - d),
            Row::PointLine { p, a, b, d } => {
                let u = unit2(dir(x, a, b));
                let w = dir(x, a, p);
                out.push((u[0] * w[1] - u[1] * w[0]).abs() - d);
            }
            Row::RadiusIs { r, d } => out.push(r.get(x) - d),
            Row::Angle { a: [a, b, c, d], rad } => {
                let (u, v) = (unit2(dir(x, a, b)), unit2(dir(x, c, d)));
                let (s, co) = rad.sin_cos();
                // How far the first line, turned by the angle, is from the second.
                let turned = [u[0] * co - u[1] * s, u[0] * s + u[1] * co];
                out.push(turned[0] * v[1] - turned[1] * v[0]);
            }
        }
    }
}

/// A set of equations and what they're about, with the unknowns numbered.
struct Problem {
    x: Vec<f64>,
    rows: Vec<(usize, Row)>,
    slot: HashMap<u32, usize>,
    radius: HashMap<u32, usize>,
    fixed_ids: Vec<u32>,
}

impl Sketch {
    pub fn point(&self, id: u32) -> Option<P2> {
        if id == ORIGIN {
            return Some([0.0, 0.0]);
        }
        self.points.iter().find(|p| p.id == id).map(|p| p.at)
    }

    pub fn curve(&self, id: u32) -> Option<&Curve> {
        self.curves.iter().find(|c| c.id == id)
    }

    fn fresh(&mut self) -> u32 {
        // Stay clear of the origin, and of anything a hand-written sketch used.
        let top = self.points.iter().map(|p| p.id).chain(self.curves.iter().map(|c| c.id)).max().unwrap_or(0);
        self.next_id = self.next_id.max(top).max(ORIGIN) + 1;
        self.next_id
    }

    pub fn add_point(&mut self, at: P2) -> u32 {
        let id = self.fresh();
        self.points.push(SkPoint { id, at });
        id
    }

    pub fn add_curve(&mut self, kind: CurveKind) -> u32 {
        let id = self.fresh();
        self.curves.push(Curve { id, kind, construction: false });
        id
    }

    pub fn add_line(&mut self, a: u32, b: u32) -> u32 {
        self.add_curve(CurveKind::Line { a, b })
    }

    /// A rectangle's four lines, kept square, with the points in order round it.
    pub fn add_rect(&mut self, lo: P2, hi: P2) -> ([u32; 4], [u32; 4]) {
        let p = [self.add_point(lo), self.add_point([hi[0], lo[1]]), self.add_point(hi), self.add_point([lo[0], hi[1]])];
        let l = [self.add_line(p[0], p[1]), self.add_line(p[1], p[2]), self.add_line(p[2], p[3]), self.add_line(p[3], p[0])];
        self.constraints.push(Constraint::Horizontal { line: l[0] });
        self.constraints.push(Constraint::Vertical { line: l[1] });
        self.constraints.push(Constraint::Horizontal { line: l[2] });
        self.constraints.push(Constraint::Vertical { line: l[3] });
        (p, l)
    }

    pub fn add_circle(&mut self, c: P2, r: f64) -> (u32, u32) {
        let centre = self.add_point(c);
        let curve = self.add_curve(CurveKind::Circle { c: centre, r });
        (centre, curve)
    }

    /// An arc through `a` and `b`, counter-clockwise about `c`. It keeps its radius from its start.
    pub fn add_arc(&mut self, c: P2, a: P2, b: P2) -> u32 {
        let (cp, ap, bp) = (self.add_point(c), self.add_point(a), self.add_point(b));
        self.add_curve(CurveKind::Arc { c: cp, a: ap, b: bp })
    }

    /// Drops a point or curve and every constraint on it.
    pub fn remove_curve(&mut self, id: u32) {
        let ends = self.curve(id).map_or(Vec::new(), |c| curve_points(&c.kind));
        self.curves.retain(|c| c.id != id);
        self.constraints.retain(|c| !constraint_touches_curve(c, id));
        self.drop_unused_points(&ends);
    }

    pub fn remove_point(&mut self, id: u32) {
        if id == ORIGIN {
            return;
        }
        let gone: Vec<u32> = self.curves.iter().filter(|c| curve_points(&c.kind).contains(&id)).map(|c| c.id).collect();
        for c in gone {
            self.remove_curve(c);
        }
        self.points.retain(|p| p.id != id);
        self.constraints.retain(|c| !constraint_points(c).contains(&id));
    }

    /// Of the points a removed curve had, those nothing else uses are leftovers. A lone point placed
    /// on purpose isn't one: a feed may be on it.
    fn drop_unused_points(&mut self, ends: &[u32]) {
        let used: Vec<u32> = self.curves.iter().flat_map(|c| curve_points(&c.kind)).collect();
        let held: Vec<u32> = self.constraints.iter().flat_map(constraint_points).collect();
        self.points.retain(|p| !ends.contains(&p.id) || used.contains(&p.id) || held.contains(&p.id));
    }

    fn problem(&self, vars: &Vars, broken: &mut Vec<(usize, String)>) -> Problem {
        let mut x = Vec::with_capacity(self.points.len() * 2 + 8);
        let mut slot = HashMap::new();
        // The origin is an unknown like the rest, with a row that holds it still.
        slot.insert(ORIGIN, 0);
        x.extend([0.0, 0.0]);
        for p in &self.points {
            if p.id != ORIGIN {
                slot.insert(p.id, x.len());
                x.extend([p.at[0] * SCALE, p.at[1] * SCALE]);
            }
        }
        let mut radius = HashMap::new();
        for c in &self.curves {
            if let CurveKind::Circle { r, .. } = c.kind {
                radius.insert(c.id, x.len());
                x.push(r * SCALE);
            }
        }
        let mut rows: Vec<(usize, Row)> = vec![(usize::MAX, Row::Fixed { p: 0, at: [0.0, 0.0] })];
        let mut fixed_ids = vec![ORIGIN];
        // What the curves themselves ask: an arc's two ends are one radius from its centre, and
        // an equation's ends go where its formula says.
        for c in &self.curves {
            match &c.kind {
                CurveKind::Arc { c: centre, a, b } => {
                    if let (Some(cc), Some(aa), Some(bb)) = (slot.get(centre), slot.get(a), slot.get(b)) {
                        rows.push((usize::MAX, Row::ArcRadius { c: *cc, a: *aa, b: *bb }));
                    }
                }
                CurveKind::Equation { a, b, u, v, t0, t1 } => {
                    for (id, t) in [(a, t0), (b, t1)] {
                        match equation_point(u, v, t, vars) {
                            Ok(at) => {
                                if let Some(s) = slot.get(id) {
                                    rows.push((usize::MAX, Row::Fixed { p: *s, at: [at[0] * SCALE, at[1] * SCALE] }));
                                    fixed_ids.push(*id);
                                }
                            }
                            Err(e) => broken.push((usize::MAX, format!("{e:#}"))),
                        }
                    }
                }
                _ => {}
            }
        }
        for (i, c) in self.constraints.iter().enumerate() {
            match self.row(c, &slot, &radius, vars) {
                Ok(Some(row)) => {
                    if let Constraint::Fixed { p } = c {
                        fixed_ids.push(*p);
                    }
                    rows.push((i, row));
                }
                Ok(None) => {}
                Err(e) => broken.push((i, format!("{e:#}"))),
            }
        }
        Problem { x, rows, slot, radius, fixed_ids }
    }

    fn line_ends(&self, id: u32, slot: &HashMap<u32, usize>) -> Option<(usize, usize)> {
        match self.curve(id)?.kind {
            CurveKind::Line { a, b } => Some((*slot.get(&a)?, *slot.get(&b)?)),
            _ => None,
        }
    }

    fn reach(&self, id: u32, slot: &HashMap<u32, usize>, radius: &HashMap<u32, usize>) -> Option<(usize, Reach)> {
        match self.curve(id)?.kind {
            CurveKind::Circle { c, .. } => Some((*slot.get(&c)?, Reach::Own(*radius.get(&id)?))),
            CurveKind::Arc { c, a, .. } => {
                let (cc, aa) = (*slot.get(&c)?, *slot.get(&a)?);
                Some((cc, Reach::Arc { c: cc, a: aa }))
            }
            _ => None,
        }
    }

    /// A constraint in solver terms. `None`: something it names is gone, so it's not asking anything.
    fn row(&self, c: &Constraint, slot: &HashMap<u32, usize>, radius: &HashMap<u32, usize>, vars: &Vars) -> Result<Option<Row>> {
        let pt = |id: &u32| slot.get(id).copied();
        let line = |id: &u32| self.line_ends(*id, slot);
        let reach = |id: &u32| self.reach(*id, slot, radius);
        let mm = |s: &String| -> Result<f64> { Ok(length(s, vars)? * SCALE) };
        Ok(match c {
            Constraint::Coincident { a, b } => pt(a).zip(pt(b)).map(|(a, b)| Row::Coincident(a, b)),
            Constraint::Horizontal { line: l } => line(l).map(|(a, b)| Row::Horizontal(a, b)),
            Constraint::Vertical { line: l } => line(l).map(|(a, b)| Row::Vertical(a, b)),
            Constraint::Parallel { a, b } => line(a).zip(line(b)).map(|(p, q)| Row::Parallel([p.0, p.1, q.0, q.1])),
            Constraint::Perpendicular { a, b } => line(a).zip(line(b)).map(|(p, q)| Row::Perpendicular([p.0, p.1, q.0, q.1])),
            Constraint::EqualLength { a, b } => line(a).zip(line(b)).map(|(p, q)| Row::EqualLength([p.0, p.1, q.0, q.1])),
            Constraint::EqualRadius { a, b } => match (reach(a), reach(b)) {
                (Some((_, Reach::Own(x))), Some((_, Reach::Own(y)))) => Some(Row::EqualRadius(x, y)),
                (Some((_, Reach::Own(r))), Some((_, Reach::Arc { c, a }))) | (Some((_, Reach::Arc { c, a })), Some((_, Reach::Own(r)))) => Some(Row::RadiusToPoint { r, c, p: a }),
                (Some((_, Reach::Arc { c: c1, a: a1 })), Some((_, Reach::Arc { c: c2, a: a2 }))) => Some(Row::EqualLength([c1, a1, c2, a2])),
                _ => None,
            },
            Constraint::Tangent { line: l, arc } => line(l).zip(reach(arc)).map(|((a, b), (c, r))| Row::Tangent { a, b, c, r }),
            Constraint::OnLine { p, line: l } => pt(p).zip(line(l)).map(|(p, (a, b))| Row::OnLine { p, a, b }),
            Constraint::OnCircle { p, curve } => pt(p).zip(reach(curve)).map(|(p, (c, r))| Row::OnCircle { p, c, r }),
            Constraint::Midpoint { p, line: l } => pt(p).zip(line(l)).map(|(p, (a, b))| Row::Midpoint { p, a, b }),
            Constraint::Symmetric { a, b, line: l } => pt(a).zip(pt(b)).zip(line(l)).map(|((p, q), (a, b))| Row::Symmetric { p, q, a, b }),
            Constraint::SymmetricAxis { a, b, axis } => pt(a).zip(pt(b)).map(|(p, q)| Row::SymmetricAxis { p, q, axis: *axis as usize }),
            Constraint::Fixed { p } => pt(p).zip(self.point(*p)).map(|(p, at)| Row::Fixed { p, at: [at[0] * SCALE, at[1] * SCALE] }),
            Constraint::Distance { a, b, value } => {
                let d = mm(value)?;
                pt(a).zip(pt(b)).map(|(a, b)| Row::Distance { a, b, d })
            }
            Constraint::DistanceU { a, b, value } => {
                let d = mm(value)?;
                pt(a).zip(pt(b)).map(|(a, b)| Row::DistanceU { a, b, d })
            }
            Constraint::DistanceV { a, b, value } => {
                let d = mm(value)?;
                pt(a).zip(pt(b)).map(|(a, b)| Row::DistanceV { a, b, d })
            }
            Constraint::PointLine { p, line: l, value } => {
                let d = mm(value)?;
                pt(p).zip(line(l)).map(|(p, (a, b))| Row::PointLine { p, a, b, d })
            }
            Constraint::Radius { curve, value } => {
                let d = mm(value)?;
                reach(curve).map(|(_, r)| Row::RadiusIs { r, d })
            }
            Constraint::Diameter { curve, value } => {
                let d = mm(value)? / 2.0;
                reach(curve).map(|(_, r)| Row::RadiusIs { r, d })
            }
            Constraint::Angle { a, b, value } => {
                let rad = angle(value, vars)?;
                line(a).zip(line(b)).map(|(p, q)| Row::Angle { a: [p.0, p.1, q.0, q.1], rad })
            }
        })
    }

    /// Solves from where everything last was.
    pub fn solve(&self, vars: &Vars) -> Solved {
        self.solve_with(vars, &[])
    }

    /// Solves with some points held where they're dragged to. If those can't be met, what the
    /// constraints say wins and the point stays as close as they let it.
    pub fn solve_with(&self, vars: &Vars, held: &[(u32, P2)]) -> Solved {
        let mut broken = Vec::new();
        let mut pb = self.problem(vars, &mut broken);
        let mut x = pb.x.clone();
        let mut held_rows = Vec::new();
        for (id, at) in held {
            if let Some(&s) = pb.slot.get(id)
                && !pb.fixed_ids.contains(id)
            {
                x[s] = at[0] * SCALE;
                x[s + 1] = at[1] * SCALE;
                held_rows.push((usize::MAX, Row::Fixed { p: s, at: [at[0] * SCALE, at[1] * SCALE] }));
            }
        }
        if !held_rows.is_empty() {
            let mut with = pb.rows.clone();
            with.extend(held_rows);
            let mut trial = x.clone();
            if !solve_rows(&mut trial, &with).iter().any(|c| *c != usize::MAX) && residual(&trial, &with) <= MET * 1e3 {
                x = trial;
            }
        }
        let conflicts: Vec<usize> = solve_rows(&mut x, &pb.rows);
        let mut conflicts: Vec<usize> = conflicts.into_iter().filter(|c| *c != usize::MAX).collect();
        // A line that shrinks to nothing has no direction, so the row says nothing: that's a conflict
        // the residual won't show unless the sketch was going to be used for something.
        conflicts.sort_unstable();
        conflicts.dedup();
        let (free, loose) = freedom(&x, &pb.rows);
        let mut out = Solved { free, conflicts, broken, ..Default::default() };
        for (id, s) in &pb.slot {
            out.points.insert(*id, [x[*s] / SCALE, x[*s + 1] / SCALE]);
            if loose.contains(s) || loose.contains(&(*s + 1)) {
                out.loose_points.push(*id);
            }
        }
        out.loose_points.sort_unstable();
        for (id, s) in pb.radius.drain() {
            out.radii.insert(id, x[s] / SCALE);
        }
        out
    }

    /// Takes the solution as the starting point for next time, so the sketch stays near it.
    pub fn keep(&mut self, s: &Solved) {
        for p in &mut self.points {
            if let Some(at) = s.points.get(&p.id) {
                p.at = *at;
            }
        }
        for c in &mut self.curves {
            if let CurveKind::Circle { r, .. } = &mut c.kind
                && let Some(v) = s.radii.get(&c.id)
            {
                *r = *v;
            }
        }
    }

    /// Adds position dimensions until nothing is free, so a sketch can be fully defined at a click.
    pub fn fully_define(&mut self, vars: &Vars) {
        let s = self.solve(vars);
        self.keep(&s);
        let radii: Vec<(u32, f64)> = self.curves.iter().filter_map(|c| s.radii.get(&c.id).map(|r| (c.id, *r))).collect();
        for (id, r) in radii {
            let mut trial = self.clone();
            trial.constraints.push(Constraint::Radius { curve: id, value: mm_text(r) });
            if trial.solve(vars).free < self.solve(vars).free {
                *self = trial;
            }
        }
        for id in self.points.iter().map(|p| p.id).collect::<Vec<_>>() {
            let at = self.point(id).unwrap_or([0.0, 0.0]);
            for u in [true, false] {
                let before = self.solve(vars).free;
                if before == 0 {
                    return;
                }
                let value = mm_text(if u { at[0] } else { at[1] });
                let mut trial = self.clone();
                trial.constraints.push(if u { Constraint::DistanceU { a: ORIGIN, b: id, value } } else { Constraint::DistanceV { a: ORIGIN, b: id, value } });
                let after = trial.solve(vars);
                if after.free < before && after.ok() {
                    *self = trial;
                }
            }
        }
    }

    /// Rounds the corner where two lines meet: an arc of `r` tangent to both, with its radius as a
    /// dimension. Refused where the lines don't meet at this point, or are too short for it.
    pub fn fillet_corner(&mut self, corner: u32, r: &str, vars: &Vars) -> Result<u32> {
        let solved = self.solve(vars);
        let radius = length(r, vars)?;
        if !(radius > 0.0) {
            bail!("A fillet needs a radius above zero.");
        }
        let lines: Vec<(u32, u32)> = self
            .curves
            .iter()
            .filter(|c| !c.construction)
            .filter_map(|c| match c.kind {
                CurveKind::Line { a, b } if a == corner => Some((c.id, b)),
                CurveKind::Line { a, b } if b == corner => Some((c.id, a)),
                _ => None,
            })
            .collect();
        let [(l1, far1), (l2, far2)] = lines[..] else {
            bail!("A corner to round has exactly two lines meeting at it.");
        };
        let p = solved.at(corner);
        let (a, b) = (solved.at(far1), solved.at(far2));
        let (d1, d2) = (unit2([a[0] - p[0], a[1] - p[1]]), unit2([b[0] - p[0], b[1] - p[1]]));
        let cos = (d1[0] * d2[0] + d1[1] * d2[1]).clamp(-1.0, 1.0);
        let half = cos.acos() / 2.0;
        if half.sin() < 1e-3 || half.cos() < 1e-3 {
            bail!("These two lines are in line with each other, so there's no corner to round.");
        }
        let reach = radius / half.tan();
        let room = (a[0] - p[0]).hypot(a[1] - p[1]).min((b[0] - p[0]).hypot(b[1] - p[1]));
        if reach >= room {
            bail!("That radius needs {:.3} mm of each line, and the shorter has {:.3} mm.", reach * 1e3, room * 1e3);
        }
        let t1 = [p[0] + d1[0] * reach, p[1] + d1[1] * reach];
        let t2 = [p[0] + d2[0] * reach, p[1] + d2[1] * reach];
        let bis = unit2([d1[0] + d2[0], d1[1] + d2[1]]);
        let c = [p[0] + bis[0] * radius / half.sin(), p[1] + bis[1] * radius / half.sin()];
        // Counter-clockwise from one tangent point to the other, the short way round.
        let turn = (t1[0] - c[0]) * (t2[1] - c[1]) - (t1[1] - c[1]) * (t2[0] - c[0]);
        let (from, to) = if turn > 0.0 { (t1, t2) } else { (t2, t1) };
        let (cp, ap, bp) = (self.add_point(c), self.add_point(from), self.add_point(to));
        let arc = self.add_curve(CurveKind::Arc { c: cp, a: ap, b: bp });
        // Each line now ends where the arc starts.
        let (at1, at2) = if turn > 0.0 { (ap, bp) } else { (bp, ap) };
        for (line, end) in [(l1, at1), (l2, at2)] {
            if let Some(cv) = self.curves.iter_mut().find(|c| c.id == line)
                && let CurveKind::Line { a, b } = &mut cv.kind
            {
                if *a == corner {
                    *a = end;
                } else {
                    *b = end;
                }
            }
            self.constraints.push(Constraint::Tangent { line, arc });
            self.constraints.push(Constraint::OnCircle { p: end, curve: arc });
        }
        self.constraints.push(Constraint::Radius { curve: arc, value: r.to_string() });
        // The sharp corner stays as a point where the two lines would meet, so any dimension to it
        // still means what it did, the way a CAD program keeps a virtual sharp.
        self.constraints.push(Constraint::OnLine { p: corner, line: l1 });
        self.constraints.push(Constraint::OnLine { p: corner, line: l2 });
        Ok(arc)
    }
}

/// A length as an expression, good to a picometre: `12.5mm`.
pub fn mm_text(v: f64) -> String {
    let rounded = (v * 1e12).round() / 1e9;
    if rounded == 0.0 { "0mm".to_string() } else { format!("{rounded}mm") }
}

fn equation_point(u: &str, v: &str, t: &str, vars: &Vars) -> Result<P2> {
    let tv = expr::eval(t, vars)?;
    let mut with = vars.clone();
    with.insert("t".into(), tv);
    Ok([expr::eval(u, &with)?, expr::eval(v, &with)?])
}

/// The points an equation curve passes through, in order.
pub fn equation_samples(u: &str, v: &str, t0: &str, t1: &str, n: usize, vars: &Vars) -> Result<Vec<P2>> {
    let (a, b) = (expr::eval(t0, vars)?, expr::eval(t1, vars)?);
    let mut with = vars.clone();
    let mut out = Vec::with_capacity(n + 1);
    for k in 0..=n {
        with.insert("t".into(), a + (b - a) * k as f64 / n as f64);
        out.push([expr::eval(u, &with)?, expr::eval(v, &with)?]);
    }
    Ok(out)
}

pub fn curve_points(k: &CurveKind) -> Vec<u32> {
    match k {
        CurveKind::Line { a, b } | CurveKind::Equation { a, b, .. } => vec![*a, *b],
        CurveKind::Circle { c, .. } => vec![*c],
        CurveKind::Arc { c, a, b } => vec![*c, *a, *b],
        CurveKind::Spline { pts } => pts.clone(),
    }
}

/// The points a constraint names directly.
pub fn constraint_points(c: &Constraint) -> Vec<u32> {
    match c {
        Constraint::Coincident { a, b } | Constraint::Symmetric { a, b, .. } | Constraint::SymmetricAxis { a, b, .. } | Constraint::Distance { a, b, .. } | Constraint::DistanceU { a, b, .. } | Constraint::DistanceV { a, b, .. } => vec![*a, *b],
        Constraint::OnLine { p, .. } | Constraint::OnCircle { p, .. } | Constraint::Midpoint { p, .. } | Constraint::Fixed { p } | Constraint::PointLine { p, .. } => vec![*p],
        _ => Vec::new(),
    }
}

pub fn constraint_touches_curve(c: &Constraint, id: u32) -> bool {
    match c {
        Constraint::Horizontal { line } | Constraint::Vertical { line } | Constraint::OnLine { line, .. } | Constraint::Midpoint { line, .. } | Constraint::Symmetric { line, .. } | Constraint::PointLine { line, .. } => *line == id,
        Constraint::Parallel { a, b } | Constraint::Perpendicular { a, b } | Constraint::EqualLength { a, b } | Constraint::EqualRadius { a, b } | Constraint::Angle { a, b, .. } => *a == id || *b == id,
        Constraint::Tangent { line, arc } => *line == id || *arc == id,
        Constraint::OnCircle { curve, .. } | Constraint::Radius { curve, .. } | Constraint::Diameter { curve, .. } => *curve == id,
        _ => false,
    }
}

fn residuals(x: &[f64], rows: &[(usize, Row)], out: &mut Vec<f64>) {
    out.clear();
    for (_, r) in rows {
        r.eval(x, out);
    }
}

fn residual(x: &[f64], rows: &[(usize, Row)]) -> f64 {
    let mut r = Vec::with_capacity(rows.len());
    residuals(x, rows, &mut r);
    r.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// Unknowns that only share a row with each other solve on their own, far faster and without
/// disturbing the rest. Each entry is the rows of one such group.
fn groups(n: usize, rows: &[(usize, Row)]) -> Vec<Vec<usize>> {
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut uses = Vec::new();
    for (_, r) in rows {
        uses.clear();
        r.uses(&mut uses);
        for w in uses.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            parent[a] = b;
        }
    }
    let mut out: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, (_, r)) in rows.iter().enumerate() {
        uses.clear();
        r.uses(&mut uses);
        if let Some(u) = uses.first() {
            out.entry(find(&mut parent, *u)).or_default().push(i);
        }
    }
    out.into_values().collect()
}

/// Moves `x` to satisfy the rows, changing it as little as it can. Returns the rows it couldn't
/// meet, by what they stand for.
fn solve_rows(x: &mut [f64], rows: &[(usize, Row)]) -> Vec<usize> {
    let mut bad = Vec::new();
    for group in groups(x.len(), rows) {
        let sub: Vec<(usize, Row)> = group.iter().map(|i| rows[*i]).collect();
        solve_group(x, &sub, &mut bad);
    }
    bad
}

/// The unknowns a group uses, in order, with a lookup from each to its column.
fn columns(rows: &[(usize, Row)]) -> (Vec<usize>, HashMap<usize, usize>) {
    let mut used = Vec::new();
    for (_, r) in rows {
        r.uses(&mut used);
    }
    used.sort_unstable();
    used.dedup();
    let map = used.iter().enumerate().map(|(i, u)| (*u, i)).collect();
    (used, map)
}

/// The derivative of every equation by every unknown, by differences. Rows are dense, columns
/// are the unknowns in `cols`.
fn jacobian(x: &[f64], rows: &[(usize, Row)], cols: &[usize], map: &HashMap<usize, usize>, m: usize) -> Vec<Vec<f64>> {
    let mut j = vec![vec![0.0; cols.len()]; m];
    let mut probe = x.to_vec();
    let mut row_at = 0;
    let (mut plus, mut minus, mut uses) = (Vec::with_capacity(2), Vec::with_capacity(2), Vec::new());
    for (_, r) in rows {
        let n = r.count();
        uses.clear();
        r.uses(&mut uses);
        uses.sort_unstable();
        uses.dedup();
        for &u in &uses {
            let h = 1e-6;
            let keep = probe[u];
            probe[u] = keep + h;
            plus.clear();
            r.eval(&probe, &mut plus);
            probe[u] = keep - h;
            minus.clear();
            r.eval(&probe, &mut minus);
            probe[u] = keep;
            for k in 0..n {
                j[row_at + k][map[&u]] = (plus[k] - minus[k]) / (2.0 * h);
            }
        }
        row_at += n;
    }
    j
}

fn solve_group(x: &mut [f64], rows: &[(usize, Row)], bad: &mut Vec<usize>) {
    let (cols, map) = columns(rows);
    let m: usize = rows.iter().map(|(_, r)| r.count()).sum();
    if m == 0 || cols.is_empty() {
        return;
    }
    let mut r = Vec::with_capacity(m);
    residuals(x, rows, &mut r);
    let norm = |v: &[f64]| v.iter().map(|a| a * a).sum::<f64>().sqrt();
    let mut lambda = 1e-10;
    let mut now = norm(&r);
    for _ in 0..60 {
        if r.iter().all(|v| v.abs() <= MET) {
            return;
        }
        let j = jacobian(x, rows, &cols, &map, m);
        // The smallest move that does it: (J Jᵀ + λI) y = r, dx = -Jᵀ y. Points nothing asks to move
        // stay where they are, which is why a sketch doesn't jump about as it's edited.
        let mut a = vec![vec![0.0; m]; m];
        for p in 0..m {
            for q in p..m {
                let dot: f64 = j[p].iter().zip(&j[q]).map(|(s, t)| s * t).sum();
                a[p][q] = dot;
                a[q][p] = dot;
            }
        }
        let mut improved = false;
        for _ in 0..12 {
            let mut lhs = a.clone();
            for (p, row) in lhs.iter_mut().enumerate() {
                row[p] += lambda;
            }
            let Some(y) = solve_dense(lhs, r.clone()) else {
                lambda = (lambda * 10.0).max(1e-8);
                continue;
            };
            let mut trial = x.to_vec();
            for (c, u) in cols.iter().enumerate() {
                let dx: f64 = (0..m).map(|p| j[p][c] * y[p]).sum();
                trial[*u] -= dx;
            }
            let mut rt = Vec::with_capacity(m);
            residuals(&trial, rows, &mut rt);
            let new = norm(&rt);
            if new < now {
                x.copy_from_slice(&trial);
                r = rt;
                now = new;
                lambda = (lambda * 0.2).max(1e-12);
                improved = true;
                break;
            }
            lambda = (lambda * 10.0).min(1e6);
        }
        if !improved {
            break;
        }
    }
    if r.iter().all(|v| v.abs() <= MET.max(1e-7)) {
        return;
    }
    // Names the rows that are off.
    let mut at = 0;
    for (id, row) in rows {
        let n = row.count();
        if r[at..at + n].iter().any(|v| v.abs() > 1e-7) {
            bad.push(*id);
        }
        at += n;
    }
}

/// Solves a square system by elimination with pivoting; `None` if it's singular.
fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for i in 0..n {
        let pivot = (i..n).max_by(|x, y| a[*x][i].abs().total_cmp(&a[*y][i].abs()))?;
        if a[pivot][i].abs() < 1e-14 {
            return None;
        }
        a.swap(i, pivot);
        b.swap(i, pivot);
        for r in i + 1..n {
            let k = a[r][i] / a[i][i];
            if k != 0.0 {
                for c in i..n {
                    a[r][c] -= k * a[i][c];
                }
                b[r] -= k * b[i];
            }
        }
    }
    for i in (0..n).rev() {
        let mut v = b[i];
        for c in i + 1..n {
            v -= a[i][c] * b[c];
        }
        b[i] = v / a[i][i];
    }
    Some(b)
}

/// How many ways the unknowns can still move, and which ones can: a row-reduced Jacobian has a
/// pivot in every column the constraints determine, and an unknown is pinned only if its row has
/// no free column in it.
fn freedom(x: &[f64], rows: &[(usize, Row)]) -> (usize, Vec<usize>) {
    let mut free = 0;
    let mut loose: Vec<usize> = Vec::new();
    // Unknowns no row mentions are free outright.
    let mut mentioned = vec![false; x.len()];
    let mut uses = Vec::new();
    for (_, r) in rows {
        uses.clear();
        r.uses(&mut uses);
        for u in &uses {
            mentioned[*u] = true;
        }
    }
    for (i, seen) in mentioned.iter().enumerate() {
        if !seen {
            free += 1;
            loose.push(i);
        }
    }
    for group in groups(x.len(), rows) {
        let sub: Vec<(usize, Row)> = group.iter().map(|i| rows[*i]).collect();
        let (cols, map) = columns(&sub);
        let m: usize = sub.iter().map(|(_, r)| r.count()).sum();
        if cols.is_empty() || m == 0 {
            continue;
        }
        let mut j = jacobian(x, &sub, &cols, &map, m);
        let n = cols.len();
        let mut pivots: Vec<(usize, usize)> = Vec::new();
        let mut row = 0;
        for c in 0..n {
            if row >= m {
                break;
            }
            let Some(p) = (row..m).max_by(|a, b| j[*a][c].abs().total_cmp(&j[*b][c].abs())) else { break };
            if j[p][c].abs() < 1e-7 {
                continue;
            }
            j.swap(row, p);
            let lead = j[row][c];
            for v in j[row].iter_mut() {
                *v /= lead;
            }
            let pivot_row = j[row].clone();
            for (r, line) in j.iter_mut().enumerate() {
                if r != row && line[c].abs() > 0.0 {
                    let k = line[c];
                    for (v, t) in line.iter_mut().zip(&pivot_row) {
                        *v -= k * t;
                    }
                }
            }
            pivots.push((row, c));
            row += 1;
        }
        free += n - pivots.len();
        let pivot_cols: Vec<usize> = pivots.iter().map(|p| p.1).collect();
        for c in 0..n {
            match pivots.iter().find(|p| p.1 == c) {
                None => loose.push(cols[c]),
                Some((r, _)) => {
                    if (0..n).any(|o| !pivot_cols.contains(&o) && j[*r][o].abs() > 1e-7) {
                        loose.push(cols[c]);
                    }
                }
            }
        }
    }
    (free, loose)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        let mut v = Vars::new();
        v.insert("L".into(), 0.04);
        v.insert("W".into(), 0.02);
        v
    }

    fn near(a: P2, b: P2) -> bool {
        (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-9
    }

    #[test]
    fn a_rectangle_takes_its_size_from_variables() {
        let mut s = Sketch::default();
        let (p, l) = s.add_rect([0.001, 0.002], [0.03, 0.01]);
        s.constraints.push(Constraint::Distance { a: p[0], b: p[1], value: "L".into() });
        s.constraints.push(Constraint::Distance { a: p[1], b: p[2], value: "W".into() });
        let v = vars();
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        assert_eq!(got.free, 2, "it can still slide about");
        let (a, c) = (got.at(p[0]), got.at(p[2]));
        assert!(((c[0] - a[0]) - 0.04).abs() < 1e-9 && ((c[1] - a[1]) - 0.02).abs() < 1e-9, "{a:?} {c:?}");
        // Position it from the origin and nothing is left free.
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p[0], value: "-L/2".into() });
        s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p[0], value: "-W/2".into() });
        let got = s.solve(&v);
        assert_eq!(got.free, 0);
        assert!(got.loose_points.is_empty(), "{:?}", got.loose_points);
        assert!(near(got.at(p[0]), [-0.02, -0.01]) && near(got.at(p[2]), [0.02, 0.01]));
        // A different value moves it, with the lines still square.
        let mut w = v.clone();
        w.insert("L".into(), 0.06);
        let moved = s.solve(&w);
        assert!(near(moved.at(p[2]), [0.03, 0.01]), "{:?}", moved.at(p[2]));
        let _ = l;
    }

    #[test]
    fn a_plain_number_is_millimetres() {
        let mut s = Sketch::default();
        let (c, curve) = s.add_circle([0.0, 0.0], 0.005);
        s.constraints.push(Constraint::Radius { curve, value: "12.5".into() });
        s.constraints.push(Constraint::Coincident { a: c, b: ORIGIN });
        let got = s.solve(&Vars::new());
        assert!((got.radii[&curve] - 0.0125).abs() < 1e-12);
        assert_eq!(got.free, 0);
    }

    #[test]
    fn what_cannot_hold_is_named() {
        let mut s = Sketch::default();
        let (p, _) = s.add_rect([0.0, 0.0], [0.01, 0.01]);
        s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: "10".into() });
        s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: "20".into() });
        let got = s.solve(&Vars::new());
        assert!(!got.ok());
        assert!(!got.conflicts.is_empty());
        // A dimension that doesn't work out says so, and leaves the rest solved.
        let mut t = Sketch::default();
        let (q, _) = t.add_rect([0.0, 0.0], [0.01, 0.01]);
        t.constraints.push(Constraint::Distance { a: q[0], b: q[1], value: "nope".into() });
        let got = t.solve(&Vars::new());
        assert_eq!(got.broken.len(), 1);
        assert!(got.broken[0].1.contains("nope"), "{:?}", got.broken);
    }

    #[test]
    fn dragging_keeps_what_the_constraints_say() {
        let mut s = Sketch::default();
        let (p, _) = s.add_rect([0.0, 0.0], [0.04, 0.02]);
        s.constraints.push(Constraint::Distance { a: p[0], b: p[1], value: "40".into() });
        let v = Vars::new();
        // Dragging a corner up: the height follows, the width doesn't.
        let got = s.solve_with(&v, &[(p[2], [0.07, 0.03])]);
        assert!(got.ok(), "{got:?}");
        let (a, b, c, d) = (got.at(p[0]), got.at(p[1]), got.at(p[2]), got.at(p[3]));
        assert!((c[0] - 0.07).abs() < 1e-9 && (c[1] - 0.03).abs() < 1e-9, "the dragged corner went where it was asked: {c:?}");
        assert!(((b[0] - a[0]).abs() - 0.04).abs() < 1e-9, "the width stayed 40 mm: {a:?} {b:?}");
        assert!((a[1] - b[1]).abs() < 1e-9 && (c[1] - d[1]).abs() < 1e-9, "the lines stayed level");
        // A point pinned by a dimension to the origin can't be dragged away from it.
        let mut t = Sketch::default();
        let q = t.add_point([0.01, 0.0]);
        t.constraints.push(Constraint::Coincident { a: q, b: ORIGIN });
        let got = t.solve_with(&v, &[(q, [0.05, 0.05])]);
        assert!(near(got.at(q), [0.0, 0.0]), "{:?}", got.at(q));
    }

    #[test]
    fn line_relations_hold() {
        let v = Vars::new();
        let mut s = Sketch::default();
        let (a, b, c, d) = (s.add_point([0.0, 0.0]), s.add_point([0.01, 0.001]), s.add_point([0.0, 0.02]), s.add_point([0.012, 0.024]));
        let (l1, l2) = (s.add_line(a, b), s.add_line(c, d));
        s.constraints.push(Constraint::Horizontal { line: l1 });
        s.constraints.push(Constraint::Parallel { a: l1, b: l2 });
        s.constraints.push(Constraint::EqualLength { a: l1, b: l2 });
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        let (pa, pb, pc, pd) = (got.at(a), got.at(b), got.at(c), got.at(d));
        assert!((pa[1] - pb[1]).abs() < 1e-9 && (pc[1] - pd[1]).abs() < 1e-9);
        assert!((((pb[0] - pa[0]).abs()) - (pd[0] - pc[0]).abs()).abs() < 1e-9);

        // A right angle, then a typed one.
        let mut s = Sketch::default();
        let (o, x, y) = (s.add_point([0.0, 0.0]), s.add_point([0.01, 0.0]), s.add_point([0.002, 0.01]));
        let (l1, l2) = (s.add_line(o, x), s.add_line(o, y));
        s.constraints.push(Constraint::Horizontal { line: l1 });
        s.constraints.push(Constraint::Perpendicular { a: l1, b: l2 });
        let got = s.solve(&v);
        assert!((got.at(o)[0] - got.at(y)[0]).abs() < 1e-9, "{got:?}");
        s.constraints.pop();
        s.constraints.push(Constraint::Angle { a: l1, b: l2, value: "60".into() });
        let got = s.solve(&v);
        let (po, py) = (got.at(o), got.at(y));
        let t = (py[1] - po[1]).atan2(py[0] - po[0]).to_degrees();
        assert!((t - 60.0).abs() < 1e-6, "{t}");
    }

    #[test]
    fn a_point_stays_on_what_it_is_put_on() {
        let v = Vars::new();
        let mut s = Sketch::default();
        let (a, b, p) = (s.add_point([0.0, 0.0]), s.add_point([0.04, 0.0]), s.add_point([0.01, 0.005]));
        let l = s.add_line(a, b);
        s.constraints.push(Constraint::OnLine { p, line: l });
        let got = s.solve(&v);
        let (qa, qb, qp) = (got.at(a), got.at(b), got.at(p));
        let cross = (qb[0] - qa[0]) * (qp[1] - qa[1]) - (qb[1] - qa[1]) * (qp[0] - qa[0]);
        assert!(cross.abs() < 1e-12, "{cross}");
        s.constraints.pop();
        s.constraints.push(Constraint::Midpoint { p, line: l });
        let got = s.solve(&v);
        let (qa, qb, qp) = (got.at(a), got.at(b), got.at(p));
        assert!(near(qp, [(qa[0] + qb[0]) / 2.0, (qa[1] + qb[1]) / 2.0]));
        let (c, circle) = s.add_circle([0.1, 0.1], 0.01);
        let q = s.add_point([0.12, 0.1]);
        s.constraints.push(Constraint::OnCircle { p: q, curve: circle });
        let got = s.solve(&v);
        let (qc, qq, r) = (got.at(c), got.at(q), got.radii[&circle]);
        assert!(((qq[0] - qc[0]).hypot(qq[1] - qc[1]) - r).abs() < 1e-9);
    }

    #[test]
    fn mirror_images_stay_mirrored() {
        let v = Vars::new();
        let mut s = Sketch::default();
        let (a, b) = (s.add_point([0.01, 0.004]), s.add_point([-0.012, 0.006]));
        s.constraints.push(Constraint::SymmetricAxis { a, b, axis: 0 });
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: a, value: "10".into() });
        let got = s.solve(&v);
        let (qa, qb) = (got.at(a), got.at(b));
        assert!((qa[0] + qb[0]).abs() < 1e-9 && (qa[1] - qb[1]).abs() < 1e-9, "{qa:?} {qb:?}");
        assert!((qa[0] - 0.01).abs() < 1e-9);

        let mut s = Sketch::default();
        let (c, d, p, q) = (s.add_point([0.0, 0.0]), s.add_point([0.02, 0.02]), s.add_point([0.01, 0.0]), s.add_point([0.0, 0.012]));
        let l = s.add_line(c, d);
        s.constraints.push(Constraint::Symmetric { a: p, b: q, line: l });
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        let (qc, qd, qp, qq) = (got.at(c), got.at(d), got.at(p), got.at(q));
        let u = unit2([qd[0] - qc[0], qd[1] - qc[1]]);
        assert!(((qq[0] - qp[0]) * u[0] + (qq[1] - qp[1]) * u[1]).abs() < 1e-9);
    }

    #[test]
    fn an_arc_keeps_one_radius_and_can_be_dimensioned() {
        let v = Vars::new();
        let mut s = Sketch::default();
        let arc = s.add_arc([0.0, 0.0], [0.01, 0.0], [0.0, 0.012]);
        let CurveKind::Arc { c, a, b } = s.curve(arc).unwrap().kind else { panic!() };
        s.constraints.push(Constraint::Radius { curve: arc, value: "5".into() });
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        let (qc, qa, qb) = (got.at(c), got.at(a), got.at(b));
        assert!(((qa[0] - qc[0]).hypot(qa[1] - qc[1]) - 0.005).abs() < 1e-9);
        assert!(((qb[0] - qc[0]).hypot(qb[1] - qc[1]) - 0.005).abs() < 1e-9);
    }

    #[test]
    fn a_corner_rounds_with_a_radius_that_stays_a_dimension() {
        let mut v = Vars::new();
        v.insert("R".into(), 0.002);
        let mut s = Sketch::default();
        let (p, l) = s.add_rect([0.0, 0.0], [0.02, 0.01]);
        s.constraints.push(Constraint::Distance { a: p[0], b: p[1], value: "20".into() });
        s.constraints.push(Constraint::Distance { a: p[1], b: p[2], value: "10".into() });
        s.constraints.push(Constraint::DistanceU { a: ORIGIN, b: p[0], value: "0".into() });
        s.constraints.push(Constraint::DistanceV { a: ORIGIN, b: p[0], value: "0".into() });
        let arc = s.fillet_corner(p[2], "R", &v).unwrap();
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        let CurveKind::Arc { c, a, b } = s.curve(arc).unwrap().kind else { panic!() };
        // Tangent to both sides, so its centre is one radius in from each.
        assert!(near(got.at(c), [0.018, 0.008]), "{:?}", got.at(c));
        assert!(near(got.at(a), [0.02, 0.008]) || near(got.at(b), [0.02, 0.008]), "{:?} {:?}", got.at(a), got.at(b));
        // The radius follows its variable.
        v.insert("R".into(), 0.003);
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        assert!(near(got.at(c), [0.017, 0.007]), "{:?}", got.at(c));
        // Too big for the corner: refused, with how much it needs.
        let mut t = Sketch::default();
        let (q, _) = t.add_rect([0.0, 0.0], [0.002, 0.002]);
        assert!(t.fillet_corner(q[1], "5", &Vars::new()).unwrap_err().to_string().contains("needs"));
        let _ = l;
    }

    #[test]
    fn a_click_can_define_everything() {
        let v = Vars::new();
        let mut s = Sketch::default();
        s.add_rect([0.003, 0.004], [0.02, 0.01]);
        s.add_circle([0.03, 0.03], 0.002);
        assert!(s.solve(&v).free > 0);
        s.fully_define(&v);
        let got = s.solve(&v);
        assert_eq!(got.free, 0, "{got:?}");
        // It stayed where it was drawn.
        assert!(s.points.iter().any(|p| near(got.at(p.id), [0.003, 0.004])));
        assert!(got.radii.values().any(|r| (r - 0.002).abs() < 1e-9));
    }

    #[test]
    fn an_equation_curve_starts_and_ends_where_its_formula_says() {
        let mut v = Vars::new();
        v.insert("a".into(), 0.01);
        let mut s = Sketch::default();
        let (p, q) = (s.add_point([0.0, 0.0]), s.add_point([0.0, 0.0]));
        s.add_curve(CurveKind::Equation { a: p, b: q, u: "t".into(), v: "a*exp(t/0.05)".into(), t0: "0".into(), t1: "0.05".into() });
        let got = s.solve(&v);
        assert!(got.ok(), "{got:?}");
        assert!(near(got.at(p), [0.0, 0.01]));
        assert!(near(got.at(q), [0.05, 0.01 * std::f64::consts::E]), "{:?}", got.at(q));
        let pts = equation_samples("t", "a*exp(t/0.05)", "0", "0.05", 10, &v).unwrap();
        assert_eq!(pts.len(), 11);
    }

    #[test]
    fn removing_something_takes_its_constraints_with_it() {
        let mut s = Sketch::default();
        let (p, l) = s.add_rect([0.0, 0.0], [0.01, 0.01]);
        s.constraints.push(Constraint::Distance { a: p[0], b: p[1], value: "10".into() });
        let before = s.constraints.len();
        s.remove_curve(l[0]);
        assert_eq!(s.constraints.len(), before - 1, "its Horizontal went; the distance names points");
        s.remove_point(p[0]);
        assert!(s.curves.iter().all(|c| !curve_points(&c.kind).contains(&p[0])));
        assert!(s.constraints.iter().all(|c| !constraint_points(c).contains(&p[0])));
        // Ids aren't reused, since a face's name comes from its curve's.
        let n = s.add_point([0.0, 0.0]);
        assert!(n > l[3], "{n}");
    }

    #[test]
    fn a_lone_point_outlives_a_curve_being_removed() {
        let mut s = Sketch::default();
        let p = s.add_point([0.01, 0.01]);
        let (corners, lines) = s.add_rect([0.0, 0.0], [0.02, 0.02]);
        s.remove_curve(lines[0]);
        assert!(s.point(p).is_some(), "it was placed on purpose, for a feed perhaps");
        assert!(s.point(corners[0]).is_some(), "still on another curve");
        s.remove_curve(lines[3]);
        assert!(s.point(corners[0]).is_none(), "nothing uses it now");
    }

    #[test]
    fn it_round_trips_through_json() {
        let mut s = Sketch::default();
        let (p, l) = s.add_rect([0.0, 0.0], [0.01, 0.01]);
        s.constraints.push(Constraint::Distance { a: p[0], b: p[1], value: "L".into() });
        s.constraints.push(Constraint::Parallel { a: l[0], b: l[2] });
        s.add_arc([0.0, 0.0], [0.01, 0.0], [0.0, 0.01]);
        let back: Sketch = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
