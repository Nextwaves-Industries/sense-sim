//! What a stated range really catches. A known truth, ranges built from `n` runs the
//! way the code builds them, and a count of how often a fresh value lands inside.
//! `cargo test -p proof --test coverage -- --nocapture` prints the table.

use proof::stats::expected_range_content;
use proof::{Dist, Evidence, Factor, Rng, RobustOptions, quantile, robust_mut};

/// `n` Latin hypercube runs of a sum of `k` equal Gaussians with unit variance overall.
fn lhs_runs(n: usize, k: usize, seed: u64) -> (Vec<f64>, (f64, f64)) {
    let sigma = 1.0 / (k as f64).sqrt();
    let factors: Vec<Factor> = (0..k).map(|i| Factor::new(&format!("f{i}"), "", Dist::Normal { mean: 0.0, sigma }, "test")).collect();
    let mut outcomes = Vec::with_capacity(n);
    let r = robust_mut("y", "", &factors, None, Evidence::Simulated, RobustOptions { samples: n, seed }, false, |v| {
        let y = v.iter().sum::<f64>();
        outcomes.push(y);
        y
    });
    outcomes.sort_by(f64::total_cmp);
    (outcomes, (r.prediction.low, r.prediction.high))
}

fn iid_runs(n: usize, rng: &mut Rng) -> Vec<f64> {
    let mut v: Vec<f64> = (0..n).map(|_| rng.gauss()).collect();
    v.sort_by(f64::total_cmp);
    v
}

fn type7(sorted: &[f64]) -> (f64, f64) {
    (quantile(sorted, 0.05), quantile(sorted, 0.95))
}

#[test]
fn range_coverage_against_a_known_truth() {
    let audit = [(4usize, 54.0), (12, 76.0), (16, 79.0), (200, 89.0)];
    println!("n | audit | old iid | old LHS k=1 | old LHS k=3 | now iid | now LHS k=1 | now LHS k=3 | label says | now, truth with model error σ=0.5 | σ=1");
    for (n, predicted) in audit {
        let trials = if n >= 200 { 3000 } else { 30000 };
        let mut rng = Rng::new(n as u64 * 977 + 1);
        let mut hits = [0usize; 8];
        for t in 0..trials {
            let seed = (t as u64).wrapping_mul(0x9E37_79B9).wrapping_add(n as u64);
            let fresh = rng.gauss();
            let inside = |r: (f64, f64), v: f64| (v >= r.0 && v <= r.1) as usize;
            let iid = iid_runs(n, &mut rng);
            hits[0] += inside(type7(&iid), fresh);
            let (s1, now1) = lhs_runs(n, 1, seed);
            hits[1] += inside(type7(&s1), fresh);
            let (s3, now3) = lhs_runs(n, 3, seed);
            hits[2] += inside(type7(&s3), fresh);
            let now_iid = (proof::stats::range_point(&iid, 0.05), proof::stats::range_point(&iid, 0.95));
            hits[3] += inside(now_iid, fresh);
            hits[4] += inside(now1, fresh);
            hits[5] += inside(now3, fresh);
            // The inputs right, but the model off by something the range knows nothing of.
            let e = rng.gauss();
            hits[6] += inside(now3, fresh + 0.5 * e);
            hits[7] += inside(now3, fresh + e);
        }
        let pct = |i: usize| hits[i] as f64 / trials as f64 * 100.0;
        let label = expected_range_content(n) * 100.0;
        println!("{n} | {predicted} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {label:.1} | {:.1} | {:.1}", pct(0), pct(1), pct(2), pct(3), pct(4), pct(5), pct(6), pct(7));
        // The old ranges were what the audit worked out by hand.
        assert!((pct(0) - predicted).abs() < 2.5, "n {n}: the old range caught {:.1} %, audit {predicted}", pct(0));
        // What a range now says about itself is true, for plain sampling.
        assert!((pct(3) - label).abs() < 2.5, "n {n}: caught {:.1} %, label {label:.1}", pct(3));
        // And it never does worse than the old way.
        assert!(pct(3) + 1.0 >= pct(0), "n {n}");
        // A model error it wasn't told of costs real coverage, whatever n is.
        assert!(pct(7) < pct(5) - 8.0, "n {n}: {:.1} vs {:.1}", pct(7), pct(5));
    }
}
