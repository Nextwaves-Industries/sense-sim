//! Runs jobs on threads of their own, a limited number at once, and remembers them.
//!
//! Each job has a folder with a `job.json` manifest, written atomically at every state change, so
//! that after a crash Sense Sim knows what was running: a job found `Running` at startup with no
//! process behind it becomes `Interrupted`, never silently lost and never shown as still going.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::process::Cancel;
use crate::results::ResultSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Queued,
    Running { stage: String, progress: f32 },
    Done,
    Failed { error: String },
    Cancelled,
    /// Was running when Sense Sim last ended.
    Interrupted,
}

impl State {
    pub fn is_active(&self) -> bool {
        matches!(self, State::Queued | State::Running { .. })
    }
}

/// What a job takes from the machine, so they don't fight over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// CPU solvers. A few at once.
    Cpu,
    /// One at a time: there is one GPU, and the viewport needs it too.
    Gpu,
    /// One at a time.
    Install,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: u64,
    pub name: String,
    pub study: String,
    pub state: State,
    pub started_unix: u64,
}

/// Writes through a temporary file and a rename, so a crash can't leave half a manifest.
fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join("job.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
    std::fs::rename(&tmp, dir.join("job.json"))?;
    Ok(())
}

/// What a running job uses to report.
#[derive(Clone)]
pub struct JobCtx {
    pub dir: PathBuf,
    pub cancel: Cancel,
    shared: Arc<Shared>,
    id: u64,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl JobCtx {
    pub fn set(&self, stage: &str, progress: f32) {
        self.shared.update(self.id, State::Running { stage: stage.to_string(), progress: progress.clamp(0.0, 1.0) });
    }

    pub fn log(&self, line: &str) {
        if let Ok(mut l) = self.log.lock() {
            if l.len() >= 400 {
                l.pop_front();
            }
            l.push_back(line.to_string());
        }
    }
}

struct Entry {
    manifest: Manifest,
    dir: PathBuf,
    kind: Kind,
    cancel: Cancel,
    log: Arc<Mutex<VecDeque<String>>>,
    result: Option<Arc<ResultSet>>,
}

struct Shared {
    entries: Mutex<Vec<Entry>>,
    wake: Condvar,
}

impl Shared {
    fn update(&self, id: u64, state: State) {
        let Ok(mut e) = self.entries.lock() else { return };
        if let Some(entry) = e.iter_mut().find(|x| x.manifest.id == id) {
            // Progress is frequent: the manifest only needs the change of stage.
            let write = !matches!((&entry.manifest.state, &state), (State::Running { stage: a, .. }, State::Running { stage: b, .. }) if a == b);
            entry.manifest.state = state;
            if write {
                let _ = write_manifest(&entry.dir, &entry.manifest);
            }
        }
        self.wake.notify_all();
    }

    /// Whether the job may start, given what else is running.
    fn may_start(entries: &[Entry], id: u64) -> bool {
        let Some(me) = entries.iter().find(|e| e.manifest.id == id) else { return false };
        // First come, first served within a kind.
        if entries.iter().any(|e| e.kind == me.kind && e.manifest.id < id && e.manifest.state == State::Queued) {
            return false;
        }
        let running = entries.iter().filter(|e| e.kind == me.kind && matches!(e.manifest.state, State::Running { .. })).count();
        running < match me.kind {
            Kind::Cpu => 2,
            Kind::Gpu | Kind::Install => 1,
        }
    }
}

/// A snapshot of a job, for the UI.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u64,
    pub name: String,
    pub study: String,
    pub state: State,
    pub dir: PathBuf,
    pub has_result: bool,
}

#[derive(Clone)]
pub struct JobManager {
    root: PathBuf,
    shared: Arc<Shared>,
}

impl JobManager {
    /// Loads what earlier runs left under `root`, marking anything left running as interrupted.
    pub fn new(root: PathBuf) -> Self {
        let mut entries = Vec::new();
        if let Ok(dirs) = std::fs::read_dir(&root) {
            for d in dirs.flatten() {
                let dir = d.path();
                let Ok(bytes) = std::fs::read(dir.join("job.json")) else { continue };
                let Ok(mut m) = serde_json::from_slice::<Manifest>(&bytes) else { continue };
                if m.state.is_active() {
                    m.state = State::Interrupted;
                    let _ = write_manifest(&dir, &m);
                }
                entries.push(Entry { manifest: m, dir, kind: Kind::Cpu, cancel: Cancel::default(), log: Default::default(), result: None });
            }
        }
        entries.sort_by_key(|e| e.manifest.id);
        Self { root, shared: Arc::new(Shared { entries: Mutex::new(entries), wake: Condvar::new() }) }
    }

    /// Starts `work` as soon as there's room. Its result is kept, and its error reported.
    pub fn submit(&self, name: &str, study: &str, kind: Kind, work: impl FnOnce(&JobCtx) -> Result<ResultSet> + Send + 'static) -> u64 {
        let mut entries = match self.shared.entries.lock() {
            Ok(e) => e,
            Err(p) => p.into_inner(),
        };
        let id = entries.iter().map(|e| e.manifest.id).max().unwrap_or(0) + 1;
        let dir = self.root.join(format!("job-{id:05}"));
        let manifest = Manifest { id, name: name.to_string(), study: study.to_string(), state: State::Queued, started_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) };
        let _ = write_manifest(&dir, &manifest);
        let cancel = Cancel::default();
        let log: Arc<Mutex<VecDeque<String>>> = Default::default();
        entries.push(Entry { manifest, dir: dir.clone(), kind, cancel: cancel.clone(), log: log.clone(), result: None });
        drop(entries);

        let shared = self.shared.clone();
        std::thread::spawn(move || {
            // Wait for room.
            {
                let mut e = match shared.entries.lock() {
                    Ok(e) => e,
                    Err(_) => return,
                };
                loop {
                    if cancel.is_cancelled() {
                        drop(e);
                        shared.update(id, State::Cancelled);
                        return;
                    }
                    if Shared::may_start(&e, id) {
                        break;
                    }
                    e = match shared.wake.wait(e) {
                        Ok(e) => e,
                        Err(_) => return,
                    };
                }
                if let Some(entry) = e.iter_mut().find(|x| x.manifest.id == id) {
                    entry.manifest.state = State::Running { stage: "Starting".into(), progress: 0.0 };
                    let _ = write_manifest(&entry.dir, &entry.manifest);
                }
            }
            let ctx = JobCtx { dir, cancel: cancel.clone(), shared: shared.clone(), id, log };
            // A panic in a backend is that job's failure, not the app's.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&ctx)));
            let state = match outcome {
                Ok(Ok(result)) => {
                    if let Ok(mut e) = shared.entries.lock()
                        && let Some(entry) = e.iter_mut().find(|x| x.manifest.id == id)
                    {
                        entry.result = Some(Arc::new(result));
                    }
                    State::Done
                }
                Ok(Err(_)) if cancel.is_cancelled() => State::Cancelled,
                Ok(Err(e)) => State::Failed { error: format!("{e:#}") },
                Err(_) => State::Failed { error: "the solver backend stopped with an internal error".into() },
            };
            shared.update(id, state);
        });
        id
    }

    pub fn cancel(&self, id: u64) {
        if let Ok(e) = self.shared.entries.lock()
            && let Some(entry) = e.iter().find(|x| x.manifest.id == id)
        {
            entry.cancel.cancel();
        }
        self.shared.wake.notify_all();
    }

    pub fn list(&self) -> Vec<JobInfo> {
        self.shared.entries.lock().map_or_else(|_| Vec::new(), |e| e.iter().map(|x| JobInfo { id: x.manifest.id, name: x.manifest.name.clone(), study: x.manifest.study.clone(), state: x.manifest.state.clone(), dir: x.dir.clone(), has_result: x.result.is_some() }).collect())
    }

    pub fn result(&self, id: u64) -> Option<Arc<ResultSet>> {
        self.shared.entries.lock().ok()?.iter().find(|x| x.manifest.id == id)?.result.clone()
    }

    pub fn log(&self, id: u64, last: usize) -> Vec<String> {
        let Ok(e) = self.shared.entries.lock() else { return Vec::new() };
        let Some(entry) = e.iter().find(|x| x.manifest.id == id) else { return Vec::new() };
        let Ok(l) = entry.log.lock() else { return Vec::new() };
        l.iter().rev().take(last).rev().cloned().collect()
    }

    /// Ends everything running. For the way out of the app.
    pub fn cancel_all(&self) {
        if let Ok(e) = self.shared.entries.lock() {
            for x in e.iter() {
                if x.manifest.state.is_active() {
                    x.cancel.cancel();
                }
            }
        }
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn root(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("sense-sim-jobs-test").join(name);
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn wait(m: &JobManager, id: u64) -> State {
        let t0 = Instant::now();
        loop {
            let s = m.list().into_iter().find(|j| j.id == id).unwrap().state;
            if !s.is_active() {
                return s;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "job {id} never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_job_runs_and_keeps_its_result_and_manifest() {
        let m = JobManager::new(root("ok"));
        let id = m.submit("a", "s", Kind::Cpu, |ctx| {
            ctx.set("working", 0.5);
            ctx.log("hello");
            Ok(ResultSet::default())
        });
        assert_eq!(wait(&m, id), State::Done);
        assert!(m.result(id).is_some());
        assert_eq!(m.log(id, 5), ["hello"]);
        let dir = &m.list()[0].dir;
        let saved: Manifest = serde_json::from_slice(&std::fs::read(dir.join("job.json")).unwrap()).unwrap();
        assert_eq!(saved.state, State::Done);
    }

    #[test]
    fn errors_and_panics_fail_the_job_not_the_app() {
        let m = JobManager::new(root("bad"));
        let a = m.submit("a", "s", Kind::Cpu, |_| anyhow::bail!("no such face"));
        assert!(matches!(wait(&m, a), State::Failed { error } if error.contains("no such face")));
        let b = m.submit("b", "s", Kind::Cpu, |_| panic!("boom"));
        assert!(matches!(wait(&m, b), State::Failed { .. }));
    }

    #[test]
    fn a_crash_leaves_jobs_marked_interrupted() {
        let dir = root("crash");
        let job = dir.join("job-00007");
        write_manifest(&job, &Manifest { id: 7, name: "long".into(), study: "s".into(), state: State::Running { stage: "Solving".into(), progress: 0.4 }, started_unix: 1 }).unwrap();
        let m = JobManager::new(dir);
        let list = m.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, State::Interrupted);
        // New jobs don't reuse its number.
        assert_eq!(m.submit("next", "s", Kind::Cpu, |_| Ok(ResultSet::default())), 8);
    }

    #[test]
    fn gpu_jobs_run_one_at_a_time_and_a_cancelled_one_never_starts() {
        let m = JobManager::new(root("gpu"));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        let first = m.submit("first", "s", Kind::Gpu, move |_| {
            let (l, c) = &*g;
            let mut open = l.lock().unwrap();
            while !*open {
                open = c.wait(open).unwrap();
            }
            Ok(ResultSet::default())
        });
        let second = m.submit("second", "s", Kind::Gpu, |_| Ok(ResultSet::default()));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(m.list().iter().find(|j| j.id == second).unwrap().state, State::Queued, "waits for the first");
        m.cancel(second);
        assert_eq!(wait(&m, second), State::Cancelled);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert_eq!(wait(&m, first), State::Done);
    }
}
