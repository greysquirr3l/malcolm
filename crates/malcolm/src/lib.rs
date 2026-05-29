//! # malcolm
//!
//! Chaos engineering fault injection library for distributed systems and async
//! services.
//!
//! This crate is the primary entry point for consumers. It re-exports
//! everything from [`malcolm_core`] and adds the assembly layer: traits,
//! concrete fault types, scenario builders, and optional tokio integration.
//!
//! # Feature Flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `tokio`  | off     | Async fault injection via Tokio runtime |
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults;
//! let _ = faults::PLACEHOLDER;
//! ```

pub use malcolm_core as core;

pub mod faults;
pub mod replay;
pub mod scenario;
pub mod topology;
pub mod traits;
