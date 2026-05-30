# Lens Guide

`malcolm-lens` adds optional narrative and anomaly interpretation on top of scenario reports.

## Advisory-only contract

Lens output is informative, not authoritative. It never mutates runtime fault behavior.

## Provider model

`LensProvider` keeps the API provider-agnostic. Current feature paths support:

- `ollama` (default)
- `anthropic` (optional)

## Environment configuration

- `MALCOLM_LENS_PROVIDER`: `ollama` or `anthropic`
- `MALCOLM_LENS_MODEL`: optional model override
- `OLLAMA_BASE_URL`: optional endpoint override
- `MALCOLM_LENS_ALLOW_REMOTE_OLLAMA`: explicit remote override
- `MALCOLM_LENS_MAX_TOKENS`: optional token budget
- `ANTHROPIC_API_KEY`: required for anthropic provider

## Security defaults

- Non-loopback Ollama base URLs are blocked unless explicitly allowed.
- Metadata endpoints are blocked.

## Analyzer usage

`LensAnalyzer` supports:

- Single directive analysis through `analyze`
- Common bundle through `analyze_all`

Each call emits tracing fields for provider, model, directive, duration, and parse status.

## Examples

```bash
cargo run -p malcolm-lens --example lens_postmortem
cargo run -p malcolm-lens --example lens_suggest
cargo run -p malcolm-lens --example lens_divergence
```
