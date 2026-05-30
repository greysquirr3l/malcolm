use crate::LensError;

const OLLAMA_DEFAULT_MODEL: &str = "llama3.2";
const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
const DEFAULT_MAX_TOKENS: u32 = 1_024;

/// Provider selection for lens analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Provider {
    /// Local or proxied Ollama deployment.
    Ollama,
    /// Anthropic API deployment.
    Anthropic,
}

impl Provider {
    fn parse(value: &str) -> Result<Self, LensError> {
        let lowered = value.to_ascii_lowercase();
        match lowered.as_str() {
            "ollama" => Ok(Self::Ollama),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(LensError::InvalidProvider(value.to_owned())),
        }
    }

    const fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => OLLAMA_DEFAULT_MODEL,
            Self::Anthropic => ANTHROPIC_DEFAULT_MODEL,
        }
    }
}

/// Runtime configuration for lens providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensConfig {
    /// Provider backend choice.
    pub provider: Provider,
    /// Model identifier used by the selected provider.
    pub model: String,
    /// Optional provider base URL override.
    pub base_url: Option<String>,
    /// Requested max response token budget.
    pub max_tokens: u32,
}

impl LensConfig {
    /// Load lens configuration from process environment variables.
    ///
    /// Reads: `MALCOLM_LENS_PROVIDER`, `MALCOLM_LENS_MODEL`, `OLLAMA_BASE_URL`,
    /// `MALCOLM_LENS_MAX_TOKENS`, and `ANTHROPIC_API_KEY`.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::InvalidProvider`] when provider parsing fails.
    pub fn from_env() -> Result<Self, LensError> {
        Self::from_reader(|key| std::env::var(key).ok())
    }

    fn from_reader<F>(mut read: F) -> Result<Self, LensError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let provider = match read("MALCOLM_LENS_PROVIDER") {
            Some(raw) => Provider::parse(&raw)?,
            None => Provider::Ollama,
        };

        let model = read("MALCOLM_LENS_MODEL")
            .filter(|raw| !raw.trim().is_empty())
            .unwrap_or_else(|| provider.default_model().to_owned());

        let allow_remote_ollama =
            read("MALCOLM_LENS_ALLOW_REMOTE_OLLAMA").is_some_and(|raw| is_truthy(&raw));

        let base_url = read("OLLAMA_BASE_URL")
            .filter(|raw| !raw.trim().is_empty())
            .map(|value| value.trim().to_owned());

        if provider == Provider::Ollama
            && let Some(candidate) = base_url.as_deref()
        {
            validate_ollama_base_url(candidate, allow_remote_ollama)?;
        }

        let max_tokens = read("MALCOLM_LENS_MAX_TOKENS")
            .and_then(|raw| raw.parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_TOKENS);

        // Intentionally read this value here so configuration diagnostics can report
        // environment shape without requiring provider construction.
        let _anthropic_api_key_present = read("ANTHROPIC_API_KEY").is_some();

        Ok(Self {
            provider,
            model,
            base_url,
            max_tokens,
        })
    }
}

fn is_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn validate_ollama_base_url(base_url: &str, allow_remote: bool) -> Result<(), LensError> {
    let Some(host) = extract_host(base_url) else {
        return Err(LensError::DisallowedBaseUrl(base_url.to_owned()));
    };

    let host_lower = host.to_ascii_lowercase();

    if host_lower == "localhost" {
        return Ok(());
    }

    if let Ok(ip) = host_lower.parse::<std::net::IpAddr>() {
        if is_metadata_ip(&ip) {
            return Err(LensError::DisallowedBaseUrl(base_url.to_owned()));
        }
        if ip.is_loopback() {
            return Ok(());
        }
        if !allow_remote {
            return Err(LensError::DisallowedBaseUrl(base_url.to_owned()));
        }
        return Ok(());
    }

    if is_metadata_host(&host_lower) {
        return Err(LensError::DisallowedBaseUrl(base_url.to_owned()));
    }

    if allow_remote {
        Ok(())
    } else {
        Err(LensError::DisallowedBaseUrl(base_url.to_owned()))
    }
}

fn extract_host(base_url: &str) -> Option<&str> {
    let without_scheme = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);

    let authority = without_scheme.split('/').next()?.rsplit('@').next()?;
    if authority.is_empty() {
        return None;
    }

    if authority.starts_with('[') {
        let closing = authority.find(']')?;
        return authority.get(1..closing);
    }

    Some(authority.split(':').next().unwrap_or(authority))
}

fn is_metadata_host(host: &str) -> bool {
    matches!(
        host,
        "metadata.google.internal" | "metadata" | "169.254.169.254"
    )
}

fn is_metadata_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => *v4 == std::net::Ipv4Addr::new(169, 254, 169, 254),
        std::net::IpAddr::V6(_v6) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{LensConfig, Provider};
    use crate::LensError;

    #[test]
    fn defaults_to_ollama_when_env_is_unset() -> Result<(), Box<dyn std::error::Error>> {
        let env = HashMap::<String, String>::new();
        let config = LensConfig::from_reader(|key| env.get(key).cloned())?;

        assert_eq!(config.provider, Provider::Ollama);
        assert_eq!(config.model, "llama3.2");
        assert_eq!(config.base_url, None);
        assert_eq!(config.max_tokens, 1_024);
        Ok(())
    }

    #[test]
    fn parses_provider_and_model_from_env() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = HashMap::<String, String>::new();
        env.insert("MALCOLM_LENS_PROVIDER".to_owned(), "anthropic".to_owned());
        env.insert(
            "MALCOLM_LENS_MODEL".to_owned(),
            "claude-sonnet-4-20250514".to_owned(),
        );

        let config = LensConfig::from_reader(|key| env.get(key).cloned())?;
        assert_eq!(config.provider, Provider::Anthropic);
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        Ok(())
    }

    #[test]
    fn rejects_metadata_base_url() {
        let mut env = HashMap::<String, String>::new();
        env.insert("MALCOLM_LENS_PROVIDER".to_owned(), "ollama".to_owned());
        env.insert(
            "OLLAMA_BASE_URL".to_owned(),
            "http://169.254.169.254:11434".to_owned(),
        );

        let config = LensConfig::from_reader(|key| env.get(key).cloned());
        assert!(matches!(config, Err(LensError::DisallowedBaseUrl(_))));
    }

    #[test]
    fn allows_remote_ollama_when_explicitly_enabled() -> Result<(), Box<dyn std::error::Error>> {
        let mut env = HashMap::<String, String>::new();
        env.insert("MALCOLM_LENS_PROVIDER".to_owned(), "ollama".to_owned());
        env.insert(
            "OLLAMA_BASE_URL".to_owned(),
            "http://10.0.0.10:11434".to_owned(),
        );
        env.insert(
            "MALCOLM_LENS_ALLOW_REMOTE_OLLAMA".to_owned(),
            "true".to_owned(),
        );

        let config = LensConfig::from_reader(|key| env.get(key).cloned())?;
        assert_eq!(config.base_url.as_deref(), Some("http://10.0.0.10:11434"));
        Ok(())
    }
}
