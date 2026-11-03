//! Subcell metal from the local static field.
//!
//! Near a sharp metal edge the field goes as `r^-1/2`, and a grid that samples it at one point
//! per cell gets the capacitance and inductance of that cell wrong, which is the first-order
//! error that makes a flat trace converge slowly. Across a cell, though, the field is close to
//! the static one, and a static field in a cross-section is known in closed form: it comes from
//! a complex potential `w`, with `Re w` the potential (constant on the metal) and `Im w` the
//! flux function. So for one primal segment `P` and the dual segment `D` crossing it,
//!
//! ```text
//!     voltage along P = Δ Re w          flux through D = Δ Im w
//! ```
//!
//! and the true `Mε` of that edge is `ε·ΔIm/ΔRe` where the grid would use `ε·|D|/|P|`. Their
//! ratio is [`factor`], `F`. The magnetic problem is the same one with the roles exchanged, so
//! the face over `P` gets `Mμ` divided by the same `F`. `L′C′ = με` then survives on the grid
//! exactly, so the correction adds no dispersion to a TEM line.
//!
//! The thirds rule is what you get by asking for `F = 1` instead: it places the mesh line where
//! no correction is needed. This computes the correction for wherever the line happens to be.

use num_complex::Complex64 as C;

/// A static field around metal in the plane `η = 0`, symmetric about that plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Local {
    /// A half-plane of metal ending at `b`, on the low side of `s` if `metal_low`.
    Half { b: f64, metal_low: bool },
    /// A strip of metal from `p` to `q`, with free space either side.
    Strip { p: f64, q: f64 },
    /// A gap from `p` to `q` between two wide pieces of metal at different potentials.
    Gap { p: f64, q: f64 },
}

impl Local {
    /// The complex potential at `(s, η)` for `η ≥ 0`. Zero real part on the metal, or a constant
    /// on each side of a gap; the imaginary part only matters through differences.
    pub fn w(self, s: f64, eta: f64) -> C {
        let eta = eta.abs();
        match self {
            Local::Half { b, metal_low } => {
                let x = if metal_low { s - b } else { b - s };
                C::new(x, eta).sqrt()
            }
            Local::Strip { p, q } => {
                let (c, a) = ((p + q) * 0.5, (q - p) * 0.5);
                acosh_uhp(C::new((s - c) / a, eta / a))
            }
            Local::Gap { p, q } => {
                let (c, g) = ((p + q) * 0.5, (q - p) * 0.5);
                asin_uhp(C::new((s - c) / g, eta / g))
            }
        }
    }

    /// `s` if it's off the metal in the plane, otherwise the nearest tip.
    fn free_point(self, s: f64) -> f64 {
        match self {
            Local::Half { b, metal_low: true } => s.max(b),
            Local::Half { b, metal_low: false } => s.min(b),
            Local::Strip { p, q } if s > p && s < q => {
                if s - p < q - s { p } else { q }
            }
            Local::Strip { .. } => s,
            Local::Gap { p, q } => s.clamp(p, q),
        }
    }
}

/// `acosh` for `Im z ≥ 0`, keeping `Re ≥ 0`, `Im` in `0..π`: analytic in the upper half-plane.
fn acosh_uhp(z: C) -> C {
    (z + (z - 1.0).sqrt() * (z + 1.0).sqrt()).ln()
}

/// `asin` for `Im z ≥ 0`, with `Im` staying `≥ 0`: analytic in the upper half-plane.
fn asin_uhp(z: C) -> C {
    let j = C::new(0.0, 1.0);
    // asin z = -j·ln(jz + √(1-z²)) with √(1-z²) = √(1-z)·√(1+z) continuous over the half-plane.
    let root = (C::new(1.0, 0.0) - z).sqrt() * (C::new(1.0, 0.0) + z).sqrt();
    let v = -j * (j * z + root).ln();
    // A conjugate-symmetric pair: the sign of the imaginary part is only a choice of branch.
    if v.im < 0.0 { C::new(v.re, -v.im) } else { v }
}

/// The largest departure from the plain grid value that is taken at face value. An edge almost
/// reaching the next line legitimately asks for about 4.5, and the factor grows as that gap closes;
/// past this the time step would pay more than the accuracy is worth.
pub const F_MAX: f64 = 6.0;
pub const F_MIN: f64 = 1.0 / F_MAX;

/// A segment of the cross-section and the dual segment crossing it, about the sheet's plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pair {
    /// `P` along `s` at height `eta`; `D` along `η` through its middle, `down` below and `up` above.
    Along { s0: f64, s1: f64, eta: f64, down: f64, up: f64 },
    /// `P` along `η` at `s`; `D` along `s` through its middle, `left` and `right` of it.
    Across { s: f64, eta0: f64, eta1: f64, left: f64, right: f64 },
}

/// The ratio of the true `Mε` of a segment to the grid's plain value. 1 where the field along
/// the segment nearly vanishes: the ratio says nothing about a voltage that isn't there.
pub fn factor(local: Local, pair: Pair) -> f64 {
    // `along` is how much the potential changes along P; `sideways` how much the flux function
    // does. Where the second dwarfs the first the field crosses P rather than following it.
    let (p_len, d_len, along, sideways, flux) = match pair {
        Pair::Along { s0, s1, eta, down, up } => {
            let mid = (s0 + s1) * 0.5;
            let (w0, w1) = (local.w(s0, eta), local.w(s1, eta));
            let re = (w1.re - w0.re).abs();
            if eta.abs() < 1e-30 {
                // In the plane the field off the metal runs exactly along P, by symmetry, so
                // there's nothing sideways. The dual face crosses the plane, and where that falls
                // on the metal it has to go round the tip instead: the charge up to the tip
                // belongs to the metal's own node, and the free node beyond has none. Without
                // this an edge more than half way along P reads far too little capacitance.
                let at = local.w(local.free_point(mid), 0.0).im;
                let im = (local.w(mid, up).im - at).abs() + (local.w(mid, down).im - at).abs();
                (s1 - s0, down + up, re, 0.0, im)
            } else {
                let (a, b) = (eta.abs() - down, eta.abs() + up);
                let im = (local.w(mid, b).im - local.w(mid, a.max(0.0)).im).abs();
                (s1 - s0, down + up, re, (w1.im - w0.im).abs(), im)
            }
        }
        Pair::Across { s, eta0, eta1, left, right } => {
            let (a, b) = (eta0.abs(), eta1.abs());
            let mid = (a + b) * 0.5;
            let (w0, w1) = (local.w(s, a), local.w(s, b));
            let im = (local.w(s + right, mid).im - local.w(s - left, mid).im).abs();
            ((b - a).abs(), left + right, (w1.re - w0.re).abs(), (w1.im - w0.im).abs(), im)
        }
    };
    if !(p_len > 0.0 && d_len > 0.0) || along < 0.02 * sideways || along <= 0.0 || flux <= 0.0 {
        return 1.0;
    }
    ((flux / d_len) / (along / p_len)).clamp(F_MIN, F_MAX)
}

/// Picks the static field for a boundary at `b` on a line whose metal is `intervals`, sorted and
/// not overlapping. `reach` is how far the local field is wanted: a strip or gap wider than
/// several of those looks like a half-plane from there.
pub fn local_at(intervals: &[(f64, f64)], index: usize, at_high_end: bool, reach: f64) -> Local {
    let (p, q) = intervals[index];
    let width = q - p;
    let b = if at_high_end { q } else { p };
    let gap = if at_high_end {
        intervals.get(index + 1).map(|n| (q, n.0))
    } else if index > 0 {
        Some((intervals[index - 1].1, p))
    } else {
        None
    };
    let gap_width = gap.map_or(f64::INFINITY, |g| g.1 - g.0);
    let narrow = 6.0 * reach;
    match (width < narrow, gap_width < narrow) {
        // Both narrow is a comb. Whichever is narrower sets the field near this edge.
        (true, false) => Local::Strip { p, q },
        (false, true) => Local::Gap { p: gap.unwrap().0, q: gap.unwrap().1 },
        (true, true) if width < gap_width => Local::Strip { p, q },
        (true, true) => Local::Gap { p: gap.unwrap().0, q: gap.unwrap().1 },
        (false, false) => Local::Half { b, metal_low: at_high_end },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closed forms hold at a metal edge: flux adds up on the metal, the potential is flat.
    #[test]
    fn potentials_are_flat_on_the_metal() {
        let half = Local::Half { b: 0.0, metal_low: true };
        assert!(half.w(-1.0, 0.0).re.abs() < 1e-12);
        assert!(half.w(-5.0, 0.0).re.abs() < 1e-12);
        assert!(half.w(2.0, 0.0).im.abs() < 1e-12);
        let strip = Local::Strip { p: -1.0, q: 1.0 };
        for s in [-0.9, -0.2, 0.3, 0.99] {
            assert!(strip.w(s, 0.0).re.abs() < 1e-9, "{s}: {}", strip.w(s, 0.0));
        }
        // All of the charge on the strip: from one edge across the top to the other edge is π.
        assert!(((strip.w(-1.0, 0.0).im - strip.w(1.0, 0.0).im).abs() - std::f64::consts::PI).abs() < 1e-6);
        let gap = Local::Gap { p: -1.0, q: 1.0 };
        for s in [1.2, 3.0] {
            assert!((gap.w(s, 0.0).re - std::f64::consts::FRAC_PI_2).abs() < 1e-9, "{s}: {}", gap.w(s, 0.0));
            assert!((gap.w(-s, 0.0).re + std::f64::consts::FRAC_PI_2).abs() < 1e-9, "{s}: {}", gap.w(-s, 0.0));
        }
        for s in [-0.5, 0.0, 0.7] {
            assert!(gap.w(s, 0.0).im.abs() < 1e-9, "{s}: {}", gap.w(s, 0.0));
        }
    }

    /// Worked by hand: an edge exactly on a mesh line reads 0.6436 of the plain value, both for
    /// the edge along the sheet and for the one standing on the metal's end.
    #[test]
    fn an_edge_on_a_mesh_line_loses_a_third() {
        let half = Local::Half { b: 0.0, metal_low: true };
        let along = factor(half, Pair::Along { s0: 0.0, s1: 1.0, eta: 0.0, down: 0.5, up: 0.5 });
        assert!((along - 0.6436).abs() < 1e-3, "{along}");
        let across = factor(half, Pair::Across { s: 0.0, eta0: 0.0, eta1: 1.0, left: 0.5, right: 0.5 });
        assert!((across - 0.6436).abs() < 1e-3, "{across}");
    }

    /// The thirds rule, recovered: some placement of the edge needs no correction at all.
    #[test]
    fn the_thirds_rule_is_where_the_correction_vanishes() {
        let f = |t: f64| factor(Local::Half { b: t, metal_low: true }, Pair::Along { s0: 0.0, s1: 1.0, eta: 0.0, down: 0.5, up: 0.5 });
        assert!(f(0.0) < 1.0 && f(0.5) > 1.0, "{} {}", f(0.0), f(0.5));
        let (mut lo, mut hi) = (0.0, 0.5);
        for _ in 0..50 {
            let m = (lo + hi) * 0.5;
            if f(m) < 1.0 {
                lo = m;
            } else {
                hi = m;
            }
        }
        // Metal this far along the first cell: about a third is the rule of thumb.
        assert!((0.2..0.4).contains(&lo), "{lo}");
    }

    /// Far from the metal the field is smooth, so the plain grid value is already right.
    #[test]
    fn far_from_the_edge_nothing_changes() {
        let half = Local::Half { b: 0.0, metal_low: true };
        let f = factor(half, Pair::Along { s0: 8.0, s1: 9.0, eta: 3.0, down: 0.5, up: 0.5 });
        assert!((f - 1.0).abs() < 0.02, "{f}");
    }
}
