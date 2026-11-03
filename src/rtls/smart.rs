//! The plan a site can live with. Every planning tool answers "where do the anchors go" for
//! one frozen picture of the floor. This one asks what the customer asks:
//!
//! - **Where do tags really go?** It works out the traffic from the layout (`flow`) and judges
//!   a plan on tag time, not on floor area, so a quiet corner can't cost as much as a busy aisle.
//! - **Does it hold on a bad day?** The search itself runs on a crowded shift, on materials worse
//!   than assumed and on half-empty racks at once, and takes the worst of them. Elsewhere those
//!   are checks made afterwards, when the plan can no longer change.
//! - **What does each extra anchor buy?** It walks the whole curve and offers real choices with
//!   prices, and the simulator, not the prediction, decides which sets pass.
//! - **Why is this anchor here?** Each one comes with what breaks if it goes.
//! - **What should the antenna do?** The cone each mount really has to serve becomes a brief
//!   for the antenna design, which no catalogue pick can give.
//!
//! Today's Auto deploy runs alongside on the same tests, so every run shows what this bought,
//! or that it bought nothing. All of it is simulated: nothing here has met real hardware.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::Serialize;

use crate::outcomes::acceptance::{self, AcceptancePlan, TestPoint};

use super::antenna::{AntennaLibrary, Antennas};
use super::deploy::{self, DeployRequest, FreeGrid, Obj, TechAccuracy};
use super::device::{Device, DeviceKind, Motion, Profile, Profiles, Zone};
use super::flow::{self, Flow, Place, PlaceKind};
use super::io::Hub;
use super::placement::{self, Candidate, Column, Config, CurveStep, Method, Metrics, Mount, Mover, Planner, Point, R95_PER_RMS, RadioScene};
use super::planning::{self, AppliedAntenna, CoverageRequest, Tech, blocked};
use super::radio::{self, SiteConditions};
use super::sim::{self, OccluderParts, Occluders};
use super::{RtlsConfig, RtlsState};
use crate::outcomes::stress::{restore_materials, scale_materials};
use crate::scene::PlacedObject;

/// A day the plan has to hold on. The figures match the stress test's, so the two can be read
/// side by side. They are typical assumptions, not this site's records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Day {
    AsModelled,
    BusyShift,
    MaterialsWorse,
    RacksHalfFull,
    /// The anchor the plan leans on most is dead.
    AnchorDown,
    /// A tag stood in turn at spots drawn evenly over the floor, used or not. Routes can't show
    /// what a plan built around them leaves out.
    AnywhereOnFloor,
}

/// Share of evenly drawn spots that must hold the target for a plan to count as covering the
/// floor. Lower than the routes are held to: a plan is meant to favour where tags go, and it
/// still mustn't abandon the rest. A judgement, with no measurement behind it.
pub const FLOOR_SPOTS_SHARE: f32 = 0.8;

/// How many spots that takes, and how long a tag stands at each (s).
const FLOOR_SPOTS: usize = 30;
const FLOOR_SPOT_SETTLE_S: f64 = 2.5;

impl Day {
    pub fn name(self) -> &'static str {
        match self {
            Day::AsModelled => "As modelled",
            Day::BusyShift => "Busy shift",
            Day::MaterialsWorse => "Materials 50 % worse",
            Day::RacksHalfFull => "Racks half full",
            Day::AnchorDown => "Worst anchor down",
            Day::AnywhereOnFloor => "Anywhere on the floor",
        }
    }

    /// One of the conditions a plan has to hold on along the routes.
    fn on_routes(self) -> bool {
        !matches!(self, Day::AnchorDown | Day::AnywhereOnFloor)
    }

    /// The site's conditions on this day, from how it stands now: a site already set up as
    /// crowded or half empty stays at least that way.
    fn conditions(self, now: SiteConditions) -> SiteConditions {
        match self {
            Day::BusyShift => SiteConditions { people_per_100m2: now.people_per_100m2.max(5.0), ..now },
            Day::RacksHalfFull => SiteConditions { rack_fill: now.rack_fill.min(0.5), ..now },
            Day::AsModelled | Day::MaterialsWorse | Day::AnchorDown | Day::AnywhereOnFloor => now,
        }
    }

    fn enter(self) -> InDay {
        let site = SiteConditions::current();
        SiteConditions::set(self.conditions(site));
        let materials = (self == Day::MaterialsWorse).then(|| scale_materials(1.5));
        InDay { site, materials }
    }
}

/// Puts the site back as it was, however the work inside ends.
struct InDay {
    site: SiteConditions,
    materials: Option<Vec<(radio::Material, Option<radio::MaterialCal>)>>,
}

impl Drop for InDay {
    fn drop(&mut self) {
        SiteConditions::set(self.site);
        if let Some(m) = self.materials.take() {
            restore_materials(m);
        }
    }
}

/// Prices of a plan. The defaults are placeholders so it can run at all: the result says so
/// until they're replaced.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Costing {
    pub anchor: f64,
    /// Fitting one anchor.
    pub fit: f64,
    pub cable_per_m: f64,
    /// A lift for a mount higher than a ladder job.
    pub lift_per_device: f64,
    pub lift_above_m: f32,
    /// Which of the four prices above are yours rather than placeholders.
    pub edited: [bool; 4],
}

impl Default for Costing {
    fn default() -> Self {
        Self { anchor: 150.0, fit: 120.0, cable_per_m: 2.0, lift_per_device: 60.0, lift_above_m: 4.0, edited: [false; 4] }
    }
}

impl Costing {
    /// Every price is yours, so no figure rests on a placeholder.
    pub fn given(&self) -> bool {
        self.edited.iter().all(|e| *e)
    }

    pub fn of(&self, anchors: &[AnchorPlan], head_end: Option<Vec3>) -> f64 {
        anchors
            .iter()
            .map(|a| {
                let p = Vec3::from(a.pos);
                let cable = head_end.map_or(0.0, |h| ((p.x - h.x).abs() + (p.y - h.y).abs() + (p.z - h.z).abs()) as f64 * self.cable_per_m);
                self.anchor + self.fit + cable + if p.y > self.lift_above_m { self.lift_per_device } else { 0.0 }
            })
            .sum()
    }
}

#[derive(Clone, Debug)]
pub struct SmartRequest {
    pub target_m: f32,
    pub tag_height: f32,
    pub max_anchors: Option<usize>,
    pub test_s: f64,
    pub walkers: usize,
    pub costing: Costing,
    /// Run today's Auto deploy on the same tests.
    pub compare: bool,
}

impl Default for SmartRequest {
    fn default() -> Self {
        Self { target_m: 0.3, tag_height: 1.2, max_anchors: None, test_s: 20.0, walkers: 3, costing: Costing::default(), compare: true }
    }
}

/// What it took the site to be, in the terms a person would check.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SiteReading {
    pub racks: usize,
    pub doors: usize,
    pub vehicles: usize,
    pub drawn_routes: usize,
    pub walkable_m2: f32,
    /// Share of the traffic through the busiest fifth of the walkable floor.
    pub busiest_fifth: f32,
    /// Where the cables run to, if anything in the scene looks like it.
    pub head_end: Option<String>,
}

/// What breaks if one anchor goes.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Why {
    /// Share of tag time that loses a usable position.
    pub lost_share: f32,
    /// The plan's p95 without it, as modelled; `None` when too much loses its position to have one.
    pub p95_without_m: Option<f32>,
    /// What's near the tag time it holds up.
    pub near: Option<String>,
    /// The day it counts for most, which these figures are from.
    pub day: Option<Day>,
}

impl Why {
    pub fn text(&self, target_m: f32) -> String {
        let near = self.near.as_ref().map_or(String::new(), |n| format!(" around {n}"));
        // An ordinary day needs no saying.
        let on = match self.day {
            Some(d) if d != Day::AsModelled => format!("{}: ", d.name()),
            _ => String::new(),
        };
        if self.lost_share >= 0.005 {
            format!("{on}without it {:.0} % of tag time{near} loses its position", self.lost_share * 100.0)
        } else {
            match self.p95_without_m {
                Some(p) if p <= target_m => format!("A spare, by the prediction: without it the plan stays within {} on the day it counts for most", deploy::dist(p)),
                Some(p) => format!("{on}sharpens{near}: without it 95 % of positions land within {}", deploy::dist(p)),
                None => format!("{on}without it too many positions{near} are lost for a 95 % figure"),
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AnchorPlan {
    pub pos: [f32; 3],
    pub facing: [f32; 3],
    pub mount: String,
    pub why: Why,
    /// Device id once placed.
    pub id: String,
}

/// One band of the cone an anchor has to serve.
#[derive(Clone, Debug, Serialize)]
pub struct BriefRow {
    pub from_deg: f32,
    pub to_deg: f32,
    /// Share of the position information coming from this band.
    pub share: f32,
    pub longest_m: f32,
    /// The least gain (dBi) towards this band that still closes the longest line-of-sight link.
    pub need_dbi: f32,
}

/// What this site asks of the anchor antenna, from the plan rather than from a catalogue.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AntennaBrief {
    pub mount: String,
    pub anchors: usize,
    pub freq_mhz: f32,
    /// 95 % of the position information arrives within this angle of where the mount faces.
    pub cone_deg: f32,
    pub rows: Vec<BriefRow>,
    /// How far the antenna's shape may shift a range across that cone before the target is at
    /// risk, given the noise the plan already has and its geometry (m).
    pub shift_budget_m: f32,
    /// Typical geometry factor: position error per metre of range error.
    pub hdop: f32,
}

impl AntennaBrief {
    /// The cone as angles from the antenna's own axis, which depends on the antenna: a patch's
    /// axis is where it faces, so the cone starts on it, while an upright wire's is straight up,
    /// so a ceiling mount looks down around it and the others out across it. Off a wall most
    /// of the angle is sideways, which a wire doesn't care about, so that range errs wide.
    pub fn theta_range(&self, axis_is_facing: bool) -> (f32, f32) {
        if axis_is_facing {
            (0.0, self.cone_deg.min(180.0))
        } else if self.mount == "ceiling" {
            ((180.0 - self.cone_deg).max(0.0), 180.0)
        } else {
            let c = self.cone_deg.min(90.0);
            (90.0 - c, 90.0 + c)
        }
    }
}

/// Whether a template's axis is where it faces, as patches and arrays have it, not straight up.
pub fn axis_is_facing(template: &str) -> bool {
    let t = template.to_ascii_lowercase();
    ["patch", "array", "pifa", "microstrip"].iter().any(|k| t.contains(k))
}

/// How a set of anchors came out on one day, in the simulator.
#[derive(Clone, Debug, Serialize)]
pub struct DayResult {
    pub day: Day,
    /// The worse of moving and parked tags, an update without a position counting as a miss.
    /// `None`: more than 5 % of updates had none.
    pub p95_m: Option<f32>,
    pub passed: bool,
    /// Moving and parked tags apart, the same way: the two have different troubles.
    pub moving_p95_m: Option<f32>,
    pub parked_p95_m: Option<f32>,
    /// Share of updates with a position, in the worse of the two.
    pub availability: Option<f32>,
    /// Anywhere on the floor: spots that held the target, of those tried.
    pub spots: Option<(usize, usize)>,
    /// Worst-measured square on the floor, as (x, z).
    pub weakest_at: Option<[f32; 2]>,
    /// What's near it.
    pub weakest_near: Option<String>,
}

/// One set of anchors.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SetPlan {
    pub anchors: Vec<AnchorPlan>,
    /// Predicted p95 by day, in the order of `SmartResult::days`.
    pub predicted_p95_m: Vec<Option<f32>>,
    /// Share of tag time with a usable position, worst day.
    pub tag_time_coverage: f32,
    /// Share of the floor with one, as modelled, however little it's used.
    pub floor_coverage: f32,
    /// Predicted p95 with any one anchor gone, where that was planned for.
    pub predicted_k1_m: Option<f32>,
    /// The anchor the plan leans on most.
    #[serde(skip)]
    pub critical: Option<usize>,
    #[serde(skip)]
    pub brief: AntennaBrief,
}

impl SetPlan {
    fn key(&self) -> String {
        let mut v: Vec<[i32; 3]> = self.anchors.iter().map(|a| [(a.pos[0] * 200.0) as i32, (a.pos[1] * 200.0) as i32, (a.pos[2] * 200.0) as i32]).collect();
        v.sort_unstable();
        format!("{v:?}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum OptionKind {
    Lean,
    /// More than Lean, and still short of holding on every day.
    Stronger,
    Balanced,
    Bulletproof,
    /// What today's Auto deploy places, on the same tests.
    Standard,
}

impl OptionKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Lean => "Lean",
            Self::Stronger => "Stronger",
            Self::Balanced => "Balanced",
            Self::Bulletproof => "Bulletproof",
            Self::Standard => "Plain Auto deploy",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PlanOption {
    pub kind: OptionKind,
    pub set: SetPlan,
    pub tests: Vec<DayResult>,
    pub cost: f64,
}

impl PlanOption {
    pub fn test(&self, day: Day) -> Option<&DayResult> {
        self.tests.iter().find(|t| t.day == day)
    }

    /// Every day it was tested on along the routes.
    pub fn holds_bad_days(&self) -> bool {
        let days: Vec<&DayResult> = self.tests.iter().filter(|t| t.day.on_routes()).collect();
        !days.is_empty() && days.iter().all(|t| t.passed)
    }

    pub fn survives_failure(&self) -> bool {
        self.test(Day::AnchorDown).is_some_and(|t| t.passed)
    }

    /// Enough of the floor holds the target, used or not.
    pub fn covers_floor(&self) -> bool {
        self.test(Day::AnywhereOnFloor).is_some_and(|t| t.passed)
    }

    /// The worst day along the routes: (day, p95, `None` if too many updates had no position).
    pub fn worst_day(&self) -> Option<(Day, Option<f32>)> {
        let rank = |t: &&DayResult| t.p95_m.unwrap_or(f32::INFINITY);
        self.tests.iter().filter(|t| t.day.on_routes()).max_by(|a, b| rank(a).total_cmp(&rank(b))).map(|t| (t.day, t.p95_m))
    }

    /// The plan in a sentence, from what the tests measured.
    pub fn verdict(&self, target_m: f32) -> String {
        let short = |t: &DayResult| t.p95_m.map_or("too many gaps".to_string(), deploy::dist);
        let misses: Vec<String> = self.tests.iter().filter(|t| !t.passed && t.day.on_routes()).map(|t| format!("{} ({})", t.day.name(), short(t))).collect();
        if let Some(t) = self.test(Day::AnywhereOnFloor).filter(|t| !t.passed) {
            let (held, tried) = t.spots.unwrap_or((0, FLOOR_SPOTS));
            let routes = if misses.is_empty() { format!("Holds {} along the routes, but", deploy::dist(target_m)) } else { format!("Misses {} on: {}, and", deploy::dist(target_m), misses.join(", ")) };
            return format!("{routes} only {held} of {tried} spots across the whole floor hold it.");
        }
        if misses.is_empty() {
            let spare = match self.test(Day::AnchorDown) {
                Some(t) if t.passed => " Also holds with its most relied-on anchor dead.".to_string(),
                Some(t) => format!(" A dead anchor is the one thing that breaks it ({}).", short(t)),
                None => String::new(),
            };
            format!("Holds {} on every day tested.{spare}", deploy::dist(target_m))
        } else {
            format!("Misses {} on: {}.", deploy::dist(target_m), misses.join(", "))
        }
    }
}

/// One point of what each extra anchor buys.
#[derive(Clone, Debug, Serialize)]
pub struct CurveRow {
    pub anchors: usize,
    pub cost: f64,
    pub predicted_as_modelled_m: Option<f32>,
    pub predicted_worst_day_m: Option<f32>,
    /// To price it again.
    #[serde(skip)]
    pub set: Vec<AnchorPlan>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SmartResult {
    pub site: SiteReading,
    pub target_m: f32,
    pub test_s: f64,
    pub days: Vec<Day>,
    pub curve: Vec<CurveRow>,
    pub options: Vec<PlanOption>,
    /// Which option is on the floor.
    pub applied: Option<usize>,
    pub recommended: Option<usize>,
    pub standard: Option<PlanOption>,
    /// Compared with what plain Auto deploy places, in plain words.
    pub compared: Option<String>,
    pub brief: AntennaBrief,
    pub prices_are_placeholders: bool,
    pub notes: Vec<String>,
    pub took_s: f32,
    /// Seconds spent in each stage, and the longest the window waited on one bite.
    pub timings: Vec<(String, f32)>,
    pub longest_bite_s: f32,
    /// Couldn't plan at all, and why.
    pub failed: Option<String>,
    /// Where the cables run to.
    #[serde(skip)]
    pub head_end_at: Option<Vec3>,
    /// What the site was when these were tested.
    #[serde(skip)]
    pub signature: Option<u64>,
    /// It has changed since.
    pub stale: bool,
}

impl SmartResult {
    /// New prices change what each plan costs, and so which is the cheapest that holds. Choices
    /// already set aside as pointless stay aside: finding plans again brings them back.
    pub fn recost(&mut self, c: &Costing) {
        self.refresh_recommendation(c);
        self.prices_are_placeholders = !c.given();
    }

    /// Costs, the recommendation, and everything said about it, from the same prices.
    fn refresh_recommendation(&mut self, c: &Costing) {
        let head_end = self.head_end_at;
        for o in self.options.iter_mut().chain(self.standard.iter_mut()) {
            o.cost = c.of(&o.set.anchors, head_end);
        }
        for row in &mut self.curve {
            row.cost = c.of(&row.set, head_end);
        }
        self.recommended = recommend(&self.options);
        let best = self.recommended.and_then(|i| self.options.get(i));
        self.compared = match (best, &self.standard) {
            (Some(b), Some(p)) => Some(compare_text(b, p, self.target_m)),
            _ => None,
        };
        self.brief = best.map_or_else(AntennaBrief::default, |b| b.set.brief.clone());
    }
}

fn finite(v: f32) -> Option<f32> {
    v.is_finite().then_some(v)
}

/// Sizes to try: every one for as far as a site is likely to need, so the fewest that pass can
/// be found, then spreading out. A very big hall doesn't need 41 and 42 told apart.
pub fn sizes(from: usize, cap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut n = from.max(1);
    while n <= cap {
        out.push(n);
        n += if n < 24 { 1 } else { n / 8 };
    }
    if out.last() != Some(&cap) && cap >= from.max(1) {
        out.push(cap);
    }
    out
}

/// The plan's report of a mount.
fn mount_name(m: Mount) -> &'static str {
    match m {
        Mount::Ceiling => "ceiling",
        Mount::Wall => "wall",
        Mount::Column => "column",
        Mount::Rack => "rack",
        Mount::Free => "free",
    }
}

/// What the plan stood on, read from the scene.
struct Reading {
    flow: Flow,
    grid: FreeGrid,
    area: (Vec2, Vec2),
    site: SiteReading,
    spots: Vec<(Vec3, Vec3)>,
    mounts: Vec<Mount>,
    zones: Vec<(Vec3, Vec3)>,
    landmarks: Vec<(String, Vec2)>,
    head_end: Option<Vec3>,
    has_racks: bool,
    objs: Vec<Obj>,
    /// The walks the test tags take: more of them on a bigger site, so a test sees the site.
    tours: Vec<Vec<Vec2>>,
    /// Long enough for them to get round, within reason (simulated s).
    test_s: f64,
    /// Where tags stand or ride whatever the traffic does: on vehicles, and your own.
    tag_spots: Vec<Vec3>,
}

fn nearest_landmark(landmarks: &[(String, Vec2)], at: Vec2) -> Option<String> {
    landmarks.iter().min_by(|a, b| a.1.distance_squared(at).total_cmp(&b.1.distance_squared(at))).map(|l| l.0.clone())
}

/// The spots along a rack's two long faces where somebody stands to reach it.
fn rack_faces(o: &Obj, grid: &FreeGrid) -> Vec<Vec2> {
    let size = o.max - o.min;
    let along_x = size.x >= size.z;
    let len = if along_x { size.x } else { size.z };
    let n = ((len / 2.5).ceil() as usize).clamp(1, 6);
    let mid = (o.min + o.max) * 0.5;
    let mut out = Vec::new();
    for k in 0..n {
        let a = (k as f32 + 0.5) / n as f32;
        for side in [-1.0f32, 1.0] {
            let p = if along_x { Vec2::new(o.min.x + size.x * a, mid.z + side * (size.z * 0.5 + 0.7)) } else { Vec2::new(mid.x + side * (size.x * 0.5 + 0.7), o.min.z + size.z * a) };
            // A face against a wall or another rack has nowhere to stand.
            if let Some((x, z)) = grid.nearest_free(p)
                && grid.center(x, z).distance(p) < 1.0
            {
                out.push(p);
            }
        }
    }
    out
}

// Not "cabinet" on its own: a warehouse is full of those, and most hold drums or tools.
const NETWORK_ROOM: [&str; 6] = ["server", "comms", "network", "switch", "telecom", "datacenter"];

/// Reads the site. Removes what an earlier run placed first.
fn read_site(world: &mut World, req: &SmartRequest) -> Result<Reading, String> {
    clear_auto(world);
    let objs = deploy::objects(world);
    let boxes: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| !(o.floor || o.structure && o.max.y < 0.1)).map(|o| (o.min, o.max)).collect();
    let floor = objs.iter().filter(|o| o.floor).fold(None, |a: Option<(Vec2, Vec2)>, o| Some(a.map_or((o.min.xz(), o.max.xz()), |(l, h): (Vec2, Vec2)| (l.min(o.min.xz()), h.max(o.max.xz())))));
    let (lo, hi) = if let Some(f) = floor {
        f
    } else if boxes.is_empty() {
        (Vec2::splat(-6.0), Vec2::splat(6.0))
    } else {
        let lo = boxes.iter().fold(Vec2::splat(f32::MAX), |a, b| a.min(b.0.xz()));
        let hi = boxes.iter().fold(Vec2::splat(f32::MIN), |a, b| a.max(b.1.xz()));
        (lo - Vec2::splat(1.5), hi + Vec2::splat(1.5))
    };
    let size = hi - lo;
    let cell = (size.x * size.y / 40_000.0).sqrt().max(0.5);
    let grid = FreeGrid::new(lo, hi, cell, &boxes, 0.4);

    let anchor_h = world.resource::<Profiles>().default_for(DeviceKind::UwbAnchor).map_or(2.5, |p| p.mount_height_m);
    let (spots, mounts) = deploy::scene_mounts(world, anchor_h, 3.0, true);
    if spots.is_empty() {
        return Err(deploy::NO_MOUNTS.to_string());
    }

    let mut places: Vec<Place> = Vec::new();
    let mut landmarks: Vec<(String, Vec2)> = Vec::new();
    let racks: Vec<&Obj> = objs.iter().filter(|o| deploy::is_rack(o)).collect();
    let doors: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| deploy::door_like(&o.name)).map(|o| (o.min, o.max)).collect();
    let walls: Vec<(Vec3, Vec3)> = objs.iter().filter(|o| o.wall).map(|o| (o.min, o.max)).collect();
    let openings = placement::find_openings(&doors, &walls);
    for o in openings.iter().take(8) {
        places.push(Place { at: o.center.xz(), kind: PlaceKind::Door, pull: 2.0 });
    }
    for o in objs.iter().filter(|o| deploy::door_like(&o.name)) {
        landmarks.push((o.name.clone(), ((o.min + o.max) * 0.5).xz()));
    }
    // Rack faces, spread evenly and capped overall: each place means another search of the
    // floor and its share of the trips, so a thousand racks mustn't mean a thousand places.
    let faces: Vec<Vec2> = racks.iter().flat_map(|r| rack_faces(r, &grid)).collect();
    for r in &racks {
        landmarks.push((r.name.clone(), ((r.min + r.max) * 0.5).xz()));
    }
    for p in faces.iter().step_by((faces.len() / 48).max(1)) {
        places.push(Place { at: *p, kind: PlaceKind::Rack, pull: 1.0 });
    }
    let vehicles: Vec<&Obj> = objs.iter().filter(|o| deploy::matches(&o.name, &deploy::VEHICLES)).collect();
    for v in vehicles.iter().take(8) {
        places.push(Place { at: ((v.min + v.max) * 0.5).xz(), kind: PlaceKind::Vehicle, pull: 0.5 });
    }
    let zones: Vec<Zone> = world.query::<&Zone>().iter(world).filter(|z| !z.auto).cloned().collect();
    for z in &zones {
        let c = Vec2::new(z.min[0] + z.max[0], z.min[2] + z.max[2]) * 0.5;
        places.push(Place { at: c, kind: PlaceKind::Zone, pull: 1.0 });
        landmarks.push((z.name.clone(), c));
    }
    if !places.is_empty()
        && let Some(s) = flow::clearest_spot(&grid)
    {
        places.push(Place { at: s, kind: PlaceKind::Staging, pull: 1.5 });
    }
    // Routes somebody set up count for more than any guess.
    let drawn: Vec<(Vec<Vec2>, f32)> = world
        .query::<&Device>()
        .iter(world)
        .filter(|d| !d.auto && d.kind.is_mobile())
        .filter_map(|d| match &d.motion {
            Motion::Path { points, .. } if points.len() >= 2 => Some((points.iter().map(|p| Vec2::new(p[0], p[2])).collect(), 1.0)),
            _ => None,
        })
        .collect();
    let f = flow::build(&grid, &places, &drawn);
    let free = grid.free.iter().filter(|x| **x).count();

    // Where the cables go, if the scene says.
    let room = objs.iter().find(|o| !o.structure && deploy::matches(&o.name, &NETWORK_ROOM));
    let head_end = room.map(|o| (o.min + o.max) * 0.5);
    let site = SiteReading {
        racks: racks.len(),
        doors: openings.len(),
        vehicles: vehicles.len(),
        drawn_routes: drawn.len(),
        walkable_m2: free as f32 * cell * cell,
        busiest_fifth: f.concentration(0.2),
        head_end: room.map(|o| o.name.clone()),
    };
    // With nothing to draw routes from, walkers circle the floor, as Auto deploy has them.
    let inset = 1.0;
    let corners = [Vec2::new(lo.x + inset, lo.y + inset), Vec2::new(hi.x - inset, lo.y + inset), Vec2::new(hi.x - inset, hi.y - inset), Vec2::new(lo.x + inset, hi.y - inset)];
    let cells: Vec<(usize, usize)> = corners.iter().filter_map(|p| grid.nearest_free(*p)).collect();
    let mut ring: Vec<Vec2> = Vec::new();
    for k in 0..cells.len() {
        if let Some(seg) = grid.path(cells[k], cells[(k + 1) % cells.len()]) {
            ring.extend(seg);
        }
    }
    let ring = deploy::simplify(&ring);
    // More walkers on a bigger floor, and long enough to get round: a few tags near the start of
    // their routes say little about the rest of a big hall.
    let walkers = ((free as f32 * cell * cell / 120.0).round() as usize).clamp(req.walkers.min(10), 10);
    let mut tours = f.tours(&grid, walkers);
    if tours.is_empty() && ring.len() >= 2 {
        tours = vec![ring];
    }
    // Long enough to see the walks, within what's worth waiting for: they start spread out.
    let longest = tours.iter().map(|t| t.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>()).fold(0.0, f32::max);
    let test_s = ((longest / 1.3) as f64).clamp(req.test_s.min(30.0), 30.0);
    let zones_boxes = zones.iter().map(|z| (Vec3::from(z.min), Vec3::from(z.max))).collect();
    // Tags that don't walk the routes still have to be found. A vehicle's sits on top of it,
    // clear of the object itself, which its own radio doesn't have to get through.
    let mut tag_spots: Vec<Vec3> = vehicles.iter().map(|v| Vec3::new((v.min.x + v.max.x) * 0.5, v.max.y + 0.05, (v.min.z + v.max.z) * 0.5)).collect();
    let own_tags: Vec<Vec3> = world.query::<(&Device, &Transform)>().iter(world).filter(|(d, _)| !d.auto && d.kind.is_mobile() && matches!(d.motion, Motion::Static)).map(|(_, t)| t.translation).collect();
    tag_spots.extend(own_tags);
    let has_racks = !racks.is_empty();
    Ok(Reading { flow: f, grid, area: (lo, hi), site, spots, mounts, zones: zones_boxes, landmarks, head_end, has_racks, objs, tours, test_s, tag_spots })
}

fn clear_auto(world: &mut World) {
    let old: Vec<Entity> = world.query::<(Entity, Option<&Device>, Option<&Zone>)>().iter(world).filter(|(_, d, z)| d.is_some_and(|d| d.auto) || z.is_some_and(|z| z.auto)).map(|(e, ..)| e).collect();
    for e in old {
        world.despawn(e);
    }
    world.flush();
}

struct PlanInput {
    flow: Flow,
    spots: Vec<(Vec3, Vec3)>,
    mounts: Vec<Mount>,
    zones: Vec<(Vec3, Vec3)>,
    landmarks: Vec<(String, Vec2)>,
    head_end: Option<Vec3>,
    days: Vec<Day>,
    target_m: f32,
    tag_height: f32,
    budget: usize,
    /// What plain Auto deploy placed: position, facing.
    standard: Vec<(Vec3, Vec3)>,
    tag_spots: Vec<Vec3>,
}

#[derive(Default)]
struct PlanOutput {
    /// As modelled only, where tags go.
    lean: Vec<SetPlan>,
    /// Every day at once.
    robust: Vec<SetPlan>,
    standard: Option<SetPlan>,
    /// Which set of each the search thinks is the first good enough.
    first_ok: [Option<usize>; 2],
    notes: Vec<String>,
}

/// A device's column on one stretch of the points, in the day that stretch stands for.
#[allow(clippy::too_many_arguments)]
fn column_on<S: RadioScene, G: Fn(&Profile, Vec3, Vec3) -> f32 + Sync, B: Fn(Vec3, Vec3) -> f32 + Sync>(scene: &S, pts: &[Point], offset: usize, method: Method, dev: &Profile, tag: &Profile, pos: Vec3, facing: Vec3, gain: &G, shape: &B) -> Column {
    let mut col = placement::column_shaped(scene, pts, method, dev, tag, pos, facing, gain, shape);
    col.iter_mut().for_each(|e| e.pt += offset as u32);
    col
}

/// Slides a device along its surface, scoring the move on every day the search covers.
struct DayMover<'a, G, B> {
    scene: &'a Occluders,
    pts: &'a [Point],
    days: &'a [Day],
    per_day: usize,
    method: Method,
    prof: &'a Profile,
    tag: &'a Profile,
    gain: &'a G,
    shape: &'a B,
    lo: Vec2,
    hi: Vec2,
}

impl<G: Fn(&Profile, Vec3, Vec3) -> f32 + Sync, B: Fn(Vec3, Vec3) -> f32 + Sync> Mover for DayMover<'_, G, B> {
    fn settle(&self, from: &Candidate, to: Vec3) -> Option<Vec3> {
        planning::slide_along_surface(self.scene, from, to, self.lo, self.hi)
    }

    fn column(&self, pos: Vec3, facing: Vec3) -> Column {
        let mut out = Vec::new();
        for (i, day) in self.days.iter().enumerate() {
            let _in_day = day.enter();
            let start = i * self.per_day;
            out.extend(column_on(self.scene, &self.pts[start..start + self.per_day], start, self.method, self.prof, self.tag, pos, facing, self.gain, self.shape));
        }
        out
    }
}

fn plan_enough(m: &Metrics, target_m: f32) -> bool {
    m.coverage >= 0.95 && m.p95 <= target_m
}

/// A set found by the search, as the plan reports it: why each anchor is there, how it's
/// predicted to do, and what it asks of the antenna.
#[allow(clippy::too_many_arguments)]
fn describe(p: &mut Planner, chosen: &[usize], days: &[Day], per_day: usize, pts: &[Point], landmarks: &[(String, Vec2)], freq_mhz: f32, budget_db: f32, tag_gain: f32, target_m: f32) -> SetPlan {
    p.set(chosen);
    let base_cover = p.covered();
    let m = p.metrics();
    let floor_w: f64 = pts[..per_day].iter().filter(|x| x.floor).map(|x| x.weight as f64).sum::<f64>().max(1e-9);
    let mut anchors = Vec::new();
    let mut lean_on: Option<(usize, f32, f32)> = None;
    for (slot, &c) in chosen.iter().enumerate() {
        let (without, cover) = p.without(c);
        // What it does on each day: an anchor may only matter on the bad ones.
        let lost_on = |i: usize| -> (f32, Vec2) {
            let start = i * per_day;
            let (mut lost, mut at) = (0.0f64, Vec2::ZERO);
            for k in start..start + per_day {
                if pts[k].floor && base_cover[k] && !cover[k] {
                    lost += pts[k].weight as f64;
                    at += pts[k].pos.xz() * pts[k].weight;
                }
            }
            ((lost / floor_w) as f32, if lost > 0.0 { at / lost as f32 } else { Vec2::ZERO })
        };
        let p95_on = |i: usize| without.by_condition.get(i).map_or(f32::INFINITY, |d| d.p95);
        // The day it counts for most: real tag time lost, then the target missed, then how far
        // positions slip. A sliver lost mustn't outrank a day that misses the target.
        let rank = |i: usize| {
            let (lost, slip) = (lost_on(i).0, if p95_on(i).is_finite() { p95_on(i) } else { 1e6 });
            if lost >= 0.005 {
                (2, lost)
            } else if slip > target_m {
                (1, slip)
            } else {
                (0, slip)
            }
        };
        let hardest = (0..days.len()).max_by(|a, b| rank(*a).partial_cmp(&rank(*b)).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
        let (share, where_lost) = lost_on(hardest);
        // The tag time it holds up, or failing that the spots it tells most about.
        let near = if share > 0.0 {
            nearest_landmark(landmarks, where_lost)
        } else {
            let col = p.column_of(c);
            let (mut w, mut sum) = (0.0f32, Vec2::ZERO);
            for e in col.iter().filter(|e| (e.pt as usize) < per_day) {
                let weight = e.w * pts[e.pt as usize].weight;
                w += weight;
                sum += pts[e.pt as usize].pos.xz() * weight;
            }
            (w > 0.0).then(|| nearest_landmark(landmarks, sum / w)).flatten()
        };
        // Which one a dead anchor's test takes out: that test runs on an ordinary day.
        let (ordinary, ordinary_p95) = (lost_on(0).0, p95_on(0));
        let score = if ordinary_p95.is_finite() { ordinary_p95 } else { 1e6 };
        if lean_on.is_none_or(|(_, s, q)| ordinary > s || (ordinary == s && score > q)) {
            lean_on = Some((slot, ordinary, score));
        }
        let cand = p.cands[c];
        let why = Why { lost_share: share, p95_without_m: finite(p95_on(hardest)), near, day: days.get(hardest).copied() };
        anchors.push(AnchorPlan { pos: cand.pos.to_array(), facing: cand.facing.to_array(), mount: mount_name(cand.mount).into(), why, id: String::new() });
    }
    let brief = antenna_brief(p, chosen, per_day, pts, freq_mhz, budget_db, tag_gain, target_m, &anchors);
    SetPlan {
        anchors,
        predicted_p95_m: (0..days.len()).map(|i| m.by_condition.get(i).and_then(|d| finite(d.p95))).collect(),
        tag_time_coverage: m.coverage,
        floor_coverage: m.by_condition.first().map_or(0.0, |d| d.floor_coverage),
        predicted_k1_m: finite(m.p95_k1),
        critical: lean_on.map(|x| x.0),
        brief,
    }
}

/// Gain needed (dBi) towards a tag `d` away to close a line-of-sight link. Antenna gains and
/// budgets are in dB; `budget_db` is the link's margin over free-space loss at 1 m and 1 MHz.
pub fn gain_needed_dbi(d: f32, freq_mhz: f32, budget_db: f32, tag_gain: f32) -> f32 {
    20.0 * d.max(0.1).log10() + 20.0 * freq_mhz.log10() - 27.55 - budget_db - tag_gain
}

/// How much a shift of `shift_m` on a range is allowed, for a target `target_m` (95 % radius),
/// noise `rms_m` already in the plan and geometry factor `hdop`.
pub fn shift_budget(target_m: f32, rms_m: f32, hdop: f32) -> f32 {
    let target_rms = target_m / R95_PER_RMS;
    (target_rms * target_rms - rms_m * rms_m).max(0.0).sqrt() / hdop.max(0.5)
}

#[allow(clippy::too_many_arguments)]
fn antenna_brief(p: &Planner, chosen: &[usize], per_day: usize, pts: &[Point], freq_mhz: f32, budget_db: f32, tag_gain: f32, target_m: f32, anchors: &[AnchorPlan]) -> AntennaBrief {
    let mut by_mount: HashMap<&str, usize> = HashMap::new();
    for a in anchors {
        *by_mount.entry(a.mount.as_str()).or_default() += 1;
    }
    // Most anchors first, then a fixed order: a brief must not depend on hashing.
    let Some(mount) = by_mount.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|x| x.0.to_string()) else { return AntennaBrief::default() };
    const BAND_DEG: f32 = 15.0;
    let mut bands = vec![0.0f32; 12];
    // How far the links in each band reach, by what they contribute: a far link that adds next
    // to nothing mustn't set what the antenna has to do.
    let mut reach: Vec<Vec<(f32, f32)>> = vec![Vec::new(); bands.len()];
    let mut seen: Vec<(f32, f32)> = Vec::new();
    let mut count = 0;
    let mut sums = vec![[0.0f32; 3]; per_day];
    for (&c, a) in chosen.iter().zip(anchors) {
        let cand = &p.cands[c];
        let col = p.column_of(c);
        let uses_mount = a.mount == mount;
        count += uses_mount as usize;
        let fwd = cand.facing.normalize_or(Vec3::NEG_Y);
        for e in col.iter().filter(|e| (e.pt as usize) < per_day) {
            let u = Vec3::from_array(e.u);
            let k = e.pt as usize;
            if e.p >= 0.5 {
                let s = &mut sums[k];
                s[0] += u.x * u.x;
                s[1] += u.x * u.z;
                s[2] += u.z * u.z;
            }
            if !uses_mount {
                continue;
            }
            let angle = fwd.angle_between(u).to_degrees();
            let weight = e.w * pts[k].weight;
            let band = ((angle / BAND_DEG) as usize).min(bands.len() - 1);
            bands[band] += weight;
            reach[band].push((pts[k].pos.distance(cand.pos), weight));
            seen.push((angle, weight));
        }
    }
    let total: f32 = bands.iter().sum();
    let cone = placement::weighted_p95(&mut seen);
    let rows = bands
        .iter()
        .zip(reach.iter_mut())
        .enumerate()
        .filter(|(_, (b, _))| **b > 0.0)
        .map(|(i, (b, r))| {
            let far = placement::weighted_p95(r);
            BriefRow { from_deg: i as f32 * BAND_DEG, to_deg: (i + 1) as f32 * BAND_DEG, share: *b / total.max(1e-6), longest_m: far, need_dbi: gain_needed_dbi(far, freq_mhz, budget_db, tag_gain) }
        })
        .collect();
    // The geometry where tags typically are...
    let mut hdops: Vec<(f32, f32)> = Vec::new();
    for (k, s) in sums.iter().enumerate() {
        let det = s[0] * s[2] - s[1] * s[1];
        if det > 1e-4 {
            hdops.push((((s[0] + s[2]) / det).sqrt(), pts[k].weight));
        }
    }
    // ...and the noise at the plan's weak end on its hardest day, not at a typical spot: the
    // target is a 95 % figure, so that's what the antenna has to leave room in.
    let mut errs: Vec<(f32, f32)> = (0..pts.len()).filter_map(|k| finite(p.err_at(k)).map(|e| (e, pts[k].weight))).collect();
    let weak_end = placement::weighted_p95(&mut errs);
    let median = |v: &mut Vec<(f32, f32)>| {
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = v.iter().map(|x| x.1).sum::<f32>() * 0.5;
        let mut seen = 0.0;
        for (value, w) in v.iter() {
            seen += w;
            if seen >= half {
                return *value;
            }
        }
        0.0
    };
    let hdop = median(&mut hdops).max(0.5);
    // With no position anywhere there's no room to give: the infinite figure leaves none.
    AntennaBrief { mount, anchors: count, freq_mhz, cone_deg: if cone.is_finite() { cone } else { 0.0 }, rows, shift_budget_m: shift_budget(target_m, weak_end, hdop), hdop }
}

/// The search, a bite at a time so the window keeps drawing: the state it keeps between bites,
/// which owns everything, since a `Planner` only lives for one.
struct PlanWork {
    input: PlanInput,
    step: PlanStep,
    pts: Vec<Point>,
    condition_of: Vec<u8>,
    per_day: usize,
    lo: Vec2,
    hi: Vec2,
    /// For the search on every day.
    cands: Vec<Candidate>,
    cols: Vec<Column>,
    fixed: Vec<(Vec3, Column)>,
    /// For the search as modelled, which fine-tunes its own spots.
    lean_cands: Vec<Candidate>,
    lean_cols: Vec<Column>,
    /// What plain Auto deploy chose, among `cands`.
    standard_at: (usize, usize),
    tried: Vec<usize>,
    at: usize,
    past: Option<usize>,
    lean_steps: Vec<CurveStep>,
    robust_steps: Vec<CurveStep>,
    out: PlanOutput,
    progress: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanStep {
    Setup,
    /// Columns on one day: it changes what every ray returns.
    Day(usize),
    Lean,
    DescribeLean,
    Robust,
    DescribeRobust,
    Done,
}

impl PlanWork {
    fn new(input: PlanInput) -> Box<Self> {
        Box::new(Self {
            input,
            step: PlanStep::Setup,
            pts: Vec::new(),
            condition_of: Vec::new(),
            per_day: 0,
            lo: Vec2::ZERO,
            hi: Vec2::ZERO,
            cands: Vec::new(),
            cols: Vec::new(),
            fixed: Vec::new(),
            lean_cands: Vec::new(),
            lean_cols: Vec::new(),
            standard_at: (0, 0),
            tried: Vec::new(),
            at: 0,
            past: None,
            lean_steps: Vec::new(),
            robust_steps: Vec::new(),
            out: PlanOutput::default(),
            progress: "Searching".into(),
        })
    }
}

/// Takes in a set just found. Returns whether this curve has gone as far as it should: nothing
/// more earns its place, or `beyond` sizes past the first that looked enough.
fn note_step(steps: &mut Vec<CurveStep>, step: CurveStep, n: usize, at: &mut usize, past: &mut Option<usize>, sizes_tried: usize, target_m: f32, beyond: usize) -> bool {
    let size = step.chosen.len();
    let good = plan_enough(&step.metrics, target_m);
    if steps.last().is_none_or(|l| l.chosen.len() < size) {
        steps.push(step);
    }
    *past = match *past {
        None if good => Some(0),
        Some(k) => Some(k + 1),
        other => other,
    };
    *at += 1;
    size < n || *at >= sizes_tried || past.is_some_and(|k| k >= beyond)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn plan_bite(
    In(mut work): In<Box<PlanWork>>,
    profiles: Res<Profiles>,
    devices: Query<(Entity, &Device, &Transform)>,
    objects: Query<(&PlacedObject, &GlobalTransform)>,
    parts: OccluderParts,
    occ_objects: Query<(Entity, &PlacedObject, &Transform)>,
    (applied, library): (Res<AppliedAntenna>, Res<AntennaLibrary>),
) -> Box<PlanWork> {
    let w = &mut *work;
    let Some(prof) = profiles.default_for(DeviceKind::UwbAnchor).cloned() else {
        w.step = PlanStep::Done;
        return work;
    };
    let tag = planning::planned_tag(&profiles, Tech::Uwb, devices.iter().map(|(_, d, _)| d));
    let method = Method::of(Tech::Uwb, &prof, &tag);
    let occ = Occluders::build(&parts, &occ_objects);
    let ants = Antennas { library: &library, applied: &applied };
    let tag_gain = ants.tag_gain_unaimed(None, &tag);
    let gain = |p: &Profile, fwd: Vec3, dir: Vec3| ants.gain(None, Tech::Uwb, true, p, fwd, dir) + tag_gain;
    let shape = |fwd: Vec3, dir: Vec3| ants.bias_m(None, Tech::Uwb, true, fwd, dir);
    let need = method.need();
    let target = w.input.target_m;
    let cfg = Config {
        method,
        need,
        target_m: target,
        err_cap: target * 3.0,
        min_spacing_m: 1.5,
        target_coverage: Some(0.95),
        target_p95: Some(target),
        zone_count: w.input.zones.len(),
        weighted: true,
        ..Default::default()
    };
    let sync_prof = prof.clone();
    let sync_scene = &occ;
    let sync = move |a: Vec3, b: Vec3| RadioScene::path(sync_scene, a, b).in_band(sync_prof.freq_mhz).uwb_passes(&sync_prof);
    let budget_db = radio::uwb_tx_total_dbm(&prof) - prof.sensitivity_dbm;
    let freq = prof.freq_mhz;
    let as_modelled = &[Day::AsModelled][..];

    match w.step {
        PlanStep::Setup => {
            let (lo, hi) = planning::area(&objects, &devices);
            (w.lo, w.hi) = (lo, hi);
            let size = hi - lo;
            let boxes = planning::object_boxes(&objects);
            // The floor, as an ordinary plan has it, but each point weighs what its traffic does.
            let ec = (size.x * size.y / 500.0).sqrt().max(0.75);
            let mut floor: Vec<Point> = Vec::new();
            let zones_of = |x: f32, z: f32| -> Vec<u16> { w.input.zones.iter().enumerate().filter(|(_, (a, b))| x >= a.x && x <= b.x && z >= a.z && z <= b.z).map(|(i, _)| i as u16).collect() };
            const ZONE_WEIGHT: f32 = 2.0;
            let mut z = lo.y + ec * 0.5;
            while z < hi.y {
                let mut x = lo.x + ec * 0.5;
                while x < hi.x {
                    if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                        let zones = zones_of(x, z);
                        let weight = flow::demand(w.input.flow.at(Vec2::new(x, z))) * if zones.is_empty() { 1.0 } else { ZONE_WEIGHT };
                        floor.push(Point { pos: Vec3::new(x, w.input.tag_height, z), weight, floor: true, zones });
                    }
                    x += ec;
                }
                z += ec;
            }
            let zc = (ec * 0.5).max(0.5);
            for (a, b) in &w.input.zones {
                let mut z = a.z + zc * 0.5;
                while z < b.z {
                    let mut x = a.x + zc * 0.5;
                    while x < b.x {
                        if !blocked(&boxes, Vec2::new(x, z), 0.1) {
                            floor.push(Point { pos: Vec3::new(x, w.input.tag_height, z), weight: ZONE_WEIGHT, floor: false, zones: zones_of(x, z) });
                        }
                        x += zc;
                    }
                    z += zc;
                }
            }
            // Tags that stay put are a test of their own: heavy enough that losing one of them
            // shows in the headline figures, as it does in the simulated test.
            if !w.input.tag_spots.is_empty() {
                let all: f32 = floor.iter().map(|p| p.weight).sum();
                let each = 0.5 * all / w.input.tag_spots.len() as f32;
                floor.extend(w.input.tag_spots.iter().map(|p| Point { pos: *p, weight: each, floor: true, zones: Vec::new() }));
            }
            w.per_day = floor.len();
            w.pts = w.input.days.iter().flat_map(|_| floor.iter().cloned()).collect();
            let per_day = w.per_day;
            w.condition_of = w.input.days.iter().enumerate().flat_map(|(i, _)| std::iter::repeat_n(i as u8, per_day)).collect();
            // The mounting points, and what plain Auto deploy chose, to judge it on the same terms.
            let mut cands = planning::candidates_from(&w.input.spots, Some(&w.input.mounts), &[], &[], w.input.head_end);
            let c = (lo + hi) * 0.5;
            let ceiling = RadioScene::surface(&occ, Vec3::new(c.x, 0.5, c.y), Vec3::Y, 30.0).map(|t| t + 0.5);
            if cands.len() * 3 <= 900 {
                cands = placement::expand_heights(&cands, ceiling);
            }
            cands.retain(|c| occ.holds(c.pos));
            let from = cands.len();
            cands.extend(w.input.standard.iter().map(|(p, f)| Candidate { pos: *p, facing: *f, mount: if f.y < -0.9 { Mount::Ceiling } else { Mount::Wall }, cost: placement::install_cost(Mount::Wall, *p, w.input.head_end) }));
            w.standard_at = (from, cands.len());
            w.cols = vec![Vec::new(); cands.len()];
            w.cands = cands;
            // Your own anchors count towards what a fix needs, and stand on the floor every day.
            let mut existing: Vec<_> = devices.iter().filter(|(_, d, _)| d.enabled && d.kind == DeviceKind::UwbAnchor).collect();
            existing.sort_by(|a, b| a.1.id.cmp(&b.1.id));
            let mine = existing.len();
            w.fixed = existing.iter().map(|(_, _, t)| (t.translation, Column::new())).collect();
            w.tried = sizes(need.saturating_sub(mine).max(1), w.input.budget);
            if w.tried.is_empty() {
                w.out.notes.push(format!("A position takes {need} anchors, and the limit leaves room for {}.", w.input.budget));
            }
            w.step = PlanStep::Day(0);
            w.progress = format!("Working out what {} mounting points can do", w.cands.len());
        }
        PlanStep::Day(i) => {
            let Some(day) = w.input.days.get(i).copied() else {
                w.step = PlanStep::Done;
                return work;
            };
            let per_day = w.per_day;
            let start = i * per_day;
            {
                let _in_day = day.enter();
                let (pts, cands) = (&w.pts[start..start + per_day], &w.cands);
                for (c, col) in cands.iter().zip(w.cols.iter_mut()) {
                    col.extend(column_on(&occ, pts, start, method, &prof, &tag, c.pos, c.facing, &gain, &shape));
                }
                let mut existing: Vec<_> = devices.iter().filter(|(_, d, _)| d.enabled && d.kind == DeviceKind::UwbAnchor).collect();
                existing.sort_by(|a, b| a.1.id.cmp(&b.1.id));
                for ((_, d, t), slot) in existing.iter().zip(w.fixed.iter_mut()) {
                    let (pos, fwd, pr, an) = (t.translation, *t.forward(), profiles.for_device(d), d.radio.antenna.clone());
                    let g = |p: &Profile, f: Vec3, dir: Vec3| ants.gain(an.as_ref(), Tech::Uwb, true, p, f, dir) + tag_gain;
                    let s = |f: Vec3, dir: Vec3| ants.bias_m(an.as_ref(), Tech::Uwb, true, f, dir);
                    slot.1.extend(column_on(&occ, pts, start, Method::of(Tech::Uwb, &pr, &tag), &pr, &tag, pos, fwd, &g, &s));
                }
            }
            if i + 1 < w.input.days.len() {
                w.step = PlanStep::Day(i + 1);
                w.progress = format!("Working out the mounting points on: {}", w.input.days[i + 1].name());
            } else {
                // The plain answer's search only looks at the first day.
                w.lean_cands = w.cands.clone();
                w.lean_cols = w.cols.iter().map(|c| c.iter().filter(|e| (e.pt as usize) < per_day).copied().collect()).collect();
                w.at = 0;
                w.past = None;
                w.step = if w.tried.is_empty() { PlanStep::Done } else { PlanStep::Lean };
                w.progress = "Searching as modelled".into();
            }
        }
        PlanStep::Lean => {
            let per_day = w.per_day;
            let n = w.tried[w.at];
            let pts = &w.pts[..per_day];
            let fixed: Vec<(Vec3, Column)> = w.fixed.iter().map(|(p, c)| (*p, c.iter().filter(|e| (e.pt as usize) < per_day).copied().collect())).collect();
            let mover = DayMover { scene: &occ, pts, days: as_modelled, per_day, method, prof: &prof, tag: &tag, gain: &gain, shape: &shape, lo: w.lo, hi: w.hi };
            let mut p = Planner::new(cfg.clone(), pts, std::mem::take(&mut w.lean_cands), std::mem::take(&mut w.lean_cols), fixed, Some(&sync));
            p.without_targets();
            let step = search_size(&mut p, w.lean_steps.last(), n, &mover);
            (w.lean_cands, w.lean_cols) = p.into_parts();
            let done = note_step(&mut w.lean_steps, step, n, &mut w.at, &mut w.past, w.tried.len(), target, 2);
            if done {
                w.step = PlanStep::DescribeLean;
            }
            w.progress = format!("Searching as modelled: {n} anchors");
        }
        PlanStep::DescribeLean => {
            let per_day = w.per_day;
            let pts = &w.pts[..per_day];
            let fixed: Vec<(Vec3, Column)> = w.fixed.iter().map(|(p, c)| (*p, c.iter().filter(|e| (e.pt as usize) < per_day).copied().collect())).collect();
            let mut p = Planner::new(cfg.clone(), pts, std::mem::take(&mut w.lean_cands), std::mem::take(&mut w.lean_cols), fixed, Some(&sync));
            p.without_targets();
            w.out.first_ok[0] = w.lean_steps.iter().position(|s| plan_enough(&s.metrics, target));
            w.out.lean = w.lean_steps.iter().map(|s| describe(&mut p, &s.chosen, as_modelled, per_day, pts, &w.input.landmarks, freq, budget_db, tag_gain, target)).collect();
            w.at = 0;
            w.past = None;
            w.step = PlanStep::Robust;
            w.progress = "Searching across every day".into();
        }
        PlanStep::Robust => {
            let n = w.tried[w.at];
            let mover = DayMover { scene: &occ, pts: &w.pts, days: &w.input.days, per_day: w.per_day, method, prof: &prof, tag: &tag, gain: &gain, shape: &shape, lo: w.lo, hi: w.hi };
            let mut p = Planner::new(Config { condition_of: w.condition_of.clone(), ..cfg.clone() }, &w.pts, std::mem::take(&mut w.cands), std::mem::take(&mut w.cols), w.fixed.clone(), Some(&sync));
            p.without_targets();
            let step = search_size(&mut p, w.robust_steps.last(), n, &mover);
            (w.cands, w.cols) = p.into_parts();
            // Some way past the first set that looks enough: a prediction can be wrong, and a
            // margin has a price worth seeing. A set that can lose an anchor comes from further
            // along this same curve, as the simulator finds it.
            let done = note_step(&mut w.robust_steps, step, n, &mut w.at, &mut w.past, w.tried.len(), target, 4);
            if done {
                w.step = PlanStep::DescribeRobust;
            }
            w.progress = format!("Searching across every day: {n} anchors");
        }
        PlanStep::DescribeRobust => {
            let mut p = Planner::new(Config { condition_of: w.condition_of.clone(), ..cfg.clone() }, &w.pts, std::mem::take(&mut w.cands), std::mem::take(&mut w.cols), w.fixed.clone(), Some(&sync));
            p.without_targets();
            w.out.first_ok[1] = w.robust_steps.iter().position(|s| plan_enough(&s.metrics, target));
            w.out.robust = w.robust_steps.iter().map(|s| describe(&mut p, &s.chosen, &w.input.days, w.per_day, &w.pts, &w.input.landmarks, freq, budget_db, tag_gain, target)).collect();
            // Plain Auto deploy's anchors, by the same yardstick.
            if !w.input.standard.is_empty() {
                let chosen: Vec<usize> = (w.standard_at.0..w.standard_at.1).collect();
                w.out.standard = Some(describe(&mut p, &chosen, &w.input.days, w.per_day, &w.pts, &w.input.landmarks, freq, budget_db, tag_gain, target));
            }
            w.step = PlanStep::Done;
        }
        PlanStep::Done => {}
    }
    work
}

/// One size of a curve: searched for properly the first time, then grown from the last.
fn search_size(p: &mut Planner, last: Option<&CurveStep>, n: usize, mover: &dyn Mover) -> CurveStep {
    match last {
        Some(l) => p.grow(&l.chosen, n, Some(mover)),
        None => {
            p.run(n, Some(mover));
            CurveStep { chosen: p.chosen.clone(), metrics: p.metrics() }
        }
    }
}

/// Which set of a curve the simulator is looking at, and how far it has gone.
#[derive(Clone, Debug)]
struct Walk {
    kind: OptionKind,
    sets: Vec<SetPlan>,
    at: usize,
    /// Fewest that passed so far.
    good: Option<usize>,
    /// Asked of a set for it to pass.
    days: Vec<Day>,
    moves: usize,
    done: bool,
}

/// Where the walk goes after a test: down when a set passes, to find the fewest that do, up
/// when it fails. The prediction only picks where to start.
pub(crate) fn next_step(at: usize, len: usize, passed: bool, good: Option<usize>, moves: usize) -> (Option<usize>, Option<usize>) {
    const MOST_MOVES: usize = 4;
    let good = if passed { Some(good.map_or(at, |g: usize| g.min(at))) } else { good };
    if moves >= MOST_MOVES {
        return (good, None);
    }
    if passed {
        // One below is known to fail, or there's nothing below.
        if at == 0 {
            return (good, None);
        }
        return (good, Some(at - 1));
    }
    if good.is_some() {
        return (good, None);
    }
    (good, (at + 1 < len).then_some(at + 1))
}

/// What the state machine is doing, one bite each frame so the window keeps drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Read,
    Standard,
    Plan,
    Test,
    Finish,
}

/// A test between bites.
struct Partial {
    key: String,
    day: Day,
    reported: bool,
    run: u64,
    merged: Option<DayResult>,
    spots: Vec<acceptance::PointResult>,
}

pub struct Job {
    req: SmartRequest,
    stage: Stage,
    started: std::time::Instant,
    reading: Option<Reading>,
    standard: Vec<(Vec3, Vec3)>,
    standard_set: Option<SetPlan>,
    /// The search, between bites.
    work: Option<Box<PlanWork>>,
    walks: Vec<Walk>,
    days: Vec<Day>,
    /// Results by set and day: the runs that decide where a walk goes...
    tests: HashMap<(String, Day), DayResult>,
    /// ...and the fresh ones that get reported for the sets it settles on.
    confirmed: HashMap<(String, Day), DayResult>,
    placed: Option<String>,
    start: Option<sim::RunStart>,
    /// Where your own devices stood before any of this ran.
    own_start: Option<sim::RunStart>,
    /// What the site was when it was read.
    signature: Option<u64>,
    floor_spots: Vec<TestPoint>,
    /// A test part way through, between bites.
    partial: Option<Partial>,
    notes: Vec<String>,
    timings: Vec<(String, f32)>,
    longest_bite_s: f32,
    pub progress: String,
}

/// The state behind the Deploy card's plans.
#[derive(Resource, Default)]
pub struct SmartPlan {
    pub job: Option<Job>,
    pub result: Option<SmartResult>,
    pub costing: Costing,
    /// Asks for an option to be put on the floor.
    pub apply: Option<usize>,
    reading: Option<Reading>,
    /// Tag height the last run tested with.
    tag_height: f32,
}

impl SmartPlan {
    pub fn start(&mut self, req: SmartRequest) {
        self.job = Some(Job {
            req,
            stage: Stage::Read,
            started: std::time::Instant::now(),
            reading: None,
            standard: Vec::new(),
            standard_set: None,
            work: None,
            walks: Vec::new(),
            days: Vec::new(),
            tests: HashMap::new(),
            confirmed: HashMap::new(),
            placed: None,
            start: None,
            own_start: None,
            signature: None,
            floor_spots: Vec::new(),
            partial: None,
            notes: Vec::new(),
            timings: Vec::new(),
            longest_bite_s: 0.0,
            progress: "Reading the site".into(),
        });
    }

    pub fn busy(&self) -> Option<&str> {
        self.job.as_ref().map(|j| j.progress.as_str())
    }

    /// Something else put devices on the floor in place of the plan's.
    pub fn floor_replaced(&mut self) {
        if let Some(r) = self.result.as_mut() {
            r.applied = None;
        }
    }
}

/// Puts a set and the moving tags on the floor. Your own devices stay.
fn place_set(world: &mut World, set: &SetPlan, reading: &Reading, height: f32) -> (Vec<String>, sim::RunStart) {
    clear_auto(world);
    let (anchor, tag) = {
        let profiles = world.resource::<Profiles>();
        (profiles.default_for(DeviceKind::UwbAnchor).cloned(), profiles.get("uwb-tag").cloned().or_else(|| profiles.default_for(DeviceKind::UwbTag).cloned()))
    };
    let mut ids = Vec::new();
    if let Some(anchor) = &anchor {
        for a in &set.anchors {
            ids.push(deploy::spawn(world, anchor, deploy::facing(Vec3::from(a.pos), Vec3::from(a.facing)), Motion::Static, None));
        }
    }
    if let Some(t) = &tag {
        deploy::tag_vehicles(world, &reading.objs, t);
        let walkers = reading.tours.len().max(1) as f32;
        for (w, tour) in reading.tours.iter().enumerate() {
            let tour = start_partway(tour, w as f32 / walkers);
            let points: Vec<[f32; 3]> = tour.iter().map(|p| [p.x, height, p.y]).collect();
            let at = Transform::from_translation(Vec3::from(points[0]));
            deploy::spawn(world, t, at, Motion::Path { points, speed: 1.3 + 0.2 * w as f32, ping_pong: false }, None);
        }
    }
    (ids, sim::RunStart::take(world))
}

/// Runs of the simulator behind every reported result, each on its own noise.
pub const RUNS_PER_TEST: u64 = 3;
/// And behind each step of finding the set to report, which a failed run ends early.
const RUNS_PER_STEP: u64 = 2;
/// Floor spots tried in one go, so the window keeps drawing.
const SPOTS_PER_BITE: usize = 6;

/// Two runs of one test as one result: the worse figure of each kind. No figure at all (too many
/// updates without a position) is worse than any figure.
fn worse_of(a: DayResult, b: DayResult) -> DayResult {
    let worse = |x: Option<f32>, y: Option<f32>| match (x, y) {
        (Some(x), Some(y)) => Some(x.max(y)),
        _ => None,
    };
    let rank = |t: &DayResult| t.p95_m.unwrap_or(f32::INFINITY);
    let keep_b = rank(&b) > rank(&a);
    let availability = match (a.availability, b.availability) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    };
    let (weakest_at, weakest_near) = if keep_b && b.weakest_at.is_some() { (b.weakest_at, b.weakest_near) } else { (a.weakest_at, a.weakest_near) };
    DayResult { day: a.day, p95_m: worse(a.p95_m, b.p95_m), passed: a.passed && b.passed, moving_p95_m: worse(a.moving_p95_m, b.moving_p95_m), parked_p95_m: worse(a.parked_p95_m, b.parked_p95_m), availability, spots: None, weakest_at, weakest_near }
}

/// One run of one day's test of what's on the floor.
#[allow(clippy::too_many_arguments)]
fn run_once(world: &mut World, day: Day, down: Option<&str>, start: &sim::RunStart, test_s: f64, target_m: f32, landmarks: &[(String, Vec2)], seed_offset: u64) -> DayResult {
    let muted = world.resource::<Hub>().muted;
    world.resource_mut::<Hub>().muted = true;
    let seed = world.resource::<RtlsConfig>().settings.seed.wrapping_add(seed_offset);
    let dead: Option<Entity> = down.and_then(|id| world.query::<(Entity, &Device)>().iter(world).find(|(_, d)| d.id == id).map(|x| x.0));
    if let Some(e) = dead
        && let Some(mut d) = world.get_mut::<Device>(e)
    {
        d.enabled = false;
    }
    let result = {
        let _in_day = day.enter();
        start.restore(world);
        world.resource_mut::<RtlsState>().restart(seed);
        sim::run_for(world, test_s);
        let st = world.resource::<RtlsState>();
        let a = TechAccuracy::measure(st, "uwb", target_m);
        let spots: Vec<(Vec3, f32)> = st.error_spots.iter().filter(|s| s.2 == "uwb").map(|s| (s.0, s.1)).collect();
        let weakest = deploy::bad_areas(&spots, target_m, 2.0).first().map(|(a, b)| (*a + *b).xz() * 0.5);
        let availability = [a.moving.availability(), a.parked.availability()].into_iter().flatten().reduce(f32::min);
        // Nothing tried is no result, never a perfect one.
        let tried = a.moving.attempts + a.parked.attempts > 0;
        DayResult {
            day,
            p95_m: if tried { finite(a.worst_p95_m()) } else { None },
            passed: a.passed(),
            moving_p95_m: a.moving.p95_m.and_then(finite),
            parked_p95_m: a.parked.p95_m.and_then(finite),
            availability,
            spots: None,
            weakest_at: weakest.map(|w| w.to_array()),
            weakest_near: weakest.and_then(|w| nearest_landmark(landmarks, w)),
        }
    };
    if let Some(e) = dead
        && let Some(mut d) = world.get_mut::<Device>(e)
    {
        d.enabled = true;
    }
    world.resource_mut::<Hub>().muted = muted;
    result
}

/// What to test next, if anything: `(set, day, whether it's the runs that get reported)`. Moves
/// walks along as their sets get answers.
fn next_test(job: &mut Job) -> Option<(SetPlan, Day, bool)> {
    let tests = &job.tests;
    let days = job.days.clone();
    for w in job.walks.iter_mut() {
        while !w.done {
            let Some(set) = w.sets.get(w.at) else {
                w.done = true;
                break;
            };
            if let Some(day) = w.days.iter().copied().find(|d| !tests.contains_key(&(set.key(), *d))) {
                return Some((set.clone(), day, false));
            }
            let passed = w.days.iter().all(|d| tests.get(&(set.key(), *d)).is_some_and(|t| t.passed));
            let (good, next) = next_step(w.at, w.sets.len(), passed, w.good, w.moves);
            w.good = good;
            w.moves += 1;
            match next {
                // Already tested and failed, or the walk is over.
                Some(n) => {
                    let tested = w.days.iter().all(|d| tests.contains_key(&(w.sets[n].key(), *d)));
                    let failed = tested && !w.days.iter().all(|d| tests.get(&(w.sets[n].key(), *d)).is_some_and(|t| t.passed));
                    if failed && w.good.is_some() {
                        w.done = true;
                    } else {
                        w.at = n;
                    }
                }
                None => w.done = true,
            }
        }
    }
    // Every final set, on every day, so they can be compared.
    let mut finals: Vec<SetPlan> = job.walks.iter().filter_map(|w| final_set(w, tests)).collect();
    finals.extend(job.standard_set.clone());
    for set in finals {
        for day in days.iter().copied().chain([Day::AnchorDown, Day::AnywhereOnFloor]) {
            if !job.confirmed.contains_key(&(set.key(), day)) {
                return Some((set, day, true));
            }
        }
    }
    None
}

/// A result for a test that couldn't run. Never a pass: nothing tested holds nothing.
fn not_tested(day: Day) -> DayResult {
    DayResult { day, p95_m: None, passed: false, moving_p95_m: None, parked_p95_m: None, availability: None, spots: None, weakest_at: None, weakest_near: None }
}

/// The anchor on the floor that a dead-anchor test takes out, found by where it stands: another
/// search can reach the same set with its anchors in another order.
fn anchor_to_kill(world: &mut World, set: &SetPlan) -> Option<String> {
    let at = Vec3::from(set.anchors.get(set.critical?)?.pos);
    let mut q = world.query::<(&Device, &Transform)>();
    let found = q.iter(world).filter(|(d, _)| d.auto && d.kind == DeviceKind::UwbAnchor).min_by(|a, b| a.1.translation.distance_squared(at).total_cmp(&b.1.translation.distance_squared(at)));
    found.filter(|(_, t)| t.translation.distance(at) < 0.01).map(|(d, _)| d.id.clone())
}

/// A tag stood at the spots from `done` on, a few at a time, as the handover test would have it.
/// Hands back what they gave, or `Err` when the test can't run.
fn floor_spots_bite(world: &mut World, points: &[TestPoint], done: usize, target_m: f32) -> Result<Vec<acceptance::PointResult>, String> {
    let seed = world.resource::<RtlsConfig>().settings.seed;
    let some = points[done.min(points.len())..].iter().take(SPOTS_PER_BITE).cloned().collect();
    let plan = AcceptancePlan { scene: String::new(), tech: "uwb".into(), target_m, pass_share: 0.95, confidence: 0.95, points: some, seed };
    acceptance::preview(world, &plan, FLOOR_SPOT_SETTLE_S)
}

/// What all the spots came to.
fn floor_spots_result(points: &[TestPoint], results: &[acceptance::PointResult], target_m: f32) -> DayResult {
    let day = Day::AnywhereOnFloor;
    if points.is_empty() {
        return not_tested(day);
    }
    let plan = AcceptancePlan { scene: String::new(), tech: "uwb".into(), target_m, pass_share: 0.95, confidence: 0.95, points: points.to_vec(), seed: 0 };
    let s = acceptance::score(&plan, results, None);
    let tried = points.len();
    DayResult { spots: Some((s.passed, tried)), p95_m: s.p95_m.map(|v| v as f32), passed: s.passed as f32 >= FLOOR_SPOTS_SHARE * tried as f32, ..not_tested(day) }
}

/// A closed walk starting a share of the way round: test tags spread over a big floor from the
/// first second, instead of all setting out from the same door.
pub fn start_partway(tour: &[Vec2], share: f32) -> Vec<Vec2> {
    if tour.len() < 3 {
        return tour.to_vec();
    }
    let body = &tour[..tour.len() - 1];
    let from = ((share.clamp(0.0, 1.0) * body.len() as f32) as usize) % body.len();
    let mut out: Vec<Vec2> = body[from..].iter().chain(&body[..from]).copied().collect();
    out.push(out[0]);
    out
}

/// Everything about a site a plan was worked out for: the objects, and the fixed devices you
/// placed yourself. Not moving tags, which move, nor what these tests put down and take away.
fn site_signature(world: &mut World) -> u64 {
    use std::hash::{Hash, Hasher};
    let bits = |t: &Transform| -> Vec<u32> { t.translation.to_array().into_iter().chain(t.rotation.to_array()).chain(t.scale.to_array()).map(f32::to_bits).collect() };
    let mut rows: Vec<(String, Vec<u32>, bool)> = world.query::<(&PlacedObject, &Transform)>().iter(world).map(|(o, t)| (o.name.clone(), bits(t), true)).collect();
    rows.extend(world.query::<(&Device, &Transform)>().iter(world).filter(|(d, _)| !d.auto && !d.kind.is_mobile()).map(|(d, t)| (d.id.clone(), bits(t), d.enabled)));
    rows.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rows.hash(&mut h);
    h.finish()
}

/// The set a walk settled on: the fewest that passed, or the best it saw if none did.
fn final_set(w: &Walk, tests: &HashMap<(String, Day), DayResult>) -> Option<SetPlan> {
    if let Some(g) = w.good {
        return w.sets.get(g).cloned();
    }
    let worst = |s: &SetPlan| w.days.iter().filter_map(|d| tests.get(&(s.key(), *d))).map(|t| t.p95_m.unwrap_or(f32::INFINITY)).fold(0.0f32, f32::max);
    let tried: Vec<&SetPlan> = w.sets.iter().filter(|s| w.days.iter().all(|d| tests.contains_key(&(s.key(), *d)))).collect();
    tried.iter().min_by(|a, b| worst(a).total_cmp(&worst(b))).map(|s| (*s).clone()).or_else(|| w.sets.last().cloned())
}

/// Which option to recommend: the cheapest that holds on every bad day and covers the floor,
/// else the one that misses the fewest of those, and of those the one that comes closest.
pub fn recommend(options: &[PlanOption]) -> Option<usize> {
    let sound = |o: &PlanOption| o.holds_bad_days() && (o.covers_floor() || o.test(Day::AnywhereOnFloor).is_none());
    if let Some(i) = options.iter().enumerate().filter(|(_, o)| sound(o)).min_by(|a, b| a.1.cost.total_cmp(&b.1.cost)).map(|x| x.0) {
        return Some(i);
    }
    let days = |o: &PlanOption| o.tests.iter().filter(|t| t.day.on_routes()).cloned().collect::<Vec<_>>();
    let misses = |o: &PlanOption| days(o).iter().filter(|t| !t.passed).count() + o.test(Day::AnywhereOnFloor).is_some_and(|t| !t.passed) as usize;
    // A day with tags losing their position is worse than one with a figure over the target.
    let gaps = |o: &PlanOption| days(o).iter().filter(|t| t.p95_m.is_none()).count();
    // Then how often there's a position at all on its worst day, to the nearest percent.
    let there = |o: &PlanOption| days(o).iter().filter_map(|t| t.availability).fold(1.0f32, f32::min);
    let dark = |o: &PlanOption| ((1.0 - there(o)) * 100.0).round() as i32;
    // The worst figure that exists: "too many gaps" on one day says nothing about the others.
    let worst = |o: &PlanOption| days(o).iter().filter_map(|t| t.p95_m).fold(0.0f32, f32::max);
    options.iter().enumerate().min_by(|a, b| misses(a.1).cmp(&misses(b.1)).then(gaps(a.1).cmp(&gaps(b.1))).then(dark(a.1).cmp(&dark(b.1))).then(worst(a.1).total_cmp(&worst(b.1))).then(a.1.cost.total_cmp(&b.1.cost))).map(|x| x.0)
}

/// No worse on anything measured, at no more cost: `a` makes `b` pointless. Day by day, since a
/// plan that does better on some days is a real choice whatever its worst day says. Close calls
/// count as the same: a few runs can't separate them.
fn beats(a: &PlanOption, b: &PlanOption) -> bool {
    const SAME_WITHIN_M: f32 = 0.02;
    const SAME_AVAILABILITY: f32 = 0.01;
    let figure = |t: &DayResult| t.p95_m.unwrap_or(f32::INFINITY);
    // "Too many gaps" covers anything from a whisker under the line to nothing at all.
    let there = |t: &DayResult| t.availability.unwrap_or(0.0);
    a.cost <= b.cost && b.tests.iter().all(|theirs| a.test(theirs.day).is_some_and(|ours| (ours.passed || !theirs.passed) && figure(ours) <= figure(theirs) + SAME_WITHIN_M && there(ours) >= there(theirs) - SAME_AVAILABILITY))
}

/// Takes out choices another makes pointless, names the rest for what they were measured to
/// do, and puts them in order of size. What a search was aiming for doesn't matter by now.
pub fn keep_real_choices(options: &mut Vec<PlanOption>) {
    options.sort_by(|a, b| a.cost.total_cmp(&b.cost));
    let mut kept: Vec<PlanOption> = Vec::new();
    for o in options.drain(..) {
        if !kept.iter().any(|k| beats(k, &o)) {
            kept.push(o);
        }
    }
    kept.sort_by_key(|o| o.set.anchors.len());
    let mut had_lean = false;
    for o in &mut kept {
        o.kind = if o.holds_bad_days() && o.survives_failure() {
            OptionKind::Bulletproof
        } else if o.holds_bad_days() {
            OptionKind::Balanced
        } else if had_lean {
            OptionKind::Stronger
        } else {
            had_lean = true;
            OptionKind::Lean
        };
    }
    *options = kept;
}

/// How the recommended plan compares with plain Auto deploy, from the tests alone.
pub fn compare_text(best: &PlanOption, plain: &PlanOption, target_m: f32) -> String {
    let show = |p: Option<f32>| p.map_or("too many gaps".to_string(), deploy::dist);
    let (b, s) = (best.worst_day(), plain.worst_day());
    let (bn, sn) = (best.set.anchors.len(), plain.set.anchors.len());
    let (bw, sw) = (b.and_then(|x| x.1), s.and_then(|x| x.1));
    let worse_day = s.map_or("", |x| x.0.name());
    let plural = |n: usize| if n == 1 { "anchor" } else { "anchors" };
    let anchors = match bn.cmp(&sn) {
        std::cmp::Ordering::Less => format!("{} fewer {}", sn - bn, plural(sn - bn)),
        std::cmp::Ordering::Equal => "the same number of anchors".to_string(),
        std::cmp::Ordering::Greater => format!("{} more {}", bn - sn, plural(bn - sn)),
    };
    if best.kind == OptionKind::Standard {
        return format!("Plain Auto deploy's {sn} anchors are the cheapest that hold {} here: the search found nothing better on this site.", deploy::dist(target_m));
    }
    let held = |o: &PlanOption| o.holds_bad_days();
    let line = format!("Plain Auto deploy places {sn} anchors: worst day {} ({worse_day}). This plan has {anchors}: worst day {}.", show(sw), show(bw));
    match (held(best), held(plain)) {
        (true, false) => format!("{line} It holds {} where plain Auto deploy doesn't.", deploy::dist(target_m)),
        (false, true) => format!("{line} Plain Auto deploy holds {} and this plan doesn't.", deploy::dist(target_m)),
        (true, true) if bn >= sn && bw.unwrap_or(f32::MAX) >= sw.unwrap_or(f32::MAX) => format!("{line} Plain Auto deploy holds here too, so this plan bought nothing on this site."),
        (true, true) if bn > sn => format!("{line} Plain Auto deploy holds here too, with fewer anchors: this plan only buys accuracy."),
        _ => line,
    }
}

/// Plain Auto deploy goes on the list when it's as good an answer as any: it holds, covers the
/// floor, and nothing found does so for less. The point is the right plan, not this search's.
fn offer_plain_if_best(options: &mut Vec<PlanOption>, plain: &PlanOption) {
    if !plain.holds_bad_days() || !plain.covers_floor() {
        return;
    }
    let beaten = options.iter().any(|o| o.holds_bad_days() && o.covers_floor() && o.cost <= plain.cost);
    if !beaten {
        options.push(plain.clone());
        options.sort_by_key(|o| o.set.anchors.len());
    }
}

fn finish(world: &mut World, job: &mut Job) -> SmartResult {
    let reading = job.reading.take();
    let days = job.days.clone();
    let tests = std::mem::take(&mut job.tests);
    let confirmed = std::mem::take(&mut job.confirmed);
    let mut result = SmartResult { target_m: job.req.target_m, days: days.clone(), notes: std::mem::take(&mut job.notes), prices_are_placeholders: !job.req.costing.given(), signature: job.signature, ..Default::default() };
    let Some(reading) = reading else {
        result.failed = Some("Nothing was read from the site.".into());
        return result;
    };
    result.test_s = reading.test_s;
    result.head_end_at = reading.head_end;
    let costing = job.req.costing;
    // What's reported comes from fresh runs, not the ones that picked the winner: a set chosen
    // for passing has been a little lucky, and those runs can't say by how much.
    let results_of = |set: &SetPlan| -> Vec<DayResult> { days.iter().copied().chain([Day::AnchorDown, Day::AnywhereOnFloor]).filter_map(|d| confirmed.get(&(set.key(), d)).cloned()).collect() };
    let option_of = |kind: OptionKind, set: SetPlan| -> PlanOption {
        let tested = results_of(&set);
        let cost = costing.of(&set.anchors, reading.head_end);
        PlanOption { kind, set, tests: tested, cost }
    };
    let mut seen: Vec<String> = Vec::new();
    for w in &job.walks {
        let Some(set) = final_set(w, &tests) else { continue };
        // Two searches can end on the very same anchors: one choice, not two.
        let key = set.key();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        result.options.push(option_of(w.kind, set));
    }
    // The choices have to be real ones, called what they turned out to be.
    keep_real_choices(&mut result.options);
    if let Some(set) = job.standard_set.take() {
        let plain = option_of(OptionKind::Standard, set);
        offer_plain_if_best(&mut result.options, &plain);
        result.standard = Some(plain);
    }
    if let Some(robust) = job.walks.iter().find(|w| w.kind == OptionKind::Balanced) {
        result.curve = robust
            .sets
            .iter()
            .map(|s| CurveRow { anchors: s.anchors.len(), cost: costing.of(&s.anchors, reading.head_end), predicted_as_modelled_m: s.predicted_p95_m.first().copied().flatten(), predicted_worst_day_m: if s.predicted_p95_m.iter().any(Option::is_none) { None } else { s.predicted_p95_m.iter().flatten().copied().reduce(f32::max) }, set: s.anchors.clone() })
            .collect();
    }
    result.refresh_recommendation(&costing);
    if reading.site.racks + reading.site.doors + reading.site.drawn_routes == 0 {
        result.notes.push("No racks, doors or drawn routes to work traffic out from, so every walkable spot counts the same.".into());
    }
    if reading.site.head_end.is_none() {
        result.notes.push("Nothing in the scene looks like a network cabinet, so cable isn't in the cost. Name an object \"network cabinet\" and it will be.".into());
    }
    result.site = reading.site.clone();
    result.took_s = job.started.elapsed().as_secs_f32();
    result.timings = job.timings.clone();
    result.longest_bite_s = job.longest_bite_s;
    // The recommended plan on the floor, and the map to go with it.
    world.resource_mut::<planning::Coverage>().traffic = Some(traffic_map(&reading.flow));
    // Your own moving tags go back to where they were before any of this ran.
    let own_start = job.own_start.take();
    if let Some(start) = &own_start {
        start.restore(world);
    }
    if let Some(i) = result.recommended {
        put_on_floor(world, &mut result, i, &reading, job.req.tag_height);
    } else {
        clear_auto(world);
    }
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    let _ = world.run_system_cached_with(planning::compute, CoverageRequest { tech: Tech::Uwb, height: job.req.tag_height, cell: 0.5 });
    // Kept so another option can go on the floor without reading the site again.
    let mut smart = world.resource_mut::<SmartPlan>();
    smart.reading = Some(reading);
    smart.tag_height = job.req.tag_height;
    result
}

fn put_on_floor(world: &mut World, result: &mut SmartResult, index: usize, reading: &Reading, height: f32) {
    let Some(option) = result.options.get_mut(index) else { return };
    let (ids, _start) = place_set(world, &option.set, reading, height);
    for (a, id) in option.set.anchors.iter_mut().zip(ids) {
        a.id = id;
    }
    result.applied = Some(index);
}

fn traffic_map(f: &Flow) -> planning::TrafficMap {
    planning::TrafficMap { origin: f.lo, cell: f.cell, nx: f.nx, nz: f.nz, traffic: f.traffic.clone() }
}

fn advance(world: &mut World, job: &mut Job) -> Option<SmartResult> {
    // Nothing worked out for one layout says anything about another.
    if let Some(was) = job.signature
        && site_signature(world) != was
    {
        clear_auto(world);
        if let Some(own) = &job.own_start {
            own.restore(world);
        }
        return Some(SmartResult { failed: Some("The scene changed while plans were being tested, so none of them would be true of it now. Find plans again.".into()), target_m: job.req.target_m, ..Default::default() });
    }
    // A plan for half a scene is a plan for a different site.
    if job.stage == Stage::Read && still_loading(world) {
        job.progress = "Waiting for the scene to finish loading".into();
        return None;
    }
    match job.stage {
        Stage::Read => match read_site(world, &job.req) {
            Ok(r) => {
                job.days = vec![Day::AsModelled, Day::BusyShift, Day::MaterialsWorse];
                if r.has_racks {
                    job.days.push(Day::RacksHalfFull);
                }
                job.signature = Some(site_signature(world));
                job.own_start = Some(sim::RunStart::take(world));
                // Drawn evenly and the same for every plan, whatever the routes do.
                job.floor_spots = acceptance::draw(world, FLOOR_SPOTS, job.req.tag_height, 2.0, 7).unwrap_or_default();
                job.reading = Some(r);
                job.stage = if job.req.compare { Stage::Standard } else { Stage::Plan };
                job.progress = if job.req.compare { "Running plain Auto deploy to compare".into() } else { "Searching".into() };
                None
            }
            Err(e) => Some(SmartResult { failed: Some(e), target_m: job.req.target_m, ..Default::default() }),
        },
        Stage::Standard => {
            let req = DeployRequest { uwb: true, target_m: job.req.target_m, tag_height: job.req.tag_height, max_anchors: job.req.max_anchors, ..Default::default() };
            let was = world.resource::<Hub>().muted;
            world.resource_mut::<Hub>().muted = true;
            deploy::auto_deploy(world, &req);
            world.resource_mut::<Hub>().muted = was;
            let mut placed: Vec<(String, Vec3, Vec3)> = world.query::<(&Device, &Transform)>().iter(world).filter(|(d, _)| d.auto && d.kind == DeviceKind::UwbAnchor).map(|(d, t)| (d.id.clone(), t.translation, *t.forward())).collect();
            placed.sort_by(|a, b| a.0.cmp(&b.0));
            job.standard = placed.into_iter().map(|(_, p, f)| (p, f)).collect();
            clear_auto(world);
            job.stage = Stage::Plan;
            job.progress = "Searching: where tags go, on the worst of each day".into();
            None
        }
        Stage::Plan => {
            let Some(reading) = job.reading.as_ref() else { return Some(SmartResult { failed: Some("Nothing was read from the site.".into()), ..Default::default() }) };
            let mine = world.query::<&Device>().iter(world).filter(|d| d.enabled && d.kind == DeviceKind::UwbAnchor).count();
            let budget = job.req.max_anchors.map_or(40, |m| m.saturating_sub(mine));
            if budget == 0 {
                return Some(SmartResult { failed: Some(format!("The limit of {} anchors is already reached by your own.", job.req.max_anchors.unwrap_or(0))), target_m: job.req.target_m, ..Default::default() });
            }
            if let Some(work) = job.work.take() {
                let work = match world.run_system_cached_with(plan_bite, work) {
                    Ok(w) => w,
                    Err(e) => return Some(SmartResult { failed: Some(format!("The search couldn't run: {e}")), target_m: job.req.target_m, ..Default::default() }),
                };
                job.progress = work.progress.clone();
                if work.step != PlanStep::Done {
                    job.work = Some(work);
                    return None;
                }
                let out = work.out;
                job.notes.extend(out.notes);
                job.standard_set = out.standard;
                let start_at = |ok: Option<usize>, sets: &[SetPlan]| ok.unwrap_or(sets.len().saturating_sub(1));
                let every = job.days.clone();
                // Lean holds as modelled, on its own curve. The others walk the curve that planned
                // for every day, and stop at the fewest anchors that pass what's asked of them.
                let walks = [
                    (OptionKind::Lean, out.lean, vec![Day::AsModelled], out.first_ok[0]),
                    (OptionKind::Balanced, out.robust.clone(), every.clone(), out.first_ok[1]),
                    (OptionKind::Bulletproof, out.robust, every.iter().copied().chain([Day::AnchorDown]).collect(), out.first_ok[1]),
                ];
                job.walks = walks.into_iter().filter(|w| !w.1.is_empty()).map(|(kind, sets, days, ok)| Walk { kind, at: start_at(ok, &sets), sets, good: None, days, moves: 0, done: false }).collect();
                job.stage = Stage::Test;
                job.progress = "Testing in the simulator".into();
                return None;
            }
            let input = PlanInput {
                flow: reading.flow.clone(),
                spots: reading.spots.clone(),
                mounts: reading.mounts.clone(),
                zones: reading.zones.clone(),
                landmarks: reading.landmarks.clone(),
                head_end: reading.head_end,
                days: job.days.clone(),
                target_m: job.req.target_m,
                tag_height: job.req.tag_height,
                budget,
                standard: job.standard.clone(),
                tag_spots: reading.tag_spots.clone(),
            };
            job.work = Some(PlanWork::new(input));
            None
        }
        Stage::Test => {
            let Some((set, day, reported)) = next_test(job) else {
                job.stage = Stage::Finish;
                job.progress = "Putting the best plan on the floor".into();
                return None;
            };
            let Some(reading) = job.reading.as_ref() else { return Some(SmartResult { failed: Some("Nothing was read from the site.".into()), ..Default::default() }) };
            let key = set.key();
            let on_floor = world.query::<&Device>().iter(world).filter(|d| d.auto && d.kind == DeviceKind::UwbAnchor).count();
            if job.placed.as_deref() != Some(key.as_str()) || job.start.is_none() || on_floor != set.anchors.len() {
                // Your own moving tags from where they began, so no set gets a head start.
                if let Some(own) = &job.own_start {
                    own.restore(world);
                }
                let (_, start) = place_set(world, &set, reading, job.req.tag_height);
                job.start = Some(start);
                job.placed = Some(key.clone());
            }
            let Some(start) = job.start.as_ref() else { return Some(SmartResult { failed: Some("The test could not start.".into()), ..Default::default() }) };
            job.progress = format!("Testing {} anchors: {}", set.anchors.len(), day.name());
            let (test_s, target) = (reading.test_s, job.req.target_m);
            let landmarks = &reading.landmarks;
            // A test in progress carries on, a run or a few spots to each bite.
            let so_far = job.partial.take().filter(|p| p.key == key && p.day == day && p.reported == reported);
            let (run, merged, spots) = so_far.map_or((0, None, Vec::new()), |p| (p.run, p.merged, p.spots));
            let finished = match day {
                Day::AnywhereOnFloor => {
                    let mut spots = spots;
                    match floor_spots_bite(world, &job.floor_spots, spots.len(), target) {
                        Ok(more) => spots.extend(more),
                        Err(_) => {
                            job.confirmed.insert((key, day), not_tested(day));
                            return None;
                        }
                    }
                    if spots.len() < job.floor_spots.len() {
                        job.partial = Some(Partial { key, day, reported, run: 0, merged: None, spots });
                        return None;
                    }
                    floor_spots_result(&job.floor_spots, &spots, target)
                }
                _ => {
                    // No anchor of its own to take out means this was never tested.
                    let dead = if day == Day::AnchorDown { anchor_to_kill(world, &set) } else { None };
                    if day == Day::AnchorDown && dead.is_none() {
                        not_tested(day)
                    } else {
                        // One run is one draw of the noise, and a plan near the line passes on some
                        // and fails on others. It holds only if it holds on all of them, its figure
                        // is the worst it saw, and the runs reported took no part in choosing it.
                        let offset = if reported { 1000 + run } else { run };
                        let this = run_once(world, day, dead.as_deref(), start, test_s, target, landmarks, offset);
                        let all = match merged {
                            Some(m) => worse_of(m, this),
                            None => this,
                        };
                        let runs = if reported { RUNS_PER_TEST } else { RUNS_PER_STEP };
                        // Looking for a set, one failed run is answer enough.
                        if run + 1 < runs && (reported || all.passed) {
                            job.partial = Some(Partial { key, day, reported, run: run + 1, merged: Some(all), spots: Vec::new() });
                            return None;
                        }
                        all
                    }
                }
            };
            if reported {
                job.confirmed.insert((key, day), finished);
            } else {
                job.tests.insert((key, day), finished);
            }
            None
        }
        Stage::Finish => Some(finish(world, job)),
    }
}

/// One bite of the running job, and any request to change which plan is on the floor.
pub fn run_jobs(world: &mut World) {
    let apply = world.resource_mut::<SmartPlan>().apply.take();
    if let Some(i) = apply {
        apply_option(world, i);
    }
    let Some(mut job) = world.resource_mut::<SmartPlan>().job.take() else { return };
    let (stage, began) = (job.stage, std::time::Instant::now());
    let done = advance(world, &mut job);
    let spent = began.elapsed().as_secs_f32();
    job.longest_bite_s = job.longest_bite_s.max(spent);
    let name = format!("{stage:?}");
    match job.timings.iter_mut().find(|t| t.0 == name) {
        Some(t) => t.1 += spent,
        None => job.timings.push((name, spent)),
    }
    match done {
        Some(r) => world.resource_mut::<SmartPlan>().result = Some(r),
        None => world.resource_mut::<SmartPlan>().job = Some(job),
    }
}

fn apply_option(world: &mut World, index: usize) {
    let mut smart = world.resource_mut::<SmartPlan>();
    if smart.result.is_none() || smart.reading.is_none() {
        return;
    }
    let (Some(mut result), Some(reading)) = (smart.result.take(), smart.reading.take()) else { return };
    let height = smart.tag_height;
    // A plan tested on another layout isn't that plan any more, whatever its figures say.
    if result.signature != Some(site_signature(world)) {
        result.stale = true;
        let mut smart = world.resource_mut::<SmartPlan>();
        smart.reading = Some(reading);
        smart.result = Some(result);
        return;
    }
    put_on_floor(world, &mut result, index, &reading, height);
    let seed = world.resource::<RtlsConfig>().settings.seed;
    world.resource_mut::<RtlsState>().restart(seed);
    let _ = world.run_system_cached_with(planning::compute, CoverageRequest { tech: Tech::Uwb, height, cell: 0.5 });
    let mut smart = world.resource_mut::<SmartPlan>();
    smart.reading = Some(reading);
    smart.result = Some(result);
}

/// Objects are still on their way into the scene.
fn still_loading(world: &World) -> bool {
    world.resource::<crate::prototype::PrototypeCache>().loading_count() > 0 || !world.resource::<crate::scene::PendingSpawns>().0.is_empty()
}

/// The whole job at once, for tools and headless runs. Loading needs frames, which this holds
/// up, so a scene still loading gets an answer saying so, not a wait that can't end.
pub fn run_to_end(world: &mut World, req: SmartRequest) -> SmartResult {
    if still_loading(world) {
        return SmartResult { failed: Some("The scene is still loading. Try again once it has finished.".into()), target_m: req.target_m, ..Default::default() };
    }
    world.resource_mut::<SmartPlan>().start(req);
    loop {
        run_jobs(world);
        if world.resource::<SmartPlan>().job.is_none() {
            break;
        }
    }
    world.resource::<SmartPlan>().result.clone().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(day: Day, p95: Option<f32>, passed: bool) -> DayResult {
        DayResult { day, p95_m: p95, passed, moving_p95_m: p95, parked_p95_m: None, availability: Some(1.0), spots: None, weakest_at: None, weakest_near: None }
    }

    fn option(kind: OptionKind, n: usize, cost: f64, tests: Vec<DayResult>) -> PlanOption {
        let anchors = (0..n).map(|k| AnchorPlan { pos: [k as f32, 3.0, 0.0], facing: [0.0, -1.0, 0.0], mount: "ceiling".into(), why: Why::default(), id: String::new() }).collect();
        PlanOption { kind, set: SetPlan { anchors, ..Default::default() }, tests, cost }
    }

    #[test]
    fn sizes_are_every_one_then_spread_out() {
        assert_eq!(sizes(3, 8), vec![3, 4, 5, 6, 7, 8]);
        let big = sizes(3, 40);
        assert_eq!(big.first(), Some(&3));
        assert_eq!(big.last(), Some(&40));
        assert!(big.contains(&19) && big.contains(&20), "every size where it's likely to matter: {big:?}");
        assert!(big.len() < 30, "{big:?}");
        assert!(big.windows(2).all(|w| w[0] < w[1]), "{big:?}");
        assert_eq!(sizes(3, 2), Vec::<usize>::new());
        assert_eq!(sizes(3, 3), vec![3]);
    }

    #[test]
    fn the_walk_goes_down_while_sets_pass_and_up_while_they_fail() {
        // Passed: try one fewer.
        assert_eq!(next_step(3, 6, true, None, 0), (Some(3), Some(2)));
        // That failed: the last that passed stands.
        assert_eq!(next_step(2, 6, false, Some(3), 1), (Some(3), None));
        // Nothing has passed yet: one more.
        assert_eq!(next_step(3, 6, false, None, 0), (None, Some(4)));
        // Out of sets, nothing passed.
        assert_eq!(next_step(5, 6, false, None, 1), (None, None));
        // The smallest set passed: nowhere to go.
        assert_eq!(next_step(0, 6, true, None, 0), (Some(0), None));
        // Out of patience, keeping the fewest that passed.
        assert_eq!(next_step(2, 6, true, Some(3), 4), (Some(2), None));
    }

    #[test]
    fn the_cheapest_plan_that_holds_is_recommended() {
        let lean = option(OptionKind::Lean, 5, 1000.0, vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(0.5), false)]);
        let balanced = option(OptionKind::Balanced, 7, 1500.0, vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(0.28), true)]);
        let bullet = option(OptionKind::Bulletproof, 9, 2000.0, vec![day(Day::AsModelled, Some(0.15), true), day(Day::BusyShift, Some(0.2), true)]);
        assert_eq!(recommend(&[lean.clone(), balanced.clone(), bullet.clone()]), Some(1));
        assert!(!lean.holds_bad_days());
        // Nothing holds: the one closest, not the most expensive.
        let far = option(OptionKind::Lean, 5, 1000.0, vec![day(Day::BusyShift, Some(0.9), false)]);
        let near = option(OptionKind::Balanced, 7, 1500.0, vec![day(Day::BusyShift, Some(0.4), false)]);
        let lost = option(OptionKind::Bulletproof, 9, 2000.0, vec![day(Day::BusyShift, None, false)]);
        assert_eq!(recommend(&[far, near, lost.clone()]), Some(1));
        assert_eq!(recommend(&[]), None);
        // From a real run: all three lost tags on the busy shift, and the cheapest also missed
        // two other days. Only the one that missed nothing else should be picked.
        let two_more = option(OptionKind::Lean, 14, 4765.0, vec![day(Day::AsModelled, Some(0.33), false), day(Day::BusyShift, None, false)]);
        let only_busy = option(OptionKind::Balanced, 17, 5667.0, vec![day(Day::AsModelled, Some(0.22), true), day(Day::BusyShift, None, false)]);
        assert_eq!(recommend(&[two_more, only_busy]), Some(1));
    }

    #[test]
    fn a_plan_that_abandons_the_rest_of_the_floor_isnt_recommended() {
        let floor = |held: usize| DayResult { spots: Some((held, 30)), passed: held as f32 >= FLOOR_SPOTS_SHARE * 30.0, ..day(Day::AnywhereOnFloor, Some(0.2), true) };
        let routes = || vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(0.25), true)];
        let with = |n, cost, held| {
            let mut t = routes();
            t.push(floor(held));
            option(OptionKind::Balanced, n, cost, t)
        };
        // The cheap one holds its routes and only 15 of the 30 spots.
        let v = [with(5, 1000.0, 15), with(8, 1600.0, 27)];
        assert!(v[0].holds_bad_days() && !v[0].covers_floor());
        assert_eq!(recommend(&v), Some(1));
        assert!(v[0].verdict(0.3).contains("15 of 30"), "{}", v[0].verdict(0.3));
        // Nor is it a pointless choice: it's cheaper, and says what it gives up.
        let mut k = v.to_vec();
        keep_real_choices(&mut k);
        assert_eq!(k.len(), 2);
    }

    #[test]
    fn plain_auto_deploy_is_offered_when_its_the_right_answer() {
        let good = |n, cost| {
            let t = vec![day(Day::AsModelled, Some(0.2), true), DayResult { spots: Some((28, 30)), ..day(Day::AnywhereOnFloor, Some(0.2), true) }];
            option(OptionKind::Standard, n, cost, t)
        };
        // Cheaper than anything found, and it holds.
        let mut v = vec![{
            let mut o = good(8, 1600.0);
            o.kind = OptionKind::Balanced;
            o
        }];
        offer_plain_if_best(&mut v, &good(6, 1200.0));
        assert_eq!(v.len(), 2);
        assert_eq!(v[recommend(&v).unwrap()].kind, OptionKind::Standard);
        let said = compare_text(&v[recommend(&v).unwrap()], &good(6, 1200.0), 0.3);
        assert!(said.contains("found nothing better"), "{said}");
        // Something found does it for less: plain stays out of the list.
        let mut v = vec![{
            let mut o = good(5, 1000.0);
            o.kind = OptionKind::Balanced;
            o
        }];
        offer_plain_if_best(&mut v, &good(6, 1200.0));
        assert_eq!(v.len(), 1);
        // And it isn't offered at all when it doesn't hold.
        let mut v = Vec::new();
        offer_plain_if_best(&mut v, &option(OptionKind::Standard, 6, 1200.0, vec![day(Day::BusyShift, Some(0.6), false)]));
        assert!(v.is_empty());
    }

    #[test]
    fn prices_change_the_recommendation_and_what_is_said_about_it() {
        let holds = |n, kind| {
            let mut o = option(kind, n, 0.0, vec![day(Day::AsModelled, Some(0.2), true)]);
            o.set.brief = AntennaBrief { anchors: n, ..Default::default() };
            o
        };
        let mut r = SmartResult { target_m: 0.3, options: vec![holds(4, OptionKind::Balanced), holds(6, OptionKind::Bulletproof)], standard: Some(holds(9, OptionKind::Standard)), curve: vec![CurveRow { anchors: 4, cost: 0.0, predicted_as_modelled_m: None, predicted_worst_day_m: None, set: holds(4, OptionKind::Balanced).set.anchors }], ..Default::default() };
        let c = Costing { anchor: 100.0, fit: 0.0, cable_per_m: 0.0, lift_per_device: 0.0, edited: [true, false, false, false], ..Default::default() };
        r.recost(&c);
        assert_eq!(r.options[0].cost, 400.0);
        assert_eq!(r.curve[0].cost, 400.0, "the curve is priced the same");
        assert_eq!(r.recommended, Some(0));
        assert_eq!(r.brief.anchors, 4, "the brief follows the recommendation");
        assert!(r.compared.as_ref().is_some_and(|t| t.contains("5 fewer")), "{:?}", r.compared);
        assert!(r.prices_are_placeholders, "three of the four are still placeholders");
    }

    #[test]
    fn a_plan_only_holds_if_it_holds_on_every_run() {
        let good = day(Day::BusyShift, Some(0.2), true);
        let near = day(Day::BusyShift, Some(0.29), true);
        let bad = day(Day::BusyShift, Some(0.4), false);
        let lost = day(Day::BusyShift, None, false);
        let m = worse_of(good.clone(), near.clone());
        assert!(m.passed);
        assert_eq!(m.p95_m, Some(0.29));
        let m = worse_of(near.clone(), bad.clone());
        assert!(!m.passed, "one failed run is a fail");
        assert_eq!(m.p95_m, Some(0.4));
        // Order doesn't matter, and a run with gaps outranks any figure.
        assert_eq!(worse_of(lost.clone(), good.clone()).p95_m, None);
        assert_eq!(worse_of(good, lost).p95_m, None);
    }

    #[test]
    fn choices_are_real_and_called_what_they_proved_to_be() {
        let good = |cost: f64, worst: f32, spare: bool| vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(worst), true), day(Day::AnchorDown, Some(if spare { 0.25 } else { 0.9 }), spare)];
        // From a real run: the "Balanced" search's six anchors cost more than the "Bulletproof"
        // search's six, and did no better.
        let lean = option(OptionKind::Lean, 5, 1457.0, vec![day(Day::AsModelled, Some(0.21), true), day(Day::BusyShift, Some(0.38), false), day(Day::AnchorDown, None, false)]);
        let balanced = option(OptionKind::Balanced, 6, 1762.0, good(1762.0, 0.29, true));
        let bullet = option(OptionKind::Bulletproof, 6, 1740.0, good(1740.0, 0.28, true));
        let mut v = vec![lean, balanced, bullet];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 2, "{:?}", v.iter().map(|o| o.cost).collect::<Vec<_>>());
        assert_eq!((v[0].kind, v[0].cost), (OptionKind::Lean, 1457.0));
        assert_eq!((v[1].kind, v[1].cost), (OptionKind::Bulletproof, 1740.0));

        // A search aiming for spare capacity that didn't get it isn't called that.
        let mut v = vec![option(OptionKind::Bulletproof, 8, 2000.0, good(2000.0, 0.2, false))];
        keep_real_choices(&mut v);
        assert_eq!(v[0].kind, OptionKind::Balanced);

        // More that does measurably better stays a choice.
        let mut v = vec![option(OptionKind::Balanced, 6, 1000.0, good(1000.0, 0.28, false)), option(OptionKind::Bulletproof, 8, 1400.0, good(1400.0, 0.2, true))];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 2);

        // Costing more and doing no better does not.
        let mut v = vec![option(OptionKind::Balanced, 6, 1000.0, good(1000.0, 0.2, true)), option(OptionKind::Bulletproof, 8, 1400.0, good(1400.0, 0.2, true))];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cost, 1000.0);
    }

    // From the review: the two real-run plans below once fell to `beats`, which only looked at
    // the worst day, both infinite, and the better one was thrown away before it could be picked.
    #[test]
    fn a_better_plan_isnt_thrown_away_because_both_lost_tags_on_one_day() {
        let two_more = option(OptionKind::Lean, 14, 4765.0, vec![day(Day::AsModelled, Some(0.33), false), day(Day::BusyShift, None, false)]);
        let only_busy = option(OptionKind::Balanced, 17, 5667.0, vec![day(Day::AsModelled, Some(0.22), true), day(Day::BusyShift, None, false)]);
        let mut v = vec![two_more, only_busy];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 2, "better on one day and no worse on the other is a real choice");
        assert_eq!(v[recommend(&v).unwrap()].cost, 5667.0);
        // Neither is a dearer plan with the same worst day but better on another.
        let a = option(OptionKind::Lean, 6, 100.0, vec![day(Day::AsModelled, Some(0.5), false), day(Day::BusyShift, Some(0.6), false)]);
        let b = option(OptionKind::Lean, 8, 200.0, vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(0.6), false)]);
        let mut v = vec![a, b];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].kind, OptionKind::Stronger, "only one can be called Lean");
    }

    #[test]
    fn too_many_gaps_doesnt_hide_how_many() {
        let seen = |availability: f32| DayResult { availability: Some(availability), ..day(Day::AsModelled, None, false) };
        let cheap = option(OptionKind::Lean, 14, 3780.0, vec![seen(0.80)]);
        let better = option(OptionKind::Lean, 20, 5400.0, vec![seen(0.94)]);
        let mut v = vec![cheap, better];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 2, "both read too many gaps, and one has a position far more often");
        assert_eq!(v[recommend(&v).unwrap()].cost, 5400.0);
        // Within a point of each other the cheaper one is as good an answer.
        let mut v = vec![option(OptionKind::Lean, 14, 3780.0, vec![seen(0.94)]), option(OptionKind::Lean, 20, 5400.0, vec![seen(0.945)])];
        keep_real_choices(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cost, 3780.0);
    }

    #[test]
    fn a_walk_can_start_partway_and_stays_closed() {
        let tour = [Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y, Vec2::ZERO];
        assert_eq!(start_partway(&tour, 0.0), tour.to_vec());
        let half = start_partway(&tour, 0.5);
        assert_eq!(half[0], Vec2::ONE);
        assert_eq!(half.first(), half.last());
        assert_eq!(half.len(), tour.len());
        // Every leg of the original survives, in the same order.
        assert_eq!(&half[1..3], &[Vec2::Y, Vec2::ZERO]);
        assert_eq!(start_partway(&[Vec2::ZERO, Vec2::X], 0.5), vec![Vec2::ZERO, Vec2::X], "too short to turn");
        assert_eq!(start_partway(&tour, 1.0)[0], Vec2::ZERO, "all the way round is the start");
    }

    #[test]
    fn nothing_tested_holds_nothing() {
        let n = not_tested(Day::AnchorDown);
        assert!(!n.passed);
        assert_eq!(n.p95_m, None);
        let o = option(OptionKind::Balanced, 4, 1.0, vec![n]);
        assert!(!o.survives_failure());
        assert!(!o.holds_bad_days(), "with no route day tested either");
    }

    #[test]
    fn the_antennas_own_axis_decides_how_the_cone_reads() {
        let ceiling = AntennaBrief { mount: "ceiling".into(), cone_deg: 70.0, ..Default::default() };
        assert_eq!(ceiling.theta_range(false), (110.0, 180.0), "a wire points up, the cone is below it");
        assert_eq!(ceiling.theta_range(true), (0.0, 70.0), "a patch faces the cone");
        let wall = AntennaBrief { mount: "wall".into(), cone_deg: 150.0, ..Default::default() };
        assert_eq!(wall.theta_range(false), (0.0, 180.0));
        assert!(axis_is_facing("uwb_patch") && !axis_is_facing("strip_dipole"));
    }

    #[test]
    fn a_dead_anchor_is_its_own_question() {
        let o = option(OptionKind::Balanced, 7, 1.0, vec![day(Day::AsModelled, Some(0.2), true), day(Day::AnchorDown, Some(0.9), false)]);
        assert!(o.holds_bad_days(), "a dead anchor isn't a bad day's condition");
        assert!(!o.survives_failure());
        assert_eq!(o.worst_day().unwrap().0, Day::AsModelled);
        assert!(o.verdict(0.3).contains("dead anchor"), "{}", o.verdict(0.3));
    }

    #[test]
    fn the_verdict_names_what_it_misses() {
        let o = option(OptionKind::Lean, 5, 1.0, vec![day(Day::AsModelled, Some(0.2), true), day(Day::BusyShift, Some(0.5), false), day(Day::MaterialsWorse, None, false)]);
        let v = o.verdict(0.3);
        assert!(v.contains("Busy shift (50 cm)") && v.contains("too many gaps"), "{v}");
    }

    #[test]
    fn the_comparison_admits_when_it_bought_nothing() {
        let both = |n, p| option(OptionKind::Balanced, n, 1.0, vec![day(Day::AsModelled, Some(p), true)]);
        let text = compare_text(&both(8, 0.25), &both(8, 0.2), 0.3);
        assert!(text.contains("bought nothing"), "{text}");
        let plain = option(OptionKind::Standard, 8, 1.0, vec![day(Day::BusyShift, Some(0.6), false)]);
        let text = compare_text(&both(8, 0.25), &plain, 0.3);
        assert!(text.contains("doesn't") && text.contains("same number"), "{text}");
        let text = compare_text(&both(6, 0.25), &both(8, 0.2), 0.3);
        assert!(text.contains("2 fewer") && !text.contains("bought nothing"), "{text}");
        let text = compare_text(&both(9, 0.2), &option(OptionKind::Standard, 8, 1.0, vec![day(Day::BusyShift, Some(0.6), false)]), 0.3);
        assert!(text.contains("1 more anchor:"), "{text}");
    }

    #[test]
    fn why_says_what_breaks() {
        let w = Why { lost_share: 0.34, p95_without_m: None, near: Some("rack_A1".into()), day: Some(Day::AsModelled) };
        assert!(w.text(0.3).contains("34 %") && w.text(0.3).contains("rack_A1"), "{}", w.text(0.3));
        assert!(!w.text(0.3).contains("As modelled"), "an ordinary day needs no saying: {}", w.text(0.3));
        // One that only matters on a bad day says which.
        let busy = Why { lost_share: 0.12, p95_without_m: None, near: None, day: Some(Day::BusyShift) };
        assert!(busy.text(0.3).starts_with("Busy shift"), "{}", busy.text(0.3));
        let spare = Why { lost_share: 0.0, p95_without_m: Some(0.25), near: None, day: Some(Day::AsModelled) };
        assert!(spare.text(0.3).starts_with("A spare"), "{}", spare.text(0.3));
        let sharper = Why { lost_share: 0.0, p95_without_m: Some(0.5), near: None, day: Some(Day::BusyShift) };
        assert!(sharper.text(0.3).contains("50 cm") && sharper.text(0.3).contains("Busy shift"), "{}", sharper.text(0.3));
    }

    #[test]
    fn costs_count_cable_and_lifts_only_when_known() {
        let c = Costing { anchor: 100.0, fit: 50.0, cable_per_m: 2.0, lift_per_device: 30.0, lift_above_m: 4.0, edited: [true; 4] };
        assert!(c.given());
        assert!(!Costing { edited: [true, true, false, true], ..c }.given(), "one placeholder left is still a placeholder");
        let a = |y: f32| AnchorPlan { pos: [10.0, y, 0.0], facing: [0.0, -1.0, 0.0], mount: "wall".into(), why: Why::default(), id: String::new() };
        assert_eq!(c.of(&[a(3.0)], None), 150.0);
        assert_eq!(c.of(&[a(6.0)], None), 180.0, "a high mount needs a lift");
        assert_eq!(c.of(&[a(3.0)], Some(Vec3::new(0.0, 3.0, 0.0))), 170.0, "ten metres of cable");
    }

    #[test]
    fn the_gain_a_link_needs_grows_with_distance() {
        // A budget that just closes 10 m at this frequency with an isotropic antenna.
        let budget = 20.0 * 10f32.log10() + 20.0 * 6489.6f32.log10() - 27.55;
        assert!(gain_needed_dbi(10.0, 6489.6, budget, 0.0).abs() < 1e-3);
        assert!((gain_needed_dbi(20.0, 6489.6, budget, 0.0) - 6.02).abs() < 0.02, "double the distance, 6 dB");
        assert!(gain_needed_dbi(5.0, 6489.6, budget, 0.0) < 0.0, "short links leave room for a weak direction");
    }

    #[test]
    fn the_shift_budget_is_what_the_noise_leaves() {
        let target_rms = 0.3 / R95_PER_RMS;
        assert!((shift_budget(0.3, 0.0, 1.0) - target_rms).abs() < 1e-5);
        assert!(shift_budget(0.3, target_rms, 1.0) < 1e-3, "no room left");
        assert!(shift_budget(0.3, target_rms * 2.0, 1.0) < 1e-3, "and none to spare past it");
        assert!(shift_budget(0.3, 0.0, 2.0) < shift_budget(0.3, 0.0, 1.0), "poor geometry amplifies it");
    }

    // On what's given, not on the live conditions: other tests read those while this runs.
    #[test]
    fn a_day_only_ever_makes_the_site_harder() {
        let plain = SiteConditions::default();
        assert_eq!(Day::AsModelled.conditions(plain), plain);
        assert_eq!(Day::BusyShift.conditions(plain).people_per_100m2, 5.0);
        assert_eq!(Day::RacksHalfFull.conditions(plain).rack_fill, 0.5);
        // A site set up harder than the day stays as hard.
        let hard = SiteConditions { rack_fill: 0.2, people_per_100m2: 12.0 };
        assert_eq!(Day::BusyShift.conditions(hard), hard);
        assert_eq!(Day::RacksHalfFull.conditions(hard), hard);
    }
}
