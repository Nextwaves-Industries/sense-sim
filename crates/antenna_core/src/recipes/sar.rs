//! A SAR pre-check for body-worn devices: how much power ends up in tissue, averaged
//! over 1 g and 10 g as the limits are written. It reads the field an FDTD run leaves
//! in a plane through the antenna and the phantom.
//!
//! The field is only known in two planes through the antenna, so the standard's cube
//! is estimated from them: how it falls off in one plane times how it falls off in the
//! other. A hotter spot off those planes isn't seen. It's a design-stage check, compared
//! with a reference dipole in `fdtd_sar_reference_dipole` in the app, and not a compliance
//! measurement. Only an accredited lab can provide that.

use serde::{Deserialize, Serialize};

/// A limit: SAR averaged over a mass of tissue. Figures are from the published rules;
/// check the current edition for your market and for limbs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SarLimit {
    pub name: &'static str,
    pub w_per_kg: f64,
    pub grams: f64,
}

pub const FCC_1G: SarLimit = SarLimit { name: "FCC, head and body, 1 g", w_per_kg: 1.6, grams: 1.0 };
pub const ICNIRP_10G: SarLimit = SarLimit { name: "ICNIRP and EU, head and trunk, 10 g", w_per_kg: 2.0, grams: 10.0 };

/// The side of a cube of tissue of this mass, metres. Density 1000 kg/m³.
pub fn cube_side_m(grams: f64) -> f64 {
    (grams * 1e-3 / DENSITY).cbrt()
}

/// Tissue density used by the SAR standards, kg/m³.
pub const DENSITY: f64 = 1000.0;

/// |E|² on a grid in a plane through the tissue (peak amplitude, V/m), not necessarily even.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaneField {
    pub u: Vec<f64>,
    pub v: Vec<f64>,
    /// `e2[iu][iv]`.
    pub e2: Vec<Vec<f64>>,
    /// Inside the tissue at each point.
    pub inside: Vec<Vec<bool>>,
}

/// Local SAR (W/kg) from a peak-amplitude field: σ|E|²/(2ρ).
pub fn local_sar(sigma: f64, e2: f64) -> f64 {
    sigma * e2 / (2.0 * DENSITY)
}

/// The conductivity behind a loss tangent, S/m.
pub fn conductivity(er: f64, tan_d: f64, f: f64) -> f64 {
    std::f64::consts::TAU * f * crate::EPS0 * er * tan_d
}

/// The highest average of local SAR over a square of side `side_m` lying fully in the
/// tissue, and the highest single point. Uses a running sum over the grid.
pub fn peak_average(p: &PlaneField, sigma: f64, side_m: f64) -> Option<(f64, f64)> {
    let (nu, nv) = (p.u.len(), p.v.len());
    if nu < 2 || nv < 2 {
        return None;
    }
    let mut point: f64 = 0.0;
    let mut best: Option<f64> = None;
    for i in 0..nu {
        for j in 0..nv {
            if p.inside[i][j] {
                point = point.max(local_sar(sigma, p.e2[i][j]));
            }
        }
    }
    // Integral images over the cells between samples, so a window covers exactly from one
    // sample to another: SAR·area by the corners' mean, area, and area touching the outside.
    let mut sum = vec![vec![0.0; nv]; nu];
    let mut area = vec![vec![0.0; nv]; nu];
    let mut outside = vec![vec![0.0; nv]; nu];
    for i in 0..nu - 1 {
        for j in 0..nv - 1 {
            let a = (p.u[i + 1] - p.u[i]) * (p.v[j + 1] - p.v[j]);
            let corners = [(i, j), (i + 1, j), (i, j + 1), (i + 1, j + 1)];
            let all_in = corners.iter().all(|c| p.inside[c.0][c.1]);
            let s = if all_in { corners.iter().map(|c| local_sar(sigma, p.e2[c.0][c.1])).sum::<f64>() / 4.0 * a } else { 0.0 };
            let o = if all_in { 0.0 } else { a };
            sum[i + 1][j + 1] = s + sum[i][j + 1] + sum[i + 1][j] - sum[i][j];
            area[i + 1][j + 1] = a + area[i][j + 1] + area[i + 1][j] - area[i][j];
            outside[i + 1][j + 1] = o + outside[i][j + 1] + outside[i + 1][j] - outside[i][j];
        }
    }
    let box_of = |t: &Vec<Vec<f64>>, a: usize, b: usize, c: usize, d: usize| t[b][d] - t[a][d] - t[b][c] + t[a][c];
    for a in 0..nu {
        let Some(b) = (a..nu).find(|i| p.u[*i] - p.u[a] >= side_m - 1e-9) else { break };
        for c in 0..nv {
            let Some(d) = (c..nv).find(|j| p.v[*j] - p.v[c] >= side_m - 1e-9) else { break };
            // Fully in the tissue: a stray cell on the boundary is fine, a real hole isn't.
            if box_of(&outside, a, b, c, d) > 0.01 * box_of(&area, a, b, c, d) {
                continue;
            }
            let v = box_of(&sum, a, b, c, d) / box_of(&area, a, b, c, d).max(1e-18);
            best = Some(best.map_or(v, |x| x.max(v)));
        }
    }
    best.map(|b| (b, point))
}

/// The same plane on an even grid `step` apart, so a window can be the cube's exact size.
/// Between samples |E|² is taken as straight lines; points with any corner outside the
/// tissue count as outside.
pub fn resampled(p: &PlaneField, step: f64) -> PlaneField {
    let axis = |x: &[f64]| -> Vec<f64> {
        let n = (((x[x.len() - 1] - x[0]) / step).floor() as usize).max(1);
        (0..=n).map(|i| x[0] + i as f64 * step).collect()
    };
    let (u, v) = (axis(&p.u), axis(&p.v));
    let bracket = |x: &[f64], at: f64| -> (usize, f64) {
        let k = x.partition_point(|c| *c <= at).clamp(1, x.len() - 1);
        (k - 1, ((at - x[k - 1]) / (x[k] - x[k - 1]).max(1e-15)).clamp(0.0, 1.0))
    };
    let (mut e2, mut inside) = (Vec::with_capacity(u.len()), Vec::with_capacity(u.len()));
    for a in &u {
        let (i, s) = bracket(&p.u, *a);
        let (mut row_e, mut row_in) = (Vec::with_capacity(v.len()), Vec::with_capacity(v.len()));
        for b in &v {
            let (j, t) = bracket(&p.v, *b);
            let corner = |di: usize, dj: usize, w: f64| (w > 1e-9).then_some((p.e2[i + di][j + dj], p.inside[i + di][j + dj], w));
            let corners = [corner(0, 0, (1.0 - s) * (1.0 - t)), corner(1, 0, s * (1.0 - t)), corner(0, 1, (1.0 - s) * t), corner(1, 1, s * t)];
            row_e.push(corners.iter().flatten().map(|c| c.0 * c.2).sum());
            row_in.push(corners.iter().flatten().all(|c| c.1));
        }
        e2.push(row_e);
        inside.push(row_in);
    }
    PlaneField { u, v, e2, inside }
}

/// The highest average along one line of the plane, over `side_m` of tissue, at the `u`
/// nearest `at_u`. It's what the two planes share, and what they both already contain.
pub fn peak_line(p: &PlaneField, sigma: f64, at_u: f64, side_m: f64) -> Option<f64> {
    let i = (0..p.u.len()).min_by(|a, b| (p.u[*a] - at_u).abs().total_cmp(&(p.u[*b] - at_u).abs()))?;
    let mut best: Option<f64> = None;
    for a in 0..p.v.len() {
        let Some(b) = (a..p.v.len()).find(|j| p.v[*j] - p.v[a] >= side_m - 1e-9) else { break };
        if (a..=b).any(|j| !p.inside[i][j]) {
            continue;
        }
        let mut sum = 0.0;
        for j in a..b {
            sum += 0.5 * (local_sar(sigma, p.e2[i][j]) + local_sar(sigma, p.e2[i][j + 1])) * (p.v[j + 1] - p.v[j]);
        }
        let v = sum / (p.v[b] - p.v[a]).max(1e-15);
        best = Some(best.map_or(v, |x| x.max(v)));
    }
    best
}

/// The cube's average from two planes sharing a line through the hot spot. If the field
/// falls off independently in each direction, S(x,y,z) = g(x)·h(y)·k(z), then the planes
/// average to ⟨g⟩⟨h⟩ and ⟨h⟩⟨k⟩, their line to ⟨h⟩, and the cube to ⟨g⟩⟨h⟩⟨k⟩.
pub fn cube_from_planes(square_a: f64, square_b: f64, line: f64) -> f64 {
    if line > 0.0 { square_a * square_b / line } else { square_a.min(square_b) }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SarCheck {
    pub limit: SarLimit,
    /// Average power this limit's SAR is worked out for, watts.
    pub power_w: f64,
    pub sar_w_per_kg: f64,
    /// Share of the limit used; above 1 it fails.
    pub share_of_limit: f64,
    /// The largest average power that stays within the limit, watts.
    pub max_power_w: f64,
}

/// Scales SAR per accepted watt to a device's average power, and checks a limit.
pub fn check(sar_per_watt: f64, average_power_w: f64, limit: SarLimit) -> SarCheck {
    let sar = sar_per_watt * average_power_w;
    SarCheck { limit, power_w: average_power_w, sar_w_per_kg: sar, share_of_limit: sar / limit.w_per_kg, max_power_w: if sar_per_watt > 0.0 { limit.w_per_kg / sar_per_watt } else { f64::INFINITY } }
}

pub fn assumptions() -> Vec<String> {
    vec![
        "A design-stage check, not a compliance result: only an accredited lab's measurement can show compliance".into(),
        "A flat block of uniform tissue stands in for the body, and the field is read in planes through the antenna, so a hotter spot off those planes isn't seen".into(),
        "The standard's cube is estimated from the two planes, assuming the field falls off independently in each direction".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize, step: f64, f: impl Fn(f64, f64) -> f64) -> PlaneField {
        let u: Vec<f64> = (0..n).map(|i| i as f64 * step).collect();
        let v = u.clone();
        let e2 = u.iter().map(|a| v.iter().map(|b| f(*a, *b)).collect()).collect();
        let inside = vec![vec![true; n]; n];
        PlaneField { u, v, e2, inside }
    }

    #[test]
    fn a_uniform_field_gives_the_textbook_sar() {
        // E = 100 V/m peak, σ = 1 S/m: SAR = 1 · 10⁴ / 2000 = 5 W/kg.
        let p = grid(40, 0.001, |_, _| 1e4);
        let (avg, point) = peak_average(&p, 1.0, 0.01).unwrap();
        assert!((avg - 5.0).abs() < 1e-9, "{avg}");
        assert!((point - 5.0).abs() < 1e-9, "{point}");
    }

    #[test]
    fn the_cube_sizes_are_the_standards() {
        assert!((cube_side_m(1.0) - 0.01).abs() < 1e-9);
        assert!((cube_side_m(10.0) - 0.02154).abs() < 1e-4);
    }

    #[test]
    fn averaging_lowers_a_hot_spot_more_over_a_larger_mass() {
        // A field decaying into the tissue with a 15 mm skin depth, from v = 0.
        let p = grid(101, 0.001, |_, v| 1e4 * (-2.0 * v / 0.015).exp());
        let (g1, point) = peak_average(&p, 1.0, cube_side_m(1.0)).unwrap();
        let (g10, _) = peak_average(&p, 1.0, cube_side_m(10.0)).unwrap();
        assert!(point > g1 && g1 > g10, "{point} {g1} {g10}");
        // Mean of exp(−2v/δ) over 10 mm: (δ/2/10mm)(1 − e^(−20/15)) ≈ 0.552, of 5 W/kg.
        assert!((g1 - 5.0 * 0.552).abs() < 0.1, "{g1}");
    }

    #[test]
    fn squares_stay_in_the_tissue() {
        // A very hot spot just outside the tissue must not count.
        let mut p = grid(40, 0.001, |_, _| 1e4);
        for i in 0..40 {
            p.e2[i][0] = 1e9;
            p.inside[i][0] = false;
        }
        let (avg, point) = peak_average(&p, 1.0, 0.01).unwrap();
        assert!((avg - 5.0).abs() < 1e-9, "{avg}");
        assert!((point - 5.0).abs() < 1e-9, "{point}");
        // No tissue, no answer.
        p.inside = vec![vec![false; 40]; 40];
        assert!(peak_average(&p, 1.0, 0.01).is_none());
    }

    #[test]
    fn resampling_gives_windows_of_the_exact_size() {
        // A coarse uneven grid of a straight ramp: straight lines keep it exact.
        let (u, v) = (vec![0.0, 0.004, 0.01, 0.02, 0.03], vec![0.0, 0.003, 0.012, 0.03]);
        let e2 = u.iter().map(|_| v.iter().map(|b: &f64| 1e4 * (1.0 + b / 0.03)).collect()).collect();
        let inside = vec![vec![true; v.len()]; u.len()];
        let r = resampled(&PlaneField { u: u.clone(), v: v.clone(), e2, inside }, 0.0005);
        assert!((r.v[1] - r.v[0] - 0.0005).abs() < 1e-12);
        let j = r.v.iter().position(|x| (*x - 0.015).abs() < 1e-9).unwrap();
        assert!((r.e2[3][j] - 1.5e4).abs() < 1.0, "{}", r.e2[3][j]);
        // A point next to the outside doesn't borrow from it.
        let mut q = PlaneField { u, v, e2: vec![vec![1.0; 4]; 5], inside: vec![vec![true; 4]; 5] };
        q.inside[0][0] = false;
        let r = resampled(&q, 0.001);
        assert!(!r.inside[0][0] && !r.inside[0][1] && r.inside[0][3] && r.inside[4][0]);
    }

    #[test]
    fn the_cube_comes_out_of_two_planes() {
        // S = g(x)·h(y)·k(z): a known product, sampled in the two planes through the peak.
        let g = |x: f64| (-(x / 0.02f64).powi(2)).exp();
        let k = |z: f64| (-(z / 0.04f64).powi(2)).exp();
        let h = |y: f64| (-2.0 * y / 0.03).exp();
        let side = cube_side_m(1.0);
        let n = 121;
        let axis: Vec<f64> = (0..n).map(|i| -0.03 + i as f64 * 0.0005).collect();
        let depth: Vec<f64> = (0..n).map(|i| i as f64 * 0.0005).collect();
        // σ = 2000 S/m·... so local SAR equals the sampled value.
        let plane = |f: &dyn Fn(f64) -> f64| PlaneField { u: axis.clone(), v: depth.clone(), e2: axis.iter().map(|a| depth.iter().map(|d| f(*a) * h(*d)).collect()).collect(), inside: vec![vec![true; n]; n] };
        let (pa, pb) = (plane(&g), plane(&k));
        let (sa, _) = peak_average(&pa, 2000.0, side).unwrap();
        let (sb, _) = peak_average(&pb, 2000.0, side).unwrap();
        let line = peak_line(&pa, 2000.0, 0.0, side).unwrap();
        let cube = cube_from_planes(sa, sb, line);
        // The true cube, by brute force around the peak.
        let (mut sum, m) = (0.0, 41);
        for i in 0..m {
            for j in 0..m {
                for l in 0..m {
                    let (x, z, y) = (-side / 2.0 + side * i as f64 / 40.0, -side / 2.0 + side * j as f64 / 40.0, side * l as f64 / 40.0);
                    let w = |q: usize| if q == 0 || q == 40 { 0.5 } else { 1.0 };
                    sum += g(x) * k(z) * h(y) * w(i) * w(j) * w(l);
                }
            }
        }
        let truth = sum / 40.0f64.powi(3);
        assert!((cube - truth).abs() / truth < 0.01, "{cube} {truth}");
        assert!(sa > cube && sb > cube, "both planes alone overstate it");
    }

    #[test]
    fn conductivity_follows_the_loss_tangent() {
        // IEEE 1528 head tissue at 900 MHz: εr 41.5, σ 0.97 S/m, so tan δ ≈ 0.467.
        assert!((conductivity(41.5, 0.467, 900e6) - 0.97).abs() < 0.01);
    }

    #[test]
    fn checks_scale_with_power() {
        let c = check(10.0, 0.1, FCC_1G);
        assert!((c.sar_w_per_kg - 1.0).abs() < 1e-12);
        assert!((c.share_of_limit - 0.625).abs() < 1e-12);
        assert!((c.max_power_w - 0.16).abs() < 1e-12);
    }
}
