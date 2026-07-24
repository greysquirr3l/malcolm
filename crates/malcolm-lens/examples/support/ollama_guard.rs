//! Shared helpers for the `malcolm-lens` example binaries.

// These helper functions are pulled into example binaries and the
// `lens_examples_contract` integration test via
// `#[path = ...] mod ollama_guard;`. Each of those compilation
// units only needs the helpers at sibling scope, so the strict
// alias's `redundant_pub_crate` / `unreachable_pub` rule pair
// produces noise without a workable alternative visibility.
// We use `#[expect(...)]` per item rather than the crate-level
// `#[allow(...)]` so the suppressed lint is visible if it ever
// stops firing.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

#[expect(
    unreachable_pub,
    reason = "included via #[path = ...] mod ollama_guard into multiple sibling compilation units"
)]
pub fn current_base_url() -> String {
    std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| DEFAULT_OLLAMA_BASE_URL.to_owned())
}

#[expect(
    unreachable_pub,
    reason = "included via #[path = ...] mod ollama_guard into multiple sibling compilation units"
)]
pub fn provider_from_env() -> String {
    std::env::var("MALCOLM_LENS_PROVIDER")
        .unwrap_or_else(|_| "ollama".to_owned())
        .to_ascii_lowercase()
}

#[expect(
    unreachable_pub,
    reason = "included via #[path = ...] mod ollama_guard into multiple sibling compilation units"
)]
pub fn ollama_reachable(base_url: &str, timeout: Duration) -> bool {
    let Some(endpoint) = endpoint_from_base_url(base_url) else {
        return false;
    };

    let Ok(addresses) = endpoint.to_socket_addrs() else {
        return false;
    };

    for address in addresses {
        if TcpStream::connect_timeout(&address, timeout).is_ok() {
            return true;
        }
    }

    false
}

fn endpoint_from_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed);

    let host_port = without_scheme.split('/').next()?;
    if host_port.is_empty() {
        return None;
    }

    if host_port.contains(':') {
        Some(host_port.to_owned())
    } else {
        Some(format!("{host_port}:11434"))
    }
}

#[cfg(test)]
mod tests {
    use super::{endpoint_from_base_url, ollama_reachable};

    #[test]
    fn endpoint_parser_fills_default_port() {
        let endpoint = endpoint_from_base_url("http://localhost");
        assert_eq!(endpoint.as_deref(), Some("localhost:11434"));
    }

    #[test]
    fn endpoint_parser_keeps_explicit_port() {
        let endpoint = endpoint_from_base_url("https://127.0.0.1:31337/api/tags");
        assert_eq!(endpoint.as_deref(), Some("127.0.0.1:31337"));
    }

    #[test]
    fn malformed_base_url_returns_unreachable() {
        assert!(!ollama_reachable("", std::time::Duration::from_millis(20)));
    }
}
