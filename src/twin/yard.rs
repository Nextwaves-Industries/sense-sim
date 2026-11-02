//! Where devices stand until someone places them.
//!
//! A network scan can't say where a device physically is, so unplaced devices wait in a
//! "yard" beside the scene, grouped by the vantage point that sees them. The UI labels it as
//! not physical. Every device keeps a slot even once placed, so dragging one out doesn't
//! shuffle the others.

use std::collections::{BTreeMap, HashMap};

use bevy::math::{Vec2, Vec3};
use bevy::prelude::Resource;
use twin_proto::model::{Device, DeviceId};

use super::Placement;

const SPACING: f32 = 1.6;
const MIN_PER_ROW: usize = 8;
const GROUP_GAP: f32 = 4.0;
/// Sensors sit around their IO-Link master rather than taking a slot of their own.
const CHILD_RADIUS: f32 = 0.55;

pub struct YardGroup {
    pub label: String,
    pub count: usize,
    pub min: Vec2,
    pub max: Vec2,
}

#[derive(Resource, Default)]
pub struct Yard {
    slots: HashMap<DeviceId, Vec2>,
    pub groups: Vec<YardGroup>,
    /// The camera should go and look at the devices.
    pub frame_pending: bool,
}

impl Yard {
    pub fn build<'a>(devices: impl Iterator<Item = &'a Device>, origin: Vec2) -> Self {
        let mut by_vantage: BTreeMap<&str, Vec<&Device>> = BTreeMap::new();
        for device in devices.filter(|d| d.parent.is_none()) {
            let vantage = device.addresses.first().map_or("Not seen yet", |a| a.vantage.as_str());
            by_vantage.entry(vantage).or_default().push(device);
        }
        let total: usize = by_vantage.values().map(Vec::len).sum();
        // Roughly square overall, so a big plant doesn't become one long line.
        let shelf_width = ((total as f32).sqrt() * SPACING * 2.0).max(SPACING * MIN_PER_ROW as f32);

        let mut yard = Yard::default();
        let (mut x, mut z, mut shelf_depth) = (origin.x, origin.y, 0.0f32);
        for (vantage, mut members) in by_vantage {
            members.sort_by(|a, b| a.class.cmp(&b.class).then_with(|| a.name().cmp(b.name())));
            // Each group is a roughly square block too, however many it holds.
            let per_row = ((members.len() as f32).sqrt().ceil() as usize).max(MIN_PER_ROW);
            let rows = members.len().div_ceil(per_row);
            let width = members.len().min(per_row) as f32 * SPACING;
            let depth = rows as f32 * SPACING;
            if x + width > origin.x + shelf_width && x > origin.x {
                x = origin.x;
                z += shelf_depth + GROUP_GAP;
                shelf_depth = 0.0;
            }
            for (i, device) in members.iter().enumerate() {
                let (col, row) = (i % per_row, i / per_row);
                yard.slots.insert(device.id.clone(), Vec2::new(x + (col as f32 + 0.5) * SPACING, z + (row as f32 + 0.5) * SPACING));
            }
            yard.groups.push(YardGroup { label: vantage.to_string(), count: members.len(), min: Vec2::new(x, z), max: Vec2::new(x + width, z + depth) });
            x += width + GROUP_GAP;
            shelf_depth = shelf_depth.max(depth);
        }
        yard
    }

    /// The floor point of a device: where it was placed, else beside its parent, else its slot.
    pub fn ground(&self, device: &Device, placed: &BTreeMap<DeviceId, Placement>) -> Option<Vec3> {
        if let Some(place) = placed.get(&device.id) {
            return Some(Vec3::from(place.position));
        }
        if let Some(parent) = &device.parent {
            let base = match placed.get(&parent.device) {
                Some(place) => Vec3::from(place.position),
                None => floor(*self.slots.get(&parent.device)?),
            };
            let angle = parent.port as f32 * std::f32::consts::FRAC_PI_2;
            return Some(base + Vec3::new(angle.cos(), 0.0, angle.sin()) * CHILD_RADIUS);
        }
        self.slots.get(&device.id).copied().map(floor)
    }

}

fn floor(p: Vec2) -> Vec3 {
    Vec3::new(p.x, 0.0, p.y)
}

#[cfg(test)]
mod tests {
    use twin_proto::demo::DemoFactory;
    use twin_proto::model::DeviceClass;

    use super::*;

    fn yard_for(count: usize) -> (DemoFactory, Yard) {
        let factory = DemoFactory::new(count);
        let yard = Yard::build(factory.inventory().devices(), Vec2::ZERO);
        (factory, yard)
    }

    #[test]
    fn every_device_gets_a_position_and_no_two_top_level_devices_share_one() {
        let (factory, yard) = yard_for(5000);
        let none = BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for device in factory.inventory().devices() {
            let p = yard.ground(device, &none).unwrap_or_else(|| panic!("{} has no position", device.name()));
            if device.parent.is_none() {
                assert!(seen.insert((p.x.to_bits(), p.z.to_bits())), "{} overlaps another device", device.name());
            }
        }
    }

    #[test]
    fn placing_a_device_does_not_move_the_others() {
        let (factory, yard) = yard_for(100);
        let devices: Vec<_> = factory.inventory().devices().collect();
        let before: Vec<_> = devices.iter().map(|d| yard.ground(d, &BTreeMap::new())).collect();

        let mut placed = BTreeMap::new();
        placed.insert(devices[0].id.clone(), Placement { position: [50.0, 0.0, 50.0] });
        assert_eq!(yard.ground(devices[0], &placed), Some(Vec3::new(50.0, 0.0, 50.0)));
        for (device, was) in devices.iter().zip(&before).skip(1) {
            if device.parent.as_ref().is_none_or(|p| p.device != devices[0].id) {
                assert_eq!(yard.ground(device, &placed), *was, "{}", device.name());
            }
        }
    }

    #[test]
    fn a_sensor_stays_with_its_master_until_placed_itself() {
        let (factory, yard) = yard_for(100);
        let sensor = factory.inventory().devices().find(|d| d.class == DeviceClass::Sensor).unwrap();
        let master = sensor.parent.as_ref().unwrap().device.clone();

        let mut placed = BTreeMap::new();
        placed.insert(master, Placement { position: [20.0, 0.0, 30.0] });
        let beside = yard.ground(sensor, &placed).unwrap();
        assert!(beside.distance(Vec3::new(20.0, 0.0, 30.0)) < 1.0);

        placed.insert(sensor.id.clone(), Placement { position: [5.0, 0.0, 5.0] });
        assert_eq!(yard.ground(sensor, &placed), Some(Vec3::new(5.0, 0.0, 5.0)));
    }

    #[test]
    fn the_yard_starts_at_its_origin_and_stays_roughly_square() {
        let factory = DemoFactory::new(5000);
        let yard = Yard::build(factory.inventory().devices(), Vec2::new(100.0, -20.0));
        let min = yard.groups.iter().map(|g| g.min).reduce(Vec2::min).unwrap();
        let max = yard.groups.iter().map(|g| g.max).reduce(Vec2::max).unwrap();
        assert_eq!(min, Vec2::new(100.0, -20.0));
        let size = max - min;
        assert!(size.x / size.y < 6.0 && size.y / size.x < 6.0, "{size:?}");
    }
}
