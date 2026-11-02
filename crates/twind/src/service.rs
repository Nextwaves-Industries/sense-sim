//! Running under the Service Control Manager.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::Foundation::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT;

use crate::pipe::{self, PipeConfig};
use crate::runtime::{self, Config, Running, Source, StartError};
use crate::stop::Stop;
use crate::{logging, say, sddl, win};

pub const SERVICE_NAME: &str = "SenseTwin";
pub const SERVICE_ACCOUNT: &str = r"NT SERVICE\SenseTwin";
pub const READERS_GROUP: &str = "SenseTwin Readers";

const LOG_FILE: &str = "service.log";
const START_HINT: Duration = Duration::from_secs(60);
const STOP_HINT: Duration = Duration::from_secs(30);

/// Exit codes the service manager records: for the event log, and for its restart rules.
const EXIT_PIPE_TAKEN: u32 = 1;
const EXIT_FAILED: u32 = 2;

/// `%ProgramData%\SenseSim`.
pub fn data_dir() -> Result<PathBuf, String> {
    let program_data = std::env::var_os("ProgramData").ok_or("The ProgramData environment variable isn't set.")?;
    Ok(PathBuf::from(program_data).join("SenseSim"))
}

define_windows_service!(ffi_service_main, service_main);

/// Blocks until the service stops. Outside the service manager this fails right away.
pub fn run_under_service_manager() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| match e {
        windows_service::Error::Winapi(os) if os.raw_os_error() == Some(ERROR_FAILED_SERVICE_CONTROLLER_CONNECT as i32) => {
            "--role core is started by Windows. To run in the foreground, add --console.".to_string()
        }
        other => format!("Couldn't start as a service: {other}"),
    })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_service() {
        say!("The service manager link failed: {e}");
    }
}

fn run_service() -> Result<(), windows_service::Error> {
    let stop = Stop::new().map_err(windows_service::Error::Winapi)?;
    let stop_request = stop.clone();
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop_request.signal();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let report = |state, accepts, exit_code, wait_hint| {
        status.set_service_status(ServiceStatus { service_type: ServiceType::OWN_PROCESS, current_state: state, controls_accepted: accepts, exit_code, checkpoint: 0, wait_hint, process_id: None })
    };
    report(ServiceState::StartPending, ServiceControlAccept::empty(), ServiceExitCode::NO_ERROR, START_HINT)?;

    let exit_code = match start(&stop) {
        Ok(running) => {
            report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, ServiceExitCode::NO_ERROR, Duration::ZERO)?;
            stop.wait(None);
            report(ServiceState::StopPending, ServiceControlAccept::empty(), ServiceExitCode::NO_ERROR, STOP_HINT)?;
            match running.shutdown() {
                Ok(()) => {
                    say!("Stopped cleanly.");
                    ServiceExitCode::NO_ERROR
                }
                Err(e) => {
                    say!("{e}");
                    ServiceExitCode::ServiceSpecific(EXIT_FAILED)
                }
            }
        }
        Err(e) => {
            say!("Not starting: {e}");
            ServiceExitCode::ServiceSpecific(if matches!(e, StartError::PipeTaken(_)) { EXIT_PIPE_TAKEN } else { EXIT_FAILED })
        }
    };
    report(ServiceState::Stopped, ServiceControlAccept::empty(), exit_code, Duration::ZERO)
}

fn start(stop: &Stop) -> Result<Running, StartError> {
    let dir = data_dir().map_err(StartError::Failed)?;
    // Only the installer makes this folder, with the right permissions. Making it here would
    // inherit %ProgramData%'s defaults, which let any user write.
    if !dir.is_dir() {
        return Err(StartError::Failed(format!("{} is missing. Run service-install again.", dir.display())));
    }
    logging::to_file(&dir.join(LOG_FILE))?;
    runtime::exit_on_panic();
    say!("Starting version {}.", env!("CARGO_PKG_VERSION"));

    let readers = win::local_group_sid(READERS_GROUP).unwrap_or_else(|e| {
        say!("Couldn't look up the {READERS_GROUP} group: {e}");
        None
    });
    if readers.is_none() {
        say!("The {READERS_GROUP} group is missing, so only administrators can read the pipe. Run service-install again to restore it.");
    }
    let service_sid = win::current_user_sid()?;
    let sddl = sddl::service_pipe(&service_sid, readers.as_deref()).map_err(StartError::Failed)?;
    let config = Config { pipe: PipeConfig { name: pipe::default_name(), sddl, sddl_is_explicit: false }, source: Source::Store(dir), dev_churn: None };
    Running::start(config, stop.clone())
}
