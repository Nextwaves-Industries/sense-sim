//! What survives a restart. One folder, written by the one process that holds its lock.
//!
//! Events reach the journal before anyone can see them, so a sequence number a viewer has
//! seen is always on disk. Device revisions are different: `inventory.json` is debounced, so
//! a kill can lose revisions a viewer has already read, and the numbers would then be used
//! again for different changes. The lock file tells the next start this may have happened,
//! and it picks a new epoch, which makes viewers reload instead of trusting their copy.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use twin_proto::inventory::{Inventory, RECENT_EVENTS};
use twin_proto::model::Event;
use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

use crate::{clock, say, win};

pub const INVENTORY_FILE: &str = "inventory.json";
pub const LOCK_FILE: &str = "twind.lock";
const TEMP_FILE: &str = "inventory.json.tmp";
/// Much longer than any one event, so the end of a file always holds its last whole line.
const TAIL_BYTES: u64 = 64 * 1024;

pub struct Opened {
    pub inventory: Inventory,
    pub journal: Journal,
    pub snapshots: Snapshots,
}

/// Loads the store, creating the folder if needed. Nothing else touches the folder until
/// this has the lock, so two services can't interleave their numbering.
pub fn open(dir: &Path) -> io::Result<Opened> {
    fs::create_dir_all(dir).map_err(|e| context(format!("Can't create {}", dir.display()), e))?;
    let (lock, left_running) = take_lock(dir)?;
    let snapshots = Snapshots { dir: dir.to_path_buf(), lock };

    let mut inventory = load_inventory(dir)?;
    let mut new_epoch_to_save = inventory.is_none();
    if left_running && inventory.is_some() {
        say!("The last run didn't stop cleanly, so its newest device changes may be lost. Viewers will reload.");
        inventory = inventory.map(with_new_epoch).transpose()?;
        new_epoch_to_save = true;
    }
    let mut inventory = match inventory {
        Some(inventory) => inventory,
        None => Inventory::new(new_epoch()?),
    };

    let restored = load_events(dir)?;
    if restored.bad_lines > 0 {
        say!("Ignored {} unreadable event line(s), such as one cut short by a crash. The rest of the journal is fine.", restored.bad_lines);
    }
    for event in restored.events {
        inventory.remember(event);
    }

    // The epoch has to be on disk before a viewer can see it, or a crash right after starting
    // would hand out a different one with the same revisions.
    if new_epoch_to_save {
        snapshots.save(&inventory).map_err(|e| context(format!("Can't write to {}", dir.display()), e))?;
    }
    Ok(Opened { inventory, journal: Journal::new(dir), snapshots })
}

fn context(what: String, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{what}: {error}"))
}

/// Returns the lock and whether a previous run left it behind. No sharing is allowed while
/// the file is open, and Windows closes it however the process ends, so a crash can't leave
/// a lock that blocks the restart.
fn take_lock(dir: &Path) -> io::Result<(File, bool)> {
    let path = dir.join(LOCK_FILE);
    let left_running = path.exists();
    match OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(&path) {
        Ok(file) => Ok((file, left_running)),
        Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => Err(io::Error::other(format!("Another Sense Twin is already using {}.", dir.display()))),
        Err(e) => Err(context(format!("Can't lock {}", path.display()), e)),
    }
}

fn load_inventory(dir: &Path) -> io::Result<Option<Inventory>> {
    let path = dir.join(INVENTORY_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        // Anything else may be temporary. Carrying on as if empty would overwrite real data.
        Err(e) => return Err(context(format!("Can't read {}", path.display()), e)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(inventory) => Ok(Some(inventory)),
        Err(e) => {
            let aside = set_aside(dir)?;
            say!("{INVENTORY_FILE} is corrupt ({e}). It's kept as {} and the inventory starts empty.", aside.display());
            Ok(None)
        }
    }
}

fn set_aside(dir: &Path) -> io::Result<PathBuf> {
    let stamp = clock::now_ms();
    let aside = (0u32..).map(|n| dir.join(format!("{INVENTORY_FILE}.corrupt-{stamp}-{n}"))).find(|path| !path.exists()).expect("a free name exists");
    fs::rename(dir.join(INVENTORY_FILE), &aside).map_err(|e| context(format!("Can't set the corrupt {INVENTORY_FILE} aside"), e))?;
    Ok(aside)
}

fn new_epoch() -> io::Result<u64> {
    // Zero reads as "never set" in a debugger or a hand-written file.
    Ok(win::random_u64()?.max(1))
}

/// `Inventory` has no setter, because only the service should ever do this.
fn with_new_epoch(inventory: Inventory) -> io::Result<Inventory> {
    let mut saved = serde_json::to_value(&inventory).map_err(io::Error::other)?;
    saved["epoch"] = new_epoch()?.into();
    serde_json::from_value(saved).map_err(io::Error::other)
}

/// Writes `inventory.json`. Holds the lock, because the snapshot writer is the last to finish.
pub struct Snapshots {
    dir: PathBuf,
    lock: File,
}

impl Snapshots {
    /// All or nothing: a kill at any point leaves either the old file or the new one.
    pub fn save(&self, inventory: &Inventory) -> io::Result<()> {
        let body = serde_json::to_vec(inventory).map_err(io::Error::other)?;
        let temp = self.dir.join(TEMP_FILE);
        let mut file = File::create(&temp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        rename_over(&temp, &self.dir.join(INVENTORY_FILE))
    }

    /// Records a clean stop. Call only after the final save succeeded.
    pub fn finish_clean(self) -> io::Result<()> {
        drop(self.lock);
        fs::remove_file(self.dir.join(LOCK_FILE))
    }
}

/// Antivirus and indexers briefly hold new files open, which fails a rename on Windows.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempts = 0;
    loop {
        match fs::rename(from, to) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempts < 10 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

/// `events-YYYYMM.jsonl`, append-only, named by each event's own UTC month.
pub struct Journal {
    dir: PathBuf,
    open: Option<(String, File)>,
    /// The file may end partway through a line, which the next write must not extend.
    torn_tail: bool,
    attempted: u64,
}

impl Journal {
    fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf(), open: None, torn_tail: false, attempted: 0 }
    }

    /// The highest number ever handed to [`append`](Self::append), even if that write failed.
    /// A failed line might still be on disk, so that number must not go to another event.
    pub fn attempted(&self) -> u64 {
        self.attempted
    }

    /// Returns once the events are on the disk, not just in the OS's cache.
    pub fn append(&mut self, events: &[Event]) -> io::Result<()> {
        if let Some(last) = events.last() {
            self.attempted = self.attempted.max(last.seq);
        }
        for same_month in events.chunk_by(|a, b| clock::month_key(a.ts_ms) == clock::month_key(b.ts_ms)) {
            self.append_month(&clock::month_key(same_month[0].ts_ms), same_month)?;
        }
        Ok(())
    }

    fn append_month(&mut self, month: &str, events: &[Event]) -> io::Result<()> {
        if self.open.as_ref().is_none_or(|(open_month, _)| open_month != month) {
            self.open_month(month)?;
        }
        let mut lines = Vec::new();
        if self.torn_tail {
            lines.push(b'\n');
        }
        for event in events {
            serde_json::to_writer(&mut lines, event).map_err(io::Error::other)?;
            lines.push(b'\n');
        }
        let (_, file) = self.open.as_mut().expect("opened above");
        self.torn_tail = true;
        file.write_all(&lines)?;
        file.sync_data()?;
        self.torn_tail = false;
        Ok(())
    }

    fn open_month(&mut self, month: &str) -> io::Result<()> {
        let path = self.dir.join(format!("events-{month}.jsonl"));
        let file = OpenOptions::new().read(true).append(true).create(true).open(path)?;
        let length = file.metadata()?.len();
        self.torn_tail = false;
        if length > 0 {
            let mut last = [0u8];
            file.seek_read(&mut last, length - 1)?;
            self.torn_tail = last[0] != b'\n';
        }
        self.open = Some((month.to_string(), file));
        Ok(())
    }
}

struct Restored {
    /// Oldest first, at most [`RECENT_EVENTS`], and always including the highest number.
    events: Vec<Event>,
    bad_lines: usize,
}

fn load_events(dir: &Path) -> io::Result<Restored> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if is_journal_file(&path) {
            paths.push(path);
        }
    }
    paths.sort();

    // Newest name first, stopping full reads once there's enough to show. The older files
    // still get their tails read, because a clock set back can put the highest number in an
    // older-named file, and that number must not be reused. Files are append-only, so a
    // file's last line holds its highest number.
    let mut events = Vec::new();
    let mut bad_lines = 0;
    for path in paths.iter().rev() {
        let (found, bad) = if events.len() < RECENT_EVENTS {
            read_lines(path, None)?
        } else {
            match read_lines(path, Some(TAIL_BYTES))? {
                (found, _) if found.is_empty() => read_lines(path, None)?,
                tail => tail,
            }
        };
        bad_lines += bad;
        events.extend(found);
    }
    events.sort_by_key(|event: &Event| event.seq);
    events.drain(..events.len().saturating_sub(RECENT_EVENTS));
    Ok(Restored { events, bad_lines })
}

fn is_journal_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.strip_prefix("events-").and_then(|n| n.strip_suffix(".jsonl")).is_some_and(|month| !month.is_empty() && month.bytes().all(|b| b.is_ascii_digit()))
}

fn read_lines(path: &Path, tail: Option<u64>) -> io::Result<(VecDeque<Event>, usize)> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut starts_mid_line = false;
    if let Some(tail) = tail
        && length > tail
    {
        file.seek(SeekFrom::Start(length - tail))?;
        starts_mid_line = true;
    }

    let mut events = VecDeque::new();
    let mut bad = 0;
    for line in BufReader::new(file).split(b'\n') {
        let line = line?;
        if std::mem::take(&mut starts_mid_line) || line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Event>(&line) {
            Ok(event) => {
                events.push_back(event);
                if events.len() > RECENT_EVENTS {
                    events.pop_front();
                }
            }
            Err(_) => bad += 1,
        }
    }
    Ok((events, bad))
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!("sense-twin-unit-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use twin_proto::model::{Device, DeviceClass, DeviceId, EventKind, Liveness, Severity};

    use super::testing::TempDir;
    use super::*;

    const OCTOBER_2026: u64 = 1_792_000_000_000;
    const NOVEMBER_2026: u64 = 1_794_000_000_000;

    fn event(seq: u64, ts_ms: u64) -> Event {
        Event { seq, ts_ms, kind: EventKind::Appeared, severity: Severity::Info, device_id: None, vantage: None, before: None, after: None, correlation_id: None, untrusted: Default::default() }
    }

    fn device(id: &str) -> Device {
        Device {
            id: DeviceId(id.into()),
            class: DeviceClass::Plc,
            liveness: Liveness::Up,
            coverage: twin_proto::model::Coverage::Counted,
            identity: Default::default(),
            addresses: Vec::new(),
            mode: None,
            parent: None,
            last_seen_ms: None,
            rev: 0,
        }
    }

    fn close_cleanly(opened: Opened) {
        opened.snapshots.save(&opened.inventory).unwrap();
        opened.snapshots.finish_clean().unwrap();
    }

    fn journal_names(dir: &Path) -> Vec<String> {
        let names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap());
        let mut names: Vec<String> = names.filter(|n| n.starts_with("events-")).collect();
        names.sort();
        names
    }

    #[test]
    fn events_rotate_by_month_and_come_back_in_order() {
        let temp = TempDir::new();
        let mut journal = open(&temp.0).unwrap().journal;
        journal.append(&[event(1, OCTOBER_2026), event(2, OCTOBER_2026), event(3, NOVEMBER_2026)]).unwrap();
        journal.append(&[event(4, NOVEMBER_2026)]).unwrap();
        assert_eq!(journal_names(&temp.0), ["events-202610.jsonl", "events-202611.jsonl"]);

        let restored = load_events(&temp.0).unwrap();
        assert_eq!(restored.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(restored.bad_lines, 0);
    }

    #[test]
    fn a_month_going_backwards_reopens_the_right_file() {
        let temp = TempDir::new();
        let mut journal = open(&temp.0).unwrap().journal;
        journal.append(&[event(1, NOVEMBER_2026)]).unwrap();
        journal.append(&[event(2, OCTOBER_2026)]).unwrap();
        assert_eq!(journal_names(&temp.0), ["events-202610.jsonl", "events-202611.jsonl"]);
        assert_eq!(load_events(&temp.0).unwrap().events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn the_highest_number_counts_even_in_an_older_named_file() {
        let temp = TempDir::new();
        let mut journal = open(&temp.0).unwrap().journal;
        // More than fits in memory in the newest file, then a clock set back by a month.
        let many: Vec<Event> = (1..=RECENT_EVENTS as u64 + 10).map(|seq| event(seq, NOVEMBER_2026)).collect();
        journal.append(&many).unwrap();
        journal.append(&[event(RECENT_EVENTS as u64 + 11, OCTOBER_2026)]).unwrap();

        let restored = load_events(&temp.0).unwrap();
        assert_eq!(restored.events.len(), RECENT_EVENTS);
        assert_eq!(restored.events.last().unwrap().seq, RECENT_EVENTS as u64 + 11);
    }

    #[test]
    fn a_torn_last_line_is_ignored_and_the_next_event_starts_on_its_own_line() {
        let temp = TempDir::new();
        let path = temp.0.join("events-202610.jsonl");
        {
            let mut journal = open(&temp.0).unwrap().journal;
            journal.append(&[event(1, OCTOBER_2026)]).unwrap();
        }
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":2,"ts_ms":17"#).unwrap();
        drop(file);

        let restored = load_events(&temp.0).unwrap();
        assert_eq!(restored.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1]);
        assert_eq!(restored.bad_lines, 1);

        let mut journal = Journal::new(&temp.0);
        journal.append(&[event(2, OCTOBER_2026)]).unwrap();
        let restored = load_events(&temp.0).unwrap();
        assert_eq!(restored.events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(restored.bad_lines, 1, "the fragment stays as it was, and the new event isn't glued to it");
    }

    #[test]
    fn a_corrupt_inventory_is_set_aside_each_time_under_its_own_name() {
        let temp = TempDir::new();
        let inventory = temp.0.join(INVENTORY_FILE);
        for garbage in ["first garbage", "second garbage"] {
            fs::write(&inventory, garbage).unwrap();
            let opened = open(&temp.0).unwrap();
            assert!(opened.inventory.is_empty());
            close_cleanly(opened);
        }
        let mut kept: Vec<String> = fs::read_dir(&temp.0).unwrap().map(|e| e.unwrap().path()).filter(|p| p.to_str().unwrap().contains(".corrupt-")).map(|p| fs::read_to_string(p).unwrap()).collect();
        kept.sort();
        assert_eq!(kept, ["first garbage", "second garbage"]);
    }

    #[test]
    fn an_unreadable_inventory_stops_startup_instead_of_starting_empty() {
        let temp = TempDir::new();
        fs::create_dir(temp.0.join(INVENTORY_FILE)).unwrap();
        assert!(open(&temp.0).is_err());
    }

    #[test]
    fn a_clean_stop_keeps_the_epoch_and_an_unclean_one_replaces_it() {
        let temp = TempDir::new();
        let mut opened = open(&temp.0).unwrap();
        opened.inventory.upsert(device("a"));
        let first = opened.inventory.cursor();
        close_cleanly(opened);

        let opened = open(&temp.0).unwrap();
        assert_eq!(opened.inventory.cursor(), first);
        assert_eq!(opened.inventory.len(), 1);
        // Dropping without `finish_clean` is what a kill looks like.
        opened.snapshots.save(&opened.inventory).unwrap();
        drop(opened);

        let opened = open(&temp.0).unwrap();
        let after_kill = opened.inventory.cursor();
        assert_ne!(after_kill.epoch, first.epoch);
        assert_eq!(after_kill.rev, first.rev);
        assert_eq!(opened.inventory.len(), 1, "nothing is lost by changing the epoch");
    }

    #[test]
    fn the_journal_can_be_ahead_of_the_inventory() {
        let temp = TempDir::new();
        let mut opened = open(&temp.0).unwrap();
        opened.journal.append(&[event(1, OCTOBER_2026), event(2, OCTOBER_2026)]).unwrap();
        // The inventory on disk still says seq 0, as it would after a kill.
        close_cleanly(opened);
        assert_eq!(open(&temp.0).unwrap().inventory.cursor().seq, 2);
    }

    #[test]
    fn saving_replaces_the_whole_file_and_a_leftover_temp_does_no_harm() {
        let temp = TempDir::new();
        let mut opened = open(&temp.0).unwrap();
        fs::write(temp.0.join(TEMP_FILE), "half a file from a kill").unwrap();
        opened.inventory.upsert(device("a"));
        opened.snapshots.save(&opened.inventory).unwrap();
        assert!(!temp.0.join(TEMP_FILE).exists());
        let saved: Inventory = serde_json::from_slice(&fs::read(temp.0.join(INVENTORY_FILE)).unwrap()).unwrap();
        assert_eq!(saved.len(), 1);
    }

    #[test]
    fn a_second_opener_of_the_same_folder_is_refused() {
        let temp = TempDir::new();
        let _first = open(&temp.0).unwrap();
        let second = open(&temp.0).err().expect("the folder is taken");
        assert!(second.to_string().contains("already using"), "{second}");
    }

    #[test]
    fn only_journal_files_count() {
        for good in ["events-202610.jsonl", "events-197001.jsonl"] {
            assert!(is_journal_file(Path::new(good)), "{good}");
        }
        for bad in ["events-.jsonl", "events-2026.jsonl.tmp", "inventory.json", "events-20x610.jsonl"] {
            assert!(!is_journal_file(Path::new(bad)), "{bad}");
        }
    }
}
