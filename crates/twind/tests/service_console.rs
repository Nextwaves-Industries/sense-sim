//! The real exe in console mode, each test with its own pipe name and data folder, talking
//! through the client the viewer uses. Nothing here touches the service manager, a local
//! group, `%ProgramData%` or the network.

#![cfg(windows)]

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender, bounded};
use twin_proto::inventory::{Cursor, Inventory};
use twin_proto::model::{Coverage, Device, DeviceClass, DeviceId, Event, EventKind, Liveness, NewEvent, Severity};
use twin_proto::read_client::ReadClient;
use twin_proto::wire::{self, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response};
use twind::clock;
use twind::pipe::{MAX_CLIENTS, MAX_REQUEST_BYTES};
use twind::sddl;
use twind::store::{INVENTORY_FILE, LOCK_FILE};
use twind::win;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, FALSE, HANDLE, INVALID_HANDLE_VALUE, LocalFree, TRUE};
use windows_sys::Win32::Security::Authorization::{ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Console::{AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler};
use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const EXE: &str = env!("CARGO_BIN_EXE_SenseTwin");
const READY_LIMIT: Duration = Duration::from_secs(30);
const STOP_LIMIT: Duration = Duration::from_secs(15);
/// Long enough for any test. A wedged server dies here, so tests fail instead of hanging.
const WATCHDOG: Duration = Duration::from_secs(180);
const PIPE_EXIT_CODE: i32 = 5;
const NOW_MS: u64 = 1_792_000_000_000;

fn unique() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

fn unique_pipe() -> String {
    format!(r"\\.\pipe\SenseTwin.Test.{}", unique())
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sense-twin-it-{}", unique()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn arg(&self) -> &str {
        self.0.to_str().unwrap()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Twin {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    pipe: String,
    log: Arc<Mutex<String>>,
    log_reader: Option<JoinHandle<()>>,
    _watchdog: Sender<()>,
}

struct Finished {
    status: ExitStatus,
    log: String,
}

impl Twin {
    fn start(pipe: &str, args: &[&str]) -> Self {
        Self::spawn(pipe, args, 0)
    }

    fn spawn(pipe: &str, args: &[&str], creation_flags: u32) -> Self {
        let mut command = Command::new(EXE);
        command.args(["--role", "core", "--console", "--pipe", pipe]).args(args);
        command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).creation_flags(creation_flags);
        let mut child = command.spawn().expect("start SenseTwin");

        let mut stderr = child.stderr.take().unwrap();
        let log: Arc<Mutex<String>> = Arc::default();
        let sink = Arc::clone(&log);
        let log_reader = thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(read) = stderr.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                sink.lock().unwrap().push_str(&String::from_utf8_lossy(&chunk[..read]));
            }
        });

        let stdin = child.stdin.take();
        let child = Arc::new(Mutex::new(child));
        let (watchdog, finished) = bounded::<()>(0);
        let target = Arc::clone(&child);
        thread::spawn(move || {
            if finished.recv_timeout(WATCHDOG) == Err(RecvTimeoutError::Timeout) {
                let _ = target.lock().unwrap().kill();
            }
        });
        Self { child, stdin, pipe: pipe.to_string(), log, log_reader: Some(log_reader), _watchdog: watchdog }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    fn try_wait(&self) -> Option<ExitStatus> {
        self.child.lock().unwrap().try_wait().unwrap()
    }

    fn client(&self) -> ReadClient {
        let deadline = Instant::now() + READY_LIMIT;
        loop {
            match ReadClient::connect_to(&self.pipe) {
                Ok(client) => return client,
                Err(e) => {
                    if let Some(status) = self.try_wait() {
                        panic!("the service exited with {status} before it could be reached:\n{}", self.log());
                    }
                    assert!(Instant::now() < deadline, "never reachable ({e}):\n{}", self.log());
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    fn wait_for_log(&self, text: &str) {
        let deadline = Instant::now() + READY_LIMIT;
        while !self.log().contains(text) {
            if let Some(status) = self.try_wait() {
                panic!("the service exited with {status} before logging {text:?}:\n{}", self.log());
            }
            assert!(Instant::now() < deadline, "never logged {text:?}:\n{}", self.log());
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn finish_within(mut self, limit: Duration) -> Finished {
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = self.try_wait() {
                break status;
            }
            assert!(Instant::now() < deadline, "still running after {limit:?}:\n{}", self.log());
            thread::sleep(Duration::from_millis(5));
        };
        // Past this point the whole log has been read.
        if let Some(reader) = self.log_reader.take() {
            let _ = reader.join();
        }
        Finished { status, log: self.log() }
    }

    /// Starts the stop, leaving the wait to the caller.
    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    /// Closes stdin and insists on a clean exit.
    fn stop(mut self) -> String {
        self.close_stdin();
        let finished = self.finish_within(STOP_LIMIT);
        assert!(finished.status.success(), "exit {}:\n{}", finished.status, finished.log);
        finished.log
    }

    fn kill(self) {
        let mut child = self.child.lock().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

impl Drop for Twin {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Polls with the viewer's own cheap sync until `done` holds.
fn wait_for_cursor(client: &mut ReadClient, done: impl Fn(&Cursor) -> bool) -> Cursor {
    let deadline = Instant::now() + READY_LIMIT;
    let mut cursor = None;
    loop {
        let now = client.sync(cursor).unwrap().cursor;
        if done(&now) {
            return now;
        }
        assert!(Instant::now() < deadline, "the service never reached the expected state, stuck at {now:?}");
        cursor = Some(now);
        thread::sleep(Duration::from_millis(2));
    }
}

fn device(id: &str) -> Device {
    Device { id: DeviceId(id.into()), class: DeviceClass::Plc, liveness: Liveness::Up, coverage: Coverage::Counted, identity: Default::default(), addresses: Vec::new(), mode: None, parent: None, last_seen_ms: None, rev: 0 }
}

/// Events numbered 1..=count, as the journal would hold them.
fn numbered_events(count: u64) -> Vec<Event> {
    let mut numbering = Inventory::new(1);
    (0..count).map(|_| numbering.record(NewEvent::new(NOW_MS, EventKind::Appeared, Severity::Info, None))).collect()
}

fn write_journal(dir: &TempDir, events: &[Event]) {
    let mut lines = String::new();
    for event in events {
        lines.push_str(&serde_json::to_string(event).unwrap());
        lines.push('\n');
    }
    fs::write(dir.file(&format!("events-{}.jsonl", clock::month_key(NOW_MS))), lines).unwrap();
}

fn write_inventory(dir: &TempDir, inventory: &Inventory) {
    fs::write(dir.file(INVENTORY_FILE), serde_json::to_vec(inventory).unwrap()).unwrap();
}

fn saved_inventory(dir: &TempDir) -> Inventory {
    let bytes = fs::read(dir.file(INVENTORY_FILE)).expect("inventory.json");
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("inventory.json doesn't load: {e}"))
}

fn journal_paths(dir: &TempDir) -> Vec<PathBuf> {
    dir.names().into_iter().filter(|n| n.starts_with("events-")).map(|n| dir.file(&n)).collect()
}

/// Every readable event number in file order, which is the order they were written.
fn journal_seqs(dir: &TempDir) -> Vec<u64> {
    let mut seqs = Vec::new();
    for path in journal_paths(dir) {
        for line in fs::read_to_string(path).unwrap().lines() {
            if let Ok(event) = serde_json::from_str::<Event>(line) {
                seqs.push(event.seq);
            }
        }
    }
    seqs
}

/// Each number once, in order, with none skipped.
fn assert_numbered_without_reuse(seqs: &[u64]) {
    let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
    assert_eq!(seqs, expected, "event numbers must be 1, 2, 3, ... with no repeats");
}

fn corrupt_files(dir: &TempDir) -> Vec<String> {
    dir.names().into_iter().filter(|n| n.contains(".corrupt-")).collect()
}

fn open_raw(pipe: &str) -> File {
    let deadline = Instant::now() + READY_LIMIT;
    loop {
        match OpenOptions::new().read(true).write(true).open(pipe) {
            Ok(file) => return file,
            Err(e) => {
                assert!(Instant::now() < deadline, "{pipe}: {e}");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// True if the server closes the connection rather than answering. In a thread, so a server
/// that leaves it open fails this instead of blocking forever.
fn server_closes(mut connection: File) -> bool {
    let (sender, receiver) = bounded(1);
    thread::spawn(move || {
        let mut byte = [0u8; 1];
        let closed = !matches!(connection.read(&mut byte), Ok(read) if read > 0);
        let _ = sender.send(closed);
    });
    receiver.recv_timeout(Duration::from_secs(20)).expect("the server left a bad connection open")
}

fn squat_on(pipe: &str) -> HANDLE {
    let name = win::wide(pipe);
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT;
    unsafe { CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, mode, PIPE_UNLIMITED_INSTANCES, 4096, 4096, 0, null()) }
}

fn last_error() -> Option<u32> {
    std::io::Error::last_os_error().raw_os_error().map(|code| code as u32)
}

// ---------------------------------------------------------------------------
// Answering

#[test]
fn a_fresh_store_answers_hello_and_sync() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);

    let mut client = twin.client();
    let first = client.sync(None).unwrap();
    assert!(first.full && first.changed.is_empty() && first.events.is_empty());
    assert!(!first.demo);
    assert_ne!(first.cursor.epoch, 0);

    let second = client.sync(Some(first.cursor)).unwrap();
    assert!(!second.full);
    assert_eq!(second.cursor, first.cursor);

    let mut raw = open_raw(&twin.pipe);
    // The reply carries the service's protocol, so a mismatched client can explain itself.
    wire::write_frame(&mut raw, &Request::Hello { protocol: 999 }).unwrap();
    match wire::read_frame(&mut raw).unwrap() {
        Response::Hello { protocol, service_version } => {
            assert_eq!(protocol, PROTOCOL_VERSION);
            assert_eq!(service_version, env!("CARGO_PKG_VERSION"));
        }
        other => panic!("{other:?}"),
    }
    drop((client, raw));
    twin.stop();
}

// ---------------------------------------------------------------------------
// Holding the pipe name

#[test]
fn a_second_service_on_the_same_pipe_is_refused_and_touches_nothing() {
    let (first_data, second_data) = (TempDir::new(), TempDir::new());
    let untouched = second_data.file("never-created");
    let pipe = unique_pipe();
    let first = Twin::start(&pipe, &["--data-dir", first_data.arg()]);
    let mut client = first.client();

    let second = Twin::start(&pipe, &["--data-dir", untouched.to_str().unwrap()]);
    let refused = second.finish_within(READY_LIMIT);
    assert_eq!(refused.status.code(), Some(PIPE_EXIT_CODE), "{}", refused.log);
    assert!(refused.log.contains(&pipe), "the refusal should name the pipe:\n{}", refused.log);
    assert!(!untouched.exists(), "a refused start must not create or read the store");

    client.sync(None).expect("the first service carries on");
    drop(client);
    first.stop();
}

#[test]
fn a_process_already_squatting_on_the_name_makes_startup_fail_loudly() {
    let data = TempDir::new();
    let untouched = data.file("never-created");
    let pipe = unique_pipe();
    let squatter = squat_on(&pipe);
    assert_ne!(squatter, INVALID_HANDLE_VALUE);

    let twin = Twin::start(&pipe, &["--data-dir", untouched.to_str().unwrap()]);
    let refused = twin.finish_within(READY_LIMIT);
    assert_eq!(refused.status.code(), Some(PIPE_EXIT_CODE), "{}", refused.log);
    assert!(refused.log.contains("already in use"), "{}", refused.log);
    assert!(!untouched.exists());
    unsafe { CloseHandle(squatter) };
}

#[test]
fn while_running_no_room_is_left_for_a_rogue_extra_instance() {
    let data = TempDir::new();
    let pipe = unique_pipe();
    let twin = Twin::start(&pipe, &["--data-dir", data.arg()]);
    drop(twin.client());

    // Same user, so the access list allows this. Only the full instance count stops it.
    let rogue = squat_on(&pipe);
    let error = last_error();
    assert_eq!(rogue, INVALID_HANDLE_VALUE, "a rogue pipe end could answer other viewers");
    assert_eq!(error, Some(ERROR_PIPE_BUSY));
    twin.stop();
}

#[test]
fn two_services_on_one_data_folder_are_refused() {
    let data = TempDir::new();
    let first = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    drop(first.client());

    let second = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let refused = second.finish_within(READY_LIMIT);
    assert_eq!(refused.status.code(), Some(4), "{}", refused.log);
    assert!(refused.log.contains("already using"), "{}", refused.log);
    first.stop();
}

// ---------------------------------------------------------------------------
// Who may connect

/// The service is up, but this user may not be able to tell, so watch its log.
fn start_with_sddl(data: &TempDir, sddl: &str) -> Twin {
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--pipe-sddl", sddl]);
    twin.wait_for_log("Running in the foreground");
    twin
}

fn assert_access_denied(twin: &Twin) {
    let error = ReadClient::connect_to(&twin.pipe).err().expect("this user should be refused");
    assert_eq!(error.raw_os_error().map(|c| c as u32), Some(ERROR_ACCESS_DENIED), "{error}");
}

#[test]
fn an_sddl_granting_only_system_gives_this_user_access_denied() {
    let data = TempDir::new();
    let twin = start_with_sddl(&data, "D:P(A;;GA;;;SY)");
    assert_access_denied(&twin);
    twin.stop();
}

#[test]
fn the_default_console_pipe_lists_only_this_user() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    drop(twin.client());

    let handle = open_raw(&twin.pipe);
    let actual = dacl_of(&handle);
    let me = win::current_user_sid().unwrap();
    let aces: Vec<&str> = actual.split('(').skip(1).map(|ace| ace.trim_end_matches(')')).collect();
    assert_eq!(aces.len(), 2, "{actual}");
    assert!(aces[0].starts_with("D;;") && aces[0].ends_with(";;;NU"), "network logons are refused first: {actual}");
    assert!(aces[1].starts_with("A;;") && aces[1].ends_with(&format!(";;;{me}")), "only this user is allowed: {actual}");
    assert!(actual.starts_with("D:P"), "the list can't be widened by inheritance: {actual}");
    drop(handle);
    twin.stop();
}

/// The access list a live pipe really has, as Windows reports it.
fn dacl_of(pipe: &File) -> String {
    let mut descriptor = null_mut();
    let status = unsafe { GetSecurityInfo(pipe.as_raw_handle(), SE_KERNEL_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), null_mut(), null_mut(), &mut descriptor) };
    assert_eq!(status, 0, "GetSecurityInfo");
    let mut text = null_mut();
    let ok = unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(descriptor, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut text, null_mut()) };
    assert_ne!(ok, 0);
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    let sddl = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe {
        LocalFree(text.cast());
        LocalFree(descriptor);
    }
    sddl
}

#[test]
fn the_service_access_list_works_for_the_process_running_it_with_every_instance() {
    let me = win::current_user_sid().unwrap();
    let data = TempDir::new();
    let twin = start_with_sddl(&data, &sddl::service_pipe(&me, None).unwrap());
    let mut client = twin.client();
    client.sync(None).unwrap();
    drop(client);
    let log = twin.stop();
    assert!(!log.contains("doesn't let this process add pipe instances"), "it must be able to create them all:\n{log}");
}

#[test]
fn owner_rights_alone_also_cover_the_process_running_it() {
    // The account's own entry is a second way in, so check the first one works without it.
    let data = TempDir::new();
    let twin = start_with_sddl(&data, "D:P(D;;GA;;;NU)(A;;GA;;;OW)");
    twin.client().sync(None).unwrap();
    let log = twin.stop();
    assert!(!log.contains("doesn't let this process add pipe instances"), "{log}");
}

#[test]
fn readers_can_read_through_the_read_client_but_cannot_add_a_pipe_end() {
    let me = win::current_user_sid().unwrap();

    let data = TempDir::new();
    let twin = start_with_sddl(&data, &format!("D:P(A;;{};;;{me})", sddl::READER_RIGHTS));
    let mut client = twin.client();
    client.sync(None).expect("these rights are enough to read the inventory");

    // This descriptor doesn't let the server add instances either, so there's room for one
    // here, and only the access list can be what refuses this.
    let rogue = squat_on(&twin.pipe);
    let error = last_error();
    assert_eq!(rogue, INVALID_HANDLE_VALUE, "a reader could answer other viewers");
    assert_eq!(error, Some(ERROR_ACCESS_DENIED));
    drop(client);
    twin.stop();

    // The client also asks to write, so read alone isn't enough.
    let data = TempDir::new();
    let twin = start_with_sddl(&data, &format!("D:P(A;;GR;;;{me})"));
    assert_access_denied(&twin);
    twin.stop();
}

/// Loopback only. This goes through the redirector, so the pipe sees a remote client.
fn open_as_a_remote_client(pipe: &str) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).open(pipe.replacen(r"\\.\", r"\\localhost\", 1))
}

#[test]
fn a_client_coming_through_the_network_path_is_refused() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    drop(twin.client());
    assert!(open_as_a_remote_client(&twin.pipe).is_err());
    twin.stop();
}

#[test]
fn remote_clients_are_refused_even_by_a_pipe_whose_access_list_would_allow_them() {
    let data = TempDir::new();
    // Everyone is allowed, so PIPE_REJECT_REMOTE_CLIENTS is all that's left to refuse this.
    let twin = start_with_sddl(&data, "D:P(A;;GA;;;WD)");
    drop(twin.client());
    assert!(open_as_a_remote_client(&twin.pipe).is_err());
    twin.stop();
}

// ---------------------------------------------------------------------------
// Storage

#[test]
fn a_clean_restart_keeps_seq_rev_and_epoch_with_no_full_reload() {
    let data = TempDir::new();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    let mid_run = wait_for_cursor(&mut client, |c| c.rev > 0 && c.seq >= 3);
    drop(client);
    twin.stop();

    let saved = saved_inventory(&data).cursor();
    assert_eq!(saved.epoch, mid_run.epoch);
    assert!(saved.rev >= mid_run.rev && saved.seq >= mid_run.seq, "{saved:?} is behind {mid_run:?}");
    assert_eq!(journal_seqs(&data).last(), Some(&saved.seq), "a clean stop leaves the inventory and journal agreeing");
    assert!(!data.file(LOCK_FILE).exists(), "a clean stop removes the lock");

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut client = twin.client();
    let caught_up = client.sync(Some(saved)).unwrap();
    assert!(!caught_up.full, "a viewer that was up to date must not reload");
    assert!(caught_up.changed.is_empty() && caught_up.events.is_empty());
    assert_eq!(caught_up.cursor, saved);

    let behind = client.sync(Some(mid_run)).unwrap();
    assert!(!behind.full, "a viewer that fell behind only needs what it missed");
    assert!(behind.changed.iter().all(|d| d.rev > mid_run.rev));
    if saved.seq > mid_run.seq {
        assert_eq!(behind.events[0].seq, mid_run.seq + 1);
    }
    drop(client);
    twin.stop();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    let later = wait_for_cursor(&mut client, |c| c.seq > saved.seq);
    assert_eq!(later.epoch, saved.epoch);
    drop(client);
    twin.stop();
    assert_numbered_without_reuse(&journal_seqs(&data));
}

#[test]
fn a_kill_leaves_a_loadable_store_and_no_number_is_used_twice() {
    let data = TempDir::new();
    let mut highest_seen = 0;
    let mut last_cursor = None;

    // Saves run back to back, so some of these kills land in the middle of one. That alone
    // would rarely catch a file being rewritten in place, so the file is also read over and
    // over while the saves run. At no moment may it hold anything but a whole inventory.
    for round in 0..20u64 {
        let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "1"]);
        let mut client = twin.client();
        let start = client.sync(None).unwrap().cursor;
        wait_for_cursor(&mut client, |c| c.seq > start.seq);
        let until = Instant::now() + Duration::from_millis(20 + round * 13);
        while Instant::now() < until {
            // Failing to open it during a rename is fine. Reading part of it isn't.
            if let Ok(bytes) = fs::read(data.file(INVENTORY_FILE)) {
                serde_json::from_slice::<Inventory>(&bytes).unwrap_or_else(|e| panic!("saw a partial inventory.json, {} bytes: {e}", bytes.len()));
            }
        }
        let cursor = client.sync(None).unwrap().cursor;
        highest_seen = highest_seen.max(cursor.seq);
        last_cursor = Some(cursor);
        twin.kill();

        saved_inventory(&data);
        assert_eq!(corrupt_files(&data), Vec::<String>::new());
    }

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut client = twin.client();
    let reply = client.sync(last_cursor).unwrap();
    assert!(reply.full, "a kill may have lost revisions a viewer saw, so its copy can't be trusted");
    assert!(!reply.changed.is_empty(), "the devices came back");
    assert!(reply.cursor.seq >= highest_seen, "number {highest_seen} was already seen, but the service restarted at {}", reply.cursor.seq);
    let seqs = journal_seqs(&data);
    assert_numbered_without_reuse(&seqs);
    assert_eq!(reply.cursor.seq, *seqs.last().unwrap());
    let restored = reply.cursor;
    drop(client);
    twin.stop();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    wait_for_cursor(&mut client, |c| c.seq > restored.seq);
    drop(client);
    twin.stop();
    assert_numbered_without_reuse(&journal_seqs(&data));
}

#[test]
fn a_corrupt_inventory_is_kept_aside_and_startup_says_so() {
    let data = TempDir::new();
    write_journal(&data, &numbered_events(5));
    let garbage = br#"{"epoch":7,"rev":3,"devi"#;
    fs::write(data.file(INVENTORY_FILE), garbage).unwrap();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert!(reply.changed.is_empty());
    assert_eq!(reply.cursor.seq, 5, "the journal still holds the numbering");
    assert_ne!(reply.cursor.epoch, 7);
    drop(client);
    let log = twin.stop();

    assert!(log.contains("corrupt"), "startup should say so:\n{log}");
    let aside = corrupt_files(&data);
    assert_eq!(aside.len(), 1, "{:?}", data.names());
    assert!(log.contains(&aside[0]), "and say where it went:\n{log}");
    assert_eq!(fs::read(data.file(&aside[0])).unwrap(), garbage, "kept exactly as it was");
    assert_eq!(saved_inventory(&data).cursor().epoch, reply.cursor.epoch, "a good file replaced it");
}

#[test]
fn a_torn_event_line_is_tolerated_and_not_extended() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    wait_for_cursor(&mut client, |c| c.seq >= 3);
    drop(client);
    twin.stop();
    let saved = saved_inventory(&data).cursor();

    let torn = r#"{"seq":999999,"ts_ms":17"#;
    let newest = journal_paths(&data).pop().unwrap();
    OpenOptions::new().append(true).open(&newest).unwrap().write_all(torn.as_bytes()).unwrap();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert_eq!(reply.cursor, saved, "the torn line's number doesn't count");
    assert_eq!(reply.events.last().unwrap().seq, saved.seq, "the whole lines came back");
    assert!(!client.sync(Some(saved)).unwrap().full);
    drop(client);
    let log = twin.stop();
    assert!(log.contains("unreadable event line"), "{log}");

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    wait_for_cursor(&mut client, |c| c.seq > saved.seq);
    drop(client);
    twin.stop();

    let text: String = journal_paths(&data).iter().map(|p| fs::read_to_string(p).unwrap()).collect();
    let unreadable: Vec<&str> = text.lines().filter(|line| !line.is_empty() && serde_json::from_str::<Event>(line).is_err()).collect();
    assert_eq!(unreadable, [torn], "the fragment stays alone on its line, and no event was glued to it");
    assert_numbered_without_reuse(&journal_seqs(&data));
}

#[test]
fn seq_comes_back_from_the_journal_when_the_inventory_lags() {
    let data = TempDir::new();
    let mut inventory = Inventory::new(42);
    inventory.upsert(device("dev_a"));
    inventory.upsert(device("dev_b"));
    write_inventory(&data, &inventory);
    write_journal(&data, &numbered_events(40));
    assert_eq!(inventory.cursor().seq, 0, "the inventory on disk hasn't heard of these events");

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert_eq!(reply.cursor, Cursor { epoch: 42, rev: 2, seq: 40 });
    assert_eq!(reply.events.len(), 40, "recent events are restored");
    drop(client);
    twin.stop();

    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);
    let mut client = twin.client();
    wait_for_cursor(&mut client, |c| c.seq > 40);
    drop(client);
    twin.stop();
    assert_numbered_without_reuse(&journal_seqs(&data));
}

// ---------------------------------------------------------------------------
// Bad and greedy clients

fn frame(declared_length: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = declared_length.to_le_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn a_malformed_frame_costs_only_that_connection() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    let mut healthy = twin.client();
    let cursor = healthy.sync(None).unwrap().cursor;

    let bad_frames = [
        frame(4, b"nope"),
        frame(u32::MAX, b""),
        frame(MAX_FRAME_BYTES as u32 + 1, b""),
        frame(MAX_REQUEST_BYTES as u32 + 1, b""),
        frame(10, br#"{"Nope":1}"#),
    ];
    // More rounds than slots: a bad connection must give its slot back.
    for round in 0..MAX_CLIENTS * 2 {
        let mut bad = open_raw(&twin.pipe);
        bad.write_all(&bad_frames[round % bad_frames.len()]).unwrap();
        assert!(server_closes(bad), "round {round}");
        assert_eq!(healthy.sync(Some(cursor)).unwrap().cursor, cursor, "the other client is unaffected");
    }

    let mut cut_short = open_raw(&twin.pipe);
    cut_short.write_all(&frame(100, b"only this")).unwrap();
    drop(cut_short);
    healthy.sync(Some(cursor)).unwrap();

    drop(twin.client());
    drop(healthy);
    let log = twin.stop();
    assert!(log.contains("malformed or oversized"), "{log}");
}

#[test]
fn a_request_at_the_size_limit_is_read_and_one_past_it_is_not() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);
    drop(twin.client());

    let mut body = serde_json::to_vec(&Request::Sync { since: None }).unwrap();
    body.resize(MAX_REQUEST_BYTES, b' ');
    let mut accepted = open_raw(&twin.pipe);
    accepted.write_all(&frame(MAX_REQUEST_BYTES as u32, &body)).unwrap();
    assert!(matches!(wire::read_frame(&mut accepted).unwrap(), Response::Sync(_)));

    body.push(b' ');
    let mut refused = open_raw(&twin.pipe);
    let _ = refused.write_all(&frame(MAX_REQUEST_BYTES as u32 + 1, &body));
    assert!(server_closes(refused));
    drop(accepted);
    twin.stop();
}

#[test]
fn replies_far_larger_than_the_pipe_buffer_arrive_whole_and_impossible_ones_are_explained() {
    let twin = Twin::start(&unique_pipe(), &["--demo", "20000"]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert_eq!(reply.changed.len(), 20000);
    drop(client);
    twin.stop();

    // Over the protocol's 64 MiB limit. The client hears why, and the connection survives.
    let twin = Twin::start(&unique_pipe(), &["--demo", "300000"]);
    let mut client = twin.client();
    let error = client.sync(None).expect_err("this can't fit in one reply");
    assert!(error.to_string().contains("too large"), "{error}");
    client.sync(None).expect_err("still answering, with the same explanation");
    drop(client);
    twin.stop();
}

#[test]
fn more_clients_than_the_cap_do_not_wedge_the_server() {
    let data = TempDir::new();
    let mut twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg()]);

    let mut clients: Vec<ReadClient> = (0..MAX_CLIENTS).map(|_| twin.client()).collect();
    for _ in 0..5 {
        let error = ReadClient::connect_to(&twin.pipe).err().expect("the cap should turn this one away");
        assert_eq!(error.raw_os_error().map(|c| c as u32), Some(ERROR_PIPE_BUSY), "{error}");
    }
    for client in &mut clients {
        client.sync(None).expect("everyone already connected is still served");
    }

    clients.pop();
    let mut newcomer = twin.client();
    newcomer.sync(None).expect("a freed slot is usable again");

    let started = Instant::now();
    twin.close_stdin();
    let finished = twin.finish_within(STOP_LIMIT);
    assert!(finished.status.success(), "{}", finished.log);
    assert!(started.elapsed() < Duration::from_secs(5), "stopping at the cap took {:?}", started.elapsed());
    assert!(newcomer.sync(None).is_err());
}

#[test]
fn a_clean_stop_is_prompt_with_clients_in_any_state() {
    let data = TempDir::new();
    let mut twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--dev-churn", "5"]);

    let mut idle = twin.client();
    wait_for_cursor(&mut idle, |c| c.rev > 0);
    let mut half_a_frame = open_raw(&twin.pipe);
    half_a_frame.write_all(&[1, 0]).unwrap();
    // Asks for a reply and never reads it.
    let mut not_reading = open_raw(&twin.pipe);
    wire::write_frame(&mut not_reading, &Request::Sync { since: None }).unwrap();
    thread::sleep(Duration::from_millis(100));

    let started = Instant::now();
    twin.close_stdin();
    let finished = twin.finish_within(STOP_LIMIT);
    assert!(finished.status.success(), "exit {}:\n{}", finished.status, finished.log);
    assert!(started.elapsed() < Duration::from_secs(5), "stopping took {:?}", started.elapsed());
    assert!(finished.log.contains("Stopped cleanly."), "{}", finished.log);

    assert!(!data.file(LOCK_FILE).exists());
    assert!(data.names().iter().all(|n| n != "inventory.json.tmp"), "{:?}", data.names());
    assert_eq!(journal_seqs(&data).last(), Some(&saved_inventory(&data).cursor().seq), "the final save has everything");
}

const CTRL_C_TARGET: &str = "SENSE_TWIN_TEST_CTRL_C_TARGET";

#[test]
fn ctrl_c_stops_the_service_cleanly() {
    let data = TempDir::new();
    // Its own hidden console, so the signal reaches nothing else. Stdin stays open, so only
    // the Ctrl-C can have stopped it.
    let twin = Twin::spawn(&unique_pipe(), &["--data-dir", data.arg()], CREATE_NO_WINDOW);
    drop(twin.client());

    let pid = twin.child.lock().unwrap().id();
    // Sharing a console changes process-wide state, so this needs a process of its own.
    let helper = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "helper_sends_ctrl_c_to_the_console_of_a_process"])
        .env(CTRL_C_TARGET, pid.to_string())
        .output()
        .unwrap();
    assert!(helper.status.success() && String::from_utf8_lossy(&helper.stdout).contains("1 passed"), "{helper:?}");

    let finished = twin.finish_within(STOP_LIMIT);
    assert!(finished.status.success(), "exit {}:\n{}", finished.status, finished.log);
    assert!(finished.log.contains("Stopped cleanly."), "{}", finished.log);
    assert!(!finished.log.contains("Stdin closed"), "{}", finished.log);
    assert!(!data.file(LOCK_FILE).exists());
}

#[test]
#[ignore = "run by ctrl_c_stops_the_service_cleanly in a process of its own"]
fn helper_sends_ctrl_c_to_the_console_of_a_process() {
    let Ok(pid) = std::env::var(CTRL_C_TARGET) else { return };
    unsafe {
        FreeConsole();
        assert_ne!(AttachConsole(pid.parse().unwrap()), FALSE, "attach: {}", std::io::Error::last_os_error());
        // This process is on that console now and has to survive its own signal.
        assert_ne!(SetConsoleCtrlHandler(None, TRUE), FALSE);
        assert_ne!(GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0), FALSE, "signal: {}", std::io::Error::last_os_error());
    }
}

// ---------------------------------------------------------------------------
// Demo mode

#[test]
fn demo_mode_leaves_the_data_dir_empty_and_ignores_the_real_store() {
    // An empty folder stays empty.
    let empty = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--demo", "50", "--data-dir", empty.arg()]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert!(reply.demo, "invented data must be flagged");
    assert_eq!(reply.changed.len(), 50);
    assert!(reply.changed.iter().all(|d| d.name().starts_with("DEMO")));
    wait_for_cursor(&mut client, |c| c.rev > reply.cursor.rev);
    drop(client);
    twin.stop();
    assert_eq!(empty.names(), Vec::<String>::new());

    // A real store is neither served nor changed, not even its lock.
    let real = TempDir::new();
    let mut inventory = Inventory::new(42);
    inventory.upsert(device("dev_real"));
    write_inventory(&real, &inventory);
    write_journal(&real, &numbered_events(3));
    let before: Vec<(String, Vec<u8>)> = real.names().into_iter().map(|n| (n.clone(), fs::read(real.file(&n)).unwrap())).collect();

    let twin = Twin::start(&unique_pipe(), &["--demo", "5", "--data-dir", real.arg()]);
    let mut client = twin.client();
    let reply = client.sync(None).unwrap();
    assert!(reply.changed.iter().all(|d| d.id.0 != "dev_real"));
    assert_ne!(reply.cursor.epoch, 42);
    assert_eq!(reply.events.iter().filter(|e| e.seq > 100).count(), 0);
    drop(client);
    twin.stop();
    let after: Vec<(String, Vec<u8>)> = real.names().into_iter().map(|n| (n.clone(), fs::read(real.file(&n)).unwrap())).collect();
    assert_eq!(before, after);

    // A folder that doesn't exist isn't created.
    let parent = TempDir::new();
    let missing = parent.file("not-here");
    let twin = Twin::start(&unique_pipe(), &["--demo", "1", "--data-dir", missing.to_str().unwrap()]);
    drop(twin.client());
    twin.stop();
    assert!(!missing.exists());
}

#[test]
fn demo_mode_moves_on_about_once_a_second() {
    let twin = Twin::start(&unique_pipe(), &["--demo", "100"]);
    let mut client = twin.client();
    let first = client.sync(None).unwrap().cursor;
    // `DemoFactory::advance` may change nothing on a given step, but never more than one
    // device, and not every step is quiet. So this shows it moves, and isn't running flat out.
    thread::sleep(Duration::from_millis(6300));
    let later = client.sync(Some(first)).unwrap();
    assert!(!later.full);
    let changes = later.cursor.rev - first.rev;
    assert!((1..=8).contains(&changes), "{changes} changes in 6.3 seconds");
    drop(client);
    twin.stop();
}

// ---------------------------------------------------------------------------
// Command line

fn run_exe(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(EXE).args(args).stdin(Stdio::null()).output().expect("run SenseTwin");
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn the_hooks_role_exits_2_and_says_when_it_arrives() {
    let (code, stderr) = run_exe(&["--role", "hooks"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("M3"), "{stderr}");
}

#[test]
fn a_bad_command_line_is_refused_before_anything_starts() {
    let cases: &[&[&str]] = &[
        &["--role"],
        &["--role", "nope"],
        &["--role", "core", "--console", "--data_dir", "x"],
        &["--role", "core", "--demo", "5"],
        &["--role", "core", "--data-dir", "x"],
        &["--role", "core", "--console", "--demo", "many"],
        &["--role", "core", "--console", "--demo"],
        &["--role", "core", "--console", "--pipe", r"\\server\pipe\x"],
        &["--role", "core", "--console", "--demo", "1", "--dev-churn", "5"],
        &["--role", "core", "--console", "--dev-churn", "5"],
    ];
    for args in cases {
        let (code, stderr) = run_exe(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
    }
}

#[test]
fn a_bad_sddl_stops_startup() {
    let data = TempDir::new();
    let twin = Twin::start(&unique_pipe(), &["--data-dir", data.arg(), "--pipe-sddl", "not sddl"]);
    let refused = twin.finish_within(READY_LIMIT);
    assert_eq!(refused.status.code(), Some(4), "{}", refused.log);
    assert!(refused.log.contains("security descriptor"), "{}", refused.log);
}

#[test]
fn outside_the_service_manager_role_core_explains_itself() {
    // Only asks Windows whether this process is a service. It changes nothing.
    let (code, stderr) = run_exe(&["--role", "core"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("--console"), "{stderr}");
}

#[test]
fn the_default_data_folder_is_never_the_services() {
    let usage = run_exe(&[]).1;
    assert!(usage.contains(r"%LOCALAPPDATA%\SenseSim\twin-console"), "{usage}");
    assert!(usage.contains("webapi-mode"), "the M0 commands are still listed");
}
