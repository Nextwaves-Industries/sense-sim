//! Running Linux solvers in a WSL distro of our own.
//!
//! Rules, each from a way this goes wrong:
//! - Only ever touch the distro named here. Never `wsl --shutdown` (it would stop the user's own
//!   distros, which share the VM), never `.wslconfig`.
//! - `wsl.exe` dying does not end the Linux process: wslhost.exe takes it over. So every solver runs
//!   under [`SUPERVISOR`], which ends it when its stdin closes or its heartbeat stops.
//! - Cases live on the distro's own disk: `/mnt/c` is far too slow for a solver's files.
//! - Text is written with LF, and arguments go through `--exec` so no shell re-parses them.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use super::process::{self, Cancel, Output};

/// Where the supervisor is installed in the distro.
pub const SUPERVISOR_PATH: &str = "/opt/sensesim/supervise.sh";

/// Runs `$@` in a new session, so that a whole process group can be ended. Sense Sim writes a byte
/// to its stdin every few seconds. On EOF (Sense Sim's end of the pipe closed), or after
/// `MISSED` silent seconds (the relay was killed, or the app hung), the group is ended: first asked
/// to stop, then killed. The solver's exit code becomes this script's.
pub const SUPERVISOR: &str = r#"#!/bin/bash
MISSED=${SENSESIM_HEARTBEAT_SECONDS:-15}
setsid "$@" <&- &
pid=$!
end_group() {
    kill -TERM -- "-$pid" 2>/dev/null
    for _ in 1 2 3 4 5; do
        kill -0 -- "-$pid" 2>/dev/null || exit "$1"
        sleep 1
    done
    kill -KILL -- "-$pid" 2>/dev/null
    exit "$1"
}
trap 'end_group 143' TERM INT HUP
silent=0
while kill -0 "$pid" 2>/dev/null; do
    if read -r -t 1 -n 1 _; then
        silent=0
    else
        rc=$?
        # Above 128 is a timeout; anything else is end of file.
        if [ "$rc" -le 128 ]; then
            end_group 137
        fi
        silent=$((silent + 1))
        if [ "$silent" -ge "$MISSED" ]; then
            end_group 137
        fi
    fi
done
wait "$pid"
"#;

/// What every supervised run starts with. The VM's memory is shared with the user's own distros, and
/// the kernel's answer to running out is to kill something, perhaps of theirs. So a run may address 80%
/// of what's free when it starts: past that its allocations fail and only it stops. The limit is passed
/// on as `SENSESIM_LIMIT_KB`, for [`RANK_LIMIT`]. Written with shell builtins: nothing to be missing.
const START: &str = "kb=0; while read -r k v _; do [ \"$k\" = MemAvailable: ] && kb=$v; done < /proc/meminfo; kb=$((kb * 8 / 10)); export SENSESIM_LIMIT_KB=$kb; [ \"$kb\" -gt 0 ] && ulimit -v \"$kb\"; cd \"$0\" && exec \"$@\"";

/// Shell that divides the run's memory limit among the 8 processes of a parallel solver, since `ulimit`
/// counts each process alone. Not below 2 GB, so a small run on a small machine isn't stopped by its
/// share of the limit: 8 ranks of OpenFOAM start on a small case even at 1 GB each (`tests/wsl_limits.rs`).
/// Use it in a subshell, so serial steps keep the whole limit: `reconstructPar` holds what all the ranks held.
pub const RANK_LIMIT: &str = "ulimit -v $(( SENSESIM_LIMIT_KB / 8 > 2097152 ? SENSESIM_LIMIT_KB / 8 : 2097152 ))";

/// Whether a run of this size fits: what it says if it doesn't. `available` is what's free now.
pub fn memory_problem(needed: f64, available: f64) -> Option<String> {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    (needed > available * 0.8).then(|| format!("This mesh would need about {:.0} GB of memory. The solvers' Linux environment has {:.0} GB free, which it shares with your own Linux distros. Use a larger element size.", needed / GB, available / GB))
}

/// `MemAvailable` out of `/proc/meminfo`, in bytes.
fn parse_available(meminfo: &str) -> Option<f64> {
    meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:")?.split_ascii_whitespace().next()?.parse::<f64>().ok()).map(|kb| kb * 1024.0)
}

/// `wsl.exe` with its output in UTF-8 instead of UTF-16.
fn wsl() -> Command {
    let mut c = Command::new("wsl.exe");
    c.env("WSL_UTF8", "1");
    c
}

/// Output of a quick management command.
fn manage(args: &[&str]) -> Result<String> {
    let mut lines = Vec::new();
    let mut cmd = wsl();
    cmd.args(args);
    process::run(cmd, &Cancel::default(), |l| lines.push(l.to_string())).with_context(|| format!("wsl {}", args.join(" ")))?;
    Ok(lines.join("\n"))
}

/// Is WSL usable on this machine at all?
pub fn available() -> bool {
    manage(&["--status"]).is_ok()
}

/// The registered distros.
pub fn distros() -> Result<Vec<String>> {
    // A machine with none reports that as a failure; to us it's an empty list.
    Ok(manage(&["--list", "--quiet"]).unwrap_or_default().lines().map(|l| l.trim().trim_matches('\0').to_string()).filter(|l| !l.is_empty()).collect())
}

/// A distro this program owns.
#[derive(Debug, Clone)]
pub struct Distro {
    pub name: String,
    /// The account solvers run as. `None`: the distro's default.
    pub user: Option<String>,
}

impl Distro {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), user: None }
    }

    pub fn exists(&self) -> Result<bool> {
        Ok(distros()?.iter().any(|d| d == &self.name))
    }

    /// Creates the distro from a root filesystem tarball, in `dir`.
    pub fn import(&self, dir: &Path, rootfs: &Path) -> Result<()> {
        if self.exists()? {
            bail!("a distro called {} already exists", self.name);
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let dir = dir.to_string_lossy();
        let rootfs = rootfs.to_string_lossy();
        manage(&["--import", &self.name, &dir, &rootfs, "--version", "2"])?;
        Ok(())
    }

    /// Removes the distro and its disk. Only ever this one name.
    pub fn unregister(&self) -> Result<()> {
        manage(&["--unregister", &self.name])?;
        Ok(())
    }

    /// Stops this distro alone. The user's distros are untouched.
    pub fn terminate(&self) -> Result<()> {
        manage(&["--terminate", &self.name])?;
        Ok(())
    }

    fn command(&self, root: bool) -> Command {
        let mut c = wsl();
        c.args(["-d", &self.name]);
        if root {
            c.args(["-u", "root"]);
        } else if let Some(u) = &self.user {
            c.args(["-u", u]);
        }
        c.arg("--exec");
        c
    }

    /// Runs a program with no supervision, for short administrative work.
    pub fn exec(&self, root: bool, argv: &[&str], cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<Output> {
        let mut c = self.command(root);
        c.args(argv);
        process::run(c, cancel, on_line)
    }

    /// Runs a shell script as root. `set -e`, LF line endings.
    pub fn script(&self, root: bool, script: &str, cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<Output> {
        let text = format!("set -e\n{}", script.replace("\r\n", "\n"));
        self.exec(root, &["/bin/bash", "-c", &text], cancel, on_line)
    }

    /// Writes a file in the distro, with Unix line endings as given.
    pub fn write_file(&self, path: &str, text: &str, root: bool) -> Result<()> {
        let mut c = self.command(root);
        c.args(["/bin/bash", "-c", "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", "_", path]);
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        hide_console(&mut c);
        let mut child = c.spawn().context("starting wsl.exe")?;
        child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?.write_all(text.replace("\r\n", "\n").as_bytes()).context("writing to the distro")?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("writing {path} in the distro: {}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }

    /// Installs the supervisor. Idempotent.
    pub fn install_supervisor(&self) -> Result<()> {
        self.write_file(SUPERVISOR_PATH, SUPERVISOR, true)?;
        self.script(true, &format!("chmod 755 {SUPERVISOR_PATH}"), &Cancel::default(), |_| {})?;
        Ok(())
    }

    /// How much memory the VM has free now, in bytes. It's the whole VM's: the user's distros count.
    pub fn memory_available(&self) -> Result<f64> {
        let mut text = String::new();
        self.exec(false, &["/usr/bin/cat", "/proc/meminfo"], &Cancel::default(), |l| {
            text.push_str(l);
            text.push('\n');
        })?;
        parse_available(&text).ok_or_else(|| anyhow!("no MemAvailable in /proc/meminfo"))
    }

    /// Runs `argv` in `workdir` under the supervisor, with a heartbeat and a memory limit. Ends with the
    /// solver, with `cancel`, or with this process, however it goes.
    pub fn run_supervised(&self, workdir: &str, env: &[(&str, &str)], argv: &[&str], cancel: &Cancel, on_line: impl FnMut(&str)) -> Result<Output> {
        let mut c = self.command(false);
        // Env and limit first, then the supervisor, with no shell parsing of the arguments: each goes
        // through `env` and [`START`] as one word.
        c.args(["/usr/bin/env"]);
        for (k, v) in env {
            c.arg(format!("{k}={v}"));
        }
        c.args(["/bin/sh", "-c", START, workdir, SUPERVISOR_PATH]);
        c.args(argv);
        c.stdin(Stdio::piped());
        let stop = Arc::new(AtomicBool::new(false));
        let stop_beat = stop.clone();
        let r = process::run_with_stdin(c, cancel, on_line, move |mut stdin| {
            std::thread::spawn(move || {
                while !stop_beat.load(Ordering::Relaxed) {
                    if stdin.write_all(b".").and_then(|()| stdin.flush()).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            });
        });
        stop.store(true, Ordering::Relaxed);
        r
    }

    /// Copies a Windows folder into the distro as `to` (created). Through a tar pipe, not `/mnt/c`.
    pub fn upload(&self, from: &Path, to: &str) -> Result<()> {
        let mut c = self.command(false);
        c.args(["/bin/bash", "-c", "rm -rf \"$1\" && mkdir -p \"$1\" && tar -xf - -C \"$1\"", "_", to]);
        let mut tar = Command::new("tar");
        tar.args(["-cf", "-", "-C"]).arg(from).arg(".");
        pipe(tar, c)
    }

    /// Copies a folder of the distro to a Windows folder (created), optionally only some paths in it.
    pub fn download(&self, from: &str, to: &Path, only: &[&str]) -> Result<()> {
        std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
        let mut c = self.command(false);
        c.args(["/usr/bin/tar", "-cf", "-", "-C", from]);
        if only.is_empty() {
            c.arg(".");
        } else {
            c.args(only);
        }
        let mut tar = Command::new("tar");
        tar.args(["-xf", "-", "-C"]).arg(to);
        pipe(c, tar)
    }
}

fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// `from | to`, both to their ends, failing if either fails.
fn pipe(mut from: Command, mut to: Command) -> Result<()> {
    hide_console(&mut from);
    hide_console(&mut to);
    from.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut source = from.spawn().context("starting the sending side")?;
    let out = source.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    to.stdin(out).stdout(Stdio::null()).stderr(Stdio::piped());
    let sink = to.spawn().context("starting the receiving side")?;
    let sink_out = sink.wait_with_output().context("waiting for the receiving side")?;
    let source_out = source.wait_with_output().context("waiting for the sending side")?;
    if !sink_out.status.success() {
        bail!("receiving files failed: {}", String::from_utf8_lossy(&sink_out.stderr));
    }
    if !source_out.status.success() {
        bail!("sending files failed: {}", String::from_utf8_lossy(&source_out.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_supervisor_has_unix_line_endings_and_no_shutdown() {
        assert!(!SUPERVISOR.contains('\r'));
        assert!(SUPERVISOR.starts_with("#!/bin/bash\n"));
        assert!(!SUPERVISOR.contains("--shutdown"));
    }

    #[test]
    fn what_is_free_comes_from_meminfo() {
        let text = "MemTotal:       16000000 kB\nMemFree:         1000000 kB\nMemAvailable:    8000000 kB\nBuffers:           10000 kB\n";
        assert_eq!(parse_available(text), Some(8_000_000.0 * 1024.0));
        assert_eq!(parse_available("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn a_run_that_would_crowd_the_vm_is_refused() {
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        assert!(memory_problem(70.0 * GB, 100.0 * GB).is_none());
        let refused = memory_problem(90.0 * GB, 100.0 * GB).expect("past 80% of what's free");
        assert!(refused.contains("about 90 GB") && refused.contains("100 GB free") && refused.contains("larger element size"), "{refused}");
    }
}
