//! Puts the service together. The service manager and console mode differ only in what
//! tells it to stop, so both run this.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use twin_proto::model::{Coverage, Device, DeviceClass, DeviceId, EventKind, Identity, Liveness, NewEvent, Severity};
use windows_sys::Win32::Foundation::TRUE;
use windows_sys::Win32::System::Console::{CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler};
use windows_sys::core::BOOL;

use crate::pipe::{self, BindError, ClientLimits, PipeConfig};
use crate::state::{ChangeSet, StateHandle, StateOwner};
use crate::stop::Stop;
use crate::{clock, say, store};

const SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const CHURN_DEVICES: usize = 3000;

pub enum Source {
    Store(PathBuf),
    Demo(usize),
}

pub struct Config {
    pub pipe: PipeConfig,
    pub source: Source,
    /// Development only: invent changes at this interval and save just as often, so tests
    /// can kill the process while it is really writing.
    pub dev_churn: Option<Duration>,
}

#[derive(Debug)]
pub enum StartError {
    /// Another process holds the pipe name.
    PipeTaken(String),
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PipeTaken(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<io::Error> for StartError {
    fn from(e: io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

pub struct Running {
    stop: Stop,
    state: StateOwner,
    server: pipe::Server,
    churn: Option<JoinHandle<()>>,
}

impl Running {
    pub fn start(config: Config, stop: Stop) -> Result<Self, StartError> {
        // The name comes first: if someone is squatting on it, the store stays untouched.
        let bound = pipe::bind(&config.pipe).map_err(|e| match e {
            BindError::NameTaken(why) => StartError::PipeTaken(why),
            BindError::Failed(why) => StartError::Failed(why),
        })?;

        let (state, debounce_note) = match config.source {
            Source::Store(dir) => {
                let debounce = config.dev_churn.unwrap_or(SAVE_DEBOUNCE);
                (StateOwner::start(store::open(&dir)?, debounce)?, format!("store {}", dir.display()))
            }
            Source::Demo(devices) => (StateOwner::start_demo(devices)?, format!("demo data, {devices} invented devices, nothing saved")),
        };
        let churn = match config.dev_churn {
            Some(period) => Some(spawn_churn(state.handle(), stop.clone(), period)?),
            None => None,
        };
        let server = bound.serve(state.handle(), stop.clone(), ClientLimits::default())?;
        say!("Serving {} from {debounce_note}.", config.pipe.name);
        Ok(Self { stop, state, server, churn })
    }

    pub fn state(&self) -> StateHandle {
        self.state.handle()
    }

    /// Readers go first, so the state can't change after the final save.
    pub fn shutdown(self) -> Result<(), String> {
        self.stop.signal();
        if let Some(churn) = self.churn {
            let _ = churn.join();
        }
        self.server.join();
        self.state.shutdown()
    }
}

/// A half-dead service is worse than a stopped one: the service manager restarts a process
/// that exits, but not one whose state thread died while its pipes stay open.
pub fn exit_on_panic() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        say!("Stopping after an internal error: {info}");
        default(info);
        std::process::abort();
    }));
}

/// Runs in the foreground until stdin closes or Ctrl-C arrives.
pub fn run_console(config: Config) -> Result<(), StartError> {
    exit_on_panic();
    let stop = Stop::new()?;
    watch_console_events(stop.clone());
    watch_stdin(stop.clone())?;

    let running = Running::start(config, stop.clone())?;
    say!("Running in the foreground. Close stdin or press Ctrl-C to stop.");
    stop.wait(None);
    say!("Stopping.");
    running.shutdown().map_err(StartError::Failed)?;
    say!("Stopped cleanly.");
    Ok(())
}

fn watch_stdin(stop: Stop) -> io::Result<()> {
    thread::Builder::new().name("twin-stdin".into()).spawn(move || {
        let mut sink = [0u8; 256];
        let mut stdin = io::stdin();
        while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
        say!("Stdin closed.");
        stop.signal();
    })?;
    Ok(())
}

static CONSOLE_STOP: OnceLock<Stop> = OnceLock::new();

fn watch_console_events(stop: Stop) {
    let _ = CONSOLE_STOP.set(stop);
    unsafe {
        // A parent's "ignore Ctrl-C" setting is inherited, and it would silence the handler.
        SetConsoleCtrlHandler(None, 0);
        SetConsoleCtrlHandler(Some(on_console_event), TRUE);
    }
}

unsafe extern "system" fn on_console_event(kind: u32) -> BOOL {
    if let Some(stop) = CONSOLE_STOP.get() {
        stop.signal();
    }
    // Windows ends the process as soon as these handlers return. Exiting from main ends this
    // thread sooner, so this only ever waits as long as the final save takes.
    if matches!(kind, CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
        thread::sleep(Duration::from_secs(5));
    }
    TRUE
}

fn spawn_churn(state: StateHandle, stop: Stop, period: Duration) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("twin-dev-churn".into()).spawn(move || {
        let mut devices: Vec<Device> = (0..CHURN_DEVICES).map(churn_device).collect();
        if let Err(e) = state.apply(ChangeSet { devices: devices.clone(), events: Vec::new() }) {
            say!("Dev churn couldn't add its devices: {e}");
        }
        let mut step = 0;
        while !stop.wait(Some(period)) {
            let now = clock::now_ms();
            let device = &mut devices[step % CHURN_DEVICES];
            device.last_seen_ms = Some(now);
            let event = NewEvent::new(now, EventKind::Appeared, Severity::Info, Some(device.id.clone()));
            if let Err(e) = state.apply(ChangeSet { devices: vec![device.clone()], events: vec![event] }) {
                say!("Dev churn: {e}");
            }
            step += 1;
        }
    })
}

fn churn_device(n: usize) -> Device {
    Device {
        id: DeviceId(format!("dev_churn{n:06}")),
        class: DeviceClass::Unknown,
        liveness: Liveness::Up,
        coverage: Coverage::Counted,
        identity: Identity { station_name: Some(format!("CHURN-{n}")), serial: Some(format!("CHURN{n:06}")), ..Identity::default() },
        addresses: Vec::new(),
        mode: None,
        parent: None,
        last_seen_ms: None,
        rev: 0,
    }
}
