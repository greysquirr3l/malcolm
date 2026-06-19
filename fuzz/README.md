# Fuzz targets

This directory contains `cargo-fuzz` targets for the external-input surfaces
of the malcolm workspace. Targets are run on demand and are **not** part of
the default CI matrix because `cargo-fuzz` requires a nightly toolchain.

## Targets

| Target | Crate | Surface |
|---|---|---|
| `classify` | `malcolm-core` | `bifurcation::classify` over arbitrary `f64` pairs |
| `envelope_from_bytes` | `malcolm` | `ScenarioEnvelope::from_bytes` over arbitrary bytes |
| `response_parser` | `malcolm-lens` | `ResponseParser::parse` over arbitrary `&str` |

## Running locally

```bash
# Install nightly (one-time).
rustup install nightly

# Build all targets.
cargo +nightly fuzz build

# Run a specific target (Ctrl-C to stop, finds corpus automatically).
cargo +nightly fuzz run envelope_from_bytes
```

## CI

These targets are run on a best-effort basis in a separate workflow when
nightly is available. Fuzz findings should be filed as GitHub issues with
the minimized `crash-*` artifact attached.
