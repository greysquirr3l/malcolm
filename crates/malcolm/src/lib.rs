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
//! use malcolm::fault::{FaultHandle, FaultRegistry};
//!
//! let mut registry = FaultRegistry::new();
//! registry.register("node-0", FaultHandle::new());
//! assert_eq!(registry.active_count("node-0"), 1);
//! ```
//!
//! # Worked Examples
//!
//! - [Async service fault injection](../examples/async_service.rs)
//! - [State machine simulation](../examples/simulation.rs)
//! - [Replay recording demo](../examples/replay_demo.rs)

pub use malcolm_core as core;
pub use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult, SkipReason};

pub mod fault;
pub mod faults;
pub mod macro_dsl;
pub mod presets;
pub mod replay;
pub mod scenario;
pub mod topology;
pub mod tracing_layer;
pub mod traits;

pub use tracing_layer::MalcolmLayer;
