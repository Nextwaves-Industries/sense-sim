//! Position solvers: TWR multilateration, TDoA hyperbolic, RSSI weighted
//! trilateration, NLOS outlier rejection, DOP and a constant-velocity Kalman
//! tracker. Everything works in metres, Y up; "2D" means the tag height is
//! known and only X/Z are solved, which is how most indoor RTLS run.

use bevy::math::DVec3;

#[derive(Clone, Debug)]
pub struct Fix {
    pub pos: DVec3,
    /// RMS of the measurement residuals at the solution (m).
    pub residual: f64,
    /// Indices of the measurements used (after outlier rejection).
    pub used: Vec<usize>,
    pub dop: f64,
}

/// Unknowns: X/Z with known Y, or full 3D.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dim {
    Plane(f64),
    Space,
}

impl Dim {
    fn n(self) -> usize {
        match self {
            Dim::Plane(_) => 2,
            Dim::Space => 3,
        }
    }
    fn to_point(self, x: &[f64; 3]) -> DVec3 {
        match self {
            Dim::Plane(y) => DVec3::new(x[0], y, x[1]),
            Dim::Space => DVec3::new(x[0], x[1], x[2]),
        }
    }
    fn from_point(self, p: DVec3) -> [f64; 3] {
        match self {
            Dim::Plane(_) => [p.x, p.z, 0.0],
            Dim::Space => [p.x, p.y, p.z],
        }
    }
    /// Gradient of |p - a| w.r.t. the unknowns.
    fn grad(self, p: DVec3, a: DVec3) -> [f64; 3] {
        let d = p - a;
        let l = d.length().max(1e-9);
        match self {
            Dim::Plane(_) => [d.x / l, d.z / l, 0.0],
            Dim::Space => [d.x / l, d.y / l, d.z / l],
        }
    }
}

/// Solves `A x = b` for n <= 3 (Gaussian elimination with partial pivoting).
fn solve_small(n: usize, mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for c in 0..n {
        let piv = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[piv][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, piv);
        b.swap(c, piv);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
            for k in c..n {
                a[r][k] -= f * a[c][k];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = [0.0; 3];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|k| a[r][k] * x[k]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    Some(x)
}

/// Inverse of a small symmetric matrix's diagonal sum: sqrt(trace((HᵀH)⁻¹)).
fn dop_from_rows(n: usize, rows: &[[f64; 3]]) -> f64 {
    let mut m = [[0.0; 3]; 3];
    for r in rows {
        for i in 0..n {
            for j in 0..n {
                m[i][j] += r[i] * r[j];
            }
        }
    }
    let mut tr = 0.0;
    for k in 0..n {
        let mut e = [0.0; 3];
        e[k] = 1.0;
        match solve_small(n, m, e) {
            Some(col) => tr += col[k],
            None => return f64::INFINITY,
        }
    }
    tr.max(0.0).sqrt()
}

/// Levenberg–Marquardt on residuals `r_i(x)` with gradients.
fn lm(
    dim: Dim,
    init: DVec3,
    m: usize,
    weights: &[f64],
    eval: impl Fn(DVec3, usize) -> (f64, [f64; 3]),
) -> Option<(DVec3, f64)> {
    let n = dim.n();
    let mut x = dim.from_point(init);
    let cost = |x: &[f64; 3]| -> f64 {
        let p = dim.to_point(x);
        (0..m).map(|i| weights[i] * eval(p, i).0.powi(2)).sum()
    };
    let mut c = cost(&x);
    let mut lambda = 1e-3;
    for _ in 0..60 {
        let p = dim.to_point(&x);
        let mut jtj = [[0.0; 3]; 3];
        let mut jtr = [0.0; 3];
        for i in 0..m {
            let (r, g) = eval(p, i);
            let w = weights[i];
            for a in 0..n {
                jtr[a] += w * g[a] * r;
                for b in 0..n {
                    jtj[a][b] += w * g[a] * g[b];
                }
            }
        }
        let mut improved = false;
        for _ in 0..10 {
            let mut a = jtj;
            for k in 0..n {
                a[k][k] += lambda * (1.0 + jtj[k][k]);
            }
            let Some(dx) = solve_small(n, a, [-jtr[0], -jtr[1], -jtr[2]]) else {
                lambda *= 10.0;
                continue;
            };
            let mut xn = x;
            for k in 0..n {
                xn[k] += dx[k];
            }
            let cn = cost(&xn);
            if cn.is_finite() && cn <= c {
                let step: f64 = dx.iter().take(n).map(|v| v * v).sum::<f64>().sqrt();
                x = xn;
                c = cn;
                lambda = (lambda * 0.3).max(1e-9);
                improved = true;
                if step < 1e-6 {
                    let wsum: f64 = weights.iter().sum::<f64>().max(1e-12);
                    return Some((dim.to_point(&x), (c / wsum).sqrt()));
                }
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    let wsum: f64 = weights.iter().sum::<f64>().max(1e-12);
    let p = dim.to_point(&x);
    p.is_finite().then_some((p, (c / wsum).sqrt()))
}

fn centroid(points: impl Iterator<Item = DVec3>) -> DVec3 {
    let (mut s, mut n) = (DVec3::ZERO, 0.0);
    for p in points {
        s += p;
        n += 1.0;
    }
    if n > 0.0 { s / n } else { DVec3::ZERO }
}

/// Starting point: weighted toward the anchors with the shortest ranges.
fn initial_guess(dim: Dim, anchors: &[DVec3], ranges: &[f64], prior: Option<DVec3>) -> DVec3 {
    if let Some(p) = prior {
        return p;
    }
    let mut s = DVec3::ZERO;
    let mut w = 0.0;
    for (a, r) in anchors.iter().zip(ranges) {
        let k = 1.0 / (r.max(0.1)).powi(2);
        s += *a * k;
        w += k;
    }
    let mut p = if w > 0.0 { s / w } else { centroid(anchors.iter().copied()) };
    if let Dim::Plane(y) = dim {
        p.y = y;
    } else {
        // Anchors are usually mounted high; start below them.
        p.y = (p.y - 1.0).max(0.0);
    }
    p
}

pub fn min_measurements(dim: Dim) -> usize {
    dim.n() + 1
}

/// Two-way-ranging multilateration. `anchors[i]` is ranged as `ranges[i]`.
pub fn solve_twr(dim: Dim, anchors: &[DVec3], ranges: &[f64], sigmas: &[f64], prior: Option<DVec3>) -> Option<Fix> {
    let m = anchors.len();
    if m < dim.n() || m != ranges.len() {
        return None;
    }
    let w: Vec<f64> = sigmas.iter().map(|s| 1.0 / s.max(0.01).powi(2)).collect();
    let init = initial_guess(dim, anchors, ranges, prior);
    let (pos, residual) = lm(dim, init, m, &w, |p, i| ((p - anchors[i]).length() - ranges[i], dim.grad(p, anchors[i])))?;
    let rows: Vec<[f64; 3]> = anchors.iter().map(|a| dim.grad(pos, *a)).collect();
    Some(Fix { pos, residual, used: (0..m).collect(), dop: dop_from_rows(dim.n(), &rows) })
}

/// TWR with NLOS/outlier rejection: when the fit is poor and there is
/// redundancy, drop the measurement(s) whose removal best explains the rest.
pub fn solve_twr_robust(
    dim: Dim,
    anchors: &[DVec3],
    ranges: &[f64],
    sigmas: &[f64],
    prior: Option<DVec3>,
    gate: f64,
) -> Option<Fix> {
    let best = solve_twr(dim, anchors, ranges, sigmas, prior)?;
    let need = min_measurements(dim);
    if best.residual <= gate || anchors.len() <= need {
        return Some(best);
    }
    let mut best = best;
    let mut keep: Vec<usize> = (0..anchors.len()).collect();
    // Greedy: drop one at a time while it helps and redundancy remains.
    while keep.len() > need && best.residual > gate {
        let mut round: Option<(Fix, usize)> = None;
        for drop in 0..keep.len() {
            let idx: Vec<usize> = keep.iter().enumerate().filter(|(k, _)| *k != drop).map(|(_, i)| *i).collect();
            let a: Vec<DVec3> = idx.iter().map(|&i| anchors[i]).collect();
            let r: Vec<f64> = idx.iter().map(|&i| ranges[i]).collect();
            let s: Vec<f64> = idx.iter().map(|&i| sigmas[i]).collect();
            if let Some(mut f) = solve_twr(dim, &a, &r, &s, prior.or(Some(best.pos))) {
                f.used = idx;
                if round.as_ref().is_none_or(|(rf, _)| f.residual < rf.residual) {
                    round = Some((f, drop));
                }
            }
        }
        match round {
            Some((f, drop)) if f.residual < best.residual * 0.7 => {
                keep.remove(drop);
                best = f;
            }
            _ => break,
        }
    }
    Some(best)
}

/// TDoA: `diffs[i]` = |p - anchors[i]| - |p - reference| (metres).
pub fn solve_tdoa(dim: Dim, reference: DVec3, anchors: &[DVec3], diffs: &[f64], sigma: f64, prior: Option<DVec3>) -> Option<Fix> {
    let m = anchors.len();
    if m < dim.n() || m != diffs.len() {
        return None;
    }
    let mut all = anchors.to_vec();
    all.push(reference);
    let init = prior.unwrap_or_else(|| {
        let mut c = centroid(all.iter().copied());
        match dim {
            Dim::Plane(y) => c.y = y,
            Dim::Space => c.y = (c.y - 1.0).max(0.0),
        }
        c
    });
    let w = vec![1.0 / sigma.max(0.01).powi(2); m];
    let (pos, residual) = lm(dim, init, m, &w, |p, i| {
        let r = (p - anchors[i]).length() - (p - reference).length() - diffs[i];
        let gi = dim.grad(p, anchors[i]);
        let gr = dim.grad(p, reference);
        (r, [gi[0] - gr[0], gi[1] - gr[1], gi[2] - gr[2]])
    })?;
    let gr = dim.grad(pos, reference);
    let rows: Vec<[f64; 3]> = anchors
        .iter()
        .map(|a| {
            let g = dim.grad(pos, *a);
            [g[0] - gr[0], g[1] - gr[1], g[2] - gr[2]]
        })
        .collect();
    Some(Fix { pos, residual, used: (0..m).collect(), dop: dop_from_rows(dim.n(), &rows) })
}

/// Horizontal (or 3D) dilution of precision for ranging from `anchors` at `p`.
pub fn dop(dim: Dim, anchors: &[DVec3], p: DVec3) -> f64 {
    if anchors.len() < dim.n() {
        return f64::INFINITY;
    }
    let rows: Vec<[f64; 3]> = anchors.iter().map(|a| dim.grad(p, *a)).collect();
    dop_from_rows(dim.n(), &rows)
}

/// Constant-velocity Kalman filter on X/Z (Y is low-pass filtered).
#[derive(Clone, Debug)]
pub struct Kalman {
    /// [x, z, vx, vz]
    pub x: [f64; 4],
    pub p: [[f64; 4]; 4],
    pub y: f64,
    pub accel_sigma: f64,
    initialised: bool,
}

impl Kalman {
    pub fn new(accel_sigma: f64) -> Self {
        Self { x: [0.0; 4], p: [[0.0; 4]; 4], y: 0.0, accel_sigma, initialised: false }
    }

    pub fn reset(&mut self) {
        self.initialised = false;
    }

    pub fn position(&self) -> DVec3 {
        DVec3::new(self.x[0], self.y, self.x[1])
    }

    pub fn predict(&mut self, dt: f64) {
        if !self.initialised || dt <= 0.0 {
            return;
        }
        let x = &mut self.x;
        x[0] += x[2] * dt;
        x[1] += x[3] * dt;
        // P = F P Fᵀ + Q (F: identity + dt on the pos/vel coupling).
        let mut f = [[0.0; 4]; 4];
        for (i, row) in f.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        f[0][2] = dt;
        f[1][3] = dt;
        let p = self.p;
        let mut fp = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                fp[i][j] = (0..4).map(|k| f[i][k] * p[k][j]).sum();
            }
        }
        let mut np = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                np[i][j] = (0..4).map(|k| fp[i][k] * f[j][k]).sum();
            }
        }
        let q = self.accel_sigma.powi(2);
        let (dt2, dt3, dt4) = (dt * dt, dt.powi(3) / 2.0, dt.powi(4) / 4.0);
        for a in 0..2 {
            np[a][a] += q * dt4;
            np[a][a + 2] += q * dt3;
            np[a + 2][a] += q * dt3;
            np[a + 2][a + 2] += q * dt2;
        }
        self.p = np;
    }

    /// Position measurement with standard deviation `sigma` (m).
    pub fn update(&mut self, z: DVec3, sigma: f64) {
        if !self.initialised {
            self.x = [z.x, z.z, 0.0, 0.0];
            self.y = z.y;
            self.p = [[0.0; 4]; 4];
            self.p[0][0] = sigma * sigma;
            self.p[1][1] = sigma * sigma;
            self.p[2][2] = 1.0;
            self.p[3][3] = 1.0;
            self.initialised = true;
            return;
        }
        let r = sigma * sigma;
        // H selects x and z; S = P[0..2][0..2] + R.
        let s = [[self.p[0][0] + r, self.p[0][1]], [self.p[1][0], self.p[1][1] + r]];
        let det = s[0][0] * s[1][1] - s[0][1] * s[1][0];
        if det.abs() < 1e-15 {
            return;
        }
        let si = [[s[1][1] / det, -s[0][1] / det], [-s[1][0] / det, s[0][0] / det]];
        let mut k = [[0.0; 2]; 4];
        for (i, row) in k.iter_mut().enumerate() {
            for j in 0..2 {
                row[j] = self.p[i][0] * si[0][j] + self.p[i][1] * si[1][j];
            }
        }
        let innov = [z.x - self.x[0], z.z - self.x[1]];
        for i in 0..4 {
            self.x[i] += k[i][0] * innov[0] + k[i][1] * innov[1];
        }
        let p = self.p;
        for i in 0..4 {
            for j in 0..4 {
                self.p[i][j] = p[i][j] - k[i][0] * p[0][j] - k[i][1] * p[1][j];
            }
        }
        self.y += 0.3 * (z.y - self.y);
    }
}

/// Free-space path loss in dB at `d` metres and `freq_mhz`.
pub fn fspl_db(d: f64, freq_mhz: f64) -> f64 {
    20.0 * d.max(0.01).log10() + 20.0 * freq_mhz.log10() - 27.55
}

/// Log-distance model inverse: RSSI -> distance.
pub fn rssi_to_distance(rssi: f64, rssi_at_1m: f64, exponent: f64) -> f64 {
    10f64.powf((rssi_at_1m - rssi) / (10.0 * exponent.max(0.5)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<DVec3> {
        vec![
            DVec3::new(0.0, 2.5, 0.0),
            DVec3::new(10.0, 2.8, 0.0),
            DVec3::new(10.0, 2.5, 8.0),
            DVec3::new(0.0, 2.8, 8.0),
        ]
    }

    #[test]
    fn twr_exact_plane() {
        let a = square();
        let truth = DVec3::new(3.2, 1.0, 5.1);
        let r: Vec<f64> = a.iter().map(|p| (*p - truth).length()).collect();
        let f = solve_twr(Dim::Plane(1.0), &a, &r, &[0.1; 4], None).unwrap();
        assert!((f.pos - truth).length() < 1e-3, "{:?}", f.pos);
        assert!(f.dop.is_finite() && f.dop > 0.5);
    }

    #[test]
    fn twr_exact_space() {
        let mut a = square();
        a.push(DVec3::new(5.0, 0.3, 4.0));
        let truth = DVec3::new(7.0, 1.4, 2.0);
        let r: Vec<f64> = a.iter().map(|p| (*p - truth).length()).collect();
        let f = solve_twr(Dim::Space, &a, &r, &[0.1; 5], None).unwrap();
        assert!((f.pos - truth).length() < 1e-3, "{:?}", f.pos);
    }

    #[test]
    fn twr_noise_bounded() {
        let a = square();
        let truth = DVec3::new(5.0, 1.0, 4.0);
        let mut rng = crate::rtls::rng::Rng::new(1);
        let mut se = 0.0;
        let n = 500;
        for _ in 0..n {
            let r: Vec<f64> = a.iter().map(|p| (*p - truth).length() + rng.gauss(0.1) as f64).collect();
            let f = solve_twr(Dim::Plane(1.0), &a, &r, &[0.1; 4], None).unwrap();
            se += (f.pos - truth).length_squared();
        }
        let rmse = (se / n as f64).sqrt();
        assert!(rmse < 0.2, "rmse {rmse}");
    }

    #[test]
    fn robust_rejects_nlos() {
        let mut a = square();
        a.push(DVec3::new(5.0, 2.5, -1.0));
        a.push(DVec3::new(5.0, 2.5, 9.0));
        let truth = DVec3::new(4.0, 1.0, 3.0);
        let mut r: Vec<f64> = a.iter().map(|p| (*p - truth).length()).collect();
        r[1] += 2.5; // one NLOS range
        let naive = solve_twr(Dim::Plane(1.0), &a, &r, &[0.1; 6], None).unwrap();
        let robust = solve_twr_robust(Dim::Plane(1.0), &a, &r, &[0.1; 6], None, 0.2).unwrap();
        assert!((robust.pos - truth).length() < 0.05, "{:?}", robust.pos);
        assert!((naive.pos - truth).length() > (robust.pos - truth).length());
        assert!(!robust.used.contains(&1));
    }

    #[test]
    fn tdoa_exact() {
        let a = square();
        let truth = DVec3::new(6.5, 1.2, 2.5);
        let reference = a[0];
        let others = &a[1..];
        let d: Vec<f64> = others.iter().map(|p| (*p - truth).length() - (reference - truth).length()).collect();
        let f = solve_tdoa(Dim::Plane(1.2), reference, others, &d, 0.1, None).unwrap();
        assert!((f.pos - truth).length() < 1e-3, "{:?}", f.pos);
    }

    #[test]
    fn dop_center_is_good_edge_is_worse() {
        let a = square();
        let c = dop(Dim::Plane(1.0), &a, DVec3::new(5.0, 1.0, 4.0));
        let e = dop(Dim::Plane(1.0), &a, DVec3::new(30.0, 1.0, 4.0));
        assert!((c - 1.0).abs() < 0.2, "{c}");
        assert!(e > 2.5 * c, "{e}");
    }

    #[test]
    fn kalman_converges_on_line() {
        let mut k = Kalman::new(1.0);
        let mut rng = crate::rtls::rng::Rng::new(3);
        let mut err = 0.0;
        for i in 0..200 {
            let t = i as f64 * 0.1;
            let truth = DVec3::new(t, 1.0, 2.0);
            k.predict(0.1);
            k.update(truth + DVec3::new(rng.gauss(0.2) as f64, 0.0, rng.gauss(0.2) as f64), 0.2);
            if i > 100 {
                err += (k.position() - truth).length();
            }
        }
        assert!(err / 99.0 < 0.15, "{}", err / 99.0);
        assert!((k.x[2] - 1.0).abs() < 0.2, "vx {}", k.x[2]);
    }

    #[test]
    fn rssi_roundtrip() {
        let d = rssi_to_distance(-59.0 - 20.0 * 4f64.log10(), -59.0, 2.0);
        assert!((d - 4.0).abs() < 1e-9);
        assert!((fspl_db(1.0, 915.0) - 31.68).abs() < 0.1);
    }
}
