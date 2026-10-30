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

pub struct AntennaResult {
    pub design: Design,
    pub sweep: Sweep,
    pub pattern: Option<Pattern>,
    pub slice: Option<Slice>,
    pub cells: u64,
    pub seconds: f64,
    pub dir: PathBuf,
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
            run(ctx, &exe, &dir, &sim, "Antenna")?;
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
            let res = AntennaResult { design, sweep, pattern, slice, cells: sim.cells(), seconds: t0.elapsed().as_secs_f64(), dir };
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
            run(ctx, &exe, &dir, &with, "Through the object (1/2)")?;
            run(ctx, &exe, &free_dir, &free, "Free space (2/2)")?;
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
                run(ctx, &exe, &d, sim, &format!("{} ({}/{n})", tx.name, i + 1))?;
                ctx.set(Status::Busy(format!("{}: field planes…", tx.name), -1.0));
                per_tx.push(scene::read_tx(&d, &plan, req.band, tx.clone(), key)?);
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

/// Writes `sim.xml` into `dir` and runs openEMS there, reporting progress from
/// its energy decay towards the end criterion.
fn run(ctx: &Ctx, exe: &Path, dir: &Path, sim: &csx::Sim, label: &str) -> Result<()> {
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
    if let Some(mut e) = stderr {
        let shared = ctx.shared.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            if !s.trim().is_empty() {
                shared.lock().unwrap().log.push_back(s.trim().to_string());
            }
        });
    }
    let mut last_err = String::new();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let l = line.trim();
        if l.contains("Timestep:") {
            // "[@ 12s] Timestep: 117 || Speed: 44.2 MC/s (…) || Energy: ~3.7e-20 (-12.34dB)"
            let db = l.rsplit('(').next().and_then(|s| s.trim_end_matches(')').trim_end_matches("dB").replace(' ', "").parse::<f64>().ok()).unwrap_or(0.0);
            let step = l.split("Timestep:").nth(1).and_then(|s| s.split("||").next()).map(|s| s.trim().to_string()).unwrap_or_default();
            let speed = l.split("Speed:").nth(1).and_then(|s| s.split('(').next()).map(|s| s.trim().to_string()).unwrap_or_default();
            let p = (-db / target_db).clamp(0.0, 1.0) as f32;
            ctx.set(Status::Busy(format!("{label}: timestep {step} · {speed} · energy {db:.0} of -{target_db:.0} dB"), p));
        } else if l.to_ascii_lowercase().contains("error") {
            last_err = l.to_string();
            ctx.log(l);
        } else if l.starts_with("RunFDTD") || l.starts_with("Time for") {
            ctx.log(format!("{label}: {l}"));
        }
    }
    let status = ctx.child.lock().unwrap().take().map(|mut c| c.wait());
    if ctx.cancelled() {
        bail!("cancelled");
    }
    match status {
        Some(Ok(s)) if s.success() => Ok(()),
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

type ObjectQuery<'w, 's> = Query<'w, 's, (Entity, &'static crate::scene::PlacedObject, &'static Transform)>;
type PartQuery<'w, 's> = Query<'w, 's, (&'static crate::scene::PartOf, &'static crate::scene::PartBvh, &'static GlobalTransform)>;

/// Every placed object as the voxeliser sees it (mesh parts only with `with_parts`).
fn scene_objects(objects: &ObjectQuery, parts: &PartQuery, with_parts: bool) -> Vec<region::SceneObj> {
    let mut objs: Vec<region::SceneObj> = objects
        .iter()
        .map(|(_, o, t)| {
            let (min, max) = world_bounds(o, t);
            let (material, weight) = crate::rtls::radio::obstruction_of_asset(&o.asset);
            region::SceneObj { name: o.name.clone(), material, weight, parts: Vec::new(), min, max }
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

/// Builds the S21 job from the scene once the UI asked for it.
pub fn collect_region(mut fdtd: ResMut<Fdtd>, objects: ObjectQuery, parts: PartQuery) {
    let Some((band, quality, target)) = fdtd.region_requested.take() else { return };
    let Ok((_, tobj, tt)) = objects.get(target) else {
        fdtd.shared.lock().unwrap().status = Some(Status::Failed("Select a scene object (wall, rack, door…) first".into()));
        return;
    };
    let (tmin, tmax) = world_bounds(tobj, tt);
    let req = region::Request { band, quality, target: tobj.name.clone(), target_min: tmin, target_max: tmax, objects: scene_objects(&objects, &parts, true) };
    fdtd.run_region(req);
}

/// The band a technology's infrastructure transmits on (UWB: its channel).
fn band_of(tech: Tech, prof: Option<&crate::rtls::device::Profile>) -> antenna::Band {
    match tech {
        Tech::Uwb if prof.is_some_and(|p| p.freq_mhz > 7200.0) => antenna::Band::Uwb9,
        Tech::Uwb => antenna::Band::Uwb5,
        Tech::Ble => antenna::Band::Ble,
        Tech::Rfid => antenna::Band::Rfid,
    }
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
    let mut band_prof = None;
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
            Tech::Ble => (prof.wall_loss_db, Some(prof.path_loss_exp)),
            Tech::Rfid => (prof.wall_loss_db, None),
        };
        txs.push(scene::Tx { entity: e, name: d.id.clone(), pos: t.translation, fwd, directional, gain_fix_db, rx_offset_db, wall_db, exponent });
        band_prof.get_or_insert(prof);
    }
    let (lo, hi) = crate::rtls::planning::area(&placed, &devices);
    let req = scene::SceneRequest {
        tech,
        band: band_of(tech, band_prof.as_ref()),
        quality,
        tag_height,
        lo,
        hi,
        objects: scene_objects(&objects, &parts, requested.is_some()),
        txs,
    };
    if requested.is_some() {
        fdtd.run_scene(req);
        return;
    }
    *next_preview = time.elapsed_secs_f64() + 1.0;
    let plan = scene::plan(&req);
    // Unchanged transmitters of the last run are reused; a moved one is not.
    let reused = fdtd.scene(tech).map_or(0, |s| {
        req.txs.iter().filter(|t| s.plan.f_sim == plan.f_sim && s.plan.lo == plan.lo && s.plan.hi == plan.hi && s.per_tx.iter().any(|f| (f.tx.pos - t.pos).length() < 0.01 && f.tx.directional == t.directional)).count()
    });
    fdtd.scene_preview = Some(ScenePreview { tech, band: req.band, plan, txs: req.txs.len(), reused });
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
        let current = crate::rtls::radio::Material::ALL.map(|m| crate::rtls::radio::MaterialCal::scales(m).0);
        let fit = scene::fit_calibration(&samples, current).map(Arc::new);
        fdtd.shared.lock().unwrap().fits.push((res.dir.clone(), fit));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let req = region::Request { band: antenna::Band::Rfid, quality: antenna::Quality::Fast, target: "wall_1".into(), target_min: a, target_max: b, objects: vec![wall] };
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
    }

    fn wait(f: &Fdtd, secs: u64) {
        let t0 = Instant::now();
        while f.busy() && t0.elapsed().as_secs() < secs {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
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
        };
        let tx = |name: &str, p: Vec3| scene::Tx {
            entity: Entity::PLACEHOLDER,
            name: name.into(),
            pos: p,
            fwd: Vec3::X,
            directional: false,
            gain_fix_db: 0.0,
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
