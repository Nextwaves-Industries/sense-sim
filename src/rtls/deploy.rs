//! One-button deployment: from the scene alone, place a complete positioning
//! system (UWB anchors, BLE beacons, RFID readers at the zones, tags on
//! vehicles and goods, workers walking the aisles), then simulate it and
//! report how well it works. Re-running replaces what it placed before.

use std::collections::{BinaryHeap, HashMap};

use bevy::prelude::*;

use super::device::{Device, DeviceKind, Motion, Profile, Profiles, Zone, epc_for, next_id};
use super::planning::{self, CoverageRequest, OptimizeRequest, Tech, blocked};
use super::{RtlsConfig, RtlsState, sim};
use crate::scene::PlacedObject;

#[derive(Clone, Debug)]
pub struct DeployRequest {
    pub uwb: bool,
    pub ble: bool,
    pub rfid: bool,
    /// Target 95th-percentile position error (m) for UWB.
    pub target_m: f32,
    pub tag_height: f32,
    pub workers: usize,
    /// Simulated seconds of the acceptance test afterwards.
    pub test_s: f64,
    /// Mounting points (position, facing) on real structure; `None` = walls + grid.
    pub mounts: Option<Vec<(Vec3, Vec3)>>,
    /// Re-plan with a stricter prediction when the simulated UWB p95 misses the target.
    pub retries: u8,
    /// Simulated UWB p95 that counts as success (defaults to `target_m`).
    pub accept_m: Option<f32>,
    /// Most UWB anchors in the scene (yours included); `None` = as few as meet the target.
    pub max_anchors: Option<usize>,
    /// UWB anchor transmit power (dBm/MHz) to plan and simulate with; set on
    /// the default anchor profile. `None` = keep the profile's.
    pub anchor_tx_dbm_mhz: Option<f32>,
}

impl Default for DeployRequest {
    fn default() -> Self {
        Self { uwb: true, ble: false, rfid: false, target_m: 0.3, tag_height: 1.2, workers: 2, test_s: 30.0, mounts: None, retries: 1, accept_m: None, max_anchors: None, anchor_tx_dbm_mhz: None }
    }
}

/// Below this anchor reach (m) a UWB plan can't work in a real room.
pub const MIN_USEFUL_RANGE_M: f32 = 4.0;

#[derive(Clone, Debug, Default)]
pub struct DeployReport {
    pub anchors: usize,
    pub ble_nodes: usize,
    pub readers: usize,
    pub tags: usize,
    pub zones_created: usize,
    pub racks: usize,
    /// Anchors mounted on racks.
    pub on_racks: usize,
    pub shelf_readers: usize,
    pub inventory_tags: usize,
    /// Share of walkable floor with ≥3 anchors in line of sight.
    pub coverage: f32,
    /// Zones whose walkable floor is fully covered by UWB, of those with floor.
    pub zones_covered: usize,
    pub zones_total: usize,
    pub predicted_p95_m: f32,
    pub targets_met: bool,
    pub rmse_m: f32,
    pub cep95_m: f32,
    /// Simulated UWB accuracy (what the target applies to).
    pub uwb_rmse_m: f32,
    pub uwb_cep95_m: f32,
    pub located: u32,
    pub total_tags: u32,
    pub rfid_unread: Vec<String>,
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

impl DeployReport {
    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        if self.anchors > 0 {
            if self.on_racks > 0 {
                parts.push(format!("{} UWB anchors ({} on racks)", self.anchors, self.on_racks));
            } else {
                parts.push(format!("{} UWB anchors", self.anchors));
            }
        }
        if self.ble_nodes > 0 {
            parts.push(format!("{} BLE beacons", self.ble_nodes));
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
}

fn objects(world: &mut World) -> Vec<Obj> {
    let mut q = world.query::<(&PlacedObject, &GlobalTransform)>();
    q.iter(world)
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
            Obj { name: o.name.clone(), min: lo, max: hi, structure: kind.is_some(), floor: kind == Some(crate::agent::structures::Kind::Floor) }
        })
        .collect()
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
    if pts.len() < 3 {
        return pts.to_vec();
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
    v.sort_by(|a, b| ((b.1.x - b.0.x) * (b.1.z - b.0.z)).total_cmp(&((a.1.x - a.0.x) * (a.1.z - a.0.z))));
    v
}

pub fn auto_deploy(world: &mut World, req: &DeployRequest) -> DeployReport {
    let mut report = DeployReport::default();

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
    let (ceiling, structural): (Vec<(Vec3, Vec3)>, Vec<(Vec3, Vec3)>) = match &req.mounts {
        Some(m) => (Vec::new(), m.clone()),
        None => {
            use crate::agent::structures::{MountKind, MountOpts, mount_points};
            let pts = mount_points(world, &MountOpts { mount_height_m: anchor_h, ..Default::default() });
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
                .run_system_cached_with(
                    planning::optimize,
                    OptimizeRequest {
                        tech: Tech::Uwb,
                        add: budget,
                        height: req.tag_height,
                        candidates: Some(cands.clone()),
                        target_coverage: Some(0.95),
                        target_p95_m: Some(req.target_m),
                        // Three in view per point; a 4th still scores (NLOS rejection) but
                        // is not required everywhere, which multiplied anchors in rack aisles.
                        min_los: None,
                        zones: zone_boxes.clone(),
                    },
                )
                .unwrap_or_default();
            let zone_cov = |r: &planning::OptimizeResult| r.zone_coverage.iter().filter(|z| !z.is_nan()).sum::<f32>();
            let better = picked.as_ref().is_none_or(|(_, b)| match (r.targets_met, b.targets_met) {
                (true, true) => r.chosen.len() < b.chosen.len(),
                (true, false) => true,
                (false, true) => false,
                (false, false) => r.coverage_after + zone_cov(&r) > b.coverage_after + zone_cov(b) + 0.02,
            });
            if better {
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
                report.notes.push(format!("Zone \"{}\": only {:.0}% of its floor has 3 anchors in view.", z.name, c * 100.0));
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
        for (p, f) in &r.chosen {
            spawn(world, &anchor, facing(*p, *f), Motion::Static, None);
            report.anchors += 1;
            if on_racks.iter().any(|(q, _)| q.distance(*p) < 1e-3) {
                report.on_racks += 1;
            }
        }
        report.coverage = r.coverage_after;
        report.predicted_p95_m = r.p95_after;
        report.targets_met = r.targets_met;
        if !r.targets_met && !too_weak {
            report.notes.push(format!(
                "UWB target not fully reached: {:.0}% of the floor has enough anchors in view, predicted p95 {:.0} cm. Racks hide some aisles; add mounting points there or relax the target.",
                r.coverage_after * 100.0,
                r.p95_after * 100.0
            ));
        }
    }

    // ---- BLE beacons.
    if req.ble
        && let Some(node) = prof(DeviceKind::BleNode)
    {
        let mut r = planning::OptimizeResult::default();
        for (_, cands) in &tiers {
            r = world
                .run_system_cached_with(
                    planning::optimize,
                    OptimizeRequest {
                        tech: Tech::Ble,
                        add: 40,
                        height: req.tag_height,
                        candidates: Some(cands.clone()),
                        target_coverage: Some(0.9),
                        target_p95_m: None,
                        min_los: None,
                        zones: zone_boxes.clone(),
                    },
                )
                .unwrap_or_default();
            if r.targets_met {
                break;
            }
        }
        for (p, f) in &r.chosen {
            spawn(world, &node, facing(*p, *f), Motion::Static, None);
            report.ble_nodes += 1;
        }
    }

    // ---- RFID: smart shelves, an antenna per shelf level at the end of the
    // rack scanning along its aisle-side edge, with inventory tags on that
    // edge (where shelf labels go). Down the middle of a stocked shelf the
    // goods and the decks between levels would block every read.
    // Elsewhere a reader per zone.
    let rfid_tag = prof_id("rfid-tag", DeviceKind::RfidTag);
    if req.rfid
        && let Some(reader) = prof_id("rfid-reader", DeviceKind::RfidReader)
    {
        for r in racks.iter().take(32) {
            let size = r.max - r.min;
            let along_x = size.x >= size.z;
            let len = if along_x { size.x } else { size.z };
            let mid = (r.min + r.max) * 0.5;
            let axis = if along_x { Vec3::X } else { Vec3::Z };
            let start = if along_x { r.min.x } else { r.min.z };
            // Aisle side: the long face towards the middle of the floor.
            let (depth, toward) = if along_x { (size.z, area_c.y - mid.z) } else { (size.x, area_c.x - mid.x) };
            let edge = toward.signum() * (depth * 0.5 - 0.05).max(0.0);
            let at = |s: f32, y: f32| if along_x { Vec3::new(start + s, y, mid.z + edge) } else { Vec3::new(mid.x + edge, y, start + s) };
            let levels = ((size.y / 0.55).round() as usize).clamp(1, 5);
            // Inventory on each level, unless real goods already sit in the rack.
            let has_goods = objs.iter().any(|o| {
                let c = (o.min + o.max) * 0.5;
                matches(&o.name, &GOODS) && c.x >= r.min.x && c.x <= r.max.x && c.z >= r.min.z && c.z <= r.max.z
            });
            for lv in 0..levels {
                let ly = r.min.y + (lv as f32 + 0.5) * size.y / levels as f32;
                // Both ends for long racks.
                spawn(world, &reader, facing(at(0.12, ly), axis), Motion::Static, None);
                report.readers += 1;
                report.shelf_readers += 1;
                if len > 6.0 {
                    spawn(world, &reader, facing(at(len - 0.12, ly), -axis), Motion::Static, None);
                    report.readers += 1;
                    report.shelf_readers += 1;
                }
                if let Some(t) = &rfid_tag
                    && !has_goods
                    && report.inventory_tags < 150
                {
                    for f in [0.3, 0.6, 0.9] {
                        spawn(world, t, Transform::from_translation(at(len * f, ly)), Motion::Static, None);
                        report.tags += 1;
                        report.inventory_tags += 1;
                    }
                }
            }
        }
        // Zones without racks get a reader on the edge facing the open floor.
        for z in &zones {
            let (a, b) = (Vec3::from(z.min), Vec3::from(z.max));
            let has_rack = racks.iter().any(|r| {
                let c = (r.min + r.max) * 0.5;
                c.x >= a.x && c.x <= b.x && c.z >= a.z && c.z <= b.z
            });
            if has_rack {
                continue;
            }
            let c = (a + b) * 0.5;
            let d = area_c - c.xz();
            let edge = if d.x.abs() >= d.y.abs() {
                Vec2::new(if d.x >= 0.0 { b.x + 0.3 } else { a.x - 0.3 }, c.z)
            } else {
                Vec2::new(c.x, if d.y >= 0.0 { b.z + 0.3 } else { a.z - 0.3 })
            };
            // On the nearest column or wall when the building has one (a reader
            // needs something to hang on), else on the zone edge.
            let near = structural.iter().map(|(m, _)| *m).filter(|m| m.xz().distance(c.xz()) < 6.0).min_by(|a, b| a.xz().distance(c.xz()).total_cmp(&b.xz().distance(c.xz())));
            let p = near.map_or(Vec3::new(edge.x, 2.2, edge.y), |m| Vec3::new(m.x, 2.2, m.z));
            let aim = Vec3::new(c.x, 1.0, c.z) - p;
            spawn(world, &reader, facing(p, aim), Motion::Static, None);
            report.readers += 1;
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
    if req.ble
        && let Some(t) = prof(DeviceKind::BleTag)
    {
        let c = grid.nearest_free(area_c).map(|(x, z)| grid.center(x, z)).unwrap_or(area_c);
        let at = Transform::from_translation(Vec3::new(c.x, 1.0, c.y));
        spawn(world, &t, at, Motion::Wander { center: [c.x, 1.0, c.y], radius: ((hi - lo).min_element() * 0.3).max(1.0), speed: 0.8 }, None);
        report.tags += 1;
    }

    // ---- Acceptance test.
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    sim::run_for(world, req.test_s);
    {
        let st = world.resource::<RtlsState>();
        let (rmse, _, cep95, _) = st.error_stats(None);
        report.rmse_m = rmse;
        report.cep95_m = cep95;
        let (ur, _, u95, _) = st.error_stats(Some("uwb"));
        report.uwb_rmse_m = ur;
        report.uwb_cep95_m = u95;
        report.located = st.snapshot.tags_located;
        report.total_tags = st.snapshot.tags_total;
        report.rfid_unread = st.tracks.values().filter(|t| t.tech == "rfid" && t.reads == 0).map(|t| t.id.clone()).collect();
        report.rfid_unread.sort();
        report.alarms = st.alarms.values().map(|a| a.message.clone()).collect();
    }
    if report.shelf_readers > 0 {
        let read = report.inventory_tags.saturating_sub(report.rfid_unread.len());
        report.notes.push(format!("Smart shelves: {} antennas in {} racks read {read} of {} shelf items.", report.shelf_readers, report.racks, report.inventory_tags));
    }
    if !report.rfid_unread.is_empty() {
        report.notes.push(format!("{} RFID tag(s) were never read: {}", report.rfid_unread.len(), report.rfid_unread.join(", ")));
    }
    // The target is judged on the simulation, not only the prediction: if
    // the measured UWB p95 misses it, plan again with a stricter prediction.
    let accept = req.accept_m.unwrap_or(req.target_m);
    if req.uwb && report.anchors > 0 && report.uwb_cep95_m > accept {
        // Re-plan only when the plan itself was met: if racks leave floor
        // uncovered, a stricter accuracy target just adds anchors that cannot help.
        if req.retries > 0 && report.targets_met {
            let stricter = DeployRequest { target_m: req.target_m * 0.6, retries: req.retries - 1, accept_m: Some(accept), ..req.clone() };
            let mut again = auto_deploy(world, &stricter);
            again.notes.insert(0, format!("First plan measured {:.0} cm (p95) in simulation; re-planned for a stricter prediction.", report.uwb_cep95_m * 100.0));
            return again;
        }
        report.targets_met = false;
        report.notes.push(format!("Simulated UWB p95 {:.0} cm is above the {:.0} cm target.", report.uwb_cep95_m * 100.0, accept * 100.0));
    }
    // Show the UWB heatmap of the result.
    let tech = if req.uwb { Tech::Uwb } else if req.ble { Tech::Ble } else { Tech::Rfid };
    let _ = world.run_system_cached_with(planning::compute, CoverageRequest { tech, height: req.tag_height, cell: 0.5 });
    // Leave the simulation running from a clean start.
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
    fn clusters_merge_neighbours() {
        let mk = |x: f32, z: f32| Obj { name: String::new(), min: Vec3::new(x, 0.0, z), max: Vec3::new(x + 1.0, 2.0, z + 1.0), structure: false, floor: false };
        let objs = [mk(0.0, 0.0), mk(1.5, 0.0), mk(10.0, 10.0)];
        let refs: Vec<&Obj> = objs.iter().collect();
        let c = clusters(&refs, 1.0);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].1.x, 2.5);
    }

    #[test]
    fn keyword_matching() {
        assert!(matches("forklift_electric_a01_3", &VEHICLES));
        assert!(matches("pallet_rackable_blue_a01_1", &GOODS));
        assert!(!matches("cabinet_file_a01", &GOODS));
    }
}
