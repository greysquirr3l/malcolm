//! Bifurcation threshold profiles and tipping-point detection.
//!
//! A [`BifurcationProfile`] describes a region in fault-parameter space with three
//! qualitative regimes: *stable* (below threshold), *sensitive* (near the tipping
//! point), and *chaotic* (above threshold). [`classify`] maps a scalar fault intensity
//! to one of these regimes given a profile.
//!
//! # Tracing note
//!
//! This crate is `no_std` and does not depend on `tracing`. Tracing instrumentation
//! (e.g. emitting a `warn!` event when [`Regime::Chaotic`] is reached) is the
//! responsibility of the `malcolm` assembly layer (T14), which wraps these primitives
//! with the appropriate tracing calls.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
//!
//! let profile = BifurcationProfile::network_partition();
//! assert_eq!(classify(0.3, &profile), Regime::Stable);
//! assert_eq!(classify(0.6, &profile), Regime::Sensitive);
//! assert_eq!(classify(0.9, &profile), Regime::Chaotic);
//! ```

// ── Regime ────────────────────────────────────────────────────────────────────

/// Qualitative behaviour regime of a system at a given fault intensity.
///
/// The enum is `#[non_exhaustive]` so that additional regimes can be added in
/// future versions without breaking downstream match arms.
///
/// # Example
///
/// ```rust
/// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
///
/// let profile = BifurcationProfile::latency_cascade();
/// assert_eq!(classify(0.1, &profile), Regime::Stable);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Regime {
    /// Perturbations damp out; the system returns to its nominal operating point.
    Stable,
    /// The system is near its tipping point; small additional faults may tip it
    /// into chaotic behaviour.
    Sensitive,
    /// The system is above its tipping point; cascading failures are likely.
    Chaotic,
}

// ── BifurcationProfile ────────────────────────────────────────────────────────

/// A named fault-parameter profile that describes where a system's behaviour
/// changes qualitatively.
///
/// The profile defines a *threshold* intensity and a *sensitivity window* centred
/// on that threshold. Intensities within the window map to [`Regime::Sensitive`];
/// those below map to [`Regime::Stable`]; those above map to [`Regime::Chaotic`].
///
/// # Example
///
/// ```rust
/// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
///
/// let profile = BifurcationProfile::memory_pressure();
/// // Well below the 0.75 threshold → stable.
/// assert_eq!(classify(0.4, &profile), Regime::Stable);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BifurcationProfile {
    /// Intensity at which behaviour changes qualitatively.
    pub threshold: f64,
    /// Width of the near-threshold band; the sensitive window spans
    /// `[threshold - sensitivity_window/2, threshold + sensitivity_window/2]`.
    pub sensitivity_window: f64,
    /// Human-readable label for this profile (e.g. `"network_partition"`).
    pub label: &'static str,
}

impl BifurcationProfile {
    /// Profile for a network partition fault.
    ///
    /// - threshold: 0.60
    /// - sensitivity\_window: 0.20
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    ///
    /// let p = BifurcationProfile::network_partition();
    /// assert_eq!(classify(0.3, &p), Regime::Stable);
    /// assert_eq!(classify(0.6, &p), Regime::Sensitive);
    /// assert_eq!(classify(0.9, &p), Regime::Chaotic);
    /// ```
    #[must_use]
    pub const fn network_partition() -> Self {
        Self {
            threshold: 0.6,
            sensitivity_window: 0.2,
            label: "network_partition",
        }
    }

    /// Profile for a memory pressure fault.
    ///
    /// - threshold: 0.75
    /// - sensitivity\_window: 0.15
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    ///
    /// let p = BifurcationProfile::memory_pressure();
    /// assert_eq!(classify(0.4, &p), Regime::Stable);
    /// assert_eq!(classify(0.75, &p), Regime::Sensitive);
    /// assert_eq!(classify(0.95, &p), Regime::Chaotic);
    /// ```
    #[must_use]
    pub const fn memory_pressure() -> Self {
        Self {
            threshold: 0.75,
            sensitivity_window: 0.15,
            label: "memory_pressure",
        }
    }

    /// Profile for a latency cascade fault.
    ///
    /// - threshold: 0.50
    /// - sensitivity\_window: 0.25
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    ///
    /// let p = BifurcationProfile::latency_cascade();
    /// assert_eq!(classify(0.1, &p), Regime::Stable);
    /// assert_eq!(classify(0.5, &p), Regime::Sensitive);
    /// assert_eq!(classify(0.9, &p), Regime::Chaotic);
    /// ```
    #[must_use]
    pub const fn latency_cascade() -> Self {
        Self {
            threshold: 0.5,
            sensitivity_window: 0.25,
            label: "latency_cascade",
        }
    }

    /// Profile for a Byzantine node fault.
    ///
    /// - threshold: 0.33
    /// - sensitivity\_window: 0.15
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    ///
    /// let p = BifurcationProfile::byzantine_node();
    /// assert_eq!(classify(0.1, &p), Regime::Stable);
    /// assert_eq!(classify(0.33, &p), Regime::Sensitive);
    /// assert_eq!(classify(0.6, &p), Regime::Chaotic);
    /// ```
    #[must_use]
    pub const fn byzantine_node() -> Self {
        Self {
            threshold: 0.33,
            sensitivity_window: 0.15,
            label: "byzantine_node",
        }
    }

    /// Profile for a clock skew fault.
    ///
    /// - threshold: 0.55
    /// - sensitivity\_window: 0.20
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    ///
    /// let p = BifurcationProfile::clock_skew();
    /// assert_eq!(classify(0.1, &p), Regime::Stable);
    /// assert_eq!(classify(0.55, &p), Regime::Sensitive);
    /// assert_eq!(classify(0.9, &p), Regime::Chaotic);
    /// ```
    #[must_use]
    pub const fn clock_skew() -> Self {
        Self {
            threshold: 0.55,
            sensitivity_window: 0.20,
            label: "clock_skew",
        }
    }
}

// ── classify ──────────────────────────────────────────────────────────────────

/// Classify a fault `intensity` against a [`BifurcationProfile`].
///
/// | Condition | Regime |
/// |---|---|
/// | `intensity < threshold - window/2` | [`Regime::Stable`] |
/// | `threshold - window/2 <= intensity <= threshold + window/2` | [`Regime::Sensitive`] |
/// | `intensity > threshold + window/2` | [`Regime::Chaotic`] |
///
/// # Tracing note
///
/// Emitting a diagnostic event when [`Regime::Chaotic`] is returned is handled by
/// the `malcolm` assembly layer; this function is `no_std` and has no side effects.
///
/// # Example
///
/// ```rust
/// use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
///
/// let p = BifurcationProfile::network_partition(); // threshold=0.6, window=0.2
/// // lower boundary of sensitive window: 0.6 - 0.1 = 0.5
/// assert_eq!(classify(0.5, &p), Regime::Sensitive);
/// // upper boundary of sensitive window: 0.6 + 0.1 = 0.7
/// assert_eq!(classify(0.7, &p), Regime::Sensitive);
/// ```
#[must_use]
pub fn classify(intensity: f64, profile: &BifurcationProfile) -> Regime {
    let half_window = profile.sensitivity_window / 2.0;
    let lower = profile.threshold - half_window;
    let upper = profile.threshold + half_window;

    if intensity < lower {
        Regime::Stable
    } else if intensity > upper {
        Regime::Chaotic
    } else {
        Regime::Sensitive
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{BifurcationProfile, Regime, classify};

    // ── network_partition ────────────────────────────────────────────────────

    #[test]
    fn network_partition_stable() {
        let p = BifurcationProfile::network_partition(); // threshold=0.6, window=0.2
        assert_eq!(classify(0.3, &p), Regime::Stable);
        assert_eq!(classify(0.0, &p), Regime::Stable);
    }

    #[test]
    fn network_partition_sensitive() {
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.6, &p), Regime::Sensitive);
        assert_eq!(classify(0.55, &p), Regime::Sensitive);
        assert_eq!(classify(0.65, &p), Regime::Sensitive);
    }

    #[test]
    fn network_partition_chaotic() {
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.9, &p), Regime::Chaotic);
        assert_eq!(classify(1.0, &p), Regime::Chaotic);
    }

    // ── memory_pressure ──────────────────────────────────────────────────────

    #[test]
    fn memory_pressure_stable() {
        let p = BifurcationProfile::memory_pressure(); // threshold=0.75, window=0.15
        assert_eq!(classify(0.4, &p), Regime::Stable);
    }

    #[test]
    fn memory_pressure_sensitive() {
        let p = BifurcationProfile::memory_pressure();
        assert_eq!(classify(0.75, &p), Regime::Sensitive);
        assert_eq!(classify(0.68, &p), Regime::Sensitive);
        assert_eq!(classify(0.82, &p), Regime::Sensitive);
    }

    #[test]
    fn memory_pressure_chaotic() {
        let p = BifurcationProfile::memory_pressure();
        assert_eq!(classify(0.95, &p), Regime::Chaotic);
    }

    // ── latency_cascade ──────────────────────────────────────────────────────

    #[test]
    fn latency_cascade_stable() {
        let p = BifurcationProfile::latency_cascade(); // threshold=0.5, window=0.25
        assert_eq!(classify(0.1, &p), Regime::Stable);
        assert_eq!(classify(0.0, &p), Regime::Stable);
    }

    #[test]
    fn latency_cascade_sensitive() {
        let p = BifurcationProfile::latency_cascade();
        assert_eq!(classify(0.5, &p), Regime::Sensitive);
        assert_eq!(classify(0.375, &p), Regime::Sensitive);
        assert_eq!(classify(0.625, &p), Regime::Sensitive);
    }

    #[test]
    fn latency_cascade_chaotic() {
        let p = BifurcationProfile::latency_cascade();
        assert_eq!(classify(0.9, &p), Regime::Chaotic);
    }

    // ── byzantine_node ───────────────────────────────────────────────────────

    #[test]
    fn byzantine_node_stable() {
        let p = BifurcationProfile::byzantine_node(); // threshold=0.33, window=0.15
        assert_eq!(classify(0.1, &p), Regime::Stable);
        assert_eq!(classify(0.0, &p), Regime::Stable);
    }

    #[test]
    fn byzantine_node_sensitive() {
        let p = BifurcationProfile::byzantine_node();
        assert_eq!(classify(0.33, &p), Regime::Sensitive);
        assert_eq!(classify(0.255, &p), Regime::Sensitive);
        assert_eq!(classify(0.405, &p), Regime::Sensitive);
    }

    #[test]
    fn byzantine_node_chaotic() {
        let p = BifurcationProfile::byzantine_node();
        assert_eq!(classify(0.6, &p), Regime::Chaotic);
    }

    // ── boundary conditions ──────────────────────────────────────────────────

    #[test]
    fn boundary_at_lower_edge_is_sensitive() {
        // network_partition: lower boundary = 0.6 - 0.1 = 0.5
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.5, &p), Regime::Sensitive);
    }

    #[test]
    fn boundary_at_upper_edge_is_sensitive() {
        // network_partition: upper boundary = 0.6 + 0.1 = 0.7
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.7, &p), Regime::Sensitive);
    }

    #[test]
    fn just_below_lower_edge_is_stable() {
        // Use a value clearly below 0.5 to avoid f64 rounding issues at the boundary.
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.499, &p), Regime::Stable);
    }

    #[test]
    fn just_above_upper_edge_is_chaotic() {
        let p = BifurcationProfile::network_partition();
        assert_eq!(classify(0.701, &p), Regime::Chaotic);
    }

    // ── non_exhaustive ───────────────────────────────────────────────────────

    /// Verify that `Regime` is `#[non_exhaustive]` by matching with a wildcard arm.
    ///
    /// Within the defining crate all variants are known so the wildcard is unreachable
    /// here — but external crates must include it. The `#[expect]` attribute documents
    /// this intent and keeps the lint clean.
    #[test]
    fn regime_non_exhaustive_wildcard_compiles() {
        let r = Regime::Stable;
        #[expect(
            unreachable_patterns,
            reason = "wildcard is only reachable in external crates"
        )]
        let _ = match r {
            Regime::Stable => 0,
            Regime::Sensitive => 1,
            Regime::Chaotic => 2,
            _ => 99,
        };
    }
}
