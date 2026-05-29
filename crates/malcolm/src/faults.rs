//! Concrete fault implementations.
//!
//! Each sub-module provides a specific fault type that implements the
//! [`Fault`](crate::fault::Fault) port trait defined in [`crate::fault`].
//!
//! Implemented:
//!
//! - **T07** — [`network`]: partition, packet-loss, latency spike, bandwidth cap
//! - **T08** — [`resource`]: memory pressure, CPU throttle, I/O degradation
//!
//! Planned:
//!
//! - **T09** — `clock`: skew, freeze, jump
//! - **T10** — `byzantine`: lies, partial responses, slow-correct

pub mod network;
pub mod resource;

// TODO(T09): add clock fault module (skew, freeze, jump)
// TODO(T10): add byzantine fault module (lies, partial_response, slow_correct)
