//! The simulation tick: move tags, measure every radio link against the
//! scene geometry, solve positions, track zones and alarms, publish events.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bevy::math::DVec3;
use std::sync::Arc;

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::device::{Device, DeviceKind, Motion, MotionState, Profile, Profiles, UwbMode, Zone};
use super::events::{RtlsEvent, Snapshot, TagRow};
use super::io::{Control, Hub};
use super::planning::Tech;
use super::radio::{self, Material, Path};
use super::solver::{self, Dim};
use super::{ActiveAlarm, Link, LinkKind, RtlsConfig, RtlsState, SeriesPoint, Track};
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
    /// BVH and world → mesh transform per part.
    parts: Vec<(Arc<Bvh>, Affine3A)>,
}

/// Mesh parts of placed objects with their BVH.
pub type OccluderParts<'w, 's> = Query<'w, 's, (&'static PartOf, &'static PartBvh, &'static GlobalTransform)>;

/// Does the segment a + t·d, t ∈ [0, 1], touch the box?
fn segment_hits_box(a: Vec3, d: Vec3, min: Vec3, max: Vec3) -> bool {
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for i in 0..3 {
        if d[i].abs() < 1e-9 {
            if a[i] < min[i] || a[i] > max[i] {
                return false;
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
                return false;
            }
        }
    }
    true
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
                let (material, weight) = radio::obstruction_of_asset(&o.asset);
                Occ { root: e, min: min - Vec3::splat(0.02), max: max + Vec3::splat(0.02), material, weight, parts: Vec::new() }
            })
            .collect();
        let index: std::collections::HashMap<Entity, usize> = objs.iter().enumerate().map(|(i, o)| (o.root, i)).collect();
        for (of, bvh, gt) in parts {
            if let Some(&i) = index.get(&of.0) {
                objs[i].parts.push((bvh.0.clone(), gt.affine().inverse()));
            }
        }
        objs.retain(|o| !o.parts.is_empty());
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
        for o in &self.objs {
            if Some(o.root) == ignore || !segment_hits_box(a, d, o.min, o.max) {
                continue;
            }
            for (bvh, inv) in &o.parts {
                // Mesh space with an unnormalized direction keeps t in world units.
                if bvh.cast(inv.transform_point3(start), inv.transform_vector3(dir), limit).is_some() {
                    path.cross_weighted(o.material, o.weight);
                    break;
                }
            }
        }
        path
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
}

/// Changes whenever a placed object moves, appears or gets its meshes.
fn scene_signature(parts: &OccluderParts, objects: &Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.iter().count().hash(&mut h);
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
        if dev.kind.is_mobile() && dev.enabled
            && let Some(obj) = apply_motion(dev, &mut t, &mut ms, dt as f32, &mut st.rng, &objects)
        {
            riding.insert(e, obj);
        }
    }

    let sig = scene_signature(&parts, &objects);
    if cache.sig != Some(sig) || cache.occ.is_none() {
        cache.occ = Some(Occluders::build(&parts, &objects));
        cache.paths.clear();
        cache.sig = Some(sig);
    }
    let PathCache { occ, paths: path_cache, .. } = &mut *cache;
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

    // ---- UWB and BLE positioning
    let ants = super::antenna::Antennas { library: &library, applied: &applied };
    for (e, id, kind, truth, tag_prof, _, tag_fwd, tag_ant) in &mobile {
        let tech = kind.tech();
        let track = st.tracks.entry(*e).or_insert_with(|| Track::new(id.clone(), tech, *truth));
        track.id = id.clone();
        track.truth = *truth;
        if !matches!(kind, DeviceKind::UwbTag | DeviceKind::BleTag) || now < track.next_update {
            continue;
        }
        track.next_update = now + 1.0 / tag_prof.update_hz.max(0.1) as f64;
        track.attempts += 1;
        let ignore = riding.get(e).copied();
        let mut links: Vec<Link> = Vec::new();
        let mut fix: Option<solver::Fix> = None;
        let mut sigma_hint = 0.1f64;

        match kind {
            DeviceKind::UwbTag => {
                let anchors: Vec<&Infra> = infra.iter().filter(|a| a.kind == DeviceKind::UwbAnchor).collect();
                // Antenna gain of both ends towards each other rides on the path.
                let paths: Vec<Path> = anchors
                    .iter()
                    .map(|a| {
                        let gain = ants.gain(a.antenna.as_ref(), Tech::Uwb, true, &a.profile, a.forward, *truth - a.pos)
                            + ants.gain(tag_ant.as_ref(), Tech::Uwb, false, tag_prof, *tag_fwd, a.pos - *truth);
                        Path { gain_db: gain, ..occ.path(*truth, a.pos, ignore) }
                    })
                    .collect();
                let dim_for = |pts: &[DVec3]| {
                    let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
                    if s.solve_3d && pts.len() >= 4 && hi - lo > 1.0 { Dim::Space } else { Dim::Plane(truth.y as f64) }
                };
                match tag_prof.mode {
                    UwbMode::Twr => {
                        let mut a_pos = Vec::new();
                        let mut ranges = Vec::new();
                        let mut sig = Vec::new();
                        let mut idx = Vec::new();
                        for (a, p) in anchors.iter().zip(&paths) {
                            let r = radio::uwb_range(&a.profile, tag_prof, *p, &mut st.rng);
                            links.push(Link { other: a.entity, kind: LinkKind::Range, walls: p.walls, blocker: p.blocker, value: r, rssi_dbm: radio::uwb_rx_power(&a.profile, *p), wall_eq: p.wall_eq, true_distance: p.distance, used: false });
                            if let Some(r) = r {
                                idx.push(links.len() - 1);
                                a_pos.push(dv(a.pos));
                                ranges.push(r as f64);
                                // First-path power diagnostic, as UWB chips report it: a blocked
                                // path arrives several dB below the free-space value expected
                                // at the measured range. Flagged ranges are down-weighted.
                                let fp = radio::uwb_rx_power(&a.profile, *p) + st.rng.gauss(1.5);
                                let expected = radio::uwb_rx_power(&a.profile, Path { distance: r, gain_db: p.gain_db, ..Default::default() });
                                let base = a.profile.range_sigma_m.max(0.02) as f64;
                                let nlos_flag = s.robust && expected - fp > 4.0;
                                sig.push(if nlos_flag { base * 6.0 + a.profile.nlos_bias_m as f64 } else { base });
                                *links_per_infra.entry(a.entity).or_default() += 1;
                                out.push(RtlsEvent::Range {
                                    ts: now,
                                    tag: id.clone(),
                                    anchor: a.id.clone(),
                                    range_m: r,
                                    los: p.los(),
                                    rssi_dbm: fp,
                                });
                            }
                        }
                        let dim = dim_for(&a_pos);
                        if a_pos.len() >= solver::min_measurements(dim).min(3) {
                            let prior = track.est.map(dv);
                            let gate = 3.0 * sig.iter().copied().fold(f64::MAX, f64::min);
                            fix = if s.robust {
                                solver::solve_twr_robust(dim, &a_pos, &ranges, &sig, prior, gate.max(0.15))
                            } else {
                                solver::solve_twr(dim, &a_pos, &ranges, &sig, prior)
                            };
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
                            let t = radio::uwb_arrival_m(&a.profile, *p, 0.0, &mut st.rng);
                            links.push(Link { other: a.entity, kind: LinkKind::Tdoa, walls: p.walls, blocker: p.blocker, value: None, rssi_dbm: radio::uwb_rx_power(&a.profile, *p), wall_eq: p.wall_eq, true_distance: p.distance, used: false });
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
                            let sigma = (anchors[rk].profile.sync_sigma_ns * radio::C_M_PER_NS).max(0.03) as f64 * 1.4;
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
            DeviceKind::BleTag => {
                let nodes: Vec<&Infra> = infra.iter().filter(|a| a.kind == DeviceKind::BleNode).collect();
                let mut a_pos = Vec::new();
                let mut d_est = Vec::new();
                let mut sig = Vec::new();
                let mut best: Option<(f32, Vec3)> = None;
                for n in &nodes {
                    let gain = ants.gain(n.antenna.as_ref(), Tech::Ble, true, &n.profile, n.forward, *truth - n.pos)
                        + ants.gain(tag_ant.as_ref(), Tech::Ble, false, tag_prof, *tag_fwd, n.pos - *truth);
                    let p = Path { gain_db: gain, ..occ.path(*truth, n.pos, ignore) };
                    let r = radio::ble_rssi(&n.profile, tag_prof, p, &mut st.rng);
                    links.push(Link { other: n.entity, kind: LinkKind::Rssi, walls: p.walls, blocker: p.blocker, value: r, rssi_dbm: r.unwrap_or_else(|| radio::ble_mean_rssi(&n.profile, tag_prof, p)), wall_eq: p.wall_eq, true_distance: p.distance, used: r.is_some() });
                    if let Some(r) = r {
                        *links_per_infra.entry(n.entity).or_default() += 1;
                        out.push(RtlsEvent::Rssi { ts: now, tag: id.clone(), node: n.id.clone(), rssi_dbm: r, los: p.los() });
                        let d = solver::rssi_to_distance(r as f64, tag_prof.rssi_at_1m as f64, n.profile.path_loss_exp as f64);
                        a_pos.push(dv(n.pos));
                        d_est.push(d);
                        // Multiplicative error of log-distance ranging.
                        sig.push((d * n.profile.shadow_sigma_db as f64 * std::f64::consts::LN_10 / (10.0 * n.profile.path_loss_exp as f64)).max(0.3));
                        if best.is_none_or(|(b, _)| r > b) {
                            best = Some((r, n.pos));
                        }
                    }
                }
                if a_pos.len() >= 3 {
                    fix = solver::solve_twr(Dim::Plane(truth.y as f64), &a_pos, &d_est, &sig, track.est.map(dv));
                    sigma_hint = sig.iter().sum::<f64>() / sig.len() as f64;
                } else if let Some((_, p)) = best {
                    // Proximity: "near the strongest beacon".
                    fix = Some(solver::Fix { pos: DVec3::new(p.x as f64, truth.y as f64, p.z as f64), residual: 3.0, used: vec![], dop: 9.9 });
                    sigma_hint = 3.0;
                }
            }
            _ => {}
        }

        track.links = links;
        if let Some(f) = fix.filter(|f| f.pos.is_finite()) {
            let raw = f.pos.as_vec3();
            // Reject wild solutions (outside any sane distance from the infrastructure).
            if (raw - *truth).length() > 200.0 {
                continue;
            }
            track.raw = Some(raw);
            let dt_fix = (now - track.last_fix).clamp(0.0, 5.0);
            let est = if s.kalman {
                if dt_fix >= 5.0 {
                    track.kf.reset();
                }
                track.kf.predict(dt_fix);
                track.kf.update(f.pos, (f.residual.max(sigma_hint) * f.dop.clamp(1.0, 10.0)).max(0.03));
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
            track.quality = ((1.0 / (1.0 + 4.0 * f.residual)) * (1.5 / f.dop.max(1.5))).clamp(0.0, 1.0) as f32;
            let err = (est - *truth).length();
            track.err = Some(err);
            st.errors.push_back((now, err, tech));
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
            let (e, _, _, truth, tprof, ..) = rfid_tags[ti];
            let r = readers[ri];
            let p = match cached {
                Some(p) => p,
                None => {
                    let p = cast.next().expect("one cast per miss");
                    path_cache.insert((*e, r.entity), (pos_bits(*truth, r.pos), riding.get(e).copied(), p));
                    p
                }
            };
            let to_tag = (*truth - r.pos).normalize_or_zero();
            let angle = r.forward.angle_between(to_tag);
            let link = radio::rfid_link(&r.profile, tprof, p, angle, &mut st.rng);
            tag_links[ti].push(Link {
                other: r.entity,
                kind: LinkKind::Rfid,
                walls: p.walls,
                blocker: p.blocker,
                value: link.readable.then_some(link.rssi_dbm),
                rssi_dbm: link.rssi_dbm,
                wall_eq: p.wall_eq,
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
            let count = lambda.floor() as u32 + st.rng.chance(lambda.fract()) as u32;
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
            if let Some((_, ri)) = best.get(&ti) {
                let r = readers[*ri];
                // Presence: "in front of reader R".
                let flat = Vec3::new(r.forward.x, 0.0, r.forward.z).normalize_or_zero();
                let est = Vec3::new(r.pos.x, truth.y, r.pos.z) + flat * 1.0;
                track.est = Some(est);
                track.err = Some((est - *truth).length());
                track.best_reader = Some(r.entity);
                track.last_fix = now;
                track.fixes += 1;
                track.reads += 1;
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
    while st.errors.front().is_some_and(|(t, _, _)| *t < now - 30.0) {
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
        let (rmse, _, cep95, _) = st.error_stats(None);
        let rfid_rate = st.rfid_rate();
        let mut tags: Vec<TagRow> = st
            .tracks
            .values()
            .map(|t| {
                let p = t.est.unwrap_or(t.truth);
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

pub fn report_json(world: &mut World) -> serde_json::Value {
    let st = world.resource::<RtlsState>();
    let (rmse, cep50, cep95, max) = st.error_stats(None);
    let per_tech: serde_json::Value = ["uwb", "ble", "rfid"]
        .iter()
        .map(|t| {
            let (r, c50, c95, m) = st.error_stats(Some(t));
            (t.to_string(), serde_json::json!({ "rmse_m": r, "cep50_m": c50, "cep95_m": c95, "max_m": m }))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
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
        "sim_time_s": st.time,
        "ms_per_tick": st.tick_ms,
        "rmse_m": rmse, "cep50_m": cep50, "cep95_m": cep95, "max_error_m": max,
        "by_tech": per_tech,
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
        // `SENSESIM_DEPLOY_TECH=uwb|ble|rfid` and `SENSESIM_DEPLOY_WORKERS=n`
        // pick what to plan (used to build the bundled sample layouts).
        let mut req = super::deploy::DeployRequest::default();
        if let Ok(tech) = std::env::var("SENSESIM_DEPLOY_TECH") {
            req.uwb = tech.contains("uwb");
            req.ble = tech.contains("ble");
            req.rfid = tech.contains("rfid");
        }
        if let Some(n) = std::env::var("SENSESIM_DEPLOY_WORKERS").ok().and_then(|n| n.parse().ok()) {
            req.workers = n;
        }
        let r = super::deploy::auto_deploy(world, &req);
        deployed = Some(serde_json::json!({ "placed": r.headline(), "coverage": r.coverage, "predicted_p95_m": r.predicted_p95_m, "targets_met": r.targets_met, "notes": r.notes }));
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
            objs: vec![Occ { root: Entity::PLACEHOLDER, min: Vec3::new(4.98, 0.0, -2.0), max: Vec3::new(5.02, 3.0, 2.0), material: Material::Concrete, weight: Material::Concrete.weight(), parts: vec![(bvh, Affine3A::IDENTITY)] }],
        }
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
