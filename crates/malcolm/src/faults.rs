//! Concrete fault implementations.
//!
//! Each sub-module provides a specific fault type that implements the
//! [`Fault`](crate::fault::Fault) port trait defined in [`crate::fault`].
//!
//! Planned implementations:
//!
//! - **T07** — `network`: partition, packet-loss, latency spike, bandwidth cap
//! - **T08** — `resource`: memory pressure, CPU throttle, I/O degradation
//! - **T09** — `clock`: skew, freeze, jump
//! - **T10** — `byzantine`: lies, partial responses, slow-correct

// TODO(T07): add network fault module (partition, packet_loss, latency_spike, bandwidth_cap)
// TODO(T08): add resource fault module (memory_pressure, cpu_throttle, io_degradation)
// TODO(T09): add clock fault module (skew, freeze, jump)
// TODO(T10): add byzantine fault module (lies, partial_response, slow_correct)
