//! The simulation tick: move tags, measure every radio link against the
//! scene geometry, solve positions, track zones and alarms, publish events.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bevy::math::DVec3;
use std::sync::Arc;

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::device::{Device, DeviceKind, Motion, MotionState, Profile, Profiles, UwbMode, Zone, RadioMethod};
use super::events::{RtlsEvent, Snapshot, TagRow};
use super::io::{Control, Hub};
use super::planning::Tech;
use super::radio::{self, Material, Path};
use super::solver::{self, Dim};
use super::{ActiveAlarm, ERROR_WINDOW_S, ErrorStats, Link, LinkKind, RtlsConfig, RtlsState, RunStats, SeriesPoint, Track, link_state};
use crate::bvh::Bvh;
use crate::scene::{PartBvh, PartOf, PlacedObject};
/// The scene as radio sees it: every placed object with its world bounds and
/// the triangle BVHs of its mesh parts (the same trees the scene uses for
/// picking). Bevy's `MeshRayCast` tests every triangle of a mesh whose bounds
/// a ray touches, which made planning on SimReady racks (~50k+ triangles each)
/// take minutes; this culls whole objects by bounds and walks the BVH.
pub struct Occluders {
    objs: Vec<Occ>,
}

struct Occ {
    root: Entity,
    min: Vec3,
    max: Vec3,
    /// What radio sees it as (from the asset name), and how many typical walls
    /// one crossing is worth (thick structures count more).
    material: Material,
    weight: f32,
    /// A storage rack: its bays can be empty, which radio passes through.
    rack: bool,
    name: String,
    /// BVH and world → mesh transform per part.
    parts: Vec<(Arc<Bvh>, Affine3A)>,
}

/// Mesh parts of placed objects with their BVH.
pub type OccluderParts<'w, 's> = Query<'w, 's, (&'static PartOf, &'static PartBvh, &'static GlobalTransform)>;

/// Does the segment a + t·d, t ∈ [0, 1], touch the box?
fn segment_hits_box(a: Vec3, d: Vec3, min: Vec3, max: Vec3) -> bool {
    segment_box_range(a, d, min, max).is_some()
}

/// Where the segment a + t·d, t ∈ [0, 1], is inside the box.
fn segment_box_range(a: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if a[i] < min[i] || a[i] > max[i] {
                return None;
            }
        } else {
            let inv = 1.0 / d[i];
            let (mut ta, mut tb) = ((min[i] - a[i]) * inv, (max[i] - a[i]) * inv);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((t0, t1))
}

impl Occluders {
    pub fn build<F: bevy::ecs::query::QueryFilter>(parts: &OccluderParts, objects: &Query<(Entity, &PlacedObject, &Transform), F>) -> Self {
        let mut objs: Vec<Occ> = objects
            .iter()
            .map(|(e, o, t)| {
                let (a, b) = o.aabb();
                let mut min = Vec3::splat(f32::MAX);
                let mut max = Vec3::splat(f32::MIN);
                for x in [a.x, b.x] {
                    for y in [a.y, b.y] {
                        for z in [a.z, b.z] {
                            let p = t.transform_point(Vec3::new(x, y, z));
                            min = min.min(p);
                            max = max.max(p);
                        }
                    }
                }
                let (material, weight) = radio::object_material(&o.name).unwrap_or_else(|| radio::obstruction_of_asset(&o.asset));
                let rack = material == Material::Metal && radio::is_rack(&o.asset);
                Occ { root: e, min: min - Vec3::splat(0.02), max: max + Vec3::splat(0.02), material, weight, rack, name: o.name.clone(), parts: Vec::new() }
            })
            .collect();
        let index: std::collections::HashMap<Entity, usize> = objs.iter().enumerate().map(|(i, o)| (o.root, i)).collect();
        for (of, bvh, gt) in parts {
            if let Some(&i) = index.get(&of.0) {
                objs[i].parts.push((bvh.0.clone(), gt.affine().inverse()));
            }
        }
        objs.retain(|o| !o.parts.is_empty());
        // In a fixed order: losses add up in floats, and the order objects
        // finished loading must not change a plan.
        objs.sort_by(|a, b| a.min.to_array().partial_cmp(&b.min.to_array()).unwrap_or(std::cmp::Ordering::Equal).then(a.max.to_array().partial_cmp(&b.max.to_array()).unwrap_or(std::cmp::Ordering::Equal)));
        Self { objs }
    }

    /// Line of sight between two points: which distinct objects the straight
    /// path crosses and what they are made of (ignoring `ignore`, e.g. the cart a tag rides on, and
    /// surfaces touching either end, e.g. the wall a device is mounted on).
    pub fn path(&self, a: Vec3, b: Vec3, ignore: Option<Entity>) -> Path {
        let d = b - a;
        let distance = d.length();
        if distance < 0.13 {
            return Path { distance, ..Default::default() };
        }
        let dir = d / distance;
        let start = a + dir * 0.06;
        let limit = distance - 0.12;
        let mut path = Path { distance, ..Default::default() };
        let fill = radio::SiteConditions::current().rack_fill;
        for o in &self.objs {
            if Some(o.root) == ignore || !segment_hits_box(a, d, o.min, o.max) {
                continue;
            }
            for (bvh, inv) in &o.parts {
                // Mesh space with an unnormalized direction keeps t in world units.
                if bvh.cast(inv.transform_point3(start), inv.transform_vector3(dir), limit).is_some() {
                    // An empty bay is just the frame: thin, and nothing stops the signal.
                    let rack_id = o.min.x.to_bits() as u64 ^ ((o.min.z.to_bits() as u64) << 32);
                    let (t0, t1) = segment_box_range(a, d, o.min, o.max).unwrap_or((0.0, 1.0));
                    if o.rack && !radio::crossing_meets_goods(rack_id, a + d * t0, a + d * t1, fill) {
                        path.cross_weighted(Material::Thin, Material::Thin.weight());
                    } else {
                        path.cross_weighted(o.material, o.weight);
                    }
                    break;
                }
            }
        }
        path
    }

    /// Every object between two points, nearest first, with what radio sees it as.
    pub fn trace(&self, a: Vec3, b: Vec3) -> Vec<(String, Material, f32, f32)> {
        let d = b - a;
        let distance = d.length();
        if distance < 0.13 {
            return Vec::new();
        }
        let dir = d / distance;
        let start = a + dir * 0.06;
        let limit = distance - 0.12;
        let mut out = Vec::new();
        for o in &self.objs {
            if !segment_hits_box(a, d, o.min, o.max) {
                continue;
            }
            for (bvh, inv) in &o.parts {
                if let Some((t, _)) = bvh.cast(inv.transform_point3(start), inv.transform_vector3(dir), limit) {
                    out.push((o.name.clone(), o.material, o.weight, t + 0.06));
                    break;
                }
            }
        }
        out.sort_by(|x, y| x.3.total_cmp(&y.3));
        out
    }

    /// Bounds of everything placed, or `None` in an empty scene.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.objs.iter().map(|o| (o.min, o.max)).reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    }

    /// Is this point inside an object's box (somewhere nobody can stand)?
    pub fn inside(&self, p: Vec3) -> bool {
        self.objs.iter().any(|o| p.cmpge(o.min).all() && p.cmple(o.max).all())
    }

    /// Nearest surface hit along a ray: distance and world normal.
    pub fn nearest(&self, ray: Ray3d, max_t: f32) -> Option<(f32, Vec3)> {
        let dir = *ray.direction;
        let mut best: Option<(f32, Vec3)> = None;
        for o in &self.objs {
            let limit = best.map_or(max_t, |b| b.0);
            if !segment_hits_box(ray.origin, dir * limit.min(1e4), o.min, o.max) {
                continue;
            }
            for (bvh, inv) in &o.parts {
                let limit = best.map_or(max_t, |b| b.0);
                if let Some((t, n)) = bvh.cast(inv.transform_point3(ray.origin), inv.transform_vector3(dir), limit) {
                    best = Some((t, (inv.matrix3.transpose() * n).normalize_or_zero()));
                }
            }
        }
        best
    }
}


/// Kept between ticks: the occlusion model and the RFID line-of-sight paths
/// only change when something in the scene moves (or finishes loading), so
/// they are rebuilt then, not 20 times a second.
#[derive(Default)]
pub struct PathCache {
    sig: Option<u64>,
    occ: Option<Occluders>,
    /// (tag, reader) → (both positions as bits, ignored object, path).
    paths: std::collections::HashMap<(Entity, Entity), ([u32; 6], Option<Entity>, Path)>,
    /// TDoA anchors outside the clock-sync group, for this scene and these anchor positions.
    unsynced: Option<(u64, Vec<[u32; 3]>, BTreeSet<Entity>)>,
}

/// Where every device stands, to run from the same start again and to leave a scene as it
/// was found. `outcomes::stress::Start` does the same for its own runs.
pub struct RunStart {
    devices: Vec<(Entity, Transform, MotionState)>,
}

impl RunStart {
    pub fn take(world: &mut World) -> Self {
        let mut q = world.query_filtered::<(Entity, &Transform, &MotionState), With<Device>>();
        Self { devices: q.iter(world).map(|(e, t, m)| (e, *t, m.clone())).collect() }
    }

    pub fn restore(&self, world: &mut World) {
        for (e, t, m) in &self.devices {
            if let Ok(mut entity) = world.get_entity_mut(*e) {
                entity.insert((*t, m.clone()));
            }
        }
        // A filter still tracking where a tag ended would see it jump.
        for t in world.resource_mut::<RtlsState>().tracks.values_mut() {
            t.kf.reset();
        }
    }
}

/// Changes whenever a placed object moves, appears or gets its meshes.
fn scene_signature(parts: &OccluderParts, objects: &Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.iter().count().hash(&mut h);
    radio::conditions_signature().hash(&mut h);
    for (e, _, t) in objects {
        e.hash(&mut h);
        for v in t.translation.to_array().into_iter().chain(t.rotation.to_array()).chain(t.scale.to_array()) {
            v.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

fn pos_bits(a: Vec3, b: Vec3) -> [u32; 6] {
    [a.x.to_bits(), a.y.to_bits(), a.z.to_bits(), b.x.to_bits(), b.y.to_bits(), b.z.to_bits()]
}

struct Infra {
    entity: Entity,
    id: String,
    kind: DeviceKind,
    pos: Vec3,
    forward: Vec3,
    profile: Profile,
    antenna: Option<super::antenna::AntennaChoice>,
}

/// Is a solution anywhere a device of this technology could have heard it from? The engine
/// can tell that without knowing where the tag is.
fn near_infrastructure(infra: &[Infra], tech: &str, at: Vec3) -> bool {
    infra.iter().filter(|a| a.kind.tech() == tech).any(|a| (a.pos - at).length() <= a.profile.max_range_m * 1.5)
}

/// Applies control commands, (re)starts outputs and runs as many fixed
/// ticks as real time × speed asks for.
pub fn drive(world: &mut World) {
    let controls: Vec<Control> = world.resource::<Hub>().control_rx.try_iter().collect();
    for c in controls {
        let mut cfg = world.resource_mut::<RtlsConfig>();
        match c {
            Control::Run(r) => cfg.settings.running = r,
            Control::Speed(s) => cfg.settings.speed = s.clamp(0.0, 100.0),
            Control::ResetMetrics => world.resource_mut::<RtlsState>().reset_metrics(),
            Control::Actuator(n, v) => {
                let mut st = world.resource_mut::<RtlsState>();
                if let Some(slot) = st.actuators.get_mut((n as usize).wrapping_sub(1)) {
                    let before = *slot;
                    *slot = Some(v);
                    if before != Some(v) {
                        st.log(format!("OpenEMS set actuator {n} (zone {n}) to {v:.0}%"));
                    }
                }
            }
        }
    }
    let s = world.resource::<RtlsConfig>().settings.clone();
    if !s.running {
        return;
    }
    let dt = world.resource::<Time>().delta_secs_f64().min(0.25);
    let tick = 1.0 / s.tick_hz.clamp(1.0, 200.0) as f64;
    let mut st = world.resource_mut::<RtlsState>();
    st.accum += dt * s.speed.max(0.0) as f64;
    let mut n = 0;
    let mut todo = 0;
    while st.accum >= tick && n < 400 {
        st.accum -= tick;
        n += 1;
        todo += 1;
    }
    if n == 400 {
        st.accum = 0.0;
    }
    for _ in 0..todo {
        let _ = world.run_system_cached_with(step, tick);
    }
}

/// Runs the simulation for `seconds` of simulated time right now (tools,
/// headless tests). Returns the number of ticks.
pub fn run_for(world: &mut World, seconds: f64) -> u64 {
    let s = world.resource::<RtlsConfig>().settings.clone();
    let tick = 1.0 / s.tick_hz.clamp(1.0, 200.0) as f64;
    let n = (seconds / tick).ceil().max(0.0) as u64;
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        let _ = world.run_system_cached_with(step, tick);
    }
    if n > 0 {
        world.resource_mut::<RtlsState>().tick_ms = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
    }
    n
}

fn move_toward(t: &mut Transform, target: Vec3, step: f32) -> bool {
    let d = target - t.translation;
    let l = d.length();
    if l <= step || l < 1e-4 {
        t.translation = target;
        return true;
    }
    let dir = d / l;
    t.translation += dir * step;
    let flat = Vec3::new(dir.x, 0.0, dir.z);
    if flat.length_squared() > 1e-6 {
        t.look_to(flat, Vec3::Y);
    }
    false
}

fn apply_motion(
    dev: &Device,
    t: &mut Transform,
    ms: &mut MotionState,
    dt: f32,
    rng: &mut super::rng::Rng,
    objects: &Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
) -> Option<Entity> {
    match &dev.motion {
        Motion::Static => None,
        Motion::Path { points, speed, ping_pong } => {
            if points.is_empty() {
                return None;
            }
            if points.len() == 1 {
                t.translation = Vec3::from(points[0]);
                return None;
            }
            let n = points.len();
            ms.leg = ms.leg.min(n - 1);
            let mut budget = speed.max(0.0) * dt;
            for _ in 0..8 {
                let target = Vec3::from(points[ms.leg]);
                let before = t.translation;
                if move_toward(t, target, budget) {
                    budget -= (target - before).length();
                    // Next waypoint.
                    if *ping_pong {
                        if ms.forward && ms.leg + 1 >= n {
                            ms.forward = false;
                        } else if !ms.forward && ms.leg == 0 {
                            ms.forward = true;
                        }
                        ms.leg = if ms.forward { (ms.leg + 1).min(n - 1) } else { ms.leg.saturating_sub(1) };
                    } else {
                        ms.forward = true;
                        ms.leg = (ms.leg + 1) % n;
                    }
                    if budget <= 0.0 {
                        break;
                    }
                } else {
                    break;
                }
            }
            None
        }
        Motion::Wander { center, radius, speed } => {
            let c = Vec3::from(*center);
            let target = *ms.target.get_or_insert_with(|| {
                let a = rng.range(0.0, std::f32::consts::TAU);
                let r = radius.max(0.0) * rng.uniform().sqrt();
                c + Vec3::new(a.cos() * r, 0.0, a.sin() * r)
            });
            if move_toward(t, Vec3::new(target.x, t.translation.y, target.z), speed.max(0.0) * dt) {
                ms.target = None;
            }
            None
        }
        Motion::Follow { object, offset } => {
            let (e, _, ot) = objects.iter().find(|(_, o, _)| &o.name == object)?;
            t.translation = ot.translation + ot.rotation * Vec3::from(*offset);
            t.rotation = ot.rotation;
            Some(e)
        }
    }
}

fn dv(v: Vec3) -> DVec3 {
    v.as_dvec3()
}

#[allow(clippy::too_many_arguments)]
pub fn step(
    In(dt): In<f64>,
    mut state: ResMut<RtlsState>,
    cfg: Res<RtlsConfig>,
    profiles: Res<Profiles>,
    mut hub: ResMut<Hub>,
    mut devices: Query<(Entity, &Device, &mut Transform, &mut MotionState)>,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    zones: Query<&Zone>,
    parts: OccluderParts,
    (library, applied): (Res<super::antenna::AntennaLibrary>, Res<super::planning::AppliedAntenna>),
    mut cache: Local<PathCache>,
) {
    let s = &cfg.settings;
    let st = &mut *state;
    st.time += dt;
    let now = st.time;
    let mut out: Vec<RtlsEvent> = Vec::new();

    // ---- Motion
    let mut riding: BTreeMap<Entity, Entity> = BTreeMap::new();
    for (e, dev, mut t, mut ms) in devices.iter_mut() {
        if dev.kind.is_mobile() && dev.enabled {
            let rng = &mut link_state(&mut st.links, st.seed, (e, e), (&dev.id, "motion"), (Vec3::ZERO, Vec3::ZERO), 0.0, f32::MAX).rng;
            if let Some(obj) = apply_motion(dev, &mut t, &mut ms, dt as f32, rng, &objects) {
                riding.insert(e, obj);
            }
        }
    }

    let sig = scene_signature(&parts, &objects);
    if cache.sig != Some(sig) || cache.occ.is_none() {
        cache.occ = Some(Occluders::build(&parts, &objects));
        cache.paths.clear();
        cache.sig = Some(sig);
    }
    let PathCache { occ, paths: path_cache, unsynced: unsynced_cache, .. } = &mut *cache;
    let occ: &Occluders = occ.as_ref().expect("built above");

    // ---- Collect devices
    let mut infra: Vec<Infra> = Vec::new();
    // (entity, id, kind, position, profile, address, facing, antenna)
    let mut mobile: Vec<(Entity, String, DeviceKind, Vec3, Profile, String, Vec3, Option<super::antenna::AntennaChoice>)> = Vec::new();
    for (e, dev, t, _) in devices.iter() {
        let profile = profiles.for_device(dev);
        if dev.kind.is_mobile() {
            if dev.enabled {
                mobile.push((e, dev.id.clone(), dev.kind, t.translation, profile, dev.address.clone(), *t.forward(), dev.radio.antenna.clone()));
            }
        } else {
            if dev.enabled {
                infra.push(Infra { entity: e, id: dev.id.clone(), kind: dev.kind, pos: t.translation, forward: *t.forward(), profile, antenna: dev.radio.antenna.clone() });
            }
        }
    }
    // Forget tracks of removed/disabled tags.
    let alive: BTreeSet<Entity> = mobile.iter().map(|m| m.0).collect();
    let removed: Vec<Entity> = st.tracks.keys().filter(|e| !alive.contains(e)).copied().collect();
    for e in removed {
        if let Some(t) = st.tracks.remove(&e) {
            st.alarms.remove(&format!("tag_lost:{}", t.id));
        }
    }

    let mut links_per_infra: BTreeMap<Entity, u32> = BTreeMap::new();

    // TDoA needs clock sync: an anchor that can't reach the others gives nothing.
    let unsynced: BTreeSet<Entity> = if mobile.iter().any(|m| m.2 == DeviceKind::UwbTag && m.4.mode == UwbMode::Tdoa) {
        let anchors: Vec<&Infra> = infra.iter().filter(|a| a.kind == DeviceKind::UwbAnchor).collect();
        let key: Vec<[u32; 3]> = anchors.iter().map(|a| a.pos.to_array().map(f32::to_bits)).collect();
        if unsynced_cache.as_ref().is_none_or(|(sg, k, _)| *sg != sig || *k != key) {
            let out = super::placement::outside_biggest_group(anchors.len(), |i, j| occ.path(anchors[i].pos, anchors[j].pos, None).in_band(anchors[i].profile.freq_mhz).uwb_passes(&anchors[i].profile));
            *unsynced_cache = Some((sig, key, out.into_iter().map(|i| anchors[i].entity).collect()));
        }
        unsynced_cache.as_ref().map_or_else(BTreeSet::new, |x| x.2.clone())
    } else {
        BTreeSet::new()
    };
    st.unsynced_anchors = unsynced.len();

    // ---- UWB and BLE positioning
    let ants = super::antenna::Antennas { library: &library, applied: &applied };
    let conditions = radio::SiteConditions::current();
    let worn: BTreeSet<Entity> = devices.iter().filter(|(_, d, ..)| d.radio.worn).map(|(e, ..)| e).collect();
    for (e, id, kind, truth, tag_prof, _, tag_fwd, tag_ant) in &mobile {
        let tech = kind.tech();
        let is_worn = worn.contains(e);
        let track = st.tracks.entry(*e).or_insert_with(|| Track::new(id.clone(), tech, *truth));
        track.id = id.clone();
        track.truth = *truth;
        if !matches!(kind, DeviceKind::UwbTag | DeviceKind::BleTag | DeviceKind::WifiClient) || now < track.next_update {
            continue;
        }
        // On a schedule, not from now: a tick that lands just after it would stretch every period.
        track.next_update = (track.next_update + 1.0 / tag_prof.update_hz.max(0.1) as f64).max(now);
        // A tag on a route is moving from its first update; anything else once it has moved.
        let on_route = track.attempts == 0 && devices.get(*e).is_ok_and(|(_, d, ..)| matches!(d.motion, Motion::Path { .. } | Motion::Wander { .. }));
        let moving = on_route || truth.distance(track.last_truth) > 0.005;
        track.last_truth = *truth;
        track.attempts += 1;
        if moving {
            track.moving_epochs += 1;
        } else {
            track.parked_epochs += 1;
        }
        st.run.entry((tech, moving)).or_default().attempts += 1;
        let plane_y = track.plane_y;
        let ignore = riding.get(e).copied();
        let mut links: Vec<Link> = Vec::new();
        let mut fix: Option<solver::Fix> = None;
        let mut sigma_hint = 0.1f64;

        match kind {
            DeviceKind::UwbTag => {
                let anchors: Vec<&Infra> = infra.iter().filter(|a| a.kind == DeviceKind::UwbAnchor).collect();
                // Antenna gain of both ends towards each other rides on the path.
                let mut paths: Vec<Path> = anchors
                    .iter()
                    .map(|a| {
                        let (to_tag, to_anchor) = (*truth - a.pos, a.pos - *truth);
                        let gain = ants.gain(a.antenna.as_ref(), Tech::Uwb, true, &a.profile, a.forward, to_tag)
                            + ants.gain(tag_ant.as_ref(), Tech::Uwb, false, tag_prof, *tag_fwd, to_anchor);
                        let mut bias = ants.bias_m(a.antenna.as_ref(), Tech::Uwb, true, a.forward, to_tag) + ants.bias_m(tag_ant.as_ref(), Tech::Uwb, false, *tag_fwd, to_anchor);
                        let mut loss = 0.0;
                        if is_worn {
                            let (l, extra) = radio::body_shadow(tag_fwd.angle_between(to_anchor), tag_prof.freq_mhz);
                            loss = l;
                            bias += extra;
                        }
                        Path { gain_db: gain - loss, antenna_bias_m: bias, ..occ.path(*truth, a.pos, ignore).in_band(a.profile.freq_mhz) }
                    })
                    .collect();
                for (a, p) in anchors.iter().zip(paths.iter_mut()) {
                    let l = link_state(&mut st.links, st.seed, (*e, a.entity), (id, &a.id), (*truth, a.pos), p.wall_eq, radio::LINK_HOLD_M);
                    // Someone standing in a link counts as a thick obstruction for that tick.
                    if conditions.people_per_100m2 > 0.0 && l.rng.chance(conditions.person_in_path(p.distance)) {
                        p.cross_weighted(Material::Generic, 1.5);
                    }
                }
                let dim_for = |pts: &[DVec3]| {
                    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
                    if s.solve_3d && pts.len() >= 4 && hi - lo > 1.0 { Dim::Space } else { Dim::Plane(plane_y as f64) }
                };
                match tag_prof.mode {
                    UwbMode::Twr => {
                        let mut a_pos = Vec::new();
                        let mut ranges = Vec::new();
                        let mut sig = Vec::new();
                        let mut idx = Vec::new();
                        for (a, p) in anchors.iter().zip(&paths) {
                            let l = st.links.get_mut(&(*e, a.entity)).expect("made above");
                            let m = radio::twr_measure(&a.profile, tag_prof, *p, l.held, s.robust, &mut l.rng);
                            links.push(Link { other: a.entity, kind: LinkKind::Range, walls: p.walls, blocker: p.blocker, value: m.map(|m| m.range), rssi_dbm: radio::uwb_rx_power(&a.profile, *p), wall_eq: p.wall_eq, bias_eq: p.bias_eq, true_distance: p.distance, used: false });
                            if let Some(m) = m {
                                idx.push(links.len() - 1);
                                a_pos.push(dv(a.pos));
                                ranges.push(m.range as f64);
                                sig.push(m.sigma);
                                *links_per_infra.entry(a.entity).or_default() += 1;
                                out.push(RtlsEvent::Range {
                                    ts: now,
                                    tag: id.clone(),
                                    anchor: a.id.clone(),
                                    range_m: m.range,
                                    los: p.los(),
                                    rssi_dbm: m.fp_dbm,
                                });
                            }
                        }
                        let dim = dim_for(&a_pos);
                        if a_pos.len() >= solver::min_measurements(dim).min(3) {
                            fix = solver::twr_solve(dim, &a_pos, &ranges, &sig, track.est.map(dv), s.robust);
                            if let Some(f) = &fix {
                                for &u in &f.used {
                                    links[idx[u]].used = true;
                                }
                            }
                            sigma_hint = sig.iter().sum::<f64>() / sig.len() as f64;
                        }
                    }
                    UwbMode::Tdoa => {
                        let mut arrivals: Vec<(usize, f32)> = Vec::new();
                        for (k, (a, p)) in anchors.iter().zip(&paths).enumerate() {
                            let l = st.links.get_mut(&(*e, a.entity)).expect("made above");
                            let t = if unsynced.contains(&a.entity) { None } else { radio::uwb_arrival_held(&a.profile, tag_prof, *p, 0.0, l.held, &mut l.rng) };
                            links.push(Link { other: a.entity, kind: LinkKind::Tdoa, walls: p.walls, blocker: p.blocker, value: None, rssi_dbm: radio::uwb_blink_rx_power(&a.profile, tag_prof, *p), wall_eq: p.wall_eq, bias_eq: p.bias_eq, true_distance: p.distance, used: false });
                            if let Some(t) = t {
                                arrivals.push((k, t));
                                *links_per_infra.entry(a.entity).or_default() += 1;
                            }
                        }
                        if let Some(&(rk, rt)) = arrivals.iter().min_by(|a, b| a.1.total_cmp(&b.1)) {
                            let others: Vec<(usize, f32)> = arrivals.iter().filter(|(k, _)| *k != rk).copied().collect();
                            links[rk].value = Some(0.0);
                            let mut a_pos = Vec::new();
                            let mut diffs = Vec::new();
                            for (k, t) in &others {
                                let d = t - rt;
                                links[*k].value = Some(d);
                                a_pos.push(dv(anchors[*k].pos));
                                diffs.push(d as f64);
                                out.push(RtlsEvent::Tdoa {
                                    ts: now,
                                    tag: id.clone(),
                                    anchor: anchors[*k].id.clone(),
                                    reference: anchors[rk].id.clone(),
                                    diff_m: d,
                                    los: paths[*k].los(),
                                });
                            }
                            let all: Vec<DVec3> = arrivals.iter().map(|(k, _)| dv(anchors[*k].pos)).collect();
                            let dim = dim_for(&all);
                            // A difference of two arrivals.
                            let sigma = (radio::uwb_arrival_sigma(&anchors[rk].profile).max(0.03) * std::f32::consts::SQRT_2) as f64;
                            if a_pos.len() >= 3 {
                                fix = solver::solve_tdoa(dim, dv(anchors[rk].pos), &a_pos, &diffs, sigma, track.est.map(dv));
                                if fix.is_some() {
                                    links[rk].used = true;
                                    for (k, _) in &others {
                                        links[*k].used = true;
                                    }
                                }
                            }
                            sigma_hint = sigma;
                        }
                    }
                }
            }
            DeviceKind::BleTag | DeviceKind::WifiClient => {
                let wifi = *kind == DeviceKind::WifiClient;
                let (node_kind, node_tech) = if wifi { (DeviceKind::WifiAp, Tech::Wifi) } else { (DeviceKind::BleNode, Tech::Ble) };
                let nodes: Vec<&Infra> = infra.iter().filter(|a| a.kind == node_kind).collect();
                let plane = Dim::Plane(plane_y as f64);
                let mut a_pos = Vec::new();
                let mut d_est = Vec::new();
                let mut sig = Vec::new();
                let (mut rtt_pos, mut rtt_r, mut rtt_sig) = (Vec::new(), Vec::new(), Vec::new());
                let (mut aoa_pos, mut aoa_dir, mut aoa_w) = (Vec::new(), Vec::new(), Vec::new());
                let mut aoa_sigma = 0.0f64;
                let mut best: Option<(f32, Vec3)> = None;
                for n in &nodes {
                    let mut gain = ants.gain(n.antenna.as_ref(), node_tech, true, &n.profile, n.forward, *truth - n.pos)
                        + ants.gain(tag_ant.as_ref(), node_tech, false, tag_prof, *tag_fwd, n.pos - *truth);
                    if is_worn {
                        gain -= radio::body_shadow(tag_fwd.angle_between(n.pos - *truth), n.profile.freq_mhz).0;
                    }
                    let mut p = Path { gain_db: gain, ..occ.path(*truth, n.pos, ignore).in_band(n.profile.freq_mhz) };
                    let l = link_state(&mut st.links, st.seed, (*e, n.entity), (id, &n.id), (*truth, n.pos), p.wall_eq, radio::LINK_HOLD_M);
                    if conditions.people_per_100m2 > 0.0 && l.rng.chance(conditions.person_in_path(p.distance)) {
                        p.cross_weighted(Material::Generic, 1.5);
                    }
                    let mean = radio::ble_mean_rssi(&n.profile, tag_prof, p);
                    let mut link = Link { other: n.entity, kind: LinkKind::Rssi, walls: p.walls, blocker: p.blocker, value: None, rssi_dbm: mean, wall_eq: p.wall_eq, bias_eq: p.bias_eq, true_distance: p.distance, used: false };
                    match n.profile.method {
                        RadioMethod::Aoa => {
                            let dir = (*truth - n.pos).normalize_or_zero();
                            let angle = n.forward.angle_between(dir);
                            link.kind = LinkKind::Aoa;
                            if let Some(b) = radio::ble_aoa_bearing(&n.profile, tag_prof, p, dir, angle, l.held, &mut l.rng) {
                                link.value = Some(angle.to_degrees());
                                link.used = true;
                                *links_per_infra.entry(n.entity).or_default() += 1;
                                out.push(RtlsEvent::Rssi { ts: now, tag: id.clone(), node: n.id.clone(), rssi_dbm: mean, los: p.los() });
                                // Far bearings miss by more: weigh by the distance the signal suggests.
                                // All the engine has is the bearing it measured and a noisy signal
                                // strength, not the true range or what's in the way.
                                let heard = mean + radio::held_noise(n.profile.shadow_sigma_db, l.held.n3, &mut l.rng);
                                let d = solver::rssi_to_distance(heard as f64, radio::rssi_1m(tag_prof) as f64, n.profile.path_loss_exp as f64).max(0.5);
                                let sd = radio::aoa_sigma_rad(&n.profile, &Path::default(), n.forward.angle_between(b)) as f64 * d;
                                aoa_pos.push(dv(n.pos));
                                aoa_dir.push(dv(b));
                                aoa_w.push(1.0 / (sd * sd).max(1e-6));
                                aoa_sigma += sd;
                            }
                        }
                        RadioMethod::Rtt => {
                            link.kind = LinkKind::WifiRtt;
                            let r = radio::wifi_rtt_range(&n.profile, tag_prof, p, l.held, &mut l.rng);
                            link.value = r;
                            if let Some(r) = r {
                                link.used = true;
                                *links_per_infra.entry(n.entity).or_default() += 1;
                                out.push(RtlsEvent::Range { ts: now, tag: id.clone(), anchor: n.id.clone(), range_m: r, los: p.los(), rssi_dbm: mean });
                                rtt_pos.push(dv(n.pos));
                                rtt_r.push(r as f64);
                                rtt_sig.push(n.profile.range_sigma_m.max(0.1) as f64);
                                if best.is_none_or(|(b, _)| mean > b) {
                                    best = Some((mean, n.pos));
                                }
                            }
                        }
                        RadioMethod::Rssi => {
                            if wifi {
                                link.kind = LinkKind::WifiRssi;
                            }
                            let r = radio::ble_rssi(&n.profile, tag_prof, p, l.held, &mut l.rng);
                            link.value = r;
                            link.rssi_dbm = r.unwrap_or(mean);
                            if let Some(r) = r {
                                link.used = true;
                                *links_per_infra.entry(n.entity).or_default() += 1;
                                out.push(RtlsEvent::Rssi { ts: now, tag: id.clone(), node: n.id.clone(), rssi_dbm: r, los: p.los() });
                                let d = solver::rssi_to_distance(r as f64, radio::rssi_1m(tag_prof) as f64, n.profile.path_loss_exp as f64);
                                a_pos.push(dv(n.pos));
                                d_est.push(d);
                                // Multiplicative error of log-distance ranging.
                                sig.push(radio::rssi_distance_sigma(&n.profile, d as f32) as f64);
                                if best.is_none_or(|(b, _)| r > b) {
                                    best = Some((r, n.pos));
                                }
                            }
                        }
                    }
                    links.push(link);
                }
                // The most precise measurement available decides.
                if !aoa_pos.is_empty() {
                    fix = solver::solve_aoa(plane, &aoa_pos, &aoa_dir, &aoa_w);
                    sigma_hint = aoa_sigma / aoa_pos.len() as f64;
                }
                if fix.is_none() && rtt_pos.len() >= 3 {
                    let gate = 3.0 * rtt_sig.iter().copied().fold(f64::MAX, f64::min);
                    fix = solver::solve_twr_robust(plane, &rtt_pos, &rtt_r, &rtt_sig, track.est.map(dv), gate);
                    sigma_hint = rtt_sig.iter().sum::<f64>() / rtt_sig.len() as f64;
                }
                if fix.is_none() && a_pos.len() >= 3 {
                    fix = solver::solve_twr(plane, &a_pos, &d_est, &sig, track.est.map(dv));
                    sigma_hint = sig.iter().sum::<f64>() / sig.len() as f64;
                }
                if fix.is_none()
                    && let Some((_, p)) = best
                {
                    // Proximity: "near the strongest node".
                    fix = Some(solver::Fix { pos: DVec3::new(p.x as f64, plane_y as f64, p.z as f64), residual: 3.0, used: vec![], dop: 9.9 });
                    sigma_hint = 3.0;
                }
            }
            _ => {}
        }

        track.links = links;
        if let Some(f) = fix.filter(|f| f.pos.is_finite()) {
            let raw = f.pos.as_vec3();
            // Reject wild solutions, and count them.
            if !near_infrastructure(&infra, tech, raw) {
                st.run.entry((tech, moving)).or_default().rejected += 1;
                continue;
            }
            track.raw = Some(raw);
            let dt_fix = (now - track.last_fix).clamp(0.0, 5.0);
            let est = if s.kalman {
                if dt_fix >= 5.0 {
                    track.kf.reset();
                }
                track.kf.predict(dt_fix);
                // Residual × DOP is a 2D error: the filter wants it per axis.
                track.kf.update(f.pos, (f.residual.max(sigma_hint) * f.dop.clamp(1.0, 10.0) * std::f64::consts::FRAC_1_SQRT_2).max(0.03));
                track.kf.position().as_vec3()
            } else {
                raw
            };
            track.est = Some(est);
            track.last_fix = now;
            track.fixes += 1;
            track.dop = f.dop as f32;
            track.residual = f.residual as f32;
            track.used = f.used.len() as u32;
            track.quality = solver::fix_quality(&f);
            let err = (est - *truth).length();
            track.err = Some(err);
            st.errors.push_back((now, err, tech));
            st.run.entry((tech, moving)).or_default().add(err);
            st.error_spots.push_back((*truth, err, tech));
            if st.error_spots.len() > 20_000 {
                st.error_spots.pop_front();
            }
            track.trail.push_back(est);
            while track.trail.len() > 200 {
                track.trail.pop_front();
            }
            out.push(RtlsEvent::Position {
                ts: now,
                tag: id.clone(),
                tech: tech.into(),
                x: est.x,
                y: est.y,
                z: est.z,
                quality: track.quality,
                anchors: track.used,
                dop: track.dop,
                error_m: Some(err),
                truth: Some(truth.to_array()),
            });
        }
    }

    // ---- RFID inventory
    let readers: Vec<&Infra> = infra.iter().filter(|a| a.kind == DeviceKind::RfidReader).collect();
    let rfid_tags: Vec<&_> = mobile.iter().filter(|m| m.2 == DeviceKind::RfidTag).collect();
    if !readers.is_empty() && !rfid_tags.is_empty() {
        let mut readable: Vec<(usize, usize, radio::RfidLink, u32, f32)> = Vec::new();
        let mut in_field = vec![0usize; readers.len()];
        let mut tag_links: Vec<Vec<Link>> = vec![Vec::new(); rfid_tags.len()];
        let mut miss = vec![1.0f32; rfid_tags.len()];
        // Paths in range: from the cache when neither end moved, else ray cast
        // (spread over the CPU cores; a stocked warehouse has thousands).
        let mut pairs: Vec<(usize, usize, Option<Path>)> = Vec::new();
        let mut misses: Vec<(Vec3, Vec3, Option<Entity>)> = Vec::new();
        for (ti, (e, _, _, truth, ..)) in rfid_tags.iter().enumerate() {
            let ignore = riding.get(e).copied();
            for (ri, r) in readers.iter().enumerate() {
                if (r.pos - *truth).length() > r.profile.max_range_m * 1.2 {
                    continue;
                }
                match path_cache.get(&(*e, r.entity)) {
                    Some((bits, ign, p)) if *bits == pos_bits(*truth, r.pos) && *ign == ignore => pairs.push((ti, ri, Some(*p))),
                    _ => {
                        pairs.push((ti, ri, None));
                        misses.push((*truth, r.pos, ignore));
                    }
                }
            }
        }
        let mut cast: std::vec::IntoIter<Path> = if misses.len() > 64 {
            let pool = bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::default);
            let chunk = misses.len().div_ceil(pool.thread_num().max(1) * 4).max(16);
            pool.scope(|s| {
                for c in misses.chunks(chunk) {
                    s.spawn(async move { c.iter().map(|(a, b, ign)| occ.path(*a, *b, *ign)).collect::<Vec<Path>>() });
                }
            })
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .into_iter()
        } else {
            misses.iter().map(|(a, b, ign)| occ.path(*a, *b, *ign)).collect::<Vec<_>>().into_iter()
        };
        for (ti, ri, cached) in pairs {
            let (e, id, _, truth, tprof, ..) = rfid_tags[ti];
            let r = readers[ri];
            let p = match cached {
                Some(p) => p,
                None => {
                    let p = cast.next().expect("one cast per miss");
                    path_cache.insert((*e, r.entity), (pos_bits(*truth, r.pos), riding.get(e).copied(), p));
                    p
                }
            };
            let p = p.in_band(r.profile.freq_mhz);
            let to_tag = (*truth - r.pos).normalize_or_zero();
            // The same gain the map and the optimiser use: the device's own antenna, then an
            // applied FDTD one, then the profile's.
            let g_r = ants.gain(r.antenna.as_ref(), Tech::Rfid, true, &r.profile, r.forward, to_tag);
            miss[ti] *= 1.0 - radio::rfid_read_probability(&r.profile, tprof, &p, g_r);
            // A parked tag keeps its fade: only moving something changes it.
            let l = link_state(&mut st.links, st.seed, (*e, r.entity), (id, &r.id), (*truth, r.pos), p.wall_eq, radio::rfid_hold_m(&r.profile));
            let link = radio::rfid_link(&r.profile, tprof, p, g_r, l.held.n1 * radio::RFID_FADE_SIGMA_DB, &mut l.rng);
            tag_links[ti].push(Link {
                other: r.entity,
                kind: LinkKind::Rfid,
                walls: p.walls,
                blocker: p.blocker,
                value: link.readable.then_some(link.rssi_dbm),
                rssi_dbm: link.rssi_dbm,
                wall_eq: p.wall_eq,
                bias_eq: p.bias_eq,
                true_distance: p.distance,
                used: link.readable,
            });
            if link.readable {
                in_field[ri] += 1;
                readable.push((ti, ri, link, p.walls, p.distance));
            }
        }
        let mut reads_this_tick = 0u32;
        let mut best: BTreeMap<usize, (f32, usize)> = BTreeMap::new();
        for (ti, ri, link, _, _) in &readable {
            let lambda = radio::rfid_reads_per_tag(&readers[*ri].profile, in_field[*ri]) * dt as f32;
            let l = st.links.get_mut(&(rfid_tags[*ti].0, readers[*ri].entity)).expect("made above");
            let count = lambda.floor() as u32 + l.rng.chance(lambda.fract()) as u32;
            if count == 0 {
                continue;
            }
            reads_this_tick += count;
            *links_per_infra.entry(readers[*ri].entity).or_default() += 1;
            let (_, id, _, _, _, addr, ..) = rfid_tags[*ti];
            let epc = if addr.is_empty() { super::device::epc_for(id) } else { addr.clone() };
            out.push(RtlsEvent::RfidRead {
                ts: now,
                reader: readers[*ri].id.clone(),
                tag: id.clone(),
                epc,
                rssi_dbm: link.rssi_dbm,
                phase_deg: link.phase_deg,
                count,
            });
            if best.get(ti).is_none_or(|(b, _)| link.rssi_dbm > *b) {
                best.insert(*ti, (link.rssi_dbm, *ri));
            }
        }
        if reads_this_tick > 0 {
            st.rfid_reads.push_back((now, reads_this_tick));
        }
        while st.rfid_reads.front().is_some_and(|(t, _)| *t < now - 10.0) {
            st.rfid_reads.pop_front();
        }
        for (ti, (e, id, _, truth, ..)) in rfid_tags.iter().enumerate() {
            let track = st.tracks.entry(*e).or_insert_with(|| Track::new(id.clone(), "rfid", *truth));
            track.links = std::mem::take(&mut tag_links[ti]);
            track.read_chance = 1.0 - miss[ti];
            if let Some((_, ri)) = best.get(&ti) {
                let r = readers[*ri];
                // Presence: "in front of reader R".
                let flat = Vec3::new(r.forward.x, 0.0, r.forward.z).normalize_or_zero();
                let est = Vec3::new(r.pos.x, track.plane_y, r.pos.z) + flat * 1.0;
                track.est = Some(est);
                track.err = Some((est - *truth).length());
                track.best_reader = Some(r.entity);
                track.last_fix = now;
                track.fixes += 1;
                track.reads += 1;
                track.first_read.get_or_insert(now);
                track.quality = 0.5;
            }
        }
    }

    // ---- Lost tags, zones, alarms
    let zone_list: Vec<&Zone> = zones.iter().collect();
    let mut occupancy: BTreeMap<String, u32> = zone_list.iter().map(|z| (z.name.clone(), 0)).collect();
    let mut alarms_raise: Vec<(String, ActiveAlarm)> = Vec::new();
    let mut alarms_clear: Vec<String> = Vec::new();
    for track in st.tracks.values_mut() {
        let lost = now - track.last_fix > s.lost_after_s as f64;
        let key = format!("tag_lost:{}", track.id);
        if lost && !track.lost {
            alarms_raise.push((key, ActiveAlarm { kind: "tag_lost".into(), source: track.id.clone(), message: format!("{} has no position for {:.0}s", track.id, s.lost_after_s), since: now }));
        } else if !lost && track.lost {
            alarms_clear.push(key);
        }
        track.lost = lost;
        let inside: BTreeSet<String> = match (track.est, lost) {
            (Some(p), false) => zone_list.iter().filter(|z| z.contains(p)).map(|z| z.name.clone()).collect(),
            _ => BTreeSet::new(),
        };
        for z in inside.difference(&track.zones) {
            out.push(RtlsEvent::Zone { ts: now, tag: track.id.clone(), zone: z.clone(), enter: true });
            if zone_list.iter().any(|zz| &zz.name == z && zz.restricted) {
                alarms_raise.push((
                    format!("restricted:{}:{}", z, track.id),
                    ActiveAlarm { kind: "restricted_zone".into(), source: track.id.clone(), message: format!("{} entered restricted zone {z}", track.id), since: now },
                ));
            }
        }
        for z in track.zones.difference(&inside) {
            out.push(RtlsEvent::Zone { ts: now, tag: track.id.clone(), zone: z.clone(), enter: false });
            alarms_clear.push(format!("restricted:{}:{}", z, track.id));
        }
        for z in &inside {
            *occupancy.entry(z.clone()).or_default() += 1;
        }
        track.zones = inside;
    }
    for z in &zone_list {
        let key = format!("capacity:{}", z.name);
        let n = occupancy.get(&z.name).copied().unwrap_or(0);
        if z.capacity > 0 && n > z.capacity {
            if !st.alarms.contains_key(&key) {
                alarms_raise.push((key, ActiveAlarm { kind: "zone_capacity".into(), source: z.name.clone(), message: format!("{} holds {n} tags (limit {})", z.name, z.capacity), since: now }));
            }
        } else if st.alarms.contains_key(&key) {
            alarms_clear.push(key);
        }
    }
    // Infrastructure that is placed but switched off counts as offline.
    for (_, dev, _, _) in devices.iter() {
        if dev.kind.is_mobile() {
            continue;
        }
        let key = format!("offline:{}", dev.id);
        if !dev.enabled && !st.alarms.contains_key(&key) {
            alarms_raise.push((key, ActiveAlarm { kind: "device_offline".into(), source: dev.id.clone(), message: format!("{} ({}) is offline", dev.id, dev.kind.label()), since: now }));
        } else if dev.enabled && st.alarms.contains_key(&key) {
            alarms_clear.push(key);
        }
    }
    for (k, a) in alarms_raise {
        if st.alarms.contains_key(&k) {
            continue;
        }
        out.push(RtlsEvent::Alarm { ts: now, kind: a.kind.clone(), source: a.source.clone(), message: a.message.clone(), active: true });
        st.log(format!("ALARM {}", a.message));
        st.alarms.insert(k, a);
    }
    for k in alarms_clear {
        if let Some(a) = st.alarms.remove(&k) {
            out.push(RtlsEvent::Alarm { ts: now, kind: a.kind.clone(), source: a.source.clone(), message: format!("cleared: {}", a.message), active: false });
            st.log(format!("cleared {}", a.message));
        }
    }
    st.zone_occupancy = occupancy.clone();

    for (e, n) in &links_per_infra {
        *st.infra_links.entry(*e).or_default() += n;
    }
    while st.errors.front().is_some_and(|(t, _, _)| *t < now - ERROR_WINDOW_S) {
        st.errors.pop_front();
    }

    // ---- Periodic device status
    if now >= st.next_status {
        st.next_status = now + s.status_every_s.max(0.5) as f64;
        for (e, dev, _, _) in devices.iter() {
            if dev.kind.is_mobile() {
                continue;
            }
            let links = st.infra_links.remove(&e).unwrap_or(0);
            out.push(RtlsEvent::DeviceStatus { ts: now, device: dev.id.clone(), kind: format!("{:?}", dev.kind), online: dev.enabled, links });
        }
    }

    // ---- Snapshot
    if now >= st.next_snapshot {
        st.next_snapshot = now + 1.0 / s.snapshot_hz.max(0.05) as f64;
        // The live view. With no fixes there is no error to report, not an error of zero.
        let (rmse, cep95) = st.error_stats_checked(None).map_or((f32::NAN, f32::NAN), |e| (e.rmse, e.cep95));
        let rfid_rate = st.rfid_rate();
        let mut tags: Vec<TagRow> = st
            .tracks
            .values()
            .map(|t| {
                // No estimate is no position: the true one isn't the engine's to report.
                let p = t.est.unwrap_or(Vec3::NAN);
                TagRow {
                    id: t.id.clone(),
                    tech: t.tech.into(),
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    quality: t.quality,
                    age_s: if t.last_fix.is_finite() { (now - t.last_fix) as f32 } else { -1.0 },
                    lost: t.lost,
                    restricted: t.zones.iter().any(|z| zone_list.iter().any(|zz| &zz.name == z && zz.restricted)),
                }
            })
            .collect();
        tags.sort_by(|a, b| a.id.cmp(&b.id));
        let located = tags.iter().filter(|t| !t.lost).count() as u32;
        let infra_total = devices.iter().filter(|(_, d, _, _)| !d.kind.is_mobile()).count() as u32;
        let ids: std::collections::HashMap<Entity, String> = devices.iter().map(|(e, d, _, _)| (e, d.id.clone())).collect();
        let rf = super::rf::analyze(&st, &ids, None).snap;
        let snap = Snapshot {
            ts: now,
            running: s.running,
            speed: s.speed,
            tags_total: tags.len() as u32,
            tags_located: located,
            tags_lost: tags.len() as u32 - located,
            infra_online: infra.len() as u32,
            infra_total,
            alarms_active: st.alarms.len() as u32,
            rmse_m: rmse,
            cep95_m: cep95,
            rfid_reads_per_s: rfid_rate,
            zones: occupancy,
            tags,
            rf,
        };
        st.series.push_back(SeriesPoint { t: now, rmse, located, reads: rfid_rate });
        while st.series.len() > 600 {
            st.series.pop_front();
        }
        st.snapshot = snap.clone();
        out.push(RtlsEvent::Snapshot(snap));
    }

    for e in &out {
        if let RtlsEvent::Zone { tag, zone, enter, .. } = e {
            let msg = format!("{tag} {} {zone}", if *enter { "entered" } else { "left" });
            st.log(msg);
        }
        hub.publish(e);
    }
    st.events_total += out.len() as u64;
}

// ---------------------------------------------------------------------------
// Headless regression test: `--rtls-test layout.json --duration 60 --report out.json`

#[derive(Resource, Clone, Debug)]
pub struct AutoTest {
    pub duration: f64,
    pub report: PathBuf,
    pub frames_ready: u32,
    /// Run auto deploy first (and save the deployed layout next to the report).
    pub deploy: bool,
}

/// Error figures, null when there were no fixes.
fn errors_json(e: Option<ErrorStats>) -> serde_json::Value {
    serde_json::json!({ "rmse_m": e.map(|e| e.rmse), "cep50_m": e.map(|e| e.cep50), "cep95_m": e.map(|e| e.cep95), "max_m": e.map(|e| e.max), "samples": e.map_or(0, |e| e.n) })
}

/// A set of updates: their errors, how many had a fix, and how far 95 % of them land with
/// no fix counted as a miss (null when more than 5 % had none).
fn run_json(r: &RunStats) -> serde_json::Value {
    let mut v = errors_json(r.stats());
    v["attempts"] = r.attempts.into();
    v["fixes"] = r.fixes.into();
    v["rejected_fixes"] = r.rejected.into();
    v["availability"] = serde_json::json!(r.availability());
    v["p95_counting_no_fix_m"] = serde_json::json!(r.p95_counting_misses().filter(|p| p.is_finite()));
    v
}

pub fn report_json(world: &mut World) -> serde_json::Value {
    let st = world.resource::<RtlsState>();
    let all = st.run_stats(None, None).stats();
    let per_tech: serde_json::Value = ["uwb", "ble", "rfid", "wifi"]
        .iter()
        .map(|t| {
            let mut v = run_json(&st.run_stats(Some(t), None));
            for (key, moving) in [("moving", true), ("parked", false)] {
                let mut class = run_json(&st.run_stats(Some(t), Some(moving)));
                class["tags"] = st.tracks.values().filter(|x| x.tech == *t && (if moving { x.moving_epochs } else { x.parked_epochs }) > 1).count().into();
                v[key] = class;
            }
            v["last_30s"] = errors_json(st.error_stats_checked(Some(t)));
            (t.to_string(), v)
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    let rfid: Vec<&Track> = st.tracks.values().filter(|t| t.tech == "rfid").collect();
    let read_by = |limit: f64| rfid.iter().filter(|t| t.first_read.is_some_and(|f| f <= limit)).count();
    let rfid_json = serde_json::json!({
        "tags": rfid.len(),
        "test_s": st.time,
        "read_within_test": read_by(f64::MAX),
        "read_within_5s": read_by(5.0),
        "mean_read_chance_per_round": (!rfid.is_empty()).then(|| rfid.iter().map(|t| t.read_chance).sum::<f32>() / rfid.len() as f32),
    });
    let mut tags: Vec<serde_json::Value> = st
        .tracks
        .values()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "tech": t.tech,
                "fix_rate": if t.attempts > 0 { t.fixes as f64 / t.attempts as f64 } else if t.tech == "rfid" { (t.reads > 0) as u8 as f64 } else { 0.0 },
                "fixes": t.fixes,
                "rfid_reads": t.reads,
                "read_chance_per_round": (t.tech == "rfid").then_some(t.read_chance),
                "first_read_s": t.first_read,
                "last_error_m": t.err,
                "lost": t.lost,
                "dop": t.dop,
                "anchors_used": t.used,
                "nlos_links": t.links.iter().filter(|l| l.walls > 0).count(),
                "links": t.links.len(),
            })
        })
        .collect();
    tags.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let alarms: Vec<String> = st.alarms.values().map(|a| a.message.clone()).collect();
    let sinks: Vec<serde_json::Value> = world
        .resource::<Hub>()
        .sinks
        .iter()
        .map(|s| {
            let st = s.shared.status();
            serde_json::json!({ "name": s.name, "state": format!("{:?}", st.state), "detail": st.detail, "sent": st.sent, "dropped": st.dropped })
        })
        .collect();
    let st = world.resource::<RtlsState>();
    serde_json::json!({
        "simulated": true,
        "seed": st.seed,
        "sim_time_s": st.time,
        "ms_per_tick": st.tick_ms,
        "window": format!("whole run, {:.0} s; last_30s is what a live view shows", st.time),
        "rmse_m": all.map(|e| e.rmse), "cep50_m": all.map(|e| e.cep50), "cep95_m": all.map(|e| e.cep95), "max_error_m": all.map(|e| e.max), "samples": all.map_or(0, |e| e.n),
        "last_30s": errors_json(st.error_stats_checked(None)),
        "by_tech": per_tech,
        "rfid": rfid_json,
        "unsynced_anchors": st.unsynced_anchors,
        "rfid_reads_per_s": st.rfid_rate(),
        "events": st.events_total,
        "snapshot": st.snapshot,
        "active_alarms": alarms,
        "tags": tags,
        "outputs": sinks,
    })
}

pub fn autotest(world: &mut World) {
    let loading = world.resource::<crate::prototype::PrototypeCache>().loading_count() > 0
        || !world.resource::<crate::scene::PendingSpawns>().0.is_empty()
        || world.resource::<Time>().elapsed_secs() < 3.0;
    let mut t = world.resource::<AutoTest>().clone();
    if loading {
        t.frames_ready = 0;
        world.insert_resource(t);
        return;
    }
    t.frames_ready += 1;
    world.insert_resource(t.clone());
    // Let transforms propagate so ray casts see the final scene.
    if t.frames_ready < 5 {
        return;
    }
    world.resource_mut::<RtlsConfig>().settings.running = false;
    let seed = world.resource::<RtlsConfig>().settings.seed;
    let mut deployed = None;
    if t.deploy {
        // `SENSESIM_DEPLOY_TECH=uwb|ble|aoa|wifi|rfid` and `SENSESIM_DEPLOY_WORKERS=n`
        // pick what to plan (used to build the bundled sample layouts).
        let mut req = super::deploy::DeployRequest::default();
        if let Ok(tech) = std::env::var("SENSESIM_DEPLOY_TECH") {
            req.uwb = tech.contains("uwb");
            req.ble = tech.contains("ble");
            req.rfid = tech.contains("rfid");
            req.ble_aoa = tech.contains("aoa");
            req.wifi = tech.contains("wifi");
        }
        if let Some(n) = std::env::var("SENSESIM_DEPLOY_WORKERS").ok().and_then(|n| n.parse().ok()) {
            req.workers = n;
        }
        // `SENSESIM_DEPLOY_OPTS=redundancy,dim3,colocate,heights` and
        // `SENSESIM_DEPLOY_TARGET=<m>`: the plan's options, for regression runs.
        if let Ok(o) = std::env::var("SENSESIM_DEPLOY_OPTS") {
            req.redundancy = o.contains("redundancy");
            req.dim3 = o.contains("dim3");
            req.colocate = o.contains("colocate");
            if o.contains("heights") {
                req.heights = vec![0.3, 1.8];
            }
        }
        if let Some(m) = std::env::var("SENSESIM_DEPLOY_TARGET").ok().and_then(|m| m.parse().ok()) {
            req.target_m = m;
        }
        let r = super::deploy::auto_deploy(world, &req);
        let plans: Vec<serde_json::Value> = r.plans.iter().map(|p| serde_json::json!({ "tech": p.tech, "devices": p.devices, "coverage": p.coverage, "reachable_coverage": p.reachable_coverage, "predicted_p95_m": p.p95_m.is_finite().then_some(p.p95_m), "cost": p.cost, "targets_met": p.targets_met })).collect();
        deployed = Some(serde_json::json!({ "placed": r.headline(), "coverage": r.coverage, "predicted_p95_m": r.predicted_p95_m.is_finite().then_some(r.predicted_p95_m), "targets_met": r.targets_met, "notes": r.notes, "plans": plans, "feedback_added": r.feedback_added, "uwb_cep95_m": r.uwb_cep95_m.is_finite().then_some(r.uwb_cep95_m), "rfid_unread": r.rfid_unread.len(), "inventory_tags": r.inventory_tags, "portal_readers": r.portal_readers }));
        let mut q = world.query::<(&super::device::Device, &Transform)>();
        let devs: Vec<(super::device::Device, Transform)> = q.iter(world).map(|(d, t)| (d.clone(), *t)).collect();
        let mut zq = world.query::<&super::device::Zone>();
        let zones: Vec<super::device::Zone> = zq.iter(world).cloned().collect();
        let mut oq = world.query::<(&crate::scene::PlacedObject, &Transform)>();
        let objs: Vec<serde_json::Value> = oq
            .iter(world)
            .map(|(o, t)| serde_json::json!({ "name": o.name, "asset": o.asset.to_string_lossy(), "translation": t.translation.to_array(), "rotation": t.rotation.to_array(), "scale": t.scale.to_array(), "joints": o.joints }))
            .collect();
        let doc = serde_json::json!({ "format": "sense_sim_layout", "version": 3, "objects": objs, "rtls": super::persist::rtls_json(devs.into_iter(), zones.into_iter()) });
        let _ = std::fs::write(t.report.with_extension("layout.json"), serde_json::to_string_pretty(&doc).unwrap_or_default());
    }
    world.resource_mut::<RtlsState>().restart(seed);
    run_for(world, t.duration);
    let mut report = report_json(world);
    if let Some(d) = deployed {
        report["auto_deploy"] = d;
    }
    let text = serde_json::to_string_pretty(&report).unwrap_or_default();
    if let Some(d) = t.report.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&t.report, &text);
    println!("{text}");
    world.write_message(AppExit::Success);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vertical wall (quad at x = 5, 0..3 m high, z -2..2) as one object.
    fn wall() -> Occluders {
        let pos = [[5.0, 0.0, -2.0], [5.0, 0.0, 2.0], [5.0, 3.0, 2.0], [5.0, 3.0, -2.0]];
        let bvh = Arc::new(Bvh::new(&pos, &[0, 1, 2, 0, 2, 3]));
        Occluders {
            objs: vec![Occ { root: Entity::PLACEHOLDER, min: Vec3::new(4.98, 0.0, -2.0), max: Vec3::new(5.02, 3.0, 2.0), material: Material::Concrete, weight: Material::Concrete.weight(), rack: false, name: String::new(), parts: vec![(bvh, Affine3A::IDENTITY)] }],
        }
    }

    /// A headless world running the real tick over the built-in profiles.
    fn world(extra: Vec<Profile>) -> World {
        let mut list: Vec<Profile> = serde_json::from_str(include_str!("../../assets/devices/generic.json")).unwrap();
        list.extend(extra);
        let mut w = World::new();
        w.insert_resource(RtlsState::new(1));
        w.insert_resource(RtlsConfig::default());
        w.insert_resource(Profiles(list, Default::default()));
        w.insert_resource(Hub::default());
        w.init_resource::<super::super::antenna::AntennaLibrary>();
        w.init_resource::<super::super::planning::AppliedAntenna>();
        w.init_resource::<super::super::planning::Coverage>();
        w.init_resource::<super::super::fdtd::Fdtd>();
        w
    }

    fn device(w: &mut World, id: &str, kind: DeviceKind, profile: &str, at: Vec3, motion: Motion) -> Entity {
        w.spawn((Device { id: id.into(), kind, profile: profile.into(), enabled: true, motion, address: String::new(), auto: false, radio: Default::default() }, Transform::from_translation(at))).id()
    }

    fn anchors(w: &mut World, profile: &str) {
        for (i, (x, z)) in [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].into_iter().enumerate() {
            device(w, &format!("A{i}"), DeviceKind::UwbAnchor, profile, Vec3::new(x, 2.6, z), Motion::Static);
        }
    }

    fn walker() -> Motion {
        Motion::Path { points: vec![[1.0, 1.2, 1.0], [9.0, 1.2, 1.0], [9.0, 1.2, 9.0], [1.0, 1.2, 9.0]], speed: 1.3, ping_pong: false }
    }

    fn restart(w: &mut World, seed: u64) {
        w.resource_mut::<RtlsState>().restart(seed);
    }

    fn tool(w: &mut World, name: &str, input: serde_json::Value) -> serde_json::Value {
        let out = super::super::tools::start(w, name, &input).unwrap();
        assert!(!out.is_error, "{}", out.summary());
        serde_json::from_str(&out.summary()).unwrap()
    }

    #[test]
    fn a_solution_nowhere_near_the_anchors_is_wild() {
        let a = Infra { entity: Entity::PLACEHOLDER, id: "A1".into(), kind: DeviceKind::UwbAnchor, pos: Vec3::ZERO, forward: Vec3::NEG_Z, profile: Profile { max_range_m: 40.0, ..Default::default() }, antenna: None };
        let infra = [a];
        assert!(near_infrastructure(&infra, "uwb", Vec3::new(50.0, 1.0, 0.0)));
        assert!(!near_infrastructure(&infra, "uwb", Vec3::new(70.0, 1.0, 0.0)));
        assert!(!near_infrastructure(&infra, "ble", Vec3::new(1.0, 1.0, 0.0)), "only its own technology's devices count");
    }

    #[test]
    fn no_fixes_is_no_result_not_a_perfect_one() {
        let mut w = world(Vec::new());
        // A tag and nothing to hear it.
        device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        run_for(&mut w, 2.0);
        let r = report_json(&mut w);
        assert!(r["rmse_m"].is_null() && r["cep95_m"].is_null(), "{r}");
        assert!(r["by_tech"]["uwb"]["cep95_m"].is_null(), "{r}");
        assert!(r["by_tech"]["uwb"]["attempts"].as_u64().unwrap_or(0) >= 10, "{r}");
        assert_eq!(r["by_tech"]["uwb"]["fixes"], 0, "{r}");
    }

    #[test]
    fn a_tag_with_no_position_does_not_report_its_true_one() {
        let mut w = world(Vec::new());
        device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        run_for(&mut w, 1.5);
        let snap = &w.resource::<RtlsState>().snapshot;
        assert!(!snap.tags[0].x.is_finite(), "{:?}", snap.tags[0]);
        assert!(snap.tags[0].lost);
    }

    #[test]
    fn a_long_run_reports_all_of_it_and_says_so() {
        let mut w = world(Vec::new());
        anchors(&mut w, "uwb-anchor");
        device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        run_for(&mut w, 45.0);
        let r = report_json(&mut w);
        let u = &r["by_tech"]["uwb"];
        // 10 Hz for 45 s, not the last 30 s of it.
        let n = u["samples"].as_u64().unwrap_or(0);
        assert!(n > 430, "{r}");
        assert!(r["window"].as_str().unwrap_or("").contains("whole run"), "{r}");
        assert!(u["last_30s"]["samples"].as_u64().unwrap_or(9999) <= 301, "{r}");
    }

    #[test]
    fn moving_and_parked_tags_are_scored_apart() {
        let mut w = world(Vec::new());
        anchors(&mut w, "uwb-anchor");
        device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(1.0, 1.2, 1.0), walker());
        device(&mut w, "T2", DeviceKind::UwbTag, "uwb-tag", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        run_for(&mut w, 5.0);
        let r = report_json(&mut w);
        let u = &r["by_tech"]["uwb"];
        assert!(u["moving"]["samples"].as_u64().unwrap_or(0) > 40, "{r}");
        assert!(u["parked"]["samples"].as_u64().unwrap_or(0) > 40, "{r}");
        assert_eq!(u["moving"]["tags"], 1, "{r}");
        assert_eq!(u["parked"]["tags"], 1, "{r}");
    }

    #[test]
    fn the_same_seed_reproduces_a_run() {
        let mut w = world(Vec::new());
        anchors(&mut w, "uwb-anchor");
        w.resource_mut::<RtlsConfig>().settings.running = false;
        let t = device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(1.0, 1.2, 1.0), walker());
        let start = w.get::<Transform>(t).unwrap().translation;
        let a = tool(&mut w, "rtls_run", serde_json::json!({ "seconds": 12.0 }));
        assert_eq!(w.get::<Transform>(t).unwrap().translation, start, "a run leaves the scene as it began");
        let b = tool(&mut w, "rtls_run", serde_json::json!({ "seconds": 12.0 }));
        for key in ["rmse_m", "cep50_m", "cep95_m", "max_error_m"] {
            assert_eq!(a[key], b[key], "{key}");
        }
        assert!(a["rmse_m"].as_f64().unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn another_tag_leaves_this_tags_draws_alone() {
        let run = |other: bool| {
            let mut w = world(Vec::new());
            anchors(&mut w, "uwb-anchor");
            let t = device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(3.0, 1.2, 4.0), Motion::Static);
            if other {
                device(&mut w, "T2", DeviceKind::UwbTag, "uwb-tag", Vec3::new(7.0, 1.2, 6.0), Motion::Static);
                device(&mut w, "A9", DeviceKind::UwbAnchor, "uwb-anchor", Vec3::new(5.0, 2.6, -3.0), Motion::Static);
            }
            run_for(&mut w, 2.0);
            let tr = &w.resource::<RtlsState>().tracks[&t];
            tr.links.iter().filter_map(|l| l.value).take(4).collect::<Vec<f32>>()
        };
        assert_eq!(run(false), run(true));
    }

    #[test]
    fn the_solver_is_not_told_the_tags_height() {
        // No noise at all: the only error left is solving at the wrong height.
        let quiet = Profile { id: "quiet-anchor".into(), kind: DeviceKind::UwbAnchor, range_sigma_m: 0.0, dropout: 0.0, ..Default::default() };
        let quiet_tag = Profile { id: "quiet-tag".into(), kind: DeviceKind::UwbTag, range_sigma_m: 0.0, mount_height_m: 1.2, ..Default::default() };
        let mut w = world(vec![quiet, quiet_tag]);
        anchors(&mut w, "quiet-anchor");
        // Starts at 1.2 m and climbs in place: a pallet lifted off the floor.
        let rise = Motion::Path { points: vec![[2.0, 1.2, 3.0], [2.0, 2.1, 3.0]], speed: 1.0, ping_pong: false };
        let t = device(&mut w, "T1", DeviceKind::UwbTag, "quiet-tag", Vec3::new(2.0, 1.2, 3.0), rise);
        run_for(&mut w, 0.5);
        run_for(&mut w, 0.45);
        let tr = &w.resource::<RtlsState>().tracks[&t];
        let raw = tr.raw.unwrap();
        let off = (Vec2::new(raw.x, raw.z) - Vec2::new(tr.truth.x, tr.truth.z)).length();
        assert!(off > 0.02, "solved at the true height: {off} m at y {}", tr.truth.y);
    }

    #[test]
    fn the_rfid_sim_uses_the_devices_own_antenna() {
        let mut w = world(Vec::new());
        let r = device(&mut w, "R1", DeviceKind::RfidReader, "rfid-reader", Vec3::new(0.0, 2.2, 0.0), Motion::Static);
        // Facing -Z, the tag right in the beam: read with the profile's 9 dBi, not through a -30 dBi antenna.
        w.get_mut::<Device>(r).unwrap().radio.antenna = Some(super::super::antenna::AntennaChoice::Omni { gain_dbi: -30.0 });
        let t = device(&mut w, "E1", DeviceKind::RfidTag, "rfid-tag", Vec3::new(0.0, 2.2, -5.0), Motion::Static);
        run_for(&mut w, 2.0);
        assert_eq!(w.resource::<RtlsState>().tracks[&t].reads, 0);
    }

    /// A reader and a tag at `margin` dB of forward link, over `seeds` runs: share ever read.
    fn ever_read(margin: f32, moving: bool, seeds: u64) -> f32 {
        let far = Profile { id: "far-reader".into(), kind: DeviceKind::RfidReader, tx_power_dbm: 30.0, antenna_gain_dbi: 9.0, beam_deg: 360.0, freq_mhz: 920.5, sensitivity_dbm: -200.0, max_range_m: 500.0, ..Default::default() };
        let d = 10f32.powf((30.0 + 9.0 + 2.0 + 18.0 - margin - 20.0 * 920.5f32.log10() + 27.55) / 20.0);
        let mut w = world(vec![far]);
        device(&mut w, "R1", DeviceKind::RfidReader, "far-reader", Vec3::new(0.0, 2.2, 0.0), Motion::Static);
        let at = Vec3::new(d, 2.2, 0.0);
        let motion = if moving { Motion::Path { points: vec![[d, 2.2, -1.5], [d, 2.2, 1.5]], speed: 1.0, ping_pong: true } } else { Motion::Static };
        let t = device(&mut w, "E1", DeviceKind::RfidTag, "rfid-tag", at, motion);
        let mut read = 0;
        for seed in 1..=seeds {
            w.get_mut::<Transform>(t).unwrap().translation = at;
            restart(&mut w, seed);
            run_for(&mut w, 3.0);
            read += (w.resource::<RtlsState>().tracks[&t].reads > 0) as u32;
        }
        read as f32 / seeds as f32
    }

    #[test]
    fn a_parked_tags_fade_is_held_until_something_moves() {
        // -2 dB of margin through a 2 dB fade reads about one round in six. Redrawn every
        // 50 ms, nearly every tag gets read within seconds; held, only those in a good spot.
        let parked = ever_read(-2.0, false, 80);
        assert!(parked > 0.05 && parked < 0.32, "{parked}");
        // A tag that keeps moving gets a new fade every half wavelength.
        let moving = ever_read(-2.0, true, 20);
        assert!(moving > 0.8, "{moving}");
    }

    #[test]
    fn rfid_reports_a_per_round_chance_and_reads_in_a_stated_time() {
        let mut w = world(Vec::new());
        device(&mut w, "R1", DeviceKind::RfidReader, "rfid-reader", Vec3::new(0.0, 2.2, 0.0), Motion::Static);
        device(&mut w, "E1", DeviceKind::RfidTag, "rfid-tag", Vec3::new(0.0, 2.2, -4.0), Motion::Static);
        run_for(&mut w, 2.0);
        let r = report_json(&mut w);
        let tag = &r["tags"][0];
        assert!(tag["read_chance_per_round"].as_f64().unwrap_or(0.0) > 0.9, "{r}");
        assert_eq!(r["rfid"]["tags"], 1, "{r}");
        assert_eq!(r["rfid"]["read_within_test"], 1, "{r}");
        assert_eq!(r["rfid"]["test_s"].as_f64().map(|s| s.round()), Some(2.0), "{r}");
    }

    #[test]
    fn an_anchor_that_cannot_reach_the_others_gives_no_tdoa() {
        let reach = Profile { id: "reach-anchor".into(), kind: DeviceKind::UwbAnchor, range_sigma_m: 0.08, max_range_m: 20.0, dropout: 0.0, ..Default::default() };
        let mut w = world(vec![reach]);
        anchors(&mut w, "reach-anchor");
        // Hears the tag, 16 m away, but is more than 20 m from every other anchor.
        let lone = device(&mut w, "A9", DeviceKind::UwbAnchor, "reach-anchor", Vec3::new(30.0, 2.6, 5.0), Motion::Static);
        let t = device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag-tdoa", Vec3::new(14.0, 1.2, 5.0), Motion::Static);
        for _ in 0..10 {
            run_for(&mut w, 0.1);
            let tr = &w.resource::<RtlsState>().tracks[&t];
            let link = tr.links.iter().find(|l| l.other == lone).expect("in the list");
            assert!(link.value.is_none() && !link.used, "{link:?}");
        }
    }

    #[test]
    fn a_blink_is_as_strong_as_the_tag_that_sends_it() {
        let mut w = world(Vec::new());
        anchors(&mut w, "uwb-anchor");
        let t = device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag-tdoa", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        w.get_mut::<Device>(t).unwrap().radio.uwb_tx_dbm_mhz = Some(-90.0);
        run_for(&mut w, 2.0);
        assert_eq!(w.resource::<RtlsState>().tracks[&t].fixes, 0);
    }

    #[test]
    fn a_parked_tag_keeps_part_of_its_ranging_error() {
        // The Kalman filter can't average away what stays with the spot.
        let mut w = world(Vec::new());
        anchors(&mut w, "uwb-anchor");
        let t = device(&mut w, "T1", DeviceKind::UwbTag, "uwb-tag", Vec3::new(5.0, 1.2, 5.0), Motion::Static);
        let mut sq = 0.0;
        let seeds = 30;
        for seed in 1..=seeds {
            restart(&mut w, seed);
            run_for(&mut w, 2.0);
            // Where it settles over 6 s: independent noise would average out to almost nothing.
            let mut sum = Vec3::ZERO;
            for _ in 0..60 {
                run_for(&mut w, 0.1);
                sum += w.resource::<RtlsState>().tracks[&t].est.unwrap();
            }
            sq += (sum / 60.0 - Vec3::new(5.0, 1.2, 5.0)).length_squared();
        }
        let rms = (sq / seeds as f32).sqrt();
        assert!(rms > 0.035, "{rms}");
    }

    #[test]
    fn the_map_measures_tdoa_when_every_tag_blinks() {
        let tdoa_err = |profile: &str| {
            let mut w = world(Vec::new());
            anchors(&mut w, "uwb-anchor");
            // 5 m outside the square, where losing the emission time hurts most.
            device(&mut w, "T1", DeviceKind::UwbTag, profile, Vec3::new(15.0, 1.2, 5.0), Motion::Static);
            let _ = w.run_system_cached_with(super::super::planning::compute, super::super::planning::CoverageRequest { tech: Tech::Uwb, height: 1.2, cell: 1.0 });
            let c = w.resource::<super::super::planning::Coverage>();
            let (ix, iz) = (((14.5 - c.origin.x) / c.cell) as usize, ((5.0 - c.origin.y) / c.cell) as usize);
            c.cells[iz * c.nx + ix].err
        };
        let twr = tdoa_err("uwb-tag");
        let tdoa = tdoa_err("uwb-tag-tdoa");
        assert!(tdoa > twr * 1.5, "tdoa {tdoa} twr {twr}");
    }

    #[test]
    fn rfid_coverage_does_not_report_decibels_as_metres() {
        let mut w = world(Vec::new());
        device(&mut w, "R1", DeviceKind::RfidReader, "rfid-reader", Vec3::new(0.0, 2.2, 0.0), Motion::Static);
        device(&mut w, "E1", DeviceKind::RfidTag, "rfid-tag", Vec3::new(4.0, 1.2, 4.0), Motion::Static);
        let r = tool(&mut w, "rtls_coverage", serde_json::json!({ "tech": "rfid", "cell": 1.0 }));
        assert!(r["p95_predicted_error_m"].is_null(), "{r}");
        assert!(r["mean_predicted_error_m"].is_null(), "{r}");
        assert!(r["quality"]["pass_fraction"].is_null(), "{r}");
        assert!(r["read_margin_db"]["p95"].as_f64().is_some(), "{r}");
    }

    #[test]
    fn wall_blocks_only_paths_through_it() {
        let o = wall();
        assert_eq!(o.path(Vec3::new(0.0, 1.0, 0.0), Vec3::new(10.0, 1.0, 0.0), None).walls, 1);
        // Over the top and beside it: clear.
        assert_eq!(o.path(Vec3::new(0.0, 4.0, 0.0), Vec3::new(10.0, 4.0, 0.0), None).walls, 0);
        assert_eq!(o.path(Vec3::new(0.0, 1.0, 3.0), Vec3::new(10.0, 1.0, 3.0), None).walls, 0);
        // A device mounted on the wall still sees the room.
        assert_eq!(o.path(Vec3::new(4.97, 1.0, 0.0), Vec3::new(0.0, 1.0, 0.0), None).walls, 0);
        // Ignoring the object (a tag riding on it).
        assert_eq!(o.path(Vec3::new(0.0, 1.0, 0.0), Vec3::new(10.0, 1.0, 0.0), Some(Entity::PLACEHOLDER)).walls, 0);
        let (t, n) = o.nearest(Ray3d::new(Vec3::new(0.0, 1.0, 0.0), Dir3::X), 100.0).unwrap();
        assert!((t - 5.0).abs() < 1e-4 && n.x.abs() > 0.99);
    }
}
