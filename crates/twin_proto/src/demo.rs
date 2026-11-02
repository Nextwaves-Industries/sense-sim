//! An invented factory for trying the app and for scale tests.
//!
//! None of this exists. Replies are flagged `demo`, station names start with `DEMO`, serials
//! start with `DEMO`, and addresses sit in 198.18.0.0/15, a range reserved for benchmarking
//! that no real device uses. It changes state on its own so the live views have something
//! to show.

use crate::inventory::{Cursor, Inventory, SyncReply};
use crate::model::{Address, Coverage, Device, DeviceClass, DeviceId, EventKind, Identity, Initiator, Liveness, NewEvent, Parent, Severity};

/// Fixed so a restarted demo reads as a replaced inventory.
const DEMO_EPOCH: u64 = 0x0DE0_0DE0;

pub struct DemoFactory {
    inventory: Inventory,
    step: u64,
}

struct Make {
    vendor: &'static str,
    model: &'static str,
    order_number: Option<&'static str>,
    firmware: &'static str,
    class: DeviceClass,
    coverage: Coverage,
    mode: Option<&'static str>,
}

const fn make(vendor: &'static str, model: &'static str, order_number: Option<&'static str>, firmware: &'static str, class: DeviceClass, coverage: Coverage, mode: Option<&'static str>) -> Make {
    Make { vendor, model, order_number, firmware, class, coverage, mode }
}

const PLC_S7: Make = make("Siemens", "S7-1516", Some("6ES7 516-3AN02-0AB0"), "V3.1", DeviceClass::Plc, Coverage::Controlled, Some("run"));
const PLC_ET: Make = make("Siemens", "ET 200SP CPU", Some("6ES7 512-1DK01-0AB0"), "V3.0", DeviceClass::Plc, Coverage::Monitored, Some("run"));
const PLC_AB: Make = make("Rockwell", "CompactLogix 5380", None, "33.011", DeviceClass::Plc, Coverage::Identified, None);
const HMI: Make = make("Siemens", "KTP700 Basic", Some("6AV2 123-2GB03-0AX0"), "V17", DeviceClass::Hmi, Coverage::Identified, None);
const SWITCH: Make = make("Siemens", "SCALANCE XC208", Some("6GK5 208-0BA00-2AC2"), "V4.3", DeviceClass::Switch, Coverage::Identified, None);
const DRIVE: Make = make("Siemens", "SINAMICS G120", Some("6SL3 244-0BB12-1FA0"), "V4.7", DeviceClass::Drive, Coverage::Identified, None);
const MASTER: Make = make("ifm", "AL1350", None, "2.4", DeviceClass::IoLinkMaster, Coverage::Identified, None);
const SENSOR: Make = make("ifm", "O5D100", None, "1.2", DeviceClass::Sensor, Coverage::Identified, None);
const ROBOT: Make = make("KUKA", "KR 10", None, "8.6", DeviceClass::Robot, Coverage::Counted, None);
const CNC: Make = make("DMG MORI", "NLX 2500", None, "", DeviceClass::Cnc, Coverage::Counted, None);
const METER: Make = make("Schneider", "PM5560", None, "2.7", DeviceClass::Meter, Coverage::Identified, None);
const IPC: Make = make("Siemens", "IPC427E", None, "Windows 10", DeviceClass::Pc, Coverage::Identified, None);
const GATEWAY: Make = make("Moxa", "MGate 5103", None, "1.3", DeviceClass::Gateway, Coverage::Identified, None);

impl DemoFactory {
    pub fn new(device_count: usize) -> Self {
        let mut inventory = Inventory::new(DEMO_EPOCH);
        let mut line = 0usize;
        let mut made = 0usize;
        while made < device_count {
            for device in build_line(line, &mut made, device_count) {
                inventory.upsert(device);
            }
            line += 1;
        }
        let mut factory = Self { inventory, step: 0 };
        factory.seed_faults();
        factory
    }

    /// Starts with a few problems showing, so every status colour is visible right away.
    fn seed_faults(&mut self) {
        let ids: Vec<DeviceId> = self.inventory.devices().map(|d| d.id.clone()).collect();
        for (i, id) in ids.iter().enumerate() {
            let liveness = match i % 23 {
                5 => Liveness::Down,
                11 => Liveness::Suspect,
                17 => Liveness::Stale,
                _ => continue,
            };
            self.set_liveness(id, liveness, 0);
        }
    }

    fn set_liveness(&mut self, id: &DeviceId, liveness: Liveness, now_ms: u64) {
        let Some(mut device) = self.inventory.get(id).cloned() else { return };
        let before = device.liveness;
        device.liveness = liveness;
        if liveness == Liveness::Up {
            device.last_seen_ms = Some(now_ms);
        }
        self.inventory.upsert(device);
        let kind = match (before, liveness) {
            (_, Liveness::Down) => (EventKind::Down, Severity::Critical),
            (Liveness::Down, Liveness::Up) => (EventKind::Recovered, Severity::Info),
            _ => return,
        };
        let mut event = NewEvent::new(now_ms, kind.0, kind.1, Some(id.clone()));
        event.before = Some(before.label().to_string());
        event.after = Some(liveness.label().to_string());
        self.inventory.record(event);
    }

    /// Moves the demo on one step. Call about once a second.
    pub fn advance(&mut self, now_ms: u64) {
        self.step += 1;
        let ids: Vec<DeviceId> = self.inventory.devices().map(|d| d.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let id = &ids[(self.step as usize).wrapping_mul(7919) % ids.len()];
        let Some(device) = self.inventory.get(id) else { return };
        match device.liveness {
            Liveness::Up if self.step % 3 == 0 => self.set_liveness(id, Liveness::Suspect, now_ms),
            Liveness::Suspect => self.set_liveness(id, Liveness::Down, now_ms),
            Liveness::Down | Liveness::Stale => self.set_liveness(id, Liveness::Up, now_ms),
            _ => self.flip_a_plc(now_ms),
        }
    }

    fn flip_a_plc(&mut self, now_ms: u64) {
        let plcs: Vec<DeviceId> = self.inventory.devices().filter(|d| d.mode.is_some()).map(|d| d.id.clone()).collect();
        if plcs.is_empty() {
            return;
        }
        let id = &plcs[self.step as usize % plcs.len()];
        let Some(mut device) = self.inventory.get(id).cloned() else { return };
        let before = device.mode.clone();
        let after = if before.as_deref() == Some("run") { "stop" } else { "run" };
        device.mode = Some(after.to_string());
        self.inventory.upsert(device);
        let mut event = NewEvent::new(now_ms, EventKind::ModeChanged { initiator: Initiator::External }, Severity::Warning, Some(id.clone()));
        event.before = before;
        event.after = Some(after.to_string());
        self.inventory.record(event);
    }

    pub fn sync(&self, since: Option<Cursor>) -> SyncReply {
        self.inventory.sync(since, true)
    }

    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }
}

/// One production line. Sensors hang off their IO-Link master rather than having addresses,
/// which is how they're really reached.
fn build_line(line: usize, made: &mut usize, limit: usize) -> Vec<Device> {
    let label = line + 1;
    let vantage = if line % 3 == 2 { format!("collector-line-{label}") } else { "scanner".to_string() };
    let mut devices = Vec::new();

    let mut add = |make: &Make, tag: String, parent: Option<Parent>| -> Option<DeviceId> {
        if *made >= limit {
            return None;
        }
        let n = *made;
        *made += 1;
        let id = DeviceId(format!("dev_demo{n:06}"));
        let address = parent.is_none().then(|| Address {
            vantage: vantage.clone(),
            ip: Some(format!("198.18.{}.{}", n / 250, n % 250 + 1)),
            mac: Some(format!("02:00:5e:00:{:02x}:{:02x}", (n >> 8) & 0xff, n & 0xff)),
        });
        devices.push(Device {
            id: id.clone(),
            class: make.class,
            liveness: Liveness::Up,
            coverage: make.coverage,
            identity: Identity {
                vendor: Some(make.vendor.to_string()),
                model: Some(make.model.to_string()),
                order_number: make.order_number.map(str::to_string),
                serial: Some(format!("DEMO{n:06}")),
                firmware: (!make.firmware.is_empty()).then(|| make.firmware.to_string()),
                station_name: Some(tag),
            },
            addresses: address.into_iter().collect(),
            mode: make.mode.map(str::to_string),
            parent,
            last_seen_ms: None,
            rev: 0,
        });
        Some(id)
    };

    let plc = if line % 4 == 3 { &PLC_AB } else if line % 2 == 1 { &PLC_ET } else { &PLC_S7 };
    add(plc, format!("DEMO-L{label}-PLC"), None);
    add(&SWITCH, format!("DEMO-L{label}-SW"), None);
    add(&HMI, format!("DEMO-L{label}-HMI"), None);
    add(&DRIVE, format!("DEMO-L{label}-DRV1"), None);
    add(&DRIVE, format!("DEMO-L{label}-DRV2"), None);
    if let Some(master) = add(&MASTER, format!("DEMO-L{label}-IOL"), None) {
        for port in 1..=4 {
            add(&SENSOR, format!("DEMO-L{label}-S{port}"), Some(Parent { device: master.clone(), port }));
        }
    }
    match line % 5 {
        0 => {
            add(&ROBOT, format!("DEMO-L{label}-ROB"), None);
        }
        1 => {
            add(&METER, format!("DEMO-L{label}-MTR"), None);
        }
        2 => {
            add(&IPC, format!("DEMO-L{label}-IPC"), None);
        }
        3 => {
            add(&CNC, format!("DEMO-L{label}-CNC"), None);
        }
        _ => {
            add(&GATEWAY, format!("DEMO-L{label}-GW"), None);
        }
    }
    devices
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::inventory::Mirror;

    #[test]
    fn it_builds_exactly_as_many_devices_as_asked() {
        for count in [0, 1, 10, 137, 5000] {
            assert_eq!(DemoFactory::new(count).inventory().len(), count);
        }
    }

    #[test]
    fn nothing_could_be_mistaken_for_a_real_device() {
        let factory = DemoFactory::new(300);
        assert!(factory.sync(None).demo);
        for device in factory.inventory().devices() {
            assert!(device.name().starts_with("DEMO"), "{}", device.name());
            assert!(device.identity.serial.as_deref().unwrap().starts_with("DEMO"));
            for address in &device.addresses {
                let ip = address.ip.as_deref().unwrap();
                assert!(ip.starts_with("198.18.") || ip.starts_with("198.19."), "{ip}");
            }
        }
    }

    #[test]
    fn ids_and_serials_are_unique() {
        let factory = DemoFactory::new(5000);
        let devices: Vec<_> = factory.inventory().devices().collect();
        assert_eq!(devices.iter().map(|d| &d.id).collect::<BTreeSet<_>>().len(), 5000);
        assert_eq!(devices.iter().map(|d| &d.identity.serial).collect::<BTreeSet<_>>().len(), 5000);
    }

    #[test]
    fn sensors_are_reached_through_their_master_and_have_no_address() {
        let factory = DemoFactory::new(200);
        let sensors: Vec<_> = factory.inventory().devices().filter(|d| d.class == DeviceClass::Sensor).collect();
        assert!(!sensors.is_empty());
        for sensor in sensors {
            let parent = sensor.parent.as_ref().expect("a sensor has a master");
            assert!(sensor.addresses.is_empty());
            assert_eq!(factory.inventory().get(&parent.device).unwrap().class, DeviceClass::IoLinkMaster);
        }
    }

    #[test]
    fn a_fresh_demo_shows_healthy_and_failing_devices() {
        let factory = DemoFactory::new(200);
        let states: BTreeSet<Liveness> = factory.inventory().devices().map(|d| d.liveness).collect();
        for expected in [Liveness::Up, Liveness::Suspect, Liveness::Down, Liveness::Stale] {
            assert!(states.contains(&expected), "{expected:?}");
        }
    }

    #[test]
    fn it_keeps_changing_and_a_mirror_stays_identical() {
        let mut factory = DemoFactory::new(200);
        let mut mirror = Mirror::default();
        mirror.apply(factory.sync(None));
        let start = mirror.cursor;
        for second in 1..=120 {
            factory.advance(second * 1000);
            mirror.apply(factory.sync(mirror.cursor));
        }
        assert_ne!(mirror.cursor, start, "the demo should change on its own");
        assert!(!mirror.events.is_empty());
        for device in factory.inventory().devices() {
            assert_eq!(mirror.devices[&device.id], *device);
        }
    }
}
