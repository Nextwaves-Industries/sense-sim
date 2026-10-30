//! ONNX Runtime, loaded at run time: the DirectML build (any DX12 GPU, CPU
//! fallback built in) is downloaded once from NuGet into
//! `%LOCALAPPDATA%/SenseSim/runtime`, so nothing native is linked and a
//! missing runtime is a reportable error, not a crash.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

const ORT_PKG: (&str, &str) = ("microsoft.ml.onnxruntime.directml", "1.24.4");
const DML_PKG: (&str, &str) = ("microsoft.ai.directml", "1.15.4");

/// Download progress shown in the assistant panel.
#[derive(Default)]
pub struct Progress {
    pub label: Mutex<String>,
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    pub fn set(&self, label: &str, total: u64) {
        *self.label.lock().unwrap() = label.to_string();
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }
    pub fn clear(&self) {
        self.label.lock().unwrap().clear();
    }
    pub fn snapshot(&self) -> Option<(String, u64, u64)> {
        let l = self.label.lock().unwrap().clone();
        (!l.is_empty()).then(|| (l, self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed)))
    }
}

pub fn progress() -> &'static Progress {
    static P: OnceLock<Progress> = OnceLock::new();
    P.get_or_init(Progress::default)
}

pub fn data_dir(sub: &str) -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    Path::new(&base).join("SenseSim").join(sub)
}

/// Streams `url` to `dest` (via `.part` + rename), reporting progress.
pub fn download(url: &str, dest: &Path, label: &str) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d)?;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(30))).build().into();
    let mut req = agent.get(url).header("User-Agent", "SenseSim");
    if url.contains("huggingface.co")
        && let Ok(t) = std::env::var("HF_TOKEN")
        && !t.trim().is_empty()
    {
        req = req.header("Authorization", format!("Bearer {}", t.trim()));
    }
    let mut resp = req.call().with_context(|| format!("GET {url}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let p = progress();
    p.set(label, total);
    let part = dest.with_extension("part");
    let mut f = std::fs::File::create(&part)?;
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut f, &buf[..n])?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    drop(f);
    p.clear();
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Extracts one file from a NuGet package (a zip).
fn extract(nupkg: &Path, entry: &str, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(nupkg)?)?;
    let mut file = zip.by_name(entry).with_context(|| format!("{entry} not in {}", nupkg.display()))?;
    let mut out = std::fs::File::create(dest.with_extension("part"))?;
    std::io::copy(&mut file, &mut out)?;
    drop(out);
    std::fs::rename(dest.with_extension("part"), dest)?;
    Ok(())
}

fn nuget(pkg: (&str, &str), entry: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let dir = dest.parent().unwrap();
    let nupkg = dir.join(format!("{}.{}.nupkg", pkg.0, pkg.1));
    let url = format!("https://api.nuget.org/v3-flatcontainer/{0}/{1}/{0}.{1}.nupkg", pkg.0, pkg.1);
    download(&url, &nupkg, &format!("GPU runtime ({})", pkg.0))?;
    extract(&nupkg, entry, dest)?;
    let _ = std::fs::remove_file(nupkg);
    Ok(())
}

pub struct Runtime {
    /// "DirectML" or "CPU".
    pub device: String,
    use_gpu: bool,
    _env: ort::environment::Environment,
}

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Downloads (first time) and initializes ONNX Runtime. Safe to call from
/// any thread; later calls return the cached result.
pub fn init(prefer_gpu: bool) -> Result<&'static Runtime> {
    let _g = INIT_LOCK.lock().unwrap();
    let r = RUNTIME.get_or_init(|| init_inner(prefer_gpu).map_err(|e| format!("{e:#}")));
    r.as_ref().map_err(|e| anyhow::anyhow!("ONNX Runtime unavailable: {e}"))
}

fn init_inner(prefer_gpu: bool) -> Result<Runtime> {
    let dir = data_dir("runtime");
    std::fs::create_dir_all(&dir)?;
    let ort_dll = dir.join("onnxruntime.dll");
    let dml_dll = dir.join("DirectML.dll");
    nuget(ORT_PKG, "runtimes/win-x64/native/onnxruntime.dll", &ort_dll)?;
    let mut use_gpu = prefer_gpu;
    if use_gpu {
        match nuget(DML_PKG, "bin/x64-win/DirectML.dll", &dml_dll) {
            // Load our DirectML.dll before onnxruntime.dll so the older copy
            // in System32 is not picked up.
            Ok(()) => {
                if ort::util::preload_dylib(dml_dll.as_os_str()).is_err() {
                    use_gpu = false;
                }
            }
            Err(_) => use_gpu = false,
        }
    }
    let env = ort::init_from(&ort_dll).map_err(|e| anyhow::anyhow!("loading {}: {e}", ort_dll.display()))?.with_name("SenseSim").build()?;
    Ok(Runtime { device: if use_gpu { "DirectML".into() } else { "CPU".into() }, use_gpu, _env: env })
}

/// Builder errors carry the builder back; keep only the message.
fn b<T>(r: std::result::Result<T, ort::Error<ort::session::builder::SessionBuilder>>) -> Result<T> {
    r.map_err(|e| anyhow::anyhow!("{}", ort::Error::<()>::from(e)))
}

/// A loaded model and where it runs.
pub struct Loaded {
    pub session: Session,
    pub device: &'static str,
}

impl Runtime {
    pub fn gpu(&self) -> bool {
        self.use_gpu
    }

    /// Loads on the GPU when available, falling back to the CPU for models
    /// DirectML can't compile.
    pub fn load(&self, model: &Path) -> Result<Loaded> {
        if self.use_gpu {
            match self.session_on(model, true) {
                Ok(session) => {
                    crate::gpu::set_vision_on_gpu(true);
                    return Ok(Loaded { session, device: "DirectML" });
                }
                Err(e) => eprintln!("[vision] {} falls back to CPU: {e:#}", model.display()),
            }
        }
        Ok(Loaded { session: self.session_on(model, false)?, device: "CPU" })
    }

    pub fn session(&self, model: &Path) -> Result<Session> {
        Ok(self.load(model)?.session)
    }

    fn session_on(&self, model: &Path, gpu: bool) -> Result<Session> {
        let sb = Session::builder(&self._env).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut sb = b(sb.with_optimization_level(GraphOptimizationLevel::Level3))?;
        if gpu {
            // DirectML requires sequential execution without memory patterns.
            sb = b(sb.with_parallel_execution(false))?;
            sb = b(sb.with_memory_pattern(false))?;
            // Device 0 is the primary display adapter, often the integrated GPU
            // on laptops: choose by preference instead (Settings → Graphics).
            use ort::ep::directml::{DeviceFilter, PerformancePreference};
            let pref = match crate::gpu::vision_device() {
                crate::gpu::VisionDevice::LowPower => PerformancePreference::MinimumPower,
                _ => PerformancePreference::HighPerformance,
            };
            let dml = ort::ep::DirectML::default().with_device_filter(DeviceFilter::Gpu).with_performance_preference(pref);
            sb = b(sb.with_execution_providers([dml.build()]))?;
        } else {
            sb = b(sb.with_intra_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)))?;
        }
        sb.commit_from_file(model).map_err(|e| anyhow::anyhow!("loading model {}: {e}", model.display()))
    }
}

/// Fetches `file` of a Hugging Face repo into the model cache.
pub fn hf_file(repo: &str, file: &str) -> Result<PathBuf> {
    let dest = data_dir("models").join(repo.replace('/', "__")).join(file);
    if !dest.exists() {
        let url = format!("https://huggingface.co/{repo}/resolve/main/{file}");
        let name = file.rsplit('/').next().unwrap_or(file);
        download(&url, &dest, &format!("model {repo} / {name}"))?;
    }
    if !dest.exists() {
        bail!("download of {repo}/{file} failed");
    }
    Ok(dest)
}
