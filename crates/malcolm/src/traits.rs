//! Additional port traits for the malcolm assembly layer.
//!
//! The primary `Fault` port trait lives in [`crate::fault`]. This module
//! will house supplementary traits as they are needed by later tasks.
//!
//! Planned additions:
//!
//! - **T09** — `MalcolmClock`: injectable time source for clock fault testing
//! - `DistributionSampler` is defined in `malcolm-core::distributions` and
//!   does not need to be redefined here.

// TODO(T09): add MalcolmClock trait (injectable time source: now_ms, advance, freeze, jump)
