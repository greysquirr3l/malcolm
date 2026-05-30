#[path = "../examples/support/ollama_guard.rs"]
mod ollama_guard;

#[test]
fn guard_reports_unreachable_for_unroutable_port() {
    let reachable =
        ollama_guard::ollama_reachable("http://127.0.0.1:1", std::time::Duration::from_millis(25));
    assert!(
        !reachable,
        "expected unreachable guard for closed local port"
    );
}

#[test]
fn guard_parses_provider_name_without_panic() {
    let provider = ollama_guard::provider_from_env();
    assert!(
        !provider.trim().is_empty(),
        "provider string should never be empty"
    );
}

#[test]
fn guard_exposes_default_base_url_shape() {
    let base_url = ollama_guard::current_base_url();
    assert!(
        base_url.starts_with("http://") || base_url.starts_with("https://"),
        "base url should include http scheme"
    );
}
