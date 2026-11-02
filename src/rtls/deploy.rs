//! One-button deployment: from the scene alone, place a complete positioning
//! system (UWB anchors, BLE beacons or AoA locators, WiFi access points, RFID
//! readers on racks, zones and doorways, tags on vehicles and goods, workers
//! walking the aisles), then simulate it, add devices where the simulation
//! measures badly, and report how well it works. Re-running replaces what it
//! placed before.

use std::collections::{BinaryHeap, HashMap};

use bevy::prelude::*;

use super::device::{Device, DeviceKind, Motion, Profile, Profiles, Zone, epc_for, next_id};
use super::placement::{self, Mount};
use super::planning::{self, CoverageRequest, OptimizeRequest, Tech, blocked};
use super::{RtlsConfig, RtlsState, sim};
use crate::scene::PlacedObject;

#[derive(Clone, Debug)]
pub struct DeployRequest {
    pub uwb: bool,
    pub ble: bool,
    pub rfid: bool,
    /// BLE with direction-finding locators instead of RSSI beacons.
    pub ble_aoa: bool,
    pub wifi: bool,
    /// Target 95th-percentile position error (m) for UWB.
    pub target_m: f32,
    pub tag_height: f32,
    pub workers: usize,
    /// Simulated seconds of the acceptance test afterwards.
    pub test_s: f64,
    /// Mounting points (position, facing) on real structure; `None` = walls + grid.
    pub mounts: Option<Vec<(Vec3, Vec3)>>,
    /// Times to add devices where the simulated UWB p95 misses the target.
    pub retries: u8,
    /// Simulated UWB p95 that counts as success (defaults to `target_m`).
    pub accept_m: Option<f32>,
    /// Most UWB anchors in the scene (yours included); `None` = as few as meet the target.
    pub max_anchors: Option<usize>,
    /// UWB anchor transmit power (dBm/MHz) to plan and simulate with; set on
    /// the default anchor profile. `None` = keep the profile's.
    pub anchor_tx_dbm_mhz: Option<f32>,
    /// More tag heights to plan for (besides `tag_height`), e.g. forklifts at 0.3 m.
    pub heights: Vec<f32>,
    /// Plan for full 3D fixes (and have the simulation solve in 3D).
    pub dim3: bool,
    /// Keep the target with any one device failed.
    pub redundancy: bool,
    pub min_spacing_m: Option<f32>,
    pub cost_weight: Option<f32>,
    /// Where cables run to: far mounts cost more.
    pub head_end: Option<Vec3>,
    /// Fine-tune positions along their mounting surface.
    pub refine: bool,
    /// Technologies may share a mount (combo devices) at much lower cost,
    /// instead of keeping every device on its own spot.
    pub colocate: bool,
}

impl Default for DeployRequest {
    fn default() -> Self {
        Self {
            uwb: true,
            ble: false,
            rfid: false,
            ble_aoa: false,
            wifi: false,
            target_m: 0.3,
            tag_height: 1.2,
            workers: 2,
            test_s: 30.0,
            mounts: None,
            retries: 3,
            accept_m: None,
            max_anchors: None,
            anchor_tx_dbm_mhz: None,
            heights: Vec::new(),
            dim3: false,
            redundancy: false,
            min_spacing_m: None,
            cost_weight: None,
            head_end: None,
            refine: true,
            colocate: false,
        }
    }
}

/// How one technology's plan came out.
#[derive(Clone, Debug, Default)]
pub struct TechPlan {
    pub tech: &'static str,
    pub devices: usize,
    pub coverage: f32,
    /// What every mounting point together could cover.
    pub reachable_coverage: f32,
    /// Predicted p95 error (m), no-fix points counted; infinite where it means nothing (RFID)
    /// or past 5 % of the floor without a position.
    pub p95_m: f32,
    pub cost: f32,
    pub targets_met: bool,
}

/// How tags of one motion class did in the test. A parked tag can settle on an estimate and a
/// moving one can't, so they are never pooled: a fleet of parked tags would hide the walkers.
#[derive(Clone, Debug, Default)]
pub struct ClassResult {
    pub tags: usize,
    /// Position updates tried.
    pub attempts: u64,
    /// Those that got a position.
    pub fixes: u64,
    /// RMS of those positions (m).
    pub rmse_m: Option<f32>,
    /// How far 95 % of the positions land (m), the updates without one left out.
    pub fix_p95_m: Option<f32>,
    /// How far 95 % of the updates land, one without a position counting as a miss: infinite
    /// when more than 5 % had none.
    pub p95_m: Option<f32>,
}

impl ClassResult {
    fn of(st: &RtlsState, tech: &str, moving: bool) -> Self {
        let r = st.run_stats(Some(tech), Some(moving));
        let tags = st.tracks.values().filter(|t| t.tech == tech && (if moving { t.moving_epochs } else { t.parked_epochs }) > 1).count();
        let e = r.stats();
        Self { tags, attempts: r.attempts, fixes: r.fixes, rmse_m: e.map(|e| e.rmse), fix_p95_m: e.map(|e| e.cep95), p95_m: r.p95_counting_misses() }
    }

    pub fn availability(&self) -> Option<f32> {
        (self.attempts > 0).then(|| self.fixes as f32 / self.attempts as f32)
    }
}

/// One technology's simulated accuracy against its own target.
#[derive(Clone, Debug, Default)]
pub struct TechAccuracy {
    pub tech: &'static str,
    /// How far 95 % of positions must land (m).
    pub target_m: f32,
    pub moving: ClassResult,
    pub parked: ClassResult,
}

impl TechAccuracy {
    pub fn measure(st: &RtlsState, tech: &'static str, target_m: f32) -> Self {
        Self { tech, target_m, moving: ClassResult::of(st, tech, true), parked: ClassResult::of(st, tech, false) }
    }

    fn classes(&self) -> impl Iterator<Item = &ClassResult> {
        [&self.moving, &self.parked].into_iter().filter(|c| c.attempts > 0)
    }

    /// Every class that was tested holds the target. Nothing tested holds nothing.
    pub fn passed(&self) -> bool {
        self.classes().count() > 0 && self.classes().all(|c| c.p95_m.is_some_and(|p| p <= self.target_m))
    }

    /// The worse class: infinite when one lost too many updates, 0 when nothing was tested.
    pub fn worst_p95_m(&self) -> f32 {
        self.classes().filter_map(|c| c.p95_m).fold(0.0, f32::max)
    }
}

/// Below this anchor reach (m) a UWB plan can't work in a real room.
pub const MIN_USEFUL_RANGE_M: f32 = 4.0;

#[derive(Clone, Debug, Default)]
pub struct DeployReport {
    pub anchors: usize,
    pub ble_nodes: usize,
    /// Of those, direction-finding locators.
    pub ble_locators: usize,
    pub wifi_aps: usize,
    pub readers: usize,
    /// RFID readers on doorways and docks.
    pub portal_readers: usize,
    /// Each technology's plan.
    pub plans: Vec<TechPlan>,
    /// Devices added because of what the simulation measured.
    pub feedback_added: usize,
    pub tags: usize,
    pub zones_created: usize,
    pub racks: usize,
    /// Anchors mounted on racks.
    pub on_racks: usize,
    pub shelf_readers: usize,
    pub inventory_tags: usize,
    /// Share of walkable floor with a usable UWB fix: at least 3 anchors heard, in line of
    /// sight or not, and 95 % of readings predicted within 3× the target.
    pub coverage: f32,
    /// What the UWB plan's mounting points could cover at most.
    pub reachable_coverage: f32,
    /// Zones reaching the coverage asked for with UWB, of those with floor.
    pub zones_covered: usize,
    pub zones_total: usize,
    pub predicted_p95_m: f32,
    /// What was asked for was met, in the plans and in the simulated test.
    pub targets_met: bool,
    /// Simulated, whole test, every technology. 0 with no fixes.
    pub rmse_m: f32,
    /// The worst technology's and class's 95 % radius, no-fix counted as a miss: infinite when
    /// more than 5 % of updates had none, 0 with nothing tested.
    pub cep95_m: f32,
    /// The same for UWB (what the UWB target applies to).
    pub uwb_rmse_m: f32,
    pub uwb_cep95_m: f32,
    /// Each positioning technology's test, moving and parked tags apart.
    pub accuracy: Vec<TechAccuracy>,
    /// How long the test ran (s).
    pub test_s: f64,
    /// Tags with a position or a read at some point in the test.
    pub found: u32,
    /// Tags located at the last snapshot of the test.
    pub located: u32,
    pub total_tags: u32,
    pub rfid_unread: Vec<String>,
    /// RFID tags in the test.
    pub rfid_tags: usize,
    /// Mean chance one inventory round reads an RFID tag.
    pub rfid_read_chance: f32,
    pub alarms: Vec<String>,
    pub notes: Vec<String>,
    /// Accuracy target the result is judged against (m).
    pub target_m: f32,
    /// How far one UWB anchor reaches at the chosen TX power (m, 0 = no UWB).
    pub anchor_reach_m: f32,
    /// The anchor limit, when it is what held the plan back.
    pub capped_at: Option<usize>,
    /// Zones where most of the floor can't see 3 anchors.
    pub weak_zones: Vec<String>,
}

/// "35 cm" or "6.4 m".
pub fn dist(m: f32) -> String {
    if !m.is_finite() {
        "–".into()
    } else if m < 1.0 {
        format!("{:.0} cm", m * 100.0)
    } else {
        format!("{m:.1} m")
    }
}

/// How a result reads at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    Almost,
    Bad,
}

impl DeployReport {
    /// The result in a line: the number the simulation measured, never the target in its place.
    pub fn verdict(&self) -> (Verdict, String) {
        let uwb = self.anchor_reach_m > 0.0;
        if uwb && self.anchor_reach_m < MIN_USEFUL_RANGE_M {
            return (Verdict::Bad, "Anchors are too weak to position anything".into());
        }
        if uwb && self.anchors == 0 {
            return (Verdict::Bad, "No anchors were placed".into());
        }
        let main = self.accuracy.first();
        let rfid_line = |what: &str| format!("{what}{} of {} RFID tags read within {:.0} s (simulated)", self.rfid_tags - self.rfid_unread.len(), self.rfid_tags, self.test_s);
        if self.targets_met {
            return match main {
                Some(a) => (Verdict::Good, format!("Good: 95% of simulated {} positions within {} (target {})", a.tech.to_uppercase(), dist(a.worst_p95_m()), dist(a.target_m))),
                None if self.rfid_tags > 0 => (Verdict::Good, rfid_line("Good: ")),
                None => (Verdict::Good, "Good: the plan meets its targets (no tags were simulated)".into()),
            };
        }
        let covered = self.plans.first().map_or(self.coverage, |p| p.coverage);
        let level = if covered >= 0.8 { Verdict::Almost } else { Verdict::Bad };
        match main {
            Some(a) if !a.classes().any(|c| c.fixes > 0) => (Verdict::Bad, format!("No {} tag got a position in the simulation", a.tech.to_uppercase())),
            Some(a) if covered >= 0.8 => (level, format!("Almost: not every spot reaches {}", dist(a.target_m))),
            Some(a) => (level, format!("Not accurate enough for {}", dist(a.target_m))),
            None if self.rfid_tags > 0 => (level, rfid_line("Short of the target: ")),
            None => (level, "The plan doesn't meet its targets".into()),
        }
    }

    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        if self.anchors > 0 {
            if self.on_racks > 0 {
                parts.push(format!("{} UWB anchors ({} on racks)", self.anchors, self.on_racks));
            } else {
                parts.push(format!("{} UWB anchors", self.anchors));
            }
        }
        if self.ble_locators > 0 {
            parts.push(format!("{} BLE AoA locators", self.ble_locators));
        } else if self.ble_nodes > 0 {
            parts.push(format!("{} BLE beacons", self.ble_nodes));
        }
        if self.wifi_aps > 0 {
            parts.push(format!("{} WiFi APs", self.wifi_aps));
        }
        if self.readers > 0 {
            if self.shelf_readers > 0 {
                parts.push(format!("{} RFID readers ({} smart-shelf)", self.readers, self.shelf_readers));
            } else {
                parts.push(format!("{} RFID readers", self.readers));
            }
        }
        parts.push(format!("{} tags", self.tags));
        parts.join(" · ")
    }
}

const VEHICLES: [&str; 8] = ["forklift", "cart", "trolley", "agv", "robot", "vehicle", "truck", "jack"];
const GOODS: [&str; 12] = ["pallet", "box", "crate", "tote", "container", "bin", "package", "carton", "drum", "barrel", "case", "bag"];

/// Whole-word match on name tokens (`pallet_rackable_blue_a01_1`), so "cabinet" is not a "bin".
fn matches(name: &str, words: &[&str]) -> bool {
    let n = name.to_ascii_lowercase();
    n.split(|c: char| !c.is_ascii_alphabetic()).any(|tok| words.iter().any(|w| tok == *w || tok.strip_suffix('s') == Some(w) || tok.strip_suffix("es") == Some(w)))
}

const DOORS: [&str; 7] = ["door", "doorway", "dock", "gate", "portal", "entrance", "shutter"];
const RACKS: [&str; 12] = ["shelf", "shelves", "shelving", "rack", "racking", "cabinet", "locker", "storage", "bookcase", "bookshelf", "gondola", "cupboard"];

/// Shelving that holds stock (by name, or simply tall and deep enough).
fn is_rack(o: &Obj) -> bool {
    let s = o.max - o.min;
    if o.structure || matches(&o.name, &VEHICLES) {
        return false;
    }
    matches(&o.name, &RACKS) || (s.y >= 1.5 && s.x * s.z >= 0.4 && !matches(&o.name, &GOODS))
}

/// Mounting points on racks: along the top (facing down) and on both long
/// faces at the anchor mount height (facing the aisle).
fn rack_mounts(racks: &[&Obj], mount_h: f32) -> Vec<(Vec3, Vec3)> {
    let mut v = Vec::new();
    for r in racks {
        let size = r.max - r.min;
        let along_x = size.x >= size.z;
        let len = if along_x { size.x } else { size.z };
        let n = ((len / 2.5).ceil() as usize).max(1);
        let mid = (r.min + r.max) * 0.5;
        for k in 0..n {
            let s = (k as f32 + 0.5) / n as f32;
            let a = if along_x { r.min.x + size.x * s } else { r.min.z + size.z * s };
            let (x, z) = if along_x { (a, mid.z) } else { (mid.x, a) };
            if r.max.y < 6.0 {
                v.push((Vec3::new(x, r.max.y + 0.12, z), Vec3::NEG_Y));
            }
            let y = mount_h.min(r.max.y - 0.1).max(r.min.y + 0.5);
            let out = if along_x { Vec3::Z } else { Vec3::X };
            let half = if along_x { size.z * 0.5 } else { size.x * 0.5 } + 0.08;
            let c = Vec3::new(x, y, z);
            v.push((c + out * half, out));
            v.push((c - out * half, -out));
        }
    }
    v
}

struct Obj {
    name: String,
    min: Vec3,
    max: Vec3,
    /// Building shell piece (floor, wall, column…), never a rack or goods.
    structure: bool,
    /// The building floor slab: bounds the area to deploy in.
    floor: bool,
    wall: bool,
}

fn objects(world: &mut World) -> Vec<Obj> {
    let mut q = world.query::<(&PlacedObject, &GlobalTransform)>();
    let mut list: Vec<Obj> = q
        .iter(world)
        .map(|(o, gt)| {
            let (a, b) = o.aabb();
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            for x in [a.x, b.x] {
                for y in [a.y, b.y] {
                    for z in [a.z, b.z] {
                        let p = gt.transform_point(Vec3::new(x, y, z));
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                }
            }
            let kind = crate::agent::structures::Kind::of_asset(&o.asset);
            Obj { name: o.name.clone(), min: lo, max: hi, structure: kind.is_some(), floor: kind == Some(crate::agent::structures::Kind::Floor), wall: kind == Some(crate::agent::structures::Kind::Wall) }
        })
        .collect();
    // Objects spawn in the order their assets finished loading: a plan and its
    // test must not depend on that.
    list.sort_by(|a, b| a.name.cmp(&b.name).then(a.min.to_array().partial_cmp(&b.min.to_array()).unwrap_or(std::cmp::Ordering::Equal)));
    list
}

fn spawn(world: &mut World, prof: &Profile, at: Transform, motion: Motion, address: Option<String>) -> String {
    let ids: Vec<String> = {
        let mut q = world.query::<&Device>();
        q.iter(world).map(|d| d.id.clone()).collect()
    };
    let id = next_id(prof.kind, ids.iter().map(String::as_str));
    let address = address.unwrap_or_else(|| if prof.kind == DeviceKind::RfidTag { epc_for(&id) } else { String::new() });
    world.spawn((
        Name::new(format!("{} {id}", prof.kind.label())),
        Device { id: id.clone(), kind: prof.kind, profile: prof.id.clone(), enabled: true, motion, address, auto: true, radio: Default::default() },
        at,
    ));
    id
}

fn facing(pos: Vec3, dir: Vec3) -> Transform {
    let dir = dir.normalize_or(Vec3::NEG_Z);
    let up = if dir.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
    Transform::from_translation(pos).looking_to(dir, up)
}

/// Walkable grid over the area.
pub struct FreeGrid {
    pub lo: Vec2,
    pub cell: f32,
    pub nx: usize,
    pub nz: usize,
    pub free: Vec<bool>,
}

impl FreeGrid {
    pub fn new(lo: Vec2, hi: Vec2, cell: f32, boxes: &[(Vec3, Vec3)], margin: f32) -> Self {
        let nx = ((hi.x - lo.x) / cell).ceil().max(1.0) as usize;
        let nz = ((hi.y - lo.y) / cell).ceil().max(1.0) as usize;
        let mut free = Vec::with_capacity(nx * nz);
        for iz in 0..nz {
            for ix in 0..nx {
                let p = lo + Vec2::new((ix as f32 + 0.5) * cell, (iz as f32 + 0.5) * cell);
                free.push(!blocked(boxes, p, margin));
            }
        }
        Self { lo, cell, nx, nz, free }
    }
    pub fn center(&self, ix: usize, iz: usize) -> Vec2 {
        self.lo + Vec2::new((ix as f32 + 0.5) * self.cell, (iz as f32 + 0.5) * self.cell)
    }
    fn is_free(&self, ix: i64, iz: i64) -> bool {
        ix >= 0 && iz >= 0 && (ix as usize) < self.nx && (iz as usize) < self.nz && self.free[iz as usize * self.nx + ix as usize]
    }
    /// Free cell closest to `p`.
    pub fn nearest_free(&self, p: Vec2) -> Option<(usize, usize)> {
        let mut best: Option<(f32, (usize, usize))> = None;
        for iz in 0..self.nz {
            for ix in 0..self.nx {
                if self.free[iz * self.nx + ix] {
                    let d = self.center(ix, iz).distance_squared(p);
                    if best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, (ix, iz)));
                    }
                }
            }
        }
        best.map(|(_, c)| c)
    }
    /// A* (8-connected, no corner cutting). Returns cell centres.
    pub fn path(&self, from: (usize, usize), to: (usize, usize)) -> Option<Vec<Vec2>> {
        #[derive(PartialEq)]
        struct N(f32, (i64, i64));
        impl Eq for N {}
        impl PartialOrd for N {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for N {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                o.0.total_cmp(&self.0)
            }
        }
        let goal = (to.0 as i64, to.1 as i64);
        let h = |c: (i64, i64)| (((c.0 - goal.0).pow(2) + (c.1 - goal.1).pow(2)) as f32).sqrt();
        let start = (from.0 as i64, from.1 as i64);
        let mut open = BinaryHeap::new();
        let mut g: HashMap<(i64, i64), f32> = HashMap::new();
        let mut came: HashMap<(i64, i64), (i64, i64)> = HashMap::new();
        g.insert(start, 0.0);
        open.push(N(h(start), start));
        while let Some(N(_, c)) = open.pop() {
            if c == goal {
                let mut out = vec![c];
                let mut cur = c;
                while let Some(p) = came.get(&cur) {
                    cur = *p;
                    out.push(cur);
                }
                out.reverse();
                return Some(out.into_iter().map(|(x, z)| self.center(x as usize, z as usize)).collect());
            }
            let gc = g[&c];
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let n = (c.0 + dx, c.1 + dz);
                if !self.is_free(n.0, n.1) || (dx != 0 && dz != 0 && (!self.is_free(c.0 + dx, c.1) || !self.is_free(c.0, c.1 + dz))) {
                    continue;
                }
                let cost = gc + if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
                if g.get(&n).is_none_or(|old| cost < *old) {
                    g.insert(n, cost);
                    came.insert(n, c);
                    open.push(N(cost + h(n), n));
                }
            }
        }
        None
    }
}

/// Drops points that lie on a straight line between their neighbours.
pub fn simplify(pts: &[Vec2]) -> Vec<Vec2> {
    // Legs joined end to end repeat each corner, and a repeat looks straight on both sides.
    let mut pts = pts.to_vec();
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut out = vec![pts[0]];
    for w in pts.windows(3) {
        let (a, b, c) = (w[0], w[1], w[2]);
        if (b - a).perp_dot(c - b).abs() > 1e-4 {
            out.push(b);
        }
    }
    out.push(*pts.last().unwrap());
    out
}

/// Clusters object footprints that are within `gap` of each other.
fn clusters(objs: &[&Obj], gap: f32) -> Vec<(Vec3, Vec3)> {
    let n = objs.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (objs[i], objs[j]);
            let near = a.min.x - gap <= b.max.x && b.min.x - gap <= a.max.x && a.min.z - gap <= b.max.z && b.min.z - gap <= a.max.z;
            if near {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut groups: HashMap<usize, (Vec3, Vec3)> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        let e = groups.entry(r).or_insert((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)));
        e.0 = e.0.min(objs[i].min);
        e.1 = e.1.max(objs[i].max);
    }
    let mut v: Vec<(Vec3, Vec3)> = groups.into_values().collect();
    let order = |a: &(Vec3, Vec3)| [a.0.x, a.0.z, a.1.x, a.1.z];
    v.sort_by(|a, b| ((b.1.x - b.0.x) * (b.1.z - b.0.z)).total_cmp(&((a.1.x - a.0.x) * (a.1.z - a.0.z))).then(order(a).partial_cmp(&order(b)).unwrap_or(std::cmp::Ordering::Equal)));
    v
}

/// Of two plans on different mounting points: one that meets the targets beats
/// one that doesn't, then the fewest devices, then the cheapest to install.
/// If neither does, the one covering the most.
/// `targets_met` is judged against what was asked, so a tier that can only reach half the floor
/// never gets to compete on device count.
fn better_plan(r: &planning::OptimizeResult, b: &planning::OptimizeResult) -> bool {
    let zone_cov = |r: &planning::OptimizeResult| r.zone_coverage.iter().filter(|z| !z.is_nan()).sum::<f32>();
    match (r.targets_met, b.targets_met) {
        (true, true) => r.chosen.len() < b.chosen.len() || (r.chosen.len() == b.chosen.len() && r.cost < b.cost - 1e-3),
        (true, false) => true,
        (false, true) => false,
        (false, false) => r.coverage_after + zone_cov(r) > b.coverage_after + zone_cov(b) + 0.02,
    }
}

/// Square areas (min, max) of size `cell` where the 95th percentile of the
/// measured errors misses `limit` (with enough samples to say so). Heights
/// are left to the planner.
pub fn bad_areas(spots: &[(Vec3, f32)], limit: f32, cell: f32) -> Vec<(Vec3, Vec3)> {
    let mut bins: HashMap<(i32, i32), Vec<f32>> = HashMap::new();
    for (p, e) in spots {
        bins.entry(((p.x / cell).floor() as i32, (p.z / cell).floor() as i32)).or_default().push(*e);
    }
    let mut v: Vec<((i32, i32), f32)> = bins
        .into_iter()
        .filter(|(_, e)| e.len() >= 5)
        .filter_map(|(k, mut e)| {
            e.sort_by(f32::total_cmp);
            let p95 = e[((e.len() - 1) as f32 * 0.95).ceil() as usize];
            (p95 > limit).then_some((k, p95))
        })
        .collect();
    // Worst first, then in a fixed order: a plan must not depend on hashing.
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter()
        .take(16)
        .map(|((x, z), _)| (Vec3::new(x as f32 * cell, 0.0, z as f32 * cell), Vec3::new((x + 1) as f32 * cell, 3.0, (z + 1) as f32 * cell)))
        .collect()
}

pub fn auto_deploy(world: &mut World, req: &DeployRequest) -> DeployReport {
    let mut report = DeployReport { test_s: req.test_s, ..Default::default() };
    // What each positioning technology is held to in the simulated test.
    let mut accuracy_targets: Vec<(&'static str, f32)> = Vec::new();

    // ---- Remove what an earlier run placed.
    let old: Vec<Entity> = {
        let mut q = world.query::<(Entity, Option<&Device>, Option<&Zone>)>();
        q.iter(world).filter(|(_, d, z)| d.is_some_and(|d| d.auto) || z.is_some_and(|z| z.auto)).map(|(e, _, _)| e).collect()
    };
    for e in old {
        world.despawn(e);
    }

    // Anchor transmit power: on the profile, so planning and the test agree.
    if let Some(tx) = req.anchor_tx_dbm_mhz {
        let mut profiles = world.resource_mut::<Profiles>();
        if let Some(p) = profiles.0.iter_mut().find(|p| p.kind == DeviceKind::UwbAnchor) {
            p.uwb_tx_dbm_mhz = tx.min(super::radio::UWB_MAX_DBM_MHZ);
            let range = super::radio::uwb_link_range_m(p);
            report.notes.push(format!("Anchor TX power {:.1} dBm/MHz: line-of-sight range {range:.1} m.", p.uwb_tx_dbm_mhz));
        }
    }

    // A plan for height needs a solver that asks for it.
    if req.dim3 {
        world.resource_mut::<RtlsConfig>().settings.solve_3d = true;
        report.notes.push("Planned for full 3D fixes: the simulation now solves height too.".into());
    }

    let objs = objects(world);
    // The floor slab is under everything, not in the way.
    let boxes: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| !(o.floor || o.structure && o.max.y < 0.1)).map(|o| (o.min, o.max)).collect();
    // A building floor bounds the area (as the planner does), so workers walk
    // inside the walls; otherwise the objects, padded.
    let floor = objs.iter().filter(|o| o.floor).fold(None, |a: Option<(Vec2, Vec2)>, o| Some(a.map_or((o.min.xz(), o.max.xz()), |(l, h)| (l.min(o.min.xz()), h.max(o.max.xz())))));
    let (lo, hi) = if let Some(f) = floor {
        f
    } else if boxes.is_empty() {
        (Vec2::splat(-6.0), Vec2::splat(6.0))
    } else {
        let lo = boxes.iter().fold(Vec2::splat(f32::MAX), |a, b| a.min(b.0.xz()));
        let hi = boxes.iter().fold(Vec2::splat(f32::MIN), |a, b| a.max(b.1.xz()));
        (lo - Vec2::splat(1.5), hi + Vec2::splat(1.5))
    };
    let area_c = (lo + hi) * 0.5;
    if boxes.is_empty() {
        report.notes.push("The scene is empty: deployed for a 12 × 12 m floor around the origin.".into());
    }
    let profiles = world.resource::<Profiles>().0.clone();
    let prof = |kind: DeviceKind| profiles.iter().find(|p| p.kind == kind).cloned();
    let prof_id = |id: &str, kind: DeviceKind| profiles.iter().find(|p| p.id == id).cloned().or_else(|| prof(kind));

    // Shelves / racks: devices go ON them (tops, aisle faces) and smart-shelf
    // RFID antennas go IN them.
    let racks: Vec<&Obj> = objs.iter().filter(|o| is_rack(o)).collect();
    report.racks = racks.len();
    let anchor_h = prof(DeviceKind::UwbAnchor).map(|p| p.mount_height_m).unwrap_or(2.5);

    // Mounting points: building structure (ceiling, walls, columns) when the
    // scene has one, the racks, and otherwise the room perimeter and a grid.
    // Mounting points, by preference: the ceiling (anchors 0.3 m under it,
    // facing down, as installers do), then columns and walls, then racks and
    // finally the room perimeter / a grid when there is no building shell.
    let mut column_spots: Vec<Vec3> = Vec::new();
    let (ceiling, structural): (Vec<(Vec3, Vec3)>, Vec<(Vec3, Vec3)>) = match &req.mounts {
        Some(m) => (Vec::new(), m.clone()),
        None => {
            use crate::agent::structures::{MountKind, MountOpts, mount_points};
            let pts = mount_points(world, &MountOpts { mount_height_m: anchor_h, ..Default::default() });
            column_spots = pts.iter().filter(|m| m.kind == MountKind::Column).map(|m| m.pos).collect();
            let (c, s): (Vec<_>, Vec<_>) = pts.into_iter().partition(|m| m.kind == MountKind::Ceiling);
            (c.into_iter().map(|m| (m.pos, m.facing)).collect(), s.into_iter().map(|m| (m.pos, m.facing)).collect())
        }
    };
    let mut structure = ceiling.clone();
    structure.extend(structural.iter().copied());
    let on_racks = rack_mounts(&racks, anchor_h);
    let mut all = structure.clone();
    all.extend(on_racks.iter().copied());
    if structure.is_empty() {
        all.extend(planning::default_candidates(lo, hi, anchor_h));
    }
    let mut tiers: Vec<(&'static str, Vec<(Vec3, Vec3)>)> = Vec::new();
    if !ceiling.is_empty() {
        tiers.push(("the ceiling", ceiling.clone()));
    }
    if !structural.is_empty() {
        tiers.push((if ceiling.is_empty() { "the columns and walls" } else { "the ceiling, columns and walls" }, structure.clone()));
    }
    tiers.push((if structure.is_empty() { "racks and the room perimeter" } else { "the structure and racks" }, all.clone()));
    if !structure.is_empty() {
        report.notes.push(format!("Building structure: {} ceiling and {} column/wall mounting points.", ceiling.len(), structural.len()));
    }
    if !on_racks.is_empty() {
        report.notes.push(format!("{} racks/shelves found: {} mounting points on their tops and aisle faces.", racks.len(), on_racks.len()));
    }

    // ---- Zones: keep the user's, otherwise one per cluster of objects. Made
    // before the anchors so the anchor plan covers every zone.
    let user_zones: Vec<Zone> = {
        let mut q = world.query::<&Zone>();
        q.iter(world).cloned().collect()
    };
    let mut zones = user_zones.clone();
    if zones.is_empty() {
        let tall: Vec<&Obj> = objs.iter().filter(|o| !o.structure && o.max.y - o.min.y > 0.3).collect();
        for (k, (a, b)) in clusters(&tall, 1.0).into_iter().take(8).enumerate() {
            let z = Zone {
                name: format!("Area {}", k + 1),
                min: [a.x - 0.5, 0.0, a.z - 0.5],
                max: [b.x + 0.5, (b.y + 0.3).max(2.5), b.z + 0.5],
                restricted: false,
                capacity: 0,
                auto: true,
            };
            world.spawn((Name::new(format!("Zone {}", z.name)), z.clone()));
            zones.push(z);
            report.zones_created += 1;
        }
    }
    let zone_boxes: Vec<(Vec3, Vec3)> = zones.iter().map(|z| (Vec3::from(z.min), Vec3::from(z.max))).collect();

    // What each kind of mount is, for install cost and fine-tuning.
    let on_racks_c = on_racks.clone();
    let has_structure = !structure.is_empty();
    let mount_of = move |p: Vec3, f: Vec3| -> Mount {
        if on_racks_c.iter().any(|(q, _)| q.distance(p) < 1e-3) {
            Mount::Rack
        } else if !has_structure {
            Mount::Free
        } else if f.y < -0.9 {
            Mount::Ceiling
        } else if column_spots.iter().any(|q| q.distance(p) < 1e-3) {
            Mount::Column
        } else {
            Mount::Wall
        }
    };
    let mounts_of = |c: &[(Vec3, Vec3)]| -> Vec<Mount> { c.iter().map(|(p, f)| mount_of(*p, *f)).collect() };
    // Tag heights the plans cover.
    let mut heights = vec![req.tag_height];
    for h in &req.heights {
        if !heights.iter().any(|x| (x - h).abs() < 0.05) {
            heights.push(*h);
        }
    }
    // The settings every technology's plan shares.
    let base_request = |tech: Tech, add: usize, cands: &[(Vec3, Vec3)]| OptimizeRequest {
        tech,
        add,
        height: req.tag_height,
        heights: heights.clone(),
        candidates: Some(cands.to_vec()),
        mounts: Some(mounts_of(cands)),
        zones: zone_boxes.clone(),
        dim3: req.dim3,
        redundancy: req.redundancy,
        min_spacing_m: req.min_spacing_m,
        cost_weight: req.cost_weight,
        head_end: req.head_end,
        refine: req.refine,
        expand_heights: true,
        ..Default::default()
    };
    // Spots taken so far: the next technology keeps off them (or shares them).
    let mut taken: Vec<Vec3> = Vec::new();

    // ---- UWB anchors: as few as meet the targets on the floor and in every zone.
    report.target_m = req.accept_m.unwrap_or(req.target_m);
    if req.uwb
        && let Some(anchor) = prof(DeviceKind::UwbAnchor)
    {
        report.anchor_reach_m = super::radio::uwb_link_range_m(&anchor);
        // Too weak to position anything: say so instead of planning on air.
        let too_weak = report.anchor_reach_m < MIN_USEFUL_RANGE_M;
        if too_weak {
            report.notes.push(format!(
                "Anchors reach only {:.1} m at this TX power, so no anchor plan can locate tags. Raise the TX power (-41.3 dBm/MHz is the legal maximum).",
                report.anchor_reach_m
            ));
        }
        // Anchors the user placed count against the cap.
        let mine = {
            let mut q = world.query::<&Device>();
            q.iter(world).filter(|d| d.kind == DeviceKind::UwbAnchor && d.enabled).count()
        };
        let budget = req.max_anchors.map_or(40, |m| m.saturating_sub(mine));
        if req.max_anchors.is_some_and(|m| mine >= m) {
            report.notes.push(format!("Anchor limit {} already reached by your own anchors: none added.", req.max_anchors.unwrap_or(0)));
        }
        // Plan on every tier of mounting points and keep the one that meets the
        // targets with the fewest anchors (ties go to the preferred tier);
        // if none meets them, the one that covers the most.
        let mut picked: Option<(&str, planning::OptimizeResult)> = None;
        for (name, cands) in tiers.iter().filter(|_| budget > 0 && !too_weak) {
            let r = world
                .run_system_cached_with(planning::optimize, OptimizeRequest { target_coverage: Some(0.95), target_p95_m: Some(req.target_m), ..base_request(Tech::Uwb, budget, cands) })
                .unwrap_or_default();
            if picked.as_ref().is_none_or(|(_, b)| better_plan(&r, b)) {
                picked = Some((name, r));
            }
        }
        let (tier, r) = picked.unwrap_or(("", Default::default()));
        if !r.chosen.is_empty() {
            report.notes.push(format!("UWB anchors mounted on {tier}."));
        }
        let zc: Vec<f32> = r.zone_coverage.iter().copied().filter(|z| !z.is_nan()).collect();
        report.zones_total = zc.len();
        report.zones_covered = r.zones_met;
        for (z, c) in zones.iter().zip(&r.zone_coverage) {
            if c.is_finite() && *c < 0.9 {
                report.notes.push(format!("Zone \"{}\": only {:.0}% of its floor gets a usable fix.", z.name, c * 100.0));
                report.weak_zones.push(z.name.clone());
            }
        }
        if let Some(m) = req.max_anchors
            && !r.targets_met
            && mine + r.chosen.len() >= m
        {
            report.capped_at = Some(m);
            report.notes.push(format!("Limited to {m} anchors: the target needs more, this is the best coverage with {m}."));
        }
        report.plans.push(TechPlan { tech: "UWB", devices: r.chosen.len(), coverage: r.coverage_after, reachable_coverage: r.reachable_coverage, p95_m: r.p95_after, cost: r.cost, targets_met: r.targets_met });
        accuracy_targets.push(("uwb", report.target_m));
        report.reachable_coverage = r.reachable_coverage;
        if r.unsynced > 0 {
            report.notes.push(format!("{} anchor(s) can't reach the others for clock sync.", r.unsynced));
        }
        for (p, f) in &r.chosen {
            spawn(world, &anchor, facing(*p, *f), Motion::Static, None);
            taken.push(*p);
            report.anchors += 1;
            if on_racks.iter().any(|(q, _)| q.distance(*p) < 1e-3) {
                report.on_racks += 1;
            }
        }
        report.coverage = r.coverage_after;
        report.predicted_p95_m = r.p95_after;
        report.targets_met = r.targets_met;
        if !r.targets_met && !too_weak {
            let down = if req.redundancy && r.p95_k1.is_finite() { format!(" ({:.0} cm with a device down)", r.p95_k1 * 100.0) } else { String::new() };
            let reach = if r.reachable_coverage < 0.95 { format!(" These mounting points can reach {:.0}% at most.", r.reachable_coverage * 100.0) } else { String::new() };
            report.notes.push(format!(
                "UWB target not fully reached: {:.0}% of the floor gets a usable fix, predicted p95 {}{down}.{reach} Racks and walls hide some aisles; add mounting points there or relax the target.",
                r.coverage_after * 100.0,
                if r.p95_after.is_finite() { dist(r.p95_after) } else { format!("{} where there is a fix", dist(r.p95_located)) },
            ));
        }
    }

    // ---- BLE beacons or AoA locators, then WiFi access points: same search,
    // their own link model, on spots the others left (or sharing them).
    let ble_prof = if req.ble_aoa { prof_id("ble-aoa-locator", DeviceKind::BleNode) } else { prof(DeviceKind::BleNode) };
    let wifi_prof = prof(DeviceKind::WifiAp);
    let others = [(req.ble || req.ble_aoa, Tech::Ble, ble_prof), (req.wifi, Tech::Wifi, wifi_prof)];
    for (wanted, tech, node) in others {
        let Some(node) = node.filter(|_| wanted) else { continue };
        let tag = planning::tag_profile(world.resource::<Profiles>(), tech);
        let method = placement::Method::of(tech, &node, &tag);
        // RSSI is rough: only coverage is asked of the plan. The simulated test still holds it
        // to a figure of its own.
        let target_p95_m = (method != placement::Method::Rssi).then(|| method.accuracy_target_m());
        accuracy_targets.push((if tech == Tech::Wifi { "wifi" } else { "ble" }, method.accuracy_target_m()));
        let mut picked: Option<planning::OptimizeResult> = None;
        for (_, cands) in &tiers {
            let mut c = cands.clone();
            if req.colocate {
                c.extend(taken.iter().map(|p| (*p, Vec3::NEG_Y)));
            }
            let (reserved, shared_mounts) = if req.colocate { (Vec::new(), taken.clone()) } else { (taken.clone(), Vec::new()) };
            let r = world
                .run_system_cached_with(planning::optimize, OptimizeRequest { profile: Some(node.id.clone()), target_coverage: Some(0.9), target_p95_m, reserved, shared_mounts, ..base_request(tech, 40, &c) })
                .unwrap_or_default();
            if picked.as_ref().is_none_or(|b| better_plan(&r, b)) {
                picked = Some(r);
            }
        }
        let r = picked.unwrap_or_default();
        report.plans.push(TechPlan { tech: tech.label(), devices: r.chosen.len(), coverage: r.coverage_after, reachable_coverage: r.reachable_coverage, p95_m: r.p95_after, cost: r.cost, targets_met: r.targets_met });
        if !r.targets_met {
            report.notes.push(format!("{} target not fully reached: {:.0}% of the floor has a fix.", tech.label(), r.coverage_after * 100.0));
        }
        for (p, f) in &r.chosen {
            spawn(world, &node, facing(*p, *f), Motion::Static, None);
            taken.push(*p);
            match tech {
                Tech::Wifi => report.wifi_aps += 1,
                _ => {
                    report.ble_nodes += 1;
                    report.ble_locators += (method == placement::Method::Aoa) as usize;
                }
            }
        }
    }

    // ---- RFID: reader pairs across doorways and docks; then the search picks
    // positions and aims that read every shelf level and zone. Shelf labels sit
    // on the racks' aisle-side edge, where goods and decks don't block the read.
    let rfid_tag = prof_id("rfid-tag", DeviceKind::RfidTag);
    if req.rfid
        && let Some(reader) = prof_id("rfid-reader", DeviceKind::RfidReader)
    {
        let doors: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| matches(&o.name, &DOORS)).map(|o| (o.min, o.max)).collect();
        let walls: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| o.wall).map(|o| (o.min, o.max)).collect();
        let portal = prof_id("rfid-portal", DeviceKind::RfidReader).unwrap_or_else(|| reader.clone());
        for o in placement::find_openings(&doors, &walls).into_iter().take(8) {
            for (p, f) in placement::portal_pair(&o, portal.mount_height_m) {
                spawn(world, &portal, facing(p, f), Motion::Static, None);
                taken.push(p);
                report.readers += 1;
                report.portal_readers += 1;
            }
        }
        if report.portal_readers > 0 {
            report.notes.push(format!("{} RFID portal antennas on {} doorway(s).", report.portal_readers, report.portal_readers / 2));
        }

        // A spot on a rack's aisle-side edge at distance `s` along it, at height `y`.
        struct Shelf {
            along_x: bool,
            start: f32,
            mid: Vec3,
            edge: f32,
            len: f32,
            levels: usize,
            min_y: f32,
            size_y: f32,
            has_goods: bool,
        }
        impl Shelf {
            fn at(&self, s: f32, y: f32) -> Vec3 {
                if self.along_x { Vec3::new(self.start + s, y, self.mid.z + self.edge) } else { Vec3::new(self.mid.x + self.edge, y, self.start + s) }
            }
            fn level_y(&self, lv: usize) -> f32 {
                self.min_y + (lv as f32 + 0.5) * self.size_y / self.levels as f32
            }
            fn axis(&self) -> Vec3 {
                if self.along_x { Vec3::X } else { Vec3::Z }
            }
        }
        let shelves: Vec<Shelf> = racks
            .iter()
            .take(32)
            .map(|r| {
                let size = r.max - r.min;
                let along_x = size.x >= size.z;
                let mid = (r.min + r.max) * 0.5;
                // Aisle side: the long face towards the middle of the floor.
                let (depth, toward) = if along_x { (size.z, area_c.y - mid.z) } else { (size.x, area_c.x - mid.x) };
                Shelf {
                    along_x,
                    start: if along_x { r.min.x } else { r.min.z },
                    mid,
                    edge: toward.signum() * (depth * 0.5 - 0.05).max(0.0),
                    len: if along_x { size.x } else { size.z },
                    levels: ((size.y / 0.55).round() as usize).clamp(1, 5),
                    min_y: r.min.y,
                    size_y: size.y,
                    // Inventory on each level, unless real goods already sit in the rack.
                    has_goods: objs.iter().any(|o| {
                        let c = (o.min + o.max) * 0.5;
                        matches(&o.name, &GOODS) && c.x >= r.min.x && c.x <= r.max.x && c.z >= r.min.z && c.z <= r.max.z
                    }),
                }
            })
            .collect();
        // The tag spots each rack must read, and antennas at the rack's ends
        // at every level as candidates alongside the structure's mounts.
        let mut targets: Vec<(Vec3, u16)> = Vec::new();
        let mut end_mounts: Vec<(Vec3, Vec3)> = Vec::new();
        for (i, s) in shelves.iter().enumerate() {
            for lv in 0..s.levels {
                let ly = s.level_y(lv);
                for f in [0.3, 0.6, 0.9] {
                    targets.push((s.at(s.len * f, ly), i as u16));
                }
                end_mounts.push((s.at(0.12, ly), s.axis()));
                end_mounts.push((s.at(s.len - 0.12, ly), -s.axis()));
            }
        }
        // Goods get tags too, wherever they stand: one group of their own.
        let goods_group = shelves.len() as u16;
        targets.extend(objs.iter().filter(|o| matches(&o.name, &GOODS)).take(80).map(|o| (Vec3::new((o.min.x + o.max.x) * 0.5, o.min.y + (o.max.y - o.min.y) * 0.5, (o.min.z + o.max.z) * 0.5), goods_group)));
        let is_end = |p: Vec3| end_mounts.iter().any(|(q, _)| q.distance(p) < 1e-3);
        // Zones without racks are read too, from whatever hangs nearby.
        let open_zones: Vec<(Vec3, Vec3)> = zones
            .iter()
            .map(|z| (Vec3::from(z.min), Vec3::from(z.max)))
            .filter(|(a, b)| {
                !racks.iter().any(|r| {
                    let c = (r.min + r.max) * 0.5;
                    c.x >= a.x && c.x <= b.x && c.z >= a.z && c.z <= b.z
                })
            })
            .collect();
        let mut cands: Vec<(Vec3, Vec3)> = end_mounts.clone();
        cands.extend(structure.iter().copied());
        if structure.is_empty() {
            cands.extend(planning::default_candidates(lo, hi, reader.mount_height_m));
        }
        let mut mounts: Vec<Mount> = cands.iter().map(|(p, f)| if is_end(*p) { Mount::Rack } else { mount_of(*p, *f) }).collect();
        // Zones get a single tag height: goods on a pallet or cart.
        if !targets.is_empty() || !open_zones.is_empty() {
            let (reserved, shared_mounts) = if req.colocate { (Vec::new(), taken.clone()) } else { (taken.clone(), Vec::new()) };
            let plan = OptimizeRequest {
                tech: Tech::Rfid,
                add: 64,
                height: 1.0,
                candidates: Some(std::mem::take(&mut cands)),
                mounts: Some(std::mem::take(&mut mounts)),
                target_coverage: Some(0.95),
                zones: open_zones.clone(),
                targets: targets.clone(),
                reserved,
                shared_mounts,
                profile: Some(reader.id.clone()),
                cost_weight: req.cost_weight,
                head_end: req.head_end,
                refine: req.refine,
                ..Default::default()
            };
            let r = world.run_system_cached_with(planning::optimize, plan).unwrap_or_default();
            report.plans.push(TechPlan { tech: "RFID", devices: r.chosen.len(), coverage: r.coverage_after, reachable_coverage: r.reachable_coverage, p95_m: f32::INFINITY, cost: r.cost, targets_met: r.targets_met });
            for (p, f) in &r.chosen {
                spawn(world, &reader, facing(*p, *f), Motion::Static, None);
                taken.push(*p);
                report.readers += 1;
                report.shelf_readers += is_end(*p) as usize;
            }
            if r.conflicts > 0 {
                report.notes.push(format!("{} RFID reader pair(s) in each other's beam: give them different channels or time slots.", r.conflicts));
            }
            // Reads leaking out only matter where a zone is meant to say where
            // a tag is; shelf inventory doesn't care.
            if !open_zones.is_empty() && r.stray > 0.3 {
                report.notes.push(format!("RFID readers also read {:.0}% of the floor outside the zones: expect stray reads.", r.stray * 100.0));
            }
            // A rack the search could not cover gets an antenna per level at its
            // ends: reading along the edge works when nothing else does.
            let before_extra = report.readers;
            for (i, s) in shelves.iter().enumerate() {
                if r.target_coverage.get(i).is_none_or(|c| c.is_nan() || *c >= 0.5) {
                    continue;
                }
                for lv in 0..s.levels {
                    let ly = s.level_y(lv);
                    spawn(world, &reader, facing(s.at(0.12, ly), s.axis()), Motion::Static, None);
                    report.readers += 1;
                    report.shelf_readers += 1;
                    if s.len > 6.0 {
                        spawn(world, &reader, facing(s.at(s.len - 0.12, ly), -s.axis()), Motion::Static, None);
                        report.readers += 1;
                        report.shelf_readers += 1;
                    }
                }
            }
            // Those readers are in the scene now: the plan's figures should say so.
            if report.readers > before_extra {
                let again = OptimizeRequest { tech: Tech::Rfid, add: 0, height: 1.0, candidates: Some(Vec::new()), mounts: Some(Vec::new()), target_coverage: Some(0.95), zones: open_zones.clone(), targets: targets.clone(), profile: Some(reader.id.clone()), ..Default::default() };
                let now = world.run_system_cached_with(planning::optimize, again).unwrap_or_default();
                if let Some(p) = report.plans.iter_mut().find(|p| p.tech == "RFID") {
                    p.coverage = now.coverage_after;
                    p.targets_met = now.targets_met;
                }
            }
        }
        // Shelf labels where the plan was told they would be.
        if let Some(t) = &rfid_tag {
            for s in shelves.iter().filter(|s| !s.has_goods) {
                for lv in 0..s.levels {
                    let ly = s.level_y(lv);
                    for f in [0.3, 0.6, 0.9] {
                        if report.inventory_tags < 150 {
                            spawn(world, t, Transform::from_translation(s.at(s.len * f, ly)), Motion::Static, None);
                            report.tags += 1;
                            report.inventory_tags += 1;
                        }
                    }
                }
            }
        }
    }

    // ---- Tags on vehicles and goods.
    let uwb_tag = prof_id("uwb-tag", DeviceKind::UwbTag);
    let mut goods = 0;
    let occ = {
        let mut ss = bevy::ecs::system::SystemState::<(sim::OccluderParts, Query<(Entity, &PlacedObject, &Transform)>)>::new(world);
        match ss.get(world) {
            Ok((parts, q)) => Some(sim::Occluders::build(&parts, &q)),
            Err(_) => None,
        }
    };
    for o in &objs {
        let h = o.max.y - o.min.y;
        if matches(&o.name, &VEHICLES)
            && req.uwb
            && let Some(t) = &uwb_tag
        {
            // On the surface under the center (a cart's tray), not on top of
            // the bounding box (its handles), which would leave it floating.
            let c = Vec3::new((o.min.x + o.max.x) * 0.5, o.max.y + 0.5, (o.min.z + o.max.z) * 0.5);
            let top = occ.as_ref().and_then(|occ| occ.nearest(Ray3d::new(c, Dir3::NEG_Y), h + 0.5)).map_or(o.max.y, |(d, _)| c.y - d);
            let lift = top - o.min.y + 0.03;
            let at = Transform::from_translation(Vec3::new(c.x, o.min.y + lift, c.z));
            spawn(world, t, at, Motion::Follow { object: o.name.clone(), offset: [0.0, lift, 0.0] }, None);
            report.tags += 1;
        } else if matches(&o.name, &GOODS)
            && req.rfid
            && goods < 80
            && let Some(t) = &rfid_tag
        {
            let at = Transform::from_translation(Vec3::new((o.min.x + o.max.x) * 0.5, o.min.y + h * 0.5, (o.min.z + o.max.z) * 0.5));
            spawn(world, t, at, Motion::Follow { object: o.name.clone(), offset: [0.0, h * 0.5, 0.0] }, None);
            report.tags += 1;
            goods += 1;
        }
    }

    // ---- Workers walking loops through the aisles.
    let grid = FreeGrid::new(lo, hi, 0.5, &boxes, 0.4);
    if req.uwb
        && let Some(t) = &uwb_tag
    {
        let inset = 1.0;
        let corners = [Vec2::new(lo.x + inset, lo.y + inset), Vec2::new(hi.x - inset, lo.y + inset), Vec2::new(hi.x - inset, hi.y - inset), Vec2::new(lo.x + inset, hi.y - inset)];
        for w in 0..req.workers {
            // Each worker starts at a different corner and walks the loop.
            let order: Vec<Vec2> = (0..4).map(|k| corners[(k + w) % 4]).collect();
            let cells: Vec<(usize, usize)> = order.iter().filter_map(|p| grid.nearest_free(*p)).collect();
            let mut pts: Vec<Vec2> = Vec::new();
            for k in 0..cells.len() {
                if let Some(seg) = grid.path(cells[k], cells[(k + 1) % cells.len()]) {
                    pts.extend(seg);
                }
            }
            let pts = simplify(&pts);
            if pts.len() < 2 {
                continue;
            }
            let points: Vec<[f32; 3]> = pts.iter().map(|p| [p.x, req.tag_height, p.y]).collect();
            let at = Transform::from_translation(Vec3::from(points[0]));
            spawn(world, t, at, Motion::Path { points, speed: 1.3 + 0.2 * w as f32, ping_pong: false }, None);
            report.tags += 1;
        }
    }
    // A wandering tag (BLE) and a handheld (WiFi) on the open floor.
    let wanderers = [(req.ble || req.ble_aoa, DeviceKind::BleTag), (req.wifi, DeviceKind::WifiClient)];
    for (_, kind) in wanderers.iter().filter(|(on, _)| *on) {
        let Some(t) = prof(*kind) else { continue };
        let c = grid.nearest_free(area_c).map(|(x, z)| grid.center(x, z)).unwrap_or(area_c);
        let at = Transform::from_translation(Vec3::new(c.x, 1.0, c.y));
        spawn(world, &t, at, Motion::Wander { center: [c.x, 1.0, c.y], radius: ((hi - lo).min_element() * 0.3).max(1.0), speed: 0.8 }, None);
        report.tags += 1;
    }

    // ---- Acceptance test, and devices where it measures badly: the prediction
    // can't know everything, so the simulation has the last word.
    let seed = world.resource::<RtlsConfig>().settings.seed;
    let accept = req.accept_m.unwrap_or(req.target_m);
    let mut attempts = req.retries;
    // Every test from the same start, or the same seed would measure a different route.
    let start = sim::RunStart::take(world);
    loop {
        start.restore(world);
        world.resource_mut::<RtlsState>().restart(seed);
        sim::run_for(world, req.test_s);
        {
            let st = world.resource::<RtlsState>();
            report.accuracy = accuracy_targets.iter().map(|(t, target)| TechAccuracy::measure(st, t, *target)).collect();
            report.rmse_m = st.run_stats(None, None).stats().map_or(0.0, |e| e.rmse);
            report.cep95_m = report.accuracy.iter().map(TechAccuracy::worst_p95_m).fold(0.0, f32::max);
            report.uwb_rmse_m = st.run_stats(Some("uwb"), None).stats().map_or(0.0, |e| e.rmse);
            report.uwb_cep95_m = report.accuracy.iter().find(|a| a.tech == "uwb").map_or(0.0, TechAccuracy::worst_p95_m);
            report.found = st.tracks.values().filter(|t| t.fixes > 0).count() as u32;
            report.located = st.snapshot.tags_located;
            report.total_tags = st.tracks.len() as u32;
            report.rfid_tags = st.tracks.values().filter(|t| t.tech == "rfid").count();
            report.rfid_read_chance = st.tracks.values().filter(|t| t.tech == "rfid").map(|t| t.read_chance).sum::<f32>() / report.rfid_tags.max(1) as f32;
            report.rfid_unread = st.tracks.values().filter(|t| t.tech == "rfid" && t.reads == 0).map(|t| t.id.clone()).collect();
            report.rfid_unread.sort();
            report.alarms = st.alarms.values().map(|a| a.message.clone()).collect();
        }
        let uwb_ok = report.accuracy.iter().find(|a| a.tech == "uwb").is_some_and(TechAccuracy::passed);
        if !req.uwb || report.anchors == 0 || uwb_ok || attempts == 0 {
            break;
        }
        attempts -= 1;
        let spots: Vec<(Vec3, f32)> = world.resource::<RtlsState>().error_spots.iter().filter(|s| s.2 == "uwb").map(|s| (s.0, s.1)).collect();
        let heavy = bad_areas(&spots, accept, 2.0);
        let room = req.max_anchors.map_or(8, |m| m.saturating_sub(report.anchors).min(8));
        let Some((_, cands)) = tiers.last().filter(|_| !heavy.is_empty() && room > 0) else { break };
        let Some(anchor) = prof(DeviceKind::UwbAnchor) else { break };
        let plan = OptimizeRequest { heavy: heavy.clone(), force_add: room.min(2), target_coverage: Some(0.95), target_p95_m: Some(req.target_m), reserved: taken.clone(), ..base_request(Tech::Uwb, room, cands) };
        let r = world.run_system_cached_with(planning::optimize, plan).unwrap_or_default();
        if r.chosen.is_empty() {
            break;
        }
        report.notes.push(format!("Simulated p95 {}: added {} anchor(s) covering {} badly measured area(s).", if report.uwb_cep95_m.is_finite() { dist(report.uwb_cep95_m) } else { "not reached".into() }, r.chosen.len(), heavy.len()));
        for (p, f) in &r.chosen {
            spawn(world, &anchor, facing(*p, *f), Motion::Static, None);
            taken.push(*p);
            report.anchors += 1;
            report.feedback_added += 1;
        }
        if let Some(p) = report.plans.iter_mut().find(|p| p.tech == "UWB") {
            p.devices += r.chosen.len();
            p.cost += r.cost;
        }
    }
    // The anchors the simulation asked for are in the scene now: the plan's figures should say so.
    if report.feedback_added > 0 {
        let again = OptimizeRequest { target_coverage: Some(0.95), target_p95_m: Some(req.target_m), ..base_request(Tech::Uwb, 0, &[]) };
        let now = world.run_system_cached_with(planning::optimize, again).unwrap_or_default();
        report.coverage = now.coverage_after;
        report.predicted_p95_m = now.p95_after;
        report.zones_covered = now.zones_met;
        if let Some(p) = report.plans.iter_mut().find(|p| p.tech == "UWB") {
            p.coverage = now.coverage_after;
            p.p95_m = now.p95_after;
            p.targets_met = now.targets_met;
        }
    }
    if report.rfid_tags > 0 {
        let read = report.rfid_tags - report.rfid_unread.len();
        report.notes.push(format!("RFID: {read} of {} tags read within {:.0} s; mean read chance per round {:.0}%.", report.rfid_tags, report.test_s, report.rfid_read_chance * 100.0));
    }
    if report.shelf_readers > 0 {
        report.notes.push(format!("Smart shelves: {} antennas in {} racks.", report.shelf_readers, report.racks));
    }
    if !report.rfid_unread.is_empty() {
        report.notes.push(format!("{} RFID tag(s) not read in the test: {}", report.rfid_unread.len(), report.rfid_unread.join(", ")));
    }
    // Every technology asked for has to have met its plan.
    report.targets_met = !report.plans.is_empty() && report.plans.iter().all(|p| p.targets_met);
    if !report.plans.iter().all(|p| p.targets_met) {
        let short: Vec<&str> = report.plans.iter().filter(|p| !p.targets_met).map(|p| p.tech).collect();
        report.notes.push(format!("Plans short of their targets: {}.", short.join(", ")));
    }
    // The simulation has the last word, on every technology with a target.
    for a in &report.accuracy {
        if a.passed() {
            continue;
        }
        let t = a.tech.to_uppercase();
        report.targets_met = false;
        report.notes.push(if !a.classes().any(|c| c.fixes > 0) {
            format!("No {t} tag got a position in the simulated test.")
        } else if a.worst_p95_m().is_finite() {
            format!("Simulated {t} p95 {} is above the {} target.", dist(a.worst_p95_m()), dist(a.target_m))
        } else {
            format!("Simulated {t}: more than 5% of position updates had no fix, so 95% within {} isn't met.", dist(a.target_m))
        });
    }
    if req.rfid && report.rfid_tags > 0 && (report.rfid_unread.len() as f32) > 0.05 * report.rfid_tags as f32 {
        report.targets_met = false;
        report.notes.push(format!("{} of {} RFID tags weren't read in the {:.0} s test.", report.rfid_unread.len(), report.rfid_tags, report.test_s));
    }
    // Show the UWB heatmap of the result.
    let tech = if req.uwb {
        Tech::Uwb
    } else if req.ble || req.ble_aoa {
        Tech::Ble
    } else if req.wifi {
        Tech::Wifi
    } else {
        Tech::Rfid
    };
    let _ = world.run_system_cached_with(planning::compute, CoverageRequest { tech, height: req.tag_height, cell: 0.5 });
    // Leave the simulation running from a clean start.
    start.restore(world);
    world.resource_mut::<RtlsState>().restart(seed);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn astar_goes_around_a_wall() {
        // A wall across the middle with a gap at the top.
        let boxes = vec![(Vec3::new(4.0, 0.0, 0.0), Vec3::new(5.0, 2.0, 8.0))];
        let g = FreeGrid::new(Vec2::ZERO, Vec2::new(10.0, 10.0), 0.5, &boxes, 0.2);
        let a = g.nearest_free(Vec2::new(1.0, 1.0)).unwrap();
        let b = g.nearest_free(Vec2::new(9.0, 1.0)).unwrap();
        let path = g.path(a, b).unwrap();
        assert!(path.iter().all(|p| !blocked(&boxes, *p, 0.1)), "path crosses the wall");
        assert!(path.iter().any(|p| p.y > 8.0), "path should go around the end of the wall");
        let s = simplify(&path);
        assert!(s.len() < path.len() && s.len() >= 3);
    }

    #[test]
    fn a_walkers_loop_keeps_its_corners() {
        // Legs of a square joined end to end, as a worker's route is built. Every worker in an
        // open scene once stood still: all that was left of the route was its start, twice.
        let c = [Vec2::ZERO, Vec2::new(2.0, 0.0), Vec2::new(2.0, 2.0), Vec2::new(0.0, 2.0)];
        let mut route = Vec::new();
        for k in 0..4 {
            let (a, b) = (c[k], c[(k + 1) % 4]);
            route.extend((0..=4).map(|i| a.lerp(b, i as f32 / 4.0)));
        }
        let s = simplify(&route);
        for corner in c {
            assert!(s.contains(&corner), "{corner} lost from {s:?}");
        }
    }

    #[test]
    fn clusters_merge_neighbours() {
        let mk = |x: f32, z: f32| Obj { name: String::new(), min: Vec3::new(x, 0.0, z), max: Vec3::new(x + 1.0, 2.0, z + 1.0), structure: false, floor: false, wall: false };
        let objs = [mk(0.0, 0.0), mk(1.5, 0.0), mk(10.0, 10.0)];
        let refs: Vec<&Obj> = objs.iter().collect();
        let c = clusters(&refs, 1.0);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].1.x, 2.5);
    }

    #[test]
    fn plans_compare_by_targets_then_devices_then_cost() {
        let plan = |met: bool, n: usize, cost: f32, cov: f32| planning::OptimizeResult { targets_met: met, chosen: vec![(Vec3::ZERO, Vec3::Y); n], cost, coverage_after: cov, ..Default::default() };
        // Meeting the targets beats any number of devices.
        assert!(better_plan(&plan(true, 30, 30.0, 0.9), &plan(false, 5, 5.0, 0.9)));
        assert!(!better_plan(&plan(false, 5, 5.0, 0.99), &plan(true, 30, 30.0, 0.9)));
        // Fewer, then cheaper.
        assert!(better_plan(&plan(true, 16, 16.0, 0.9), &plan(true, 28, 22.0, 0.9)));
        assert!(better_plan(&plan(true, 16, 12.0, 0.9), &plan(true, 16, 16.0, 0.9)));
        // Neither met: a later try that covers less must not win (it once did,
        // by simply being the last one tried).
        assert!(!better_plan(&plan(false, 7, 8.0, 0.68), &plan(false, 25, 24.0, 0.95)));
    }

    #[test]
    fn bad_areas_need_evidence_and_rank_the_worst_first() {
        let mut spots = Vec::new();
        for _ in 0..20 {
            spots.push((Vec3::new(1.0, 1.2, 1.0), 0.1)); // Fine.
            spots.push((Vec3::new(5.0, 1.2, 1.0), 0.8)); // Bad.
            spots.push((Vec3::new(9.0, 1.2, 1.0), 2.0)); // Worse.
        }
        spots.push((Vec3::new(13.0, 1.2, 13.0), 9.0)); // One sample proves nothing.
        let v = bad_areas(&spots, 0.3, 2.0);
        assert_eq!(v.len(), 2, "{v:?}");
        assert_eq!(v[0].0.x, 8.0);
        assert_eq!(v[1].0.x, 4.0);
    }

    fn class(tags: usize, attempts: u64, fixes: u64, p95: f32) -> ClassResult {
        ClassResult { tags, attempts, fixes, rmse_m: Some(p95 * 0.6), fix_p95_m: Some(p95), p95_m: Some(if (fixes as f64) < attempts as f64 * 0.95 { f32::INFINITY } else { p95 }) }
    }

    #[test]
    fn the_verdict_prints_the_result_not_the_target() {
        // RSSI at a real 3 m once printed "within 30 cm".
        let ble = TechAccuracy { tech: "ble", target_m: 5.25, moving: class(1, 60, 60, 3.0), parked: ClassResult::default() };
        let r = DeployReport { targets_met: true, accuracy: vec![ble], ..Default::default() };
        let (level, text) = r.verdict();
        assert_eq!(level, Verdict::Good);
        assert!(text.contains("3.0 m") && text.contains("simulated"), "{text}");
    }

    #[test]
    fn an_rfid_plan_reports_reads_not_a_position() {
        // It once printed "within 0 cm".
        let r = DeployReport { targets_met: true, rfid_tags: 150, rfid_unread: vec!["E1".into()], test_s: 30.0, ..Default::default() };
        let (_, text) = r.verdict();
        assert!(text.contains("149 of 150") && text.contains("30 s"), "{text}");
        assert!(!text.contains("cm"), "{text}");
    }

    #[test]
    fn no_fixes_is_not_a_pass() {
        let nothing = TechAccuracy { tech: "uwb", target_m: 0.3, ..Default::default() };
        assert!(!nothing.passed());
        assert_eq!(nothing.worst_p95_m(), 0.0, "0 means nothing tested, for callers that guard on it");
        let lost = TechAccuracy { tech: "uwb", target_m: 0.3, moving: class(2, 600, 0, 0.0), ..Default::default() };
        assert!(!lost.passed());
    }

    #[test]
    fn updates_without_a_fix_fail_and_parked_tags_cannot_hide_walkers() {
        // Good positions, but 12 % of updates had none.
        let gaps = TechAccuracy { tech: "uwb", target_m: 0.3, moving: class(2, 600, 528, 0.1), ..Default::default() };
        assert!(!gaps.passed());
        assert!(gaps.worst_p95_m().is_infinite());
        // Plenty of parked tags inside the target don't carry walkers outside it.
        let mix = TechAccuracy { tech: "uwb", target_m: 0.3, moving: class(2, 600, 600, 1.5), parked: class(10, 3000, 3000, 0.1) };
        assert!(!mix.passed());
        assert_eq!(mix.worst_p95_m(), 1.5);
    }

    #[test]
    fn a_tier_that_reaches_half_the_floor_loses_to_one_that_covers_it() {
        // With targets judged as asked, the half-floor tier is no longer "met" on fewer anchors.
        let plan = |met: bool, n: usize, cov: f32| planning::OptimizeResult { targets_met: met, chosen: vec![(Vec3::ZERO, Vec3::Y); n], coverage_after: cov, ..Default::default() };
        assert!(better_plan(&plan(true, 6, 0.96), &plan(false, 5, 0.49)));
        assert!(!better_plan(&plan(false, 5, 0.49), &plan(true, 6, 0.96)));
    }

    #[test]
    fn a_p95_from_few_samples_is_their_worst_not_the_fourth() {
        let mut spots = vec![(Vec3::new(1.0, 1.2, 1.0), 0.1); 4];
        spots.push((Vec3::new(1.0, 1.2, 1.0), 2.0));
        assert_eq!(bad_areas(&spots, 0.3, 2.0).len(), 1);
    }

    #[test]
    fn keyword_matching() {
        assert!(matches("forklift_electric_a01_3", &VEHICLES));
        assert!(matches("pallet_rackable_blue_a01_1", &GOODS));
        assert!(!matches("cabinet_file_a01", &GOODS));
    }
}
