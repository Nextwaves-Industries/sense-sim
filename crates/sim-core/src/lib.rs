//! Sense Sim's simulation core. Deliberately free of Bevy and egui: solver decks, meshes, result
//! readers and jobs are plain data and processes, so their tests need no window or GPU surface.

pub mod catalog;
pub mod ccx;
pub mod check;
pub mod elmer;
pub mod elmer_study;
pub mod foam;
pub mod geometry;
pub mod jobs;
pub mod lbm;
pub mod materials;
pub mod mesh;
pub mod post;
pub mod provision;
pub mod results;
pub mod results_cut;
pub mod results_vtk;
pub mod study;
pub mod tools;
