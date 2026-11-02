//! Reads one HTTP/1.1 response from a device, within fixed limits.
//!
//! `ureq` can't take a custom TLS verifier without its unstable transport API, so the Web
//! API client writes its own request and reads the reply here. Siemens CPUs answer with
//! `transfer-encoding: chunked`, and the sender is untrusted, so every size is bounded.

use std::io::{self, Read};

pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Room for headers and chunk framing on top of the body limit.
const MAX_WIRE_BYTES: usize = MAX_BODY_BYTES + 64 * 1024;
const MAX_HEADERS: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Malformed(&'static str),
    TooLarge,
}

enum Framing {
    Chunked,
    Length(usize),
    UntilClose,
}

struct Incoming<'a, R> {
    reader: &'a mut R,
    buf: Vec<u8>,
    closed: bool,
}

impl<R: Read> Incoming<'_, R> {
    /// Reads more. Returns false once the peer has closed.
    fn fill(&mut self) -> Result<bool, ReadError> {
        if self.closed {
            return Ok(false);
        }
        if self.buf.len() > MAX_WIRE_BYTES {
            return Err(ReadError::TooLarge);
        }
        let mut chunk = [0u8; 8192];
        match self.reader.read(&mut chunk) {
            Ok(0) => {
                self.closed = true;
                Ok(false)
            }
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            // Devices often drop TCP without a TLS close. Framing or the JSON parser
            // catches a reply that really was cut short.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.closed = true;
                Ok(false)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(true),
            Err(e) => Err(ReadError::Io(e)),
        }
    }

    /// Makes sure `end` bytes have arrived.
    fn need(&mut self, end: usize) -> Result<(), ReadError> {
        while self.buf.len() < end {
            if !self.fill()? {
                return Err(ReadError::Malformed("connection closed before the reply was complete"));
            }
        }
        Ok(())
    }
}

pub fn read_response(reader: &mut impl Read) -> Result<Response, ReadError> {
    let mut input = Incoming { reader, buf: Vec::new(), closed: false };
    let (status, framing, body_start) = read_head(&mut input)?;
    let body = match framing {
        Framing::Chunked => read_chunked(&mut input, body_start)?,
        Framing::Length(len) => {
            if len > MAX_BODY_BYTES {
                return Err(ReadError::TooLarge);
            }
            input.need(body_start + len)?;
            input.buf[body_start..body_start + len].to_vec()
        }
        Framing::UntilClose => {
            while input.fill()? {}
            if input.buf.len() - body_start > MAX_BODY_BYTES {
                return Err(ReadError::TooLarge);
            }
            input.buf[body_start..].to_vec()
        }
    };
    Ok(Response { status, body })
}

fn read_head<R: Read>(input: &mut Incoming<'_, R>) -> Result<(u16, Framing, usize), ReadError> {
    loop {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut response = httparse::Response::new(&mut headers);
        match response.parse(&input.buf) {
            Ok(httparse::Status::Complete(end)) => {
                let status = response.code.ok_or(ReadError::Malformed("no status code"))?;
                return Ok((status, framing_of(response.headers)?, end));
            }
            Ok(httparse::Status::Partial) => {}
            Err(_) => return Err(ReadError::Malformed("invalid HTTP response head")),
        }
        if !input.fill()? {
            return Err(ReadError::Malformed("connection closed before the reply headers"));
        }
    }
}

fn framing_of(headers: &[httparse::Header<'_>]) -> Result<Framing, ReadError> {
    let value = |name: &str| headers.iter().find(|h| h.name.eq_ignore_ascii_case(name)).map(|h| String::from_utf8_lossy(h.value).to_ascii_lowercase());
    if value("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return Ok(Framing::Chunked);
    }
    match value("content-length") {
        Some(len) => len.trim().parse().map(Framing::Length).map_err(|_| ReadError::Malformed("invalid content-length")),
        None => Ok(Framing::UntilClose),
    }
}

fn read_chunked<R: Read>(input: &mut Incoming<'_, R>, mut at: usize) -> Result<Vec<u8>, ReadError> {
    let mut body = Vec::new();
    loop {
        let (consumed, size) = loop {
            match httparse::parse_chunk_size(&input.buf[at..]) {
                Ok(httparse::Status::Complete(found)) => break found,
                Ok(httparse::Status::Partial) => {
                    if !input.fill()? {
                        return Err(ReadError::Malformed("connection closed inside a chunk header"));
                    }
                }
                Err(_) => return Err(ReadError::Malformed("invalid chunk size")),
            }
        };
        at += consumed;
        if size == 0 {
            return Ok(body);
        }
        let size = usize::try_from(size).map_err(|_| ReadError::TooLarge)?;
        if size > MAX_BODY_BYTES - body.len() {
            return Err(ReadError::TooLarge);
        }
        input.need(at + size + 2)?;
        body.extend_from_slice(&input.buf[at..at + size]);
        if &input.buf[at + size..at + size + 2] != b"\r\n" {
            return Err(ReadError::Malformed("chunk not terminated"));
        }
        at += size + 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands over one byte per read, so every split point gets exercised.
    struct OneByteAtATime<'a>(&'a [u8]);

    impl Read for OneByteAtATime<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else { return Ok(0) };
            out[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    fn read(wire: &[u8]) -> Result<Response, ReadError> {
        read_response(&mut OneByteAtATime(wire))
    }

    #[test]
    fn chunked_reply_as_siemens_sends_it() {
        let wire = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n7;ext=1\r\n world!\r\n0\r\n\r\n";
        assert_eq!(read(wire).unwrap(), Response { status: 200, body: b"hello world!".to_vec() });
    }

    #[test]
    fn header_names_and_values_are_case_insensitive() {
        let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n";
        assert_eq!(read(wire).unwrap().body, b"ok");
    }

    #[test]
    fn content_length_reply() {
        let wire = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\n\r\nnoEXTRA";
        assert_eq!(read(wire).unwrap(), Response { status: 403, body: b"no".to_vec() });
    }

    #[test]
    fn reply_without_framing_runs_until_close() {
        assert_eq!(read(b"HTTP/1.1 200 OK\r\n\r\nall of it").unwrap().body, b"all of it");
    }

    #[test]
    fn truncated_replies_are_errors_not_short_bodies() {
        for wire in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"[..],
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\nA\r\nshort",
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5",
            b"HTTP/1.1 200 OK\r\nContent-Le",
        ] {
            assert!(matches!(read(wire), Err(ReadError::Malformed(_))), "{}", String::from_utf8_lossy(wire));
        }
    }

    #[test]
    fn malformed_framing_is_rejected() {
        for wire in [
            &b"not http at all\r\n\r\n"[..],
            b"HTTP/1.1 200 OK\r\nContent-Length: lots\r\n\r\n",
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n2\r\nokXX0\r\n\r\n",
        ] {
            assert!(matches!(read(wire), Err(ReadError::Malformed(_))), "{}", String::from_utf8_lossy(wire));
        }
    }

    #[test]
    fn oversized_declarations_fail_before_waiting_for_the_data() {
        let by_length = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1);
        assert!(matches!(read(by_length.as_bytes()), Err(ReadError::TooLarge)));

        let by_chunk = format!("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n", MAX_BODY_BYTES + 1);
        assert!(matches!(read(by_chunk.as_bytes()), Err(ReadError::TooLarge)));

        let absurd = "HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n";
        assert!(read(absurd.as_bytes()).is_err());
    }

    #[test]
    fn a_device_cannot_exceed_the_limit_by_never_stopping() {
        // Many small chunks that together pass the limit.
        let mut wire = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        let piece = format!("{:x}\r\n{}\r\n", 60_000, "x".repeat(60_000));
        for _ in 0..(MAX_BODY_BYTES / 60_000 + 2) {
            wire.extend_from_slice(piece.as_bytes());
        }
        assert!(matches!(read_response(&mut &wire[..]), Err(ReadError::TooLarge)));

        // No framing at all, just an endless body.
        let mut endless = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        endless.resize(MAX_WIRE_BYTES + 100_000, b'x');
        assert!(matches!(read_response(&mut &endless[..]), Err(ReadError::TooLarge)));
    }
}
