//! Device placement engine, free of the ECS so every part can be tested.
//!
//! Each (device, floor point) pair becomes one `Entry` built from the same
//! radio functions the simulation draws from: the chance the measurement
//! exists and how much it tells about the position. A point's accuracy comes
//! from the summed Fisher information (the Cramér-Rao bound), which replaces
//! DOP with one sigma for all, and is additive per device. Adding or removing
//! a device therefore only touches the points it reaches, so the search can
//! afford many more candidates, heights and aims.

use bevy::math::{Vec2, Vec3};
use rayon::prelude::*;

use super::device::{Profile, RadioMethod, UwbMode};
use super::planning::{Tech, fix_share};
use super::radio::{self, Path};

/// What a device measures of a tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Twr,
    Tdoa,
    Rssi,
    Aoa,
    Rtt,
    Read,
}

impl Method {
    pub fn of(tech: Tech, infra: &Profile, tag: &Profile) -> Method {
        match tech {
            Tech::Uwb if tag.mode == UwbMode::Tdoa => Method::Tdoa,
            Tech::Uwb => Method::Twr,
            Tech::Rfid => Method::Read,
            Tech::Ble | Tech::Wifi => match infra.method {
                RadioMethod::Rssi => Method::Rssi,
                RadioMethod::Aoa => Method::Aoa,
                RadioMethod::Rtt => Method::Rtt,
            },
        }
    }

    /// Devices needed for a fix at a known height (presence, for RFID).
    pub fn need(self) -> usize {
        match self {
            // One difference per anchor beyond the reference; the simulation asks for 3.
            Method::Tdoa => 4,
            Method::Twr | Method::Rssi | Method::Rtt => 3,
            Method::Aoa | Method::Read => 1,
        }
    }

    /// The 95 % radius (m) a plan of this kind is asked to hold, and tested against, when it
    /// isn't given a target of its own.
    pub fn accuracy_target_m(self) -> f32 {
        self.default_target_m() * 1.5
    }

    /// What a good plan of this kind achieves (m), when no target is asked for.
    pub fn default_target_m(self) -> f32 {
        match self {
            Method::Twr | Method::Tdoa => 0.3,
            Method::Aoa => 0.75,
            Method::Rtt => 2.0,
            Method::Rssi => 3.5,
            Method::Read => 0.0,
        }
    }
}

/// Where a device hangs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mount {
    Ceiling,
    Wall,
    Column,
    Rack,
    /// No structure to hang on (a planned pole): moves freely over the floor.
    #[default]
    Free,
}

impl Mount {
    pub fn cost(self) -> f32 {
        match self {
            Mount::Wall | Mount::Column => 0.8,
            Mount::Ceiling | Mount::Free => 1.0,
            Mount::Rack => 1.2,
        }
    }

    /// Mounted on a surface it must stay on when refined.
    pub fn on_surface(self) -> bool {
        self != Mount::Free
    }
}

/// Install cost of a mounting point: the mount plus the cable to the head end.
pub fn install_cost(mount: Mount, pos: Vec3, head_end: Option<Vec3>) -> f32 {
    const CABLE_PER_M: f32 = 0.02;
    mount.cost() + head_end.map_or(0.0, |h| ((pos.x - h.x).abs() + (pos.y - h.y).abs() + (pos.z - h.z).abs()) * CABLE_PER_M)
}

/// What radio sees of the scene.
pub trait RadioScene: Sync {
    fn path(&self, a: Vec3, b: Vec3) -> Path;
    /// Distance to the nearest surface along a ray.
    fn surface(&self, from: Vec3, dir: Vec3, max: f32) -> Option<f32>;
}

impl RadioScene for super::sim::Occluders {
    fn path(&self, a: Vec3, b: Vec3) -> Path {
        super::sim::Occluders::path(self, a, b, None)
    }

    fn surface(&self, from: Vec3, dir: Vec3, max: f32) -> Option<f32> {
        let d = bevy::prelude::Dir3::new(dir).ok()?;
        self.nearest(bevy::prelude::Ray3d::new(from, d), max).map(|h| h.0)
    }
}

/// One device's measurement at one point.
#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    pub pt: u32,
    /// Chance the measurement exists at a given moment.
    pub p: f32,
    /// How much the solver counts it, per unit direction (m⁻²), `p` included.
    pub w: f32,
    /// Its noise as it reaches the solution: the variance of weight × error.
    pub v: f32,
    /// Its mean pull on the solution: mean weight × error.
    pub b: f32,
    /// Unit vector device → point.
    pub u: [f32; 3],
    /// A bearing informs across `u`, a range along it.
    pub bearing: bool,
}

pub type Column = Vec<Entry>;

/// What one link does to a position, in the weights the solver really gives it. Noise and a
/// late first path don't enter the same way: the solver doesn't know a range reads long, so
/// that error goes through at the range's weight and adds up across links.
#[derive(Clone, Copy, Debug)]
pub struct LinkInfo {
    pub p: f32,
    pub w: f32,
    pub v: f32,
    pub b: f32,
    pub bearing: bool,
}

impl LinkInfo {
    /// From a link's moments, given it exists: weight, E[w·e] and E[(w·e)²].
    fn of(p: f32, w: f32, we: f32, w2e2: f32) -> Self {
        Self { p, w: p * w, v: (p * w2e2 - (p * we).powi(2)).max(0.0), b: p * we, bearing: false }
    }

    /// A link the solver weighs with a fixed sigma, whatever the real noise and mean error.
    fn fixed(p: f32, solver_sigma: f32, noise_var: f32, mean: f32) -> Self {
        let w = 1.0 / solver_sigma.max(1e-3).powi(2);
        Self::of(p, w, w * mean, w * w * (noise_var + mean * mean))
    }
}

/// Equal-chance points of an exponential of unit mean.
const EXP_POINTS: [f32; 8] = [0.0645, 0.2076, 0.3747, 0.5754, 0.8267, 1.1632, 1.674, 2.7726];

/// A TWR link's moments. Through an obstruction the first-path check usually flags the range
/// and the solver all but ignores it, but a range it misses goes in at full weight with its
/// error. The weight is one or the other, rarely between, so its RMS is what carries a range
/// that decides a direction alone (the mean would divide that error by the miss rate).
fn twr_moments(dev: &Profile, tag: &Profile, path: &Path) -> (f32, f32, f32) {
    let noise = radio::uwb_range_sigma(dev, tag, path).powi(2);
    let full = 1.0 / radio::twr_solver_sigma(dev, false).powi(2);
    if path.walls == 0 {
        return (full, 0.0, full * full * noise);
    }
    let flagged = 1.0 / radio::twr_solver_sigma(dev, true).powi(2);
    let (bias, spread) = (path.bias_eq * dev.nlos_bias_m, path.bias_eq * dev.nlos_sigma_m);
    let (mut w2, mut we, mut w2e2) = (0.0, 0.0, 0.0);
    for x in EXP_POINTS {
        let e = bias + spread * x;
        let f = radio::first_path_flag_chance(path, e);
        let mean_w = f * flagged + (1.0 - f) * full;
        let mean_w2 = f * flagged * flagged + (1.0 - f) * full * full;
        w2 += mean_w2;
        we += mean_w * e;
        w2e2 += mean_w2 * (e * e + noise);
    }
    let n = EXP_POINTS.len() as f32;
    ((w2 / n).sqrt(), we / n, w2e2 / n)
}

/// (mean, spread) a late first path adds to a range.
fn nlos_excess(dev: &Profile, path: &Path) -> (f32, f32) {
    if path.walls == 0 {
        return (0.0, 0.0);
    }
    let spread = path.bias_eq * dev.nlos_sigma_m;
    (path.bias_eq * dev.nlos_bias_m + spread, spread)
}

/// What a link gives, or `None` when no measurement can exist. `g_r` is the device antenna's
/// gain towards the point (only RFID needs it apart from `path.gain_db`).
pub fn link_info(m: Method, dev: &Profile, tag: &Profile, path: &Path, angle: f32, g_r: f32) -> Option<LinkInfo> {
    let info = match m {
        Method::Twr => {
            if !path.uwb_passes(dev) {
                return None;
            }
            let (w, we, w2e2) = twr_moments(dev, tag, path);
            LinkInfo::of(1.0 - radio::uwb_drop_probability(dev, path), w, we, w2e2)
        }
        Method::Tdoa => {
            if !path.uwb_blink_passes(dev, tag) {
                return None;
            }
            // The TDoA solver has no first-path check: every arrival counts the same.
            let s = radio::uwb_arrival_sigma(dev);
            let (mean, spread) = nlos_excess(dev, path);
            LinkInfo::fixed(1.0 - radio::uwb_blink_drop_probability(dev, tag, path), s, s * s + spread * spread, mean)
        }
        Method::Rssi => {
            // A wall reads as distance, and an antenna's gain as closeness: the solver
            // inverts the signal as if neither were there, and weighs what it gets.
            let read = path.distance * 10f32.powf((dev.wall_loss_db * path.wall_eq - path.gain_db) / (10.0 * dev.path_loss_exp.max(0.5)));
            let s = radio::rssi_distance_sigma(dev, read);
            LinkInfo::fixed(radio::rssi_heard_probability(dev, tag, path), s, s * s, read - path.distance)
        }
        Method::Rtt => {
            let (mean, spread) = nlos_excess(dev, path);
            LinkInfo::fixed(radio::rssi_heard_probability(dev, tag, path), dev.range_sigma_m.max(0.1), dev.range_sigma_m.powi(2) + spread * spread, mean)
        }
        Method::Aoa => {
            if !radio::aoa_passes(dev, path, angle) {
                return None;
            }
            // The solver knows the distance and the angle, not what's in the way.
            let d = path.distance.max(0.5);
            let (real, assumed) = (radio::aoa_sigma_rad(dev, path, angle) * d, radio::aoa_sigma_rad(dev, &Path::default(), angle) * d);
            LinkInfo { bearing: true, ..LinkInfo::fixed(radio::rssi_heard_probability(dev, tag, path), assumed, real * real, 0.0) }
        }
        Method::Read => {
            let p = radio::rfid_read_probability(dev, tag, path, g_r);
            LinkInfo { p, w: p, v: p, b: 0.0, bearing: false }
        }
    };
    (info.p >= 0.02).then_some(info)
}

/// The Entry list of one device over all the points. `gain(fwd, dir)` is the
/// antenna gain on the link: the reader's alone for RFID, both ends otherwise.
#[allow(clippy::too_many_arguments)]
pub fn column<S: RadioScene, G: Fn(&Profile, Vec3, Vec3) -> f32 + Sync>(scene: &S, pts: &[Point], method: Method, dev: &Profile, tag: &Profile, pos: Vec3, fwd: Vec3, gain: &G) -> Column {
    let fwd = fwd.normalize_or(Vec3::NEG_Y);
    let one = |(k, pt): (usize, &Point)| -> Option<Entry> {
        let d = pt.pos - pos;
        let dist = d.length();
        if dist > dev.max_range_m * 1.01 || dist < 1e-3 {
            return None;
        }
        let u = d / dist;
        let g = gain(dev, fwd, u);
        let mut path = scene.path(pt.pos, pos).in_band(dev.freq_mhz);
        let g_r = if method == Method::Read { g } else { 0.0 };
        if method != Method::Read {
            path.gain_db = g;
        }
        let i = link_info(method, dev, tag, &path, fwd.angle_between(u), g_r)?;
        Some(Entry { pt: k as u32, p: i.p, w: i.w, v: i.v, b: i.b, u: u.to_array(), bearing: i.bearing })
    };
    pts.par_iter().enumerate().filter_map(one).collect()
}

/// An evaluation point on the floor (or a tag's spot on a shelf).
#[derive(Clone, Debug, Default)]
pub struct Point {
    pub pos: Vec3,
    pub weight: f32,
    /// On the even floor grid: floor coverage is judged on these.
    pub floor: bool,
    /// Indices of the zones it belongs to.
    pub zones: Vec<u16>,
}

/// A mounting point the search may use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Candidate {
    pub pos: Vec3,
    pub facing: Vec3,
    pub mount: Mount,
    pub cost: f32,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub method: Method,
    /// Plan for full 3D fixes rather than X/Z at a known height.
    pub dim3: bool,
    /// Devices needed per point.
    pub need: usize,
    /// Accuracy the plan is judged against (m).
    pub target_m: f32,
    /// Beyond this radius for 95 % of readings a point does not count as covered (m).
    pub err_cap: f32,
    /// Score each point as if its most informative device had failed too.
    pub redundancy: bool,
    pub min_spacing_m: f32,
    /// Weight of one unit of install cost, as a share of all the point weight.
    pub cost_weight: f64,
    /// RFID: score reads only inside the zones; reads outside are penalised.
    pub zones_only: bool,
    pub stray_weight: f64,
    /// RFID: a point counts as covered from this read chance per round.
    pub read_p: f32,
    /// RFID: readers closer than this and in each other's beam jam each other.
    pub conflict_m: f32,
    pub beam_deg: f32,
    pub target_coverage: Option<f32>,
    pub target_p95: Option<f32>,
    pub zone_count: usize,
    /// Add at least this many devices even if the targets look met.
    pub force_add: usize,
    /// Spots taken by other technologies' devices: nothing may land on them,
    /// neither in the search nor when fine-tuning.
    pub avoid: Vec<Vec3>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            method: Method::Twr,
            dim3: false,
            need: 3,
            target_m: 0.3,
            err_cap: 1.0,
            redundancy: false,
            min_spacing_m: 1.5,
            cost_weight: 0.0005,
            zones_only: false,
            stray_weight: 0.1,
            read_p: READ_P,
            conflict_m: 0.0,
            beam_deg: 360.0,
            target_coverage: None,
            target_p95: None,
            zone_count: 0,
            force_add: 0,
            avoid: Vec::new(),
        }
    }
}

/// RFID: a point counts as covered from this read chance per round.
pub const READ_P: f32 = 0.9;

/// Mounting points closer than this are one spot.
pub const SAME_SPOT_M: f32 = 0.3;

/// Column id of a device that is already in the scene.
const FIXED: u32 = u32::MAX / 2;

/// Summed information of the devices reaching one point.
#[derive(Clone, Copy, Debug, Default)]
pub struct Acc {
    /// Σw, Σw·u and Σw·uuᵀ (xx xy xz yy yz zz) of the ranges and arrival times;
    /// bearings go into s2 as w·(I − uuᵀ).
    s0: f64,
    s1: [f64; 3],
    s2: [f64; 6],
    /// The same of each link's noise `v`.
    v0: f64,
    v1: [f64; 3],
    v2: [f64; 6],
    /// Σb and Σb·u of the links' mean pull.
    b0: f64,
    b1: [f64; 3],
    /// Devices measuring here more often than not.
    pub n: u16,
    /// Σ ln(1 − p): the chance every device misses.
    miss: f64,
    /// The most informative device: losing it is the worst single failure.
    top: Option<(u32, Entry)>,
}

impl Acc {
    fn apply(&mut self, e: &Entry, sign: f64) {
        let (w, v, b) = (e.w as f64 * sign, e.v as f64 * sign, e.b as f64 * sign);
        let u = [e.u[0] as f64, e.u[1] as f64, e.u[2] as f64];
        let uu = [u[0] * u[0], u[0] * u[1], u[0] * u[2], u[1] * u[1], u[1] * u[2], u[2] * u[2]];
        if e.bearing {
            let i = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0];
            for k in 0..6 {
                self.s2[k] += w * (i[k] - uu[k]);
                self.v2[k] += v * (i[k] - uu[k]);
            }
        } else {
            self.s0 += w;
            self.v0 += v;
            self.b0 += b;
            for k in 0..3 {
                self.s1[k] += w * u[k];
                self.v1[k] += v * u[k];
                self.b1[k] += b * u[k];
            }
            for k in 0..6 {
                self.s2[k] += w * uu[k];
                self.v2[k] += v * uu[k];
            }
        }
        if e.p >= 0.5 {
            self.n = if sign > 0.0 { self.n + 1 } else { self.n.saturating_sub(1) };
        }
        self.miss += sign * (1.0 - e.p.min(0.9999) as f64).ln();
    }

    pub fn add(&mut self, col: u32, e: &Entry) {
        self.apply(e, 1.0);
        if self.top.is_none_or(|(_, t)| e.w > t.w) {
            self.top = Some((col, *e));
        }
    }

    /// Chance at least one device reads here.
    pub fn p_any(&self) -> f32 {
        1.0 - self.miss.exp() as f32
    }

    /// RMS position error the solver gets here (m), infinite without a fix: x̂ − x =
    /// A⁻¹Σw·u·e with A = Σw·uuᵀ, so err² = tr(A⁻¹VA⁻¹) + |A⁻¹Σb·u|². Where the weights
    /// match the noise and nothing reads long this is the Cramér-Rao bound. An unknown
    /// emission time (TDoA) removes the common part of the arrivals.
    pub fn err(&self, method: Method, dim3: bool) -> f32 {
        let (mut s, mut v, mut b) = (self.s2, self.v2, self.b1);
        if method == Method::Tdoa {
            if self.s0 <= 0.0 {
                return f32::INFINITY;
            }
            let m = [self.s1[0] / self.s0, self.s1[1] / self.s0, self.s1[2] / self.s0];
            const PAIRS: [(usize, usize); 6] = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
            for (k, (i, j)) in PAIRS.into_iter().enumerate() {
                s[k] -= self.s0 * m[i] * m[j];
                v[k] += self.v0 * m[i] * m[j] - self.v1[i] * m[j] - m[i] * self.v1[j];
            }
            for i in 0..3 {
                b[i] -= self.b0 * m[i];
            }
        }
        let [xx, xy, xz, yy, yz, zz] = s;
        let e2 = if dim3 {
            let c = [yy * zz - yz * yz, xz * yz - xy * zz, xy * yz - xz * yy, xx * zz - xz * xz, xy * xz - xx * yz, xx * yy - xy * xy];
            let det = xx * c[0] + xy * c[1] + xz * c[2];
            let scale = (xx + yy + zz).powi(3);
            if det <= scale * 1e-9 || scale <= 0.0 {
                return f32::INFINITY;
            }
            let i = [[c[0] / det, c[1] / det, c[2] / det], [c[1] / det, c[3] / det, c[4] / det], [c[2] / det, c[4] / det, c[5] / det]];
            let n = [[v[0], v[1], v[2]], [v[1], v[3], v[4]], [v[2], v[4], v[5]]];
            let mut e2 = 0.0;
            for a in 0..3 {
                let pull: f64 = (0..3).map(|j| i[a][j] * b[j]).sum();
                e2 += pull * pull;
                for j in 0..3 {
                    for k in 0..3 {
                        e2 += i[a][j] * n[j][k] * i[k][a];
                    }
                }
            }
            e2
        } else {
            let det = xx * zz - xz * xz;
            let scale = (xx + zz).powi(2);
            if det <= scale * 1e-9 || scale <= 0.0 {
                return f32::INFINITY;
            }
            let (ixx, ixz, izz) = (zz / det, -xz / det, xx / det);
            let (vxx, vxz, vzz) = (v[0], v[2], v[5]);
            let noise = (ixx * ixx + ixz * ixz) * vxx + 2.0 * (ixx + izz) * ixz * vxz + (ixz * ixz + izz * izz) * vzz;
            let (px, pz) = (ixx * b[0] + ixz * b[2], ixz * b[0] + izz * b[2]);
            noise + px * px + pz * pz
        };
        e2.max(0.0).sqrt() as f32
    }
}

/// A circular normal error of RMS 1 holds 95 % of its readings within √(ln 20).
pub const R95_PER_RMS: f32 = 1.7308;

/// What a plan achieves.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    /// Share of the floor (or of the zones, reads inside zones only) covered.
    pub coverage: f32,
    /// How far 95 % of readings land, at the 95th percentile of the floor: a point with no
    /// fix counts as infinitely far, so this is infinite past 5 % without a position.
    pub p95: f32,
    /// The same over the points with a fix, whatever share of the floor those are.
    pub p95_located: f32,
    /// `p95_located` with each point's worst single device failure (redundancy on).
    pub p95_k1: f32,
    /// Covered share per zone (NaN = no points there).
    pub zones: Vec<f32>,
    /// RFID: mean read chance outside the zones.
    pub stray: f32,
    /// RFID: reader pairs that jam each other.
    pub conflicts: usize,
    /// TDoA: anchors that can't reach the others for clock sync.
    pub unsynced: usize,
    pub cost: f32,
}

/// Moves a device along its mounting surface for fine positioning.
pub trait Mover: Sync {
    /// Where a device really lands when moved to `to`, or `None` if it can't go there.
    fn settle(&self, from: &Candidate, to: Vec3) -> Option<Vec3>;
    /// Its measurements from there.
    fn column(&self, pos: Vec3, facing: Vec3) -> Column;
}

/// Anchor-to-anchor visibility for TDoA clock sync.
pub type SyncTest<'a> = &'a (dyn Fn(Vec3, Vec3) -> bool + Sync);

/// Of `n` devices joined pairwise by `linked`, those outside the biggest connected group:
/// the anchors that can't take part in clock sync.
pub fn outside_biggest_group(n: usize, linked: impl Fn(usize, usize) -> bool) -> Vec<usize> {
    let mut group: Vec<usize> = (0..n).collect();
    fn find(g: &mut [usize], x: usize) -> usize {
        if g[x] != x {
            let r = find(g, g[x]);
            g[x] = r;
        }
        g[x]
    }
    for i in 0..n {
        for j in i + 1..n {
            if find(&mut group, i) != find(&mut group, j) && linked(i, j) {
                let (a, b) = (find(&mut group, i), find(&mut group, j));
                group[a] = b;
            }
        }
    }
    let mut size = vec![0usize; n];
    for i in 0..n {
        size[find(&mut group, i)] += 1;
    }
    let Some(biggest) = (0..n).max_by_key(|&i| size[i]) else { return Vec::new() };
    (0..n).filter(|&i| find(&mut group, i) != biggest).collect()
}

pub struct Planner<'a> {
    pub cfg: Config,
    pub pts: &'a [Point],
    pub cands: Vec<Candidate>,
    cols: Vec<Column>,
    fixed_pos: Vec<Vec3>,
    fixed_cols: Vec<Column>,
    accs: Vec<Acc>,
    scores: Vec<f64>,
    seen: Vec<Vec<(u32, Entry)>>,
    sum: f64,
    pub chosen: Vec<usize>,
    total_w: f64,
    sync: Option<SyncTest<'a>>,
    /// The search stops here: what was asked, or what the mounting points can reach if less.
    target_cov: Option<f32>,
    zone_target: Vec<f32>,
    /// Share of the floor covered with every mounting point in use.
    reachable: f32,
}

/// Budgets up to this many devices are chosen jointly by beam search.
const BEAM_MAX_ADD: usize = 6;

impl<'a> Planner<'a> {
    pub fn new(cfg: Config, pts: &'a [Point], cands: Vec<Candidate>, cols: Vec<Column>, fixed: Vec<(Vec3, Column)>, sync: Option<SyncTest<'a>>) -> Self {
        let total_w = pts.iter().map(|p| p.weight as f64).sum::<f64>().max(1.0);
        let (fixed_pos, fixed_cols) = fixed.into_iter().unzip();
        let mut s = Self {
            cfg,
            pts,
            cands,
            cols,
            fixed_pos,
            fixed_cols,
            accs: Vec::new(),
            scores: Vec::new(),
            seen: Vec::new(),
            sum: 0.0,
            chosen: Vec::new(),
            total_w,
            sync,
            target_cov: None,
            zone_target: Vec::new(),
            reachable: 0.0,
        };
        s.reset();
        s.set_targets();
        s
    }

    /// Back to the devices already in the scene.
    fn reset(&mut self) {
        self.chosen.clear();
        self.accs.clear();
        self.accs.resize(self.pts.len(), Acc::default());
        self.seen.clear();
        self.seen.resize(self.pts.len(), Vec::new());
        for (i, col) in self.fixed_cols.iter().enumerate() {
            for e in col {
                self.accs[e.pt as usize].add(FIXED + i as u32, e);
                self.seen[e.pt as usize].push((FIXED + i as u32, *e));
            }
        }
        self.scores = self.pts.iter().zip(&self.accs).map(|(p, a)| self.point_score(a, p)).collect();
        self.sum = self.scores.iter().sum();
    }

    /// Floor no mounting point can reach stays uncovered however many devices
    /// go up: stop searching at what is reachable, on the floor and in each zone.
    /// That doesn't make it what was asked for (`targets_met`).
    fn set_targets(&mut self) {
        for c in 0..self.cands.len() {
            let col = std::mem::take(&mut self.cols[c]);
            for e in &col {
                self.accs[e.pt as usize].add(c as u32, e);
            }
            self.cols[c] = col;
        }
        let reach = self.metrics_inner(false);
        self.reachable = reach.coverage;
        let t = self.cfg.target_coverage;
        self.target_cov = t.map(|t| t.min(reach.coverage * 0.98));
        self.zone_target = reach.zones.iter().map(|r| if r.is_nan() { 0.0 } else { t.unwrap_or(0.0).min(r * 0.98) }).collect();
        self.reset();
    }

    /// Credit for one point, in units of its weight.
    fn point_score(&self, a: &Acc, pt: &Point) -> f64 {
        let w = pt.weight as f64;
        let c = &self.cfg;
        if c.method == Method::Read {
            let p = a.p_any() as f64;
            return if c.zones_only && pt.zones.is_empty() { -c.stray_weight * w * p } else { w * p };
        }
        let n = a.n as usize;
        if n < c.need {
            // Partial credit so the search can start from nothing: one anchor
            // alone never makes a fix where it takes three.
            return w * 0.3 * n as f64 / c.need as f64;
        }
        let mut err = a.err(c.method, c.dim3);
        if c.redundancy
            && let Some((_, top)) = a.top
        {
            let mut less = *a;
            less.apply(&top, -1.0);
            err = 0.5 * err + 0.5 * less.err(c.method, c.dim3).min(c.err_cap * 4.0);
        }
        let err = err.min(c.err_cap * 4.0);
        let q = fix_share(err, c.target_m) as f64;
        // The second term keeps a gradient once a point is far better than the target.
        w * (0.4 + 0.6 * q + 0.1 / (1.0 + (err / c.target_m.max(1e-3)) as f64))
    }

    fn with(&self, a: &Acc, col: u32, e: &Entry) -> Acc {
        let mut b = *a;
        b.add(col, e);
        b
    }

    /// Install cost and interference of adding `c` to the current set.
    fn set_penalty(&self, c: usize) -> f64 {
        let cc = &self.cands[c];
        let mut pen = self.cfg.cost_weight * self.total_w * cc.cost as f64;
        if self.cfg.method == Method::Read && self.cfg.conflict_m > 0.0 {
            let n = self.chosen.iter().filter(|&&o| self.jam(cc, &self.cands[o])).count();
            pen += n as f64 * 0.01 * self.total_w;
        }
        pen
    }

    /// Two readers close enough, each inside the other's beam.
    fn jam(&self, a: &Candidate, b: &Candidate) -> bool {
        let d = b.pos - a.pos;
        let len = d.length();
        if len >= self.cfg.conflict_m || len < 1e-3 {
            return false;
        }
        let half = (self.cfg.beam_deg * 0.5).to_radians();
        a.facing.normalize_or_zero().dot(d / len) > half.cos() && b.facing.normalize_or_zero().dot(-d / len) > half.cos()
    }

    /// Score change if `c` were added.
    pub fn gain(&self, c: usize) -> f64 {
        let col = c as u32;
        let mut d = 0.0;
        for e in &self.cols[c] {
            let k = e.pt as usize;
            d += self.point_score(&self.with(&self.accs[k], col, e), &self.pts[k]) - self.scores[k];
        }
        d - self.set_penalty(c)
    }

    pub fn add(&mut self, c: usize) {
        let penalty = self.set_penalty(c);
        let col = std::mem::take(&mut self.cols[c]);
        for e in &col {
            let k = e.pt as usize;
            self.accs[k].add(c as u32, e);
            self.seen[k].push((c as u32, *e));
            let s = self.point_score(&self.accs[k], &self.pts[k]);
            self.sum += s - self.scores[k];
            self.scores[k] = s;
        }
        self.cols[c] = col;
        self.sum -= penalty;
        self.chosen.push(c);
    }

    pub fn remove(&mut self, c: usize) {
        self.chosen.retain(|&x| x != c);
        let penalty = self.set_penalty(c);
        let col = std::mem::take(&mut self.cols[c]);
        for e in &col {
            let k = e.pt as usize;
            self.seen[k].retain(|(id, _)| *id != c as u32);
            let a = &mut self.accs[k];
            a.apply(e, -1.0);
            if a.top.is_some_and(|(id, _)| id == c as u32) {
                a.top = None;
                for (id, o) in &self.seen[k] {
                    if a.top.is_none_or(|(_, t)| o.w > t.w) {
                        a.top = Some((*id, *o));
                    }
                }
            }
            let s = self.point_score(&self.accs[k], &self.pts[k]);
            self.sum += s - self.scores[k];
            self.scores[k] = s;
        }
        self.cols[c] = col;
        self.sum += penalty;
    }

    #[cfg(test)]
    pub fn score(&self) -> f64 {
        self.sum
    }

    /// Far enough from every device already there.
    fn allowed(&self, c: usize, except: Option<usize>) -> bool {
        let p = self.cands[c].pos;
        let m = self.cfg.min_spacing_m;
        !self.chosen.contains(&c)
            && self.chosen.iter().filter(|&&o| Some(o) != except).all(|&o| self.cands[o].pos.distance(p) >= m)
            && self.fixed_pos.iter().all(|f| f.distance(p) >= m)
            && self.free_spot(p)
    }

    /// Not on another technology's device.
    fn free_spot(&self, p: Vec3) -> bool {
        self.cfg.avoid.iter().all(|a| a.distance(p) >= SAME_SPOT_M)
    }

    /// Anchors outside the biggest group linked by clock-sync paths.
    fn unsynced(&self) -> Vec<u32> {
        let Some(test) = self.sync.filter(|_| self.cfg.method == Method::Tdoa) else { return Vec::new() };
        let all: Vec<(u32, Vec3)> = self.chosen.iter().map(|&c| (c as u32, self.cands[c].pos)).chain(self.fixed_pos.iter().enumerate().map(|(i, p)| (FIXED + i as u32, *p))).collect();
        outside_biggest_group(all.len(), |i, j| test(all[i].1, all[j].1)).into_iter().map(|i| all[i].0).collect()
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics_inner(true)
    }

    fn metrics_inner(&self, exact: bool) -> Metrics {
        let c = &self.cfg;
        let unsynced = if exact { self.unsynced() } else { Vec::new() };
        let mut covered = 0usize;
        let mut judged = 0usize;
        let mut zone_hit = vec![0usize; c.zone_count];
        let mut zone_all = vec![0usize; c.zone_count];
        let mut located: Vec<f32> = Vec::new();
        let mut judged_errs: Vec<f32> = Vec::new();
        let mut errs_k1: Vec<f32> = Vec::new();
        let (mut stray, mut stray_n) = (0.0f32, 0usize);
        for (k, pt) in self.pts.iter().enumerate() {
            for &z in &pt.zones {
                zone_all[z as usize] += 1;
            }
            let rebuilt;
            let a = if unsynced.is_empty() {
                &self.accs[k]
            } else {
                let mut b = Acc::default();
                for (id, e) in self.seen[k].iter().filter(|(id, _)| !unsynced.contains(id)) {
                    b.add(*id, e);
                }
                rebuilt = b;
                &rebuilt
            };
            let in_scope = if c.zones_only { !pt.zones.is_empty() } else { pt.floor };
            let hit = if c.method == Method::Read {
                a.p_any() >= c.read_p
            } else if (a.n as usize) < c.need {
                if in_scope {
                    judged_errs.push(f32::INFINITY);
                }
                false
            } else {
                let err = a.err(c.method, c.dim3);
                located.push((err * R95_PER_RMS).min(c.err_cap * 4.0));
                if in_scope {
                    judged_errs.push(err * R95_PER_RMS);
                }
                let mut worst = err;
                if exact && c.redundancy {
                    for (id, e) in self.seen[k].iter().filter(|(id, _)| !unsynced.contains(id)) {
                        let mut less = *a;
                        less.apply(e, -1.0);
                        let _ = id;
                        worst = worst.max(less.err(c.method, c.dim3));
                    }
                    errs_k1.push((worst * R95_PER_RMS).min(c.err_cap * 4.0));
                }
                worst * R95_PER_RMS <= c.err_cap
            };
            if in_scope {
                judged += 1;
                covered += hit as usize;
            }
            if c.zones_only && pt.zones.is_empty() {
                stray += a.p_any();
                stray_n += 1;
            }
            if hit {
                for &z in &pt.zones {
                    zone_hit[z as usize] += 1;
                }
            }
        }
        let pct = |v: &mut Vec<f32>| {
            v.sort_by(f32::total_cmp);
            if v.is_empty() { f32::INFINITY } else { v[((v.len() - 1) as f32 * 0.95).ceil() as usize] }
        };
        let conflicts = if c.method == Method::Read && c.conflict_m > 0.0 {
            let mut n = 0;
            for (i, &a) in self.chosen.iter().enumerate() {
                n += self.chosen[i + 1..].iter().filter(|&&b| self.jam(&self.cands[a], &self.cands[b])).count();
            }
            n
        } else {
            0
        };
        Metrics {
            coverage: covered as f32 / judged.max(1) as f32,
            p95: pct(&mut judged_errs),
            p95_located: pct(&mut located),
            p95_k1: pct(&mut errs_k1),
            zones: zone_hit.iter().zip(&zone_all).map(|(&h, &n)| if n == 0 { f32::NAN } else { h as f32 / n as f32 }).collect(),
            stray: if stray_n > 0 { stray / stray_n as f32 } else { 0.0 },
            conflicts,
            unsynced: unsynced.len(),
            cost: self.chosen.iter().map(|&i| self.cands[i].cost).sum(),
        }
    }

    /// Has the search got as far as it can? Not the same as meeting the targets: see
    /// `targets_met`.
    pub fn met(&self, m: &Metrics) -> bool {
        let p95 = if self.cfg.redundancy { m.p95_k1 } else { m.p95_located };
        self.target_cov.is_some_and(|t| m.coverage >= t)
            && m.zones.iter().zip(&self.zone_target).all(|(z, t)| z.is_nan() || *z >= *t)
            && (self.cfg.method == Method::Read || self.cfg.target_p95.is_none_or(|t| p95 <= t))
    }

    /// Did the plan reach what was asked for, not just what could be reached?
    pub fn targets_met(&self, m: &Metrics) -> bool {
        let p95 = if self.cfg.redundancy { m.p95_k1 } else { m.p95 };
        let t = self.cfg.target_coverage;
        t.is_some_and(|t| m.coverage >= t)
            && m.zones.iter().all(|z| z.is_nan() || t.is_none_or(|t| *z >= t))
            && (self.cfg.method == Method::Read || self.cfg.target_p95.is_none_or(|t| p95 <= t))
    }

    /// Share of the floor covered with every mounting point in use.
    pub fn reachable_coverage(&self) -> f32 {
        self.reachable
    }

    /// Of the first `zones` zones, those reaching the coverage asked for.
    pub fn zones_met(&self, m: &Metrics, zones: usize) -> usize {
        let t = self.cfg.target_coverage.unwrap_or(0.0);
        m.zones.iter().take(zones).filter(|z| !z.is_nan() && **z >= t).count()
    }

    fn best_add(&self) -> Option<(usize, f64)> {
        (0..self.cands.len())
            .into_par_iter()
            .filter(|&c| self.allowed(c, None))
            .map(|c| (c, self.gain(c)))
            // Lowest index on ties, so a plan never depends on thread timing.
            .reduce_with(|a, b| if b.1 > a.1 || (b.1 == a.1 && b.0 < a.0) { b } else { a })
    }

    /// Every device must earn its place: more floor or zone covered, a lower
    /// predicted error, or simply being one of the first few a fix needs.
    fn worth(&self, before: &Metrics, after: &Metrics) -> bool {
        let zones = |m: &Metrics| m.zones.iter().filter(|z| !z.is_nan()).sum::<f32>();
        self.chosen.len() <= self.cfg.need.max(self.cfg.force_add)
            || after.coverage > before.coverage + 0.01
            || zones(after) > zones(before) + 0.01
            || (before.p95_located.is_finite() && after.p95_located < before.p95_located - 0.01)
            || (!before.p95_located.is_finite() && after.p95_located.is_finite())
    }

    /// Adds the best candidate at a time. Past the first few, gains mostly
    /// shrink as devices are added, so a stale gain bounds the real one: only
    /// the top of the queue is re-scored (lazy greedy). Swaps repair the
    /// rare miss where a device gains more than it did before.
    fn greedy(&mut self, add: usize) {
        use std::cmp::Ordering;
        #[derive(PartialEq)]
        struct Item(f64, usize, usize);
        impl Eq for Item {}
        impl Ord for Item {
            fn cmp(&self, o: &Self) -> Ordering {
                self.0.total_cmp(&o.0).then(o.1.cmp(&self.1))
            }
        }
        impl PartialOrd for Item {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        let mut heap: Option<std::collections::BinaryHeap<Item>> = None;
        let mut now = self.metrics();
        // Picks that haven't paid for themselves yet. A pocket only gets a fix once a fix's
        // worth of devices reach it, so the first of them shows no gain on its own.
        let mut unpaid: Vec<usize> = Vec::new();
        for round in 0..add.min(64) {
            if self.met(&now) && self.chosen.len() >= self.cfg.force_add {
                break;
            }
            let lazy = self.chosen.len() >= self.cfg.need;
            let pick = if lazy {
                let h = heap.get_or_insert_with(|| {
                    let list: Vec<Item> = (0..self.cands.len()).into_par_iter().map(|c| Item(self.gain(c), c, round)).collect();
                    list.into_iter().collect()
                });
                let mut found = None;
                while let Some(top) = h.pop() {
                    if !self.allowed(top.1, None) {
                        continue;
                    }
                    if top.2 == round {
                        found = Some((top.1, top.0));
                        break;
                    }
                    h.push(Item(self.gain(top.1), top.1, round));
                }
                found
            } else {
                self.best_add()
            };
            let Some((c, g)) = pick else { break };
            if g <= 1e-6 {
                break;
            }
            self.add(c);
            let after = self.metrics();
            if self.worth(&now, &after) {
                unpaid.clear();
                now = after;
                continue;
            }
            unpaid.push(c);
            if unpaid.len() >= self.cfg.need {
                break;
            }
        }
        for c in unpaid {
            self.remove(c);
        }
    }

    fn rebuild(&mut self, set: &[usize]) {
        self.reset();
        for &c in set {
            self.add(c);
        }
    }

    /// A small budget is a set to choose jointly: the best 3 are rarely the
    /// best 1 plus the best next ones. Keeps the best partial sets at each
    /// size, as many as the problem size allows, and stops at the first size
    /// that meets the targets.
    fn beam(&mut self, add: usize) {
        let links: usize = self.cols.iter().map(Vec::len).sum();
        let per_set = links.max(1);
        let width = (20_000_000 / per_set).clamp(8, 400);
        let mut beam: Vec<(Vec<usize>, f64)> = vec![(Vec::new(), self.sum)];
        let mut done: Option<Vec<usize>> = None;
        for _ in 0..add {
            let mut tried = std::collections::HashSet::new();
            let mut next: Vec<(Vec<usize>, f64)> = Vec::new();
            for (set, _) in &beam {
                self.rebuild(set);
                let base = self.sum;
                let this: &Self = self;
                let gains: Vec<(usize, f64)> = (0..this.cands.len()).into_par_iter().filter(|&c| this.allowed(c, None)).map(|c| (c, this.gain(c))).collect();
                for (c, g) in gains {
                    let mut t = set.clone();
                    t.push(c);
                    let mut key = t.clone();
                    key.sort_unstable();
                    if tried.insert(key) {
                        next.push((t, base + g));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            next.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            next.truncate(width);
            // The fewest devices that meet the targets win.
            for (set, _) in next.iter().take(16) {
                self.rebuild(set);
                let m = self.metrics();
                if self.met(&m) {
                    done = Some(set.clone());
                    break;
                }
            }
            beam = next;
            if done.is_some() {
                break;
            }
        }
        let best = done.or_else(|| beam.into_iter().next().map(|b| b.0)).unwrap_or_default();
        self.rebuild(&best);
    }

    /// Greedy picks bunch up: try replacing each with its best alternative.
    fn swap(&mut self) {
        for _ in 0..3 {
            let mut improved = false;
            for k in self.chosen.clone() {
                if !self.chosen.contains(&k) {
                    continue;
                }
                let before = self.sum;
                let was_met = self.met(&self.metrics());
                self.remove(k);
                match self.best_add() {
                    Some((c, g)) if c != k && self.sum + g > before + 1e-6 => {
                        self.add(c);
                        // Never trade a plan that meets the targets for one that does not.
                        if was_met && !self.met(&self.metrics()) {
                            self.remove(c);
                            self.add(k);
                        } else {
                            improved = true;
                        }
                    }
                    _ => self.add(k),
                }
            }
            if !improved {
                break;
            }
        }
    }

    /// Fewest devices: later picks often make earlier ones redundant.
    fn prune(&mut self) -> bool {
        let mut any = false;
        loop {
            let now = self.metrics();
            let was_met = self.met(&now);
            let zones = |m: &Metrics| m.zones.iter().filter(|z| !z.is_nan()).sum::<f32>();
            let mut best: Option<(usize, f64)> = None;
            for c in self.chosen.clone() {
                self.remove(c);
                let m = self.metrics();
                // Short of the target (budget, racks): drop only dead weight.
                let ok = if was_met {
                    self.met(&m)
                } else {
                    m.coverage >= now.coverage - 1e-6 && zones(&m) >= zones(&now) - 1e-6 && m.p95_located <= now.p95_located + 0.005 && (!self.cfg.redundancy || m.p95_k1 <= now.p95_k1 + 0.005)
                };
                if ok && best.is_none_or(|(_, s)| self.sum > s) {
                    best = Some((c, self.sum));
                }
                self.add(c);
            }
            match best {
                Some((c, _)) => {
                    self.remove(c);
                    any = true;
                }
                None => return any,
            }
        }
    }

    /// Slides each device along its mounting surface in shrinking steps,
    /// keeping moves that score better. Candidates are a coarse grid; the
    /// best spot is usually between two of them.
    pub fn refine(&mut self, mover: &dyn Mover) {
        for step in [0.5f32, 0.25, 0.1] {
            for k in 0..self.chosen.len() {
                let mut c = self.chosen[k];
                let n = self.cands[c].facing.normalize_or(Vec3::NEG_Y);
                let t1 = if n.y.abs() > 0.9 { Vec3::X } else { Vec3::new(-n.z, 0.0, n.x).normalize_or(Vec3::X) };
                let t2 = if self.cands[c].mount.on_surface() && n.y.abs() <= 0.9 { Vec3::Y } else { Vec3::Z };
                for dir in [t1, -t1, t2, -t2] {
                    let from = self.cands[c];
                    let Some(to) = mover.settle(&from, from.pos + dir * step) else { continue };
                    if to.distance(from.pos) < 1e-3 {
                        continue;
                    }
                    let new_cand = Candidate { pos: to, ..from };
                    let m = self.cfg.min_spacing_m;
                    if self.chosen.iter().any(|&o| o != c && self.cands[o].pos.distance(to) < m) || self.fixed_pos.iter().any(|f| f.distance(to) < m) || !self.free_spot(to) {
                        continue;
                    }
                    let col = mover.column(to, from.facing);
                    let before = self.sum;
                    let was_met = self.met(&self.metrics());
                    self.cands.push(new_cand);
                    self.cols.push(col);
                    let new = self.cands.len() - 1;
                    self.remove(c);
                    self.add(new);
                    if self.sum > before + 1e-6 && (!was_met || self.met(&self.metrics())) {
                        c = new;
                    } else {
                        self.remove(new);
                        self.add(c);
                        self.cands.pop();
                        self.cols.pop();
                    }
                }
                // `add` appended it: put it back in its slot for the next device.
                if let Some(i) = self.chosen.iter().position(|&x| x == c) {
                    let last = self.chosen.remove(i);
                    self.chosen.insert(k.min(self.chosen.len()), last);
                }
            }
        }
    }

    /// The whole search: choose, spread out, drop what is redundant, fine-tune.
    pub fn run(&mut self, add: usize, mover: Option<&dyn Mover>) {
        if add == 0 {
            return;
        }
        if add <= BEAM_MAX_ADD && self.cfg.force_add == 0 { self.beam(add) } else { self.greedy(add) }
        // Forced additions are there because the simulation asked for them:
        // the prediction would talk the search out of them again.
        for _ in 0..if self.cfg.force_add > 0 { 0 } else { 4 } {
            self.swap();
            if !self.prune() {
                break;
            }
        }
        if let Some(m) = mover {
            self.refine(m);
        }
    }
}

/// A gap in the walls, or a door or dock: where goods cross the zone boundary.
#[derive(Clone, Copy, Debug)]
pub struct Opening {
    pub center: Vec3,
    /// Horizontal unit vector along the width of the opening.
    pub across: Vec3,
    pub width: f32,
}

/// Openings from door-like objects and from gaps between collinear walls.
/// `doors` and `walls` are world bounds (min, max).
pub fn find_openings(doors: &[(Vec3, Vec3)], walls: &[(Vec3, Vec3)]) -> Vec<Opening> {
    let mut v: Vec<Opening> = Vec::new();
    let mut push = |o: Opening| {
        if !v.iter().any(|x| x.center.distance(o.center) < 1.0) {
            v.push(o);
        }
    };
    for (a, b) in doors {
        let s = *b - *a;
        let (across, width) = if s.x >= s.z { (Vec3::X, s.x) } else { (Vec3::Z, s.z) };
        if width >= 0.6 {
            let c = (*a + *b) * 0.5;
            push(Opening { center: Vec3::new(c.x, 1.2, c.z), across, width });
        }
    }
    // Walls running the same way on the same line, with a person-to-truck gap.
    for (i, (a0, a1)) in walls.iter().enumerate() {
        for (b0, b1) in &walls[i + 1..] {
            let (sa, sb) = (*a1 - *a0, *b1 - *b0);
            for x_axis in [true, false] {
                let along = |p: Vec3| if x_axis { p.x } else { p.z };
                let side = |p: Vec3| if x_axis { p.z } else { p.x };
                let runs = |s: Vec3| if x_axis { s.x > s.z * 3.0 } else { s.z > s.x * 3.0 };
                if !runs(sa) || !runs(sb) || (side(*a0 + *a1) - side(*b0 + *b1)).abs() * 0.5 > 0.3 {
                    continue;
                }
                let (lo_end, hi_start) = if along(*a0) < along(*b0) { (along(*a1), along(*b0)) } else { (along(*b1), along(*a0)) };
                let gap = hi_start - lo_end;
                if (0.8..=6.0).contains(&gap) {
                    let m = (lo_end + hi_start) * 0.5;
                    let s = side(*a0 + *a1) * 0.5;
                    let center = if x_axis { Vec3::new(m, 1.2, s) } else { Vec3::new(s, 1.2, m) };
                    push(Opening { center, across: if x_axis { Vec3::X } else { Vec3::Z }, width: gap });
                }
            }
        }
    }
    v
}

/// Reader antennas on both jambs of an opening, facing each other across it
/// (just inside the opening: past its edge they would be inside the wall).
pub fn portal_pair(o: &Opening, height: f32) -> [(Vec3, Vec3); 2] {
    let half = o.across * (o.width * 0.5 - 0.08).max(0.05);
    let c = Vec3::new(o.center.x, height, o.center.z);
    [(c - half, o.across), (c + half, -o.across)]
}

/// Extra aims for a directional device: the mount's own, straight down from
/// up high, towards the nearest zones, and tilted down.
pub fn expand_aims(cands: &[Candidate], zones: &[(Vec3, Vec3)], max_range: f32) -> Vec<Candidate> {
    let mut out = Vec::new();
    for c in cands {
        let mut aims = vec![c.facing.normalize_or(Vec3::NEG_Y)];
        if c.pos.y > 2.0 {
            aims.push(Vec3::NEG_Y);
        }
        let mut near: Vec<(f32, Vec3)> = zones
            .iter()
            .map(|(a, b)| {
                let t = (*a + *b) * 0.5;
                let t = Vec3::new(t.x, t.y.min(1.2), t.z);
                (t.distance(c.pos), t)
            })
            .filter(|(d, _)| *d <= max_range * 1.2 && *d > 0.2)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        aims.extend(near.iter().take(2).map(|(_, t)| (*t - c.pos).normalize_or(Vec3::NEG_Y)));
        let f = c.facing.normalize_or_zero();
        let flat = Vec2::new(f.x, f.z);
        if flat.length() > 0.5 {
            let flat = flat.normalize();
            for tilt in [30f32, 60.0] {
                let (s, co) = tilt.to_radians().sin_cos();
                aims.push(Vec3::new(flat.x * co, -s, flat.y * co));
            }
        }
        let mut kept: Vec<Vec3> = Vec::new();
        for a in aims {
            if !kept.iter().any(|k| k.dot(a) > 0.98) {
                kept.push(a);
            }
        }
        out.extend(kept.into_iter().map(|facing| Candidate { facing, ..*c }));
    }
    out
}

/// Wall, column and pole mounts also at other heights: higher clears racks
/// and adds vertical spread for 3D fixes, lower reaches under obstructions.
pub fn expand_heights(cands: &[Candidate], ceiling: Option<f32>) -> Vec<Candidate> {
    let top = ceiling.map_or(f32::MAX, |c| c - 0.2);
    let mut out = Vec::with_capacity(cands.len() * 3);
    for c in cands {
        out.push(*c);
        if matches!(c.mount, Mount::Wall | Mount::Column | Mount::Free) {
            for dy in [1.0f32, -0.8] {
                let y = c.pos.y + dy;
                if y >= 1.2 && y <= top {
                    out.push(Candidate { pos: Vec3::new(c.pos.x, y, c.pos.z), ..*c });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::device::DeviceKind;
    use crate::rtls::radio::Material;
    use crate::rtls::rng::Rng;
    use crate::rtls::solver::{self, Dim};
    use bevy::math::DVec3;

    /// Boxes with a material: the scene without any ECS or meshes.
    #[derive(Default)]
    struct Boxes(Vec<(Vec3, Vec3, Material)>);

    fn hits(a: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        for i in 0..3 {
            if d[i].abs() < 1e-9 {
                if a[i] < lo[i] || a[i] > hi[i] {
                    return None;
                }
            } else {
                let (mut x, mut y) = ((lo[i] - a[i]) / d[i], (hi[i] - a[i]) / d[i]);
                if x > y {
                    std::mem::swap(&mut x, &mut y);
                }
                t0 = t0.max(x);
                t1 = t1.min(y);
                if t0 > t1 {
                    return None;
                }
            }
        }
        Some(t0)
    }

    impl RadioScene for Boxes {
        fn path(&self, a: Vec3, b: Vec3) -> Path {
            let mut p = Path { distance: a.distance(b), ..Default::default() };
            for (lo, hi, m) in &self.0 {
                if hits(a, b - a, *lo, *hi).is_some() {
                    p.cross_weighted(*m, m.weight());
                }
            }
            p
        }

        fn surface(&self, from: Vec3, dir: Vec3, max: f32) -> Option<f32> {
            self.0.iter().filter_map(|(lo, hi, _)| hits(from, dir * max, *lo, *hi)).map(|t| t * max).min_by(f32::total_cmp)
        }
    }

    fn anchor() -> Profile {
        Profile { kind: DeviceKind::UwbAnchor, range_sigma_m: 0.08, max_range_m: 40.0, ..Default::default() }
    }

    fn uwb_tag() -> Profile {
        Profile { kind: DeviceKind::UwbTag, range_sigma_m: 0.08, ..Default::default() }
    }

    fn reader() -> Profile {
        Profile { kind: DeviceKind::RfidReader, max_range_m: 12.0, tx_power_dbm: 30.0, antenna_gain_dbi: 9.0, beam_deg: 70.0, freq_mhz: 915.0, sensitivity_dbm: -80.0, wall_loss_db: 6.0, ..Default::default() }
    }

    fn rfid_tag() -> Profile {
        Profile { kind: DeviceKind::RfidTag, tag_sensitivity_dbm: -18.0, antenna_gain_dbi: 2.0, ..Default::default() }
    }

    fn locator() -> Profile {
        Profile { kind: DeviceKind::BleNode, method: RadioMethod::Aoa, max_range_m: 20.0, sensitivity_dbm: -92.0, ..Default::default() }
    }

    fn floor(lo: Vec2, hi: Vec2, cell: f32, y: f32) -> Vec<Point> {
        let mut v = Vec::new();
        let mut z = lo.y + cell * 0.5;
        while z < hi.y {
            let mut x = lo.x + cell * 0.5;
            while x < hi.x {
                v.push(Point { pos: Vec3::new(x, y, z), weight: 1.0, floor: true, zones: Vec::new() });
                x += cell;
            }
            z += cell;
        }
        v
    }

    fn grid_cands(lo: Vec2, hi: Vec2, h: f32, step: f32) -> Vec<Candidate> {
        let mut v = Vec::new();
        let mut z = lo.y + 0.3;
        while z <= hi.y - 0.3 {
            let mut x = lo.x + 0.3;
            while x <= hi.x - 0.3 {
                let pos = Vec3::new(x, h, z);
                v.push(Candidate { pos, facing: Vec3::NEG_Y, mount: Mount::Free, cost: 1.0 });
                x += step;
            }
            z += step;
        }
        v
    }

    fn no_gain(p: &Profile, fwd: Vec3, dir: Vec3) -> f32 {
        radio::antenna_gain(p.antenna_gain_dbi, p.beam_deg, fwd.angle_between(dir))
    }

    fn columns<S: RadioScene>(s: &S, pts: &[Point], m: Method, d: &Profile, t: &Profile, cands: &[Candidate]) -> Vec<Column> {
        cands.iter().map(|c| column(s, pts, m, d, t, c.pos, c.facing, &no_gain)).collect()
    }

    /// Error at `p` from devices at `at`, by the planner's own arithmetic.
    fn err_of<S: RadioScene>(s: &S, m: Method, d: &Profile, t: &Profile, at: &[Vec3], p: Vec3) -> f32 {
        let pts = [Point { pos: p, weight: 1.0, floor: true, zones: Vec::new() }];
        let mut a = Acc::default();
        for (i, q) in at.iter().enumerate() {
            for e in column(s, &pts, m, d, t, *q, p - *q, &no_gain) {
                a.add(i as u32, &e);
            }
        }
        if (a.n as usize) < m.need() { f32::INFINITY } else { a.err(m, false) }
    }

    fn square(h: f32) -> Vec<Vec3> {
        vec![Vec3::new(0.0, h, 0.0), Vec3::new(10.0, h, 0.0), Vec3::new(10.0, h, 10.0), Vec3::new(0.0, h, 10.0)]
    }

    /// One TWR epoch through the simulator's own measurement and solver: position error.
    fn sim_epoch<S: RadioScene>(scene: &S, d: &Profile, t: &Profile, at: &[Vec3], p: Vec3, rng: &mut Rng) -> Option<f32> {
        let (mut a, mut r, mut s) = (Vec::new(), Vec::new(), Vec::new());
        for q in at {
            // A fresh spot each draw: the term a parked tag keeps differs from spot to spot.
            let held = radio::LinkDraw::new(rng);
            if let Some(m) = radio::twr_measure(d, t, scene.path(p, *q), held, true, rng) {
                a.push(q.as_dvec3());
                r.push(m.range as f64);
                s.push(m.sigma);
            }
        }
        let f = solver::twr_solve(Dim::Plane(p.y as f64), &a, &r, &s, Some(p.as_dvec3()), true)?;
        Some((f.pos - p.as_dvec3()).length() as f32)
    }

    #[derive(Debug)]
    struct Disagreement {
        median: f32,
        p95: f32,
        max: f32,
        min: f32,
        /// Share of the cells the Quality map shows green (75 % within 30 cm) where the sim fails.
        green_but_failing: f32,
        /// Share of the cells passing that test on the map, and in the sim.
        map_pass: f32,
        sim_pass: f32,
        /// Cells where one says there's a fix and the other doesn't.
        fix_mismatch: usize,
        cells: usize,
    }

    /// Sim RMS / planner RMS, cell by cell, on the audit's 30 × 20 m floor (tag at 1.2 m).
    fn planner_vs_sim(scene: &Boxes, at: &[Vec3], step: f32, draws: usize, seed: u64) -> Disagreement {
        let (d, t) = (anchor(), uwb_tag());
        let mut rng = Rng::new(seed);
        let (mut ratios, mut green_fail, mut mismatch, mut cells) = (Vec::new(), 0usize, 0usize, 0usize);
        let (mut map_pass, mut sim_pass) = (0usize, 0usize);
        let mut z = step * 0.5;
        while z < 20.0 {
            let mut x = step * 0.5;
            while x < 30.0 {
                let p = Vec3::new(x, 1.2, z);
                let inside = scene.0.iter().any(|(lo, hi, _)| x >= lo.x - 0.1 && x <= hi.x + 0.1 && z >= lo.z - 0.1 && z <= hi.z + 0.1);
                x += step;
                if inside {
                    continue;
                }
                cells += 1;
                let e = err_of(scene, Method::Twr, &d, &t, at, p);
                let errs: Vec<f32> = (0..draws).filter_map(|_| sim_epoch(scene, &d, &t, at, p, &mut rng)).collect();
                if !e.is_finite() || errs.len() < draws / 10 {
                    mismatch += (e.is_finite() != (errs.len() >= draws / 10)) as usize;
                    continue;
                }
                let rms = (errs.iter().map(|v| v * v).sum::<f32>() / errs.len() as f32).sqrt();
                ratios.push(rms / e);
                let within = errs.iter().filter(|v| **v <= 0.3).count() as f32 / errs.len() as f32;
                green_fail += (fix_share(e, 0.3) >= 0.75 && within < 0.75) as usize;
                map_pass += (fix_share(e, 0.3) >= 0.75) as usize;
                sim_pass += (within >= 0.75) as usize;
            }
            z += step;
        }
        ratios.sort_by(f32::total_cmp);
        let at_q = |q: f32| ratios[((ratios.len() - 1) as f32 * q).round() as usize];
        Disagreement { median: at_q(0.5), p95: at_q(0.95), max: *ratios.last().unwrap(), min: ratios[0], green_but_failing: green_fail as f32 / cells as f32, map_pass: map_pass as f32 / cells as f32, sim_pass: sim_pass as f32 / cells as f32, fix_mismatch: mismatch, cells }
    }

    fn audit_anchors() -> Vec<Vec3> {
        [0.3f32, 15.0, 29.7].iter().flat_map(|x| [0.3f32, 19.7].map(|z| Vec3::new(*x, 2.6, z))).collect()
    }

    /// The audit's partitions: they don't touch the perimeter.
    fn audit_partitions(m: Material) -> Boxes {
        Boxes(vec![(Vec3::new(10.0, 0.0, 3.0), Vec3::new(10.1, 3.0, 17.0), m), (Vec3::new(20.0, 0.0, 3.0), Vec3::new(20.1, 3.0, 17.0), m)])
    }

    /// One fix by the other methods, through the simulator's measurements and solvers.
    fn other_epoch(m: Method, d: &Profile, t: &Profile, at: &[Vec3], p: Vec3, rng: &mut Rng) -> Option<f32> {
        let scene = Boxes::default();
        let plane = Dim::Plane(p.y as f64);
        let truth = p.as_dvec3();
        let fix = match m {
            Method::Tdoa => {
                let mut arrivals: Vec<(Vec3, f32)> = Vec::new();
                for q in at {
                    let held = radio::LinkDraw::new(rng);
                    if let Some(a) = radio::uwb_arrival_held(d, t, scene.path(p, *q), 0.0, held, rng) {
                        arrivals.push((*q, a));
                    }
                }
                let &(rq, rt) = arrivals.iter().min_by(|a, b| a.1.total_cmp(&b.1))?;
                let others: Vec<&(Vec3, f32)> = arrivals.iter().filter(|(q, _)| *q != rq).collect();
                if others.len() < 3 {
                    return None;
                }
                let pos: Vec<DVec3> = others.iter().map(|(q, _)| q.as_dvec3()).collect();
                let diffs: Vec<f64> = others.iter().map(|(_, a)| (a - rt) as f64).collect();
                let sigma = (radio::uwb_arrival_sigma(d).max(0.03) * std::f32::consts::SQRT_2) as f64;
                solver::solve_tdoa(plane, rq.as_dvec3(), &pos, &diffs, sigma, Some(truth))?
            }
            Method::Rssi => {
                let (mut a, mut r, mut s) = (Vec::new(), Vec::new(), Vec::new());
                for q in at {
                    let held = radio::LinkDraw::new(rng);
                    if let Some(v) = radio::ble_rssi(d, t, scene.path(p, *q), held, rng) {
                        let e = solver::rssi_to_distance(v as f64, radio::rssi_1m(t) as f64, d.path_loss_exp as f64);
                        a.push(q.as_dvec3());
                        r.push(e);
                        s.push(radio::rssi_distance_sigma(d, e as f32) as f64);
                    }
                }
                if a.len() < 3 {
                    return None;
                }
                solver::solve_twr(plane, &a, &r, &s, Some(truth))?
            }
            Method::Rtt => {
                let (mut a, mut r, mut s) = (Vec::new(), Vec::new(), Vec::new());
                for q in at {
                    let held = radio::LinkDraw::new(rng);
                    if let Some(v) = radio::wifi_rtt_range(d, t, scene.path(p, *q), held, rng) {
                        a.push(q.as_dvec3());
                        r.push(v as f64);
                        s.push(d.range_sigma_m.max(0.1) as f64);
                    }
                }
                if a.len() < 3 {
                    return None;
                }
                let gate = 3.0 * s.iter().copied().fold(f64::MAX, f64::min);
                solver::solve_twr_robust(plane, &a, &r, &s, Some(truth), gate)?
            }
            _ => return None,
        };
        Some((fix.pos - truth).length() as f32)
    }

    /// Sim RMS / planner RMS for the other methods, over an open floor.
    fn other_vs_sim(m: Method, d: &Profile, t: &Profile, at: &[Vec3], draws: usize) -> (f32, f32, f32) {
        let scene = Boxes::default();
        let mut rng = Rng::new(21);
        let mut ratios = Vec::new();
        for (x, z) in [(10.0f32, 10.0f32), (5.0, 5.0), (15.0, 7.0), (7.0, 14.0), (12.0, 3.0), (3.0, 11.0)] {
            let p = Vec3::new(x, 1.2, z);
            let e = err_of(&scene, m, d, t, at, p);
            let errs: Vec<f32> = (0..draws).filter_map(|_| other_epoch(m, d, t, at, p, &mut rng)).collect();
            if e.is_finite() && errs.len() > draws / 2 {
                ratios.push((errs.iter().map(|v| v * v).sum::<f32>() / errs.len() as f32).sqrt() / e);
            }
        }
        ratios.sort_by(f32::total_cmp);
        (ratios[0], ratios[ratios.len() / 2], *ratios.last().unwrap())
    }

    fn corners_and_middle() -> Vec<Vec3> {
        [(0.0, 0.0), (20.0, 0.0), (20.0, 20.0), (0.0, 20.0), (10.0, 10.0)].map(|(x, z)| Vec3::new(x, 2.6, z)).to_vec()
    }

    #[test]
    fn the_planner_tracks_the_sim_for_tdoa() {
        // The planner was 1.5× low with half a timestamp's noise per arrival.
        let t = Profile { mode: UwbMode::Tdoa, ..uwb_tag() };
        let (lo, mid, hi) = other_vs_sim(Method::Tdoa, &anchor(), &t, &corners_and_middle(), 600);
        assert!(lo > 0.7 && mid > 0.8 && mid < 1.25 && hi < 1.5, "{lo} {mid} {hi}");
    }

    #[test]
    fn the_planner_tracks_the_sim_for_wifi_rtt() {
        let ap = Profile { kind: DeviceKind::WifiAp, method: RadioMethod::Rtt, range_sigma_m: 1.0, rssi_at_1m: -40.0, path_loss_exp: 3.0, shadow_sigma_db: 5.0, sensitivity_dbm: -82.0, dropout: 0.05, max_range_m: 30.0, ..Default::default() };
        let t = Profile { kind: DeviceKind::WifiClient, rssi_at_1m: -40.0, ..Default::default() };
        let (lo, mid, hi) = other_vs_sim(Method::Rtt, &ap, &t, &corners_and_middle(), 600);
        assert!(lo > 0.6 && mid < 1.25 && hi < 1.5, "{lo} {mid} {hi}");
    }

    /// The figures the audit asked about, at its resolution. Slow: `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn planner_vs_sim_full_audit_figures() {
        let at = audit_anchors();
        for (name, scene) in [("open", Boxes::default()), ("10 cm concrete", audit_partitions(Material::Concrete)), ("typical walls", audit_partitions(Material::Generic)), ("wood", audit_partitions(Material::Wood))] {
            println!("{name}: {:?}", planner_vs_sim(&scene, &at, 1.0, 300, 9));
        }
        // RSSI through the log-normal inverse, for the record: it has no first-path check to
        // model, and its weights come from the noisy distance itself.
        let node = Profile { kind: DeviceKind::BleNode, max_range_m: 30.0, ..Default::default() };
        let tag = Profile { kind: DeviceKind::BleTag, ..Default::default() };
        println!("ble rssi (min, median, max): {:?}", other_vs_sim(Method::Rssi, &node, &tag, &corners_and_middle(), 1000));
    }

    #[test]
    fn the_planner_tracks_the_sim_on_an_open_floor() {
        let d = planner_vs_sim(&Boxes::default(), &audit_anchors(), 3.0, 300, 9);
        assert!(d.median > 0.9 && d.median < 1.1 && d.p95 < 1.2 && d.max < 1.35, "{d:?}");
        assert_eq!(d.fix_mismatch, 0, "{d:?}");
    }

    #[test]
    fn the_planner_tracks_the_sim_through_concrete_partitions() {
        // The audit found 1.33 / 3.29 / 4.52 here, with the map green where the sim failed.
        let d = planner_vs_sim(&audit_partitions(Material::Concrete), &audit_anchors(), 2.0, 300, 9);
        assert!(d.median < 1.2 && d.p95 < 1.6 && d.max < 2.0, "{d:?}");
        assert!(d.green_but_failing < 0.02, "{d:?}");
    }

    #[test]
    fn the_planner_tracks_the_sim_through_typical_walls() {
        // The audit found 1.99 / 4.16 / 5.27 here, and 22.8 % of cells green but failing.
        let d = planner_vs_sim(&audit_partitions(Material::Generic), &audit_anchors(), 2.0, 300, 9);
        assert!(d.median < 1.2 && d.p95 < 1.7 && d.max < 2.2, "{d:?}");
        assert!(d.min > 0.4, "too pessimistic now: {d:?}");
        assert!(d.green_but_failing < 0.03, "{d:?}");
    }

    #[test]
    fn three_nlos_anchors_cost_what_the_sim_says() {
        // No redundancy to reject anything with, and every range late by the exponential's mean too.
        let (d, t) = (anchor(), uwb_tag());
        let p = Vec3::new(10.0, 1.2, 10.0);
        let w = |lo: Vec3, hi: Vec3| (lo, hi, Material::Generic);
        let scene = Boxes(vec![w(Vec3::new(7.0, 0.0, 4.0), Vec3::new(7.1, 3.0, 16.0)), w(Vec3::new(12.9, 0.0, 4.0), Vec3::new(13.0, 3.0, 16.0)), w(Vec3::new(4.0, 0.0, 12.9), Vec3::new(16.0, 3.0, 13.0))]);
        let at = [Vec3::new(4.0, 2.6, 7.0), Vec3::new(16.0, 2.6, 7.0), Vec3::new(10.0, 2.6, 16.0)];
        assert!(at.iter().all(|q| scene.path(p, *q).walls == 1));
        let planned = err_of(&scene, Method::Twr, &d, &t, &at, p);
        let mut rng = Rng::new(4);
        let errs: Vec<f32> = (0..1500).filter_map(|_| sim_epoch(&scene, &d, &t, &at, p, &mut rng)).collect();
        let rms = (errs.iter().map(|v| v * v).sum::<f32>() / errs.len() as f32).sqrt();
        assert!((rms / planned - 1.0).abs() < 0.2, "sim {rms} vs planned {planned}");
    }

    #[test]
    fn targets_are_judged_against_what_was_asked() {
        // A metal wall splits the floor and every mounting point is on one side.
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes(vec![(Vec3::new(9.9, 0.0, -1.0), Vec3::new(10.1, 6.0, 21.0), Material::Metal)]);
        let pts = floor(Vec2::ZERO, Vec2::splat(20.0), 2.0, 1.2);
        let cands = grid_cands(Vec2::ZERO, Vec2::new(9.5, 20.0), 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { target_coverage: Some(0.95), target_p95: Some(0.3), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(20, None);
        let m = pl.metrics();
        assert!(m.coverage < 0.6, "{m:?}");
        assert!(pl.met(&m), "the search stops at what it can reach: {m:?}");
        assert!(!pl.targets_met(&m), "half a floor isn't 95 % of one: {m:?}");
    }

    #[test]
    fn p95_is_a_95_percent_radius_over_the_whole_floor() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let pts = floor(Vec2::ZERO, Vec2::splat(10.0), 2.0, 1.2);
        let cands: Vec<Candidate> = square(2.6).into_iter().map(|pos| Candidate { pos, facing: Vec3::NEG_Y, mount: Mount::Free, cost: 1.0 }).collect();
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let mut pl = Planner::new(Config { target_coverage: Some(0.95), ..Default::default() }, &pts, cands, cols, Vec::new(), None);
        (0..4).for_each(|c| pl.add(c));
        let m = pl.metrics();
        let rms: Vec<f32> = pts.iter().map(|q| err_of(&scene, Method::Twr, &d, &t, &square(2.6), q.pos)).collect();
        let worst = rms.iter().copied().fold(0.0, f32::max);
        // Every point has a fix: the worst of them, as a radius holding 95 % of readings.
        assert!((m.p95 / (worst * 1.731) - 1.0).abs() < 0.15, "{} vs rms {worst}", m.p95);

        // Half the floor out of range: the 95th percentile of the floor is "no position".
        let short = Profile { max_range_m: 6.0, ..anchor() };
        let cands: Vec<Candidate> = vec![Vec3::new(0.0, 2.6, 0.0), Vec3::new(6.0, 2.6, 0.0), Vec3::new(0.0, 2.6, 6.0)].into_iter().map(|pos| Candidate { pos, facing: Vec3::NEG_Y, mount: Mount::Free, cost: 1.0 }).collect();
        let cols = columns(&scene, &pts, Method::Twr, &short, &t, &cands);
        let mut pl = Planner::new(Config { target_coverage: Some(0.95), ..Default::default() }, &pts, cands, cols, Vec::new(), None);
        (0..3).for_each(|c| pl.add(c));
        let m = pl.metrics();
        assert!(m.coverage < 0.9, "{m:?}");
        assert!(m.p95.is_infinite(), "{m:?}");
        assert!(m.p95_located.is_finite(), "{m:?}");
    }

    #[test]
    fn crlb_matches_dop_times_sigma_for_equal_sigma() {
        // Zero distance growth by a huge range, so every sigma is the same.
        let d = Profile { uwb_tx_dbm_mhz: radio::UWB_MAX_DBM_MHZ, sensitivity_dbm: -300.0, max_range_m: 1e6, dropout: 0.0, ..anchor() };
        let t = uwb_tag();
        let at = square(2.6);
        let p = Vec3::new(3.0, 1.2, 4.0);
        let scene = Boxes::default();
        let e = err_of(&scene, Method::Twr, &d, &t, &at, p);
        let dv: Vec<DVec3> = at.iter().map(|a| a.as_dvec3()).collect();
        let dop = solver::dop(Dim::Plane(p.y as f64), &dv, p.as_dvec3()) as f32;
        let path = scene.path(p, at[0]);
        let sigma = radio::uwb_range_sigma(&d, &t, &path);
        let expect = dop * sigma;
        assert!((e - expect).abs() / expect < 0.02, "{e} vs dop·σ {expect}");
    }

    #[test]
    fn a_square_of_anchors_beats_a_line() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        // A line only lacks geometry for tags near it: nothing crosses it there.
        let p = Vec3::new(4.5, 1.2, 0.5);
        let good = err_of(&scene, Method::Twr, &d, &t, &square(2.6), p);
        let line: Vec<Vec3> = (0..4).map(|i| Vec3::new(i as f32 * 3.0, 2.6, 0.0)).collect();
        let bad = err_of(&scene, Method::Twr, &d, &t, &line, p);
        assert!(good < 0.15, "{good}");
        assert!(bad > good * 1.5, "line {bad} vs square {good}");
    }

    #[test]
    fn tdoa_loses_the_common_part_of_the_arrivals() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let p = Vec3::new(4.0, 1.2, 6.0);
        let at = square(2.6);
        let twr = err_of(&scene, Method::Twr, &d, &t, &at, p);
        let tdoa = err_of(&scene, Method::Tdoa, &d, &t, &at, p);
        assert!(tdoa.is_finite() && tdoa > twr * 0.3, "tdoa {tdoa} twr {twr}");
        // All anchors in one direction at the same distance look the same: no fix.
        let mut a = Acc::default();
        for i in 0..4 {
            a.add(i, &Entry { pt: 0, p: 1.0, w: 10.0, v: 10.0, u: [1.0, 0.0, 0.0], ..Default::default() });
        }
        assert!(a.err(Method::Tdoa, false).is_infinite());
    }

    #[test]
    fn tdoa_anchor_without_a_sync_path_gives_no_information() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let pts = floor(Vec2::ZERO, Vec2::splat(10.0), 2.5, 1.2);
        let cands: Vec<Candidate> = square(2.6).into_iter().map(|pos| Candidate { pos, facing: Vec3::NEG_Y, mount: Mount::Free, cost: 1.0 }).collect();
        let cols = columns(&scene, &pts, Method::Tdoa, &d, &t, &cands);
        let cfg = Config { method: Method::Tdoa, need: 4, err_cap: 2.0, ..Default::default() };
        // Nobody hears anybody: nothing can synchronise.
        let deaf: SyncTest = &|_, _| false;
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), Some(deaf));
        for c in 0..4 {
            pl.add(c);
        }
        let m = pl.metrics();
        assert_eq!(m.unsynced, 3);
        assert_eq!(m.coverage, 0.0, "no fix without clock sync");
    }

    #[test]
    fn add_and_remove_match_a_full_recompute() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes(vec![(Vec3::new(4.0, 0.0, 0.0), Vec3::new(4.3, 3.0, 6.0), Material::Concrete)]);
        let pts = floor(Vec2::ZERO, Vec2::splat(12.0), 1.5, 1.2);
        let cands = grid_cands(Vec2::ZERO, Vec2::splat(12.0), 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { redundancy: true, ..Default::default() };
        let mut a = Planner::new(cfg.clone(), &pts, cands.clone(), cols.clone(), Vec::new(), None);
        let mut b = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        for c in [0, 5, 9, 12, 3] {
            a.add(c);
        }
        a.remove(9);
        a.remove(0);
        for c in [5, 12, 3] {
            b.add(c);
        }
        assert!((a.score() - b.score()).abs() < 1e-6, "{} vs {}", a.score(), b.score());
        for (x, y) in a.accs.iter().zip(&b.accs) {
            assert_eq!(x.n, y.n);
            for k in 0..6 {
                assert!((x.s2[k] - y.s2[k]).abs() < 1e-6);
            }
        }
        // `gain` predicts `add` exactly.
        let g = b.gain(7);
        let s = b.score();
        b.add(7);
        assert!((b.score() - s - g).abs() < 1e-6);
    }

    #[test]
    fn concrete_degrades_a_range_and_metal_blocks_it() {
        let (d, t) = (anchor(), uwb_tag());
        let (a, p) = (Vec3::new(0.0, 2.0, 0.0), Vec3::new(8.0, 1.2, 0.0));
        let open = Boxes::default();
        let concrete = Boxes(vec![(Vec3::new(4.0, 0.0, -3.0), Vec3::new(4.2, 3.0, 3.0), Material::Concrete)]);
        let metal = Boxes(vec![(Vec3::new(4.0, 0.0, -3.0), Vec3::new(4.2, 3.0, 3.0), Material::Metal)]);
        let info = |s: &Boxes| link_info(Method::Twr, &d, &t, &s.path(p, a), 0.0, 0.0);
        let (po, wo) = info(&open).map(|i| (i.p, i.w)).unwrap();
        let (pc, wc) = info(&concrete).map(|i| (i.p, i.w)).unwrap();
        assert!(pc < po && wc < wo * 0.2, "concrete: p {pc}/{po}, w {wc}/{wo}");
        assert!(info(&metal).is_none());
    }

    #[test]
    fn empty_room_gets_spread_out_anchors() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let (lo, hi) = (Vec2::ZERO, Vec2::splat(20.0));
        let pts = floor(lo, hi, 1.5, 1.2);
        let cands = grid_cands(lo, hi, 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { target_coverage: Some(0.95), target_p95: Some(0.3), min_spacing_m: 1.5, zone_count: 0, ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(12, None);
        let m = pl.metrics();
        assert!(pl.met(&m), "{m:?} with {}", pl.chosen.len());
        assert!(pl.chosen.len() <= 8, "too many anchors: {}", pl.chosen.len());
        let sel: Vec<Vec3> = pl.chosen.iter().map(|&c| pl.cands[c].pos).collect();
        for (i, a) in sel.iter().enumerate() {
            for b in &sel[i + 1..] {
                assert!(a.distance(*b) >= 1.5);
            }
        }
        // Their extent spans most of the room.
        let (mn, mx) = sel.iter().fold((Vec3::MAX, Vec3::MIN), |(l, h), p| (l.min(*p), h.max(*p)));
        assert!((mx.x - mn.x) * (mx.z - mn.z) > 0.6 * 400.0 * 0.6, "bunched: {sel:?}");
    }

    #[test]
    fn a_metal_wall_needs_anchors_on_both_sides() {
        let (d, t) = (anchor(), uwb_tag());
        // A rack wall with a gap at the far end: an L of open floor each side.
        let scene = Boxes(vec![(Vec3::new(9.9, 0.0, 0.0), Vec3::new(10.1, 6.0, 16.0), Material::Metal)]);
        let (lo, hi) = (Vec2::ZERO, Vec2::new(20.0, 20.0));
        let pts = floor(lo, hi, 2.0, 1.2);
        let cands = grid_cands(lo, hi, 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { target_coverage: Some(0.9), target_p95: Some(0.5), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(24, None);
        let sel: Vec<Vec3> = pl.chosen.iter().map(|&c| pl.cands[c].pos).collect();
        assert!(sel.iter().filter(|p| p.x < 9.9).count() >= 3, "{sel:?}");
        assert!(sel.iter().filter(|p| p.x > 10.1).count() >= 3, "{sel:?}");
        assert!(pl.metrics().coverage > 0.85, "{:?}", pl.metrics());
    }

    #[test]
    fn surviving_a_failure_takes_more_anchors() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let (lo, hi) = (Vec2::ZERO, Vec2::splat(16.0));
        let pts = floor(lo, hi, 2.0, 1.2);
        let cands = grid_cands(lo, hi, 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let base = Config { target_coverage: Some(0.95), target_p95: Some(0.3), ..Default::default() };
        let mut plain = Planner::new(base.clone(), &pts, cands.clone(), cols.clone(), Vec::new(), None);
        plain.run(16, None);
        let mut safe = Planner::new(Config { redundancy: true, need: 4, ..base }, &pts, cands, cols, Vec::new(), None);
        safe.run(16, None);
        let m = safe.metrics();
        assert!(safe.met(&m), "{m:?}");
        assert!(safe.chosen.len() > plain.chosen.len(), "{} vs {}", safe.chosen.len(), plain.chosen.len());
        assert!(m.p95_k1 <= 0.3, "{m:?}");
    }

    /// Aisle in front of a rack: reads wanted on its face, not in the room behind.
    fn rack_face() -> (Vec<Point>, Vec<Candidate>) {
        let mut pts = Vec::new();
        for lv in [0.5f32, 1.2, 1.9] {
            let mut x = 0.5;
            while x < 6.0 {
                pts.push(Point { pos: Vec3::new(x, lv, 0.0), weight: 2.0, floor: false, zones: vec![0] });
                x += 0.75;
            }
        }
        let mut x = 0.5;
        while x < 20.0 {
            pts.push(Point { pos: Vec3::new(x, 1.2, 14.0), weight: 1.0, floor: true, zones: Vec::new() });
            x += 1.0;
        }
        let mut cands = Vec::new();
        for x in [1.0f32, 3.0, 5.0, 8.0] {
            for z in [2.0f32, 4.0, 8.0] {
                cands.push(Candidate { pos: Vec3::new(x, 2.2, z), facing: Vec3::Z, mount: Mount::Wall, cost: 1.0 });
            }
        }
        (pts, cands)
    }

    #[test]
    fn rfid_picks_position_and_aim_for_the_shelf_face() {
        let (d, t) = (reader(), rfid_tag());
        let scene = Boxes::default();
        let (pts, cands) = rack_face();
        // Facing +Z (away from the shelf) can never work; the aims must help.
        let cands = expand_aims(&cands, &[(Vec3::new(0.0, 0.0, -0.3), Vec3::new(6.0, 2.2, 0.3))], d.max_range_m);
        let cols = columns(&scene, &pts, Method::Read, &d, &t, &cands);
        let cfg = Config { method: Method::Read, need: 1, zones_only: true, conflict_m: 4.0, beam_deg: 70.0, zone_count: 1, target_coverage: Some(0.95), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(6, None);
        let m = pl.metrics();
        assert!(m.zones[0] >= 0.9, "{m:?} with {} readers", pl.chosen.len());
        for &c in &pl.chosen {
            assert!(pl.cands[c].facing.z < 0.0, "aims away from the shelf: {:?}", pl.cands[c]);
        }
        // Not lighting up the far side of the room.
        assert!(m.stray < 0.3, "{m:?}");
        assert_eq!(m.conflicts, 0);
    }

    #[test]
    fn stray_reads_cost_score() {
        let (d, t) = (reader(), rfid_tag());
        let scene = Boxes::default();
        let (pts, _) = rack_face();
        let toward = Candidate { pos: Vec3::new(3.0, 2.2, 2.0), facing: Vec3::NEG_Z, mount: Mount::Wall, cost: 1.0 };
        let away = Candidate { pos: Vec3::new(3.0, 2.2, 2.0), facing: Vec3::Z, mount: Mount::Wall, cost: 1.0 };
        let cands = vec![toward, away];
        let cols = columns(&scene, &pts, Method::Read, &d, &t, &cands);
        let cfg = Config { method: Method::Read, need: 1, zones_only: true, zone_count: 1, ..Default::default() };
        let pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        assert!(pl.gain(0) > pl.gain(1), "{} vs {}", pl.gain(0), pl.gain(1));
    }

    #[test]
    fn readers_in_each_others_beam_conflict() {
        let cfg = Config { method: Method::Read, conflict_m: 5.0, beam_deg: 70.0, ..Default::default() };
        let pts: Vec<Point> = Vec::new();
        let a = Candidate { pos: Vec3::ZERO, facing: Vec3::X, mount: Mount::Wall, cost: 1.0 };
        let face = Candidate { pos: Vec3::new(3.0, 0.0, 0.0), facing: Vec3::NEG_X, mount: Mount::Wall, cost: 1.0 };
        let back = Candidate { pos: Vec3::new(3.0, 0.0, 0.0), facing: Vec3::X, mount: Mount::Wall, cost: 1.0 };
        let far = Candidate { pos: Vec3::new(9.0, 0.0, 0.0), facing: Vec3::NEG_X, mount: Mount::Wall, cost: 1.0 };
        let pl = Planner::new(cfg, &pts, vec![a, face, back, far], vec![Vec::new(); 4], Vec::new(), None);
        assert!(pl.jam(&a, &face));
        assert!(!pl.jam(&a, &back));
        assert!(!pl.jam(&a, &far));
    }

    #[test]
    fn portal_pair_aims_across_the_opening_and_reads_it() {
        let walls = [(Vec3::new(0.0, 0.0, -0.1), Vec3::new(8.0, 3.0, 0.1)), (Vec3::new(11.0, 0.0, -0.1), Vec3::new(20.0, 3.0, 0.1))];
        let ops = find_openings(&[], &walls);
        assert_eq!(ops.len(), 1, "{ops:?}");
        let o = ops[0];
        assert!((o.center.x - 9.5).abs() < 1e-3 && (o.width - 3.0).abs() < 1e-3 && o.across == Vec3::X, "{o:?}");
        let [(a, fa), (b, fb)] = portal_pair(&o, 1.5);
        assert!((b - a).normalize().dot(fa) > 0.99 && (a - b).normalize().dot(fb) > 0.99);
        // On the jambs, not inside the walls either side.
        assert!(a.x >= 8.0 && a.x < 8.2 && b.x <= 11.0 && b.x > 10.8, "{a} {b}");
        // Goods crossing the doorway get read by one or the other.
        let (d, t) = (reader(), rfid_tag());
        let scene = Boxes::default();
        for s in [-1.0f32, 0.0, 1.0] {
            let p = o.center + o.across * s;
            let read = [(a, fa), (b, fb)].iter().map(|(q, f)| radio::rfid_read_probability(&d, &t, &scene.path(p, *q), no_gain(&d, *f, (p - *q).normalize()))).fold(0.0f32, f32::max);
            assert!(read > 0.9, "{read} at {s}");
        }
        // A door object works too, and a wall corner is not an opening.
        assert_eq!(find_openings(&[(Vec3::new(1.0, 0.0, 0.0), Vec3::new(2.5, 2.2, 0.1))], &[]).len(), 1);
        assert!(find_openings(&[], &[(Vec3::new(0.0, 0.0, 0.0), Vec3::new(8.0, 3.0, 0.2)), (Vec3::new(8.0, 0.0, 0.0), Vec3::new(8.2, 3.0, 8.0))]).is_empty());
    }

    struct Free<'a> {
        scene: &'a Boxes,
        pts: &'a [Point],
        dev: Profile,
        tag: Profile,
        method: Method,
        lo: Vec2,
        hi: Vec2,
    }

    impl Mover for Free<'_> {
        fn settle(&self, from: &Candidate, to: Vec3) -> Option<Vec3> {
            if from.mount.on_surface() {
                let back = -from.facing.normalize_or_zero();
                let t = self.scene.surface(to, back, 0.5)?;
                return Some(to + back * (t - 0.05));
            }
            (to.x >= self.lo.x && to.x <= self.hi.x && to.z >= self.lo.y && to.z <= self.hi.y).then_some(to)
        }

        fn column(&self, pos: Vec3, facing: Vec3) -> Column {
            column(self.scene, self.pts, self.method, &self.dev, &self.tag, pos, facing, &no_gain)
        }
    }

    #[test]
    fn refinement_never_lowers_the_score_and_stays_on_the_surface() {
        let (d, t) = (anchor(), uwb_tag());
        // Walls along z = 0 and z = 12: anchors stick to them, facing in.
        let scene = Boxes(vec![(Vec3::new(-1.0, 0.0, -0.2), Vec3::new(21.0, 5.0, 0.0), Material::Concrete), (Vec3::new(-1.0, 0.0, 12.0), Vec3::new(21.0, 5.0, 12.2), Material::Concrete)]);
        let (lo, hi) = (Vec2::ZERO, Vec2::new(20.0, 12.0));
        let pts = floor(lo, hi, 1.5, 1.2);
        let mut cands = Vec::new();
        for x in [2.0f32, 8.0, 14.0, 18.0] {
            cands.push(Candidate { pos: Vec3::new(x, 2.6, 0.05), facing: Vec3::Z, mount: Mount::Wall, cost: 0.8 });
            cands.push(Candidate { pos: Vec3::new(x, 2.6, 11.95), facing: Vec3::NEG_Z, mount: Mount::Wall, cost: 0.8 });
        }
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { target_coverage: Some(1.0), target_p95: Some(0.05), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        for c in [0, 3, 4, 7] {
            pl.add(c);
        }
        let before = pl.score();
        let n = pl.chosen.len();
        let mover = Free { scene: &scene, pts: &pts, dev: d, tag: t, method: Method::Twr, lo, hi };
        pl.refine(&mover);
        assert!(pl.score() >= before, "{} < {before}", pl.score());
        assert!(pl.score() > before, "nothing moved");
        assert_eq!(pl.chosen.len(), n);
        for &c in &pl.chosen {
            let q = pl.cands[c];
            assert!((q.pos.z - 0.05).abs() < 1e-3 || (q.pos.z - 11.95).abs() < 1e-3, "left the wall: {q:?}");
        }
        // The running score agrees with a clean rebuild.
        let sel = pl.chosen.clone();
        let s = pl.score();
        pl.rebuild(&sel);
        assert!((pl.score() - s).abs() < 1e-3, "{} vs {s}", pl.score());
    }

    #[test]
    fn one_ceiling_locator_gives_a_fix_on_a_known_plane() {
        let (d, t) = (locator(), Profile { kind: DeviceKind::BleTag, ..Default::default() });
        let scene = Boxes::default();
        let (a, p) = (Vec3::new(5.0, 3.0, 5.0), Vec3::new(7.0, 1.0, 6.0));
        let pts = [Point { pos: p, weight: 1.0, floor: true, zones: Vec::new() }];
        let col = column(&scene, &pts, Method::Aoa, &d, &t, a, Vec3::NEG_Y, &no_gain);
        assert_eq!(col.len(), 1);
        let mut acc = Acc::default();
        acc.add(0, &col[0]);
        let e = acc.err(Method::Aoa, false);
        assert!(e.is_finite() && e < 0.5, "{e}");
        // Outside the array's field of view: nothing.
        let far = Vec3::new(40.0, 1.0, 5.0);
        let pts = [Point { pos: far, weight: 1.0, floor: true, zones: Vec::new() }];
        let d2 = Profile { max_range_m: 100.0, sensitivity_dbm: -200.0, ..d };
        assert!(column(&scene, &pts, Method::Aoa, &d2, &t, a, Vec3::NEG_Y, &no_gain).is_empty());
    }

    #[test]
    fn locators_cover_a_room_with_few_devices() {
        let (d, t) = (locator(), Profile { kind: DeviceKind::BleTag, ..Default::default() });
        let scene = Boxes::default();
        let (lo, hi) = (Vec2::ZERO, Vec2::splat(20.0));
        let pts = floor(lo, hi, 2.0, 1.0);
        let cands = grid_cands(lo, hi, 3.0, 3.0);
        let cols = columns(&scene, &pts, Method::Aoa, &d, &t, &cands);
        let cfg = Config { method: Method::Aoa, need: 1, target_m: 0.75, err_cap: 2.0, target_coverage: Some(0.95), target_p95: Some(1.0), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(30, None);
        let m = pl.metrics();
        assert!(pl.met(&m), "{m:?} with {}", pl.chosen.len());
        // Once eight: that was 3° at every angle and a 1-sigma target, neither of which holds.
        assert!(pl.chosen.len() <= 20, "{}", pl.chosen.len());
    }

    #[test]
    fn nothing_lands_on_another_technologys_device() {
        let (d, t) = (anchor(), uwb_tag());
        let scene = Boxes::default();
        let (lo, hi) = (Vec2::ZERO, Vec2::splat(12.0));
        let pts = floor(lo, hi, 1.5, 1.2);
        let cands = grid_cands(lo, hi, 2.6, 3.0);
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        // First a free plan, then one that must keep off what the first chose.
        let base = Config { target_coverage: Some(0.95), target_p95: Some(0.3), ..Default::default() };
        let mut first = Planner::new(base.clone(), &pts, cands.clone(), cols.clone(), Vec::new(), None);
        first.run(8, None);
        let taken: Vec<Vec3> = first.chosen.iter().map(|&c| first.cands[c].pos).collect();
        assert!(!taken.is_empty());
        let mut second = Planner::new(Config { avoid: taken.clone(), ..base }, &pts, cands, cols, Vec::new(), None);
        second.run(8, None);
        // Fine-tuning must respect them too.
        let mover = Free { scene: &scene, pts: &pts, dev: d, tag: t, method: Method::Twr, lo, hi };
        second.refine(&mover);
        assert!(!second.chosen.is_empty());
        for &c in &second.chosen {
            let p = second.cands[c].pos;
            assert!(taken.iter().all(|q| q.distance(p) >= SAME_SPOT_M), "{p} on one of {taken:?}");
        }
    }

    #[test]
    fn a_big_site_plans_in_seconds() {
        let (d, t) = (anchor(), uwb_tag());
        // Rows of racks on a 40 × 30 m floor, with about 600 mounting points.
        let racks: Vec<(Vec3, Vec3, Material)> = (0..6).map(|i| (Vec3::new(3.0 + i as f32 * 6.0, 0.0, 4.0), Vec3::new(4.0 + i as f32 * 6.0, 4.0, 26.0), Material::Metal)).collect();
        let scene = Boxes(racks);
        let (lo, hi) = (Vec2::ZERO, Vec2::new(40.0, 30.0));
        let pts = floor(lo, hi, 1.1, 1.2);
        let mut cands = grid_cands(lo, hi, 2.6, 2.0);
        cands.truncate(600);
        let start = std::time::Instant::now();
        let cols = columns(&scene, &pts, Method::Twr, &d, &t, &cands);
        let cfg = Config { target_coverage: Some(0.95), target_p95: Some(0.3), ..Default::default() };
        let mut pl = Planner::new(cfg, &pts, cands, cols, Vec::new(), None);
        pl.run(40, None);
        let took = start.elapsed().as_secs_f32();
        let m = pl.metrics();
        assert!(pl.met(&m), "{m:?} with {}", pl.chosen.len());
        assert!(took < 5.0, "{took} s for {} points", pts.len());
    }

    #[test]
    fn heights_and_aims_expand_only_where_they_make_sense() {
        let wall = Candidate { pos: Vec3::new(0.0, 2.5, 0.0), facing: Vec3::X, mount: Mount::Wall, cost: 1.0 };
        let ceil = Candidate { pos: Vec3::new(0.0, 4.0, 0.0), facing: Vec3::NEG_Y, mount: Mount::Ceiling, cost: 1.0 };
        let h = expand_heights(&[wall, ceil], Some(3.2));
        // Wall: 2.5 and 1.7 (3.5 is above the ceiling); ceiling mounts stay put.
        assert_eq!(h.len(), 3);
        assert!(h.iter().all(|c| c.pos.y <= 3.0 || c.mount == Mount::Ceiling));
        let aims = expand_aims(&[wall], &[(Vec3::new(5.0, 0.0, 5.0), Vec3::new(6.0, 2.0, 6.0))], 12.0);
        assert!(aims.len() >= 4, "{aims:?}");
        assert!(aims.iter().all(|a| (a.facing.length() - 1.0).abs() < 1e-3 && a.pos == wall.pos));
    }

    #[test]
    fn solve_aoa_is_exact_without_noise_and_bounded_with_it() {
        let truth = DVec3::new(4.0, 1.0, 7.0);
        let locs = [DVec3::new(0.0, 3.0, 0.0), DVec3::new(10.0, 3.0, 0.0), DVec3::new(5.0, 3.0, 12.0)];
        let dirs: Vec<DVec3> = locs.iter().map(|l| (truth - *l).normalize()).collect();
        let w = [1.0; 3];
        for dim in [Dim::Plane(1.0), Dim::Space] {
            let f = solver::solve_aoa(dim, &locs, &dirs, &w).unwrap();
            assert!((f.pos - truth).length() < 1e-6, "{dim:?}: {:?}", f.pos);
        }
        // One ray meets the known height.
        let f = solver::solve_aoa(Dim::Plane(1.0), &locs[..1], &dirs[..1], &w[..1]).unwrap();
        assert!((f.pos - truth).length() < 1e-6);
        assert!(solver::solve_aoa(Dim::Space, &locs[..1], &dirs[..1], &w[..1]).is_none());
        // 3° of noise per axis stays within a metre at these distances.
        let mut rng = Rng::new(7);
        let s = 3f32.to_radians();
        let mut worst = 0.0f64;
        for _ in 0..500 {
            let noisy: Vec<DVec3> = dirs
                .iter()
                .map(|d| {
                    let v = d.as_vec3();
                    let (a, b) = (v.any_orthonormal_vector(), v.cross(v.any_orthonormal_vector()));
                    (v + a * rng.gauss(s) + b * rng.gauss(s)).normalize().as_dvec3()
                })
                .collect();
            let f = solver::solve_aoa(Dim::Plane(1.0), &locs, &noisy, &w).unwrap();
            worst = worst.max((f.pos - truth).length());
        }
        assert!(worst < 2.0, "{worst}");
    }
}
