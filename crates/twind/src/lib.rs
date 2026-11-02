//! Sense Twin service: everything that talks to real devices lives here.
//!
//! So far this holds the read-only Siemens Web API client, which proves that a PLC can
//! be reached with its TLS key pinned. The service itself arrives in milestone M1.

pub mod http;
pub mod pinned_tls;
pub mod webapi;
