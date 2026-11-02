//! Python for the assistant: the app's own copy, in a Windows sandbox, downloaded only once the
//! user agrees. A script gets its chat's working folder and nothing else: no network, no API keys,
//! none of the user's files. It reaches the app only through `sensesim.call`, and every such call
//! passes the gate and lands in the turn's log, so the self-checks still see what it did.

use std::hash::{BuildHasher, RandomState};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use crossbeam_channel::RecvTimeoutError;
use serde_json::{Value, json};
use sim_core::jobs::Cancel;
use sim_core::provision::{self, Download};

use super::gate::{CallCtx, Origin};
use super::harness::{Turn, Who};
use super::llm::{Block, ToolDef};
use super::tools::{self, ToolOutput};
use super::{Event, Question, images};

pub const TOOL: &str = "python_run";

const CONTAINER: &str = "SenseSim.Python";
const PYTHON_VERSION: &str = "3.13.18";
const UV: Download = Download { url: "https://github.com/astral-sh/uv/releases/download/0.13.6/uv-x86_64-pc-windows-msvc.zip", sha256: "A2A8820D5EEA32F08EF467EF3B7F66883A9B076D5737A2FC2FBB5F458D1BB50F", size: 15_239_500 };
/// Every package with its hashes, for `--require-hashes`: nothing unpinned gets installed.
const PACKAGES: &str = include_str!("python_packages.lock");
const HELPER: &str = include_str!("python_helper.py");

const TIMEOUT_DEFAULT_S: u64 = 120;
const TIMEOUT_MAX_S: u64 = 1800;
const MEMORY_BYTES: usize = 4 << 30;
const OUTPUT_KEPT_CHARS: usize = 20_000;
const FIGURES_KEPT: usize = 4;
const KEEP_INSTALL: &str = "Install Python";

pub fn def() -> ToolDef {
    ToolDef {
        name: TOOL.into(),
        description: "Run a Python 3.13 script for calculations, data, plots and file formats: numpy, scipy, pandas, matplotlib, openpyxl and pypdf are there. It runs in a sandbox, in your working folder for this chat: no network, and none of the user's other files. Save what you make there; plots saved as images come back for you to see. `import sensesim; sensesim.call(\"get_scene\")` calls the app's tools, with the same approvals as any call. Anything a script computes counts as an estimate, and it can't record measurements.".into(),
        schema: json!({ "type": "object", "properties": {
            "code": { "type": "string", "description": "The script" },
            "timeout_s": { "type": "integer", "description": "Longest it may run, 120 by default, 1800 at most" }
        }, "required": ["code"] }),
    }
}

fn root() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    Path::new(&base).join("SenseSim").join("python")
}

fn python_exe(root: &Path) -> PathBuf {
    root.join("venv").join("Scripts").join("python.exe")
}

fn marker(root: &Path) -> PathBuf {
    root.join("ready.txt")
}

/// Whether everything is installed, for this version of the app.
pub fn installed() -> bool {
    let r = root();
    python_exe(&r).is_file() && std::fs::read_to_string(marker(&r)).is_ok_and(|v| v.trim() == PYTHON_VERSION)
}

/// How `uv` is told to stay in its own folder. Without `--no-bin` and `--no-registry` it would put
/// a `python3.13.exe` on the user's PATH and register itself in the Windows registry.
fn uv_env(root: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![("UV_PYTHON_INSTALL_DIR", root.join("cpython")), ("UV_CACHE_DIR", root.join("cache")), ("UV_TOOL_DIR", root.join("tools")), ("UV_PYTHON_BIN_DIR", root.join("bin"))]
}

fn install_python_args() -> Vec<String> {
    ["python", "install", PYTHON_VERSION, "--no-bin", "--no-registry"].iter().map(|s| s.to_string()).collect()
}

fn uv(root: &Path, args: &[String], cancel: &Cancel, on_line: &mut dyn FnMut(&str)) -> Result<()> {
    let mut cmd = Command::new(root.join("uv").join("uv.exe"));
    cmd.args(args).current_dir(root).env("UV_NO_CONFIG", "1").env("UV_PYTHON_PREFERENCE", "only-managed");
    for (k, v) in uv_env(root) {
        cmd.env(k, v);
    }
    sim_core::jobs::process::run(cmd, cancel, |l| on_line(l)).with_context(|| format!("uv {}", args.first().map_or("", String::as_str)))?;
    Ok(())
}

/// Downloads and sets up everything, in its own folder. Safe to run again after a failure.
fn install(cancel: &Cancel, say: &dyn Fn(String)) -> Result<()> {
    let r = root();
    std::fs::create_dir_all(&r)?;
    let _ = std::fs::remove_file(marker(&r));
    say("Downloading uv, pinned by its hash.".into());
    let zip = r.join("uv.zip");
    provision::fetch(&UV, &zip, cancel, |_, _| {})?;
    provision::unzip(&zip, &r.join("uv"))?;
    let _ = std::fs::remove_file(&zip);
    let mut quiet = |_: &str| {};
    say(format!("Installing Python {PYTHON_VERSION}."));
    uv(&r, &install_python_args(), cancel, &mut quiet)?;
    let venv = r.join("venv");
    uv(&r, &["venv".to_string(), venv.to_string_lossy().to_string(), "--python".to_string(), PYTHON_VERSION.to_string()], cancel, &mut quiet)?;
    say("Installing numpy, scipy, pandas, matplotlib, openpyxl and pypdf, each checked against its hash.".into());
    let lock = r.join("packages.lock");
    std::fs::write(&lock, PACKAGES)?;
    uv(&r, &["pip".to_string(), "install".to_string(), "--python".to_string(), python_exe(&r).to_string_lossy().to_string(), "--require-hashes".to_string(), "-r".to_string(), lock.to_string_lossy().to_string()], cancel, &mut quiet)?;
    // Where scripts can read it and can't change it.
    std::fs::write(venv.join("Lib").join("site-packages").join("sensesim.py"), HELPER)?;
    let _ = std::fs::remove_dir_all(r.join("cache"));
    say("Letting the sandbox read it.".into());
    sim_core::jobs::sandbox::grant(CONTAINER, &r, false)?;
    std::fs::write(marker(&r), PYTHON_VERSION)?;
    Ok(())
}

/// Asks for consent on a card, the first time, stating the size. Blocks this worker thread.
fn ensure_installed(turn: &Turn) -> std::result::Result<(), ToolOutput> {
    if installed() {
        return Ok(());
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    let question = Question {
        text: "The assistant wants to run a Python script, and Python isn't set up yet. This downloads about 150 MB from GitHub and the Python package index and takes about 400 MB on disk, in this app's own folder. Your system Python isn't touched. Scripts run in a sandbox with no network and none of your other files.".to_string(),
        options: vec![KEEP_INSTALL.to_string(), "Not now".to_string()],
        candidates: Vec::new(),
        allow_text: false,
        input: String::new(),
        answer: None,
        reply: Some(tx),
        approval: true,
    };
    if turn.ev.send(Event::Question(question)).is_err() {
        return Err(ToolOutput::err("The app is closing."));
    }
    let said = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(a) => break a.summary(),
            Err(RecvTimeoutError::Disconnected) => break String::new(),
            Err(RecvTimeoutError::Timeout) if turn.cancel.load(Ordering::Relaxed) => break String::new(),
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    if said != format!("User answered: {KEEP_INSTALL}") {
        return Err(ToolOutput::err("The user didn't install Python, so scripts can't run. Do what you can without one, and don't ask again in this chat unless they bring it up."));
    }
    let cancel = stop_with(turn);
    let ev = turn.ev.clone();
    let say = move |t: String| {
        let _ = ev.send(Event::Info(t));
    };
    match install(&cancel, &say) {
        Ok(()) => {
            say("Python is ready.".to_string());
            Ok(())
        }
        Err(e) => Err(ToolOutput::err(format!("Python couldn't be installed: {e:#}. Tell the user what failed; they may be offline, or a firewall may block GitHub."))),
    }
}

/// A `Cancel` that fires when the turn is stopped.
fn stop_with(turn: &Turn) -> Cancel {
    let cancel = Cancel::default();
    let (stop, flag) = (cancel.clone(), turn.cancel.clone());
    std::thread::spawn(move || {
        while !stop.is_cancelled() {
            if flag.load(Ordering::Relaxed) {
                stop.cancel();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    cancel
}

/// A random marker for this run's calls, so printing a file can't trigger one.
fn fresh_prefix() -> String {
    let s = RandomState::new();
    format!("{:016x}{:016x}", s.hash_one(1u8), s.hash_one(2u8))
}

/// The environment a script gets. Nothing else of the user's: see `sandbox::run` for the rest.
fn script_env(files: &Path, prefix: &str) -> Vec<(String, String)> {
    let tmp = files.join(".tmp").to_string_lossy().to_string();
    vec![
        ("MPLBACKEND".into(), "Agg".into()),
        ("MPLCONFIGDIR".into(), files.join(".matplotlib").to_string_lossy().to_string()),
        ("PYTHONUTF8".into(), "1".into()),
        ("SENSESIM_RPC_PREFIX".into(), prefix.to_string()),
        ("TEMP".into(), tmp.clone()),
        ("TMP".into(), tmp),
    ]
}

/// One line of a script's output: its own, or a call for the app.
fn parse_call(line: &str, prefix: &str) -> Option<std::result::Result<(String, Value), String>> {
    let body = line.strip_prefix('\u{1e}')?.strip_prefix(prefix)?.strip_prefix(' ')?;
    Some(match serde_json::from_str::<Value>(body) {
        Ok(v) => match v["tool"].as_str() {
            Some(t) => Ok((t.to_string(), v["input"].clone())),
            None => Err("a call needs a tool".to_string()),
        },
        Err(e) => Err(format!("that call wasn't valid JSON: {e}")),
    })
}

/// Keeps the start and the end of long output, where the interesting parts usually are.
fn clip_output(lines: &[String]) -> String {
    let all = lines.join("\n");
    let chars: Vec<char> = all.chars().collect();
    if chars.len() <= OUTPUT_KEPT_CHARS {
        return all;
    }
    let (head, tail) = (OUTPUT_KEPT_CHARS / 4, OUTPUT_KEPT_CHARS * 3 / 4);
    format!("{}\n[… {} characters of output left out …]\n{}", chars[..head].iter().collect::<String>(), chars.len() - head - tail, chars[chars.len() - tail..].iter().collect::<String>())
}

/// Files the script made or changed, newest first, leaving out the app's own housekeeping.
fn files_since(files: &Path, since: SystemTime) -> Vec<PathBuf> {
    fn walk(dir: &Path, since: SystemTime, out: &mut Vec<(SystemTime, PathBuf)>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(&path, since, out);
            } else if let Ok(modified) = e.metadata().and_then(|m| m.modified())
                && modified >= since
            {
                out.push((modified, path));
            }
        }
    }
    let mut found = Vec::new();
    walk(files, since, &mut found);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

fn is_figure(path: &Path) -> bool {
    path.extension().is_some_and(|e| ["png", "jpg", "jpeg"].contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Runs a script in the sandbox. The gate has already agreed to `python_run` itself.
pub fn run(input: &Value, turn: &Turn) -> ToolOutput {
    let Some(code) = input["code"].as_str().filter(|c| !c.trim().is_empty()) else { return ToolOutput::err("code is required") };
    let Some(files) = turn.files.clone() else { return ToolOutput::err("This run has no working folder for a script.") };
    if let Err(no) = ensure_installed(turn) {
        return no;
    }
    match run_script(code, input["timeout_s"].as_u64().unwrap_or(TIMEOUT_DEFAULT_S).clamp(1, TIMEOUT_MAX_S), &files, turn) {
        Ok(out) => out,
        // Never falls back to running it outside the sandbox.
        Err(e) => ToolOutput::err(format!("The script couldn't run in the sandbox, so it wasn't run at all: {e:#}")),
    }
}

fn run_script(code: &str, timeout_s: u64, files: &Path, turn: &Turn) -> Result<ToolOutput> {
    for dir in [files.to_path_buf(), files.join(".tmp"), files.join(".matplotlib"), files.join(".scripts")] {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    sim_core::jobs::sandbox::grant(CONTAINER, files, true)?;
    let started = SystemTime::now();
    let script = files.join(".scripts").join(format!("script-{}.py", started.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis())));
    std::fs::write(&script, code)?;

    let r = root();
    let exe = python_exe(&r);
    if !exe.is_file() {
        bail!("Python isn't where it was installed: {}", exe.display());
    }
    let prefix = fresh_prefix();
    let env = script_env(files, &prefix);
    let args = vec!["-I".to_string(), "-B".to_string(), script.to_string_lossy().to_string()];
    let spec = sim_core::jobs::sandbox::Spec { container: CONTAINER, program: &exe, args: &args, cwd: files, env: &env, timeout: Duration::from_secs(timeout_s), memory_bytes: MEMORY_BYTES };
    let cancel = stop_with(turn);
    let ctx = CallCtx { origin: Origin::Script, log: Some(turn.log.clone()), cancel: Some(turn.cancel.clone()), planning: Some(turn.planning.clone()) };
    let mut lines: Vec<String> = Vec::new();
    let mut calls = 0usize;
    let ended = sim_core::jobs::sandbox::run(&spec, &cancel, |line, stdin| match parse_call(line, &prefix) {
        None => lines.push(line.to_string()),
        Some(call) => {
            calls += 1;
            let out = match call {
                Ok((tool, args)) => {
                    let out = tools::call_ctx(&turn.tx, &tool, args.clone(), ctx.clone());
                    turn.log.push(Who::Script, &tool, &args, &out);
                    out
                }
                Err(why) => ToolOutput::err(why),
            };
            let reply = json!({ "text": out.summary(), "is_error": out.is_error });
            let _ = writeln!(stdin, "{reply}");
            let _ = stdin.flush();
        }
    });
    cancel.cancel();
    let ended = ended?;

    let made = files_since(files, started);
    let names: Vec<String> = made.iter().filter_map(|p| p.strip_prefix(files).ok()).map(|p| p.to_string_lossy().replace('\\', "/")).collect();
    let body = json!({
        "exit_code": ended.code,
        "timed_out": ended.timed_out,
        "output": clip_output(&lines),
        "files": names,
        "app_calls": calls,
        // Stamped by the harness, not the script, and never higher than this.
        "evidence": "Estimate: computed by a script in this chat, not simulated or measured",
        "source": "script",
    });
    let mut content = vec![Block::Text(serde_json::to_string_pretty(&body).unwrap_or_default())];
    for figure in made.iter().filter(|p| is_figure(p)).take(FIGURES_KEPT) {
        if let Ok((_, data, mime)) = images::load_file(figure) {
            content.push(Block::Image { data: Arc::clone(&data), mime });
        }
    }
    Ok(ToolOutput { content, is_error: ended.timed_out || ended.code != 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_installed_outside_its_own_folder() {
        let args = install_python_args();
        assert!(args.iter().any(|a| a == "--no-bin"), "it would put python.exe on the user's PATH");
        assert!(args.iter().any(|a| a == "--no-registry"), "it would register in the user's registry");
        let r = PathBuf::from(r"C:\x\python");
        for (name, path) in uv_env(&r) {
            assert!(path.starts_with(&r), "{name} leaves the folder");
        }
    }

    #[test]
    fn the_download_is_pinned_and_the_packages_are_locked() {
        assert_eq!(UV.sha256.len(), 64);
        assert!(UV.url.contains("/0.13.6/"), "a pinned release, never 'latest'");
        for name in ["numpy==", "scipy==", "pandas==", "matplotlib==", "openpyxl==", "pypdf=="] {
            assert!(PACKAGES.lines().any(|l| l.starts_with(name)), "{name}");
        }
        // Every package has at least one hash under it.
        let packages = PACKAGES.lines().filter(|l| !l.starts_with(' ') && l.contains("==")).count();
        assert!(packages >= 6);
        assert!(PACKAGES.matches("--hash=sha256:").count() >= packages);
    }

    #[test]
    fn a_call_needs_this_runs_prefix_so_printing_a_file_cant_make_one() {
        let good = format!("\u{1e}abc {}", json!({ "tool": "get_scene", "input": {} }));
        assert_eq!(parse_call(&good, "abc").unwrap().unwrap().0, "get_scene");
        assert!(parse_call(&format!("\u{1e}xyz {}", json!({ "tool": "clear_scene" })), "abc").is_none(), "another run's marker");
        assert!(parse_call(r#"{"tool":"clear_scene"}"#, "abc").is_none(), "plain output");
        assert!(parse_call("hello", "abc").is_none());
        assert!(parse_call("\u{1e}abc not json", "abc").unwrap().is_err());
        assert!(parse_call("\u{1e}abc {}", "abc").unwrap().is_err(), "no tool");
    }

    #[test]
    fn each_run_gets_its_own_marker() {
        assert_ne!(fresh_prefix(), fresh_prefix());
        assert!(fresh_prefix().len() >= 32);
    }

    #[test]
    fn a_script_only_gets_harmless_settings() {
        let env = script_env(Path::new(r"C:\w\files"), "abc");
        assert!(env.iter().any(|(k, v)| k == "SENSESIM_RPC_PREFIX" && v == "abc"));
        for (k, _) in &env {
            assert!(!k.contains("KEY") && !k.contains("TOKEN"), "{k}");
        }
    }

    #[test]
    fn long_output_keeps_both_ends() {
        let lines: Vec<String> = (0..20_000).map(|i| format!("line {i}")).collect();
        let t = clip_output(&lines);
        assert!(t.chars().count() < OUTPUT_KEPT_CHARS + 100);
        assert!(t.contains("line 0") && t.contains("line 19999"), "the error is usually at the end");
        assert!(t.contains("left out"));
        assert_eq!(clip_output(&["short".to_string()]), "short");
    }

    #[test]
    fn it_reports_what_the_script_made_and_not_the_apps_housekeeping() {
        let d = std::env::temp_dir().join(format!("sense-sim-python-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".scripts")).unwrap();
        std::fs::create_dir_all(d.join("plots")).unwrap();
        std::fs::write(d.join(".scripts").join("s.py"), "x").unwrap();
        std::fs::write(d.join("plots").join("a.png"), "x").unwrap();
        std::fs::write(d.join("r.csv"), "x").unwrap();
        let found = files_since(&d, SystemTime::UNIX_EPOCH);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|p| p.ends_with("a.png")) && !found.iter().any(|p| p.ends_with("s.py")));
        assert!(files_since(&d, SystemTime::now() + Duration::from_secs(3600)).is_empty());
        let _ = std::fs::remove_dir_all(&d);
        assert!(is_figure(Path::new("a.PNG")) && !is_figure(Path::new("a.csv")));
    }
}
