//! Finding where anchors really are from the ranges between them, instead of surveying
//! each one by hand. Errors in anchor positions pass straight into tag positions, and
//! self-calibration is known to struggle when links are blocked. The scene is the edge
//! here: it says which links are blocked and by what, so those count for what they're
//! worth rather than pulling anchors out of place.

use anyhow::{Result, bail};
use bevy::prelude::*;
use proof::{Param, Problem, calibrate};
use serde::{Deserialize, Serialize};

use crate::rtls::device::{Device, DeviceKind, Profiles};
use crate::rtls::rng::Rng;
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::scene::PlacedObject;

const RANGE_SIGMA: f64 = 0.1;

/// One range between two anchors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnchorRange {
    pub a: String,
    pub b: String,
    pub range_m: f32,
    /// Made up by a practice run, not measured.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
}

pub fn parse(text: &str) -> Result<Vec<AnchorRange>> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate().map(|(i, l)| (i, l.trim())).filter(|(_, l)| !l.is_empty() && !l.starts_with('#')) {
        if l.starts_with('{') {
            out.push(serde_json::from_str::<AnchorRange>(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1))?);
            continue;
        }
        let p: Vec<&str> = l.split(',').map(str::trim).collect();
        if p.len() >= 2 && p[0].eq_ignore_ascii_case("a") && p[1].eq_ignore_ascii_case("b") {
            continue;
        }
        if p.len() < 3 {
            bail!("line {}: expected a,b,range_m", i + 1);
        }
        let range_m: f32 = p[2].parse().map_err(|_| anyhow::anyhow!("line {}: range '{}' isn't a number", i + 1, p[2]))?;
        out.push(AnchorRange { a: p[0].into(), b: p[1].into(), range_m, synthetic: false });
    }
    if out.is_empty() {
        bail!("no ranges in the file");
    }
    check_ranges(&out)?;
    Ok(out)
}

/// Ranges from anywhere have to be distances.
pub fn check_ranges(ranges: &[AnchorRange]) -> Result<()> {
    if let Some(r) = ranges.iter().find(|r| !(r.range_m.is_finite() && r.range_m >= 0.0)) {
        bail!("the range between {} and {} isn't a distance", r.a, r.b);
    }
    Ok(())
}

/// Enough better on pairs it never saw to be worth moving anything for.
pub fn worth_applying(before: f64, after: f64) -> bool {
    after < 0.9 * before
}

/// One range ready to fit: which two anchors, and what the scene says about the link.
#[derive(Clone, Debug)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    pub range: f64,
    /// Excess the obstructions on the way add on average, metres.
    pub excess: f64,
    pub sigma: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnchorResult {
    pub id: String,
    pub design: [f32; 3],
    pub fitted: [f32; 3],
    pub moved_m: f32,
    /// How well it's placed against the other anchors.
    pub sigma_m: f32,
    /// How well it's placed in the building. Ranges between anchors can't say where the whole
    /// set sits, so without surveyed anchors this is mostly how sure the design was.
    pub absolute_sigma_m: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnchorFit {
    pub anchors: Vec<AnchorResult>,
    pub ranges: usize,
    /// Distinct pairs. Repeats of one pair share its multipath, so they count once.
    pub pairs: usize,
    pub blocked_links: usize,
    pub rms_before_m: f64,
    pub rms_after_m: f64,
    pub outliers: usize,
    pub synthetic: bool,
    pub unknown_devices: Vec<String>,
    /// Anchors the ranges didn't really pin down: still mostly the design position.
    pub not_pinned_down: Vec<String>,
    /// Anchors whose position in the scene was surveyed, which ties the set to the building.
    pub surveyed: Vec<String>,
}

fn dist(p: &[f64], y: &[f64], a: usize, b: usize) -> f64 {
    let (dx, dz) = (p[2 * a] - p[2 * b], p[2 * a + 1] - p[2 * b + 1]);
    (dx * dx + dz * dz + (y[a] - y[b]).powi(2)).sqrt()
}

/// Repeats of a pair become one reading, the median, at the same noise. They share the same
/// multipath, so treating them as independent would claim far more certainty than they give.
fn one_per_pair(links: &[Link]) -> Vec<Link> {
    let mut groups: std::collections::BTreeMap<(usize, usize), Vec<&Link>> = Default::default();
    for l in links {
        groups.entry((l.a.min(l.b), l.a.max(l.b))).or_default().push(l);
    }
    groups
        .into_values()
        .map(|g| {
            let mut r: Vec<f64> = g.iter().map(|l| l.range).collect();
            r.sort_by(f64::total_cmp);
            Link { range: r[r.len() / 2], ..g[0].clone() }
        })
        .collect()
}

/// Whether the fit predicts pairs it never saw better than the design does: mean error on
/// held-out pairs, before and after, from five folds. Each pair counts for at most a metre, so
/// a blocked link can't decide it alone, while one moved anchor, which touches only some
/// pairs and so wouldn't show in a median, still does. `None` when too few pairs remain.
pub fn held_out(names: &[String], design: &[[f32; 3]], all: &[Link], prior_m: f64, surveyed: &[String]) -> Option<(f64, f64)> {
    let pairs = one_per_pair(all);
    let y: Vec<f64> = design.iter().map(|d| d[1] as f64).collect();
    let flat = |at: &[[f32; 3]]| -> Vec<f64> { at.iter().flat_map(|p| [p[0] as f64, p[2] as f64]).collect() };
    let start = flat(design);
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for fold in 0..5 {
        let train: Vec<Link> = pairs.iter().enumerate().filter(|(k, _)| k % 5 != fold).map(|(_, l)| l.clone()).collect();
        let f = fit(names, design, &train, prior_m, surveyed).ok()?;
        let at = flat(&f.anchors.iter().map(|a| a.fitted).collect::<Vec<_>>());
        for l in pairs.iter().enumerate().filter(|(k, _)| k % 5 == fold).map(|(_, l)| l) {
            before.push((l.range - dist(&start, &y, l.a, l.b) - l.excess).abs());
            after.push((l.range - dist(&at, &y, l.a, l.b) - l.excess).abs());
        }
    }
    let capped = |v: &[f64]| v.iter().map(|x| x.min(1.0)).sum::<f64>() / v.len().max(1) as f64;
    (!before.is_empty()).then(|| (capped(&before), capped(&after)))
}

/// How well a laser survey places an anchor, metres.
const SURVEYED_SIGMA: f64 = 0.02;

/// Each anchor's uncertainty against the others: what's left of the covariance once the
/// whole set shifting and turning together is taken out, as ranges between them can't see
/// that. `at` is x, z per anchor.
fn relative_sigmas(cov: &[Vec<f64>], at: &[f64]) -> Vec<f64> {
    let (m, n) = (at.len(), at.len() / 2);
    let mean = |o: usize| (0..n).map(|k| at[2 * k + o]).sum::<f64>() / n.max(1) as f64;
    let (cx, cz) = (mean(0), mean(1));
    let mut basis: Vec<Vec<f64>> = Vec::new();
    for mut b in [
        (0..m).map(|i| if i % 2 == 0 { 1.0 } else { 0.0 }).collect::<Vec<f64>>(),
        (0..m).map(|i| if i % 2 == 1 { 1.0 } else { 0.0 }).collect(),
        (0..n).flat_map(|k| [-(at[2 * k + 1] - cz), at[2 * k] - cx]).collect(),
    ] {
        for q in &basis {
            let d: f64 = b.iter().zip(q).map(|(x, y)| x * y).sum();
            b.iter_mut().zip(q).for_each(|(x, y)| *x -= d * y);
        }
        let len = b.iter().map(|x| x * x).sum::<f64>().sqrt();
        if len > 1e-9 {
            b.iter_mut().for_each(|x| *x /= len);
            basis.push(b);
        }
    }
    // (I − Σbbᵀ) C (I − Σbbᵀ)
    let mut c: Vec<Vec<f64>> = cov.to_vec();
    for b in &basis {
        let row: Vec<f64> = (0..m).map(|j| (0..m).map(|i| b[i] * c[i][j]).sum()).collect();
        for i in 0..m {
            for j in 0..m {
                c[i][j] -= b[i] * row[j];
            }
        }
    }
    for b in &basis {
        let col: Vec<f64> = (0..m).map(|i| (0..m).map(|j| c[i][j] * b[j]).sum()).collect();
        for i in 0..m {
            for j in 0..m {
                c[i][j] -= col[i] * b[j];
            }
        }
    }
    (0..n).map(|k| (c[2 * k][2 * k].max(0.0) + c[2 * k + 1][2 * k + 1].max(0.0)).sqrt()).collect()
}

/// Fits each anchor's floor position (x, z) with its design position as a prior. Height
/// is taken as known: mount heights are easy to measure, and ranges between anchors at
/// about the same height say almost nothing about it.
pub fn fit(names: &[String], design: &[[f32; 3]], all: &[Link], prior_m: f64, surveyed: &[String]) -> Result<AnchorFit> {
    let collapsed = one_per_pair(all);
    let links = &collapsed[..];
    if links.len() < names.len() {
        bail!("{} pairs of anchors can't pin down {} anchors; log every pair you can", links.len(), names.len());
    }
    let mut params = Vec::with_capacity(2 * names.len());
    let mut y = Vec::with_capacity(names.len());
    for (n, d) in names.iter().zip(design) {
        let prior = if surveyed.contains(n) { SURVEYED_SIGMA } else { prior_m };
        params.push(Param::new(&format!("{n} x"), d[0] as f64, prior, d[0] as f64 - 50.0, d[0] as f64 + 50.0));
        params.push(Param::new(&format!("{n} z"), d[2] as f64, prior, d[2] as f64 - 50.0, d[2] as f64 + 50.0));
        y.push(d[1] as f64);
    }
    let measured: Vec<f64> = links.iter().map(|l| l.range).collect();
    let mut sigma: Vec<f64> = links.iter().map(|l| l.sigma).collect();
    let solve = |sigma: &[f64]| calibrate(&Problem { params: &params, measured: &measured, sigma, robust_k: Some(3.0) }, |x| links.iter().map(|l| dist(x, &y, l.a, l.b) + l.excess).collect()).map_err(|e| anyhow::anyhow!(e));
    let mut cal = solve(&sigma)?;
    // Something in the way only ever makes a range read long. So one running long may be a
    // blockage nobody knew of and says little, but one reading short has to be taken at its
    // word: a robust fit that treats both alike can be dragged off by two blocked links.
    let long = |x: &[f64], l: &Link| l.range - dist(x, &y, l.a, l.b) - l.excess;
    for _ in 0..8 {
        let x = cal.values();
        let next: Vec<f64> = links.iter().map(|l| { let r = long(&x, l); if r > 3.0 * l.sigma { r } else { l.sigma } }).collect();
        if next.iter().zip(&sigma).all(|(a, b)| (a - b).abs() < 0.01) {
            break;
        }
        sigma = next;
        cal = solve(&sigma)?;
    }
    let x = cal.values();
    let explained = |l: &&Link| long(&x, l) <= 3.0 * l.sigma;
    let rms_of = |at: &[f64]| { let kept: Vec<f64> = links.iter().filter(explained).map(|l| l.range - dist(at, &y, l.a, l.b) - l.excess).collect(); (kept.iter().map(|r| r * r).sum::<f64>() / kept.len().max(1) as f64).sqrt() };
    let start: Vec<f64> = params.iter().map(|p| p.prior_mean).collect();
    let (rms_before_m, rms_after_m) = (rms_of(&start), rms_of(&x));
    let outliers = links.iter().filter(|l| !explained(l)).count();
    let relative = relative_sigmas(&cal.covariance, &x);
    let mut anchors = Vec::with_capacity(names.len());
    let mut not_pinned_down = Vec::new();
    for (k, n) in names.iter().enumerate() {
        let (px, pz) = (&cal.params[2 * k], &cal.params[2 * k + 1]);
        let d = design[k];
        let fitted = [px.value as f32, d[1], pz.value as f32];
        let moved = ((fitted[0] - d[0]).powi(2) + (fitted[2] - d[2]).powi(2)).sqrt();
        anchors.push(AnchorResult { id: n.clone(), design: d, fitted, moved_m: moved, sigma_m: relative[k] as f32, absolute_sigma_m: (px.sigma * px.sigma + pz.sigma * pz.sigma).sqrt() as f32 });
        // Against an absolute line: how much it learned depends on how vague the prior was.
        let heard = links.iter().filter(|l| l.a == k || l.b == k).count();
        if !surveyed.contains(n) && (heard < 3 || relative[k] > 0.3) {
            not_pinned_down.push(n.clone());
        }
    }
    Ok(AnchorFit { anchors, ranges: all.len(), pairs: links.len(), blocked_links: links.iter().filter(|l| l.excess > 0.0).count(), rms_before_m, rms_after_m, outliers, synthetic: false, unknown_devices: Vec::new(), not_pinned_down, surveyed: surveyed.to_vec() })
}

/// Everything needed about the anchors in the scene.
fn anchors_of(devices: &Query<(&Device, &Transform)>, profiles: &Profiles) -> Vec<(String, Vec3, f32, f32)> {
    let mut v: Vec<(String, Vec3, f32, f32)> = devices
        .iter()
        .filter(|(d, _)| d.enabled && d.kind == DeviceKind::UwbAnchor)
        .map(|(d, t)| {
            let p = profiles.for_device(d);
            (d.id.clone(), t.translation, p.nlos_bias_m, p.nlos_sigma_m)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Where the anchors range: a scale only counts in the band it was fitted in.
fn uwb_mhz(devices: &Query<(&Device, &Transform)>, profiles: &Profiles) -> f32 {
    devices.iter().find(|(d, _)| d.enabled && d.kind == DeviceKind::UwbAnchor).map_or(6489.6, |(d, _)| profiles.for_device(d).freq_mhz)
}

/// Turns ranges into links using the scene.
#[allow(clippy::type_complexity)]
pub fn links_system(
    In((ranges, use_scene, at)): In<(Vec<AnchorRange>, bool, Option<Vec<[f32; 3]>>)>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
) -> (Vec<String>, Vec<[f32; 3]>, Vec<Link>, Vec<String>) {
    let occ = Occluders::build(&parts, &objects);
    let anchors = anchors_of(&devices, &profiles);
    let names: Vec<String> = anchors.iter().map(|a| a.0.clone()).collect();
    let design: Vec<[f32; 3]> = anchors.iter().map(|a| a.1.to_array()).collect();
    let (mut links, mut unknown) = (Vec::new(), Vec::new());
    for r in &ranges {
        let (Some(a), Some(b)) = (names.iter().position(|n| *n == r.a), names.iter().position(|n| *n == r.b)) else {
            for id in [&r.a, &r.b] {
                if !names.contains(id) && !unknown.contains(id) {
                    unknown.push(id.clone());
                }
            }
            continue;
        };
        if !use_scene {
            links.push(Link { a, b, range: r.range_m as f64, excess: 0.0, sigma: RANGE_SIGMA });
            continue;
        }
        let spot = |k: usize| at.as_ref().and_then(|v| v.get(k)).map_or(anchors[k].1, |p| Vec3::from_array(*p));
        let path = occ.path(spot(a), spot(b), None).in_band(uwb_mhz(&devices, &profiles));
        let (bias, spread) = (anchors[a].2 as f64, anchors[a].3 as f64);
        let w = path.bias_eq as f64;
        // Excess delay spreads about as much as its own mean, so a blocked link says little.
        links.push(Link { a, b, range: r.range_m as f64, excess: w * (bias + spread), sigma: (RANGE_SIGMA * RANGE_SIGMA + (w * spread).powi(2)).sqrt() });
    }
    (names, design, links, unknown)
}

/// A practice log: ranges as the anchors would measure them from where `moves` really
/// puts them. For trying the loop and testing it against a known answer.
pub fn practice_system(
    In((moves, rounds, seed)): In<(std::collections::BTreeMap<String, [f32; 2]>, usize, u64)>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<(&Device, &Transform)>,
    profiles: Res<Profiles>,
) -> Vec<AnchorRange> {
    let occ = Occluders::build(&parts, &objects);
    let anchors = anchors_of(&devices, &profiles);
    let real: Vec<Vec3> = anchors.iter().map(|a| a.1 + moves.get(&a.0).map_or(Vec3::ZERO, |m| Vec3::new(m[0], 0.0, m[1]))).collect();
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for _ in 0..rounds.max(1) {
        for i in 0..anchors.len() {
            for j in i + 1..anchors.len() {
                let path = occ.path(real[i], real[j], None).in_band(uwb_mhz(&devices, &profiles));
                let w = path.bias_eq;
                let range = path.distance + rng.gauss(0.08) + w * anchors[i].2 + rng.exp((w * anchors[i].3).max(1e-4));
                out.push(AnchorRange { a: anchors[i].0.clone(), b: anchors[j].0.clone(), range_m: range, synthetic: true });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> (Vec<String>, Vec<[f32; 3]>) {
        let design: Vec<[f32; 3]> = vec![[0.0, 3.0, 0.0], [20.0, 3.0, 0.0], [20.0, 3.0, 20.0], [0.0, 3.0, 20.0], [10.0, 3.0, 0.0], [10.0, 3.0, 20.0]];
        ((0..6).map(|i| format!("A{}", i + 1)).collect(), design)
    }

    fn flat(p: [f32; 3], q: [f32; 3]) -> f64 {
        (((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)) as f64).sqrt()
    }

    /// Ranges from where the anchors really are. `blocked` lists pairs running long.
    fn links(real: &[[f32; 3]], blocked: &[(usize, usize)], scene_knows: bool, rng: &mut Rng) -> Vec<Link> {
        let mut out = Vec::new();
        for _ in 0..4 {
            for a in 0..real.len() {
                for b in a + 1..real.len() {
                    let is_blocked = blocked.contains(&(a, b));
                    let range = flat(real[a], real[b]) + rng.gauss(0.08) as f64 + if is_blocked { 1.5 } else { 0.0 };
                    let (excess, sigma) = if is_blocked && scene_knows { (1.5, 1.0) } else { (0.0, RANGE_SIGMA) };
                    out.push(Link { a, b, range, excess, sigma });
                }
            }
        }
        out
    }

    fn error_of(fit: &AnchorFit, real: &[[f32; 3]]) -> f32 {
        fit.anchors.iter().zip(real).map(|(r, t)| ((r.fitted[0] - t[0]).powi(2) + (r.fitted[2] - t[2]).powi(2)).sqrt()).fold(0.0, f32::max)
    }

    #[test]
    fn a_moved_anchor_is_found_from_the_ranges_alone() {
        let (names, design) = ring();
        let mut real = design.clone();
        real[1][0] -= 1.0;
        real[1][2] += 0.6;
        let mut rng = Rng::new(1);
        let f = fit(&names, &design, &links(&real, &[], true, &mut rng), 2.0, &[]).unwrap();
        println!("{f:#?}");
        let a2 = &f.anchors[1];
        assert!((a2.fitted[0] - real[1][0]).abs() < 0.25 && (a2.fitted[2] - real[1][2]).abs() < 0.25, "{a2:?}");
        assert!(a2.moved_m > 0.8);
        assert!(f.rms_after_m < f.rms_before_m / 3.0, "{} -> {}", f.rms_before_m, f.rms_after_m);
    }

    #[test]
    fn knowing_which_links_are_blocked_beats_not_knowing() {
        let (names, design) = ring();
        let mut real = design.clone();
        real[1][0] -= 1.0;
        // Everything A2 and A3 see across the middle runs long.
        let blocked = [(1, 3), (1, 5), (2, 4), (0, 2), (1, 2), (2, 3)];
        let mut worse = 0;
        let (mut aware_total, mut naive_total) = (0.0, 0.0);
        for seed in 0..12 {
            let aware = fit(&names, &design, &links(&real, &blocked, true, &mut Rng::new(seed)), 2.0, &[]).unwrap();
            let naive = fit(&names, &design, &links(&real, &blocked, false, &mut Rng::new(seed)), 2.0, &[]).unwrap();
            let (a, n) = (error_of(&aware, &real), error_of(&naive, &real));
            aware_total += a;
            naive_total += n;
            if a > n {
                worse += 1;
            }
        }
        println!("mean worst-anchor error: aware {:.3} m, naive {:.3} m", aware_total / 12.0, naive_total / 12.0);
        assert!(aware_total < 0.7 * naive_total, "aware {aware_total} naive {naive_total}");
        assert!(worse <= 2, "{worse}");
    }

    /// From a real run: an anchor at the edge of the site, with two of its seven links
    /// running 2 to 4 m long behind racks the model didn't know about. A symmetric robust fit
    /// put it 1.2 m from where it was, the wrong way.
    #[test]
    fn links_running_long_cant_drag_an_anchor_off() {
        let design: Vec<[f32; 3]> = vec![[6.0, 2.6, 1.28], [12.64, 2.6, 7.14], [2.21, 2.6, 2.69], [2.84, 2.6, 0.15], [2.41, 2.6, 10.38], [-0.43, 2.6, 3.64], [12.64, 2.6, 0.15], [9.37, 2.6, 10.64]];
        let names: Vec<String> = (0..8).map(|i| format!("A{}", i + 1)).collect();
        let mut real = design.clone();
        real[0][0] += 1.0;
        real[0][2] -= 0.6;
        let mut rng = Rng::new(7);
        let mut l = Vec::new();
        for _ in 0..5 {
            for a in 0..8 {
                for b in a + 1..8 {
                    let hidden = match (a, b) {
                        (0, 4) => 3.8 + rng.exp(0.5) as f64,
                        (0, 7) => 2.4 + rng.exp(0.5) as f64,
                        _ => 0.0,
                    };
                    l.push(Link { a, b, range: flat(real[a], real[b]) + rng.gauss(0.08) as f64 + hidden, excess: 0.0, sigma: RANGE_SIGMA });
                }
            }
        }
        let f = fit(&names, &design, &l, 2.0, &[]).unwrap();
        let (a1, t) = (&f.anchors[0], real[0]);
        let off = ((a1.fitted[0] - t[0]).powi(2) + (a1.fitted[2] - t[2]).powi(2)).sqrt();
        assert!(off < 0.2, "{off} m off: {a1:?}");
        assert!(a1.sigma_m < 0.2, "{a1:?}");
        assert_eq!(f.outliers, 2, "the two blocked pairs");
        assert_eq!((f.ranges, f.pairs), (140, 28));
        assert!(f.rms_after_m < 0.15, "{}", f.rms_after_m);
    }

    #[test]
    fn repeating_a_pair_teaches_nothing_new() {
        let (names, design) = ring();
        let mut rng = Rng::new(9);
        let once: Vec<Link> = links(&design, &[], true, &mut rng).into_iter().take(15).collect();
        let many: Vec<Link> = once.iter().cycle().take(150).cloned().collect();
        let (a, b) = (fit(&names, &design, &once, 2.0, &[]).unwrap(), fit(&names, &design, &many, 2.0, &[]).unwrap());
        assert_eq!(a.anchors[0].sigma_m, b.anchors[0].sigma_m);
    }

    #[test]
    fn an_anchor_heard_by_two_others_isnt_pinned_down_however_vague_the_prior() {
        let (names, design) = ring();
        let mut rng = Rng::new(4);
        let mut l = links(&design, &[], true, &mut rng);
        // A6 keeps only two partners, which leave it a mirror image to choose between.
        l.retain(|x| (x.a != 5 && x.b != 5) || x.a == 0 || x.a == 1);
        let f = fit(&names, &design, &l, 50.0, &[]).unwrap();
        assert!(f.not_pinned_down.contains(&"A6".to_string()), "{:?}", f.not_pinned_down);
    }

    #[test]
    fn the_held_out_check_rewards_a_real_move_and_nothing_else() {
        let (names, design) = ring();
        let mut real = design.clone();
        real[1][0] -= 1.0;
        real[1][2] += 0.6;
        let (before, after) = held_out(&names, &design, &links(&real, &[], true, &mut Rng::new(2)), 2.0, &[]).unwrap();
        assert!(worth_applying(before, after), "{before} -> {after}");
        // Where the design was right, the fit has nothing to add.
        let wrongly = (0..8)
            .filter(|seed| {
                let (before, after) = held_out(&names, &design, &links(&design, &[], true, &mut Rng::new(*seed)), 2.0, &[]).unwrap();
                worth_applying(before, after)
            })
            .count();
        assert!(wrongly <= 1, "{wrongly} of 8");
    }

    #[test]
    fn ranges_fix_the_shape_and_surveyed_anchors_fix_where_it_sits() {
        let (names, design) = ring();
        let mut rng = Rng::new(5);
        let l = links(&design, &[], true, &mut rng);
        let free = fit(&names, &design, &l, 2.0, &[]).unwrap();
        for a in &free.anchors {
            assert!(a.sigma_m < 0.1, "well placed against the others: {a:?}");
            assert!(a.absolute_sigma_m > 0.4, "not in the building, with nothing to tie it: {a:?}");
        }
        let tied = fit(&names, &design, &l, 2.0, &["A1".to_string(), "A3".to_string()]).unwrap();
        for a in &tied.anchors {
            assert!(a.absolute_sigma_m < 0.2, "two surveyed anchors tie the set down: {a:?}");
        }
    }

    #[test]
    fn an_anchor_nobody_hears_stays_where_it_was_and_says_so() {
        let (names, design) = ring();
        let mut rng = Rng::new(3);
        let mut l = links(&design, &[], true, &mut rng);
        l.retain(|x| x.a != 5 && x.b != 5);
        let f = fit(&names, &design, &l, 2.0, &[]).unwrap();
        assert!(f.not_pinned_down.contains(&"A6".to_string()), "{:?}", f.not_pinned_down);
        assert!(f.anchors[5].moved_m < 0.01, "{:?}", f.anchors[5]);
    }

    #[test]
    fn too_few_ranges_and_bad_files_get_a_message() {
        let (names, design) = ring();
        assert!(fit(&names, &design, &[], 1.0, &[]).is_err());
        let r = parse("a,b,range_m\nA1,A2,5.5\n").unwrap();
        assert_eq!((r[0].a.as_str(), r[0].range_m), ("A1", 5.5));
        assert!(parse("A1,A2").unwrap_err().to_string().contains("line 1"));
        assert!(parse("A1,A2,nan").is_err());
        assert!(parse("").is_err());
        // A real pair that happens to start like the header isn't skipped.
        assert_eq!(parse("A,B1,5.5").unwrap()[0].b, "B1");
        assert!(parse("A1,A2,-1").is_err());
        assert!(parse("# note\n\nA1,A2").unwrap_err().to_string().contains("line 3"));
        let j = parse(r#"{"a":"A1","b":"A2","range_m":3.0}"#).unwrap();
        assert!(!j[0].synthetic);
    }
}
