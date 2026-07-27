//! Real-world `TargetAdapter` implementations.
//!
//! Each adapter lives in its own file behind a feature flag so the
//! default build of `malcolm-agent` is the in-process port plus
//! the safety + cleanup plumbing. Compile-gated adapters still
//! consult the safety interlock at runtime.

#[cfg(all(target_os = "linux", feature = "cgroups"))]
pub mod cgroups;
#[cfg(all(target_os = "linux", feature = "netem"))]
pub mod netem;
#[cfg(all(target_os = "linux", feature = "netem"))]
mod netem_cmd;
#[cfg(all(unix, feature = "process"))]
pub mod process;
// x86_64-only: the ptrace register-manipulation internals operate on
// the x86_64 `user_regs_struct` layout (`orig_rax` / `rax`). Other
// Linux architectures (aarch64, etc.) are a documented follow-up —
// see `syscall/mod.rs`.
#[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "syscall"))]
pub mod syscall;
