//! Electronic shelf labels: can every label be reached, and how long does a price change
//! across the store take? Metal fixtures are what get in the way, and a vendor's coverage
//! radius assumes they aren't there. Labels sit on the faces of the shelves; each listens
//! to its best gateway, gateways that hear each other share airtime unless they're on
//! different channels.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, Target, robust_mut};
use serde::{Deserialize, Serialize};

use super::stress::{restore_materials, scale_materials};
use crate::rtls::device::{Device, DeviceKind, Motion, Profile, Profiles, next_id};
use crate::rtls::radio;
use crate::rtls::rng::Rng;
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

/// A sample of a whole store is enough, and keeps a large one quick.
const MAX_LABELS: usize = 4000;

/// Is this object somewhere labels live?
fn is_fixture(name: &str, asset: &std::path::Path) -> bool {
    let text = format!("{} {}", name, asset.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default()).to_ascii_lowercase();
    // By word, so "rack" doesn't match "track" or "bracket".
    text.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| ["shelf", "shelv", "gondola", "rack", "fixture", "refrigerat", "fridge"].iter().any(|k| w.starts_with(k)))
}

/// Chance a label gets a packet at this margin above its limit, dB. Fades about 3 dB
/// around the mean, in the same logistic form as the rest of the radio model.
pub fn packet_success(margin_db: f32) -> f32 {
    radio::normal_above(margin_db, 3.0)
}

/// Gives each gateway one of `channels`, avoiding neighbours where it can. Returns the
/// channel per gateway and how many neighbouring pairs still share one.
pub fn assign_channels(hears: &[Vec<bool>], channels: usize) -> (Vec<usize>, usize) {
    let n = hears.len();
    let mut order: Vec<usize> = (0..n).collect();
    let degree = |i: usize| hears[i].iter().filter(|x| **x).count();
    order.sort_by_key(|i| std::cmp::Reverse(degree(*i)));
    let mut chosen = vec![usize::MAX; n];
    for i in order {
        let mut used = vec![0usize; channels.max(1)];
        for j in 0..n {
            if hears[i][j] && chosen[j] != usize::MAX {
                used[chosen[j]] += 1;
            }
        }
        chosen[i] = (0..used.len()).min_by_key(|c| used[*c]).unwrap_or(0);
    }
    let mut conflicts = 0;
    for i in 0..n {
        for j in i + 1..n {
            if hears[i][j] && chosen[i] == chosen[j] {
                conflicts += 1;
            }
        }
    }
    (chosen, conflicts)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EslResult {
    /// Sampled, up to a limit in a big store.
    pub labels: usize,
    /// What the update time is for: as given, or one per spot on the shelves.
    pub assumed_labels: usize,
    pub reached_share: f32,
    /// Heard, but within a few dB of the limit.
    pub weak_share: f32,
    /// Time to update every reachable label once, seconds.
    pub update_all_s: f32,
    pub channels: Vec<usize>,
    pub channel_conflicts: usize,
    pub labels_per_gateway: Vec<usize>,
    /// Fixtures with the most unreachable labels: (name, share unreachable).
    pub worst_fixtures: Vec<(String, f32)>,
}

/// Scores from the signal at each label from each gateway. `hears` is gateway to gateway.
pub fn score(signals: &[Vec<f32>], fixture_of: &[usize], fixtures: &[String], hears: &[Vec<bool>], sensitivity_dbm: f32, updates_per_s: f32, channels: usize) -> EslResult {
    let gateways = signals.first().map_or(0, Vec::len);
    let mut tries = vec![0f32; gateways];
    let mut count = vec![0usize; gateways];
    let (mut reached, mut weak) = (0usize, 0usize);
    let mut lost = vec![(0usize, 0usize); fixtures.len()];
    for (l, s) in signals.iter().enumerate() {
        let Some((g, best)) = s.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1)) else { continue };
        lost[fixture_of[l]].1 += 1;
        let margin = best - sensitivity_dbm;
        if margin < 0.0 {
            lost[fixture_of[l]].0 += 1;
            continue;
        }
        reached += 1;
        if margin < 6.0 {
            weak += 1;
        }
        tries[g] += 1.0 / packet_success(margin).max(0.01);
        count[g] += 1;
    }
    let (assigned, conflicts) = assign_channels(hears, channels);
    // Gateways on one channel that hear each other take turns.
    let mut update_all_s = 0f32;
    for g in 0..gateways {
        let sharing: f32 = (0..gateways).filter(|o| *o == g || (hears[g][*o] && assigned[*o] == assigned[g])).map(|o| tries[o]).sum();
        update_all_s = update_all_s.max(sharing / updates_per_s.max(0.01));
    }
    let n = signals.len().max(1) as f32;
    let mut worst: Vec<(String, f32)> = fixtures.iter().zip(&lost).filter(|(_, l)| l.0 > 0).map(|(f, l)| (f.clone(), l.0 as f32 / l.1.max(1) as f32)).collect();
    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
    worst.truncate(5);
    EslResult { labels: signals.len(), assumed_labels: signals.len(), reached_share: reached as f32 / n, weak_share: weak as f32 / n, update_all_s, channels: assigned, channel_conflicts: conflicts, labels_per_gateway: count, worst_fixtures: worst }
}

pub struct Scene {
    pub signals: Vec<Vec<f32>>,
    pub fixture_of: Vec<usize>,
    pub fixtures: Vec<String>,
    pub hears: Vec<Vec<bool>>,
    pub gateways: Vec<String>,
    /// Spots on the shelves before sampling.
    pub all_spots: usize,
    pub sensitivity_dbm: f32,
}

/// Labels on the faces of the fixtures, and what each gateway delivers to them.
#[allow(clippy::type_complexity)]
pub fn scene_system(In((extra, label_profile, gateway_profile)): In<(Vec<Vec3>, Profile, Profile)>, parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>, devices: Query<(&Device, &Transform)>, profiles: Res<Profiles>) -> Scene {
    let occ = Occluders::build(&parts, &objects);
    let mut gws: Vec<(String, Vec3, Profile)> = devices.iter().filter(|(d, _)| d.enabled && d.kind == DeviceKind::BleNode && d.profile.contains("esl")).map(|(d, t)| (d.id.clone(), t.translation, profiles.for_device(d))).collect();
    gws.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, at) in extra.iter().enumerate() {
        gws.push((format!("new {}", k + 1), *at, gateway_profile.clone()));
    }
    let (mut points, mut fixture_of, mut fixtures) = (Vec::new(), Vec::new(), Vec::new());
    for (_, o, t) in objects.iter() {
        if !is_fixture(&o.name, &o.asset) {
            continue;
        }
        let (mn, mx) = crate::agent::tools::world_bounds(o, t);
        let f = fixtures.len();
        fixtures.push(o.name.clone());
        // Along both long faces, every half metre, at each shelf height.
        let along_x = mx.x - mn.x >= mx.z - mn.z;
        let length = if along_x { mx.x - mn.x } else { mx.z - mn.z };
        let n = (length / 0.5).ceil().max(1.0) as usize;
        for k in 0..n {
            let s = (k as f32 + 0.5) / n as f32;
            for h in [0.3f32, 0.8, 1.3, 1.8].into_iter().filter(|h| mn.y + h < mx.y) {
                let y = mn.y + h;
                let faces = if along_x { [Vec3::new(mn.x + s * length, y, mn.z - 0.05), Vec3::new(mn.x + s * length, y, mx.z + 0.05)] } else { [Vec3::new(mn.x - 0.05, y, mn.z + s * length), Vec3::new(mx.x + 0.05, y, mn.z + s * length)] };
                for p in faces {
                    points.push(p);
                    fixture_of.push(f);
                }
            }
        }
    }
    // What one sends and the other hears: the gateway's power, down to the label.
    // At random: every nth would keep the same face and heights all along a shelf.
    let all_spots = points.len();
    let keep = (MAX_LABELS as f32 / all_spots.max(1) as f32).min(1.0);
    let mut rng = Rng::new(1);
    let (points, fixture_of): (Vec<Vec3>, Vec<usize>) = points.into_iter().zip(fixture_of).filter(|_| keep >= 1.0 || rng.chance(keep)).unzip();
    let signals = points.iter().map(|p| gws.iter().map(|(_, at, prof)| radio::ble_mean_rssi(&label_profile, prof, radio::Path { gain_db: prof.antenna_gain_dbi, ..occ.path(*p, *at, None).in_band(prof.freq_mhz) })).collect()).collect();
    let hears = gws.iter().map(|(_, a, pa)| gws.iter().map(|(_, b, pb)| a != b && radio::ble_mean_rssi(pb, pa, radio::Path { gain_db: pa.antenna_gain_dbi + pb.antenna_gain_dbi, ..occ.path(*a, *b, None).in_band(pb.freq_mhz) }) > pb.sensitivity_dbm).collect()).collect();
    let sensitivity_dbm = label_profile.sensitivity_dbm;
    Scene { signals, fixture_of, fixtures, hears, gateways: gws.into_iter().map(|g| g.0).collect(), all_spots, sensitivity_dbm }
}

/// Places `count` gateways on the ceiling, walls and columns (racks if the building has
/// none of those), each where it reaches the most labels nothing else reaches yet.
/// Returns where each one goes and which way it faces.
pub fn place_greedy(world: &mut World, count: usize, label: &Profile, gateway: &Profile) -> Result<Vec<(Vec3, Vec3)>, String> {
    let bounds = world.run_system_cached(bounds_system).map_err(|e| e.to_string())?;
    let Some((lo, hi)) = bounds else { return Err("the scene is empty".into()) };
    // A coarser grid in a bigger store.
    let step = (((hi.x - lo.x) * (hi.z - lo.z)).max(1.0) / 150.0).sqrt().max(3.0);
    let (spots, mounts) = crate::rtls::deploy::scene_mounts(world, gateway.mount_height_m, step, true);
    let structure: Vec<(Vec3, Vec3)> = spots.iter().zip(&mounts).filter(|(_, m)| **m != crate::rtls::placement::Mount::Rack).map(|(s, _)| *s).collect();
    let spots = if structure.is_empty() { spots } else { structure };
    if spots.is_empty() {
        return Err("nothing to mount gateways on: add a ceiling, walls, columns or racks first".into());
    }
    let candidates: Vec<Vec3> = spots.iter().map(|s| s.0).collect();
    let all = world.run_system_cached_with(scene_system, (candidates.clone(), label.clone(), gateway.clone())).map_err(|e| e.to_string())?;
    let existing = all.gateways.len() - candidates.len();
    let limit = label.sensitivity_dbm + 6.0;
    let mut covered: Vec<bool> = all.signals.iter().map(|s| s[..existing].iter().any(|v| *v >= limit)).collect();
    let mut out = Vec::new();
    for _ in 0..count {
        let gain = |c: usize| all.signals.iter().enumerate().filter(|(l, s)| !covered[*l] && s[existing + c] >= limit).count();
        let Some(best) = (0..candidates.len()).max_by_key(|c| gain(*c)) else { break };
        if gain(best) == 0 {
            break;
        }
        for (l, s) in all.signals.iter().enumerate() {
            if s[existing + best] >= limit {
                covered[l] = true;
            }
        }
        out.push(spots[best]);
    }
    Ok(out)
}

fn bounds_system(parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>) -> Option<(Vec3, Vec3)> {
    Occluders::build(&parts, &objects).bounds()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EslReport {
    pub result: EslResult,
    pub gateways: Vec<String>,
    pub placed: Vec<[f32; 3]>,
    pub claim: Claim,
    pub summary: String,
}

#[allow(clippy::too_many_arguments)]
pub fn run(world: &mut World, place: usize, apply: bool, labels_total: Option<usize>, updates_per_s: f32, channels: usize, min_reach: f64, samples: usize, scene: &str) -> Result<EslReport, String> {
    let (label, gateway) = {
        let p = world.resource::<Profiles>();
        (p.get("esl-label").cloned().ok_or("no esl-label profile")?, p.get("esl-gateway").cloned().ok_or("no esl-gateway profile")?)
    };
    let placed = if place > 0 { place_greedy(world, place, &label, &gateway)? } else { Vec::new() };
    if apply {
        for (at, facing) in &placed {
            let ids: Vec<String> = world.query::<&Device>().iter(world).map(|d| d.id.clone()).collect();
            let id = next_id(gateway.kind, ids.iter().map(String::as_str));
            let up = if facing.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
            world.spawn((Name::new(format!("{} {id}", gateway.kind.label())), Device { id, kind: gateway.kind, profile: gateway.id.clone(), enabled: true, motion: Motion::Static, address: String::new(), auto: false, radio: Default::default() }, Transform::from_translation(*at).looking_to(*facing, up)));
        }
        world.flush();
    }
    let extra: Vec<Vec3> = if apply { Vec::new() } else { placed.iter().map(|p| p.0).collect() };
    let s = world.run_system_cached_with(scene_system, (extra.clone(), label.clone(), gateway.clone())).map_err(|e| e.to_string())?;
    if s.signals.is_empty() {
        return Err("no shelves or racks in the scene to put labels on".into());
    }
    if s.gateways.is_empty() {
        return Err("no gateways: place esl-gateway devices, or ask for some with place".into());
    }
    let mut result = score(&s.signals, &s.fixture_of, &s.fixtures, &s.hears, s.sensitivity_dbm, updates_per_s, channels);
    // Airtime goes with how many labels there really are, not how many were sampled.
    result.assumed_labels = labels_total.unwrap_or(s.all_spots).max(1);
    result.update_all_s *= result.assumed_labels as f32 / result.labels.max(1) as f32;
    let factors = vec![Factor::new("How much fixtures and walls cost", "×", Dist::LogNormal { median: 1.0, sigma: 0.4 }, "Built-in material costs are typical values, not this store's")];
    let target = Target::at_least(min_reach);
    let report = robust_mut("Labels reached", "share", &factors, Some(target), Evidence::Estimate, RobustOptions { samples: samples.max(4), seed: 121 }, false, |v| {
        let saved = scale_materials(v[0] as f32);
        let out = world.run_system_cached_with(scene_system, (extra.clone(), label.clone(), gateway.clone())).map_or(f64::NAN, |x| score(&x.signals, &x.fixture_of, &x.fixtures, &x.hears, x.sensitivity_dbm, updates_per_s, channels).reached_share as f64);
        restore_materials(saved);
        out
    });
    let claim = Claim::from_report(&format!("esl:{scene}"), "shelf_labels", &format!("Shelf labels reached, {scene}"), &report, Some(target), vec!["The gateway's power and the label's sensitivity are placeholders, which keeps this an estimate: use your vendor's figures".into(), "Signal strength and airtime only: other 2.4 GHz traffic isn't modelled".into(), format!("{updates_per_s} label updates per second per gateway is assumed: use your vendor's figure"), format!("Labels are sampled along shelf faces, every half metre at four heights{}", if labels_total.is_some() { "" } else { ", and the update time takes one label per spot: give labels for your real count" })]);
    let summary = format!("{:.0} % of {} sampled spots reached ({:.0} % only just), and updating {} labels takes about {:.0} s with {} channel conflicts.", result.reached_share * 100.0, result.labels, result.weak_share * 100.0, result.assumed_labels, result.update_all_s, result.channel_conflicts);
    Ok(EslReport { gateways: s.gateways, placed: placed.iter().map(|v| v.0.to_array()).collect(), result, claim, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbours_get_different_channels_when_there_are_enough() {
        // Three gateways all hearing each other, three channels.
        let all = vec![vec![false, true, true], vec![true, false, true], vec![true, true, false]];
        let (c, conflicts) = assign_channels(&all, 3);
        assert_eq!(conflicts, 0);
        assert!(c[0] != c[1] && c[1] != c[2] && c[0] != c[2], "{c:?}");
        // Only two channels: one pair has to share, and it's counted.
        assert_eq!(assign_channels(&all, 2).1, 1);
        // Nobody hears anybody: nothing to avoid.
        let none = vec![vec![false; 3]; 3];
        assert_eq!(assign_channels(&none, 1).1, 0);
    }

    #[test]
    fn unreachable_labels_are_counted_and_named() {
        let signals = vec![vec![-60.0], vec![-60.0], vec![-100.0], vec![-100.0]];
        let r = score(&signals, &[0, 0, 1, 1], &["good".into(), "bad".into()], &[vec![false]], -90.0, 10.0, 3);
        assert_eq!(r.reached_share, 0.5);
        assert_eq!(r.worst_fixtures, vec![("bad".to_string(), 1.0)]);
    }

    #[test]
    fn update_time_follows_the_labels_and_shared_airtime() {
        let signals = vec![vec![-40.0, -200.0]; 100];
        let fixture = vec![0; 100];
        let names = vec!["f".to_string()];
        // A strong link needs one try: 100 labels at 10 per second.
        let alone = score(&signals, &fixture, &names, &[vec![false, false], vec![false, false]], -90.0, 10.0, 1);
        assert!((alone.update_all_s - 10.0).abs() < 0.1, "{}", alone.update_all_s);
        // Half on each of two gateways hearing each other, one channel: they take turns.
        let mut split = vec![vec![-40.0, -200.0]; 50];
        split.extend(vec![vec![-200.0, -40.0]; 50]);
        let hears = vec![vec![false, true], vec![true, false]];
        let shared = score(&split, &fixture, &names, &hears, -90.0, 10.0, 1);
        assert!((shared.update_all_s - 10.0).abs() < 0.1, "{}", shared.update_all_s);
        // Separate channels: each finishes its half in half the time.
        let apart = score(&split, &fixture, &names, &hears, -90.0, 10.0, 2);
        assert!((apart.update_all_s - 5.0).abs() < 0.1, "{}", apart.update_all_s);
    }

    #[test]
    fn a_weak_link_takes_more_tries() {
        assert!(packet_success(12.0) > 0.99);
        assert!(packet_success(0.0) < 0.6);
        let at = |db: f32| score(&vec![vec![db]; 10], &[0; 10], &["f".to_string()], &[vec![false]], -90.0, 1.0, 1).update_all_s;
        assert!(at(-89.0) > at(-60.0));
    }

    #[test]
    fn fixtures_are_recognised_by_name() {
        let p = std::path::Path::new("sm_store_shelf_a01.usd");
        assert!(is_fixture("x", p));
        assert!(is_fixture("gondola_3", std::path::Path::new("box.usd")));
        assert!(!is_fixture("cart_1", std::path::Path::new("sm_cart.usd")));
        assert!(!is_fixture("bracket_1", std::path::Path::new("sm_track.usd")));
    }
}
