//! Runs a program in a Windows AppContainer: no network at all, loopback included, and no access
//! to the user's files except folders granted here. Proven on real Windows with Python, numpy and
//! matplotlib: API keys, the profile, the LAN, the internet and this app's MCP port were all out
//! of reach. `std::process::Command` can't start one, so this calls `CreateProcessW` itself.

use std::io::{BufRead, BufReader, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::{EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, FreeSid, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::process::{Cancel, job_object};

/// Reading and running what's in a folder, without changing it.
const READ_EXECUTE: u32 = 0x1200A9;
/// Everything, for its working folder.
const FULL: u32 = 0x1F01FF;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The container's identity. Windows keeps a profile per name, so runs share one.
struct Sid(PSID);

impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: the SID came from one of the two calls in `open`, and is freed once.
        unsafe {
            FreeSid(self.0);
        }
    }
}

fn open(container: &str) -> Result<Sid> {
    let name = wide(std::ffi::OsStr::new(container));
    let mut sid: PSID = null_mut();
    // SAFETY: the strings outlive the calls, and `sid` is written only on success.
    unsafe {
        let created = CreateAppContainerProfile(name.as_ptr(), name.as_ptr(), name.as_ptr(), null(), 0, &mut sid);
        if created != 0 {
            let derived = DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid);
            if derived != 0 {
                bail!("couldn't set up the sandbox: {created:#x}, {derived:#x}");
            }
        }
    }
    Ok(Sid(sid))
}

/// Lets the sandbox into a folder and all below it, on top of the access already there.
pub fn grant(container: &str, folder: &Path, write: bool) -> Result<()> {
    let sid = open(container)?;
    let path = wide(folder.as_os_str());
    // SAFETY: plain Win32 calls; `path`, `access` and `old` outlive them, and what they allocate
    // is released with LocalFree once, on every path.
    unsafe {
        let mut old: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let read = GetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), &mut old, null_mut(), &mut descriptor);
        if read != ERROR_SUCCESS {
            bail!("couldn't read the permissions of {}: {read}", folder.display());
        }
        let mut access: EXPLICIT_ACCESS_W = std::mem::zeroed();
        access.grfAccessPermissions = if write { FULL } else { READ_EXECUTE };
        access.grfAccessMode = GRANT_ACCESS;
        access.grfInheritance = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;
        access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
        access.Trustee.TrusteeType = TRUSTEE_IS_WELL_KNOWN_GROUP;
        access.Trustee.ptstrName = sid.0 as *mut u16;
        let mut new: *mut ACL = null_mut();
        let merged = SetEntriesInAclW(1, &access, old, &mut new);
        let set = if merged == ERROR_SUCCESS { SetNamedSecurityInfoW(path.as_ptr() as *mut u16, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), new, null_mut()) } else { merged };
        if !new.is_null() {
            LocalFree(new as _);
        }
        LocalFree(descriptor as _);
        if set != ERROR_SUCCESS {
            bail!("couldn't give the sandbox access to {}: {set}", folder.display());
        }
    }
    Ok(())
}

/// What to run.
pub struct Spec<'a> {
    pub container: &'a str,
    pub program: &'a Path,
    pub args: &'a [String],
    pub cwd: &'a Path,
    /// The whole environment, on top of the few paths Windows insists on: nothing else of the
    /// user's reaches the program.
    pub env: &'a [(String, String)],
    pub timeout: Duration,
    pub memory_bytes: usize,
}

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    pub code: u32,
    pub timed_out: bool,
}

/// A command line word, quoted as programs on Windows parse it back, and only if it has to be:
/// a program with its own rules, like `cmd.exe`, won't read a quoted `"/C"` as a switch.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// The environment block, sorted by name as Windows requires. Starting an AppContainer fails with
/// error 203 unless these paths are in it: Windows redirects them to the container's own folders.
fn environment(extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = ["APPDATA", "LOCALAPPDATA", "USERPROFILE", "SystemRoot"].iter().filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v))).collect();
    vars.extend(extra.iter().cloned());
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Runs it to its end. Each line of output goes to `on_line`, with the program's stdin to answer
/// on. A line that isn't valid UTF-8 is repaired. Ends early on a cancel or at the time limit, along
/// with anything it started: it can't outlive this app either.
pub fn run(spec: &Spec, cancel: &Cancel, mut on_line: impl FnMut(&str, &mut dyn Write)) -> Result<Ended> {
    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    let sid = open(spec.container)?;
    let (out_read, out_write) = std::io::pipe()?;
    let (in_read, mut in_write) = std::io::pipe()?;
    let child_out = out_write.as_raw_handle() as HANDLE;
    let child_in = in_read.as_raw_handle() as HANDLE;

    let mut command: Vec<u16> = std::iter::once(quote(&spec.program.to_string_lossy())).chain(spec.args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ").encode_utf16().collect();
    command.push(0);
    let cwd = wide(spec.cwd.as_os_str());
    let env = environment(spec.env);
    let mut process: PROCESS_INFORMATION;

    // SAFETY: every pointer handed over below (capabilities, handle list, buffers) lives on this
    // frame until `CreateProcessW` returns and the attribute list is deleted.
    unsafe {
        for h in [child_out, child_in] {
            if SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                bail!("couldn't set up the sandbox's pipes: {}", GetLastError());
            }
        }
        let mut caps: SECURITY_CAPABILITIES = std::mem::zeroed();
        caps.AppContainerSid = sid.0;
        let handles = [child_out, child_in];
        let mut size = 0usize;
        InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size);
        let mut buffer = vec![0u8; size];
        let list = buffer.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 2, 0, &mut size) == 0 {
            bail!("couldn't set up the sandbox: {}", GetLastError());
        }
        let ready = UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize, &caps as *const _ as _, std::mem::size_of::<SECURITY_CAPABILITIES>(), null_mut(), null()) != 0
            // Only these two reach it, not every inheritable handle in this app.
            && UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, handles.as_ptr() as _, std::mem::size_of_val(&handles), null_mut(), null()) != 0;
        if !ready {
            let e = GetLastError();
            DeleteProcThreadAttributeList(list);
            bail!("couldn't set up the sandbox: {e}");
        }
        let mut info: STARTUPINFOEXW = std::mem::zeroed();
        info.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        info.StartupInfo.hStdOutput = child_out;
        info.StartupInfo.hStdError = child_out;
        info.StartupInfo.hStdInput = child_in;
        info.lpAttributeList = list;
        process = std::mem::zeroed();
        // Suspended, so it joins its job before it can do anything.
        let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;
        let started = CreateProcessW(null(), command.as_mut_ptr(), null(), null(), 1, flags, env.as_ptr() as _, cwd.as_ptr(), &info as *const _ as *const STARTUPINFOW, &mut process);
        let e = GetLastError();
        DeleteProcThreadAttributeList(list);
        if started == 0 {
            bail!("couldn't start {} in the sandbox: error {e}", spec.program.display());
        }
    }
    // SAFETY: these handles are ours, from the call above, and closed once by OwnedHandle.
    let (process_handle, thread_handle) = unsafe { (OwnedHandle::from_raw_handle(process.hProcess as _), OwnedHandle::from_raw_handle(process.hThread as _)) };
    let Some(job) = job_object::attach_handle(process.hProcess, Some(spec.memory_bytes)) else {
        // SAFETY: the process is ours and hasn't run a single instruction of its program.
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(process.hProcess, 1);
        }
        bail!("couldn't put the sandboxed program in a job, so it wasn't run");
    };
    cancel.set_job(job);
    // SAFETY: the thread handle is valid, and resuming a suspended thread is all this does.
    unsafe {
        ResumeThread(thread_handle.as_raw_handle() as HANDLE);
    }
    // This process's copies: the output ends when the program's last copy closes.
    drop(out_write);
    drop(in_read);

    let done = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (limit, watch_cancel, watch_done, watch_late) = (spec.timeout, cancel.clone(), done.clone(), timed_out.clone());
    let watchdog = std::thread::spawn(move || {
        let started = Instant::now();
        while !watch_done.load(Ordering::SeqCst) {
            if started.elapsed() >= limit {
                watch_late.store(true, Ordering::SeqCst);
                watch_cancel.cancel();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    for line in BufReader::new(out_read).split(b'\n') {
        let Ok(line) = line else { break };
        on_line(String::from_utf8_lossy(&line).trim_end_matches('\r'), &mut in_write);
    }
    drop(in_write);
    let mut code = 1u32;
    // SAFETY: the handle is ours and open until the end of this function.
    let ended = unsafe {
        let r = WaitForSingleObject(process.hProcess, 30_000);
        GetExitCodeProcess(process.hProcess, &mut code);
        r == WAIT_OBJECT_0
    };
    done.store(true, Ordering::SeqCst);
    let _ = watchdog.join();
    // Closing it ends anything the program left behind.
    cancel.clear_job();
    drop(process_handle);
    if cancel.is_cancelled() && !timed_out.load(Ordering::SeqCst) {
        bail!("cancelled");
    }
    if !ended {
        bail!("the sandboxed program didn't end");
    }
    Ok(Ended { code, timed_out: timed_out.load(Ordering::SeqCst) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONTAINER: &str = "SenseSim.SandboxTest";

    fn folder(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sense-sim-sandbox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Runs a batch file in the sandbox. A file because `cmd` has its own quoting rules, and a test
    /// that fails to run its script would pass for the wrong reason: each script below proves it
    /// ran by printing something.
    fn cmd(script: &str, cwd: &Path, timeout: u64, cancel: &Cancel) -> (Result<Ended>, Vec<String>) {
        let system = std::env::var("SystemRoot").unwrap();
        let program = Path::new(&system).join("System32").join("cmd.exe");
        std::fs::write(cwd.join("run.cmd"), format!("@echo off\r\n{}\r\n", script.replace('\n', "\r\n"))).unwrap();
        let args = vec!["/C".to_string(), "run.cmd".to_string()];
        let mut lines = Vec::new();
        let spec = Spec { container: TEST_CONTAINER, program: &program, args: &args, cwd, env: &[("MARKER".to_string(), "seen".to_string())], timeout: Duration::from_secs(timeout), memory_bytes: 1 << 31 };
        let ended = run(&spec, cancel, |l, _| lines.push(l.to_string()));
        (ended, lines)
    }

    #[test]
    fn a_word_survives_the_command_line() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("/C"), "/C", "cmd.exe needs its switches bare");
        assert_eq!(quote(""), "\"\"", "an empty word still counts");
        assert_eq!(quote(r"C:\a b\c.py"), "\"C:\\a b\\c.py\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote(r"ends\"), r"ends\", "nothing to quote");
        assert_eq!(quote(r"a b\"), "\"a b\\\\\"", "a final backslash mustn't escape the closing quote");
    }

    #[test]
    fn the_environment_is_sorted_and_holds_only_what_it_should() {
        // SAFETY: only this test reads this name, and it sets it before any thread asks for it.
        unsafe { std::env::set_var("SENSE_SIM_TEST_API_KEY", "sk-secret") };
        let block = String::from_utf16(&environment(&[("TEMP".into(), "x".into()), ("ZED".into(), "1".into()), ("ALPHA".into(), "2".into())])).unwrap();
        let names: Vec<String> = block.split('\0').filter(|s| !s.is_empty()).map(|s| s.split('=').next().unwrap().to_uppercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "Windows wants these sorted");
        for must in ["APPDATA", "LOCALAPPDATA", "USERPROFILE", "SYSTEMROOT", "TEMP"] {
            assert!(names.iter().any(|n| n == must), "{must}: starting fails with error 203 without it");
        }
        assert!(!block.contains("sk-secret"), "the user's environment stays out");
    }

    /// These start real AppContainers: the sandbox is the point, so it has to be tested for real.
    #[test]
    fn it_runs_in_its_folder_and_sees_only_its_environment() {
        let work = folder("run");
        grant(TEST_CONTAINER, &work, true).unwrap();
        let (ended, lines) = cmd("echo hello %MARKER%\necho written> out.txt", &work, 30, &Cancel::default());
        assert_eq!(ended.unwrap(), Ended { code: 0, timed_out: false }, "{lines:?}");
        assert_eq!(lines.first().map(|l| l.trim()), Some("hello seen"), "{lines:?}");
        assert!(work.join("out.txt").exists(), "its folder is writable");
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn it_cant_read_or_write_outside_what_it_was_granted() {
        let (work, outside) = (folder("inside"), folder("outside"));
        std::fs::write(outside.join("secret.txt"), "sk-secret").unwrap();
        grant(TEST_CONTAINER, &work, true).unwrap();
        let script = format!("echo started\ntype \"{0}\\secret.txt\"\necho x> \"{0}\\planted.txt\"\necho finished", outside.display());
        let (ended, lines) = cmd(&script, &work, 30, &Cancel::default());
        ended.unwrap();
        // It really tried: this isn't passing just because the script never ran.
        assert!(lines.iter().any(|l| l.contains("started")) && lines.iter().any(|l| l.contains("finished")), "{lines:?}");
        assert!(lines.iter().any(|l| l.to_lowercase().contains("denied")), "Windows refused it: {lines:?}");
        assert!(!lines.iter().any(|l| l.contains("sk-secret")), "{lines:?}");
        assert!(!outside.join("planted.txt").exists(), "it wrote outside its folder");
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_read_only_grant_stays_read_only() {
        let (work, library) = (folder("ro-work"), folder("ro-library"));
        std::fs::write(library.join("lib.txt"), "library contents").unwrap();
        grant(TEST_CONTAINER, &work, true).unwrap();
        grant(TEST_CONTAINER, &library, false).unwrap();
        let script = format!("type \"{0}\\lib.txt\"\necho x> \"{0}\\changed.txt\"", library.display());
        let (ended, lines) = cmd(&script, &work, 30, &Cancel::default());
        ended.unwrap();
        assert!(lines.iter().any(|l| l.contains("library contents")), "{lines:?}");
        assert!(!library.join("changed.txt").exists(), "a script could tamper with the runtime");
        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&library);
    }

    #[test]
    fn the_time_limit_ends_it_and_says_so() {
        let work = folder("late");
        grant(TEST_CONTAINER, &work, true).unwrap();
        let started = Instant::now();
        // Not `timeout` or a network ping: both need things the sandbox denies.
        let (ended, _) = cmd("for /L %%i in (1,1,100000000) do @rem", &work, 1, &Cancel::default());
        assert!(ended.unwrap().timed_out);
        assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn cancel_ends_it() {
        let work = folder("cancel");
        grant(TEST_CONTAINER, &work, true).unwrap();
        let cancel = Cancel::default();
        let stopper = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            stopper.cancel();
        });
        let started = Instant::now();
        let (ended, _) = cmd("for /L %%i in (1,1,100000000) do @rem", &work, 120, &cancel);
        assert!(ended.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_program_that_isnt_there_is_an_error_not_a_hang() {
        let work = folder("missing");
        let spec = Spec { container: TEST_CONTAINER, program: Path::new(r"C:\no\such\program.exe"), args: &[], cwd: &work, env: &[], timeout: Duration::from_secs(5), memory_bytes: 1 << 30 };
        assert!(run(&spec, &Cancel::default(), |_, _| {}).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }
}
