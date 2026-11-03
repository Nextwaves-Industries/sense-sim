//! Complex matrices, Hermitian eigenvalues, polynomial roots and the FFT: what array and
//! signal processing need beyond [`crate::linalg`].

use std::f64::consts::{PI, TAU};

use crate::C64;

/// A complex matrix by rows.
pub type CMat = Vec<Vec<C64>>;

pub const ZERO: C64 = C64::new(0.0, 0.0);
pub const ONE: C64 = C64::new(1.0, 0.0);

pub fn zeros(rows: usize, cols: usize) -> CMat {
    vec![vec![ZERO; cols]; rows]
}

pub fn identity(n: usize) -> CMat {
    let mut m = zeros(n, n);
    for (i, row) in m.iter_mut().enumerate() {
        row[i] = ONE;
    }
    m
}

/// The conjugate transpose.
pub fn herm(a: &CMat) -> CMat {
    let cols = a.first().map_or(0, Vec::len);
    (0..cols).map(|c| a.iter().map(|row| row[c].conj()).collect()).collect()
}

pub fn mat_mul(a: &CMat, b: &CMat) -> CMat {
    let cols = b.first().map_or(0, Vec::len);
    a.iter()
        .map(|row| {
            let mut out = vec![ZERO; cols];
            for (v, b_row) in row.iter().zip(b) {
                for (o, x) in out.iter_mut().zip(b_row) {
                    *o += *v * *x;
                }
            }
            out
        })
        .collect()
}

pub fn mat_vec(a: &CMat, x: &[C64]) -> Vec<C64> {
    a.iter().map(|row| row.iter().zip(x).map(|(v, y)| *v * *y).sum()).collect()
}

/// `aᴴ·b`.
pub fn inner(a: &[C64], b: &[C64]) -> C64 {
    a.iter().zip(b).map(|(x, y)| x.conj() * *y).sum()
}

pub fn norm_sqr(a: &[C64]) -> f64 {
    a.iter().map(C64::norm_sqr).sum()
}

/// `aᴴ·M·a`, which is real for a Hermitian `M`.
pub fn quadratic(m: &CMat, a: &[C64]) -> f64 {
    m.iter().zip(a).map(|(row, x)| (x.conj() * row.iter().zip(a).map(|(v, y)| *v * *y).sum::<C64>()).re).sum()
}

pub fn unit(v: Vec<C64>) -> Vec<C64> {
    let n = norm_sqr(&v).sqrt();
    if n > 0.0 { v.into_iter().map(|x| x / n).collect() } else { v }
}

/// Eigenvalues (ascending) and eigenvectors (by column) of a Hermitian matrix, by complex
/// Jacobi rotations. Slower than tridiagonalising beyond a few hundred channels, but
/// accurate for the small eigenvalues a noise subspace depends on.
pub fn hermitian_eig(a: &CMat) -> (Vec<f64>, CMat) {
    let n = a.len();
    let mut m = a.clone();
    let mut v = identity(n);
    let total: f64 = m.iter().flatten().map(C64::norm_sqr).sum();
    let floor = 1e-30 * total.max(1e-300);
    for _ in 0..80 {
        let off: f64 = (0..n).flat_map(|p| (p + 1..n).map(move |q| (p, q))).map(|(p, q)| m[p][q].norm_sqr()).sum();
        if off <= floor {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = m[p][q];
                let size = apq.norm();
                if size * size <= floor / (n * n) as f64 {
                    continue;
                }
                let phase = apq / size;
                let tau = (m[q][q].re - m[p][p].re) / (2.0 * size);
                let t = tau.signum() / (tau.abs() + (tau * tau + 1.0).sqrt());
                let t = if tau == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                // J = [[c, s], [−s·e^{−jα}, c·e^{−jα}]] with e^{jα} = apq/|apq|: first
                // rotate the phase away, then do the real rotation. A' = Jᴴ·A·J, V' = V·J.
                let back = phase.conj();
                for row in m.iter_mut() {
                    let (x, y) = (row[p], row[q]);
                    row[p] = x * c - y * back * s;
                    row[q] = x * s + y * back * c;
                }
                let forward = phase;
                let (row_p, row_q) = (m[p].clone(), m[q].clone());
                for k in 0..n {
                    m[p][k] = row_p[k] * c - row_q[k] * forward * s;
                    m[q][k] = row_p[k] * s + row_q[k] * forward * c;
                }
                for row in v.iter_mut() {
                    let (x, y) = (row[p], row[q]);
                    row[p] = x * c - y * back * s;
                    row[q] = x * s + y * back * c;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|i, j| m[*i][*i].re.total_cmp(&m[*j][*j].re));
    let values = order.iter().map(|k| m[*k][*k].re).collect();
    let vectors = v.iter().map(|row| order.iter().map(|k| row[*k]).collect()).collect();
    (values, vectors)
}

/// The roots of `c[0] + c[1]·z + … + c[n]·zⁿ`, by Aberth-Ehrlich iteration: every root
/// improves at once and repels the others, so close pairs (as on the unit circle in
/// Root-MUSIC) aren't lost.
pub fn poly_roots(coefficients: &[C64]) -> Vec<C64> {
    let top = coefficients.iter().map(|v| v.norm()).fold(0.0, f64::max);
    if top == 0.0 {
        return Vec::new();
    }
    let degree = coefficients.iter().rposition(|c| c.norm() > 1e-14 * top).unwrap_or(0);
    if degree == 0 {
        return Vec::new();
    }
    let lead = coefficients[degree];
    let c: Vec<C64> = coefficients[..=degree].iter().map(|v| *v / lead).collect();
    let radius = (1..=degree).map(|k| c[degree - k].norm().powf(1.0 / k as f64)).fold(0.0, f64::max).max(1e-3);
    let mut z: Vec<C64> = (0..degree).map(|i| C64::from_polar(radius, TAU * i as f64 / degree as f64 + 0.4)).collect();
    for _ in 0..600 {
        let mut biggest = 0.0f64;
        for i in 0..degree {
            let (mut p, mut d) = (c[degree], ZERO);
            for a in c[..degree].iter().rev() {
                d = d * z[i] + p;
                p = p * z[i] + *a;
            }
            if p.norm() == 0.0 {
                continue;
            }
            let ratio = p / d;
            let repel: C64 = (0..degree).filter(|j| *j != i).map(|j| ONE / (z[i] - z[j])).sum();
            let step = ratio / (ONE - ratio * repel);
            if step.is_finite() {
                z[i] -= step;
                biggest = biggest.max(step.norm() / (1.0 + z[i].norm()));
            }
        }
        if biggest < 1e-14 {
            break;
        }
    }
    z
}

/// The eigenvalues of a small general matrix: its characteristic polynomial, from the
/// Faddeev-LeVerrier recurrence, then [`poly_roots`]. Well conditioned to about 10×10.
pub fn eigenvalues(a: &CMat) -> Vec<C64> {
    let n = a.len();
    if n == 0 {
        return Vec::new();
    }
    let mut c = vec![ZERO; n + 1];
    c[n] = ONE;
    let mut m = a.clone();
    for k in 1..=n {
        let tr: C64 = (0..n).map(|i| m[i][i]).sum();
        let ck = -tr / k as f64;
        c[n - k] = ck;
        // M ← A·(M + ck·I)
        let mut shifted = m.clone();
        for (i, row) in shifted.iter_mut().enumerate() {
            row[i] += ck;
        }
        m = mat_mul(a, &shifted);
    }
    poly_roots(&c)
}

fn fft_radix2(x: &mut [C64], inverse: bool) {
    let n = x.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            x.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let step = C64::from_polar(1.0, sign * TAU / len as f64);
        for chunk in x.chunks_exact_mut(len) {
            let mut w = ONE;
            let (lo, hi) = chunk.split_at_mut(len / 2);
            for (a, b) in lo.iter_mut().zip(hi) {
                let t = *b * w;
                *b = *a - t;
                *a += t;
                w *= step;
            }
        }
        len *= 2;
    }
}

/// An FFT of any length without the 1/N, by Bluestein's chirp when it isn't a power of two.
fn transform(x: &mut [C64], inverse: bool) {
    let n = x.len();
    if n < 2 {
        return;
    }
    if n.is_power_of_two() {
        fft_radix2(x, inverse);
        return;
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let m = (2 * n - 1).next_power_of_two();
    // The square is taken modulo 2n, so a long transform keeps its phase accuracy.
    let chirp: Vec<C64> = (0..n).map(|k| C64::from_polar(1.0, sign * PI * ((k * k) % (2 * n)) as f64 / n as f64)).collect();
    let mut a = vec![ZERO; m];
    let mut b = vec![ZERO; m];
    for k in 0..n {
        a[k] = x[k] * chirp[k];
        b[k] = chirp[k].conj();
        if k > 0 {
            b[m - k] = chirp[k].conj();
        }
    }
    fft_radix2(&mut a, false);
    fft_radix2(&mut b, false);
    for (p, q) in a.iter_mut().zip(&b) {
        *p *= *q;
    }
    fft_radix2(&mut a, true);
    for k in 0..n {
        x[k] = a[k] / m as f64 * chirp[k];
    }
}

/// `X[k] = Σ x[n]·e^{−j2πkn/N}`.
pub fn fft(x: &mut [C64]) {
    transform(x, false);
}

/// The inverse of [`fft`], with its 1/N.
pub fn ifft(x: &mut [C64]) {
    transform(x, true);
    let n = x.len() as f64;
    x.iter_mut().for_each(|v| *v /= n);
}

/// Hann window, with the ends just inside it so no sample is thrown away.
pub fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (TAU * (i as f64 + 0.5) / n as f64).cos()).collect()
}

/// Modified Bessel function of the first kind, order zero.
pub fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..200 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-16 * sum {
            break;
        }
    }
    sum
}

/// Moves the zero bin to the middle, as a Doppler axis or a spectrum plot wants.
pub fn fft_shift<T: Clone>(x: &[T]) -> Vec<T> {
    let half = x.len().div_ceil(2);
    x[half..].iter().chain(&x[..half]).cloned().collect()
}

/// Parabola through three neighbours: how far from the middle one the top is, in samples.
pub fn parabolic_peak(a: f64, b: f64, c: f64) -> f64 {
    let d = a - 2.0 * b + c;
    if d < 0.0 { (0.5 * (a - c) / d).clamp(-0.5, 0.5) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_hermitian(n: usize, seed: u64) -> CMat {
        let mut r = proof::Rng::new(seed);
        let g: CMat = (0..n).map(|_| (0..n).map(|_| C64::new(r.gauss(), r.gauss())).collect()).collect();
        mat_mul(&g, &herm(&g))
    }

    #[test]
    fn eigen_decomposition_rebuilds_the_matrix() {
        for n in [1, 2, 5, 16, 40] {
            let a = random_hermitian(n, n as u64);
            let (values, v) = hermitian_eig(&a);
            assert!(values.windows(2).all(|w| w[0] <= w[1]), "{values:?}");
            // A·V = V·Λ, and VᴴV = I.
            let av = mat_mul(&a, &v);
            for r in 0..n {
                for c in 0..n {
                    let want = v[r][c] * values[c];
                    assert!((av[r][c] - want).norm() < 1e-9 * (1.0 + values[n - 1]), "n {n} ({r},{c}) {} vs {want}", av[r][c]);
                }
            }
            let vhv = mat_mul(&herm(&v), &v);
            for r in 0..n {
                for c in 0..n {
                    let want = if r == c { 1.0 } else { 0.0 };
                    assert!((vhv[r][c] - want).norm() < 1e-10, "unitary at ({r},{c}): {}", vhv[r][c]);
                }
            }
            let trace: f64 = (0..n).map(|i| a[i][i].re).sum();
            assert!((values.iter().sum::<f64>() - trace).abs() < 1e-9 * trace.max(1.0));
        }
    }

    #[test]
    fn eigen_finds_a_known_rank_one_matrix() {
        // a·aᴴ + I has eigenvalues 1 (n−1 times) and 1 + ‖a‖².
        let a: Vec<C64> = (0..6).map(|i| C64::from_polar(1.0, i as f64)).collect();
        let mut m = identity(6);
        for r in 0..6 {
            for c in 0..6 {
                m[r][c] += a[r] * a[c].conj();
            }
        }
        let (values, v) = hermitian_eig(&m);
        for x in &values[..5] {
            assert!((x - 1.0).abs() < 1e-12, "{values:?}");
        }
        assert!((values[5] - 7.0).abs() < 1e-12, "{values:?}");
        let top: Vec<C64> = v.iter().map(|row| row[5]).collect();
        assert!((inner(&top, &a).norm() - 6f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn roots_of_known_polynomials() {
        // (z − 1)(z − 2j)(z + 3) expanded.
        let (r1, r2, r3) = (C64::new(1.0, 0.0), C64::new(0.0, 2.0), C64::new(-3.0, 0.0));
        let c = vec![-(r1 * r2 * r3), r1 * r2 + r1 * r3 + r2 * r3, -(r1 + r2 + r3), ONE];
        let found = poly_roots(&c);
        for want in [r1, r2, r3] {
            assert!(found.iter().any(|z| (*z - want).norm() < 1e-10), "{want} in {found:?}");
        }
        // zⁿ − 1: the unit circle, the case Root-MUSIC lives on.
        let n = 24;
        let mut c = vec![ZERO; n + 1];
        c[0] = -ONE;
        c[n] = ONE;
        let found = poly_roots(&c);
        assert_eq!(found.len(), n);
        for k in 0..n {
            let want = C64::from_polar(1.0, TAU * k as f64 / n as f64);
            assert!(found.iter().any(|z| (*z - want).norm() < 1e-9), "{want}");
        }
    }

    #[test]
    fn eigenvalues_of_a_general_matrix() {
        // Upper triangular: its diagonal.
        let m: CMat = vec![vec![C64::new(1.0, 1.0), C64::new(2.0, 0.0), C64::new(0.0, 3.0)], vec![ZERO, C64::new(-2.0, 0.5), C64::new(1.0, 0.0)], vec![ZERO, ZERO, C64::new(0.0, -1.0)]];
        let found = eigenvalues(&m);
        for want in [C64::new(1.0, 1.0), C64::new(-2.0, 0.5), C64::new(0.0, -1.0)] {
            assert!(found.iter().any(|z| (*z - want).norm() < 1e-9), "{want} in {found:?}");
        }
    }

    fn dft(x: &[C64]) -> Vec<C64> {
        let n = x.len();
        (0..n).map(|k| (0..n).map(|i| x[i] * C64::from_polar(1.0, -TAU * (k * i) as f64 / n as f64)).sum()).collect()
    }

    #[test]
    fn fft_matches_the_definition_at_any_length() {
        let mut r = proof::Rng::new(4);
        for n in [1, 2, 3, 7, 8, 12, 16, 31, 64, 100, 257] {
            let x: Vec<C64> = (0..n).map(|_| C64::new(r.gauss(), r.gauss())).collect();
            let mut got = x.clone();
            fft(&mut got);
            for (a, b) in got.iter().zip(dft(&x)) {
                assert!((*a - b).norm() < 1e-8 * n as f64, "n {n}");
            }
            ifft(&mut got);
            for (a, b) in got.iter().zip(&x) {
                assert!((*a - *b).norm() < 1e-9 * n as f64, "round trip n {n}");
            }
        }
    }

    #[test]
    fn parseval_and_shift() {
        let x: Vec<C64> = (0..128).map(|i| C64::from_polar(1.0 + (i as f64 * 0.3).sin(), i as f64)).collect();
        let mut f = x.clone();
        fft(&mut f);
        assert!((norm_sqr(&f) / 128.0 - norm_sqr(&x)).abs() < 1e-8);
        assert_eq!(fft_shift(&[0, 1, 2, 3]), vec![2, 3, 0, 1]);
        assert_eq!(fft_shift(&[0, 1, 2, 3, 4]), vec![3, 4, 0, 1, 2]);
    }

    #[test]
    fn bessel_known_values() {
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-12);
        assert!((bessel_i0(5.0) - 27.239_871_823_604_44).abs() < 1e-9);
    }
}
