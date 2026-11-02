//! Radio along a robot's route. A robot doesn't care about the coverage map's average:
//! it stops at the first gap on its path, and pauses at every handoff. This walks the
//! route, follows which access point it would hold on to, and sees how it changes as
//! racks fill and empty.

use bevy::prelude::*;
use proof::{Claim, Dist, Evidence, Factor, RobustOptions, Target, robust_mut};
use serde::{Deserialize, Serialize};

use super::stress::{restore_materials, scale_materials};
use crate::rtls::device::{Device, Profile, Profiles};
use crate::rtls::planning::Tech;
use crate::rtls::radio::{self, SiteConditions};
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

/// Evenly spaced points along a polyline, about `step` apart, and the spacing they got.
pub fn walk(points: &[Vec3], step: f32) -> (Vec<Vec3>, f32) {
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    if points.len() < 2 || total <= 0.0 {
        return (points.to_vec(), step.max(0.05));
    }
    let n = (total / step.max(0.05)).ceil().max(1.0) as usize;
    let spacing = total / n as f32;
    let mut out = Vec::with_capacity(n + 1);
    let (mut leg, mut before) = (0usize, 0.0f32);
    for k in 0..n {
        let along = k as f32 * spacing;
        while leg + 2 < points.len() && along > before + points[leg].distance(points[leg + 1]) {
            before += points[leg].distance(points[leg + 1]);
            leg += 1;
        }
        let length = points[leg].distance(points[leg + 1]).max(1e-6);
        out.push(points[leg].lerp(points[leg + 1], ((along - before) / length).clamp(0.0, 1.0)));
    }
    out.push(points[points.len() - 1]);
    (out, spacing)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteRules {
    /// Below this the link counts as gone, dBm.
    pub threshold_dbm: f32,
    /// How much stronger another access point must be to move to it, dB.
    pub hysteresis_db: f32,
    pub speed_mps: f32,
    /// Link down per handoff, seconds.
    pub handoff_s: f32,
    /// Throughput the robot needs, Mbit/s.
    pub need_mbps: f32,
    /// What the receiver's noise floor is, dBm.
    pub noise_dbm: f32,
    /// Channel width, MHz.
    pub channel_mhz: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteStats {
    pub length_m: f32,
    /// Share of the route with no usable link.
    pub weak_share: f32,
    /// The longest stretch with no usable link, metres.
    pub worst_gap_m: f32,
    /// Where it starts, as an index along `walk`.
    pub worst_gap_at: usize,
    pub roams: usize,
    /// Time with no link per pass, gaps and handoffs together, seconds.
    pub outage_s: f32,
    /// Share of the route slower than the robot needs.
    pub slow_share: f32,
}

/// Throughput the link would carry, half of Shannon's limit, Mbit/s.
pub fn rate_mbps(signal_dbm: f32, noise_dbm: f32, channel_mhz: f32) -> f32 {
    let snr = 10f32.powf((signal_dbm - noise_dbm) / 10.0);
    0.5 * channel_mhz * (1.0 + snr).log2()
}

/// `signals[sample][ap]` along the route, one sample every `step` metres.
pub fn analyse(signals: &[Vec<f32>], step: f32, r: RouteRules) -> RouteStats {
    if signals.is_empty() || signals[0].is_empty() {
        return RouteStats::default();
    }
    let best = |s: &[f32]| s.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |x| x.0);
    let mut serving = best(&signals[0]);
    let (mut roams, mut weak, mut slow) = (0usize, 0usize, 0usize);
    let (mut gap, mut worst, mut worst_at, mut gap_start) = (0usize, 0usize, 0usize, 0usize);
    for (i, s) in signals.iter().enumerate() {
        let b = best(s);
        // Hold on until something's clearly better, or the link's gone and anything's better.
        if b != serving && (s[b] >= s[serving] + r.hysteresis_db || (s[serving] < r.threshold_dbm && s[b] > s[serving])) {
            serving = b;
            roams += 1;
        }
        if s[serving] < r.threshold_dbm {
            if gap == 0 {
                gap_start = i;
            }
            gap += 1;
            weak += 1;
            if gap > worst {
                worst = gap;
                worst_at = gap_start;
            }
        } else {
            gap = 0;
        }
        if rate_mbps(s[serving], r.noise_dbm, r.channel_mhz) < r.need_mbps {
            slow += 1;
        }
    }
    let n = signals.len() as f32;
    let length_m = (n - 1.0).max(0.0) * step;
    let weak_m = weak as f32 * step;
    RouteStats { length_m, weak_share: weak as f32 / n, worst_gap_m: worst as f32 * step, worst_gap_at: worst_at, roams, outage_s: weak_m / r.speed_mps.max(0.01) + roams as f32 * r.handoff_s, slow_share: slow as f32 / n }
}

/// Signal from each access point at each sample.
#[allow(clippy::type_complexity)]
pub fn signals_system(In((samples, tech)): In<(Vec<Vec3>, Tech)>, parts: OccluderParts, objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>, devices: Query<(&Device, &Transform)>, profiles: Res<Profiles>) -> (Vec<String>, Vec<Vec<f32>>) {
    let occ = Occluders::build(&parts, &objects);
    let mut aps: Vec<(String, Vec3, Profile)> = devices.iter().filter(|(d, _)| d.enabled && d.kind == tech.infra()).map(|(d, t)| (d.id.clone(), t.translation, profiles.for_device(d))).collect();
    aps.sort_by(|a, b| a.0.cmp(&b.0));
    let signals = samples
        .iter()
        .map(|p| {
            aps.iter()
                .map(|(_, at, prof)| {
                    let path = occ.path(*p, *at, None).in_band(prof.freq_mhz);
                    // What the robot hears from it: its power, over the link it sets out.
                    radio::ble_mean_rssi(prof, prof, radio::Path { gain_db: prof.antenna_gain_dbi, ..path })
                })
                .collect()
        })
        .collect();
    (aps.into_iter().map(|a| a.0).collect(), signals)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteReport {
    pub access_points: Vec<String>,
    pub racks_full: RouteStats,
    pub racks_empty: RouteStats,
    pub claim: Claim,
    /// The start of the worst gap with racks full.
    pub worst_gap_near: [f32; 3],
    pub summary: String,
}

pub fn run(world: &mut World, points: &[Vec3], tech: Tech, rules: RouteRules, max_gap_m: f32, samples: usize, scene: &str) -> Result<RouteReport, String> {
    if points.len() < 2 {
        return Err("a route needs at least two points".into());
    }
    let (along, step) = walk(points, 0.5);
    let before = SiteConditions::current();
    let measure = |world: &mut World| -> Result<(Vec<String>, RouteStats), String> {
        let (ids, signals) = world.run_system_cached_with(signals_system, (along.clone(), tech)).map_err(|e| e.to_string())?;
        Ok((ids, analyse(&signals, step, rules)))
    };
    SiteConditions::set(SiteConditions { rack_fill: 1.0, ..before });
    let first = measure(world);
    let (ids, full) = match first {
        Ok(v) => v,
        Err(e) => {
            SiteConditions::set(before);
            return Err(e);
        }
    };
    if ids.is_empty() {
        SiteConditions::set(before);
        return Err(format!("no {} access points in the scene: place some first", tech.label()));
    }
    SiteConditions::set(SiteConditions { rack_fill: 0.1, ..before });
    let second = measure(world);
    let empty = match second {
        Ok(v) => v.1,
        Err(e) => {
            SiteConditions::set(before);
            return Err(e);
        }
    };
    let factors = vec![
        Factor::new("Rack fill", "share", Dist::Uniform { lo: 0.1, hi: 1.0 }, "Stock changes what stands between the robot and the radio"),
        Factor::new("Material scale", "×", Dist::LogNormal { median: 1.0, sigma: 0.3 }, "Built-in material costs are typical values, not this site's"),
    ];
    let target = Target::at_most(max_gap_m as f64);
    let report = robust_mut("Longest stretch with no link", "m", &factors, Some(target), Evidence::Simulated, RobustOptions { samples: samples.max(4), seed: 111 }, false, |v| {
        SiteConditions::set(SiteConditions { rack_fill: v[0] as f32, ..before });
        let saved = scale_materials(v[1] as f32);
        let out = measure(world).map_or(f64::NAN, |x| x.1.worst_gap_m as f64);
        restore_materials(saved);
        out
    });
    SiteConditions::set(before);
    let worst_gap_near = along.get(full.worst_gap_at).copied().unwrap_or(Vec3::ZERO).to_array();
    let claim = Claim::from_report(&format!("route:{scene}:{}", tech.label()), "robot_route", &format!("Radio along the robot's route, {scene}"), &report, Some(target), vec![
        "Signal strength only: interference, congestion and how the robot's radio really roams aren't modelled".into(),
        format!("The link is gone below {:.0} dBm, and each handoff takes {:.0} ms", rules.threshold_dbm, rules.handoff_s * 1000.0),
        "A pilot on the real floor is still the test".into(),
    ]);
    let summary = format!(
        "On this {:.0} m route: longest gap {:.1} m with racks full and {:.1} m nearly empty, {} handoffs, about {:.1} s with no link per pass. {}",
        full.length_m,
        full.worst_gap_m,
        empty.worst_gap_m,
        full.roams,
        full.outage_s,
        if full.worst_gap_m > 0.0 { format!("The worst gap starts near x {:.1}, z {:.1}.", worst_gap_near[0], worst_gap_near[2]) } else { "No gaps with racks full.".to_string() }
    );
    Ok(RouteReport { access_points: ids, racks_full: full, racks_empty: empty, claim, worst_gap_near, summary })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> RouteRules {
        RouteRules { threshold_dbm: -70.0, hysteresis_db: 6.0, speed_mps: 1.0, handoff_s: 0.1, need_mbps: 1.0, noise_dbm: -90.0, channel_mhz: 20.0 }
    }

    #[test]
    fn walking_a_route_spaces_points_evenly() {
        let (w, step) = walk(&[Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.0, 0.0, 1.0)], 0.5);
        assert_eq!(w.len(), 7);
        assert_eq!(step, 0.5);
        assert_eq!(w[0], Vec3::ZERO);
        assert_eq!(*w.last().unwrap(), Vec3::new(2.0, 0.0, 1.0));
        // Legs that aren't a multiple of the step still add up to the real length.
        let legs: Vec<Vec3> = (0..4).map(|k| Vec3::new(k as f32 * 0.7, 0.0, 0.0)).collect();
        let (w, step) = walk(&legs, 0.5);
        assert!(((w.len() - 1) as f32 * step - 2.1).abs() < 1e-4, "{} × {step}", w.len());
        assert!(w.windows(2).all(|p| (p[0].distance(p[1]) - step).abs() < 1e-4), "{w:?}");
    }

    #[test]
    fn a_strong_route_has_no_gaps() {
        let s = analyse(&vec![vec![-50.0, -80.0]; 20], 0.5, rules());
        assert_eq!(s.worst_gap_m, 0.0);
        assert_eq!(s.roams, 0);
        assert_eq!(s.weak_share, 0.0);
        assert_eq!(s.outage_s, 0.0);
    }

    #[test]
    fn a_dead_stretch_is_measured_and_placed() {
        let mut sig = vec![vec![-50.0]; 10];
        sig.extend(vec![vec![-90.0]; 6]);
        sig.extend(vec![vec![-50.0]; 4]);
        let s = analyse(&sig, 0.5, rules());
        assert_eq!(s.worst_gap_m, 3.0);
        assert_eq!(s.worst_gap_at, 10);
        assert!((s.weak_share - 0.3).abs() < 1e-6);
        assert!((s.outage_s - 3.0).abs() < 1e-5, "{}", s.outage_s);
    }

    #[test]
    fn hysteresis_keeps_a_robot_from_flapping() {
        // Two access points trading a 2 dB lead: under the 6 dB hysteresis, so no handoffs.
        let sig: Vec<Vec<f32>> = (0..20).map(|i| if i % 2 == 0 { vec![-60.0, -62.0] } else { vec![-62.0, -60.0] }).collect();
        assert_eq!(analyse(&sig, 0.5, rules()).roams, 0);
        // One clear crossover is one handoff, and it costs its pause.
        let cross: Vec<Vec<f32>> = (0..20).map(|i| if i < 10 { vec![-50.0, -65.0] } else { vec![-65.0, -50.0] }).collect();
        let s = analyse(&cross, 0.5, rules());
        assert_eq!(s.roams, 1);
        assert!((s.outage_s - 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_dying_link_moves_even_without_the_full_hysteresis() {
        let sig = vec![vec![-69.0, -72.0], vec![-71.0, -68.0]];
        // −71 is below the line and −68 is better, though only by 3 dB.
        assert_eq!(analyse(&sig, 0.5, rules()).roams, 1);
    }

    #[test]
    fn throughput_grows_with_signal() {
        // 30 dB of SNR on 20 MHz at half of Shannon: about 100 Mbit/s.
        assert!((rate_mbps(-60.0, -90.0, 20.0) - 99.7).abs() < 1.0);
        assert!(rate_mbps(-90.0, -90.0, 20.0) < rate_mbps(-60.0, -90.0, 20.0));
    }
}
