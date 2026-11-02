//! Links between ports, and the snap that moves an object so one of its
//! ports mates with a port of another object.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::ports::{Port, PortKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// Flange to flange: liquid flows through.
    Pipe,
    /// Coaxial shaft coupling: same speed.
    Shaft,
    /// Belt between two pulleys: speed scales with the diameter ratio.
    Belt,
    /// Conveyor end to the next conveyor's start: items move across.
    Conveyor,
}

impl LinkKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::Shaft => "shaft",
            Self::Belt => "belt",
            Self::Conveyor => "conveyor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct PortRef {
    pub object: String,
    pub port: String,
}

impl PortRef {
    pub fn new(object: &str, port: &str) -> Self {
        Self { object: object.into(), port: port.into() }
    }
    /// `"object.port"` (object names may contain dots: the port is after the last one).
    pub fn parse(s: &str) -> Option<Self> {
        let (o, p) = s.rsplit_once('.')?;
        (!o.is_empty() && !p.is_empty()).then(|| Self::new(o, p))
    }
    pub fn label(&self) -> String {
        format!("{}.{}", self.object, self.port)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub a: PortRef,
    pub b: PortRef,
    pub kind: LinkKind,
}

impl Link {
    pub fn other(&self, p: &PortRef) -> Option<&PortRef> {
        if &self.a == p {
            Some(&self.b)
        } else if &self.b == p {
            Some(&self.a)
        } else {
            None
        }
    }
    pub fn touches(&self, object: &str) -> bool {
        self.a.object == object || self.b.object == object
    }
}

#[derive(Resource, Default, Clone)]
pub struct Links(pub Vec<Link>);

impl Links {
    pub fn partner(&self, p: &PortRef) -> Option<(&Link, &PortRef)> {
        self.0.iter().find_map(|l| l.other(p).map(|o| (l, o)))
    }
    /// Adds a link; a port mates with one other port, so links already using
    /// either end are replaced.
    pub fn add(&mut self, link: Link) {
        self.0.retain(|l| l.other(&link.a).is_none() && l.other(&link.b).is_none());
        self.0.push(link);
    }
    pub fn remove_port(&mut self, p: &PortRef) -> usize {
        let n = self.0.len();
        self.0.retain(|l| l.other(p).is_none());
        n - self.0.len()
    }
    pub fn remove_object(&mut self, object: &str) -> usize {
        let n = self.0.len();
        self.0.retain(|l| !l.touches(object));
        n - self.0.len()
    }
    pub fn rename(&mut self, old: &str, new: &str) {
        for l in &mut self.0 {
            for p in [&mut l.a, &mut l.b] {
                if p.object == old {
                    p.object = new.to_string();
                }
            }
        }
    }
}

/// Heading (radians about +Y) that turns +X onto the horizontal part of `v`.
fn heading(v: Vec3) -> f32 {
    (-v.z).atan2(v.x)
}

/// The direction the mating port must face for a link of `kind`, given the
/// target port's world direction.
pub fn wanted_dir(kind: LinkKind, target_dir: Vec3) -> Vec3 {
    match kind {
        // Conveyors continue in the same travel direction.
        LinkKind::Conveyor => target_dir,
        _ => -target_dir,
    }
}

/// How the snapped object may turn.
#[derive(Clone, Copy, Debug)]
pub enum Turn {
    /// Upright equipment: yaw only (keeps standing on the floor).
    Yaw,
    /// Piping: any rotation; `aim` (world) is where the part's other port
    /// should point, when it has one.
    Free { aim: Option<Vec3> },
}

/// Transform that puts `mine` (a port of the object, local) onto the target
/// port (world position and direction), keeping the object's scale.
pub fn snap(target_pos: Vec3, target_dir: Vec3, kind: LinkKind, mine: &Port, other: Option<&Port>, current: &Transform, turn: Turn) -> Result<Transform, String> {
    let want = wanted_dir(kind, target_dir).normalize_or(Vec3::X);
    let local = mine.dir.normalize_or(Vec3::X);
    let rotation = match turn {
        Turn::Yaw => {
            let (lv, wv) = (local.y, want.y);
            if lv.abs() > 0.95 && wv.abs() > 0.95 {
                if lv.signum() != wv.signum() {
                    return Err(format!(
                        "port '{}' points {} but must point {}: an upright object can't flip over; connect the other way round or add an elbow",
                        mine.id,
                        if lv > 0.0 { "up" } else { "down" },
                        if wv > 0.0 { "up" } else { "down" }
                    ));
                }
                // Vertical mate: any yaw works; keep the current one.
                current.rotation
            } else if lv.abs() < 0.2 && wv.abs() < 0.2 {
                Quat::from_rotation_y(heading(want) - heading(local))
            } else {
                return Err(format!(
                    "port '{}' is {} but the mate needs it {}: add an elbow between them (plant_add_part kind \"elbow\") or route a pipe (plant_route_pipe)",
                    mine.id,
                    if lv.abs() > 0.5 { "vertical" } else { "horizontal" },
                    if wv.abs() > 0.5 { "vertical" } else { "horizontal" }
                ));
            }
        }
        Turn::Free { aim } => {
            let r0 = Quat::from_rotation_arc(local, want);
            match (aim, other) {
                (Some(aim), Some(o)) => {
                    // Roll about the mating axis so the other port faces `aim`.
                    let v = (r0 * o.dir).reject_from_normalized(want);
                    let w = aim.reject_from_normalized(want);
                    if v.length() < 1e-3 || w.length() < 1e-3 {
                        r0
                    } else {
                        let (v, w) = (v.normalize(), w.normalize());
                        let ang = v.cross(w).dot(want).atan2(v.dot(w));
                        Quat::from_axis_angle(want, ang) * r0
                    }
                }
                _ => r0,
            }
        }
    };
    let translation = target_pos - rotation * (mine.pos * current.scale);
    Ok(Transform { translation, rotation, scale: current.scale })
}

/// Position (m) and angle (degrees) error between two linked ports.
pub fn misalignment(kind: LinkKind, a: (Vec3, Vec3), b: (Vec3, Vec3)) -> (f32, f32) {
    match kind {
        LinkKind::Belt => {
            // Pulleys: parallel axes, same belt plane.
            let ang = a.1.dot(b.1).abs().clamp(-1.0, 1.0).acos().to_degrees();
            let plane = (b.0 - a.0).dot(a.1).abs();
            (plane, ang)
        }
        _ => {
            let want = wanted_dir(kind, a.1);
            let ang = want.dot(b.1).clamp(-1.0, 1.0).acos().to_degrees();
            ((a.0 - b.0).length(), ang)
        }
    }
}

/// Whether two ports can be linked, and the kind of link.
pub fn link_kind(a: &Port, b: &Port) -> Result<LinkKind, String> {
    let k = PortKind::mates(a.kind, b.kind).ok_or_else(|| format!("a {} port can't connect to a {} port", a.kind.name(), b.kind.name()))?;
    if k == LinkKind::Pipe {
        let (lo, hi) = (a.size.min(b.size), a.size.max(b.size));
        if hi > lo * 1.15 + 0.002 {
            return Err(format!(
                "pipe sizes differ ({:.0} mm vs {:.0} mm): put a reducer between them (plant_add_part kind \"reducer\" d1/d2)",
                a.size * 1000.0,
                b.size * 1000.0
            ));
        }
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plant::ports::PortSource;

    fn port(id: &str, pos: Vec3, dir: Vec3) -> Port {
        Port::new(id, PortKind::Pipe, pos, dir, 0.1, PortSource::Generated)
    }

    #[test]
    fn yaw_snap_puts_ports_face_to_face() {
        let mine = port("a", Vec3::new(-1.0, 0.3, 0.0), -Vec3::X);
        let target = (Vec3::new(5.0, 0.3, 2.0), Vec3::Z);
        let t = snap(target.0, target.1, LinkKind::Pipe, &mine, None, &Transform::default(), Turn::Yaw).unwrap();
        let (p, d) = mine.world(&t);
        assert!((p - target.0).length() < 1e-4, "{p}");
        assert!((d + target.1).length() < 1e-4, "{d}");
        let (e, a) = misalignment(LinkKind::Pipe, target, (p, d));
        assert!(e < 1e-4 && a < 0.1);
    }

    #[test]
    fn free_snap_rolls_towards_aim() {
        // Elbow-like part: a faces -X, b faces +Y.
        let a = port("a", Vec3::ZERO, -Vec3::X);
        let b = port("b", Vec3::new(0.15, 0.15, 0.0), Vec3::Y);
        let t = snap(Vec3::new(1.0, 1.0, 1.0), Vec3::X, LinkKind::Pipe, &a, Some(&b), &Transform::default(), Turn::Free { aim: Some(Vec3::Z) }).unwrap();
        let (_, bd) = b.world(&t);
        assert!((bd - Vec3::Z).length() < 1e-3, "{bd}");
        let (ap, _) = a.world(&t);
        assert!((ap - Vec3::ONE).length() < 1e-4);
    }

    #[test]
    fn upright_objects_refuse_impossible_mates() {
        let up = port("o", Vec3::new(0.0, 1.0, 0.0), Vec3::Y);
        assert!(snap(Vec3::ZERO, Vec3::X, LinkKind::Pipe, &up, None, &Transform::default(), Turn::Yaw).is_err());
        let r = PortRef::parse("tank.v2.outlet").unwrap();
        assert_eq!((r.object.as_str(), r.port.as_str()), ("tank.v2", "outlet"));
    }
}
