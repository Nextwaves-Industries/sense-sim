//! Where the viewer's device list comes from: the service over the read pipe, or the demo
//! factory. Runs on its own thread so a slow pipe can't stall a frame.
//!
//! This only ever reads. The viewer has no way to start a scan or send a command, here or
//! anywhere else: `twin_proto` has no such message, and `twin_control_proto` isn't linked.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use twin_proto::demo::DemoFactory;
use twin_proto::inventory::{Cursor, SyncReply};
use twin_proto::read_client::ReadClient;
use twin_proto::wire::READ_PIPE;

const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LinkStatus {
    #[default]
    Starting,
    /// Nothing answers on the pipe: the service isn't installed or isn't running.
    NoService,
    Connected,
    /// It was connected and stopped answering. What's on screen may be out of date.
    Lost(String),
    Demo,
}

impl LinkStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Starting => "Connecting",
            Self::NoService => "No service",
            Self::Connected => "Connected",
            Self::Lost(_) => "Connection lost",
            Self::Demo => "Demo data",
        }
    }
}

pub enum Update {
    Status(LinkStatus),
    Sync(SyncReply),
    /// Forget everything: the demo ended.
    Clear,
}

pub enum Command {
    /// Show this many invented devices, or `None` to go back to the service.
    Demo(Option<usize>),
}

pub struct Link {
    pub commands: Sender<Command>,
    pub updates: Receiver<Update>,
}

impl Link {
    pub fn start() -> Self {
        let (commands, command_rx) = unbounded();
        let (update_tx, updates) = unbounded();
        // `SENSESIM_TWIN_PIPE`: talk to a service run in the foreground under another name.
        let pipe = std::env::var("SENSESIM_TWIN_PIPE").unwrap_or_else(|_| READ_PIPE.to_string());
        let spawned = std::thread::Builder::new().name("twin-link".into()).spawn(move || run(&pipe, &command_rx, &update_tx));
        if let Err(e) = spawned {
            bevy::log::error!("couldn't start the twin link: {e}");
        }
        Self { commands, updates }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn run(pipe: &str, commands: &Receiver<Command>, updates: &Sender<Update>) {
    let mut demo: Option<DemoFactory> = None;
    let mut client: Option<ReadClient> = None;
    let mut cursor: Option<Cursor> = None;
    let mut last_status = LinkStatus::Starting;
    let mut was_connected = false;

    let set_status = |status: LinkStatus, last: &mut LinkStatus| {
        if status != *last {
            *last = status.clone();
            let _ = updates.send(Update::Status(status));
        }
    };
    loop {
        // Dropping the app's end of the channel ends this thread.
        match commands.recv_timeout(POLL) {
            Ok(Command::Demo(Some(count))) => {
                demo = Some(DemoFactory::new(count));
                client = None;
                cursor = None;
                was_connected = false;
                set_status(LinkStatus::Demo, &mut last_status);
            }
            Ok(Command::Demo(None)) => {
                demo = None;
                cursor = None;
                let _ = updates.send(Update::Clear);
                set_status(LinkStatus::Starting, &mut last_status);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let reply = if let Some(factory) = &mut demo {
            factory.advance(now_ms());
            Ok(factory.sync(cursor))
        } else {
            if client.is_none() {
                match ReadClient::connect_to(pipe) {
                    Ok(connected) => {
                        client = Some(connected);
                        was_connected = true;
                        set_status(LinkStatus::Connected, &mut last_status);
                    }
                    Err(e) if was_connected => set_status(LinkStatus::Lost(e.to_string()), &mut last_status),
                    Err(_) => set_status(LinkStatus::NoService, &mut last_status),
                }
            }
            match &mut client {
                Some(connected) => connected.sync(cursor),
                None => continue,
            }
        };
        match reply {
            Ok(reply) => {
                cursor = Some(reply.cursor);
                if reply.full || !reply.changed.is_empty() || !reply.events.is_empty() {
                    let _ = updates.send(Update::Sync(reply));
                }
            }
            Err(e) => {
                client = None;
                set_status(LinkStatus::Lost(e.to_string()), &mut last_status);
            }
        }
    }
}
