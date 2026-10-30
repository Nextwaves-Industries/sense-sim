//! In-process vision models (ONNX Runtime, GPU via DirectML): metric depth,
//! open-vocabulary detection and CLIP, used by the assistant to measure a
//! reference photo instead of guessing.

pub mod geometry;
pub mod models;
pub mod perceive;
pub mod runtime;
