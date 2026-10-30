//! OpenEMS acting on the scene: actuator N (0..100 %) read back from
//! `rtls0/ActuatorN` drives the moving parts (doors, gates, shutters,
//! drawers) of every object that sits in zone N, zones sorted by name, the
//! same order as `rtls0/ZoneNOccupancy`. The rule itself lives in OpenEMS
//! (the RTLS bundle opens while the zone is occupied unless `Automation`=0),
//! so the loop is RTLS → OpenEMS → scene.

use bevy::prelude::*;

use super::RtlsState;
use super::device::Zone;
use crate::scene::PlacedObject;

/// (zone number 1..8, zone name, object entity, object name) for every
/// object with moving parts that lies in a zone.
pub fn bindings(zones: &[Zone], objects: &[(Entity, String, Vec3, bool)]) -> Vec<(u8, String, Entity, String)> {
    let mut zs: Vec<&Zone> = zones.iter().collect();
    zs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = Vec::new();
    for (k, z) in zs.iter().take(8).enumerate() {
        for (e, name, center, has_joints) in objects {
            if !*has_joints {
                continue;
            }
            let m = 1.0;
            let inside = center.x >= z.min[0] - m && center.x <= z.max[0] + m && center.z >= z.min[2] - m && center.z <= z.max[2] + m;
            if inside {
                out.push((k as u8 + 1, z.name.clone(), *e, name.clone()));
            }
        }
    }
    out
}

pub fn object_list(objects: &Query<(Entity, &mut PlacedObject, &GlobalTransform)>) -> Vec<(Entity, String, Vec3, bool)> {
    objects
        .iter()
        .map(|(e, o, gt)| {
            let (a, b) = o.aabb();
            (e, o.name.clone(), gt.transform_point((a + b) * 0.5), !o.proto.joints.is_empty())
        })
        .collect()
}

/// Objects OpenEMS can move right now: (actuator, zone, object).
#[derive(Resource, Default)]
pub struct EmsBindings(pub Vec<(u8, String, String)>);

/// Moves bound objects' joints toward the actuator position (smoothly).
pub fn apply(
    state: Res<RtlsState>,
    zones: Query<&Zone>,
    mut objects: Query<(Entity, &mut PlacedObject, &GlobalTransform)>,
    time: Res<Time>,
    mut shown: ResMut<EmsBindings>,
) {
    let zl: Vec<Zone> = zones.iter().cloned().collect();
    let list = object_list(&objects);
    let b = bindings(&zl, &list);
    let summary: Vec<(u8, String, String)> = b.iter().map(|(k, z, _, n)| (*k, z.clone(), n.clone())).collect();
    if shown.0 != summary {
        shown.0 = summary;
    }
    if state.actuators.iter().all(Option::is_none) {
        return;
    }
    let rate = (time.delta_secs() * 0.8).min(1.0);
    for (k, _, e, _) in b {
        let Some(pct) = state.actuators.get(k as usize - 1).copied().flatten() else { continue };
        let Ok((_, mut obj, _)) = objects.get_mut(e) else { continue };
        let proto = obj.proto.clone();
        for (i, j) in proto.joints.iter().enumerate() {
            let (lo, hi) = j.range(proto.unit);
            let target = lo + (hi - lo) * (pct / 100.0).clamp(0.0, 1.0);
            if let Some(cur) = obj.joints.get(i).copied() {
                let next = cur + (target - cur) * rate.max(0.05);
                if (next - cur).abs() > 1e-4 {
                    obj.joints[i] = if (target - next).abs() < (hi - lo).abs() * 0.002 { target } else { next };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_bind_to_their_zone_in_name_order() {
        let z = |name: &str, x: f32| Zone { name: name.into(), min: [x, 0.0, 0.0], max: [x + 4.0, 2.5, 4.0], restricted: false, capacity: 0, auto: false };
        let zones = vec![z("Dock", 10.0), z("Area 1", 0.0)];
        let objs = vec![
            (Entity::from_raw_u32(1).unwrap(), "door_1".to_string(), Vec3::new(2.0, 1.0, 2.0), true),
            (Entity::from_raw_u32(2).unwrap(), "pallet".to_string(), Vec3::new(2.5, 0.2, 2.0), false),
            (Entity::from_raw_u32(3).unwrap(), "gate".to_string(), Vec3::new(14.5, 1.0, 1.0), true),
        ];
        let b = bindings(&zones, &objs);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].0, b[0].1.as_str(), b[0].3.as_str()), (1, "Area 1", "door_1"));
        assert_eq!((b[1].0, b[1].1.as_str(), b[1].3.as_str()), (2, "Dock", "gate"));
    }
}
