//! Step 1 of the loop: predict with a range. A model is any function of its
//! uncertain factors; this samples them (Latin hypercube, so few samples cover
//! each factor's range evenly), summarises the outcome and ranks what drives it.

use serde::{Deserialize, Serialize};

use crate::claim::{Evidence, Prediction, Verdict};
use crate::quantile;
use crate::rng::{Rng, normal_quantile};
use crate::stats::{self, range_point};

/// How an uncertain input is distributed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Dist {
    /// A value we simply don't know within a range.
    Uniform { lo: f64, hi: f64 },
    Normal { mean: f64, sigma: f64 },
    /// Positive, multiplicative uncertainty: `median · exp(σ·z)`.
    LogNormal { median: f64, sigma: f64 },
    /// A fixed value (a factor switched off).
    Fixed { value: f64 },
}

impl Dist {
    /// Value at cumulative probability `u` in (0, 1).
    pub fn at(&self, u: f64) -> f64 {
        match *self {
            Dist::Uniform { lo, hi } => lo + (hi - lo) * u,
            Dist::Normal { mean, sigma } => mean + sigma * normal_quantile(u),
            Dist::LogNormal { median, sigma } => median * (sigma * normal_quantile(u)).exp(),
            Dist::Fixed { value } => value,
        }
    }

    pub fn nominal(&self) -> f64 {
        self.at(0.5)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Factor {
    pub name: String,
    pub unit: String,
    pub dist: Dist,
    /// Why this range: a source, a datasheet, or "assumed". Reports show it.
    pub basis: String,
}

impl Factor {
    pub fn new(name: &str, unit: &str, dist: Dist, basis: &str) -> Self {
        Self { name: name.into(), unit: unit.into(), dist, basis: basis.into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    AtMost,
    AtLeast,
}

/// How a chance of meeting a target was counted, so a percentage never travels without
/// its sample size. The bounds are about further runs from the same assumed inputs, not
/// about the real site. Latin hypercube draws are at least as even as random ones, so
/// on them the bounds err on the cautious side.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Runs {
    pub n: usize,
    /// Runs that met the target. Runs with no answer count as misses.
    pub passed: usize,
    /// Runs that gave no answer at all.
    pub failed: usize,
    /// The chance is at least this, at 95 % confidence (one-sided Clopper–Pearson).
    pub lower: f64,
    /// And at most this.
    pub upper: f64,
    /// Half the width of the two-sided 95 % interval.
    pub margin: f64,
    /// What the runs were drawn with, to repeat them.
    pub seed: u64,
}

impl Runs {
    pub fn new(n: usize, passed: usize, failed: usize, seed: u64) -> Self {
        let (lo, hi) = stats::interval(n, passed, 0.95);
        Self { n, passed, failed, lower: stats::shown_share(n, passed, 0.95), upper: stats::upper_bound(n, passed, 0.95), margin: (hi - lo) / 2.0, seed }
    }
}

/// What counts as meeting the requirement and, once judged, how that was counted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub direction: Direction,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Runs>,
    /// Only in a ledger file: a verdict older builds have no name for. See `LedgerFile`.
    #[serde(default, deserialize_with = "crate::claim::verdict_if_known", skip_serializing_if = "Option::is_none")]
    pub saved_verdict: Option<Verdict>,
}

impl Target {
    pub fn at_most(value: f64) -> Self {
        Self { direction: Direction::AtMost, value, runs: None, saved_verdict: None }
    }
    pub fn at_least(value: f64) -> Self {
        Self { direction: Direction::AtLeast, value, runs: None, saved_verdict: None }
    }
    pub fn met_by(&self, v: f64) -> bool {
        match self.direction {
            Direction::AtMost => v <= self.value,
            Direction::AtLeast => v >= self.value,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RobustOptions {
    pub samples: usize,
    pub seed: u64,
}

impl Default for RobustOptions {
    fn default() -> Self {
        Self { samples: 200, seed: 1 }
    }
}

/// How far the outcome moves when one factor goes from its 10th to its 90th
/// percentile with the others at nominal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Swing {
    pub factor: String,
    pub at_low: f64,
    pub at_high: f64,
    pub span: f64,
    /// Squared rank correlation with the outcome over the samples (0..1): roughly how
    /// much this factor explains on its own, not a true share of the variance.
    pub share: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RobustReport {
    pub prediction: Prediction,
    pub nominal: f64,
    /// Share of runs that met the target; `None` without a target.
    pub pass_probability: Option<f64>,
    /// 95 % half-width on `pass_probability` from sampling alone (exact, so 16 of 16
    /// still has one), not from the model being wrong.
    pub pass_margin: Option<f64>,
    /// Largest first.
    pub swings: Vec<Swing>,
    pub samples: usize,
    /// Samples the model could not evaluate (NaN or infinite result).
    pub failed: usize,
    /// The worst sample, with its factor values.
    pub worst: Vec<(String, f64)>,
    /// Runs that met the target; `None` without a target.
    #[serde(default)]
    pub passed: Option<usize>,
    #[serde(default)]
    pub seed: u64,
    /// Each factor and why it has the range it has.
    #[serde(default)]
    pub bases: Vec<(String, String)>,
    /// No factor had any spread, so however many runs there were, it's one case. Different
    /// from factors that vary and an outcome that doesn't: that's a robust result.
    #[serde(default)]
    pub nothing_varied: bool,
}

impl RobustReport {
    /// The factor most worth pinning down.
    pub fn main_driver(&self) -> Option<&Swing> {
        self.swings.first()
    }

    /// The count behind `pass_probability`.
    pub fn runs(&self) -> Option<Runs> {
        self.passed.map(|p| Runs::new(self.samples, p, self.failed, self.seed))
    }

    /// Factors whose link to the outcome stands out from chance. With swings worked out
    /// their size says so; without them, a squared rank correlation under about
    /// 3.84/(n − 1) could be luck.
    pub fn clear_drivers(&self) -> Vec<&Swing> {
        let chance = 3.84 / (self.samples.max(2) as f64 - 1.0);
        self.swings.iter().filter(|s| if s.at_low.is_finite() { s.span > 0.0 } else { s.share >= chance }).collect()
    }
}

/// Evaluates `model` over the factors and summarises the outcome. `model` gets
/// factor values in the order given. With a `target`, "worst" means furthest
/// from meeting it; without one, the highest value.
pub fn robust(metric: &str, unit: &str, factors: &[Factor], target: Option<Target>, evidence: Evidence, opts: RobustOptions, model: impl Fn(&[f64]) -> f64 + Sync) -> RobustReport {
    let n = opts.samples.max(2);
    let k = factors.len();
    let mut rng = Rng::new(opts.seed);
    // One stratified, shuffled column per factor.
    let columns: Vec<Vec<f64>> = (0..k)
        .map(|f| {
            let mut strata: Vec<f64> = (0..n).map(|i| (i as f64 + rng.uniform()) / n as f64).collect();
            rng.shuffle(&mut strata);
            strata.into_iter().map(|u| factors[f].dist.at(u)).collect()
        })
        .collect();
    let at = |i: usize| -> Vec<f64> { columns.iter().map(|c| c[i]).collect() };
    let outcomes = par_map(n, |i| model(&at(i)));

    let good: Vec<usize> = (0..n).filter(|&i| outcomes[i].is_finite()).collect();
    let mut sorted: Vec<f64> = good.iter().map(|&i| outcomes[i]).collect();
    sorted.sort_by(f64::total_cmp);
    let prediction = Prediction {
        metric: metric.into(),
        unit: unit.into(),
        median: quantile(&sorted, 0.5),
        low: range_point(&sorted, 0.05),
        high: range_point(&sorted, 0.95),
        evidence,
    };

    // A run that produced no answer at all (say, every position lost) has met nothing.
    let passed = target.map(|t| good.iter().filter(|&&i| t.met_by(outcomes[i])).count());
    let pass_probability = passed.map(|p| p as f64 / n as f64);
    let pass_margin = passed.map(|p| Runs::new(n, p, n - good.len(), opts.seed).margin);
    let badness = |v: f64| match target.map(|t| t.direction) {
        Some(Direction::AtLeast) => -v,
        _ => v,
    };
    let worst_i = good.iter().copied().max_by(|a, b| badness(outcomes[*a]).total_cmp(&badness(outcomes[*b])));
    let worst = worst_i.map_or(Vec::new(), |i| factors.iter().enumerate().map(|(f, x)| (x.name.clone(), columns[f][i])).collect());

    let nominal_x: Vec<f64> = factors.iter().map(|f| f.dist.nominal()).collect();
    let nominal = model(&nominal_x);
    let out_ranks = ranks(&good.iter().map(|&i| outcomes[i]).collect::<Vec<_>>());
    let mut swings: Vec<Swing> = (0..k)
        .map(|f| {
            let mut x = nominal_x.clone();
            x[f] = factors[f].dist.at(0.1);
            let at_low = model(&x);
            x[f] = factors[f].dist.at(0.9);
            let at_high = model(&x);
            let col: Vec<f64> = good.iter().map(|&i| columns[f][i]).collect();
            let r = correlation(&ranks(&col), &out_ranks);
            Swing { factor: factors[f].name.clone(), at_low, at_high, span: (at_high - at_low).abs(), share: r * r }
        })
        .collect();
    swings.sort_by(|a, b| b.span.total_cmp(&a.span).then(b.share.total_cmp(&a.share)));

    RobustReport { prediction, nominal, pass_probability, pass_margin, swings, samples: n, failed: n - good.len(), worst, passed, seed: opts.seed, bases: factors.iter().map(|f| (f.name.clone(), f.basis.clone())).collect(), nothing_varied: nothing_varies(factors) }
}

fn nothing_varies(factors: &[Factor]) -> bool {
    factors.iter().all(|f| f.dist.at(0.1) == f.dist.at(0.9))
}

/// The same for a model that can't run in parallel, such as a whole scene being
/// simulated. With `swings` off, only the samples are run (each factor then gets a
/// `share` but no low/high pair), which saves `2·factors + 1` expensive evaluations.
pub fn robust_mut(metric: &str, unit: &str, factors: &[Factor], target: Option<Target>, evidence: Evidence, opts: RobustOptions, swings: bool, mut model: impl FnMut(&[f64]) -> f64) -> RobustReport {
    let n = opts.samples.max(2);
    let k = factors.len();
    let mut rng = Rng::new(opts.seed);
    let columns: Vec<Vec<f64>> = (0..k)
        .map(|f| {
            let mut strata: Vec<f64> = (0..n).map(|i| (i as f64 + rng.uniform()) / n as f64).collect();
            rng.shuffle(&mut strata);
            strata.into_iter().map(|u| factors[f].dist.at(u)).collect()
        })
        .collect();
    let outcomes: Vec<f64> = (0..n).map(|i| model(&columns.iter().map(|c| c[i]).collect::<Vec<_>>())).collect();
    let good: Vec<usize> = (0..n).filter(|&i| outcomes[i].is_finite()).collect();
    let mut sorted: Vec<f64> = good.iter().map(|&i| outcomes[i]).collect();
    sorted.sort_by(f64::total_cmp);
    let prediction = Prediction { metric: metric.into(), unit: unit.into(), median: quantile(&sorted, 0.5), low: range_point(&sorted, 0.05), high: range_point(&sorted, 0.95), evidence };
    // A run that produced no answer at all (say, every position lost) has met nothing.
    let passed = target.map(|t| good.iter().filter(|&&i| t.met_by(outcomes[i])).count());
    let pass_probability = passed.map(|p| p as f64 / n as f64);
    let pass_margin = passed.map(|p| Runs::new(n, p, n - good.len(), opts.seed).margin);
    let badness = |v: f64| match target.map(|t| t.direction) {
        Some(Direction::AtLeast) => -v,
        _ => v,
    };
    let worst_i = good.iter().copied().max_by(|a, b| badness(outcomes[*a]).total_cmp(&badness(outcomes[*b])));
    let worst = worst_i.map_or(Vec::new(), |i| factors.iter().enumerate().map(|(f, x)| (x.name.clone(), columns[f][i])).collect());
    let nominal_x: Vec<f64> = factors.iter().map(|f| f.dist.nominal()).collect();
    let nominal = if swings { model(&nominal_x) } else { f64::NAN };
    let out_ranks = ranks(&good.iter().map(|&i| outcomes[i]).collect::<Vec<_>>());
    let mut list: Vec<Swing> = (0..k)
        .map(|f| {
            let col: Vec<f64> = good.iter().map(|&i| columns[f][i]).collect();
            let r = correlation(&ranks(&col), &out_ranks);
            let (at_low, at_high) = if swings {
                let mut x = nominal_x.clone();
                x[f] = factors[f].dist.at(0.1);
                let lo = model(&x);
                x[f] = factors[f].dist.at(0.9);
                (lo, model(&x))
            } else {
                (f64::NAN, f64::NAN)
            };
            let span = if swings { (at_high - at_low).abs() } else { r * r };
            Swing { factor: factors[f].name.clone(), at_low, at_high, span, share: r * r }
        })
        .collect();
    list.sort_by(|a, b| b.span.total_cmp(&a.span).then(b.share.total_cmp(&a.share)));
    RobustReport { prediction, nominal, pass_probability, pass_margin, swings: list, samples: n, failed: n - good.len(), worst, passed, seed: opts.seed, bases: factors.iter().map(|f| (f.name.clone(), f.basis.clone())).collect(), nothing_varied: nothing_varies(factors) }
}

/// Parallel map over `0..n` on plain threads, so results come back in order and
/// the crate stays free of dependencies.
fn par_map<F: Fn(usize) -> f64 + Sync>(n: usize, f: F) -> Vec<f64> {
    let threads = std::thread::available_parallelism().map_or(1, |p| p.get()).min(n).max(1);
    if threads == 1 {
        return (0..n).map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out = vec![f64::NAN; n];
    let chunks: Vec<Vec<(usize, f64)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n {
                            return local;
                        }
                        local.push((i, f(i)));
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    for (i, v) in chunks.into_iter().flatten() {
        out[i] = v;
    }
    out
}

/// Average ranks, ties shared.
fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|a, b| v[*a].total_cmp(&v[*b]));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let rank = (i + j) as f64 / 2.0;
        for k in i..=j {
            r[idx[k]] = rank;
        }
        i = j + 1;
    }
    r
}

fn correlation(a: &[f64], b: &[f64]) -> f64 {
    if a.len() < 2 {
        return 0.0;
    }
    let (ma, mb) = (a.iter().sum::<f64>() / a.len() as f64, b.iter().sum::<f64>() / b.len() as f64);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        ab += (x - ma) * (y - mb);
        aa += (x - ma).powi(2);
        bb += (y - mb).powi(2);
    }
    if aa <= 0.0 || bb <= 0.0 { 0.0 } else { ab / (aa * bb).sqrt() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(name: &str, sigma: f64) -> Factor {
        Factor::new(name, "", Dist::Normal { mean: 0.0, sigma }, "test")
    }

    #[test]
    fn range_matches_a_sum_of_gaussians() {
        // a + b with σ 3 and 4 is σ 5: the 5–95 % range is ±8.22.
        let r = robust("x", "", &[normal("a", 3.0), normal("b", 4.0)], None, Evidence::Simulated, RobustOptions { samples: 4000, seed: 3 }, |v| v[0] + v[1]);
        assert!(r.prediction.median.abs() < 0.3, "{:?}", r.prediction);
        assert!((r.prediction.high - 8.22).abs() < 0.5, "{:?}", r.prediction);
        assert!((r.prediction.low + 8.22).abs() < 0.5, "{:?}", r.prediction);
        assert_eq!(r.failed, 0);
    }

    #[test]
    fn the_biggest_factor_ranks_first_with_most_of_the_variance() {
        let r = robust("x", "", &[normal("small", 1.0), normal("big", 10.0)], None, Evidence::Simulated, RobustOptions { samples: 2000, seed: 1 }, |v| v[0] + v[1]);
        assert_eq!(r.main_driver().unwrap().factor, "big");
        assert!(r.swings[0].share > 0.9, "{:?}", r.swings);
        assert!(r.swings[1].share < 0.1, "{:?}", r.swings);
    }

    #[test]
    fn pass_probability_matches_the_normal_tail() {
        let t = Target::at_most(1.0);
        let r = robust("x", "", &[normal("a", 1.0)], Some(t), Evidence::Simulated, RobustOptions { samples: 5000, seed: 2 }, |v| v[0]);
        assert!((r.pass_probability.unwrap() - 0.8413).abs() < 0.01, "{:?}", r.pass_probability);
        assert!(r.pass_margin.unwrap() < 0.02);
        // The worst sample of an at-most target is its highest value.
        assert!(r.worst[0].1 > 2.5, "{:?}", r.worst);
    }

    #[test]
    fn worst_of_an_at_least_target_is_the_lowest() {
        let t = Target::at_least(0.0);
        let r = robust("x", "", &[normal("a", 1.0)], Some(t), Evidence::Simulated, RobustOptions { samples: 500, seed: 2 }, |v| v[0]);
        assert!(r.worst[0].1 < -2.0, "{:?}", r.worst);
    }

    #[test]
    fn failed_samples_are_counted_not_averaged() {
        let f = Factor::new("a", "", Dist::Uniform { lo: 0.0, hi: 1.0 }, "test");
        let r = robust("x", "", &[f], None, Evidence::Simulated, RobustOptions { samples: 1000, seed: 1 }, |v| if v[0] < 0.25 { f64::NAN } else { v[0] });
        assert!((r.failed as f64 - 250.0).abs() < 2.0, "{}", r.failed);
        assert!(r.prediction.low >= 0.25);
    }

    #[test]
    fn a_run_with_no_answer_counts_as_a_miss() {
        let f = Factor::new("a", "", Dist::Uniform { lo: 0.0, hi: 1.0 }, "test");
        // Half the runs give no answer; every run that does meets the target.
        let r = robust("x", "", &[f], Some(Target::at_most(10.0)), Evidence::Simulated, RobustOptions { samples: 1000, seed: 1 }, |v| if v[0] < 0.5 { f64::NAN } else { v[0] });
        assert!((r.pass_probability.unwrap() - 0.5).abs() < 0.01, "{:?}", r.pass_probability);
    }

    // OUT-B8: at n = 12 the old 5–95 % points sat inside the runs, holding about 76 %.
    #[test]
    fn a_small_run_gives_its_lowest_and_highest_and_a_large_one_its_percentiles() {
        let of = |n: usize| {
            let mut runs = Vec::new();
            let r = robust_mut("x", "", &[normal("a", 1.0)], None, Evidence::Simulated, RobustOptions { samples: n, seed: 9 }, false, |v| {
                runs.push(v[0]);
                v[0]
            });
            runs.sort_by(f64::total_cmp);
            (r.prediction, runs)
        };
        let (p, runs) = of(12);
        assert_eq!((p.low, p.high), (runs[0], runs[11]));
        let (p, runs) = of(199);
        assert_eq!((p.low, p.high), (runs[9], runs[189]), "plotting positions 10 and 190");
    }

    // OUT-B8: 16 of 16 had a margin of exactly 0.
    #[test]
    fn a_clean_sweep_still_has_a_margin() {
        let r = robust("x", "", &[normal("a", 1.0)], Some(Target::at_most(100.0)), Evidence::Simulated, RobustOptions { samples: 16, seed: 1 }, |v| v[0]);
        assert_eq!(r.pass_probability, Some(1.0));
        assert!(r.pass_margin.unwrap() > 0.05, "{:?}", r.pass_margin);
        let k = r.runs().unwrap();
        assert_eq!((k.n, k.passed, k.seed), (16, 16, 1));
        assert!((k.lower - 0.829).abs() < 0.002, "{k:?}");
    }

    #[test]
    fn results_are_reproducible() {
        let a = robust("x", "", &[normal("a", 1.0)], None, Evidence::Simulated, RobustOptions::default(), |v| v[0]);
        let b = robust("x", "", &[normal("a", 1.0)], None, Evidence::Simulated, RobustOptions::default(), |v| v[0]);
        assert_eq!(a, b);
    }

    #[test]
    fn the_sequential_form_agrees_with_the_parallel_one() {
        let factors = [normal("a", 3.0), normal("b", 1.0)];
        let opts = RobustOptions { samples: 300, seed: 5 };
        let par = robust("x", "", &factors, None, Evidence::Simulated, opts, |v| v[0] + v[1]);
        let mut calls = 0;
        let seq = robust_mut("x", "", &factors, None, Evidence::Simulated, opts, true, |v| {
            calls += 1;
            v[0] + v[1]
        });
        assert_eq!(par, seq);
        assert_eq!(calls, 300 + 1 + 2 * 2);
        let mut cheap = 0;
        let fast = robust_mut("x", "", &factors, None, Evidence::Simulated, opts, false, |v| {
            cheap += 1;
            v[0] + v[1]
        });
        assert_eq!(cheap, 300, "no swing runs");
        assert_eq!(fast.prediction, par.prediction);
        assert_eq!(fast.main_driver().unwrap().factor, "a");
    }

    #[test]
    fn distributions_hit_their_quantiles() {
        assert_eq!(Dist::Uniform { lo: 2.0, hi: 4.0 }.at(0.25), 2.5);
        assert!((Dist::LogNormal { median: 2.0, sigma: 0.5 }.nominal() - 2.0).abs() < 1e-9);
        assert_eq!(Dist::Fixed { value: 7.0 }.at(0.9), 7.0);
    }
}
