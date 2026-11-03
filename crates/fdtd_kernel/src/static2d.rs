//! A cross-section solved statically on the same grid weights the 3D solver uses: the exact
//! answer for what the grid thinks a line's capacitance is. A zero-thickness stripline has a
//! closed-form impedance, so this measures the subcell model's error and its convergence order
//! with no time stepping, pulse or port in the way.

use crate::edge::{self, Local, Pair};

/// Complete elliptic integral of the first kind, from the modulus (arithmetic-geometric mean).
pub fn ellip_k(k: f64) -> f64 {
    let (mut a, mut b) = (1.0f64, (1.0 - k * k).max(0.0).sqrt());
    for _ in 0..40 {
        let (na, nb) = ((a + b) * 0.5, (a * b).sqrt());
        a = na;
        b = nb;
    }
    std::f64::consts::FRAC_PI_2 / a
}

/// Zero-thickness strip of width `w` centred between plates `b` apart, air: Cohn's exact result.
pub fn stripline_z0_exact(w: f64, b: f64) -> f64 {
    let t = std::f64::consts::PI * w / (2.0 * b);
    let (k, kp) = (t.tanh(), 1.0 / t.cosh());
    crate::ETA0 / 4.0 * ellip_k(kp) / ellip_k(k)
}

/// A strip across `s` in the plane `eta = 0`, plates at the ends of `eta`, walls at the ends of `s`.
pub struct Section {
    pub s: Vec<f64>,
    pub eta: Vec<f64>,
    pub strip: (f64, f64),
    /// Use the static-field correction; false is the plain grid, metal snapped to the lines.
    pub corrected: bool,
}

impl Section {
    fn dual(c: &[f64], i: usize) -> (f64, f64) {
        let l = if i > 0 { (c[i] - c[i - 1]) * 0.5 } else { 0.0 };
        let r = if i + 1 < c.len() { (c[i + 1] - c[i]) * 0.5 } else { 0.0 };
        (l, r)
    }

    /// Capacitance per metre over ε, by conjugate gradients on the grid's own Laplacian.
    pub fn capacitance(&self) -> f64 {
        let (ns, ne) = (self.s.len(), self.eta.len());
        let plane = self.eta.iter().position(|y| y.abs() < 1e-12).expect("the strip's plane needs a line");
        let id = |i: usize, j: usize| i + ns * j;
        let (p, q) = self.strip;
        let on_strip = |i: usize| self.s[i] >= p - 1e-12 && self.s[i] <= q + 1e-12;
        // The metal keeps the line nearest its centre if it would otherwise fall between two.
        let any = (0..ns).any(on_strip);
        let snapped = (!any).then(|| (0..ns).min_by(|a, b| (self.s[*a] - (p + q) * 0.5).abs().total_cmp(&(self.s[*b] - (p + q) * 0.5).abs())).unwrap());
        let metal = |i: usize, j: usize| j == plane && (on_strip(i) || snapped == Some(i));
        let fixed = |i: usize, j: usize| i == 0 || j == 0 || i == ns - 1 || j == ne - 1 || metal(i, j);
        let (cp, cq) = match snapped {
            Some(i) => (self.s[i] - (q - p) * 0.5, self.s[i] + (q - p) * 0.5),
            None => (p, q),
        };
        let intervals = [(cp, cq)];
        let corrected = |pair: Pair, mid: f64, cell: f64| -> f64 {
            if !self.corrected {
                return 1.0;
            }
            let dist = (mid - cp).abs().min((mid - cq).abs());
            if dist > 2.5 * cell && !(mid > cp && mid < cq) {
                return 1.0;
            }
            let high = (mid - cq).abs() < (mid - cp).abs();
            let local: Local = edge::local_at(&intervals, 0, high, cell);
            edge::factor(local, pair)
        };
        // Weights of the segments along s and along eta.
        let mut wa = vec![0.0; ns * ne];
        let mut wb = vec![0.0; ns * ne];
        for j in 0..ne {
            for i in 0..ns {
                let (dn, up) = Self::dual(&self.eta, j);
                if i + 1 < ns {
                    let (a, b) = (self.s[i], self.s[i + 1]);
                    let near_plane = (self.eta[j]).abs() <= 2.5 * (b - a).max(dn + up);
                    let f = if near_plane {
                        corrected(Pair::Along { s0: a, s1: b, eta: self.eta[j], down: dn, up }, (a + b) * 0.5, (b - a).max(dn + up))
                    } else {
                        1.0
                    };
                    wa[id(i, j)] = f * (dn + up) / (b - a);
                }
                if j + 1 < ne {
                    let (l, r) = Self::dual(&self.s, i);
                    let (a, b) = (self.eta[j], self.eta[j + 1]);
                    // A segment reaching across the plane isn't one this model has: the plane
                    // always has a line on it.
                    let near_plane = a.abs().min(b.abs()) <= 2.5 * (b - a).max(l + r);
                    let f = if near_plane {
                        corrected(Pair::Across { s: self.s[i], eta0: a, eta1: b, left: l, right: r }, self.s[i], (l + r).max(b - a))
                    } else {
                        1.0
                    };
                    wb[id(i, j)] = f * (l + r) / (b - a);
                }
            }
        }
        // Metal at 1, outer walls at 0: CG on the free nodes.
        let mut x = vec![0.0; ns * ne];
        for j in 0..ne {
            for i in 0..ns {
                if metal(i, j) {
                    x[id(i, j)] = 1.0;
                }
            }
        }
        let apply = |v: &[f64], out: &mut [f64]| {
            for j in 0..ne {
                for i in 0..ns {
                    let n = id(i, j);
                    let mut y = 0.0;
                    if i + 1 < ns {
                        y += wa[n] * (v[n] - v[n + 1]);
                    }
                    if i > 0 {
                        y += wa[n - 1] * (v[n] - v[n - 1]);
                    }
                    if j + 1 < ne {
                        y += wb[n] * (v[n] - v[n + ns]);
                    }
                    if j > 0 {
                        y += wb[n - ns] * (v[n] - v[n - ns]);
                    }
                    out[n] = y;
                }
            }
        };
        let free = |n: usize| !fixed(n % ns, n / ns);
        let mut r = vec![0.0; ns * ne];
        apply(&x, &mut r);
        for n in 0..ns * ne {
            r[n] = if free(n) { -r[n] } else { 0.0 };
        }
        let mut d = r.clone();
        let mut ad = vec![0.0; ns * ne];
        let mut rr: f64 = r.iter().map(|v| v * v).sum();
        let start = rr.max(1e-300);
        for _ in 0..200_000 {
            if rr < 1e-22 * start {
                break;
            }
            apply(&d, &mut ad);
            for n in 0..ns * ne {
                if !free(n) {
                    ad[n] = 0.0;
                }
            }
            let dad: f64 = d.iter().zip(&ad).map(|(a, b)| a * b).sum();
            let alpha = rr / dad;
            for n in 0..ns * ne {
                x[n] += alpha * d[n];
                r[n] -= alpha * ad[n];
            }
            let next: f64 = r.iter().map(|v| v * v).sum();
            let beta = next / rr;
            rr = next;
            for n in 0..ns * ne {
                d[n] = r[n] + beta * d[n];
            }
        }
        // Charge on the metal is what the Laplacian leaves there.
        apply(&x, &mut ad);
        (0..ns * ne).filter(|n| metal(n % ns, n / ns)).map(|n| ad[n]).sum()
    }

    /// Characteristic impedance in air: `Z0 = η/(C′/ε)`.
    pub fn z0(&self) -> f64 {
        crate::ETA0 / self.capacitance()
    }
}

/// Lines from `lo` to `hi` at spacing `h`, one of them exactly at `at`.
pub fn lines_through(lo: f64, hi: f64, h: f64, at: f64) -> Vec<f64> {
    let first = ((lo - at) / h).ceil() as i64;
    let last = ((hi - at) / h).floor() as i64;
    let mut v: Vec<f64> = vec![lo];
    for n in first..=last {
        let x = at + n as f64 * h;
        if x > lo + h * 0.25 && x < hi - h * 0.25 {
            v.push(x);
        }
    }
    v.push(hi);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exact_impedance_matches_the_published_value() {
        // Cohn's tables give about 65 Ω for an air stripline with w/b = 1.
        let z = stripline_z0_exact(1.0, 1.0);
        assert!((z - 65.0).abs() < 3.0, "{z}");
    }

    fn error(w: f64, b: f64, h: f64, offset: f64, corrected: bool) -> (f64, f64) {
        let side = 8.0 * b;
        let s = lines_through(-side / 2.0, side / 2.0, h, offset);
        let eta = lines_through(-b / 2.0, b / 2.0, h, 0.0);
        let sec = Section { s, eta, strip: (-w / 2.0, w / 2.0), corrected };
        let z = sec.z0();
        (z, (z / stripline_z0_exact(w, b) - 1.0) * 100.0)
    }

    /// The go/no-go for the subcell model. A strip's edges fall anywhere between lines: the plain
    /// grid's answer swings with where they land, and the corrected grid's must not.
    #[test]
    fn subcell_edges_beat_the_plain_grid_on_an_exact_answer() {
        let (w, b) = (1.0, 1.0);
        println!("exact {:.3} ohm", stripline_z0_exact(w, b));
        let mut worst_plain: f64 = 0.0;
        let mut worst_fixed: f64 = 0.0;
        for cells in [4.0, 8.0, 16.0, 32.0] {
            let h = w / cells;
            let mut plain = Vec::new();
            let mut fixed = Vec::new();
            for t in [0.0, 0.17, 1.0 / 3.0, 0.5, 0.71, 0.9] {
                // The strip runs from -w/2, so this moves the lines against its edges by t cells.
                plain.push(error(w, b, h, t * h, false).1);
                fixed.push(error(w, b, h, t * h, true).1);
            }
            let spread = |v: &[f64]| v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min);
            let big = |v: &[f64]| v.iter().map(|e| e.abs()).fold(0.0, f64::max);
            println!("{cells:>3} cells across: plain worst {:6.2} % (spread {:5.2}), corrected worst {:6.2} % (spread {:5.2})", big(&plain), spread(&plain), big(&fixed), spread(&fixed));
            println!("     plain {plain:.2?}\n     fixed {fixed:.2?}");
            if cells == 8.0 {
                worst_plain = big(&plain);
                worst_fixed = big(&fixed);
            }
        }
        assert!(worst_fixed < worst_plain / 3.0, "{worst_fixed} vs {worst_plain}");
    }
}
