//! The viewer's end of the read pipe.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;

use crate::inventory::{Cursor, SyncReply};
use crate::wire::{self, PROTOCOL_VERSION, READ_PIPE, Request, Response};

/// `SECURITY_IDENTIFICATION`: whoever answers on this pipe can learn who we are but can't
/// act as us. Without it, a process squatting on the pipe name could impersonate the user.
const SECURITY_IDENTIFICATION: u32 = 1 << 16;

pub struct ReadClient {
    pipe: File,
}

impl ReadClient {
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(READ_PIPE)
    }

    /// Connects and checks that both ends speak the same protocol.
    pub fn connect_to(pipe_name: &str) -> io::Result<Self> {
        let pipe = OpenOptions::new().read(true).write(true).security_qos_flags(SECURITY_IDENTIFICATION).open(pipe_name)?;
        let mut client = Self { pipe };
        match client.ask(&Request::Hello { protocol: PROTOCOL_VERSION })? {
            Response::Hello { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(client),
            Response::Hello { protocol, .. } => Err(io::Error::new(io::ErrorKind::InvalidData, format!("the service speaks protocol {protocol}, this app speaks {PROTOCOL_VERSION}"))),
            other => Err(unexpected(other)),
        }
    }

    pub fn sync(&mut self, since: Option<Cursor>) -> io::Result<SyncReply> {
        match self.ask(&Request::Sync { since })? {
            Response::Sync(reply) => Ok(reply),
            other => Err(unexpected(other)),
        }
    }

    fn ask(&mut self, request: &Request) -> io::Result<Response> {
        wire::write_frame(&mut self.pipe, request)?;
        wire::read_frame(&mut self.pipe)
    }
}

fn unexpected(response: Response) -> io::Error {
    let what = match response {
        Response::Error { message } => message,
        _ => "unexpected reply".to_string(),
    };
    io::Error::new(io::ErrorKind::InvalidData, what)
}
