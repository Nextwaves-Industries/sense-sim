//! `service-install` and `service-uninstall`. These change the system, so nothing in the
//! test suite runs them, and they refuse to do anything without administrator rights.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_service::service::{ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceSidType, ServiceStartType, ServiceState, ServiceType};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::{ERROR_ALIAS_EXISTS, ERROR_MEMBER_IN_ALIAS, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_EXISTS, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::{LOCALGROUP_INFO_1, LOCALGROUP_MEMBERS_INFO_0, NERR_GroupExists, NetLocalGroupAdd, NetLocalGroupAddMembers};
use windows_sys::Win32::Security::Authorization::{ConvertStringSidToSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW};
use windows_sys::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, WinBuiltinAdministratorsSid, WinLocalSystemSid};
use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT};

use crate::sddl;
use crate::service::{READERS_GROUP, SERVICE_ACCOUNT, SERVICE_NAME, data_dir};
use crate::win::{self, SecurityDescriptor, wide};

const DISPLAY_NAME: &str = "Sense Twin";
const DESCRIPTION: &str = "Keeps the Sense Sim factory twin's device inventory and events. Viewers read from it over a local pipe.";
const GROUP_COMMENT: &str = "May read the Sense Twin device inventory over the local pipe. Grants no control over devices.";
const STOP_LIMIT: Duration = Duration::from_secs(30);

fn require_admin() -> Result<(), String> {
    match win::is_admin() {
        Ok(true) => Ok(()),
        Ok(false) => Err("This needs administrator rights. Run it from an elevated prompt.".into()),
        Err(e) => Err(format!("Couldn't check for administrator rights: {e}")),
    }
}

fn service_info(exe: PathBuf) -> ServiceInfo {
    ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec![OsString::from("--role"), OsString::from("core")],
        dependencies: Vec::new(),
        account_name: Some(SERVICE_ACCOUNT.into()),
        // Virtual accounts have no password.
        account_password: None,
    }
}

pub fn install() -> Result<(), String> {
    require_admin()?;
    let exe = std::env::current_exe().map_err(|e| format!("Couldn't find this program's path: {e}"))?;
    warn_if_user_writable(&exe);

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE).map_err(|e| format!("Couldn't open the service manager: {e}"))?;
    let info = service_info(exe);
    // Restart actions need START as well as CHANGE_CONFIG.
    let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_STATUS | ServiceAccess::START;
    let service = match manager.create_service(&info, access) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ERROR_SERVICE_EXISTS as i32) => {
            let service = manager.open_service(SERVICE_NAME, access).map_err(|e| format!("Couldn't open the existing service: {e}"))?;
            service.change_config(&info).map_err(|e| format!("Couldn't update the existing service: {e}"))?;
            service
        }
        Err(e) => return Err(format!("Couldn't create the service: {e}")),
    };
    service.set_description(DESCRIPTION).map_err(|e| format!("Couldn't set the description: {e}"))?;
    service.set_config_service_sid_info(ServiceSidType::Unrestricted).map_err(|e| format!("Couldn't set the service's SID type: {e}"))?;
    let restart = |seconds| ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(seconds) };
    service
        .update_failure_actions(ServiceFailureActions { reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)), reboot_msg: None, command: None, actions: Some(vec![restart(5), restart(30), restart(60)]) })
        .map_err(|e| format!("Couldn't set the restart rules: {e}"))?;
    // A refused start, such as a squatted pipe, exits with an error rather than crashing, and
    // should be retried too.
    service.set_failure_actions_on_non_crash_failures(true).map_err(|e| format!("Couldn't set the restart rules: {e}"))?;

    // The account's SID can only be looked up once the service exists.
    let (service_sid, _) = win::account_sid(SERVICE_ACCOUNT).map_err(|e| format!("Couldn't look up {SERVICE_ACCOUNT}: {e}"))?.ok_or_else(|| format!("Windows doesn't know {SERVICE_ACCOUNT}."))?;
    let dir = data_dir()?;
    secure_data_dir(&dir, &service_sid)?;
    println!("Data folder: {}", dir.display());

    ensure_readers_group()?;
    let user = win::current_user_sid().map_err(|e| format!("Couldn't identify the installing user: {e}"))?;
    add_to_readers_group(&user)?;
    println!("Added this user to the {READERS_GROUP} group. They need to sign out and back in before it takes effect.");

    match service.start(&[] as &[&OsStr]) {
        Ok(()) => println!("Service {SERVICE_NAME} installed and started."),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ERROR_SERVICE_ALREADY_RUNNING as i32) => println!("Service {SERVICE_NAME} updated. It was already running, so restart it to use this version."),
        Err(e) => return Err(format!("The service is installed but didn't start: {e}")),
    }
    Ok(())
}

/// A service whose program any user can replace runs that user's code under the service's
/// identity. The installer always uses Program Files, so this only catches a manual run.
fn warn_if_user_writable(exe: &Path) {
    let inside = std::env::var_os("ProgramFiles").is_some_and(|root| {
        let root = root.to_string_lossy().to_lowercase();
        exe.to_string_lossy().to_lowercase().starts_with(&format!("{}\\", root.trim_end_matches('\\')))
    });
    if !inside {
        eprintln!("Warning: {} is outside Program Files, so a user may be able to replace the service's program.", exe.display());
    }
}

/// Any user can create folders in `%ProgramData%`, so one that's already there may have been
/// planted, with files in it and rights kept for whoever made it.
fn secure_data_dir(dir: &Path, service_sid: &str) -> Result<(), String> {
    let descriptor = SecurityDescriptor::from_sddl(&sddl::data_dir(service_sid)?).map_err(|e| format!("Couldn't build the data folder's permissions: {e}"))?;
    let path = wide(dir);
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let attributes = descriptor.attributes();
            if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
                return Err(format!("Couldn't create {}: {}", dir.display(), io::Error::last_os_error()));
            }
            Ok(())
        }
        Err(e) => Err(format!("Couldn't check {}: {e}", dir.display())),
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(format!("{} isn't a plain folder. Refusing to use it; remove it and install again.", dir.display()));
            }
            refuse_unless_owned_by_admins(dir, &path)?;
            apply_permissions(dir, &path, &descriptor)
        }
    }
}

fn refuse_unless_owned_by_admins(dir: &Path, path: &[u16]) -> Result<(), String> {
    let (mut owner, mut descriptor) = (null_mut(), null_mut());
    let status = unsafe { GetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &mut owner, null_mut(), null_mut(), null_mut(), &mut descriptor) };
    if status != 0 {
        return Err(format!("Couldn't read the owner of {}: {}", dir.display(), io::Error::from_raw_os_error(status as i32)));
    }
    let trusted = unsafe { IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0 || IsWellKnownSid(owner, WinLocalSystemSid) != 0 };
    unsafe { LocalFree(descriptor) };
    if trusted {
        Ok(())
    } else {
        Err(format!("{} already exists and isn't owned by administrators, so someone else may have made it. Check it and remove it, then install again.", dir.display()))
    }
}

fn apply_permissions(dir: &Path, path: &[u16], descriptor: &SecurityDescriptor) -> Result<(), String> {
    let (mut present, mut defaulted, mut dacl) = (0, 0, null_mut::<ACL>());
    if unsafe { GetSecurityDescriptorDacl(descriptor.as_raw(), &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 {
        return Err("The data folder's permissions have no access list.".into());
    }
    let status = unsafe { SetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, null_mut(), null_mut(), dacl, null()) };
    if status != 0 {
        return Err(format!("Couldn't set the permissions on {}: {}", dir.display(), io::Error::from_raw_os_error(status as i32)));
    }
    Ok(())
}

fn ensure_readers_group() -> Result<(), String> {
    let (mut name, mut comment) = (wide(READERS_GROUP), wide(GROUP_COMMENT));
    let info = LOCALGROUP_INFO_1 { lgrpi1_name: name.as_mut_ptr(), lgrpi1_comment: comment.as_mut_ptr() };
    let mut bad_field = 0;
    match unsafe { NetLocalGroupAdd(null(), 1, (&raw const info).cast(), &mut bad_field) } {
        0 | ERROR_ALIAS_EXISTS => Ok(()),
        code if code == NERR_GroupExists => Ok(()),
        code => Err(format!("Couldn't create the {READERS_GROUP} group: {}", io::Error::from_raw_os_error(code as i32))),
    }
}

fn add_to_readers_group(user_sid: &str) -> Result<(), String> {
    let text = wide(user_sid);
    let mut sid = null_mut();
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
        return Err(format!("Couldn't read this user's SID: {}", io::Error::last_os_error()));
    }
    let group = wide(READERS_GROUP);
    let member = LOCALGROUP_MEMBERS_INFO_0 { lgrmi0_sid: sid };
    let status = unsafe { NetLocalGroupAddMembers(null(), group.as_ptr(), 0, (&raw const member).cast(), 1) };
    unsafe { LocalFree(sid) };
    match status {
        0 | ERROR_MEMBER_IN_ALIAS => Ok(()),
        code => Err(format!("Couldn't add this user to the {READERS_GROUP} group: {}", io::Error::from_raw_os_error(code as i32))),
    }
}

pub fn uninstall() -> Result<(), String> {
    require_admin()?;
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(|e| format!("Couldn't open the service manager: {e}"))?;
    let service = match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => {
            println!("Service {SERVICE_NAME} isn't installed.");
            return Ok(());
        }
        Err(e) => return Err(format!("Couldn't open the service: {e}")),
    };

    let state = |service: &windows_service::service::Service| service.query_status().map(|s| s.current_state).map_err(|e| format!("Couldn't query the service: {e}"));
    if state(&service)? != ServiceState::Stopped {
        // An error here is fine if it's already on its way down. The wait below decides.
        let _ = service.stop();
        let started = Instant::now();
        while state(&service)? != ServiceState::Stopped {
            if started.elapsed() > STOP_LIMIT {
                return Err(format!("The service didn't stop within {} seconds.", STOP_LIMIT.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete().map_err(|e| format!("Couldn't remove the service: {e}"))?;

    println!("Service {SERVICE_NAME} removed.");
    println!("Kept the data folder and the {READERS_GROUP} group, so a reinstall picks up where this left off.");
    Ok(())
}
