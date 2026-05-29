//! # malcolm-lens
//!
//! LLM interpretability layer for malcolm chaos engineering reports.
//!
//! This crate is strictly advisory — it reads [`malcolm::scenario::ScenarioReport`]
//! output and produces post-mortem narratives, anomaly flags, and replay
//! divergence explanations via an LLM provider. It is never on the fault
//! injection path.
//!
//! # Example
//!
//! ```rust
//! use malcolm_lens::LensProvider;
//! // Implementations (OllamaLens, AnthropicLens) will be added in T17.
//! let _: Option<Box<dyn LensProvider>> = None;
//! ```

use thiserror::Error;

/// Errors that can occur in the lens layer.
///
/// # Example
///
/// ```rust
/// use malcolm_lens::LensError;
/// let err = LensError::NotImplemented;
/// assert_eq!(err.to_string(), "not implemented");
/// ```
#[derive(Debug, Error)]
pub enum LensError {
    /// The requested operation has not been implemented yet.
    // TODO(T17): expand with provider-specific variants
    #[error("not implemented")]
    NotImplemented,
}

/// Contract for LLM providers that can analyze a chaos scenario report.
///
/// Each provider (Ollama, Anthropic, etc.) implements this trait.
/// The LLM is strictly advisory — never on the fault injection path.
///
/// # Example
///
/// ```rust
/// use malcolm_lens::{LensProvider, LensError};
///
/// struct StubProvider;
///
/// impl LensProvider for StubProvider {
///     fn analyze(&self, _report: &str) -> Result<String, LensError> {
///         Err(LensError::NotImplemented)
///     }
/// }
///
/// let provider = StubProvider;
/// assert!(provider.analyze("{}").is_err());
/// ```
// TODO(T17): expand with async variant, structured LensReport input/output
pub trait LensProvider {
    /// Analyze a serialized chaos scenario report and return a narrative.
    ///
    /// # Errors
    ///
    /// Returns [`LensError`] if the provider cannot process the report.
    fn analyze(&self, report: &str) -> Result<String, LensError>;
}
