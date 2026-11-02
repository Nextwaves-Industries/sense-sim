//! Read-only data shared by the twin service, the collector and the viewer.
//!
//! Nothing here can ask a device to do anything. Commands live in
//! `twin_control_proto`, which the viewer and the collector must never depend on;
//! `tests/control_boundary.rs` enforces that.

pub mod text;
