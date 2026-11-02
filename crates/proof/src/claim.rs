//! Step 4 of the loop: a prediction, what it rests on, and how it held up. The
//! ledger is what a person reads before committing to a number, so it says
//! plainly when something is an estimate and when it has been checked.

use serde::{Deserialize, Serialize};

use crate::robust::{Direction, RobustReport, Runs, Target};
use crate::stats;

/// Said wherever predictions are read, until measurements say otherwise.
pub const NOT_VALIDATED: &str = "None of these models has been validated on real hardware yet, so treat every number as a prediction until your own measurements back it.";

/// What a number rests on, weakest first. Ordering matters: a claim is only as
/// strong as its weakest input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// A rule of thumb or fitted formula, not solved for this design.
    Estimate,
    /// A full-wave or scene simulation with default materials.
    Simulated,
    /// A simulation whose parameters were fitted to real measurements.
    Calibrated,
    /// Measured on the real thing.
    Measured,
}

impl Evidence {
    pub fn label(self) -> &'static str {
        match self {
            Evidence::Estimate => "Estimate",
            Evidence::Simulated => "Simulated",
            Evidence::Calibrated => "Calibrated to measurements",
            Evidence::Measured => "Measured",
        }
    }
}

/// JSON has no NaN: it's written as `null`. Reading that back as NaN means one
/// failed run can't make a whole ledger unreadable.
fn number_or_nan<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

/// A number with its range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub metric: String,
    pub unit: String,
    #[serde(deserialize_with = "number_or_nan")]
    pub median: f64,
    /// 5th percentile of the runs; the lowest run below 19 of them. It spreads the
    /// assumed inputs and nothing else, so it isn't a promise about measurements.
    #[serde(deserialize_with = "number_or_nan")]
    pub low: f64,
    /// 95th percentile of the runs; the highest run below 19 of them.
    #[serde(deserialize_with = "number_or_nan")]
    pub high: f64,
    pub evidence: Evidence,
}

impl Prediction {
    /// A value with no spread, such as a direct measurement.
    pub fn exact(metric: &str, unit: &str, value: f64, evidence: Evidence) -> Self {
        Self { metric: metric.into(), unit: unit.into(), median: value, low: value, high: value, evidence }
    }

    /// One number with no range: a single solve. A measurement can be near it, but
    /// can't land inside it, so it isn't scored against a range.
    pub fn is_point(&self) -> bool {
        (self.high - self.low).abs() < 1e-12
    }

    pub fn contains(&self, v: f64) -> bool {
        v >= self.low && v <= self.high
    }

    pub fn text(&self) -> String {
        if (self.high - self.low).abs() < 1e-12 {
            format!("{} {}", sig(self.median), self.unit)
        } else {
            format!("{} {} (range {} to {})", sig(self.median), self.unit, sig(self.low), sig(self.high))
        }
    }
}

/// Three significant figures, no scientific notation in the normal range.
fn sig(v: f64) -> String {
    if !v.is_finite() {
        return "n/a".into();
    }
    let a = v.abs();
    if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else if a >= 0.1 || a == 0.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.2e}")
    }
}

/// What the numbers say about meeting the requirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Meets the target across nearly the whole range, shown at 95 % confidence by
    /// enough runs of a real solve.
    Likely,
    /// Could go either way: measure, or redesign.
    Uncertain,
    /// Misses the target across most of the range.
    Unlikely,
    /// No target given.
    NoTarget,
    /// Meets it on an estimate: enough to try it, not to promise it.
    WorthTesting,
    /// One solve meets it, with no range to say how close it came to missing.
    MeetsOnce,
    /// Nearly every run met it, but there were too few to say Likely.
    TooFewRuns,
}

impl Verdict {
    pub fn from_probability(p: Option<f64>) -> Self {
        match p {
            Some(p) if p >= 0.9 => Verdict::Likely,
            Some(p) if p <= 0.2 => Verdict::Unlikely,
            Some(_) => Verdict::Uncertain,
            None => Verdict::NoTarget,
        }
    }

    /// From the count, not the bare share: "Likely" takes a 95 % lower bound of at
    /// least 90 %, which starts at 29 passes out of 29.
    pub fn from_runs(r: &Runs) -> Self {
        let p = r.passed as f64 / r.n.max(1) as f64;
        if r.lower >= 0.9 {
            Verdict::Likely
        } else if p >= 0.9 {
            Verdict::TooFewRuns
        } else if p <= 0.2 {
            Verdict::Unlikely
        } else {
            Verdict::Uncertain
        }
    }

    /// No verdict says more than what's behind it can carry. Measured stands; an
    /// estimate is at best worth testing, and one solve is only that one solve.
    pub fn capped(self, evidence: Evidence, point: bool) -> Self {
        match (self, evidence) {
            (Verdict::Likely | Verdict::TooFewRuns, Evidence::Estimate) => Verdict::WorthTesting,
            (Verdict::Likely, Evidence::Simulated | Evidence::Calibrated) if point => Verdict::MeetsOnce,
            _ => self,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Verdict::Likely => "Likely to meet the target",
            Verdict::Uncertain => "Could go either way",
            Verdict::Unlikely => "Unlikely to meet the target",
            Verdict::NoTarget => "No target set",
            Verdict::WorthTesting => "Worth testing",
            Verdict::MeetsOnce => "Meets in this one solve",
            Verdict::TooFewRuns => "Met in nearly every run, too few runs to say Likely",
        }
    }
}

/// A prediction set against what was then measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    #[serde(deserialize_with = "number_or_nan")]
    pub measured: f64,
    #[serde(deserialize_with = "number_or_nan")]
    pub error: f64,
    /// The measurement fell inside the predicted range.
    pub within_range: bool,
    pub note: String,
    /// Unix seconds; 0 on records older than this field.
    #[serde(default)]
    pub at: u64,
    /// Where it came from: a file, a person, a test.
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    /// Which recipe produced it, e.g. "uwb_antenna_bias".
    pub recipe: String,
    /// What it's about, in the user's terms.
    pub subject: String,
    pub prediction: Prediction,
    /// With its [`Runs`]: the sample size and margin behind `pass_probability`.
    pub target: Option<Target>,
    pub pass_probability: Option<f64>,
    pub verdict: Verdict,
    /// Largest uncertainty first.
    pub drivers: Vec<String>,
    /// Things assumed rather than known. Never empty for an estimate.
    pub assumptions: Vec<String>,
    pub checks: Vec<Comparison>,
    /// Unix seconds.
    pub created: u64,
}

impl Claim {
    pub fn from_report(id: &str, recipe: &str, subject: &str, report: &RobustReport, target: Option<Target>, mut assumptions: Vec<String>) -> Self {
        let runs = report.runs();
        let evidence = report.prediction.evidence;
        let verdict = runs.map_or(Verdict::NoTarget, |r| Verdict::from_runs(&r)).capped(evidence, report.prediction.is_point());
        let drivers: Vec<String> = report.clear_drivers().iter().take(3).map(|s| s.factor.clone()).collect();
        if drivers.is_empty() && report.swings.iter().any(|s| s.share > 0.0) {
            assumptions.push(format!("With {} runs no factor stands out from chance, so none is named as the main cause", report.samples));
        }
        assumptions.push(range_note(report));
        assumptions.extend(report.bases.iter().map(|(name, basis)| format!("{name}: {basis}")));
        Self {
            id: id.into(),
            recipe: recipe.into(),
            subject: subject.into(),
            prediction: report.prediction.clone(),
            target: target.map(|t| Target { runs, ..t }),
            pass_probability: report.pass_probability,
            verdict,
            drivers,
            assumptions,
            checks: Vec::new(),
            created: now(),
        }
    }

    /// For a model too rough to quote a chance from: the verdict stays, the number goes.
    pub fn without_pass_chance(mut self) -> Self {
        self.pass_probability = None;
        if let Some(t) = self.target.as_mut() {
            t.runs = None;
        }
        self
    }

    /// The sample size and margin behind the chance of meeting the target.
    pub fn runs(&self) -> Option<&Runs> {
        self.target.as_ref().and_then(|t| t.runs.as_ref())
    }

    /// The value and what its range really is, as a person should read it.
    pub fn range_text(&self) -> String {
        let p = &self.prediction;
        if p.is_point() {
            return p.text();
        }
        match self.runs() {
            Some(r) if r.n < stats::RUNS_FOR_A_90_RANGE => format!("{} {} (lowest {} and highest {} of {} runs)", sig(p.median), p.unit, sig(p.low), sig(p.high), r.n),
            Some(_) => format!("{} {} (spread across assumed inputs {} to {})", sig(p.median), p.unit, sig(p.low), sig(p.high)),
            None => p.text(),
        }
    }

    /// The verdict with the count behind it, never a bare percentage.
    pub fn verdict_text(&self) -> String {
        let label = self.verdict.label();
        match (self.runs(), self.pass_probability) {
            (Some(r), _) => format!("{label} ({} of {} runs met it: at least {:.0} % of runs from these inputs, at 95 % confidence)", r.passed, r.n, r.lower * 100.0),
            (None, Some(p)) => format!("{label} ({:.0} % of runs; how many wasn't recorded)", p * 100.0),
            _ => label.to_string(),
        }
    }

    /// Records what was measured, and upgrades nothing by itself: the prediction
    /// stays as made, so the ledger can score it.
    pub fn check(&mut self, measured: f64, note: &str) {
        self.check_from(measured, note, "not stated");
    }

    /// The same, saying where the measurement came from.
    pub fn check_from(&mut self, measured: f64, note: &str, source: &str) {
        self.checks.push(Comparison { measured, error: measured - self.prediction.median, within_range: self.prediction.contains(measured), note: note.into(), at: now(), source: source.into() });
    }
}

/// What a range is, in the words a claim keeps.
fn range_note(r: &RobustReport) -> String {
    let n = r.samples;
    let what = if n < stats::RUNS_FOR_A_90_RANGE {
        format!("The range is the lowest and highest of {n} runs (seed {}), which would hold about {:.0} % of further runs", r.seed, stats::expected_range_content(n) * 100.0)
    } else {
        format!("The range holds about 90 % of runs from the same inputs ({n} runs, seed {})", r.seed)
    };
    let failed = if r.failed > 0 { format!(", leaving out the {} that gave no answer", r.failed) } else { String::new() };
    let covers = if r.bases.iter().any(|(name, _)| name.starts_with("Model error")) {
        "It spreads the inputs below and an assumed model error, none of it measured"
    } else {
        "It spreads the inputs below only, not model or measurement error, so a real measurement can land outside it"
    };
    format!("{what}{failed}. {covers}")
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Equal but for rounding: a value read back from the file can differ in its last digit.
fn same_prediction(a: &Prediction, b: &Prediction) -> bool {
    let close = |x: f64, y: f64| (x.is_nan() && y.is_nan()) || (x - y).abs() <= 1e-9 * x.abs().max(y.abs());
    a.metric == b.metric && a.unit == b.unit && a.evidence == b.evidence && close(a.median, b.median) && close(a.low, b.low) && close(a.high, b.high)
}

/// A claim someone took off the list, kept so the record stays whole.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Removed {
    pub claim: Claim,
    /// Unix seconds.
    pub at: u64,
}

/// All claims of a project, and how they've held up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub claims: Vec<Claim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<Removed>,
}

impl Ledger {
    /// Adds a claim, with no verdict beyond what its evidence can carry. One with the
    /// same id replaces the earlier one and keeps its measurements, and adds any new
    /// ones it brings. If the prediction moved, they belong to what was measured, so
    /// the earlier claim stays on record under a new id: a measurement is never lost.
    pub fn put(&mut self, mut c: Claim) {
        c.verdict = c.verdict.capped(c.prediction.evidence, c.prediction.is_point());
        let Some(i) = self.claims.iter().position(|o| o.id == c.id) else {
            self.claims.push(c);
            return;
        };
        if same_prediction(&self.claims[i].prediction, &c.prediction) {
            let mut kept = std::mem::take(&mut self.claims[i].checks);
            // A repeat of the same reading from the same place isn't a new measurement.
            for k in c.checks.drain(..) {
                if !kept.iter().any(|o| o.measured.to_bits() == k.measured.to_bits() && o.note == k.note && o.source == k.source) {
                    kept.push(k);
                }
            }
            c.checks = kept;
            self.claims[i] = c;
        } else if self.claims[i].checks.is_empty() {
            self.claims[i] = c;
        } else {
            let mut id = format!("{}@{}", c.id, self.claims[i].created);
            while self.claims.iter().any(|o| o.id == id) {
                id.push('+');
            }
            let old = &mut self.claims[i];
            old.id = id;
            old.subject = format!("{} (earlier)", old.subject);
            self.claims.push(c);
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Claim> {
        self.claims.iter_mut().find(|c| c.id == id)
    }

    /// Takes a claim off the list. It stays on record, and in the score: a miss doesn't
    /// stop having happened because it was removed.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(i) = self.claims.iter().position(|c| c.id == id) else { return false };
        let claim = self.claims.remove(i);
        self.removed.push(Removed { claim, at: now() });
        true
    }

    fn on_record(&self) -> impl Iterator<Item = &Claim> {
        self.claims.iter().chain(self.removed.iter().map(|r| &r.claim))
    }

    /// Applies today's rules to what an older version wrote: a "Likely" with no count
    /// behind it can't stand, and neither can one above its evidence.
    pub fn reassess(&mut self) {
        for c in self.claims.iter_mut().chain(self.removed.iter_mut().map(|r| &mut r.claim)) {
            let v = match (c.runs(), c.pass_probability) {
                (Some(r), _) => Verdict::from_runs(r),
                (None, Some(_)) if c.verdict == Verdict::Likely => Verdict::TooFewRuns,
                _ => c.verdict,
            };
            c.verdict = v.capped(c.prediction.evidence, c.prediction.is_point());
        }
    }

    /// Of the predictions with a range that have been measured, how many kept every
    /// measurement inside it. Per claim: ten readings of one thing are one test of its
    /// range, not ten.
    pub fn hit_rate(&self) -> Option<(usize, usize)> {
        let scored: Vec<&Claim> = self.on_record().filter(|c| !c.prediction.is_point() && !c.checks.is_empty()).collect();
        (!scored.is_empty()).then(|| (scored.iter().filter(|c| c.checks.iter().all(|k| k.within_range)).count(), scored.len()))
    }

    /// How the predictions have held up, in one line, and how little that may say.
    pub fn score_line(&self) -> String {
        let measurements: usize = self.on_record().map(|c| c.checks.len()).sum();
        match self.hit_rate() {
            Some((hit, total)) => {
                let (lo, hi) = stats::interval(total, hit, 0.95);
                let rest = if total < 20 { "That's too few to judge the ranges yet." } else { "The ranges spread assumed inputs only, not model or measurement error, so expect fewer than 9 in 10 to hold." };
                format!("{hit} of {total} measured predictions kept every measurement inside their range ({measurements} measurements in all). The true share is between {:.0} % and {:.0} %, at 95 % confidence. {rest}", lo * 100.0, hi * 100.0)
            }
            None if measurements > 0 => format!("{measurements} measurements so far, all against single solves, which have no range to land in: each says how far off it was."),
            None => "Nothing has been measured yet: every number below is a prediction.".to_string(),
        }
    }

    fn claim_markdown(c: &Claim) -> String {
        let mut s = String::new();
        s.push_str(&format!("- **{}:** {}\n", c.prediction.metric, c.range_text()));
        s.push_str(&format!("- **Rests on:** {}\n", c.prediction.evidence.label()));
        if let Some(t) = c.target {
            let word = match t.direction {
                Direction::AtMost => "at most",
                Direction::AtLeast => "at least",
            };
            s.push_str(&format!("- **Target:** {word} {} {}. {}.\n", sig(t.value), c.prediction.unit, c.verdict_text()));
        }
        if !c.drivers.is_empty() {
            s.push_str(&format!("- **Most uncertain because of:** {}\n", c.drivers.join(", ")));
        }
        if !c.assumptions.is_empty() {
            s.push_str(&format!("- **Assumed:** {}\n", c.assumptions.join("; ")));
        }
        for k in &c.checks {
            let from = if k.source.is_empty() { String::new() } else { format!(" From: {}.", k.source) };
            if c.prediction.is_point() {
                s.push_str(&format!("- **Measured:** {} {} (off by {}). {}{from}\n", sig(k.measured), c.prediction.unit, sig(k.error), k.note));
            } else {
                s.push_str(&format!("- **Measured:** {} {} ({}, off by {}). {}{from}\n", sig(k.measured), c.prediction.unit, if k.within_range { "inside the range" } else { "outside the range" }, sig(k.error), k.note));
            }
        }
        s
    }

    /// Writes the ledger whole or not at all: to a file beside it, then swapped in, so a
    /// crash or a full disk mid-write can't take the earlier record with it.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::from("# Predictions and how they held up\n\n");
        s.push_str(&format!("{NOT_VALIDATED}\n\n{}\n\n", self.score_line()));
        for c in &self.claims {
            s.push_str(&format!("## {}\n\n{}\n", c.subject, Self::claim_markdown(c)));
        }
        if !self.removed.is_empty() {
            s.push_str("# Taken off the list, still on record\n\n");
            for r in &self.removed {
                s.push_str(&format!("## {}\n\n{}\n", r.claim.subject, Self::claim_markdown(&r.claim)));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::robust::{Dist, Factor, RobustOptions, robust, robust_mut};

    fn claim(id: &str, median: f64) -> Claim {
        let p = Prediction { metric: "p95 error".into(), unit: "cm".into(), median, low: median - 5.0, high: median + 5.0, evidence: Evidence::Simulated };
        Claim { id: id.into(), recipe: "r".into(), subject: "s".into(), prediction: p, target: None, pass_probability: None, verdict: Verdict::NoTarget, drivers: vec![], assumptions: vec![], checks: vec![], created: 0 }
    }

    /// A report with `passed` of `n` runs meeting an at-most target, for `evidence`.
    fn report_of(n: usize, passed: usize, evidence: Evidence) -> RobustReport {
        let f = Factor::new("x", "", Dist::Uniform { lo: 0.0, hi: 1.0 }, "test");
        let mut i = 0;
        robust_mut("m", "u", &[f], Some(Target::at_most(1.0)), evidence, RobustOptions { samples: n, seed: 1 }, true, |_| {
            i += 1;
            if i <= passed { 0.5 + i as f64 * 1e-5 } else { 2.0 + i as f64 * 1e-5 }
        })
    }

    fn verdict_of(n: usize, passed: usize, evidence: Evidence) -> Verdict {
        Claim::from_report("a", "r", "s", &report_of(n, passed, evidence), Some(Target::at_most(1.0)), vec![]).verdict
    }

    #[test]
    fn evidence_orders_weakest_first() {
        assert!(Evidence::Estimate < Evidence::Simulated && Evidence::Simulated < Evidence::Calibrated && Evidence::Calibrated < Evidence::Measured);
    }

    #[test]
    fn verdict_follows_the_probability() {
        assert_eq!(Verdict::from_probability(Some(0.95)), Verdict::Likely);
        assert_eq!(Verdict::from_probability(Some(0.5)), Verdict::Uncertain);
        assert_eq!(Verdict::from_probability(Some(0.1)), Verdict::Unlikely);
        assert_eq!(Verdict::from_probability(None), Verdict::NoTarget);
    }

    // OUT-B8: 16 of 16 was "Likely (100 %)", with a margin of exactly 0.
    #[test]
    fn likely_rests_on_the_lower_bound_not_the_share() {
        let e = Evidence::Simulated;
        assert_eq!(verdict_of(4, 4, e), Verdict::TooFewRuns);
        assert_eq!(verdict_of(16, 16, e), Verdict::TooFewRuns);
        assert_eq!(verdict_of(16, 15, e), Verdict::TooFewRuns);
        assert_eq!(verdict_of(28, 28, e), Verdict::TooFewRuns);
        assert_eq!(verdict_of(29, 29, e), Verdict::Likely);
        assert_eq!(verdict_of(30, 29, e), Verdict::TooFewRuns);
        assert_eq!(verdict_of(2000, 1860, e), Verdict::Likely);
        assert_eq!(verdict_of(16, 8, e), Verdict::Uncertain);
        assert_eq!(verdict_of(16, 3, e), Verdict::Unlikely);
    }

    #[test]
    fn a_claim_keeps_its_count_and_margin_and_says_them() {
        let c = Claim::from_report("a", "r", "s", &report_of(16, 16, Evidence::Simulated), Some(Target::at_most(1.0)), vec![]);
        let r = c.runs().copied().unwrap();
        assert_eq!((r.n, r.passed, r.failed, r.seed), (16, 16, 0, 1));
        assert!((r.lower - 0.05f64.powf(1.0 / 16.0)).abs() < 1e-6, "{r:?}");
        assert!(r.margin > 0.05, "a margin of zero was the bug: {r:?}");
        let text = c.verdict_text();
        assert!(text.contains("16 of 16 runs") && text.contains("at least 83 %") && !text.contains("100 %"), "{text}");
        // And it survives a save.
        let back: Claim = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.runs(), c.runs());
    }

    #[test]
    fn small_runs_show_lowest_and_highest_not_a_percentile_range() {
        let c = Claim::from_report("a", "r", "s", &report_of(12, 12, Evidence::Simulated), Some(Target::at_most(1.0)), vec![]);
        assert!(c.range_text().contains("lowest") && c.range_text().contains("of 12 runs"), "{}", c.range_text());
        assert!(c.assumptions.iter().any(|a| a.contains("about 85 %")), "{:?}", c.assumptions);
        let big = Claim::from_report("a", "r", "s", &report_of(200, 200, Evidence::Simulated), Some(Target::at_most(1.0)), vec![]);
        assert!(big.range_text().contains("spread across assumed inputs"), "{}", big.range_text());
    }

    // OUT-B9: the ledger used to promise 9 in 10 of real measurements.
    #[test]
    fn a_range_says_what_it_leaves_out() {
        let c = Claim::from_report("a", "r", "s", &report_of(200, 200, Evidence::Simulated), Some(Target::at_most(1.0)), vec![]);
        assert!(c.assumptions.iter().any(|a| a.contains("not model or measurement error")), "{:?}", c.assumptions);
        assert!(c.assumptions.iter().any(|a| a.starts_with("x: test")), "the basis of each input: {:?}", c.assumptions);
        let mut l = Ledger::default();
        for i in 0..3 {
            let id = format!("c{i}");
            l.put(claim(&id, 30.0));
            l.get_mut(&id).unwrap().check(33.0, "");
        }
        let md = l.to_markdown();
        assert!(!md.contains("9 in 10") && md.contains("too few to judge"), "{md}");
        assert!(md.contains("3 of 3") && md.contains("between 29 % and 100 %"), "{md}");
        for i in 3..25 {
            let id = format!("c{i}");
            l.put(claim(&id, 30.0));
            l.get_mut(&id).unwrap().check(33.0, "");
        }
        assert!(l.to_markdown().contains("fewer than 9 in 10"), "at a usable sample size it may say so");
    }

    // OUT-B10
    #[test]
    fn a_verdict_never_says_more_than_its_evidence() {
        assert_eq!(verdict_of(200, 200, Evidence::Estimate), Verdict::WorthTesting);
        assert_eq!(verdict_of(16, 16, Evidence::Estimate), Verdict::WorthTesting);
        assert_eq!(verdict_of(200, 200, Evidence::Simulated), Verdict::Likely);
        assert_eq!(verdict_of(200, 100, Evidence::Estimate), Verdict::Uncertain, "doubt isn't capped");
        assert_eq!(verdict_of(200, 10, Evidence::Estimate), Verdict::Unlikely);
        assert_eq!(Verdict::Likely.capped(Evidence::Simulated, true), Verdict::MeetsOnce);
        assert_eq!(Verdict::Likely.capped(Evidence::Calibrated, true), Verdict::MeetsOnce);
        assert_eq!(Verdict::Likely.capped(Evidence::Measured, true), Verdict::Likely);
        assert_eq!(Verdict::Unlikely.capped(Evidence::Simulated, true), Verdict::Unlikely);
        // The ledger holds every recipe to it, even one that builds its claim by hand.
        let mut l = Ledger::default();
        let mut c = claim("a", 1.0);
        c.prediction.evidence = Evidence::Estimate;
        c.verdict = Verdict::Likely;
        l.put(c);
        assert_eq!(l.claims[0].verdict, Verdict::WorthTesting);
    }

    #[test]
    fn old_ledger_files_still_load_and_lose_verdicts_they_cant_support() {
        let old = r#"{ "claims": [
            { "id": "tag", "recipe": "tag_on_product", "subject": "s", "prediction": { "metric": "m", "unit": "m", "median": 5.0, "low": 4.0, "high": 6.0, "evidence": "estimate" },
              "target": { "direction": "at_least", "value": 3.0 }, "pass_probability": 1.0, "verdict": "likely", "drivers": [], "assumptions": [],
              "checks": [ { "measured": 4.5, "error": -0.5, "within_range": true, "note": "walk" } ], "created": 5 },
            { "id": "site", "recipe": "site_stress", "subject": "s", "prediction": { "metric": "m", "unit": "m", "median": 0.2, "low": 0.1, "high": 0.25, "evidence": "simulated" },
              "target": { "direction": "at_most", "value": 0.3 }, "pass_probability": 0.94, "verdict": "likely", "drivers": [], "assumptions": [], "checks": [], "created": 6 },
            { "id": "solve", "recipe": "r", "subject": "s", "prediction": { "metric": "m", "unit": "dB", "median": 0.5, "low": 0.5, "high": 0.5, "evidence": "simulated" },
              "target": { "direction": "at_most", "value": 1.0 }, "pass_probability": null, "verdict": "likely", "drivers": [], "assumptions": [], "checks": [], "created": 7 },
            { "id": "test", "recipe": "acceptance", "subject": "s", "prediction": { "metric": "m", "unit": "m", "median": 0.2, "low": 0.2, "high": 0.2, "evidence": "measured" },
              "target": { "direction": "at_most", "value": 0.3 }, "pass_probability": null, "verdict": "likely", "drivers": [], "assumptions": [], "checks": [], "created": 8 }
        ] }"#;
        let mut l: Ledger = serde_json::from_str(old).unwrap();
        assert_eq!(l.claims.len(), 4);
        assert!(l.removed.is_empty());
        assert_eq!(l.claims[0].checks[0].at, 0);
        assert_eq!(l.claims[0].checks[0].source, "");
        assert_eq!(l.claims[0].runs(), None);
        l.reassess();
        assert_eq!(l.claims[0].verdict, Verdict::WorthTesting);
        assert_eq!(l.claims[1].verdict, Verdict::TooFewRuns, "no count on record, so no Likely");
        assert!(l.claims[1].verdict_text().contains("wasn't recorded"), "{}", l.claims[1].verdict_text());
        assert_eq!(l.claims[2].verdict, Verdict::MeetsOnce);
        assert_eq!(l.claims[3].verdict, Verdict::Likely, "measured stands");
        // Written back and read again, nothing is lost.
        let again: Ledger = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(again, l);
    }

    // The old assertion was (1, 2): two readings of one claim pooled as if independent.
    #[test]
    fn ledger_scores_measurements_against_the_original_range() {
        let mut l = Ledger::default();
        l.put(claim("a", 30.0));
        assert!(l.hit_rate().is_none());
        l.get_mut("a").unwrap().check(33.0, "walk 1");
        assert_eq!(l.hit_rate(), Some((1, 1)));
        l.get_mut("a").unwrap().check(60.0, "walk 2");
        assert_eq!(l.hit_rate(), Some((0, 1)), "one claim, one vote, and a miss stands");
        let k = &l.claims[0].checks[1];
        assert_eq!(k.error, 30.0);
        assert!(!k.within_range);
        l.put(claim("b", 30.0));
        l.get_mut("b").unwrap().check(31.0, "");
        l.get_mut("b").unwrap().check(32.0, "");
        l.get_mut("b").unwrap().check(33.0, "");
        assert_eq!(l.hit_rate(), Some((1, 2)), "three readings are still one test");
    }

    #[test]
    fn a_changed_prediction_never_loses_a_measurement() {
        let mut l = Ledger::default();
        l.put(claim("a", 30.0));
        l.get_mut("a").unwrap().check(33.0, "");
        l.put(claim("a", 30.0));
        assert_eq!(l.claims[0].checks.len(), 1, "same prediction keeps them");
        l.put(claim("a", 40.0));
        assert_eq!(l.claims.len(), 2, "what was measured stays on record");
        assert_eq!(l.claims[0].checks.len(), 1);
        assert!(l.claims[0].id.starts_with("a@"), "{}", l.claims[0].id);
        assert_eq!(l.get_mut("a").unwrap().prediction.median, 40.0);
        assert!(l.get_mut("a").unwrap().checks.is_empty(), "a new design has not been measured");
        // Nothing measured: it's simply replaced.
        l.put(claim("a", 50.0));
        assert_eq!(l.claims.len(), 2);
        // Archiving twice in the same second can't collide.
        l.get_mut("a").unwrap().check(1.0, "");
        l.put(claim("a", 60.0));
        l.get_mut("a").unwrap().check(2.0, "");
        l.put(claim("a", 70.0));
        let mut ids: Vec<&str> = l.claims.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), l.claims.len(), "{ids:?}");
    }

    // OUT-M13: re-running with an unchanged prediction dropped what the new run measured.
    #[test]
    fn a_repeat_run_keeps_a_new_measurement_and_doesnt_double_the_old_one() {
        let run = |measured: f64| {
            let mut c = claim("vna", 2.45);
            c.check_from(measured, "From the VNA sweep", "a.s1p");
            c
        };
        let mut l = Ledger::default();
        l.put(run(2.40));
        l.put(run(2.40));
        assert_eq!(l.claims[0].checks.len(), 1, "the same reading from the same file");
        l.put(run(2.41));
        assert_eq!(l.claims[0].checks.len(), 2, "the file changed: a new measurement");
        assert_eq!(l.claims[0].checks[1].source, "a.s1p");
        assert!(l.claims[0].checks[1].at > 0, "stamped");
    }

    #[test]
    fn removing_a_claim_keeps_it_on_record_and_in_the_score() {
        let mut l = Ledger::default();
        l.put(claim("miss", 30.0));
        l.get_mut("miss").unwrap().check(99.0, "");
        l.put(claim("hit", 30.0));
        l.get_mut("hit").unwrap().check(31.0, "");
        assert_eq!(l.hit_rate(), Some((1, 2)));
        assert!(l.remove("miss"));
        assert!(!l.remove("miss"));
        assert_eq!(l.claims.len(), 1);
        assert_eq!(l.hit_rate(), Some((1, 2)), "removing a miss doesn't improve the record");
        assert!(l.to_markdown().contains("still on record"));
        let back: Ledger = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back.removed.len(), 1);
        assert_eq!(back.removed[0].claim.checks[0].measured, 99.0);
    }

    #[test]
    fn a_single_solve_is_not_scored_as_a_range() {
        let mut l = Ledger::default();
        let mut c = claim("a", 2.45);
        c.prediction.low = 2.45;
        c.prediction.high = 2.45;
        c.check(2.449, "VNA");
        l.put(c);
        assert_eq!(l.hit_rate(), None, "a point can't catch a measurement");
        let md = l.to_markdown();
        assert!(md.contains("off by") && !md.contains("outside the range"), "{md}");
        // It used to say nothing had been measured, right above the measurement.
        assert!(!md.contains("Nothing has been measured"), "{md}");
        assert!(md.contains("1 measurements so far"), "{md}");
    }

    #[test]
    fn a_failed_number_doesnt_make_the_ledger_unreadable() {
        let mut l = Ledger::default();
        let mut c = claim("a", f64::NAN);
        c.check(3.0, "measured against nothing");
        l.put(c);
        l.put(claim("b", 30.0));
        let text = serde_json::to_string(&l).unwrap();
        let back: Ledger = serde_json::from_str(&text).unwrap();
        assert_eq!(back.claims.len(), 2);
        assert!(back.claims[0].prediction.median.is_nan());
        assert_eq!(back.claims[1].prediction.median, 30.0);
        assert!(l.to_markdown().contains("n/a"));
    }

    // "1 of 1 measurements" was the pooled count of OUT-B9.
    #[test]
    fn markdown_says_when_nothing_was_measured_and_that_nothing_is_validated() {
        let mut l = Ledger::default();
        l.put(claim("a", 30.0));
        let md = l.to_markdown();
        assert!(md.contains("Nothing has been measured yet"), "{md}");
        assert!(md.contains("validated on real hardware"), "{md}");
        assert!(md.contains("30.0 cm (range 25.0 to 35.0)"), "{md}");
        l.get_mut("a").unwrap().check(33.0, "walk");
        let md = l.to_markdown();
        assert!(md.contains("1 of 1 measured predictions"), "{md}");
        assert!(md.contains("validated on real hardware"), "{md}");
    }

    #[test]
    fn a_claim_carries_the_report() {
        let f = Factor::new("rack fill", "%", Dist::Uniform { lo: 0.0, hi: 1.0 }, "assumed");
        let t = Target::at_most(0.5);
        let r = robust("x", "m", &[f], Some(t), Evidence::Simulated, RobustOptions { samples: 400, seed: 1 }, |v| v[0]);
        let c = Claim::from_report("id", "recipe", "subject", &r, Some(t), vec!["racks are steel".into()]);
        assert_eq!(c.verdict, Verdict::Uncertain);
        assert_eq!(c.drivers, vec!["rack fill".to_string()]);
        assert_eq!(c.assumptions[0], "racks are steel");
        let back: Claim = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(same_prediction(&c.prediction, &back.prediction), "{:?} {:?}", c.prediction, back.prediction);
        assert_eq!((c.target.map(|t| t.value), c.runs().map(|r| r.n)), (back.target.map(|t| t.value), back.runs().map(|r| r.n)));
        assert_eq!((c.verdict, c.drivers, c.assumptions), (back.verdict, back.drivers, back.assumptions));
    }

    // serde_json's parser can move a float's last digit. That read as a moved
    // prediction, which sent a claim's measurements off under an "(earlier)" copy.
    #[test]
    fn a_value_read_back_from_the_file_is_the_same_prediction() {
        let mut l = Ledger::default();
        let mut c = claim("a", 30.0);
        c.prediction.high = 0.9510885276761029;
        l.put(c.clone());
        l.get_mut("a").unwrap().check(33.0, "");
        let mut rerun = c.clone();
        rerun.prediction.high = 0.9510885276761027;
        l.put(rerun);
        assert_eq!(l.claims.len(), 1, "{:?}", l.claims);
        assert_eq!(l.claims[0].checks.len(), 1);
        // A failed run compared with itself isn't a change either.
        let mut failed = claim("b", f64::NAN);
        failed.check(1.0, "");
        l.put(failed.clone());
        failed.checks.clear();
        l.put(failed);
        assert_eq!(l.claims.len(), 2);
    }

    // OUT-M11: at n = 16 a rank correlation needs r² of about 0.25 to mean anything.
    #[test]
    fn a_driver_is_only_named_when_it_stands_out_from_chance() {
        let noise = Factor::new("noise", "", Dist::Uniform { lo: 0.0, hi: 1.0 }, "test");
        let mut rng = crate::Rng::new(3);
        let weak = robust_mut("x", "", std::slice::from_ref(&noise), None, Evidence::Simulated, RobustOptions { samples: 16, seed: 4 }, false, |_| rng.gauss());
        assert!(weak.swings[0].share < 3.84 / 15.0, "this seed gives no real link: {:?}", weak.swings);
        let c = Claim::from_report("a", "r", "s", &weak, None, vec![]);
        assert!(c.drivers.is_empty(), "{:?}", c.drivers);
        assert!(c.assumptions.iter().any(|a| a.contains("stands out from chance")), "{:?}", c.assumptions);
        let strong = robust_mut("x", "", &[noise], None, Evidence::Simulated, RobustOptions { samples: 16, seed: 4 }, false, |v| v[0]);
        assert_eq!(Claim::from_report("a", "r", "s", &strong, None, vec![]).drivers, vec!["noise".to_string()]);
    }

    #[test]
    fn a_save_replaces_the_file_whole_and_a_failure_leaves_it_alone() {
        let dir = std::env::temp_dir().join(format!("proof_ledger_{}", std::process::id()));
        let path = dir.join("predictions.json");
        let mut l = Ledger::default();
        l.put(claim("a", 30.0));
        l.save_to(&path).unwrap();
        l.put(claim("b", 31.0));
        l.save_to(&path).unwrap();
        let back: Ledger = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.claims.len(), 2);
        assert!(!path.with_extension("json.tmp").exists());
        // The target is a directory, so the swap fails: an error, and nothing left behind.
        let blocked = dir.join("blocked.json");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(l.save_to(&blocked).is_err());
        assert!(!blocked.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tag_estimate_can_keep_its_verdict_and_drop_its_chance() {
        let c = Claim::from_report("a", "r", "s", &report_of(200, 200, Evidence::Estimate), Some(Target::at_most(1.0)), vec![]).without_pass_chance();
        assert_eq!(c.pass_probability, None);
        assert_eq!(c.runs(), None);
        assert_eq!(c.verdict_text(), "Worth testing");
        let mut l = Ledger { claims: vec![c], removed: vec![] };
        l.reassess();
        assert_eq!(l.claims[0].verdict, Verdict::WorthTesting);
    }
}
