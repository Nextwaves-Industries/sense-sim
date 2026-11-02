//! Running solvers.

pub mod manager;
pub mod process;
#[cfg(windows)]
pub mod sandbox;
pub mod wsl;

pub use manager::{JobCtx, JobInfo, JobManager, Kind, State};
pub use process::{Cancel, Output};
