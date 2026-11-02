//! Connection points ("ports") of an asset: pipe nozzles, shafts, pulleys
//! and conveyor ends, in the object's local frame (Y up, meters, bottom
//! centered, the same frame as `Prototype::aabb_*`).
//!
//! Sources, first match wins:
//! 1. generated plant parts (`parts::Spec`): exact, from their parameters;
//! 2. the catalog (`assets/plant/catalog.json`, then
//!    `%APPDATA%/SenseSim/plant/catalog.json` overriding per asset folder);
//! 3. auto-detection on the mesh at load time ([`detect`]): open circular rims
//!    of fluid equipment, revolute joint shafts, conveyor ends; with
//!    "inferred" fallbacks for pumps, tanks and valves that have no rims.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::parts::Spec;
use crate::prototype::{ProtoJoint, Prototype};
use crate::usd_load::{JointKind, MeshPart};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    /// Flanged pipe end: mates face to face with another pipe port.
    Pipe,
    /// Shaft end: couples coaxially with another shaft (ratio 1).
    Shaft,
    /// Pulley: joined to another pulley by a belt (ratio = diameter ratio).
    Pulley,
    /// Conveyor ends: items leave `conveyor_out` onto the next `conveyor_in`.
    ConveyorIn,
    ConveyorOut,
}

impl PortKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "pipe" | "nozzle" | "flange" => Self::Pipe,
            "shaft" | "coupling" => Self::Shaft,
            "pulley" | "belt" | "sprocket" => Self::Pulley,
            "conveyor_in" | "in" => Self::ConveyorIn,
            "conveyor_out" | "out" => Self::ConveyorOut,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::Shaft => "shaft",
            Self::Pulley => "pulley",
            Self::ConveyorIn => "conveyor_in",
            Self::ConveyorOut => "conveyor_out",
        }
    }
    pub fn color(self) -> Color {
        match self {
            Self::Pipe => Color::srgb(0.25, 0.6, 1.0),
            Self::Shaft => Color::srgb(1.0, 0.75, 0.2),
            Self::Pulley => Color::srgb(1.0, 0.55, 0.15),
            Self::ConveyorIn | Self::ConveyorOut => Color::srgb(0.6, 0.9, 0.35),
        }
    }
    /// Whether two port kinds can be joined, and how.
    pub fn mates(a: Self, b: Self) -> Option<super::connect::LinkKind> {
        use super::connect::LinkKind as L;
        match (a, b) {
            (Self::Pipe, Self::Pipe) => Some(L::Pipe),
            (Self::Shaft, Self::Shaft) => Some(L::Shaft),
            (Self::Pulley, Self::Pulley) => Some(L::Belt),
            (Self::ConveyorOut, Self::ConveyorIn) | (Self::ConveyorIn, Self::ConveyorOut) => Some(L::Conveyor),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSource {
    Generated,
    Catalog,
    Detected,
    /// Placed by convention (no geometry found): check it visually.
    Inferred,
}

#[derive(Clone, Debug)]
pub struct Port {
    pub id: String,
    pub kind: PortKind,
    /// Local position of the port face center.
    pub pos: Vec3,
    /// Local outward unit direction (pipe: out of the nozzle; shaft: along
    /// the shaft away from the body; conveyor: the belt's travel direction).
    pub dir: Vec3,
    /// Nominal diameter in meters (pipe bore, shaft, pulley, belt width).
    pub size: f32,
    pub source: PortSource,
}

impl Port {
    pub fn new(id: &str, kind: PortKind, pos: Vec3, dir: Vec3, size: f32, source: PortSource) -> Self {
        Self { id: id.into(), kind, pos, dir: dir.normalize_or(Vec3::X), size, source }
    }
    pub fn world(&self, t: &Transform) -> (Vec3, Vec3) {
        (t.transform_point(self.pos), (t.rotation * self.dir).normalize_or(Vec3::X))
    }
}

// ---------------------------------------------------------------------------
// Roles

/// What an object does in a connected system.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Drive source with a crank (pistons follow the crank angle).
    Engine,
    /// Drive source (electric motor).
    Motor,
    /// Lifts liquid; driven by its own motor or through its shaft.
    Pump,
    /// Rotating consumer (compressor, supercharger, fan, alternator).
    Rotor,
    /// Holds liquid.
    Tank,
    Valve,
    /// Passive piping (pipes, elbows, tees, reducers, hoses).
    Fitting,
    Conveyor,
    Other,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Motor => "motor",
            Self::Pump => "pump",
            Self::Rotor => "rotor",
            Self::Tank => "tank",
            Self::Valve => "valve",
            Self::Fitting => "fitting",
            Self::Conveyor => "conveyor",
            Self::Other => "other",
        }
    }
    /// Whether the object can be switched on and drive others.
    pub fn is_source(self) -> bool {
        matches!(self, Self::Engine | Self::Motor | Self::Pump | Self::Conveyor)
    }
    pub fn is_fluid(self) -> bool {
        matches!(self, Self::Pump | Self::Tank | Self::Valve | Self::Fitting)
    }
}

/// Asset folder name (`.../<folder>/<file>.usd` or `.../<folder>/simready_usd/<file>.usd`).
pub fn folder_of(asset: &Path) -> String {
    let mut p = asset.parent();
    while let Some(d) = p {
        let n = d.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if n != "simready_usd" && n != "payloads" && !n.is_empty() {
            return n;
        }
        p = d.parent();
    }
    asset.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// Role from the asset name, for assets the catalog doesn't describe.
pub fn role_from_name(asset: &Path) -> Role {
    let n = folder_of(asset).to_ascii_lowercase();
    let has = |k: &str| n.contains(k);
    if has("conveyor") {
        Role::Conveyor
    } else if has("engineassembly") {
        Role::Engine
    } else if has("motor") && !has("motorcycle") {
        Role::Motor
    } else if has("pump") && !has("pumpkin") {
        Role::Pump
    } else if has("compressor") || has("supercharger") || has("alternator") || (has("fan") && !has("fan_shroud")) {
        Role::Rotor
    } else if (has("tank") || has("reservoir") || has("ibc")) && !has("tanktop") {
        Role::Tank
    } else if has("valve") && !has("lifter") && !has("rocker") {
        Role::Valve
    } else if has("pipe") && !has("wrench") || has("hose_") {
        Role::Fitting
    } else {
        Role::Other
    }
}

/// Whether the loader should look for open circular rims (costly) on this asset.
pub fn wants_rims(asset: &Path) -> bool {
    role_from_name(asset).is_fluid() || catalog().contains_key(&folder_of(asset))
}

// ---------------------------------------------------------------------------
// Detection (runs off the main thread, while vertex data is available)

/// An open circular edge loop: a pipe end or nozzle.
#[derive(Clone, Debug)]
pub struct Rim {
    pub center: Vec3,
    /// Points out of the opening (away from the part's bulk).
    pub normal: Vec3,
    pub radius: f32,
}

/// Geometry of a movable joint's moving body, in the local frame.
#[derive(Clone, Debug, Default)]
pub struct JointGeom {
    pub anchor: Vec3,
    pub axis: Vec3,
    /// Largest distance of the moving parts from the axis.
    pub radius: f32,
    /// Extent of the moving parts along the axis, relative to the anchor.
    pub span: (f32, f32),
    pub continuous: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Detected {
    pub rims: Vec<Rim>,
    /// One per movable joint, in `Prototype::joints` order.
    pub joints: Vec<JointGeom>,
}

/// Finds rims (when `rims` is set) and measures joint bodies. `basis` maps
/// stage space to the local frame; `part_body[i]` is the body of part i.
pub fn detect(parts: &[MeshPart], part_body: &[Option<usize>], links: &[ProtoJoint], basis: Mat4, rims: bool) -> Detected {
    let mut out = Detected::default();
    let rot = Mat3::from_mat4(basis);
    for j in links.iter().filter(|j| j.kind != JointKind::Fixed) {
        let anchor = basis.transform_point3(j.anchor);
        let axis = (rot * j.axis).normalize_or(Vec3::X);
        let mut radius: f32 = 0.0;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for (p, b) in parts.iter().zip(part_body) {
            if *b != Some(j.child) {
                continue;
            }
            let m = basis * Mat4::from_cols_array(&p.world);
            // Subsample big meshes: the extent is what matters.
            let step = (p.positions.len() / 4000).max(1);
            for v in p.positions.iter().step_by(step) {
                let w = m.transform_point3(Vec3::from_array(*v)) - anchor;
                let along = w.dot(axis);
                lo = lo.min(along);
                hi = hi.max(along);
                radius = radius.max((w - axis * along).length());
            }
        }
        if lo > hi {
            (lo, hi) = (0.0, 0.0);
        }
        let continuous = j.kind == JointKind::Revolute && j.lower.is_none() && j.upper.is_none();
        out.joints.push(JointGeom { anchor, axis, radius, span: (lo, hi), continuous });
    }
    if rims {
        for p in parts {
            if p.indices.len() / 3 > 400_000 {
                continue;
            }
            let m = basis * Mat4::from_cols_array(&p.world);
            let pts: Vec<Vec3> = p.positions.iter().map(|v| m.transform_point3(Vec3::from_array(*v))).collect();
            out.rims.extend(rims_of(&pts, &p.indices));
        }
        out.rims = dedupe_rims(out.rims);
    }
    out
}

/// Open circular boundary loops of one triangle mesh.
pub fn rims_of(pts: &[Vec3], indices: &[u32]) -> Vec<Rim> {
    if pts.is_empty() || indices.len() < 3 {
        return Vec::new();
    }
    // Weld vertices split by normals/UVs.
    let (mn, mx) = pts.iter().fold((Vec3::MAX, Vec3::MIN), |(a, b), p| (a.min(*p), b.max(*p)));
    let q = ((mx - mn).max_element() * 1e-5).max(1e-6);
    let mut weld: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut id = Vec::with_capacity(pts.len());
    let mut wp: Vec<Vec3> = Vec::new();
    for p in pts {
        let k = ((p.x / q).round() as i64, (p.y / q).round() as i64, (p.z / q).round() as i64);
        let n = *weld.entry(k).or_insert_with(|| {
            wp.push(*p);
            wp.len() as u32 - 1
        });
        id.push(n);
    }
    let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
    for t in indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (a, b) = (id[a as usize], id[b as usize]);
            if a == b {
                continue;
            }
            *edges.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&(a, b), &n) in &edges {
        if n == 1 {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    let centroid = wp.iter().copied().sum::<Vec3>() / wp.len() as f32;
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut out = Vec::new();
    let starts: Vec<u32> = adj.keys().copied().collect();
    for s in starts {
        if seen.contains(&s) || adj[&s].len() != 2 {
            continue;
        }
        // Walk the loop.
        let mut lp = vec![s];
        seen.insert(s);
        let (mut prev, mut cur) = (s, adj[&s][0]);
        let mut closed = false;
        for _ in 0..100_000 {
            if cur == s {
                closed = true;
                break;
            }
            if seen.contains(&cur) || adj.get(&cur).map(|v| v.len()) != Some(2) {
                break;
            }
            seen.insert(cur);
            lp.push(cur);
            let nb = &adj[&cur];
            let next = if nb[0] == prev { nb[1] } else { nb[0] };
            prev = cur;
            cur = next;
        }
        if !closed || lp.len() < 8 {
            continue;
        }
        let ring: Vec<Vec3> = lp.iter().map(|&i| wp[i as usize]).collect();
        if let Some(mut r) = fit_circle(&ring) {
            if r.normal.dot(r.center - centroid) < 0.0 {
                r.normal = -r.normal;
            }
            out.push(r);
        }
    }
    out
}

/// Fits a planar circle to a closed ring of points.
pub fn fit_circle(ring: &[Vec3]) -> Option<Rim> {
    let n = ring.len() as f32;
    let c = ring.iter().copied().sum::<Vec3>() / n;
    // Newell's method for the plane normal.
    let mut nrm = Vec3::ZERO;
    for i in 0..ring.len() {
        let (a, b) = (ring[i] - c, ring[(i + 1) % ring.len()] - c);
        nrm += a.cross(b);
    }
    let nrm = nrm.try_normalize()?;
    let radii: Vec<f32> = ring.iter().map(|p| (*p - c).reject_from_normalized(nrm).length()).collect();
    let r = radii.iter().sum::<f32>() / n;
    if !(0.004..=0.8).contains(&r) {
        return None;
    }
    let var = radii.iter().map(|x| (x - r) * (x - r)).sum::<f32>() / n;
    let flat = ring.iter().map(|p| (*p - c).dot(nrm).abs()).fold(0.0f32, f32::max);
    (var.sqrt() < 0.08 * r && flat < 0.1 * r).then_some(Rim { center: c, normal: nrm, radius: r })
}

/// Concentric rims (inner and outer edge of one pipe wall, flange faces) collapse into
/// one, sized as the smallest: that's the bore. Keeping a flange's outside diameter
/// instead cut friction 170 times over. At most 8 remain, biggest nozzle first.
fn dedupe_rims(mut rims: Vec<Rim>) -> Vec<Rim> {
    rims.sort_by(|a, b| b.radius.total_cmp(&a.radius));
    let mut keep: Vec<(Rim, f32)> = Vec::new();
    for r in rims {
        let same = keep.iter().position(|(k, outer)| {
            let d = r.center - k.center;
            k.normal.dot(r.normal).abs() > 0.95 && d.reject_from_normalized(k.normal).length() < 0.5 * outer && d.dot(k.normal).abs() < 2.0 * outer
        });
        match same {
            // Far smaller than the opening it sits in is a pin hole, not the bore.
            Some(i) => {
                if r.radius >= 0.2 * keep[i].1 {
                    keep[i].0.radius = keep[i].0.radius.min(r.radius);
                }
            }
            None => {
                let outer = r.radius;
                keep.push((r, outer));
            }
        }
    }
    keep.truncate(8);
    keep.into_iter().map(|(r, _)| r).collect()
}

// ---------------------------------------------------------------------------
// Catalog

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogPort {
    pub id: String,
    pub kind: String,
    /// Local position in meters...
    pub pos: Option<[f32; 3]>,
    /// ...or as fractions (0..1) of the asset's bounding box.
    pub at: Option<[f32; 3]>,
    pub dir: [f32; 3],
    pub diameter_m: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogEntry {
    pub role: Option<Role>,
    pub ports: Vec<CatalogPort>,
    /// Rated speed when switched on.
    pub rpm: Option<f32>,
    /// Pump: shut-off head (m) and maximum flow (l/s).
    pub head_m: Option<f32>,
    pub max_flow_lps: Option<f32>,
    /// Tank volume in liters.
    pub capacity_l: Option<f32>,
    /// Joint speed ratios to the main shaft, by joint name (negative turns
    /// the other way). Joints not listed follow their size.
    pub joint_ratios: HashMap<String, f32>,
    pub notes: Option<String>,
}

fn user_catalog_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("SenseSim").join("plant").join("catalog.json")
}

/// Entries of a catalog file; keys starting with `_` are comments, and a
/// malformed entry is skipped rather than losing the whole file.
fn parse_catalog(text: &str) -> HashMap<String, CatalogEntry> {
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(text) else { return HashMap::new() };
    m.into_iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .filter_map(|(k, v)| serde_json::from_value::<CatalogEntry>(v).ok().map(|e| (k, e)))
        .collect()
}

fn load_catalog() -> HashMap<String, CatalogEntry> {
    let mut map = parse_catalog(include_str!("../../assets/plant/catalog.json"));
    if let Ok(txt) = std::fs::read_to_string(user_catalog_path()) {
        map.extend(parse_catalog(&txt));
    }
    map
}

static CATALOG: OnceLock<Mutex<Arc<HashMap<String, CatalogEntry>>>> = OnceLock::new();

pub fn catalog() -> Arc<HashMap<String, CatalogEntry>> {
    CATALOG.get_or_init(|| Mutex::new(Arc::new(load_catalog()))).lock().unwrap().clone()
}

/// Adds or replaces one port of an asset in the user catalog.
pub fn define_port(folder: &str, port: CatalogPort, role: Option<Role>) -> anyhow::Result<()> {
    let path = user_catalog_path();
    let mut user = std::fs::read_to_string(&path).map(|t| parse_catalog(&t)).unwrap_or_default();
    let builtin = load_catalog();
    let e = user.entry(folder.to_string()).or_insert_with(|| builtin.get(folder).cloned().unwrap_or_default());
    e.ports.retain(|p| p.id != port.id);
    e.ports.push(port);
    if role.is_some() {
        e.role = role;
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&user)?)?;
    *CATALOG.get_or_init(|| Mutex::new(Arc::new(HashMap::new()))).lock().unwrap() = Arc::new(load_catalog());
    PORTS.get_or_init(Default::default).lock().unwrap().clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution

/// Everything the plant needs to know about an asset.
#[derive(Clone, Debug)]
pub struct Info {
    pub role: Role,
    pub ports: Vec<Port>,
    pub spec: Option<Spec>,
    pub entry: Option<CatalogEntry>,
}

impl Info {
    pub fn port(&self, id: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.id.eq_ignore_ascii_case(id))
    }
}

static PORTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Info>>>> = OnceLock::new();

/// Ports and role of a loaded asset (cached per asset path).
pub fn info(asset: &Path, proto: &Prototype) -> Arc<Info> {
    let cache = PORTS.get_or_init(Default::default);
    if let Some(i) = cache.lock().unwrap().get(asset) {
        return i.clone();
    }
    let i = Arc::new(resolve(asset, proto.aabb_min, proto.aabb_max, &proto.detected));
    cache.lock().unwrap().insert(asset.to_path_buf(), i.clone());
    i
}

/// Ports and role from an asset's local bounds and detected geometry.
pub fn resolve(asset: &Path, mn: Vec3, mx: Vec3, det: &Detected) -> Info {
    if let Some(spec) = Spec::of_asset(asset) {
        return Info { role: spec.role(), ports: spec.ports(), spec: Some(spec), entry: None };
    }
    let cat = catalog();
    let entry = cat.get(&folder_of(asset)).cloned();
    let role = entry.as_ref().and_then(|e| e.role).unwrap_or_else(|| role_from_name(asset));
    if let Some(e) = &entry
        && !e.ports.is_empty()
    {
        let ports = e
            .ports
            .iter()
            .filter_map(|p| {
                let kind = PortKind::parse(&p.kind)?;
                let pos = match (p.pos, p.at) {
                    (Some(v), _) => Vec3::from(v),
                    (None, Some(f)) => mn + (mx - mn) * Vec3::from(f),
                    _ => return None,
                };
                Some(Port::new(&p.id, kind, pos, Vec3::from(p.dir), p.diameter_m.max(0.001), PortSource::Catalog))
            })
            .collect();
        return Info { role, ports, spec: None, entry };
    }
    Info { role, ports: auto_ports(role, mn, mx, det), spec: None, entry }
}

fn auto_ports(role: Role, mn: Vec3, mx: Vec3, det: &Detected) -> Vec<Port> {
    let size = mx - mn;
    let mid = (mn + mx) * 0.5;
    let mut ports = Vec::new();
    match role {
        Role::Conveyor if det.joints.iter().filter(|j| j.continuous && j.axis.y.abs() < 0.3).count() >= 2 => {
            // From the rollers: the belt runs across their axes, on their tops.
            let rollers: Vec<&JointGeom> = det.joints.iter().filter(|j| j.continuous && j.axis.y.abs() < 0.3).collect();
            let a0 = rollers[0].axis;
            let across = rollers.iter().map(|j| if j.axis.dot(a0) < 0.0 { -j.axis } else { j.axis }).sum::<Vec3>().with_y(0.0).normalize_or(Vec3::Z);
            let mut dir = across.cross(Vec3::Y).normalize_or(Vec3::X);
            if dir.x.abs() >= dir.z.abs() && dir.x < 0.0 || dir.z.abs() > dir.x.abs() && dir.z < 0.0 {
                dir = -dir;
            }
            let top = rollers.iter().map(|j| j.anchor.y + j.radius).fold(f32::MIN, f32::max);
            let r = rollers.iter().map(|j| j.radius).sum::<f32>() / rollers.len() as f32;
            let along: Vec<f32> = rollers.iter().map(|j| j.anchor.dot(dir)).collect();
            let (lo, hi) = (along.iter().copied().fold(f32::MAX, f32::min) - r, along.iter().copied().fold(f32::MIN, f32::max) + r);
            let side = rollers.iter().map(|j| (j.anchor + j.axis * (j.span.0 + j.span.1) * 0.5).dot(across)).sum::<f32>() / rollers.len() as f32;
            let width = rollers.iter().map(|j| j.span.1 - j.span.0).fold(0.0f32, f32::max);
            let at = |s: f32| across * side + dir * s + Vec3::Y * top;
            ports.push(Port::new("in", PortKind::ConveyorIn, at(lo), dir, width, PortSource::Detected));
            ports.push(Port::new("out", PortKind::ConveyorOut, at(hi), dir, width, PortSource::Detected));
        }
        Role::Conveyor => {
            // Along the long horizontal axis, at belt height.
            let along_x = size.x >= size.z;
            let (axis, half, width) = if along_x { (Vec3::X, size.x * 0.5, size.z) } else { (Vec3::Z, size.z * 0.5, size.x) };
            let top = Vec3::new(mid.x, mx.y, mid.z);
            ports.push(Port::new("in", PortKind::ConveyorIn, top - axis * half, axis, width, PortSource::Detected));
            ports.push(Port::new("out", PortKind::ConveyorOut, top + axis * half, axis, width, PortSource::Detected));
        }
        Role::Engine | Role::Motor | Role::Rotor | Role::Pump => {
            // The biggest continuously spinning body is the main shaft; its
            // end that reaches furthest out of the bounds is the coupling.
            if let Some(j) = det.joints.iter().filter(|j| j.continuous).max_by(|a, b| a.radius.total_cmp(&b.radius)) {
                let a = j.anchor + j.axis * j.span.1;
                let b = j.anchor + j.axis * j.span.0;
                let out_of = |p: Vec3| ((p - mid) / (size * 0.5).max(Vec3::splat(1e-3))).abs().max_element();
                let (p, d) = if out_of(a) >= out_of(b) { (a, j.axis) } else { (b, -j.axis) };
                ports.push(Port::new("shaft", PortKind::Shaft, p, d, (j.radius * 2.0).clamp(0.01, 2.0), PortSource::Detected));
            }
        }
        _ => {}
    }
    if role.is_fluid() {
        let mut rims: Vec<&Rim> = det.rims.iter().collect();
        if role == Role::Pump && rims.len() >= 2 {
            // Suction is usually axial (horizontal), discharge points up.
            rims.truncate(2);
            rims.sort_by(|a, b| a.normal.y.abs().total_cmp(&b.normal.y.abs()));
            ports.push(Port::new("inlet", PortKind::Pipe, rims[0].center, rims[0].normal, rims[0].radius * 2.0, PortSource::Detected));
            ports.push(Port::new("outlet", PortKind::Pipe, rims[1].center, rims[1].normal, rims[1].radius * 2.0, PortSource::Detected));
        } else if !rims.is_empty() && role != Role::Pump {
            // Highest first: tanks fill from the top and drain low.
            rims.sort_by(|a, b| b.center.y.total_cmp(&a.center.y));
            for (i, r) in rims.iter().enumerate() {
                let id = match (role, i, rims.len()) {
                    (Role::Tank, 0, _) => "inlet".to_string(),
                    (Role::Tank, 1, _) => "outlet".to_string(),
                    (Role::Valve | Role::Fitting, 0, 2) => "a".to_string(),
                    (Role::Valve | Role::Fitting, 1, 2) => "b".to_string(),
                    _ => format!("nozzle{}", i + 1),
                };
                ports.push(Port::new(&id, PortKind::Pipe, r.center, r.normal, r.radius * 2.0, PortSource::Detected));
            }
        } else {
            // Nothing measurable: conventional nozzle positions.
            let d = (size.min_element() * 0.15).clamp(0.025, 0.2);
            match role {
                Role::Tank => {
                    ports.push(Port::new("inlet", PortKind::Pipe, Vec3::new(mid.x, mx.y, mid.z), Vec3::Y, d, PortSource::Inferred));
                    ports.push(Port::new("outlet", PortKind::Pipe, Vec3::new(mx.x, mn.y + size.y * 0.12, mid.z), Vec3::X, d, PortSource::Inferred));
                }
                Role::Pump => {
                    ports.push(Port::new("inlet", PortKind::Pipe, Vec3::new(mn.x, mn.y + size.y * 0.45, mid.z), -Vec3::X, d, PortSource::Inferred));
                    ports.push(Port::new("outlet", PortKind::Pipe, Vec3::new(mid.x, mx.y, mid.z), Vec3::Y, d, PortSource::Inferred));
                }
                Role::Valve | Role::Fitting => {
                    ports.push(Port::new("a", PortKind::Pipe, Vec3::new(mn.x, mid.y, mid.z), -Vec3::X, d, PortSource::Inferred));
                    ports.push(Port::new("b", PortKind::Pipe, Vec3::new(mx.x, mid.y, mid.z), Vec3::X, d, PortSource::Inferred));
                }
                _ => {}
            }
        }
    }
    ports
}

pub fn port_json(p: &Port, t: &Transform) -> Value {
    let (w, d) = p.world(t);
    let r3 = |v: Vec3, k: f32| [(v.x * k).round() / k, (v.y * k).round() / k, (v.z * k).round() / k];
    json!({
        "id": p.id,
        "kind": p.kind.name(),
        "position": r3(w, 1000.0),
        "direction": r3(d, 1000.0),
        "diameter_m": (p.size * 1000.0).round() / 1000.0,
        "source": p.source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // OUT-M15: a 165 mm flange became the size of a 53 mm bore, cutting friction 170×.
    #[test]
    fn a_flange_doesnt_become_the_bore() {
        let rim = |r: f32, at: Vec3| Rim { center: at, normal: Vec3::X, radius: r };
        let kept = dedupe_rims(vec![rim(0.0265, Vec3::new(0.01, 0.0, 0.0)), rim(0.0825, Vec3::ZERO), rim(0.03, Vec3::new(0.0, 1.0, 0.0))]);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[0].radius, 0.0265, "the bore, at the flange's position");
        assert_eq!(kept[0].center, Vec3::ZERO);
        assert_eq!(kept[1].radius, 0.03, "another nozzle stands on its own");
        // A pin hole on the axis isn't the bore.
        let kept = dedupe_rims(vec![rim(0.05, Vec3::ZERO), rim(0.004, Vec3::ZERO)]);
        assert_eq!(kept[0].radius, 0.05);
    }

    #[test]
    fn circle_fit_finds_a_pipe_end() {
        let ring: Vec<Vec3> = (0..24)
            .map(|i| {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                Vec3::new(1.0, 0.5 + 0.05 * a.cos(), 0.05 * a.sin())
            })
            .collect();
        let r = fit_circle(&ring).unwrap();
        assert!((r.radius - 0.05).abs() < 1e-3);
        assert!(r.normal.x.abs() > 0.999);
        assert!((r.center - Vec3::new(1.0, 0.5, 0.0)).length() < 1e-4);
    }

    #[test]
    fn open_tube_has_two_rims_pointing_out() {
        // Open cylinder along X from 0 to 1, radius 0.1.
        let n = 16;
        let mut pts = Vec::new();
        for x in [0.0f32, 1.0] {
            for i in 0..n {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                pts.push(Vec3::new(x, 0.1 * a.cos(), 0.1 * a.sin()));
            }
        }
        let mut idx = Vec::new();
        for i in 0..n as u32 {
            let j = (i + 1) % n as u32;
            idx.extend([i, j, n as u32 + i, j, n as u32 + j, n as u32 + i]);
        }
        let mut rims = rims_of(&pts, &idx);
        rims.sort_by(|a, b| a.center.x.total_cmp(&b.center.x));
        assert_eq!(rims.len(), 2);
        assert!(rims[0].normal.x < -0.99 && rims[1].normal.x > 0.99);
        assert!((rims[1].radius - 0.1).abs() < 0.005);
    }
}
