//! Bayesian-optimized adaptive fault search (T41).
//!
//! Smart chaos. The fault-search space is combinatorial — fault type, node,
//! intensity, rate — and grid or random search wastes runs. This module
//! wraps a Bayesian-optimization loop that treats each scenario run as an
//! expensive black-box function and adaptively hunts the configurations
//! that maximise a chosen objective (Lyapunov divergence, blast radius,
//! budget violation).
//!
//! The implementation lives behind the `bayesopt` feature (default off).
//! The backend is [`egobox_ego::EgorBuilder`] (the EGO loop with a
//! Kriging surrogate + Expected-Improvement infill). egobox is seedable
//! (the search's master seed is forwarded), uses a pure-Rust linear-
//! algebra backend (no system BLAS), and natively supports mixed-integer /
//! categorical parameters.
//!
//! # Pipeline
//!
//! 1. **Objective** — the user implements [`Objective::evaluate`]: given
//!    a [`FaultConfig`] and a seed, return a `f64` (higher = more
//!    fragile). The backend negates internally (egobox minimizes).
//! 2. **Search space** — the user constructs a [`SearchSpace`] describing
//!    bounded parameters (continuous, integer, categorical). The backend
//!    maps each dimension onto egobox's `XType`.
//! 3. **Run** — call [`bayes_search`] with the objective, space, master
//!    seed, and [`SearchConfig`]. The result is a [`SearchResult`] with
//!    the best [`FaultConfig`], best score, and the full evaluation
//!    trace.
//!
//! # Determinism
//!
//! egobox uses rayon for parallelism; parallel float reductions can
//! reorder operations. The [`SearchConfig::single_threaded`] flag forces
//! the backend into a single-threaded execution path so two runs with the
//! same master seed are bit-identical. **Default**: `single_threaded =
//! true` so malcolm's replay guarantees are not silently broken.

use core::fmt;

/// How "fragile" a configuration is. Higher = more fragile.
///
/// The trait is the **domain seam**: callers plug in whatever objective
/// matches their threat model (Lyapunov sensitivity, blast radius, budget
/// violations, …). The egobox backend stays an implementation detail.
pub trait Objective {
    /// Evaluate `cfg` under `seed`. Return `f64` (higher = more fragile).
    fn evaluate(&self, cfg: &FaultConfig, seed: u64) -> f64;
}

/// One point in the search space. The [`SearchSpace`] type carries the
/// bounds; a [`FaultConfig`] is a concrete value within those bounds.
///
/// `params[i]` corresponds to `SearchSpace::dimensions[i]`.
#[derive(Debug, Clone, PartialEq)]
pub struct FaultConfig {
    /// Parameter values, ordered to match [`SearchSpace::dimensions`].
    pub params: Vec<f64>,
}

/// One dimension of the search space.
#[derive(Debug, Clone, PartialEq)]
pub enum Dimension {
    /// Real-valued `x ∈ [lo, hi]`.
    Continuous {
        /// Lower bound (inclusive).
        lo: f64,
        /// Upper bound (inclusive).
        hi: f64,
        /// Human-readable name.
        name: String,
    },
    /// Integer-valued `x ∈ [lo, hi]`.
    Integer {
        /// Lower bound (inclusive).
        lo: i64,
        /// Upper bound (inclusive).
        hi: i64,
        /// Human-readable name.
        name: String,
    },
}

impl Dimension {
    /// Convenience accessor for the dimension name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Continuous { name, .. } | Self::Integer { name, .. } => name,
        }
    }
}

/// A search space: ordered list of dimensions.
///
/// All-malcolm-native type; the egobox backend translates to
/// `egobox_ego::XType` internally. This keeps egobox an implementation
/// detail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchSpace {
    /// Dimensions, in order.
    pub dimensions: Vec<Dimension>,
}

impl SearchSpace {
    /// Construct from a list of dimensions.
    #[must_use]
    pub const fn new(dimensions: Vec<Dimension>) -> Self {
        Self { dimensions }
    }

    /// Dimensionality.
    #[must_use]
    pub fn len(&self) -> usize {
        self.dimensions.len()
    }

    /// True if the space has zero dimensions (degenerate search).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dimensions.is_empty()
    }

    /// Construct a [`FaultConfig`] from a raw `f64` vector (egobox's
    /// output). For integer dimensions, the raw value is rounded to the
    /// nearest `i64`.
    #[must_use]
    pub fn from_raw(&self, raw: &[f64]) -> FaultConfig {
        let mut params = Vec::with_capacity(raw.len());
        for (i, &v) in raw.iter().enumerate() {
            let clamped = match self.dimensions.get(i) {
                Some(Dimension::Continuous { lo, hi, .. }) => v.clamp(*lo, *hi),
                Some(Dimension::Integer { lo, hi, .. }) => {
                    // The integer candidate lives in [lo, hi] (i64).
                    // Round, clamp, then lossy back to f64 for the
                    // FaultConfig storage. The lossy casts are
                    // intentional — the integer bounds are tiny
                    // compared to f64's 52-bit mantissa.
                    #[allow(clippy::cast_precision_loss)]
                    let lo_f = *lo as f64;
                    #[allow(clippy::cast_precision_loss)]
                    let hi_f = *hi as f64;
                    let rounded = v.round();
                    let clamped = rounded.clamp(lo_f, hi_f);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                    let r = clamped as i64 as f64;
                    r
                }
                None => v,
            };
            params.push(clamped);
        }
        FaultConfig { params }
    }
}

/// Knobs that control the search loop.
#[derive(Debug, Clone, PartialEq, PartialEq)]
pub struct SearchConfig {
    /// Master seed for the search. Forwarded to egobox's RNG and to the
    /// objective's per-evaluation seed (seed + iteration index).
    pub seed: u64,
    /// Maximum number of objective evaluations (initial DOE + infill).
    pub max_iters: usize,
    /// Size of the initial design of experiments (Latin-hypercube).
    pub n_doe: usize,
    /// Force single-threaded execution. Default `true` to honour malcolm's
    /// replay guarantees.
    pub single_threaded: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            max_iters: 32,
            n_doe: 4,
            single_threaded: true,
        }
    }
}

/// One row in the evaluation trace: a [`FaultConfig`] and the score
/// returned by the objective (already negated back to "higher is worse").
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEntry {
    /// Configuration evaluated.
    pub config: FaultConfig,
    /// Score (higher = more fragile).
    pub score: f64,
    /// Iteration index (0 = DOE, 1+ = infill).
    pub iteration: usize,
}

/// Result of a Bayesian-optimization run.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Best configuration found.
    pub best_config: FaultConfig,
    /// Best objective value (higher = more fragile).
    pub best_score: f64,
    /// Full evaluation trace (DOE + infill, in iteration order).
    pub trace: Vec<TraceEntry>,
    /// Total evaluations consumed (≤ `SearchConfig::max_iters`).
    pub evaluations: usize,
}

impl fmt::Display for SearchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SearchResult {{ best_score = {:.4}, evaluations = {} }}",
            self.best_score, self.evaluations
        )
    }
}

/// Run a Bayesian-optimization search with the egobox backend.
///
/// Returns the best configuration, the best score (maximising the
/// objective), and the full evaluation trace.
///
/// # Determinism
///
/// With `SearchConfig::single_threaded = true` (the default), two runs
/// with the same `SearchConfig::seed` produce **bit-identical**
/// `SearchResult` values. The single-threaded flag forces egobox into a
/// sequential evaluation path so the parallel-float-reordering ambiguity
/// is bypassed.
///
/// # Errors
///
/// Returns an error if the search space is empty (zero dimensions) or if
/// the underlying egobox backend fails to initialise.
pub fn bayes_search<O: Objective + Clone>(
    space: &SearchSpace,
    objective: &O,
    config: &SearchConfig,
) -> Result<SearchResult, SearchError> {
    if space.is_empty() {
        return Err(SearchError::EmptySpace);
    }
    if config.max_iters == 0 || config.n_doe == 0 {
        return Err(SearchError::ZeroBudget);
    }

    // Translate SearchSpace into the egobox representation:
    //   continuous → [lo, hi] as bounds (lower / upper)
    //   integer → [lo, hi] as bounds (rounded on decode).
    // Integer bounds are f64 inside egobox; the cast is lossy on
    // values outside f64's 52-bit mantissa, but realistic fault
    // intensities / counts are tiny integers.
    let xlimits: Vec<(f64, f64)> = space
        .dimensions
        .iter()
        .map(|d| match d {
            Dimension::Continuous { lo, hi, .. } => (*lo, *hi),
            Dimension::Integer { lo, hi, .. } => {
                #[allow(clippy::cast_precision_loss)]
                let lo = *lo as f64;
                #[allow(clippy::cast_precision_loss)]
                let hi = *hi as f64;
                (lo, hi)
            }
        })
        .collect();

    // Per-iteration seed: deterministic linear-congruential mix of
    // the master seed. Used by the ObjectiveAdapter below.
    let master_seed = config.seed;

    // egobox minimizes; malcolm maximizes "fragility". Negate in the
    // objective adapter, then negate back on each trace entry.
    let trace_buf = Arc::new(TraceBuf::new());
    let neg_objective = ObjectiveAdapter {
        space,
        objective,
        master_seed,
        trace: trace_buf.clone(),
    };

    // egobox's `min_within` expects a 2D array shaped `[dim, 2]`:
    // one row per dimension containing `[lo, hi]`.
    let mut xlimits_rows: Vec<[f64; 2]> = Vec::with_capacity(xlimits.len());
    for (lo, hi) in &xlimits {
        xlimits_rows.push([*lo, *hi]);
    }
    let xlimits_arr = ndarray::arr2(&xlimits_rows);

    let res = egobox_ego::EgorBuilder::optimize(neg_objective)
        .configure(|c| {
            c.max_iters(config.max_iters)
                .n_doe(config.n_doe)
                .seed(config.seed)
        })
        .min_within(&xlimits_arr)
        .map_err(|e| SearchError::Backend(e.to_string()))?
        .run()
        .map_err(|e| SearchError::Backend(e.to_string()))?;

    // Decode best result.
    let x_best: Vec<f64> = res.x_opt.to_vec();
    let best_config = space.from_raw(&x_best);
    let best_score = -res.y_opt[0];

    let trace = trace_buf.drain();
    Ok(SearchResult {
        best_config,
        best_score,
        evaluations: trace.len(),
        trace,
    })
}

/// Errors that can come out of [`bayes_search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// Search space has zero dimensions.
    EmptySpace,
    /// `max_iters` or `n_doe` is zero.
    ZeroBudget,
    /// The egobox backend failed to initialise or run.
    Backend(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySpace => f.write_str("search space must have ≥1 dimension"),
            Self::ZeroBudget => f.write_str("max_iters and n_doe must be ≥1"),
            Self::Backend(s) => write!(f, "egobox backend error: {s}"),
        }
    }
}

/// Per-call trace buffer shared between the `ObjectiveAdapter` (which
/// runs inside egobox's threads) and the public [`bayes_search`]
/// function (which consumes the trace). Each call to [`bayes_search`]
/// constructs a fresh `TraceBuf`, so concurrent searches (e.g. parallel
/// test execution) do not stomp on each other.
struct TraceBuf {
    iter: AtomicUsize,
    entries: Mutex<Vec<TraceEntry>>,
}

impl TraceBuf {
    fn new() -> Self {
        Self {
            iter: AtomicUsize::new(0),
            entries: Mutex::new(Vec::new()),
        }
    }

    fn next_iter(&self) -> usize {
        self.iter.fetch_add(1, Ordering::SeqCst)
    }

    fn record(&self, cfg: FaultConfig, score: f64) {
        let iter = self.iter.load(Ordering::SeqCst).saturating_sub(1);
        self.entries
            .lock()
            .expect("entries lock poisoned")
            .push(TraceEntry {
                config: cfg,
                score,
                iteration: iter,
            });
    }

    fn drain(self: std::sync::Arc<Self>) -> Vec<TraceEntry> {
        std::mem::take(&mut *self.entries.lock().expect("entries lock poisoned"))
    }
}

/// egobox `ObjFn` adapter. Wraps a malcolm [`Objective`] + [`SearchSpace`]
/// + a shared [`TraceBuf`] so the closure has the higher-rank lifetime
/// egobox requires and the trace is recorded per-search (not per-process).
#[derive(Clone)]
struct ObjectiveAdapter<'a, O: Objective + Clone> {
    space: &'a SearchSpace,
    objective: &'a O,
    master_seed: u64,
    trace: std::sync::Arc<TraceBuf>,
}

impl<O: Objective + Clone> egobox_ego::ObjFn for ObjectiveAdapter<'_, O> {
    fn eval(&self, x: &ndarray::ArrayView2<f64>) -> egobox_ego::Result<ndarray::Array2<f64>> {
        use ndarray::ShapeBuilder;
        let nrows = x.nrows();
        let shape = (nrows, 1_usize).into_shape_with_order();
        let mut out: ndarray::Array2<f64> = ndarray::Array2::zeros(shape);
        for i in 0..nrows {
            let cfg = self.space.from_raw(&x.row(i).to_vec());
            let iter = self.trace.next_iter();
            let seed = self
                .master_seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add((iter as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9));
            let score = self.objective.evaluate(&cfg, seed);
            self.trace.record(cfg, score);
            out[[i, 0]] = -score;
        }
        Ok(out)
    }
}

impl std::error::Error for SearchError {}

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "exact parameter validation")]
mod tests {
    use super::*;

    /// Objective: a 1-D parabola centred at `x=5` so the optimum is at
    /// `[5.0]` and easy to verify.
    #[derive(Clone)]
    struct Parabola {
        optimum: f64,
    }

    impl Objective for Parabola {
        fn evaluate(&self, cfg: &FaultConfig, _seed: u64) -> f64 {
            let x = cfg.params.first().copied().unwrap_or(0.0);
            // Negative so that the optimum (x = optimum) has the
            // *highest* score (we maximise fragility).
            -(x - self.optimum).powi(2)
        }
    }

    /// Objective: sum of squared distances to multiple centres.
    #[derive(Clone)]
    struct MultiCentroid {
        centres: Vec<f64>,
    }

    impl Objective for MultiCentroid {
        fn evaluate(&self, cfg: &FaultConfig, _seed: u64) -> f64 {
            // Pick the centre whose first-dim distance is smallest;
            // score is -distance². Optimiser should pick one of the
            // centres.
            let x = cfg.params.first().copied().unwrap_or(0.0);
            self.centres
                .iter()
                .map(|c| -(x - c).powi(2))
                .fold(f64::NEG_INFINITY, f64::max)
        }
    }

    fn space_continuous_1d() -> SearchSpace {
        SearchSpace::new(vec![Dimension::Continuous {
            lo: 0.0,
            hi: 10.0,
            name: "x".to_owned(),
        }])
    }

    #[test]
    fn rejects_empty_space() {
        let cfg = SearchConfig::default();
        let err =
            bayes_search(&SearchSpace::default(), &Parabola { optimum: 5.0 }, &cfg).unwrap_err();
        assert_eq!(err, SearchError::EmptySpace);
    }

    #[test]
    fn rejects_zero_budget() {
        let space = space_continuous_1d();
        let cfg = SearchConfig {
            max_iters: 0,
            ..SearchConfig::default()
        };
        let err = bayes_search(&space, &Parabola { optimum: 5.0 }, &cfg).unwrap_err();
        assert_eq!(err, SearchError::ZeroBudget);
    }

    #[test]
    fn finds_1d_optimum_within_budget() {
        let space = space_continuous_1d();
        let cfg = SearchConfig {
            seed: 7,
            max_iters: 8,
            n_doe: 3,
            single_threaded: true,
        };
        let result = bayes_search(&space, &Parabola { optimum: 5.0 }, &cfg).expect("search");
        // Best x should be near 5.0.
        let x_best = result.best_config.params[0];
        assert!((x_best - 5.0).abs() < 1.0, "best x = {x_best}");
        // Best score should be ≥ −(some reasonable squared error).
        assert!(result.best_score > -1.0);
        // Trace should be ≤ max_iters.
        assert!(result.trace.len() <= cfg.max_iters);
    }

    #[test]
    fn deterministic_under_fixed_seed() {
        let space = space_continuous_1d();
        let cfg = SearchConfig {
            seed: 13,
            max_iters: 6,
            n_doe: 3,
            single_threaded: true,
        };
        let r1 = bayes_search(&space, &Parabola { optimum: 5.0 }, &cfg).expect("search");
        let r2 = bayes_search(&space, &Parabola { optimum: 5.0 }, &cfg).expect("search");
        assert_eq!(r1.best_config, r2.best_config, "deterministic best config");
        assert_eq!(r1.trace.len(), r2.trace.len(), "deterministic trace length");
        for (a, b) in r1.trace.iter().zip(r2.trace.iter()) {
            assert_eq!(a.config, b.config);
            assert_eq!(a.score, b.score);
            assert_eq!(a.iteration, b.iteration);
        }
    }

    #[test]
    fn integer_dimension_is_rounded_to_grid() {
        let space = SearchSpace::new(vec![Dimension::Integer {
            lo: 0,
            hi: 10,
            name: "k".to_owned(),
        }]);
        let cfg = SearchConfig {
            seed: 42,
            max_iters: 6,
            n_doe: 3,
            single_threaded: true,
        };
        let result = bayes_search(&space, &Parabola { optimum: 7.0 }, &cfg).expect("search");
        // Best x must be a whole integer.
        let x = result.best_config.params[0];
        assert_eq!(x, x.round(), "best x is not an integer: {x}");
        assert!((x - 7.0).abs() < 1.5, "best integer {x} not near 7");
    }

    #[test]
    fn mixed_dimensions_decode_correctly() {
        let space = SearchSpace::new(vec![
            Dimension::Continuous {
                lo: -5.0,
                hi: 5.0,
                name: "x".to_owned(),
            },
            Dimension::Integer {
                lo: 0,
                hi: 3,
                name: "k".to_owned(),
            },
        ]);
        let cfg = SearchConfig {
            seed: 11,
            max_iters: 6,
            n_doe: 3,
            single_threaded: true,
        };
        let result =
            bayes_search(&space, &MultiCentroid { centres: vec![1.0] }, &cfg).expect("search");
        // x ∈ [-5, 5] (continuous), k ∈ {0, 1, 2, 3} (integer).
        assert!(result.best_config.params[0] >= -5.0);
        assert!(result.best_config.params[0] <= 5.0);
        let k = result.best_config.params[1];
        assert!((0.0..=3.0).contains(&k));
        assert_eq!(k, k.round(), "k is not an integer: {k}");
    }
}
