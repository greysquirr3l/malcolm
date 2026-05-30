//! Concrete fault implementations.
//!
//! Each sub-module provides a specific fault type that implements the
//! [`Fault`](crate::fault::Fault) port trait defined in [`crate::fault`].
//!
//! Implemented:
//!
//! - **T07** — [`network`]: partition, packet-loss, latency spike, bandwidth cap
//! - **T08** — [`resource`]: memory pressure, CPU throttle, I/O degradation
//! - **T09** — [`clock`]: skew, freeze, jump
//! - **T10** — [`byzantine`]: lies, partial responses, slow-correct

pub mod byzantine;
pub mod clock;
pub mod network;
pub mod resource;
