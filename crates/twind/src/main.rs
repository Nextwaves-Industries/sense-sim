//! `SenseTwin.exe`: the always-on service, plus the M0 bench check that reads one PLC's
//! operating mode, trusting only its pinned TLS key.
//!
//! The `webapi` commands talk to the one host named on the command line. Nothing here scans
//! a network or changes anything on a device.

use std::process::ExitCode;

use twind::pinned_tls::SpkiPin;
use twind::webapi::{self, Target, WebApi, WebApiError};

const PASSWORD_VAR: &str = "SENSETWIN_PLC_PASSWORD";

const USAGE: &str = "\
SenseTwin --role core
    Runs as the Windows service. Windows starts this; it isn't for running by hand.

SenseTwin --role core --console [--demo <devices>] [--data-dir <path>] [--pipe <name>]
    Runs in the foreground, for development. Stops when stdin closes or on Ctrl-C.
    --demo        Serves invented devices. Reads and writes nothing on disk.
    --data-dir    Defaults to %LOCALAPPDATA%\\SenseSim\\twin-console, never the service's data.
    --pipe        Defaults to the name viewers use, and fails if the service holds it.
    --pipe-sddl   Replaces the pipe's permissions, which otherwise allow only this user.
    --dev-churn <ms>  Invents device changes and saves constantly, for tests. Needs --data-dir.

SenseTwin --role hooks
    Arrives in milestone M3.

SenseTwin service-install
SenseTwin service-uninstall
    Adds or removes the service, as an administrator. Uninstalling keeps the data folder.

SenseTwin webapi-cert --host <address> [--port 443]
    Shows the TLS key a device presents. Sends no credentials.
    Compare it with the certificate in TIA Portal before trusting it.

SenseTwin webapi-mode --host <address> --pin sha256:<hex> --user <name> [--port 443]
    Logs in, reads the CPU's operating mode, and logs out. Changes nothing.
    The password comes from the SENSETWIN_PLC_PASSWORD environment variable, so it
    doesn't show up in process listings or shell history. The user needs the
    read_diagnostics permission.

Exit codes: 0 done, 2 bad arguments, 3 device key not trusted, 4 any other failure,
5 another process holds the pipe name.
";

enum Failure {
    Usage(String),
    Device(WebApiError),
    Service(String),
    #[cfg_attr(not(windows), allow(dead_code))]
    PipeTaken(String),
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
        Err(Failure::Service(why)) => {
            eprintln!("{why}");
            ExitCode::from(4)
        }
        Err(Failure::PipeTaken(why)) => {
            eprintln!("{why}");
            ExitCode::from(5)
        }
    }
}

fn run(args: &[String]) -> Result<(), Failure> {
    if let Some(at) = args.iter().position(|a| a == "--role") {
        let role = args.get(at + 1).ok_or_else(|| Failure::Usage("--role needs a value.".into()))?;
        let others: Vec<String> = args.iter().enumerate().filter(|(i, _)| *i != at && *i != at + 1).map(|(_, a)| a.clone()).collect();
        return match role.as_str() {
            "core" => service_role::core(&others),
            "hooks" => Err(Failure::Usage("--role hooks arrives in milestone M3.".into())),
            other => Err(Failure::Usage(format!("Unknown role {other:?}."))),
        };
    }
    let Some((command, options)) = args.split_first() else {
        return Err(Failure::Usage("No command given.".into()));
    };
    match command.as_str() {
        "webapi-cert" => webapi_cert(options),
        "webapi-mode" => webapi_mode(options),
        "service-install" => service_role::install(options),
        "service-uninstall" => service_role::uninstall(options),
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

#[cfg(windows)]
mod service_role {
    use std::path::PathBuf;
    use std::time::Duration;

    use twind::pipe::{self, PipeConfig};
    use twind::runtime::{self, Config, Source, StartError};
    use twind::{install as installer, say, sddl, service, win};

    use super::Failure;

    #[derive(Default)]
    struct CoreOptions {
        console: bool,
        demo: Option<usize>,
        data_dir: Option<PathBuf>,
        pipe: Option<String>,
        pipe_sddl: Option<String>,
        dev_churn_ms: Option<u64>,
    }

    /// Unlike the `webapi` commands, unknown options are refused: a mistyped `--data-dir`
    /// silently falling back to the default would put test data in the wrong store.
    fn parse(options: &[String]) -> Result<CoreOptions, Failure> {
        let mut parsed = CoreOptions::default();
        let mut rest = options.iter();
        while let Some(flag) = rest.next() {
            let mut value = || rest.next().filter(|v| !v.starts_with("--")).ok_or_else(|| Failure::Usage(format!("{flag} needs a value.")));
            match flag.as_str() {
                "--console" => parsed.console = true,
                "--demo" => {
                    let count = value()?;
                    parsed.demo = Some(count.parse().map_err(|_| Failure::Usage(format!("{count:?} isn't a device count.")))?);
                }
                "--data-dir" => parsed.data_dir = Some(PathBuf::from(value()?)),
                "--pipe" => parsed.pipe = Some(value()?.clone()),
                "--pipe-sddl" => parsed.pipe_sddl = Some(value()?.clone()),
                "--dev-churn" => {
                    let ms = value()?;
                    parsed.dev_churn_ms = Some(ms.parse().map_err(|_| Failure::Usage(format!("{ms:?} isn't a number of milliseconds.")))?);
                }
                other => return Err(Failure::Usage(format!("Unknown option {other:?} for --role core."))),
            }
        }
        Ok(parsed)
    }

    pub fn core(options: &[String]) -> Result<(), Failure> {
        let options = parse(options)?;
        if !options.console {
            let any_option = options.demo.is_some() || options.data_dir.is_some() || options.pipe.is_some() || options.pipe_sddl.is_some() || options.dev_churn_ms.is_some();
            if any_option {
                return Err(Failure::Usage("These options need --console. The service takes none, so it can't be pointed at other data.".into()));
            }
            return service::run_under_service_manager().map_err(Failure::Service);
        }
        if options.dev_churn_ms.is_some() && (options.data_dir.is_none() || options.demo.is_some()) {
            return Err(Failure::Usage("--dev-churn invents devices, so it needs an explicit --data-dir and can't run with --demo.".into()));
        }

        let name = match &options.pipe {
            Some(name) => pipe::normalize_name(name).map_err(Failure::Usage)?,
            None => pipe::default_name(),
        };
        let source = match (options.demo, options.data_dir) {
            (Some(devices), data_dir) => {
                if data_dir.is_some() {
                    say!("Demo mode doesn't use --data-dir.");
                }
                Source::Demo(devices)
            }
            (None, Some(dir)) => Source::Store(dir),
            (None, None) => Source::Store(default_console_dir()?),
        };
        let sddl_is_explicit = options.pipe_sddl.is_some();
        let sddl = match options.pipe_sddl {
            Some(sddl) => sddl,
            None => {
                let user = win::current_user_sid().map_err(|e| Failure::Service(format!("Couldn't identify this user: {e}")))?;
                sddl::console_pipe(&user).map_err(Failure::Service)?
            }
        };
        let config = Config { pipe: PipeConfig { name, sddl, sddl_is_explicit }, source, dev_churn: options.dev_churn_ms.map(Duration::from_millis) };
        runtime::run_console(config).map_err(|e| match e {
            StartError::PipeTaken(why) => Failure::PipeTaken(why),
            StartError::Failed(why) => Failure::Service(why),
        })
    }

    /// Per-user, so a development run can't reach the service's folder.
    fn default_console_dir() -> Result<PathBuf, Failure> {
        let root = std::env::var_os("LOCALAPPDATA").ok_or_else(|| Failure::Usage("LOCALAPPDATA isn't set, so give --data-dir.".into()))?;
        Ok(PathBuf::from(root).join("SenseSim").join("twin-console"))
    }

    fn no_options(command: &str, options: &[String]) -> Result<(), Failure> {
        match options.first() {
            Some(extra) => Err(Failure::Usage(format!("{command} takes no options, but got {extra:?}."))),
            None => Ok(()),
        }
    }

    pub fn install(options: &[String]) -> Result<(), Failure> {
        no_options("service-install", options)?;
        installer::install().map_err(Failure::Service)
    }

    pub fn uninstall(options: &[String]) -> Result<(), Failure> {
        no_options("service-uninstall", options)?;
        installer::uninstall().map_err(Failure::Service)
    }
}

#[cfg(not(windows))]
mod service_role {
    use super::Failure;

    const WINDOWS_ONLY: &str = "The service only runs on Windows.";

    pub fn core(_: &[String]) -> Result<(), Failure> {
        Err(Failure::Service(WINDOWS_ONLY.into()))
    }

    pub fn install(_: &[String]) -> Result<(), Failure> {
        Err(Failure::Service(WINDOWS_ONLY.into()))
    }

    pub fn uninstall(_: &[String]) -> Result<(), Failure> {
        Err(Failure::Service(WINDOWS_ONLY.into()))
    }
}
