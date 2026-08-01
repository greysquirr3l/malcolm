//! # malcolm-core
//!
//! Pure, `no_std`-compatible math domain primitives for the malcolm chaos
//! engineering library.
//!
//! This crate contains only value objects and deterministic algorithms — no I/O,
//! no async runtimes, no port traits. It is safe to use in `no_std` environments
//! (e.g. embedded simulators, WASM) as long as an allocator is provided.
//!
//! # Example
//!
//! ```rust
//! // All public sub-modules are accessible through the crate root.
//! use malcolm_core::types;
//! let _ = types::MALCOLM_CORE_VERSION;
//! ```
#![no_std]
extern crate alloc;

pub mod bifurcation;
pub mod distributions;
pub mod inference;
pub mod lyapunov;
pub mod noise;
pub mod posterior;
pub mod types;
