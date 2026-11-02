//! M0's trust rules, run against a fake PLC over real TLS on loopback. This proves how the
//! client behaves, not that real firmware accepts it. That's the lab bench's job.

use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use twin_mock::{MockConfig, MockPlc, Signing, TlsVersions};
use twind::pinned_tls::SpkiPin;
use twind::webapi::{self, OperatingMode, Target, WebApi, WebApiError};

const USER: &str = "twin";
const PASSWORD: &str = "bench-password";

fn target(plc: &MockPlc) -> Target {
    Target::new("127.0.0.1", plc.address().port())
}

fn pin_of(plc: &MockPlc) -> SpkiPin {
    SpkiPin::of_cert(plc.cert_der()).unwrap()
}

fn other_pin() -> SpkiPin {
    pin_of(&MockPlc::start(MockConfig::default()))
}

fn plc_with(signing: Signing, tls: TlsVersions) -> MockPlc {
    MockPlc::start(MockConfig { signing, tls, ..MockConfig::default() })
}

fn assert_untrusted_and_nothing_sent<T>(result: Result<T, WebApiError>, plc: &MockPlc) {
    match result {
        Err(WebApiError::Untrusted(_)) => {}
        Err(other) => panic!("expected the device to be refused, got: {other}"),
        Ok(_) => panic!("expected the device to be refused, but the login worked"),
    }
    assert_eq!(plc.request_count(), 0, "nothing may reach a device that failed the trust check");
    assert!(!plc.received_bytes(PASSWORD.as_bytes()));
}

#[test]
fn reads_the_mode_with_the_right_pin_on_both_tls_versions() {
    for tls in [TlsVersions::Any, TlsVersions::Tls12Only] {
        let plc = plc_with(Signing::Genuine, tls);
        let api = WebApi::new(target(&plc), pin_of(&plc)).unwrap();

        let session = api.login(USER, PASSWORD).unwrap();
        assert_eq!(plc.open_sessions(), 1);
        assert_eq!(session.read_operating_mode().unwrap(), OperatingMode::Run);
        session.logout().unwrap();

        assert_eq!(plc.open_sessions(), 0);
        assert_eq!(plc.methods(), ["Api.Login", "Plc.ReadOperatingMode", "Api.Logout"], "M0 only ever reads");
    }
}

#[test]
fn a_wrong_pin_is_refused_before_the_password_is_sent() {
    for tls in [TlsVersions::Any, TlsVersions::Tls12Only] {
        let plc = plc_with(Signing::Genuine, tls);
        let api = WebApi::new(target(&plc), other_pin()).unwrap();
        assert_untrusted_and_nothing_sent(api.login(USER, PASSWORD), &plc);
    }
}

/// The attack that a pin check alone would miss: a certificate is public, so an impostor
/// can present the real one. Only the handshake signature shows it lacks the private key.
#[test]
fn an_impostor_presenting_the_real_certificate_is_refused() {
    for tls in [TlsVersions::Any, TlsVersions::Tls12Only] {
        let plc = plc_with(Signing::Impostor, tls);
        let api = WebApi::new(target(&plc), pin_of(&plc)).unwrap();
        assert_untrusted_and_nothing_sent(api.login(USER, PASSWORD), &plc);
    }
}

#[test]
fn the_pin_is_the_hash_of_the_public_key() {
    let plc = MockPlc::start(MockConfig::default());
    let hex: String = Sha256::digest(plc.public_key_der()).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(pin_of(&plc), SpkiPin::parse(&format!("sha256:{hex}")).unwrap());
}

#[test]
fn a_wrong_password_opens_no_session_and_isnt_repeated_back() {
    let plc = MockPlc::start(MockConfig::default());
    let api = WebApi::new(target(&plc), pin_of(&plc)).unwrap();
    match api.login(USER, "guess-123") {
        Err(e @ WebApiError::Rpc { .. }) => assert!(!e.to_string().contains("guess-123")),
        Err(other) => panic!("expected the CPU's refusal, got: {other}"),
        Ok(_) => panic!("the wrong password worked"),
    }
    assert_eq!(plc.open_sessions(), 0);
}

#[test]
fn dropping_a_session_frees_the_cpus_slot() {
    let plc = MockPlc::start(MockConfig::default());
    let api = WebApi::new(target(&plc), pin_of(&plc)).unwrap();
    {
        let session = api.login(USER, PASSWORD).unwrap();
        session.read_operating_mode().unwrap();
        assert_eq!(plc.open_sessions(), 1);
        // No explicit logout, as when a later read fails and the error returns early.
    }
    assert_eq!(plc.open_sessions(), 0);
}

#[test]
fn a_hostile_mode_string_comes_back_cleaned() {
    let plc = MockPlc::start(MockConfig { mode: "evil\r\nfake log\u{202E}".into(), ..MockConfig::default() });
    let api = WebApi::new(target(&plc), pin_of(&plc)).unwrap();
    let session = api.login(USER, PASSWORD).unwrap();
    assert_eq!(session.read_operating_mode().unwrap(), OperatingMode::Other("evilfake log".into()));
}

#[test]
fn reading_the_presented_key_sends_no_request_at_all() {
    let plc = MockPlc::start(MockConfig::default());
    let key = webapi::read_presented_key(&target(&plc)).unwrap();
    assert_eq!(key.pin, pin_of(&plc));
    assert_eq!(plc.request_count(), 0);
}

#[test]
fn an_unreachable_device_is_a_network_error_not_a_trust_failure() {
    let port = {
        let plc = MockPlc::start(MockConfig::default());
        plc.address().port()
    };
    let api = WebApi::new(Target { timeout: std::time::Duration::from_secs(2), ..Target::new("127.0.0.1", port) }, other_pin()).unwrap();
    assert!(matches!(api.login(USER, PASSWORD), Err(WebApiError::Io(_))));
}

// The bench proof, end to end through the real executable.

fn sense_twin(args: &[&str], password: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_SenseTwin"));
    command.args(args).env_remove("SENSETWIN_PLC_PASSWORD");
    if let Some(password) = password {
        command.env("SENSETWIN_PLC_PASSWORD", password);
    }
    command.output().expect("run SenseTwin")
}

#[test]
fn cli_full_bench_procedure() {
    let plc = MockPlc::start(MockConfig::default());
    let port = plc.address().port().to_string();

    // Step 1: read the key. No credentials involved.
    let cert = sense_twin(&["webapi-cert", "--host", "127.0.0.1", "--port", &port], None);
    assert_eq!(cert.status.code(), Some(0), "{}", String::from_utf8_lossy(&cert.stderr));
    let stdout = String::from_utf8_lossy(&cert.stdout);
    let pin = stdout.lines().find_map(|l| l.strip_prefix("pin:")).expect("a pin line").trim().to_string();
    assert_eq!(plc.request_count(), 0);

    // Step 2: read the mode through that pin.
    let mode = sense_twin(&["webapi-mode", "--host", "127.0.0.1", "--port", &port, "--pin", &pin, "--user", USER], Some(PASSWORD));
    assert_eq!(mode.status.code(), Some(0), "{}", String::from_utf8_lossy(&mode.stderr));
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "mode: run");
    assert_eq!(plc.open_sessions(), 0, "the CLI must log out");
}

#[test]
fn cli_a_wrong_pin_exits_3_and_sends_nothing() {
    let plc = MockPlc::start(MockConfig::default());
    let port = plc.address().port().to_string();
    let wrong = other_pin().to_string();
    let out = sense_twin(&["webapi-mode", "--host", "127.0.0.1", "--port", &port, "--pin", &wrong, "--user", USER], Some(PASSWORD));
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(plc.request_count(), 0);
}

#[test]
fn cli_a_missing_password_or_bad_arguments_exit_2_without_touching_the_device() {
    let plc = MockPlc::start(MockConfig::default());
    let port = plc.address().port().to_string();
    let pin = pin_of(&plc).to_string();
    let attempts: [&[&str]; 5] = [
        &["webapi-mode", "--host", "127.0.0.1", "--port", &port, "--pin", &pin, "--user", USER],
        &["webapi-mode", "--host", "127.0.0.1", "--port", &port, "--pin", "sha256:nope", "--user", USER],
        &["webapi-mode", "--host", "127.0.0.1", "--port", &port, "--user", USER],
        &["webapi-mode", "--host", "--port", &port],
        &["scan-everything"],
    ];
    for args in attempts {
        let out = sense_twin(args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(plc.request_count(), 0);
}
