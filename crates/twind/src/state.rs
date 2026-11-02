//! One thread owns the inventory. Everything else asks it through a channel, so there are
//! no locks to get wrong and a reader always sees a whole change or none of it.
//!
//! Later milestones feed it with [`StateHandle::apply`], and read with
//! [`StateHandle::read`] when a decision depends on what's already known.

use std::io;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, never, select, tick};
use twin_proto::demo::DemoFactory;
use twin_proto::inventory::{Cursor, Inventory, SyncReply};
use twin_proto::model::{Device, Event, NewEvent};

use crate::store::{Journal, Opened, Snapshots};
use crate::{clock, say};

const DEMO_STEP: Duration = Duration::from_secs(1);
const SAVE_RETRY: Duration = Duration::from_secs(5);

/// Applied together, so a viewer never sees an event without the device change behind it.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    pub devices: Vec<Device>,
    pub events: Vec<NewEvent>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StateError {
    /// The service is shutting down.
    Stopped,
    /// Demo data can't be changed from outside.
    Demo,
    /// The events couldn't be saved, so none of the changes were made. Safe to try again.
    Journal(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("the service is stopping"),
            Self::Demo => f.write_str("demo data can't be changed"),
            Self::Journal(why) => write!(f, "couldn't save the events: {why}"),
        }
    }
}

enum Command {
    Sync { since: Option<Cursor>, reply: Sender<SyncReply> },
    Apply { changes: ChangeSet, reply: Sender<Result<Vec<Event>, StateError>> },
    Read(Box<dyn FnOnce(&Inventory) + Send>),
    Stop,
}

#[derive(Clone)]
pub struct StateHandle {
    commands: Sender<Command>,
}

impl StateHandle {
    pub fn sync(&self, since: Option<Cursor>) -> Result<SyncReply, StateError> {
        let (reply, answer) = bounded(1);
        self.commands.send(Command::Sync { since, reply }).map_err(|_| StateError::Stopped)?;
        answer.recv().map_err(|_| StateError::Stopped)
    }

    /// Saves any events, then makes everything visible at once. Returns the numbered events.
    #[must_use = "a refused change was not made"]
    pub fn apply(&self, changes: ChangeSet) -> Result<Vec<Event>, StateError> {
        let (reply, answer) = bounded(1);
        self.commands.send(Command::Apply { changes, reply }).map_err(|_| StateError::Stopped)?;
        answer.recv().map_err(|_| StateError::Stopped)?
    }

    #[must_use = "a refused change was not made"]
    pub fn upsert(&self, device: Device) -> Result<(), StateError> {
        self.apply(ChangeSet { devices: vec![device], events: Vec::new() }).map(|_| ())
    }

    #[must_use = "a refused event was not recorded"]
    pub fn record(&self, event: NewEvent) -> Result<Event, StateError> {
        let mut events = self.apply(ChangeSet { devices: Vec::new(), events: vec![event] })?;
        events.pop().ok_or(StateError::Stopped)
    }

    /// Runs `look` on the owner thread, so keep it short.
    pub fn read<R: Send + 'static>(&self, look: impl FnOnce(&Inventory) -> R + Send + 'static) -> Result<R, StateError> {
        let (reply, answer) = bounded(1);
        let task = Box::new(move |inventory: &Inventory| {
            let _ = reply.send(look(inventory));
        });
        self.commands.send(Command::Read(task)).map_err(|_| StateError::Stopped)?;
        answer.recv().map_err(|_| StateError::Stopped)
    }
}

enum Persist {
    Changed,
    Final(Box<Inventory>),
}

enum Backing {
    Stored { inventory: Inventory, journal: Journal, persist: Sender<Persist> },
    Demo(DemoFactory),
}

impl Backing {
    fn handle(&mut self, command: Command) {
        match command {
            Command::Sync { since, reply } => {
                let answer = match self {
                    Self::Stored { inventory, .. } => inventory.sync(since, false),
                    Self::Demo(factory) => factory.sync(since),
                };
                let _ = reply.send(answer);
            }
            Command::Apply { changes, reply } => {
                let _ = reply.send(self.apply(changes));
            }
            Command::Read(task) => match self {
                Self::Stored { inventory, .. } => task(inventory),
                Self::Demo(factory) => task(factory.inventory()),
            },
            Command::Stop => unreachable!("handled by the owner loop"),
        }
    }

    fn apply(&mut self, changes: ChangeSet) -> Result<Vec<Event>, StateError> {
        let Self::Stored { inventory, journal, persist } = self else {
            return Err(StateError::Demo);
        };
        let first = inventory.cursor().seq.max(journal.attempted()) + 1;
        let events: Vec<Event> = changes.events.into_iter().zip(first..).map(|(new, seq)| numbered(seq, new)).collect();

        // On disk before it's visible, so a number a viewer has seen can't be handed out again
        // after a crash. If the save fails nothing has changed, and the caller can retry.
        if !events.is_empty() {
            journal.append(&events).map_err(|e| StateError::Journal(e.to_string()))?;
        }
        for device in changes.devices {
            inventory.upsert(device);
        }
        for event in &events {
            inventory.remember(event.clone());
        }
        // Full means a save is already due, and that save will include this.
        let _ = persist.try_send(Persist::Changed);
        Ok(events)
    }

    fn advance_demo(&mut self) {
        if let Self::Demo(factory) = self {
            factory.advance(clock::now_ms());
        }
    }

    fn finish(self) {
        if let Self::Stored { inventory, persist, .. } = self {
            let _ = persist.send(Persist::Final(Box::new(inventory)));
        }
    }
}

fn numbered(seq: u64, new: NewEvent) -> Event {
    Event { seq, ts_ms: new.ts_ms, kind: new.kind, severity: new.severity, device_id: new.device_id, vantage: new.vantage, before: new.before, after: new.after, correlation_id: new.correlation_id, untrusted: new.untrusted }
}

fn run_owner(mut backing: Backing, commands: Receiver<Command>) {
    let demo_steps = if matches!(backing, Backing::Demo(_)) { tick(DEMO_STEP) } else { never() };
    loop {
        select! {
            recv(commands) -> command => match command {
                Ok(Command::Stop) | Err(_) => break,
                Ok(command) => backing.handle(command),
            },
            recv(demo_steps) -> _ => backing.advance_demo(),
        }
    }
    // Dropped before the final send: the persistence thread may be waiting on a read still in
    // this queue, and dropping it wakes that thread so it can receive the final inventory.
    drop(commands);
    backing.finish();
}

/// Waits for changes to settle, then asks the owner for a copy, so a burst of changes costs
/// one write and the owner only pays for a copy when a write is due.
fn run_persistence(changes: Receiver<Persist>, state: StateHandle, snapshots: Snapshots, debounce: Duration) -> Result<(), String> {
    let mut saved: Option<Cursor> = None;
    let mut due: Option<Instant> = None;
    loop {
        let next = match due {
            Some(at) => changes.recv_deadline(at),
            None => changes.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Persist::Changed) => {
                due.get_or_insert_with(|| Instant::now() + debounce);
            }
            Ok(Persist::Final(inventory)) => {
                snapshots.save(&inventory).map_err(|e| format!("The final save failed: {e}"))?;
                return snapshots.finish_clean().map_err(|e| format!("Couldn't record the clean stop: {e}"));
            }
            Err(RecvTimeoutError::Timeout) => {
                due = None;
                let Ok(inventory) = state.read(Inventory::clone) else { continue };
                if saved == Some(inventory.cursor()) {
                    continue;
                }
                match snapshots.save(&inventory) {
                    Ok(()) => saved = Some(inventory.cursor()),
                    Err(e) => {
                        say!("Couldn't save the inventory, will retry: {e}");
                        due = Some(Instant::now() + SAVE_RETRY);
                    }
                }
            }
            // The owner is gone without a final copy. Leaving the lock file behind is correct.
            Err(RecvTimeoutError::Disconnected) => return Err("The state owner stopped without a final copy.".into()),
        }
    }
}

/// The running threads. Dropping this doesn't stop them; call [`shutdown`](Self::shutdown).
pub struct StateOwner {
    handle: StateHandle,
    owner: JoinHandle<()>,
    persistence: Option<JoinHandle<Result<(), String>>>,
}

impl StateOwner {
    pub fn start(opened: Opened, debounce: Duration) -> io::Result<Self> {
        let Opened { inventory, journal, snapshots } = opened;
        // One slot is enough, because `Changed` carries no data.
        let (persist, changes) = bounded(1);
        let (handle, commands) = handle_and_queue();
        let state = handle.clone();
        let persistence = thread::Builder::new().name("twin-persist".into()).spawn(move || run_persistence(changes, state, snapshots, debounce))?;
        let owner = spawn_owner(Backing::Stored { inventory, journal, persist }, commands)?;
        Ok(Self { handle, owner, persistence: Some(persistence) })
    }

    /// Serves invented data. Nothing here can reach the disk.
    pub fn start_demo(devices: usize) -> io::Result<Self> {
        let (handle, commands) = handle_and_queue();
        let owner = spawn_owner(Backing::Demo(DemoFactory::new(devices)), commands)?;
        Ok(Self { handle, owner, persistence: None })
    }

    pub fn handle(&self) -> StateHandle {
        self.handle.clone()
    }

    /// Stops the owner, then writes the final copy. Every caller of the handle must be done.
    pub fn shutdown(self) -> Result<(), String> {
        let _ = self.handle.commands.send(Command::Stop);
        let _ = self.owner.join();
        match self.persistence {
            Some(persistence) => persistence.join().unwrap_or_else(|_| Err("The persistence thread panicked.".into())),
            None => Ok(()),
        }
    }
}

fn handle_and_queue() -> (StateHandle, Receiver<Command>) {
    let (commands, queue) = bounded(1024);
    (StateHandle { commands }, queue)
}

fn spawn_owner(backing: Backing, commands: Receiver<Command>) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("twin-state".into()).spawn(move || run_owner(backing, commands))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use twin_proto::model::{Coverage, DeviceClass, DeviceId, EventKind, Liveness, Severity};

    use super::*;
    use crate::store::testing::TempDir;
    use crate::store::{self, INVENTORY_FILE, LOCK_FILE};

    const NOW_MS: u64 = 1_792_000_000_000;

    fn device(id: &str) -> Device {
        Device { id: DeviceId(id.into()), class: DeviceClass::Plc, liveness: Liveness::Up, coverage: Coverage::Counted, identity: Default::default(), addresses: Vec::new(), mode: None, parent: None, last_seen_ms: None, rev: 0 }
    }

    fn new_event() -> NewEvent {
        NewEvent::new(NOW_MS, EventKind::Appeared, Severity::Info, None)
    }

    fn start(dir: &Path) -> StateOwner {
        StateOwner::start(store::open(dir).unwrap(), Duration::from_millis(10)).unwrap()
    }

    #[test]
    fn a_change_and_its_event_arrive_together_and_are_numbered() {
        let temp = TempDir::new();
        let owner = start(&temp.0);
        let state = owner.handle();
        let first = state.sync(None).unwrap();

        let events = state.apply(ChangeSet { devices: vec![device("a")], events: vec![new_event(), new_event()] }).unwrap();
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);

        let reply = state.sync(Some(first.cursor)).unwrap();
        assert_eq!((reply.changed.len(), reply.events.len()), (1, 2));
        assert_eq!(state.read(|inventory| inventory.len()).unwrap(), 1);
        owner.shutdown().unwrap();
    }

    #[test]
    fn a_refused_save_changes_nothing_and_its_number_is_not_used_again() {
        let temp = TempDir::new();
        let owner = start(&temp.0);
        let state = owner.handle();
        let before = state.sync(None).unwrap().cursor;

        // A folder where the month's file belongs makes the journal fail.
        let blocker = temp.0.join(format!("events-{}.jsonl", clock::month_key(NOW_MS)));
        fs::create_dir(&blocker).unwrap();
        let refused = state.apply(ChangeSet { devices: vec![device("a")], events: vec![new_event()] });
        assert!(matches!(refused, Err(StateError::Journal(_))), "{refused:?}");
        let after = state.sync(Some(before)).unwrap();
        assert_eq!(after.cursor, before, "nothing became visible");
        assert!(after.changed.is_empty() && after.events.is_empty());

        fs::remove_dir(&blocker).unwrap();
        // The failed write might have reached the disk, so its number stays used.
        assert_eq!(state.record(new_event()).unwrap().seq, 2);
        owner.shutdown().unwrap();
    }

    #[test]
    fn changes_reach_the_disk_after_the_debounce_without_a_stop() {
        let temp = TempDir::new();
        let owner = start(&temp.0);
        owner.handle().upsert(device("a")).unwrap();
        let path = temp.0.join(INVENTORY_FILE);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let saved: Inventory = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if saved.len() == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "the change was never saved");
            thread::sleep(Duration::from_millis(5));
        }
        owner.shutdown().unwrap();
    }

    #[test]
    fn a_clean_shutdown_flushes_and_removes_the_lock() {
        let temp = TempDir::new();
        // A long debounce, so only the final save can have written this.
        let owner = StateOwner::start(store::open(&temp.0).unwrap(), Duration::from_secs(3600)).unwrap();
        owner.handle().upsert(device("a")).unwrap();
        owner.shutdown().unwrap();
        let saved: Inventory = serde_json::from_slice(&fs::read(temp.0.join(INVENTORY_FILE)).unwrap()).unwrap();
        assert_eq!(saved.len(), 1);
        assert!(!temp.0.join(LOCK_FILE).exists());
    }

    #[test]
    fn a_failed_final_save_keeps_the_lock_so_the_next_start_knows() {
        let temp = TempDir::new();
        let owner = start(&temp.0);
        // A folder where the temp file belongs makes every save fail.
        fs::create_dir(temp.0.join("inventory.json.tmp")).unwrap();
        assert!(owner.shutdown().is_err());
        assert!(temp.0.join(LOCK_FILE).exists());
    }

    #[test]
    fn demo_data_refuses_changes_and_uses_no_folder() {
        let owner = StateOwner::start_demo(10).unwrap();
        let state = owner.handle();
        assert!(state.sync(None).unwrap().demo);
        assert_eq!(state.upsert(device("a")), Err(StateError::Demo));
        assert_eq!(state.read(|inventory| inventory.len()).unwrap(), 10);
        owner.shutdown().unwrap();
    }

    #[test]
    fn a_handle_outliving_the_owner_reports_that_it_stopped() {
        let owner = StateOwner::start_demo(1).unwrap();
        let state = owner.handle();
        owner.shutdown().unwrap();
        assert_eq!(state.sync(None).unwrap_err(), StateError::Stopped);
    }
}
