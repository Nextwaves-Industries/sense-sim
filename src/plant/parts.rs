//! Generated plant parts written as small USD files, so they go through the
//! normal asset path (selection, layouts, USDA export, verify): pipes,
//! elbows, tees, reducers, valves, pumps, tanks, motors, pulleys and an
//! inline engine. Parameters are encoded in the path
//! (`%LOCALAPPDATA%/SenseSim/plant/<kind>/<params>/<kind>.usda`), so the
//! exact ports of any placed part are known from its asset path alone.
//!
//! Moving parts are real USD physics joints (rotors, valve levers, the
//! engine's crank, pistons and rods), animated by `plant::sim`.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use bevy::math::{Quat, Vec3};

use super::ports::{Port, PortKind, PortSource, Role};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spec {
    /// Straight pipe along X; `d` outer diameter, `len` flange to flange.
    Pipe { d: f32, len: f32 },
    /// Bend of `angle` degrees with radius 1.5 d.
    Elbow { d: f32, angle: f32 },
    /// Equal tee: run along X, branch up.
    Tee { d: f32 },
    Reducer { d1: f32, d2: f32 },
    /// Ball valve with a lever (0° open, 90° closed).
    Valve { d: f32 },
    /// Centrifugal pump; with its own motor, or a bare shaft to be driven.
    /// `base`: height of the concrete plinth it stands on (0 = none).
    Pump { d: f32, motor: bool, base: f32 },
    /// Vertical tank on legs: shell diameter, shell height, nozzle diameter.
    Tank { dia: f32, h: f32, d: f32 },
    /// Electric motor; `d` is the shaft diameter.
    Motor { d: f32 },
    /// Belt pulley that mounts on a shaft.
    Pulley { dia: f32, d: f32 },
    /// Inline engine with `n` cylinders (crank, pistons and rods move).
    Engine { n: u32 },
    /// Roller conveyor along X: length, belt width, belt height.
    Conveyor { len: f32, w: f32, h: f32 },
}

/// Second line of every generated USD file: bump it when part geometry
/// changes so cached files are rebuilt.
const PART_REV: &str = "# sensesim-part rev 2";

/// Roller radius of generated conveyors.
pub const ROLLER_R: f32 = 0.03;

/// Engine dimensions (m): crank radius, rod length, cylinder pitch, crank height.
pub const ENGINE_R: f32 = 0.04;
pub const ENGINE_L: f32 = 0.14;
const ENGINE_PITCH: f32 = 0.11;
const ENGINE_HC: f32 = 0.2;

impl Spec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pipe { .. } => "pipe",
            Self::Elbow { .. } => "elbow",
            Self::Tee { .. } => "tee",
            Self::Reducer { .. } => "reducer",
            Self::Valve { .. } => "valve",
            Self::Pump { .. } => "pump",
            Self::Tank { .. } => "tank",
            Self::Motor { .. } => "motor",
            Self::Pulley { .. } => "pulley",
            Self::Engine { .. } => "engine",
            Self::Conveyor { .. } => "conveyor",
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Self::Pipe { .. } | Self::Elbow { .. } | Self::Tee { .. } | Self::Reducer { .. } => Role::Fitting,
            Self::Valve { .. } => Role::Valve,
            Self::Pump { .. } => Role::Pump,
            Self::Tank { .. } => Role::Tank,
            Self::Motor { .. } => Role::Motor,
            Self::Pulley { .. } => Role::Rotor,
            Self::Engine { .. } => Role::Engine,
            Self::Conveyor { .. } => Role::Conveyor,
        }
    }

    /// Rounded to whole millimeters so equal parts share one file.
    pub fn normalized(self) -> Self {
        let mm = |v: f32| (v.max(0.005) * 1000.0).round() / 1000.0;
        match self {
            Self::Pipe { d, len } => Self::Pipe { d: mm(d), len: mm(len.max(0.01)) },
            Self::Elbow { d, angle } => Self::Elbow { d: mm(d), angle: angle.round().clamp(5.0, 180.0) },
            Self::Tee { d } => Self::Tee { d: mm(d) },
            Self::Reducer { d1, d2 } => Self::Reducer { d1: mm(d1), d2: mm(d2) },
            Self::Valve { d } => Self::Valve { d: mm(d) },
            Self::Pump { d, motor, base } => Self::Pump { d: mm(d), motor, base: if base > 0.0 { mm(base) } else { 0.0 } },
            Self::Tank { dia, h, d } => Self::Tank { dia: mm(dia.max(0.2)), h: mm(h.max(0.2)), d: mm(d) },
            Self::Motor { d } => Self::Motor { d: mm(d) },
            Self::Pulley { dia, d } => Self::Pulley { dia: mm(dia.max(d * 1.5)), d: mm(d) },
            Self::Engine { n } => Self::Engine { n: n.clamp(1, 8) },
            Self::Conveyor { len, w, h } => Self::Conveyor { len: mm(len.max(0.3)), w: mm(w.max(0.2)), h: mm(h.max(0.2)) },
        }
    }

    fn params(&self) -> String {
        match self {
            Self::Pipe { d, len } => format!("d{d:.3}_l{len:.3}"),
            Self::Elbow { d, angle } => format!("d{d:.3}_a{angle:.0}"),
            Self::Tee { d } | Self::Valve { d } | Self::Motor { d } => format!("d{d:.3}"),
            Self::Reducer { d1, d2 } => format!("d{d1:.3}_e{d2:.3}"),
            Self::Pump { d, motor, base } if *base > 0.0 => format!("d{d:.3}_m{}_b{base:.3}", *motor as u8),
            Self::Pump { d, motor, .. } => format!("d{d:.3}_m{}", *motor as u8),
            Self::Tank { dia, h, d } => format!("w{dia:.3}_h{h:.3}_d{d:.3}"),
            Self::Pulley { dia, d } => format!("w{dia:.3}_d{d:.3}"),
            Self::Engine { n } => format!("n{n}"),
            Self::Conveyor { len, w, h } => format!("l{len:.3}_w{w:.3}_h{h:.3}"),
        }
    }

    /// Parses `kind` + `params` (the two directories under `plant/`).
    pub fn parse(kind: &str, params: &str) -> Option<Self> {
        let mut v: HashMap<char, f32> = HashMap::new();
        for tok in params.split('_') {
            let mut c = tok.chars();
            let k = c.next()?;
            v.insert(k, c.as_str().parse().ok()?);
        }
        let g = |k: char| v.get(&k).copied();
        Some(match kind {
            "pipe" => Self::Pipe { d: g('d')?, len: g('l')? },
            "elbow" => Self::Elbow { d: g('d')?, angle: g('a')? },
            "tee" => Self::Tee { d: g('d')? },
            "reducer" => Self::Reducer { d1: g('d')?, d2: g('e')? },
            "valve" => Self::Valve { d: g('d')? },
            "pump" => Self::Pump { d: g('d')?, motor: g('m')? > 0.5, base: g('b').unwrap_or(0.0) },
            "tank" => Self::Tank { dia: g('w')?, h: g('h')?, d: g('d')? },
            "motor" => Self::Motor { d: g('d')? },
            "pulley" => Self::Pulley { dia: g('w')?, d: g('d')? },
            "engine" => Self::Engine { n: g('n')? as u32 },
            "conveyor" => Self::Conveyor { len: g('l')?, w: g('w')?, h: g('h')? },
            _ => return None,
        })
    }

    /// The spec of a generated part from its asset path.
    pub fn of_asset(asset: &Path) -> Option<Self> {
        let s = asset.to_string_lossy().replace('\\', "/");
        let i = s.find("/plant/")?;
        let mut it = s[i + "/plant/".len()..].split('/');
        let (kind, params) = (it.next()?, it.next()?);
        Self::parse(kind, params)
    }

    /// Writes the USD once and returns its path.
    pub fn usd(self) -> Result<PathBuf> {
        let spec = self.normalized();
        let base = crate::app_paths::data_base();
        let dir = base.join("SenseSim").join("plant").join(spec.kind()).join(spec.params());
        let path = dir.join(format!("{}.usda", spec.kind()));
        // Rewritten when the geometry code changed since the file was made.
        let current = std::fs::read_to_string(&path).is_ok_and(|t| t.lines().nth(1).is_some_and(|l| l.trim() == PART_REV));
        if !current {
            std::fs::create_dir_all(&dir)?;
            let tmp = dir.join(format!("{}.tmp", spec.kind()));
            std::fs::write(&tmp, usda(spec.kind(), &spec.build()))?;
            std::fs::rename(&tmp, &path)?;
        }
        Ok(path)
    }

    pub fn ports(&self) -> Vec<Port> {
        self.built().ports.clone()
    }

    /// Flow paths from the part's center to each pipe port (for the flow
    /// arrows), in the local frame.
    pub fn paths(&self) -> Vec<(String, Vec<Vec3>)> {
        self.built().paths.clone()
    }

    pub fn built(&self) -> Arc<Built> {
        static CACHE: OnceLock<Mutex<HashMap<String, Arc<Built>>>> = OnceLock::new();
        let spec = self.normalized();
        let key = format!("{}/{}", spec.kind(), spec.params());
        let cache = CACHE.get_or_init(Default::default);
        if let Some(b) = cache.lock().unwrap().get(&key) {
            return b.clone();
        }
        let b = Arc::new(spec.build());
        cache.lock().unwrap().insert(key, b.clone());
        b
    }

    /// Inner diameter of the bore, for flow velocity.
    pub fn bore(&self) -> f32 {
        match *self {
            Self::Pipe { d, .. } | Self::Elbow { d, .. } | Self::Tee { d } | Self::Valve { d } | Self::Pump { d, .. } | Self::Tank { d, .. } => d * 0.9,
            Self::Reducer { d1, d2 } => d1.min(d2) * 0.9,
            _ => 0.0,
        }
    }

    /// Equivalent pipe length (m) and fitting loss coefficient K of a two-port part.
    pub fn resistance(&self) -> (f32, f32) {
        match *self {
            Self::Pipe { len, .. } => (len, 0.0),
            Self::Elbow { d, angle } => (1.5 * d * angle.to_radians(), 0.3 * angle / 90.0),
            Self::Tee { d } => (1.5 * d, 0.4),
            Self::Reducer { .. } => (0.1, 0.2),
            Self::Valve { d } => (3.0 * d, 0.1),
            _ => (0.0, 0.0),
        }
    }

    fn build(&self) -> Built {
        let mut b = Builder::default();
        match *self {
            Self::Pipe { d, len } => build_pipe(&mut b, d, len),
            Self::Elbow { d, angle } => build_elbow(&mut b, d, angle),
            Self::Tee { d } => build_tee(&mut b, d),
            Self::Reducer { d1, d2 } => build_reducer(&mut b, d1, d2),
            Self::Valve { d } => build_valve(&mut b, d),
            Self::Pump { d, motor, base } => build_pump(&mut b, d, motor, base),
            Self::Tank { dia, h, d } => build_tank(&mut b, dia, h, d),
            Self::Motor { d } => build_motor(&mut b, d),
            Self::Pulley { dia, d } => build_pulley(&mut b, dia, d),
            Self::Engine { n } => build_engine(&mut b, n),
            Self::Conveyor { len, w, h } => build_conveyor(&mut b, len, w, h),
        }
        b.finish()
    }
}

// ---------------------------------------------------------------------------
// Geometry

type Rgb = [f32; 3];
const STEEL: Rgb = [0.75, 0.77, 0.8];
const PIPE: Rgb = [0.55, 0.6, 0.66];
const DARK: Rgb = [0.12, 0.13, 0.14];
const BLUE: Rgb = [0.08, 0.26, 0.7];
const GREEN: Rgb = [0.1, 0.45, 0.25];
const RED: Rgb = [0.85, 0.1, 0.06];
const YELLOW: Rgb = [1.0, 0.72, 0.05];
const WHITE: Rgb = [0.92, 0.92, 0.9];
const GREY: Rgb = [0.4, 0.42, 0.45];

#[derive(Default, Clone)]
pub struct MeshBuf {
    pub pos: Vec<Vec3>,
    pub nrm: Vec<Vec3>,
    pub col: Vec<Rgb>,
    pub idx: Vec<u32>,
}

impl MeshBuf {
    fn vert(&mut self, p: Vec3, n: Vec3, c: Rgb) -> u32 {
        self.pos.push(p);
        self.nrm.push(n);
        self.col.push(c);
        self.pos.len() as u32 - 1
    }
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        self.idx.extend([a, b, c]);
    }
}

pub struct JointDef {
    pub name: String,
    pub revolute: bool,
    pub parent: Option<usize>,
    pub child: usize,
    pub anchor: Vec3,
    pub axis: char,
    pub limits: Option<(f32, f32)>,
}

pub struct Built {
    /// Body 0 is static; the others move with joints.
    pub bodies: Vec<(String, MeshBuf)>,
    pub joints: Vec<JointDef>,
    pub ports: Vec<Port>,
    pub paths: Vec<(String, Vec<Vec3>)>,
}

#[derive(Default)]
struct Builder {
    bodies: Vec<(String, MeshBuf)>,
    cur: usize,
    joints: Vec<JointDef>,
    ports: Vec<Port>,
    paths: Vec<(String, Vec<Vec3>)>,
}

/// Two unit vectors perpendicular to `axis`.
fn basis(axis: Vec3) -> (Vec3, Vec3) {
    let a = axis.normalize();
    let helper = if a.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let u = a.cross(helper).normalize();
    (u, a.cross(u))
}

impl Builder {
    fn body(&mut self, name: &str) -> usize {
        if self.bodies.is_empty() {
            self.bodies.push(("static".into(), MeshBuf::default()));
        }
        if let Some(i) = self.bodies.iter().position(|(n, _)| n == name) {
            self.cur = i;
            return i;
        }
        self.bodies.push((name.into(), MeshBuf::default()));
        self.cur = self.bodies.len() - 1;
        self.cur
    }
    fn m(&mut self) -> &mut MeshBuf {
        if self.bodies.is_empty() {
            self.bodies.push(("static".into(), MeshBuf::default()));
        }
        &mut self.bodies[self.cur].1
    }

    /// Frustum from `a` (radius ra) to `b` (radius rb), optionally capped.
    fn frustum(&mut self, a: Vec3, b: Vec3, ra: f32, rb: f32, caps: bool, c: Rgb) {
        let axis = (b - a).normalize_or(Vec3::X);
        let (u, v) = basis(axis);
        let seg = 24;
        let slope = (ra - rb) / (b - a).length().max(1e-6);
        let m = self.m();
        let base = m.pos.len() as u32;
        for i in 0..=seg {
            let t = i as f32 / seg as f32 * TAU;
            let radial = u * t.cos() + v * t.sin();
            let n = (radial + axis * slope).normalize();
            m.vert(a + radial * ra, n, c);
            m.vert(b + radial * rb, n, c);
        }
        for i in 0..seg as u32 {
            let (p0, p1, q0, q1) = (base + 2 * i, base + 2 * i + 1, base + 2 * i + 2, base + 2 * i + 3);
            m.tri(p0, q0, p1);
            m.tri(p1, q0, q1);
        }
        if caps {
            for (center, r, n) in [(a, ra, -axis), (b, rb, axis)] {
                let c0 = m.vert(center, n, c);
                let start = m.pos.len() as u32;
                for i in 0..=seg {
                    let t = i as f32 / seg as f32 * TAU;
                    m.vert(center + (u * t.cos() + v * t.sin()) * r, n, c);
                }
                for i in 0..seg as u32 {
                    if n == axis {
                        m.tri(c0, start + i, start + i + 1);
                    } else {
                        m.tri(c0, start + i + 1, start + i);
                    }
                }
            }
        }
    }
    fn cyl(&mut self, a: Vec3, b: Vec3, r: f32, c: Rgb) {
        self.frustum(a, b, r, r, true, c);
    }
    /// Open tube (no caps), for sleeves you can see into.
    fn sleeve(&mut self, a: Vec3, b: Vec3, r: f32, c: Rgb) {
        self.frustum(a, b, r, r, false, c);
    }
    fn cuboid(&mut self, center: Vec3, half: Vec3, rot: Quat, c: Rgb) {
        let m = self.m();
        let faces = [
            (Vec3::X, Vec3::Y, Vec3::Z),
            (-Vec3::X, Vec3::Z, Vec3::Y),
            (Vec3::Y, Vec3::Z, Vec3::X),
            (-Vec3::Y, Vec3::X, Vec3::Z),
            (Vec3::Z, Vec3::X, Vec3::Y),
            (-Vec3::Z, Vec3::Y, Vec3::X),
        ];
        for (n, u, v) in faces {
            let base = m.pos.len() as u32;
            let nn = rot * n;
            for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let p = n * half + u * half * su + v * half * sv;
                m.vert(center + rot * p, nn, c);
            }
            m.tri(base, base + 1, base + 2);
            m.tri(base, base + 2, base + 3);
        }
    }
    /// Flange disc from `at` to `at + dir·t`. Callers pass the port face minus
    /// one thickness, so the outer face is the port face and two mated
    /// flanges meet face to face.
    fn flange(&mut self, at: Vec3, dir: Vec3, d: f32) {
        let t = flange_t(d);
        self.cyl(at, at + dir * t, d * 0.8, STEEL);
    }
    /// Swept tube along an arc: center `c`, from radial `u` towards `v`.
    fn torus_arc(&mut self, c: Vec3, u: Vec3, v: Vec3, big_r: f32, r: f32, angle: f32, col: Rgb) {
        let segs = ((angle / 10.0).ceil() as u32).max(4);
        let ring = 24u32;
        let m = self.m();
        let base = m.pos.len() as u32;
        let axis = u.cross(v).normalize();
        for i in 0..=segs {
            let a = angle.to_radians() * i as f32 / segs as f32;
            let radial = u * a.cos() + v * a.sin();
            let center = c + radial * big_r;
            for j in 0..=ring {
                let b = j as f32 / ring as f32 * TAU;
                let n = radial * b.cos() + axis * b.sin();
                m.vert(center + n * r, n, col);
            }
        }
        let w = ring + 1;
        for i in 0..segs {
            for j in 0..ring {
                let (a0, a1, b0, b1) = (base + i * w + j, base + i * w + j + 1, base + (i + 1) * w + j, base + (i + 1) * w + j + 1);
                m.tri(a0, a1, b0);
                m.tri(a1, b1, b0);
            }
        }
    }
    fn port(&mut self, id: &str, kind: PortKind, pos: Vec3, dir: Vec3, size: f32) {
        self.ports.push(Port::new(id, kind, pos, dir, size, PortSource::Generated));
    }
    fn path(&mut self, id: &str, pts: Vec<Vec3>) {
        self.paths.push((id.into(), pts));
    }
    fn joint(&mut self, name: &str, revolute: bool, parent: Option<usize>, child: usize, anchor: Vec3, axis: char, limits: Option<(f32, f32)>) {
        self.joints.push(JointDef { name: name.into(), revolute, parent, child, anchor, axis, limits });
    }

    /// Moves everything so the part stands bottom-centered on its origin,
    /// exactly as the loader will place it.
    fn finish(mut self) -> Built {
        let (mut mn, mut mx) = (Vec3::MAX, Vec3::MIN);
        for (_, m) in &self.bodies {
            for p in &m.pos {
                mn = mn.min(*p);
                mx = mx.max(*p);
            }
        }
        let off = Vec3::new(-(mn.x + mx.x) * 0.5, -mn.y, -(mn.z + mx.z) * 0.5);
        for (_, m) in &mut self.bodies {
            m.pos.iter_mut().for_each(|p| *p += off);
        }
        self.joints.iter_mut().for_each(|j| j.anchor += off);
        self.ports.iter_mut().for_each(|p| p.pos += off);
        self.paths.iter_mut().for_each(|(_, pts)| pts.iter_mut().for_each(|p| *p += off));
        Built { bodies: self.bodies, joints: self.joints, ports: self.ports, paths: self.paths }
    }
}

fn build_pipe(b: &mut Builder, d: f32, len: f32) {
    let r = d * 0.5;
    let (a, e) = (Vec3::new(-len * 0.5, 0.0, 0.0), Vec3::new(len * 0.5, 0.0, 0.0));
    b.cyl(a, e, r, PIPE);
    if len > 0.08 {
        b.flange(a + Vec3::X * flange_t(d), -Vec3::X, d);
        b.flange(e - Vec3::X * flange_t(d), Vec3::X, d);
    }
    b.port("a", PortKind::Pipe, a, -Vec3::X, d);
    b.port("b", PortKind::Pipe, e, Vec3::X, d);
    b.path("a", vec![Vec3::ZERO, a]);
    b.path("b", vec![Vec3::ZERO, e]);
}

/// Flange thickness for a pipe diameter.
fn flange_t(d: f32) -> f32 {
    (d * 0.12).clamp(0.006, 0.03)
}

/// Elbow radius for a pipe diameter.
pub fn elbow_radius(d: f32) -> f32 {
    1.5 * d
}

fn build_elbow(b: &mut Builder, d: f32, angle: f32) {
    let big = elbow_radius(d);
    let c = Vec3::new(0.0, big, 0.0);
    // Start at the origin travelling +X, bending up towards +Y.
    b.torus_arc(c, -Vec3::Y, Vec3::X, big, d * 0.5, angle, PIPE);
    let th = angle.to_radians();
    let end = c + Vec3::new(th.sin(), -th.cos(), 0.0) * big;
    let end_dir = Vec3::new(th.cos(), th.sin(), 0.0);
    let t = flange_t(d);
    b.flange(Vec3::X * t, -Vec3::X, d);
    b.flange(end - end_dir * t, end_dir, d);
    b.port("a", PortKind::Pipe, Vec3::ZERO, -Vec3::X, d);
    b.port("b", PortKind::Pipe, end, end_dir, d);
    let mid = th * 0.5;
    let pm = c + Vec3::new(mid.sin(), -mid.cos(), 0.0) * big;
    let arc = |from: f32, to: f32| -> Vec<Vec3> {
        (0..=8).map(|i| from + (to - from) * i as f32 / 8.0).map(|a| c + Vec3::new(a.sin(), -a.cos(), 0.0) * big).collect()
    };
    b.path("a", arc(mid, 0.0));
    b.path("b", arc(mid, th));
    let _ = pm;
}

fn build_tee(b: &mut Builder, d: f32) {
    let h = d * 1.5;
    let (a, e, top) = (Vec3::new(-h, 0.0, 0.0), Vec3::new(h, 0.0, 0.0), Vec3::new(0.0, h, 0.0));
    b.cyl(a, e, d * 0.5, PIPE);
    b.cyl(Vec3::ZERO, top, d * 0.5, PIPE);
    let t = flange_t(d);
    b.flange(a + Vec3::X * t, -Vec3::X, d);
    b.flange(e - Vec3::X * t, Vec3::X, d);
    b.flange(top - Vec3::Y * t, Vec3::Y, d);
    b.port("a", PortKind::Pipe, a, -Vec3::X, d);
    b.port("b", PortKind::Pipe, e, Vec3::X, d);
    b.port("c", PortKind::Pipe, top, Vec3::Y, d);
    b.path("a", vec![Vec3::ZERO, a]);
    b.path("b", vec![Vec3::ZERO, e]);
    b.path("c", vec![Vec3::ZERO, top]);
}

fn build_reducer(b: &mut Builder, d1: f32, d2: f32) {
    let len = d1.max(d2) * 2.0;
    let (a, e) = (Vec3::new(-len * 0.5, 0.0, 0.0), Vec3::new(len * 0.5, 0.0, 0.0));
    let q = len * 0.25;
    b.cyl(a, a + Vec3::X * q, d1 * 0.5, PIPE);
    b.frustum(a + Vec3::X * q, e - Vec3::X * q, d1 * 0.5, d2 * 0.5, false, PIPE);
    b.cyl(e - Vec3::X * q, e, d2 * 0.5, PIPE);
    b.flange(a + Vec3::X * flange_t(d1), -Vec3::X, d1);
    b.flange(e - Vec3::X * flange_t(d2), Vec3::X, d2);
    b.port("a", PortKind::Pipe, a, -Vec3::X, d1);
    b.port("b", PortKind::Pipe, e, Vec3::X, d2);
    b.path("a", vec![Vec3::ZERO, a]);
    b.path("b", vec![Vec3::ZERO, e]);
}

fn build_valve(b: &mut Builder, d: f32) {
    // Flanged ball valve, 3d face to face: end necks, a bulbous body around
    // the ball, a bonnet with the stem, and a quarter-turn lever.
    let len = d * 3.0;
    let r = d * 0.5;
    let (a, e) = (Vec3::new(-len * 0.5, 0.0, 0.0), Vec3::new(len * 0.5, 0.0, 0.0));
    let t = flange_t(d);
    let (rn, rb, x0) = (r * 1.08, r * 1.6, d * 0.8);
    let xi = x0 * 0.4;
    b.cyl(a, Vec3::new(-x0, 0.0, 0.0), rn, BLUE);
    b.frustum(Vec3::new(-x0, 0.0, 0.0), Vec3::new(-xi, 0.0, 0.0), rn, rb, false, BLUE);
    b.cyl(Vec3::new(-xi, 0.0, 0.0), Vec3::new(xi, 0.0, 0.0), rb, BLUE);
    b.frustum(Vec3::new(xi, 0.0, 0.0), Vec3::new(x0, 0.0, 0.0), rb, rn, false, BLUE);
    b.cyl(Vec3::new(x0, 0.0, 0.0), e, rn, BLUE);
    b.flange(a + Vec3::X * t, -Vec3::X, d);
    b.flange(e - Vec3::X * t, Vec3::X, d);
    // Bonnet and stem.
    let bonnet = rb + (0.25 * d).max(0.012);
    b.cyl(Vec3::new(0.0, rb * 0.8, 0.0), Vec3::new(0.0, bonnet, 0.0), r * 0.6, BLUE);
    let stem_top = bonnet + (0.3 * d).max(0.015);
    b.cyl(Vec3::new(0.0, bonnet, 0.0), Vec3::new(0.0, stem_top, 0.0), (d * 0.12).max(0.006), STEEL);
    // Lever: along the pipe when open, across it when closed (quarter turn).
    let lever = b.body("lever");
    let lv = (3.0 * d).max(0.15);
    let w = (d * 0.25).max(0.012);
    b.cuboid(Vec3::new(lv * 0.5 - w, stem_top + w * 0.5, 0.0), Vec3::new(lv * 0.5, w * 0.5, w * 0.6), Quat::IDENTITY, RED);
    b.cyl(Vec3::new(0.0, stem_top - w * 0.2, 0.0), Vec3::new(0.0, stem_top + w * 1.1, 0.0), w * 0.9, STEEL);
    b.joint("lever", true, None, lever, Vec3::new(0.0, stem_top, 0.0), 'Y', Some((0.0, 90.0)));
    b.body("static");
    b.port("a", PortKind::Pipe, a, -Vec3::X, d);
    b.port("b", PortKind::Pipe, e, Vec3::X, d);
    b.path("a", vec![Vec3::ZERO, a]);
    b.path("b", vec![Vec3::ZERO, e]);
}

/// Height of a pump's suction axis above its feet (without a plinth).
pub fn pump_axis(d: f32) -> f32 {
    0.22 * (d / 0.05).clamp(0.6, 4.0)
}

fn build_pump(b: &mut Builder, d: f32, motor: bool, base: f32) {
    let k = (d / 0.05).clamp(0.6, 4.0);
    // Concrete plinth; everything else stands on it.
    let y0 = base.max(0.0);
    if y0 > 0.0 {
        b.cuboid(Vec3::new(-0.1 * k, y0 * 0.5, 0.0), Vec3::new(0.44 * k, y0 * 0.5, 0.2 * k), Quat::IDENTITY, GREY);
    }
    let h = y0 + pump_axis(d);
    // Base plate.
    b.cuboid(Vec3::new(-0.1 * k, y0 + 0.02 * k, 0.0), Vec3::new(0.4 * k, 0.02 * k, 0.17 * k), Quat::IDENTITY, DARK);
    if motor {
        b.cuboid(Vec3::new(-0.31 * k, y0 + 0.07 * k, 0.0), Vec3::new(0.17 * k, 0.03 * k, 0.1 * k), Quat::IDENTITY, GREEN);
        b.cyl(Vec3::new(-0.48 * k, h, 0.0), Vec3::new(-0.14 * k, h, 0.0), 0.12 * k, GREEN);
        b.cyl(Vec3::new(-0.52 * k, h, 0.0), Vec3::new(-0.48 * k, h, 0.0), 0.1 * k, DARK);
        for i in 0..8 {
            let a = i as f32 / 8.0 * PI;
            b.cuboid(Vec3::new(-0.31 * k, h, 0.0), Vec3::new(0.16 * k, 0.127 * k, 0.006 * k), Quat::from_rotation_x(a), GREEN);
        }
    }
    // Bearing bracket and volute.
    b.cuboid(Vec3::new(-0.02 * k, y0 + 0.1 * k, 0.0), Vec3::new(0.05 * k, 0.06 * k, 0.07 * k), Quat::IDENTITY, BLUE);
    b.cyl(Vec3::new(0.0, h, 0.0), Vec3::new(0.12 * k, h, 0.0), 0.14 * k, BLUE);
    // Suction (axial, +X) and discharge (up) nozzles.
    let s_end = Vec3::new(0.24 * k, h, 0.0);
    b.cyl(Vec3::new(0.12 * k, h, 0.0), s_end, d * 0.5, BLUE);
    b.flange(s_end - Vec3::X * flange_t(d), Vec3::X, d);
    let d_end = Vec3::new(0.06 * k, h + 0.26 * k, 0.0);
    b.cyl(Vec3::new(0.06 * k, h, 0.0), d_end, d * 0.5, BLUE);
    b.flange(d_end - Vec3::Y * flange_t(d), Vec3::Y, d);
    // Rotor: shaft and a coupling with lugs, so you can see it turn.
    let rotor = b.body("rotor");
    let x0 = if motor { -0.14 * k } else { -0.3 * k };
    b.cyl(Vec3::new(x0, h, 0.0), Vec3::new(0.0, h, 0.0), 0.022 * k, STEEL);
    b.cyl(Vec3::new(-0.1 * k, h, 0.0), Vec3::new(-0.07 * k, h, 0.0), 0.07 * k, YELLOW);
    b.cuboid(Vec3::new(-0.085 * k, h + 0.06 * k, 0.0), Vec3::new(0.018 * k, 0.02 * k, 0.02 * k), Quat::IDENTITY, DARK);
    b.cuboid(Vec3::new(-0.085 * k, h - 0.06 * k, 0.0), Vec3::new(0.018 * k, 0.02 * k, 0.02 * k), Quat::IDENTITY, DARK);
    b.joint("rotor", true, None, rotor, Vec3::new(0.0, h, 0.0), 'X', None);
    b.body("static");
    if !motor {
        b.port("shaft", PortKind::Shaft, Vec3::new(x0, h, 0.0), -Vec3::X, 0.044 * k);
    }
    b.port("inlet", PortKind::Pipe, s_end, Vec3::X, d);
    b.port("outlet", PortKind::Pipe, d_end, Vec3::Y, d);
    let c = Vec3::new(0.06 * k, h, 0.0);
    b.path("inlet", vec![c, s_end]);
    b.path("outlet", vec![c, d_end]);
}

/// Shell geometry of a generated tank: (bottom y, radius, shell height), local frame.
pub fn tank_shell(dia: f32, h: f32) -> (f32, f32, f32) {
    let legs = (0.25 * dia).clamp(0.15, 0.5);
    (legs, dia * 0.5, h)
}

/// Height of a generated tank's side nozzles (outlet, return) above its feet.
pub fn tank_nozzle_y(dia: f32, h: f32, d: f32) -> f32 {
    tank_shell(dia, h).0 + (0.1 * h).max(d)
}

fn build_tank(b: &mut Builder, dia: f32, h: f32, d: f32) {
    let (y0, r, h) = tank_shell(dia, h);
    for (sx, sz) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
        let p = Vec3::new(sx * r * 0.7, 0.0, sz * r * 0.7);
        b.cyl(p, p + Vec3::Y * (y0 + 0.05), (0.04 * dia).max(0.02), GREY);
    }
    b.frustum(Vec3::new(0.0, y0, 0.0), Vec3::new(0.0, y0 + h, 0.0), r, r, false, WHITE);
    b.frustum(Vec3::new(0.0, y0 - 0.06 * r, 0.0), Vec3::new(0.0, y0, 0.0), r * 0.3, r, false, WHITE);
    b.frustum(Vec3::new(0.0, y0 + h, 0.0), Vec3::new(0.0, y0 + h + 0.15 * r, 0.0), r, r * 0.25, true, WHITE);
    // Top inlet and low side outlet.
    let top = y0 + h + 0.15 * r;
    let inlet = Vec3::new(0.0, top + 0.1, 0.0);
    b.cyl(Vec3::new(0.0, top - 0.05, 0.0), inlet, d * 0.5, STEEL);
    b.flange(inlet - Vec3::Y * flange_t(d), Vec3::Y, d);
    let oy = tank_nozzle_y(dia, h, d);
    let outlet = Vec3::new(r + 0.12, oy, 0.0);
    b.cyl(Vec3::new(r - 0.02, oy, 0.0), outlet, d * 0.5, STEEL);
    b.flange(outlet - Vec3::X * flange_t(d), Vec3::X, d);
    // Return nozzle opposite the outlet.
    let ret = Vec3::new(-r - 0.12, oy, 0.0);
    b.cyl(Vec3::new(-r + 0.02, oy, 0.0), ret, d * 0.5, STEEL);
    b.flange(ret + Vec3::X * flange_t(d), -Vec3::X, d);
    b.port("inlet", PortKind::Pipe, inlet, Vec3::Y, d);
    b.port("outlet", PortKind::Pipe, outlet, Vec3::X, d);
    b.port("return", PortKind::Pipe, ret, -Vec3::X, d);
}

fn build_motor(b: &mut Builder, d: f32) {
    let k = (d / 0.03).clamp(0.5, 5.0);
    let h = 0.15 * k;
    b.cuboid(Vec3::new(-0.16 * k, 0.025 * k, 0.0), Vec3::new(0.14 * k, 0.025 * k, 0.11 * k), Quat::IDENTITY, DARK);
    b.cyl(Vec3::new(-0.32 * k, h, 0.0), Vec3::new(0.0, h, 0.0), 0.11 * k, GREEN);
    b.cyl(Vec3::new(-0.36 * k, h, 0.0), Vec3::new(-0.32 * k, h, 0.0), 0.095 * k, DARK);
    for i in 0..8 {
        let a = i as f32 / 8.0 * PI;
        b.cuboid(Vec3::new(-0.16 * k, h, 0.0), Vec3::new(0.15 * k, 0.117 * k, 0.005 * k), Quat::from_rotation_x(a), GREEN);
    }
    b.cuboid(Vec3::new(-0.12 * k, h + 0.13 * k, 0.0), Vec3::new(0.05 * k, 0.03 * k, 0.05 * k), Quat::IDENTITY, DARK);
    let rotor = b.body("rotor");
    let end = Vec3::new(0.09 * k, h, 0.0);
    b.cyl(Vec3::new(-0.01 * k, h, 0.0), end, d * 0.5, STEEL);
    b.cuboid(Vec3::new(0.05 * k, h + d * 0.5, 0.0), Vec3::new(0.03 * k, d * 0.12, d * 0.15), Quat::IDENTITY, DARK);
    b.joint("rotor", true, None, rotor, Vec3::new(0.0, h, 0.0), 'X', None);
    b.body("static");
    b.port("shaft", PortKind::Shaft, end, Vec3::X, d);
}

fn build_pulley(b: &mut Builder, dia: f32, d: f32) {
    let w = (dia * 0.25).clamp(0.02, 0.08);
    let rotor = b.body("rotor");
    b.cyl(Vec3::new(-w * 0.5, 0.0, 0.0), Vec3::new(-w * 0.15, 0.0, 0.0), dia * 0.5, STEEL);
    b.cyl(Vec3::new(-w * 0.15, 0.0, 0.0), Vec3::new(w * 0.15, 0.0, 0.0), dia * 0.42, DARK);
    b.cyl(Vec3::new(w * 0.15, 0.0, 0.0), Vec3::new(w * 0.5, 0.0, 0.0), dia * 0.5, STEEL);
    b.cyl(Vec3::new(-w * 0.8, 0.0, 0.0), Vec3::new(-w * 0.5, 0.0, 0.0), d * 0.9, STEEL);
    for i in 0..4 {
        let a = i as f32 / 4.0 * TAU;
        b.cuboid(Vec3::new(w * 0.5 + 0.002, a.cos() * dia * 0.25, a.sin() * dia * 0.25), Vec3::new(0.002, dia * 0.05, dia * 0.05), Quat::IDENTITY, YELLOW);
    }
    b.joint("rotor", true, None, rotor, Vec3::ZERO, 'X', None);
    b.port("shaft", PortKind::Shaft, Vec3::new(-w * 0.8, 0.0, 0.0), -Vec3::X, d);
    b.port("belt", PortKind::Pulley, Vec3::ZERO, Vec3::X, dia);
}

/// Crank phases (degrees) of an inline engine with `n` cylinders.
pub fn crank_phases(n: u32) -> Vec<f32> {
    match n {
        1 => vec![0.0],
        2 => vec![0.0, 180.0],
        3 => vec![0.0, 240.0, 120.0],
        4 => vec![0.0, 180.0, 180.0, 0.0],
        6 => vec![0.0, 240.0, 120.0, 120.0, 240.0, 0.0],
        _ => (0..n).map(|k| (k as f32 * 360.0 / n as f32) % 360.0).collect(),
    }
}

/// Piston pin height above the crank axis at crank angle `a` (radians).
pub fn pin_height(a: f32, r: f32, l: f32) -> f32 {
    r * a.cos() + (l * l - r * r * a.sin() * a.sin()).max(0.0).sqrt()
}

fn build_engine(b: &mut Builder, n: u32) {
    let (r, l, hc) = (ENGINE_R, ENGINE_L, ENGINE_HC);
    let bore = 0.075;
    let xs: Vec<f32> = (0..n).map(|k| (k as f32 - (n as f32 - 1.0) * 0.5) * ENGINE_PITCH).collect();
    let (x0, x1) = (xs[0] - ENGINE_PITCH * 0.6, xs[n as usize - 1] + ENGINE_PITCH * 0.6);
    let phases = crank_phases(n);
    // Crankcase (low, open on top so the crank shows), bearing pedestals, sleeves.
    b.cuboid(Vec3::new((x0 + x1) * 0.5, 0.05, 0.0), Vec3::new((x1 - x0) * 0.5, 0.05, 0.13), Quat::IDENTITY, GREY);
    for side in [-1.0f32, 1.0] {
        b.cuboid(Vec3::new((x0 + x1) * 0.5, 0.15, side * 0.125), Vec3::new((x1 - x0) * 0.5, 0.06, 0.005), Quat::IDENTITY, GREY);
    }
    for k in 0..=n as usize {
        let x = if k == 0 { x0 + 0.02 } else if k == n as usize { x1 - 0.02 } else { (xs[k - 1] + xs[k]) * 0.5 };
        b.cuboid(Vec3::new(x, (hc + 0.1) * 0.5, 0.0), Vec3::new(0.01, (hc - 0.1) * 0.5 + 0.03, 0.04), Quat::IDENTITY, GREY);
    }
    let sleeve_lo = hc + l - r - 0.03;
    let sleeve_hi = hc + l + r + 0.07;
    for &x in &xs {
        b.sleeve(Vec3::new(x, sleeve_lo, 0.0), Vec3::new(x, sleeve_hi, 0.0), bore * 0.5 + 0.004, STEEL);
        b.sleeve(Vec3::new(x, sleeve_lo, 0.0), Vec3::new(x, sleeve_hi, 0.0), bore * 0.5 + 0.012, GREY);
    }
    // Crank: main shaft, throws, flywheel (+X) and front pulley (-X).
    let crank = b.body("crank");
    let axis_c = Vec3::new(0.0, hc, 0.0);
    b.cyl(Vec3::new(x0 - 0.06, hc, 0.0), Vec3::new(x1 + 0.08, hc, 0.0), 0.02, STEEL);
    for (k, &x) in xs.iter().enumerate() {
        let ps = phases[k].to_radians();
        let off = Vec3::new(0.0, ps.cos(), ps.sin()) * r;
        b.cyl(Vec3::new(x - 0.018, hc, 0.0) + off, Vec3::new(x + 0.018, hc, 0.0) + off, 0.017, STEEL);
        let rot = Quat::from_rotation_x(ps);
        for s in [-1.0f32, 1.0] {
            b.cuboid(Vec3::new(x + s * 0.024, hc, 0.0) + off * 0.5, Vec3::new(0.006, r * 0.5 + 0.03, 0.032), rot, DARK);
        }
    }
    let fw = Vec3::new(x1 + 0.05, hc, 0.0);
    b.cyl(fw, fw + Vec3::X * 0.03, 0.13, DARK);
    b.cuboid(fw + Vec3::new(0.032, 0.09, 0.0), Vec3::new(0.003, 0.025, 0.012), Quat::IDENTITY, YELLOW);
    let fp = Vec3::new(x0 - 0.04, hc, 0.0);
    b.cyl(fp - Vec3::X * 0.025, fp, 0.06, STEEL);
    b.cuboid(fp + Vec3::new(-0.027, 0.04, 0.0), Vec3::new(0.003, 0.012, 0.008), Quat::IDENTITY, YELLOW);
    b.joint("crank", true, None, crank, axis_c, 'X', None);
    // Pistons (slide in Y) carrying rods (swing about X at the wrist pin).
    let rod_lim = (r / l).asin().to_degrees() + 3.0;
    for (k, &x) in xs.iter().enumerate() {
        let ps = phases[k];
        let w = hc + pin_height(ps.to_radians(), r, l);
        // Prim names allow no dots: lengths in whole millimeters.
        let tag = format!("{}_p{:.0}_r{:.0}_l{:.0}", k + 1, ps, r * 1000.0, l * 1000.0);
        let piston = b.body(&format!("piston{}", k + 1));
        b.cyl(Vec3::new(x, w - 0.025, 0.0), Vec3::new(x, w + 0.045, 0.0), bore * 0.5, WHITE);
        b.cyl(Vec3::new(x - 0.025, w, 0.0), Vec3::new(x + 0.025, w, 0.0), 0.009, STEEL);
        b.joint(&format!("piston_{tag}"), false, None, piston, Vec3::new(x, w, 0.0), 'Y', Some((-2.2 * r, 2.2 * r)));
        let rod = b.body(&format!("rod{}", k + 1));
        let pin = Vec3::new(x, w - l, 0.0);
        b.cuboid(Vec3::new(x, w - l * 0.5, 0.0), Vec3::new(0.008, l * 0.5, 0.011), Quat::IDENTITY, STEEL);
        b.cyl(pin - Vec3::X * 0.012, pin + Vec3::X * 0.012, 0.026, STEEL);
        b.joint(&format!("rod_{tag}"), true, Some(piston), rod, Vec3::new(x, w, 0.0), 'X', Some((-rod_lim, rod_lim)));
    }
    b.body("static");
    b.port("flywheel", PortKind::Shaft, fw + Vec3::X * 0.03, Vec3::X, 0.04);
    b.port("pulley", PortKind::Pulley, fp - Vec3::X * 0.0125, -Vec3::X, 0.12);
}

fn build_conveyor(b: &mut Builder, len: f32, w: f32, h: f32) {
    let top = h;
    let axis_y = top - ROLLER_R;
    let hw = w * 0.5;
    // Side rails, legs and cross braces.
    for s in [-1.0f32, 1.0] {
        b.cuboid(Vec3::new(0.0, axis_y + 0.005, s * (hw + 0.025)), Vec3::new(len * 0.5, 0.06, 0.012), Quat::IDENTITY, BLUE);
        let nl = ((len / 1.5).ceil() as usize).max(1) + 1;
        for i in 0..nl {
            let x = -len * 0.5 + 0.05 + (len - 0.1) * i as f32 / (nl - 1) as f32;
            b.cuboid(Vec3::new(x, (axis_y - 0.05) * 0.5, s * (hw + 0.025)), Vec3::new(0.02, (axis_y - 0.05) * 0.5, 0.02), Quat::IDENTITY, GREY);
        }
    }
    b.cuboid(Vec3::new(0.0, (axis_y - 0.05) * 0.35, 0.0), Vec3::new(len * 0.5 - 0.05, 0.01, hw + 0.02), Quat::IDENTITY, GREY);
    // Rollers: one body each, so each one spins.
    let pitch = ROLLER_R * 2.15;
    let n = ((len - ROLLER_R * 2.0) / pitch).floor() as usize + 1;
    let x0 = -(n as f32 - 1.0) * pitch * 0.5;
    for i in 0..n {
        let x = x0 + i as f32 * pitch;
        let body = b.body(&format!("roller{}", i + 1));
        b.cyl(Vec3::new(x, axis_y, -hw), Vec3::new(x, axis_y, hw), ROLLER_R, STEEL);
        b.cuboid(Vec3::new(x, axis_y + ROLLER_R * 0.98, 0.0), Vec3::new(0.004, 0.003, hw * 0.9), Quat::IDENTITY, DARK);
        b.joint(&format!("roller_{}", i + 1), true, None, body, Vec3::new(x, axis_y, 0.0), 'Z', None);
    }
    b.body("static");
    b.port("in", PortKind::ConveyorIn, Vec3::new(-len * 0.5, top, 0.0), Vec3::X, w);
    b.port("out", PortKind::ConveyorOut, Vec3::new(len * 0.5, top, 0.0), Vec3::X, w);
}

// ---------------------------------------------------------------------------
// USD

fn usda(name: &str, b: &Built) -> String {
    let mut s = format!("#usda 1.0\n{PART_REV}\n(\n    defaultPrim = \"{name}\"\n    metersPerUnit = 1\n    upAxis = \"Y\"\n)\n\ndef Xform \"{name}\" (\n    kind = \"component\"\n)\n{{\n");
    for (bname, m) in &b.bodies {
        if m.idx.is_empty() {
            continue;
        }
        let _ = writeln!(s, "    def Xform \"{bname}\"\n    {{\n        def Mesh \"geometry\"\n        {{");
        let f = |v: &Vec3| format!("({:.5}, {:.5}, {:.5})", v.x, v.y, v.z);
        let counts = vec!["3"; m.idx.len() / 3].join(", ");
        let idx: Vec<String> = m.idx.iter().map(|i| i.to_string()).collect();
        let pts: Vec<String> = m.pos.iter().map(f).collect();
        let nrm: Vec<String> = m.nrm.iter().map(f).collect();
        let col: Vec<String> = m.col.iter().map(|c| format!("({:.3}, {:.3}, {:.3})", c[0], c[1], c[2])).collect();
        let _ = writeln!(s, "            int[] faceVertexCounts = [{counts}]");
        let _ = writeln!(s, "            int[] faceVertexIndices = [{}]", idx.join(", "));
        let _ = writeln!(s, "            point3f[] points = [{}]", pts.join(", "));
        let _ = writeln!(s, "            normal3f[] normals = [{}] (\n                interpolation = \"vertex\"\n            )", nrm.join(", "));
        let _ = writeln!(s, "            color3f[] primvars:displayColor = [{}] (\n                interpolation = \"vertex\"\n            )", col.join(", "));
        let _ = writeln!(s, "            uniform token orientation = \"rightHanded\"\n            uniform token subdivisionScheme = \"none\"\n            uniform bool doubleSided = 1\n        }}\n    }}\n");
    }
    for j in &b.joints {
        let ty = if j.revolute { "PhysicsRevoluteJoint" } else { "PhysicsPrismaticJoint" };
        let a = j.anchor;
        let _ = writeln!(s, "    def {ty} \"joint_{}\"\n    {{", j.name);
        if let Some(p) = j.parent {
            let _ = writeln!(s, "        rel physics:body0 = </{name}/{}>", b.bodies[p].0);
        }
        let _ = writeln!(s, "        rel physics:body1 = </{name}/{}>", b.bodies[j.child].0);
        let _ = writeln!(s, "        point3f physics:localPos0 = ({:.5}, {:.5}, {:.5})", a.x, a.y, a.z);
        let _ = writeln!(s, "        point3f physics:localPos1 = ({:.5}, {:.5}, {:.5})", a.x, a.y, a.z);
        let _ = writeln!(s, "        uniform token physics:axis = \"{}\"", j.axis);
        if let Some((lo, hi)) = j.limits {
            let _ = writeln!(s, "        float physics:lowerLimit = {lo}\n        float physics:upperLimit = {hi}");
        }
        let _ = writeln!(s, "    }}\n");
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_roundtrip() {
        for spec in [
            Spec::Pipe { d: 0.1, len: 2.345 },
            Spec::Elbow { d: 0.05, angle: 90.0 },
            Spec::Pump { d: 0.065, motor: false, base: 0.0 },
            Spec::Tank { dia: 1.2, h: 1.5, d: 0.05 },
            Spec::Engine { n: 4 },
            Spec::Pulley { dia: 0.2, d: 0.03 },
        ] {
            let s = spec.normalized();
            let p = PathBuf::from(format!("C:/x/SenseSim/plant/{}/{}/{}.usda", s.kind(), s.params(), s.kind()));
            assert_eq!(Spec::of_asset(&p), Some(s));
        }
    }

    #[test]
    fn parts_sit_bottom_centered_and_ports_line_up() {
        let b = Spec::Pipe { d: 0.1, len: 2.0 }.built();
        let a = b.ports.iter().find(|p| p.id == "a").unwrap();
        let e = b.ports.iter().find(|p| p.id == "b").unwrap();
        assert!(((e.pos - a.pos).length() - 2.0).abs() < 1e-4);
        assert!((a.pos.y - 0.08).abs() < 1e-3, "flanges make the pipe 0.16 tall: axis at 0.08, got {}", a.pos.y);
        // Elbow: ports a quarter circle apart, perpendicular.
        let b = Spec::Elbow { d: 0.1, angle: 90.0 }.built();
        let (a, e) = (&b.ports[0], &b.ports[1]);
        assert!(a.dir.dot(e.dir).abs() < 1e-4);
        let r = elbow_radius(0.1);
        assert!(((e.pos - a.pos).length() - r * 2f32.sqrt()).abs() < 1e-4);
    }

    #[test]
    fn flanges_sit_flush_on_their_port_faces() {
        // The flange disc (wider than the pipe) must reach each pipe port's
        // face, so two mated parts meet flange to flange with no gap.
        for spec in [Spec::Pipe { d: 0.05, len: 1.0 }, Spec::Valve { d: 0.05 }, Spec::Elbow { d: 0.05, angle: 90.0 }, Spec::Tee { d: 0.05 }, Spec::Pump { d: 0.05, motor: true, base: 0.15 }, Spec::Tank { dia: 1.0, h: 1.2, d: 0.05 }] {
            let b = spec.normalized().built();
            let pts: Vec<Vec3> = b.bodies.iter().filter(|(n, _)| n == "static").flat_map(|(_, m)| m.pos.iter().copied()).collect();
            for p in b.ports.iter().filter(|p| p.kind == PortKind::Pipe) {
                // Points of the flange rim: in the port's plane region, well outside the bore.
                let reach = pts
                    .iter()
                    .filter(|v| {
                        let rel = **v - p.pos;
                        let along = rel.dot(p.dir);
                        (rel - p.dir * along).length() > p.size * 0.7 && along > -0.05 && along < 0.01
                    })
                    .map(|v| (*v - p.pos).dot(p.dir))
                    .fold(f32::MIN, f32::max);
                assert!(reach.abs() < 5e-4, "{} port {}: flange face {reach:.4} m from the port face", spec.kind(), p.id);
            }
        }
    }

    #[test]
    fn every_part_loads_through_the_usd_loader() {
        let dir = std::env::temp_dir().join(format!("sense_sim_parts_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for spec in [
            Spec::Pipe { d: 0.05, len: 1.0 },
            Spec::Elbow { d: 0.05, angle: 90.0 },
            Spec::Tee { d: 0.05 },
            Spec::Reducer { d1: 0.08, d2: 0.05 },
            Spec::Valve { d: 0.05 },
            Spec::Pump { d: 0.05, motor: true, base: 0.15 },
            Spec::Tank { dia: 1.0, h: 1.2, d: 0.05 },
            Spec::Motor { d: 0.03 },
            Spec::Pulley { dia: 0.12, d: 0.03 },
            Spec::Engine { n: 4 },
            Spec::Conveyor { len: 1.0, w: 0.5, h: 0.7 },
        ] {
            let s = spec.normalized();
            let b = s.built();
            let path = dir.join(format!("{}.usda", s.kind()));
            std::fs::write(&path, usda(s.kind(), &b)).unwrap();
            let loaded = crate::usd_load::load(&path).unwrap_or_else(|e| panic!("{}: {e:#}", s.kind()));
            assert!(!loaded.parts.is_empty(), "{}", s.kind());
            assert_eq!(loaded.joints.len(), b.joints.len(), "{}: joints", s.kind());
            assert!(loaded.joints.iter().all(|j| j.body1.is_some()), "{}", s.kind());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_pistons_follow_the_crank() {
        assert!((pin_height(0.0, ENGINE_R, ENGINE_L) - (ENGINE_R + ENGINE_L)).abs() < 1e-6);
        assert!((pin_height(PI, ENGINE_R, ENGINE_L) - (ENGINE_L - ENGINE_R)).abs() < 1e-6);
        let b = Spec::Engine { n: 4 }.built();
        assert_eq!(b.joints.iter().filter(|j| j.name.starts_with("piston")).count(), 4);
        assert_eq!(b.joints.iter().filter(|j| j.name.starts_with("rod")).count(), 4);
        let text = usda("engine", &b);
        assert!(text.contains("PhysicsPrismaticJoint") && text.contains("rel physics:body0 = </engine/piston1>"));
    }
}
