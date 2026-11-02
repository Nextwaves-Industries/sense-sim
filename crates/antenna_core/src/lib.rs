//! Antenna design kernel used by Sense Sim's Antenna Studio.
//!
//! Conventions, everywhere in this crate:
//! - SI units (metres, hertz, ohms, watts).
//! - Phasors use `exp(+jωt)`, so an outgoing spherical wave is `exp(-jkr)/r`.
//! - Spherical angles: θ from +z, φ from +x towards +y, both in degrees in the
//!   public API.
//! - Peak (not RMS) amplitudes: power is `½·Re(V·I*)`.

pub mod array;
pub mod assess;
pub mod calc;
pub mod cad;
pub mod checks;
pub mod csx;
pub mod expr;
pub mod farfield;
pub mod geometry;
pub mod linalg;
pub mod matching;
pub mod material;
pub mod mom;
pub mod optimize;
pub mod project;
pub mod reconfig;
pub mod recipes;
pub mod references;
pub mod report;
pub mod samples;
pub mod sketch;
pub mod sparam;
pub mod tag;
pub mod templates;
#[cfg(test)]
mod rfid_tuning;

pub use num_complex::Complex64 as C64;

pub const C0: f64 = 299_792_458.0;
pub const MU0: f64 = 1.256_637_062_12e-6;
pub const EPS0: f64 = 8.854_187_812_8e-12;
pub const ETA0: f64 = 376.730_313_668;

/// Evenly spaced values from `a` to `b` inclusive.
pub fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    if n < 2 {
        return vec![a];
    }
    (0..n).map(|k| a + (b - a) * k as f64 / (n - 1) as f64).collect()
}

/// 20·log10(|v|), floored so a zero stays finite. A NaN stays a NaN: floored, it would
/// read as a perfect match.
pub fn db20(v: f64) -> f64 {
    if v.is_nan() { v } else { 20.0 * v.abs().max(1e-15).log10() }
}

/// 10·log10(v), floored so a zero stays finite. A NaN stays a NaN.
pub fn db10(v: f64) -> f64 {
    if v.is_nan() { v } else { 10.0 * v.abs().max(1e-30).log10() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nan_stays_a_nan_in_db() {
        assert!(db20(f64::NAN).is_nan(), "{}", db20(f64::NAN));
        assert!(db10(f64::NAN).is_nan(), "{}", db10(f64::NAN));
        assert_eq!(db20(0.0), -300.0);
        assert_eq!(db10(0.0), -300.0);
    }
}
