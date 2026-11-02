//! The server end of the read pipe.
//!
//! Every instance the pipe allows is created at startup and kept for the life of the
//! service, one thread each. That is the connection cap, and it also closes two holes: the
//! name is never released, so nobody can take it over between clients, and no instance is
//! left for anyone to add a pipe end of their own and answer other viewers.
//!
//! All I/O is overlapped, and every wait includes the stop signal, so nothing a client does
//! or doesn't do can hold up a shutdown.

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null_mut;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use twin_proto::wire::{self, PROTOCOL_VERSION, READ_PIPE, Request, Response};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

use crate::say;
use crate::state::StateHandle;
use crate::stop::{Stop, manual_reset_event};
use crate::win::{self, SecurityDescriptor};

/// A handful of viewers at two connections each, with room to spare.
pub const MAX_CLIENTS: usize = 32;
/// A request is a cursor, so this is generous. The protocol's own limit is sized for replies.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

const BUFFER_BYTES: u32 = 64 * 1024;
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(1);

/// How long a connected client may hold its slot without progress. With a cap on clients,
/// one that hangs would otherwise keep a slot for as long as its process lives.
#[derive(Clone, Copy)]
pub struct ClientLimits {
    pub idle: Duration,
    pub unread_reply: Duration,
}

impl Default for ClientLimits {
    /// A viewer polls every second and reconnects when it has to, so these are generous.
    fn default() -> Self {
        Self { idle: Duration::from_secs(120), unread_reply: Duration::from_secs(30) }
    }
}

pub struct PipeConfig {
    pub name: String,
    pub sddl: String,
    /// Given with `--pipe-sddl` for tests, so it may not grant this process anything.
    pub sddl_is_explicit: bool,
}

#[derive(Debug)]
pub enum BindError {
    /// Someone else holds the name. They could be impersonating the service to viewers.
    NameTaken(String),
    Failed(String),
}

/// `--pipe` takes a full path or a bare name. Nothing but a local pipe path is accepted.
pub fn normalize_name(name: &str) -> Result<String, String> {
    const PREFIX: &str = r"\\.\pipe\";
    let leaf = name.strip_prefix(PREFIX).unwrap_or(name);
    if leaf.is_empty() || leaf.contains(['\\', '/']) {
        return Err(format!("{name:?} isn't a local pipe name. Use a bare name or {PREFIX}name."));
    }
    Ok(format!("{PREFIX}{leaf}"))
}

pub fn default_name() -> String {
    READ_PIPE.to_string()
}

/// A pipe with all its instances created and nobody served yet.
pub struct Bound {
    instances: Vec<OwnedHandle>,
}

/// Takes the name. Fails if anyone already holds it, so run this before touching the store.
pub fn bind(config: &PipeConfig) -> Result<Bound, BindError> {
    let security = SecurityDescriptor::from_sddl(&config.sddl).map_err(|e| BindError::Failed(format!("The pipe's security descriptor {:?} is invalid: {e}", config.sddl)))?;
    let attributes = security.attributes();
    let name = win::wide(&config.name);

    let first = create_instance(&name, &attributes, FILE_FLAG_FIRST_PIPE_INSTANCE).map_err(|e| match e.raw_os_error().map(|code| code as u32) {
        Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY) => BindError::NameTaken(format!("The pipe {} is already in use by another process. Refusing to start, because viewers could be talking to that process instead of this service.", config.name)),
        _ => BindError::Failed(format!("Can't create the pipe {}: {e}", config.name)),
    })?;

    let mut instances = vec![first];
    while instances.len() < MAX_CLIENTS {
        match create_instance(&name, &attributes, 0) {
            Ok(instance) => instances.push(instance),
            // Adding instances needs FILE_CREATE_PIPE_INSTANCE, which a test's descriptor may
            // not give this process.
            Err(e) if config.sddl_is_explicit => {
                say!("--pipe-sddl doesn't let this process add pipe instances ({e}), so {} client(s) at a time.", instances.len());
                break;
            }
            Err(e) => return Err(BindError::Failed(format!("Can't create instance {} of the pipe {}: {e}", instances.len() + 1, config.name))),
        }
    }
    Ok(Bound { instances })
}

fn create_instance(name: &[u16], security: &SECURITY_ATTRIBUTES, extra_flags: u32) -> io::Result<OwnedHandle> {
    let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | extra_flags;
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    let handle = unsafe { CreateNamedPipeW(name.as_ptr(), open_mode, pipe_mode, MAX_CLIENTS as u32, BUFFER_BYTES, BUFFER_BYTES, 0, security) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

impl Bound {
    pub fn serve(self, state: StateHandle, stop: Stop, limits: ClientLimits) -> io::Result<Server> {
        let mut threads = Vec::with_capacity(self.instances.len());
        for (slot, instance) in self.instances.into_iter().enumerate() {
            let (state, stop) = (state.clone(), stop.clone());
            threads.push(thread::Builder::new().name(format!("twin-pipe-{slot}")).spawn(move || run_slot(instance, state, stop, limits))?);
        }
        Ok(Server { threads })
    }
}

pub struct Server {
    threads: Vec<JoinHandle<()>>,
}

impl Server {
    /// Waits for every slot to finish. Signal the stop first.
    pub fn join(self) {
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

fn run_slot(instance: OwnedHandle, state: StateHandle, stop: Stop, limits: ClientLimits) {
    let event = match manual_reset_event() {
        Ok(event) => event,
        Err(e) => {
            say!("A pipe slot couldn't start: {e}");
            return;
        }
    };
    let io = PipeIo { pipe: instance.as_raw_handle(), event: event.as_raw_handle(), stop: &stop, limits };
    while !stop.is_set() {
        match io.accept() {
            Ok(()) => {
                serve_client(&io, &state, &stop);
                io.disconnect();
            }
            Err(e) if is_stop(&e) => break,
            Err(e) if client_already_gone(&e) => io.disconnect(),
            Err(e) => {
                say!("A pipe slot failed to accept a client: {e}");
                io.disconnect();
                stop.wait(Some(RETRY_AFTER_FAILURE));
            }
        }
    }
}

/// Only a bad frame or a dead client ends here, and either way only this connection is lost.
fn serve_client(io: &PipeIo<'_>, state: &StateHandle, stop: &Stop) {
    let mut conn = Conn { io };
    while !stop.is_set() {
        let request = match read_request(&mut conn) {
            Ok(request) => request,
            Err(e) => {
                // The parser's message can quote what the client sent, so don't log it.
                if e.kind() == io::ErrorKind::InvalidData {
                    say!("Closed a read pipe connection that sent a malformed or oversized message.");
                }
                return;
            }
        };
        let response = match request {
            Request::Hello { .. } => Response::Hello { protocol: PROTOCOL_VERSION, service_version: env!("CARGO_PKG_VERSION").to_string() },
            Request::Sync { since } => match state.sync(since) {
                Ok(reply) => Response::Sync(reply),
                Err(_) => return,
            },
        };
        match wire::write_frame(&mut conn, &response) {
            Ok(()) => {}
            // Too big to send, so nothing has gone out yet. Say so rather than hang up silently.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let message = "The inventory is too large to send in one reply.".to_string();
                if wire::write_frame(&mut conn, &Response::Error { message }).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// The length is checked here, tighter than `read_frame` would, because a request has no
/// reason to be megabytes long and a client shouldn't be able to make us hold that much.
fn read_request(conn: &mut Conn<'_, '_>) -> io::Result<Request> {
    let mut header = [0u8; 4];
    conn.read_exact(&mut header)?;
    if u32::from_le_bytes(header) as usize > MAX_REQUEST_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "request too large"));
    }
    wire::read_frame(&mut io::Cursor::new(header).chain(conn))
}

/// One pipe instance's overlapped I/O. The kernel holds pointers into `OVERLAPPED` and the
/// buffer until an operation ends, so every path here waits for that before returning.
struct PipeIo<'a> {
    pipe: RawHandle,
    event: RawHandle,
    stop: &'a Stop,
    limits: ClientLimits,
}

impl PipeIo<'_> {
    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED { hEvent: self.event, ..unsafe { std::mem::zeroed() } }
    }

    fn accept(&self) -> io::Result<()> {
        let mut overlapped = self.overlapped();
        if unsafe { ConnectNamedPipe(self.pipe, &mut overlapped) } != 0 {
            return Ok(());
        }
        match unsafe { GetLastError() } {
            ERROR_PIPE_CONNECTED => Ok(()),
            ERROR_IO_PENDING => self.finish(&overlapped, INFINITE).map(|_| ()),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }

    fn disconnect(&self) {
        unsafe { DisconnectNamedPipe(self.pipe) };
    }

    fn finish(&self, overlapped: &OVERLAPPED, limit_ms: u32) -> io::Result<u32> {
        let waits = [self.event, self.stop.raw()];
        let woke = unsafe { WaitForMultipleObjects(waits.len() as u32, waits.as_ptr(), 0, limit_ms) };
        if woke != WAIT_OBJECT_0 {
            unsafe { CancelIoEx(self.pipe, overlapped) };
            let mut ignored = 0;
            unsafe { GetOverlappedResult(self.pipe, overlapped, &mut ignored, 1) };
            return Err(if woke == WAIT_OBJECT_0 + 1 { stopping() } else { io::ErrorKind::TimedOut.into() });
        }
        let mut transferred = 0;
        if unsafe { GetOverlappedResult(self.pipe, overlapped, &mut transferred, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred)
    }
}

// ConnectionAborted, not Interrupted, because `read_exact` retries on Interrupted.
fn stopping() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "the service is stopping")
}

fn is_stop(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ConnectionAborted
}

fn client_already_gone(error: &io::Error) -> bool {
    matches!(error.raw_os_error().map(|code| code as u32), Some(ERROR_NO_DATA | ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED))
}

/// Blocking reads and writes that give up on stop or on a client that went quiet.
struct Conn<'a, 'b> {
    io: &'a PipeIo<'b>,
}

impl Read for Conn<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut overlapped = self.io.overlapped();
        let length = buffer.len().min(u32::MAX as usize) as u32;
        let ok = unsafe { ReadFile(self.io.pipe, buffer.as_mut_ptr(), length, null_mut(), &mut overlapped) };
        if ok == 0 {
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE => return Ok(0),
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        match self.io.finish(&overlapped, self.io.limits.idle.as_millis() as u32) {
            Ok(read) => Ok(read as usize),
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl Write for Conn<'_, '_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut overlapped = self.io.overlapped();
        let length = buffer.len().min(u32::MAX as usize) as u32;
        let ok = unsafe { WriteFile(self.io.pipe, buffer.as_ptr(), length, null_mut(), &mut overlapped) };
        if ok == 0 {
            match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {}
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
        self.io.finish(&overlapped, self.io.limits.unread_reply.as_millis() as u32).map(|written| written as usize)
    }

    /// Nothing to do, and `FlushFileBuffers` would wait for the client to read everything.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::sddl;
    use crate::state::StateOwner;

    /// Serves `devices` demo devices until dropped.
    struct TestServer {
        name: String,
        stop: Stop,
        server: Option<Server>,
        state: Option<StateOwner>,
    }

    impl TestServer {
        fn start(devices: usize, limits: ClientLimits) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(r"\\.\pipe\SenseTwin.Unit.{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            let sddl = sddl::console_pipe(&win::current_user_sid().unwrap()).unwrap();
            let config = PipeConfig { name: name.clone(), sddl, sddl_is_explicit: false };
            let bound = bind(&config).expect("bind");
            let stop = Stop::new().unwrap();
            let state = StateOwner::start_demo(devices).unwrap();
            let server = bound.serve(state.handle(), stop.clone(), limits).unwrap();
            Self { name, stop, server: Some(server), state: Some(state) }
        }

        fn connect(&self) -> File {
            OpenOptions::new().read(true).write(true).open(&self.name).unwrap()
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.stop.signal();
            self.server.take().unwrap().join();
            let _ = self.state.take().unwrap().shutdown();
        }
    }

    /// In a thread, so a server that keeps it open fails this rather than blocking forever.
    fn assert_closed_soon(mut connection: File) {
        let (closed, wait) = crossbeam_channel::bounded(1);
        thread::spawn(move || {
            let _ = connection.read_to_end(&mut Vec::new());
            let _ = closed.send(());
        });
        wait.recv_timeout(Duration::from_secs(20)).expect("the server kept the connection open");
    }

    #[test]
    fn a_silent_client_loses_its_slot_and_others_are_unaffected() {
        let limits = ClientLimits { idle: Duration::from_millis(50), ..ClientLimits::default() };
        let server = TestServer::start(1, limits);
        assert_closed_soon(server.connect());
        // A frame cut off partway is just as silent.
        let mut half = server.connect();
        half.write_all(&[1, 0]).unwrap();
        assert_closed_soon(half);

        let mut next = server.connect();
        wire::write_frame(&mut next, &Request::Hello { protocol: PROTOCOL_VERSION }).unwrap();
        assert!(matches!(wire::read_frame(&mut next).unwrap(), Response::Hello { .. }));
    }

    #[test]
    fn a_client_that_never_reads_its_reply_loses_its_slot() {
        // Too big to fit in the pipe's buffer, so the server's write can't finish.
        let limits = ClientLimits { unread_reply: Duration::from_millis(50), ..ClientLimits::default() };
        let server = TestServer::start(5000, limits);
        let mut greedy = server.connect();
        wire::write_frame(&mut greedy, &Request::Sync { since: None }).unwrap();

        // The slot comes back only once the server has given up on the write.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut others: Vec<File> = (0..MAX_CLIENTS - 1).map(|_| server.connect()).collect();
        let freed = loop {
            match OpenOptions::new().read(true).write(true).open(&server.name) {
                Ok(file) => break file,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => panic!("the slot never came back: {e}"),
            }
        };
        others.push(freed);
    }

    #[test]
    fn a_pipe_name_is_local_or_refused() {
        assert_eq!(normalize_name("SenseTwin.Dev").unwrap(), r"\\.\pipe\SenseTwin.Dev");
        assert_eq!(normalize_name(r"\\.\pipe\SenseTwin.Dev").unwrap(), r"\\.\pipe\SenseTwin.Dev");
        for bad in ["", r"\\.\pipe\", r"\\server\pipe\x", r"a\b", "a/b"] {
            assert!(normalize_name(bad).is_err(), "{bad:?}");
        }
    }
}
