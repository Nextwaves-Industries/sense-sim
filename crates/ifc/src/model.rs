//! Reads the IFC entity graph: units, relationships, materials and storeys, and
//! turns each building element into an [`Element`].

use crate::geometry::{Body, Geo};
use crate::math::*;
use crate::step::{Rec, Step, Value, attr};
use crate::{Building, Element, ElementKind};
use std::collections::{HashMap, HashSet};

/// How deep material and spatial relationships are followed.
const MAX_RELATION_DEPTH: usize = 64;
/// A parent element counts as covered by its parts within this margin, in metres.
const PARTS_COVER_MARGIN_M: f64 = 0.10;

/// How an element's kind is decided.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Base {
    Fixed(ElementKind),
    /// `Roof` if typed or aggregated as a roof, else `Slab`.
    Slab,
    /// `Window` if glazing, else `Wall`.
    Plate,
    /// A building element part takes its parent's kind.
    Part,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Ignored,
    Product(Base),
    Contained,
    Aggregates,
    Fills,
    Voids,
    AssociatesMaterial,
    DefinesByType,
    Project,
    BuildingEntity,
    UnitAssignment,
}

fn product_base(ty: &str) -> Option<Base> {
    use ElementKind::*;
    Some(match ty {
        "IFCWALL" | "IFCWALLSTANDARDCASE" | "IFCWALLELEMENTEDCASE" => Base::Fixed(Wall),
        "IFCSLAB" | "IFCSLABSTANDARDCASE" | "IFCSLABELEMENTEDCASE" => Base::Slab,
        "IFCROOF" => Base::Fixed(Roof),
        "IFCCOLUMN" | "IFCCOLUMNSTANDARDCASE" => Base::Fixed(Column),
        "IFCBEAM" | "IFCBEAMSTANDARDCASE" => Base::Fixed(Beam),
        "IFCDOOR" | "IFCDOORSTANDARDCASE" => Base::Fixed(Door),
        "IFCWINDOW" | "IFCWINDOWSTANDARDCASE" => Base::Fixed(Window),
        "IFCCOVERING" => Base::Fixed(Covering),
        // A curtain wall is mostly glazing and blocks radio like glass, not masonry.
        "IFCCURTAINWALL" => Base::Fixed(Window),
        "IFCPLATE" | "IFCPLATESTANDARDCASE" => Base::Plate,
        "IFCBUILDINGELEMENTPART" => Base::Part,
        "IFCMEMBER" | "IFCMEMBERSTANDARDCASE" | "IFCSTAIR" | "IFCSTAIRFLIGHT" | "IFCRAMP" | "IFCRAMPFLIGHT"
        | "IFCRAILING" | "IFCFOOTING" | "IFCPILE" | "IFCCHIMNEY" | "IFCSHADINGDEVICE"
        | "IFCBUILDINGELEMENTPROXY" => Base::Fixed(Other),
        _ => return None,
    })
}

fn role_of(ty: &str) -> Role {
    if let Some(base) = product_base(ty) {
        return Role::Product(base);
    }
    match ty {
        "IFCRELCONTAINEDINSPATIALSTRUCTURE" => Role::Contained,
        "IFCRELAGGREGATES" | "IFCRELNESTS" => Role::Aggregates,
        "IFCRELFILLSELEMENT" => Role::Fills,
        "IFCRELVOIDSELEMENT" => Role::Voids,
        "IFCRELASSOCIATESMATERIAL" => Role::AssociatesMaterial,
        "IFCRELDEFINESBYTYPE" => Role::DefinesByType,
        "IFCPROJECT" => Role::Project,
        "IFCBUILDING" => Role::BuildingEntity,
        "IFCUNITASSIGNMENT" => Role::UnitAssignment,
        _ => Role::Ignored,
    }
}

/// The relationship graph, as maps from child to parent. Linear to build. Where a
/// file relates an object twice, its first relationship wins.
#[derive(Default)]
struct Relations {
    /// Element to the spatial structure containing it.
    container: HashMap<u64, u64>,
    /// Part to whole, from `IfcRelAggregates` and `IfcRelNests`.
    parent: HashMap<u64, u64>,
    /// Door or window to the opening it fills.
    fills: HashMap<u64, u64>,
    /// Opening to the wall it cuts.
    voids: HashMap<u64, u64>,
    material: HashMap<u64, u64>,
    type_object: HashMap<u64, u64>,
}

fn link_many(map: &mut HashMap<u64, u64>, children: &Value<'_>, parent: &Value<'_>) {
    if let Some(parent) = parent.as_ref_id() {
        for child in children.refs() {
            map.entry(child).or_insert(parent);
        }
    }
}

impl Relations {
    fn add(&mut self, role: Role, args: &[Value<'_>]) {
        let (a, b) = (attr(args, 4), attr(args, 5));
        match role {
            Role::Contained => link_many(&mut self.container, a, b),
            Role::Aggregates => link_many(&mut self.parent, b, a),
            Role::Fills => link_many(&mut self.fills, b, a),
            Role::Voids => link_many(&mut self.voids, b, a),
            Role::AssociatesMaterial => link_many(&mut self.material, a, b),
            Role::DefinesByType => link_many(&mut self.type_object, a, b),
            _ => {}
        }
    }
}

fn si_prefix(prefix: &str) -> Option<f64> {
    Some(match prefix.to_ascii_uppercase().as_str() {
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 1e1,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        "PICO" => 1e-12,
        "FEMTO" => 1e-15,
        "ATTO" => 1e-18,
        _ => return None,
    })
}

/// Standard factor for a conversion-based unit whose definition is unusable.
fn named_unit(name: &str) -> Option<f64> {
    Some(match name.to_ascii_uppercase().as_str() {
        "INCH" | "IN" => 0.0254,
        "FOOT" | "FT" => 0.3048,
        "YARD" | "YD" => 0.9144,
        "MILE" => 1609.344,
        "DEGREE" | "DEG" => std::f64::consts::PI / 180.0,
        _ => return None,
    })
}

/// SI size of unit entity `id`, if it is a unit of type `want` such as `LENGTHUNIT`.
fn unit_factor(step: &Step<'_>, id: u64, want: &str, depth: usize) -> Option<f64> {
    if depth > 8 {
        return None;
    }
    let rec = step.rec(id)?;
    let args = step.args(rec).ok()?;
    if !attr(&args, 1).is_enum(want) {
        return None;
    }
    match step.type_name(rec) {
        "IFCSIUNIT" => match attr(&args, 2).as_enum() {
            None => Some(1.0),
            Some(prefix) => si_prefix(prefix),
        },
        "IFCCONVERSIONBASEDUNIT" | "IFCCONVERSIONBASEDUNITWITHOFFSET" => {
            let defined = attr(&args, 3).as_ref_id().and_then(|measure| {
                let measure = step.args(step.rec(measure)?).ok()?;
                let value = attr(&measure, 0).as_f64()?;
                // The value can be in any unit of the same type (0.3048 m, 304.8 mm).
                let base = attr(&measure, 1)
                    .as_ref_id()
                    .and_then(|unit| unit_factor(step, unit, want, depth + 1))
                    .unwrap_or(1.0);
                Some(value * base)
            });
            defined
                .filter(|f| f.is_finite() && *f > 0.0)
                .or_else(|| attr(&args, 2).as_text().and_then(|name| named_unit(&name)))
        }
        _ => None,
    }
}

/// Find a unit in the project's `IFCUNITASSIGNMENT`.
fn assigned_unit(step: &Step<'_>, assignment: Option<u64>, want: &str) -> Option<f64> {
    let args = step.args(step.rec(assignment?)?).ok()?;
    attr(&args, 0)
        .refs()
        .into_iter()
        .find_map(|unit| unit_factor(step, unit, want, 0))
        .filter(|f| f.is_finite() && *f > 0.0)
}

#[derive(Debug, Clone, Default)]
struct MaterialInfo {
    names: Vec<String>,
    /// Total of the material layers, in file units.
    layer_total: Option<f64>,
}

impl MaterialInfo {
    fn joined(&self) -> Option<String> {
        (!self.names.is_empty()).then(|| self.names.join("; "))
    }

    fn add_name(&mut self, value: &Value<'_>) {
        if let Some(name) = value.as_text() {
            let name = name.trim();
            if !name.is_empty() && !self.names.iter().any(|n| n == name) {
                self.names.push(name.to_string());
            }
        }
    }
}

/// Follow a `RelatingMaterial` select down to material names and layer thickness.
fn collect_material(step: &Step<'_>, id: u64, info: &mut MaterialInfo, depth: usize) {
    if depth > MAX_RELATION_DEPTH {
        return;
    }
    let Some(rec) = step.rec(id) else { return };
    let Ok(args) = step.args(rec) else { return };
    let each = |list: &Value<'_>, info: &mut MaterialInfo| {
        for child in list.refs() {
            collect_material(step, child, info, depth + 1);
        }
    };
    match step.type_name(rec) {
        "IFCMATERIAL" => info.add_name(attr(&args, 0)),
        "IFCMATERIALLIST" | "IFCMATERIALLAYERSETUSAGE" | "IFCMATERIALLAYERSET" | "IFCMATERIALPROFILESETUSAGE"
        | "IFCMATERIALPROFILESETUSAGETAPERING" => each(attr(&args, 0), info),
        "IFCMATERIALLAYER" | "IFCMATERIALLAYERWITHOFFSETS" => {
            each(attr(&args, 0), info);
            if let Some(thickness) = attr(&args, 1).as_f64().filter(|t| *t >= 0.0) {
                *info.layer_total.get_or_insert(0.0) += thickness;
            }
        }
        "IFCMATERIALCONSTITUENTSET" | "IFCMATERIALCONSTITUENT" | "IFCMATERIALPROFILESET" | "IFCMATERIALPROFILE"
        | "IFCMATERIALPROFILEWITHOFFSETS" => each(attr(&args, 2), info),
        _ => {}
    }
}

struct Model<'s, 'a> {
    step: &'s Step<'a>,
    geo: Geo<'s, 'a>,
    rels: Relations,
    /// Everything that is some other object's aggregate parent.
    has_parts: HashSet<u64>,
    unit: f64,
    materials: HashMap<u64, MaterialInfo>,
    storeys: HashMap<u64, Option<String>>,
}

impl Model<'_, '_> {
    fn material_info(&mut self, relating: u64) -> &MaterialInfo {
        let step = self.step;
        self.materials.entry(relating).or_insert_with(|| {
            let mut info = MaterialInfo::default();
            collect_material(step, relating, &mut info, 0);
            info
        })
    }

    /// Element's material: its own association, then its type's, then its parent's.
    /// A parent's layers describe the parent, so its thickness is not passed down.
    fn material_of(&mut self, id: u64) -> MaterialInfo {
        let own_type = self.rels.type_object.get(&id).copied();
        for candidate in [Some(id), own_type].into_iter().flatten() {
            if let Some(&relating) = self.rels.material.get(&candidate) {
                let info = self.material_info(relating).clone();
                if !info.names.is_empty() || info.layer_total.is_some() {
                    return info;
                }
            }
        }
        let parent = self.rels.parent.get(&id).copied();
        let parent_type = parent.and_then(|p| self.rels.type_object.get(&p).copied());
        for candidate in [parent, parent_type].into_iter().flatten() {
            if let Some(&relating) = self.rels.material.get(&candidate) {
                let info = self.material_info(relating).clone();
                if !info.names.is_empty() {
                    return MaterialInfo { names: info.names, layer_total: None };
                }
            }
        }
        MaterialInfo::default()
    }

    /// Storey name, found through containment. An element with no containment of
    /// its own inherits through its aggregate parent, or a door through the opening
    /// it fills and the wall that opening cuts.
    fn storey_of(&mut self, id: u64) -> Option<String> {
        let mut current = id;
        for _ in 0..MAX_RELATION_DEPTH {
            if let Some(&structure) = self.rels.container.get(&current) {
                return self.storey_above(structure);
            }
            let next = self.rels.parent.get(&current).or_else(|| self.rels.fills.get(&current));
            current = *next.or_else(|| self.rels.voids.get(&current))?;
        }
        None
    }

    /// Climb from a spatial structure (a space, say) to its building storey.
    fn storey_above(&mut self, structure: u64) -> Option<String> {
        if let Some(known) = self.storeys.get(&structure) {
            return known.clone();
        }
        let mut current = structure;
        let mut found = None;
        for _ in 0..MAX_RELATION_DEPTH {
            if self.step.type_of(current) == "IFCBUILDINGSTOREY" {
                found = self.storey_name(current);
                break;
            }
            let Some(&parent) = self.rels.parent.get(&current) else { break };
            current = parent;
        }
        self.storeys.insert(structure, found.clone());
        found
    }

    fn storey_name(&self, id: u64) -> Option<String> {
        let args = self.step.args(self.step.rec(id)?).ok()?;
        let name = [attr(&args, 2), attr(&args, 7)]
            .into_iter()
            .filter_map(Value::as_text)
            .find(|n| !n.trim().is_empty());
        Some(name.unwrap_or_else(|| format!("#{id}")))
    }

    fn parent_type(&self, id: u64) -> &str {
        self.rels.parent.get(&id).map_or("", |p| self.step.type_of(*p))
    }

    fn resolve_kind(&self, base: Base, id: u64, args: &[Value<'_>], material: &MaterialInfo) -> ElementKind {
        let parent = self.parent_type(id);
        match base {
            Base::Fixed(kind) => kind,
            Base::Slab => {
                if attr(args, 8).is_enum("ROOF") || parent == "IFCROOF" {
                    ElementKind::Roof
                } else {
                    ElementKind::Slab
                }
            }
            Base::Plate => {
                let glazing_parent = matches!(parent, "IFCCURTAINWALL" | "IFCWINDOW");
                let glazing_type = attr(args, 8).is_enum("CURTAIN_PANEL");
                let says_glass = material
                    .names
                    .iter()
                    .cloned()
                    .chain([attr(args, 2), attr(args, 4)].into_iter().filter_map(Value::as_text))
                    .any(|text| {
                        let text = text.to_ascii_lowercase();
                        text.contains("glas") || text.contains("glaz")
                    });
                if glazing_parent || glazing_type || says_glass {
                    ElementKind::Window
                } else {
                    ElementKind::Wall
                }
            }
            Base::Part => match product_base(parent) {
                Some(Base::Fixed(kind)) => kind,
                Some(Base::Slab) => ElementKind::Slab,
                Some(Base::Plate) => ElementKind::Wall,
                _ => ElementKind::Other,
            },
        }
    }

    fn element(&mut self, rec: &Rec, base: Base) -> Result<(Element, Vec<String>), Rejected> {
        let args = self.step.args(rec).map_err(|e| Rejected::new(format!("malformed entity: {e}")))?;
        let name = attr(&args, 2).as_text().unwrap_or_default();
        let describe = |reason: String| Rejected { name: name.clone(), reason };
        let representation = attr(&args, 6);
        if representation.is_null() {
            return Err(describe(if self.has_parts.contains(&rec.id) {
                "no geometry of its own; its aggregated parts are read separately".to_string()
            } else {
                "no representation".to_string()
            }));
        }
        let shape = representation
            .as_ref_id()
            .ok_or_else(|| describe("representation is not an entity reference".to_string()))?;
        let frame = self.geo.placement(attr(&args, 5)).map_err(describe)?;
        if !frame.is_finite() {
            return Err(describe("placement is not finite".to_string()));
        }
        let body = self.geo.product_shape(shape, &frame).map_err(describe)?;
        let unit = self.unit;
        let points: Vec<V3> = body.pts.iter().map(|p| mul(*p, unit)).collect();
        if !points.iter().flatten().all(|c| c.is_finite()) {
            return Err(describe("coordinates are not finite".to_string()));
        }
        let material = self.material_of(rec.id);
        let kind = self.resolve_kind(base, rec.id, &args, &material);
        let storey = self.storey_of(rec.id);
        let id = attr(&args, 0).as_text().filter(|g| !g.is_empty()).unwrap_or_else(|| format!("#{}", rec.id));
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for p in &points {
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        let fit = fit_plan(&points, frame_yaw(&frame));
        let size = [fit.size[0], fit.size[1], max[2] - min[2]];
        let element = Element {
            id,
            name,
            kind,
            storey,
            material: material.joined(),
            min,
            max,
            center: [fit.center[0], fit.center[1], (min[2] + max[2]) * 0.5],
            size,
            yaw_rad: fit.yaw,
            thickness_m: thickness(kind, &material, &body, size, unit),
            exact: body.exact && body.notes.is_empty() && body.is_upright(),
        };
        Ok((element, body.notes))
    }
}

/// An element that could not be read.
struct Rejected {
    name: String,
    reason: String,
}

impl Rejected {
    fn new(reason: String) -> Self {
        Rejected { name: String::new(), reason }
    }
}

/// Vertical extent counted as a slab's thickness only below this share of its long
/// side. Above that it is more likely the rise of a pitched roof than a thickness.
const FLAT_SLAB_MAX_RISE: f64 = 0.15;

/// Material layers are the designer's stated build-up, so they win when present.
/// Otherwise thickness comes from the geometry: the short plan side of a wall,
/// door or window; the extrusion depth of a slab or roof, which stays true on a
/// pitched roof where the vertical extent does not; and the smallest extent of a
/// covering, which can be a ceiling or a cladding. Without an extrusion depth a
/// slab or roof uses its vertical extent if it looks flat, and otherwise has none:
/// no number is better than the rise of a roof passed off as its thickness. Columns,
/// beams and other elements are not sheets, so they have none.
fn thickness(kind: ElementKind, material: &MaterialInfo, body: &Body, size: V3, unit: f64) -> Option<f64> {
    use ElementKind::*;
    let from_layers = material.layer_total.map(|t| t * unit).filter(|t| *t > 0.0);
    let from_geometry = match kind {
        Wall | Door | Window => Some(size[1]),
        Slab | Roof => body
            .depths
            .common()
            .map(|d| d * unit)
            .or_else(|| (size[2] <= size[0] * FLAT_SLAB_MAX_RISE).then_some(size[2])),
        Covering => Some(size[1].min(size[2])),
        Column | Beam | Other => None,
    };
    let chosen = match kind {
        Wall | Slab | Roof | Covering => from_layers.or(from_geometry),
        _ => from_geometry,
    };
    chosen.filter(|t| *t > 0.0)
}

/// Drop parents whose aggregated parts were read and cover them, because keeping
/// both would double-count the same material as a radio obstruction.
fn drop_covered_parents(
    loaded: &mut Vec<(u64, &str, Element)>,
    parents: &HashMap<u64, u64>,
    skipped: &mut Vec<(String, String)>,
) {
    let index: HashMap<u64, usize> = loaded.iter().enumerate().map(|(i, e)| (e.0, i)).collect();
    let mut parts_box: HashMap<u64, (V3, V3)> = HashMap::new();
    for (id, _, element) in loaded.iter() {
        if let Some(parent) = parents.get(id).filter(|p| index.contains_key(p)) {
            let entry = parts_box.entry(*parent).or_insert((element.min, element.max));
            for axis in 0..3 {
                entry.0[axis] = entry.0[axis].min(element.min[axis]);
                entry.1[axis] = entry.1[axis].max(element.max[axis]);
            }
        }
    }
    let covered: HashSet<u64> = parts_box
        .iter()
        .filter(|(parent, (min, max))| {
            index.get(parent).is_some_and(|&i| {
                let whole = &loaded[i].2;
                (0..3).all(|axis| {
                    whole.min[axis] >= min[axis] - PARTS_COVER_MARGIN_M
                        && whole.max[axis] <= max[axis] + PARTS_COVER_MARGIN_M
                })
            })
        })
        .map(|(parent, _)| *parent)
        .collect();
    loaded.retain(|(id, ty, element)| {
        let keep = !covered.contains(id);
        if !keep {
            skipped.push((
                (*ty).to_string(),
                format!("#{id} '{}': replaced by its aggregated parts", element.name),
            ));
        }
        keep
    });
}

/// Name for the building: the `IFCBUILDING`'s, then the project's.
fn building_name(step: &Step<'_>, building: Option<&Rec>, project: Option<&Rec>) -> String {
    let text = |rec: Option<&Rec>, attribute: usize| {
        let args = step.args(rec?).ok()?;
        attr(&args, attribute).as_text().filter(|t| !t.trim().is_empty())
    };
    text(building, 2)
        .or_else(|| text(building, 7))
        .or_else(|| text(project, 5))
        .or_else(|| text(project, 2))
        .unwrap_or_default()
}

/// Read every supported element from an indexed file.
pub(crate) fn build(step: &Step<'_>) -> Building {
    let mut skipped: Vec<(String, String)> = Vec::new();
    for problem in step.problems() {
        skipped.push(("STEP".to_string(), format!("malformed statement at {problem}")));
    }
    if step.problem_count() > step.problems().len() {
        skipped.push((
            "STEP".to_string(),
            format!("{} more malformed statements", step.problem_count() - step.problems().len()),
        ));
    }
    if step.duplicates() > 0 {
        skipped.push(("STEP".to_string(), format!("{} duplicate entity ids; the first of each was used", step.duplicates())));
    }

    let roles: Vec<Role> = step.type_names().iter().map(|name| role_of(name)).collect();
    let mut rels = Relations::default();
    let mut products: Vec<(&Rec, Base)> = Vec::new();
    let (mut project, mut building, mut unit_assignment) = (None, None, None);
    for rec in step.records() {
        match roles.get(rec.ty as usize).copied().unwrap_or(Role::Ignored) {
            Role::Ignored => {}
            Role::Product(base) => products.push((rec, base)),
            Role::Project => project = project.or(Some(rec)),
            Role::BuildingEntity => building = building.or(Some(rec)),
            Role::UnitAssignment => unit_assignment = unit_assignment.or(Some(rec.id)),
            role => match step.args(rec) {
                Ok(args) => rels.add(role, &args),
                Err(e) => skipped.push((step.type_name(rec).to_string(), format!("#{}: malformed relationship: {e}", rec.id))),
            },
        }
    }

    // Prefer the project's own assignment over any other in the file.
    let project_units = project.and_then(|p| step.args(p).ok()).and_then(|args| attr(&args, 8).as_ref_id());
    let assignment = project_units.or(unit_assignment);
    let unit_to_m = assigned_unit(step, assignment, "LENGTHUNIT").unwrap_or_else(|| {
        skipped.push(("IFCUNITASSIGNMENT".to_string(), "no usable length unit; assuming metres".to_string()));
        1.0
    });
    let angle_unit = assigned_unit(step, assignment, "PLANEANGLEUNIT").unwrap_or(1.0);

    let name = building_name(step, building, project);
    let mut model = Model {
        step,
        geo: Geo::new(step, angle_unit),
        has_parts: rels.parent.values().copied().collect(),
        rels,
        unit: unit_to_m,
        materials: HashMap::new(),
        storeys: HashMap::new(),
    };
    let mut loaded: Vec<(u64, &str, Element)> = Vec::with_capacity(products.len());
    for (rec, base) in &products {
        let ty = step.type_name(rec);
        match model.element(rec, *base) {
            Ok((element, notes)) => {
                for note in notes {
                    skipped.push((ty.to_string(), format!("#{} '{}': partial: {note}", rec.id, element.name)));
                }
                loaded.push((rec.id, ty, element));
            }
            Err(Rejected { name, reason }) => {
                skipped.push((ty.to_string(), format!("#{} '{name}': {reason}", rec.id)));
            }
        }
    }
    drop_covered_parents(&mut loaded, &model.rels.parent, &mut skipped);
    Building {
        name,
        unit_to_m,
        elements: loaded.into_iter().map(|(_, _, element)| element).collect(),
        skipped,
    }
}
