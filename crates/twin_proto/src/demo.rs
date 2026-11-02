//! An invented factory for trying the app and for scale tests.
//!
//! None of this exists. Replies are flagged `demo`, station names start with `DEMO`, serials
//! start with `DEMO`, and addresses sit in 198.18.0.0/15, a range reserved for benchmarking
//! that no real device uses. It changes state on its own so the live views have something
//! to show.

use crate::inventory::{Cursor, Inventory, SyncReply};
use crate::model::{Address, Coverage, Device, DeviceClass, DeviceId, EventKind, Flow, Identity, Initiator, Line, LineId, Liveness, Machine, NewEvent, Parent, Severity};

/// Fixed so a restarted demo reads as a replaced inventory.
const DEMO_EPOCH: u64 = 0x0DE0_0DE0;
/// How much plant time one [`DemoFactory::advance`] stands for.
const STEP_MS: u32 = 1000;
/// Parts that fit between two machines. A failure takes this long to reach its neighbours.
const BUFFER: u32 = 8;
const SHIFT_MS: u32 = 8 * 60 * 60 * 1000;
/// Each kind of starting fault hits one device in this many.
const FAULT_EVERY: usize = 97;
/// Once one device in this many is in trouble, the demo only works through what's already wrong.
/// Left running, it stays a plant that mostly works instead of slowly falling apart.
const TROUBLED_ONE_IN: usize = 32;

pub struct DemoFactory {
    inventory: Inventory,
    step: u64,
    pace: Vec<LinePace>,
}

/// The working of a line, which only the demo knows. A real line would just report results.
struct LinePace {
    line: LineId,
    /// How long each machine takes for a part.
    cycle_ms: Vec<u32>,
    /// How far each machine is into its current part.
    worked_ms: Vec<u32>,
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
        let mut pace = Vec::new();
        let mut line = 0usize;
        let mut made = 0usize;
        while made < device_count {
            let (devices, built) = build_line(line, &mut made, device_count);
            for device in devices {
                inventory.upsert(device);
            }
            if let Some((built, line_pace)) = built {
                inventory.upsert_line(built);
                pace.push(line_pace);
            }
            line += 1;
        }
        let mut factory = Self { inventory, step: 0, pace };
        factory.seed_faults();
        // No time passes, but the lines should already show what those faults do to them.
        factory.run_lines(0);
        factory
    }

    /// Moves every line on: a machine works if its devices let it, it has a part, and there's
    /// room for what it finishes. From the last machine back, so room opens up before the
    /// machine behind looks for it.
    fn run_lines(&mut self, elapsed_ms: u32) {
        let mut pace = std::mem::take(&mut self.pace);
        for line_pace in &mut pace {
            let Some(mut line) = self.inventory.line(&line_pace.line).cloned() else { continue };
            let halted = line.control.iter().any(|id| self.inventory.get(id).is_some_and(|d| d.liveness == Liveness::Down || d.mode.as_deref() == Some("stop")));
            let last = line.machines.len() - 1;
            for i in (0..=last).rev() {
                let down = line.machines[i].devices.iter().any(|id| self.inventory.get(id).is_some_and(|d| d.liveness == Liveness::Down));
                let made = line.machines[i].made;
                let flow = if halted || down {
                    Flow::Stopped
                } else if i > 0 && line.machines[i - 1].made <= made {
                    Flow::Starved
                } else if i < last && made - line.machines[i + 1].made >= BUFFER {
                    Flow::Blocked
                } else {
                    Flow::Running
                };
                line.machines[i].flow = flow;
                if flow != Flow::Running {
                    continue;
                }
                line_pace.worked_ms[i] += elapsed_ms;
                if line_pace.worked_ms[i] >= line_pace.cycle_ms[i] {
                    line_pace.worked_ms[i] -= line_pace.cycle_ms[i];
                    line.machines[i].made += 1;
                    line.machines[i].cycle_ms = Some(wobble(line_pace.cycle_ms[i], line.machines[i].made));
                }
            }
            self.inventory.upsert_line(line);
        }
        self.pace = pace;
    }

    /// Starts with a few problems showing, so every status colour is visible right away. These
    /// land on machines of different lines, so even the smallest demo shows each state of a line.
    fn seed_faults(&mut self) {
        let ids: Vec<DeviceId> = self.inventory.devices().map(|d| d.id.clone()).collect();
        for (i, id) in ids.iter().enumerate() {
            let liveness = match i % FAULT_EVERY {
                3 => Liveness::Down,
                22 => Liveness::Stale,
                48 => Liveness::Suspect,
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
        self.change_a_device(now_ms);
        // After the device, so a line never reports what its devices were doing a step ago.
        self.run_lines(STEP_MS);
    }

    fn change_a_device(&mut self, now_ms: u64) {
        let ids: Vec<DeviceId> = self.inventory.devices().map(|d| d.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let troubled: Vec<DeviceId> = self.inventory.devices().filter(|d| d.liveness != Liveness::Up).map(|d| d.id.clone()).collect();
        let pool = if troubled.len() * TROUBLED_ONE_IN > ids.len() { &troubled } else { &ids };
        let id = &pool[(self.step as usize).wrapping_mul(7919) % pool.len()];
        let Some(device) = self.inventory.get(id) else { return };
        match device.liveness {
            Liveness::Up if self.step.is_multiple_of(3) => self.set_liveness(id, Liveness::Suspect, now_ms),
            Liveness::Suspect => self.set_liveness(id, Liveness::Down, now_ms),
            Liveness::Down | Liveness::Stale => self.set_liveness(id, Liveness::Up, now_ms),
            // Now and then, so mode changes don't crowd out everything else.
            _ if self.step.is_multiple_of(11) => self.flip_a_plc(now_ms),
            _ => {}
        }
    }

    /// Stops a PLC, or restarts the one that's stopped: a stopped PLC stops its whole line, so
    /// one at a time is plenty.
    fn flip_a_plc(&mut self, now_ms: u64) {
        let plcs: Vec<DeviceId> = self.inventory.devices().filter(|d| d.mode.is_some()).map(|d| d.id.clone()).collect();
        if plcs.is_empty() {
            return;
        }
        let stopped = plcs.iter().find(|id| self.inventory.get(id).is_some_and(|d| d.mode.as_deref() == Some("stop")));
        let id = stopped.unwrap_or(&plcs[self.step as usize % plcs.len()]);
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

/// A measured cycle is never quite the same twice. Small enough that a line never drifts across
/// its target, so lines don't jump around a list sorted by how they're doing.
fn wobble(cycle_ms: u32, made: u32) -> u32 {
    let percent = 99 + made % 3;
    cycle_ms / 100 * percent
}

/// The devices of one production line, and the line they form if enough of them fit under
/// `limit`. Sensors hang off their IO-Link master rather than having addresses, which is how
/// they're really reached.
fn build_line(line: usize, made: &mut usize, limit: usize) -> (Vec<Device>, Option<(Line, LinePace)>) {
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
    let control = [add(plc, format!("DEMO-L{label}-PLC"), None), add(&SWITCH, format!("DEMO-L{label}-SW"), None)];
    add(&HMI, format!("DEMO-L{label}-HMI"), None);
    let infeed_drive = add(&DRIVE, format!("DEMO-L{label}-DRV1"), None);
    let packing_drive = add(&DRIVE, format!("DEMO-L{label}-DRV2"), None);
    let mut sensors = [None, None, None, None];
    if let Some(master) = add(&MASTER, format!("DEMO-L{label}-IOL"), None) {
        for port in 1..=4 {
            sensors[port as usize - 1] = add(&SENSOR, format!("DEMO-L{label}-S{port}"), Some(Parent { device: master.clone(), port }));
        }
    }
    let [infeed_sensor, inspection_a, inspection_b, packing_sensor] = sensors;
    let cnc = add(&CNC, format!("DEMO-L{label}-CNC"), None);
    let robot = add(&ROBOT, format!("DEMO-L{label}-ROB"), None);
    match line % 5 {
        1 => {
            add(&METER, format!("DEMO-L{label}-MTR"), None);
        }
        2 => {
            add(&IPC, format!("DEMO-L{label}-IPC"), None);
        }
        4 => {
            add(&GATEWAY, format!("DEMO-L{label}-GW"), None);
        }
        _ => {}
    }

    let stages = [
        ("Infeed", vec![infeed_drive, infeed_sensor]),
        ("Machining", vec![cnc]),
        ("Assembly", vec![robot]),
        ("Inspection", vec![inspection_a, inspection_b]),
        ("Packing", vec![packing_drive, packing_sensor]),
    ];
    let line_built = demo_line(line, control.into_iter().flatten().collect(), stages);
    (devices, line_built)
}

/// Lays the stages out as a line partway through a shift, leaving out any stage whose devices
/// didn't fit. Every fourth line has a machine too slow for its plan.
fn demo_line(line: usize, control: Vec<DeviceId>, stages: [(&str, Vec<Option<DeviceId>>); 5]) -> Option<(Line, LinePace)> {
    let stages: Vec<(&str, Vec<DeviceId>)> = stages.into_iter().map(|(name, devices)| (name, devices.into_iter().flatten().collect::<Vec<_>>())).filter(|(_, devices)| !devices.is_empty()).collect();
    if stages.len() < 2 {
        return None;
    }

    let target_cycle_ms = (10 + line as u32 % 5 * 5) * 1000;
    let plan = SHIFT_MS / target_cycle_ms;
    let slow_stage = (line % 4 == 2).then_some(line % stages.len());
    let cycle_ms: Vec<u32> = (0..stages.len()).map(|i| if slow_stage == Some(i) { target_cycle_ms / 100 * 115 } else { target_cycle_ms / 100 * (60 + (line as u32 + i as u32 * 7) % 33) }).collect();

    // Counting back from the end of the line, because each machine has made at least as many
    // as the one after it.
    let mut made = plan / 100 * (25 + (line as u32 * 37) % 60);
    let mut machines: Vec<Machine> = Vec::with_capacity(stages.len());
    for (i, (name, devices)) in stages.into_iter().enumerate().rev() {
        machines.push(Machine { name: name.to_string(), devices, flow: Flow::Running, made, cycle_ms: Some(cycle_ms[i]) });
        made += (line as u32 + i as u32 * 3) % (BUFFER / 2);
    }
    machines.reverse();

    let id = LineId(format!("line_demo{line:04}"));
    let worked_ms = vec![0; machines.len()];
    let built = Line { id: id.clone(), name: format!("DEMO Line {}", line + 1), control, machines, plan, target_cycle_ms, rev: 0 };
    Some((built, LinePace { line: id, cycle_ms, worked_ms }))
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
        for line in factory.inventory().lines() {
            assert!(line.name.starts_with("DEMO"), "{}", line.name);
        }
    }

    fn first_healthy_line(factory: &DemoFactory) -> Line {
        factory
            .inventory()
            .lines()
            .find(|line| {
                let mut devices = line.control.iter().chain(line.machines.iter().flat_map(|m| &m.devices));
                line.machines.len() == 5 && !line.is_slow() && devices.all(|id| factory.inventory().get(id).is_some_and(|d| d.liveness == Liveness::Up && d.mode.as_deref() != Some("stop")))
            })
            .expect("a demo this size has a healthy line")
            .clone()
    }

    fn flows(factory: &DemoFactory, id: &LineId) -> Vec<Flow> {
        factory.inventory().line(id).unwrap().machines.iter().map(|m| m.flow).collect()
    }

    /// How many steps a healthy line needs to run out of what its buffers can hide.
    const LONG_ENOUGH: usize = 2000;

    #[test]
    fn a_line_is_machines_in_order_made_of_devices_that_exist() {
        let factory = DemoFactory::new(500);
        let lines: Vec<_> = factory.inventory().lines().collect();
        assert!(lines.len() > 10);
        let order = ["Infeed", "Machining", "Assembly", "Inspection", "Packing"];
        let last = lines.len() - 1;
        for (i, line) in lines.into_iter().enumerate() {
            let names: Vec<&str> = line.machines.iter().map(|m| m.name.as_str()).collect();
            // The device limit can cut the last line short, but what's left stays in order.
            if i < last {
                assert_eq!(names, order, "{}", line.name);
            } else {
                assert!(names.is_sorted_by_key(|name| order.iter().position(|o| o == name)), "{names:?}");
            }
            for id in line.control.iter().chain(line.machines.iter().flat_map(|m| &m.devices)) {
                assert!(factory.inventory().get(id).is_some(), "{id:?} isn't in the inventory");
            }
        }
    }

    #[test]
    fn a_demo_too_small_for_a_line_has_none_and_does_not_panic() {
        for count in [0, 1, 3] {
            let mut factory = DemoFactory::new(count);
            assert_eq!(factory.inventory().lines().count(), 0, "{count} devices");
            factory.advance(1000);
        }
        // Cut short partway through a line: the stages that fit still form one.
        let mut factory = DemoFactory::new(8);
        factory.advance(1000);
        for line in factory.inventory().lines() {
            assert!(line.machines.len() >= 2 && line.machines.iter().all(|m| !m.devices.is_empty()), "{line:?}");
        }
    }

    #[test]
    fn parts_never_outrun_the_machine_before_and_never_overflow_a_buffer() {
        let mut factory = DemoFactory::new(300);
        for second in 1..=LONG_ENOUGH {
            factory.advance(second as u64 * 1000);
            for line in factory.inventory().lines() {
                for pair in line.machines.windows(2) {
                    assert!(pair[0].made >= pair[1].made, "{} at {second} s", line.name);
                    assert!(pair[0].made - pair[1].made <= BUFFER, "{} at {second} s", line.name);
                }
            }
        }
    }

    #[test]
    fn a_healthy_line_keeps_producing() {
        let mut factory = DemoFactory::new(300);
        let line = first_healthy_line(&factory);
        let before = line.made();
        for _ in 0..LONG_ENOUGH {
            factory.run_lines(STEP_MS);
        }
        let after = factory.inventory().line(&line.id).unwrap();
        assert!(after.made() > before, "{} made nothing", line.name);
    }

    #[test]
    fn a_machine_going_down_blocks_what_is_before_it_and_starves_what_is_after_it() {
        let mut factory = DemoFactory::new(300);
        let line = first_healthy_line(&factory);
        let broken = line.machines[2].devices[0].clone();
        factory.set_liveness(&broken, Liveness::Down, 0);
        for _ in 0..LONG_ENOUGH {
            factory.run_lines(STEP_MS);
        }
        assert_eq!(flows(&factory, &line.id), [Flow::Blocked, Flow::Blocked, Flow::Stopped, Flow::Starved, Flow::Starved]);

        // And it all drains and refills once the machine is back.
        let stuck = factory.inventory().line(&line.id).unwrap().made();
        factory.set_liveness(&broken, Liveness::Up, 0);
        for _ in 0..LONG_ENOUGH {
            factory.run_lines(STEP_MS);
        }
        assert!(factory.inventory().line(&line.id).unwrap().made() > stuck);
    }

    #[test]
    fn the_line_stops_without_its_plc_whether_it_is_down_or_switched_to_stop() {
        let mut factory = DemoFactory::new(300);
        let line = first_healthy_line(&factory);
        let plc = line.control[0].clone();

        factory.set_liveness(&plc, Liveness::Down, 0);
        factory.run_lines(STEP_MS);
        assert!(flows(&factory, &line.id).iter().all(|f| *f == Flow::Stopped));
        factory.set_liveness(&plc, Liveness::Up, 0);
        factory.run_lines(STEP_MS);
        assert!(flows(&factory, &line.id).iter().all(|f| *f != Flow::Stopped));

        if factory.inventory().get(&plc).unwrap().mode.is_some() {
            let mut stopped = factory.inventory().get(&plc).unwrap().clone();
            stopped.mode = Some("stop".into());
            factory.inventory.upsert(stopped);
            factory.run_lines(STEP_MS);
            assert!(flows(&factory, &line.id).iter().all(|f| *f == Flow::Stopped));
        }
    }

    #[test]
    fn some_lines_are_too_slow_for_their_plan_and_stay_that_way() {
        let mut factory = DemoFactory::new(500);
        let slow = |factory: &DemoFactory| factory.inventory().lines().filter(|l| l.machines.iter().all(|m| m.cycle_ms.is_some()) && l.machines.iter().any(|m| m.cycle_ms.unwrap() > l.target_cycle_ms)).map(|l| l.id.clone()).collect::<BTreeSet<_>>();
        let at_start = slow(&factory);
        assert!(!at_start.is_empty());
        assert!(at_start.len() < factory.inventory().lines().count());
        for _ in 0..LONG_ENOUGH {
            factory.run_lines(STEP_MS);
        }
        assert_eq!(slow(&factory), at_start);
    }

    #[test]
    fn a_line_never_reports_what_its_devices_were_doing_a_step_ago() {
        let mut factory = DemoFactory::new(200);
        for second in 1..=1500 {
            factory.advance(second * 1000);
            let inventory = factory.inventory();
            let down = |id: &DeviceId| inventory.get(id).is_some_and(|d| d.liveness == Liveness::Down);
            let stop = |id: &DeviceId| inventory.get(id).is_some_and(|d| d.mode.as_deref() == Some("stop"));
            for line in inventory.lines() {
                let halted = line.control.iter().any(|id| down(id) || stop(id));
                for machine in &line.machines {
                    let should_stop = halted || machine.devices.iter().any(down);
                    assert_eq!(machine.flow == Flow::Stopped, should_stop, "{} {} at {second} s", line.name, machine.name);
                }
            }
        }
    }

    #[test]
    fn a_demo_left_running_stays_a_plant_that_mostly_works() {
        for (count, least_percent) in [(60, 30), (500, 50)] {
            let mut factory = DemoFactory::new(count);
            for second in 1..=4000 {
                factory.advance(second * 1000);
                if second % 100 == 0 {
                    let lines = factory.inventory().lines().count();
                    let working = factory.inventory().lines().filter(|l| l.machines.iter().all(|m| m.flow != Flow::Stopped)).count();
                    assert!(working * 100 >= lines * least_percent, "{working} of {lines} lines work after {second} s with {count} devices");
                }
            }
        }
    }

    #[test]
    fn at_most_one_plc_is_stopped_at_a_time() {
        let mut factory = DemoFactory::new(500);
        for second in 1..=4000 {
            factory.advance(second * 1000);
            let stopped = factory.inventory().devices().filter(|d| d.mode.as_deref() == Some("stop")).count();
            assert!(stopped <= 1, "{stopped} PLCs stopped at {second} s");
        }
    }

    #[test]
    fn line_counts_changing_do_not_look_like_devices_changing() {
        let mut factory = DemoFactory::new(300);
        let start = factory.inventory().cursor();
        for _ in 0..100 {
            factory.run_lines(STEP_MS);
        }
        let end = factory.inventory().cursor();
        assert_eq!(end.rev, start.rev);
        assert!(end.line_rev > start.line_rev);
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
        assert_eq!(mirror.lines.len(), factory.inventory().lines().count());
        for line in factory.inventory().lines() {
            assert_eq!(mirror.lines[&line.id], *line);
        }
    }
}
