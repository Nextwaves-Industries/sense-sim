//! Running solvers.

pub mod manager;
pub mod process;
pub mod wsl;

pub use manager::{JobCtx, JobInfo, JobManager, Kind, State};
pub use process::{Cancel, Output};
