//! The feature tree: what a model is, in order, each step an expression of what came before.
//! Building it is a pure function of the tree and the variables, which is what makes a sweep
//! or an optimiser step just another build. A step that fails is marked and the rest go on, so
//! one bad dimension doesn't take the whole model off the canvas.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::boolean::{self, Op};
use super::blend;
use super::mesh::{FaceId, Frame, Mesh, Xform, face_tag, feature_of, local_of};
use super::prims::{self, Profiles, Region, Ring};
use super::sketch::P2;
use super::sketch::{Sketch, Solved};
use super::{angle, length};
use crate::expr::{self, Vars};
use crate::geometry::{MeshHint, P3, PortDef, Shape, Solid, add, cross, dot, norm, scale, sub, unit};
use crate::material;

/// Points closer than this are one point, metres.
const WELD: f64 = 1e-9;

fn default_facets() -> u32 {
    32
}

/// A plane to sketch on or mirror in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlaneRef {
    /// Normal to a world axis: 2 is Top (x, y), 1 is Front (x, z), 0 is Right (y, z).
    Base { axis: usize, offset: String },
    /// On a flat face of a body, by its name, so it follows the face.
    Face { face: FaceId, offset: String },
    Custom { origin: [String; 3], normal: [String; 3] },
    /// A base plane of a coordinate system, so it follows the system when that moves or turns.
    InCoord { coord: u32, axis: usize, offset: String },
}

impl Default for PlaneRef {
    fn default() -> Self {
        Self::Base { axis: 2, offset: "0".into() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Extent {
    /// Along the plane's normal.
    #[default]
    One,
    Reverse,
    /// Half each way.
    Both,
    /// Through everything there is, both ways: for a cut that has to go right through.
    Through,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    New,
    Add,
    Cut,
    Common,
}

/// Where what a feature makes goes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    #[serde(default)]
    pub mode: Mode,
    /// The body it adds to or cuts from; `None` is the latest one.
    #[serde(default)]
    pub target: Option<u32>,
    /// Empty: the project's metal.
    #[serde(default)]
    pub material: String,
    /// The FDTD cell or surface mesh edge wanted on this body, as an expression. Empty: chosen for it.
    #[serde(default)]
    pub mesh: String,
    /// A surface with no thickness rather than a solid.
    #[serde(default)]
    pub surface: bool,
    /// How thick a flat sheet's metal is, up from its plane, as an expression. Empty or zero: it has
    /// none. Time-domain solvers see it as a real slab; the method of moments keeps the sheet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thick: String,
    /// The coordinate system it's drawn in. `None`: the world's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coord: Option<u32>,
}

/// A coordinate system to draw in: where its origin is, and how it's turned from the world's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoordSys {
    pub id: u32,
    pub name: String,
    pub origin: [String; 3],
    /// Turns about the world's x, y and z axes, in that order.
    pub turn: [String; 3],
}

impl CoordSys {
    /// What drawing in it does to a shape: turned, then carried to its origin.
    pub fn xform(&self, vars: &Vars) -> Result<Xform> {
        let o = three(&self.origin, vars).context("its origin")?;
        let mut m = Xform::IDENTITY;
        for (k, a) in self.turn.iter().enumerate() {
            let t = if a.trim().is_empty() { 0.0 } else { angle(a, vars).context("its turn")? };
            let mut axis = [0.0; 3];
            axis[k] = 1.0;
            m = Xform::rotate([0.0; 3], axis, t).after(&m);
        }
        Ok(Xform { m: m.m, t: o })
    }
}

/// A flat shape on a plane, by the dimensions a designer thinks of it in. Coordinates are the
/// plane's own, as an expression each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum Flat {
    /// From a corner. A size can be negative: it runs the other way.
    Rect { p0: [String; 2], size: [String; 2] },
    Circle { c: [String; 2], r: String },
    Ellipse { c: [String; 2], rx: String, ry: String },
    /// A regular polygon, with a corner on `+u`.
    Polygon { c: [String; 2], r: String, sides: String },
}

impl Flat {
    /// The outline, counter-clockwise, with no holes.
    fn region(&self, vars: &Vars, facets: usize) -> Result<Region> {
        let two = |q: &[String; 2]| -> Result<P2> { Ok([length(&q[0], vars)?, length(&q[1], vars)?]) };
        let round = |c: P2, rx: f64, ry: f64, n: usize| -> Vec<P2> { (0..n).map(|k| { let t = std::f64::consts::TAU * k as f64 / n as f64; [c[0] + rx * t.cos(), c[1] + ry * t.sin()] }).collect() };
        let (pts, sides_named) = match self {
            Flat::Rect { p0, size } => {
                let (a, s) = (two(p0)?, two(size)?);
                if s[0] == 0.0 || s[1] == 0.0 {
                    bail!("A rectangle needs some size both ways.");
                }
                (vec![a, [a[0] + s[0], a[1]], [a[0] + s[0], a[1] + s[1]], [a[0], a[1] + s[1]]], true)
            }
            Flat::Circle { c, r } => {
                let (c, r) = (two(c)?, length(r, vars)?);
                if !(r > 0.0) {
                    bail!("A circle needs a radius above zero.");
                }
                (round(c, r, r, facets), false)
            }
            Flat::Ellipse { c, rx, ry } => {
                let (c, rx, ry) = (two(c)?, length(rx, vars)?, length(ry, vars)?);
                if !(rx > 0.0 && ry > 0.0) {
                    bail!("An ellipse needs both radii above zero.");
                }
                (round(c, rx, ry, facets), false)
            }
            Flat::Polygon { c, r, sides } => {
                let (c, r) = (two(c)?, length(r, vars)?);
                let n = expr::eval(sides, vars).context("the number of sides")?.round();
                if !(r > 0.0) {
                    bail!("A polygon needs a radius above zero.");
                }
                if !(3.0..=256.0).contains(&n) {
                    bail!("A polygon has 3 to 256 sides, and this comes to {n}.");
                }
                (round(c, r, r, n as usize), true)
            }
        };
        // A straight side is a face of its own when the shape's given depth. A round one is one face.
        let tags = (0..pts.len()).map(|i| if sides_named { i as u32 } else { 0 }).collect();
        Ok(Region { outer: Ring { pts, tags }.counter_clockwise(), holes: Vec::new() })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Flat::Rect { .. } => "Rectangle",
            Flat::Circle { .. } => "Circle",
            Flat::Ellipse { .. } => "Ellipse",
            Flat::Polygon { .. } => "Polygon",
        }
    }
}

/// What a sketch turns about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AxisRef {
    /// The sketch's own u axis.
    U,
    V,
    /// A line in the sketch, usually a construction line.
    Line { curve: u32 },
}

/// A route through space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PathRef {
    Sketch { sketch: u32 },
    Helix {
        origin: [String; 3],
        axis: [String; 3],
        radius: String,
        /// The radius at the far end for a conical helix. Empty: the same.
        #[serde(default)]
        radius2: String,
        pitch: String,
        turns: String,
        #[serde(default)]
        left: bool,
    },
    Points { pts: Vec<[String; 3]> },
}

/// A plain solid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Prim {
    Box { min: [String; 3], max: [String; 3] },
    /// A box by a corner and how far it runs from there. A size can be negative: it runs the other way.
    Cuboid { p0: [String; 3], size: [String; 3] },
    Cylinder { a: [String; 3], b: [String; 3], r: String },
    Cone { a: [String; 3], b: [String; 3], r0: String, r1: String },
    Sphere { c: [String; 3], r: String },
    Torus { c: [String; 3], axis: [String; 3], big: String, small: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layout {
    Linear {
        step: [String; 3],
        count: String,
        #[serde(default)]
        step2: Option<[String; 3]>,
        #[serde(default)]
        count2: String,
    },
    Circular { origin: [String; 3], axis: [String; 3], count: String, total: String },
}

/// One end of a feed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Anchor {
    Xyz { at: [String; 3] },
    /// A point of a sketch, so the feed goes where its dimensions take it.
    Sketch {
        sketch: u32,
        point: u32,
        /// How far off the sketch's plane along its normal.
        #[serde(default)]
        height: String,
    },
    /// A fixed step from a sketch point: a feed's far end, which stays square to it wherever it goes.
    Offset { sketch: u32, point: u32, by: [String; 3] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineOp {
    Add,
    Cut,
    Common,
    /// What's in one or the other, but not both.
    Exclude,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Sketch { plane: PlaneRef, sketch: Sketch },
    Extrude {
        sketch: u32,
        depth: String,
        #[serde(default)]
        extent: Extent,
        #[serde(default)]
        out: Output,
    },
    /// The flat surface a closed sketch fills: a patch, a ground plane, a slot in it.
    Sheet {
        sketch: u32,
        #[serde(default)]
        out: Output,
    },
    /// A flat shape with no sketch behind it: a rectangle, a circle, an ellipse, a polygon.
    Face {
        plane: PlaneRef,
        shape: Flat,
        #[serde(default)]
        out: Output,
    },
    Revolve {
        sketch: u32,
        axis: AxisRef,
        angle: String,
        #[serde(default)]
        out: Output,
    },
    Sweep {
        sketch: u32,
        path: PathRef,
        #[serde(default)]
        out: Output,
    },
    Loft {
        sections: Vec<u32>,
        #[serde(default)]
        out: Output,
    },
    Solid {
        shape: Prim,
        #[serde(default)]
        out: Output,
    },
    /// A thin round conductor along a path.
    Wire {
        path: PathRef,
        radius: String,
        #[serde(default)]
        out: Output,
    },
    Combine {
        op: CombineOp,
        target: u32,
        tools: Vec<u32>,
        #[serde(default)]
        keep_tools: bool,
    },
    Pattern {
        bodies: Vec<u32>,
        layout: Layout,
        /// Into one body, rather than a body each.
        #[serde(default)]
        merge: bool,
        /// Feeds to copy along with it, so an array gets a port per element.
        #[serde(default)]
        feeds: Vec<u32>,
    },
    Mirror {
        bodies: Vec<u32>,
        plane: PlaneRef,
        #[serde(default)]
        merge: bool,
    },
    Move {
        bodies: Vec<u32>,
        by: [String; 3],
        #[serde(default)]
        origin: Option<[String; 3]>,
        #[serde(default)]
        axis: Option<[String; 3]>,
        #[serde(default)]
        angle: String,
        /// Leaves the original where it is.
        #[serde(default)]
        copy: bool,
    },
    /// Rounds or bevels the edges between pairs of faces.
    Fillet {
        body: u32,
        edges: Vec<[FaceId; 2]>,
        radius: String,
        #[serde(default)]
        chamfer: bool,
    },
    /// Hollows a solid out, leaving some faces open. Thickness 0 leaves only the surface: a
    /// horn's walls, ready for the method of moments.
    Shell {
        body: u32,
        #[serde(default)]
        open: Vec<FaceId>,
        thick: String,
    },
    /// Gives a surface a thickness, half each side.
    Thicken { body: u32, thick: String },
    Import {
        file: String,
        tris: Vec<[P3; 3]>,
        #[serde(default)]
        out: Output,
    },
    Feed {
        a: Anchor,
        b: Anchor,
        #[serde(default)]
        width: String,
        z0: String,
    },
    /// A feed along the sketch's normal through a point, between the metal it meets first and last.
    Probe {
        sketch: u32,
        point: u32,
        #[serde(default)]
        width: String,
        z0: String,
    },
}

impl Kind {
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Sketch { .. } => "Sketch",
            Kind::Extrude { .. } => "Extrude",
            Kind::Sheet { .. } => "Sheet",
            Kind::Face { shape, .. } => shape.label(),
            Kind::Revolve { .. } => "Revolve",
            Kind::Sweep { .. } => "Sweep",
            Kind::Loft { .. } => "Loft",
            Kind::Solid { shape, .. } => match shape {
                Prim::Box { .. } | Prim::Cuboid { .. } => "Box",
                Prim::Cylinder { .. } => "Cylinder",
                Prim::Cone { .. } => "Cone",
                Prim::Sphere { .. } => "Sphere",
                Prim::Torus { .. } => "Torus",
            },
            Kind::Wire { .. } => "Wire",
            Kind::Combine { .. } => "Combine",
            Kind::Pattern { .. } => "Pattern",
            Kind::Mirror { .. } => "Mirror",
            Kind::Move { .. } => "Move",
            Kind::Fillet { chamfer: false, .. } => "Fillet",
            Kind::Fillet { .. } => "Chamfer",
            Kind::Shell { .. } => "Shell",
            Kind::Thicken { .. } => "Thicken",
            Kind::Import { .. } => "Import",
            Kind::Feed { .. } => "Feed",
            Kind::Probe { .. } => "Probe",
        }
    }

    /// The other features this one needs.
    pub fn uses(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let path = |p: &PathRef, out: &mut Vec<u32>| {
            if let PathRef::Sketch { sketch } = p {
                out.push(*sketch);
            }
        };
        let anchor = |a: &Anchor, out: &mut Vec<u32>| {
            if let Anchor::Sketch { sketch, .. } | Anchor::Offset { sketch, .. } = a {
                out.push(*sketch);
            }
        };
        match self {
            Kind::Sketch { .. } | Kind::Solid { .. } | Kind::Import { .. } | Kind::Face { .. } => {}
            Kind::Extrude { sketch, .. } | Kind::Sheet { sketch, .. } | Kind::Revolve { sketch, .. } | Kind::Probe { sketch, .. } => out.push(*sketch),
            Kind::Sweep { sketch, path: p, .. } => {
                out.push(*sketch);
                path(p, &mut out);
            }
            Kind::Loft { sections, .. } => out.extend(sections),
            Kind::Wire { path: p, .. } => path(p, &mut out),
            Kind::Combine { target, tools, .. } => {
                out.push(*target);
                out.extend(tools);
            }
            Kind::Pattern { bodies, feeds, .. } => {
                out.extend(bodies);
                out.extend(feeds);
            }
            Kind::Mirror { bodies, .. } | Kind::Move { bodies, .. } => out.extend(bodies),
            Kind::Fillet { body, .. } | Kind::Shell { body, .. } | Kind::Thicken { body, .. } => out.push(*body),
            Kind::Feed { a, b, .. } => {
                anchor(a, &mut out);
                anchor(b, &mut out);
            }
        }
        out
    }

    /// Where its output settings are, if it makes geometry of its own.
    pub fn output_mut(&mut self) -> Option<&mut Output> {
        match self {
            Kind::Extrude { out, .. } | Kind::Sheet { out, .. } | Kind::Face { out, .. } | Kind::Revolve { out, .. } | Kind::Sweep { out, .. } | Kind::Loft { out, .. } | Kind::Solid { out, .. } | Kind::Wire { out, .. } | Kind::Import { out, .. } => Some(out),
            _ => None,
        }
    }

    pub fn output(&self) -> Option<&Output> {
        match self {
            Kind::Extrude { out, .. } | Kind::Sheet { out, .. } | Kind::Face { out, .. } | Kind::Revolve { out, .. } | Kind::Sweep { out, .. } | Kind::Loft { out, .. } | Kind::Solid { out, .. } | Kind::Wire { out, .. } | Kind::Import { out, .. } => Some(out),
            _ => None,
        }
    }

    /// Every reference to another feature, for a copy that has to point at its own.
    fn remap(&mut self, map: &BTreeMap<u32, u32>) {
        let id = |x: &mut u32| {
            if let Some(n) = map.get(x) {
                *x = *n;
            }
        };
        let face = |x: &mut FaceId| {
            if let Some(n) = map.get(&feature_of(*x)) {
                *x = face_tag(*n, local_of(*x));
            }
        };
        let list = |v: &mut Vec<u32>| v.iter_mut().for_each(id);
        let plane = |p: &mut PlaneRef| {
            if let PlaneRef::Face { face: f, .. } = p {
                face(f);
            }
        };
        let path = |p: &mut PathRef| {
            if let PathRef::Sketch { sketch } = p {
                id(sketch);
            }
        };
        let anchor = |a: &mut Anchor| {
            if let Anchor::Sketch { sketch, .. } | Anchor::Offset { sketch, .. } = a {
                id(sketch);
            }
        };
        if let Some(out) = self.output_mut()
            && let Some(t) = &mut out.target
        {
            id(t);
        }
        match self {
            Kind::Sketch { plane: p, .. } | Kind::Face { plane: p, .. } => plane(p),
            Kind::Extrude { sketch, .. } | Kind::Sheet { sketch, .. } | Kind::Revolve { sketch, .. } | Kind::Probe { sketch, .. } => id(sketch),
            Kind::Sweep { sketch, path: p, .. } => {
                id(sketch);
                path(p);
            }
            Kind::Loft { sections, .. } => list(sections),
            Kind::Wire { path: p, .. } => path(p),
            Kind::Solid { .. } | Kind::Import { .. } => {}
            Kind::Combine { target, tools, .. } => {
                id(target);
                list(tools);
            }
            Kind::Pattern { bodies, feeds, .. } => {
                list(bodies);
                list(feeds);
            }
            Kind::Mirror { bodies, plane: p, .. } => {
                list(bodies);
                plane(p);
            }
            Kind::Move { bodies, .. } => list(bodies),
            Kind::Fillet { body, edges, .. } => {
                id(body);
                for e in edges {
                    face(&mut e[0]);
                    face(&mut e[1]);
                }
            }
            Kind::Shell { body, open, .. } => {
                id(body);
                open.iter_mut().for_each(face);
            }
            Kind::Thicken { body, .. } => id(body),
            Kind::Feed { a, b, .. } => {
                anchor(a);
                anchor(b);
            }
        }
    }

    /// Leaves out bodies that aren't in a copy, so a move of several doesn't move the original twice.
    fn keep_only(&mut self, within: &[u32]) {
        if let Kind::Pattern { bodies, .. } | Kind::Mirror { bodies, .. } | Kind::Move { bodies, .. } = self {
            bodies.retain(|b| within.contains(b));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub id: u32,
    pub name: String,
    /// Suppressed: kept in the tree, doing nothing.
    #[serde(default)]
    pub off: bool,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub features: Vec<Feature>,
    /// Rolled back: only this many features count.
    #[serde(default)]
    pub rollback: Option<usize>,
    /// How many flat sides a full circle gets.
    #[serde(default = "default_facets")]
    pub facets: u32,
    /// Coordinate systems to draw in, besides the world's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coords: Vec<CoordSys>,
}

impl Default for Model {
    fn default() -> Self {
        Self { features: Vec::new(), rollback: None, facets: default_facets(), coords: Vec::new() }
    }
}

/// What a tree shows as one object: a body, and the steps that made it what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSteps {
    /// The feature that made it, which is also the body's id.
    pub id: u32,
    pub name: String,
    /// In the tree's order: the sketches it came from, what made it, and what's changed it since.
    pub steps: Vec<u32>,
}

impl Model {
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Nothing that a saved project would need.
    pub fn nothing_to_save(&self) -> bool {
        self.features.is_empty() && self.coords.is_empty()
    }

    pub fn coord(&self, id: u32) -> Option<&CoordSys> {
        self.coords.iter().find(|c| c.id == id)
    }

    /// A new coordinate system at the world's origin, unturned.
    pub fn add_coord(&mut self) -> u32 {
        let id = self.coords.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        let name = (1..).map(|k| format!("System {k}")).find(|n| !self.coords.iter().any(|c| &c.name == n)).unwrap_or_default();
        let zero = || ["0".to_string(), "0".to_string(), "0".to_string()];
        self.coords.push(CoordSys { id, name, origin: zero(), turn: zero() });
        id
    }

    /// Removes a coordinate system. What was drawn in it goes back to the global one, which moves it
    /// unless the system was at rest, so the caller says so.
    pub fn remove_coord(&mut self, id: u32) {
        self.coords.retain(|c| c.id != id);
        for f in &mut self.features {
            if let Some(out) = f.kind.output_mut()
                && out.coord == Some(id)
            {
                out.coord = None;
            }
            if let Kind::Sketch { plane, .. } = &mut f.kind
                && let PlaneRef::InCoord { coord, axis, offset } = plane
                && *coord == id
            {
                *plane = PlaneRef::Base { axis: *axis, offset: offset.clone() };
            }
        }
    }

    /// What's drawn in this coordinate system.
    pub fn drawn_in(&self, id: u32) -> Vec<&Feature> {
        self.features
            .iter()
            .filter(|f| f.kind.output().is_some_and(|o| o.coord == Some(id)) || matches!(&f.kind, Kind::Sketch { plane: PlaneRef::InCoord { coord, .. }, .. } if *coord == id))
            .collect()
    }

    /// Takes a whole object out: what made it and what's changed it. A sketch that something else is
    /// also made from stays, and a step that also works on other bodies goes on working on them.
    pub fn remove_object(&mut self, id: u32) {
        let Some(object) = self.objects().into_iter().find(|o| o.id == id) else {
            self.remove(id);
            return;
        };
        let bodies: Vec<u32> = std::iter::once(id).chain(object.steps.iter().copied().filter(|s| self.feature(*s).is_some_and(|f| f.kind.output().is_some_and(|o| o.mode == Mode::New)))).collect();
        let mut going: Vec<u32> = Vec::new();
        for s in &object.steps {
            let others = match self.feature(*s).map(|f| &f.kind) {
                Some(Kind::Move { bodies: list, .. } | Kind::Pattern { bodies: list, .. } | Kind::Mirror { bodies: list, .. }) => list.iter().any(|b| !bodies.contains(b)),
                _ => false,
            };
            if !others {
                going.push(*s);
            }
        }
        // A sketch stays while something staying is made from it.
        loop {
            let before = going.len();
            let keep: Vec<u32> = going.iter().copied().filter(|s| self.sketch(*s).is_none() || self.dependents(*s).iter().all(|d| going.contains(d))).collect();
            going = keep;
            if going.len() == before {
                break;
            }
        }
        for s in &going {
            self.remove(*s);
        }
        // What's left mustn't go on asking for what's gone.
        let mut emptied = Vec::new();
        for f in &mut self.features {
            if let Kind::Move { bodies: list, .. } | Kind::Pattern { bodies: list, .. } | Kind::Mirror { bodies: list, .. } = &mut f.kind {
                let had = list.len();
                list.retain(|b| !bodies.contains(b));
                if had > 0 && list.is_empty() {
                    emptied.push(f.id);
                }
            }
        }
        for s in emptied {
            self.remove(s);
        }
    }

    /// Which body each step works on, from what it says and, where it says "the latest", from
    /// the order they stand in. The same rule `deliver` builds by.
    fn bodies_of_steps(&self) -> BTreeMap<u32, Vec<u32>> {
        let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        // The bodies standing so far, wires aside. The latest is the highest id, as in `deliver`.
        let mut standing: Vec<u32> = Vec::new();
        let n = self.rollback.unwrap_or(self.features.len());
        for (i, f) in self.features.iter().enumerate() {
            let counts = !f.off && i < n;
            let on: Vec<u32> = match &f.kind {
                Kind::Combine { target, tools, keep_tools, .. } => {
                    if counts && !keep_tools {
                        standing.retain(|b| !tools.contains(b));
                    }
                    let mut all = vec![*target];
                    all.extend(tools);
                    all
                }
                Kind::Fillet { body, .. } | Kind::Shell { body, .. } | Kind::Thicken { body, .. } => vec![*body],
                Kind::Pattern { bodies, .. } | Kind::Mirror { bodies, .. } | Kind::Move { bodies, .. } => bodies.clone(),
                k => match k.output() {
                    Some(o) if o.mode != Mode::New && !matches!(k, Kind::Wire { .. }) => o.target.or_else(|| standing.iter().max().copied()).into_iter().collect(),
                    Some(_) => {
                        if counts && !matches!(k, Kind::Wire { .. }) {
                            standing.push(f.id);
                        }
                        vec![f.id]
                    }
                    None => Vec::new(),
                },
            };
            out.insert(f.id, on);
        }
        out
    }

    /// The tree as objects, each with its steps. A sketch goes with what's made from it, and only
    /// the first if several are. A body combined into another goes with it, so nothing's listed twice.
    pub fn objects(&self) -> Vec<ObjectSteps> {
        let on = self.bodies_of_steps();
        let makes = |f: &Feature| f.kind.output().is_some_and(|o| o.mode == Mode::New || matches!(f.kind, Kind::Wire { .. }));
        // Combined away: which body each ended up in. Not by a Combine that's off or rolled back:
        // those bodies are still there, each its own object.
        let mut into: BTreeMap<u32, u32> = BTreeMap::new();
        let n = self.rollback.unwrap_or(self.features.len());
        for (i, f) in self.features.iter().enumerate() {
            if f.off || i >= n {
                continue;
            }
            if let Kind::Combine { target, tools, keep_tools: false, .. } = &f.kind {
                for t in tools {
                    into.insert(*t, *target);
                }
            }
        }
        let root = |mut b: u32| {
            let mut hops = 0;
            while let Some(n) = into.get(&b) {
                b = *n;
                hops += 1;
                if hops > self.features.len() {
                    break;
                }
            }
            b
        };
        let mut list: Vec<ObjectSteps> = self.features.iter().filter(|f| makes(f) && !into.contains_key(&f.id)).map(|f| ObjectSteps { id: f.id, name: f.name.clone(), steps: Vec::new() }).collect();
        let mut placed: Vec<u32> = Vec::new();
        for f in &self.features {
            let Some(first) = on.get(&f.id).and_then(|l| l.first()) else { continue };
            let owner = root(*first);
            let Some(o) = list.iter_mut().find(|o| o.id == owner) else { continue };
            for s in f.kind.uses() {
                if self.sketch(s).is_some() && !placed.contains(&s) {
                    placed.push(s);
                    o.steps.push(s);
                }
            }
            if !placed.contains(&f.id) {
                placed.push(f.id);
                o.steps.push(f.id);
            }
        }
        list
    }

    /// Steps no object claims: sketches nothing's made from yet.
    pub fn loose_steps(&self) -> Vec<u32> {
        let claimed: Vec<u32> = self.objects().into_iter().flat_map(|o| o.steps).collect();
        self.features.iter().filter(|f| !claimed.contains(&f.id) && !matches!(f.kind, Kind::Feed { .. } | Kind::Probe { .. })).map(|f| f.id).collect()
    }

    /// Everything a copy of these steps needs to be built on its own, in the tree's order.
    pub fn steps_to_copy(&self, ids: &[u32]) -> Vec<u32> {
        let on = self.bodies_of_steps();
        let mut wanted: Vec<u32> = ids.to_vec();
        loop {
            let before = wanted.len();
            for f in &self.features {
                // What a wanted step needs. A move or a pattern brought along by one body it works on
                // doesn't need the others: `keep_only` leaves them out of the copy.
                let along_for_one = matches!(f.kind, Kind::Move { .. } | Kind::Pattern { .. } | Kind::Mirror { .. }) && !ids.contains(&f.id);
                if wanted.contains(&f.id) && !along_for_one {
                    for u in f.kind.uses() {
                        if !wanted.contains(&u) && self.feature(u).is_some() {
                            wanted.push(u);
                        }
                    }
                }
                // What changed a wanted body. A pattern of it is not the body. A body combined into
                // another is copied as it was: only copying what's left of a Combine brings the rest.
                let list = on.get(&f.id).map_or(&[][..], Vec::as_slice);
                let changes = match &f.kind {
                    Kind::Combine { .. } => list.first().is_some_and(|b| wanted.contains(b)),
                    Kind::Move { copy: false, .. } | Kind::Fillet { .. } | Kind::Shell { .. } | Kind::Thicken { .. } | Kind::Mirror { merge: true, .. } | Kind::Pattern { merge: true, .. } => list.iter().any(|b| wanted.contains(b)),
                    k => k.output().is_some_and(|o| o.mode != Mode::New) && list.iter().any(|b| wanted.contains(b)),
                };
                if changes && !wanted.contains(&f.id) {
                    wanted.push(f.id);
                }
            }
            if wanted.len() == before {
                break;
            }
        }
        self.features.iter().filter(|f| wanted.contains(&f.id)).map(|f| f.id).collect()
    }

    /// The features named, as they stand, for pasting later. Their references to each other are
    /// kept, and pasting points them at the copies.
    pub fn copy_steps(&self, ids: &[u32]) -> Vec<Feature> {
        let want = self.steps_to_copy(ids);
        self.features.iter().filter(|f| want.contains(&f.id)).cloned().collect()
    }

    /// Adds copies of these features with their own ids and names, so each has its own dimensions
    /// from here on. Gives their new ids, in order.
    pub fn paste_steps(&mut self, list: &[Feature]) -> Vec<u32> {
        let mut next = self.fresh_id();
        let mut map = BTreeMap::new();
        for f in list {
            map.insert(f.id, next);
            next += 1;
        }
        let within: Vec<u32> = list.iter().map(|f| f.id).collect();
        let mut out = Vec::with_capacity(list.len());
        for f in list {
            let mut kind = f.kind.clone();
            kind.keep_only(&within);
            kind.remap(&map);
            let name = (1..).map(|k| if k == 1 { format!("{} copy", f.name) } else { format!("{} copy {k}", f.name) }).find(|n| !self.features.iter().any(|x| &x.name == n)).unwrap_or_else(|| f.name.clone());
            let id = map[&f.id];
            self.add_named(id, name, kind);
            if let Some(n) = self.feature_mut(id) {
                n.off = f.off;
            }
            out.push(id);
        }
        out
    }

    pub fn fresh_id(&self) -> u32 {
        self.features.iter().map(|f| f.id).max().unwrap_or(0) + 1
    }

    /// The first free `base 1`, `base 2`, …
    pub fn free_name(&self, base: &str) -> String {
        (1..).map(|k| format!("{base} {k}")).find(|n| !self.features.iter().any(|f| &f.name == n)).unwrap_or_else(|| base.to_string())
    }

    /// Adds a feature named after what it is, after the rollback point if there is one.
    pub fn add(&mut self, kind: Kind) -> u32 {
        let id = self.fresh_id();
        let name = self.free_name(kind.label());
        self.add_named(id, name, kind)
    }

    pub fn add_named(&mut self, id: u32, name: String, kind: Kind) -> u32 {
        let at = self.rollback.unwrap_or(self.features.len()).min(self.features.len());
        self.features.insert(at, Feature { id, name, off: false, kind });
        if let Some(r) = &mut self.rollback {
            *r += 1;
        }
        id
    }

    pub fn feature(&self, id: u32) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    pub fn feature_mut(&mut self, id: u32) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }

    pub fn sketch_mut(&mut self, id: u32) -> Option<&mut Sketch> {
        match &mut self.feature_mut(id)?.kind {
            Kind::Sketch { sketch, .. } => Some(sketch),
            _ => None,
        }
    }

    pub fn sketch(&self, id: u32) -> Option<&Sketch> {
        match &self.feature(id)?.kind {
            Kind::Sketch { sketch, .. } => Some(sketch),
            _ => None,
        }
    }

    /// Features that would break without this one.
    pub fn dependents(&self, id: u32) -> Vec<u32> {
        self.features.iter().filter(|f| f.kind.uses().contains(&id)).map(|f| f.id).collect()
    }

    pub fn remove(&mut self, id: u32) {
        if let Some(at) = self.features.iter().position(|f| f.id == id) {
            self.features.remove(at);
            if let Some(r) = &mut self.rollback
                && at < *r
            {
                *r -= 1;
            }
        }
    }

    /// The most recent sketch, which is what a new feature is usually about.
    pub fn last_sketch(&self) -> Option<u32> {
        self.features.iter().rev().find(|f| matches!(f.kind, Kind::Sketch { .. })).map(|f| f.id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Solid,
    Sheet,
    Wire,
}

/// One body as it stands.
#[derive(Clone, Debug)]
pub struct Body {
    pub id: u32,
    pub name: String,
    /// Empty: the project's metal.
    pub material: String,
    pub kind: BodyKind,
    pub mesh: Arc<Mesh>,
    /// The same shape in the solvers' own exact terms, while it still has one: a box stays a box.
    pub exact: Option<Vec<Shape>>,
    /// A wire's route, and its radius.
    pub wires: Vec<(Vec<P3>, f64)>,
    /// The cell or edge asked for on this body.
    pub cell: Option<f64>,
    /// The feature that last changed it.
    pub last: u32,
    /// Made by copying another: a pattern's or a mirror's. Not what "the latest body" means.
    pub copy: bool,
}

/// A sketch as it solved, where it lies, and what its curves come to.
#[derive(Clone, Debug)]
pub struct SolvedSketch {
    pub frame: Frame,
    pub solved: Arc<Solved>,
    pub profiles: Arc<Profiles>,
}

#[derive(Clone, Debug, Default)]
struct State {
    coords: Vec<CoordSys>,
    bodies: BTreeMap<u32, Arc<Body>>,
    sketches: BTreeMap<u32, SolvedSketch>,
    ports: Vec<(u32, PortDef)>,
    /// The ends of each sketch line as solved: `(sketch, curve)`, for a revolve about one.
    line_points: BTreeMap<(u32, u32), ([f64; 2], [f64; 2])>,
    errors: Vec<(u32, String)>,
}

/// What building a model gives.
#[derive(Clone, Debug, Default)]
pub struct Built {
    pub bodies: Vec<Arc<Body>>,
    pub sketches: BTreeMap<u32, SolvedSketch>,
    pub ports: Vec<PortDef>,
    /// Features that failed, by id, with why.
    pub errors: Vec<(u32, String)>,
}

fn three(q: &[String; 3], vars: &Vars) -> Result<P3> {
    Ok([length(&q[0], vars)?, length(&q[1], vars)?, length(&q[2], vars)?])
}

/// A direction: plain numbers here are just numbers, not millimetres.
fn direction(q: &[String; 3], vars: &Vars) -> Result<P3> {
    let v: Vec<f64> = q.iter().map(|s| expr::eval(s, vars)).collect::<Result<_>>()?;
    let d = [v[0], v[1], v[2]];
    if norm(d) == 0.0 {
        bail!("A direction can't be all zeros.");
    }
    Ok(unit(d))
}

fn count(s: &str, vars: &Vars) -> Result<usize> {
    let n = expr::eval(s, vars)?;
    if !(1.0..=2000.0).contains(&n) {
        bail!("A count has to be between 1 and 2000, and this comes to {n}.");
    }
    Ok(n.round() as usize)
}

impl PlaneRef {
    pub fn frame(&self, vars: &Vars, bodies: &[&Mesh]) -> Result<Frame> {
        match self {
            PlaneRef::Base { axis, offset } => {
                let d = length(offset, vars)?;
                Ok(match axis {
                    2 => Frame { o: [0.0, 0.0, d], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], n: [0.0, 0.0, 1.0] },
                    1 => Frame { o: [0.0, -d, 0.0], u: [1.0, 0.0, 0.0], v: [0.0, 0.0, 1.0], n: [0.0, -1.0, 0.0] },
                    _ => Frame { o: [d, 0.0, 0.0], u: [0.0, 1.0, 0.0], v: [0.0, 0.0, 1.0], n: [1.0, 0.0, 0.0] },
                })
            }
            PlaneRef::Face { face, offset } => {
                let d = length(offset, vars)?;
                for m in bodies {
                    if m.has_face(*face) {
                        let (o, n) = m.plane_of(*face, 1e-8).ok_or_else(|| anyhow!("The face it's on isn't flat any more."))?;
                        // Where the world's origin lands on the plane, so moving the face along itself
                        // leaves what's drawn on it alone.
                        let on = add(scale(n, dot(o, n) + d), [0.0; 3]);
                        return Ok(Frame::from_normal(on, n));
                    }
                }
                bail!("The face it was on is gone.")
            }
            PlaneRef::Custom { origin, normal } => Ok(Frame::from_normal(three(origin, vars)?, direction(normal, vars)?)),
            // Only the model knows its coordinate systems: `State::plane_frame` answers this one.
            PlaneRef::InCoord { .. } => bail!("This plane is in a coordinate system, which only the model can place."),
        }
    }
}

/// Whether a direction lies along a world axis, and which.
fn world_axis(d: P3) -> Option<usize> {
    (0..3).find(|k| d[*k].abs() > 1.0 - 1e-9)
}

/// A world point in the `(u, v)` the solvers use for a plane normal to `axis`.
fn solver_uv(axis: usize, p: P3) -> [f64; 2] {
    [p[(axis + 1) % 3], p[(axis + 2) % 3]]
}

/// A region in the solver's terms, if its plane is square to the world: one outline with its holes
/// joined in, thickness `thick` from `at`.
fn exact_polygon(region: &Region, f: &Frame, z0: f64, thick: f64) -> Option<Shape> {
    let axis = world_axis(f.n)?;
    let ring = |r: &Ring| -> Vec<[f64; 2]> { r.pts.iter().map(|q| solver_uv(axis, f.at(*q, z0))).collect() };
    let mut pts = ring(&region.outer);
    for h in &region.holes {
        pts = crate::cad::cut_hole(&pts, &ring(h));
    }
    let lo = f.at(region.outer.pts[0], z0)[axis].min(f.at(region.outer.pts[0], z0 + thick)[axis]);
    Some(Shape::Polygon { axis, at: lo, pts, thick })
}

fn translate_shape(s: &Shape, d: P3) -> Shape {
    let t = |p: &P3| add(*p, d);
    match s {
        Shape::Box { min, max } => Shape::Box { min: t(min), max: t(max) },
        Shape::Cylinder { a, b, r } => Shape::Cylinder { a: t(a), b: t(b), r: *r },
        Shape::Sphere { c, r } => Shape::Sphere { c: t(c), r: *r },
        Shape::Polygon { axis, at, pts, thick } => {
            let (u, v) = ((*axis + 1) % 3, (*axis + 2) % 3);
            Shape::Polygon { axis: *axis, at: at + d[*axis], pts: pts.iter().map(|q| [q[0] + d[u], q[1] + d[v]]).collect(), thick: *thick }
        }
        Shape::Wire { pts, r } => Shape::Wire { pts: pts.iter().map(t).collect(), r: *r },
        Shape::Tris { tris, solid, sheet } => Shape::Tris { tris: tris.iter().map(|q| q.map(|p| add(p, d))).collect(), solid: *solid, sheet: *sheet },
    }
}

fn is_translation(x: &Xform) -> bool {
    (0..3).all(|i| (0..3).all(|j| (x.m[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-12))
}

/// The same surface with every face renamed for this copy, keeping them apart. Faces are numbered
/// by their names' order, so a copy's face stays the same one while the original's faces do.
fn renamed_faces(m: &Mesh, copy: u32) -> Mesh {
    let mut names = m.face_ids();
    names.sort_unstable();
    let mut out = m.clone();
    for f in &mut out.face {
        let k = names.binary_search(f).unwrap_or(0) as u32;
        *f = face_tag(copy, k);
    }
    out
}

fn moved(b: &Body, x: &Xform, id: u32, name: String, last: u32) -> Body {
    let exact = if is_translation(x) { b.exact.as_ref().map(|l| l.iter().map(|s| translate_shape(s, x.t)).collect()) } else { None };
    let copy = id != b.id;
    let mesh = b.mesh.transformed(x);
    Body {
        id,
        name,
        material: b.material.clone(),
        kind: b.kind,
        mesh: Arc::new(if copy { renamed_faces(&mesh, id) } else { mesh }),
        exact,
        wires: b.wires.iter().map(|(pts, r)| (pts.iter().map(|p| x.apply(*p)).collect(), *r)).collect(),
        cell: b.cell,
        last,
        copy: copy || b.copy,
    }
}

/// Faces a solid outward and checks it closes, or says what's wrong.
fn finish_solid(m: &Mesh, what: &str) -> Result<Mesh> {
    let w = m.welded(WELD);
    if w.is_empty() {
        bail!("{what} made nothing: its profile may have no area, or its depth may be zero.");
    }
    if !w.is_closed() {
        bail!("{what} doesn't close into a solid ({} open edges). Use a surface, or check the profile.", w.open_edges());
    }
    Ok(if w.volume() < 0.0 { w.flipped() } else { w })
}

fn output_cell(out: &Output, vars: &Vars) -> Result<Option<f64>> {
    if out.mesh.trim().is_empty() {
        return Ok(None);
    }
    let c = length(&out.mesh, vars).context("the mesh size")?;
    if !(c > 0.0) {
        bail!("A mesh size has to be above zero.");
    }
    Ok(Some(c))
}

impl State {
    fn meshes(&self) -> Vec<&Mesh> {
        self.bodies.values().map(|b| b.mesh.as_ref()).collect()
    }

    fn sketch(&self, id: u32) -> Result<&SolvedSketch> {
        self.sketches.get(&id).ok_or_else(|| anyhow!("Its sketch is gone or didn't solve."))
    }

    /// Everything's overall size, for "through all" and the like.
    fn reach(&self) -> f64 {
        let mut out: f64 = 1e-3;
        for b in self.bodies.values() {
            if !b.mesh.is_empty() {
                let (lo, hi) = b.mesh.bounds();
                out = out.max(norm(lo)).max(norm(hi)).max(norm(sub(hi, lo)));
            }
        }
        out
    }

    /// The new shape goes where the output says: its own body, or into another.
    fn deliver(&mut self, f: &Feature, out: &Output, mesh: Mesh, kind: BodyKind, exact: Option<Vec<Shape>>, wires: Vec<(Vec<P3>, f64)>, vars: &Vars) -> Result<()> {
        let cell = output_cell(out, vars)?;
        let (mesh, exact, wires) = match out.coord {
            Some(id) => {
                let cs = self.coords.iter().find(|c| c.id == id).ok_or_else(|| anyhow!("The coordinate system it's drawn in is gone."))?;
                let x = cs.xform(vars).with_context(|| format!("the coordinate system {}", cs.name))?;
                // A turn leaves nothing square to the world, so only a shift keeps the exact shape.
                let exact = if is_translation(&x) { exact.map(|l| l.iter().map(|s| translate_shape(s, x.t)).collect()) } else { None };
                let wires = wires.iter().map(|(pts, r)| (pts.iter().map(|p| x.apply(*p)).collect(), *r)).collect();
                (mesh.transformed(&x), exact, wires)
            }
            None => (mesh, exact, wires),
        };
        if out.mode == Mode::New {
            self.bodies.insert(f.id, Arc::new(Body { id: f.id, name: f.name.clone(), material: out.material.clone(), kind, mesh: Arc::new(mesh), exact, wires, cell, last: f.id, copy: false }));
            return Ok(());
        }
        let target = match out.target {
            Some(t) => t,
            None => {
                let can = |b: &Body| b.kind != BodyKind::Wire;
                let latest = self.bodies.iter().rev().find(|(_, b)| can(b) && !b.copy).or_else(|| self.bodies.iter().rev().find(|(_, b)| can(b)));
                latest.map(|(id, _)| *id).ok_or_else(|| anyhow!("There's no body yet for this to change."))?
            }
        };
        let body = self.bodies.get(&target).ok_or_else(|| anyhow!("The body it changes is gone."))?.clone();
        let op = match out.mode {
            Mode::Add => Op::Union,
            Mode::Cut => Op::Difference,
            _ => Op::Intersection,
        };
        let result = boolean::boolean(&body.mesh, &mesh, op)?;
        if result.is_empty() {
            bail!("Nothing would be left of {}.", body.name);
        }
        let mut changed = (*body).clone();
        changed.mesh = Arc::new(result);
        changed.exact = None;
        changed.last = f.id;
        if let Some(c) = cell {
            changed.cell = Some(c);
        }
        self.bodies.insert(target, Arc::new(changed));
        Ok(())
    }

    fn step(&mut self, f: &Feature, vars: &Vars, facets: usize) -> Result<()> {
        match &f.kind {
            Kind::Sketch { plane, sketch } => {
                let frame = self.plane_frame(plane, vars)?;
                let solved = sketch.solve(vars);
                let profiles = prims::profiles(sketch, &solved, vars, facets);
                let (bad, broken) = (solved.conflicts.len(), solved.broken.first().map(|b| b.1.clone()));
                self.sketches.insert(f.id, SolvedSketch { frame, solved: Arc::new(solved), profiles: Arc::new(profiles) });
                if let Some(why) = broken {
                    bail!("{why}");
                }
                if bad > 0 {
                    bail!("{bad} of its constraints can't hold together with the others.");
                }
                Ok(())
            }
            Kind::Extrude { sketch, depth, extent, out } => {
                let sk = self.sketch(*sketch)?.clone();
                check_profile(&sk.profiles)?;
                let d = match extent {
                    Extent::Through => self.reach() * 4.0,
                    _ => length(depth, vars).context("the depth")?,
                };
                if d == 0.0 {
                    bail!("An extrusion needs a depth.");
                }
                let (z0, z1) = match extent {
                    Extent::One => (d.min(0.0), d.max(0.0)),
                    Extent::Reverse => ((-d).min(0.0), (-d).max(0.0)),
                    Extent::Both | Extent::Through => (-d.abs() / 2.0, d.abs() / 2.0),
                };
                if out.surface {
                    let mut m = Mesh::default();
                    for r in &sk.profiles.regions {
                        for ring in r.rings() {
                            m.append(&prims::extrude_sheet(ring, true, &sk.frame, z0, z1, f.id));
                        }
                    }
                    for chain in &sk.profiles.open {
                        m.append(&prims::extrude_sheet(chain, false, &sk.frame, z0, z1, f.id));
                    }
                    if m.is_empty() {
                        bail!("Its sketch has nothing to extrude.");
                    }
                    return self.deliver(f, out, m.welded(WELD), BodyKind::Sheet, None, Vec::new(), vars);
                }
                if sk.profiles.regions.is_empty() {
                    bail!("A solid extrusion needs a closed sketch. For an open one, tick Surface.");
                }
                let mut m = Mesh::default();
                let mut exact = Some(Vec::new());
                for r in &sk.profiles.regions {
                    m.append(&prims::extrude(r, &sk.frame, z0, z1, f.id));
                    exact = exact.and_then(|mut l: Vec<Shape>| {
                        l.push(exact_polygon(r, &sk.frame, z0, z1 - z0)?);
                        Some(l)
                    });
                }
                self.deliver(f, out, finish_solid(&m, "This extrusion")?, BodyKind::Solid, exact, Vec::new(), vars)
            }
            Kind::Sheet { sketch, out } => {
                let sk = self.sketch(*sketch)?.clone();
                check_profile(&sk.profiles)?;
                if sk.profiles.regions.is_empty() {
                    bail!("A sheet fills a closed sketch, and this one has no closed loop.");
                }
                self.flat_body(f, &sk.profiles.regions, &sk.frame, out, vars)
            }
            Kind::Face { plane, shape, out } => {
                let frame = self.plane_frame(plane, vars)?;
                let region = shape.region(vars, facets)?;
                self.flat_body(f, std::slice::from_ref(&region), &frame, out, vars)
            }
            Kind::Revolve { sketch, axis, angle: a, out } => self.revolve(f, *sketch, axis, a, out, vars, facets),
            Kind::Sweep { sketch, path, out } => {
                let sk = self.sketch(*sketch)?.clone();
                check_profile(&sk.profiles)?;
                let routes = self.route(path, vars, facets)?;
                let Some((route, closed)) = routes.first() else { bail!("The path has nothing in it.") };
                if route.len() < 2 {
                    bail!("A path needs two different points.");
                }
                let start = unit(sub(route[1], route[0]));
                if dot(start, sk.frame.n).abs() < 0.1 {
                    bail!("The path has to leave the profile's plane, and it starts along it.");
                }
                if out.surface || sk.profiles.regions.is_empty() {
                    let Some(chain) = sk.profiles.open.first().or_else(|| sk.profiles.regions.first().map(|r| &r.outer)) else { bail!("Its sketch has nothing to sweep.") };
                    let ring_closed = sk.profiles.open.is_empty();
                    let m = prims::sweep(None, chain, ring_closed, route, *closed, &sk.frame, f.id);
                    return self.deliver(f, out, m.welded(WELD), BodyKind::Sheet, None, Vec::new(), vars);
                }
                let mut m = Mesh::default();
                for r in &sk.profiles.regions {
                    m.append(&prims::sweep(Some(r), &r.outer, true, route, *closed, &sk.frame, f.id));
                    for h in &r.holes {
                        m.append(&prims::sweep(None, h, true, route, *closed, &sk.frame, f.id));
                    }
                }
                self.deliver(f, out, finish_solid(&m, "This sweep")?, BodyKind::Solid, None, Vec::new(), vars)
            }
            Kind::Loft { sections, out } => {
                let mut list = Vec::with_capacity(sections.len());
                for s in sections {
                    let sk = self.sketch(*s)?;
                    check_profile(&sk.profiles)?;
                    let Some(r) = sk.profiles.regions.first() else { bail!("Each section of a loft has to be a closed loop.") };
                    list.push((sk.frame, r.clone()));
                }
                if out.surface {
                    let m = prims::loft(&list, false, f.id)?;
                    return self.deliver(f, out, m.welded(WELD), BodyKind::Sheet, None, Vec::new(), vars);
                }
                let m = prims::loft(&list, true, f.id)?;
                self.deliver(f, out, finish_solid(&m, "This loft")?, BodyKind::Solid, None, Vec::new(), vars)
            }
            Kind::Solid { shape, out } => {
                let n = facets;
                let (m, exact) = match shape {
                    Prim::Box { .. } | Prim::Cuboid { .. } => {
                        let (a, b) = match shape {
                            Prim::Box { min, max } => (three(min, vars)?, three(max, vars)?),
                            Prim::Cuboid { p0, size } => {
                                let a = three(p0, vars).context("its corner")?;
                                (a, add(a, three(size, vars).context("its size")?))
                            }
                            _ => unreachable!(),
                        };
                        let lo: P3 = std::array::from_fn(|k| a[k].min(b[k]));
                        let hi: P3 = std::array::from_fn(|k| a[k].max(b[k]));
                        if (0..3).any(|k| hi[k] - lo[k] <= 0.0) {
                            bail!("A box needs some size along all three axes. For something flat, use a sheet.");
                        }
                        (prims::cuboid(lo, hi, f.id), Some(vec![Shape::Box { min: lo, max: hi }]))
                    }
                    Prim::Cylinder { a, b, r } => {
                        let (a, b, r) = (three(a, vars)?, three(b, vars)?, length(r, vars)?);
                        if !(r > 0.0) || norm(sub(b, a)) <= 0.0 {
                            bail!("A cylinder needs a radius and two different ends.");
                        }
                        (prims::cone(a, b, r, r, n, f.id), Some(vec![Shape::Cylinder { a, b, r }]))
                    }
                    Prim::Cone { a, b, r0, r1 } => {
                        let (a, b, r0, r1) = (three(a, vars)?, three(b, vars)?, length(r0, vars)?, length(r1, vars)?);
                        if r0 < 0.0 || r1 < 0.0 || (r0 == 0.0 && r1 == 0.0) || norm(sub(b, a)) <= 0.0 {
                            bail!("A cone needs a radius at one end at least, and two different ends.");
                        }
                        (prims::cone(a, b, r0, r1, n, f.id), None)
                    }
                    Prim::Sphere { c, r } => {
                        let (c, r) = (three(c, vars)?, length(r, vars)?);
                        if !(r > 0.0) {
                            bail!("A sphere needs a radius above zero.");
                        }
                        (prims::sphere(c, r, n, f.id), Some(vec![Shape::Sphere { c, r }]))
                    }
                    Prim::Torus { c, axis, big, small } => {
                        let (c, a, big, small) = (three(c, vars)?, direction(axis, vars)?, length(big, vars)?, length(small, vars)?);
                        if !(big > small && small > 0.0) {
                            bail!("A torus needs its ring radius bigger than its tube radius.");
                        }
                        (prims::torus(c, a, big, small, n, f.id), None)
                    }
                };
                let m = finish_solid(&m, "This solid")?;
                self.deliver(f, out, m, BodyKind::Solid, exact, Vec::new(), vars)
            }
            Kind::Wire { path, radius, out } => {
                let r = length(radius, vars).context("the radius")?;
                if !(r > 0.0) {
                    bail!("A wire needs a radius above zero.");
                }
                let routes = self.route(path, vars, facets)?;
                if routes.is_empty() {
                    bail!("The path has nothing in it.");
                }
                let mut wires = Vec::with_capacity(routes.len());
                for (pts, closed) in routes {
                    let mut pts = pts;
                    if closed {
                        pts.push(pts[0]);
                    }
                    if pts.len() < 2 {
                        bail!("A wire needs two different points.");
                    }
                    wires.push((pts, r));
                }
                let mesh = wire_tubes(&wires, f.id);
                let mut single = out.clone();
                single.mode = Mode::New;
                self.deliver(f, &single, mesh, BodyKind::Wire, None, wires, vars)
            }
            Kind::Combine { op, target, tools, keep_tools } => {
                let mut result = (**self.body(*target)?).clone();
                if tools.is_empty() {
                    bail!("Pick at least one body to combine it with.");
                }
                if result.kind == BodyKind::Wire || tools.iter().any(|t| self.bodies.get(t).is_some_and(|b| b.kind == BodyKind::Wire)) {
                    bail!("A wire is a route, not a surface, so it can't be combined. Join metal to it with a feed or by touching it, and use FDTD.");
                }
                let mut mesh = (*result.mesh).clone();
                for t in tools {
                    let tool = self.body(*t)?;
                    let o = match op {
                        CombineOp::Add => Op::Union,
                        CombineOp::Cut => Op::Difference,
                        CombineOp::Common => Op::Intersection,
                        CombineOp::Exclude => Op::Exclusion,
                    };
                    mesh = boolean::boolean(&mesh, &tool.mesh, o)?;
                    if mesh.is_empty() {
                        bail!("Nothing would be left of {}.", result.name);
                    }
                }
                result.mesh = Arc::new(mesh);
                result.exact = None;
                result.last = f.id;
                if !*keep_tools {
                    for t in tools {
                        self.bodies.remove(t);
                    }
                }
                self.bodies.insert(*target, Arc::new(result));
                Ok(())
            }
            Kind::Pattern { bodies, layout, merge, feeds } => self.pattern(f, bodies, layout, *merge, feeds, vars),
            Kind::Mirror { bodies, plane, merge } => {
                let frame = self.plane_frame(plane, vars)?;
                let x = Xform::mirror(frame.o, frame.n);
                for (i, id) in bodies.iter().enumerate() {
                    let b = self.body(*id)?.clone();
                    if *merge {
                        let mut changed = (*b).clone();
                        let new = f.id * 4096 + i as u32 + 1;
                        let copy = renamed_faces(&b.mesh.transformed(&x), new);
                        let mesh = if b.kind == BodyKind::Wire {
                            changed.wires.extend(b.wires.iter().map(|(pts, r)| (pts.iter().map(|p| x.apply(*p)).collect(), *r)));
                            let mut m = (*b.mesh).clone();
                            m.append(&copy);
                            m
                        } else {
                            boolean::boolean(&b.mesh, &copy, Op::Union)?
                        };
                        changed.mesh = Arc::new(mesh);
                        changed.exact = None;
                        changed.last = f.id;
                        self.bodies.insert(*id, Arc::new(changed));
                    } else {
                        let new = f.id * 4096 + i as u32 + 1;
                        self.bodies.insert(new, Arc::new(moved(&b, &x, new, format!("{} (mirror)", b.name), f.id)));
                    }
                }
                Ok(())
            }
            Kind::Move { bodies, by, origin, axis, angle: a, copy } => {
                let d = three(by, vars).context("the move")?;
                let mut x = Xform::translate(d);
                if let Some(ax) = axis {
                    let o = origin.as_ref().map_or(Ok([0.0; 3]), |o| three(o, vars))?;
                    let turn = Xform::rotate(o, direction(ax, vars)?, angle(a, vars).context("the angle")?);
                    x = Xform { m: turn.m, t: add(turn.t, d) };
                }
                for (i, id) in bodies.iter().enumerate() {
                    let b = self.body(*id)?.clone();
                    if *copy {
                        let new = f.id * 4096 + i as u32 + 1;
                        self.bodies.insert(new, Arc::new(moved(&b, &x, new, format!("{} (copy)", b.name), f.id)));
                    } else {
                        self.bodies.insert(*id, Arc::new(moved(&b, &x, *id, b.name.clone(), f.id)));
                    }
                }
                Ok(())
            }
            Kind::Fillet { body, edges, radius, chamfer } => {
                let b = self.body(*body)?.clone();
                if b.kind != BodyKind::Solid {
                    bail!("Only a solid's edges can be rounded here: a surface has no inside for the tool to work against. Round the corner in its sketch instead.");
                }
                let r = length(radius, vars).context("the radius")?;
                if !(r > 0.0) {
                    bail!("A fillet needs a radius above zero.");
                }
                if edges.is_empty() {
                    bail!("Pick at least one edge.");
                }
                let mut mesh = (*b.mesh).clone();
                for (k, e) in edges.iter().enumerate() {
                    mesh = blend::blend(&mesh, *e, r, *chamfer, facets, face_tag(f.id, 2 + k as u32))?;
                }
                let mut changed = (*b).clone();
                changed.mesh = Arc::new(mesh);
                changed.exact = None;
                changed.last = f.id;
                self.bodies.insert(*body, Arc::new(changed));
                Ok(())
            }
            Kind::Shell { body, open, thick } => {
                let b = self.body(*body)?.clone();
                if b.kind != BodyKind::Solid {
                    bail!("Only a solid can be hollowed.");
                }
                if let Some(gone) = open.iter().find(|f| !b.mesh.has_face(**f)) {
                    bail!("A face it leaves open isn't on this body any more (face {gone}).");
                }
                let t = length(thick, vars).context("the thickness")?;
                let mut changed = (*b).clone();
                if t <= 0.0 {
                    let mut m = Mesh { verts: b.mesh.verts.clone(), ..Default::default() };
                    for (i, tri) in b.mesh.tris.iter().enumerate() {
                        if !open.contains(&b.mesh.face[i]) {
                            m.tris.push(*tri);
                            m.face.push(b.mesh.face[i]);
                        }
                    }
                    changed.mesh = Arc::new(m.compacted());
                    changed.kind = BodyKind::Sheet;
                } else {
                    changed.mesh = Arc::new(blend::shell(&b.mesh, open, t)?);
                }
                changed.exact = None;
                changed.last = f.id;
                self.bodies.insert(*body, Arc::new(changed));
                Ok(())
            }
            Kind::Thicken { body, thick } => {
                let b = self.body(*body)?.clone();
                if b.kind != BodyKind::Sheet {
                    bail!("Only a surface can be thickened.");
                }
                let t = length(thick, vars).context("the thickness")?;
                if !(t > 0.0) {
                    bail!("A thickness has to be above zero.");
                }
                let mut changed = (*b).clone();
                changed.mesh = Arc::new(finish_solid(&blend::thicken(&b.mesh, t, f.id), "This thickening")?);
                changed.kind = BodyKind::Solid;
                changed.exact = None;
                changed.last = f.id;
                self.bodies.insert(*body, Arc::new(changed));
                Ok(())
            }
            Kind::Import { file, tris, out } => {
                let mut m = Mesh::default();
                let face = face_tag(f.id, 0);
                for t in tris {
                    m.push_tri(t[0], t[1], t[2], face);
                }
                let w = m.welded(WELD);
                if w.is_empty() {
                    bail!("{file} has no triangles with any area.");
                }
                let (kind, mesh) = if w.is_closed() { (BodyKind::Solid, if w.volume() < 0.0 { w.flipped() } else { w }) } else { (BodyKind::Sheet, w) };
                self.deliver(f, out, mesh, kind, None, Vec::new(), vars)
            }
            Kind::Feed { a, b, width, z0 } => {
                let pa = self.anchor(a, vars)?;
                let pb = self.anchor(b, vars)?;
                let w = if width.trim().is_empty() { 0.0 } else { length(width, vars).context("the width")? };
                let z = expr::eval(z0, vars).context("the impedance")?;
                let mut port = PortDef { name: f.name.clone(), a: pa, b: pb, width: w, z0: z };
                snap_to_axis(&mut port);
                self.ports.push((f.id, port));
                Ok(())
            }
            Kind::Probe { sketch, point, width, z0 } => {
                let sk = self.sketch(*sketch)?.clone();
                let at = sk.frame.at(point_of(&sk, *point)?, 0.0);
                // Each stretch of the line that's in metal. A sheet is a stretch of no length, and a
                // solid is one from each face it enters to the one it leaves by.
                let mut spans: Vec<(f64, f64)> = Vec::new();
                for b in self.bodies.values().filter(|b| is_metal(&b.material)) {
                    let m = &b.mesh;
                    let mut hits: Vec<f64> = (0..m.tris.len()).filter_map(|t| line_hits_triangle(at, sk.frame.n, m.corners(t))).collect();
                    hits.sort_by(f64::total_cmp);
                    hits.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
                    if b.kind == BodyKind::Solid {
                        spans.extend(hits.chunks(2).map(|c| (c[0], *c.last().unwrap_or(&c[0]))));
                    } else {
                        spans.extend(hits.iter().map(|h| (*h, *h)));
                    }
                }
                if spans.is_empty() {
                    bail!("A probe joins two sheets of metal, and this point has none over or under it.");
                }
                let (lo, hi) = probe_span(&spans).ok_or_else(|| anyhow!("A probe joins two sheets of metal, and this point has metal at only one height."))?;
                let w = if width.trim().is_empty() { 0.0 } else { length(width, vars).context("the width")? };
                let z = expr::eval(z0, vars).context("the impedance")?;
                let mut port = PortDef { name: f.name.clone(), a: add(at, scale(sk.frame.n, lo)), b: add(at, scale(sk.frame.n, hi)), width: w, z0: z };
                snap_to_axis(&mut port);
                self.ports.push((f.id, port));
                Ok(())
            }
        }
    }

    fn body(&self, id: u32) -> Result<&Arc<Body>> {
        self.bodies.get(&id).ok_or_else(|| anyhow!("A body it uses is gone."))
    }

    /// Where a plane is, including one in a coordinate system.
    fn plane_frame(&self, plane: &PlaneRef, vars: &Vars) -> Result<Frame> {
        if let PlaneRef::InCoord { coord, axis, offset } = plane {
            let cs = self.coords.iter().find(|c| c.id == *coord).ok_or_else(|| anyhow!("The coordinate system it's drawn in is gone."))?;
            let x = cs.xform(vars).with_context(|| format!("the coordinate system {}", cs.name))?;
            let f = PlaneRef::Base { axis: *axis, offset: offset.clone() }.frame(vars, &[])?;
            return Ok(Frame { o: x.apply(f.o), u: x.apply_dir(f.u), v: x.apply_dir(f.v), n: x.apply_dir(f.n) });
        }
        plane.frame(vars, &self.meshes())
    }

    /// Flat metal: a surface, or with a thickness a slab. The slab goes up the world's axis from the
    /// plane, the way a box's height does, even on a plane whose normal points down it, so the plane
    /// stays its lowest face. That's where the method of moments meshes the sheet it keeps, and a
    /// time-domain solver gets the real slab.
    fn flat_body(&mut self, f: &Feature, regions: &[Region], frame: &Frame, out: &Output, vars: &Vars) -> Result<()> {
        let t = if out.thick.trim().is_empty() { 0.0 } else { length(&out.thick, vars).context("the thickness")? };
        if t < 0.0 {
            bail!("A thickness can't be negative. To have it go the other way, move the plane.");
        }
        let downward = world_axis(frame.n).is_some_and(|k| frame.n[k] < 0.0);
        let (z0, z1) = if downward { (-t, 0.0) } else { (0.0, t) };
        let mut m = Mesh::default();
        let mut exact = Some(Vec::new());
        for r in regions {
            if t == 0.0 {
                m.append(&prims::sheet(r, frame, f.id));
            } else {
                m.append(&prims::extrude(r, frame, z0, z1, f.id));
            }
            exact = exact.and_then(|mut l: Vec<Shape>| {
                l.push(exact_polygon(r, frame, z0, t)?);
                Some(l)
            });
        }
        if t == 0.0 {
            self.deliver(f, out, m.welded(WELD), BodyKind::Sheet, exact, Vec::new(), vars)
        } else {
            self.deliver(f, out, finish_solid(&m, "This sheet")?, BodyKind::Solid, exact, Vec::new(), vars)
        }
    }

    fn anchor(&self, a: &Anchor, vars: &Vars) -> Result<P3> {
        match a {
            Anchor::Xyz { at } => three(at, vars),
            Anchor::Sketch { sketch, point, height } => {
                let sk = self.sketch(*sketch)?;
                let h = if height.trim().is_empty() { 0.0 } else { length(height, vars)? };
                Ok(sk.frame.at(point_of(sk, *point)?, h))
            }
            Anchor::Offset { sketch, point, by } => {
                let sk = self.sketch(*sketch)?;
                Ok(add(sk.frame.at(point_of(sk, *point)?, 0.0), three(by, vars)?))
            }
        }
    }

    fn route(&self, path: &PathRef, vars: &Vars, facets: usize) -> Result<Vec<(Vec<P3>, bool)>> {
        match path {
            PathRef::Sketch { sketch } => {
                let sk = self.sketch(*sketch)?;
                let mut out = Vec::new();
                for chain in &sk.profiles.open {
                    out.push((chain.pts.iter().map(|q| sk.frame.at(*q, 0.0)).collect(), false));
                }
                for r in &sk.profiles.regions {
                    for ring in r.rings() {
                        out.push((ring.pts.iter().map(|q| sk.frame.at(*q, 0.0)).collect(), true));
                    }
                }
                Ok(out)
            }
            PathRef::Helix { origin, axis, radius, radius2, pitch, turns, left } => {
                let (o, a, r) = (three(origin, vars)?, direction(axis, vars)?, length(radius, vars).context("the radius")?);
                let r2 = if radius2.trim().is_empty() { r } else { length(radius2, vars).context("the far radius")? };
                let (p, t) = (length(pitch, vars).context("the pitch")?, expr::eval(turns, vars).context("the number of turns")?);
                if !(r > 0.0 && r2 >= 0.0 && p != 0.0 && t > 0.0 && t <= 500.0) {
                    bail!("A helix needs a radius, a pitch and up to 500 turns.");
                }
                Ok(vec![(prims::helix_points(o, a, r, r2, p, t, *left, facets), false)])
            }
            PathRef::Points { pts } => {
                let list: Vec<P3> = pts.iter().map(|q| three(q, vars)).collect::<Result<_>>()?;
                Ok(vec![(list, false)])
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn revolve(&mut self, f: &Feature, sketch: u32, axis: &AxisRef, a: &str, out: &Output, vars: &Vars, facets: usize) -> Result<()> {
        let sk = self.sketch(sketch)?.clone();
        check_profile(&sk.profiles)?;
        let (at, dir) = match axis {
            AxisRef::U => ([0.0, 0.0], [1.0, 0.0]),
            AxisRef::V => ([0.0, 0.0], [0.0, 1.0]),
            AxisRef::Line { curve } => {
                // Which line it was isn't in the solved profile, so its ends come from the tree.
                let (p, q) = self.line_points.get(&(sketch, *curve)).copied().ok_or_else(|| anyhow!("The line it turns about is gone."))?;
                (p, [q[0] - p[0], q[1] - p[1]])
            }
        };
        let n = dir[0].hypot(dir[1]);
        if n <= 0.0 {
            bail!("The axis needs two different points.");
        }
        let dir = [dir[0] / n, dir[1] / n];
        let turn = angle(a, vars).context("the angle")?.abs();
        if !(turn > 0.0) {
            bail!("A revolve needs an angle.");
        }
        let turn = turn.min(std::f64::consts::TAU);
        let full = turn >= std::f64::consts::TAU - 1e-9;
        // Which side of the axis each point is on: a profile across it would turn through itself.
        let off = |q: [f64; 2]| (q[0] - at[0]) * dir[1] - (q[1] - at[1]) * dir[0];
        let mut sides = [false, false];
        let mut note = |pts: &[[f64; 2]]| {
            for q in pts {
                let d = off(*q);
                if d > 1e-9 {
                    sides[0] = true;
                }
                if d < -1e-9 {
                    sides[1] = true;
                }
            }
        };
        for r in &sk.profiles.regions {
            for ring in r.rings() {
                note(&ring.pts);
            }
        }
        for c in &sk.profiles.open {
            note(&c.pts);
        }
        if sides[0] && sides[1] {
            bail!("The profile lies on both sides of its axis, so it would turn through itself.");
        }
        let solid_wanted = !out.surface && !sk.profiles.regions.is_empty();
        if solid_wanted {
            let mut rings: Vec<(&Ring, bool)> = Vec::new();
            for r in &sk.profiles.regions {
                rings.extend(r.rings().map(|x| (x, true)));
            }
            let mut m = prims::revolve(&rings, &sk.frame, at, dir, turn, facets, f.id);
            if !full {
                for r in &sk.profiles.regions {
                    prims::revolve_caps(r, &sk.frame, at, dir, turn, f.id, &mut m);
                }
            }
            return self.deliver(f, out, finish_solid(&m, "This revolve")?, BodyKind::Solid, None, Vec::new(), vars);
        }
        let mut rings: Vec<(&Ring, bool)> = sk.profiles.open.iter().map(|c| (c, false)).collect();
        for r in &sk.profiles.regions {
            rings.extend(r.rings().map(|x| (x, true)));
        }
        if rings.is_empty() {
            bail!("Its sketch has nothing to revolve.");
        }
        let m = prims::revolve(&rings, &sk.frame, at, dir, turn, facets, f.id).welded(WELD);
        // Both ends on the axis closes it, like a dome shut at the top and bottom.
        if !out.surface && m.is_closed() && full {
            let m = if m.volume() < 0.0 { m.flipped() } else { m };
            return self.deliver(f, out, m, BodyKind::Solid, None, Vec::new(), vars);
        }
        self.deliver(f, out, m, BodyKind::Sheet, None, Vec::new(), vars)
    }

    fn pattern(&mut self, f: &Feature, bodies: &[u32], layout: &Layout, merge: bool, feeds: &[u32], vars: &Vars) -> Result<()> {
        // Every copy after the original, as a move.
        let mut moves: Vec<Xform> = Vec::new();
        match layout {
            Layout::Linear { step, count: n, step2, count2 } => {
                let s = three(step, vars).context("the step")?;
                let k = count(n, vars).context("the count")?;
                let (s2, k2) = match step2 {
                    Some(q) => (three(q, vars)?, count(count2, vars)?),
                    None => ([0.0; 3], 1),
                };
                if k * k2 > 2000 {
                    bail!("A pattern of more than 2000 copies isn't something a solver can take.");
                }
                for i in 0..k {
                    for j in 0..k2 {
                        if i + j > 0 {
                            moves.push(Xform::translate(add(scale(s, i as f64), scale(s2, j as f64))));
                        }
                    }
                }
            }
            Layout::Circular { origin, axis, count: n, total } => {
                let (o, a) = (three(origin, vars)?, direction(axis, vars)?);
                let k = count(n, vars).context("the count")?;
                let t = angle(total, vars).context("the angle")?;
                // A full circle has no copy on top of the first.
                let share = if (t.abs() - std::f64::consts::TAU).abs() < 1e-9 { t / k as f64 } else { t / (k.max(2) - 1) as f64 };
                for i in 1..k {
                    moves.push(Xform::rotate(o, a, share * i as f64));
                }
            }
        }
        for (bi, id) in bodies.iter().enumerate() {
            let b = self.body(*id)?.clone();
            if merge {
                let mut mesh = (*b.mesh).clone();
                let mut changed = (*b).clone();
                for (i, x) in moves.iter().enumerate() {
                    let new = f.id * 4096 + (bi * moves.len() + i) as u32 + 1;
                    let copy = renamed_faces(&b.mesh.transformed(x), new);
                    if b.kind == BodyKind::Wire {
                        changed.wires.extend(b.wires.iter().map(|(pts, r)| (pts.iter().map(|p| x.apply(*p)).collect(), *r)));
                        mesh.append(&copy);
                    } else {
                        mesh = boolean::boolean(&mesh, &copy, Op::Union)?;
                    }
                }
                changed.mesh = Arc::new(mesh);
                changed.exact = None;
                changed.last = f.id;
                self.bodies.insert(*id, Arc::new(changed));
            } else {
                for (i, x) in moves.iter().enumerate() {
                    let new = f.id * 4096 + (bi * moves.len() + i) as u32 + 1;
                    self.bodies.insert(new, Arc::new(moved(&b, x, new, format!("{} [{}]", b.name, i + 2), f.id)));
                }
            }
        }
        for id in feeds {
            let Some(port) = self.ports.iter().find(|(from, _)| from == id).map(|(_, p)| p.clone()) else { bail!("A feed it copies is gone or comes later.") };
            for (i, x) in moves.iter().enumerate() {
                self.ports.push((f.id, PortDef { name: format!("{} [{}]", port.name, i + 2), a: x.apply(port.a), b: x.apply(port.b), width: port.width, z0: port.z0 }));
            }
        }
        Ok(())
    }
}

/// Where a sketch point is. One that's been deleted is an error: a feed quietly landing on the origin
/// would still solve, and give an answer for the wrong antenna.
fn point_of(sk: &SolvedSketch, point: u32) -> Result<[f64; 2]> {
    sk.solved.points.get(&point).copied().ok_or_else(|| anyhow!("The sketch point it's on is gone."))
}

fn check_profile(p: &Profiles) -> Result<()> {
    if let Some(why) = &p.problem {
        bail!("{why}");
    }
    Ok(())
}

/// Rounding from a tilted frame can leave a port a hair off its axis, which the check refuses.
fn snap_to_axis(p: &mut PortDef) {
    let d = sub(p.b, p.a);
    let big = (0..3).map(|k| d[k].abs()).fold(0.0, f64::max);
    for k in 0..3 {
        if d[k].abs() < big * 1e-9 {
            p.b[k] = p.a[k];
        }
    }
}

fn is_metal(material: &str) -> bool {
    material.is_empty() || material::by_name(material).is_some_and(|m| m.is_conductor())
}

/// Which two of the metal surfaces along a sketch's normal a probe joins, as distances from the plane.
/// From the metal on the plane itself to the nearest below it, or above if there's none below. Not
/// first to last: in a stacked patch that would run straight through the driven patch to the
/// parasitic one. With no metal on the plane, the nearest either side of it.
fn probe_span(spans: &[(f64, f64)]) -> Option<(f64, f64)> {
    const ON: f64 = 1e-7;
    let mut list: Vec<(f64, f64)> = Vec::with_capacity(spans.len());
    let mut sorted = spans.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    for s in sorted {
        match list.last_mut() {
            Some(l) if s.0 <= l.1 + ON => l.1 = l.1.max(s.1),
            _ => list.push(s),
        }
    }
    // The gap is what the port goes across, so it runs from one piece's face to the next one's.
    if let Some(i) = list.iter().position(|s| s.0 - ON <= 0.0 && 0.0 <= s.1 + ON) {
        if i > 0 {
            return Some((list[i - 1].1, list[i].0));
        }
        return list.get(i + 1).map(|next| (list[i].1, next.0));
    }
    let below = list.iter().rev().find(|s| s.1 < 0.0)?;
    let above = list.iter().find(|s| s.0 > 0.0)?;
    Some((below.1, above.0))
}

/// Where the line through `o` along `d` passes through a triangle, as a distance along it.
fn line_hits_triangle(o: P3, d: P3, t: [P3; 3]) -> Option<f64> {
    let (e1, e2) = (sub(t[1], t[0]), sub(t[2], t[0]));
    let h = cross(d, e2);
    let det = dot(e1, h);
    if det.abs() < 1e-30 {
        return None;
    }
    let s = sub(o, t[0]);
    let u = dot(s, h) / det;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) / det;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    Some(dot(e2, q) / det)
}

/// Tubes showing wires on the canvas. The solvers take the route, not these.
fn wire_tubes(wires: &[(Vec<P3>, f64)], feature: u32) -> Mesh {
    const SIDES: usize = 12;
    let mut m = Mesh::default();
    let face = face_tag(feature, 2);
    for (pts, r) in wires {
        let frames = prims::path_frames(pts, false, None);
        for w in 0..pts.len() - 1 {
            let (_, ua, va) = frames[w];
            let (_, ub, vb) = frames[w + 1];
            for k in 0..SIDES {
                let (a0, a1) = (std::f64::consts::TAU * k as f64 / SIDES as f64, std::f64::consts::TAU * (k + 1) as f64 / SIDES as f64);
                let on = |p: P3, u: P3, v: P3, t: f64| add(p, add(scale(u, r * t.cos()), scale(v, r * t.sin())));
                m.push_quad(on(pts[w], ua, va, a0), on(pts[w], ua, va, a1), on(pts[w + 1], ub, vb, a1), on(pts[w + 1], ub, vb, a0), face);
            }
        }
    }
    m.welded(WELD)
}

/// The most models to remember at once: enough for the canvas, a solve and a sweep together.
const REMEMBERED: usize = 6;

struct Remembered {
    features: Vec<Feature>,
    coords: Vec<CoordSys>,
    /// A probe asks what's metal, and a project's own material can change its mind.
    materials: u64,
    facets: u32,
    vars: Vars,
    /// How things stood after each feature.
    states: Vec<Arc<State>>,
}

static MEMORY: Mutex<Vec<Remembered>> = Mutex::new(Vec::new());

fn line_points_of(features: &[Feature], states: &[Arc<State>]) -> BTreeMap<(u32, u32), ([f64; 2], [f64; 2])> {
    let mut out = BTreeMap::new();
    for (f, st) in features.iter().zip(states) {
        if let Kind::Sketch { sketch, .. } = &f.kind
            && let Some(solved) = st.sketches.get(&f.id)
        {
            for c in &sketch.curves {
                if let super::sketch::CurveKind::Line { a, b } = c.kind {
                    out.insert((f.id, c.id), (solved.solved.at(a), solved.solved.at(b)));
                }
            }
        }
    }
    out
}

impl Model {
    /// Builds the tree, picking up from the last feature nothing has changed before.
    pub fn build(&self, vars: &Vars) -> Built {
        self.build_keeping(vars, true)
    }

    /// The same, for values only being tried: they would push out of the memory what the canvas
    /// needs for the values it's showing.
    pub fn build_unremembered(&self, vars: &Vars) -> Built {
        self.build_keeping(vars, false)
    }

    fn build_keeping(&self, vars: &Vars, remember: bool) -> Built {
        let n = self.rollback.unwrap_or(self.features.len()).min(self.features.len());
        let features = &self.features[..n];
        let mut start = 0;
        let mut states: Vec<Arc<State>> = Vec::with_capacity(n);
        if let Ok(memory) = MEMORY.lock() {
            let mut best = 0;
            for m in memory.iter().filter(|m| m.facets == self.facets && m.vars == *vars && m.coords == self.coords && m.materials == material::changes()) {
                let same = m.features.iter().zip(features).take_while(|(x, y)| x == y).count().min(m.states.len());
                if same > best {
                    best = same;
                    states = m.states[..same].to_vec();
                }
            }
            start = best;
        }
        let mut state: State = states.last().map(|s| (**s).clone()).unwrap_or_default();
        state.coords = self.coords.clone();
        state.line_points = line_points_of(&features[..start], &states);
        for f in &features[start..] {
            if !f.off {
                let facets = self.facets.clamp(8, 256) as usize;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.step(f, vars, facets)));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => state.errors.push((f.id, format!("{e:#}"))),
                    Err(_) => state.errors.push((f.id, "This feature couldn't be worked out.".to_string())),
                }
                if let Kind::Sketch { sketch, .. } = &f.kind
                    && let Some(solved) = state.sketches.get(&f.id)
                {
                    let solved = solved.solved.clone();
                    for c in &sketch.curves {
                        if let super::sketch::CurveKind::Line { a, b } = c.kind {
                            state.line_points.insert((f.id, c.id), (solved.at(a), solved.at(b)));
                        }
                    }
                }
            }
            states.push(Arc::new(state.clone()));
        }
        if remember && let Ok(mut memory) = MEMORY.lock() {
            memory.retain(|m| !(m.vars == *vars && m.facets == self.facets && m.features == features && m.coords == self.coords));
            memory.insert(0, Remembered { features: features.to_vec(), coords: self.coords.clone(), materials: material::changes(), facets: self.facets, vars: vars.clone(), states });
            memory.truncate(REMEMBERED);
        }
        Built { bodies: state.bodies.values().cloned().collect(), sketches: state.sketches, ports: state.ports.into_iter().map(|p| p.1).collect(), errors: state.errors }
    }
}

impl Body {
    /// Its name and how it should mesh, for the solvers.
    pub fn solids(&self, metal: &str) -> Vec<Solid> {
        let material = if self.material.is_empty() { metal.to_string() } else { self.material.clone() };
        let shapes = self.shapes();
        let many = shapes.len() > 1;
        shapes.into_iter().enumerate().map(|(k, shape)| Solid { name: if many { format!("{} {}", self.name, k + 1) } else { self.name.clone() }, material: material.clone(), shape }).collect()
    }

    /// The shapes a solver gets: its exact form if it still has one. A surface of flat faces goes
    /// face by face, so faces square to the world are the outlines every solver treats exactly.
    pub fn shapes(&self) -> Vec<Shape> {
        if let Some(e) = &self.exact {
            return e.clone();
        }
        match self.kind {
            BodyKind::Wire => self.wires.iter().map(|(pts, r)| Shape::Wire { pts: pts.clone(), r: *r }).collect(),
            BodyKind::Solid => vec![Shape::Tris { tris: all_tris(&self.mesh, None), solid: true, sheet: 0.0 }],
            BodyKind::Sheet => {
                let faces = self.mesh.face_ids();
                let planes: Vec<Option<(P3, P3)>> = faces.iter().map(|f| self.mesh.plane_of(*f, 1e-8)).collect();
                if planes.iter().any(Option::is_none) {
                    return vec![Shape::Tris { tris: all_tris(&self.mesh, None), solid: false, sheet: 0.0 }];
                }
                let mut out = Vec::with_capacity(faces.len());
                for (face, plane) in faces.iter().zip(planes) {
                    let Some((o, n)) = plane else { continue };
                    match world_axis(n).map(|axis| outlines_of_face(&self.mesh, *face, axis, o[axis])) {
                        Some(list) if !list.is_empty() => out.extend(list),
                        _ => out.push(Shape::Tris { tris: all_tris(&self.mesh, Some(*face)), solid: false, sheet: 0.0 }),
                    }
                }
                out
            }
        }
    }

    /// Roughly how fine a mesh it wants on its own. A hint asks for its cell along all three axes
    /// over the whole body, so a thin plate's thickness mustn't set it: that doubled the cells of a
    /// patch's board for nothing, since lines on its flat faces and the plan's own rule for thin
    /// dielectrics already see to the thickness.
    fn wanted_cell(&self) -> Option<f64> {
        if let Some(c) = self.cell {
            return Some(c);
        }
        if self.kind == BodyKind::Wire || self.mesh.is_empty() || !is_metal(&self.material) {
            return None;
        }
        let mut narrowest = f64::INFINITY;
        let mut outlined = false;
        for s in self.shapes() {
            if let Shape::Polygon { pts, .. } = &s {
                outlined = true;
                narrowest = narrowest.min(crate::cad::narrowest_of(std::slice::from_ref(pts)));
            }
        }
        if !outlined {
            let (lo, hi) = self.mesh.bounds();
            let mut sizes: Vec<f64> = (0..3).map(|k| hi[k] - lo[k]).filter(|e| *e > 0.0).collect();
            sizes.sort_by(f64::total_cmp);
            narrowest = sizes.get(1).or(sizes.first()).copied().unwrap_or(f64::INFINITY);
        }
        (narrowest.is_finite() && narrowest > 0.0).then_some(narrowest / 3.0)
    }

    /// What FDTD should know: how fine to go here, and where flat faces lie, since a mesh line on a
    /// face is what makes a face exact.
    pub fn hints(&self, finest: f64) -> Vec<MeshHint> {
        let mut out = Vec::new();
        if self.mesh.is_empty() {
            return out;
        }
        let (lo, hi) = self.mesh.bounds();
        if let Some(c) = self.wanted_cell() {
            out.push(MeshHint { min: lo, max: hi, cell: c.max(finest) });
        }
        if self.exact.is_none() && self.kind != BodyKind::Wire {
            let mut planes: Vec<(usize, f64)> = Vec::new();
            for f in self.mesh.face_ids() {
                if let Some((o, n)) = self.mesh.plane_of(f, 1e-8)
                    && let Some(axis) = world_axis(n)
                    && !planes.iter().any(|(a, at)| *a == axis && (at - o[axis]).abs() < 1e-9)
                {
                    planes.push((axis, o[axis]));
                }
            }
            for (axis, at) in planes.into_iter().take(64) {
                let (mut min, mut max) = (lo, hi);
                min[axis] = at;
                max[axis] = at;
                out.push(MeshHint { min, max, cell: f64::MAX });
            }
        }
        out
    }
}

fn all_tris(m: &Mesh, face: Option<FaceId>) -> Vec<[P3; 3]> {
    (0..m.tris.len()).filter(|t| face.is_none_or(|f| m.face[*t] == f)).map(|t| m.corners(t)).collect()
}

/// A flat face as outlines in the solvers' terms: each outer loop with the holes inside it.
fn outlines_of_face(m: &Mesh, face: FaceId, axis: usize, at: f64) -> Vec<Shape> {
    let loops: Vec<Vec<[f64; 2]>> = m.boundary_loops(face).iter().map(|l| l.iter().map(|v| solver_uv(axis, m.verts[*v as usize])).collect()).collect();
    if loops.is_empty() {
        return Vec::new();
    }
    let inside = |outer: &[[f64; 2]], q: [f64; 2]| crate::cad::point_inside(outer, q);
    // Halfway along an edge, as two loops can touch at a corner and that says nothing.
    let probe = |r: &[[f64; 2]]| [(r[0][0] + r[1][0]) / 2.0, (r[0][1] + r[1][1]) / 2.0];
    let depth = |i: usize| loops.iter().enumerate().filter(|(j, o)| *j != i && prims::signed_area(o).abs() > prims::signed_area(&loops[i]).abs() && inside(o, probe(&loops[i]))).count();
    let mut out = Vec::new();
    for i in 0..loops.len() {
        if depth(i) % 2 != 0 {
            continue;
        }
        let mut pts = loops[i].clone();
        for j in 0..loops.len() {
            if j != i && depth(j) == depth(i) + 1 && inside(&loops[i], probe(&loops[j])) {
                pts = crate::cad::cut_hole(&pts, &loops[j]);
            }
        }
        out.push(Shape::Polygon { axis, at, pts, thick: 0.0 });
    }
    out
}

impl Built {
    pub fn solids(&self, metal: &str) -> Vec<Solid> {
        self.bodies.iter().flat_map(|b| b.solids(metal)).collect()
    }

    pub fn hints(&self, finest: f64) -> Vec<MeshHint> {
        self.bodies.iter().flat_map(|b| b.hints(finest)).collect()
    }

    /// Everything drawn, for framing the canvas.
    pub fn bounds(&self) -> Option<(P3, P3)> {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        let mut any = false;
        for b in self.bodies.iter().filter(|b| !b.mesh.is_empty()) {
            let (a, c) = b.mesh.bounds();
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(c[k]);
            }
            any = true;
        }
        any.then_some((lo, hi))
    }

    /// Metal bodies that touch but won't carry current between them in the method of moments.
    /// Flat faces and exact shapes get imprinted on each other by its mesher. A surface it takes as
    /// triangles only joins where a vertex happens to coincide, so that takes a Combine.
    pub fn touching_unjoined(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let metal: Vec<&Arc<Body>> = self.bodies.iter().filter(|b| is_metal(&b.material) && !b.mesh.is_empty() && b.kind != BodyKind::Wire).collect();
        let triangles: Vec<bool> = metal.iter().map(|b| b.shapes().iter().any(|s| matches!(s, Shape::Tris { tris, .. } if crate::mom::mesher::goes_as_soup(tris, 1e-9)))).collect();
        for (i, a) in metal.iter().enumerate() {
            for (j, b) in metal.iter().enumerate().skip(i + 1) {
                if !triangles[i] && !triangles[j] {
                    continue;
                }
                let (la, ha) = a.mesh.bounds();
                let (lb, hb) = b.mesh.bounds();
                let tol = 1e-7;
                if (0..3).all(|k| la[k] <= hb[k] + tol && lb[k] <= ha[k] + tol) && boolean::touches(&a.mesh, &b.mesh) {
                    out.push((a.name.clone(), b.name.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modeler::sketch::{Constraint, ORIGIN};

    fn vars() -> Vars {
        let mut v = Vars::new();
        v.insert("L".into(), 0.04);
        v.insert("W".into(), 0.03);
        v.insert("h".into(), 0.002);
        v
    }

    /// A centred rectangle, its size in variables.
    fn plate_sketch(l: &str, w: &str) -> Sketch {
        let mut s = Sketch::default();
        let (p, _) = s.add_rect([-0.01, -0.01], [0.01, 0.01]);
        s.constraints.push(Constraint::DistanceU { a: p[0], b: p[1], value: l.into() });
        s.constraints.push(Constraint::DistanceV { a: p[1], b: p[2], value: w.into() });
        s.constraints.push(Constraint::SymmetricAxis { a: p[0], b: p[1], axis: 0 });
        s.constraints.push(Constraint::SymmetricAxis { a: p[1], b: p[2], axis: 1 });
        s
    }

    fn top() -> PlaneRef {
        PlaneRef::Base { axis: 2, offset: "0".into() }
    }

    fn no_errors(b: &Built) {
        assert!(b.errors.is_empty(), "{:?}", b.errors);
    }

    fn sized(b: &Body) -> P3 {
        let (lo, hi) = b.mesh.bounds();
        std::array::from_fn(|k| hi[k] - lo[k])
    }

    #[test]
    fn a_sheet_given_a_thickness_is_a_slab_every_solver_can_still_read() {
        let mut m = Model::default();
        let s = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let id = m.add(Kind::Sheet { sketch: s, out: Output::default() });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies[0].kind, BodyKind::Sheet);
        assert_eq!(sized(&b.bodies[0])[2], 0.0);

        m.feature_mut(id).unwrap().kind.output_mut().unwrap().thick = "h".into();
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies[0].kind, BodyKind::Solid);
        assert!(b.bodies[0].mesh.is_closed());
        assert!((sized(&b.bodies[0])[2] - 0.002).abs() < 1e-12, "{:?}", sized(&b.bodies[0]));
        // The outline stays exact: the method of moments keeps it a sheet, FDTD sees the slab.
        let Some([Shape::Polygon { at, thick, .. }]) = b.bodies[0].exact.as_deref() else { panic!("{:?}", b.bodies[0].exact) };
        assert!(at.abs() < 1e-12 && (thick - 0.002).abs() < 1e-12, "{at} {thick}");

        // Zero and nothing both mean a surface, and a bad expression says what it was.
        m.feature_mut(id).unwrap().kind.output_mut().unwrap().thick = "0".into();
        assert_eq!(m.build(&vars()).bodies[0].kind, BodyKind::Sheet);
        m.feature_mut(id).unwrap().kind.output_mut().unwrap().thick = "nope".into();
        assert!(m.build(&vars()).errors[0].1.contains("thickness"));
        m.feature_mut(id).unwrap().kind.output_mut().unwrap().thick = "-1mm".into();
        assert!(m.build(&vars()).errors[0].1.contains("negative"));
    }

    /// The method of moments meshes a slab's sheet at its lowest world coordinate, which has to be
    /// the plane it was drawn on, whichever way that plane's normal points.
    #[test]
    fn a_slab_rises_from_its_plane_on_every_plane() {
        let rect = Flat::Rect { p0: ["0".into(), "0".into()], size: ["10mm".into(), "10mm".into()] };
        for axis in [0, 1, 2] {
            let mut m = Model::default();
            m.add(Kind::Face { plane: PlaneRef::Base { axis, offset: "0".into() }, shape: rect.clone(), out: Output { thick: "2mm".into(), ..Default::default() } });
            let b = m.build(&vars());
            no_errors(&b);
            let (lo, hi) = b.bodies[0].mesh.bounds();
            assert!(lo[axis].abs() < 1e-12 && (hi[axis] - 0.002).abs() < 1e-12, "axis {axis}: {lo:?} {hi:?}");
            let Some([Shape::Polygon { at, thick, .. }]) = b.bodies[0].exact.as_deref() else { panic!() };
            assert!(at.abs() < 1e-12 && (thick - 0.002).abs() < 1e-12, "axis {axis}: {at} {thick}");
        }
    }

    #[test]
    fn a_probe_crosses_the_gap_and_not_a_slabs_own_metal() {
        // The same line through: two sheets, a sheet over a slab, a slab either side.
        assert_eq!(probe_span(&[(0.0, 0.0), (5.0, 5.0)]), Some((0.0, 5.0)));
        assert_eq!(probe_span(&[(0.0, 0.002), (5.0, 5.0)]), Some((0.002, 5.0)), "from the slab's far face");
        assert_eq!(probe_span(&[(-5.0, -4.0), (0.0, 1.0)]), Some((-4.0, 0.0)), "metal below comes first");
        assert_eq!(probe_span(&[(-5.0, -5.0), (5.0, 5.0)]), Some((-5.0, 5.0)), "none on the plane: one each side");
        assert_eq!(probe_span(&[(0.0, 1.0)]), None, "a slab on its own has nothing to join");
        assert_eq!(probe_span(&[(-1.0, 1.0), (1.0, 2.0)]), None, "touching pieces are one piece");

        let mut m = Model::default();
        let mut ground = Sketch::default();
        let (p, _) = ground.add_rect([-0.02, -0.02], [0.02, 0.02]);
        let _ = p;
        let point = ground.add_point([0.0, 0.0]);
        let sk = m.add(Kind::Sketch { plane: top(), sketch: ground });
        let g = m.add(Kind::Sheet { sketch: sk, out: Output { thick: "1mm".into(), ..Default::default() } });
        let _ = g;
        m.add(Kind::Face { plane: PlaneRef::Base { axis: 2, offset: "5mm".into() }, shape: Flat::Circle { c: ["0".into(), "0".into()], r: "10mm".into() }, out: Output::default() });
        m.add(Kind::Probe { sketch: sk, point, width: String::new(), z0: "50".into() });
        let b = m.build(&vars());
        no_errors(&b);
        let port = &b.ports[0];
        let (a, c) = (port.a[2].min(port.b[2]), port.a[2].max(port.b[2]));
        assert!((a - 0.001).abs() < 1e-9 && (c - 0.005).abs() < 1e-9, "from the top of the ground to the patch: {port:?}");
    }

    #[test]
    fn deleting_an_object_takes_only_what_is_its_own() {
        let mut m = Model::default();
        let s = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let a = m.add(Kind::Extrude { sketch: s, depth: "h".into(), extent: Extent::One, out: Output::default() });
        let b = m.add(Kind::Sheet { sketch: s, out: Output::default() });
        // A sketch something else is made from stays.
        m.remove_object(a);
        assert!(m.feature(a).is_none());
        assert!(m.feature(s).is_some() && m.feature(b).is_some());
        no_errors(&m.build(&vars()));
        // Its last user going takes it too.
        m.remove_object(b);
        assert!(m.is_empty(), "{:?}", m.features);

        // A move of two: taking one away leaves the other moved.
        let sphere = |x: &str| Kind::Solid { shape: Prim::Sphere { c: [x.into(), "0".into(), "0".into()], r: "1mm".into() }, out: Output::default() };
        let mut m = Model::default();
        let (a, c) = (m.add(sphere("0")), m.add(sphere("9mm")));
        let mv = m.add(Kind::Move { bodies: vec![a, c], by: ["0".into(), "0".into(), "5mm".into()], origin: None, axis: None, angle: String::new(), copy: false });
        m.remove_object(a);
        let Some(Kind::Move { bodies, .. }) = m.feature(mv).map(|f| &f.kind) else { panic!("the move's still needed") };
        assert_eq!(*bodies, [c]);
        let built = m.build(&vars());
        no_errors(&built);
        assert!((built.bodies[0].mesh.bounds().1[2] - 0.006).abs() < 1e-9);
        // Its only body going takes it too.
        m.remove_object(c);
        assert!(m.is_empty(), "{:?}", m.features);
    }

    #[test]
    fn a_sketch_in_a_coordinate_system_lands_where_a_shape_does_and_follows_it() {
        let mut m = Model::default();
        let cs = m.add_coord();
        m.coords[0].origin = ["100mm".into(), "0".into(), "0".into()];
        m.coords[0].turn = ["0".into(), "0".into(), "30deg".into()];
        let rect = Flat::Rect { p0: ["0".into(), "0".into()], size: ["10mm".into(), "20mm".into()] };
        // A shape drawn in it, and the same outline sketched on its plane.
        m.add(Kind::Face { plane: top(), shape: rect, out: Output { coord: Some(cs), ..Default::default() } });
        let mut s = Sketch::default();
        s.add_rect([0.0, 0.0], [0.01, 0.02]);
        let sk = m.add(Kind::Sketch { plane: PlaneRef::InCoord { coord: cs, axis: 2, offset: "0".into() }, sketch: s });
        m.add(Kind::Sheet { sketch: sk, out: Output::default() });
        let b = m.build(&vars());
        no_errors(&b);
        let (lo0, hi0) = b.bodies[0].mesh.bounds();
        let (lo1, hi1) = b.bodies[1].mesh.bounds();
        for k in 0..3 {
            assert!((lo0[k] - lo1[k]).abs() < 1e-9 && (hi0[k] - hi1[k]).abs() < 1e-9, "{lo0:?} {hi0:?} against {lo1:?} {hi1:?}");
        }
        assert_eq!(m.drawn_in(cs).len(), 2);
        // Moving the system moves the sketch too.
        m.coords[0].origin = ["200mm".into(), "0".into(), "0".into()];
        let moved = m.build(&vars());
        assert!((moved.bodies[1].mesh.bounds().0[0] - lo1[0] - 0.1).abs() < 1e-9);
        // Taking it away leaves a sketch that still builds.
        m.remove_coord(cs);
        no_errors(&m.build(&vars()));
    }

    #[test]
    fn a_step_says_the_body_it_works_on_the_way_the_build_decides_it() {
        // Combined away, the latest body standing is the older one, and that's what "the latest" means.
        let mut m = Model::default();
        let ball = |x: &str| Kind::Solid { shape: Prim::Sphere { c: [x.into(), "0".into(), "0".into()], r: "2mm".into() }, out: Output::default() };
        let (a, b) = (m.add(ball("0")), m.add(ball("1mm")));
        m.add(Kind::Combine { op: CombineOp::Add, target: a, tools: vec![b], keep_tools: false });
        let cut = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "0".into()], r: "1mm".into() }, out: Output { mode: Mode::Cut, ..Default::default() } });
        let o = m.objects();
        assert_eq!(o.len(), 1);
        assert!(o[0].steps.contains(&cut), "{o:?}");
        no_errors(&m.build(&vars()));
        // A Combine switched off leaves its tools as objects.
        m.features[2].off = true;
        assert_eq!(m.objects().iter().map(|x| x.id).collect::<Vec<_>>(), [a, b]);
    }

    #[test]
    fn flat_shapes_come_out_the_size_they_say_on_any_plane() {
        let rect = |p: [&str; 2], s: [&str; 2]| Flat::Rect { p0: [p[0].into(), p[1].into()], size: [s[0].into(), s[1].into()] };
        let mut m = Model::default();
        m.add(Kind::Face { plane: top(), shape: rect(["0", "0"], ["L", "W"]), out: Output::default() });
        // A negative size runs the other way, the way dragging leftwards does.
        m.add(Kind::Face { plane: PlaneRef::Base { axis: 1, offset: "5mm".into() }, shape: rect(["0", "0"], ["-10mm", "20mm"]), out: Output::default() });
        m.add(Kind::Face { plane: top(), shape: Flat::Circle { c: ["0".into(), "0".into()], r: "10mm".into() }, out: Output::default() });
        m.add(Kind::Face { plane: top(), shape: Flat::Ellipse { c: ["0".into(), "0".into()], rx: "20mm".into(), ry: "10mm".into() }, out: Output::default() });
        m.add(Kind::Face { plane: top(), shape: Flat::Polygon { c: ["0".into(), "0".into()], r: "10mm".into(), sides: "6".into() }, out: Output::default() });
        m.facets = 256;
        let b = m.build(&vars());
        no_errors(&b);
        let area = |i: usize| b.bodies[i].mesh.area();
        assert!((area(0) - 0.04 * 0.03).abs() < 1e-12);
        assert!((area(1) - 0.01 * 0.02).abs() < 1e-12);
        let (lo, hi) = b.bodies[1].mesh.bounds();
        assert!((lo[0] + 0.01).abs() < 1e-12 && hi[0].abs() < 1e-12, "it runs towards -x: {lo:?} {hi:?}");
        assert!((lo[1] + 0.005).abs() < 1e-12 && (hi[1] + 0.005).abs() < 1e-12, "offset along the Front plane's normal");
        let pi = std::f64::consts::PI;
        assert!((area(2) - pi * 1e-4).abs() / (pi * 1e-4) < 1e-3, "{}", area(2));
        assert!((area(3) - pi * 2e-4).abs() / (pi * 2e-4) < 1e-3, "{}", area(3));
        let hex = 3.0 * 3f64.sqrt() / 2.0 * 1e-4;
        assert!((area(4) - hex).abs() < 1e-12, "{}", area(4));
        assert_eq!(m.feature(5).unwrap().name, "Polygon 1", "named after what they are");

        for bad in [Flat::Polygon { c: ["0".into(), "0".into()], r: "1mm".into(), sides: "2".into() }, rect(["0", "0"], ["0", "1mm"]), Flat::Circle { c: ["0".into(), "0".into()], r: "-1mm".into() }] {
            let mut m = Model::default();
            m.add(Kind::Face { plane: top(), shape: bad, out: Output::default() });
            assert_eq!(m.build(&vars()).errors.len(), 1);
        }
    }

    #[test]
    fn what_is_drawn_in_a_coordinate_system_goes_where_it_says_and_follows_it() {
        let mut m = Model::default();
        let cs = m.add_coord();
        let boxy = |coord| Kind::Solid { shape: Prim::Box { min: ["0".into(), "0".into(), "0".into()], max: ["10mm".into(), "20mm".into(), "30mm".into()] }, out: Output { coord, ..Default::default() } };
        m.add(boxy(Some(cs)));
        let at_rest = m.build(&vars());
        no_errors(&at_rest);
        assert!(matches!(at_rest.bodies[0].exact.as_deref(), Some([Shape::Box { .. }])), "at rest it's still an exact box");

        m.coords[0].origin = ["L".into(), "0".into(), "0".into()];
        let b = m.build(&vars());
        no_errors(&b);
        let (lo, _) = b.bodies[0].mesh.bounds();
        assert!((lo[0] - 0.04).abs() < 1e-12, "the build isn't taken from memory after the system moved: {lo:?}");
        let Some([Shape::Box { min, .. }]) = b.bodies[0].exact.as_deref() else { panic!() };
        assert!((min[0] - 0.04).abs() < 1e-12, "a shift keeps the exact box, moved");

        // A quarter turn about z takes x to y.
        m.coords[0].origin = ["0".into(), "0".into(), "0".into()];
        m.coords[0].turn = ["0".into(), "0".into(), "90deg".into()];
        let b = m.build(&vars());
        no_errors(&b);
        let s = sized(&b.bodies[0]);
        assert!((s[0] - 0.02).abs() < 1e-9 && (s[1] - 0.01).abs() < 1e-9 && (s[2] - 0.03).abs() < 1e-9, "{s:?}");
        assert!(b.bodies[0].exact.is_none(), "turned, it's no longer a box square to the world");

        // Turns go about x, then y, then z: x→y under z, then that's left alone by a turn about x first.
        m.coords[0].turn = ["90deg".into(), "0".into(), "90deg".into()];
        let x = m.coords[0].xform(&vars()).unwrap();
        let d = x.apply_dir([0.0, 1.0, 0.0]);
        assert!(d[0].abs() < 1e-12 && d[1].abs() < 1e-12 && (d[2] - 1.0).abs() < 1e-12, "y→z under x, and z stays under z: {d:?}");

        // Taking the system away leaves the shape in the world's system, not broken.
        m.remove_coord(cs);
        no_errors(&m.build(&vars()));
        assert_eq!(m.features[0].kind.output().unwrap().coord, None);
    }

    #[test]
    fn a_model_of_only_a_coordinate_system_is_still_saved_and_old_files_still_read() {
        let mut m = Model::default();
        assert!(m.nothing_to_save());
        m.add_coord();
        assert!(!m.nothing_to_save());
        assert!(m.is_empty(), "there's still nothing to solve");
        let text = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Model>(&text).unwrap(), m);
        let old: Model = serde_json::from_str(r#"{"features":[{"id":1,"name":"S","kind":{"type":"solid","shape":{"type":"sphere","c":["0","0","0"],"r":"1mm"},"out":{"mode":"new","target":null,"material":"","mesh":"","surface":false}}}],"rollback":null,"facets":32}"#).unwrap();
        assert!(old.coords.is_empty());
        assert_eq!(old.features[0].kind.output().unwrap().thick, "");
        // And a feature with nothing new set writes nothing new, so saved files don't churn.
        let written = serde_json::to_string(&old).unwrap();
        assert!(!written.contains("thick") && !written.contains("coord"), "{written}");
    }

    #[test]
    fn objects_gather_the_steps_that_made_them() {
        let mut m = Model::default();
        let s1 = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let plate = m.add(Kind::Extrude { sketch: s1, depth: "h".into(), extent: Extent::One, out: Output::default() });
        let s2 = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("5mm", "5mm") });
        // Says "the latest body" rather than naming it.
        let hole = m.add(Kind::Extrude { sketch: s2, depth: "1mm".into(), extent: Extent::Through, out: Output { mode: Mode::Cut, ..Default::default() } });
        let round = m.add(Kind::Fillet { body: plate, edges: Vec::new(), radius: "1mm".into(), chamfer: false });
        let other = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "0".into()], r: "1mm".into() }, out: Output::default() });
        let third = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "0".into()], r: "1mm".into() }, out: Output::default() });
        let join = m.add(Kind::Combine { op: CombineOp::Add, target: other, tools: vec![third], keep_tools: false });
        let unused = m.add(Kind::Sketch { plane: top(), sketch: Sketch::default() });
        m.add(Kind::Feed { a: Anchor::Xyz { at: ["0".into(), "0".into(), "0".into()] }, b: Anchor::Xyz { at: ["0".into(), "0".into(), "1mm".into()] }, width: String::new(), z0: "50".into() });

        let o = m.objects();
        assert_eq!(o.iter().map(|x| x.id).collect::<Vec<_>>(), [plate, other], "a body combined away isn't its own object");
        assert_eq!(o[0].steps, [s1, plate, s2, hole, round], "the cut with no target goes with the latest body");
        assert_eq!(o[1].steps, [other, third, join], "what was combined in is listed under what it became part of");
        assert_eq!(m.loose_steps(), [unused]);
    }

    #[test]
    fn a_pasted_object_is_a_separate_one_with_its_own_dimensions() {
        let mut m = Model::default();
        let s = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let plate = m.add(Kind::Extrude { sketch: s, depth: "h".into(), extent: Extent::One, out: Output::default() });
        let edges = {
            let b = m.build(&vars());
            let (top, low) = (face_tag(plate, crate::modeler::prims::END_HIGH), face_tag(plate, crate::modeler::prims::END_LOW));
            // Any side meets the top along an edge.
            let side = b.bodies[0].mesh.face_ids().into_iter().find(|f| *f != top && *f != low).unwrap();
            vec![[top, side]]
        };
        m.add(Kind::Fillet { body: plate, edges, radius: "0.5mm".into(), chamfer: false });
        let other = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "0".into()], r: "1mm".into() }, out: Output::default() });
        // A move of both must not carry the original along when only one's copied.
        m.add(Kind::Move { bodies: vec![plate, other], by: ["0".into(), "0".into(), "1mm".into()], origin: None, axis: None, angle: String::new(), copy: false });
        let before = m.build(&vars());
        assert_eq!(before.errors, Vec::<(u32, String)>::new());
        assert_eq!(before.bodies.len(), 2);

        let clip = m.copy_steps(&[plate]);
        assert_eq!(clip.len(), 4, "the sketch, the extrusion, its fillet and its move, and not the sphere: {:?}", clip.iter().map(|f| &f.name).collect::<Vec<_>>());
        let new = m.paste_steps(&clip);
        let after = m.build(&vars());
        assert_eq!(after.errors, Vec::<(u32, String)>::new());
        assert_eq!(after.bodies.len(), 3);
        let copy = after.bodies.iter().find(|b| b.id == new[1]).unwrap();
        let first = after.bodies.iter().find(|b| b.id == plate).unwrap();
        assert!((copy.mesh.volume() - first.mesh.volume()).abs() < 1e-12);
        assert_eq!(copy.mesh.bounds(), first.mesh.bounds(), "pasted where the original is, moved the same");
        let sphere = after.bodies.iter().find(|b| b.id == other).unwrap();
        assert_eq!(sphere.mesh.bounds(), before.bodies.iter().find(|b| b.id == other).unwrap().mesh.bounds(), "the sphere was moved once, not twice");

        // Its dimensions are its own: changing the copy leaves the original alone.
        let Some(Kind::Extrude { sketch, depth, .. }) = m.feature_mut(new[1]).map(|f| &mut f.kind) else { panic!() };
        assert_eq!(*sketch, new[0], "it uses its own sketch");
        *depth = "9mm".into();
        let edited = m.build(&vars());
        assert_eq!(edited.errors, Vec::<(u32, String)>::new());
        assert!(edited.bodies.iter().find(|b| b.id == new[1]).unwrap().mesh.volume() > edited.bodies.iter().find(|b| b.id == plate).unwrap().mesh.volume() * 2.0);
        assert!(m.features.iter().any(|f| f.name.ends_with("copy")));

        // Pasting again gives another name, not a clash.
        let again = m.paste_steps(&clip);
        let names: Vec<&str> = again.iter().map(|i| m.feature(*i).unwrap().name.as_str()).collect();
        assert!(names[0].ends_with("copy 2"), "{names:?}");
    }

    #[test]
    fn an_extruded_sketch_follows_its_variables() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let body = m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output::default() });
        let mut v = vars();
        let b = m.build(&v);
        no_errors(&b);
        assert_eq!(b.bodies.len(), 1);
        assert_eq!(b.bodies[0].id, body);
        assert!((b.bodies[0].mesh.volume() - 0.04 * 0.03 * 0.002).abs() < 1e-12);
        // It's square to the world, so the solvers get the exact outline.
        let Some(Shape::Polygon { axis: 2, thick, .. }) = b.bodies[0].exact.as_ref().and_then(|e| e.first()) else { panic!("{:?}", b.bodies[0].exact) };
        assert!((*thick - 0.002).abs() < 1e-12);
        v.insert("L".into(), 0.05);
        let b = m.build(&v);
        assert!((b.bodies[0].mesh.volume() - 0.05 * 0.03 * 0.002).abs() < 1e-12, "{}", b.bodies[0].mesh.volume());
    }

    #[test]
    fn a_cut_goes_through_and_the_body_keeps_its_name() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let body = m.add(Kind::Extrude { sketch: sk, depth: "10".into(), extent: Extent::One, out: Output::default() });
        let mut hole = Sketch::default();
        let (c, curve) = hole.add_circle([0.0, 0.0], 0.004);
        hole.constraints.push(Constraint::Coincident { a: c, b: ORIGIN });
        hole.constraints.push(Constraint::Radius { curve, value: "4".into() });
        let hs = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "10".into() }, sketch: hole });
        m.add(Kind::Extrude { sketch: hs, depth: "0".into(), extent: Extent::Through, out: Output { mode: Mode::Cut, ..Default::default() } });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies.len(), 1);
        assert_eq!(b.bodies[0].id, body);
        assert!(b.bodies[0].mesh.is_closed());
        let exact = (0.04 * 0.03 - std::f64::consts::PI * 1.6e-5) * 0.01;
        assert!((b.bodies[0].mesh.volume() - exact).abs() / exact < 3e-3, "{} vs {exact}", b.bodies[0].mesh.volume());
        assert!(b.bodies[0].exact.is_none(), "a cut body has no exact outline any more");
    }

    #[test]
    fn a_sketch_on_a_face_follows_the_face() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let body = m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output::default() });
        // On the top of it, whose name is the extrusion's high end.
        let top_face = face_tag(body, prims::END_HIGH);
        let mut boss = Sketch::default();
        let (c, curve) = boss.add_circle([0.0, 0.0], 0.005);
        boss.constraints.push(Constraint::Coincident { a: c, b: ORIGIN });
        boss.constraints.push(Constraint::Radius { curve, value: "5".into() });
        let bs = m.add(Kind::Sketch { plane: PlaneRef::Face { face: top_face, offset: "0".into() }, sketch: boss });
        m.add(Kind::Extrude { sketch: bs, depth: "3".into(), extent: Extent::One, out: Output { mode: Mode::Add, ..Default::default() } });
        let mut v = vars();
        let b = m.build(&v);
        no_errors(&b);
        assert!((b.bodies[0].mesh.bounds().1[2] - 0.005).abs() < 1e-9, "{:?}", b.bodies[0].mesh.bounds());
        // A thicker plate carries it up.
        v.insert("h".into(), 0.004);
        let b = m.build(&v);
        no_errors(&b);
        assert!((b.bodies[0].mesh.bounds().1[2] - 0.007).abs() < 1e-9);
        assert!(b.bodies[0].mesh.is_closed());
    }

    #[test]
    fn one_bad_step_is_marked_and_the_others_still_build() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output::default() });
        let bad = m.add(Kind::Extrude { sketch: sk, depth: "nope".into(), extent: Extent::One, out: Output::default() });
        let also = m.add(Kind::Extrude { sketch: 999, depth: "1".into(), extent: Extent::One, out: Output::default() });
        let good = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "20".into()], r: "5".into() }, out: Output::default() });
        let b = m.build(&vars());
        assert_eq!(b.errors.iter().map(|e| e.0).collect::<Vec<_>>(), vec![bad, also]);
        assert!(b.errors[0].1.contains("nope") && b.errors[1].1.contains("gone"), "{:?}", b.errors);
        assert!(b.bodies.iter().any(|x| x.id == good));
    }

    #[test]
    fn rolling_back_shows_the_model_as_it_stood() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output::default() });
        m.add(Kind::Solid { shape: Prim::Box { min: ["50".into(), "0".into(), "0".into()], max: ["60".into(), "10".into(), "10".into()] }, out: Output::default() });
        assert_eq!(m.build(&vars()).bodies.len(), 2);
        m.rollback = Some(2);
        assert_eq!(m.build(&vars()).bodies.len(), 1);
        // Something new goes in where the rollback is.
        let id = m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "20".into()], r: "5".into() }, out: Output::default() });
        assert_eq!(m.features[2].id, id);
        assert_eq!(m.rollback, Some(3));
    }

    #[test]
    fn an_edit_late_in_the_tree_doesnt_redo_what_came_before() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let e = m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output::default() });
        // A distinctive value, so nothing another test built can stand in for it.
        let mut v = vars();
        v.insert("L".into(), 0.0417);
        let first = m.build(&v);
        m.add(Kind::Solid { shape: Prim::Sphere { c: ["0".into(), "0".into(), "20".into()], r: "5".into() }, out: Output::default() });
        let second = m.build(&v);
        let same = |b: &Built| b.bodies.iter().find(|x| x.id == e).unwrap().mesh.clone();
        assert!(Arc::ptr_eq(&same(&first), &same(&second)), "the extrusion wasn't built twice");
        // A different value is a different model.
        v.insert("L".into(), 0.0418);
        let third = m.build(&v);
        assert!(!Arc::ptr_eq(&same(&first), &same(&third)));
    }

    #[test]
    fn a_pattern_makes_an_array_with_a_feed_per_element() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "h".into() }, sketch: plate_sketch("10", "10") });
        let patch = m.add(Kind::Sheet { sketch: sk, out: Output::default() });
        let feed = m.add(Kind::Feed { a: Anchor::Xyz { at: ["0".into(), "0".into(), "0".into()] }, b: Anchor::Xyz { at: ["0".into(), "0".into(), "h".into()] }, width: String::new(), z0: "50".into() });
        m.add(Kind::Pattern { bodies: vec![patch], layout: Layout::Linear { step: ["20".into(), "0".into(), "0".into()], count: "2".into(), step2: Some(["0".into(), "30".into(), "0".into()]), count2: "2".into() }, merge: false, feeds: vec![feed] });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies.len(), 4);
        assert_eq!(b.ports.len(), 4);
        // The copies are still exact, since they only moved.
        assert!(b.bodies.iter().all(|x| x.exact.is_some()));
        let far = b.ports.iter().map(|p| p.a[0] + p.a[1]).fold(0.0, f64::max);
        assert!((far - 0.05).abs() < 1e-9, "{far}");
    }

    #[test]
    fn mirroring_and_moving_keep_what_a_body_is() {
        let mut m = Model::default();
        let body = m.add(Kind::Solid { shape: Prim::Box { min: ["5".into(), "0".into(), "0".into()], max: ["15".into(), "10".into(), "10".into()] }, out: Output::default() });
        m.add(Kind::Mirror { bodies: vec![body], plane: PlaneRef::Base { axis: 0, offset: "0".into() }, merge: false });
        m.add(Kind::Move { bodies: vec![body], by: ["0".into(), "0".into(), "7".into()], origin: None, axis: None, angle: String::new(), copy: false });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies.len(), 2);
        let moved = b.bodies.iter().find(|x| x.id == body).unwrap();
        assert!((moved.mesh.bounds().0[2] - 0.007).abs() < 1e-12);
        assert!(matches!(moved.exact.as_ref().unwrap()[0], Shape::Box { .. }), "a box that moved is still a box");
        let image = b.bodies.iter().find(|x| x.id != body).unwrap();
        assert!((image.mesh.bounds().1[0] + 0.005).abs() < 1e-12);
        assert!(image.mesh.volume() > 0.0 && image.mesh.is_closed(), "a mirror image still faces out");
    }

    #[test]
    fn revolving_a_profile_across_its_axis_is_refused() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 1, offset: "0".into() }, sketch: plate_sketch("10", "10") });
        let id = m.add(Kind::Revolve { sketch: sk, axis: AxisRef::V, angle: "360".into(), out: Output::default() });
        let b = m.build(&vars());
        assert_eq!(b.errors[0].0, id);
        assert!(b.errors[0].1.contains("both sides"), "{:?}", b.errors);
    }

    #[test]
    fn a_revolve_about_a_line_in_the_sketch_is_a_ring() {
        let mut m = Model::default();
        let mut s = Sketch::default();
        s.add_rect([0.02, -0.005], [0.03, 0.005]);
        let (a, q) = (s.add_point([0.0, -0.01]), s.add_point([0.0, 0.01]));
        let axis = s.add_line(a, q);
        s.curves.iter_mut().find(|c| c.id == axis).unwrap().construction = true;
        let sk = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 1, offset: "0".into() }, sketch: s });
        m.add(Kind::Revolve { sketch: sk, axis: AxisRef::Line { curve: axis }, angle: "360".into(), out: Output::default() });
        let b = m.build(&vars());
        no_errors(&b);
        assert!(b.bodies[0].mesh.is_closed());
        let exact = std::f64::consts::PI * (0.03f64.powi(2) - 0.02f64.powi(2)) * 0.01;
        assert!((b.bodies[0].mesh.volume() - exact).abs() / exact < 0.01);
    }

    #[test]
    fn a_helix_is_a_wire_the_method_of_moments_can_take() {
        let mut m = Model::default();
        let path = PathRef::Helix { origin: ["0".into(), "0".into(), "0".into()], axis: ["0".into(), "0".into(), "1".into()], radius: "20".into(), radius2: String::new(), pitch: "10".into(), turns: "3".into(), left: false };
        m.add(Kind::Wire { path, radius: "0.5".into(), out: Output::default() });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies[0].kind, BodyKind::Wire);
        let solids = b.solids("PEC");
        let Shape::Wire { pts, r } = &solids[0].shape else { panic!("{:?}", solids[0].shape) };
        assert!((r - 0.0005).abs() < 1e-12);
        assert!((pts.last().unwrap()[2] - 0.03).abs() < 1e-12);
        assert!(!b.bodies[0].mesh.is_empty(), "a tube to see it by");
    }

    #[test]
    fn a_bent_plate_is_flat_faces_a_solver_can_take_exactly() {
        let mut m = Model::default();
        let mut s = Sketch::default();
        let (a, c, d) = (s.add_point([0.0, 0.0]), s.add_point([0.02, 0.0]), s.add_point([0.02, 0.01]));
        s.add_line(a, c);
        s.add_line(c, d);
        let sk = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 1, offset: "0".into() }, sketch: s });
        m.add(Kind::Extrude { sketch: sk, depth: "5".into(), extent: Extent::Reverse, out: Output { surface: true, ..Default::default() } });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies[0].kind, BodyKind::Sheet);
        let shapes = b.bodies[0].shapes();
        assert_eq!(shapes.len(), 2, "{shapes:?}");
        assert!(shapes.iter().all(|s| matches!(s, Shape::Polygon { thick, .. } if *thick == 0.0)));
    }

    #[test]
    fn hollowing_leaves_walls_or_just_surfaces() {
        let mut m = Model::default();
        let body = m.add(Kind::Solid { shape: Prim::Box { min: ["0".into(), "0".into(), "0".into()], max: ["20".into(), "20".into(), "20".into()] }, out: Output::default() });
        let top_face = face_tag(body, prims::END_HIGH);
        let mut solid = m.clone();
        solid.add(Kind::Shell { body, open: vec![top_face], thick: "2".into() });
        let b = solid.build(&vars());
        no_errors(&b);
        let exact = 0.02f64.powi(3) - 0.016 * 0.016 * 0.018;
        assert!(b.bodies[0].mesh.is_closed());
        assert!((b.bodies[0].mesh.volume() - exact).abs() / exact < 1e-6, "{} vs {exact}", b.bodies[0].mesh.volume());
        m.add(Kind::Shell { body, open: vec![top_face], thick: "0".into() });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies[0].kind, BodyKind::Sheet);
        assert!((b.bodies[0].mesh.area() - 5.0 * 4e-4).abs() < 1e-9);
        assert_eq!(b.bodies[0].shapes().len(), 5, "five flat walls");
    }

    #[test]
    fn a_probe_finds_the_metal_over_and_under_a_point() {
        let mut m = Model::default();
        let ground = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Sheet { sketch: ground, out: Output::default() });
        let mut up = plate_sketch("20", "20");
        let point = up.add_point([0.003, 0.0]);
        up.constraints.push(Constraint::DistanceU { a: ORIGIN, b: point, value: "Fx".into() });
        up.constraints.push(Constraint::DistanceV { a: ORIGIN, b: point, value: "0".into() });
        let patch = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "h".into() }, sketch: up });
        m.add(Kind::Sheet { sketch: patch, out: Output::default() });
        let id = m.add(Kind::Probe { sketch: patch, point, width: String::new(), z0: "50".into() });
        let mut v = vars();
        v.insert("Fx".into(), 0.004);
        let b = m.build(&v);
        no_errors(&b);
        assert_eq!(b.ports.len(), 1);
        let port = &b.ports[0];
        assert!((port.a[0] - 0.004).abs() < 1e-9 && port.a[2].abs() < 1e-12 && (port.b[2] - 0.002).abs() < 1e-12, "{port:?}");
        // The feed moves with its dimension.
        v.insert("Fx".into(), 0.006);
        assert!((m.build(&v).ports[0].a[0] - 0.006).abs() < 1e-9);
        // Off the patch there's metal at one height only.
        v.insert("Fx".into(), 0.015);
        let b = m.build(&v);
        assert_eq!(b.errors[0].0, id);
        assert!(b.errors[0].1.contains("one height"), "{:?}", b.errors);
    }

    #[test]
    fn a_feed_on_a_point_that_is_gone_says_so() {
        let mut m = Model::default();
        let mut s = plate_sketch("L", "W");
        let p = s.add_point([0.005, 0.0]);
        let sk = m.add(Kind::Sketch { plane: top(), sketch: s });
        m.add(Kind::Sheet { sketch: sk, out: Output::default() });
        let far = Anchor::Offset { sketch: sk, point: p, by: ["0".into(), "0".into(), "h".into()] };
        let feed = m.add(Kind::Feed { a: Anchor::Sketch { sketch: sk, point: p, height: String::new() }, b: far, width: String::new(), z0: "50".into() });
        let v = vars();
        let b = m.build(&v);
        no_errors(&b);
        assert!((b.ports[0].a[0] - 0.005).abs() < 1e-9 && (b.ports[0].b[2] - 0.002).abs() < 1e-9, "{:?}", b.ports[0]);
        m.sketch_mut(sk).unwrap().remove_point(p);
        let b = m.build(&v);
        assert_eq!(b.errors[0].0, feed);
        assert!(b.errors[0].1.contains("gone"), "{:?}", b.errors);
        assert!(b.ports.is_empty(), "not moved to the origin");
    }

    #[test]
    fn a_probe_stops_at_the_next_metal_rather_than_going_through_it() {
        let sheets = |h: &[f64]| -> Option<(f64, f64)> { probe_span(&h.iter().map(|x| (*x, *x)).collect::<Vec<_>>()) };
        assert_eq!(sheets(&[-0.002, 0.0, 0.003]), Some((-0.002, 0.0)), "a stacked patch: ground to driven, not to the parasitic");
        assert_eq!(sheets(&[-0.004, -0.002, 0.0]), Some((-0.002, 0.0)), "the near face of what's below");
        assert_eq!(sheets(&[0.0, 0.003]), Some((0.0, 0.003)), "nothing below, so what's above");
        assert_eq!(sheets(&[-0.002, 0.003]), Some((-0.002, 0.003)), "a point between two sheets");
        assert_eq!(sheets(&[0.0]), None);
        assert_eq!(sheets(&[0.003]), None);
    }

    #[test]
    fn a_cut_with_no_target_goes_to_the_latest_real_body() {
        let mut m = Model::default();
        let plate = m.add(Kind::Solid { shape: Prim::Box { min: ["-L/2".into(), "-W/2".into(), "0".into()], max: ["L/2".into(), "W/2".into(), "h".into()] }, out: Output::default() });
        m.add(Kind::Pattern { bodies: vec![plate], layout: Layout::Linear { step: ["100".into(), "0".into(), "0".into()], count: "2".into(), step2: None, count2: String::new() }, merge: false, feeds: Vec::new() });
        m.add(Kind::Wire { path: PathRef::Points { pts: vec![["0".into(), "50".into(), "0".into()], ["10".into(), "50".into(), "0".into()]] }, radius: "1".into(), out: Output::default() });
        m.add(Kind::Solid { shape: Prim::Cylinder { a: ["0".into(), "0".into(), "-1".into()], b: ["0".into(), "0".into(), "5".into()], r: "2".into() }, out: Output { mode: Mode::Cut, ..Default::default() } });
        let b = m.build(&vars());
        no_errors(&b);
        let original = b.bodies.iter().find(|x| x.id == plate).unwrap();
        assert!(original.mesh.volume() < 0.04 * 0.03 * 0.002 * 0.99, "the plate was cut, not its copy or the wire");
        let copy = b.bodies.iter().find(|x| x.copy).unwrap();
        assert!((copy.mesh.volume() - 0.04 * 0.03 * 0.002).abs() < 1e-12);
    }

    #[test]
    fn a_face_on_a_copy_is_that_copy_s_own() {
        let mut m = Model::default();
        let body = m.add(Kind::Solid { shape: Prim::Box { min: ["5".into(), "0".into(), "0".into()], max: ["15".into(), "10".into(), "10".into()] }, out: Output::default() });
        m.add(Kind::Mirror { bodies: vec![body], plane: PlaneRef::Base { axis: 0, offset: "0".into() }, merge: false });
        let v = vars();
        let b = m.build(&v);
        no_errors(&b);
        let copy = b.bodies.iter().find(|x| x.id != body).unwrap();
        let original = b.bodies.iter().find(|x| x.id == body).unwrap();
        for f in copy.mesh.face_ids() {
            assert!(!original.mesh.has_face(f), "{f}");
        }
        assert_eq!(copy.mesh.face_ids().len(), 6);
        // A sketch on it lands on the copy's plane, not the original's.
        let side = copy.mesh.face_ids().into_iter().find(|f| copy.mesh.plane_of(*f, 1e-9).is_some_and(|(o, n)| n[0].abs() > 0.9 && o[0] < -0.014)).unwrap();
        let meshes: Vec<&Mesh> = b.bodies.iter().map(|x| x.mesh.as_ref()).collect();
        let frame = (PlaneRef::Face { face: side, offset: "0".into() }).frame(&v, &meshes).unwrap();
        assert!((frame.o[0] + 0.015).abs() < 1e-9, "{:?}", frame.o);
    }

    #[test]
    fn joined_wires_are_all_solved_and_a_boolean_on_one_is_refused() {
        let mut m = Model::default();
        let wire = m.add(Kind::Wire { path: PathRef::Points { pts: vec![["0".into(), "0".into(), "0".into()], ["0".into(), "0".into(), "10".into()]] }, radius: "0.5".into(), out: Output::default() });
        m.add(Kind::Pattern { bodies: vec![wire], layout: Layout::Linear { step: ["20".into(), "0".into(), "0".into()], count: "3".into(), step2: None, count2: String::new() }, merge: true, feeds: Vec::new() });
        let b = m.build(&vars());
        no_errors(&b);
        assert_eq!(b.bodies.len(), 1);
        assert_eq!(b.solids("PEC").len(), 3, "what's drawn is what's solved");
        let box_ = m.add(Kind::Solid { shape: Prim::Box { min: ["50".into(), "0".into(), "0".into()], max: ["60".into(), "10".into(), "10".into()] }, out: Output::default() });
        let bad = m.add(Kind::Combine { op: CombineOp::Add, target: box_, tools: vec![wire], keep_tools: false });
        let b = m.build(&vars());
        assert_eq!(b.errors[0].0, bad);
        assert!(b.bodies.iter().any(|x| x.id == box_) && b.bodies.iter().any(|x| x.id == wire), "nothing lost");
    }

    #[test]
    fn a_fillet_on_a_sheet_and_a_shell_of_a_missing_face_are_refused() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        let sheet = m.add(Kind::Sheet { sketch: sk, out: Output::default() });
        let f = m.add(Kind::Fillet { body: sheet, edges: vec![[1, 2]], radius: "1".into(), chamfer: false });
        let b = m.build(&vars());
        assert_eq!(b.errors[0].0, f);
        assert!(b.errors[0].1.contains("solid"), "{:?}", b.errors);
        let mut m = Model::default();
        let body = m.add(Kind::Solid { shape: Prim::Box { min: ["0".into(), "0".into(), "0".into()], max: ["10".into(), "10".into(), "10".into()] }, out: Output::default() });
        let s = m.add(Kind::Shell { body, open: vec![face_tag(body, 900)], thick: "0".into() });
        let b = m.build(&vars());
        assert_eq!(b.errors[0].0, s);
        assert!(b.errors[0].1.contains("isn't on"), "{:?}", b.errors);
    }

    #[test]
    fn sections_with_different_corners_loft_to_a_closed_solid() {
        let mut m = Model::default();
        let mut round = Sketch::default();
        round.add_circle([0.0, 0.0], 0.01);
        let a = m.add(Kind::Sketch { plane: top(), sketch: round });
        let b = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 2, offset: "30".into() }, sketch: plate_sketch("30", "30") });
        m.add(Kind::Loft { sections: vec![a, b], out: Output::default() });
        let built = m.build(&vars());
        no_errors(&built);
        assert!(built.bodies[0].mesh.is_closed());
        assert!(built.bodies[0].mesh.volume() > 0.0);
    }

    #[test]
    fn touching_metal_that_won_t_share_a_mesh_is_found() {
        // Triangles on triangles, sharing only a face: no volume in common, but they touch.
        let mut m = Model::default();
        let cone = |m: &mut Model, z0: &str, z1: &str| m.add(Kind::Solid { shape: Prim::Cone { a: ["0".into(), "0".into(), z0.into()], b: ["0".into(), "0".into(), z1.into()], r0: "5".into(), r1: "5".into() }, out: Output::default() });
        cone(&mut m, "0", "10");
        cone(&mut m, "10", "20");
        assert_eq!(m.build(&vars()).touching_unjoined().len(), 1);
        // And two apart aren't.
        let mut m = Model::default();
        cone(&mut m, "0", "10");
        cone(&mut m, "11", "20");
        assert!(m.build(&vars()).touching_unjoined().is_empty());
        // Exact sheets are joined by the mesher, so they're no concern.
        let mut m = Model::default();
        let a = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Sheet { sketch: a, out: Output::default() });
        let c = m.add(Kind::Sketch { plane: PlaneRef::Base { axis: 1, offset: "0".into() }, sketch: plate_sketch("10", "10") });
        m.add(Kind::Sheet { sketch: c, out: Output::default() });
        assert!(m.build(&vars()).touching_unjoined().is_empty());
    }

    #[test]
    fn it_round_trips_through_json_and_a_project_without_one_still_opens() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::Both, out: Output { mode: Mode::New, material: "FR-4".into(), mesh: "1".into(), ..Default::default() } });
        let back: Model = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        let bare: Model = serde_json::from_str("{}").unwrap();
        assert!(bare.is_empty());
        assert_eq!(bare.facets, 32);
    }

    #[test]
    fn what_a_body_asks_of_a_mesh() {
        let mut m = Model::default();
        let sk = m.add(Kind::Sketch { plane: top(), sketch: plate_sketch("L", "W") });
        m.add(Kind::Extrude { sketch: sk, depth: "h".into(), extent: Extent::One, out: Output { mesh: "0.5".into(), ..Default::default() } });
        let b = m.build(&vars());
        let hints = b.hints(1e-6);
        assert_eq!(hints.len(), 1, "an exact outline needs no lines to find its faces");
        assert!((hints[0].cell - 0.0005).abs() < 1e-12);
        // Cut, it's triangles now, so its flat faces ask for lines of their own.
        m.add(Kind::Solid { shape: Prim::Cylinder { a: ["0".into(), "0".into(), "-1".into()], b: ["0".into(), "0".into(), "9".into()], r: "2".into() }, out: Output { mode: Mode::Cut, ..Default::default() } });
        let b = m.build(&vars());
        no_errors(&b);
        let hints = b.hints(1e-6);
        assert!(hints.len() >= 7, "{hints:?}");
        assert!(hints.iter().any(|h| h.min[2] == h.max[2] && (h.min[2] - 0.002).abs() < 1e-9), "the top face has its line");
    }
}
