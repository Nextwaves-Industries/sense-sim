//! The device list and recent events, plus the cheap way to keep a copy of them current.
//!
//! The service owns an [`Inventory`]; each viewer holds a [`Mirror`]. A viewer polls with
//! the [`Cursor`] it last saw and receives only what changed, so thousands of devices don't
//! cross the pipe every second. Devices are never deleted, only marked `Retired`, so a
//! change list never has to say what disappeared.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::model::{Device, DeviceId, Event, NewEvent};

/// Events the service keeps in memory for viewers. The full history is on disk.
pub const RECENT_EVENTS: usize = 5000;
/// Most events sent in one reply. A viewer catches up over a few polls.
pub const MAX_EVENTS_PER_SYNC: usize = 500;

/// Marks how far a viewer has read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Changes if the inventory was replaced, which makes revisions incomparable.
    pub epoch: u64,
    pub rev: u64,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReply {
    pub cursor: Cursor,
    /// Discard what you have: `changed` holds every device.
    pub full: bool,
    pub changed: Vec<Device>,
    pub events: Vec<Event>,
    /// This is invented data for trying the app. Always show that plainly.
    pub demo: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inventory {
    epoch: u64,
    rev: u64,
    seq: u64,
    devices: BTreeMap<DeviceId, Device>,
    #[serde(skip)]
    recent: VecDeque<Event>,
}

impl Inventory {
    pub fn new(epoch: u64) -> Self {
        Self { epoch, rev: 0, seq: 0, devices: BTreeMap::new(), recent: VecDeque::new() }
    }

    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.values()
    }

    pub fn get(&self, id: &DeviceId) -> Option<&Device> {
        self.devices.get(id)
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn cursor(&self) -> Cursor {
        Cursor { epoch: self.epoch, rev: self.rev, seq: self.seq }
    }

    /// Adds or replaces a device. Writing identical data doesn't count as a change, so a
    /// repeated scan result stays off the wire.
    pub fn upsert(&mut self, mut device: Device) {
        if let Some(existing) = self.devices.get(&device.id) {
            device.rev = existing.rev;
            if *existing == device {
                return;
            }
        }
        self.rev += 1;
        device.rev = self.rev;
        self.devices.insert(device.id.clone(), device);
    }

    pub fn record(&mut self, new: NewEvent) -> Event {
        self.seq += 1;
        let event = Event {
            seq: self.seq,
            ts_ms: new.ts_ms,
            kind: new.kind,
            severity: new.severity,
            device_id: new.device_id,
            vantage: new.vantage,
            before: new.before,
            after: new.after,
            correlation_id: new.correlation_id,
            untrusted: new.untrusted,
        };
        self.remember(event.clone());
        event
    }

    /// Puts back an event read from the journal after a restart.
    pub fn remember(&mut self, event: Event) {
        self.seq = self.seq.max(event.seq);
        self.recent.push_back(event);
        while self.recent.len() > RECENT_EVENTS {
            self.recent.pop_front();
        }
    }

    pub fn sync(&self, since: Option<Cursor>, demo: bool) -> SyncReply {
        let now = self.cursor();
        let usable = since.filter(|c| c.epoch == now.epoch && c.rev <= now.rev && c.seq <= now.seq);
        let full = usable.is_none();
        let (after_rev, after_seq) = usable.map_or((0, 0), |c| (c.rev, c.seq));

        let changed = self.devices.values().filter(|d| d.rev > after_rev).cloned().collect();
        let events: Vec<Event> = if full {
            // A new viewer only needs the latest; the full history is on disk.
            self.recent.iter().skip(self.recent.len().saturating_sub(MAX_EVENTS_PER_SYNC)).cloned().collect()
        } else {
            // Oldest first, so a viewer that fell behind misses nothing.
            self.recent.iter().filter(|e| e.seq > after_seq).take(MAX_EVENTS_PER_SYNC).cloned().collect()
        };
        // Only claim to have read as far as what this reply actually contains.
        let seq = if !full && events.len() == MAX_EVENTS_PER_SYNC { events.last().map_or(now.seq, |e| e.seq) } else { now.seq };
        SyncReply { cursor: Cursor { seq, ..now }, full, changed, events, demo }
    }
}

/// A viewer's copy of the service's inventory.
#[derive(Debug, Default)]
pub struct Mirror {
    pub cursor: Option<Cursor>,
    pub devices: BTreeMap<DeviceId, Device>,
    /// Newest last.
    pub events: VecDeque<Event>,
    pub demo: bool,
}

impl Mirror {
    pub fn apply(&mut self, reply: SyncReply) {
        if reply.full {
            self.devices.clear();
            self.events.clear();
        }
        for device in reply.changed {
            self.devices.insert(device.id.clone(), device);
        }
        for event in reply.events {
            self.events.push_back(event);
        }
        while self.events.len() > RECENT_EVENTS {
            self.events.pop_front();
        }
        self.cursor = Some(reply.cursor);
        self.demo = reply.demo;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Coverage, DeviceClass, EventKind, Liveness, Severity};

    fn device(id: &str, liveness: Liveness) -> Device {
        Device {
            id: DeviceId(id.into()),
            class: DeviceClass::Plc,
            liveness,
            coverage: Coverage::Counted,
            identity: Default::default(),
            addresses: Vec::new(),
            mode: None,
            parent: None,
            last_seen_ms: None,
            rev: 0,
        }
    }

    fn event(ts_ms: u64) -> NewEvent {
        NewEvent::new(ts_ms, EventKind::Down, Severity::Critical, None)
    }

    #[test]
    fn a_new_viewer_gets_everything_then_only_changes() {
        let mut inventory = Inventory::new(1);
        inventory.upsert(device("a", Liveness::Up));
        inventory.upsert(device("b", Liveness::Up));

        let mut mirror = Mirror::default();
        let first = inventory.sync(mirror.cursor, false);
        assert!(first.full);
        assert_eq!(first.changed.len(), 2);
        mirror.apply(first);

        let quiet = inventory.sync(mirror.cursor, false);
        assert!(!quiet.full && quiet.changed.is_empty() && quiet.events.is_empty());

        inventory.upsert(device("a", Liveness::Down));
        inventory.record(event(5));
        let update = inventory.sync(mirror.cursor, false);
        assert!(!update.full);
        assert_eq!(update.changed.len(), 1);
        assert_eq!(update.events.len(), 1);
        mirror.apply(update);

        assert_eq!(mirror.devices.len(), 2);
        assert_eq!(mirror.devices[&DeviceId("a".into())].liveness, Liveness::Down);
    }

    #[test]
    fn writing_identical_data_is_not_a_change() {
        let mut inventory = Inventory::new(1);
        inventory.upsert(device("a", Liveness::Up));
        let cursor = inventory.cursor();
        inventory.upsert(device("a", Liveness::Up));
        assert_eq!(inventory.cursor(), cursor);
    }

    #[test]
    fn a_replaced_or_rewound_inventory_forces_a_full_reply() {
        let mut inventory = Inventory::new(2);
        inventory.upsert(device("a", Liveness::Up));
        let other_epoch = Cursor { epoch: 1, rev: 0, seq: 0 };
        let from_the_future = Cursor { epoch: 2, rev: 99, seq: 0 };
        assert!(inventory.sync(Some(other_epoch), false).full);
        assert!(inventory.sync(Some(from_the_future), false).full);
    }

    #[test]
    fn a_full_reply_replaces_what_a_viewer_held() {
        let mut mirror = Mirror::default();
        let mut old = Inventory::new(1);
        old.upsert(device("gone", Liveness::Up));
        mirror.apply(old.sync(None, false));

        let mut new = Inventory::new(2);
        new.upsert(device("kept", Liveness::Up));
        mirror.apply(new.sync(mirror.cursor, false));

        assert_eq!(mirror.devices.keys().map(|k| k.0.as_str()).collect::<Vec<_>>(), ["kept"]);
    }

    #[test]
    fn a_viewer_far_behind_catches_up_without_losing_events() {
        let mut inventory = Inventory::new(1);
        let mut mirror = Mirror::default();
        mirror.apply(inventory.sync(None, false));
        for i in 0..1200 {
            inventory.record(event(i));
        }
        let mut seen = Vec::new();
        for _ in 0..10 {
            let reply = inventory.sync(mirror.cursor, false);
            seen.extend(reply.events.iter().map(|e| e.seq));
            mirror.apply(reply);
        }
        assert_eq!(seen.len(), 1200);
    }

    #[test]
    fn a_new_viewer_starts_from_the_latest_events() {
        let mut inventory = Inventory::new(1);
        for i in 0..1200 {
            inventory.record(event(i));
        }
        let reply = inventory.sync(None, false);
        assert_eq!(reply.events.len(), MAX_EVENTS_PER_SYNC);
        assert_eq!(reply.events.last().unwrap().seq, 1200);
        assert_eq!(reply.cursor.seq, 1200);
    }

    #[test]
    fn restored_events_continue_the_numbering() {
        let mut first = Inventory::new(1);
        let saved = first.record(event(1));
        let mut restarted = Inventory::new(1);
        restarted.remember(saved.clone());
        assert_eq!(restarted.record(event(2)).seq, saved.seq + 1);
    }

    #[test]
    fn the_demo_flag_reaches_the_viewer() {
        let mut mirror = Mirror::default();
        mirror.apply(Inventory::new(1).sync(None, true));
        assert!(mirror.demo);
    }
}
