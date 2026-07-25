//! Real-world `TargetAdapter` implementations.
//!
//! Each adapter lives in its own file behind a feature flag so the
//! default build of `malcolm-agent` is the in-process port plus
//! the safety + cleanup plumbing. Compile-gated adapters still
//! consult the safety interlock at runtime.

#[cfg(all(target_os = "linux", feature = "cgroups"))]
pub mod cgroups;
#[cfg(all(unix, feature = "process"))]
pub mod process;
