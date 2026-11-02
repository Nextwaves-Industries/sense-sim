//! Complete systems assembled port to port from generated parts, so every
//! connection is exact: a pump loop between two tanks, an engine driving a
//! coolant pump by belt (and a generator off the flywheel), and a conveyor
//! line. Used by the `plant_build` tool and the built-in samples.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use bevy::prelude::*;
use serde_json::{Value, json};

use super::connect::{self, Link, LinkKind, PortRef, Turn};
use super::parts::Spec;
use super::route::{self, Obstacle};
use super::sim::Control;

pub struct Item {
    pub name: String,
    pub usd: PathBuf,
    pub transform: Transform,
    /// Generated part (ports known), or `None` for other assets.
    pub spec: Option<Spec>,
}

#[derive(Default)]
pub struct Assembly {
    pub items: Vec<Item>,
    pub links: Vec<Link>,
    pub controls: Vec<(String, Control)>,
    /// Valve lever targets (degrees: 0 open, 90 closed).
    pub levers: Vec<(String, f32)>,
    pub summary: Value,
}

/// Unique names: `prefix_kind`, then `prefix_kind_2`, … avoiding `taken`.
pub struct Namer {
    pub prefix: String,
    pub taken: HashSet<String>,
}

impl Namer {
    pub fn new(prefix: &str, taken: HashSet<String>) -> Self {
        Self { prefix: prefix.into(), taken }
    }
    pub fn name(&mut self, kind: &str) -> String {
        let base = if self.prefix.is_empty() { kind.to_string() } else { format!("{}_{kind}", self.prefix) };
        let mut n = base.clone();
        let mut i = 2;
        while self.taken.contains(&n) {
            n = format!("{base}_{i}");
            i += 1;
        }
        self.taken.insert(n.clone());
        n
    }
}

/// Local bounds of a generated part.
pub fn spec_bounds(spec: &Spec) -> (Vec3, Vec3) {
    let b = spec.built();
    let mut mn = Vec3::MAX;
    let mut mx = Vec3::MIN;
    for (_, m) in &b.bodies {
        for p in &m.pos {
            mn = mn.min(*p);
            mx = mx.max(*p);
        }
    }
    (mn, mx)
}

pub fn world_box(t: &Transform, (a, b): (Vec3, Vec3)) -> (Vec3, Vec3) {
    let mut mn = Vec3::MAX;
    let mut mx = Vec3::MIN;
    for x in [a.x, b.x] {
        for y in [a.y, b.y] {
            for z in [a.z, b.z] {
                let p = t.transform_point(Vec3::new(x, y, z));
                mn = mn.min(p);
                mx = mx.max(p);
            }
        }
    }
    (mn, mx)
}

impl Assembly {
    pub(crate) fn add(&mut self, name: String, spec: Spec, t: Transform) -> Result<String> {
        let spec = spec.normalized();
        self.items.push(Item { name: name.clone(), usd: spec.usd()?, transform: t, spec: Some(spec) });
        Ok(name)
    }
    fn item(&self, name: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.name == name)
    }
    /// World pose of a port of an item.
    pub fn port(&self, r: &PortRef) -> Result<(Vec3, Vec3, f32)> {
        let it = self.item(&r.object).ok_or_else(|| anyhow!("no part {}", r.object))?;
        let spec = it.spec.ok_or_else(|| anyhow!("{} has no known ports", r.object))?;
        let ports = spec.ports();
        let p = ports.iter().find(|p| p.id == r.port).ok_or_else(|| anyhow!("{} has no port {}", r.object, r.port))?;
        let (w, d) = p.world(&it.transform);
        Ok((w, d, p.size))
    }
    /// Places `spec` so its port `mine` mates with `target`, and links them.
    pub(crate) fn attach(&mut self, name: String, spec: Spec, mine: &str, target: &PortRef, aim: Option<Vec3>) -> Result<String> {
        let spec = spec.normalized();
        let (tp, td, _) = self.port(target)?;
        let ports = spec.ports();
        let p = ports.iter().find(|p| p.id == mine).ok_or_else(|| anyhow!("{} has no port {mine}", spec.kind()))?;
        let other = ports.iter().find(|q| q.id != mine && q.kind == p.kind);
        let tgt_kind = self.port_kind(target)?;
        let kind = super::ports::PortKind::mates(tgt_kind, p.kind).ok_or_else(|| anyhow!("ports don't mate"))?;
        let t = connect::snap(tp, td, kind, p, other, &Transform::default(), Turn::Free { aim }).map_err(|e| anyhow!(e))?;
        let n = self.add(name, spec, t)?;
        self.links.push(Link { a: target.clone(), b: PortRef::new(&n, mine), kind });
        Ok(n)
    }
    fn port_kind(&self, r: &PortRef) -> Result<super::ports::PortKind> {
        let it = self.item(&r.object).ok_or_else(|| anyhow!("no part {}", r.object))?;
        let spec = it.spec.ok_or_else(|| anyhow!("{} has no known ports", r.object))?;
        spec.ports().iter().find(|p| p.id == r.port).map(|p| p.kind).ok_or_else(|| anyhow!("{} has no port {}", r.object, r.port))
    }
    fn obstacles(&self, extra: &[Obstacle]) -> Vec<Obstacle> {
        let mut v: Vec<Obstacle> = self
            .items
            .iter()
            .filter_map(|i| {
                let s = i.spec?;
                let (min, max) = world_box(&i.transform, spec_bounds(&s));
                Some(Obstacle { name: i.name.clone(), min, max })
            })
            .collect();
        v.extend(extra.iter().map(|o| Obstacle { name: o.name.clone(), min: o.min, max: o.max }));
        v
    }
    /// Routes a pipe between two item ports.
    pub(crate) fn pipe(&mut self, from: &PortRef, to: &PortRef, d: f32, extra: &[Obstacle], names: &mut Namer) -> Result<f32> {
        let (p0, d0, _) = self.port(from)?;
        let (p1, d1, _) = self.port(to)?;
        let obstacles = self.obstacles(extra);
        let mut nm = |k: &str| names.name(k);
        let r = route::plan((from, p0, d0), (to, p1, d1), d, &[], &obstacles, &mut nm).map_err(|e| anyhow!("{} → {}: {e}", from.label(), to.label()))?;
        for p in r.pieces {
            self.add(p.name, p.spec, p.transform)?;
        }
        self.links.extend(r.links);
        Ok(r.length)
    }
    /// Moves the whole assembly: rotate by `yaw` about the origin, then translate.
    fn place(&mut self, at: Vec3, yaw: f32) {
        let m = Transform::from_translation(at).with_rotation(Quat::from_rotation_y(yaw));
        for it in &mut self.items {
            it.transform = m * it.transform;
        }
    }
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

/// Two tanks, a pump and three valves: the pump lifts liquid from tank A
/// into the top of tank B, and B drains back to A by gravity through the
/// open return line. The discharge valve is throttled to 50° so the pump
/// delivers what the return can carry: the levels settle (about 30 % / 70 %)
/// and the loop circulates steadily, about 2.4 l/s of water.
pub fn pump_loop(names: &mut Namer, at: Vec3, yaw: f32, d: f32, tank_dia: f32, tank_h: f32, extra: &[Obstacle]) -> Result<Assembly> {
    let mut a = Assembly::default();
    let tank = Spec::Tank { dia: tank_dia, h: tank_h, d };
    let gap = (tank_dia * 0.5 + 1.6).max(2.0);
    let ta = a.add(names.name("tank_a"), tank, Transform::IDENTITY)?;
    let tb = a.add(names.name("tank_b"), tank, Transform::from_xyz(gap * 2.0, 0.0, 0.0))?;
    // Pump in the middle, suction facing tank A, on a plinth that puts its
    // suction at the tank outlet's height: a straight, self-venting suction line.
    let Spec::Tank { dia, h, d: dn } = tank.normalized() else { unreachable!() };
    let base = (super::parts::tank_nozzle_y(dia, h, dn) - super::parts::pump_axis(dn)).max(0.0);
    let pump = a.add(names.name("pump"), Spec::Pump { d, motor: true, base },Transform::from_xyz(gap * 0.9, 0.0, 0.0).with_rotation(Quat::from_rotation_y(std::f32::consts::PI)))?;
    let v1 = a.attach(names.name("valve_suction"), Spec::Valve { d }, "a", &PortRef::new(&ta, "outlet"), Some(Vec3::X))?;
    let v2 = a.attach(names.name("valve_discharge"), Spec::Valve { d }, "a", &PortRef::new(&pump, "outlet"), Some(Vec3::Y))?;
    let v3 = a.attach(names.name("valve_return"), Spec::Valve { d }, "a", &PortRef::new(&tb, "return"), Some(-Vec3::X))?;
    let l1 = a.pipe(&PortRef::new(&v1, "b"), &PortRef::new(&pump, "inlet"), d, extra, names)?;
    let l2 = a.pipe(&PortRef::new(&v2, "b"), &PortRef::new(&tb, "inlet"), d, extra, names)?;
    let l3 = a.pipe(&PortRef::new(&v3, "b"), &PortRef::new(&ta, "return"), d, extra, names)?;
    a.controls.push((pump.clone(), Control { on: true, ..default() }));
    a.controls.push((ta.clone(), Control { fill: Some(0.85), medium: Some("water".into()), ..default() }));
    a.controls.push((tb.clone(), Control { fill: Some(0.15), medium: Some("water".into()), ..default() }));
    a.levers.push((v2.clone(), 50.0));
    a.place(at, yaw);
    a.summary = json!({
        "tanks": [ta, tb], "pump": pump, "valves": { "suction": v1, "discharge (50° throttled)": v2, "return": v3 },
        "pipe_m": r2(l1 + l2 + l3),
    });
    Ok(a)
}

/// An inline engine driving a coolant pump by belt (engine pulley → pump
/// pulley), a generator coupled to the flywheel, and a coolant tank piped
/// to the pump.
pub fn engine_drive(names: &mut Namer, at: Vec3, yaw: f32, cylinders: u32, extra: &[Obstacle]) -> Result<Assembly> {
    let mut a = Assembly::default();
    let d = 0.04;
    let engine = a.add(names.name("engine"), Spec::Engine { n: cylinders }, Transform::IDENTITY)?;
    let (_, eb) = spec_bounds(&Spec::Engine { n: cylinders }.normalized());
    let (eng_pulley, _, eng_dia) = a.port(&PortRef::new(&engine, "pulley"))?;
    // Driven pump beside the engine, its pulley in the engine pulley's plane.
    let pump_spec = Spec::Pump { d, motor: false, base: 0.0 };
    let mut pump_t = Transform::from_xyz(0.0, 0.0, eb.z + 0.45);
    let pulley_spec = Spec::Pulley { dia: 0.08, d: 0.044 * (d / 0.05_f32).clamp(0.6, 4.0) };
    let pump = a.add(names.name("coolant_pump"), pump_spec, pump_t)?;
    let pulley = a.attach(names.name("pump_pulley"), pulley_spec, "shaft", &PortRef::new(&pump, "shaft"), None)?;
    let (pp, _, _) = a.port(&PortRef::new(&pulley, "belt"))?;
    let shift = Vec3::new(eng_pulley.x - pp.x, 0.0, 0.0);
    for it in a.items.iter_mut().filter(|i| i.name == pump || i.name == pulley) {
        it.transform.translation += shift;
    }
    pump_t.translation += shift;
    a.links.push(Link { a: PortRef::new(&engine, "pulley"), b: PortRef::new(&pulley, "belt"), kind: LinkKind::Belt });
    // Generator on the flywheel.
    let gen_name = a.attach(names.name("generator"), Spec::Motor { d: 0.04 }, "shaft", &PortRef::new(&engine, "flywheel"), Some(Vec3::Y))?;
    // Upright again: snap with yaw only (the motor stands on its feet).
    {
        let (fp, fd, _) = a.port(&PortRef::new(&engine, "flywheel"))?;
        let spec = Spec::Motor { d: 0.04 }.normalized();
        let ports = spec.ports();
        let t = connect::snap(fp, fd, LinkKind::Shaft, &ports[0], None, &Transform::default(), Turn::Yaw).map_err(|e| anyhow!(e))?;
        if let Some(it) = a.items.iter_mut().find(|i| i.name == gen_name) {
            it.transform = t;
        }
    }
    // Coolant tank in front of the pump suction.
    let (inlet, _, _) = a.port(&PortRef::new(&pump, "inlet"))?;
    let tank_spec = Spec::Tank { dia: 0.5, h: 0.6, d };
    let tank = a.add(names.name("coolant_tank"), tank_spec, Transform::from_xyz(inlet.x + 1.3, 0.0, pump_t.translation.z).with_rotation(Quat::from_rotation_y(std::f32::consts::PI)))?;
    let l1 = a.pipe(&PortRef::new(&tank, "outlet"), &PortRef::new(&pump, "inlet"), d, extra, names)?;
    let l2 = a.pipe(&PortRef::new(&pump, "outlet"), &PortRef::new(&tank, "inlet"), d, extra, names)?;
    a.controls.push((engine.clone(), Control { on: true, rpm: Some(900.0), ..default() }));
    a.controls.push((tank.clone(), Control { fill: Some(0.7), medium: Some("coolant".into()), ..default() }));
    a.place(at, yaw);
    let ratio = eng_dia / pulley_spec.normalized().ports().iter().find(|p| p.id == "belt").map(|p| p.size).unwrap_or(0.08);
    a.summary = json!({
        "engine": engine, "pump": pump, "pump_pulley": pulley, "generator": gen_name, "tank": tank,
        "belt_ratio": r2(ratio), "pump_rpm_at_900": (900.0 * ratio).round(), "pipe_m": r2(l1 + l2),
    });
    Ok(a)
}

/// `count` roller conveyors end to end; `boxes` items start on the first ones.
pub fn conveyor_line(names: &mut Namer, at: Vec3, yaw: f32, count: usize, len: f32, w: f32, h: f32, boxes: usize, speed: f32) -> Result<Assembly> {
    let mut a = Assembly::default();
    let spec = Spec::Conveyor { len, w, h }.normalized();
    let mut prev: Option<String> = None;
    let mut first = String::new();
    for _ in 0..count.clamp(1, 30) {
        let n = match &prev {
            None => {
                let n = a.add(names.name("conveyor"), spec, Transform::IDENTITY)?;
                first = n.clone();
                n
            }
            Some(p) => {
                let target = PortRef::new(p, "out");
                let (tp, td, _) = a.port(&target)?;
                let ports = spec.ports();
                let t = connect::snap(tp, td, LinkKind::Conveyor, &ports[0], None, &Transform::default(), Turn::Yaw).map_err(|e| anyhow!(e))?;
                let n = a.add(names.name("conveyor"), spec, t)?;
                a.links.push(Link { a: target, b: PortRef::new(&n, "in"), kind: LinkKind::Conveyor });
                n
            }
        };
        a.controls.push((n.clone(), Control { on: true, speed_mps: Some(speed), ..default() }));
        prev = Some(n);
    }
    let (start, _, _) = a.port(&PortRef::new(&first, "in"))?;
    let size = Vec3::new(0.3, 0.25, (w * 0.6).min(0.4));
    let block = crate::agent::structures::usd_for(crate::agent::structures::Kind::Block, size)?;
    for i in 0..boxes {
        let x = start.x + 0.25 + i as f32 * 0.55;
        a.items.push(Item { name: names.name("box"), usd: block.clone(), transform: Transform::from_xyz(x, start.y, start.z), spec: None });
    }
    a.place(at, yaw);
    a.summary = json!({ "conveyors": count, "length_m": r2(len * count as f32), "speed_mps": speed, "boxes": boxes });
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_links(a: &Assembly) {
        for l in &a.links {
            let (Ok(pa), Ok(pb)) = (a.port(&l.a), a.port(&l.b)) else { continue };
            let (err, ang) = connect::misalignment(l.kind, (pa.0, pa.1), (pb.0, pb.1));
            let tol = if l.kind == LinkKind::Belt { 0.01 } else { 0.004 };
            assert!(err < tol && (ang < 2.0 || l.kind == LinkKind::Belt && ang > 178.0 || ang < 2.0), "{} – {}: {err:.4} m, {ang:.2}°", l.a.label(), l.b.label());
        }
    }

    #[test]
    fn pump_loop_is_fully_connected() {
        let mut n = Namer::new("t", HashSet::new());
        let a = pump_loop(&mut n, Vec3::new(1.0, 0.0, 2.0), 0.3, 0.05, 1.0, 1.2, &[]).unwrap();
        check_links(&a);
        // Every pipe port of every fitting, valve and the pump is linked.
        for it in &a.items {
            let Some(s) = it.spec else { continue };
            if matches!(s, Spec::Tank { .. }) {
                continue;
            }
            for p in s.ports().iter().filter(|p| p.kind == crate::plant::ports::PortKind::Pipe) {
                let r = PortRef::new(&it.name, &p.id);
                assert!(a.links.iter().any(|l| l.a == r || l.b == r), "{} open", r.label());
            }
        }
    }

    #[test]
    fn engine_drive_and_conveyors_line_up() {
        let mut n = Namer::new("", HashSet::new());
        let a = engine_drive(&mut n, Vec3::ZERO, 0.0, 4, &[]).unwrap();
        check_links(&a);
        let a = conveyor_line(&mut n, Vec3::new(0.0, 0.0, 5.0), 1.0, 3, 2.0, 0.6, 0.8, 2, 0.4).unwrap();
        check_links(&a);
        assert_eq!(a.links.len(), 2);
    }
}
