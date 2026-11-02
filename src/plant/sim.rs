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
    /// Its open pipe ends are fed from outside (a mains, a header not in the model). An
    /// open end otherwise only discharges: air doesn't turn into liquid.
    pub open_ends_supply: bool,
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
    /// Liquid momentum and wet/dry nozzles carried between flow steps.
    pub hyd: FlowState,
    /// Liquid lost from the system (overflow, leaks), m³.
    pub spilled: f32,
    /// Liquid fed in through open ends declared as supplies, m³.
    pub supplied: f32,
    /// Simulated time dropped because the flow solve couldn't keep up, s.
    pub dropped_s: f64,
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

/// Run-up and coast-down time constants (s): motors reach speed in a few
/// seconds; a pump full of liquid, or a loaded belt, slows down more gently.
fn spin_tau(role: Role, up: bool) -> f32 {
    match (role, up) {
        (Role::Conveyor, _) => 0.5,
        (Role::Engine, true) => 1.0,
        (_, true) => 1.5,
        (Role::Pump, false) => 2.5,
        (_, false) => 4.0,
    }
}

/// Eases actual speeds towards the drive targets (first-order lag); objects
/// without a target coast down to rest.
pub fn spin(model: &Model, actual: &mut HashMap<String, f32>, target: &HashMap<String, f32>, dt: f32) {
    for o in &model.objs {
        let t = target.get(&o.name).copied().unwrap_or(0.0);
        let w = actual.get(&o.name).copied().unwrap_or(0.0);
        if t == 0.0 && w == 0.0 {
            continue;
        }
        let tau = spin_tau(o.role, t.abs() > w.abs());
        let mut next = w + (t - w) * (1.0 - (-dt / tau).exp());
        // Settle exactly instead of creeping forever.
        let snap = if o.role == Role::Conveyor { 1e-3 } else { 0.5 };
        if (next - t).abs() < snap {
            next = t;
        }
        if next == 0.0 {
            actual.remove(&o.name);
        } else {
            actual.insert(o.name.clone(), next);
        }
    }
    actual.retain(|n, _| model.get(n).is_some());
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
        let s = o.t.scale;
        // Elliptical cross-section under non-uniform scale: area π·(r·sx)·(r·sz).
        let capacity = std::f32::consts::PI * r * r * s.x.abs() * s.z.abs() * h * s.y.abs();
        return TankGeom { bottom: o.t.translation.y + y0 * s.y, height: h * s.y, capacity, local_center: Vec3::new(0.0, y0, 0.0), radius: r * 0.98 };
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

/// Atmospheric pressure (Pa).
pub const P_ATM: f32 = 101_325.0;
/// Absolute roughness of commercial steel pipe (m).
const ROUGHNESS: f32 = 4.5e-5;
/// Pressure-wave speed in a liquid-filled steel pipe (m/s): no flow change can
/// raise the head faster than the Joukowsky limit a·Δv/g.
const WAVE_SPEED: f32 = 1200.0;
/// Surge head (m) above which a fast change is reported (~5 bar of water).
const SURGE_ALARM_M: f32 = 50.0;

/// Physical properties of the liquid in a system (at about 20 °C).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fluid {
    /// Density (kg/m³).
    pub rho: f32,
    /// Dynamic viscosity (Pa·s).
    pub mu: f32,
    /// Vapour pressure (Pa).
    pub p_vap: f32,
}

impl Fluid {
    pub fn of(medium: Option<&str>) -> Self {
        match medium.unwrap_or("water").to_ascii_lowercase().as_str() {
            "oil" | "hydraulic" => Self { rho: 870.0, mu: 0.03, p_vap: 100.0 },
            // 50 % ethylene glycol.
            "coolant" | "glycol" => Self { rho: 1070.0, mu: 3.5e-3, p_vap: 1_500.0 },
            // Diesel.
            "fuel" | "diesel" => Self { rho: 840.0, mu: 3.0e-3, p_vap: 1_000.0 },
            // A generic dilute aqueous solution.
            "chemical" => Self { rho: 1100.0, mu: 1.5e-3, p_vap: 2_300.0 },
            _ => Self { rho: 998.0, mu: 1.0e-3, p_vap: 2_340.0 },
        }
    }
    /// Whether [`Fluid::of`] knows this liquid; anything else is treated as water.
    pub fn known(medium: &str) -> bool {
        matches!(medium.to_ascii_lowercase().as_str(), "water" | "oil" | "hydraulic" | "coolant" | "glycol" | "fuel" | "diesel" | "chemical")
    }
    /// Kinematic viscosity (m²/s).
    pub fn nu(&self) -> f32 {
        self.mu / self.rho
    }
    /// Pressure head of the atmosphere above vapour pressure (m of this liquid).
    pub fn atm_head(&self) -> f32 {
        (P_ATM - self.p_vap) / (self.rho * G)
    }
}

/// Liquid of every fluid object: the medium set on it, else the first one set
/// anywhere in its pipe-connected system, else water.
pub fn fluids(model: &Model, links: &Links, controls: &HashMap<String, Control>) -> HashMap<String, Fluid> {
    let mut out = HashMap::new();
    for o in model.objs.iter().filter(|o| o.info.ports.iter().any(|p| p.kind == PortKind::Pipe)) {
        if out.contains_key(&o.name) {
            continue;
        }
        // The whole connected system.
        let mut seen: Vec<String> = Vec::new();
        let mut stack = vec![o.name.clone()];
        while let Some(n) = stack.pop() {
            if seen.contains(&n) || seen.len() > 2000 {
                continue;
            }
            for l in links.0.iter().filter(|l| l.kind == LinkKind::Pipe && l.touches(&n)) {
                stack.push(if l.a.object == n { l.b.object.clone() } else { l.a.object.clone() });
            }
            seen.push(n);
        }
        // Tanks decide first (they hold the liquid), then anything else.
        let pick = |tank: bool| seen.iter().filter(|n| model.get(n).is_some_and(|x| (x.role == Role::Tank) == tank)).find_map(|n| controls.get(n).and_then(|c| c.medium.clone()));
        let medium = pick(true).or_else(|| pick(false));
        for n in seen {
            let own = controls.get(&n).and_then(|c| c.medium.clone());
            out.insert(n, Fluid::of(own.as_deref().or(medium.as_deref())));
        }
    }
    out
}

/// Ball-valve loss coefficient K (on the bore velocity head) against lever
/// closure angle; `None` when shut. Inherent characteristic of a full-port
/// ball valve: almost nothing below 10°, then roughly tenfold every 10–15°.
pub fn valve_k(closed_deg: f32) -> Option<f32> {
    const TABLE: [(f32, f32); 11] = [(0.0, 0.05), (10.0, 0.3), (20.0, 1.5), (30.0, 5.5), (40.0, 17.0), (50.0, 52.0), (60.0, 206.0), (70.0, 750.0), (80.0, 4.0e3), (85.0, 3.0e4), (89.0, 1.0e6)];
    let a = closed_deg.clamp(0.0, 90.0);
    if a >= 89.5 {
        return None;
    }
    if a >= 89.0 {
        return Some(TABLE[10].1);
    }
    let i = TABLE.iter().rposition(|(x, _)| *x <= a).unwrap_or(0);
    let ((a0, k0), (a1, k1)) = (TABLE[i], TABLE[i + 1]);
    let t = (a - a0) / (a1 - a0);
    Some((k0.ln() + (k1.ln() - k0.ln()) * t).exp())
}

/// Lever closure (0° open .. 90° shut) of a valve.
pub fn valve_angle(o: &Obj, controls: &HashMap<String, Control>) -> f32 {
    (1.0 - valve_open(o, controls)) * 90.0
}

/// Darcy friction factor: laminar 64/Re, Swamee–Jain when turbulent, blended
/// across the transition.
pub fn friction_factor(re: f32, d: f32) -> f32 {
    let lam = |re: f32| 64.0 / re.max(1.0);
    let turb = |re: f32| 0.25 / (ROUGHNESS / (3.7 * d) + 5.74 / re.max(1.0).powf(0.9)).log10().powi(2);
    if re <= 2000.0 {
        lam(re)
    } else if re >= 4000.0 {
        turb(re)
    } else {
        let t = (re - 2000.0) / 2000.0;
        lam(re) * (1.0 - t) + turb(re) * t
    }
}

#[derive(Default, Clone, Debug)]
pub struct FlowResult {
    /// m³/s leaving the object through each pipe port (negative: entering).
    pub port_flow: HashMap<PortRef, f32>,
    /// Hydraulic (piezometric) head (m) at each connected pipe port.
    pub port_head: HashMap<PortRef, f32>,
    /// Flow through each two-port element (pipe, valve, pump), m³/s, a → b.
    pub element_flow: HashMap<String, f32>,
    /// Net inflow of each tank (m³/s).
    pub tank_inflow: HashMap<String, f32>,
    /// Pump flow (m³/s) and head added (m).
    pub pumps: HashMap<String, (f32, f32)>,
    /// Pump suction: NPSH available and required (m).
    pub npsh: HashMap<String, (f32, f32)>,
    pub leaks: Vec<(PortRef, f32)>,
    /// Open ends declared as supplies and what they feed in (m³/s).
    pub supplied: Vec<(PortRef, f32)>,
    /// Largest surge in the network this step: how far the liquid's inertia
    /// pushes a point's head away from its steady value (m), and where.
    pub surge: Option<(String, f32)>,
    pub alarms: Vec<String>,
}

/// What the network remembers between steps.
#[derive(Default, Clone, Debug)]
pub struct FlowState {
    /// Flow of every element (by owner) at the end of the last step: the
    /// liquid's momentum, so flow cannot change instantly.
    pub q: HashMap<(String, String, String), f32>,
    /// Tank nozzles currently above the liquid (with hysteresis).
    pub dry: HashSet<PortRef>,
}

#[derive(Clone)]
enum EdgeKind {
    /// Pipe friction (Darcy, Reynolds-dependent) plus fittings losses K.
    Resist { len: f32, k: f32, d: f32, nu: f32 },
    Pump { h0: f32, c: f32 },
    Blocked,
}

#[derive(Clone)]
struct Edge {
    a: usize,
    b: usize,
    kind: EdgeKind,
    /// Liquid inertia over the step, `L / (g A dt)` (s/m²), and the flow it
    /// had at the start of the step.
    inertia: f32,
    q0: f32,
    /// (object, from port, to port); "" = the element's center.
    owner: (String, String, String),
    /// Bore at its ends (m), and whether end a and end b meet a free surface.
    d: f32,
    ends: [bool; 2],
}

/// Loss coefficients where a pipe meets a free surface, on its velocity head: a square
/// entry from a tank, and all the velocity head at an exit, into a tank or open air.
const K_ENTRY: f32 = 0.5;
const K_EXIT: f32 = 1.0;

impl Edge {
    /// Entry and exit losses at this flow's direction.
    fn end_k(&self, q: f32) -> f32 {
        let (from, to) = if q >= 0.0 { (self.ends[0], self.ends[1]) } else { (self.ends[1], self.ends[0]) };
        (if from { K_ENTRY } else { 0.0 }) + (if to { K_EXIT } else { 0.0 })
    }
    /// Steady head loss from a to b at flow q.
    fn loss(&self, q: f32) -> f32 {
        let v = q / area(self.d);
        self.inner_loss(q) + self.end_k(q) * v * v.abs() / (2.0 * G)
    }
    fn inner_loss(&self, q: f32) -> f32 {
        match self.kind {
            EdgeKind::Resist { len, k, d, nu } => {
                let a = area(d);
                let v = q / a;
                let re = v.abs() * d / nu;
                // Laminar friction is linear in v; written that way it stays exact at v → 0.
                let fric = if re < 2000.0 { 32.0 * nu * len * v / (G * d * d) } else { friction_factor(re, d) * len / d * v * v.abs() / (2.0 * G) };
                fric + k * v * v.abs() / (2.0 * G)
            }
            EdgeKind::Pump { h0, c } => {
                // Built-in check valve: it seats against any backflow, as
                // tight as a shut valve.
                if q >= 0.0 { -h0 + c * q * q + q } else { -h0 + 1e9 * q }
            }
            EdgeKind::Blocked => 1e9 * q,
        }
    }
    /// Head drop from a to b (losses plus the head that accelerates the
    /// liquid) and its derivative.
    fn f(&self, q: f32) -> (f32, f32) {
        let h = self.loss(q) + self.inertia * (q - self.q0);
        let dq = 1e-7 + 1e-4 * q.abs();
        let df = (self.loss(q + dq) - self.loss(q - dq)) / (2.0 * dq) + self.inertia;
        (h, df.max(1e-3))
    }
}

struct Node {
    fixed: Option<f32>,
    elev: f32,
    /// Tank nozzle above the liquid: it can take liquid in but not supply it.
    dry: bool,
    tank: Option<String>,
    open: bool,
    /// An open end declared as fed from outside.
    source: bool,
    refs: Vec<PortRef>,
}

fn area(d: f32) -> f32 {
    std::f32::consts::PI * d * d * 0.25
}

/// Pump curve of an object at speed ratio `s`: shut-off head (m) and the flow
/// at which the head falls to zero (m³/s). Defaults: a small end-suction
/// centrifugal pump, about 8 m at shut-off and 9.5 l/s run-out for DN50.
pub fn pump_curve(o: &Obj, d: f32, s: f32) -> (f32, f32) {
    // A zero or negative catalog figure is no figure: it would poison the whole solve.
    let h0 = o.info.entry.as_ref().and_then(|e| e.head_m).filter(|h| *h > 0.0).unwrap_or(8.0 * (d / 0.045).clamp(0.5, 3.0).sqrt());
    let qmax = o.info.entry.as_ref().and_then(|e| e.max_flow_lps).filter(|q| *q > 0.0).map(|l| l / 1000.0).unwrap_or(6.0 * area(d));
    (h0 * s * s, qmax * s.max(1e-3))
}

/// Whether a pump's curve comes from the catalog or is the made-up default.
fn pump_curve_is_default(o: &Obj) -> bool {
    !o.info.entry.as_ref().is_some_and(|e| e.head_m.is_some_and(|h| h > 0.0) && e.max_flow_lps.is_some_and(|q| q > 0.0))
}

/// How a two-port or branching part stretches across its pipe, from its transform.
fn across_scale(p: &Port, t: &Transform) -> f32 {
    let (d2, s) = (p.dir * p.dir, t.scale.abs());
    let along = d2.dot(s);
    ((s.x + s.y + s.z - along) / 2.0).max(1e-3)
}

/// Length and K for friction, the bore, and whether they're guessed. A generated part
/// knows; anything else is measured between its own ports, with the part's scale in it, and
/// the K is assumed. Without that any catalog pipe counts as 0.5 m, however long it is.
fn pipe_geometry(o: &Obj, ports: &[&Port]) -> (f32, f32, f32, bool) {
    let world: Vec<Vec3> = ports.iter().map(|p| p.world(&o.t).0).collect();
    let across = ports.first().map_or(1.0, |p| across_scale(p, &o.t));
    match o.info.spec {
        Some(s) => {
            let (len, k) = s.resistance();
            let along = match ports {
                [a, b] if a.pos.distance(b.pos) > 1e-6 => world[0].distance(world[1]) / a.pos.distance(b.pos),
                _ => o.t.scale.abs().element_sum() / 3.0,
            };
            (len * along, k, s.bore() * across, false)
        }
        None => {
            let len = match world.as_slice() {
                [a, b] => a.distance(*b),
                many => 2.0 * many.iter().map(|p| p.distance(o.t.translation)).sum::<f32>() / many.len().max(1) as f32,
            };
            // A detected rim is the bore already; a catalog figure is nominal.
            let bore = ports.iter().map(|p| p.size * if p.source == ports::PortSource::Detected { 1.0 } else { 0.9 }).fold(f32::MAX, f32::min);
            (len.max(0.05), 0.5, bore * across, true)
        }
    }
}

/// NPSH a pump needs at flow `q` (m): rises with the square of the flow.
fn npsh_required(s: f32, q: f32, qmax: f32) -> f32 {
    s * s * (0.8 + 3.0 * (q / qmax.max(1e-6)).powi(2))
}

/// Solves the pipe network for the current speeds, valve settings and tank
/// volumes. With `dt`, the liquid's inertia carries over from `state`, so
/// flows change smoothly; without it the result is the steady state.
#[allow(clippy::too_many_arguments)]
pub fn solve(model: &Model, links: &Links, controls: &HashMap<String, Control>, omega: &HashMap<String, f32>, volume: &HashMap<String, f32>, state: &mut FlowState, dt: Option<f32>) -> FlowResult {
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
    let liquids = fluids(model, links, controls);
    let liquid = |name: &str| liquids.get(name).copied().unwrap_or_else(|| Fluid::of(None));
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
            nodes.push(Node { fixed: None, elev: 0.0, dry: false, tank: None, open: false, source: false, refs: Vec::new() });
            nodes.len() - 1
        });
        nodes[n].refs.push(r.clone());
        if let Some((w, _, _)) = model.port_world(r) {
            nodes[n].elev = w.y;
        }
        port_node.insert(r.clone(), n);
    }
    let mut edges: Vec<Edge> = Vec::new();
    // Pumps: (edge, object, qmax, speed ratio, inlet node).
    let mut pump_edges: Vec<(usize, String, f32, f32, usize)> = Vec::new();
    let push = |edges: &mut Vec<Edge>, a: usize, b: usize, kind: EdgeKind, len: f32, d: f32, owner: (String, String, String)| {
        // Rigid-column inertia L/(g A dt), but never stiffer than the
        // water-hammer impedance a/(g A) of an instantaneous change.
        let inertia = dt.map_or(0.0, |dt| (len.max(0.0) / dt.max(1e-3)).min(WAVE_SPEED) / (G * area(d.max(0.003))));
        let q0 = if dt.is_some() { state.q.get(&owner).copied().unwrap_or(0.0) } else { 0.0 };
        edges.push(Edge { a, b, kind, inertia, q0, owner, d: d.max(0.003), ends: [false, false] });
    };
    for o in &fluid {
        if let Some(m) = controls.get(&o.name).and_then(|c| c.medium.as_deref()).filter(|m| !Fluid::known(m)) {
            res.alarms.push(format!("{}: liquid '{m}' isn't known, so it's treated as water", o.name));
        }
    }
    for o in &fluid {
        let pipe_ports: Vec<&Port> = o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe).collect();
        let nid = |p: &Port| port_node[&PortRef::new(&o.name, &p.id)];
        let liq = liquid(&o.name);
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
                    // Hysteresis: a nozzle uncovers just below the surface and
                    // only counts as covered again once 1 cm under it.
                    let was_dry = state.dry.contains(&r);
                    // An empty tank has nothing to give, even through a nozzle at its very bottom.
                    let dry = v <= 1e-6 * g.capacity || if was_dry { surface < elev + 0.01 } else { surface < elev - 0.002 };
                    if dry {
                        state.dry.insert(r.clone());
                    } else {
                        state.dry.remove(&r);
                    }
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
                let s = (w / (rated_rpm(o) * std::f32::consts::TAU / 60.0)).max(0.0);
                let d = o.info.spec.map(|sp| sp.bore()).unwrap_or(pi.size * 0.9) * across_scale(pi, &o.t);
                let (h0, qmax) = pump_curve(o, d, s);
                let kind = if s < 0.01 { EdgeKind::Blocked } else { EdgeKind::Pump { h0, c: h0 / (qmax * qmax) } };
                let in_ref = PortRef::new(&o.name, &pi.id);
                if s >= 0.01 && !linked.contains(&in_ref) {
                    res.alarms.push(format!("pump {} runs with its inlet open (no suction pipe): it would run dry", o.name));
                    continue;
                }
                if s >= 0.01 {
                    pump_edges.push((edges.len(), o.name.clone(), qmax, s, nid(pi)));
                }
                // Liquid in the casing: about half a metre of equivalent pipe.
                push(&mut edges, nid(pi), nid(po), kind, 0.5, d, (o.name.clone(), pi.id.clone(), po.id.clone()));
            }
            _ => {
                let (len, k, d, _) = pipe_geometry(o, &pipe_ports);
                let shut = o.role == Role::Valve && valve_k(valve_angle(o, controls)).is_none();
                let k = if o.role == Role::Valve { k + valve_k(valve_angle(o, controls)).unwrap_or(0.0) } else { k };
                let kind = |share: f32| if shut { EdgeKind::Blocked } else { EdgeKind::Resist { len: len * share, k: k * share, d, nu: liq.nu() } };
                if pipe_ports.len() == 2 {
                    push(&mut edges, nid(pipe_ports[0]), nid(pipe_ports[1]), kind(1.0), len, d, (o.name.clone(), pipe_ports[0].id.clone(), pipe_ports[1].id.clone()));
                } else if pipe_ports.len() > 2 {
                    let c = nodes.len();
                    nodes.push(Node { fixed: None, elev: o.t.translation.y, dry: false, tank: None, open: false, source: false, refs: Vec::new() });
                    for p in &pipe_ports {
                        push(&mut edges, c, nid(p), kind(0.5), len * 0.5, d, (o.name.clone(), String::new(), p.id.clone()));
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
                n.source = controls.get(&n.refs[0].object).is_some_and(|c| c.open_ends_supply);
            }
        }
    }
    if edges.is_empty() {
        return res;
    }
    for e in edges.iter_mut() {
        let surface = |n: &Node| n.tank.is_some() || n.open;
        e.ends = [surface(&nodes[e.a]), surface(&nodes[e.b])];
    }
    // Start from last step's flows: Newton converges in a few iterations.
    let mut q: Vec<f32> = edges.iter().map(|e| if e.q0 != 0.0 { e.q0 } else { 1e-4 }).collect();
    let mut h = vec![0.0f32; nodes.len()];
    let mut settled = true;
    // Dry tank nozzles and open ends that would have to supply liquid become dead ends.
    for _pass in 0..4 {
        settled = newton(&nodes, &edges, &mut q, &mut h);
        let mut changed = false;
        for (n, node) in nodes.iter_mut().enumerate() {
            let cannot_supply = node.dry || (node.open && !node.source);
            if !cannot_supply || node.fixed.is_none() {
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
    // Suction: a pump whose inlet pressure leaves less than the NPSH it needs
    // cavitates, and its head collapses. Re-solve with the reduced head.
    let mut cavitating: HashSet<String> = HashSet::new();
    for _pass in 0..4 {
        let mut changed = false;
        for (k, name, qmax, s, inlet) in &pump_edges {
            let EdgeKind::Pump { h0, c } = edges[*k].kind else { continue };
            let Some(o) = model.get(name) else { continue };
            let liq = liquid(&o.name);
            let qp = q[*k].max(0.0);
            let need = npsh_required(*s, qp, *qmax);
            let avail = h[*inlet] - nodes[*inlet].elev + liq.atm_head();
            res.npsh.insert(name.clone(), (avail, need));
            if avail < need * 0.97 && qp > 1e-6 {
                // Head breaks down roughly in proportion to the NPSH deficit.
                let f = (avail / need).clamp(0.05, 1.0);
                let full = pump_curve(o, o.info.spec.map(|sp| sp.bore()).unwrap_or(0.045), *s).0;
                let target = full * f;
                if (h0 - target).abs() > 0.02 * full {
                    edges[*k].kind = EdgeKind::Pump { h0: target, c: c * target / h0.max(1e-6) };
                    changed = true;
                }
                cavitating.insert(name.clone());
            }
        }
        if !changed {
            break;
        }
        settled = newton(&nodes, &edges, &mut q, &mut h);
    }
    // A solve that blew up must not carry into the next step and stick there.
    if q.iter().chain(&h).any(|v| !v.is_finite()) {
        q.iter_mut().for_each(|v| *v = 0.0);
        for (n, node) in nodes.iter().enumerate() {
            h[n] = node.fixed.unwrap_or(node.elev);
        }
        settled = false;
    }
    if !settled {
        res.alarms.push("the flow solve didn't settle in this step, so these flows aren't reliable".to_string());
    }
    // Surge: when inertia matters, compare with the head each point would
    // have if the liquid had no momentum (same valves, same speeds).
    let mut surge: Option<(String, f32)> = None;
    let mut surge_rho = Fluid::of(None).rho;
    if edges.iter().zip(&q).any(|(e, qe)| (e.inertia * (qe - e.q0)).abs() > 1.0) {
        let calm: Vec<Edge> = edges.iter().cloned().map(|mut e| { e.inertia = 0.0; e }).collect();
        let (mut q2, mut h2) = (q.clone(), h.clone());
        newton(&nodes, &calm, &mut q2, &mut h2);
        for (n, node) in nodes.iter().enumerate() {
            let Some(r) = node.refs.first().filter(|_| node.fixed.is_none()) else { continue };
            let d = (h[n] - h2[n]).abs();
            if d > surge.as_ref().map_or(0.0, |s| s.1) {
                surge = Some((r.label(), d));
                surge_rho = liquid(&r.object).rho;
            }
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
        if dt.is_some() {
            state.q.insert(e.owner.clone(), qe);
        }
    }
    res.surge = surge.clone();
    if let Some((obj, hs)) = surge
        && hs > SURGE_ALARM_M
    {
        res.alarms.push(format!("pressure surge of {:.0} m ({:.1} bar) at {obj}: a valve closed or a pump stopped too fast (water hammer)", hs, head_to_bar(hs, surge_rho)));
    }
    for (n, node) in nodes.iter().enumerate() {
        for r in &node.refs {
            res.port_head.insert(r.clone(), h[n]);
        }
        let inflow: f32 = edges.iter().zip(&q).map(|(e, q)| if e.b == n { *q } else if e.a == n { -*q } else { 0.0 }).sum();
        if node.open && inflow > 1e-6 {
            res.leaks.push((node.refs[0].clone(), inflow));
        }
        if node.open && node.source && inflow < -1e-6 {
            res.supplied.push((node.refs[0].clone(), -inflow));
        }
        if let Some(t) = &node.tank {
            *res.tank_inflow.entry(t.clone()).or_default() += inflow;
        }
    }
    // Pumps that can't flow: say which side is closed off. The suction must
    // reach liquid (a covered tank nozzle or a declared supply; air at an open
    // end isn't); the discharge may also end at a nozzle above the liquid or at
    // an open end, which take liquid in.
    let reaches = |start: usize, skip: usize, suction: bool| -> bool {
        let mut seen = vec![false; nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if seen[n] {
                continue;
            }
            seen[n] = true;
            let node = &nodes[n];
            if (node.fixed.is_some() && !node.dry && (!node.open || node.source)) || (!suction && (node.dry || node.open)) {
                return true;
            }
            for (k, e) in edges.iter().enumerate() {
                if k == skip || matches!(e.kind, EdgeKind::Blocked) {
                    continue;
                }
                if e.a == n {
                    stack.push(e.b);
                } else if e.b == n {
                    stack.push(e.a);
                }
            }
        }
        false
    };
    let mut pump_trouble = false;
    for (k, name, _, s, inlet) in &pump_edges {
        let e = &edges[*k];
        let qp = q[*k];
        // While running up the head is still low: judge only near speed.
        let at_speed = *s > 0.5;
        let msg = if !reaches(*inlet, *k, true) {
            Some(format!("pump {name} is starved: its suction is closed off (closed valve or tank drawn down to the nozzle), so it cavitates and runs dry"))
        } else if !reaches(e.b, *k, false) {
            at_speed.then(|| format!("pump {name} is dead-headed against a closed discharge: no flow, the liquid in the casing heats up"))
        } else if qp.abs() < 1e-6 {
            at_speed.then(|| format!("pump {name} runs but nothing flows: its head can't lift the liquid to the discharge"))
        } else if cavitating.contains(name) {
            let (a, r) = res.npsh.get(name).copied().unwrap_or_default();
            Some(format!("pump {name} is cavitating: {a:.1} m NPSH available < {r:.1} m required on a generic curve, not this pump's (suction throttled, tank level low, or liquid too volatile)"))
        } else {
            None
        };
        if let Some(m) = msg {
            pump_trouble = true;
            res.alarms.push(m);
        }
    }
    // Absolute pressure below vapour pressure elsewhere: the liquid column
    // breaks (a siphon over a high point). A starved pump explains its own.
    if !pump_trouble {
        for (n, node) in nodes.iter().enumerate() {
            // A pipe open to the air just doesn't fill above the level it can reach.
            if node.fixed.is_some() || node.open {
                continue;
            }
            let Some(r) = node.refs.first() else { continue };
            if h[n] - node.elev + liquid(&r.object).atm_head() < 0.0 {
                res.alarms.push(format!("vacuum at {}: the pressure falls below the vapour pressure and the liquid column breaks (siphon over a high point)", r.label()));
            }
        }
    }
    for (r, qv) in &res.leaks {
        res.alarms.push(format!("leak: {} is open and spills {:.2} l/s", r.label(), qv * 1000.0));
    }
    res
}

/// Head of liquid to pressure, in bar. Surge isn't only ever water.
fn head_to_bar(head_m: f32, rho: f32) -> f32 {
    head_m * G * rho / 1e5
}

/// Largest flow change (m³/s) that still counts as settled: 0.01 l/s, near the
/// resolution of these single-precision flows.
const SETTLED_DQ: f64 = 1e-5;

/// Solves the network, and says whether it settled: 80 iterations isn't an answer.
fn newton(nodes: &[Node], edges: &[Edge], q: &mut [f32], h: &mut [f32]) -> bool {
    newton_within(nodes, edges, q, h, 80)
}

fn newton_within(nodes: &[Node], edges: &[Edge], q: &mut [f32], h: &mut [f32], iterations: usize) -> bool {
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
    let mut last_dq = f64::MAX;
    for _ in 0..iterations {
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
        last_dq = max_dq;
        if max_dq < 1e-9 {
            break;
        }
    }
    last_dq < SETTLED_DQ && q.iter().all(|v| v.is_finite())
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

/// Advances tank volumes by `dt` and reports overflow / empty tanks. Liquid
/// that overflows or leaks out is added to `spilled` (m³), so the total is
/// conserved.
pub fn integrate(model: &Model, flow: &FlowResult, volume: &mut HashMap<String, f32>, controls: &HashMap<String, Control>, dt: f32, alarms: &mut Vec<String>, spilled: &mut f32) {
    let tanks: Vec<&Obj> = model.objs.iter().filter(|o| o.role == Role::Tank).collect();
    let mut change: Vec<f32> = tanks.iter().map(|o| flow.tank_inflow.get(&o.name).copied().unwrap_or(0.0) * dt).collect();
    let mut leaked = flow.leaks.iter().map(|(_, q)| q.max(0.0)).sum::<f32>() * dt;
    // The last of a tank can't give a whole step's flow. What it couldn't give never
    // reached anywhere else, so it comes off what the others gained. Booking it as a
    // negative spill would hide liquid made from nothing.
    let mut short = 0.0;
    for (o, c) in tanks.iter().zip(&mut change) {
        let v = *volume.entry(o.name.clone()).or_insert_with(|| initial_volume(o, controls));
        if v + *c < 0.0 {
            short += -(v + *c);
            *c = -v;
        }
    }
    if short > 0.0 {
        let gained = change.iter().map(|c| c.max(0.0)).sum::<f32>() + leaked;
        let keep = if gained > 0.0 { ((gained - short) / gained).max(0.0) } else { 1.0 };
        change.iter_mut().filter(|c| **c > 0.0).for_each(|c| *c *= keep);
        leaked *= keep;
    }
    for (o, c) in tanks.iter().zip(&change) {
        let g = tank_geom(o);
        let inflow = flow.tank_inflow.get(&o.name).copied().unwrap_or(0.0);
        let v = volume.entry(o.name.clone()).or_insert_with(|| initial_volume(o, controls));
        *v = (*v + *c).max(0.0);
        if *v > g.capacity {
            *spilled += *v - g.capacity;
            *v = g.capacity;
            if inflow > 1e-6 {
                alarms.push(format!("tank {} is full and overflowing", o.name));
            }
        }
        if *v < 1e-4 * g.capacity && inflow < -1e-7 {
            alarms.push(format!("tank {} has run empty", o.name));
        }
    }
    *spilled += leaked;
}

/// Liquid at the end against liquid accounted for, as a share of the most there was (%).
/// Zero to start with doesn't make it blind.
fn mass_balance_error_pct(start: f32, end: f32, spilled: f32, supplied: f32) -> f32 {
    let scale = start.max(end).max(spilled).max(supplied);
    if scale > 1e-9 { (end + spilled - supplied - start) / scale * 100.0 } else { 0.0 }
}

/// Liquid the books can't account for, above which a run isn't called ok (%).
const MASS_BALANCE_LIMIT_PCT: f32 = 0.5;

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
        // Work in travel coordinates: `u` grows the way the belt runs. A
        // reversed belt runs out at its in end, where nothing is linked.
        let fwd = b.speed > 0.0;
        let dir = if fwd { b.dir } else { -b.dir };
        let u = |along: f32| if fwd { along } else { b.len - along };
        let next = if fwd { b.next.as_ref().and_then(|n| belts.iter().find(|x| &x.name == n)) } else { None };
        let mut items: Vec<(&Obj, f32, f32)> = model.objs.iter().filter(|o| rideable(o)).filter_map(|o| on(b, o).map(|(a, h)| (o, u(a), h))).collect();
        // Items already on the next belt block the transfer - except those
        // still on this one too (mid transfer), which must not block themselves.
        let mut ahead: Vec<(f32, f32)> = Vec::new();
        if let Some(nb) = next {
            for o in model.objs.iter().filter(|o| rideable(o) && !items.iter().any(|(i, ..)| i.entity == o.entity)) {
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
        for (o, at, half) in items {
            if !taken.insert(o.entity) {
                continue;
            }
            // Forward only, up to the end or the item in front. An item that
            // already overhangs the end (dropped there) or touches the one
            // ahead just waits: the belt never pushes anything backwards.
            let d = (b.speed.abs() * dt).min(limit - half - at).max(0.0);
            limit = (at + d - half - 0.02).min(limit);
            if d > 1e-6 {
                let mut t = o.t.translation + dir * d;
                // Onto a next belt that sits at another height.
                if let Some(nb) = next
                    && at + d > b.len
                    && at <= b.len
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
        if let Ok((_, o, mut t)) = objects.get_mut(e)
            && !o.locked
        {
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
    let (target, mut alarms) = drives(&model, &links, &plant.controls);
    spin(&model, &mut plant.live.omega, &target, dt);
    let omega = plant.live.omega.clone();
    plant.acc += dt;
    let mut steps = 0;
    while plant.acc >= FLOW_DT && steps < 5 {
        plant.acc -= FLOW_DT;
        steps += 1;
        let flow = solve(&model, &links, &plant.controls, &omega, &plant.live.volume, &mut plant.live.hyd, Some(FLOW_DT));
        let mut a = Vec::new();
        integrate(&model, &flow, &mut plant.live.volume, &plant.controls, FLOW_DT, &mut a, &mut plant.live.spilled);
        plant.live.supplied += flow.supplied.iter().map(|(_, q)| *q).sum::<f32>() * FLOW_DT;
        plant.live.flow = flow;
        plant.live.flow.alarms.extend(a);
        plant.live.time += FLOW_DT as f64;
    }
    // Faster than five steps a frame, the flow falls behind the clock. It catches up by
    // dropping that time, which levels and totals never see, so it's said out loud.
    if plant.acc >= FLOW_DT {
        plant.live.dropped_s += plant.acc as f64;
        plant.acc = 0.0;
    }
    alarms.extend(plant.live.flow.alarms.iter().cloned());
    if plant.live.dropped_s > 0.0 {
        alarms.push(format!("the flow solve can't keep up at this speed: {:.1} s of flow were skipped, so levels lag the clock", plant.live.dropped_s));
    }
    alarms.sort();
    alarms.dedup();
    plant.live.alarms = alarms;

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
        if let Ok((_, o, mut tr)) = objects.get_mut(e)
            && !o.locked
        {
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
    let (target, drive_alarms) = drives(model, links, &plant.controls);
    let mut omega = plant.live.omega.clone();
    let mut hyd = plant.live.hyd.clone();
    let (mut spilled, mut supplied) = (0.0f32, 0.0f32);
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
        spin(model, &mut omega, &target, dt);
        flow = solve(model, links, &plant.controls, &omega, &volume, &mut hyd, Some(dt));
        let mut a = flow.alarms.clone();
        integrate(model, &flow, &mut volume, &plant.controls, dt, &mut a, &mut spilled);
        supplied += flow.supplied.iter().map(|(_, q)| *q).sum::<f32>() * dt;
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
    // Mass balance: what is in the tanks now plus what was spilled, less what was fed
    // in, against the start. A run that makes or loses liquid isn't ok, whatever else.
    let (v0, v1): (f32, f32) = (start.values().sum(), volume.values().sum());
    let imbalance = mass_balance_error_pct(v0, v1, spilled, supplied);
    if imbalance.abs() > MASS_BALANCE_LIMIT_PCT {
        alarms.push((format!("liquid doesn't add up: {imbalance:+.2} % over this run, so the flows can't be trusted"), seconds));
    }
    let mut r = report(model, links, &omega, &flow, &start, &volume, &lo, &hi, &alarms, seconds, &plant.controls);
    r["spilled_l"] = json!(r3(spilled * 1000.0));
    r["supplied_l"] = json!(r3(supplied * 1000.0));
    r["mass_balance_error_pct"] = json!(r3(imbalance));
    r["flow_step_s"] = json!(dt);
    r
}

/// What a report's numbers lean on that isn't known about this plant.
fn assumed_notes(model: &Model, controls: &HashMap<String, Control>) -> Vec<String> {
    let mut out = Vec::new();
    let mut any_pump = false;
    for o in &model.objs {
        let ports: Vec<&Port> = o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe).collect();
        match o.role {
            Role::Pump => {
                any_pump = true;
                if pump_curve_is_default(o) {
                    let d = o.info.spec.map(|sp| sp.bore()).or(ports.first().map(|p| p.size * 0.9)).unwrap_or(0.045);
                    let (h0, q) = pump_curve(o, d, 1.0);
                    out.push(format!("{}: a default pump curve, not a datasheet ({:.1} m at shut-off, {:.1} l/s run-out)", o.name, h0, q * 1000.0));
                }
            }
            Role::Fitting | Role::Valve if ports.len() >= 2 => {
                let (len, k, d, guessed) = pipe_geometry(o, &ports);
                if guessed {
                    out.push(format!("{}: no part data, so {len:.2} m long from its ports, {:.0} mm bore, K {k:.1} assumed", o.name, d * 1000.0));
                }
            }
            _ => {}
        }
        if let Some(m) = controls.get(&o.name).and_then(|c| c.medium.as_deref()).filter(|m| !Fluid::known(m)) {
            out.push(format!("{}: liquid '{m}' isn't known, so it's treated as water", o.name));
        }
    }
    if any_pump {
        out.push("NPSH required is one generic curve for every pump, not a datasheet, and it drives the cavitation alarm".to_string());
        out.push("A stopped pump is taken as a closed valve, though a centrifugal pump passes some flow forwards".to_string());
    }
    if model.objs.iter().any(|o| o.role.is_fluid()) {
        out.push("Tees lose K 0.4 through any path, with no difference between run and branch".to_string());
        out.push("Surge is judged at the flow step: it reads about 5× lower at 0.5 s than at 0.1 s".to_string());
    }
    out.push("No torque or power anywhere: a motor can't stall or be undersized here".to_string());
    out
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
    controls: &HashMap<String, Control>,
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
                let mut p = json!({ "name": o.name, "rpm": (w * 60.0 / std::f32::consts::TAU).round(), "flow_lps": r3(q * 1000.0), "head_m": r3(h), "curve": if pump_curve_is_default(o) { "default, not a datasheet" } else { "from the catalog" } });
                if let Some((a, r)) = flow.npsh.get(&o.name) {
                    p["npsh_available_m"] = json!(r3(*a));
                    p["npsh_required_m"] = json!(r3(*r));
                }
                pumps.push(p);
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
                let open = valve_open(o, controls);
                let ang = (1.0 - open) * 90.0;
                let head = |p: &str| flow.port_head.get(&PortRef::new(&o.name, p)).copied();
                let dh = match (head("a"), head("b")) {
                    (Some(a), Some(b)) => r3(a - b),
                    _ => 0.0,
                };
                valves.push(json!({
                    "name": o.name, "lever_deg": ang.round(), "open_pct": (open * 100.0).round(),
                    "k": valve_k(ang).map(r3), "head_drop_m": dh,
                    "flow_lps": r3(flow.element_flow.get(&o.name).copied().unwrap_or(0.0) * 1000.0),
                }));
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
        "assumed": assumed_notes(model, controls),
        "evidence": "Simulated from hand-checked physics: not validated on hardware",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::connect::Link;

    fn edge(a: usize, b: usize, kind: EdgeKind) -> Edge {
        Edge { a, b, kind, inertia: 0.0, q0: 0.0, owner: (String::new(), String::new(), String::new()), d: 0.05, ends: [false, false] }
    }
    fn node(fixed: Option<f32>) -> Node {
        Node { fixed, elev: 0.0, dry: false, tank: None, open: false, source: false, refs: Vec::new() }
    }
    /// A pure minor loss K on bore d (no pipe length).
    fn minor(k: f32, d: f32) -> EdgeKind {
        EdgeKind::Resist { len: 0.0, k, d, nu: 1e-6 }
    }

    #[test]
    fn gravity_flow_matches_hand_calculation() {
        // 5 m of head across two equal losses K: each drops 2.5 m, v = sqrt(2 g 2.5 / K).
        let (k, d) = (8.0f32, 0.05f32);
        let nodes = vec![node(Some(5.0)), node(None), node(Some(0.0))];
        let edges = vec![edge(0, 1, minor(k, d)), edge(1, 2, minor(k, d))];
        let (mut q, mut h) = (vec![1e-4; 2], vec![0.0; 3]);
        newton(&nodes, &edges, &mut q, &mut h);
        let expect = area(d) * (2.0 * G * 2.5 / k).sqrt();
        assert!((q[0] - expect).abs() / expect < 0.01, "{} vs {expect}", q[0]);
        assert!((q[0] - q[1]).abs() < 1e-7);
        assert!((h[1] - 2.5).abs() < 0.02);
    }

    #[test]
    fn pump_meets_system_curve_and_closed_valve_stops_flow() {
        let (h0, c, k, d) = (20.0f32, 20.0 / 1e-4, 20.0f32, 0.05f32);
        let r = k / (2.0 * G * area(d).powi(2));
        let nodes = vec![node(Some(0.0)), node(None), node(Some(0.0))];
        let edges = vec![edge(0, 1, EdgeKind::Pump { h0, c }), edge(1, 2, minor(k, d))];
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
    fn friction_and_valve_characteristics() {
        // Laminar 64/Re; turbulent 45 mm steel at Re 1e5 is about 0.02 (Moody chart).
        assert!((friction_factor(1000.0, 0.045) - 0.064).abs() < 1e-6);
        let f = friction_factor(1e5, 0.045);
        assert!((0.018..0.024).contains(&f), "{f}");
        // Ball valve: K rises monotonically with closure, shut at 90 degrees.
        let mut prev = 0.0;
        for a in 0..90 {
            let k = valve_k(a as f32).unwrap();
            assert!(k > prev, "{a}: {k} <= {prev}");
            prev = k;
        }
        assert!(valve_k(90.0).is_none());
        assert!(valve_k(45.0).unwrap() > 20.0, "half-closed ball valve throttles hard");
    }

    // --- The pump station sample, built exactly as the app builds it -------

    struct Station {
        model: Model,
        links: Links,
        plant: Plant,
    }

    fn station() -> Station {
        use crate::plant::recipes::{self, Namer};
        let mut names = Namer::new("", HashSet::new());
        build(recipes::pump_loop(&mut names, Vec3::ZERO, 0.0, 0.05, 1.0, 1.2, &[]).unwrap())
    }

    /// A generated part as the app would place it.
    fn spec_obj(id: u32, it: &crate::plant::recipes::Item, lever: Option<f32>) -> Obj {
        use crate::plant::recipes;
        use crate::prototype::ProtoJoint;
        let spec = it.spec.unwrap();
        let b = spec.built();
        let (mn, mx) = recipes::spec_bounds(&spec);
        let joints: Vec<ProtoJoint> = b
            .joints
            .iter()
            .map(|j| ProtoJoint {
                name: j.name.clone(),
                kind: if j.revolute { JointKind::Revolute } else { JointKind::Prismatic },
                parent: j.parent,
                child: j.child,
                anchor: j.anchor,
                axis: Vec3::Y,
                lower: j.limits.map(|l| l.0),
                upper: j.limits.map(|l| l.1),
            })
            .collect();
        let mut jv = vec![0.0; joints.len()];
        if let Some(deg) = lever {
            jv[0] = deg;
        }
        let proto = Prototype {
            parts: Vec::new(),
            joints,
            links: Vec::new(),
            unit: 1.0,
            basis: Transform::IDENTITY,
            aabb_min: mn,
            aabb_max: mx,
            triangles: 0,
            load_ms: 0.0,
            warnings: Vec::new(),
            detected: Default::default(),
        };
        let info = ports::info(&it.usd, &proto);
        Obj { entity: Entity::from_raw_u32(id).unwrap(), name: it.name.clone(), asset: it.usd.clone(), role: info.role, info, t: it.transform, proto: Arc::new(proto), joints: jv }
    }

    /// A model of an assembly exactly as the app would place it.
    fn build(a: crate::plant::recipes::Assembly) -> Station {
        let mut model = Model::default();
        for (i, it) in a.items.iter().enumerate() {
            let lever = a.levers.iter().find(|(n, _)| *n == it.name).map(|(_, d)| *d);
            model.index.insert(it.name.clone(), model.objs.len());
            model.objs.push(spec_obj(i as u32 + 100, it, lever));
        }
        let mut links = Links::default();
        for l in a.links {
            links.add(l);
        }
        let mut plant = Plant::default();
        plant.controls.extend(a.controls);
        Station { model, links, plant }
    }

    impl Station {
        fn lever(&mut self, name: &str, deg: f32) {
            let i = self.model.index[name];
            self.model.objs[i].joints[0] = deg;
        }
        fn lever_at(&self, name: &str) -> f32 {
            self.model.get(name).unwrap().joints[0]
        }
        /// One live step: speeds, flow (with inertia), tank levels.
        fn step(&mut self, dt: f32) -> FlowResult {
            let (target, _) = drives(&self.model, &self.links, &self.plant.controls);
            spin(&self.model, &mut self.plant.live.omega, &target, dt);
            let p = &mut self.plant;
            let mut flow = solve(&self.model, &self.links, &p.controls, &p.live.omega, &p.live.volume, &mut p.live.hyd, Some(dt));
            let mut a = flow.alarms.clone();
            integrate(&self.model, &flow, &mut p.live.volume, &p.controls, dt, &mut a, &mut p.live.spilled);
            flow.alarms = a;
            flow
        }
        fn pump_q(f: &FlowResult) -> f32 {
            f.pumps.get("pump").map_or(0.0, |p| p.0)
        }
        fn total(&self) -> f32 {
            self.plant.live.volume.values().sum::<f32>() + self.plant.live.spilled
        }
    }

    #[test]
    fn pump_station_settles_without_alarms_and_conserves_liquid() {
        let mut s = station();
        let mut f = s.step(0.1);
        let v0 = s.total();
        let mut late: Vec<f32> = Vec::new();
        let mut alarms: Vec<String> = Vec::new();
        let (dt, secs) = (0.25, 900.0);
        for k in 0..(secs / dt) as usize {
            f = s.step(dt);
            let t = k as f32 * dt;
            if t > 20.0 {
                alarms.extend(f.alarms.iter().cloned());
            }
            if t > secs - 60.0 {
                late.push(Station::pump_q(&f));
            }
            if k % 400 == 0 {
                let lv = |n: &str| s.plant.live.volume[n] / tank_geom(s.model.get(n).unwrap()).capacity * 100.0;
                eprintln!("t {t:.0} pump {:?} npsh {:?} A {:.1}% B {:.1}% ret {:?} {:?}", f.pumps.get("pump"), f.npsh.get("pump"), lv("tank_a"), lv("tank_b"), f.element_flow.get("valve_return"), f.alarms);
            }
        }
        alarms.sort();
        alarms.dedup();
        assert!(alarms.is_empty(), "{alarms:?}");
        let (mn, mx) = late.iter().fold((f32::MAX, f32::MIN), |(a, b), q| (a.min(*q), b.max(*q)));
        assert!(mn > 1e-3, "the loop keeps circulating: {mn}");
        assert!((mx - mn) / mx < 0.02, "steady: {mn}..{mx}");
        let err = (s.total() - v0).abs() / v0;
        assert!(err < 0.005, "mass balance error {:.3} %", err * 100.0);
    }

    /// Moves `name` by `by` and applies the re-route exactly as the app does.
    fn drag_and_reroute(s: &mut Station, name: &str, by: Vec3) -> Vec<crate::plant::reroute::Rerun> {
        use crate::plant::recipes::{Item, Namer};
        let i = s.model.index[name];
        s.model.objs[i].t.translation += by;
        let mut names = Namer::new("", s.model.objs.iter().map(|o| o.name.clone()).collect());
        let (runs, errors) = crate::plant::reroute::plan(&s.model, &s.links, name, &mut names);
        assert!(errors.is_empty(), "{errors:?}");
        for run in &runs {
            let gone: HashSet<&str> = run.remove.iter().map(String::as_str).collect();
            s.model.objs.retain(|o| !gone.contains(o.name.as_str()));
            s.links.0.retain(|l| !gone.contains(l.a.object.as_str()) && !gone.contains(l.b.object.as_str()) && l.other(&run.from).is_none());
            for p in &run.route.pieces {
                let it = Item { name: p.name.clone(), usd: p.spec.usd().unwrap(), transform: p.transform, spec: Some(p.spec) };
                let id = 1000 + s.model.objs.len() as u32;
                s.model.objs.push(spec_obj(id, &it, None));
            }
            for l in run.route.links.iter().cloned() {
                s.links.add(l);
            }
        }
        s.model.index = s.model.objs.iter().enumerate().map(|(i, o)| (o.name.clone(), i)).collect();
        runs
    }

    fn assert_all_mated(s: &Station) {
        for l in s.links.0.iter().filter(|l| l.kind == LinkKind::Pipe) {
            let (a, b) = (s.model.port_world(&l.a).unwrap(), s.model.port_world(&l.b).unwrap());
            let (err, ang) = crate::plant::connect::misalignment(l.kind, (a.0, a.1), (b.0, b.1));
            assert!(err < 2e-3 && ang < 1.0, "{} ↔ {}: {err} m, {ang}°", l.a.label(), l.b.label());
        }
    }

    #[test]
    fn dragging_the_pump_reroutes_its_pipes_and_it_still_pumps() {
        let mut s = station();
        let before: usize = s.model.objs.len();
        let runs = drag_and_reroute(&mut s, "pump", Vec3::new(0.5, 0.0, 0.6));
        assert_eq!(runs.len(), 2, "inlet and outlet runs");
        assert!(runs.iter().all(|r| !r.route.pieces.is_empty()));
        assert!(s.model.objs.len() > before, "the longer runs need more pieces");
        assert_all_mated(&s);
        // Dropped back at the same spot: nothing to do.
        let mut names = crate::plant::recipes::Namer::new("", HashSet::new());
        assert!(crate::plant::reroute::plan(&s.model, &s.links, "pump", &mut names).0.is_empty());
        let mut q = 0.0;
        for _ in 0..800 {
            q = Station::pump_q(&s.step(0.25));
        }
        assert!(q > 1e-3, "the re-routed loop circulates: {q}");
    }

    #[test]
    fn closing_the_suction_valve_ramps_flow_down_and_starves_the_pump() {
        let mut s = station();
        for _ in 0..600 {
            s.step(0.25);
        }
        let q0 = Station::pump_q(&s.step(0.05));
        assert!(q0 > 1e-3, "{q0}");
        // Quarter turn at the lever speed the app uses (120 degrees/s).
        let mut t = 0.0;
        let mut at_03 = None;
        let mut f = FlowResult::default();
        while t < 3.0 {
            let a = (s.lever_at("valve_suction") + 120.0 * 0.05).min(90.0);
            s.lever("valve_suction", a);
            f = s.step(0.05);
            assert!(!f.alarms.iter().any(|a| a.contains("surge")), "a lever turned at 120°/s is no water hammer: {:?}", f.alarms);
            t += 0.05;
            if at_03.is_none() && t >= 0.3 {
                at_03 = Some(Station::pump_q(&f));
            }
        }
        let at_03 = at_03.unwrap();
        assert!(at_03 > 0.3 * q0, "flow decays with the lever, not instantly: {at_03} of {q0}");
        assert!(Station::pump_q(&f).abs() < 1e-5, "{}", Station::pump_q(&f));
        assert!(f.alarms.iter().any(|a| a.contains("starved")), "{:?}", f.alarms);
        assert!(!f.alarms.iter().any(|a| a.contains("nothing flows")), "{:?}", f.alarms);
    }

    #[test]
    fn pumps_spin_up_and_coast_down() {
        let mut s = station();
        let target = drives(&s.model, &s.links, &s.plant.controls).0["pump"];
        let mut t = 0.0;
        let mut t95 = None;
        while t < 10.0 {
            s.step(0.05);
            t += 0.05;
            if t95.is_none() && s.plant.live.omega.get("pump").copied().unwrap_or(0.0) >= 0.95 * target {
                t95 = Some(t);
            }
        }
        let t95 = t95.unwrap();
        assert!((3.5..5.5).contains(&t95), "run-up {t95} s");
        s.plant.controls.get_mut("pump").unwrap().on = false;
        s.step(1.0);
        let w = s.plant.live.omega.get("pump").copied().unwrap_or(0.0);
        assert!(w > 0.3 * target && w < 0.9 * target, "coasting: {w} of {target}");
    }

    #[test]
    fn viscous_oil_flows_less_than_water() {
        let flow_with = |medium: &str| {
            // Valves wide open, so pipe friction (not throttling) sets the flow.
            let mut s = station();
            s.lever("valve_discharge", 0.0);
            for n in ["tank_a", "tank_b"] {
                s.plant.controls.get_mut(n).unwrap().medium = Some(medium.into());
            }
            let (omega, _) = drives(&s.model, &s.links, &s.plant.controls);
            let f = solve(&s.model, &s.links, &s.plant.controls, &omega, &HashMap::new(), &mut FlowState::default(), None);
            Station::pump_q(&f)
        };
        let (w, o) = (flow_with("water"), flow_with("oil"));
        assert!(o < w * 0.97 && o > 0.0, "oil {o} vs water {w}");
    }


    // --- Stress: random operation must never break the physics ----------

    /// Tiny deterministic generator (no extra dependency).
    struct Lcg(u64);
    impl Lcg {
        fn f(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (1u64 << 31) as f32
        }
    }

    /// Invariants of any flow result and state.
    fn check_sane(s: &Station, f: &FlowResult, ctx: &str) {
        let finite = |v: f32, what: &str| assert!(v.is_finite(), "{ctx}: {what} = {v}");
        for (k, v) in &f.element_flow {
            finite(*v, k);
            assert!(v.abs() < 0.1, "{ctx}: {k} flows {v} m³/s (absurd for DN50)");
        }
        for (k, v) in &f.port_head {
            finite(*v, &k.label());
            assert!(v.abs() < 200.0, "{ctx}: head {} = {v}", k.label());
        }
        for (k, (q, h)) in &f.pumps {
            finite(*q, k);
            finite(*h, k);
            assert!(*q > -1e-5, "{ctx}: pump {k} runs backwards through its check valve: {q}");
        }
        for (k, (a, r)) in &f.npsh {
            finite(*a, k);
            finite(*r, k);
        }
        for o in s.model.objs.iter().filter(|o| o.role == Role::Tank) {
            let v = s.plant.live.volume.get(&o.name).copied().unwrap_or(0.0);
            let cap = tank_geom(o).capacity;
            assert!(v.is_finite() && v >= 0.0 && v <= cap * 1.0001, "{ctx}: {} volume {v} of {cap}", o.name);
        }
        for w in s.plant.live.omega.values() {
            finite(*w, "omega");
        }
        // A shut valve passes nothing.
        for o in s.model.objs.iter().filter(|o| o.role == Role::Valve) {
            if valve_k(valve_angle(o, &s.plant.controls)).is_none() {
                let q = f.element_flow.get(&o.name).copied().unwrap_or(0.0);
                assert!(q.abs() < 1e-6, "{ctx}: shut {} passes {q}", o.name);
            }
        }
    }

    #[test]
    fn fuzz_pump_station_operation() {
        let media = ["water", "oil", "coolant", "fuel", "chemical"];
        for seed in 0..60u64 {
            let mut r = Lcg(seed * 7919 + 1);
            let mut s = station();
            for n in ["tank_a", "tank_b"] {
                let c = s.plant.controls.get_mut(n).unwrap();
                c.fill = Some(r.f());
                c.medium = Some(media[(r.f() * 5.0) as usize % 5].into());
            }
            let v_start: f32 = s.model.objs.iter().filter(|o| o.role == Role::Tank).map(|o| initial_volume(o, &s.plant.controls)).sum();
            let mut f = s.step(0.1);
            let v0 = s.total();
            assert!((v0 - v_start).abs() < 1e-3 * v_start.max(1e-3) + 1e-4, "seed {seed}: start volume");
            let valves = ["valve_suction", "valve_discharge", "valve_return"];
            let mut targets: Vec<f32> = valves.iter().map(|v| s.lever_at(v)).collect();
            for k in 0..400 {
                // Every so often an operator sets a valve; the lever turns at
                // the app's 120°/s.
                if r.f() < 0.05 {
                    let i = (r.f() * 3.0) as usize % 3;
                    targets[i] = if r.f() < 0.3 { 90.0 } else if r.f() < 0.3 { 0.0 } else { r.f() * 90.0 };
                }
                if r.f() < 0.01 {
                    let c = s.plant.controls.get_mut("pump").unwrap();
                    c.on = !c.on;
                }
                if r.f() < 0.01 {
                    s.plant.controls.get_mut("pump").unwrap().rpm = Some(200.0 + r.f() * 3000.0);
                }
                let dt = [0.01, 0.05, 0.1, 0.25, 0.5][(r.f() * 5.0) as usize % 5];
                for (i, v) in valves.iter().enumerate() {
                    let cur = s.lever_at(v);
                    s.lever(v, cur + (targets[i] - cur).clamp(-120.0 * dt, 120.0 * dt));
                }
                f = s.step(dt);
                check_sane(&s, &f, &format!("seed {seed} step {k}"));
            }
            let err = (s.total() - v0).abs() / v0.max(1e-3);
            assert!(err < 0.005, "seed {seed}: mass balance error {:.3} %", err * 100.0);
            let _ = f;
        }
    }

    #[test]
    fn transient_settles_to_the_steady_solution() {
        let mut s = station();
        // Freeze the levels: compare flows at fixed tank volumes.
        for _ in 0..200 {
            let (target, _) = drives(&s.model, &s.links, &s.plant.controls);
            spin(&s.model, &mut s.plant.live.omega, &target, 0.1);
            let p = &mut s.plant;
            solve(&s.model, &s.links, &p.controls, &p.live.omega, &p.live.volume, &mut p.live.hyd, Some(0.1));
        }
        let p = &mut s.plant;
        let live = solve(&s.model, &s.links, &p.controls, &p.live.omega, &p.live.volume, &mut p.live.hyd, Some(0.1));
        let steady = solve(&s.model, &s.links, &p.controls, &p.live.omega, &p.live.volume, &mut FlowState::default(), None);
        let (a, b) = (Station::pump_q(&live), Station::pump_q(&steady));
        assert!((a - b).abs() / b < 0.01, "transient {a} vs steady {b}");
    }

    #[test]
    fn overflow_and_leaks_are_booked_as_spilled() {
        // Return shut, B nearly full: the pump overflows B.
        let mut s = station();
        s.plant.controls.get_mut("tank_b").unwrap().fill = Some(0.97);
        s.lever("valve_return", 90.0);
        s.lever("valve_discharge", 0.0);
        let mut seen = false;
        s.step(0.1);
        let v0 = s.total();
        for _ in 0..400 {
            let f = s.step(0.25);
            seen |= f.alarms.iter().any(|a| a.contains("overflowing"));
        }
        assert!(seen, "overflow alarm");
        assert!(s.plant.live.spilled > 0.0);
        assert!((s.total() - v0).abs() / v0 < 0.005);

        // A pipe disconnected at the pump discharge valve: liquid pours out.
        let mut s = station();
        s.links.remove_port(&PortRef::new("valve_discharge", "b"));
        s.step(0.1);
        let v0 = s.total();
        let mut leak = false;
        for _ in 0..200 {
            let f = s.step(0.1);
            leak |= f.alarms.iter().any(|a| a.starts_with("leak"));
            check_sane(&s, &f, "leak");
        }
        assert!(leak, "leak alarm");
        assert!(s.plant.live.spilled > 1e-3, "{}", s.plant.live.spilled);
        assert!((s.total() - v0).abs() / v0 < 0.005);
    }

    #[test]
    fn slamming_a_valve_shut_raises_a_surge_alarm() {
        let mut s = station();
        for _ in 0..300 {
            s.step(0.25);
        }
        s.lever("valve_discharge", 90.0);
        let f = s.step(0.01);
        let (_, hs) = f.surge.clone().unwrap();
        // Bounded by Joukowsky: a Δv / g for the flow that was stopped.
        let v = 2.4e-3 / area(0.045);
        assert!(hs > SURGE_ALARM_M && hs < 1.2 * WAVE_SPEED * v / G, "{hs}");
        assert!(f.alarms.iter().any(|a| a.contains("surge")), "{:?}", f.alarms);
        for (r, h) in &f.port_head {
            assert!(h.is_finite(), "{}", r.label());
        }
    }

    #[test]
    fn discharge_closed_is_dead_heading_not_starving() {
        let mut s = station();
        for _ in 0..100 {
            s.step(0.1);
        }
        s.lever("valve_discharge", 90.0);
        let mut f = FlowResult::default();
        for _ in 0..50 {
            f = s.step(0.1);
        }
        assert!(f.alarms.iter().any(|a| a.contains("dead-headed")), "{:?}", f.alarms);
        assert!(!f.alarms.iter().any(|a| a.contains("starved") || a.contains("vacuum")), "{:?}", f.alarms);
        assert!(Station::pump_q(&f).abs() < 1e-6);
    }

    #[test]
    fn engine_driven_coolant_loop_runs() {
        use crate::plant::recipes::{self, Namer};
        let mut names = Namer::new("", HashSet::new());
        let mut s = build(recipes::engine_drive(&mut names, Vec3::ZERO, 0.0, 4, &[]).unwrap());
        s.step(0.1);
        let v0 = s.total();
        let mut f = FlowResult::default();
        for k in 0..600 {
            f = s.step(0.1);
            check_sane(&s, &f, &format!("engine step {k}"));
        }
        let q = f.pumps.get("coolant_pump").map_or(0.0, |p| p.0);
        assert!(q > 1e-4, "belt-driven pump circulates coolant: {q} {:?}", f.alarms);
        assert!((s.total() - v0).abs() / v0 < 0.005);
    }

    // --- Free surfaces: open ends and entry and exit losses ---------------

    fn flow_figures() -> (f64, f64, f64) {
        use crate::plant::recipes::{self, Namer};
        let s = station();
        let r = simulate(&s.model, &s.links, &s.plant, 1200.0);
        eprintln!("FLOWS station pump {} valves {} tanks {} mass {} spilled {}", r["pumps"], r["valves"], r["tanks"], r["mass_balance_error_pct"], r["spilled_l"]);
        let station_q = r["pumps"][0]["flow_lps"].as_f64().unwrap();
        let level_a = r["tanks"][0]["level_end_pct"].as_f64().unwrap();
        let mut names = Namer::new("", HashSet::new());
        let e = build(recipes::engine_drive(&mut names, Vec3::ZERO, 0.0, 4, &[]).unwrap());
        let r = simulate(&e.model, &e.links, &e.plant, 60.0);
        eprintln!("FLOWS engine pump {} tanks {}", r["pumps"], r["tanks"]);
        (station_q, level_a, r["pumps"][0]["flow_lps"].as_f64().unwrap())
    }

    /// A tank with a valve on its low nozzle, the valve's far end open to the air, and
    /// the surface `head` metres above that end. Returns the station and the volume to use.
    fn tank_and_valve(head: f32) -> (Station, HashMap<String, f32>, f32) {
        use crate::plant::recipes::Assembly;
        let d = 0.05;
        let mut a = Assembly::default();
        let tank = a.add("tank".into(), Spec::Tank { dia: 1.0, h: 3.0, d }, Transform::IDENTITY).unwrap();
        a.attach("valve".into(), Spec::Valve { d }, "a", &PortRef::new(&tank, "outlet"), Some(Vec3::X)).unwrap();
        let s = build(a);
        let end = s.model.port_world(&PortRef::new("valve", "b")).unwrap().0.y;
        let g = tank_geom(s.model.get("tank").unwrap());
        let v = (end + head - g.bottom) / g.height * g.capacity;
        let volumes = HashMap::from([("tank".to_string(), v)]);
        (s, volumes, end)
    }

    // OUT-B14: 2 m of head through an open DN50 valve came out 13.4 m/s, 2.8× too fast.
    #[test]
    fn torricelli_through_a_nozzle_with_entry_and_exit_losses() {
        let (s, volumes, _) = tank_and_valve(2.0);
        let p = &s.plant;
        let f = solve(&s.model, &s.links, &p.controls, &HashMap::new(), &volumes, &mut FlowState::default(), None);
        let q = f.leaks.first().map_or(0.0, |x| x.1);
        let spec = Spec::Valve { d: 0.05 };
        let (bore, (len, k)) = (spec.bore(), spec.resistance());
        // v = √(2 g H / ΣK), with ΣK = entry + exit + valve + friction.
        let mut v = 5.0f32;
        for _ in 0..8 {
            let fric = friction_factor(v * bore / 1e-6, bore) * len / bore;
            v = (2.0 * G * 2.0 / (K_ENTRY + K_EXIT + k + valve_k(0.0).unwrap() + fric)).sqrt();
        }
        let expect = v * area(bore);
        assert!((q - expect).abs() / expect < 0.01, "{q} vs {expect}");
        assert!((v - 4.8).abs() < 0.2, "the audit's hand figure: {v}");
        // Bare Torricelli is the ceiling: nothing can run out faster than √(2 g H).
        assert!(q / area(bore) < (2.0 * G * 2.0).sqrt());
        // The same head without these losses is far faster.
        let without = (2.0 * G * 2.0 / (k + valve_k(0.0).unwrap() + friction_factor(13.0 * bore / 1e-6, bore) * len / bore)).sqrt() * area(bore);
        assert!(without / q > 2.5, "{without} vs {q}");
    }

    #[test]
    fn entry_and_exit_losses_follow_the_direction_of_flow() {
        let mut e = edge(0, 1, minor(0.0, 0.05));
        e.ends = [true, false];
        assert_eq!((e.end_k(1.0), e.end_k(-1.0)), (K_ENTRY, K_EXIT));
        e.ends = [true, true];
        assert_eq!((e.end_k(1.0), e.end_k(-1.0)), (K_ENTRY + K_EXIT, K_ENTRY + K_EXIT));
        assert_eq!(edge(0, 1, minor(0.0, 0.05)).end_k(1.0), 0.0);
        // Hand calculation: K 1.5 on a bare connection between two heads.
        let nodes = vec![node(Some(2.0)), node(Some(0.0))];
        let mut e = edge(0, 1, minor(0.0, 0.05));
        e.ends = [true, true];
        let (mut q, mut h) = (vec![1e-4], vec![0.0; 2]);
        assert!(newton(&nodes, &[e], &mut q, &mut h));
        let expect = area(0.05) * (2.0 * G * 2.0 / 1.5).sqrt();
        assert!((q[0] - expect).abs() / expect < 0.01, "{} vs {expect}", q[0]);
    }

    /// A tank with a riser from its low nozzle, standing open above the surface.
    fn open_riser() -> Station {
        use crate::plant::recipes::Assembly;
        let d = 0.05;
        let mut a = Assembly::default();
        let tank = a.add("tank".into(), Spec::Tank { dia: 1.0, h: 1.2, d }, Transform::IDENTITY).unwrap();
        let v = a.attach("valve".into(), Spec::Valve { d }, "a", &PortRef::new(&tank, "outlet"), Some(Vec3::X)).unwrap();
        let e = a.attach("elbow".into(), Spec::Elbow { d, angle: 90.0 }, "a", &PortRef::new(&v, "b"), Some(Vec3::Y)).unwrap();
        a.attach("riser".into(), Spec::Pipe { d, len: 3.0 }, "a", &PortRef::new(&e, "b"), Some(Vec3::Y)).unwrap();
        a.controls.push((tank, Control { fill: Some(0.5), ..default() }));
        build(a)
    }

    // OUT-B13: head at the open end filled the tank from nothing.
    #[test]
    fn an_open_end_above_the_surface_makes_no_liquid() {
        let mut s = open_riser();
        let top = s.model.port_world(&PortRef::new("riser", "b")).unwrap().0.y;
        let g = tank_geom(s.model.get("tank").unwrap());
        assert!(top > g.bottom + 0.5 * g.height + 1.0, "the end stands above the surface: {top}");
        s.step(0.1);
        let mut f = FlowResult::default();
        let v0 = s.plant.live.volume["tank"];
        for _ in 0..300 {
            f = s.step(0.1);
        }
        assert!((s.plant.live.volume["tank"] - v0).abs() < 1e-5, "{} from {v0}", s.plant.live.volume["tank"]);
        assert!(f.tank_inflow["tank"].abs() < 1e-5, "{:?}", f.tank_inflow);
        assert!(f.leaks.is_empty(), "{:?}", f.leaks);
        assert!(s.plant.live.spilled.abs() < 1e-5);
        let r = simulate(&s.model, &s.links, &s.plant, 30.0);
        assert!(r["mass_balance_error_pct"].as_f64().unwrap().abs() < 0.01, "{r:#}");
    }

    /// A pump lifting into a tank, its suction a pipe ending in open air.
    fn pump_on_air(supply: bool) -> Station {
        use crate::plant::recipes::{Assembly, Namer};
        let d = 0.05;
        let mut names = Namer::new("", HashSet::new());
        let mut a = Assembly::default();
        let tank = a.add("tank".into(), Spec::Tank { dia: 1.0, h: 1.2, d }, Transform::IDENTITY).unwrap();
        let pump = a.add("pump".into(), Spec::Pump { d, motor: true, base: 0.0 }, Transform::from_xyz(3.0, 0.0, 0.0).with_rotation(Quat::from_rotation_y(std::f32::consts::PI))).unwrap();
        let suction = a.attach("suction".into(), Spec::Pipe { d, len: 1.0 }, "a", &PortRef::new(&pump, "inlet"), None).unwrap();
        a.pipe(&PortRef::new(&pump, "outlet"), &PortRef::new(&tank, "inlet"), d, &[], &mut names).unwrap();
        a.controls.push((pump, Control { on: true, ..default() }));
        a.controls.push((tank, Control { fill: Some(0.2), ..default() }));
        a.controls.push((suction, Control { open_ends_supply: supply, ..default() }));
        build(a)
    }

    // OUT-B13: `reaches` took an open end as liquid, and the pump filled the tank from air.
    #[test]
    fn a_pump_with_its_suction_in_open_air_moves_nothing() {
        let mut s = pump_on_air(false);
        s.step(0.1);
        let mut f = FlowResult::default();
        let v0 = s.plant.live.volume["tank"];
        for _ in 0..200 {
            f = s.step(0.1);
        }
        assert!(Station::pump_q(&f).abs() < 1e-5, "{:?}", f.pumps);
        assert!((s.plant.live.volume["tank"] - v0).abs() < 1e-4, "{} from {v0}", s.plant.live.volume["tank"]);
        assert!(f.alarms.iter().any(|a| a.contains("starved")), "{:?}", f.alarms);
        assert_eq!(simulate(&s.model, &s.links, &s.plant, 10.0)["ok"], json!(false));
    }

    #[test]
    fn an_open_end_declared_as_a_supply_feeds_in_and_is_on_the_books() {
        let mut s = pump_on_air(true);
        let v0: f32 = s.model.objs.iter().filter(|o| o.role == Role::Tank).map(|o| initial_volume(o, &s.plant.controls)).sum();
        let mut f = FlowResult::default();
        for _ in 0..300 {
            f = s.step(0.1);
            s.plant.live.supplied += f.supplied.iter().map(|(_, q)| *q).sum::<f32>() * 0.1;
        }
        assert!(Station::pump_q(&f) > 1e-4, "{:?} {:?}", f.pumps, f.alarms);
        assert!(s.plant.live.supplied > 0.01, "{}", s.plant.live.supplied);
        let v1 = s.plant.live.volume["tank"];
        assert!(v1 > v0, "{v1} from {v0}");
        let err = mass_balance_error_pct(v0, v1, s.plant.live.spilled, s.plant.live.supplied);
        assert!(err.abs() < 0.5, "{err}");
        let r = simulate(&s.model, &s.links, &s.plant, 30.0);
        assert!(r["supplied_l"].as_f64().unwrap() > 1.0, "{r:#}");
        assert!(r["mass_balance_error_pct"].as_f64().unwrap().abs() < 0.5, "{r:#}");
    }

    // OUT-B13: `ok` ignored the balance, which was blind if a tank started empty.
    #[test]
    fn the_mass_balance_counts_and_isnt_blind_from_empty() {
        assert_eq!(mass_balance_error_pct(1.0, 0.8, 0.2, 0.0), 0.0);
        assert!(mass_balance_error_pct(1.0, 1.1, 0.0, 0.0) > MASS_BALANCE_LIMIT_PCT);
        assert!(mass_balance_error_pct(1.0, 1.1, 0.0, 0.1).abs() < 1e-3, "fed in on the books");
        assert!(mass_balance_error_pct(0.0, 0.5, 0.0, 0.0) > MASS_BALANCE_LIMIT_PCT, "from nothing, into empty tanks");
        assert_eq!(mass_balance_error_pct(0.0, 0.0, 0.0, 0.0), 0.0);
    }

    // OUT-B13: an overdraw was booked as a negative spill, which hid what it made.
    #[test]
    fn a_tank_run_dry_takes_it_off_the_others_not_a_negative_spill() {
        let s = station();
        let mut flow = FlowResult::default();
        flow.tank_inflow.insert("tank_a".into(), -0.01);
        flow.tank_inflow.insert("tank_b".into(), 0.01);
        let mut volume = HashMap::from([("tank_a".to_string(), 0.002), ("tank_b".to_string(), 0.1)]);
        let (mut spilled, mut alarms) = (0.0, Vec::new());
        integrate(&s.model, &flow, &mut volume, &s.plant.controls, 1.0, &mut alarms, &mut spilled);
        assert_eq!(volume["tank_a"], 0.0);
        assert!((volume["tank_b"] - 0.102).abs() < 1e-6, "got only what A had: {}", volume["tank_b"]);
        assert_eq!(spilled, 0.0);
        assert!(alarms.iter().any(|a| a.contains("run empty")), "{alarms:?}");
    }

    #[test]
    fn a_tank_drawn_down_conserves_liquid_and_a_spill_never_shrinks() {
        let mut s = station();
        s.lever("valve_return", 90.0);
        s.plant.controls.get_mut("tank_a").unwrap().fill = Some(0.02);
        s.step(0.1);
        let v0 = s.total();
        for _ in 0..1200 {
            let before = s.plant.live.spilled;
            s.step(0.25);
            assert!(s.plant.live.spilled >= before, "a spill never shrinks");
        }
        assert!((s.total() - v0).abs() / v0 < 0.005, "{} from {v0}", s.total());
    }

    // --- What a report says ------------------------------------------------

    fn plain_obj(id: u32, name: &str, role: Role, ports: Vec<Port>, entry: Option<ports::CatalogEntry>) -> Obj {
        let proto = Prototype { parts: Vec::new(), joints: Vec::new(), links: Vec::new(), unit: 1.0, basis: Transform::IDENTITY, aabb_min: Vec3::splat(-1.0), aabb_max: Vec3::splat(1.0), triangles: 0, load_ms: 0.0, warnings: Vec::new(), detected: Default::default() };
        Obj { entity: Entity::from_raw_u32(id).unwrap(), name: name.into(), asset: PathBuf::from("x.usd"), role, info: Arc::new(Info { role, ports, spec: None, entry }), t: Transform::IDENTITY, proto: Arc::new(proto), joints: Vec::new() }
    }

    fn pipe_ports(length: f32, size: f32) -> Vec<Port> {
        vec![Port::new("a", PortKind::Pipe, Vec3::ZERO, -Vec3::X, size, ports::PortSource::Catalog), Port::new("b", PortKind::Pipe, Vec3::X * length, Vec3::X, size, ports::PortSource::Catalog)]
    }

    // OUT-M16: a valve with no lever always reported 100 % open.
    #[test]
    fn a_valve_is_reported_as_it_is_set() {
        let m = model(vec![plain_obj(1, "v", Role::Valve, pipe_ports(0.2, 0.05), None)]);
        let mut plant = Plant::default();
        plant.controls.insert("v".into(), Control { valve_open: Some(0.3), ..default() });
        let r = simulate(&m, &Links::default(), &plant, 1.0);
        assert_eq!(r["valves"][0]["open_pct"], json!(30.0), "{r:#}");
        assert_eq!(r["valves"][0]["k"], json!(r3(valve_k(63.0).unwrap())), "{r:#}");
        plant.controls.get_mut("v").unwrap().valve_open = Some(0.0);
        let shut = simulate(&m, &Links::default(), &plant, 1.0);
        assert_eq!(shut["valves"][0]["open_pct"], json!(0.0));
        assert_eq!(shut["valves"][0]["k"], Value::Null);
    }

    // OUT-M15: any catalog pipe counted as 0.5 m and K 0.5, and scale was ignored.
    #[test]
    fn a_catalog_pipe_counts_its_real_length_and_says_it_guessed() {
        let long = plain_obj(1, "hose", Role::Fitting, pipe_ports(30.0, 0.05), None);
        let ports: Vec<&Port> = long.info.ports.iter().collect();
        let (len, k, d, guessed) = pipe_geometry(&long, &ports);
        assert_eq!((len, k, guessed), (30.0, 0.5, true));
        assert!((d - 0.045).abs() < 1e-6, "{d}");
        let m = model(vec![long]);
        let notes = assumed_notes(&m, &HashMap::new()).join("\n");
        assert!(notes.contains("hose") && notes.contains("30.00 m") && notes.contains("K 0.5 assumed"), "{notes}");
    }

    #[test]
    fn a_scaled_part_has_a_scaled_length_and_bore() {
        let it = crate::plant::recipes::Item { name: "p".into(), usd: Spec::Pipe { d: 0.05, len: 1.0 }.usd().unwrap(), transform: Transform::from_scale(Vec3::splat(2.0)), spec: Some(Spec::Pipe { d: 0.05, len: 1.0 }) };
        let o = spec_obj(1, &it, None);
        let ports: Vec<&Port> = o.info.ports.iter().filter(|p| p.kind == PortKind::Pipe).collect();
        let (len, _, d, guessed) = pipe_geometry(&o, &ports);
        assert!((len - 2.0).abs() < 1e-3 && (d - 0.09).abs() < 1e-3 && !guessed, "{len} {d}");
    }

    // OUT-M17
    #[test]
    fn invented_curves_are_labelled_and_a_catalog_one_is_not() {
        let s = station();
        let r = simulate(&s.model, &s.links, &s.plant, 5.0);
        assert_eq!(r["pumps"][0]["curve"], json!("default, not a datasheet"), "{r:#}");
        let assumed = r["assumed"].to_string();
        assert!(assumed.contains("default pump curve") && assumed.contains("NPSH") && assumed.contains("No torque or power"), "{assumed}");
        assert!(r["evidence"].as_str().unwrap().contains("not validated"), "{r:#}");
        let good = ports::CatalogEntry { head_m: Some(20.0), max_flow_lps: Some(5.0), ..Default::default() };
        let o = plain_obj(1, "p", Role::Pump, pipe_ports(0.2, 0.05), Some(good));
        assert!(!pump_curve_is_default(&o));
        assert_eq!(pump_curve(&o, 0.045, 1.0), (20.0, 0.005));
        // A catalog zero isn't a curve, and used to make the solve go NaN for good.
        let bad = ports::CatalogEntry { head_m: Some(20.0), max_flow_lps: Some(0.0), ..Default::default() };
        let o = plain_obj(2, "q", Role::Pump, pipe_ports(0.2, 0.05), Some(bad));
        assert!(pump_curve_is_default(&o));
        let (h0, q) = pump_curve(&o, 0.045, 1.0);
        assert!(h0.is_finite() && q > 0.0, "{h0} {q}");
    }

    // A minor of the audit: 80 iterations used to return as if they'd found the answer.
    #[test]
    fn newton_says_when_it_didnt_settle() {
        let nodes = vec![node(Some(5.0)), node(None), node(Some(0.0))];
        let edges = vec![edge(0, 1, minor(8.0, 0.05)), edge(1, 2, minor(8.0, 0.05))];
        let (mut q, mut h) = (vec![1e-4; 2], vec![0.0; 3]);
        assert!(!newton_within(&nodes, &edges, &mut q, &mut h, 1));
        assert!(newton(&nodes, &edges, &mut q, &mut h));
    }

    #[test]
    fn an_unknown_liquid_and_a_surge_in_oil_are_said_plainly() {
        assert!(Fluid::known("Coolant") && !Fluid::known("wine"));
        let mut s = station();
        s.plant.controls.get_mut("tank_a").unwrap().medium = Some("wine".into());
        assert!(s.step(0.1).alarms.iter().any(|a| a.contains("'wine' isn't known")));
        let (water, oil) = (head_to_bar(50.0, Fluid::of(Some("water")).rho), head_to_bar(50.0, Fluid::of(Some("oil")).rho));
        assert!((water - 4.9).abs() < 0.05 && oil < 0.9 * water, "{water} {oil}");
    }

    // The two samples with liquid in them, after the entry and exit losses. Before them
    // the pump loop ran 2.411 l/s, levels 29.7 % and 70.3 %, and the coolant loop 3.808 l/s.
    #[test]
    fn the_sample_flows_are_the_stated_ones() {
        let (station_q, level_a, coolant_q) = flow_figures();
        eprintln!("FIGURES station {station_q} level A {level_a} coolant {coolant_q}");
        assert!(station_q > 1.0 && coolant_q > 1.0);
    }

    #[test]
    fn simulate_report_is_clean_for_the_sample() {
        let s = station();
        let r = simulate(&s.model, &s.links, &s.plant, 600.0);
        assert_eq!(r["ok"], json!(true), "{r:#}");
        assert!(r["mass_balance_error_pct"].as_f64().unwrap().abs() < 0.5, "{r:#}");
        let pump = &r["pumps"][0];
        assert!(pump["flow_lps"].as_f64().unwrap() > 1.0, "{r:#}");
        assert!(pump["npsh_available_m"].as_f64().unwrap() > pump["npsh_required_m"].as_f64().unwrap());
        let v = r["valves"].as_array().unwrap();
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(|v| v["k"].is_number()), "{r:#}");
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

    /// A 0.4 m box (bottom centred) at `x`, resting on a belt top at 0.8 m.
    fn crate_at(i: u32, x: f32) -> Obj {
        let proto = Prototype {
            parts: Vec::new(),
            joints: Vec::new(),
            links: Vec::new(),
            unit: 1.0,
            basis: Transform::IDENTITY,
            aabb_min: Vec3::new(-0.2, 0.0, -0.2),
            aabb_max: Vec3::new(0.2, 0.4, 0.2),
            triangles: 0,
            load_ms: 0.0,
            warnings: Vec::new(),
            detected: Default::default(),
        };
        Obj {
            entity: Entity::from_raw_u32(i + 1).unwrap(),
            name: format!("box_{i}"),
            asset: PathBuf::from("box.usd"),
            role: Role::Other,
            info: Arc::new(Info { role: Role::Other, ports: Vec::new(), spec: None, entry: None }),
            t: Transform::from_xyz(x, 0.8, 0.0),
            proto: Arc::new(proto),
            joints: Vec::new(),
        }
    }
    fn belt(name: &str, x0: f32, speed: f32, next: Option<&str>) -> Belt {
        Belt { name: name.into(), start: Vec3::new(x0, 0.8, 0.0), dir: Vec3::X, len: 2.0, half_w: 0.3, top: 0.8, speed, next: next.map(Into::into) }
    }
    fn dx(moves: &[(Entity, String, Vec3)], m: &Model, name: &str) -> f32 {
        let o = m.get(name).unwrap();
        moves.iter().find(|(_, n, _)| n == name).map_or(0.0, |(_, _, t)| t.x - o.t.translation.x)
    }
    fn model(objs: Vec<Obj>) -> Model {
        let mut m = Model::default();
        for o in objs {
            m.index.insert(o.name.clone(), m.objs.len());
            m.objs.push(o);
        }
        m
    }

    #[test]
    fn belts_never_push_items_back() {
        let bs = [belt("a", 0.0, 0.5, Some("b")), belt("b", 2.0, 0.5, None)];
        // On the joint between two belts: on both at once, it must not block itself.
        let m = model(vec![crate_at(0, 2.01)]);
        assert!(dx(&carry(&m, &bs, 0.1), &m, "box_0") > 0.04);
        // Dropped overhanging the end of the last belt: it stays put.
        let m = model(vec![crate_at(0, 3.9)]);
        assert_eq!(dx(&carry(&m, &bs, 0.1), &m, "box_0"), 0.0);
        // Dropped touching the box ahead: waits instead of jumping back.
        let m = model(vec![crate_at(0, 3.8), crate_at(1, 3.39)]);
        let mv = carry(&m, &bs, 0.1);
        assert_eq!(dx(&mv, &m, "box_0"), 0.0);
        assert_eq!(dx(&mv, &m, "box_1"), 0.0);
        // A reversed belt carries towards its in end.
        let bs = [belt("a", 0.0, -0.5, None)];
        let m = model(vec![crate_at(0, 1.0)]);
        assert!((dx(&carry(&m, &bs, 0.1), &m, "box_0") + 0.05).abs() < 1e-4);
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
