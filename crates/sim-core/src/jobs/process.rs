//! Runs a solver as a child process: no console window, killed with Sense Sim however Sense Sim
//! ends (a crash, Task Manager, `process::exit`), cancellable, and with its output delivered a
//! line at a time.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};

/// Shared between a job and whoever may cancel it.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(windows)]
    job: Arc<Mutex<Option<job_object::Job>>>,
}

impl Cancel {
    /// Ends the running process and everything it started. Killing only the process would leave
    /// a solver a launcher started running, still holding our pipes open.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        if let Ok(job) = self.job.lock()
            && let Some(job) = job.as_ref()
        {
            job.terminate();
        }
        if let Ok(mut c) = self.child.lock()
            && let Some(child) = c.as_mut()
        {
            let _ = child.kill();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Windows ends every process of a job when its last handle closes, which happens when this
/// process dies, so a solver cannot outlive Sense Sim.
#[cfg(windows)]
mod job_object {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle can be used and closed from any thread.
    unsafe impl Send for Job {}

    impl Job {
        /// Ends every process in the job.
        pub fn terminate(&self) {
            // SAFETY: the handle is ours and open for as long as `self`.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is ours and closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn attach(child: &std::process::Child) -> Option<Job> {
        // SAFETY: plain Win32 calls on handles we own; `info` outlives the call.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = Job(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, &info as *const _ as *const core::ffi::c_void, std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
            (set != 0 && AssignProcessToJobObject(job.0, child.as_raw_handle() as _) != 0).then_some(job)
        }
    }
}

/// What a finished process left behind.
#[derive(Debug, Default, Clone)]
pub struct Output {
    /// The last lines of its output, for error messages.
    pub tail: Vec<String>,
}

const TAIL: usize = 40;

/// Runs `cmd` to its end, handing each line of stdout and stderr to `on_line`. A non-zero exit is
/// an error carrying the end of the output; so is a cancel.
pub fn run(mut cmd: Command, cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<Output> {
    cmd.stdin(Stdio::null());
    start(cmd, cancel, on_line, |_| {})
}

/// As [`run`], handing the child's stdin (which `cmd` must have piped) to `with_stdin` once started.
pub fn run_with_stdin(cmd: Command, cancel: &Cancel, on_line: impl FnMut(&str), with_stdin: impl FnOnce(std::process::ChildStdin)) -> Result<Output> {
    start(cmd, cancel, on_line, with_stdin)
}

fn start(mut cmd: Command, cancel: &Cancel, mut on_line: impl FnMut(&str), with_stdin: impl FnOnce(std::process::ChildStdin)) -> Result<Output> {
    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let program = cmd.get_program().to_string_lossy().to_string();
    let mut child = cmd.spawn().with_context(|| format!("starting {program}"))?;
    #[cfg(windows)]
    {
        *cancel.job.lock().map_err(|_| anyhow!("poisoned"))? = job_object::attach(&child);
    }
    if let Some(stdin) = child.stdin.take() {
        with_stdin(stdin);
    }
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout from {program}"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr from {program}"))?;
    *cancel.child.lock().map_err(|_| anyhow!("poisoned"))? = Some(child);
    // A cancel that arrived between the check above and here found nothing to kill.
    if cancel.is_cancelled() {
        cancel.cancel();
    }

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let tx2 = tx.clone();
    let out = std::thread::spawn(move || pump(stdout, tx));
    let err = std::thread::spawn(move || pump(stderr, tx2));
    let mut tail = std::collections::VecDeque::with_capacity(TAIL);
    for line in rx {
        on_line(&line);
        if tail.len() == TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    let _ = out.join();
    let _ = err.join();
    let status = {
        let mut guard = cancel.child.lock().map_err(|_| anyhow!("poisoned"))?;
        let mut child = guard.take().ok_or_else(|| anyhow!("{program} vanished"))?;
        child.wait().with_context(|| format!("waiting for {program}"))?
    };
    #[cfg(windows)]
    {
        // Closing it ends anything the process left behind.
        *cancel.job.lock().map_err(|_| anyhow!("poisoned"))? = None;
    }
    if cancel.is_cancelled() {
        bail!("cancelled");
    }
    let tail: Vec<String> = tail.into();
    if !status.success() {
        bail!("{program} failed ({status}):\n{}", tail.join("\n"));
    }
    Ok(Output { tail })
}

fn pump(stream: impl Read, tx: std::sync::mpsc::Sender<String>) {
    for line in BufReader::new(stream).split(b'\n') {
        let Ok(line) = line else { break };
        // Solvers aren't always UTF-8 (wsl.exe, some Fortran runtimes).
        let text = String::from_utf8_lossy(&line).trim_end().to_string();
        if tx.send(text).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut c = Command::new("cmd");
        c.args(["/C", script]);
        c
    }

    #[test]
    fn delivers_lines_and_reports_failure_with_output() {
        let mut seen = Vec::new();
        run(shell("echo one&& echo two"), &Cancel::default(), |l| seen.push(l.to_string())).unwrap();
        assert_eq!(seen, ["one", "two"]);
        let e = run(shell("echo boom&& exit 3"), &Cancel::default(), |_| {}).unwrap_err().to_string();
        assert!(e.contains("boom"), "{e}");
    }

    #[test]
    fn a_missing_program_is_an_error() {
        assert!(run(Command::new("no-such-program-sense-sim"), &Cancel::default(), |_| {}).is_err());
    }

    #[test]
    fn cancel_ends_a_running_process() {
        let cancel = Cancel::default();
        let other = cancel.clone();
        let started = std::time::Instant::now();
        let t = std::thread::spawn(move || run(shell("ping -n 30 127.0.0.1"), &other, |_| {}));
        std::thread::sleep(std::time::Duration::from_millis(400));
        cancel.cancel();
        let e = t.join().unwrap().unwrap_err().to_string();
        assert!(e.contains("cancelled"), "{e}");
        assert!(started.elapsed().as_secs() < 20, "cancel took {:?}", started.elapsed());
    }
}
