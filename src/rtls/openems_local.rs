//! A private OpenEMS Edge managed by Sense Sim: one click downloads a
//! portable Java 21 runtime and the official `openems-edge.jar`, preseeds a
//! working configuration (scheduler, REST API, a small energy simulation and
//! the `Rtls.SenseSim` component), installs our RTLS bundle, starts OpenEMS
//! hidden and connects the simulator to it. Nothing needs admin rights; it
//! all lives in `%LOCALAPPDATA%\SenseSim\openems`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::RtlsConfig;
use super::io::{ChannelMap, Hub, OpenEmsCfg};

pub const JRE_URL: &str = "https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jre/hotspot/normal/eclipse";
pub const EDGE_URL: &str = "https://github.com/OpenEMS/openems/releases/latest/download/openems-edge.jar";
pub const REST_URL: &str = "http://127.0.0.1:8084";
pub const CONSOLE_URL: &str = "http://127.0.0.1:8080/system/console/components";
/// The OpenEMS bundle that holds the RTLS channels (`tools/openems_rtls`).
const RTLS_BUNDLE: &[u8] = include_bytes!("../../assets/openems/sensesim-rtls.jar");

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// Step description and progress 0..1 (negative = unknown).
    Working(String, f32),
    Running { attached: bool },
    Failed(String),
}

#[derive(Resource)]
pub struct LocalEms {
    pub phase: Arc<Mutex<Phase>>,
    child: Arc<Mutex<Option<Child>>>,
    /// Outputs were pointed at this instance.
    connected: bool,
    checked: bool,
    /// Reconnect to the local OpenEMS at startup (off for headless runs).
    pub auto_start: bool,
    /// Watchdog: next health check, and consecutive failed checks.
    watch: Option<Instant>,
    dead_checks: u8,
}

impl Default for LocalEms {
    fn default() -> Self {
        Self::new(true)
    }
}

pub fn base_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("SenseSim").join("openems")
}

impl LocalEms {
    pub fn new(auto_start: bool) -> Self {
        Self {
            phase: Arc::new(Mutex::new(Phase::Idle)),
            child: Arc::new(Mutex::new(None)),
            connected: false,
            checked: false,
            auto_start,
            watch: None,
            dead_checks: 0,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase.lock().unwrap().clone()
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase(), Phase::Working(..))
    }

    pub fn installed() -> bool {
        base_dir().join("openems-edge.jar").exists() && java_exe(&base_dir()).is_some()
    }

    /// Install (if needed) and start, in the background.
    pub fn start(&mut self) {
        if self.busy() {
            return;
        }
        self.connected = false;
        let (phase, child) = (self.phase.clone(), self.child.clone());
        *phase.lock().unwrap() = Phase::Working("Starting…".into(), -1.0);
        std::thread::spawn(move || {
            let set = |s: &str, p: f32| *phase.lock().unwrap() = Phase::Working(s.into(), p);
            let result = install_and_start(&base_dir(), &set, &child);
            *phase.lock().unwrap() = match result {
                Ok(attached) => Phase::Running { attached },
                Err(e) => Phase::Failed(format!("{e:#}")),
            };
        });
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
            release_lock();
        }
        *self.phase.lock().unwrap() = Phase::Idle;
        self.connected = false;
    }

    pub fn owns_process(&self) -> bool {
        self.child.lock().unwrap().is_some()
    }
}

impl Drop for LocalEms {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
        }
    }
}

fn java_exe(base: &Path) -> Option<PathBuf> {
    let jre = base.join("jre");
    std::fs::read_dir(&jre).ok()?.flatten().map(|e| e.path().join("bin").join("java.exe")).find(|p| p.exists())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(600))).http_status_as_error(false).build().into()
}

/// Something listens on the OpenEMS REST port (cheap; says nothing about readiness).
pub fn port_open() -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], 8084)), Duration::from_millis(300)).is_ok()
}

/// OpenEMS answers REST (it can take seconds when busy).
fn rest_alive() -> bool {
    let a: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).http_status_as_error(false).build().into();
    a.get(format!("{REST_URL}/rest/channel/_sum/State"))
        .header("authorization", super::io::openems::auth_header("x", "admin"))
        .call()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn download(url: &str, to: &Path, label: &str, set: &dyn Fn(&str, f32)) -> anyhow::Result<()> {
    let mut resp = agent().get(url).call()?;
    if !resp.status().is_success() {
        anyhow::bail!("{label}: HTTP {} from {url}", resp.status().as_u16());
    }
    let total: Option<u64> = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let tmp = to.with_extension("part");
    let mut out = std::fs::File::create(&tmp)?;
    let mut reader = resp.body_mut().with_config().limit(1 << 30).reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if last.elapsed() > Duration::from_millis(200) {
            last = Instant::now();
            let p = total.map(|t| done as f32 / t.max(1) as f32).unwrap_or(-1.0);
            set(&format!("{label} ({:.0} MB)", done as f32 / 1e6), p);
        }
    }
    out.flush()?;
    drop(out);
    std::fs::rename(&tmp, to)?;
    Ok(())
}

/// Felix ConfigAdmin file value.
pub enum Val<'a> {
    S(&'a str),
    B(bool),
    I(i64),
}

fn uuid() -> String {
    let mut x: u128 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    x ^= (std::process::id() as u128) << 64;
    x = x.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835);
    let h = format!("{x:032x}");
    format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
}

/// Text of a factory configuration file in Felix's format.
pub fn config_text(factory_pid: &str, pid: &str, props: &[(&str, Val)]) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut lines: Vec<String> = props
        .iter()
        .map(|(k, v)| match v {
            Val::S(s) => format!("{k}=\"{}\"", esc(s)),
            Val::B(b) => format!("{k}=B\"{b}\""),
            Val::I(i) => format!("{k}=I\"{i}\""),
        })
        .collect();
    lines.push(format!("service.factoryPid=\"{factory_pid}\""));
    lines.push(format!("service.pid=\"{pid}\""));
    lines.sort();
    format!(":org.apache.felix.configadmin.revision:=L\"1\"\n{}\n", lines.join("\n"))
}

fn write_config(config: &Path, factory_pid: &str, props: &[(&str, Val)]) -> anyhow::Result<()> {
    let id = uuid();
    let dir = factory_pid.split('.').fold(config.to_path_buf(), |p, s| p.join(s));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{id}.config")), config_text(factory_pid, &format!("{factory_pid}.{id}"), props))?;
    Ok(())
}

/// First-run configuration: scheduler, REST API, an energy simulation so the
/// EMS has live data, the RTLS component and the GPU meter.
fn preseed(config: &Path) -> anyhow::Result<()> {
    let marker = config.join(".sensesim-seeded");
    if marker.exists() {
        return seed_gpu(config);
    }
    use Val::*;
    let base = |id: &'static str| vec![("id", S(id)), ("alias", S("")), ("enabled", B(true))];
    write_config(config, "Scheduler.AllAlphabetically", &base("scheduler0"))?;
    let mut rest = base("ctrlApiRest0");
    rest.extend([("port", I(8084)), ("apiTimeout", I(60)), ("connectionlimit", I(32)), ("debugMode", B(false))]);
    write_config(config, "Controller.Api.Rest.ReadWrite", &rest)?;
    let mut rtls = base("rtls0");
    rtls[1] = ("alias", S("Sense Sim RTLS"));
    write_config(config, "Rtls.SenseSim", &rtls)?;
    let mut ds = base("datasource0");
    ds.extend([("source", S("H0_HOUSEHOLD_SUMMER_WEEKDAY_STANDARD_LOAD_PROFILE")), ("timeDelta", I(-1))]);
    write_config(config, "Simulator.Datasource.CSV.Predefined", &ds)?;
    let mut meter = base("meter0");
    meter.push(("datasource.id", S("datasource0")));
    write_config(config, "Simulator.GridMeter.Acting", &meter)?;
    let mut ess = base("ess0");
    ess.extend([("capacity", I(10000)), ("initialSoc", I(50)), ("maxApparentPower", I(10000))]);
    write_config(config, "Simulator.EssSymmetric.Reacting", &ess)?;
    seed_gpu(config)?;
    std::fs::write(marker, "configured by Sense Sim\n")?;
    Ok(())
}

/// The `Gpu.SenseSim` meter (`gpu0`), also added to installs seeded before it
/// existed. Read when OpenEMS starts; one that already runs gets it over
/// JSON-RPC instead (see `gpu::link`).
fn seed_gpu(config: &Path) -> anyhow::Result<()> {
    if config.join("Gpu").join("SenseSim").exists() {
        return Ok(());
    }
    use Val::*;
    write_config(config, "Gpu.SenseSim", &[("id", S("gpu0")), ("alias", S("Sense Sim GPU")), ("enabled", B(true))])
}

fn install_and_start(base: &Path, set: &dyn Fn(&str, f32), child: &Mutex<Option<Child>>) -> anyhow::Result<bool> {
    if rest_alive() {
        // Something (ours from before, or the user's own) already answers.
        return Ok(true);
    }
    std::fs::create_dir_all(base)?;
    // Another Sense Sim may be starting it right now: wait for that one
    // instead of launching a second OpenEMS.
    let lock = base.join("openems.lock");
    if let Ok(txt) = std::fs::read_to_string(&lock)
        && let Ok(owner) = txt.trim().parse::<u32>()
        && owner != std::process::id()
        && proc::alive(owner)
    {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(150) {
            if rest_alive() {
                return Ok(true);
            }
            set("Waiting for the OpenEMS another Sense Sim is starting", -1.0);
            std::thread::sleep(Duration::from_millis(800));
        }
        anyhow::bail!("another Sense Sim (pid {owner}) holds OpenEMS but it never answered");
    }
    // The port is taken (an OpenEMS that is busy, or the user's own): never
    // start a second one on the same ports, wait for it instead.
    if port_open() {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(60) {
            if rest_alive() {
                return Ok(true);
            }
            set("OpenEMS is starting up", -1.0);
            std::thread::sleep(Duration::from_millis(800));
        }
        anyhow::bail!("something listens on port 8084 but does not answer as OpenEMS");
    }
    std::fs::write(&lock, std::process::id().to_string())?;
    if java_exe(base).is_none() {
        let zip = base.join("jre.zip");
        download(JRE_URL, &zip, "Downloading Java 21 runtime", set)?;
        set("Unpacking Java runtime", -1.0);
        let jre = base.join("jre");
        let _ = std::fs::remove_dir_all(&jre);
        zip::ZipArchive::new(std::fs::File::open(&zip)?)?.extract(&jre)?;
        let _ = std::fs::remove_file(&zip);
    }
    let java = java_exe(base).ok_or_else(|| anyhow::anyhow!("Java runtime missing after unpacking"))?;
    let jar = base.join("openems-edge.jar");
    if !jar.exists() {
        download(EDGE_URL, &jar, "Downloading OpenEMS Edge", set)?;
    }
    set("Configuring OpenEMS", -1.0);
    let config = base.join("config");
    let data = base.join("data");
    let load = base.join("load");
    for d in [&config, &data, &load] {
        std::fs::create_dir_all(d)?;
    }
    preseed(&config)?;
    std::fs::write(load.join("sensesim-rtls.jar"), RTLS_BUNDLE)?;

    set("Starting OpenEMS", -1.0);
    let log = std::fs::File::create(base.join("sensesim-openems-console.log"))?;
    let mut cmd = Command::new(&java);
    cmd.arg(format!("-Dfelix.cm.dir={}", config.display()))
        .arg(format!("-Dopenems.data.dir={}", data.display()))
        .arg(format!("-Dfelix.fileinstall.dir={}", load.display()))
        .arg("-jar")
        .arg(&jar)
        .current_dir(base)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut c = cmd.spawn()?;
    if !proc::kill_with_us(&c) {
        bevy::log::warn!("could not tie OpenEMS to Sense Sim; it is stopped on normal exit only");
    }
    let started = Instant::now();
    loop {
        if let Some(status) = c.try_wait()? {
            let tail = std::fs::read_to_string(base.join("sensesim-openems-console.log")).unwrap_or_default();
            let tail: String = tail.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
            release_lock();
            anyhow::bail!("OpenEMS exited ({status}): {tail}");
        }
        if rest_alive() {
            break;
        }
        if started.elapsed() > Duration::from_secs(120) {
            let _ = c.kill();
            release_lock();
            anyhow::bail!("OpenEMS did not answer on {REST_URL} within 2 minutes (see {})", base.join("sensesim-openems-console.log").display());
        }
        set(&format!("Starting OpenEMS ({}s)", started.elapsed().as_secs()), -1.0);
        std::thread::sleep(Duration::from_millis(700));
    }
    *child.lock().unwrap() = Some(c);
    Ok(false)
}

/// The REST sink settings for the local instance.
pub fn local_sink() -> OpenEmsCfg {
    let m = |metric: &str, ch: &str| ChannelMap { metric: metric.into(), address: format!("rtls0/{ch}") };
    let mut write = vec![
        m("tags_total", "TagsTotal"),
        m("tags_located", "TagsLocated"),
        m("tags_lost", "TagsLost"),
        m("infra_online", "InfraOnline"),
        m("infra_total", "InfraTotal"),
        m("alarms_active", "AlarmsActive"),
        m("rmse_cm", "RmseCm"),
        m("cep95_cm", "Cep95Cm"),
        m("rfid_reads_per_s", "RfidReadsPerSec"),
    ];
    for k in 1..=8 {
        write.push(m(&format!("zone#{k}"), &format!("Zone{k}Occupancy")));
    }
    // RF analysis (see `rf`).
    for (metric, ch) in [
        ("rf_links", "RfLinks"),
        ("rf_success_pct", "RfSuccessPct"),
        ("rf_avg_dbm", "RfAvgDbm"),
        ("rf_min_dbm", "RfMinDbm"),
        ("rf_weak_links", "RfWeakLinks"),
        ("rf_nlos_pct", "RfNlosPct"),
        ("rf_blocked_links", "RfBlockedLinks"),
        ("rf_nlos_bias_cm", "RfNlosBiasCm"),
        ("rf_tags_under3", "RfTagsUnder3"),
        ("rf_anchors_weak", "RfAnchorsWeak"),
        ("rf_covered_pct", "RfCoveredPct"),
        ("rf_dead_area_m2", "RfDeadAreaM2"),
    ] {
        write.push(m(metric, ch));
    }
    for k in 1..=8 {
        for (field, ch) in [("heard", "Heard"), ("success", "SuccessPct"), ("dbm", "Dbm"), ("nlos", "NlosPct")] {
            write.push(m(&format!("anchor#{k}:{field}"), &format!("Anchor{k}{ch}")));
        }
    }
    OpenEmsCfg {
        enabled: true,
        url: REST_URL.into(),
        username: "x".into(),
        password: "admin".into(),
        period_s: 1.0,
        write,
        read: vec![
            "rtls0/TagsLocated".into(),
            "rtls0/SimRunning".into(),
            "rtls0/SimSpeedX10".into(),
            "rtls0/Actuator.*".into(),
            "_sum/State".into(),
            "_sum/EssSoc".into(),
            "_sum/GridActivePower".into(),
        ],
    }
}

/// Points the outputs at the local OpenEMS once it runs; stops it on exit.
pub fn sync(mut ems: ResMut<LocalEms>, mut cfg: ResMut<RtlsConfig>, mut hub: ResMut<Hub>, mut exit: MessageReader<AppExit>) {
    if exit.read().next().is_some() {
        ems.stop();
        return;
    }
    // At startup, notice an OpenEMS that is already running locally.
    if !ems.checked {
        ems.checked = true;
        if ems.auto_start && cfg.outputs.openems.enabled && cfg.outputs.openems.url == REST_URL && LocalEms::installed() && !ems.busy() {
            ems.start();
        }
    }
    // Watchdog: if the OpenEMS we use goes away (its owner exited, it
    // crashed), start it again; start() takes over the lock.
    if let Phase::Running { .. } = ems.phase() {
        let now = Instant::now();
        if ems.watch.is_none_or(|t| now >= t) {
            ems.watch = Some(now + Duration::from_secs(3));
            let ours_died = ems.child.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_some());
            if ours_died {
                *ems.child.lock().unwrap() = None;
                release_lock();
            }
            if ours_died || !port_open() {
                ems.dead_checks += 1;
            } else {
                ems.dead_checks = 0;
            }
            if ems.dead_checks >= 2 {
                ems.dead_checks = 0;
                bevy::log::info!("OpenEMS went away; restarting it");
                ems.start();
            }
        }
    }
    if let Phase::Running { .. } = ems.phase()
        && !ems.connected
    {
        ems.connected = true;
        cfg.outputs.openems = local_sink();
        cfg.save();
        hub.restart(&cfg.outputs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn felix_config_format() {
        let t = config_text("Controller.Api.Rest.ReadWrite", "Controller.Api.Rest.ReadWrite.abc", &[("id", Val::S("ctrlApiRest0")), ("enabled", Val::B(true)), ("port", Val::I(8084)), ("alias", Val::S("a \"b\""))]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], ":org.apache.felix.configadmin.revision:=L\"1\"");
        assert!(lines.contains(&"enabled=B\"true\""));
        assert!(lines.contains(&"port=I\"8084\""));
        assert!(lines.contains(&"alias=\"a \\\"b\\\"\""));
        assert!(lines.contains(&"service.factoryPid=\"Controller.Api.Rest.ReadWrite\""));
        // Felix sorts keys.
        let keys: Vec<&str> = lines[1..].iter().map(|l| l.split('=').next().unwrap()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn preseed_writes_once() {
        let dir = std::env::temp_dir().join(format!("sensesim-preseed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        preseed(&dir).unwrap();
        preseed(&dir).unwrap();
        let n = std::fs::read_dir(dir.join("Rtls").join("SenseSim")).unwrap().count();
        assert_eq!(n, 1, "second preseed must not duplicate");
        assert!(dir.join("Controller").join("Api").join("Rest").join("ReadWrite").exists());
        assert_eq!(std::fs::read_dir(dir.join("Gpu").join("SenseSim")).unwrap().count(), 1);
        // Installs seeded before the GPU meter existed get it added once.
        std::fs::remove_dir_all(dir.join("Gpu")).unwrap();
        preseed(&dir).unwrap();
        preseed(&dir).unwrap();
        assert_eq!(std::fs::read_dir(dir.join("Gpu").join("SenseSim")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Process helpers: tie OpenEMS to our lifetime, and check other instances.
#[cfg(windows)]
mod proc {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    /// Puts `child` in a job that Windows kills when Sense Sim exits, even on
    /// a crash. The job handle is intentionally kept open until then.
    pub fn kill_with_us(child: &std::process::Child) -> bool {
        // SAFETY: plain Win32 calls on handles we own; `info` outlives the call.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return false;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            set != 0 && AssignProcessToJobObject(job, child.as_raw_handle() as _) != 0
        }
    }

    pub fn alive(pid: u32) -> bool {
        // SAFETY: query-only handle, closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
}

#[cfg(not(windows))]
mod proc {
    pub fn kill_with_us(_: &std::process::Child) -> bool {
        false
    }
    pub fn alive(_: u32) -> bool {
        false
    }
}

/// Drops our claim on the managed OpenEMS (only if it is ours).
fn release_lock() {
    let lock = base_dir().join("openems.lock");
    if std::fs::read_to_string(&lock).is_ok_and(|t| t.trim() == std::process::id().to_string()) {
        let _ = std::fs::remove_file(lock);
    }
}
