//! Sense Twin service: everything that talks to real devices lives here.
//!
//! The viewer never touches a device. It reads from this service over a named pipe, and
//! that pipe has no request that changes anything. The Siemens Web API client is here too,
//! ready for the monitoring that arrives in M2.

pub mod http;
pub mod pinned_tls;
pub mod webapi;

pub mod clock;
pub mod logging;
pub mod sddl;

#[cfg(windows)]
pub mod install;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod runtime;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod state;
#[cfg(windows)]
pub mod stop;
#[cfg(windows)]
pub mod store;
#[cfg(windows)]
pub mod win;
