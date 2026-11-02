//! Turns IFC placements and shape representations into world-space point sets.
//!
//! Every representation item reduces to points whose hull bounds the solid.
//! Coordinates stay in file units until the model layer scales them to metres.

use crate::math::*;
use crate::step::{Step, Value, attr};
use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;
use std::rc::Rc;

/// Deepest item recursion, for hostile mapped-item and composite-curve cycles.
/// Real files nest 2 or 3 levels.
const MAX_DEPTH: usize = 32;
/// Most points one element may gather. Mapped items can multiply geometry
/// exponentially, so this keeps a hostile file from exhausting memory.
const MAX_BODY_POINTS: usize = 5_000_000;
/// Most points all cached mapped representations may hold together.
const MAX_CACHED_POINTS: usize = 50_000_000;
/// Longest placement chain followed. Real files use around 5.
const MAX_PLACEMENT_CHAIN: usize = 1024;
/// Longest boolean chain followed. Clipped walls chain a few dozen at most.
const MAX_BOOLEAN_CHAIN: usize = 100_000;
/// Circles and arcs are sampled at this many points per turn.
const CIRCLE_SEGMENTS: usize = 64;
/// An extrusion counts as sweeping through a slab's face when it rises at least
/// this much per unit length: a slab up to 60 degrees from level.
const MIN_SHEET_SWEEP_RISE: f64 = 0.5;

/// Longest failure reason kept. Reasons nest through mapped items, so without
/// a limit they could grow exponentially.
const MAX_NOTE_LEN: usize = 300;
/// Most distinct failure reasons kept per body.
const MAX_NOTES: usize = 8;
/// Swept-frame directions kept per body before duplicates are removed.
const MAX_AXES: usize = 1024;

/// The extrusion depths in a body, measured normal to each profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Depths {
    None,
    Range(f64, f64),
    /// A non-uniform scale makes the depth direction-dependent.
    Unknown,
}

impl Depths {
    fn merge(self, other: Depths) -> Depths {
        match (self, other) {
            (Depths::Unknown, _) | (_, Depths::Unknown) => Depths::Unknown,
            (Depths::None, x) | (x, Depths::None) => x,
            (Depths::Range(a, b), Depths::Range(c, d)) => Depths::Range(a.min(c), b.max(d)),
        }
    }

    /// The depth, when all of the body's extrusions agree on one.
    pub(crate) fn common(self) -> Option<f64> {
        match self {
            Depths::Range(low, high) if high - low <= high.abs() * 0.01 => Some(high),
            _ => None,
        }
    }
}

/// The points of one element or mapped representation.
#[derive(Debug, Clone)]
pub(crate) struct Body {
    pub pts: Vec<V3>,
    /// Directions of the swept solids' frames, to detect tilted geometry.
    pub axes: Vec<V3>,
    pub depths: Depths,
    /// False once any item was approximated.
    pub exact: bool,
    /// Descriptions of items that could not be read.
    pub notes: Vec<String>,
}

/// `text` limited to `MAX_NOTE_LEN` bytes, cut at a character boundary.
fn clipped(mut text: String) -> String {
    if text.len() > MAX_NOTE_LEN {
        let mut end = MAX_NOTE_LEN;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

impl Body {
    fn new() -> Self {
        Body { pts: Vec::new(), axes: Vec::new(), depths: Depths::None, exact: true, notes: Vec::new() }
    }

    fn note(&mut self, reason: String) {
        let reason = clipped(reason);
        if self.notes.len() < MAX_NOTES && !self.notes.contains(&reason) {
            self.notes.push(reason);
        }
    }

    fn add_axes(&mut self, axes: impl IntoIterator<Item = V3>) {
        self.axes.extend(axes);
        if self.axes.len() > MAX_AXES {
            sort_points(&mut self.axes);
            self.axes.dedup();
            // Too many directions to be a building element. Giving up on them
            // only costs the `exact` flag, and bounds their memory.
            if self.axes.len() > MAX_AXES / 2 {
                self.axes.clear();
                self.exact = false;
            }
        }
    }

    fn extend_from(&mut self, other: &Body, xf: &Xf) -> Result<(), String> {
        if self.pts.len().saturating_add(other.pts.len()) > MAX_BODY_POINTS {
            return Err(format!("more than {MAX_BODY_POINTS} points in one element"));
        }
        self.pts.extend(other.pts.iter().map(|p| xf.point(*p)));
        self.add_axes(other.axes.iter().map(|a| xf.vector(*a)));
        let scale = length(xf.x);
        let uniform = (scale - length(xf.y)).abs() < 1e-9 && (scale - length(xf.z)).abs() < 1e-9;
        let depths = match other.depths {
            Depths::Range(low, high) if uniform => Depths::Range(low * scale, high * scale),
            Depths::Range(..) => Depths::Unknown,
            unchanged => unchanged,
        };
        self.depths = self.depths.merge(depths);
        self.exact &= other.exact;
        for note in &other.notes {
            self.note(note.clone());
        }
        Ok(())
    }

    /// True when every swept frame's axes are horizontal or vertical, so an
    /// upright box rotated about Z can describe the solid.
    pub(crate) fn is_upright(&self) -> bool {
        self.axes.iter().all(|axis| {
            let l = length(*axis);
            if l <= 0.0 {
                return true;
            }
            let z = (axis[2] / l).abs();
            z < 1e-3 || z > 1.0 - 1e-6
        })
    }
}

fn sort_points(points: &mut [V3]) {
    points.sort_unstable_by(|a, b| {
        a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])).then(a[2].total_cmp(&b[2]))
    });
}

/// A closed 2D outline, the footprint of a profile definition.
#[derive(Debug, Default)]
struct Profile {
    /// Outline corners, or points on it. A curve's arcs are sampled.
    pts: Vec<[f64; 2]>,
    ellipses: Vec<Ellipse>,
    approximate: bool,
}

/// A full ellipse: `c + a*cos(t) + b*sin(t)`.
#[derive(Debug, Clone, Copy)]
struct Ellipse {
    c: [f64; 2],
    a: [f64; 2],
    b: [f64; 2],
}

/// Reader for the placement and representation graph, with its caches.
pub(crate) struct Geo<'s, 'a> {
    step: &'s Step<'a>,
    /// Radians per file angle unit.
    angle_unit: f64,
    maps: HashMap<u64, Result<Rc<Body>, String>>,
    maps_in_progress: HashSet<u64>,
    cached_points: usize,
    placements: HashMap<u64, Xf>,
}

type Args<'a> = Vec<Value<'a>>;

fn reference(value: &Value<'_>, what: &str) -> Result<u64, String> {
    value.as_ref_id().ok_or_else(|| format!("{what} is not an entity reference"))
}

fn number(value: &Value<'_>, what: &str) -> Result<f64, String> {
    value.as_f64().ok_or_else(|| format!("{what} is not a finite number"))
}

fn positive(value: &Value<'_>, what: &str) -> Result<f64, String> {
    let n = number(value, what)?;
    if n < 0.0 {
        return Err(format!("{what} is negative ({n})"));
    }
    Ok(n)
}

/// Coordinates from a number list, padding a 2D point with z = 0.
fn coordinates(value: &Value<'_>) -> Option<V3> {
    let list = value.as_list()?;
    let x = list.first()?.as_f64()?;
    let y = list.get(1)?.as_f64()?;
    let z = match list.get(2) {
        Some(z) => z.as_f64()?,
        None => 0.0,
    };
    Some([x, y, z])
}

/// Points of an `IfcCartesianPointList2D/3D` coordinate list.
fn coordinate_list(value: &Value<'_>) -> Result<Vec<V3>, String> {
    let list = value.as_list().ok_or("coordinate list is missing")?;
    list.iter()
        .map(|row| coordinates(row).ok_or_else(|| "bad row in coordinate list".to_string()))
        .collect()
}

impl<'s, 'a> Geo<'s, 'a> {
    pub(crate) fn new(step: &'s Step<'a>, angle_unit: f64) -> Self {
        Geo {
            step,
            angle_unit,
            maps: HashMap::new(),
            maps_in_progress: HashSet::new(),
            cached_points: 0,
            placements: HashMap::new(),
        }
    }

    /// An entity's type and tokenised attributes.
    fn load(&self, id: u64) -> Result<(&'s str, Args<'a>), String> {
        let step: &'s Step<'a> = self.step;
        let rec = step.rec(id).ok_or_else(|| format!("dangling reference #{id}"))?;
        let ty = step.type_name(rec);
        let args = step.args(rec).map_err(|e| format!("{ty} #{id} is malformed: {e}"))?;
        Ok((ty, args))
    }

    fn point(&self, id: u64) -> Result<V3, String> {
        let (ty, args) = self.load(id)?;
        if ty != "IFCCARTESIANPOINT" {
            return Err(format!("#{id} is {ty}, expected IFCCARTESIANPOINT"));
        }
        coordinates(attr(&args, 0)).ok_or_else(|| format!("IFCCARTESIANPOINT #{id} has bad coordinates"))
    }

    fn point_or_origin(&self, value: &Value<'_>) -> Result<V3, String> {
        match value.as_ref_id() {
            Some(id) => self.point(id),
            None => Ok([0.0; 3]),
        }
    }

    /// An optional `IfcDirection` attribute. A direction that is unreadable
    /// counts as absent, so the IFC default applies instead of losing the element.
    fn direction(&self, value: &Value<'_>) -> Option<V3> {
        let (ty, args) = self.load(value.as_ref_id()?).ok()?;
        (ty == "IFCDIRECTION").then(|| coordinates(attr(&args, 0))).flatten()
    }

    /// An `IfcAxis2Placement2D/3D` reference; `$` is the identity.
    fn axis_placement(&self, value: &Value<'_>) -> Result<Xf, String> {
        if value.is_null() {
            return Ok(Xf::IDENTITY);
        }
        let id = reference(value, "axis placement")?;
        let (ty, args) = self.load(id)?;
        let frame = match ty {
            "IFCAXIS2PLACEMENT3D" => placement_frame(
                self.point_or_origin(attr(&args, 0))?,
                self.direction(attr(&args, 1)),
                self.direction(attr(&args, 2)),
            ),
            "IFCAXIS2PLACEMENT2D" => {
                placement_frame_2d(self.point_or_origin(attr(&args, 0))?, self.direction(attr(&args, 1)))
            }
            other => return Err(format!("#{id} is {other}, expected an axis placement")),
        };
        Ok(frame)
    }

    /// World frame of an `ObjectPlacement` attribute, following the chain of
    /// `IFCLOCALPLACEMENT`s. `$` is the identity. Results are cached per entity,
    /// so each link is computed once however many elements share it.
    pub(crate) fn placement(&mut self, value: &Value<'_>) -> Result<Xf, String> {
        if value.is_null() {
            return Ok(Xf::IDENTITY);
        }
        let first = reference(value, "object placement")?;
        let mut links: Vec<(u64, Xf)> = Vec::new();
        let mut next = Some(first);
        let mut base = Xf::IDENTITY;
        while let Some(id) = next {
            if let Some(known) = self.placements.get(&id) {
                base = *known;
                break;
            }
            if links.len() >= MAX_PLACEMENT_CHAIN || links.iter().any(|(seen, _)| *seen == id) {
                return Err(format!("placement chain through #{id} is cyclic or too long"));
            }
            let (ty, args) = self.load(id)?;
            if ty != "IFCLOCALPLACEMENT" {
                return Err(format!("unsupported object placement {ty}"));
            }
            links.push((id, self.axis_placement(attr(&args, 1))?));
            next = attr(&args, 0).as_ref_id();
        }
        for (id, local) in links.into_iter().rev() {
            base = base.then(&local);
            self.placements.insert(id, base);
        }
        Ok(base)
    }

    /// The solid body of an `IfcProductDefinitionShape`, in the frame `xf`.
    ///
    /// Tries representations from most to least faithful and uses the first that
    /// yields points: Body, then other 3D representations, then a Box. Axis,
    /// FootPrint and other 2D representations are never used as solids.
    pub(crate) fn product_shape(&mut self, shape: u64, xf: &Xf) -> Result<Body, String> {
        let (ty, args) = self.load(shape)?;
        // AutoCAD Architecture writes the abstract supertype, with the same attributes.
        if !matches!(ty, "IFCPRODUCTDEFINITIONSHAPE" | "IFCPRODUCTREPRESENTATION") {
            return Err(format!("representation #{shape} is {ty}"));
        }
        let mut ranked: Vec<(u8, Args<'a>)> = Vec::new();
        let mut ignored: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for rep in attr(&args, 2).refs() {
            match self.load(rep) {
                Ok((_, rep_args)) => {
                    let id = attr(&rep_args, 1).as_text().unwrap_or_default();
                    let kind = attr(&rep_args, 2).as_text().unwrap_or_default();
                    match representation_rank(&id, &kind) {
                        Some(rank) => ranked.push((rank, rep_args)),
                        None => ignored.push(format!("{id}/{kind}")),
                    }
                }
                Err(reason) => failures.push(reason),
            }
        }
        ranked.sort_by_key(|(rank, _)| *rank);
        for (rank, rep_args) in ranked {
            let mut body = Body::new();
            for item in attr(&rep_args, 3).refs() {
                if let Err(reason) = self.item(item, xf, 0, &mut body) {
                    body.note(reason);
                }
            }
            if body.pts.is_empty() {
                failures.append(&mut body.notes);
                continue;
            }
            if rank >= RANK_BOX {
                body.exact = false;
            }
            for failure in failures {
                body.note(failure);
            }
            return Ok(body);
        }
        if failures.is_empty() {
            Err(if ignored.is_empty() {
                "shape has no representations".to_string()
            } else {
                format!("no 3D body, only {}", ignored.join(", "))
            })
        } else {
            failures.sort();
            failures.dedup();
            Err(clipped(failures.join("; ")))
        }
    }

    /// Add one representation item's points, transformed by `xf`, to `out`.
    fn item(&mut self, id: u64, xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(format!("items nested deeper than {MAX_DEPTH} levels"));
        }
        let (ty, args) = self.load(id)?;
        match ty {
            "IFCEXTRUDEDAREASOLID" => self.extruded(&args, xf, depth, out),
            "IFCEXTRUDEDAREASOLIDTAPERED" => {
                // Both end profiles swept the full depth: a box around the taper.
                out.exact = false;
                self.extruded(&args, xf, depth, out)?;
                let mut end_args = args;
                if end_args.len() > 4 {
                    end_args.swap(0, 4);
                    self.extruded(&end_args, xf, depth, out)?;
                }
                Ok(())
            }
            "IFCREVOLVEDAREASOLID" => self.revolved(&args, xf, depth, out),
            "IFCSWEPTDISKSOLID" => self.swept_disk(&args, xf, depth, out),
            "IFCMAPPEDITEM" => self.mapped(&args, xf, depth, out),
            "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT" => self.boolean(args, xf, depth, out),
            "IFCCSGSOLID" => {
                out.exact = false;
                self.item(reference(attr(&args, 0), "CSG tree")?, xf, depth + 1, out)
            }
            "IFCBLOCK" => {
                let m = xf.then(&self.axis_placement(attr(&args, 0))?);
                let size = [
                    positive(attr(&args, 1), "XLength")?,
                    positive(attr(&args, 2), "YLength")?,
                    positive(attr(&args, 3), "ZLength")?,
                ];
                push_box(&m, [0.0; 3], size, out);
                out.add_axes([m.x, m.y, m.z]);
                Ok(())
            }
            "IFCRIGHTCIRCULARCYLINDER" => {
                let m = xf.then(&self.axis_placement(attr(&args, 0))?);
                let height = positive(attr(&args, 1), "Height")?;
                let r = positive(attr(&args, 2), "Radius")?;
                let profile = Profile {
                    ellipses: vec![Ellipse { c: [0.0; 2], a: [r, 0.0], b: [0.0, r] }],
                    ..Profile::default()
                };
                sweep(&m, &profile, [0.0, 0.0, height], out);
                out.add_axes([m.x, m.y, m.z]);
                Ok(())
            }
            "IFCBOUNDINGBOX" => {
                out.exact = false;
                let corner = self.point_or_origin(attr(&args, 0))?;
                let size = [
                    positive(attr(&args, 1), "XDim")?,
                    positive(attr(&args, 2), "YDim")?,
                    positive(attr(&args, 3), "ZDim")?,
                ];
                push_box(xf, corner, size, out);
                Ok(())
            }
            "IFCFACETEDBREP" | "IFCFACETEDBREPWITHVOIDS" | "IFCADVANCEDBREP" | "IFCADVANCEDBREPWITHVOIDS" => {
                out.exact = false;
                self.shell(reference(attr(&args, 0), "Outer shell")?, xf, out)
            }
            "IFCCLOSEDSHELL" | "IFCOPENSHELL" | "IFCCONNECTEDFACESET" => self.faces(attr(&args, 0), xf, out),
            "IFCFACEBASEDSURFACEMODEL" | "IFCSHELLBASEDSURFACEMODEL" => {
                out.exact = false;
                for shell in attr(&args, 0).refs() {
                    self.shell(shell, xf, out)?;
                }
                Ok(())
            }
            "IFCTRIANGULATEDFACESET" | "IFCTRIANGULATEDIRREGULARNETWORK" | "IFCPOLYGONALFACESET" => {
                out.exact = false;
                let (list_ty, list) = self.load(reference(attr(&args, 0), "Coordinates")?)?;
                if !list_ty.starts_with("IFCCARTESIANPOINTLIST") {
                    return Err(format!("face set coordinates are {list_ty}"));
                }
                out.pts.extend(coordinate_list(attr(&list, 0))?.into_iter().map(|p| xf.point(p)));
                Ok(())
            }
            other => Err(format!("unsupported representation item {other}")),
        }
    }

    fn extruded(&mut self, args: &[Value<'a>], xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        let profile = self.profile(reference(attr(args, 0), "SweptArea")?, depth + 1)?;
        let position = self.axis_placement(attr(args, 1))?;
        let direction = self
            .direction(attr(args, 2))
            .and_then(unit)
            .ok_or("extrusion direction is missing or zero")?;
        let distance = positive(attr(args, 3), "Depth")?;
        let m = xf.then(&position);
        sweep(&m, &profile, mul(direction, distance), out);
        out.exact &= !profile.approximate;
        out.add_axes([m.x, m.y, m.vector(direction)]);
        // The depth says how thick a sheet is only when it is swept through its
        // face. A roof swept along its ridge has the ridge's length here instead.
        let world = m.vector(direction);
        let swept_through_face = world[2].abs() >= MIN_SHEET_SWEEP_RISE * length(world);
        let normal_depth = distance * direction[2].abs() * length(m.z);
        out.depths = out.depths.merge(if swept_through_face {
            Depths::Range(normal_depth, normal_depth)
        } else {
            Depths::Unknown
        });
        Ok(())
    }

    /// `IfcRevolvedAreaSolid`: the profile's points rotated about the axis in
    /// steps of at most `TAU / CIRCLE_SEGMENTS`. The hull is a close approximation.
    fn revolved(&mut self, args: &[Value<'a>], xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        let profile = self.profile(reference(attr(args, 0), "SweptArea")?, depth + 1)?;
        let m = xf.then(&self.axis_placement(attr(args, 1))?);
        let (axis_ty, axis_args) = self.load(reference(attr(args, 2), "Axis")?)?;
        if axis_ty != "IFCAXIS1PLACEMENT" {
            return Err(format!("revolution axis is {axis_ty}"));
        }
        let origin = self.point_or_origin(attr(&axis_args, 0))?;
        let axis = self.direction(attr(&axis_args, 1)).and_then(unit).unwrap_or(EZ);
        let angle = number(attr(args, 3), "Angle")? * self.angle_unit;
        let steps = ((angle.abs() / TAU * CIRCLE_SEGMENTS as f64).ceil() as usize).clamp(1, CIRCLE_SEGMENTS);
        let mut outline = Vec::new();
        outline_points(&profile, &mut outline);
        for step in 0..=steps {
            let t = angle * step as f64 / steps as f64;
            let (s, c) = t.sin_cos();
            for p in &outline {
                // Rodrigues' rotation about the axis through `origin`.
                let v = sub([p[0], p[1], 0.0], origin);
                let rotated = add(
                    add(mul(v, c), mul(cross(axis, v), s)),
                    mul(axis, dot(axis, v) * (1.0 - c)),
                );
                out.pts.push(m.point(add(rotated, origin)));
            }
        }
        out.exact = false;
        Ok(())
    }

    /// `IfcSweptDiskSolid`: a box of the disk's radius around each directrix point.
    fn swept_disk(&mut self, args: &[Value<'a>], xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        let mut line = Vec::new();
        let mut approximate = false;
        self.curve(reference(attr(args, 0), "Directrix")?, depth + 1, &mut line, &mut approximate)?;
        let r = positive(attr(args, 1), "Radius")?;
        for p in line {
            push_box(xf, sub(p, [r, r, r]), [2.0 * r; 3], out);
        }
        out.exact = false;
        Ok(())
    }

    /// `IfcMappedItem`. The instance transform is `MappingTarget * MappingOrigin`
    /// with no inverse of the origin, matching IfcOpenShell (`mapping::map_impl`),
    /// because that is how the geometry consumers read these files in practice.
    fn mapped(&mut self, args: &[Value<'a>], xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        let source = reference(attr(args, 0), "MappingSource")?;
        let (ty, map_args) = self.load(source)?;
        if ty != "IFCREPRESENTATIONMAP" {
            return Err(format!("mapping source is {ty}"));
        }
        let origin = self.axis_placement(attr(&map_args, 0))?;
        let target = self.transform_operator(attr(args, 1))?;
        let body = self.map_body(source, &map_args, depth + 1)?;
        out.extend_from(&body, &xf.then(&target).then(&origin))
    }

    /// A mapped representation's points in its own frame, computed once. Repeated
    /// instances (say 5000 identical windows) only pay for a transform.
    fn map_body(&mut self, id: u64, map_args: &[Value<'a>], depth: usize) -> Result<Rc<Body>, String> {
        if let Some(known) = self.maps.get(&id) {
            return known.clone();
        }
        if !self.maps_in_progress.insert(id) {
            return Err(format!("representation map #{id} contains itself"));
        }
        let built = self.build_map(attr(map_args, 1), depth);
        self.maps_in_progress.remove(&id);
        let result = built.and_then(|mut body| {
            // Faces share vertices, so caching only distinct points saves a lot.
            sort_points(&mut body.pts);
            body.pts.dedup();
            sort_points(&mut body.axes);
            body.axes.dedup();
            if self.cached_points.saturating_add(body.pts.len()) > MAX_CACHED_POINTS {
                return Err(format!("mapped geometry exceeds {MAX_CACHED_POINTS} points"));
            }
            self.cached_points += body.pts.len();
            Ok(Rc::new(body))
        });
        self.maps.insert(id, result.clone());
        result
    }

    fn build_map(&mut self, representation: &Value<'_>, depth: usize) -> Result<Body, String> {
        let (_, rep_args) = self.load(reference(representation, "MappedRepresentation")?)?;
        let mut body = Body::new();
        for item in attr(&rep_args, 3).refs() {
            if let Err(reason) = self.item(item, &Xf::IDENTITY, depth, &mut body) {
                body.note(reason);
            }
        }
        if body.pts.is_empty() {
            return Err(if body.notes.is_empty() {
                "mapped representation has no items".to_string()
            } else {
                clipped(body.notes.join("; "))
            });
        }
        Ok(body)
    }

    /// `IfcCartesianTransformationOperator2D/3D`, optionally non-uniform.
    fn transform_operator(&self, value: &Value<'_>) -> Result<Xf, String> {
        let Some(id) = value.as_ref_id() else {
            return Ok(Xf::IDENTITY);
        };
        let (ty, args) = self.load(id)?;
        let scale = attr(&args, 3).as_f64().unwrap_or(1.0);
        let (axis3, scale2, scale3) = match ty {
            "IFCCARTESIANTRANSFORMATIONOPERATOR3D" => (self.direction(attr(&args, 4)), scale, scale),
            "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM" => (
                self.direction(attr(&args, 4)),
                attr(&args, 5).as_f64().unwrap_or(scale),
                attr(&args, 6).as_f64().unwrap_or(scale),
            ),
            "IFCCARTESIANTRANSFORMATIONOPERATOR2D" => (None, scale, 1.0),
            "IFCCARTESIANTRANSFORMATIONOPERATOR2DNONUNIFORM" => {
                (None, attr(&args, 4).as_f64().unwrap_or(scale), 1.0)
            }
            other => return Err(format!("unsupported mapping target {other}")),
        };
        Ok(operator_frame(
            self.direction(attr(&args, 0)),
            self.direction(attr(&args, 1)),
            axis3,
            self.point_or_origin(attr(&args, 2))?,
            [scale, scale2, scale3],
        ))
    }

    /// Boolean results reduce to their first operand, or both for a union. Cuts
    /// are not applied, so the box can be larger than the solid and it is not exact.
    /// Revit clips walls under roofs this way, in chains, so this walks them in a
    /// loop instead of recursing.
    fn boolean(&mut self, mut args: Args<'a>, xf: &Xf, depth: usize, out: &mut Body) -> Result<(), String> {
        out.exact = false;
        for _ in 0..MAX_BOOLEAN_CHAIN {
            if attr(&args, 0).is_enum("UNION") {
                self.item(reference(attr(&args, 2), "SecondOperand")?, xf, depth + 1, out)?;
            }
            let first = reference(attr(&args, 1), "FirstOperand")?;
            let (ty, next) = self.load(first)?;
            if !matches!(ty, "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT") {
                return self.item(first, xf, depth + 1, out);
            }
            args = next;
        }
        Err("boolean chain is cyclic or too long".to_string())
    }

    /// A shell's vertices. A vertex shared by many faces is read once.
    fn shell(&mut self, id: u64, xf: &Xf, out: &mut Body) -> Result<(), String> {
        let (ty, args) = self.load(id)?;
        match ty {
            "IFCCLOSEDSHELL" | "IFCOPENSHELL" | "IFCCONNECTEDFACESET" => self.faces(attr(&args, 0), xf, out),
            other => Err(format!("unsupported shell {other}")),
        }
    }

    fn faces(&mut self, list: &Value<'_>, xf: &Xf, out: &mut Body) -> Result<(), String> {
        out.exact = false;
        let mut seen: HashSet<u64> = HashSet::new();
        for face in list.refs() {
            let (_, face_args) = self.load(face)?;
            for bound in attr(&face_args, 0).refs() {
                let (_, bound_args) = self.load(bound)?;
                let (loop_ty, loop_args) = self.load(reference(attr(&bound_args, 0), "face bound")?)?;
                match loop_ty {
                    "IFCPOLYLOOP" => {
                        for point in attr(&loop_args, 0).refs() {
                            if seen.insert(point) {
                                out.pts.push(xf.point(self.point(point)?));
                            }
                        }
                    }
                    "IFCVERTEXLOOP" => {
                        self.vertex(attr(&loop_args, 0), xf, &mut seen, out)?;
                    }
                    "IFCEDGELOOP" => {
                        for oriented in attr(&loop_args, 0).refs() {
                            let (_, edge_args) = self.load(oriented)?;
                            let (_, curve_args) = self.load(reference(attr(&edge_args, 2), "EdgeElement")?)?;
                            self.vertex(attr(&curve_args, 0), xf, &mut seen, out)?;
                            self.vertex(attr(&curve_args, 1), xf, &mut seen, out)?;
                        }
                    }
                    other => return Err(format!("unsupported face loop {other}")),
                }
            }
            if out.pts.len() > MAX_BODY_POINTS {
                return Err(format!("more than {MAX_BODY_POINTS} points in one element"));
            }
        }
        Ok(())
    }

    fn vertex(&self, value: &Value<'_>, xf: &Xf, seen: &mut HashSet<u64>, out: &mut Body) -> Result<(), String> {
        let id = reference(value, "vertex")?;
        if seen.insert(id) {
            let (_, args) = self.load(id)?;
            let point = self.point(reference(attr(&args, 0), "VertexGeometry")?)?;
            out.pts.push(xf.point(point));
        }
        Ok(())
    }

    /// Outline of a profile definition in its own plane.
    fn profile(&mut self, id: u64, depth: usize) -> Result<Profile, String> {
        if depth > MAX_DEPTH {
            return Err(format!("profiles nested deeper than {MAX_DEPTH} levels"));
        }
        let (ty, args) = self.load(id)?;
        let mut profile = Profile::default();
        match ty {
            // Hollow and rounded sections share the solid rectangle's outer envelope
            // (a rounded corner makes it very slightly larger than the real section).
            "IFCRECTANGLEPROFILEDEF" | "IFCROUNDEDRECTANGLEPROFILEDEF" | "IFCRECTANGLEHOLLOWPROFILEDEF" => {
                let (w, h) = (positive(attr(&args, 3), "XDim")?, positive(attr(&args, 4), "YDim")?);
                self.rectangle(&args, w, h, &mut profile)?;
            }
            "IFCCIRCLEPROFILEDEF" | "IFCCIRCLEHOLLOWPROFILEDEF" => {
                let r = positive(attr(&args, 3), "Radius")?;
                self.ellipse(&args, r, r, &mut profile)?;
            }
            "IFCELLIPSEPROFILEDEF" => {
                let (a, b) = (positive(attr(&args, 3), "SemiAxis1")?, positive(attr(&args, 4), "SemiAxis2")?);
                self.ellipse(&args, a, b, &mut profile)?;
            }
            // Structural sections become their bounding rectangle, centred as the
            // schema defines. All four corners of I, U and C sections lie on the
            // section, so their extents are exact in any frame.
            "IFCISHAPEPROFILEDEF" => {
                let (w, h) = (positive(attr(&args, 3), "OverallWidth")?, positive(attr(&args, 4), "OverallDepth")?);
                self.rectangle(&args, w, h, &mut profile)?;
            }
            "IFCASYMMETRICISHAPEPROFILEDEF" => {
                let bottom = positive(attr(&args, 3), "OverallWidth")?;
                let top = attr(&args, 8).as_f64().unwrap_or(bottom);
                let h = positive(attr(&args, 4), "OverallDepth")?;
                self.rectangle(&args, bottom.max(top), h, &mut profile)?;
            }
            "IFCUSHAPEPROFILEDEF" => {
                let (h, w) = (positive(attr(&args, 3), "Depth")?, positive(attr(&args, 4), "FlangeWidth")?);
                self.rectangle(&args, w, h, &mut profile)?;
            }
            "IFCCSHAPEPROFILEDEF" => {
                let (h, w) = (positive(attr(&args, 3), "Depth")?, positive(attr(&args, 4), "Width")?);
                self.rectangle(&args, w, h, &mut profile)?;
            }
            // The corners of L, T and Z sections are not all on the section.
            "IFCTSHAPEPROFILEDEF" => {
                let (h, w) = (positive(attr(&args, 3), "Depth")?, positive(attr(&args, 4), "FlangeWidth")?);
                self.rectangle(&args, w, h, &mut profile)?;
                profile.approximate = true;
            }
            "IFCLSHAPEPROFILEDEF" => {
                let h = positive(attr(&args, 3), "Depth")?;
                let w = attr(&args, 4).as_f64().unwrap_or(h);
                self.rectangle(&args, w, h, &mut profile)?;
                profile.approximate = true;
            }
            "IFCZSHAPEPROFILEDEF" => {
                let h = positive(attr(&args, 3), "Depth")?;
                let flange = positive(attr(&args, 4), "FlangeWidth")?;
                let web = attr(&args, 5).as_f64().unwrap_or(0.0);
                self.rectangle(&args, 2.0 * flange - web, h, &mut profile)?;
                profile.approximate = true;
            }
            // Inner voids do not change the outer envelope.
            "IFCARBITRARYCLOSEDPROFILEDEF" | "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                let mut line = Vec::new();
                let curve = reference(attr(&args, 2), "OuterCurve")?;
                self.curve(curve, depth + 1, &mut line, &mut profile.approximate)?;
                profile.pts.extend(line.iter().map(|p| [p[0], p[1]]));
            }
            "IFCCOMPOSITEPROFILEDEF" => {
                for part in attr(&args, 2).refs() {
                    let inner = self.profile(part, depth + 1)?;
                    profile.pts.extend(inner.pts);
                    profile.ellipses.extend(inner.ellipses);
                    profile.approximate |= inner.approximate;
                }
            }
            "IFCDERIVEDPROFILEDEF" | "IFCMIRROREDPROFILEDEF" => {
                let parent = self.profile(reference(attr(&args, 2), "ParentProfile")?, depth + 1)?;
                let op = self.transform_operator(attr(&args, 3))?;
                place_profile(&op, parent, &mut profile);
            }
            other => return Err(format!("unsupported profile {other}")),
        }
        if profile.pts.is_empty() && profile.ellipses.is_empty() {
            return Err(format!("{ty} #{id} has no outline"));
        }
        Ok(profile)
    }

    fn rectangle(&self, args: &[Value<'a>], w: f64, h: f64, out: &mut Profile) -> Result<(), String> {
        let m = self.axis_placement(attr(args, 2))?;
        let (x, y) = (w * 0.5, h * 0.5);
        for corner in [[-x, -y, 0.0], [x, -y, 0.0], [x, y, 0.0], [-x, y, 0.0]] {
            let p = m.point(corner);
            out.pts.push([p[0], p[1]]);
        }
        Ok(())
    }

    fn ellipse(&self, args: &[Value<'a>], a: f64, b: f64, out: &mut Profile) -> Result<(), String> {
        let m = self.axis_placement(attr(args, 2))?;
        let (c, ax, bx) = (m.point([0.0; 3]), m.vector([a, 0.0, 0.0]), m.vector([0.0, b, 0.0]));
        out.ellipses.push(Ellipse { c: [c[0], c[1]], a: [ax[0], ax[1]], b: [bx[0], bx[1]] });
        Ok(())
    }

    /// Points on a curve, in its own frame (z = 0 for 2D curves). `approximate`
    /// is set when curved parts are sampled or bounded by control points.
    fn curve(&mut self, id: u64, depth: usize, out: &mut Vec<V3>, approximate: &mut bool) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err(format!("curves nested deeper than {MAX_DEPTH} levels"));
        }
        let (ty, args) = self.load(id)?;
        match ty {
            "IFCPOLYLINE" => {
                for point in attr(&args, 0).refs() {
                    out.push(self.point(point)?);
                }
                Ok(())
            }
            "IFCINDEXEDPOLYCURVE" => {
                let (list_ty, list) = self.load(reference(attr(&args, 0), "Points")?)?;
                if !list_ty.starts_with("IFCCARTESIANPOINTLIST") {
                    return Err(format!("polycurve points are {list_ty}"));
                }
                out.extend(coordinate_list(attr(&list, 0))?);
                let has_arc = attr(&args, 1).as_list().is_some_and(|segments| {
                    segments.iter().any(|s| matches!(s, Value::Typed(name, _) if name.eq_ignore_ascii_case("IFCARCINDEX")))
                });
                *approximate |= has_arc;
                Ok(())
            }
            "IFCCOMPOSITECURVE" | "IFC2DCOMPOSITECURVE" | "IFCCOMPOSITECURVEONSURFACE" => {
                for segment in attr(&args, 0).refs() {
                    let (_, segment_args) = self.load(segment)?;
                    let parent = reference(attr(&segment_args, 2), "ParentCurve")?;
                    self.curve(parent, depth + 1, out, approximate)?;
                }
                Ok(())
            }
            "IFCTRIMMEDCURVE" => self.trimmed(&args, depth, out, approximate),
            "IFCCIRCLE" => {
                let m = self.axis_placement(attr(&args, 0))?;
                let r = positive(attr(&args, 1), "Radius")?;
                *approximate = true;
                sample_arc(&m, r, r, 0.0, TAU, out);
                Ok(())
            }
            "IFCELLIPSE" => {
                let m = self.axis_placement(attr(&args, 0))?;
                let a = positive(attr(&args, 1), "SemiAxis1")?;
                let b = positive(attr(&args, 2), "SemiAxis2")?;
                *approximate = true;
                sample_arc(&m, a, b, 0.0, TAU, out);
                Ok(())
            }
            // A B-spline lies inside the hull of its control points.
            "IFCBSPLINECURVEWITHKNOTS" | "IFCRATIONALBSPLINECURVEWITHKNOTS" | "IFCBSPLINECURVE" => {
                *approximate = true;
                for point in attr(&args, 1).refs() {
                    out.push(self.point(point)?);
                }
                Ok(())
            }
            other => Err(format!("unsupported curve {other}")),
        }
    }

    fn trimmed(&mut self, args: &[Value<'a>], depth: usize, out: &mut Vec<V3>, approximate: &mut bool) -> Result<(), String> {
        let basis = reference(attr(args, 0), "BasisCurve")?;
        let (ty, basis_args) = self.load(basis)?;
        let (trim1, param1) = self.trim(attr(args, 1))?;
        let (trim2, param2) = self.trim(attr(args, 2))?;
        match ty {
            "IFCLINE" => {
                let origin = self.point(reference(attr(&basis_args, 0), "Pnt")?)?;
                let (_, vector) = self.load(reference(attr(&basis_args, 1), "Dir")?)?;
                let direction = self.direction(attr(&vector, 0)).and_then(unit).ok_or("line has no direction")?;
                let magnitude = attr(&vector, 1).as_f64().unwrap_or(1.0);
                for (point, param) in [(trim1, param1), (trim2, param2)] {
                    out.push(match (point, param) {
                        (Some(point), _) => point,
                        (None, Some(t)) => add(origin, mul(direction, t * magnitude)),
                        (None, None) => return Err("trimmed line has no trim".to_string()),
                    });
                }
                Ok(())
            }
            "IFCCIRCLE" | "IFCELLIPSE" => {
                let m = self.axis_placement(attr(&basis_args, 0))?;
                let a = positive(attr(&basis_args, 1), "radius")?;
                let b = if ty == "IFCCIRCLE" { a } else { positive(attr(&basis_args, 2), "SemiAxis2")? };
                let angle = |point: Option<V3>, param: Option<f64>| -> Result<f64, String> {
                    if let Some(point) = point {
                        // Prefer the Cartesian trim: it needs no angle unit.
                        let v = sub(point, m.t);
                        let (u, w) = (dot(v, m.x) / a.max(1e-12), dot(v, m.y) / b.max(1e-12));
                        return Ok(w.atan2(u));
                    }
                    param.map(|t| t * self.angle_unit).ok_or_else(|| "trimmed arc has no trim".to_string())
                };
                let start = angle(trim1, param1)?;
                let end = angle(trim2, param2)?;
                let forward = !attr(args, 3).is_enum("F");
                let sweep = if forward { (end - start).rem_euclid(TAU) } else { -(start - end).rem_euclid(TAU) };
                let sweep = if sweep == 0.0 { TAU } else { sweep };
                *approximate = true;
                sample_arc(&m, a, b, start, sweep, out);
                Ok(())
            }
            // Trimming a polyline or composite only shortens it: use all of it.
            _ => {
                *approximate = true;
                self.curve(basis, depth + 1, out, approximate)
            }
        }
    }

    /// A trim select `(IFCPARAMETERVALUE(0.), #point)` as (point, parameter).
    fn trim(&self, value: &Value<'_>) -> Result<(Option<V3>, Option<f64>), String> {
        let single = std::slice::from_ref(value);
        let entries = value.as_list().unwrap_or(single);
        let (mut point, mut param) = (None, None);
        for entry in entries {
            match entry {
                Value::Ref(id) => point = Some(self.point(*id)?),
                Value::Typed(..) | Value::Real(_) | Value::Int(_) => param = entry.as_f64().or(param),
                _ => {}
            }
        }
        Ok((point, param))
    }
}

/// How faithful a representation is as a solid; lower is better. `None` means
/// never use it as a solid, for 2D and helper representations.
fn representation_rank(identifier: &str, kind: &str) -> Option<u8> {
    let identifier = identifier.to_ascii_lowercase();
    let kind = kind.to_ascii_lowercase();
    match identifier.as_str() {
        "body" => return Some(0),
        "facetation" | "mesh" => return Some(1),
        "box" => return Some(RANK_BOX),
        "axis" | "footprint" | "profile" | "annotation" | "plan" | "reference" | "cog" | "clearance"
        | "lighting" | "survey" | "foundation" => return None,
        _ => {}
    }
    match kind.as_str() {
        "boundingbox" => Some(RANK_BOX),
        "curve" | "curve2d" | "curve3d" | "geometricset" | "geometriccurveset" | "annotation2d" | "point"
        | "pointcloud" | "fillarea" | "surface2d" => None,
        _ => Some(2),
    }
}

/// Ranks from here on are bounding boxes, so never exact.
const RANK_BOX: u8 = 4;

/// Add the 8 corners of a box given its minimum corner and size.
fn push_box(m: &Xf, corner: V3, size: V3, out: &mut Body) {
    for i in 0..8u8 {
        let pick = |bit: u8, axis: usize| if i & (1 << bit) != 0 { size[axis] } else { 0.0 };
        out.pts.push(m.point(add(corner, [pick(0, 0), pick(1, 1), pick(2, 2)])));
    }
}

/// Sweep a profile by `vector` (given in the profile's frame): both end outlines.
fn sweep(m: &Xf, profile: &Profile, vector: V3, out: &mut Body) {
    for offset in [[0.0; 3], vector] {
        for p in &profile.pts {
            out.pts.push(m.point(add([p[0], p[1], 0.0], offset)));
        }
        for e in &profile.ellipses {
            let center = m.point(add([e.c[0], e.c[1], 0.0], offset));
            let a = m.vector([e.a[0], e.a[1], 0.0]);
            let b = m.vector([e.b[0], e.b[1], 0.0]);
            for k in 0..CIRCLE_SEGMENTS {
                let (s, c) = (k as f64 / CIRCLE_SEGMENTS as f64 * TAU).sin_cos();
                out.pts.push(add(center, add(mul(a, c), mul(b, s))));
            }
            // The exact extreme point along each world axis keeps the world box
            // exact in any frame; samples alone would fall slightly short.
            for axis in 0..3 {
                let reach = (a[axis] * a[axis] + b[axis] * b[axis]).sqrt();
                if reach > 0.0 {
                    let offset = add(mul(a, a[axis] / reach), mul(b, b[axis] / reach));
                    out.pts.push(add(center, offset));
                    out.pts.push(sub(center, offset));
                }
            }
        }
    }
}

/// All of a profile's outline as plain points, sampling its ellipses.
fn outline_points(profile: &Profile, out: &mut Vec<[f64; 2]>) {
    out.extend_from_slice(&profile.pts);
    for e in &profile.ellipses {
        for k in 0..CIRCLE_SEGMENTS {
            let (s, c) = (k as f64 / CIRCLE_SEGMENTS as f64 * TAU).sin_cos();
            out.push([e.c[0] + e.a[0] * c + e.b[0] * s, e.c[1] + e.a[1] * c + e.b[1] * s]);
        }
    }
}

/// Apply a 2D operator to a parent profile.
fn place_profile(op: &Xf, parent: Profile, out: &mut Profile) {
    let on_plane = |p: V3| [p[0], p[1]];
    out.pts.extend(parent.pts.iter().map(|p| on_plane(op.point([p[0], p[1], 0.0]))));
    out.ellipses.extend(parent.ellipses.iter().map(|e| Ellipse {
        c: on_plane(op.point([e.c[0], e.c[1], 0.0])),
        a: on_plane(op.vector([e.a[0], e.a[1], 0.0])),
        b: on_plane(op.vector([e.b[0], e.b[1], 0.0])),
    }));
    out.approximate |= parent.approximate;
}

/// Points along an elliptical arc starting at angle `start` and turning by `sweep`.
fn sample_arc(m: &Xf, a: f64, b: f64, start: f64, sweep: f64, out: &mut Vec<V3>) {
    let steps = ((sweep.abs() / TAU * CIRCLE_SEGMENTS as f64).ceil() as usize).clamp(1, CIRCLE_SEGMENTS);
    for k in 0..=steps {
        let (s, c) = (start + sweep * k as f64 / steps as f64).sin_cos();
        out.push(m.point([a * c, b * s, 0.0]));
    }
}
