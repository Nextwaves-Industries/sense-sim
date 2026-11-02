//! Agreeing what "working" means before the install, and proving it after. A contract
//! number like "30 cm at 95 %" needs a test both sides accept: how many points, where,
//! and what result counts. The statistics set the point count, so a pass means something:
//! 59 points all within the limit shows 95 % at 95 % confidence, and fewer can't.

use anyhow::{Result, bail};
use bevy::prelude::*;
pub use proof::stats::points_needed;
use proof::stats::shown_share;
use proof::{Claim, Evidence, Prediction, Runs, Target, Verdict};
use serde::{Deserialize, Serialize};

use super::stress::Start;
use crate::rtls::device::{Device, DeviceKind, Motion, Zone};
use crate::rtls::io::Hub;
use crate::rtls::rng::Rng;
use crate::rtls::sim::{OccluderParts, Occluders};
use crate::rtls::{RtlsConfig, RtlsState, sim};
use crate::scene::PlacedObject;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestPoint {
    pub id: String,
    pub at: [f32; 3],
    pub zone: Option<String>,
}

/// What both sides agree to before anything is installed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptancePlan {
    pub scene: String,
    pub tech: String,
    pub target_m: f32,
    pub pass_share: f64,
    pub confidence: f64,
    pub points: Vec<TestPoint>,
    /// Drew the points, so the draw can be checked.
    #[serde(default)]
    pub seed: u64,
}

impl AcceptancePlan {
    /// Changes if anything in the plan does.
    pub fn fingerprint(&self) -> String {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for b in serde_json::to_string(self).unwrap_or_default().bytes() {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}

/// The plan in force, fixed before anything is tested.
#[derive(Resource, Default)]
pub struct AcceptanceState {
    pub plan: Option<AcceptancePlan>,
}

/// `n` spots drawn at random, spread out: the free floor is split into `n` patches of about
/// equal size, and one spot is drawn from each. The statistics only hold for a random draw,
/// so hand-picked or "evenly spread" spots wouldn't show anything about the rest of the floor;
/// drawing from equal patches keeps it random, and can't leave a whole area untested.
pub fn stratified_pick(free: &[Vec3], n: usize, rng: &mut Rng) -> Vec<Vec3> {
    fn split(spots: &mut [Vec3], n: usize, rng: &mut Rng, out: &mut Vec<Vec3>) {
        if n == 0 || spots.is_empty() {
            return;
        }
        if n == 1 {
            let k = ((rng.uniform() * spots.len() as f32) as usize).min(spots.len() - 1);
            out.push(spots[k]);
            return;
        }
        let (lo, hi) = spots.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
        let wide_x = hi.x - lo.x >= hi.z - lo.z;
        spots.sort_by(|a, b| if wide_x { a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)) } else { a.z.total_cmp(&b.z).then(a.x.total_cmp(&b.x)) });
        let first = n / 2;
        let at = (spots.len() * first / n).clamp(1, spots.len() - 1);
        let (a, b) = spots.split_at_mut(at);
        split(a, first, rng, out);
        split(b, n - first, rng, out);
    }
    let mut spots = free.to_vec();
    let mut out = Vec::with_capacity(n);
    split(&mut spots, n.min(free.len()), rng, &mut out);
    out
}

/// The middle of each square of the floor. All inside the site: a spot past its walls is in no
/// object's box, so it would pass as free floor, and nobody could walk to it.
pub fn floor_spots(lo: Vec3, hi: Vec3, step: f32, height: f32) -> Vec<Vec3> {
    let (nx, nz) = (((hi.x - lo.x) / step).ceil().max(1.0) as usize, ((hi.z - lo.z) / step).ceil().max(1.0) as usize);
    let mut out = Vec::with_capacity(nx * nz);
    for ix in 0..nx {
        for iz in 0..nz {
            let p = Vec3::new(lo.x + step * (ix as f32 + 0.5), height, lo.z + step * (iz as f32 + 0.5));
            if p.x <= hi.x && p.z <= hi.z {
                out.push(p);
            }
        }
    }
    out
}

/// Draws `n` points, on a finer grid if a small site needs one to fit them.
pub fn draw(world: &mut World, n: usize, height: f32, mut step: f32, seed: u64) -> Result<Vec<TestPoint>, String> {
    loop {
        let got = world.run_system_cached_with(plan_system, (n, height, step, seed)).map_err(|e| e.to_string())?;
        if got.len() >= n || step <= 0.3 {
            return Ok(got);
        }
        step /= 2.0;
    }
}

/// The test points, on the free floor at this height.
pub fn plan_system(
    In((n, height, step, seed)): In<(usize, f32, f32, u64)>,
    parts: OccluderParts,
    objects: Query<(Entity, &PlacedObject, &Transform), Without<Device>>,
    devices: Query<&Transform, With<Device>>,
    zones: Query<&Zone>,
) -> Vec<TestPoint> {
    let occ = Occluders::build(&parts, &objects);
    let bounds = occ.bounds().or_else(|| devices.iter().map(|t| (t.translation, t.translation)).reduce(|a, b| (a.0.min(b.0), a.1.max(b.1))));
    let Some((lo, hi)) = bounds else { return Vec::new() };
    let free: Vec<Vec3> = floor_spots(lo, hi, step, height).into_iter().filter(|p| !occ.inside(*p)).collect();
    let zone_of = |p: Vec3| zones.iter().find(|z| z.contains(p)).map(|z| z.name.clone());
    stratified_pick(&free, n, &mut Rng::new(seed)).iter().enumerate().map(|(k, p)| TestPoint { id: format!("P{:03}", k + 1), at: p.to_array(), zone: zone_of(*p) }).collect()
}

/// One position reported at a test point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointResult {
    pub id: String,
    /// Where the system said the tag was.
    #[serde(default)]
    pub reported: Option<[f32; 3]>,
    /// Or the error directly, if that's what was logged.
    #[serde(default)]
    pub error_m: Option<f32>,
    /// From the simulator, not a walk.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub simulated: bool,
}

pub fn parse_results(text: &str) -> Result<Vec<PointResult>> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate().map(|(i, l)| (i, l.trim())).filter(|(_, l)| !l.is_empty() && !l.starts_with('#')) {
        if l.starts_with('{') {
            out.push(serde_json::from_str::<PointResult>(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1))?);
            continue;
        }
        if l.to_ascii_lowercase().starts_with("id,") {
            continue;
        }
        let p: Vec<&str> = l.split(',').map(str::trim).collect();
        let num = |s: &str| s.parse::<f32>().ok().filter(|v| v.is_finite());
        match p.as_slice() {
            [id, e] => out.push(PointResult { id: (*id).into(), reported: None, error_m: Some(num(e).filter(|v| *v >= 0.0).ok_or_else(|| anyhow::anyhow!("line {}: error '{e}' isn't a distance", i + 1))?), simulated: false }),
            [id, x, y, z] => match (num(x), num(y), num(z)) {
                (Some(x), Some(y), Some(z)) => out.push(PointResult { id: (*id).into(), reported: Some([x, y, z]), error_m: None, simulated: false }),
                _ => bail!("line {}: a position isn't made of numbers", i + 1),
            },
            _ => bail!("line {}: expected id,x,y,z or id,error_m", i + 1),
        }
    }
    if out.is_empty() {
        bail!("no results in the file");
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointScore {
    pub id: String,
    pub zone: Option<String>,
    /// `None`: no position reported, which fails.
    pub error_m: Option<f32>,
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceScore {
    pub tested: usize,
    pub missing: Vec<String>,
    pub passed: usize,
    pub shown_share: f64,
    pub p95_m: Option<f64>,
    pub verdict: String,
    pub accepted: bool,
    /// From the simulator: a preview, never an acceptance.
    pub simulated: bool,
    /// Who measured these and how. Without it nothing here counts as measured: a file of
    /// numbers doesn't say where it came from.
    pub source: Option<String>,
    /// Results for points the plan doesn't have.
    pub unknown_points: Vec<String>,
    pub points: Vec<PointScore>,
}

pub fn score(plan: &AcceptancePlan, results: &[PointResult], source: Option<&str>) -> AcceptanceScore {
    let simulated = results.iter().any(|r| r.simulated);
    let source = source.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let unknown_points: Vec<String> = results.iter().filter(|r| !plan.points.iter().any(|p| p.id == r.id)).map(|r| r.id.clone()).collect();
    let mut points = Vec::with_capacity(plan.points.len());
    let mut missing = Vec::new();
    for p in &plan.points {
        let found: Vec<&PointResult> = results.iter().filter(|r| r.id == p.id).collect();
        if found.is_empty() {
            missing.push(p.id.clone());
            continue;
        }
        let at = Vec3::from_array(p.at);
        // The floor plane: most systems report height poorly and it isn't what's bought. The
        // worst of any repeats, so a retest can't hide a fail, and an error that isn't a
        // distance counts as no position at all.
        let error = found.iter().map(|r| r.error_m.or_else(|| r.reported.map(|q| Vec2::new(q[0] - at.x, q[2] - at.z).length())).filter(|e| e.is_finite() && *e >= 0.0)).try_fold(0.0f32, |worst, e| e.map(|e| worst.max(e)));
        points.push(PointScore { id: p.id.clone(), zone: p.zone.clone(), error_m: error, passed: error.is_some_and(|e| e <= plan.target_m) });
    }
    let passed = points.iter().filter(|p| p.passed).count();
    // A point that wasn't tested counts against the result.
    let n = plan.points.len();
    let shown = shown_share(n, passed, plan.confidence);
    let mut errors: Vec<f64> = points.iter().filter_map(|p| p.error_m.map(|e| e as f64)).collect();
    errors.sort_by(f64::total_cmp);
    // Over every point in the plan: one with no position is worse than any error, so if too
    // many are missing there's no 95th percentile to give.
    let p95_m = errors.get(((n as f64 * 0.95).ceil() as usize).saturating_sub(1)).copied();
    let met = shown >= plan.pass_share;
    let accepted = met && !simulated && source.is_some();
    let verdict = if simulated {
        format!("Simulated preview: {} of {n} points pass. This predicts the test; it isn't the test.", passed)
    } else if source.is_none() {
        format!("Not signed off: {} of {n} points pass, but nobody has said who measured these or how, so they don't count as a measurement.", passed)
    } else if met {
        format!("Accepted: {} of {n} points within {:.0} cm shows at least {:.0} % of the floor at {:.0} % confidence.", passed, plan.target_m * 100.0, plan.pass_share * 100.0, plan.confidence * 100.0)
    } else {
        format!("Not shown: {} of {n} points within {:.0} cm shows only {:.1} % of the floor at {:.0} % confidence, against {:.0} % agreed.", passed, plan.target_m * 100.0, shown * 100.0, plan.confidence * 100.0, plan.pass_share * 100.0)
    };
    AcceptanceScore { tested: points.len(), missing, passed, shown_share: shown, p95_m, verdict, accepted, simulated, source, unknown_points, points }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The handover document: what was agreed, what happened, and who signed.
pub fn report_html(plan: &AcceptancePlan, s: &AcceptanceScore) -> String {
    let colour = if s.simulated { "#d17808" } else if s.accepted { "#209b50" } else { "#de3834" };
    let mut rows = String::new();
    for p in &s.points {
        let e = p.error_m.map_or("no position".to_string(), |e| format!("{:.0} cm", e * 100.0));
        rows.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{e}</td><td style=\"color:{}\">{}</td></tr>", esc(&p.id), esc(p.zone.as_deref().unwrap_or("")), if p.passed { "#209b50" } else { "#de3834" }, if p.passed { "Pass" } else { "Fail" }));
    }
    for m in &s.missing {
        rows.push_str(&format!("<tr><td>{}</td><td></td><td>not tested</td><td style=\"color:#de3834\">Fail</td></tr>", esc(m)));
    }
    let banner = if s.simulated { "<p style=\"background:#fff3e0;padding:10px\"><b>Simulated preview.</b> No real measurement stands behind this page, and it can't accept anything.</p>" } else if s.source.is_none() { "<p style=\"background:#fff3e0;padding:10px\"><b>Not a measurement yet.</b> Nobody has said who measured these results or how, so this page can't accept anything.</p>" } else { "" };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Positioning acceptance: {scene}</title><style>body{{font-family:sans-serif;max-width:800px;margin:2em auto;color:#26292e}}table{{border-collapse:collapse;width:100%}}td,th{{border:1px solid #e0e3e7;padding:5px 8px;text-align:left}}</style></head><body><h1>Positioning acceptance: {scene}</h1>{banner}<h2 style=\"color:{colour}\">{verdict}</h2><h3>What was agreed</h3><ul><li>Technology: {tech}</li><li>At least {share:.0} % of the floor within {target:.0} cm, shown at {conf:.0} % confidence</li><li>{n} test points, drawn at random from the free floor before testing (draw {seed})</li><li>Plan fingerprint: {print}</li></ul><h3>What happened</h3><ul><li>Measured by: {source}</li><li>{passed} points passed, {missing} not tested</li><li>Shown: at least {shown:.1} % of the floor</li><li>95th percentile error: {p95}</li></ul><h3>Points</h3><table><tr><th>Point</th><th>Zone</th><th>Error</th><th>Result</th></tr>{rows}</table><h3>Sign-off</h3><p>Tested by: ______________ &nbsp; Date: ________</p><p>Accepted for the customer: ______________ &nbsp; Date: ________</p></body></html>",
        scene = esc(&plan.scene),
        seed = plan.seed,
        print = plan.fingerprint(),
        source = esc(s.source.as_deref().unwrap_or("not stated")),
        verdict = esc(&s.verdict),
        tech = esc(&plan.tech),
        share = plan.pass_share * 100.0,
        target = plan.target_m * 100.0,
        conf = plan.confidence * 100.0,
        n = plan.points.len(),
        passed = s.passed,
        missing = s.missing.len(),
        shown = s.shown_share * 100.0,
        p95 = s.p95_m.map_or("none: too many points have no position".to_string(), |v| format!("{:.0} cm", v * 100.0)),
    )
}

/// The result as a claim: only real results count as measured.
pub fn claim_of(plan: &AcceptancePlan, s: &AcceptanceScore) -> Option<Claim> {
    if s.tested == 0 {
        return None;
    }
    // A failed test still goes on the record, with no percentile to its name.
    let p95 = s.p95_m.unwrap_or(f64::NAN);
    let (evidence, kind) = match (s.simulated, &s.source) {
        (true, _) => (Evidence::Simulated, "preview"),
        (false, Some(_)) => (Evidence::Measured, "test"),
        (false, None) => (Evidence::Estimate, "unattributed"),
    };
    let verdict = if s.shown_share >= plan.pass_share {
        Verdict::Likely
    } else if s.passed == plan.points.len() {
        // Nothing failed; there just weren't enough points to show it.
        Verdict::TooFewRuns
    } else {
        Verdict::Unlikely
    }
    .capped(evidence, false);
    Some(Claim {
        id: format!("acceptance_{kind}:{}", plan.scene),
        recipe: "acceptance".into(),
        subject: format!("Acceptance {kind}, {}", plan.scene),
        prediction: Prediction::exact("95th percentile position error", "m", p95, evidence),
        target: Some(Target { runs: Some(Runs::new(plan.points.len(), s.passed, plan.points.len() - s.points.iter().filter(|p| p.error_m.is_some()).count(), plan.seed)), ..Target::at_most(plan.target_m as f64) }),
        pass_probability: None,
        verdict,
        drivers: Vec::new(),
        assumptions: vec![format!("{} points drawn at random before testing; {} passed, showing at least {:.1} % of the floor at {:.0} % confidence", plan.points.len(), s.passed, s.shown_share * 100.0, plan.confidence * 100.0), format!("Measured by: {}", s.source.as_deref().unwrap_or("not stated"))],
        checks: Vec::new(),
        created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    })
}

/// Stands a tag at each point in the simulator and sees what it reports. This predicts the
/// real test; it isn't one.
pub fn preview(world: &mut World, plan: &AcceptancePlan, settle_s: f64) -> Result<Vec<PointResult>, String> {
    let tag = {
        let mut q = world.query::<(Entity, &Device)>();
        let wanted = match plan.tech.as_str() {
            "ble" => DeviceKind::BleTag,
            "wifi" => DeviceKind::WifiClient,
            _ => DeviceKind::UwbTag,
        };
        let mut v: Vec<(String, Entity)> = q.iter(world).filter(|(_, d)| d.enabled && d.kind == wanted).map(|(e, d)| (d.id.clone(), e)).collect();
        v.sort();
        v.first().map(|x| x.1)
    };
    let Some(tag) = tag else { return Err(format!("the preview needs a {} tag in the scene", plan.tech)) };
    let start = Start::take(world);
    let motion = world.get::<Device>(tag).map(|d| d.motion.clone());
    let was_muted = world.resource::<Hub>().muted;
    world.resource_mut::<Hub>().muted = true;
    if let Some(mut d) = world.get_mut::<Device>(tag) {
        d.motion = Motion::Static;
    }
    let seed = world.resource::<RtlsConfig>().settings.seed;
    let mut out = Vec::with_capacity(plan.points.len());
    for p in &plan.points {
        if let Some(mut t) = world.get_mut::<Transform>(tag) {
            t.translation = Vec3::from_array(p.at);
        }
        world.resource_mut::<RtlsState>().restart(seed);
        sim::run_for(world, settle_s);
        let reported = world.resource::<RtlsState>().tracks.get(&tag).filter(|t| !t.lost).and_then(|t| t.est).map(|e| e.to_array());
        out.push(PointResult { id: p.id.clone(), reported, error_m: None, simulated: true });
    }
    if let (Some(m), Some(mut d)) = (motion, world.get_mut::<Device>(tag)) {
        d.motion = m;
    }
    start.restore(world);
    world.resource_mut::<RtlsState>().restart(seed);
    world.resource_mut::<Hub>().muted = was_muted;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(n: usize) -> AcceptancePlan {
        AcceptancePlan { scene: "site".into(), tech: "uwb".into(), target_m: 0.3, pass_share: 0.95, confidence: 0.95, seed: 1, points: (0..n).map(|k| TestPoint { id: format!("P{k}"), at: [k as f32, 1.2, 0.0], zone: None }).collect() }
    }

    fn scored(p: &AcceptancePlan, r: &[PointResult]) -> AcceptanceScore {
        score(p, r, Some("Total station, J. Doe"))
    }

    fn hits(n: usize, misses: usize) -> Vec<PointResult> {
        (0..n).map(|k| PointResult { id: format!("P{k}"), reported: Some([k as f32 + if k < misses { 1.0 } else { 0.1 }, 1.2, 0.0]), error_m: None, simulated: false }).collect()
    }

    #[test]
    fn all_points_within_the_limit_are_accepted() {
        let s = scored(&plan(59), &hits(59, 0));
        assert!(s.accepted, "{}", s.verdict);
        assert_eq!(s.passed, 59);
        assert!(s.verdict.starts_with("Accepted"), "{}", s.verdict);
    }

    #[test]
    fn one_miss_at_the_minimum_count_fails_and_more_points_can_absorb_it() {
        let s = scored(&plan(59), &hits(59, 1));
        assert!(!s.accepted, "{}", s.verdict);
        assert!(s.verdict.starts_with("Not shown"), "{}", s.verdict);
        // 93 points with one miss does show 95 % at 95 %.
        assert!(scored(&plan(93), &hits(93, 1)).accepted);
    }

    #[test]
    fn a_point_not_tested_counts_against_the_result() {
        let mut r = hits(59, 0);
        r.pop();
        let s = scored(&plan(59), &r);
        assert!(!s.accepted);
        assert_eq!(s.missing, vec!["P58".to_string()]);
        assert!(report_html(&plan(59), &s).contains("not tested"));
    }

    #[test]
    fn a_simulated_run_is_never_an_acceptance() {
        let mut r = hits(59, 0);
        r.iter_mut().for_each(|x| x.simulated = true);
        let s = scored(&plan(59), &r);
        assert!(!s.accepted);
        assert!(s.simulated && s.verdict.contains("isn't the test"), "{}", s.verdict);
        assert!(report_html(&plan(59), &s).contains("Simulated preview"));
        assert_eq!(claim_of(&plan(59), &s).unwrap().prediction.evidence, Evidence::Simulated);
        let real = scored(&plan(59), &hits(59, 0));
        assert_eq!(claim_of(&plan(59), &real).unwrap().prediction.evidence, Evidence::Measured);
    }

    #[test]
    fn results_nobody_vouches_for_arent_a_measurement() {
        for source in [None, Some(""), Some("   ")] {
            let s = score(&plan(59), &hits(59, 0), source);
            assert!(!s.accepted, "{}", s.verdict);
            assert!(s.verdict.starts_with("Not signed off"), "{}", s.verdict);
            assert_eq!(claim_of(&plan(59), &s).unwrap().prediction.evidence, Evidence::Estimate);
            assert_ne!(claim_of(&plan(59), &s).unwrap().verdict, Verdict::Likely);
            assert!(report_html(&plan(59), &s).contains("Not a measurement yet"));
        }
        let ok = scored(&plan(59), &hits(59, 0));
        assert!(report_html(&plan(59), &ok).contains("Total station"));
    }

    #[test]
    fn points_with_no_position_count_in_the_percentile() {
        // 10 of 20 with no position: half the floor failed, whatever the rest did.
        let mut r = hits(20, 0);
        r.iter_mut().take(10).for_each(|x| x.reported = None);
        let s = scored(&plan(20), &r);
        assert_eq!(s.p95_m, None);
        let c = claim_of(&plan(20), &s).unwrap();
        assert!(c.prediction.median.is_nan(), "a failed test stays on the record");
        assert_eq!(c.verdict, Verdict::Unlikely);
        // One of 100 missing doesn't stop there being a 95th percentile.
        let mut r = hits(100, 0);
        r[0].reported = None;
        assert!(scored(&plan(100), &r).p95_m.is_some());
    }

    #[test]
    fn every_spot_on_the_grid_is_inside_the_site() {
        let (lo, hi) = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(20.0, 3.0, 10.0));
        let spots = floor_spots(lo, hi, 2.0, 1.2);
        assert_eq!(spots.len(), 50);
        assert!(spots.iter().all(|p| p.x > lo.x && p.x < hi.x && p.z > lo.z && p.z < hi.z), "{spots:?}");
        // A step that doesn't divide the site evenly stays inside too.
        assert!(floor_spots(lo, hi, 3.0, 1.2).iter().all(|p| p.x <= hi.x && p.z <= hi.z));
    }

    #[test]
    fn a_retest_cant_hide_a_fail_and_a_nonsense_error_is_no_position() {
        let at = |id: &str, e: f32| PointResult { id: id.into(), reported: None, error_m: Some(e), simulated: false };
        let s = scored(&plan(1), &[at("P0", 2.0), at("P0", 0.1)]);
        assert!(!s.points[0].passed, "{:?}", s.points[0]);
        assert_eq!(s.points[0].error_m, Some(2.0));
        let s = scored(&plan(1), &[at("P0", -1.0)]);
        assert_eq!((s.points[0].error_m, s.points[0].passed), (None, false));
        // A line number in an error is the file's, whatever comes before it.
        assert!(parse_results("# note\n\nP0,1,2").unwrap_err().to_string().contains("line 3"));
    }

    #[test]
    fn stray_results_and_negative_errors_dont_slip_through() {
        let mut r = hits(3, 0);
        r.push(PointResult { id: "nope".into(), reported: None, error_m: Some(0.0), simulated: false });
        assert_eq!(scored(&plan(3), &r).unknown_points, vec!["nope".to_string()]);
        assert!(parse_results("P0,-1").is_err());
    }

    #[test]
    fn changing_a_plan_changes_its_fingerprint() {
        let a = plan(3);
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.target_m = 0.5;
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn the_draw_is_random_and_covers_the_floor() {
        let free: Vec<Vec3> = (0..20).flat_map(|x| (0..10).map(move |z| Vec3::new(x as f32, 1.2, z as f32))).collect();
        let pick = |seed: u64, n: usize| stratified_pick(&free, n, &mut Rng::new(seed));
        assert_eq!(pick(1, 16).len(), 16);
        assert_ne!(pick(1, 16), pick(2, 16), "another draw gives other points");
        assert_eq!(pick(1, 16), pick(1, 16), "the same draw can be checked");
        // No part of the floor goes untested: every quadrant gets its share.
        let p = pick(3, 16);
        for (x, z) in [(false, false), (false, true), (true, false), (true, true)] {
            let c = p.iter().filter(|q| (q.x >= 10.0) == x && (q.z >= 5.0) == z).count();
            assert!(c >= 2, "{x} {z}: {c}");
        }
        // And every spot is about as likely as any other, which the statistics rest on.
        let mut seen = vec![0u32; free.len()];
        for seed in 0..4000 {
            for q in pick(seed, 16) {
                seen[free.iter().position(|f| *f == q).unwrap()] += 1;
            }
        }
        let want = 4000.0 * 16.0 / free.len() as f32;
        for (k, c) in seen.iter().enumerate() {
            assert!((*c as f32 / want - 1.0).abs() < 0.35, "spot {k}: {c} against {want}");
        }
        // Asking for more points than there's room for gives what there is.
        assert_eq!(stratified_pick(&free[..5], 16, &mut Rng::new(1)).len(), 5);
    }

    #[test]
    fn height_errors_dont_fail_a_floor_test() {
        let r = vec![PointResult { id: "P0".into(), reported: Some([0.1, 5.0, 0.0]), error_m: None, simulated: false }];
        let s = scored(&plan(1), &r);
        assert!(s.points[0].passed, "{:?}", s.points[0]);
    }

    #[test]
    fn files_parse_in_each_form_and_refuse_nonsense() {
        let r = parse_results("id,x,y,z\nP0,1,2,3\nP1,0.25\n").unwrap();
        assert_eq!(r[0].reported, Some([1.0, 2.0, 3.0]));
        assert_eq!(r[1].error_m, Some(0.25));
        assert!(parse_results("P0,nan").is_err());
        assert!(parse_results("P0,1,2").unwrap_err().to_string().contains("line 1"));
        assert!(parse_results("").is_err());
    }

    #[test]
    fn the_report_escapes_names() {
        let mut p = plan(1);
        p.scene = "<b>x</b>".into();
        let html = report_html(&p, &scored(&p, &[]));
        assert!(html.contains("&lt;b&gt;x") && !html.contains("<b>x"));
    }
}
