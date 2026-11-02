//! Device command messages, for the twin service and the operator console only.
//!
//! The viewer, the collector and discovery must never depend on this crate.
//! `twin_proto/tests/control_boundary.rs` fails the build if one of them does.
//!
//! Empty until milestone M7. It exists now so that boundary is enforced from the start.
