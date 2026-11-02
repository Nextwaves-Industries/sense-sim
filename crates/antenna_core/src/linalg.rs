//! Dense complex LU with partial pivoting, on a column-major matrix.
//!
//! Blocked right-looking: a recursive panel factorisation, then the swaps, the
//! triangular solve and the trailing update for each column to the right, shared
//! out over rayon. The update walks the panel in row blocks small enough to stay
//! in a core's L2 and uses AVX-512 or AVX2 kernels when the CPU has them.

use std::sync::OnceLock;

use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::C64;

/// Panel width of the outer loop.
const BLOCK: usize = 96;
/// Rows of the panel handled at a time by the trailing update.
const ROW_BLOCK: usize = 384;
/// Columns updated together, so one row block of the panel is reused while cached.
const COL_GROUP: usize = 12;
/// Panel updates smaller than this many entries are not worth sending to rayon.
const PANEL_PAR: usize = 200_000;

/// The pool factorisations run on: half the logical CPUs, one per physical core
/// on an SMT machine. Sibling hyperthreads add nothing to AVX work, and fork-join
/// then waits on the slowest thread: at 56-core scale the full thread count is
/// two to three times slower than this.
fn factor_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads((rayon::current_num_threads() / 2).max(1)).build().ok()).as_ref()
}

/// A factorised square matrix: `P·A = L·U`.
pub struct Lu {
    n: usize,
    lu: Vec<C64>,
    piv: Vec<usize>,
    /// `‖A‖₁`, kept for the condition estimate.
    anorm: f64,
}

impl Lu {
    /// Factorises the `n`×`n` column-major matrix `a`, consuming it.
    pub fn factor(a: Vec<C64>, n: usize) -> Result<Self> {
        match factor_pool() {
            Some(pool) => pool.install(|| Self::factor_here(a, n)),
            None => Self::factor_here(a, n),
        }
    }

    fn factor_here(mut a: Vec<C64>, n: usize) -> Result<Self> {
        if a.len() != n * n {
            bail!("matrix storage holds {} entries, not {n}²", a.len());
        }
        // A pivot at rounding level means the columns were dependent.
        let biggest = a.par_iter().map(|v| v.norm_sqr()).reduce(|| 0.0, f64::max);
        let floor = (n as f64 * f64::EPSILON).powi(2) * biggest;
        let anorm = a.par_chunks(n.max(1)).map(|col| col.iter().map(|v| v.norm()).sum::<f64>()).reduce(|| 0.0, f64::max);
        let mut piv = vec![0; n];
        let mut k = 0;
        while k < n {
            let w = BLOCK.min(n - k);
            let (left, rest) = a.split_at_mut(k * n);
            let (panel, right) = rest.split_at_mut(w * n);
            factor_panel(panel, n, k, w, &mut piv[k..k + w], floor)?;
            let swaps = &piv[k..k + w];
            left.par_chunks_mut(n).for_each(|col| apply_swaps(col, k, swaps));
            let panel = &*panel;
            right.par_chunks_mut(n * COL_GROUP).for_each(|group| update_group(group, panel, n, k, w, swaps));
            k += w;
        }
        Ok(Self { n, lu: a, piv, anorm })
    }

    pub fn size(&self) -> usize {
        self.n
    }

    /// Reciprocal condition number in the 1-norm, 1 for the identity, near 0 when the
    /// matrix is close to singular. An estimate from a few solves (Hager and Higham, as
    /// LAPACK's zgecon does it), never below the true value and rarely far above it.
    pub fn rcond(&self) -> f64 {
        let n = self.n;
        if n == 0 {
            return 1.0;
        }
        let norm1 = |v: &[C64]| v.iter().map(|x| x.norm()).sum::<f64>();
        let unit_phase = |v: &[C64]| -> Vec<C64> { v.iter().map(|x| if x.norm() > 0.0 { *x / x.norm() } else { C64::new(1.0, 0.0) }).collect() };
        let largest = |v: &[C64]| (0..n).max_by(|i, j| v[*i].norm().total_cmp(&v[*j].norm())).unwrap_or(0);
        let mut x = vec![C64::new(1.0 / n as f64, 0.0); n];
        self.solve(&mut x);
        let mut est = norm1(&x);
        if n > 1 {
            let mut xi = unit_phase(&x);
            self.solve_adjoint(&mut xi);
            let mut j = largest(&xi);
            for _ in 0..4 {
                let mut e = vec![C64::new(0.0, 0.0); n];
                e[j] = C64::new(1.0, 0.0);
                self.solve(&mut e);
                let next = norm1(&e);
                if next <= est {
                    break;
                }
                est = next;
                let mut xi = unit_phase(&e);
                self.solve_adjoint(&mut xi);
                let k = largest(&xi);
                if k == j {
                    break;
                }
                j = k;
            }
            // An alternating vector catches what the iteration can get stuck on.
            let mut alt: Vec<C64> = (0..n).map(|i| C64::new(if i % 2 == 0 { 1.0 } else { -1.0 } * (1.0 + i as f64 / (n - 1) as f64), 0.0)).collect();
            self.solve(&mut alt);
            est = est.max(2.0 * norm1(&alt) / (3.0 * n as f64));
        }
        if est > 0.0 && self.anorm > 0.0 { (1.0 / (est * self.anorm)).min(1.0) } else { 0.0 }
    }

    /// Replaces `b` with the solution of `Aᴴ·x = b`, with `Aᴴ = Uᴴ·Lᴴ·P`.
    fn solve_adjoint(&self, b: &mut [C64]) {
        let n = self.n;
        for j in 0..n {
            let col = &self.lu[j * n..(j + 1) * n];
            let above: C64 = col[..j].iter().zip(&b[..j]).map(|(u, y)| u.conj() * y).sum();
            b[j] = (b[j] - above) / col[j].conj();
        }
        for j in (0..n).rev() {
            let col = &self.lu[j * n..(j + 1) * n];
            let below: C64 = col[j + 1..].iter().zip(&b[j + 1..]).map(|(l, y)| l.conj() * y).sum();
            b[j] -= below;
        }
        for (j, &p) in self.piv.iter().enumerate().rev() {
            b.swap(j, p);
        }
    }

    /// Replaces `b` with the solution of `A·x = b`.
    pub fn solve(&self, b: &mut [C64]) {
        let n = self.n;
        for (j, &p) in self.piv.iter().enumerate() {
            b.swap(j, p);
        }
        for j in 0..n {
            let (head, tail) = b.split_at_mut(j + 1);
            let y = head[j];
            for (v, l) in tail.iter_mut().zip(&self.lu[j * n + j + 1..(j + 1) * n]) {
                *v -= *l * y;
            }
        }
        for j in (0..n).rev() {
            let (head, tail) = b.split_at_mut(j);
            tail[0] /= self.lu[j * n + j];
            let y = tail[0];
            for (v, u) in head.iter_mut().zip(&self.lu[j * n..j * n + j]) {
                *v -= *u * y;
            }
        }
    }

    /// Solves for several right-hand sides at once.
    pub fn solve_many(&self, rhs: &mut [Vec<C64>]) {
        rhs.par_iter_mut().for_each(|b| self.solve(b));
    }
}

/// Inverse of a small matrix given by rows.
pub fn invert(a: &[Vec<C64>]) -> Result<Vec<Vec<C64>>> {
    let n = a.len();
    let mut flat = vec![C64::new(0.0, 0.0); n * n];
    for (i, row) in a.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            flat[j * n + i] = *v;
        }
    }
    let lu = Lu::factor(flat, n)?;
    let mut out = vec![vec![C64::new(0.0, 0.0); n]; n];
    for j in 0..n {
        let mut e = vec![C64::new(0.0, 0.0); n];
        e[j] = C64::new(1.0, 0.0);
        lu.solve(&mut e);
        for i in 0..n {
            out[i][j] = e[i];
        }
    }
    Ok(out)
}

fn apply_swaps(col: &mut [C64], first: usize, swaps: &[usize]) {
    for (j, &p) in swaps.iter().enumerate() {
        col.swap(first + j, p);
    }
}

/// Recursive LU of `w` columns over rows `r0..n`. `swaps` receives absolute row indices.
fn factor_panel(p: &mut [C64], n: usize, r0: usize, w: usize, swaps: &mut [usize], floor: f64) -> Result<()> {
    if w == 1 {
        let (mut best, mut big) = (r0, -1.0);
        for i in r0..n {
            let m = p[i].norm_sqr();
            if m > big {
                big = m;
                best = i;
            }
        }
        if !(big > floor) || !big.is_finite() {
            bail!("the matrix is singular at column {r0}");
        }
        swaps[0] = best;
        p.swap(r0, best);
        let inv = p[r0].inv();
        for v in &mut p[r0 + 1..n] {
            *v *= inv;
        }
        return Ok(());
    }
    let w1 = w / 2;
    let (left, right) = p.split_at_mut(w1 * n);
    let (s1, s2) = swaps.split_at_mut(w1);
    factor_panel(left, n, r0, w1, s1, floor)?;
    let (l, s) = (&*left, &*s1);
    if (n - r0) * w > PANEL_PAR {
        right.par_chunks_mut(n).for_each(|col| update_group(col, l, n, r0, w1, s));
    } else {
        right.chunks_mut(n).for_each(|col| update_group(col, l, n, r0, w1, s));
    }
    factor_panel(right, n, r0 + w1, w - w1, s2, floor)?;
    left.chunks_mut(n).for_each(|col| apply_swaps(col, r0 + w1, s2));
    Ok(())
}

/// Swaps, triangular solve and trailing update of whole columns against a panel
/// of `w` columns starting on row `r0`.
fn update_group(group: &mut [C64], l: &[C64], n: usize, r0: usize, w: usize, swaps: &[usize]) {
    for col in group.chunks_mut(n) {
        apply_swaps(col, r0, swaps);
        for p in 0..w {
            let y = col[r0 + p];
            let lp = &l[p * n..(p + 1) * n];
            for q in p + 1..w {
                col[r0 + q] -= lp[r0 + q] * y;
            }
        }
    }
    let first = r0 + w;
    let mut start = first;
    while start < n {
        let end = (start + ROW_BLOCK).min(n);
        for col in group.chunks_mut(n) {
            let (head, tail) = col.split_at_mut(first);
            subtract_combination(&mut tail[start - first..end - first], l, n, start, &head[r0..first]);
        }
        start = end;
    }
}

/// `y -= Σ coef[p]·l[p]`, each `l[p]` being the same row range of a panel column.
fn subtract_combination(y: &mut [C64], l: &[C64], n: usize, row: usize, coef: &[C64]) {
    let len = y.len();
    let col = |p: usize| &l[p * n + row..p * n + row + len];
    let mut p = 0;
    while p + 4 <= coef.len() {
        kernel4(y, [col(p), col(p + 1), col(p + 2), col(p + 3)], [coef[p], coef[p + 1], coef[p + 2], coef[p + 3]]);
        p += 4;
    }
    while p < coef.len() {
        let c = coef[p];
        for (v, x) in y.iter_mut().zip(col(p)) {
            *v -= *x * c;
        }
        p += 1;
    }
}

#[cfg(target_arch = "x86_64")]
fn kernel4(y: &mut [C64], xs: [&[C64]; 4], cs: [C64; 4]) {
    if is_x86_feature_detected!("avx512f") {
        // SAFETY: the feature was just detected.
        unsafe { kernel4_avx512(y, xs, cs) }
    } else if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        // SAFETY: the features were just detected.
        unsafe { kernel4_avx2(y, xs, cs) }
    } else {
        kernel4_scalar(y, xs, cs, 0);
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn kernel4(y: &mut [C64], xs: [&[C64]; 4], cs: [C64; 4]) {
    kernel4_scalar(y, xs, cs, 0);
}

fn kernel4_scalar(y: &mut [C64], xs: [&[C64]; 4], cs: [C64; 4], from: usize) {
    for i in from..y.len() {
        y[i] -= xs[0][i] * cs[0] + xs[1][i] * cs[1] + xs[2][i] * cs[2] + xs[3][i] * cs[3];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn kernel4_avx512(y: &mut [C64], xs: [&[C64]; 4], cs: [C64; 4]) {
    use std::arch::x86_64::*;
    let n = y.len();
    let yp = y.as_mut_ptr() as *mut f64;
    let xp = xs.map(|x| x.as_ptr() as *const f64);
    let re = cs.map(|c| _mm512_set1_pd(c.re));
    let im = cs.map(|c| _mm512_set1_pd(c.im));
    let mut i = 0;
    while i + 4 <= n {
        // SAFETY: i + 4 <= n and every slice holds n complex values, 2·n doubles.
        unsafe {
            let mut acc = _mm512_loadu_pd(yp.add(2 * i));
            for k in 0..4 {
                let x = _mm512_loadu_pd(xp[k].add(2 * i));
                let t = _mm512_mul_pd(im[k], _mm512_permute_pd(x, 0b0101_0101));
                acc = _mm512_sub_pd(acc, _mm512_fmaddsub_pd(re[k], x, t));
            }
            _mm512_storeu_pd(yp.add(2 * i), acc);
        }
        i += 4;
    }
    kernel4_scalar(y, xs, cs, i);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn kernel4_avx2(y: &mut [C64], xs: [&[C64]; 4], cs: [C64; 4]) {
    use std::arch::x86_64::*;
    let n = y.len();
    let yp = y.as_mut_ptr() as *mut f64;
    let xp = xs.map(|x| x.as_ptr() as *const f64);
    let re = cs.map(|c| _mm256_set1_pd(c.re));
    let im = cs.map(|c| _mm256_set1_pd(c.im));
    let mut i = 0;
    while i + 2 <= n {
        // SAFETY: i + 2 <= n and every slice holds n complex values, 2·n doubles.
        unsafe {
            let mut acc = _mm256_loadu_pd(yp.add(2 * i));
            for k in 0..4 {
                let x = _mm256_loadu_pd(xp[k].add(2 * i));
                let t = _mm256_mul_pd(im[k], _mm256_permute_pd(x, 0b0101));
                acc = _mm256_sub_pd(acc, _mm256_fmaddsub_pd(re[k], x, t));
            }
            _mm256_storeu_pd(yp.add(2 * i), acc);
        }
        i += 2;
    }
    kernel4_scalar(y, xs, cs, i);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small linear-congruential generator: enough for test matrices.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) - 0.5
        }
        fn complex(&mut self) -> C64 {
            C64::new(self.next(), self.next())
        }
    }

    fn random(n: usize, seed: u64) -> Vec<C64> {
        let mut r = Rng(seed);
        (0..n * n).map(|_| r.complex()).collect()
    }

    fn residual(a: &[C64], n: usize, x: &[C64], b: &[C64]) -> f64 {
        let mut r = vec![C64::new(0.0, 0.0); n];
        for j in 0..n {
            for i in 0..n {
                r[i] += a[j * n + i] * x[j];
            }
        }
        r.iter().zip(b).map(|(u, v)| (*u - *v).norm()).fold(0.0, f64::max)
    }

    #[test]
    fn solves_random_systems_of_many_sizes() {
        for n in [1, 2, 3, 7, 50, 95, 96, 97, 193, 500] {
            let a = random(n, n as u64 + 1);
            let mut rng = Rng(77);
            let b: Vec<C64> = (0..n).map(|_| rng.complex()).collect();
            let lu = Lu::factor(a.clone(), n).unwrap();
            let mut x = b.clone();
            lu.solve(&mut x);
            let r = residual(&a, n, &x, &b);
            assert!(r < 1e-9, "n = {n}: residual {r}");
        }
    }

    #[test]
    fn needs_pivoting_and_finds_it() {
        // A zero on the diagonal: fails without row exchanges.
        let a = vec![C64::new(0.0, 0.0), C64::new(1.0, 0.0), C64::new(2.0, 0.0), C64::new(0.0, 0.0)];
        let lu = Lu::factor(a, 2).unwrap();
        let mut x = vec![C64::new(4.0, 0.0), C64::new(3.0, 0.0)];
        lu.solve(&mut x);
        assert!((x[0] - C64::new(3.0, 0.0)).norm() < 1e-14);
        assert!((x[1] - C64::new(2.0, 0.0)).norm() < 1e-14);
    }

    #[test]
    fn several_right_hand_sides() {
        let n = 200;
        let a = random(n, 5);
        let lu = Lu::factor(a.clone(), n).unwrap();
        let mut rng = Rng(9);
        let bs: Vec<Vec<C64>> = (0..5).map(|_| (0..n).map(|_| rng.complex()).collect()).collect();
        let mut xs = bs.clone();
        lu.solve_many(&mut xs);
        for (x, b) in xs.iter().zip(&bs) {
            assert!(residual(&a, n, x, b) < 1e-10);
        }
    }

    #[test]
    fn a_singular_matrix_is_an_error() {
        let n = 120;
        let mut a = random(n, 3);
        // Column 1 repeats column 0: a zero pivot at the second step, up to rounding.
        for i in 0..n {
            a[n + i] = a[i];
        }
        assert!(Lu::factor(a, n).is_err());
        let mut b = random(4, 8);
        b[5] = C64::new(f64::NAN, 0.0);
        assert!(Lu::factor(b, 4).is_err());
    }

    /// Exact 1-norm condition of a matrix, from its full inverse.
    fn exact_rcond(a: &[C64], n: usize) -> f64 {
        let lu = Lu::factor(a.to_vec(), n).unwrap();
        let col_norm = |c: &[C64]| c.iter().map(|v| v.norm()).sum::<f64>();
        let anorm = a.chunks(n).map(col_norm).fold(0.0, f64::max);
        let mut inv = 0.0f64;
        for j in 0..n {
            let mut e = vec![C64::new(0.0, 0.0); n];
            e[j] = C64::new(1.0, 0.0);
            lu.solve(&mut e);
            inv = inv.max(col_norm(&e));
        }
        1.0 / (anorm * inv)
    }

    #[test]
    fn rcond_tracks_the_true_condition() {
        // Identity, then near-singular ones the pivot floor lets through.
        let n = 40;
        let mut eye = vec![C64::new(0.0, 0.0); n * n];
        for i in 0..n {
            eye[i * n + i] = C64::new(1.0, 0.0);
        }
        assert!((Lu::factor(eye.clone(), n).unwrap().rcond() - 1.0).abs() < 1e-9);
        for (n, shrink) in [(40usize, 1e-4), (150, 1e-8), (40, 1e-11)] {
            let mut a = random(n, n as u64 + 2);
            // Column 1 nearly repeats column 0.
            for i in 0..n {
                a[n + i] = a[i] + a[n + i] * shrink;
            }
            let (got, want) = (Lu::factor(a.clone(), n).unwrap().rcond(), exact_rcond(&a, n));
            println!("n {n}, shrink {shrink:e}: rcond estimate {got:.3e}, exact {want:.3e}");
            // An estimate of the inverse's norm from below, so rcond from above, by a small factor.
            assert!(got >= want * 0.999 && got <= want * 10.0, "{got:e} vs {want:e}");
        }
        let plain = random(200, 7);
        let (got, want) = (Lu::factor(plain.clone(), 200).unwrap().rcond(), exact_rcond(&plain, 200));
        assert!(got >= want * 0.999 && got <= want * 10.0, "{got:e} vs {want:e}");
    }

    #[test]
    fn inverse_times_matrix_is_the_identity() {
        let n = 6;
        let flat = random(n, 21);
        let a: Vec<Vec<C64>> = (0..n).map(|i| (0..n).map(|j| flat[j * n + i]).collect()).collect();
        let inv = invert(&a).unwrap();
        for i in 0..n {
            for j in 0..n {
                let v: C64 = (0..n).map(|k| a[i][k] * inv[k][j]).sum();
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - want).norm() < 1e-10, "{i},{j}: {v}");
            }
        }
    }

    #[test]
    fn factors_3000_unknowns_in_seconds() {
        let n = 3000;
        let a = random(n, 11);
        let t = std::time::Instant::now();
        let lu = Lu::factor(a.clone(), n).unwrap();
        let factor = t.elapsed().as_secs_f64();
        let mut rng = Rng(13);
        let b: Vec<C64> = (0..n).map(|_| rng.complex()).collect();
        let mut x = b.clone();
        let t = std::time::Instant::now();
        lu.solve(&mut x);
        let solve = t.elapsed().as_secs_f64();
        let r = residual(&a, n, &x, &b);
        println!("LU n = {n}: factor {factor:.3} s, one solve {solve:.4} s, residual {r:.2e}");
        assert!(r < 1e-8, "residual {r}");
        assert!(factor < 20.0, "factor took {factor} s");
    }
}
