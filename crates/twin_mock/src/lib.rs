//! Fake devices for automated tests. Never ships and only listens on loopback.
//!
//! [`MockPlc`] answers the small part of the Siemens Web API the client uses, following the
//! Web server function manual, including its chunked replies. It records what it received
//! so a test can prove what never reached a device. It shows the client's behaviour, not
//! how real firmware behaves; only the lab bench can prove that.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::PublicKeyData;
use rustls::crypto::ring::{default_provider, sign::any_supported_type};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signing {
    /// Holds the private key for its certificate, like a real device.
    Genuine,
    /// Presents the certificate but signs with a different key: an attacker who copied a
    /// device's public certificate.
    Impostor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsVersions {
    Any,
    /// Older firmware may stop at TLS 1.2, which verifies signatures on a separate path.
    Tls12Only,
}

#[derive(Clone, Debug)]
pub struct MockConfig {
    pub user: String,
    pub password: String,
    pub mode: String,
    pub signing: Signing,
    pub tls: TlsVersions,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self { user: "twin".into(), password: "bench-password".into(), mode: "run".into(), signing: Signing::Genuine, tls: TlsVersions::Any }
    }
}

struct Shared {
    config: MockConfig,
    sessions: Mutex<HashSet<String>>,
    methods: Mutex<Vec<String>>,
    raw_requests: Mutex<Vec<Vec<u8>>>,
    next_token: AtomicU64,
}

pub struct MockPlc {
    address: SocketAddr,
    cert_der: Vec<u8>,
    public_key_der: Vec<u8>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl MockPlc {
    pub fn start(config: MockConfig) -> Self {
        let key = rcgen::KeyPair::generate().expect("generate key");
        let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("certificate parameters");
        let cert = params.self_signed(&key).expect("self-sign");
        let cert_der = cert.der().to_vec();
        let public_key_der = key.subject_public_key_info();

        let signing_key = match config.signing {
            Signing::Genuine => key,
            Signing::Impostor => rcgen::KeyPair::generate().expect("generate impostor key"),
        };
        let tls = server_config(cert_der.clone(), signing_key, config.tls);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("non-blocking listener");
        let address = listener.local_addr().expect("local address");

        let shared = Arc::new(Shared { config, sessions: Mutex::default(), methods: Mutex::default(), raw_requests: Mutex::default(), next_token: AtomicU64::new(1) });
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::spawn(move || accept_loop(listener, tls, shared, stop))
        };
        Self { address, cert_der, public_key_der, shared, stop, accept: Some(accept) }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The certificate's SubjectPublicKeyInfo, straight from the key generator, so tests
    /// have a source for the expected pin that's independent of the client's parser.
    pub fn public_key_der(&self) -> &[u8] {
        &self.public_key_der
    }

    pub fn open_sessions(&self) -> usize {
        self.shared.sessions.lock().unwrap().len()
    }

    /// Every method received, in order.
    pub fn methods(&self) -> Vec<String> {
        self.shared.methods.lock().unwrap().clone()
    }

    /// Whether any decrypted request contained these bytes.
    pub fn received_bytes(&self, needle: &[u8]) -> bool {
        self.shared.raw_requests.lock().unwrap().iter().any(|r| r.windows(needle.len()).any(|w| w == needle))
    }

    pub fn request_count(&self) -> usize {
        self.shared.raw_requests.lock().unwrap().len()
    }
}

impl Drop for MockPlc {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// Picks this certificate and key whether they belong together or not. Rustls's normal
/// server setup refuses a mismatch, which would make the impostor impossible to test.
#[derive(Debug)]
struct FixedIdentity(Arc<CertifiedKey>);

impl ResolvesServerCert for FixedIdentity {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

fn server_config(cert_der: Vec<u8>, signing_key: rcgen::KeyPair, tls: TlsVersions) -> Arc<ServerConfig> {
    let der: PrivateKeyDer = PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into();
    let signer = any_supported_type(&der).expect("usable signing key");
    let identity = FixedIdentity(Arc::new(CertifiedKey::new(vec![CertificateDer::from(cert_der)], signer)));

    let versions: &[&rustls::SupportedProtocolVersion] = match tls {
        TlsVersions::Any => rustls::ALL_VERSIONS,
        TlsVersions::Tls12Only => &[&rustls::version::TLS12],
    };
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_protocol_versions(versions)
        .expect("protocol versions")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(identity));
    Arc::new(config)
}

fn accept_loop(listener: TcpListener, tls: Arc<ServerConfig>, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((tcp, _)) => {
                let (tls, shared) = (tls.clone(), shared.clone());
                std::thread::spawn(move || serve(tcp, tls, shared));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn serve(tcp: TcpStream, tls: Arc<ServerConfig>, shared: Arc<Shared>) {
    let _ = tcp.set_nonblocking(false);
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(connection) = ServerConnection::new(tls) else { return };
    let mut stream = StreamOwned::new(connection, tcp);

    // A refused handshake ends here, before any request is recorded.
    let Some((body, token)) = read_request(&mut stream, &shared) else { return };
    let reply = answer(&body, token.as_deref(), &shared).to_string();

    // Split in two chunks, in the manual's lowercase header form.
    let (first, second) = reply.as_bytes().split_at(reply.len() / 2);
    let mut wire = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
    for part in [first, second] {
        wire.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        wire.extend_from_slice(part);
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    let _ = stream.write_all(&wire);
    stream.conn.send_close_notify();
    let _ = stream.flush();
}

fn read_request(stream: &mut impl Read, shared: &Shared) -> Option<(Vec<u8>, Option<String>)> {
    let mut wire = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        if let Ok(httparse::Status::Complete(body_start)) = request.parse(&wire) {
            let value = |name: &str| request.headers.iter().find(|h| h.name.eq_ignore_ascii_case(name)).map(|h| String::from_utf8_lossy(h.value).to_string());
            let length: usize = value("content-length")?.parse().ok()?;
            if wire.len() >= body_start + length {
                let token = value("x-auth-token");
                let body = wire[body_start..body_start + length].to_vec();
                shared.raw_requests.lock().unwrap().push(wire.clone());
                return Some((body, token));
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => wire.extend_from_slice(&chunk[..n]),
        }
    }
}

fn answer(body: &[u8], token: Option<&str>, shared: &Shared) -> Value {
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return error(Value::Null, -32700, "Parse error");
    };
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or("");
    shared.methods.lock().unwrap().push(method.to_string());

    let signed_in = token.is_some_and(|t| shared.sessions.lock().unwrap().contains(t));
    match method {
        "Api.Login" => {
            let params = &request["params"];
            if params["user"].as_str() == Some(&shared.config.user) && params["password"].as_str() == Some(&shared.config.password) {
                let token = format!("mock-token-{:017}", shared.next_token.fetch_add(1, Ordering::Relaxed));
                shared.sessions.lock().unwrap().insert(token.clone());
                ok(id, json!({ "token": token }))
            } else {
                error(id, 100, "Login failed")
            }
        }
        // Per the manual, logout answers true even for an invalid token.
        "Api.Logout" => {
            if let Some(token) = token {
                shared.sessions.lock().unwrap().remove(token);
            }
            ok(id, json!(true))
        }
        "Plc.ReadOperatingMode" if signed_in => ok(id, json!(shared.config.mode)),
        "Plc.ReadOperatingMode" => error(id, 2, "Permission denied"),
        _ => error(id, -32601, "Method not found"),
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
