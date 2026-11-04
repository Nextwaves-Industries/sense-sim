//! Graphics card support: the renderer starts on the discrete (RTX) GPU by
//! default, and Settings → Graphics picks the backend and adapter, vsync and
//! a frame cap. Live telemetry comes from `nvidia-smi`. The GPU is linked to
//! OpenEMS as the `gpu0` consumption meter (`Gpu.SenseSim` in
//! `tools/openems_rtls`). Sense Sim writes power, load and temperature, and
//! OpenEMS can write `gpu0/FpsLimit` back to cap the frame rate.
//!
//! Preferences live in `%APPDATA%\SenseSim\gpu.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapter, RenderAdapterInfo};
use bevy::render::settings::{Backends, PowerPreference, WgpuFeatures, WgpuSettings};
use bevy::window::{PresentMode, PrimaryWindow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::rtls::RtlsConfig;
use crate::rtls::io::openems::{auth_header, channel_url};
use crate::rtls::openems_local::REST_URL;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Backend {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    Gl,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Power {
    /// Discrete GPU (RTX) on machines that also have integrated graphics.
    #[default]
    HighPerformance,
    LowPower,
}

/// Where the vision models (ONNX Runtime) run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum VisionDevice {
    /// DirectML on the high-performance GPU.
    #[default]
    Gpu,
    /// DirectML on the power-saving GPU.
    LowPower,
    Cpu,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuPrefs {
    pub backend: Backend,
    pub power: Power,
    /// Adapter name to use ("" = chosen by `power`). Matches by substring.
    pub adapter: String,
    pub vsync: bool,
    /// Frame cap (0 = none).
    pub fps_limit: u32,
    pub vision: VisionDevice,
    /// Write GPU telemetry to OpenEMS (`gpu0`) and obey `gpu0/FpsLimit`.
    pub openems: bool,
    pub openems_period_s: f32,
    /// Set while a renderer with these settings starts. Still set at the next
    /// launch means it crashed, and the device choice is reset.
    pub starting: bool,
}

impl Default for GpuPrefs {
    fn default() -> Self {
        Self {
            backend: Backend::Auto,
            power: Power::HighPerformance,
            adapter: String::new(),
            vsync: true,
            fps_limit: 0,
            vision: VisionDevice::Gpu,
            openems: true,
            openems_period_s: 2.0,
            starting: false,
        }
    }
}

impl GpuPrefs {
    pub fn path() -> PathBuf {
        let base = crate::app_paths::config_base_or_current();
        base.join("SenseSim").join("gpu.json")
    }

    fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, t);
        }
    }

    /// The renderer needs a restart for these.
    fn device_choice(&self) -> (Backend, Power, &str) {
        (self.backend, self.power, self.adapter.trim())
    }

    pub fn wgpu_settings(&self) -> WgpuSettings {
        let mut s = WgpuSettings {
            power_preference: match self.power {
                Power::HighPerformance => PowerPreference::HighPerformance,
                Power::LowPower => PowerPreference::LowPower,
            },
            ..default()
        };
        // WGPU_BACKEND set by the user still wins (WgpuSettings::default reads it).
        if std::env::var_os("WGPU_BACKEND").is_none() {
            match self.backend {
                // Explicit: "any" could choose wgpu's OpenGL backend if a dependency compiles it in.
                Backend::Auto => s.backends = Some(Backends::VULKAN | Backends::DX12),
                Backend::Dx12 => s.backends = Some(Backends::DX12),
                Backend::Vulkan => s.backends = Some(Backends::VULKAN),
                Backend::Gl => s.backends = Some(Backends::GL),
            }
        }
        let name = self.adapter.trim();
        if !name.is_empty() {
            s.adapter_name = Some(name.to_lowercase());
        }
        s
    }

    pub fn present_mode(&self) -> PresentMode {
        if self.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync }
    }
}

/// Reads the preferences for this launch and marks it as starting. If the
/// last launch never got a renderer up, it falls back to the automatic
/// choice. Returns the preferences and whether that happened.
pub fn startup_prefs(headless: bool) -> (GpuPrefs, bool) {
    let mut p = GpuPrefs::load();
    VISION.store(p.vision as u8, Ordering::Relaxed);
    if headless {
        // Scripted runs leave the crash guard alone.
        return (p, false);
    }
    let recovered = p.starting && p.device_choice() != GpuPrefs::default().device_choice();
    if recovered {
        warn!("the last start with backend {:?} / adapter '{}' failed; using the automatic GPU choice", p.backend, p.adapter);
        p.backend = Backend::Auto;
        p.adapter.clear();
        p.power = Power::HighPerformance;
    }
    p.starting = true;
    p.save();
    p.starting = false;
    (p, recovered)
}

static VISION: AtomicU8 = AtomicU8::new(0);
static VISION_ON_GPU: AtomicBool = AtomicBool::new(false);

/// Vision device for models loaded from now on.
pub fn vision_device() -> VisionDevice {
    match VISION.load(Ordering::Relaxed) {
        1 => VisionDevice::LowPower,
        2 => VisionDevice::Cpu,
        _ => VisionDevice::Gpu,
    }
}

/// Reported by the vision runtime once a model is loaded.
pub fn set_vision_on_gpu(on: bool) {
    VISION_ON_GPU.store(on, Ordering::Relaxed);
}

/// The adapter the renderer runs on.
#[derive(Clone, Debug)]
pub struct ActiveAdapter {
    pub name: String,
    pub backend: String,
    pub kind: String,
    pub driver: String,
    pub discrete: bool,
    pub software: bool,
    /// Hardware ray tracing (RTX): ray queries supported by the adapter.
    pub ray_tracing: bool,
}

/// An adapter found by enumeration (one row per GPU and backend).
#[derive(Clone, Debug)]
pub struct AdapterRow {
    pub name: String,
    pub backend: String,
    pub kind: String,
}

/// One GPU as `nvidia-smi` reports it. Missing values are `None`.
#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub name: String,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub util_pct: Option<f32>,
    pub mem_util_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub mem_used_mb: Option<f32>,
    pub mem_total_mb: Option<f32>,
    pub clock_mhz: Option<f32>,
    pub fan_pct: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    pub gpus: Vec<Reading>,
    /// Why there is none (no NVIDIA driver, …).
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LinkStatus {
    pub ok: bool,
    pub text: String,
    /// `gpu0/FpsLimit` as last read from OpenEMS (0 = none).
    pub ems_fps_limit: u32,
}

/// Link settings the worker thread reads every period.
#[derive(Clone, Debug, Default)]
struct LinkCfg {
    enabled: bool,
    period_s: f32,
    url: String,
    auth: String,
    render_name: String,
    ray_tracing: bool,
}

#[derive(Default)]
struct Shared {
    telemetry: Mutex<Telemetry>,
    link: Mutex<LinkStatus>,
    cfg: Mutex<LinkCfg>,
    /// Frames per second x10, from the main thread.
    fps_x10: AtomicU32,
    /// The UI showed telemetry recently: keep polling without the link.
    wanted: Mutex<Option<Instant>>,
}

#[derive(Resource)]
pub struct Gpu {
    pub prefs: GpuPrefs,
    /// What the running renderer started with.
    launched: GpuPrefs,
    /// The saved device choice crashed the last launch and was reset.
    pub recovered: bool,
    pub active: Option<ActiveAdapter>,
    adapters: Arc<Mutex<Option<Vec<AdapterRow>>>>,
    listing: bool,
    shared: Arc<Shared>,
    frames: u32,
    headless: bool,
    /// Settings asked to restart with the new GPU choice.
    pub restart: bool,
}

impl Gpu {
    pub fn restart_needed(&self) -> bool {
        self.prefs.device_choice() != self.launched.device_choice()
    }

    pub fn telemetry(&self) -> Telemetry {
        *self.shared.wanted.lock().unwrap() = Some(Instant::now());
        self.shared.telemetry.lock().unwrap().clone()
    }

    pub fn link(&self) -> LinkStatus {
        self.shared.link.lock().unwrap().clone()
    }

    /// A scripted run (screenshot or test), not someone at the window.
    pub fn headless(&self) -> bool {
        self.headless
    }

    pub fn fps(&self) -> f32 {
        self.shared.fps_x10.load(Ordering::Relaxed) as f32 / 10.0
    }

    /// Effective frame cap: the tighter of the user's and OpenEMS's (0 = none).
    pub fn fps_cap(&self) -> u32 {
        let ems = if self.prefs.openems { self.shared.link.lock().unwrap().ems_fps_limit } else { 0 };
        match (self.prefs.fps_limit, ems) {
            (0, e) => e,
            (u, 0) => u,
            (u, e) => u.min(e),
        }
    }

    /// All adapters, enumerated once in the background (None while listing).
    pub fn adapters(&mut self) -> Option<Vec<AdapterRow>> {
        if !self.listing {
            self.listing = true;
            let out = self.adapters.clone();
            std::thread::spawn(move || *out.lock().unwrap() = Some(enumerate_adapters()));
        }
        self.adapters.lock().unwrap().clone()
    }

    pub fn save(&mut self) {
        VISION.store(self.prefs.vision as u8, Ordering::Relaxed);
        self.prefs.save();
    }
}

fn enumerate_adapters() -> Vec<AdapterRow> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let mut rows: Vec<AdapterRow> = bevy::tasks::block_on(instance.enumerate_adapters(Backends::VULKAN | Backends::DX12))
        .into_iter()
        .map(|a| {
            let i = a.get_info();
            AdapterRow { name: i.name, backend: format!("{:?}", i.backend), kind: device_kind(i.device_type).into() }
        })
        .collect();
    rows.sort_by(|a, b| (a.kind != "Discrete", &a.name, &a.backend).cmp(&(b.kind != "Discrete", &b.name, &b.backend)));
    rows.dedup_by(|a, b| a.name == b.name && a.backend == b.backend);
    rows
}

fn device_kind(t: wgpu::DeviceType) -> &'static str {
    match t {
        wgpu::DeviceType::DiscreteGpu => "Discrete",
        wgpu::DeviceType::IntegratedGpu => "Integrated",
        wgpu::DeviceType::VirtualGpu => "Virtual",
        wgpu::DeviceType::Cpu => "Software",
        wgpu::DeviceType::Other => "Other",
    }
}

pub struct GpuPlugin {
    pub prefs: GpuPrefs,
    pub recovered: bool,
    /// Scripted runs: no telemetry thread, no OpenEMS link.
    pub headless: bool,
}

impl Plugin for GpuPlugin {
    fn build(&self, app: &mut App) {
        let shared = Arc::new(Shared::default());
        if !self.headless {
            let s = shared.clone();
            std::thread::Builder::new().name("gpu-telemetry".into()).spawn(move || worker(s)).ok();
        }
        let mut prefs = self.prefs.clone();
        if self.headless {
            prefs.openems = false;
        }
        app.insert_resource(Gpu {
            launched: prefs.clone(),
            prefs,
            recovered: self.recovered,
            active: None,
            adapters: Arc::new(Mutex::new(None)),
            listing: false,
            shared,
            frames: 0,
            restart: false,
            headless: self.headless,
        })
        .add_systems(Update, (detect_adapter, sync_window, sync_link, restart))
        .add_systems(Last, frame_cap);
    }
}

/// Reads the adapter once the renderer is up, then clears the crash guard.
fn detect_adapter(mut gpu: ResMut<Gpu>, info: Option<Res<RenderAdapterInfo>>, adapter: Option<Res<RenderAdapter>>) {
    if gpu.active.is_none()
        && let Some(info) = info
    {
        let ray_tracing = adapter.is_some_and(|a| a.features().contains(WgpuFeatures::EXPERIMENTAL_RAY_QUERY));
        let a = ActiveAdapter {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            kind: device_kind(info.device_type).into(),
            driver: [info.driver.as_str(), info.driver_info.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" "),
            discrete: info.device_type == wgpu::DeviceType::DiscreteGpu,
            software: info.device_type == wgpu::DeviceType::Cpu,
            ray_tracing,
        };
        info!("GPU: {} ({}, {}), ray tracing {}", a.name, a.backend, a.kind, if a.ray_tracing { "yes" } else { "no" });
        if a.software {
            warn!("rendering in software ({}): install or update the graphics driver", a.name);
        }
        gpu.active = Some(a);
    }
    // The renderer survived a few frames with these settings.
    if gpu.active.is_some() && gpu.frames < 30 && !gpu.headless {
        gpu.frames += 1;
        if gpu.frames == 30 {
            let mut p = GpuPrefs::load();
            p.starting = false;
            p.save();
        }
    }
}

fn sync_window(gpu: Res<Gpu>, mut window: Single<&mut Window, With<PrimaryWindow>>, diagnostics: Res<DiagnosticsStore>) {
    let mode = gpu.prefs.present_mode();
    if window.present_mode != mode {
        window.present_mode = mode;
    }
    let fps = diagnostics.get(&FrameTimeDiagnosticsPlugin::FPS).and_then(|d| d.smoothed()).unwrap_or(0.0);
    gpu.shared.fps_x10.store((fps * 10.0).round().max(0.0) as u32, Ordering::Relaxed);
}

/// Gives the worker the OpenEMS that the RTLS outputs use (the local one by default).
fn sync_link(gpu: Res<Gpu>, rtls: Option<Res<RtlsConfig>>) {
    let o = rtls.as_ref().map(|c| &c.outputs.openems);
    let url = o.map(|o| o.url.trim()).filter(|u| !u.is_empty()).unwrap_or(REST_URL).to_string();
    let auth = o.map(|o| auth_header(&o.username, &o.password)).unwrap_or_else(|| auth_header("x", "admin"));
    let active = gpu.active.as_ref();
    let cfg = LinkCfg {
        enabled: gpu.prefs.openems,
        period_s: gpu.prefs.openems_period_s,
        url,
        auth,
        render_name: active.map(|a| a.name.clone()).unwrap_or_default(),
        ray_tracing: active.is_some_and(|a| a.ray_tracing),
    };
    let mut c = gpu.shared.cfg.lock().unwrap();
    if c.enabled != cfg.enabled || c.url != cfg.url || c.auth != cfg.auth || c.period_s != cfg.period_s || c.render_name != cfg.render_name || c.ray_tracing != cfg.ray_tracing {
        *c = cfg;
    }
}

/// Sleeps out the rest of the frame when a frame cap is set.
fn frame_cap(gpu: Res<Gpu>, mut last: Local<Option<Instant>>) {
    let cap = gpu.fps_cap();
    if cap > 0 {
        let frame = Duration::from_secs_f64(1.0 / cap.max(1) as f64);
        if let Some(t) = *last {
            let spent = t.elapsed();
            if spent < frame {
                std::thread::sleep(frame - spent);
            }
        }
    }
    *last = Some(Instant::now());
}

const QUERY: &str = "name,power.draw,power.limit,utilization.gpu,utilization.memory,temperature.gpu,memory.used,memory.total,clocks.gr,fan.speed";

fn nvidia_smi() -> Result<Vec<Reading>, String> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.arg(format!("--query-gpu={QUERY}")).arg("--format=csv,noheader,nounits");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|_| "no NVIDIA driver (nvidia-smi not found); live telemetry needs an NVIDIA GPU".to_string())?;
    if !out.status.success() {
        return Err(format!("nvidia-smi: {}", String::from_utf8_lossy(&out.stdout).trim()));
    }
    Ok(parse_smi(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_smi(text: &str) -> Vec<Reading> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let n = |i: usize| f.get(i).and_then(|s| s.parse::<f32>().ok());
            Reading {
                name: f.first().unwrap_or(&"").to_string(),
                power_w: n(1),
                power_limit_w: n(2),
                util_pct: n(3),
                mem_util_pct: n(4),
                temp_c: n(5),
                mem_used_mb: n(6),
                mem_total_mb: n(7),
                clock_mhz: n(8),
                fan_pct: n(9),
            }
        })
        .collect()
}

/// `gpu0` channel values from the telemetry. Power is the sum over all GPUs,
/// everything else is the render GPU's.
fn channels(t: &Telemetry, cfg: &LinkCfg, fps: f32, vision_gpu: bool) -> Vec<(&'static str, i64)> {
    let main = t.gpus.iter().find(|g| !cfg.render_name.is_empty() && cfg.render_name.contains(&g.name)).or(t.gpus.first());
    let r = |v: Option<f32>| v.map(|x| x.round() as i64);
    let mut out = vec![
        ("Online", (t.error.is_none() && !t.gpus.is_empty()) as i64),
        ("GpuCount", t.gpus.len() as i64),
        ("RenderFps", fps.round() as i64),
        ("RayTracing", cfg.ray_tracing as i64),
        ("VisionOnGpu", vision_gpu as i64),
    ];
    // No reading is no value, not 0 W (and one GPU without a reading spoils the sum).
    let power: Option<f32> = if t.gpus.is_empty() { None } else { t.gpus.iter().map(|g| g.power_w).sum() };
    if let Some(p) = power {
        out.push(("PowerW", p.round() as i64));
    }
    if let Some(g) = main {
        for (ch, v) in [
            ("PowerLimitW", g.power_limit_w),
            ("UtilizationPct", g.util_pct),
            ("MemoryUtilPct", g.mem_util_pct),
            ("TemperatureC", g.temp_c),
            ("MemoryUsedMb", g.mem_used_mb),
            ("MemoryTotalMb", g.mem_total_mb),
            ("ClockMhz", g.clock_mhz),
            ("FanPct", g.fan_pct),
        ] {
            if let Some(v) = r(v) {
                out.push((ch, v));
            }
        }
    }
    out
}

fn worker(shared: Arc<Shared>) {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).http_status_as_error(false).build().into();
    // Last value written per channel, and when (OpenEMS applies a write on its
    // next cycle and the request blocks until then: send only changes).
    let mut written: HashMap<&'static str, (i64, Instant)> = HashMap::new();
    let mut created_for = String::new();
    let mut last_url = String::new();
    loop {
        let cfg = shared.cfg.lock().unwrap().clone();
        let period = Duration::from_secs_f32(cfg.period_s.clamp(0.5, 60.0));
        let wanted = shared.wanted.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(5));
        if !cfg.enabled && !wanted {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let t0 = Instant::now();
        let telemetry = match nvidia_smi() {
            Ok(gpus) => Telemetry { gpus, error: None },
            Err(e) => Telemetry { gpus: Vec::new(), error: Some(e) },
        };
        *shared.telemetry.lock().unwrap() = telemetry.clone();
        if cfg.enabled {
            if cfg.url != last_url {
                last_url = cfg.url.clone();
                written.clear();
            }
            let fps = shared.fps_x10.load(Ordering::Relaxed) as f32 / 10.0;
            let status = push(&agent, &cfg, &channels(&telemetry, &cfg, fps, VISION_ON_GPU.load(Ordering::Relaxed)), &mut written, &mut created_for);
            *shared.link.lock().unwrap() = status;
        } else {
            *shared.link.lock().unwrap() = LinkStatus { ok: false, text: "off".into(), ems_fps_limit: 0 };
        }
        if let Some(rest) = period.checked_sub(t0.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Writes the changed channels to `gpu0` in parallel, then reads `FpsLimit`.
fn push(agent: &ureq::Agent, cfg: &LinkCfg, values: &[(&'static str, i64)], written: &mut HashMap<&'static str, (i64, Instant)>, created_for: &mut String) -> LinkStatus {
    let pending: Vec<(&'static str, i64)> = values
        .iter()
        .copied()
        .filter(|(ch, v)| written.get(ch).is_none_or(|(old, at)| old != v || at.elapsed() > Duration::from_secs(30)))
        .collect();
    let results: Vec<(&'static str, i64, Result<u16, String>)> = std::thread::scope(|sc| {
        let handles: Vec<_> = pending
            .iter()
            .map(|&(ch, v)| {
                let url = channel_url(&cfg.url, &format!("gpu0/{ch}"));
                let auth = &cfg.auth;
                sc.spawn(move || {
                    let body = json!({ "value": v }).to_string();
                    let r = agent.post(&url).header("authorization", auth).header("content-type", "application/json").send(body.as_bytes());
                    (ch, v, r.map(|r| r.status().as_u16()).map_err(|e| e.to_string()))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut errors = Vec::new();
    let mut missing = false;
    for (ch, v, r) in results {
        match r {
            Ok(c) if (200..300).contains(&c) => {
                written.insert(ch, (v, Instant::now()));
            }
            Ok(404) => missing = true,
            Ok(c) => errors.push(format!("write gpu0/{ch} → HTTP {c}")),
            Err(e) => {
                return LinkStatus { ok: false, text: format!("{} unreachable: {e}", cfg.url), ems_fps_limit: 0 };
            }
        }
    }
    if missing {
        // The bundle is there but gpu0 is not configured (an OpenEMS seeded
        // before the GPU meter existed): create it once.
        if *created_for != cfg.url {
            *created_for = cfg.url.clone();
            return match create_component(agent, cfg) {
                Ok(()) => LinkStatus { ok: false, text: "created the gpu0 meter in OpenEMS; it starts on the next cycle".into(), ems_fps_limit: 0 },
                Err(e) => LinkStatus { ok: false, text: format!("gpu0 not found and could not be created ({e}). Is the Sense Sim bundle (sensesim-rtls.jar) installed?"), ems_fps_limit: 0 },
            };
        }
        return LinkStatus { ok: false, text: "gpu0 not found in OpenEMS: install the Sense Sim bundle (assets/openems/sensesim-rtls.jar) and restart OpenEMS".into(), ems_fps_limit: 0 };
    }
    let mut ems_fps_limit = 0;
    match agent.get(channel_url(&cfg.url, "gpu0/FpsLimit")).header("authorization", &cfg.auth).call() {
        Ok(mut r) if r.status().is_success() => {
            let v: Value = serde_json::from_str(&r.body_mut().read_to_string().unwrap_or_default()).unwrap_or(Value::Null);
            ems_fps_limit = v["value"].as_f64().map(|x| x.clamp(0.0, 1000.0) as u32).unwrap_or(0);
        }
        Ok(r) => errors.push(format!("read gpu0/FpsLimit → HTTP {}", r.status().as_u16())),
        Err(e) => errors.push(format!("read gpu0/FpsLimit: {e}")),
    }
    if errors.is_empty() {
        let power = values.iter().find(|(c, _)| *c == "PowerW").map(|(_, v)| format!(", {v} W")).unwrap_or_default();
        LinkStatus { ok: true, text: format!("{} ok: {} channels on gpu0{power}", cfg.url, values.len()), ems_fps_limit }
    } else {
        let hint = if errors.iter().any(|e| e.contains("401")) { " - check the password/role" } else { "" };
        LinkStatus { ok: false, text: format!("{}{hint}", errors.join("; ")), ems_fps_limit }
    }
}

/// JSON-RPC `createComponentConfig` for `Gpu.SenseSim` / `gpu0`.
fn create_component(agent: &ureq::Agent, cfg: &LinkCfg) -> Result<(), String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "00000000-0000-4000-a000-00000000a601",
        "method": "createComponentConfig",
        "params": {
            "factoryPid": "Gpu.SenseSim",
            "properties": [
                { "name": "id", "value": "gpu0" },
                { "name": "alias", "value": "Sense Sim GPU" },
                { "name": "enabled", "value": true },
            ],
        },
    })
    .to_string();
    let url = format!("{}/jsonrpc", cfg.url.trim().trim_end_matches('/'));
    let mut r = agent
        .post(&url)
        .header("authorization", &cfg.auth)
        .header("content-type", "application/json")
        .send(body.as_bytes())
        .map_err(|e| e.to_string())?;
    let text = r.body_mut().read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !r.status().is_success() || v.get("error").is_some() {
        let msg = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {}", r.status().as_u16()));
        return Err(msg);
    }
    Ok(())
}

/// Relaunches and exits once Settings asks for it.
fn restart(mut gpu: ResMut<Gpu>, mut exit: MessageWriter<AppExit>) {
    if std::mem::take(&mut gpu.restart) {
        gpu.save();
        match relaunch() {
            Ok(()) => {
                exit.write(AppExit::Success);
            }
            Err(e) => error!("restart failed: {e}"),
        }
    }
}

/// Starts this exe again with the same arguments (the renderer can't switch
/// GPUs while running).
fn relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi() {
        let g = parse_smi("NVIDIA RTX A2000 12GB, 61.29, 70.00, 96, 40, 90, 4971, 12282, 1770, [N/A]\nNVIDIA GeForce RTX 4090, 300.5, 450.00, 80, 50, 70, 1000, 24564, 2520, 55\n");
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "NVIDIA RTX A2000 12GB");
        assert_eq!(g[0].power_w, Some(61.29));
        assert_eq!(g[0].temp_c, Some(90.0));
        assert_eq!(g[0].fan_pct, None, "[N/A] is missing, not 0");
        assert_eq!(g[1].fan_pct, Some(55.0));
    }

    #[test]
    fn channels_sum_power_and_pick_render_gpu() {
        let t = Telemetry { gpus: parse_smi("GPU A, 50, 70, 10, 1, 40, 1, 2, 3, 4\nGPU B, 100.4, 450, 90, 1, 80, 1, 2, 3, 4"), error: None };
        let cfg = LinkCfg { render_name: "GPU B".into(), ray_tracing: true, ..default() };
        let ch: HashMap<_, _> = channels(&t, &cfg, 59.6, true).into_iter().collect();
        assert_eq!(ch["PowerW"], 150);
        assert_eq!(ch["UtilizationPct"], 90);
        assert_eq!(ch["TemperatureC"], 80);
        assert_eq!(ch["Online"], 1);
        assert_eq!(ch["GpuCount"], 2);
        assert_eq!(ch["RenderFps"], 60);
        assert_eq!(ch["RayTracing"], 1);
        // No telemetry: still online=0 and the app values, no made-up power.
        let none = Telemetry { gpus: vec![], error: Some("x".into()) };
        let ch: HashMap<_, _> = channels(&none, &cfg, 30.0, false).into_iter().collect();
        assert_eq!(ch["Online"], 0);
        assert!(!ch.contains_key("PowerW"));
    }

    #[test]
    fn wgpu_settings_follow_prefs() {
        let p = GpuPrefs { backend: Backend::Vulkan, power: Power::LowPower, adapter: " RTX ".into(), ..default() };
        let s = p.wgpu_settings();
        assert_eq!(s.power_preference, PowerPreference::LowPower);
        assert_eq!(s.adapter_name.as_deref(), Some("rtx"));
        if std::env::var_os("WGPU_BACKEND").is_none() {
            assert_eq!(s.backends, Some(Backends::VULKAN));
        }
        assert_eq!(GpuPrefs::default().wgpu_settings().power_preference, PowerPreference::HighPerformance);
    }
}
