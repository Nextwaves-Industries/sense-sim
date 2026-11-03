//! The rectilinear grid. Every field array has one entry per node, so an index means the same
//! place in all of them; the few entries that stand for no real edge or face stay zero.

/// Mesh lines along each axis, increasing.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub lines: [Vec<f64>; 3],
}

impl Grid {
    pub fn new(lines: [Vec<f64>; 3]) -> Self {
        for (k, l) in lines.iter().enumerate() {
            assert!(l.len() >= 2, "axis {k} needs at least two lines");
            assert!(l.windows(2).all(|w| w[1] > w[0]), "axis {k} lines must increase");
        }
        Self { lines }
    }

    pub fn n(&self) -> [usize; 3] {
        [self.lines[0].len(), self.lines[1].len(), self.lines[2].len()]
    }

    /// Entries in one field array.
    pub fn nodes(&self) -> usize {
        self.lines.iter().map(|l| l.len()).product()
    }

    /// Real cells, the figure solver speeds are quoted against.
    pub fn cells(&self) -> u64 {
        self.lines.iter().map(|l| l.len() as u64 - 1).product()
    }

    pub fn strides(&self) -> [usize; 3] {
        let [nx, ny, _] = self.n();
        [1, nx, nx * ny]
    }

    #[inline]
    pub fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        let [nx, ny, _] = self.n();
        i + nx * (j + ny * k)
    }

    /// Length of the primal edge from line `i` to `i + 1`; zero past the last line.
    #[inline]
    pub fn primal(&self, axis: usize, i: usize) -> f64 {
        let l = &self.lines[axis];
        if i + 1 < l.len() { l[i + 1] - l[i] } else { 0.0 }
    }

    /// The dual edge through line `i`, as the part below and the part above it.
    #[inline]
    pub fn dual_parts(&self, axis: usize, i: usize) -> (f64, f64) {
        let l = &self.lines[axis];
        let lo = if i > 0 { (l[i] - l[i - 1]) * 0.5 } else { 0.0 };
        let hi = if i + 1 < l.len() { (l[i + 1] - l[i]) * 0.5 } else { 0.0 };
        (lo, hi)
    }

    /// Length of the dual edge through line `i`.
    #[inline]
    pub fn dual(&self, axis: usize, i: usize) -> f64 {
        let (lo, hi) = self.dual_parts(axis, i);
        lo + hi
    }

    /// The line nearest `x`.
    pub fn nearest(&self, axis: usize, x: f64) -> usize {
        let l = &self.lines[axis];
        match l.binary_search_by(|v| v.total_cmp(&x)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) if i >= l.len() => l.len() - 1,
            Err(i) => {
                if x - l[i - 1] <= l[i] - x {
                    i - 1
                } else {
                    i
                }
            }
        }
    }

    /// The first line at or above `x`, and the first strictly above `y`: a half-open range of
    /// lines inside `x..=y`.
    pub fn span(&self, axis: usize, x: f64, y: f64) -> (usize, usize) {
        let l = &self.lines[axis];
        (l.partition_point(|v| *v < x), l.partition_point(|v| *v <= y))
    }

    /// Smallest cell along any axis.
    pub fn min_cell(&self) -> f64 {
        self.lines.iter().flat_map(|l| l.windows(2).map(|w| w[1] - w[0])).fold(f64::MAX, f64::min)
    }
}

/// `n` evenly spaced lines from `a` to `b`.
pub fn even(a: f64, b: f64, n: usize) -> Vec<f64> {
    (0..n).map(|k| a + (b - a) * k as f64 / (n - 1) as f64).collect()
}
