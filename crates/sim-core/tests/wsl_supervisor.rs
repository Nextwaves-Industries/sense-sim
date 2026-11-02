//! Spike S5: a scratch WSL distro, the supervisor, and whether a solver can outlive its parent.
//! Touches only a distro called `SenseSimSpike`, which it removes at the end.
//!
//!     cargo test --test wsl_supervisor -- --ignored --nocapture --test-threads=1

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sim_core::jobs::Cancel;
use sim_core::jobs::wsl::{self, Distro, SUPERVISOR_PATH};

const NAME: &str = "SenseSimSpike";

fn rootfs() -> PathBuf {
    sim_core::tools::solvers_dir().join("rootfs").join("ubuntu-noble-wsl-amd64.tar.gz")
}

/// How many processes of the marker command are running in the distro.
fn running(d: &Distro, marker: &str) -> usize {
    let mut n = 0;
    // The pattern is split so that this pgrep's own command line doesn't match it.
    let pattern = format!("[{}]{}", &marker[..1], &marker[1..]);
    let _ = d.exec(true, &["/usr/bin/pgrep", "-c", "-f", &pattern], &Cancel::default(), |l| n = l.trim().parse().unwrap_or(0));
    n
}

fn wait_until_gone(d: &Distro, marker: &str, within: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if running(d, marker) == 0 {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

fn user_distros_state() -> String {
    let out = Command::new("wsl.exe").env("WSL_UTF8", "1").args(["--list", "--verbose"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.contains(NAME)).collect::<Vec<_>>().join("\n")
}

#[test]
#[ignore = "creates and removes a scratch WSL distro"]
fn sim_wsl_supervisor() {
    assert!(wsl::available(), "WSL is not available");
    let before = user_distros_state();
    println!("user's distros before:\n{before}");
    let d = Distro::new(NAME);
    if d.exists().unwrap() {
        d.unregister().unwrap();
    }
    let dir = sim_core::tools::solvers_dir().join("spike-distro");
    let t0 = Instant::now();
    d.import(&dir, &rootfs()).unwrap();
    println!("imported in {:.1} s", t0.elapsed().as_secs_f64());
    d.install_supervisor().unwrap();

    // 1. It runs, delivers output, and passes on the exit code.
    let mut lines = Vec::new();
    d.run_supervised("/tmp", &[("GREETING", "hello world")], &["/bin/bash", "-c", "echo \"$GREETING from $(pwd)\""], &Cancel::default(), |l| lines.push(l.to_string())).unwrap();
    assert_eq!(lines, ["hello world from /tmp"], "arguments survive with their spaces");
    let e = d.run_supervised("/tmp", &[], &["/bin/bash", "-c", "exit 7"], &Cancel::default(), |_| {}).unwrap_err().to_string();
    assert!(e.contains('7'), "exit code is reported: {e}");
    println!("[pass] output, arguments and exit codes");

    // 2. Cancel ends the whole group, including what the solver started.
    let cancel = Cancel::default();
    let other = cancel.clone();
    let d2 = d.clone();
    let t = std::thread::spawn(move || d2.run_supervised("/tmp", &[], &["/bin/bash", "-c", "sleep 11111 & sleep 11111"], &other, |_| {}));
    std::thread::sleep(Duration::from_secs(3));
    assert!(running(&d, "sleep 11111") >= 2, "the sleepers should be up");
    cancel.cancel();
    let _ = t.join();
    let gone = wait_until_gone(&d, "sleep 11111", Duration::from_secs(25));
    println!("after cancel: gone in {gone:?}");
    assert!(gone.is_some(), "a cancelled solver kept running");
    println!("[pass] cancel ends the group");

    // 3. The parent is killed outright, as Task Manager or a crash would: TerminateProcess.
    let mut c = Command::new("wsl.exe");
    c.env("WSL_UTF8", "1").args(["-d", NAME, "--exec", SUPERVISOR_PATH, "/bin/bash", "-c", "sleep 22222 & sleep 22222"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b".").unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(running(&d, "sleep 22222") >= 2, "the sleepers should be up");
    child.kill().unwrap();
    let _ = child.wait();
    let gone = wait_until_gone(&d, "sleep 22222", Duration::from_secs(25));
    println!("after killing wsl.exe: gone in {gone:?}");
    drop(stdin);
    assert!(gone.is_some(), "the solver outlived its parent");
    println!("[pass] no orphan after the parent is killed");

    // 4. The user's distros were not disturbed, and we leave no trace.
    let after = user_distros_state();
    assert_eq!(before, after, "the user's distros changed");
    println!("[pass] the user's distros are as they were");
    d.unregister().unwrap();
    assert!(!d.exists().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
    println!("[pass] scratch distro removed");
}
