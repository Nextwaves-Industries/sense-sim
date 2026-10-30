//! The running plant:
//! - **drives**: engines, motors, self-driven pumps and conveyors that are
//!   switched on turn; speed spreads through shaft couplings (1:1) and belts
//!   (pulley diameter ratio) to everything connected;
//! - **motion**: every moving joint follows its drive (rotors and rollers
//!   spin, pistons and rods follow the crank, valve levers ease to their
//!   setting), and items on running conveyors travel and transfer;
//! - **flow**: a hydraulic network solved with Newton's method on heads and
//!   flows (pipes and fittings with Darcy losses, valves, pump curves,
//!   tanks as level-dependent heads, open ends as leaks), with tank levels
//!   integrated over time.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::connect::{LinkKind, Links, PortRef};
use super::parts::{self, Spec, pin_height};
use super::ports::{self, Info, Port, PortKind, Role};
use crate::prototype::Prototype;
use crate::scene::PlacedObject;
use crate::usd_load::JointKind;

const G: f32 = 9.81;
/// Fixed step of the hydraulic solve (s).
pub const FLOW_DT: f32 = 0.1;

// ---------------------------------------------------------------------------
// State

/// Per-object settings, saved with the layout.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Control {
    /// Switched on (engines, motors, pumps with a motor, conveyors).
    pub on: bool,
    pub rpm: Option<f32>,
    /// Conveyor belt speed.
    pub speed_mps: Option<f32>,
    /// 0 closed .. 1 open (valves without a lever take it directly).
    pub valve_open: Option<f32>,
    /// Tank fill (0..1) when the layout starts.
    pub fill: Option<f32>,
    /// Liquid in the system: water, oil, coolant, fuel, chemical.
    pub medium: Option<String>,
}

#[derive(Resource)]
pub struct Plant {
    /// Global play/pause.
    pub running: bool,
    pub speed: f32,
    pub show_ports: bool,
    pub show_flow: bool,
    pub controls: HashMap<String, Control>,
    /// Valve lever targets (degrees) to ease towards.
    pub lever_targets: HashMap<String, f32>,
    pub live: Live,
    acc: f32,
}

impl Default for Plant {
    fn default() -> Self {
        Self {
            running: true,
            speed: 1.0,
            show_ports: false,
            show_flow: true,
            controls: HashMap::new(),
            lever_targets: HashMap::new(),
            live: Live::default(),
            acc: 0.0,
        }
    }
}

#[derive(Default, Clone)]
pub struct Live {
    /// rad/s (conveyors: belt m/s).
    pub omega: HashMap<String, f32>,
    /// Accumulated angle (rad) or belt travel (m).
    pub angle: HashMap<String, f32>,
    /// Liquid volume in each tank (m³).
    pub volume: HashMap<String, f32>,
    pub flow: FlowResult,
    pub alarms: Vec<String>,
    pub time: f64,
    /// Distance each item has travelled on conveyors (m).
    pub carried: HashMap<String, f32>,
    /// Objects whose joints the plant set in the last step (undo ignores these changes).
    pub posed: HashSet<String>,
    /// Items the conveyors moved in the last step.
    pub moved: HashSet<String>,
}

impl Plant {
    pub fn control(&self, name: &str) -> Control {
        self.controls.get(name).cloned().unwrap_or_default()
    }
    pub fn rename(&mut self, old: &str, new: &str) {
        if let Some(c) = self.controls.remove(old) {
            self.controls.insert(new.into(), c);
        }
        if let Some(v) = self.live.volume.remove(old) {
            self.live.volume.insert(new.into(), v);
        }
    }
    pub fn clear(&mut self) {
        self.controls.clear();
        self.lever_targets.clear();
        self.live = Live::default();
    }
}

// ---------------------------------------------------------------------------
// Model: what the plant needs from the scene, in one snapshot

pub struct Obj {
    pub entity: Entity,
    pub name: String,
    pub asset: PathBuf,
    pub role: Role,
    pub info: Arc<Info>,
    pub t: Transform,
    pub proto: Arc<Prototype>,
    pub joints: Vec<f32>,
}

impl Obj {
    pub fn port(&self, id: &str) -> Option<&Port> {
        self.info.port(id)
    }
    pub fn world_bounds(&self) -> (Vec3, Vec3) {
        let (a, b) = (self.proto.aabb_min, self.proto.aabb_max);
        let mut mn = Vec3::splat(f32::MAX);
        let mut mx = Vec3::splat(f32::MIN);
        for x in [a.x, b.x] {
            for y in [a.y, b.y] {
                for z in [a.z, b.z] {
                    let p = self.t.transform_point(Vec3::new(x, y, z));
                    mn = mn.min(p);
                    mx = mx.max(p);
                }
            }
        }
        (mn, mx)
    }
}

#[derive(Default)]
pub struct Model {
    pub objs: Vec<Obj>,
    index: HashMap<String, usize>,
}

impl Model {
    pub fn build<'a>(it: impl Iterator<Item = (Entity, &'a PlacedObject, &'a Transform)>) -> Self {
        let mut m = Model::default();
        for (e, o, t) in it {
            let info = ports::info(&o.asset, &o.proto);
            m.index.insert(o.name.clone(), m.objs.len());
            m.objs.push(Obj {
                entity: e,
                name: o.name.clone(),
                asset: o.asset.clone(),
                role: info.role,
                info,
                t: *t,
                proto: o.proto.clone(),
                joints: o.joints.clone(),
            });
        }
        m
    }
    pub fn from_world(world: &mut World) -> Self {
        let mut q = world.query::<(Entity, &PlacedObject, &Transform)>();
        Self::build(q.iter(world))
    }
    pub fn get(&self, name: &str) -> Option<&Obj> {
        self.index.get(name).map(|&i| &self.objs[i])
    }
    /// World position, direction and the port itself.
    pub fn port_world(&self, r: &PortRef) -> Option<(Vec3, Vec3, &Port)> {
        let o = self.get(&r.object)?;
        let p = o.port(&r.port)?;
        let (w, d) = p.world(&o.t);
        Some((w, d, p))
    }
}

// ---------------------------------------------------------------------------
// Drives

pub fn rated_rpm(o: &Obj) -> f32 {
    if let Some(r) = o.info.entry.as_ref().and_then(|e| e.rpm) {
        return r;
    }
    match o.role {
        Role::Engine => 900.0,
        _ => 1450.0,
    }
}

/// Whether the object turns by itself when switched on.
pub fn self_driven(o: &Obj) -> bool {
    match o.role {
        Role::Engine | Role::Motor | Role::Conveyor => true,
        Role::Pump => !matches!(o.info.spec, Some(Spec::Pump { motor: false, .. })),
        _ => false,
    }
}

/// Speed of every driven object (rad/s; conveyors m/s), and drive alarms.
pub fn drives(model: &Model, links: &Links, controls: &HashMap<String, Control>) -> (HashMap<String, f32>, Vec<String>) {
    let mut omega: HashMap<String, f32> = HashMap::new();
    let mut alarms = Vec::new();
    let mut queue = VecDeque::new();
    for o in &model.objs {
        let Some(c) = controls.get(&o.name).filter(|c| c.on) else { continue };
        if !self_driven(o) {
            if o.role.is_source() || o.role == Role::Pump {
                alarms.push(format!("{} is switched on but has no motor: couple a motor or engine to its shaft", o.name));
            }
            continue;
        }
        let w = match o.role {
            Role::Conveyor => c.speed_mps.unwrap_or(0.4),
            _ => c.rpm.unwrap_or_else(|| rated_rpm(o)) * std::f32::consts::TAU / 60.0,
        };
        omega.insert(o.name.clone(), w);
        queue.push_back(o.name.clone());
    }
    while let Some(name) = queue.pop_front() {
        let w = omega[&name];
        for l in &links.0 {
            if !matches!(l.kind, LinkKind::Shaft | LinkKind::Belt) || !l.touches(&name) {
                continue;
            }
            let (me, other) = if l.a.object == name { (&l.a, &l.b) } else { (&l.b, &l.a) };
            let ratio = if l.kind == LinkKind::Belt {
                let s = |r: &PortRef| model.port_world(r).map(|p| p.2.size).unwrap_or(1.0).max(1e-3);
                s(me) / s(other)
            } else {
                1.0
            };
            let wo = w * ratio;
            match omega.get(&other.object) {
                Some(&prev) if (prev - wo).abs() > 0.05 * prev.abs().max(1.0) => {
                    alarms.push(format!("{} is driven at two different speeds ({:.0} and {:.0} rpm)", other.object, prev * 60.0 / std::f32::consts::TAU, wo * 60.0 / std::f32::consts::TAU));
                }
                Some(_) => {}
                None => {
                    omega.insert(other.object.clone(), wo);
                    queue.push_back(other.object.clone());
                }
            }
        }
    }
    (omega, alarms)
}

// ---------------------------------------------------------------------------
// Joint rigs

#[derive(Clone, Debug, PartialEq)]
pub enum Drive {
    /// Spins with the drive: joint angle = ratio × drive angle.
    Spin(f32),
    /// Slider-crank piston (prismatic): crank phase (rad), crank radius, rod length.
    Piston { phase: f32, r: f32, l: f32 },
    /// Connecting rod swinging at the wrist pin.
    Rod { phase: f32, r: f32, l: f32 },
    /// Limited joint of a machine: swings through its range.
    Swing { phase: f32, lo: f32, hi: f32 },
    /// Valve lever: follows the valve setting.
    Lever,
    Free,
}

fn tok(name: &str, k: char) -> Option<f32> {
    name.split_whitespace().find_map(|t| t.strip_prefix(k).and_then(|v| v.parse().ok()))
}

/// How each joint of an asset follows its drive (cached per asset).
pub fn rig(asset: &std::path::Path, info: &Info, proto: &Prototype) -> Arc<Vec<Drive>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<Drive>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(r) = cache.lock().unwrap().get(asset).filter(|r| r.len() == proto.joints.len()) {
        return r.clone();
    }
    let r = Arc::new(build_rig(info, proto));
    cache.lock().unwrap().insert(asset.to_path_buf(), r.clone());
    r
}

fn build_rig(info: &Info, proto: &Prototype) -> Vec<Drive> {
    let det = &proto.detected.joints;
    let main_r = det.iter().filter(|j| j.continuous).map(|j| j.radius).fold(0.0f32, f32::max).max(1e-3);
    // Conveyors: belt travel direction, for the roller spin sign.
    let travel = match (info.ports.iter().find(|p| p.kind == PortKind::ConveyorIn), info.ports.iter().find(|p| p.kind == PortKind::ConveyorOut)) {
        (Some(a), Some(b)) => (b.pos - a.pos).normalize_or(Vec3::X),
        _ => Vec3::X,
    };
    let mut swings = 0;
    proto
        .joints
        .iter()
        .enumerate()
        .map(|(i, j)| {
            let g = det.get(i).cloned().unwrap_or_default();
            let lname = j.name.to_ascii_lowercase();
            if j.kind == JointKind::Prismatic && lname.starts_with("piston") {
                if let (Some(p), Some(r), Some(l)) = (tok(&lname, 'p'), tok(&lname, 'r'), tok(&lname, 'l')) {
                    return Drive::Piston { phase: p.to_radians(), r: r / 1000.0, l: l / 1000.0 };
                }
            }
            if j.kind == JointKind::Revolute && lname.starts_with("rod") {
                if let (Some(p), Some(r), Some(l)) = (tok(&lname, 'p'), tok(&lname, 'r'), tok(&lname, 'l')) {
                    return Drive::Rod { phase: p.to_radians(), r: r / 1000.0, l: l / 1000.0 };
                }
            }
            if info.role == Role::Valve && lname.contains("lever") {
                return Drive::Lever;
            }
            if let Some(r) = info.entry.as_ref().and_then(|e| e.joint_ratios.get(&j.name).copied()) {
                return Drive::Spin(r);
            }
            let continuous = j.kind == JointKind::Revolute && j.lower.is_none() && j.upper.is_none();
            if continuous {
                if info.role == Role::Conveyor {
                    // Belt travel (m) to roller angle: v = ω r, top surface moves with the belt.
                    let s = g.axis.cross(Vec3::Y).dot(travel).signum();
                    let s = if s == 0.0 { 1.0 } else { s };
                    return Drive::Spin(s / g.radius.max(0.005));
                }
                if lname == "crank" || lname == "rotor" {
                    return Drive::Spin(1.0);
                }
                return Drive::Spin((main_r / g.radius.max(1e-3)).clamp(0.2, 8.0));
            }
            if matches!(info.role, Role::Engine | Role::Rotor | Role::Pump | Role::Motor) {
                let (lo, hi) = j.range(proto.unit);
                swings += 1;
                return Drive::Swing { phase: swings as f32 * 1.3, lo, hi };
            }
            Drive::Free
        })
        .collect()
}

fn wrap_deg(a: f32) -> f32 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// Joint values for drive angle `theta`.
pub fn pose(rig: &[Drive], current: &[f32], theta: f32, unit: f32) -> Vec<f32> {
    rig.iter()
        .zip(current)
        .map(|(d, &q)| match *d {
            Drive::Spin(k) => wrap_deg((theta * k).to_degrees()),
            Drive::Piston { phase, r, l } => (pin_height(theta + phase, r, l) - pin_height(phase, r, l)) / unit,
            Drive::Rod { phase, r, l } => -((r * (theta + phase).sin() / l).clamp(-1.0, 1.0)).asin().to_degrees(),
            Drive::Swing { phase, lo, hi } => (lo + hi) * 0.5 + (hi - lo) * 0.4 * (theta + phase).sin(),
            Drive::Lever | Drive::Free => q,
        })
        .collect()
}

/// Valve opening 0..1: from the lever when the valve has one.
pub fn valve_open(o: &Obj, controls: &HashMap<String, Control>) -> f32 {
    if o.role != Role::Valve {
        return 1.0;
    }
    if let Some(i) = o.proto.joints.iter().position(|j| j.name.to_ascii_lowercase().contains("lever")) {
        return (1.0 - o.joints.get(i).copied().unwrap_or(0.0) / 90.0).clamp(0.0, 1.0);
    }
    controls.get(&o.name).and_then(|c| c.valve_open).unwrap_or(1.0).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tanks

pub struct TankGeom {
    /// World height of the shell bottom.
    pub bottom: f32,
    pub height: f32,
    pub capacity: f32,
    /// Local center and radius of the liquid column (for the liquid visual).
    pub local_center: Vec3,
    pub radius: f32,
}

pub fn tank_geom(o: &Obj) -> TankGeom {
    if let Some(Spec::Tank { dia, h, .. }) = o.info.spec {
        let (y0, r, h) = parts::tank_shell(dia, h);
        let s = o.t.scale.y;
        return TankGeom { bottom: o.t.translation.y + y0 * s, height: h * s, capacity: std::f32::consts::PI * r * r * h * s.powi(3), local_center: Vec3::new(0.0, y0, 0.0), radius: r * 0.98 };
    }
    let (mn, mx) = (o.proto.aabb_min, o.proto.aabb_max);
    let size = (mx - mn) * o.t.scale;
    let h = size.y * 0.9;
    let r = size.x.min(size.z) * 0.5;
    let cap = o.info.entry.as_ref().and_then(|e| e.capacity_l).map(|l| l / 1000.0).unwrap_or(std::f32::consts::PI * r * r * h * 0.95);
    let c = (mn + mx) * 0.5;
    TankGeom {
        bottom: o.t.translation.y + (mn.y + (mx.y - mn.y) * 0.05) * o.t.scale.y,
        height: h,
        capacity: cap,
        local_center: Vec3::new(c.x, mn.y + (mx.y - mn.y) * 0.05, c.z),
        radius: ((mx - mn).x.min((mx - mn).z) * 0.45).max(0.01),
    }
}

/// Starting volume of a tank.
pub fn initial_volume(o: &Obj, controls: &HashMap<String, Control>) -> f32 {
    let g = tank_geom(o);
    controls.get(&o.name).and_then(|c| c.fill).unwrap_or(0.5).clamp(0.0, 1.0) * g.capacity
}

// ---------------------------------------------------------------------------
// Hydraulics

#[derive(Default, Clone, Debug)]
pub struct FlowResult {
    /// m³/s leaving the object through each pipe port (negative: entering).
    pub port_flow: HashMap<PortRef, f32>,
    /// Hydraulic head (m) at each connected pipe port.
    pub port_head: HashMap<PortRef, f32>,
    /// Flow through each two-port element (pipe, valve, pump), m³/s, a → b.
    pub element_flow: HashMap<String, f32>,
    /// Net inflow of each tank (m³/s).
    pub tank_inflow: HashMap<String, f32>,
    /// Pump flow (m³/s) and head added (m).
    pub pumps: HashMap<String, (f32, f32)>,
    pub leaks: Vec<(PortRef, f32)>,
    pub alarms: Vec<String>,
}

enum EdgeKind {
    Resist { r: f32 },
    Pump { h0: f32, c: f32 },
    Blocked,
}

struct Edge {
    a: usize,
    b: usize,
    kind: EdgeKind,
    /// (object, from port, to port); "" = the element's center.
    owner: (String, String, String),
}

impl Edge {
    /// Head loss from a to b and its derivative.
    fn f(&self, q: f32) -> (f32, f32) {
        let lin = 1.0;
        match self.kind {
            EdgeKind::Resist { r } => (r * q * q.abs() + lin * q, 2.0 * r * q.abs() + lin),
            EdgeKind::Pump { h0, c } => {
                // Check valve: backflow sees a huge resistance.
                let cc = if q >= 0.0 { c } else { c * 1e4 + 1e7 };
                (-h0 + cc * q * q.abs() + lin * q, 2.0 * cc * q.abs() + lin)
            }
            EdgeKind::Blocked => (1e9 * q, 1e9),
        }
    }
}

struct Node {
    fixed: Option<f32>,
    elev: f32,
    /// Tank nozzle above the liquid: it can take liquid in but not supply it.
    dry: bool,
    tank: Option<String>,
    open: bool,
    refs: Vec<PortRef>,
}

fn area(d: f32) -> f32 {
    std::f32::consts::PI * d * d * 0.25
}

fn resist(len: f32, k: f32, d: f32) -> f32 {
    let a = area(d.max(0.003));
    ((0.02 * len / d.max(0.003) + k) / (2.0 * G * a * a)).max(1.0)
}

/// Solves the pipe network for the current speeds, valve settings and tank volumes.
pub fn solve(model: &Model, links: &Links, controls: &HashMap<String, Control>, omega: &HashMap<String, f32>, volume: &HashMap<String, f32>) -> FlowResult {
    let mut res = FlowResult::default();
    // Union-find over pipe links.
    let mut id: HashMap<PortRef, usize> = HashMap::new();
    let mut parent: Vec<usize> = Vec::new();
    let mut refs: Vec<PortRef> = Vec::new();
    let node_of = |r: &PortRef, id: &mut HashMap<PortRef, usize>, parent: &mut Vec<usize>, refs: &mut Vec<PortRef>| -> usize {
        *id.entry(r.clone()).or_insert_with(|| {
            parent.push(parent.len());
            refs.push(r.clone());
            parent.len() - 1
        })
    };
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let fluid: Vec<&Obj> = model.objs.iter().filter(|o| o.info.ports.iter().any(|p| p.kind == PortKind::Pipe)).collect();
    for o in &fluid {
        for p in o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe) {
            node_of(&PortRef::new(&o.name, &p.id), &mut id, &mut parent, &mut refs);
        }
    }
    let mut linked: HashSet<PortRef> = HashSet::new();
    for l in links.0.iter().filter(|l| l.kind == LinkKind::Pipe) {
        let (Some(&a), Some(&b)) = (id.get(&l.a), id.get(&l.b)) else { continue };
        linked.insert(l.a.clone());
        linked.insert(l.b.clone());
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    // Compact nodes.
    let mut nodes: Vec<Node> = Vec::new();
    let mut root_to_node: HashMap<usize, usize> = HashMap::new();
    let mut port_node: HashMap<PortRef, usize> = HashMap::new();
    for (i, r) in refs.iter().enumerate() {
        let root = find(&mut parent, i);
        let n = *root_to_node.entry(root).or_insert_with(|| {
            nodes.push(Node { fixed: None, elev: 0.0, dry: false, tank: None, open: false, refs: Vec::new() });
            nodes.len() - 1
        });
        nodes[n].refs.push(r.clone());
        if let Some((w, _, _)) = model.port_world(r) {
            nodes[n].elev = w.y;
        }
        port_node.insert(r.clone(), n);
    }
    let mut edges: Vec<Edge> = Vec::new();
    for o in &fluid {
        let pipe_ports: Vec<&Port> = o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe).collect();
        let nid = |p: &Port| port_node[&PortRef::new(&o.name, &p.id)];
        match o.role {
            Role::Tank => {
                let g = tank_geom(o);
                let v = volume.get(&o.name).copied().unwrap_or_else(|| initial_volume(o, controls));
                let surface = g.bottom + (v / g.capacity.max(1e-6)).clamp(0.0, 1.0) * g.height;
                for p in &pipe_ports {
                    let r = PortRef::new(&o.name, &p.id);
                    if !linked.contains(&r) {
                        continue;
                    }
                    let n = nid(p);
                    let elev = p.world(&o.t).0.y;
                    let dry = elev > surface - 0.005;
                    nodes[n].fixed = Some(if dry { elev } else { surface });
                    nodes[n].dry = dry;
                    nodes[n].tank = Some(o.name.clone());
                }
            }
            Role::Pump => {
                let inlet = pipe_ports.iter().find(|p| p.id.contains("in") || p.id.contains("suction")).or(pipe_ports.first()).copied();
                let outlet = pipe_ports.iter().find(|p| p.id.contains("out") || p.id.contains("discharge")).or(pipe_ports.get(1)).copied();
                let (Some(pi), Some(po)) = (inlet, outlet) else { continue };
                if pi.id == po.id {
                    continue;
                }
                let w = omega.get(&o.name).copied().unwrap_or(0.0);
                let s = w / (rated_rpm(o) * std::f32::consts::TAU / 60.0);
                let d = o.info.spec.map(|sp| sp.bore()).unwrap_or(pi.size * 0.9);
                let h0 = o.info.entry.as_ref().and_then(|e| e.head_m).unwrap_or(20.0) * s * s;
                let qmax = o.info.entry.as_ref().and_then(|e| e.max_flow_lps).map(|l| l / 1000.0).unwrap_or(4.5 * area(d)) * s.max(1e-3);
                let kind = if s < 0.01 { EdgeKind::Blocked } else { EdgeKind::Pump { h0, c: h0 / (qmax * qmax) } };
                let in_ref = PortRef::new(&o.name, &pi.id);
                if s >= 0.01 && !linked.contains(&in_ref) {
                    res.alarms.push(format!("pump {} runs with its inlet open (no suction pipe): it would run dry", o.name));
                    continue;
                }
                edges.push(Edge { a: nid(pi), b: nid(po), kind, owner: (o.name.clone(), pi.id.clone(), po.id.clone()) });
            }
            _ => {
                let (len, k) = o.info.spec.map(|s| s.resistance()).unwrap_or((0.5, 0.5));
                let open = valve_open(o, controls);
                let d = o.info.spec.map(|s| s.bore()).filter(|d| *d > 0.0).unwrap_or_else(|| pipe_ports.iter().map(|p| p.size).fold(1.0, f32::min) * 0.9);
                let kind = |share: f32| {
                    if o.role == Role::Valve && open < 0.02 {
                        EdgeKind::Blocked
                    } else {
                        let kv = if o.role == Role::Valve { k + 0.5 * (1.0 / (open * open) - 1.0) } else { k };
                        EdgeKind::Resist { r: resist(len * share, kv * share, d) }
                    }
                };
                if pipe_ports.len() == 2 {
                    edges.push(Edge { a: nid(pipe_ports[0]), b: nid(pipe_ports[1]), kind: kind(1.0), owner: (o.name.clone(), pipe_ports[0].id.clone(), pipe_ports[1].id.clone()) });
                } else if pipe_ports.len() > 2 {
                    let c = nodes.len();
                    nodes.push(Node { fixed: None, elev: o.t.translation.y, dry: false, tank: None, open: false, refs: Vec::new() });
                    for p in &pipe_ports {
                        edges.push(Edge { a: c, b: nid(p), kind: kind(0.5), owner: (o.name.clone(), String::new(), p.id.clone()) });
                    }
                }
            }
        }
    }
    // Unlinked pipe ends of piping open to the air.
    for n in nodes.iter_mut() {
        if n.fixed.is_none() && n.refs.len() == 1 && !linked.contains(&n.refs[0]) {
            let tank_port = model.get(&n.refs[0].object).is_some_and(|o| o.role == Role::Tank);
            if !tank_port {
                n.fixed = Some(n.elev);
                n.open = true;
            }
        }
    }
    if edges.is_empty() {
        return res;
    }
    let mut q = vec![1e-4f32; edges.len()];
    let mut h = vec![0.0f32; nodes.len()];
    // Dry tank nozzles that would have to supply liquid become dead ends.
    for _pass in 0..3 {
        newton(&nodes, &edges, &mut q, &mut h);
        let mut changed = false;
        for (n, node) in nodes.iter_mut().enumerate() {
            if !node.dry || node.fixed.is_none() {
                continue;
            }
            let out: f32 = edges.iter().zip(&q).map(|(e, q)| if e.a == n { *q } else if e.b == n { -*q } else { 0.0 }).sum();
            if out > 1e-7 {
                node.fixed = None;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Results.
    for (e, &qe) in edges.iter().zip(&q) {
        let (obj, pa, pb) = &e.owner;
        if !pa.is_empty() {
            *res.port_flow.entry(PortRef::new(obj, pa)).or_default() -= qe;
            res.element_flow.insert(obj.clone(), qe);
        }
        *res.port_flow.entry(PortRef::new(obj, pb)).or_default() += qe;
        if let EdgeKind::Pump { .. } = e.kind {
            res.pumps.insert(obj.clone(), (qe, h[e.b] - h[e.a]));
        }
    }
    for (n, node) in nodes.iter().enumerate() {
        for r in &node.refs {
            res.port_head.insert(r.clone(), h[n]);
        }
        let inflow: f32 = edges.iter().zip(&q).map(|(e, q)| if e.b == n { *q } else if e.a == n { -*q } else { 0.0 }).sum();
        if node.open && inflow > 1e-6 {
            res.leaks.push((node.refs[0].clone(), inflow));
        }
        if let Some(t) = &node.tank {
            *res.tank_inflow.entry(t.clone()).or_default() += inflow;
        }
    }
    for o in &fluid {
        if o.role == Role::Pump && omega.get(&o.name).copied().unwrap_or(0.0) > 1.0 {
            let qp = res.pumps.get(&o.name).map(|p| p.0).unwrap_or(0.0);
            if qp.abs() < 1e-6 && !res.alarms.iter().any(|a| a.contains(&o.name)) {
                res.alarms.push(format!("pump {} runs but nothing flows: a valve is closed, the suction tank is empty, or the discharge is dead-ended", o.name));
            }
        }
    }
    for (r, qv) in &res.leaks {
        res.alarms.push(format!("leak: {} is open and spills {:.2} l/s", r.label(), qv * 1000.0));
    }
    res
}

fn newton(nodes: &[Node], edges: &[Edge], q: &mut [f32], h: &mut [f32]) {
    let free: Vec<usize> = (0..nodes.len()).filter(|&n| nodes[n].fixed.is_none()).collect();
    let mut col = vec![usize::MAX; nodes.len()];
    for (i, &n) in free.iter().enumerate() {
        col[n] = i;
    }
    let fixed_mean = {
        let f: Vec<f32> = nodes.iter().filter_map(|n| n.fixed).collect();
        if f.is_empty() { 0.0 } else { f.iter().sum::<f32>() / f.len() as f32 }
    };
    for (n, node) in nodes.iter().enumerate() {
        h[n] = match node.fixed {
            Some(v) => v,
            None if h[n] == 0.0 => fixed_mean,
            None => h[n],
        };
    }
    let nf = free.len();
    for _ in 0..80 {
        let mut a = vec![0.0f64; nf * nf];
        let mut rhs = vec![0.0f64; nf];
        let mut gs = Vec::with_capacity(edges.len());
        let mut rs = Vec::with_capacity(edges.len());
        for (e, &qe) in edges.iter().zip(q.iter()) {
            let (f, df) = e.f(qe);
            let g = 1.0 / df.max(1e-9) as f64;
            let r = (f - (h[e.a] - h[e.b])) as f64;
            gs.push(g);
            rs.push(r);
            for (n, s) in [(e.a, 1.0f64), (e.b, -1.0f64)] {
                let i = col[n];
                if i == usize::MAX {
                    continue;
                }
                rhs[i] += -s * qe as f64 + s * g * r;
                if col[e.a] != usize::MAX {
                    a[i * nf + col[e.a]] += s * g;
                }
                if col[e.b] != usize::MAX {
                    a[i * nf + col[e.b]] -= s * g;
                }
            }
        }
        for i in 0..nf {
            a[i * nf + i] += 1e-12;
        }
        let dh = gauss(a, rhs, nf);
        let mut max_dq = 0.0f64;
        for (k, e) in edges.iter().enumerate() {
            let dha = if col[e.a] != usize::MAX { dh[col[e.a]] } else { 0.0 };
            let dhb = if col[e.b] != usize::MAX { dh[col[e.b]] } else { 0.0 };
            let dq = gs[k] * (-rs[k] + dha - dhb);
            q[k] += dq as f32;
            max_dq = max_dq.max(dq.abs());
        }
        for (i, &n) in free.iter().enumerate() {
            h[n] += dh[i] as f32;
        }
        if max_dq < 1e-9 {
            break;
        }
    }
}

/// Dense Gaussian elimination with partial pivoting.
fn gauss(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Vec<f64> {
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i * n + c].abs().total_cmp(&a[j * n + c].abs())).unwrap_or(c);
        if a[p * n + c].abs() < 1e-300 {
            continue;
        }
        if p != c {
            for k in 0..n {
                a.swap(c * n + k, p * n + k);
            }
            b.swap(c, p);
        }
        let piv = a[c * n + c];
        for r in c + 1..n {
            let f = a[r * n + c] / piv;
            if f == 0.0 {
                continue;
            }
            for k in c..n {
                a[r * n + k] -= f * a[c * n + k];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for c in (0..n).rev() {
        let mut s = b[c];
        for k in c + 1..n {
            s -= a[c * n + k] * x[k];
        }
        let d = a[c * n + c];
        x[c] = if d.abs() < 1e-300 { 0.0 } else { s / d };
    }
    x
}

/// Advances tank volumes by `dt` and reports overflow / empty tanks.
pub fn integrate(model: &Model, flow: &FlowResult, volume: &mut HashMap<String, f32>, controls: &HashMap<String, Control>, dt: f32, alarms: &mut Vec<String>) {
    for o in model.objs.iter().filter(|o| o.role == Role::Tank) {
        let g = tank_geom(o);
        let v = volume.entry(o.name.clone()).or_insert_with(|| initial_volume(o, controls));
        let inflow = flow.tank_inflow.get(&o.name).copied().unwrap_or(0.0);
        *v += inflow * dt;
        if *v > g.capacity {
            *v = g.capacity;
            if inflow > 1e-6 {
                alarms.push(format!("tank {} is full and overflowing", o.name));
            }
        }
        if *v < 0.0 {
            *v = 0.0;
        }
        if *v < 1e-4 * g.capacity && inflow < -1e-7 {
            alarms.push(format!("tank {} has run empty", o.name));
        }
    }
}

// ---------------------------------------------------------------------------
// Conveyors

pub struct Belt {
    pub name: String,
    /// World start (in port) and unit travel direction, length, half width.
    pub start: Vec3,
    pub dir: Vec3,
    pub len: f32,
    pub half_w: f32,
    pub top: f32,
    pub speed: f32,
    pub next: Option<String>,
}

pub fn belts(model: &Model, links: &Links, omega: &HashMap<String, f32>) -> Vec<Belt> {
    model
        .objs
        .iter()
        .filter(|o| o.role == Role::Conveyor)
        .filter_map(|o| {
            let pin = o.info.ports.iter().find(|p| p.kind == PortKind::ConveyorIn)?;
            let pout = o.info.ports.iter().find(|p| p.kind == PortKind::ConveyorOut)?;
            let (a, b) = (pin.world(&o.t).0, pout.world(&o.t).0);
            let dir = (b - a).with_y(0.0).normalize_or(Vec3::X);
            let next = links.partner(&PortRef::new(&o.name, &pout.id)).map(|(_, r)| r.object.clone());
            Some(Belt {
                name: o.name.clone(),
                start: a,
                dir,
                len: (b - a).with_y(0.0).length(),
                half_w: pin.size * 0.5 * o.t.scale.x,
                top: a.y.max(b.y),
                speed: omega.get(&o.name).copied().unwrap_or(0.0),
                next,
            })
        })
        .collect()
}

/// Objects that can ride a conveyor (not conveyors, structures or piping).
fn rideable(o: &Obj) -> bool {
    !matches!(o.role, Role::Conveyor | Role::Fitting | Role::Valve | Role::Tank | Role::Engine | Role::Motor | Role::Pump)
        && matches!(crate::agent::structures::Kind::of_asset(&o.asset), None | Some(crate::agent::structures::Kind::Block))
}

/// Item moves for one frame: (entity, item name, new translation).
pub fn carry(model: &Model, belts: &[Belt], dt: f32) -> Vec<(Entity, String, Vec3)> {
    let mut moves = Vec::new();
    let mut taken: HashSet<Entity> = HashSet::new();
    // Items on each belt: along-belt coordinate and half length.
    let on = |b: &Belt, o: &Obj| -> Option<(f32, f32)> {
        let (mn, mx) = o.world_bounds();
        let c = (mn + mx) * 0.5;
        let rel = c - b.start;
        let along = rel.dot(b.dir);
        let side = rel.dot(b.dir.cross(Vec3::Y)).abs();
        let half = ((mx - mn) * 0.5).dot(b.dir.abs());
        let base_ok = mn.y > b.top - 0.35 && mn.y < b.top + 0.06;
        (base_ok && along >= -0.02 && along <= b.len + 0.02 && side <= b.half_w + 0.05).then_some((along, half))
    };
    for b in belts.iter().filter(|b| b.speed.abs() > 1e-4) {
        let next = b.next.as_ref().and_then(|n| belts.iter().find(|x| &x.name == n));
        let mut items: Vec<(&Obj, f32, f32)> = model.objs.iter().filter(|o| rideable(o)).filter_map(|o| on(b, o).map(|(a, h)| (o, a, h))).collect();
        // Items already on the next belt block the transfer.
        let mut ahead: Vec<(f32, f32)> = Vec::new();
        if let Some(nb) = next {
            for o in model.objs.iter().filter(|o| rideable(o)) {
                if let Some((a, h)) = on(nb, o) {
                    let world = nb.start + nb.dir * a;
                    ahead.push(((world - b.start).dot(b.dir), h));
                }
            }
        }
        items.sort_by(|x, y| y.1.total_cmp(&x.1));
        let mut limit = if next.is_some() { f32::MAX } else { b.len };
        if let Some(&(a, h)) = ahead.iter().min_by(|x, y| x.0.total_cmp(&y.0)) {
            limit = limit.min(a - h - 0.02);
        }
        for (o, along, half) in items {
            if !taken.insert(o.entity) {
                continue;
            }
            let step = b.speed * dt;
            let max_front = limit;
            let new_along = (along + step).min(max_front - half).max(along.min(max_front - half));
            limit = new_along - half - 0.02;
            let d = new_along - along;
            if d.abs() > 1e-6 {
                let mut t = o.t.translation + b.dir * d;
                // Onto a next belt that sits at another height.
                if let Some(nb) = next
                    && new_along > b.len
                    && along <= b.len
                    && (nb.top - b.top).abs() < 0.3
                {
                    t.y += nb.top - b.top;
                }
                moves.push((o.entity, o.name.clone(), t));
            }
        }
    }
    moves
}

/// A loose item resting *inside* a conveyor's frame - dropped between the
/// rollers onto a cross brace, out of sight and never carried - goes back on
/// top of the belt. Skips what the user is holding and anything connected to
/// the plant (a drive motor on the shaft, say).
pub fn keep_on_belts(
    links: Res<Links>,
    drag: Res<crate::scene::DragState>,
    // One ParamSet: a `Changed<Transform>` filter reads Transform, which the
    // mutable query also writes.
    mut set: ParamSet<(Query<(), (Changed<Transform>, With<PlacedObject>)>, Query<(Entity, &mut PlacedObject, &mut Transform), Without<crate::scene::Ghost>>)>,
) {
    if set.p0().is_empty() {
        return;
    }
    let mut objects = set.p1();
    let model = Model::build(objects.iter());
    let bs = belts(&model, &links, &HashMap::new());
    if bs.is_empty() {
        return;
    }
    let held = drag.holding();
    let mut lifts: Vec<(Entity, f32)> = Vec::new();
    for o in model.objs.iter().filter(|o| rideable(o) && Some(o.entity) != held && !links.0.iter().any(|l| l.touches(&o.name))) {
        let (mn, mx) = o.world_bounds();
        let c = (mn + mx) * 0.5;
        for b in &bs {
            let rel = c - b.start;
            let along = rel.dot(b.dir);
            let side = rel.dot(b.dir.cross(Vec3::Y)).abs();
            // Over the belt, base below the rollers but off the floor, and small
            // enough to fit between the rails.
            let inside = (0.0..=b.len).contains(&along) && side <= b.half_w && mn.y > 0.03 && mn.y < b.top - 0.02 && (mx - mn).y < b.top;
            if inside {
                lifts.push((o.entity, b.top - mn.y));
                break;
            }
        }
    }
    for (e, dy) in lifts {
        if let Ok((_, _, mut t)) = objects.get_mut(e) {
            t.translation.y += dy;
        }
    }
}

// ---------------------------------------------------------------------------
// Live system

#[allow(clippy::type_complexity)]
pub fn step(
    time: Res<Time>,
    mut plant: ResMut<Plant>,
    links: Res<Links>,
    mut objects: Query<(Entity, &mut PlacedObject, &mut Transform), Without<crate::scene::Ghost>>,
    drag: Res<crate::scene::DragState>,
) {
    let dt = time.delta_secs().min(0.1);
    plant.live.posed.clear();
    plant.live.moved.clear();
    // Valve levers ease to their target even while paused.
    if !plant.lever_targets.is_empty() {
        let targets = plant.lever_targets.clone();
        for (_, mut obj, _) in objects.iter_mut() {
            let Some(&target) = targets.get(&obj.name) else { continue };
            let Some(i) = obj.proto.joints.iter().position(|j| j.name.to_ascii_lowercase().contains("lever")) else {
                plant.lever_targets.remove(&obj.name.clone());
                continue;
            };
            let cur = obj.joints[i];
            let next = cur + (target - cur).clamp(-120.0 * dt, 120.0 * dt);
            if (next - cur).abs() > 1e-4 {
                obj.joints[i] = next;
                let n = obj.name.clone();
                plant.live.posed.insert(n);
            }
            if (target - next).abs() < 1e-3 {
                let n = obj.name.clone();
                plant.lever_targets.remove(&n);
            }
        }
    }
    if !plant.running {
        return;
    }
    let dt = dt * plant.speed;
    let model = Model::build(objects.iter());
    let plant = plant.as_mut();
    let (omega, mut alarms) = drives(&model, &links, &plant.controls);
    plant.acc += dt;
    let mut steps = 0;
    while plant.acc >= FLOW_DT && steps < 5 {
        plant.acc -= FLOW_DT;
        steps += 1;
        let flow = solve(&model, &links, &plant.controls, &omega, &plant.live.volume);
        let mut a = Vec::new();
        integrate(&model, &flow, &mut plant.live.volume, &plant.controls, FLOW_DT, &mut a);
        plant.live.flow = flow;
        plant.live.flow.alarms.extend(a);
        plant.live.time += FLOW_DT as f64;
    }
    if steps == 5 {
        plant.acc = 0.0;
    }
    alarms.extend(plant.live.flow.alarms.iter().cloned());
    alarms.sort();
    alarms.dedup();
    plant.live.alarms = alarms;
    plant.live.omega = omega.clone();

    // Joints follow their drives.
    for o in &model.objs {
        let w = omega.get(&o.name).copied().unwrap_or(0.0);
        if w == 0.0 {
            continue;
        }
        let th = plant.live.angle.entry(o.name.clone()).or_insert(0.0);
        *th += w * dt;
        // Keep the angle bounded (a full cycle of every rig is 2π / gcd; 1e4 rad is plenty).
        if th.abs() > 1e4 * std::f32::consts::TAU && o.role != Role::Conveyor {
            *th = th.rem_euclid(std::f32::consts::TAU);
        }
        let theta = *th;
        if o.joints.is_empty() {
            continue;
        }
        let r = rig(&o.asset, &o.info, &o.proto);
        let q = pose(&r, &o.joints, theta, o.proto.unit);
        if q != o.joints
            && let Ok((_, mut obj, _)) = objects.get_mut(o.entity)
        {
            obj.joints = q;
            plant.live.posed.insert(o.name.clone());
        }
    }

    // Conveyors carry what rests on them.
    let bs = belts(&model, &links, &omega);
    let held = drag.holding();
    for (e, name, t) in carry(&model, &bs, dt) {
        if Some(e) == held {
            continue;
        }
        if let Ok((_, _, mut tr)) = objects.get_mut(e) {
            let d = t.distance(tr.translation);
            tr.translation = t;
            plant.live.moved.insert(name.clone());
            *plant.live.carried.entry(name).or_default() += d;
        }
    }
}

/// Generated pipes, fittings and valves hang in the air on their connections.
pub fn mark_floating(mut commands: Commands, added: Query<(Entity, &PlacedObject), Added<PlacedObject>>) {
    for (e, o) in &added {
        if matches!(Spec::of_asset(&o.asset).map(|s| s.role()), Some(Role::Fitting | Role::Valve)) {
            commands.entity(e).insert(crate::scene::Floating);
        }
    }
}

// ---------------------------------------------------------------------------
// Fast-forward (for the assistant and the headless test)

/// Runs the plant for `seconds` on a copy of the live state and reports.
pub fn simulate(model: &Model, links: &Links, plant: &Plant, seconds: f32) -> Value {
    let (omega, drive_alarms) = drives(model, links, &plant.controls);
    let dt = if seconds > 120.0 { 0.5 } else { FLOW_DT };
    let steps = ((seconds / dt).ceil() as usize).clamp(1, 2400);
    let mut volume = plant.live.volume.clone();
    for o in model.objs.iter().filter(|o| o.role == Role::Tank) {
        volume.entry(o.name.clone()).or_insert_with(|| initial_volume(o, &plant.controls));
    }
    let start = volume.clone();
    let mut lo = volume.clone();
    let mut hi = volume.clone();
    let mut alarms: Vec<(String, f32)> = drive_alarms.into_iter().map(|a| (a, 0.0)).collect();
    let mut flow = FlowResult::default();
    for k in 0..steps {
        flow = solve(model, links, &plant.controls, &omega, &volume);
        let mut a = flow.alarms.clone();
        integrate(model, &flow, &mut volume, &plant.controls, dt, &mut a);
        for (name, v) in &volume {
            lo.entry(name.clone()).and_modify(|x| *x = x.min(*v));
            hi.entry(name.clone()).and_modify(|x| *x = x.max(*v));
        }
        for m in a {
            if !alarms.iter().any(|(x, _)| *x == m) {
                alarms.push((m, (k + 1) as f32 * dt));
            }
        }
    }
    report(model, links, &omega, &flow, &start, &volume, &lo, &hi, &alarms, seconds)
}

fn r3(v: f32) -> f64 {
    (v as f64 * 1000.0).round() / 1000.0
}

#[allow(clippy::too_many_arguments)]
fn report(
    model: &Model,
    links: &Links,
    omega: &HashMap<String, f32>,
    flow: &FlowResult,
    start: &HashMap<String, f32>,
    end: &HashMap<String, f32>,
    lo: &HashMap<String, f32>,
    hi: &HashMap<String, f32>,
    alarms: &[(String, f32)],
    seconds: f32,
) -> Value {
    let mut machines = Vec::new();
    let mut tanks = Vec::new();
    let mut pumps = Vec::new();
    let mut valves = Vec::new();
    let mut conveyors = Vec::new();
    for o in &model.objs {
        let w = omega.get(&o.name).copied().unwrap_or(0.0);
        match o.role {
            Role::Engine | Role::Motor | Role::Rotor => machines.push(json!({ "name": o.name, "role": o.role.name(), "rpm": (w * 60.0 / std::f32::consts::TAU).round() })),
            Role::Conveyor => conveyors.push(json!({ "name": o.name, "speed_mps": r3(w), "feeds": links.partner(&PortRef::new(&o.name, "out")).map(|(_, r)| r.object.clone()) })),
            Role::Pump => {
                let (q, h) = flow.pumps.get(&o.name).copied().unwrap_or((0.0, 0.0));
                pumps.push(json!({ "name": o.name, "rpm": (w * 60.0 / std::f32::consts::TAU).round(), "flow_lps": r3(q * 1000.0), "head_m": r3(h) }));
            }
            Role::Tank => {
                let g = tank_geom(o);
                let f = |v: Option<&f32>| r3(v.copied().unwrap_or(0.0) / g.capacity.max(1e-6) * 100.0);
                tanks.push(json!({
                    "name": o.name,
                    "capacity_l": r3(g.capacity * 1000.0),
                    "level_start_pct": f(start.get(&o.name)),
                    "level_end_pct": f(end.get(&o.name)),
                    "level_min_pct": f(lo.get(&o.name)),
                    "level_max_pct": f(hi.get(&o.name)),
                    "net_inflow_lps": r3(flow.tank_inflow.get(&o.name).copied().unwrap_or(0.0) * 1000.0),
                }));
            }
            Role::Valve => {
                let open = valve_open(o, &HashMap::new());
                valves.push(json!({ "name": o.name, "open_pct": (open * 100.0).round(), "flow_lps": r3(flow.element_flow.get(&o.name).copied().unwrap_or(0.0) * 1000.0) }));
            }
            _ => {}
        }
    }
    let unlinked: Vec<String> = model
        .objs
        .iter()
        .filter(|o| matches!(o.role, Role::Fitting | Role::Valve | Role::Pump))
        .flat_map(|o| o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe).map(move |p| PortRef::new(&o.name, &p.id)))
        .filter(|r| links.partner(r).is_none())
        .map(|r| r.label())
        .collect();
    json!({
        "simulated_s": seconds,
        "machines": machines,
        "pumps": pumps,
        "valves": valves,
        "tanks": tanks,
        "conveyors": conveyors,
        "links": links.0.len(),
        "open_pipe_ports": unlinked,
        "leaks": flow.leaks.iter().map(|(r, q)| json!({ "port": r.label(), "lps": r3(q * 1000.0) })).collect::<Vec<_>>(),
        "alarms": alarms.iter().map(|(a, t)| json!({ "at_s": r3(*t), "alarm": a })).collect::<Vec<_>>(),
        "ok": alarms.is_empty() && flow.leaks.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::connect::Link;

    fn edge(a: usize, b: usize, kind: EdgeKind) -> Edge {
        Edge { a, b, kind, owner: (String::new(), String::new(), String::new()) }
    }
    fn node(fixed: Option<f32>) -> Node {
        Node { fixed, elev: 0.0, dry: false, tank: None, open: false, refs: Vec::new() }
    }

    #[test]
    fn gravity_flow_matches_hand_calculation() {
        // 5 m of head across one resistance r: Q = sqrt(5 / r).
        let r = 2.0e4;
        let nodes = vec![node(Some(5.0)), node(None), node(Some(0.0))];
        let edges = vec![edge(0, 1, EdgeKind::Resist { r: r * 0.5 }), edge(1, 2, EdgeKind::Resist { r: r * 0.5 })];
        let (mut q, mut h) = (vec![1e-4; 2], vec![0.0; 3]);
        newton(&nodes, &edges, &mut q, &mut h);
        let expect = (5.0f32 / r).sqrt();
        assert!((q[0] - expect).abs() / expect < 0.02, "{} vs {expect}", q[0]);
        assert!((q[0] - q[1]).abs() < 1e-7);
        assert!((h[1] - 2.5).abs() < 0.05);
    }

    #[test]
    fn pump_meets_system_curve_and_closed_valve_stops_flow() {
        // Pump h0 = 20, c = 20 / 0.01²; system r = 1e5 (level ground).
        let (h0, c, r) = (20.0f32, 20.0 / 1e-4, 1.0e5f32);
        let nodes = vec![node(Some(0.0)), node(None), node(Some(0.0))];
        let edges = vec![edge(0, 1, EdgeKind::Pump { h0, c }), edge(1, 2, EdgeKind::Resist { r })];
        let (mut q, mut h) = (vec![1e-4; 2], vec![0.0; 3]);
        newton(&nodes, &edges, &mut q, &mut h);
        let expect = (h0 / (c + r)).sqrt();
        assert!((q[0] - expect).abs() / expect < 0.02, "{} vs {expect}", q[0]);
        let edges = vec![edge(0, 1, EdgeKind::Pump { h0, c }), edge(1, 2, EdgeKind::Blocked)];
        let (mut q, mut h) = (vec![1e-4; 2], vec![0.0; 3]);
        newton(&nodes, &edges, &mut q, &mut h);
        assert!(q[1].abs() < 1e-7, "{}", q[1]);
    }

    #[test]
    fn crank_rig_round_trip() {
        let rig = vec![Drive::Spin(1.0), Drive::Piston { phase: 0.0, r: 0.04, l: 0.14 }, Drive::Rod { phase: 0.0, r: 0.04, l: 0.14 }];
        let q = pose(&rig, &[0.0; 3], std::f32::consts::PI, 1.0);
        assert!((q[0].abs() - 180.0).abs() < 1e-3);
        assert!((q[1] + 0.08).abs() < 1e-5, "piston drops a full stroke at BDC: {}", q[1]);
        assert!(q[2].abs() < 1e-3);
        let q = pose(&rig, &[0.0; 3], std::f32::consts::FRAC_PI_2, 1.0);
        assert!((q[2] + (0.04f32 / 0.14).asin().to_degrees()).abs() < 1e-3);
    }

    #[test]
    fn links_propagate_belt_ratio() {
        // Exercised through drives() in plant::tests (needs a model); here the
        // link bookkeeping alone.
        let mut l = Links::default();
        l.add(Link { a: PortRef::new("m", "shaft"), b: PortRef::new("p", "shaft"), kind: LinkKind::Shaft });
        l.add(Link { a: PortRef::new("m", "shaft"), b: PortRef::new("q", "shaft"), kind: LinkKind::Shaft });
        assert_eq!(l.0.len(), 1, "a port mates with one other port");
        assert_eq!(l.partner(&PortRef::new("q", "shaft")).unwrap().1.object, "m");
    }
}
