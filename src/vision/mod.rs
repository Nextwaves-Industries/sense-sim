//! In-process vision models (ONNX Runtime, GPU via DirectML) for reference photos:
//! open-vocabulary detection finds objects and CLIP classifies materials. Metric
//! depth only verifies a finished layout; it never builds one.

pub mod geometry;
pub mod materials;
pub mod models;
pub mod perceive;
pub mod runtime;
