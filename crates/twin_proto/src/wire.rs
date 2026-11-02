//! Messages on `\\.\pipe\SenseTwin.Read`: a little-endian `u32` length, then that much JSON.
//!
//! This pipe only reads. There is no request here that changes anything, on the service or
//! on a device, and the viewer can only ever speak this protocol.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::inventory::{Cursor, SyncReply};

pub const READ_PIPE: &str = r"\\.\pipe\SenseTwin.Read";
pub const PROTOCOL_VERSION: u32 = 1;
/// Room for tens of thousands of devices in a full reply.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Hello { protocol: u32 },
    Sync { since: Option<Cursor> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Hello { protocol: u32, service_version: String },
    Sync(SyncReply),
    Error { message: String },
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    // One write per frame, so a message-mode pipe never sees a split header.
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame)?;
    writer.flush()
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    // Read through `take` rather than allocating up front, so a declared length can't force
    // an allocation the sender never backs with data.
    let mut body = Vec::new();
    reader.take(length as u64).read_to_end(&mut body)?;
    if body.len() != length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let mut wire = Vec::new();
        let sent = Request::Sync { since: Some(Cursor { epoch: 1, rev: 2, seq: 3, line_rev: 4 }) };
        write_frame(&mut wire, &sent).unwrap();
        write_frame(&mut wire, &Request::Hello { protocol: PROTOCOL_VERSION }).unwrap();

        let mut reader = &wire[..];
        assert_eq!(read_frame::<Request>(&mut reader).unwrap(), sent);
        assert_eq!(read_frame::<Request>(&mut reader).unwrap(), Request::Hello { protocol: PROTOCOL_VERSION });
    }

    #[test]
    fn an_absurd_length_is_refused_before_reading_on() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(read_frame::<Request>(&mut &wire[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_short_or_invalid_message_is_an_error() {
        let mut short = Vec::new();
        short.extend_from_slice(&100u32.to_le_bytes());
        short.extend_from_slice(b"only this");
        assert_eq!(read_frame::<Request>(&mut &short[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);

        let mut invalid = Vec::new();
        invalid.extend_from_slice(&4u32.to_le_bytes());
        invalid.extend_from_slice(b"nope");
        assert_eq!(read_frame::<Request>(&mut &invalid[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
