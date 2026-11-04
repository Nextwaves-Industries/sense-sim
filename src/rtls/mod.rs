//! RTLS / IoT device simulator: UWB anchors and tags (TWR and TDoA), BLE
//! beacons and tags (RSSI), UHF RFID readers and tags (EPC Gen2). Radio
//! links are ray cast against the scene's USD geometry, positions are solved
//! the way real engines do, and everything streams to OpenEMS (REST and
//! Modbus), MQTT, webhooks, UDP or a recording in real time.

pub mod actuate;
pub mod antenna;
pub mod deploy;
pub mod device;
pub mod events;
pub mod fdtd;
pub mod flow;
pub mod io;
pub mod openems_local;
pub mod persist;
pub mod placement;
pub mod planning;
pub mod radio;
pub mod region;
pub mod rf;
pub mod rng;
pub mod sim;
pub mod smart;
pub mod smart_ui;
pub mod solver;
pub mod tag_card;
pub mod tools;
pub mod ui;
pub mod view;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use device::Profiles;
use events::Snapshot;
use io::{Hub, OutputsConfig};
use rng::Rng;
use solver::Kalman;

// ---------------------------------------------------------------------------
// Settings (persisted in %APPDATA%/SenseSim/rtls.json)

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RtlsSettings {
    pub running: bool,
    pub speed: f32,
    pub tick_hz: f32,
    pub seed: u64,
    /// Solve height too when anchors have enough vertical spread.
    pub solve_3d: bool,
    pub kalman: bool,
    /// Reject NLOS/outlier ranges when there is redundancy.
    pub robust: bool,
    pub lost_after_s: f32,
    pub snapshot_hz: f32,
    pub status_every_s: f32,
    pub show_links: bool,
    pub show_ranges: bool,
    pub show_trails: bool,
    pub show_truth: bool,
    pub show_zones: bool,
    pub show_beams: bool,
    pub show_labels: bool,
    /// Regulatory region id (`region::Region`); empty = profile values as they are.
    pub region: String,
}

impl Default for RtlsSettings {
    fn default() -> Self {
        Self {
            running: true,
            speed: 1.0,
            tick_hz: 20.0,
            seed: 1,
            solve_3d: false,
            kalman: true,
            robust: true,
            lost_after_s: 3.0,
            snapshot_hz: 1.0,
            status_every_s: 5.0,
            show_links: true,
            show_ranges: false,
            show_trails: true,
            show_truth: true,
            show_zones: true,
            show_beams: true,
            show_labels: true,
            region: String::new(),
        }
    }
}

#[derive(Resource, Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct RtlsConfig {
    pub settings: RtlsSettings,
    pub outputs: OutputsConfig,
}

impl RtlsConfig {
    pub fn path() -> PathBuf {
        let base = crate::app_paths::config_base_or_current();
        base.join("SenseSim").join("rtls.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------
// Runtime state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Range,
    Tdoa,
    Rssi,
    Rfid,
    /// BLE angle of arrival: the value is the angle off the array's boresight (°).
    Aoa,
    WifiRtt,
    WifiRssi,
}

/// One radio link of a tag during its last update.
#[derive(Clone, Debug)]
pub struct Link {
    pub other: Entity,
    pub kind: LinkKind,
    pub walls: u32,
    /// Metal / liquid in the way (no UWB first path).
    pub blocker: Option<radio::Material>,
    /// Range (m), TDoA (m), RSSI (dBm) or RFID RSSI (dBm). `None` = lost.
    pub value: Option<f32>,
    /// Received signal strength (dBm), also for lost links: UWB first-path
    /// power, BLE RSSI (expected mean when not heard), RFID tag backscatter.
    pub rssi_dbm: f32,
    /// Obstruction in typical walls along the path.
    pub wall_eq: f32,
    /// The same for the UWB first-path delay (differs once materials are calibrated).
    pub bias_eq: f32,
    pub true_distance: f32,
    pub used: bool,
}

impl Link {
    /// "LOS", "NLOS ×2", or what stops a UWB link ("blocked: metal").
    pub fn path_label(&self) -> String {
        match (self.kind, self.blocker) {
            (LinkKind::Range | LinkKind::Tdoa, Some(m)) => format!("blocked: {}", m.name()),
            _ if self.walls == 0 => "LOS".into(),
            _ => format!("NLOS ×{}", self.walls),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub id: String,
    pub tech: &'static str,
    pub truth: Vec3,
    pub raw: Option<Vec3>,
    pub est: Option<Vec3>,
    pub kf: Kalman,
    pub last_fix: f64,
    pub next_update: f64,
    pub links: Vec<Link>,
    pub trail: VecDeque<Vec3>,
    pub zones: BTreeSet<String>,
    pub lost: bool,
    pub dop: f32,
    pub residual: f32,
    pub quality: f32,
    pub used: u32,
    pub err: Option<f32>,
    pub fixes: u64,
    pub attempts: u64,
    pub reads: u64,
    pub best_reader: Option<Entity>,
    /// The height the engine is configured to solve at: where the tag stood when first seen.
    /// It doesn't follow the tag, so a tag that climbs or is lifted shows the error.
    pub plane_y: f32,
    /// Where it was at its last update, to tell a moving tag from a parked one.
    pub last_truth: Vec3,
    /// Updates spent moving and parked.
    pub moving_epochs: u64,
    pub parked_epochs: u64,
    /// RFID: chance one inventory round reads it, over the readers in range and their fades.
    pub read_chance: f32,
    /// RFID: when it was first read (s).
    pub first_read: Option<f64>,
}

impl Track {
    pub fn new(id: String, tech: &'static str, truth: Vec3) -> Self {
        Self {
            id,
            tech,
            truth,
            raw: None,
            est: None,
            kf: Kalman::new(1.5),
            last_fix: f64::NEG_INFINITY,
            next_update: 0.0,
            links: Vec::new(),
            trail: VecDeque::new(),
            zones: BTreeSet::new(),
            lost: false,
            dop: 0.0,
            residual: 0.0,
            quality: 0.0,
            used: 0,
            err: None,
            fixes: 0,
            attempts: 0,
            reads: 0,
            best_reader: None,
            plane_y: truth.y,
            last_truth: truth,
            moving_epochs: 0,
            parked_epochs: 0,
            read_chance: 0.0,
            first_read: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActiveAlarm {
    pub kind: String,
    pub source: String,
    pub message: String,
    pub since: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SeriesPoint {
    pub t: f64,
    pub rmse: f32,
    pub located: u32,
    pub reads: f32,
}

/// Position errors over a set of fixes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ErrorStats {
    pub rmse: f32,
    pub cep50: f32,
    pub cep95: f32,
    pub max: f32,
    pub n: u64,
}

/// How long `RtlsState::errors` looks back (s). What a live view shows; a run's result is
/// in `RtlsState::run`.
pub const ERROR_WINDOW_S: f64 = 30.0;

/// Position errors of one technology over a whole run, kept in bins 1 % wide, so an hour of
/// fixes takes no more room than a minute.
#[derive(Clone, Debug, Default)]
pub struct RunStats {
    /// Updates tried, with or without a fix.
    pub attempts: u64,
    pub fixes: u64,
    /// Solutions thrown out for landing nowhere near the infrastructure.
    pub rejected: u64,
    sum_sq: f64,
    max: f32,
    bins: BTreeMap<u16, u64>,
}

impl RunStats {
    const FIRST_M: f32 = 0.001;
    const STEP: f32 = 1.01;

    pub fn add(&mut self, e: f32) {
        self.fixes += 1;
        self.sum_sq += (e as f64).powi(2);
        self.max = self.max.max(e);
        let bin = ((e / Self::FIRST_M).max(1.0).ln() / Self::STEP.ln()) as u16;
        *self.bins.entry(bin).or_default() += 1;
    }

    pub fn merge(&mut self, o: &Self) {
        self.attempts += o.attempts;
        self.fixes += o.fixes;
        self.rejected += o.rejected;
        self.sum_sq += o.sum_sq;
        self.max = self.max.max(o.max);
        for (b, n) in &o.bins {
            *self.bins.entry(*b).or_default() += n;
        }
    }

    /// The `rank`th smallest error, counting from 1.
    fn ranked(&self, rank: u64) -> f32 {
        let mut seen = 0;
        for (b, n) in &self.bins {
            seen += n;
            if seen >= rank {
                return (Self::FIRST_M * Self::STEP.powi(*b as i32 + 1)).min(self.max);
            }
        }
        self.max
    }

    /// `None`: no fixes, so nothing is known.
    pub fn stats(&self) -> Option<ErrorStats> {
        (self.fixes > 0).then(|| ErrorStats {
            rmse: (self.sum_sq / self.fixes as f64).sqrt() as f32,
            cep50: self.ranked((self.fixes as f64 * 0.5).ceil() as u64),
            cep95: self.ranked((self.fixes as f64 * 0.95).ceil() as u64),
            max: self.max,
            n: self.fixes,
        })
    }

    /// Share of the updates that got a fix.
    pub fn availability(&self) -> Option<f32> {
        (self.attempts > 0).then(|| self.fixes as f32 / self.attempts as f32)
    }

    /// How far 95 % of the updates land, one without a fix counting as infinitely far:
    /// infinite when more than 5 % of them had none, `None` if none were tried.
    pub fn p95_counting_misses(&self) -> Option<f32> {
        (self.attempts > 0).then(|| {
            let rank = (self.attempts as f64 * 0.95).ceil() as u64;
            if rank > self.fixes { f32::INFINITY } else { self.ranked(rank) }
        })
    }
}

/// One link's own random stream, and what stays with it while nothing moves. A stream per
/// link means adding a device, or a different device order, changes no other link's draws.
#[derive(Clone, Debug)]
pub struct LinkState {
    pub rng: Rng,
    pub held: radio::LinkDraw,
    ends: (Vec3, Vec3),
    wall_eq: f32,
}

/// The state of a link (or a tag's own stream, keyed by itself), drawing what it holds again
/// if an end has moved by `hold_m` or what's in the way has changed.
pub fn link_state<'a>(links: &'a mut HashMap<(Entity, Entity), LinkState>, seed: u64, key: (Entity, Entity), ids: (&str, &str), ends: (Vec3, Vec3), wall_eq: f32, hold_m: f32) -> &'a mut LinkState {
    let l = links.entry(key).or_insert_with(|| {
        let mut h = 0xcbf2_9ce4_8422_2325u64 ^ seed;
        for b in ids.0.bytes().chain([0]).chain(ids.1.bytes()) {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        let mut rng = Rng::new(h);
        let held = radio::LinkDraw::new(&mut rng);
        LinkState { rng, held, ends, wall_eq }
    });
    if l.ends.0.distance(ends.0) > hold_m || l.ends.1.distance(ends.1) > hold_m || l.wall_eq != wall_eq {
        l.held = radio::LinkDraw::new(&mut l.rng);
        l.ends = ends;
        l.wall_eq = wall_eq;
    }
    l
}

#[derive(Resource)]
pub struct RtlsState {
    pub time: f64,
    pub accum: f64,
    pub rng: Rng,
    /// What this run was started with.
    pub seed: u64,
    pub links: HashMap<(Entity, Entity), LinkState>,
    pub tracks: HashMap<Entity, Track>,
    /// (time, error m, tech) of the fixes of the last `ERROR_WINDOW_S`, for live RMSE/CEP.
    pub errors: VecDeque<(f64, f32, &'static str)>,
    /// The whole run, by technology and by whether the tag was moving.
    pub run: BTreeMap<(&'static str, bool), RunStats>,
    /// TDoA anchors without a clock-sync path to the others.
    pub unsynced_anchors: usize,
    /// Where those errors happened (true position, error, technology), for
    /// finding the places a plan works badly.
    pub error_spots: VecDeque<(Vec3, f32, &'static str)>,
    pub series: VecDeque<SeriesPoint>,
    pub log: VecDeque<(f64, String)>,
    pub alarms: BTreeMap<String, ActiveAlarm>,
    pub rfid_reads: VecDeque<(f64, u32)>,
    pub infra_links: HashMap<Entity, u32>,
    pub next_snapshot: f64,
    pub next_status: f64,
    pub snapshot: Snapshot,
    pub events_total: u64,
    pub zone_occupancy: BTreeMap<String, u32>,
    /// OpenEMS actuators 1..8 (0..100 %), see `actuate`.
    pub actuators: [Option<f32>; 8],
    /// Last coverage scan: (covered share 0..1, dead area m²), see `rf`.
    pub rf_coverage: Option<(f32, f32)>,
    /// Average cost of one simulation tick (ms), for diagnostics.
    pub tick_ms: f64,
}

impl Default for RtlsState {
    fn default() -> Self {
        Self::new(1)
    }
}

impl RtlsState {
    pub fn new(seed: u64) -> Self {
        Self {
            time: 0.0,
            accum: 0.0,
            rng: Rng::new(seed),
            seed,
            links: HashMap::new(),
            tracks: HashMap::new(),
            errors: VecDeque::new(),
            run: BTreeMap::new(),
            unsynced_anchors: 0,
            error_spots: VecDeque::new(),
            series: VecDeque::new(),
            log: VecDeque::new(),
            alarms: BTreeMap::new(),
            rfid_reads: VecDeque::new(),
            infra_links: HashMap::new(),
            next_snapshot: 0.0,
            next_status: 0.0,
            snapshot: Snapshot::default(),
            events_total: 0,
            zone_occupancy: BTreeMap::new(),
            actuators: [None; 8],
            rf_coverage: None,
            tick_ms: 0.0,
        }
    }

    pub fn log(&mut self, s: impl Into<String>) {
        self.log.push_back((self.time, s.into()));
        while self.log.len() > 300 {
            self.log.pop_front();
        }
    }

    /// Clears statistics (keeps devices and time).
    pub fn reset_metrics(&mut self) {
        self.errors.clear();
        self.run.clear();
        self.error_spots.clear();
        self.series.clear();
        self.rfid_reads.clear();
        for t in self.tracks.values_mut() {
            t.fixes = 0;
            t.attempts = 0;
            t.reads = 0;
            t.moving_epochs = 0;
            t.parked_epochs = 0;
            t.first_read = None;
            t.trail.clear();
        }
    }

    /// Full restart: time zero, fresh RNG, no tracks.
    pub fn restart(&mut self, seed: u64) {
        // Actuators mirror OpenEMS, not the simulation: keep them.
        let actuators = self.actuators;
        let rf_coverage = self.rf_coverage;
        *self = Self::new(seed);
        self.actuators = actuators;
        // Coverage depends on the layout, not on the run.
        self.rf_coverage = rf_coverage;
    }

    /// The fixes of the last `ERROR_WINDOW_S`, `None` when there are none: no data isn't a
    /// perfect result.
    pub fn error_stats_checked(&self, tech: Option<&str>) -> Option<ErrorStats> {
        let mut e: Vec<f32> = self.errors.iter().filter(|(_, _, t)| tech.is_none_or(|x| x == *t)).map(|(_, e, _)| *e).collect();
        if e.is_empty() {
            return None;
        }
        e.sort_by(f32::total_cmp);
        let rmse = (e.iter().map(|x| x * x).sum::<f32>() / e.len() as f32).sqrt();
        let pct = |p: f64| e[(((e.len() as f64) * p).ceil() as usize).clamp(1, e.len()) - 1];
        Some(ErrorStats { rmse, cep50: pct(0.5), cep95: pct(0.95), max: *e.last().unwrap(), n: e.len() as u64 })
    }

    /// The whole run so far, for one technology (or all) and motion class (or both).
    pub fn run_stats(&self, tech: Option<&str>, moving: Option<bool>) -> RunStats {
        let mut out = RunStats::default();
        for ((t, m), s) in &self.run {
            if tech.is_none_or(|x| x == *t) && moving.is_none_or(|x| x == *m) {
                out.merge(s);
            }
        }
        out
    }

    pub fn rfid_rate(&self) -> f32 {
        let window = 5.0;
        let n: u32 = self.rfid_reads.iter().filter(|(t, _)| *t >= self.time - window).map(|(_, n)| n).sum();
        n as f32 / window.min(self.time.max(0.1)) as f32
    }
}

/// Mode for clicking in the viewport.
#[derive(Resource, Default)]
pub struct DevicePlace {
    /// Placing devices of this profile id.
    pub profile: Option<String>,
    pub sticky: bool,
    /// Adding path waypoints to this tag by clicking the floor.
    pub waypoints_for: Option<Entity>,
    pub preview: Option<Vec3>,
}

#[derive(Resource, Default)]
pub struct RtlsSelection {
    pub device: Option<Entity>,
    pub zone: Option<Entity>,
    pub hovered: Option<Entity>,
}

// ---------------------------------------------------------------------------
// Plugin

pub struct RtlsPlugin {
    pub autotest: Option<sim::AutoTest>,
    /// Screenshot / test runs: never start OpenEMS on their own.
    pub headless: bool,
}

impl Plugin for RtlsPlugin {
    fn build(&self, app: &mut App) {
        let cfg = RtlsConfig::load();
        let mut hub = Hub::default();
        hub.apply(&cfg.outputs);
        let mut profiles = Profiles::load();
        if let Err(e) = profiles.set_region(&cfg.settings.region) {
            warn!("RTLS region: {e}");
        }
        app.insert_resource(RtlsState::new(cfg.settings.seed))
            .insert_resource(cfg)
            .insert_resource(hub)
            .insert_resource(profiles)
            .init_resource::<DevicePlace>()
            .init_resource::<RtlsSelection>()
            .init_resource::<planning::Coverage>()
            .init_resource::<planning::AppliedAntenna>()
            .insert_resource(antenna::AntennaLibrary::load())
            .init_resource::<persist::LayoutHook>()
            .init_resource::<view::DeviceHandles>()
            .init_resource::<ui::PlanJobs>()
            .init_resource::<ui::DeployState>()
            .init_resource::<ui::RtlsUiState>()
            .init_resource::<ui::ObjectNames>()
            .init_resource::<actuate::EmsBindings>()
            .insert_resource(openems_local::LocalEms::new(!self.headless))
            .init_resource::<tag_card::TagHistory>()
            .init_resource::<rf::RfHistory>()
            .init_resource::<fdtd::Fdtd>()
            .add_systems(Update, (fdtd::ui::track_selection, fdtd::collect_region, fdtd::collect_scene, fdtd::fit_scene, fdtd::sync_overlay))
            .add_systems(Startup, view::setup_assets)
            .add_systems(
                Update,
                (
                    view::device_pointer.before(crate::scene::interact).after(crate::scene::orbit_camera),
                    view::sync_selection.after(crate::scene::interact),
                    view::ensure_visuals,
                    view::raycast_backfaces,
                    persist::peek_actions.before(crate::scene::apply_actions),
                    persist::after_actions.after(crate::scene::apply_actions),
                    sim::drive.after(crate::scene::apply_actions),
                    tag_card::sample.after(sim::drive),
                    (rf::sample.after(sim::drive), rf::sync_coverage),
                    (planning::refresh_on_change, planning::sync_heatmap).chain(),
                    ui::run_plan_jobs,
                    ui::run_deploy_jobs,
                    ui::refresh_object_names,
                    actuate::apply.after(crate::scene::apply_actions).before(crate::scene::apply_joints),
                ),
            )
            .add_systems(Last, openems_local::sync)
            .add_systems(PostUpdate, view::draw.after(TransformSystems::Propagate))
            .add_systems(bevy_egui::EguiPrimaryContextPass, ui::rtls_ui.after(crate::ui::ui_system));
        app.init_resource::<smart::SmartPlan>().add_systems(Update, smart::run_jobs);
        if let Some(t) = &self.autotest {
            app.insert_resource(t.clone()).add_systems(Update, sim::autotest.after(sim::drive));
        }
    }
}
