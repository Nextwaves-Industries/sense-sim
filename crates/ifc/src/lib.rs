//! Reads buildings from IFC (BIM) files for radio coverage prediction.
//!
//! Parses IFC2x3 and IFC4 STEP physical files (ISO 10303-21, text `.ifc`) into a
//! flat list of [`Element`]s: walls, slabs, roofs, columns, beams, doors, windows
//! and coverings, each with an oriented box, its storey and its material names.
//! It does not triangulate and does not apply opening cuts. An element is a box,
//! which is the level of detail a radio obstruction needs.
//!
//! ```no_run
//! let building = ifc::read_file(std::path::Path::new("house.ifc"))?;
//! for (kind, count) in building.counts() {
//!     println!("{kind:?}: {count}");
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # What each element means
//!
//! All coordinates are metres on the file's own axes (Z up). `TrueNorth`,
//! `IfcMapConversion` and the context's `WorldCoordinateSystem` are not applied,
//! so a georeferenced model stays wherever its placements put it.
//!
//! - `min` / `max` is the true world box of the element's points.
//! - `center`, `size` and `yaw_rad` are an upright oriented box: a box of `size`,
//!   rotated by `yaw_rad` about Z, centred on `center`. `size[0]` is the long plan
//!   side, `size[1]` the short plan side and `size[2]` the vertical extent. So for
//!   a wall, `size` reads (length, thickness, height), and for a slab (long side,
//!   short side, height).
//! - `yaw_rad` is the placement's X axis in plan, in (-pi, pi], unless another
//!   direction fits clearly better: a box at least 0.5 % smaller for a rectangular
//!   outline (a wall drawn in world coordinates under an identity placement, say),
//!   or 10 % smaller for any other, so that a faceted round column keeps its frame.
//!   If X turns out to be the short side, the yaw is turned by 90 degrees, because
//!   `size[0]` is always the long side.
//! - `thickness_m` comes from the material layers when they exist, else from the
//!   geometry: the short plan side for walls, doors and windows, the extrusion depth
//!   for slabs and roofs swept through their face, and the smallest extent for
//!   coverings. A slab or roof with no such depth uses its vertical extent if that
//!   is under 15 % of its long side, and otherwise has `None`, since the rise of a
//!   pitched roof is not a thickness. It is always `None` for columns, beams and
//!   `Other`.
//! - `material` is the distinct material names in file order, joined with `"; "`.
//!   Names can contain commas (Revit's `Concrete, Cast-in-Place gray`).
//!
//! # `exact`
//!
//! `exact: true` means `min`/`max` are analytically right for the modelled solid
//! (before openings), and the solid is upright so the oriented box fits it about
//! as well as a box can. `exact: false` means any of:
//!
//! - it came from facets (`IfcFacetedBrep`, `IfcTriangulatedFaceSet`,
//!   `IfcPolygonalFaceSet` and so on). The world box is still the true box of those
//!   vertices, but the solid was reduced to points.
//! - it came from an `IfcBoundingBox`.
//! - a boolean was reduced to its first operand, so a cut (such as a wall clipped
//!   under a roof) was ignored and the box may be too large.
//! - a curved outline was sampled, or a section (L, T, Z) was replaced by its
//!   bounding rectangle.
//! - some axis of the swept frame is tilted, like a pitched roof or a sloped beam.
//!   The world box stays right but the upright box over-covers the solid.
//! - part of the element's representation could not be read.
//!
//! A swept solid with a non-rectangular outline stays `exact`, because its extents
//! are exact, even though a box over-covers an L-shaped floor plate.
//!
//! # Geometry
//!
//! Solids: `IfcExtrudedAreaSolid` and `...Tapered`, `IfcRevolvedAreaSolid`,
//! `IfcSweptDiskSolid`, `IfcMappedItem`, `IfcBooleanResult` and
//! `IfcBooleanClippingResult`, `IfcCsgSolid` with `IfcBlock` and
//! `IfcRightCircularCylinder`, `IfcBoundingBox`, `IfcFacetedBrep`, `IfcAdvancedBrep`,
//! shells, `IfcFaceBasedSurfaceModel`, `IfcShellBasedSurfaceModel`,
//! `IfcTriangulatedFaceSet`, `IfcPolygonalFaceSet`.
//!
//! Profiles: rectangle (also hollow, rounded), circle (also hollow), ellipse,
//! I, asymmetric I, U, C, T, L, Z, arbitrary closed (also with voids), composite,
//! derived and mirrored. Curves: `IfcPolyline`, `IfcIndexedPolyCurve`,
//! `IfcCompositeCurve`, `IfcTrimmedCurve`, `IfcCircle`, `IfcEllipse`, B-splines (by
//! control points).
//!
//! # Which entities load as which kind
//!
//! - The standard-case variants map to their base entity.
//! - `IfcCurtainWall` is a `Window`, since it is mostly glazing and blocks radio
//!   like glass rather than like masonry.
//! - `IfcPlate` is a `Window` when it is glazing: aggregated by a curtain wall or
//!   window, typed `CURTAIN_PANEL`, or its name or material mentions glass.
//!   Otherwise it is a `Wall`, because an opaque panel blocks radio like a wall.
//! - `IfcSlab` typed `ROOF`, or aggregated by an `IfcRoof`, is a `Roof`.
//! - `IfcBuildingElementPart` takes its parent's kind.
//! - `IfcMember`, stairs, ramps, railings, footings, piles, chimneys, shading devices
//!   and `IfcBuildingElementProxy` are `Other`.
//!
//! An element whose aggregated parts were read and cover its box is dropped, so
//! that a decomposed roof or curtain wall does not count twice. Furniture, spaces
//! and MEP are not read.
//!
//! # `skipped`
//!
//! Entries are `(entity type, reason)`. For whole elements that could not be read,
//! the reason reads `#id 'name': why`. A reason of `#id 'name': partial: why` means
//! the element did load but some of its representation was ignored; it then has
//! `exact: false`. Type `STEP` reports malformed statements in the file itself and
//! `IFCUNITASSIGNMENT` a missing length unit.

#![forbid(unsafe_code)]

mod geometry;
mod math;
mod model;
mod step;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What an element is, for choosing its radio material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ElementKind {
    Wall,
    Slab,
    Roof,
    Column,
    Beam,
    Door,
    Window,
    Covering,
    Other,
}

/// One building element as an upright box. See the crate docs for the conventions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// `GlobalId`, or `#id` in a file that lacks one.
    pub id: String,
    pub name: String,
    pub kind: ElementKind,
    /// Name of the building storey containing it, directly or through a space,
    /// an aggregate parent, or the opening it fills.
    pub storey: Option<String>,
    /// Distinct material names, in file order, joined with `"; "`.
    pub material: Option<String>,
    /// World axis-aligned box, metres, IFC axes (Z up).
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Centre of the oriented box.
    pub center: [f64; 3],
    /// Oriented box extents before rotation: long plan side, short plan side,
    /// vertical extent. For a wall that is (length, thickness, height).
    pub size: [f64; 3],
    /// Rotation about Z of the oriented box, in (-pi, pi].
    pub yaw_rad: f64,
    pub thickness_m: Option<f64>,
    /// False if the geometry was approximated or came from a bounding box only.
    pub exact: bool,
}

/// A building read from one IFC file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Building {
    pub name: String,
    /// Metres per file length unit (0.001 for a millimetre file).
    pub unit_to_m: f64,
    pub elements: Vec<Element>,
    /// `(entity type, reason)` for what could not be read. See the crate docs.
    pub skipped: Vec<(String, String)>,
}

impl Building {
    /// World box around every element, or `None` for an empty building.
    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        let first = self.elements.first()?;
        let mut min = first.min;
        let mut max = first.max;
        for element in &self.elements {
            for axis in 0..3 {
                min[axis] = min[axis].min(element.min[axis]);
                max[axis] = max[axis].max(element.max[axis]);
            }
        }
        Some((min, max))
    }

    /// Number of elements of each kind that occurs, in `ElementKind` order.
    pub fn counts(&self) -> Vec<(ElementKind, usize)> {
        use ElementKind::*;
        [Wall, Slab, Roof, Column, Beam, Door, Window, Covering, Other]
            .into_iter()
            .filter_map(|kind| {
                let count = self.elements.iter().filter(|e| e.kind == kind).count();
                (count > 0).then_some((kind, count))
            })
            .collect()
    }
}

/// Read a building from the text of an IFC STEP file.
///
/// Returns an error only when the text is not an IFC STEP file at all. Anything
/// wrong inside a real one goes to [`Building::skipped`] and the rest still loads.
pub fn read(text: &str) -> Result<Building> {
    Ok(model::build(&step::Step::parse(text)?))
}

/// Read a building from an `.ifc` file.
///
/// The format is meant to be ASCII but some exporters write raw ISO 8859-1 bytes
/// in names, so text that is not valid UTF-8 is read as ISO 8859-1. Compressed
/// `.ifczip` and XML `.ifcxml` files are refused with a clear error.
pub fn read_file(path: &Path) -> Result<Building> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = text_from_bytes(bytes).with_context(|| format!("reading {}", path.display()))?;
    read(&text).with_context(|| format!("reading {}", path.display()))
}

fn text_from_bytes(bytes: Vec<u8>) -> Result<String> {
    if bytes.starts_with(b"PK") {
        bail!("this is a compressed .ifczip; extract the .ifc inside it first");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE]) {
        bail!("UTF-16 text is not a valid IFC STEP file");
    }
    if bytes.iter().take(256).copied().find(|b| !b.is_ascii_whitespace()) == Some(b'<') {
        bail!("this is XML (.ifcxml); only STEP (.ifc) files are supported");
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => error.into_bytes().into_iter().map(char::from).collect(),
    })
}
