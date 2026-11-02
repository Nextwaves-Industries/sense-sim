//! A D3Q19 lattice Boltzmann solver on the GPU: incompressible flow on a Cartesian grid.

pub mod gpu;
pub mod shader;
pub mod study;
pub mod thermal;
pub mod voxel;

pub use gpu::{Lbm, Macros, Setup};
pub use shader::{FLUID, INLET, MOVING_WALL, OUTLET, ROUGH_LAYER, SLIP_FLOOR, WALL};
