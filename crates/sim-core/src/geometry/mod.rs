//! Geometry on its way to a solver.

pub mod classify;
pub mod partition;
pub mod regions;

pub use classify::{Assignment, Issue, Limits, classify};
pub use partition::{Partition, read_tagged_3mf};
