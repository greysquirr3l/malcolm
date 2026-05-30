use thiserror::Error;

/// Errors that can occur while configuring or calling lens providers.
///
/// # Example
///
/// ```rust
/// use malcolm_lens::LensError;
///
/// let err = LensError::MissingApiKey;
/// assert_eq!(err.to_string(), "missing required API key");
/// ```
#[derive(Debug, Error)]
pub enum LensError {
    /// Provider name in `MALCOLM_LENS_PROVIDER` was not recognized.
    #[error("invalid provider: {0}")]
    InvalidProvider(String),

    /// Provider base URL failed local security policy checks.
    #[error("disallowed provider base url: {0}")]
    DisallowedBaseUrl(String),

    /// Required API key is missing for the selected provider.
    #[error("missing required API key")]
    MissingApiKey,

    /// Requested provider is not compiled in via Cargo features.
    #[error("provider is disabled by feature flag: {0}")]
    FeatureDisabled(&'static str),

    /// Input report could not be serialized for model prompting.
    #[error("failed to serialize scenario report: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Upstream provider returned an error.
    #[error("provider error: {0}")]
    ProviderError(String),

    /// LLM response could not be parsed into the expected structure.
    #[error("failed to parse llm response: {reason}")]
    ParseFailure {
        /// Original LLM payload that failed to parse.
        raw: String,
        /// Human-readable parser failure reason.
        reason: String,
    },

    /// Upstream provider timed out while waiting for a response.
    #[error("provider timeout")]
    Timeout,
}
