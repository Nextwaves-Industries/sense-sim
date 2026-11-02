//! Is the site still the site in the model? Live ranges are compared with what the
//! model expects. A device whose ranges are off by a direction-dependent amount has
//! moved; off by the same amount everywhere, its delay changed; ranges going long
//! only along lines through one place mean something new is standing there.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// One live range with the tag's estimated position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub device: String,
    pub tag: [f32; 3],
    pub range_m: f32,
    /// Excess the model already expects on this link (known walls), metres.
    #[serde(default)]
    pub expected_excess_m: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Finding {
    /// Reports nothing, though it's expected to.
    Silent { device: String },
    /// Its ranges say it's somewhere else.
    Moved { device: String, by_m: [f32; 3], distance_m: f32, samples: usize },
    /// Every range off by the same amount: delay, firmware or a swapped unit.
    DelayChanged { device: String, by_m: f32, samples: usize },
    /// Ranges through one place run long, from several devices.
    NewObstruction { at: [f32; 2], extra_m: f32, lines: usize, devices: Vec<String> },
    /// Ranges disagree with the model, in no pattern this check recognises. Worth a
    /// look: "no finding" must not read as "all is well".
    Unexplained { device: String, rms_m: f32, samples: usize },
}

impl Finding {
    pub fn summary(&self) -> String {
        match self {
            Finding::Silent { device } => format!("{device} has stopped reporting. Check its power and network."),
            Finding::Moved { device, by_m, distance_m, samples } => format!("{device} seems to have moved about {:.0} cm (x {:+.2}, z {:+.2} m), from {samples} ranges. Check its mount, then re-survey or update its position.", distance_m * 100.0, by_m[0], by_m[2]),
            Finding::DelayChanged { device, by_m, samples } => format!("{device}'s ranges all read {:+.0} cm off, from {samples} ranges. Was it replaced or updated? Recalibrate its antenna delay.", by_m * 100.0),
            Finding::NewObstruction { at, extra_m, lines, devices } => format!("Something new near x {:.1}, z {:.1}: {lines} lines through it run about {:.0} cm long, from {}. Look for new racks, stock or machinery.", at[0], at[1], extra_m * 100.0, devices.join(", ")),
            Finding::Unexplained { device, rms_m, samples } => format!("{device}'s ranges differ from the model by {:.0} cm RMS over {samples} ranges, in no pattern this check knows. Worth a look.", rms_m * 100.0),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DriftReport {
    pub findings: Vec<Finding>,
    pub samples: usize,
    /// RMS difference from the model per device, metres.
    pub residual_rms: Vec<(String, f32)>,
    /// Devices with too few ranges to judge. A check that couldn't look isn't a pass.
    pub too_few_samples: Vec<String>,
    pub healthy: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DriftOptions {
    /// Range noise when all is well, metres.
    pub noise_m: f32,
    /// Don't report a shift below this, metres.
    pub min_shift_m: f32,
    /// Fewest samples per device to say anything.
    pub min_samples: usize,
    /// Size of the map cells for finding obstructions, metres.
    pub cell_m: f32,
    /// Fewest separate tag spots per device to say anything: a tag standing still sees
    /// one fixed reflection however many ranges it takes, which looks like a changed delay.
    pub min_spots: usize,
}

impl Default for DriftOptions {
    fn default() -> Self {
        Self { noise_m: 0.12, min_shift_m: 0.15, min_samples: 20, cell_m: 1.0, min_spots: 5 }
    }
}

/// How often it cries wolf, as measured by `the_false_alarm_rate_is_what_the_tool_says`.
pub const FALSE_ALARMS: &str = "On simulated healthy sites with independent range noise at the assumed level, under 1 % of checks raised a finding. Real noise is correlated and multipath is not simulated noise, so the real rate isn't known.";

/// Separate floor cells the samples of one device were taken from.
fn spots(list: &[(&Sample, f32, [f32; 3])], cell: f32) -> usize {
    let mut seen: Vec<(i32, i32)> = list.iter().map(|(s, _, _)| ((s.tag[0] / cell).floor() as i32, (s.tag[2] / cell).floor() as i32)).collect();
    seen.sort();
    seen.dedup();
    seen.len()
}

/// Where along the floor a device must really be to explain these residuals:
/// Gauss–Newton on the true distances, since a move can be large next to a nearby
/// tag. Returns (x, 0, z).
fn fit_move(list: &[(&Sample, f32, [f32; 3])], p: [f32; 3]) -> [f32; 3] {
    let (mut dx, mut dz) = (0f32, 0f32);
    for _ in 0..8 {
        let (mut a, mut b, mut c, mut gx, mut gz) = (0f32, 0f32, 0f32, 0f32, 0f32);
        for (s, r, _) in list {
            let d0 = ((s.tag[0] - p[0]).powi(2) + (s.tag[1] - p[1]).powi(2) + (s.tag[2] - p[2]).powi(2)).sqrt();
            let (ex, ey, ez) = (s.tag[0] - p[0] - dx, s.tag[1] - p[1], s.tag[2] - p[2] - dz);
            let d = (ex * ex + ey * ey + ez * ez).sqrt().max(1e-3);
            let e = r - (d - d0);
            let (jx, jz) = (-ex / d, -ez / d);
            a += jx * jx;
            b += jx * jz;
            c += jz * jz;
            gx += jx * e;
            gz += jz * e;
        }
        // A touch of damping keeps a direction nobody sampled at zero.
        let n = list.len() as f32 * 1e-3;
        let (a, c) = (a + n, c + n);
        let det = a * c - b * b;
        if det.abs() < 1e-6 {
            break;
        }
        let (sx, sz) = ((c * gx - b * gz) / det, (a * gz - b * gx) / det);
        dx += sx;
        dz += sz;
        if sx.abs() + sz.abs() < 1e-3 {
            break;
        }
    }
    [dx, 0.0, dz]
}

fn floor_cells(a: [f32; 3], b: [f32; 3], cell: f32) -> Vec<(i32, i32)> {
    let d = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
    let n = (d / (cell * 0.5)).ceil().max(1.0) as usize;
    let mut out: Vec<(i32, i32)> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            (((a[0] + (b[0] - a[0]) * t) / cell).floor() as i32, ((a[2] + (b[2] - a[2]) * t) / cell).floor() as i32)
        })
        .collect();
    out.dedup();
    out
}

/// Compares `samples` with the devices' positions on record.
pub fn analyse(samples: &[Sample], devices: &BTreeMap<String, [f32; 3]>, o: DriftOptions) -> DriftReport {
    let mut findings = Vec::new();
    let mut per: BTreeMap<&str, Vec<(&Sample, f32, [f32; 3])>> = BTreeMap::new();
    for s in samples {
        let Some(p) = devices.get(&s.device) else { continue };
        let d = [s.tag[0] - p[0], s.tag[1] - p[1], s.tag[2] - p[2]];
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
        let u = [d[0] / dist, d[1] / dist, d[2] / dist];
        per.entry(s.device.as_str()).or_default().push((s, s.range_m - dist - s.expected_excess_m, u));
    }
    for name in devices.keys() {
        if !per.contains_key(name.as_str()) {
            findings.push(Finding::Silent { device: name.clone() });
        }
    }
    let mut residual_rms = Vec::new();
    let mut explained: HashMap<&str, ()> = HashMap::new();
    for (name, list) in &per {
        let n = list.len();
        let rms = (list.iter().map(|x| x.1 * x.1).sum::<f32>() / n as f32).sqrt();
        residual_rms.push((name.to_string(), rms));
        if n < o.min_samples || spots(list, o.cell_m) < o.min_spots {
            continue;
        }
        let p = devices[*name];
        let sse_none: f32 = list.iter().map(|x| x.1 * x.1).sum();
        // The same error on every range: a delay.
        let offset = list.iter().map(|x| x.1).sum::<f32>() / n as f32;
        let sse_delay: f32 = list.iter().map(|x| (x.1 - offset).powi(2)).sum();
        // The device really at p + δ along the floor. A wall mount slides or gets
        // knocked sideways; from one corner, height and delay look almost the same,
        // so they aren't mixed into this.
        let by = fit_move(list, p);
        let shifted = |tag: [f32; 3]| -> f32 { ((tag[0] - p[0] - by[0]).powi(2) + (tag[1] - p[1]).powi(2) + (tag[2] - p[2] - by[2]).powi(2)).sqrt() };
        let sse_move: f32 = list.iter().map(|(s, r, u)| {
            let dist = (s.tag[0] - p[0]) * u[0] + (s.tag[1] - p[1]) * u[1] + (s.tag[2] - p[2]) * u[2];
            (r - (shifted(s.tag) - dist)).powi(2)
        }).sum();
        let moved = (by[0] * by[0] + by[2] * by[2]).sqrt();
        // An explanation must beat "nothing changed" by far more than noise could, and
        // must leave little behind: anything else isn't this device's doing.
        let noise = o.noise_m * o.noise_m;
        let beats = |better: f32, params: f32| (sse_none - better) > 20.0 * params * noise;
        let fits = |sse: f32| sse / n as f32 <= 4.0 * noise;
        let move_ok = moved >= o.min_shift_m && beats(sse_move, 2.0) && fits(sse_move);
        let delay_ok = offset.abs() >= o.min_shift_m && beats(sse_delay, 1.0) && fits(sse_delay);
        // Both fit: believe the one that fits better, by a clear margin for the
        // extra parameter it uses.
        if move_ok && (!delay_ok || sse_move + 2.0 * noise < sse_delay) {
            findings.push(Finding::Moved { device: name.to_string(), by_m: by, distance_m: moved, samples: n });
            explained.insert(name, ());
        } else if delay_ok {
            findings.push(Finding::DelayChanged { device: name.to_string(), by_m: offset, samples: n });
            explained.insert(name, ());
        }
    }
    // Long ranges not explained above: vote for where their lines cross.
    let mut crossings: HashMap<(i32, i32), (usize, usize, f32, Vec<String>)> = HashMap::new();
    let threshold = (o.min_shift_m).max(3.0 * o.noise_m);
    for (name, list) in &per {
        // Lines from one spot all cross at that spot: a device seen from too few says nothing.
        if explained.contains_key(name) || spots(list, o.cell_m) < o.min_spots {
            continue;
        }
        let p = devices[*name];
        for (s, r, _) in list {
            let bad = *r > threshold;
            for c in floor_cells(s.tag, p, o.cell_m) {
                let e = crossings.entry(c).or_default();
                e.0 += 1;
                if bad {
                    e.1 += 1;
                    e.2 += *r;
                    if !e.3.iter().any(|d| d == name) {
                        e.3.push(name.to_string());
                    }
                }
            }
        }
    }
    if let Some((cell, c)) = crossings.iter().filter(|(_, c)| c.3.len() >= 2 && c.1 >= 8 && c.1 as f32 / c.0 as f32 >= 0.7).max_by(|a, b| {
        let score = |c: &(usize, usize, f32, Vec<String>)| c.1 as f32 * c.1 as f32 / c.0 as f32;
        score(a.1).total_cmp(&score(b.1)).then(a.0.cmp(b.0))
    }) {
        let mut devs = c.3.clone();
        devs.sort();
        for d in &devs {
            explained.insert(per.get_key_value(d.as_str()).map_or("", |k| *k.0), ());
        }
        findings.push(Finding::NewObstruction { at: [(cell.0 as f32 + 0.5) * o.cell_m, (cell.1 as f32 + 0.5) * o.cell_m], extra_m: c.2 / c.1 as f32, lines: c.1, devices: devs });
    }
    // Far from the model with nothing to show for it: say so, don't call it healthy. The
    // limit is where a move or a delay stops being accepted as the explanation, so nothing
    // falls between them. A noisier device than assumed lands here too.
    let limit = (2.0 * o.noise_m).max(o.min_shift_m);
    let judged = |l: &[(&Sample, f32, [f32; 3])]| l.len() >= o.min_samples && spots(l, o.cell_m) >= o.min_spots;
    for (name, rms) in &residual_rms {
        let list = per.get(name.as_str()).map_or(&[][..], Vec::as_slice);
        if judged(list) && *rms > limit && !explained.contains_key(name.as_str()) {
            findings.push(Finding::Unexplained { device: name.clone(), rms_m: *rms, samples: list.len() });
        }
    }
    let too_few_samples: Vec<String> = per.iter().filter(|(_, l)| !judged(l)).map(|(n, _)| n.to_string()).collect();
    let healthy = findings.is_empty() && too_few_samples.is_empty();
    DriftReport { findings, samples: samples.len(), residual_rms, too_few_samples, healthy }
}

/// Reads live ranges from a JSON-lines file of [`Sample`].
pub fn parse_samples(text: &str) -> anyhow::Result<Vec<Sample>> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).enumerate().map(|(i, l)| serde_json::from_str(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1))).collect()
}

/// The layout as designed, to compare against.
#[derive(bevy::prelude::Resource, Default)]
pub struct DriftState {
    pub baseline: BTreeMap<String, [f32; 3]>,
    pub last: Option<DriftReport>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::rng::Rng;

    fn anchors() -> BTreeMap<String, [f32; 3]> {
        [("A1", [0.0, 3.0, 0.0]), ("A2", [20.0, 3.0, 0.0]), ("A3", [20.0, 3.0, 20.0]), ("A4", [0.0, 3.0, 20.0])].into_iter().map(|(n, p)| (n.to_string(), p)).collect()
    }

    /// Walks tags over the floor and reads ranges from where devices really are.
    fn walk(real: &BTreeMap<String, [f32; 3]>, extra: impl Fn(&str, [f32; 3], f32) -> f32, n: usize) -> Vec<Sample> {
        let mut rng = Rng::new(7);
        let mut out = Vec::new();
        for i in 0..n {
            let tag = [(i * 7 % 20) as f32 + 0.3, 1.2, (i * 13 % 20) as f32 + 0.3];
            for (name, p) in real {
                let d = ((tag[0] - p[0]).powi(2) + (tag[1] - p[1]).powi(2) + (tag[2] - p[2]).powi(2)).sqrt();
                out.push(Sample { device: name.clone(), tag, range_m: d + extra(name, tag, d) + rng.gauss(0.1), expected_excess_m: 0.0 });
            }
        }
        out
    }

    #[test]
    fn a_healthy_site_reports_nothing() {
        let a = anchors();
        let r = analyse(&walk(&a, |_, _, _| 0.0, 200), &a, DriftOptions::default());
        assert!(r.healthy, "{:?}", r.findings);
        assert!(r.residual_rms.iter().all(|x| x.1 < 0.15), "{:?}", r.residual_rms);
    }

    #[test]
    fn a_moved_anchor_is_found_with_how_far_and_which_way() {
        let design = anchors();
        let mut real = design.clone();
        real.get_mut("A2").unwrap()[0] -= 1.0;
        real.get_mut("A2").unwrap()[2] += 0.5;
        let r = analyse(&walk(&real, |_, _, _| 0.0, 300), &design, DriftOptions::default());
        println!("{:#?}", r.findings);
        let m = r.findings.iter().find_map(|f| match f {
            Finding::Moved { device, by_m, .. } if device == "A2" => Some(*by_m),
            _ => None,
        });
        let by = m.unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!((by[0] + 1.0).abs() < 0.2 && (by[2] - 0.5).abs() < 0.2, "{by:?}");
        assert!(!r.findings.iter().any(|f| matches!(f, Finding::Moved { device, .. } if device != "A2")), "{:?}", r.findings);
    }

    #[test]
    fn a_changed_delay_is_told_apart_from_a_move() {
        let a = anchors();
        let r = analyse(&walk(&a, |n, _, _| if n == "A3" { 0.4 } else { 0.0 }, 300), &a, DriftOptions::default());
        println!("{:#?}", r.findings);
        let f = r.findings.iter().find(|f| matches!(f, Finding::DelayChanged { device, .. } if device == "A3"));
        match f {
            Some(Finding::DelayChanged { by_m, .. }) => assert!((by_m - 0.4).abs() < 0.08, "{by_m}"),
            _ => panic!("{:?}", r.findings),
        }
        assert!(!r.findings.iter().any(|f| matches!(f, Finding::Moved { .. })), "{:?}", r.findings);
    }

    #[test]
    fn a_silent_device_is_named() {
        let design = anchors();
        let mut real = design.clone();
        real.remove("A4");
        let r = analyse(&walk(&real, |_, _, _| 0.0, 100), &design, DriftOptions::default());
        assert!(r.findings.contains(&Finding::Silent { device: "A4".into() }), "{:?}", r.findings);
        assert!(!r.healthy);
    }

    #[test]
    fn a_new_obstruction_is_located_where_the_long_lines_cross() {
        let a = anchors();
        // A block near (10, 10): lines through it run 60 cm long.
        let block = |name: &str, tag: [f32; 3], _d: f32| -> f32 {
            let p = a[name];
            let (dx, dz) = (tag[0] - p[0], tag[2] - p[2]);
            let t = (((10.0 - p[0]) * dx + (10.0 - p[2]) * dz) / (dx * dx + dz * dz).max(1e-6)).clamp(0.0, 1.0);
            let (cx, cz) = (p[0] + t * dx, p[2] + t * dz);
            if ((cx - 10.0).powi(2) + (cz - 10.0).powi(2)).sqrt() < 1.5 { 0.6 } else { 0.0 }
        };
        let r = analyse(&walk(&a, block, 400), &a, DriftOptions::default());
        println!("{:#?}", r.findings);
        let at = r.findings.iter().find_map(|f| match f {
            Finding::NewObstruction { at, devices, .. } => {
                assert!(devices.len() >= 2);
                Some(*at)
            }
            _ => None,
        });
        let at = at.unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!((at[0] - 10.0).abs() < 2.5 && (at[1] - 10.0).abs() < 2.5, "{at:?}");
    }

    #[test]
    fn disagreement_with_no_pattern_is_never_called_healthy() {
        // Large, random errors on one anchor: no move, delay or obstruction explains them.
        let a = anchors();
        let mut rng = Rng::new(11);
        let mut s = walk(&a, |_, _, _| 0.0, 200);
        for x in s.iter_mut().filter(|x| x.device == "A1") {
            x.range_m += rng.gauss(1.5);
        }
        let r = analyse(&s, &a, DriftOptions::default());
        assert!(!r.healthy, "{r:?}");
        assert!(r.findings.iter().any(|f| matches!(f, Finding::Unexplained { device, .. } if device == "A1")), "{:?}", r.findings);
    }

    #[test]
    fn known_walls_are_not_a_finding() {
        let a = anchors();
        let mut s = walk(&a, |_, _, _| 0.5, 200);
        s.iter_mut().for_each(|x| x.expected_excess_m = 0.5);
        assert!(analyse(&s, &a, DriftOptions::default()).healthy);
    }

    #[test]
    fn too_few_samples_says_nothing_rather_than_guessing() {
        let design = anchors();
        let mut real = design.clone();
        real.get_mut("A2").unwrap()[0] -= 2.0;
        let r = analyse(&walk(&real, |_, _, _| 0.0, 3), &design, DriftOptions::default());
        assert!(!r.findings.iter().any(|f| matches!(f, Finding::Moved { .. })), "{:?}", r.findings);
        assert!(!r.healthy, "too little data must not read as healthy");
        assert_eq!(r.too_few_samples.len(), 4);
    }

    fn noisy_walk(real: &BTreeMap<String, [f32; 3]>, sigma: f32, shift: f32, seed: u64, n: usize) -> Vec<Sample> {
        let mut rng = Rng::new(seed);
        let mut out = Vec::new();
        for i in 0..n {
            let tag = [(i * 7 % 20) as f32 + 0.3, 1.2, (i * 13 % 20) as f32 + 0.3];
            for (name, p) in real {
                let d = ((tag[0] - p[0]).powi(2) + (tag[1] - p[1]).powi(2) + (tag[2] - p[2]).powi(2)).sqrt();
                out.push(Sample { device: name.clone(), tag, range_m: d + rng.gauss(sigma) + if name == "A1" { shift } else { 0.0 }, expected_excess_m: 0.0 });
            }
        }
        out
    }

    // OUT-M12: 0.35 m of noise read as healthy, under the old limit of 0.36.
    #[test]
    fn a_device_noisier_than_assumed_is_not_healthy() {
        let a = anchors();
        let r = analyse(&noisy_walk(&a, 0.35, 0.0, 3, 300), &a, DriftOptions::default());
        assert!(!r.healthy, "{r:?}");
        assert!(r.findings.iter().any(|f| matches!(f, Finding::Unexplained { .. })), "{:?}", r.findings);
        // Told that this system is that noisy, it isn't a finding.
        let told = DriftOptions { noise_m: 0.35, ..Default::default() };
        assert!(analyse(&noisy_walk(&a, 0.35, 0.0, 3, 300), &a, told).healthy);
    }

    // OUT-M12: 15 cm of delay at σ = 0.30 fell between "explained" (RMS under 0.24) and
    // "unexplained" (RMS over 0.36), and gave no finding at all.
    #[test]
    fn a_shift_in_noise_doesnt_fall_between_the_two_limits() {
        let a = anchors();
        let r = analyse(&noisy_walk(&a, 0.30, 0.15, 5, 300), &a, DriftOptions::default());
        let rms = r.residual_rms.iter().find(|x| x.0 == "A1").unwrap().1;
        assert!(rms > 0.24 && rms < 0.36, "this is the dead zone: {rms}");
        assert!(!r.healthy, "{r:?}");
    }

    // OUT-M12: a static tag's fixed multipath bias looked like 20 independent readings.
    #[test]
    fn a_tag_standing_still_can_say_nothing_about_a_delay() {
        let a = anchors();
        let tag = [7.3, 1.2, 9.3];
        let samples: Vec<Sample> = a.iter().flat_map(|(name, p)| {
            let d = ((tag[0] - p[0]).powi(2) + (tag[1] - p[1]).powi(2) + (tag[2] - p[2]).powi(2)).sqrt();
            (0..50).map(move |k| Sample { device: name.clone(), tag, range_m: d + 0.3 + (k % 2) as f32 * 0.01, expected_excess_m: 0.0 })
        }).collect();
        let r = analyse(&samples, &a, DriftOptions::default());
        assert!(!r.findings.iter().any(|f| matches!(f, Finding::DelayChanged { .. } | Finding::Moved { .. } | Finding::NewObstruction { .. })), "{:?}", r.findings);
        assert!(!r.healthy, "and it isn't a pass either");
        assert_eq!(r.too_few_samples.len(), 4, "{r:?}");
    }

    #[test]
    fn the_false_alarm_rate_is_what_the_tool_says() {
        let a = anchors();
        let (mut alarms, runs) = (0, 300);
        for seed in 0..runs {
            if !analyse(&noisy_walk(&a, 0.12, 0.0, seed + 100, 60), &a, DriftOptions::default()).healthy {
                alarms += 1;
            }
        }
        println!("{alarms} of {runs} healthy sites raised a finding");
        assert!((alarms as f64) < 0.01 * runs as f64, "{alarms} of {runs}: update FALSE_ALARMS");
    }

    #[test]
    fn samples_read_from_json_lines() {
        let s = parse_samples("{\"device\":\"A1\",\"tag\":[1,2,3],\"range_m\":4.5}\n\n").unwrap();
        assert_eq!(s[0].range_m, 4.5);
        assert!(parse_samples("nope").unwrap_err().to_string().contains("line 1"));
    }
}
