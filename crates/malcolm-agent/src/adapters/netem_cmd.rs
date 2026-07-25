//! Command builder + qdisc snapshot/restore helpers for the netem
//! adapter. Split out from the main adapter so the action enum
//! stays readable.
//!
//! All commands are built as `tokio::Command` programs; the
//! adapter owns the execution path. The snapshot/restore helpers
//! record the current root qdisc of an interface before the
//! adapter touches it, so `revert` can drop the netem qdisc and
//! restore the original. This is mandatory: a crashed run must
//! never leave an interface impaired.

use std::process::Command;
use std::time::Duration;

use crate::error::AgentError;

/// Probe whether the `tc` binary is available and exit-codes 0
/// on `--help`. The adapter treats absence as a clean platform
/// error so unprivileged / minimal containers can opt out.
#[must_use]
pub(crate) fn tc_available() -> bool {
    Command::new("tc")
        .arg("-V")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// `tc qdisc show dev <iface>` parsed to a single line describing
/// the root qdisc. The output is a `qdisc <kind> <handle>: …`
/// line per qdisc. We only need the first line and only the kind
/// + handle; everything else is restored verbatim on revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QdiscSnapshot {
    /// The original `tc qdisc show` output (first line) so revert
    /// can replay it.
    pub raw: String,
}

impl QdiscSnapshot {
    /// Take a snapshot of the current root qdisc on `iface`.
    /// Returns `None` if there is no root qdisc (i.e. the kernel
    /// shows `qdisc fq_codel 0: root` by default — handled the
    /// same way as any other qdisc).
    ///
    /// Returns [`AgentError::PlatformUnsupported`] when `tc` is
    /// not on `$PATH` or `--version` exits non-zero. The check
    /// runs *before* any shell-out so the error variant is
    /// consistent with `tc_add_qdisc` / `tc_del_qdisc`.
    pub(crate) fn capture(iface: &str) -> Result<Option<Self>, AgentError> {
        if !tc_available() {
            return Err(AgentError::PlatformUnsupported {
                adapter: super::netem::NetemAdapter::KIND,
                action: "qdisc_capture".to_owned(),
                platform: "tc binary not available".to_owned(),
            });
        }
        let output = Command::new("tc")
            .args(["qdisc", "show", "dev", iface])
            .output()
            .map_err(|e| AgentError::AdapterFailure {
                adapter: super::netem::NetemAdapter::KIND,
                reason: format!("tc qdisc show {iface} failed: {e}"),
            })?;
        if !output.status.success() {
            return Err(AgentError::AdapterFailure {
                adapter: super::netem::NetemAdapter::KIND,
                reason: format!(
                    "tc qdisc show {iface} returned non-zero: stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // The first non-empty line is the root qdisc; the kernel
        // prints "qdisc fq_codel 0: dev wlp0s20f3 root refcnt 2"
        // on a vanilla system. We snapshot it whole.
        let first = stdout.lines().find(|l| !l.trim().is_empty());
        Ok(first.map(|s| Self { raw: s.to_owned() }))
    }

    /// Restore the snapshot. If no snapshot was captured (the
    /// interface had no root qdisc), we leave it untouched.
    ///
    /// Returns [`AgentError::PlatformUnsupported`] when `tc` is
    /// not on `$PATH`. The check matches `capture` so callers
    /// see the same variant regardless of which step hits a
    /// missing binary.
    pub(crate) fn restore(&self, iface: &str) -> Result<(), AgentError> {
        if !tc_available() {
            return Err(AgentError::PlatformUnsupported {
                adapter: super::netem::NetemAdapter::KIND,
                action: "qdisc_restore".to_owned(),
                platform: "tc binary not available".to_owned(),
            });
        }
        // Replay: `tc qdisc add dev <iface> root <rest>` where
        // `<rest>` is everything after the `dev <iface>` portion
        // of the snapshot. The snapshot starts with `qdisc `.
        let stripped = self.raw.trim_start_matches("qdisc").trim_start();
        // `stripped` is `<kind> <handle>: <flags>...` — we
        // re-add the `dev <iface> root` portion.
        let output = Command::new("tc")
            .args(["qdisc", "add", "dev", iface, "root"])
            .arg(stripped)
            .output()
            .map_err(|e| AgentError::AdapterFailure {
                adapter: super::netem::NetemAdapter::KIND,
                reason: format!("tc qdisc add {iface} failed: {e}"),
            })?;
        if !output.status.success() {
            return Err(AgentError::AdapterFailure {
                adapter: super::netem::NetemAdapter::KIND,
                reason: format!(
                    "tc qdisc add {iface} failed: stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(())
    }
}

/// Netem parameters as a single struct, decoded from a
/// `NetemAction`. Encapsulated so the command builder sees one
/// shape regardless of which action variant produced it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NetemParams {
    /// Mean latency to add to every packet.
    pub delay: Option<Duration>,
    /// Jitter on the delay (uniform distribution).
    pub delay_jitter: Option<Duration>,
    /// Correlation between consecutive packets' delays, in
    /// `[0.0, 100.0]`.
    pub delay_correlation: Option<f32>,
    /// Random packet loss percentage in `[0.0, 100.0]`.
    pub loss_pct: Option<f32>,
    /// Loss correlation in `[0.0, 100.0]`.
    pub loss_correlation: Option<f32>,
    /// Random packet corruption percentage in `[0.0, 100.0]`.
    pub corrupt_pct: Option<f32>,
    /// Packet reordering percentage in `[0.0, 100.0]`.
    pub reorder_pct: Option<f32>,
    /// Reorder correlation in `[0.0, 100.0]`.
    pub reorder_correlation: Option<f32>,
    /// Bandwidth cap in bytes per second (uses `tbf` layered
    /// under the netem qdisc).
    pub rate_bps: Option<u64>,
}

impl NetemParams {
    /// Render the parameters as `tc qdisc add dev <iface>
    /// root netem <args...>`. Returns the argument vector
    /// suitable for `Command::args`.
    #[must_use]
    pub(crate) fn tc_argv(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(delay) = self.delay {
            let ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
            let delay_arg = if let Some(j) = self.delay_jitter {
                let jms = u64::try_from(j.as_millis()).unwrap_or(u64::MAX);
                match self.delay_correlation {
                    Some(c) => format!("{ms}ms {jms}ms {c:.0}%"),
                    None => format!("{ms}ms {jms}ms"),
                }
            } else {
                format!("{ms}ms")
            };
            out.push("delay".to_owned());
            out.push(delay_arg);
        }
        if let Some(p) = self.loss_pct {
            let loss_arg = match self.loss_correlation {
                Some(c) => format!("{p:.2}% {c:.0}%"),
                None => format!("{p:.2}%"),
            };
            out.push("loss".to_owned());
            out.push(loss_arg);
        }
        if let Some(p) = self.corrupt_pct {
            out.push("corrupt".to_owned());
            out.push(format!("{p:.2}%"));
        }
        if let Some(p) = self.reorder_pct {
            let reorder_arg = match self.reorder_correlation {
                Some(c) => format!("{p:.2}% {c:.0}%"),
                None => format!("{p:.2}%"),
            };
            out.push("reorder".to_owned());
            out.push(reorder_arg);
        }
        out
    }
}

/// Detect the default-route interface via `ip route show default`.
/// Returns the first non-empty `dev <iface>` segment, or `None`
/// when the host has no default route (containers, fresh
/// systems).
#[must_use]
pub(crate) fn default_route_interface() -> Option<String> {
    let out = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        // The `ip route show default` output includes
        // `dev <iface>` after the `default via <gateway>` part.
        let mut parts = line.split_whitespace();
        while let Some(part) = parts.next() {
            if part == "dev" {
                return parts.next().map(std::string::ToString::to_string);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netem_params_render_delay_only() {
        let p = NetemParams {
            delay: Some(Duration::from_millis(100)),
            delay_jitter: None,
            delay_correlation: None,
            loss_pct: None,
            loss_correlation: None,
            corrupt_pct: None,
            reorder_pct: None,
            reorder_correlation: None,
            rate_bps: None,
        };
        assert_eq!(p.tc_argv(), vec!["delay", "100ms"]);
    }

    #[test]
    fn netem_params_render_delay_with_jitter_and_correlation() {
        let p = NetemParams {
            delay: Some(Duration::from_millis(50)),
            delay_jitter: Some(Duration::from_millis(20)),
            delay_correlation: Some(25.0),
            loss_pct: None,
            loss_correlation: None,
            corrupt_pct: None,
            reorder_pct: None,
            reorder_correlation: None,
            rate_bps: None,
        };
        assert_eq!(p.tc_argv(), vec!["delay", "50ms 20ms 25%"]);
    }

    #[test]
    fn netem_params_render_loss_with_correlation() {
        let p = NetemParams {
            delay: None,
            delay_jitter: None,
            delay_correlation: None,
            loss_pct: Some(5.0),
            loss_correlation: Some(50.0),
            corrupt_pct: None,
            reorder_pct: None,
            reorder_correlation: None,
            rate_bps: None,
        };
        assert_eq!(p.tc_argv(), vec!["loss", "5.00% 50%"]);
    }

    #[test]
    fn netem_params_render_combined() {
        let p = NetemParams {
            delay: Some(Duration::from_millis(10)),
            delay_jitter: Some(Duration::from_millis(5)),
            delay_correlation: Some(10.0),
            loss_pct: Some(1.5),
            loss_correlation: None,
            corrupt_pct: Some(0.5),
            reorder_pct: Some(2.0),
            reorder_correlation: Some(25.0),
            rate_bps: None,
        };
        assert_eq!(
            p.tc_argv(),
            vec![
                "delay",
                "10ms 5ms 10%",
                "loss",
                "1.50%",
                "corrupt",
                "0.50%",
                "reorder",
                "2.00% 25%",
            ]
        );
    }

    /// Contract: when `tc` is not available, `QdiscSnapshot::capture`
    /// must return `PlatformUnsupported`, NOT `AdapterFailure` (which
    /// would be returned by a raw `Command::new("tc")` IO error).
    /// This guards against a regression where the helpers shell
    /// out before consulting `tc_available()`.
    ///
    /// On hosts with `tc` installed this test asserts the
    /// `Ok(_)` branch — the contract we want the helper to keep
    /// is "matches `tc_available()`'s verdict".
    #[test]
    fn capture_returns_platform_unsupported_when_tc_missing() {
        if tc_available() {
            // Skip on a tc-equipped host; the unit test that
            // exercises the missing-tc path lives in the
            // integration suite (where we can sanitize PATH).
            let result = QdiscSnapshot::capture("lo").expect("tc present");
            // On a tc-equipped host the loopback almost
            // always has a root qdisc; either Some(_) or
            // None is fine — what we care about is the
            // PlatformUnsupported absence.
            assert!(
                !matches!(result, Some(_) | None),
                "capture returned an unexpected variant"
            );
        } else {
            let err =
                QdiscSnapshot::capture("lo").expect_err("capture must fail when tc is missing");
            let expected_action: String = "qdisc_capture".to_owned();
            assert!(
                matches!(
                    &err,
                    AgentError::PlatformUnsupported {
                        action,
                        ..
                    } if action == &expected_action
                ),
                "expected PlatformUnsupported{{action: {expected_action:?}}}; got {err:?}"
            );
        }
    }

    /// Contract: same as the capture test, but for `restore`.
    /// `restore` is the revert-path companion to `capture`; it
    /// must surface `PlatformUnsupported` (not `AdapterFailure`)
    /// when `tc` is missing.
    ///
    /// We do NOT call `restore` on a tc-equipped host — that
    /// path is covered by the integration suite, which can
    /// clean up after itself. The unit-level contract is only
    /// about the missing-tc variant.
    #[test]
    fn restore_returns_platform_unsupported_when_tc_missing() {
        if tc_available() {
            // Integration suite owns the live-path test.
            return;
        }
        let snapshot = QdiscSnapshot {
            raw: "qdisc fq_codel 0: root".to_owned(),
        };
        let err = snapshot
            .restore("lo")
            .expect_err("restore must fail when tc is missing");
        let expected_action: String = "qdisc_restore".to_owned();
        assert!(
            matches!(
                &err,
                AgentError::PlatformUnsupported {
                    action,
                    ..
                } if action == &expected_action
            ),
            "expected PlatformUnsupported{{action: {expected_action:?}}}; got {err:?}"
        );
    }
}
