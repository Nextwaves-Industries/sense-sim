//! Quadrature rules: Gauss-Legendre on an interval, symmetric rules on a triangle.

use std::sync::LazyLock;

/// Gauss-Legendre `(node, weight)` on [-1, 1].
pub const GL3: [(f64, f64); 3] = [(-0.774_596_669_241_483_4, 0.555_555_555_555_555_6), (0.0, 0.888_888_888_888_888_9), (0.774_596_669_241_483_4, 0.555_555_555_555_555_6)];
pub const GL4: [(f64, f64); 4] = [
    (-0.861_136_311_594_052_6, 0.347_854_845_137_453_8),
    (-0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.339_981_043_584_856_3, 0.652_145_154_862_546_1),
    (0.861_136_311_594_052_6, 0.347_854_845_137_453_8),
];
pub const GL6: [(f64, f64); 6] = [
    (-0.932_469_514_203_152_1, 0.171_324_492_379_170_4),
    (-0.661_209_386_466_264_5, 0.360_761_573_048_138_6),
    (-0.238_619_186_083_196_9, 0.467_913_934_572_691_0),
    (0.238_619_186_083_196_9, 0.467_913_934_572_691_0),
    (0.661_209_386_466_264_5, 0.360_761_573_048_138_6),
    (0.932_469_514_203_152_1, 0.171_324_492_379_170_4),
];

/// Maps a rule onto `[a, b]`.
pub fn on_interval(rule: &[(f64, f64)], a: f64, b: f64) -> impl Iterator<Item = (f64, f64)> + '_ {
    let (mid, half) = (0.5 * (a + b), 0.5 * (b - a));
    rule.iter().map(move |&(x, w)| (mid + half * x, w * half))
}

/// A point of a triangle rule: barycentric coordinates and a weight (weights sum to 1).
pub type TriPoint = ([f64; 3], f64);

/// Degree 2, edge-free.
pub const TRI3: [TriPoint; 3] = [([2.0 / 3.0, 1.0 / 6.0, 1.0 / 6.0], 1.0 / 3.0), ([1.0 / 6.0, 2.0 / 3.0, 1.0 / 6.0], 1.0 / 3.0), ([1.0 / 6.0, 1.0 / 6.0, 2.0 / 3.0], 1.0 / 3.0)];

fn expand(center: Option<f64>, s21: &[(f64, f64, f64)], s111: &[(f64, f64, f64, f64)]) -> Vec<TriPoint> {
    let mut v = Vec::new();
    if let Some(w) = center {
        v.push(([1.0 / 3.0; 3], w));
    }
    for &(a, b, w) in s21 {
        v.push(([a, b, b], w));
        v.push(([b, a, b], w));
        v.push(([b, b, a], w));
    }
    for &(a, b, c, w) in s111 {
        for p in [[a, b, c], [a, c, b], [b, a, c], [b, c, a], [c, a, b], [c, b, a]] {
            v.push((p, w));
        }
    }
    v
}

/// Dunavant degree 4.
pub static TRI6: LazyLock<Vec<TriPoint>> = LazyLock::new(|| expand(None, &[(0.108_103_018_168_070, 0.445_948_490_915_965, 0.223_381_589_678_011), (0.816_847_572_980_459, 0.091_576_213_509_771, 0.109_951_743_655_322)], &[]));

/// Dunavant degree 5.
pub static TRI7: LazyLock<Vec<TriPoint>> = LazyLock::new(|| expand(Some(0.225), &[(0.059_715_871_789_770, 0.470_142_064_105_115, 0.132_394_152_788_506), (0.797_426_985_353_087, 0.101_286_507_323_456, 0.125_939_180_544_827)], &[]));

/// Dunavant degree 8.
pub static TRI16: LazyLock<Vec<TriPoint>> = LazyLock::new(|| {
    expand(
        Some(0.144_315_607_677_787),
        &[(0.081_414_823_414_554, 0.459_292_588_292_723, 0.095_091_634_267_285), (0.658_861_384_496_480, 0.170_569_307_751_760, 0.103_217_370_534_718), (0.898_905_543_365_938, 0.050_547_228_317_031, 0.032_458_497_623_198)],
        &[(0.008_394_777_409_958, 0.263_112_829_634_638, 0.728_492_392_955_404, 0.027_230_314_174_435)],
    )
});

#[cfg(test)]
mod tests {
    use super::*;

    /// ∫λ₀^a λ₁^b dA over a triangle of area 1 is 2·a!·b!/(a+b+2)!.
    fn exact(a: u32, b: u32) -> f64 {
        let f = |n: u32| (1..=n).map(|k| k as f64).product::<f64>();
        2.0 * f(a) * f(b) / f(a + b + 2)
    }

    fn check(rule: &[TriPoint], degree: u32) {
        for a in 0..=degree {
            for b in 0..=(degree - a) {
                let got: f64 = rule.iter().map(|(p, w)| w * p[0].powi(a as i32) * p[1].powi(b as i32)).sum();
                assert!((got - exact(a, b)).abs() < 1e-11, "degree {a},{b}: {got} vs {}", exact(a, b));
            }
        }
    }

    #[test]
    fn triangle_rules_reach_their_degree() {
        check(&TRI3, 2);
        check(&TRI6, 4);
        check(&TRI7, 5);
        check(&TRI16, 8);
    }

    #[test]
    fn line_rules_reach_their_degree() {
        for (rule, degree) in [(&GL3[..], 5), (&GL4[..], 7), (&GL6[..], 11)] {
            for d in 0..=degree {
                let got: f64 = on_interval(rule, 0.0, 2.0).map(|(s, w)| w * s.powi(d)).sum();
                let want = 2f64.powi(d + 1) / (d + 1) as f64;
                assert!((got - want).abs() < 1e-11, "x^{d}: {got} vs {want}");
            }
        }
    }
}
