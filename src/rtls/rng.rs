//! Small seeded RNG (SplitMix64) so simulation runs are reproducible
//! without pulling in `rand`.

#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
    spare: Option<f32>,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed ^ 0x9E37_79B9_7F4A_7C15, spare: None }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.uniform()
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.uniform() < p
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let u1 = self.uniform().max(1e-7);
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (std::f32::consts::TAU * u2).sin_cos();
        self.spare = Some(r * s);
        r * c
    }

    pub fn gauss(&mut self, sigma: f32) -> f32 {
        self.normal() * sigma
    }

    /// Exponential with the given mean (used for NLOS excess delay).
    pub fn exp(&mut self, mean: f32) -> f32 {
        -mean * (1.0 - self.uniform()).max(1e-7).ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_has_unit_variance() {
        let mut r = Rng::new(7);
        let n = 20000;
        let xs: Vec<f32> = (0..n).map(|_| r.normal()).collect();
        let mean = xs.iter().sum::<f32>() / n as f32;
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n as f32;
        assert!(mean.abs() < 0.03, "{mean}");
        assert!((var - 1.0).abs() < 0.05, "{var}");
    }

    #[test]
    fn seeded_is_reproducible() {
        let a: Vec<u64> = { let mut r = Rng::new(42); (0..5).map(|_| r.next_u64()).collect() };
        let b: Vec<u64> = { let mut r = Rng::new(42); (0..5).map(|_| r.next_u64()).collect() };
        assert_eq!(a, b);
    }
}
