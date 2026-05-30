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
//! use malcolm_lens::{LensConfig, LensProvider, provider_from_config};
//!
//! let config = LensConfig::from_env().expect("config should parse");
//! let _provider: Result<Box<dyn LensProvider>, _> = provider_from_config(config);
//! ```
//!
//! # Worked Examples
//!
//! - [Post-mortem narrative](../examples/lens_postmortem.rs)
//! - [Adaptive scenario suggestions](../examples/lens_suggest.rs)
//! - [Replay divergence investigation](../examples/lens_divergence.rs)

mod analyzer;
mod config;
mod error;
mod parser;
mod prompt;
mod provider;
mod report;

#[cfg(feature = "anthropic")]
mod anthropic;
#[cfg(feature = "ollama")]
mod ollama;

pub use analyzer::{LensAnalyzer, LensAnalyzerBuilder};
pub use config::{LensConfig, Provider};
pub use error::LensError;
pub use parser::ResponseParser;
pub use prompt::{Directive, PromptBuilder};
pub use provider::{LensProvider, provider_from_config};
pub use report::{
    AnomalyFlag, DivergenceExplanation, LensReport, NarrativeReport, ParseWarning,
    ScenarioSuggestion, Severity,
};

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicLens;
#[cfg(feature = "ollama")]
pub use ollama::OllamaLens;
