//! Parallel assembly of a symmetric Galerkin matrix from element-pair blocks.

use std::sync::Mutex;

use rayon::prelude::*;

use crate::C64;

/// One local shape function of an element: which unknown it feeds, which shape
/// function of the element it is, and with what factor.
#[derive(Clone, Copy, Debug)]
pub struct Dof {
    pub basis: u32,
    pub local: u8,
    pub weight: f64,
}

/// Tile side when mirroring the two triangles of the matrix.
const TILE: usize = 64;

struct Shared(*mut C64);
// SAFETY: `symmetrise` touches each entry from exactly one tile pair.
unsafe impl Sync for Shared {}

/// Builds the column-major `n`×`n` matrix. `block(p, q)` is called once for each
/// element pair with `p <= q`; `block(p, p)` has to be symmetric.
///
/// Each task writes only the rows of its own element's unknowns (behind per-row
/// locks that are almost never contended) and the two triangles are added up at
/// the end.
pub fn fill_symmetric<const D: usize>(n: usize, dofs: &[Vec<Dof>], block: impl Fn(usize, usize) -> [[C64; D]; D] + Sync) -> Vec<C64> {
    if n == 0 {
        return Vec::new();
    }
    let mut w = vec![C64::new(0.0, 0.0); n * n];
    {
        let rows: Vec<Mutex<&mut [C64]>> = w.chunks_mut(n).map(Mutex::new).collect();
        let ne = dofs.len();
        (0..ne).into_par_iter().with_max_len(1).for_each(|p| {
            if dofs[p].is_empty() {
                return;
            }
            let blocks: Vec<[[C64; D]; D]> = (p..ne).map(|q| if dofs[q].is_empty() { [[C64::new(0.0, 0.0); D]; D] } else { block(p, q) }).collect();
            for dp in &dofs[p] {
                let mut row = rows[dp.basis as usize].lock().unwrap();
                for (q, b) in (p..ne).zip(&blocks) {
                    let half = if q == p { 0.5 } else { 1.0 };
                    for dq in &dofs[q] {
                        row[dq.basis as usize] += b[dp.local as usize][dq.local as usize] * (dp.weight * dq.weight * half);
                    }
                }
            }
        });
    }
    symmetrise(&mut w, n);
    w
}

/// Replaces `W` with `W + Wᵀ`.
fn symmetrise(w: &mut [C64], n: usize) {
    let shared = Shared(w.as_mut_ptr());
    let shared = &shared;
    let tiles = n.div_ceil(TILE);
    (0..tiles).into_par_iter().for_each(|ti| {
        for tj in ti..tiles {
            for m in ti * TILE..((ti + 1) * TILE).min(n) {
                let first = if ti == tj { m } else { tj * TILE };
                for k in first..((tj + 1) * TILE).min(n) {
                    // SAFETY: (m, k) and (k, m) belong to the tile pair (ti, tj) alone.
                    unsafe {
                        let a = shared.0.add(m * n + k);
                        let b = shared.0.add(k * n + m);
                        let s = *a + *b;
                        *a = s;
                        *b = s;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_every_ordered_element_pair_once() {
        // Three one-function elements coupled through a known symmetric rule; the
        // two unknowns overlap on element 1.
        let dofs = vec![
            vec![Dof { basis: 0, local: 0, weight: 1.0 }],
            vec![Dof { basis: 0, local: 0, weight: 2.0 }, Dof { basis: 1, local: 0, weight: -1.0 }],
            vec![Dof { basis: 1, local: 0, weight: 3.0 }],
        ];
        let g = |p: usize, q: usize| (p + q + 1) as f64;
        let z = fill_symmetric::<1>(2, &dofs, |p, q| [[C64::new(g(p, q), 0.0)]]);
        // Basis 0 = e0 + 2·e1, basis 1 = -e1 + 3·e2.
        let want = |a: &[(usize, f64)], b: &[(usize, f64)]| -> f64 { a.iter().flat_map(|x| b.iter().map(move |y| x.1 * y.1 * g(x.0, y.0))).sum() };
        let (b0, b1) = (vec![(0, 1.0), (1, 2.0)], vec![(1, -1.0), (2, 3.0)]);
        assert!((z[0].re - want(&b0, &b0)).abs() < 1e-12, "{}", z[0]);
        assert!((z[1].re - want(&b0, &b1)).abs() < 1e-12, "{}", z[1]);
        assert!((z[2].re - want(&b0, &b1)).abs() < 1e-12, "{}", z[2]);
        assert!((z[3].re - want(&b1, &b1)).abs() < 1e-12, "{}", z[3]);
    }

    #[test]
    fn mirroring_covers_every_entry_of_a_big_matrix() {
        let n = 200;
        let mut w: Vec<C64> = (0..n * n).map(|i| C64::new(i as f64, 0.0)).collect();
        let orig = w.clone();
        symmetrise(&mut w, n);
        for m in 0..n {
            for k in 0..n {
                let want = orig[m * n + k] + orig[k * n + m];
                assert_eq!(w[m * n + k], want, "{m},{k}");
            }
        }
    }
}
