//! Shared value types and constants for malcolm-core.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::types::MALCOLM_CORE_VERSION;
//! assert!(!MALCOLM_CORE_VERSION.is_empty());
//! ```

/// Crate version string, re-exported for runtime inspection.
pub const MALCOLM_CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
