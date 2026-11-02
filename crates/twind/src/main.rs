//! `SenseTwin.exe`. The Windows service arrives in milestone M1. For now this holds the M0
//! bench check: read one PLC's operating mode, trusting only its pinned TLS key.
//!
//! Each command talks to the one host named on the command line. Nothing here scans a
//! network or changes anything on a device.

use std::process::ExitCode;

use twind::pinned_tls::SpkiPin;
use twind::webapi::{self, Target, WebApi, WebApiError};

const PASSWORD_VAR: &str = "SENSETWIN_PLC_PASSWORD";

const USAGE: &str = "\
SenseTwin webapi-cert --host <address> [--port 443]
    Shows the TLS key a device presents. Sends no credentials.
    Compare it with the certificate in TIA Portal before trusting it.

SenseTwin webapi-mode --host <address> --pin sha256:<hex> --user <name> [--port 443]
    Logs in, reads the CPU's operating mode, and logs out. Changes nothing.
    The password comes from the SENSETWIN_PLC_PASSWORD environment variable, so it
    doesn't show up in process listings or shell history. The user needs the
    read_diagnostics permission.

Exit codes: 0 done, 2 bad arguments, 3 device key not trusted, 4 any other failure.
";

enum Failure {
    Usage(String),
    Device(WebApiError),
}

impl From<WebApiError> for Failure {
    fn from(e: WebApiError) -> Self {
        Self::Device(e)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(why)) => {
            eprintln!("{why}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Device(e)) => {
            eprintln!("{e}");
            ExitCode::from(if matches!(e, WebApiError::Untrusted(_)) { 3 } else { 4 })
        }
    }
}

fn run(args: &[String]) -> Result<(), Failure> {
    let Some((command, options)) = args.split_first() else {
        return Err(Failure::Usage("No command given.".into()));
    };
    match command.as_str() {
        "webapi-cert" => webapi_cert(options),
        "webapi-mode" => webapi_mode(options),
        other => Err(Failure::Usage(format!("Unknown command {other:?}."))),
    }
}

fn webapi_cert(options: &[String]) -> Result<(), Failure> {
    let target = target(options)?;
    let key = webapi::read_presented_key(&target)?;
    println!("pin:     {}", key.pin);
    println!("subject: {}", key.subject);
    println!("Not verified yet: compare this with the certificate in TIA Portal before using the pin.");
    Ok(())
}

fn webapi_mode(options: &[String]) -> Result<(), Failure> {
    let target = target(options)?;
    let pin = option(options, "--pin")?.ok_or_else(|| Failure::Usage("--pin is required. webapi-cert shows the device's key.".into()))?;
    let pin = SpkiPin::parse(pin).map_err(Failure::Usage)?;
    let user = option(options, "--user")?.ok_or_else(|| Failure::Usage("--user is required.".into()))?;
    let password = std::env::var(PASSWORD_VAR).map_err(|_| Failure::Usage(format!("Set the {PASSWORD_VAR} environment variable first.")))?;

    let api = WebApi::new(target, pin)?;
    let session = api.login(user, &password)?;
    let mode = session.read_operating_mode()?;
    session.logout()?;
    println!("mode: {mode}");
    Ok(())
}

fn target(options: &[String]) -> Result<Target, Failure> {
    let host = option(options, "--host")?.ok_or_else(|| Failure::Usage("--host is required.".into()))?;
    let port = match option(options, "--port")? {
        Some(port) => port.parse().map_err(|_| Failure::Usage(format!("{port:?} isn't a port number.")))?,
        None => 443,
    };
    Ok(Target::new(host, port))
}

fn option<'a>(options: &'a [String], name: &str) -> Result<Option<&'a str>, Failure> {
    let Some(at) = options.iter().position(|o| o == name) else { return Ok(None) };
    match options.get(at + 1) {
        Some(value) if !value.starts_with("--") => Ok(Some(value)),
        _ => Err(Failure::Usage(format!("{name} needs a value."))),
    }
}
