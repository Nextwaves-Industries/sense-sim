//! Read-only client for the Siemens Web API (JSON-RPC over HTTPS).
//!
//! Follows the Web server function manual (11/2025, A5E53797648-AB, chapter 6). Each login
//! takes one of a CPU's limited session slots until logout or a 120-second idle timeout,
//! so a [`Session`] always logs out when dropped.
//!
//! Run/stop and tag writes are deliberately absent. They arrive in milestone M7, behind the
//! safety gates in the service.

use std::fmt;
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use serde_json::{Value, json};
use twin_proto::text::clean_device_text;

use crate::http::{self, ReadError};
use crate::pinned_tls::{self, SpkiPin};

const ENDPOINT: &str = "/api/jsonrpc";
const MAX_TOKEN_BYTES: usize = 512;

#[derive(Debug)]
pub enum WebApiError {
    /// The server failed the pin or couldn't prove it holds the pinned key.
    /// No request was sent, so no credential left this machine.
    Untrusted(String),
    Io(io::Error),
    /// The reply wasn't valid HTTP or JSON-RPC.
    Protocol(String),
    /// The CPU answered with a JSON-RPC error. The message is cleaned device text.
    Rpc { code: i64, message: String },
}

impl fmt::Display for WebApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted(why) => write!(f, "refused to trust this device: {why}"),
            Self::Io(e) => write!(f, "network error: {e}"),
            Self::Protocol(why) => write!(f, "unexpected reply: {why}"),
            Self::Rpc { code, message } => write!(f, "the device refused (code {code}): {message}"),
        }
    }
}

impl std::error::Error for WebApiError {}

impl From<io::Error> for WebApiError {
    fn from(e: io::Error) -> Self {
        // rustls reports handshake failures as an io::Error wrapping its own error.
        match e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
            Some(rustls::Error::InvalidCertificate(why)) => Self::Untrusted(format!("{why:?}")),
            _ => Self::Io(e),
        }
    }
}

impl From<ReadError> for WebApiError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::Io(e) => e.into(),
            ReadError::Malformed(why) => Self::Protocol(why.into()),
            ReadError::TooLarge => Self::Protocol("the reply is too large".into()),
        }
    }
}

/// Mode strings from the manual's `Plc.ReadOperatingMode` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatingMode {
    Stop,
    StopSelfInit,
    Startup,
    Run,
    RunRedundant,
    Hold,
    Syncup,
    RunSyncup,
    RemoteUnknown,
    Unknown,
    /// A string this version doesn't know, cleaned because the device supplied it.
    Other(String),
}

impl OperatingMode {
    fn parse(text: &str) -> Self {
        match text {
            "stop" => Self::Stop,
            "stop_self_init" => Self::StopSelfInit,
            "startup" => Self::Startup,
            "run" => Self::Run,
            "run_redundant" => Self::RunRedundant,
            "hold" => Self::Hold,
            "syncup" => Self::Syncup,
            "run_syncup" => Self::RunSyncup,
            "remote_unknown" => Self::RemoteUnknown,
            "unknown" => Self::Unknown,
            other => Self::Other(clean_device_text(other)),
        }
    }
}

impl fmt::Display for OperatingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stop => "stop",
            Self::StopSelfInit => "stop_self_init",
            Self::Startup => "startup",
            Self::Run => "run",
            Self::RunRedundant => "run_redundant",
            Self::Hold => "hold",
            Self::Syncup => "syncup",
            Self::RunSyncup => "run_syncup",
            Self::RemoteUnknown => "remote_unknown",
            Self::Unknown => "unknown",
            Self::Other(text) => text,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub timeout: Duration,
}

impl Target {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port, timeout: Duration::from_secs(10) }
    }

    fn open(&self, config: Arc<ClientConfig>) -> Result<StreamOwned<ClientConnection, TcpStream>, WebApiError> {
        let name = ServerName::try_from(self.host.clone()).map_err(|_| WebApiError::Protocol(format!("{:?} isn't a valid host", self.host)))?;
        let connection = ClientConnection::new(config, name).map_err(|e| WebApiError::Protocol(e.to_string()))?;
        Ok(StreamOwned::new(connection, self.connect()?))
    }

    fn connect(&self) -> io::Result<TcpStream> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "the host name has no address");
        for address in (self.host.as_str(), self.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&address, self.timeout) {
                Ok(tcp) => {
                    tcp.set_read_timeout(Some(self.timeout))?;
                    tcp.set_write_timeout(Some(self.timeout))?;
                    return Ok(tcp);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// `Host` header value; IPv6 addresses need brackets.
    fn authority(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }
}

/// What a device says its key is. Unverified: an operator must compare the pin against the
/// certificate configured in TIA Portal before trusting it.
#[derive(Debug)]
pub struct PresentedKey {
    pub pin: SpkiPin,
    /// Cleaned device text.
    pub subject: String,
}

/// Reads the certificate a device presents, then aborts. Sends no request and no credential.
pub fn read_presented_key(target: &Target) -> Result<PresentedKey, WebApiError> {
    let (config, capture) = pinned_tls::capturing_client().map_err(|e| WebApiError::Protocol(e.to_string()))?;
    let mut stream = target.open(config)?;
    // The handshake is expected to fail: the capturing verifier aborts on purpose.
    let _ = stream.conn.complete_io(&mut stream.sock);
    let der = capture.take().ok_or_else(|| WebApiError::Protocol("the device didn't present a certificate".into()))?;
    let pin = SpkiPin::of_cert(&der).map_err(WebApiError::Protocol)?;
    let subject = x509_parser::parse_x509_certificate(&der).map(|(_, cert)| cert.subject().to_string()).unwrap_or_default();
    Ok(PresentedKey { pin, subject: clean_device_text(&subject) })
}

/// A device reached through its pinned key.
pub struct WebApi {
    target: Target,
    config: Arc<ClientConfig>,
    next_id: AtomicU64,
}

impl WebApi {
    pub fn new(target: Target, pin: SpkiPin) -> Result<Self, WebApiError> {
        let config = pinned_tls::pinned_client(pin).map_err(|e| WebApiError::Protocol(e.to_string()))?;
        Ok(Self { target, config, next_id: AtomicU64::new(1) })
    }

    /// Opens a session. The password is only used for this call and is never kept.
    pub fn login(&self, user: &str, password: &str) -> Result<Session<'_>, WebApiError> {
        let result = self.call("Api.Login", json!({ "user": user, "password": password }), None)?;
        let token = result["token"].as_str().ok_or_else(|| WebApiError::Protocol("the login reply has no token".into()))?;
        // The token goes back in a header, so a hostile one must not be able to add lines.
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(WebApiError::Protocol("the login reply has an invalid token".into()));
        }
        Ok(Session { api: self, token: Some(token.to_string()) })
    }

    fn call(&self, method: &str, params: Value, token: Option<&str>) -> Result<Value, WebApiError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": id }).to_string();

        let mut head = format!("POST {ENDPOINT} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", self.target.authority(), body.len());
        if let Some(token) = token {
            head.push_str(&format!("X-Auth-Token: {token}\r\n"));
        }
        head.push_str("\r\n");

        let mut stream = self.target.open(self.config.clone())?;
        // Written only once the handshake succeeds, so a failed pin never sends the password.
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.flush()?;

        let response = http::read_response(&mut stream)?;
        if response.status != 200 {
            return Err(WebApiError::Protocol(format!("HTTP status {}", response.status)));
        }
        parse_reply(&response.body, id)
    }
}

fn parse_reply(body: &[u8], id: u64) -> Result<Value, WebApiError> {
    let reply: Value = serde_json::from_slice(body).map_err(|e| WebApiError::Protocol(format!("invalid JSON: {e}")))?;
    if let Some(error) = reply.get("error") {
        let code = error["code"].as_i64().unwrap_or(0);
        let message = clean_device_text(error["message"].as_str().unwrap_or("no message"));
        return Err(WebApiError::Rpc { code, message });
    }
    if reply["id"].as_u64() != Some(id) {
        return Err(WebApiError::Protocol("the reply answers a different request".into()));
    }
    reply.get("result").cloned().ok_or_else(|| WebApiError::Protocol("the reply has no result".into()))
}

/// A logged-in session, which logs out when dropped.
pub struct Session<'a> {
    api: &'a WebApi,
    token: Option<String>,
}

impl Session<'_> {
    /// Needs the `read_diagnostics` permission. R/H CPUs also need a redundancy ID, which
    /// isn't supported yet, so they answer with a "Invalid params" error.
    pub fn read_operating_mode(&self) -> Result<OperatingMode, WebApiError> {
        let result = self.api.call("Plc.ReadOperatingMode", json!({}), self.token.as_deref())?;
        let mode = result.as_str().ok_or_else(|| WebApiError::Protocol("the operating mode isn't a string".into()))?;
        Ok(OperatingMode::parse(mode))
    }

    /// Ends the session and reports whether that worked. Dropping does the same silently.
    pub fn logout(mut self) -> Result<(), WebApiError> {
        match self.token.take() {
            Some(token) => self.api.call("Api.Logout", json!({}), Some(&token)).map(|_| ()),
            None => Ok(()),
        }
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            // Best effort. If this fails, the CPU frees the slot after its idle timeout.
            let _ = self.api.call("Api.Logout", json!({}), Some(&token));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_in_the_manual_round_trips() {
        for text in ["stop", "stop_self_init", "startup", "run", "run_redundant", "hold", "syncup", "run_syncup", "remote_unknown", "unknown"] {
            let mode = OperatingMode::parse(text);
            assert!(!matches!(mode, OperatingMode::Other(_)), "{text}");
            assert_eq!(mode.to_string(), text);
        }
    }

    #[test]
    fn an_unknown_mode_is_kept_but_cleaned() {
        assert_eq!(OperatingMode::parse("new_mode\r\n\u{202E}x"), OperatingMode::Other("new_modex".into()));
    }

    #[test]
    fn a_reply_to_another_request_is_rejected() {
        assert!(parse_reply(br#"{"jsonrpc":"2.0","id":7,"result":"run"}"#, 7).is_ok());
        assert!(matches!(parse_reply(br#"{"jsonrpc":"2.0","id":8,"result":"run"}"#, 7), Err(WebApiError::Protocol(_))));
    }

    #[test]
    fn device_error_text_is_cleaned() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":2,\"message\":\"no\\r\\nfake log\\u202e\"}}";
        match parse_reply(body.as_bytes(), 1) {
            Err(WebApiError::Rpc { code: 2, message }) => assert_eq!(message, "nofake log"),
            other => panic!("{other:?}"),
        }
    }
}
