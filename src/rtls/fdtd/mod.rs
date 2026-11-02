//! Full-wave FDTD with openEMS (openems.de, the electromagnetic field solver,
//! not the OpenEMS energy-management system): antenna S11 / impedance /
//! radiation pattern, S21 through a piece of the scene, and the field shown in
//! the viewport. openEMS itself is downloaded on first use and run as a
//! separate process; geometry goes in as CSXCAD XML, results come back as
//! probe files and VTK field dumps.

pub mod antenna;
pub mod csx;
pub mod post;
pub mod region;
pub mod scene;
pub mod ui;

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, bail};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use antenna::Design;
use post::{Pattern, Sweep};

use crate::rtls::planning::Tech;

pub const OPENEMS_VERSION: &str = "v0.37.0";
pub const OPENEMS_URL: &str = "https://github.com/thliebig/openEMS-Project/releases/download/v0.37.0/openEMS_x64_v0.37.0_msvc.zip";

pub fn base_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("SenseSim").join("openems-fdtd")
}

pub fn exe() -> Option<PathBuf> {
    let p = base_dir().join("openEMS").join("openEMS.exe");
    p.exists().then_some(p)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    /// Message and progress 0..1 (negative: unknown).
    Busy(String, f32),
    Failed(String),
}

/// A field slice, magnitude normalised to its maximum: `abs` (|E| envelope)
/// and the animation frames (instantaneous |E| over one period).
#[derive(Clone, Debug, Default)]
pub struct Slice {
    pub nu: usize,
    pub nv: usize,
    /// Plane corners in simulation coordinates (u and v axes span it).
    pub u: (f64, f64),
    pub v: (f64, f64),
    /// The plane in the world: corner at (u0, v0) and its two edges.
    pub corner: Vec3,
    pub du: Vec3,
    pub dv: Vec3,
    pub abs: Vec<f32>,
    pub frames: Vec<Vec<f32>>,
}

/// How an openEMS run ended.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunEnd {
    /// It hit the step limit with the energy still above the end criterion.
    pub truncated: bool,
    /// The last energy the solver reported (dB, ≤ 0).
    pub energy_db: Option<f64>,
}

pub struct AntennaResult {
    pub design: Design,
    pub sweep: Sweep,
    pub pattern: Option<Pattern>,
    pub slice: Option<Slice>,
    /// Radiated over accepted power at the band centre, with the pattern.
    pub efficiency: Option<f64>,
    pub run: RunEnd,
    pub cells: u64,
    pub seconds: f64,
    pub dir: PathBuf,
}

/// Radiated power as far as a fit can be trusted: above this the transform and the port
/// disagree, and the number says more about them than about the antenna.
pub const MAX_EFFICIENCY: f64 = 1.05;

impl AntennaResult {
    /// Gain (dB) to add to directivity for the planner, or why it has none to give.
    pub fn efficiency_db(&self) -> Result<f64, String> {
        match self.efficiency {
            Some(e) if e > 0.0 && e <= MAX_EFFICIENCY => Ok(10.0 * e.min(1.0).log10()),
            Some(e) => Err(format!("its efficiency came out at {:.0} %, which isn't physical: check the run before using it", e * 100.0)),
            None => Err("this run has no efficiency to give".to_string()),
        }
    }

    /// What the planner adds to a link, by direction: directivity is what a lossless
    /// antenna would do, so its losses come off here.
    pub fn planner_gain(&self) -> Result<Vec<Vec<f32>>, String> {
        let p = self.pattern.as_ref().ok_or("this run has no pattern")?;
        let loss = self.efficiency_db()?;
        Ok(p.d.iter().map(|row| row.iter().map(|v| (*v + loss) as f32).collect()).collect())
    }
}

pub struct RegionResult {
    pub band: antenna::Band,
    pub target: String,
    pub layout: region::Layout,
    pub with: Sweep,
    pub free: Sweep,
    pub slice: Option<Slice>,
    /// Every field plane of the run (slice, vertical cut, levels), on one colour scale.
    pub planes: Vec<Slice>,
    /// Either run hit the step limit before its energy had decayed.
    pub truncated: bool,
    pub cells: u64,
    pub seconds: f64,
    pub dir: PathBuf,
}

/// The whole-scene run the current scene would need.
#[derive(Clone, Debug)]
pub struct ScenePreview {
    pub tech: Tech,
    pub band: antenna::Band,
    pub plan: scene::ScenePlan,
    pub txs: usize,
    /// Of those, how many an earlier run already covers (no re-simulation).
    pub reused: usize,
    /// Why this scene can't be run at all.
    pub problem: Option<String>,
}

#[derive(Default)]
struct Shared {
    status: Option<Status>,
    antenna: Option<Arc<AntennaResult>>,
    region: Option<Arc<RegionResult>>,
    /// Last result per object, so selecting it again shows its own.
    regions: std::collections::HashMap<String, Arc<RegionResult>>,
    /// Latest whole-scene run per technology, and the ray-model fit of each run.
    scenes: Vec<Arc<scene::SceneResult>>,
    fits: Vec<(PathBuf, Option<Arc<scene::Fit>>)>,
    log: VecDeque<String>,
}

/// FDTD jobs and their results. One job at a time.
#[derive(Resource)]
pub struct Fdtd {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Set by the UI; `collect_region` turns it into a job using the scene.
    pub region_requested: Option<(antenna::Band, antenna::Quality, Entity)>,
    /// Set by the UI (technology, mesh, tag height); `collect_scene` builds the job.
    pub scene_requested: Option<(Tech, antenna::Quality, f32)>,
    /// What a whole-scene run would cost now (refreshed while that tab is open).
    pub scene_preview: Option<ScenePreview>,
    pub ui: ui::UiState,
}

impl Default for Fdtd {
    fn default() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared { status: Some(Status::Idle), ..Default::default() })),
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            region_requested: None,
            scene_requested: None,
            scene_preview: None,
            ui: ui::UiState::default(),
        }
    }
}

impl Drop for Fdtd {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
        }
    }
}

impl Fdtd {
    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status.clone().unwrap_or(Status::Idle)
    }
    pub fn busy(&self) -> bool {
        matches!(self.status(), Status::Busy(..))
    }
    pub fn antenna(&self) -> Option<Arc<AntennaResult>> {
        self.shared.lock().unwrap().antenna.clone()
    }
    /// The result for the selected object if it has one, else the latest.
    pub fn region(&self) -> Option<Arc<RegionResult>> {
        let s = self.shared.lock().unwrap();
        self.ui.selected.as_ref().and_then(|(_, n)| s.regions.get(n).cloned()).or_else(|| s.region.clone())
    }
    /// The latest whole-scene run for `tech`.
    pub fn scene(&self, tech: Tech) -> Option<Arc<scene::SceneResult>> {
        self.shared.lock().unwrap().scenes.iter().find(|s| s.tech == tech).cloned()
    }
    /// The ray-model fit of a whole-scene run: `None` while not computed,
    /// `Some(None)` when there were too few samples.
    pub fn scene_fit(&self, dir: &Path) -> Option<Option<Arc<scene::Fit>>> {
        self.shared.lock().unwrap().fits.iter().find(|f| f.0 == dir).map(|f| f.1.clone())
    }
    pub fn log_tail(&self, n: usize) -> Vec<String> {
        let s = self.shared.lock().unwrap();
        s.log.iter().rev().take(n).rev().cloned().collect()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }

    fn spawn(&mut self, job: impl FnOnce(&Ctx) -> Result<()> + Send + 'static) {
        if self.busy() {
            return;
        }
        self.cancel.store(false, Ordering::SeqCst);
        let ctx = Ctx { shared: self.shared.clone(), cancel: self.cancel.clone(), child: self.child.clone() };
        ctx.set(Status::Busy("Starting…".into(), -1.0));
        std::thread::spawn(move || {
            let r = job(&ctx);
            let status = match r {
                Ok(()) => Status::Idle,
                Err(_) if ctx.cancel.load(Ordering::SeqCst) => Status::Failed("Cancelled".into()),
                Err(e) => Status::Failed(format!("{e:#}")),
            };
            ctx.set(status);
        });
    }

    /// Downloads and unpacks openEMS (~65 MB) into `base_dir()`.
    pub fn install(&mut self) {
        self.spawn(|ctx| install(ctx));
    }

    pub fn run_antenna(&mut self, design: Design) {
        self.spawn(move |ctx| {
            let exe = ensure_installed(ctx)?;
            let t0 = Instant::now();
            let sim = design.sim();
            let dir = job_dir("antenna")?;
            let end = run(ctx, &exe, &dir, &sim, "Antenna")?;
            ctx.set(Status::Busy("S-parameters…".into(), -1.0));
            let (fa, fb) = design.band.sweep();
            let sweep = Sweep::read(&dir, [design.port_ohm(), 50.0], false, post::linspace(fa, fb, 301))?;
            ctx.set(Status::Busy("Radiation pattern (near → far field)…".into(), -1.0));
            let f = design.band.center();
            let pattern = match post::nf2ff(&dir, "nf2ff", f, 5.0, [0.0; 3]) {
                Ok(p) => Some(p),
                Err(e) => {
                    ctx.log(format!("No pattern: {e:#}"));
                    None
                }
            };
            let slice = read_slice(&dir, f).inspect_err(|e| ctx.log(format!("No field slice: {e:#}"))).ok();
            let accepted = post::port_accepted_power(&dir, 1, design.port_ohm(), &[f])?[0];
            let efficiency = pattern.as_ref().map(|p| p.p_rad / accepted).filter(|e| e.is_finite());
            let res = AntennaResult { design, sweep, pattern, slice, efficiency, run: end, cells: sim.cells(), seconds: t0.elapsed().as_secs_f64(), dir };
            ctx.shared.lock().unwrap().antenna = Some(Arc::new(res));
            Ok(())
        });
    }

    pub fn run_region(&mut self, req: region::Request) {
        self.spawn(move |ctx| {
            let exe = ensure_installed(ctx)?;
            let t0 = Instant::now();
            ctx.set(Status::Busy("Voxelising the scene…".into(), -1.0));
            let (with, free, layout) = region::build(&req)?;
            ctx.log(format!("{} voxels of {} objects, cell {:.1} mm, {} cells", layout.voxels, layout.objects.len(), layout.cell * 1000.0, with.cells()));
            let dir = job_dir("s21")?;
            let free_dir = dir.join("free");
            std::fs::create_dir_all(&free_dir)?;
            let end_with = run(ctx, &exe, &dir, &with, "Through the object (1/2)")?;
            let end_free = run(ctx, &exe, &free_dir, &free, "Free space (2/2)")?;
            ctx.set(Status::Busy("S-parameters…".into(), -1.0));
            let f0 = req.band.center();
            let freqs = post::linspace(f0 * 0.85, f0 * 1.15, 201);
            let with_s = Sweep::read(&dir, [73.0, 73.0], true, freqs.clone())?;
            let free_s = Sweep::read(&free_dir, [73.0, 73.0], true, freqs)?;
            let planes = read_planes(&dir, &layout.planes, f0, &mut |m| ctx.log(m));
            let slice = planes.first().cloned();
            let res = RegionResult {
                band: req.band,
                target: req.target.clone(),
                layout,
                with: with_s,
                free: free_s,
                slice,
                planes,
                truncated: end_with.truncated || end_free.truncated,
                cells: with.cells(),
                seconds: t0.elapsed().as_secs_f64(),
                dir,
            };
            let res = Arc::new(res);
            let mut sh = ctx.shared.lock().unwrap();
            sh.regions.insert(res.target.clone(), res.clone());
            sh.region = Some(res);
            Ok(())
        });
    }

    /// The whole room, one openEMS run per transmitter. Transmitters whose run
    /// would be identical to the previous result's (same geometry, position
    /// and antenna) are reused, so moving one anchor re-simulates only it.
    pub fn run_scene(&mut self, req: scene::SceneRequest) {
        let prev = self.scene(req.tech);
        self.spawn(move |ctx| {
            let exe = ensure_installed(ctx)?;
            let t0 = Instant::now();
            ctx.set(Status::Busy("Voxelising the scene…".into(), -1.0));
            let plan = scene::plan(&req);
            let (sims, mut voxels) = scene::build(&req, &plan)?;
            let geometry = voxels.hash();
            // Only the occupancy is kept with the result.
            voxels.metal = Vec::new();
            voxels.dielectrics = Vec::new();
            let n = req.txs.len();
            let scaled = if plan.scaled() { format!(", frequency-scaled ×{:.2} to {:.3} GHz", plan.scale, plan.f_sim / 1e9) } else { String::new() };
            ctx.log(format!(
                "Whole scene: {n} transmitters, {} voxels, cell {:.1} mm, {:.1} M cells per run{scaled}",
                voxels.voxels,
                plan.cell * 1000.0,
                plan.cells as f64 / 1e6
            ));
            let dir = job_dir("scene")?;
            let mut per_tx = Vec::with_capacity(n);
            let mut simulated = 0;
            for (i, (tx, sim)) in req.txs.iter().zip(&sims).enumerate() {
                let key = scene::tx_key(geometry, &plan, tx);
                if let Some(old) = prev.as_ref().and_then(|p| p.per_tx.iter().find(|f| f.key == key)) {
                    ctx.log(format!("{}: unchanged, reusing its last run", tx.name));
                    per_tx.push(scene::TxField { tx: tx.clone(), ..old.clone() });
                    continue;
                }
                let d = dir.join(format!("tx{i}"));
                std::fs::create_dir_all(&d)?;
                let end = run(ctx, &exe, &d, sim, &format!("{} ({}/{n})", tx.name, i + 1))?;
                ctx.set(Status::Busy(format!("{}: field planes…", tx.name), -1.0));
                per_tx.push(scene::TxField { run: end, ..scene::read_tx(&d, &plan, req.band, tx.clone(), key)? });
                simulated += 1;
            }
            let res = scene::SceneResult {
                tech: req.tech,
                band: req.band,
                quality: req.quality,
                plan,
                tag_height: req.tag_height,
                per_tx,
                voxels,
                simulated,
                seconds: t0.elapsed().as_secs_f64(),
                dir,
            };
            let mut sh = ctx.shared.lock().unwrap();
            sh.scenes.retain(|s| s.tech != res.tech);
            sh.scenes.push(Arc::new(res));
            Ok(())
        });
    }
}

struct Ctx {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Ctx {
    fn set(&self, s: Status) {
        self.shared.lock().unwrap().status = Some(s);
    }
    fn log(&self, s: impl Into<String>) {
        let mut sh = self.shared.lock().unwrap();
        sh.log.push_back(s.into());
        while sh.log.len() > 200 {
            sh.log.pop_front();
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

fn job_dir(kind: &str) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let runs = base_dir().join("runs");
    // Keep the last few runs only.
    if let Ok(rd) = std::fs::read_dir(&runs) {
        let mut old: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        old.sort();
        let n = old.len();
        for p in old.into_iter().take(n.saturating_sub(5)) {
            let _ = std::fs::remove_dir_all(p);
        }
    }
    let d = runs.join(format!("{stamp}_{kind}"));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn ensure_installed(ctx: &Ctx) -> Result<PathBuf> {
    if let Some(e) = exe() {
        return Ok(e);
    }
    install(ctx)?;
    exe().ok_or_else(|| anyhow::anyhow!("openEMS.exe missing after install"))
}

fn install(ctx: &Ctx) -> Result<()> {
    let dir = base_dir();
    std::fs::create_dir_all(&dir)?;
    let zip_path = dir.join(format!("openEMS_{OPENEMS_VERSION}.zip"));
    if !zip_path.exists() {
        ctx.set(Status::Busy(format!("Downloading openEMS {OPENEMS_VERSION}…"), 0.0));
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(900))).build().into();
        let resp = agent.get(OPENEMS_URL).call().map_err(|e| anyhow::anyhow!("downloading openEMS: {e}"))?;
        let total: Option<u64> = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        let tmp = zip_path.with_extension("part");
        let mut out = std::fs::File::create(&tmp)?;
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        loop {
            if ctx.cancelled() {
                bail!("cancelled");
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            done += n as u64;
            let p = total.map_or(-1.0, |t| done as f32 / t as f32);
            ctx.set(Status::Busy(format!("Downloading openEMS {OPENEMS_VERSION}: {:.0} MB", done as f64 / 1e6), p));
        }
        drop(out);
        std::fs::rename(&tmp, &zip_path)?;
    }
    ctx.set(Status::Busy("Unpacking openEMS…".into(), -1.0));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&zip_path)?)?;
    let n = zip.len();
    for i in 0..n {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let out = dir.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut f, &mut w)?;
        if i % 50 == 0 {
            ctx.set(Status::Busy("Unpacking openEMS…".into(), i as f32 / n as f32));
        }
    }
    if exe().is_none() {
        bail!("openEMS.exe not found in the download");
    }
    let _ = std::fs::remove_file(&zip_path);
    ctx.log(format!("openEMS {OPENEMS_VERSION} installed in {}", dir.display()));
    Ok(())
}

/// openEMS says this on stderr when it runs out of steps before the end criterion.
fn says_truncated(stderr: &str) -> bool {
    stderr.contains("Max. number of timesteps was reached")
}

/// The dB figure at the end of a progress line: "... Energy: ~3.7e-20 (-12.34dB)".
fn energy_db_of(line: &str) -> Option<f64> {
    line.rsplit('(').next()?.trim_end_matches(')').trim_end_matches("dB").replace(' ', "").parse().ok()
}

/// Writes `sim.xml` into `dir` and runs openEMS there, reporting progress from
/// its energy decay towards the end criterion.
fn run(ctx: &Ctx, exe: &Path, dir: &Path, sim: &csx::Sim, label: &str) -> Result<RunEnd> {
    std::fs::write(dir.join("sim.xml"), sim.to_xml())?;
    let target_db = -10.0 * sim.end_criteria.log10();
    ctx.log(format!("{label}: {} cells, est. {:.0} s", sim.cells(), sim.estimate_s()));
    let mut cmd = Command::new(exe);
    cmd.arg("sim.xml").current_dir(dir).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("starting {}: {e}", exe.display()))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let stderr = child.stderr.take();
    *ctx.child.lock().unwrap() = Some(child);
    // Read to the end on the side, so a full pipe can't stall the solver. Joined below.
    let stderr = stderr.map(|mut e| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            s
        })
    });
    let mut last_err = String::new();
    let mut energy_db = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let l = line.trim();
        if l.contains("Timestep:") {
            // "[@ 12s] Timestep: 117 || Speed: 44.2 MC/s (…) || Energy: ~3.7e-20 (-12.34dB)"
            let db = energy_db_of(l);
            energy_db = db.or(energy_db);
            let db = db.unwrap_or(0.0);
            let step = l.split("Timestep:").nth(1).and_then(|s| s.split("||").next()).map(|s| s.trim().to_string()).unwrap_or_default();
            let speed = l.split("Speed:").nth(1).and_then(|s| s.split('(').next()).map(|s| s.trim().to_string()).unwrap_or_default();
            // The run ends on the energy or the step limit, whichever comes first.
            let steps_done = step.parse::<f64>().map_or(0.0, |s| s / sim.max_timesteps.max(1) as f64);
            let p = (-db / target_db).max(steps_done).clamp(0.0, 1.0) as f32;
            ctx.set(Status::Busy(format!("{label}: timestep {step} · {speed} · energy {db:.0} of -{target_db:.0} dB"), p));
        } else if l.to_ascii_lowercase().contains("error") {
            last_err = l.to_string();
            ctx.log(l);
        } else if l.starts_with("RunFDTD") || l.starts_with("Time for") {
            ctx.log(format!("{label}: {l}"));
        }
    }
    let status = ctx.child.lock().unwrap().take().map(|mut c| c.wait());
    let errors = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
    let errors = errors.trim();
    if !errors.is_empty() {
        ctx.log(format!("{label}: {errors}"));
    }
    if ctx.cancelled() {
        bail!("cancelled");
    }
    let truncated = says_truncated(errors);
    if truncated {
        ctx.log(format!("{label}: stopped at the step limit of {} before the energy reached -{target_db:.0} dB", sim.max_timesteps));
    }
    match status {
        Some(Ok(s)) if s.success() => Ok(RunEnd { truncated, energy_db }),
        Some(Ok(s)) => bail!("openEMS exited with {s}{}", if last_err.is_empty() { String::new() } else { format!(": {last_err}") }),
        Some(Err(e)) => Err(e.into()),
        None => bail!("openEMS was stopped"),
    }
}

/// One plane dump: |E| envelope and the phase frames, unnormalised, with
/// where the plane is in the world.
fn read_plane(dir: &Path, name: &str, f: f64) -> Result<Slice> {
    let abs = post::read_vtr(&post::fd_file(dir, name, f, "abs"))?;
    // The two axes the plane spans, and the one it is fixed on.
    let axes: Vec<usize> = [abs.x.len(), abs.y.len(), abs.z.len()].iter().enumerate().filter(|(_, n)| **n > 1).map(|(k, _)| k).collect();
    if axes.len() != 2 {
        bail!("{name} is not a plane");
    }
    let fixed = 3 - axes[0] - axes[1];
    let coord = |g: &post::Grid, k: usize| match k {
        0 => g.x.clone(),
        1 => g.y.clone(),
        _ => g.z.clone(),
    };
    let (cu, cv, cf) = (coord(&abs, axes[0]), coord(&abs, axes[1]), coord(&abs, fixed));
    let mag = |g: &post::Grid| -> Vec<f32> { (0..g.len()).map(|p| g.at(p).iter().map(|v| v * v).sum::<f32>().sqrt()).collect() };
    let env = mag(&abs);
    let frames = post::phase_frames(dir, name, f).iter().filter_map(|p| post::read_vtr(p).ok()).filter(|g| g.len() == abs.len()).map(|g| mag(&g)).collect();
    // Sim → world: a permutation, so it maps edge vectors too.
    let mut c = [0.0; 3];
    c[axes[0]] = cu[0];
    c[axes[1]] = cv[0];
    c[fixed] = cf[0];
    let mut eu = [0.0; 3];
    eu[axes[0]] = cu[cu.len() - 1] - cu[0];
    let mut ev = [0.0; 3];
    ev[axes[1]] = cv[cv.len() - 1] - cv[0];
    Ok(Slice {
        nu: cu.len(),
        nv: cv.len(),
        u: (cu[0], cu[cu.len() - 1]),
        v: (cv[0], cv[cv.len() - 1]),
        corner: region::to_world(c),
        du: region::to_world(eu),
        dv: region::to_world(ev),
        abs: env,
        frames,
    })
}

/// The "slice" dump normalised to its own maximum (antenna runs).
fn read_slice(dir: &Path, f: f64) -> Result<Slice> {
    let mut s = read_plane(dir, "slice", f)?;
    let peak = s.abs.iter().copied().fold(0.0f32, f32::max).max(1e-30);
    s.abs.iter_mut().for_each(|v| *v /= peak);
    s.frames.iter_mut().flatten().for_each(|v| *v /= peak);
    Ok(s)
}

/// All planes of a region run on one colour scale. 0 dB is the 99th
/// percentile of the field, not its peak: the peak sits on the transmitter
/// and would push everything else below the bottom of the scale.
fn read_planes(dir: &Path, names: &[String], f: f64, log: &mut dyn FnMut(String)) -> Vec<Slice> {
    let mut planes: Vec<Slice> = names.iter().filter_map(|n| read_plane(dir, n, f).inspect_err(|e| log(format!("No field plane {n}: {e:#}"))).ok()).collect();
    let mut all: Vec<f32> = planes.iter().flat_map(|p| p.abs.iter().copied()).filter(|v| v.is_finite()).collect();
    if all.is_empty() {
        return planes;
    }
    let k = ((all.len() - 1) as f32 * 0.99) as usize;
    let reference = *all.select_nth_unstable_by(k, f32::total_cmp).1;
    let reference = reference.max(1e-30);
    for p in &mut planes {
        p.abs.iter_mut().for_each(|v| *v /= reference);
        p.frames.iter_mut().flatten().for_each(|v| *v /= reference);
    }
    planes
}

// ---------------------------------------------------------------------------
// Colour map and the field overlay in the scene

/// Field magnitude (0..1, linear) → colour on a dB scale (-40..0 dB).
pub fn colormap(v: f32) -> [u8; 4] {
    let db = 20.0 * v.max(1e-6).log10();
    let t = ((db + 40.0) / 40.0).clamp(0.0, 1.0);
    // Dark blue → cyan → yellow → red.
    let stops = [(0.0, [20.0, 24.0, 82.0]), (0.35, [30.0, 150.0, 220.0]), (0.65, [240.0, 220.0, 60.0]), (1.0, [230.0, 40.0, 30.0])];
    let mut c = stops[stops.len() - 1].1;
    for w in stops.windows(2) {
        if t <= w[1].0 {
            let k = (t - w[0].0) / (w[1].0 - w[0].0);
            c = std::array::from_fn(|i| w[0].1[i] + (w[1].1[i] - w[0].1[i]) * k);
            break;
        }
    }
    // Weak field nearly clear, so stacked planes stay see-through.
    let alpha = (12.0 + 230.0 * t * t.sqrt()) as u8;
    [c[0] as u8, c[1] as u8, c[2] as u8, alpha]
}

pub fn slice_rgba(s: &Slice, data: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.nu * s.nv * 4);
    // Image rows top to bottom = v descending.
    for j in (0..s.nv).rev() {
        for i in 0..s.nu {
            out.extend(colormap(data.get(i + s.nu * j).copied().unwrap_or(0.0)));
        }
    }
    out
}

/// One field plane in the scene (index into the planes shown).
#[derive(Component)]
pub struct FieldOverlay(usize);

/// A field plane as a translucent textured quad.
fn spawn_plane(commands: &mut Commands, images: &mut Assets<Image>, meshes: &mut Assets<Mesh>, mats: &mut Assets<StandardMaterial>, p: &Slice, data: Vec<u8>, i: usize) {
    let image = Image::new(
        Extent3d { width: p.nu as u32, height: p.nv as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let handle = images.add(image);
    let (c, du, dv) = (p.corner, p.du, p.dv);
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![c.to_array(), (c + du).to_array(), (c + du + dv).to_array(), (c + dv).to_array()]);
    let n = du.cross(dv).normalize_or(Vec3::Y);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![n.to_array(); 4]);
    // Image row 0 is v1 (top).
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
    mesh.insert_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]));
    let mat = mats.add(StandardMaterial {
        base_color_texture: Some(handle),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(mat),
        Transform::IDENTITY,
        FieldOverlay(i),
        bevy::light::NotShadowCaster,
        bevy::picking::Pickable::IGNORE,
        Name::new("FDTD field"),
    ));
}

/// Shows an FDTD field in the scene as translucent textured quads. On the
/// whole-scene tab: received power of the latest run (levels over the room's
/// height and vertical cuts through the transmitters, on the floor map's
/// signal scale). Otherwise the S21 field: every dumped plane (horizontal at
/// the antenna, a vertical cut through the object, levels over its height),
/// animated over one RF period, plus markers for the two antennas and the
/// simulated region.
#[allow(clippy::too_many_arguments)]
pub fn sync_overlay(
    mut commands: Commands,
    fdtd: Res<Fdtd>,
    time: Res<Time>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    overlay: Query<(Entity, &FieldOverlay, &MeshMaterial3d<StandardMaterial>)>,
    mut gizmos: Gizmos,
    mut shown: Local<Option<String>>,
    mut frame: Local<f32>,
) {
    let clear = |commands: &mut Commands, shown: &mut Option<String>| {
        for (e, _, _) in &overlay {
            commands.entity(e).despawn();
        }
        *shown = None;
    };
    if fdtd.ui.mode == ui::Mode::Scene {
        let Some(res) = fdtd.scene(fdtd.ui.scene_tech).filter(|_| fdtd.ui.show_field) else {
            clear(&mut commands, &mut shown);
            return;
        };
        let pick = fdtd.ui.scene_tx.filter(|i| *i < res.per_tx.len());
        for (i, t) in res.per_tx.iter().enumerate() {
            let c = if pick.is_none_or(|k| k == i) { Color::srgb(1.0, 0.3, 0.2) } else { Color::srgba(1.0, 1.0, 1.0, 0.4) };
            gizmos.sphere(Isometry3d::from_translation(t.tx.pos), 0.05, c);
            gizmos.line(t.tx.pos - Vec3::Y * 0.12, t.tx.pos + Vec3::Y * 0.12, c);
        }
        let (lmin, lmax) = res.region();
        gizmos.cube(Transform::from_translation((lmin + lmax) * 0.5).with_scale(lmax - lmin), Color::srgba(1.0, 1.0, 1.0, 0.25));
        let key = format!("scene|{}|{pick:?}|{}", res.dir.display(), fdtd.ui.scene_all_levels);
        if shown.as_deref() != Some(key.as_str()) {
            clear(&mut commands, &mut shown);
            *shown = Some(key);
            // Top of the floor map's signal scale = full colour.
            let (_, hi) = crate::rtls::planning::signal_scale(res.tech);
            for (i, p) in res.planes(pick, fdtd.ui.scene_all_levels, |dbm| 10f32.powf((dbm - hi) / 20.0)).iter().enumerate() {
                spawn_plane(&mut commands, &mut images, &mut meshes, &mut mats, p, slice_rgba(p, &p.abs), i);
            }
        }
        return;
    }
    let Some(res) = fdtd.region().filter(|_| fdtd.ui.show_field) else {
        clear(&mut commands, &mut shown);
        return;
    };
    let l = &res.layout;
    for (p, c) in [(l.tx, Color::srgb(1.0, 0.3, 0.2)), (l.rx, Color::srgb(0.3, 1.0, 0.4))] {
        gizmos.sphere(Isometry3d::from_translation(p), 0.03, c);
        gizmos.line(p - Vec3::Y * 0.08, p + Vec3::Y * 0.08, c);
    }
    let lmin = l.region_min;
    let lmax = l.region_max;
    gizmos.cube(Transform::from_translation((lmin + lmax) * 0.5).with_scale(lmax - lmin), Color::srgba(1.0, 1.0, 1.0, 0.25));
    // Older results (one slice only) still show that slice.
    let planes: &[Slice] = if res.planes.is_empty() { res.slice.as_slice() } else { &res.planes };
    if planes.is_empty() {
        return;
    }
    let animate = fdtd.ui.animate && planes.iter().all(|p| !p.frames.is_empty());
    if animate {
        let n = planes.iter().map(|p| p.frames.len()).min().unwrap_or(1).max(1);
        *frame = (*frame + time.delta_secs() * 12.0) % n as f32;
    }
    let data = |p: &'_ Slice| -> Vec<u8> { slice_rgba(p, if animate { &p.frames[*frame as usize] } else { &p.abs }) };
    let key = format!("region|{}", res.dir.display());
    if shown.as_deref() != Some(key.as_str()) {
        clear(&mut commands, &mut shown);
        *shown = Some(key);
        for (i, p) in planes.iter().enumerate() {
            spawn_plane(&mut commands, &mut images, &mut meshes, &mut mats, p, data(p), i);
        }
        return;
    }
    // Same run: update the textures in place (animation).
    if animate {
        for (_, o, m) in &overlay {
            if let Some(p) = planes.get(o.0)
                && let Some(mat) = mats.get(&m.0)
                && let Some(tex) = mat.base_color_texture.clone()
                && let Some(mut img) = images.get_mut(&tex)
            {
                img.data = Some(data(p));
            }
        }
    }
}

/// World bounds of a placed object.
fn world_bounds(o: &crate::scene::PlacedObject, t: &Transform) -> (Vec3, Vec3) {
    let (a, b) = o.aabb();
    (0..8).fold((Vec3::MAX, Vec3::MIN), |(lo, hi), k| {
        let c = Vec3::new(if k & 1 == 0 { a.x } else { b.x }, if k & 2 == 0 { a.y } else { b.y }, if k & 4 == 0 { a.z } else { b.z });
        let w = t.transform_point(c);
        (lo.min(w), hi.max(w))
    })
}

/// What radio sees an object as: what someone stated first (a BIM file, `site_set_material`),
/// then the guess from its asset name. The same lookup as the ray model's.
fn radio_of(name: &str, asset: &Path) -> (crate::rtls::radio::Material, f32) {
    crate::rtls::radio::object_material(name).unwrap_or_else(|| crate::rtls::radio::obstruction_of_asset(asset))
}

type ObjectQuery<'w, 's> = Query<'w, 's, (Entity, &'static crate::scene::PlacedObject, &'static Transform)>;
type PartQuery<'w, 's> = Query<'w, 's, (&'static crate::scene::PartOf, &'static crate::scene::PartBvh, &'static GlobalTransform)>;

/// Every placed object as the voxeliser sees it (mesh parts only with `with_parts`).
fn scene_objects(objects: &ObjectQuery, parts: &PartQuery, with_parts: bool) -> Vec<region::SceneObj> {
    let mut objs: Vec<region::SceneObj> = objects
        .iter()
        .map(|(_, o, t)| {
            let (min, max) = world_bounds(o, t);
            let (material, weight) = radio_of(&o.name, &o.asset);
            // Same id and bays as the ray model, so the same bays are empty in both.
            let fill = crate::rtls::radio::SiteConditions::current().rack_fill;
            let is_rack = material == crate::rtls::radio::Material::Metal && crate::rtls::radio::is_rack(&o.asset);
            let edge = min - Vec3::splat(0.02);
            let rack = is_rack.then_some((edge.x.to_bits() as u64 ^ ((edge.z.to_bits() as u64) << 32), fill));
            region::SceneObj { name: o.name.clone(), material, weight, parts: Vec::new(), min, max, rack }
        })
        .collect();
    if with_parts {
        let ents: Vec<Entity> = objects.iter().map(|(e, _, _)| e).collect();
        for (of, bvh, gt) in parts {
            if let Some(i) = ents.iter().position(|e| *e == of.0) {
                objs[i].parts.push((bvh.0.clone(), gt.affine()));
            }
        }
    }
    objs
}

/// dB the floor map charges a typical wall on this band's technology.
fn wall_db_of(band: antenna::Band, profiles: &crate::rtls::device::Profiles) -> f32 {
    match ui::band_tech(band) {
        // `radio::uwb_rx_power`.
        Tech::Uwb => 6.0,
        t => profiles.default_for(t.infra()).map_or(6.0, |p| p.wall_loss_db),
    }
}

/// Builds the S21 job from the scene once the UI asked for it.
pub fn collect_region(mut fdtd: ResMut<Fdtd>, objects: ObjectQuery, parts: PartQuery, profiles: Res<crate::rtls::device::Profiles>) {
    let Some((band, quality, target)) = fdtd.region_requested.take() else { return };
    let Ok((_, tobj, tt)) = objects.get(target) else {
        fdtd.shared.lock().unwrap().status = Some(Status::Failed("Select a scene object (wall, rack, door…) first".into()));
        return;
    };
    let (tmin, tmax) = world_bounds(tobj, tt);
    let req = region::Request { band, quality, target: tobj.name.clone(), target_min: tmin, target_max: tmax, objects: scene_objects(&objects, &parts, true), wall_db: wall_db_of(band, &profiles) };
    fdtd.run_region(req);
}

/// The band a device transmits on, from its profile. A run at another frequency would
/// get every number in the map wrong, so one outside every band isn't run.
fn band_of(tech: Tech, prof: Option<&crate::rtls::device::Profile>) -> Result<antenna::Band, String> {
    let Some(p) = prof else {
        return Ok(match tech {
            Tech::Uwb => antenna::Band::Uwb5,
            Tech::Ble | Tech::Wifi => antenna::Band::Ble,
            Tech::Rfid => antenna::Band::Rfid,
        });
    };
    antenna::Band::at_mhz(p.freq_mhz).ok_or_else(|| {
        let known: Vec<&str> = antenna::Band::ALL.iter().map(|b| b.label()).collect();
        format!("{} at {:.3} GHz: no simulated band covers it (only {})", tech.label(), p.freq_mhz / 1000.0, known.join(", "))
    })
}

/// Builds the whole-scene job once the UI asked for it, and while the
/// whole-scene tab is open keeps an estimate of what a run would cost.
#[allow(clippy::too_many_arguments)]
pub fn collect_scene(
    mut fdtd: ResMut<Fdtd>,
    objects: ObjectQuery,
    parts: PartQuery,
    placed: Query<(&crate::scene::PlacedObject, &GlobalTransform)>,
    devices: Query<(Entity, &crate::rtls::device::Device, &Transform)>,
    profiles: Res<crate::rtls::device::Profiles>,
    (library, applied): (Res<crate::rtls::antenna::AntennaLibrary>, Res<crate::rtls::planning::AppliedAntenna>),
    time: Res<Time>,
    mut next_preview: Local<f64>,
) {
    use crate::rtls::antenna::AntennaChoice;
    let requested = fdtd.scene_requested.take();
    let preview = fdtd.ui.mode == ui::Mode::Scene && time.elapsed_secs_f64() >= *next_preview;
    if requested.is_none() && !preview {
        return;
    }
    let (tech, quality, tag_height) = requested.unwrap_or((fdtd.ui.scene_tech, fdtd.ui.scene_quality, fdtd.ui.scene_height));
    let ants = crate::rtls::antenna::Antennas { library: &library, applied: &applied };
    let tag = crate::rtls::planning::tag_profile(&profiles, tech);
    let tag_gain = ants.tag_gain_unaimed(None, &tag);
    let mut txs = Vec::new();
    let mut bands: Vec<Result<antenna::Band, String>> = Vec::new();
    for (e, d, t) in &devices {
        if !d.enabled || d.kind != tech.infra() {
            continue;
        }
        let prof = profiles.for_device(d);
        let fwd = *t.forward();
        let choice = d.radio.antenna.as_ref();
        let directional = match choice {
            Some(AntennaChoice::Directional { .. }) => true,
            Some(AntennaChoice::Omni { .. }) => false,
            Some(AntennaChoice::Library { name }) => library.get(name).is_some_and(|a| a.axis_forward),
            None => match applied.0.as_ref().filter(|a| a.tech == tech) {
                Some(a) => a.axis_forward,
                None => prof.beam_deg < 300.0,
            },
        };
        // The simulated source stands in for the device's antenna: correct by
        // the difference of their peak gains.
        let peak = ants.gain(choice, tech, true, &prof, fwd, fwd);
        let gain_fix_db = peak - if directional { scene::REFLECTOR_DBI } else { scene::DIPOLE_DBI };
        let rx_offset_db = crate::rtls::planning::fdtd_rx_dbm(tech, &prof, &tag, tag_gain, gain_fix_db);
        let (wall_db, exponent) = match tech {
            Tech::Uwb => (6.0, None),
            Tech::Ble | Tech::Wifi => (prof.wall_loss_db, Some(prof.path_loss_exp)),
            Tech::Rfid => (prof.wall_loss_db, None),
        };
        // The pattern only when it's about to be used: this runs every second for the estimate.
        let gain_fix = requested.is_some().then(|| Arc::new(scene::GainFix::sample(|dir| ants.gain(choice, tech, true, &prof, fwd, dir), directional, fwd)));
        txs.push(scene::Tx { entity: e, name: d.id.clone(), pos: t.translation, fwd, directional, gain_fix_db, gain_fix, rx_offset_db, wall_db, exponent });
        bands.push(band_of(tech, Some(&prof)));
    }
    if bands.is_empty() {
        bands.push(band_of(tech, None));
    }
    let band = bands.iter().find_map(|b| b.as_ref().ok().copied()).unwrap_or(antenna::Band::Ble);
    // One run is one band: every transmitter must be on it.
    let band_problem = bands.iter().find_map(|b| b.as_ref().err().cloned()).or_else(|| {
        bands.iter().flatten().any(|b| *b != band).then(|| format!("these {} use more than one band: one run simulates one, so give them the same band", tech.label()))
    });
    let (lo, hi) = crate::rtls::planning::area(&placed, &devices);
    let req = scene::SceneRequest {
        tech,
        band,
        quality,
        tag_height,
        lo,
        hi,
        objects: scene_objects(&objects, &parts, requested.is_some()),
        txs,
    };
    if requested.is_some() {
        match band_problem {
            Some(why) => fdtd.shared.lock().unwrap().status = Some(Status::Failed(why)),
            None => fdtd.run_scene(req),
        }
        return;
    }
    *next_preview = time.elapsed_secs_f64() + 1.0;
    let plan = scene::plan(&req);
    let problem = band_problem.or_else(|| scene::tx_problem(&req, &plan));
    // Unchanged transmitters of the last run are reused; a moved one is not.
    let reused = fdtd.scene(tech).map_or(0, |s| {
        req.txs.iter().filter(|t| s.plan.f_sim == plan.f_sim && s.plan.lo == plan.lo && s.plan.hi == plan.hi && s.per_tx.iter().any(|f| (f.tx.pos - t.pos).length() < 0.01 && f.tx.directional == t.directional)).count()
    });
    fdtd.scene_preview = Some(ScenePreview { tech, band: req.band, plan, txs: req.txs.len(), reused, problem });
}

/// Fits the ray model's material losses to each new whole-scene result:
/// floor points at tag height, every 0.5 m, from every omnidirectional
/// transmitter, FDTD loss next to the ray model's free-space loss and the
/// materials its straight path crosses.
pub fn fit_scene(fdtd: Res<Fdtd>, parts: crate::rtls::sim::OccluderParts, objects: ObjectQuery) {
    let pending: Vec<Arc<scene::SceneResult>> = {
        let sh = fdtd.shared.lock().unwrap();
        sh.scenes.iter().filter(|s| !sh.fits.iter().any(|f| f.0 == s.dir)).cloned().collect()
    };
    if pending.is_empty() {
        return;
    }
    let occ = crate::rtls::sim::Occluders::build(&parts, &objects);
    for res in pending {
        let f_mhz = res.plan.f_real / 1e6;
        let near = (csx::C0 / res.plan.f_real).max(1.0) as f32;
        let k = res.plan.levels.iter().enumerate().min_by(|a, b| (a.1 - res.tag_height).abs().total_cmp(&(b.1 - res.tag_height).abs())).map(|x| x.0);
        let mut samples = Vec::new();
        for t in res.per_tx.iter().filter(|t| !t.tx.directional) {
            let Some(lvl) = k.and_then(|k| t.levels.get(k)) else { continue };
            let stride = (0.5 / lvl.step).round().max(1.0) as usize;
            for j in (0..lvl.nv).step_by(stride) {
                for i in (0..lvl.nu).step_by(stride) {
                    let db = lvl.db[i + lvl.nu * j];
                    if !db.is_finite() {
                        continue;
                    }
                    let s = [lvl.u0 + i as f64 * lvl.step, lvl.v0 + j as f64 * lvl.step, lvl.fixed];
                    let p = region::to_world(s);
                    let d = (p - t.tx.pos).length();
                    if d < near || res.voxels.at(s).is_some() {
                        continue;
                    }
                    let path = occ.path(t.tx.pos, p, None);
                    let base = match t.tx.exponent {
                        Some(n) => crate::rtls::solver::fspl_db(1.0, f_mhz) as f32 + 10.0 * n * d.log10(),
                        None => crate::rtls::solver::fspl_db(d as f64, f_mhz) as f32,
                    };
                    let loss = scene::source_gain_db(p - t.tx.pos) - db;
                    samples.push(scene::Sample { loss, base, wall_db: t.tx.wall_db, w: path.per_material });
                }
            }
        }
        let band = crate::rtls::radio::CalBand::of_mhz(f_mhz as f32);
        let current = crate::rtls::radio::Material::ALL.map(|m| crate::rtls::radio::MaterialCal::scales_in(m, band).0);
        let fit = scene::fit_calibration(&samples, current).map(Arc::new);
        fdtd.shared.lock().unwrap().fits.push((res.dir.clone(), fit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stated_materials_reach_the_solver() {
        let asset = std::path::Path::new("C:/SenseSim/structures/wall/4.00x3.00x0.20/wall.usda");
        crate::rtls::radio::set_object_material("fdtd_test_partition", Some((crate::rtls::radio::Material::Thin, 0.15)));
        let got = radio_of("fdtd_test_partition", asset);
        crate::rtls::radio::set_object_material("fdtd_test_partition", None);
        assert_eq!(got, (crate::rtls::radio::Material::Thin, 0.15));
        assert_eq!(radio_of("fdtd_test_other", asset).0, crate::rtls::radio::Material::Concrete);
    }

    #[test]
    fn wifi_at_5_ghz_is_not_simulated_at_2_ghz() {
        let at = |t: Tech, f: f32| band_of(t, Some(&crate::rtls::device::Profile { freq_mhz: f, ..Default::default() }));
        let b = at(Tech::Wifi, 5180.0).unwrap();
        assert_eq!(b, antenna::Band::Wifi5);
        assert!((b.center() / 5.18e9 - 1.0).abs() < 0.015);
        assert_eq!(at(Tech::Wifi, 2437.0).unwrap(), antenna::Band::Ble);
        assert_eq!(at(Tech::Uwb, 6489.6).unwrap(), antenna::Band::Uwb5);
        assert_eq!(at(Tech::Uwb, 7987.2).unwrap(), antenna::Band::Uwb9);
        assert_eq!(at(Tech::Rfid, 920.5).unwrap(), antenna::Band::Rfid);
        // Nothing here simulates these, so they're refused rather than moved.
        assert!(at(Tech::Uwb, 3993.6).unwrap_err().contains("no simulated band"));
        assert!(at(Tech::Wifi, 5825.0).is_err());
        assert_eq!(band_of(Tech::Wifi, None).unwrap(), antenna::Band::Ble);
    }

    #[test]
    fn every_infrastructure_profile_shipped_has_a_band() {
        let list: Vec<crate::rtls::device::Profile> = serde_json::from_str(include_str!("../../../assets/devices/generic.json")).unwrap();
        for t in Tech::ALL {
            for p in list.iter().filter(|p| p.kind == t.infra()) {
                assert!(band_of(t, Some(p)).is_ok(), "{t:?} at {}", p.freq_mhz);
            }
        }
    }

    #[test]
    fn a_step_limit_is_read_from_stderr() {
        // As the openEMS v0.37 binary prints it.
        assert!(says_truncated("RunFDTD: Warning: Max. number of timesteps was reached before the end-criteria of -50dB was reached... \nYou may want to choose a higher number of max. timesteps... "));
        assert!(!says_truncated("RunFDTD: end-criteria of -50dB reached"));
        assert!(!says_truncated(""));
    }

    #[test]
    fn the_energy_is_read_from_a_progress_line() {
        assert_eq!(energy_db_of("[@ 12s] Timestep: 117 || Speed: 44.2 MC/s (2.1e-3 s/TS) || Energy: ~3.7e-20 (-12.34dB)"), Some(-12.34));
        assert_eq!(energy_db_of("no figure here"), None);
    }

    #[test]
    fn an_impossible_efficiency_gives_the_planner_nothing() {
        let with = |e: Option<f64>| AntennaResult { design: Design::tuned(antenna::Kind::Patch, antenna::Band::Ble, antenna::Quality::Fast), sweep: Default::default(), pattern: None, slice: None, efficiency: e, run: Default::default(), cells: 0, seconds: 0.0, dir: Default::default() };
        // The README's patch: 44 % takes 3.6 dB off its 5.75 dBi.
        assert!((with(Some(0.44)).efficiency_db().unwrap() + 3.57).abs() < 0.01);
        assert_eq!(with(Some(1.03)).efficiency_db().unwrap(), 0.0, "numerics, not gain from nowhere");
        assert!(with(Some(1.4)).efficiency_db().is_err());
        assert!(with(Some(0.0)).efficiency_db().is_err());
        assert!(with(None).efficiency_db().is_err());
        // What the planner ends up adding to a link, not the lossless figure.
        let patch = AntennaResult { pattern: Some(Pattern { d: vec![vec![5.75]], ..Default::default() }), ..with(Some(0.44)) };
        assert!((patch.planner_gain().unwrap()[0][0] - 2.18).abs() < 0.01);
        assert!(AntennaResult { pattern: Some(Pattern::default()), ..with(Some(1.4)) }.planner_gain().is_err());
    }

    /// Runs the real solver (needs openEMS installed): `cargo test --release -- --ignored fdtd`.
    /// S21 through a 20 cm concrete wall at UHF RFID (needs openEMS): the
    /// obstruction loss must be positive and in a plausible range.
    #[test]
    #[ignore]
    fn fdtd_wall_s21_end_to_end() {
        let (a, b) = (Vec3::new(-1.0, 0.0, -0.1), Vec3::new(1.0, 2.5, 0.1));
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        let wall = region::SceneObj {
            name: "wall_1".into(),
            material: crate::rtls::radio::Material::Concrete,
            weight: crate::rtls::radio::Material::Concrete.weight(),
            parts: vec![(Arc::new(crate::bvh::Bvh::new(&pos, &idx)), bevy::math::Affine3A::IDENTITY)],
            min: a,
            max: b,
            rack: None,
        };
        let req = region::Request { band: antenna::Band::Rfid, quality: antenna::Quality::Fast, target: "wall_1".into(), target_min: a, target_max: b, objects: vec![wall], wall_db: 8.0 };
        let mut f = Fdtd::default();
        f.run_region(req);
        let t0 = Instant::now();
        while f.busy() && t0.elapsed().as_secs() < 1800 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let res = f.region().unwrap_or_else(|| panic!("no result: {:?} {:?}", f.status(), f.log_tail(10)));
        let k = res.with.f.len() / 2;
        let (w, fr) = (Sweep::db(res.with.s21.as_ref().unwrap()[k]), Sweep::db(res.free.s21.as_ref().unwrap()[k]));
        eprintln!("S21 with {w:.1} dB, free {fr:.1} dB, loss {:.1} dB, {} cells, {:.0} s", fr - w, res.cells, res.seconds);
        assert!(fr > -40.0, "free-space S21 {fr}");
        assert!((2.0..40.0).contains(&(fr - w)), "loss {}", fr - w);
        assert!(res.slice.is_some());
        // The whole wall (2.5 m high) is covered, and every plane came back.
        assert!(res.layout.region_max.y > 2.5 && res.layout.region_min.y < 0.05, "{:?} {:?}", res.layout.region_min, res.layout.region_max);
        assert_eq!(res.planes.len(), res.layout.planes.len(), "{:?}", f.log_tail(10));
        assert!(res.planes.iter().any(|p| p.du.y.abs() > 1.0 || p.dv.y.abs() > 1.0), "no vertical plane");
        assert!(!res.truncated, "{:?}", f.log_tail(10));
    }

    /// S21 through 20 cm of concrete at UWB, the case the audit suspected reads the patch's
    /// edges instead of the wall (needs openEMS): the same region with its cut edges inside
    /// the domain and carried on through the absorbing boundary, against P.2040's slab.
    #[test]
    #[ignore]
    fn fdtd_wall_uwb_edge_study() {
        use crate::rtls::radio::Material;
        let exe = exe().expect("openEMS installed");
        let base = std::env::temp_dir().join(format!("sensesim_edge_study_{}", std::process::id()));
        let w = solid("wall", Vec3::new(-2.0, 0.0, -0.1), Vec3::new(2.0, 3.0, 0.1), Material::Concrete);
        let req = region::Request { band: antenna::Band::Uwb5, quality: antenna::Quality::Fast, target: w.name.clone(), target_min: w.min, target_max: w.max, objects: vec![w], wall_db: 6.0 };
        let (with, free, l) = region::build(&req).unwrap();
        assert!(l.shrunk);
        let (lo, hi): (csx::P3, csx::P3) = {
            let (a, b) = (region::to_sim(l.region_min), region::to_sim(l.region_max));
            (std::array::from_fn(|k| a[k].min(b[k])), std::array::from_fn(|k| a[k].max(b[k])))
        };
        // The wall as it was: cut off at the region's edge, inside the domain.
        let mut cut = with.clone();
        for d in &mut cut.dielectrics {
            for b in &mut d.boxes {
                for k in 0..3 {
                    b.min[k] = b.min[k].max(lo[k]);
                    b.max[k] = b.max[k].min(hi[k] + l.cell);
                }
            }
        }
        let ctx = Ctx { shared: Arc::new(Mutex::new(Shared::default())), cancel: Arc::new(AtomicBool::new(false)), child: Arc::new(Mutex::new(None)) };
        let f0 = req.band.center();
        let freqs = post::linspace(f0 * 0.95, f0 * 1.05, 21);
        let mut go = |name: &str, sim: &csx::Sim| {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            let t0 = Instant::now();
            let end = run(&ctx, &exe, &d, sim, name).unwrap();
            let s = Sweep::read(&d, [73.0, 73.0], true, freqs.clone()).unwrap();
            let s21 = s.s21.unwrap();
            let db: f64 = s21.iter().map(|v| Sweep::db(*v)).sum::<f64>() / s21.len() as f64;
            eprintln!("{name}: {:.2} M cells, {:.0} s, truncated {}, S21 {db:.2} dB over the channel", sim.cells() as f64 / 1e6, t0.elapsed().as_secs_f64(), end.truncated);
            db
        };
        let free_db = go("free", &free);
        let cut_db = go("cut", &cut);
        let onward_db = go("onward", &with);
        let _ = std::fs::remove_dir_all(&base);
        // Normal incidence on a lossy slab: attenuation plus the two interfaces.
        let (eps, kappa) = region::electrical(Material::Concrete, f0).unwrap();
        let alpha = kappa * 376.73 / (2.0 * eps.sqrt());
        let slab = 8.686 * alpha * 0.2 + 20.0 * ((1.0 + eps.sqrt()).powi(2) / (4.0 * eps.sqrt())).log10();
        eprintln!("cell {:.2} mm, region {:.3} × {:.3} m: loss with cut edges {:.1} dB, carried through {:.1} dB, slab estimate {slab:.1} dB", l.cell * 1e3, hi[0] - lo[0], hi[2] - lo[2], free_db - cut_db, free_db - onward_db);
    }

    /// The simulated reflector source against the pattern `GainFix` takes out (needs
    /// openEMS): front, side and back at a few metres in an empty room.
    #[test]
    #[ignore]
    fn fdtd_reflector_pattern_study() {
        let at = Vec3::new(0.0, 1.2, 0.0);
        let t = scene::Tx { directional: true, ..study_tx(at) };
        let req = scene::SceneRequest { tech: Tech::Rfid, band: antenna::Band::Rfid, quality: antenna::Quality::Fast, tag_height: 1.2, lo: Vec2::new(-2.5, -2.5), hi: Vec2::new(2.5, 2.5), objects: Vec::new(), txs: vec![t.clone()] };
        let mut f = Fdtd::default();
        f.run_scene(req);
        wait(&f, 3600);
        let res = f.scene(Tech::Rfid).unwrap_or_else(|| panic!("no result: {:?} {:?}", f.status(), f.log_tail(10)));
        let fr = res.plan.f_real / 1e6;
        for (name, dir) in [("front", Vec3::X), ("45°", Vec3::new(0.707, 0.0, 0.707)), ("side", Vec3::Z), ("other side", Vec3::NEG_Z), ("back", Vec3::NEG_X)] {
            let d = 2.0;
            let l = res.link_db_at(0, at + dir * d).unwrap();
            let gain = l + crate::rtls::solver::fspl_db(d as f64, fr) as f32;
            eprintln!("{name}: {gain:.1} dBi from the field, {:.1} dBi modelled", scene::simulated_gain_db(true, t.fwd, dir));
        }
    }

    /// The Positioning tab's FDTD patch (needs openEMS): what its substrate loss costs.
    #[test]
    #[ignore]
    fn fdtd_patch_efficiency_end_to_end() {
        let design = Design::tuned(antenna::Kind::Patch, antenna::Band::Ble, antenna::Quality::Fast);
        let mut f = Fdtd::default();
        f.run_antenna(design);
        wait(&f, 1800);
        let res = f.antenna().unwrap_or_else(|| panic!("no result: {:?} {:?}", f.status(), f.log_tail(10)));
        let p = res.pattern.as_ref().expect("pattern");
        let e = res.efficiency.expect("efficiency");
        let db = res.efficiency_db().unwrap();
        eprintln!("patch: directivity {:.2} dBi, efficiency {:.0} %, gain {:.2} dBi, {:.0} s, truncated {}", p.max_dbi, e * 100.0, p.max_dbi + db, res.seconds, res.run.truncated);
        assert!(!res.run.truncated);
        // A thin FR-4 patch loses most of its power in the substrate, and the gain the
        // planner gets has to show it.
        assert!((0.2..0.8).contains(&e), "efficiency {e}");
        assert!(p.max_dbi + db < p.max_dbi - 1.0);
    }

    #[test]
    #[ignore]
    fn fdtd_dipole_end_to_end() {
        let design = Design::tuned(antenna::Kind::Dipole, antenna::Band::Uwb5, antenna::Quality::Fast);
        let mut f = Fdtd::default();
        f.run_antenna(design);
        let t0 = Instant::now();
        while f.busy() && t0.elapsed().as_secs() < 900 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let res = f.antenna().unwrap_or_else(|| panic!("no result: {:?} {:?}", f.status(), f.log_tail(10)));
        let (fr, db) = res.sweep.resonance().unwrap();
        eprintln!("resonance {:.3} GHz at {db:.1} dB, {:.0} s, {} cells", fr / 1e9, res.seconds, res.cells);
        assert!((fr / antenna::Band::Uwb5.center() - 1.0).abs() < 0.08, "resonance {fr}");
        assert!(db < -10.0, "S11 {db}");
        let p = res.pattern.as_ref().expect("pattern");
        eprintln!("directivity {:.2} dBi at {:?}", p.max_dbi, p.max_dir);
        assert!((1.5..3.0).contains(&p.max_dbi), "directivity {}", p.max_dbi);
        // A dipole along z radiates broadside (θ ≈ 90°).
        assert!((p.max_dir.0 - 90.0).abs() <= 15.0, "max at {:?}", p.max_dir);
        assert!(res.slice.as_ref().is_some_and(|s| s.nu > 10 && s.nv > 10 && !s.frames.is_empty()));
        // Nothing lossy in a metal dipole: the port's power should all come out as radiation.
        let e = res.efficiency.expect("efficiency");
        eprintln!("efficiency {:.1} %", e * 100.0);
        assert!((0.9..1.1).contains(&e), "efficiency {e}");
    }

    fn wait(f: &Fdtd, secs: u64) {
        let t0 = Instant::now();
        while f.busy() && t0.elapsed().as_secs() < secs {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    fn solid(name: &str, a: Vec3, b: Vec3, m: crate::rtls::radio::Material) -> region::SceneObj {
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        region::SceneObj { name: name.into(), material: m, weight: m.weight(), parts: vec![(Arc::new(crate::bvh::Bvh::new(&pos, &idx)), bevy::math::Affine3A::IDENTITY)], min: a, max: b, rack: None }
    }

    fn study_tx(p: Vec3) -> scene::Tx {
        scene::Tx { entity: Entity::PLACEHOLDER, name: "r1".into(), pos: p, fwd: Vec3::X, directional: false, gain_fix_db: 0.0, gain_fix: None, rx_offset_db: 0.0, wall_db: 8.0, exponent: None }
    }

    /// The stop level against a small room, for the trade-off in the report: how far each
    /// level moves the floor map from a run that uses the whole planned window, what it
    /// costs, and when the last probe sample falls. `needs openEMS`, tens of minutes:
    /// `cargo test --bin SenseSim fdtd_scene_stop_study -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn fdtd_scene_stop_study() {
        use crate::rtls::radio::Material;
        let exe = exe().expect("openEMS installed");
        let base = std::env::temp_dir().join(format!("sensesim_stop_study_{}", std::process::id()));
        let shell = |lo: Vec2, hi: Vec2, h: f32, m: Material| {
            vec![
                solid("n", Vec3::new(lo.x, 0.0, lo.y), Vec3::new(hi.x, h, lo.y + 0.15), m),
                solid("s", Vec3::new(lo.x, 0.0, hi.y - 0.15), Vec3::new(hi.x, h, hi.y), m),
                solid("w", Vec3::new(lo.x, 0.0, lo.y), Vec3::new(lo.x + 0.15, h, hi.y), m),
                solid("e", Vec3::new(hi.x - 0.15, 0.0, lo.y), Vec3::new(hi.x, h, hi.y), m),
                solid("roof", Vec3::new(lo.x, h, lo.y), Vec3::new(hi.x, h + 0.15, hi.y), m),
            ]
        };
        let small = (Vec2::new(-1.8, -1.5), Vec2::new(1.8, 1.5), Vec3::new(-1.1, 1.2, -0.6));
        let long = (Vec2::new(-5.0, -2.0), Vec2::new(5.0, 2.0), Vec3::new(-4.5, 1.2, -1.5));
        let divider = |a: Vec2, b: Vec2| solid("divider", Vec3::new(-0.1, 0.0, a.y), Vec3::new(0.1, 2.0, b.y), Material::Concrete);
        let with_shell = |r: (Vec2, Vec2, Vec3), m: Material| {
            let mut v = shell(r.0, r.1, 2.2, m);
            v.push(divider(r.0, r.1));
            v.push(solid("rack", Vec3::new(r.2.x + 0.5, 0.0, r.1.y - 0.8), Vec3::new(r.2.x + 0.9, 1.9, r.1.y - 0.3), Material::Metal));
            v
        };
        // Open: the walls are the boundary. Closed: concrete all round, a slab and some steel
        // keep energy in. Steel: the worst case for reverberation. Long: the audit's far corner.
        let rooms = [
            ("open", small, vec![divider(small.0, small.1)]),
            ("closed", small, with_shell(small, Material::Concrete)),
            ("steel", small, with_shell(small, Material::Metal)),
            ("long", long, vec![divider(long.0, long.1)]),
            ("long_closed", long, with_shell(long, Material::Concrete)),
        ];
        let only = std::env::var("STOP_STUDY_ROOMS").unwrap_or_default();
        let levels = [1e-3, 1e-4, 1e-5, 1e-6, 1e-9];
        for (room, (lo, hi, at), objects) in rooms {
            if !only.is_empty() && !only.split(',').any(|r| r == room) {
                continue;
            }
            let req = scene::SceneRequest {
                tech: Tech::Rfid,
                band: antenna::Band::Rfid,
                quality: antenna::Quality::Fast,
                tag_height: 1.2,
                lo,
                hi,
                objects,
                txs: vec![study_tx(at)],
            };
            let plan = scene::plan(&req);
            let (sims, _) = scene::build(&req, &plan).unwrap();
            let lambda_dt = plan.cell / (csx::C0 * 3f64.sqrt());
            eprintln!("\n{room}: {:.2} M cells, cell {:.1} mm, planned window {:.1} ns, step cap {} ({:.1} ns)", plan.cells as f64 / 1e6, plan.cell * 1e3, plan.steps as f64 * lambda_dt * 1e9, sims[0].max_timesteps, sims[0].max_timesteps as f64 * lambda_dt * 1e9);
            let mut results: Vec<(f64, f64, f64, scene::TxField)> = Vec::new();
            for level in levels {
                let dir = base.join(format!("{room}_{level:e}"));
                std::fs::create_dir_all(&dir).unwrap();
                let mut sim = sims[0].clone();
                sim.end_criteria = level;
                let ctx = Ctx { shared: Arc::new(Mutex::new(Shared::default())), cancel: Arc::new(AtomicBool::new(false)), child: Arc::new(Mutex::new(None)) };
                let t0 = Instant::now();
                let end = run(&ctx, &exe, &dir, &sim, "study").unwrap();
                let secs = t0.elapsed().as_secs_f64();
                let (t, _) = post::read_probe(&dir.join("port_ut1")).unwrap();
                let last = t[t.len() - 1];
                let f = scene::read_tx(&dir, &plan, req.band, req.txs[0].clone(), 0).unwrap();
                eprintln!("  stop {level:e}: {secs:.0} s, last probe {:.1} ns (needs {:.1}), {:.1} Gcell-steps, hit the step limit: {}", last * 1e9, plan.min_window_s() * 1e9, plan.cells as f64 * (last / lambda_dt) / 1e9, end.truncated);
                results.push((level, secs, last, f));
                let _ = std::fs::remove_dir_all(&dir);
            }
            let k = plan.levels.iter().enumerate().min_by(|a, b| (a.1 - 1.2).abs().total_cmp(&(b.1 - 1.2).abs())).unwrap().0;
            let reference = results.last().unwrap().3.levels[k].clone();
            // Past the divider, against everything in the room.
            let behind = |i: usize| reference.u0 + i as f64 * reference.step > 0.15;
            eprintln!("  vs the longest run (dB moved at tag height): stop | all floor mean/p95/max | behind the wall mean/p95/max | signed mean behind");
            for (level, secs, last, f) in &results {
                let pl = &f.levels[k];
                let mut all = Vec::new();
                let mut back = Vec::new();
                let mut signed = 0.0f64;
                for j in 0..pl.nv {
                    for i in 0..pl.nu {
                        let (a, b) = (pl.db[i + pl.nu * j], reference.db[i + pl.nu * j]);
                        if !(a.is_finite() && b.is_finite()) {
                            continue;
                        }
                        let d = (a - b) as f64;
                        all.push(d.abs());
                        if behind(i) {
                            back.push(d.abs());
                            signed += d;
                        }
                    }
                }
                let stat = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    (v.iter().sum::<f64>() / v.len().max(1) as f64, v[(v.len() as f64 * 0.95) as usize], v[v.len() - 1])
                };
                let (am, ap, ax) = stat(&mut all);
                let n = back.len().max(1) as f64;
                let (bm, bp, bx) = stat(&mut back);
                eprintln!("  {level:e} | {am:.2}/{ap:.2}/{ax:.2} | {bm:.2}/{bp:.2}/{bx:.2} | {:+.2} | {secs:.0} s, last {:.1} ns", signed / n, last * 1e9);
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Whole scene at UHF RFID (needs openEMS): a 6 × 5 m room split by a
    /// 20 cm concrete wall, two readers on one side. In line of sight the link
    /// follows Friis (dipole gain, real frequency); behind the wall it loses
    /// several dB more; moving one reader re-simulates only that one.
    #[test]
    #[ignore]
    fn fdtd_scene_end_to_end() {
        let (a, b) = (Vec3::new(-0.1, 0.0, -2.5), Vec3::new(0.1, 3.0, 2.5));
        let v = |x: bool, y: bool, z: bool| [if x { b.x } else { a.x }, if y { b.y } else { a.y }, if z { b.z } else { a.z }];
        let pos: Vec<[f32; 3]> = (0..8).map(|i| v(i & 1 != 0, i & 2 != 0, i & 4 != 0)).collect();
        let idx = [0, 1, 3, 0, 3, 2, 4, 6, 7, 4, 7, 5, 0, 4, 5, 0, 5, 1, 2, 3, 7, 2, 7, 6, 0, 2, 6, 0, 6, 4, 1, 5, 7, 1, 7, 3];
        let wall = region::SceneObj {
            name: "wall_1".into(),
            material: crate::rtls::radio::Material::Concrete,
            weight: crate::rtls::radio::Material::Concrete.weight(),
            parts: vec![(Arc::new(crate::bvh::Bvh::new(&pos, &idx)), bevy::math::Affine3A::IDENTITY)],
            min: a,
            max: b,
            rack: None,
        };
        let tx = |name: &str, p: Vec3| scene::Tx {
            entity: Entity::PLACEHOLDER,
            name: name.into(),
            pos: p,
            fwd: Vec3::X,
            directional: false,
            gain_fix_db: 0.0,
            gain_fix: None,
            rx_offset_db: 0.0,
            wall_db: 6.0,
            exponent: None,
        };
        let mut req = scene::SceneRequest {
            tech: Tech::Rfid,
            band: antenna::Band::Rfid,
            quality: antenna::Quality::Fast,
            tag_height: 1.2,
            lo: Vec2::new(-3.0, -2.5),
            hi: Vec2::new(3.0, 2.5),
            objects: vec![wall],
            txs: vec![tx("r1", Vec3::new(-1.5, 1.2, -1.5)), tx("r2", Vec3::new(-2.0, 1.2, 1.5))],
        };
        let mut f = Fdtd::default();
        f.run_scene(req.clone());
        wait(&f, 3600);
        let res = f.scene(Tech::Rfid).unwrap_or_else(|| panic!("no result: {:?} {:?}", f.status(), f.log_tail(10)));
        assert!(!res.plan.scaled());
        assert_eq!(res.simulated, 2);
        let fr = res.plan.f_real / 1e6;
        // Deficit against Friis (dipole to isotropic, free space).
        let deficit = |p: Vec3| {
            let l = res.link_db_at(0, p).expect("point inside the run");
            let d = (p - req.txs[0].pos).length() as f64;
            let friis = scene::DIPOLE_DBI - crate::rtls::solver::fspl_db(d, fr) as f32;
            friis - l
        };
        let los = deficit(Vec3::new(-1.5, 1.2, 1.0));
        let behind = deficit(Vec3::new(1.0, 1.2, -1.5));
        eprintln!("deficit vs Friis: LOS {los:.1} dB, behind the wall {behind:.1} dB; {:.0} s, {} cells", res.seconds, res.plan.cells);
        assert!(los.abs() < 3.0, "LOS deficit {los}");
        assert!(behind - los > 5.0, "wall loss {}", behind - los);
        assert!(!res.planes(None, true, |_| 1.0).is_empty());

        // Move r2 only: r1 is reused.
        req.txs[1].pos.z = 1.0;
        f.run_scene(req);
        wait(&f, 3600);
        let res2 = f.scene(Tech::Rfid).unwrap();
        assert_eq!(res2.simulated, 1, "{:?}", f.log_tail(10));
        assert_eq!(res2.per_tx.len(), 2);
    }
}
