//! A finite-integration time-domain solver.
//!
//! The unknowns are grid voltages, not field samples: `e` is the integral of E along a primal
//! edge, `h` the integral of H along a dual edge. Faraday's and Ampere's laws are then exact
//! integer stencils, and everything about geometry and material sits in two diagonal matrices,
//! `Mε` (dual-face flux from edge voltage) and `Mμ` (primal-face flux from dual-edge voltage).
//!
//! That is what subcell metal needs: anything the grid can't resolve goes into those diagonals,
//! where it can't break the time stepping. With positive diagonals and `C̃ = Cᵀ` the update is
//! stable for any `dt` below `2/√λmax`, and `λmax` is measured from the real operator.
//!
//! Conventions: SI units, right-handed axes 0, 1, 2 for x, y, z.

pub mod assemble;
pub mod edge;
pub mod engine;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod grid;
pub mod problem;
pub mod real;
pub mod record;
pub mod team;
pub mod waveport;
pub mod static2d;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_canon;
#[cfg(test)]
mod tests_review;

pub const C0: f64 = 299_792_458.0;
pub const MU0: f64 = 1.256_637_062_12e-6;
pub const EPS0: f64 = 8.854_187_812_8e-12;
pub const ETA0: f64 = 376.730_313_668;
