# Contributing to malcolm

Thanks for your interest in the malcolm chaos engineering library. This file
is a short orientation; the full contributor handbook lives in the
[mdBook](docs/book/src/contributing.md) and is the source of truth.

## Quick start

```bash
git clone https://github.com/greysquirr3l/malcom
cd malcom
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

## Workflow

1. Fork the repository and create a topic branch off `main`.
2. Make focused commits with [conventional commit](https://www.conventionalcommits.org/)
   prefixes: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`.
3. Keep changes small enough to review in one sitting and explain the
   user-visible impact in the commit body.
4. Add or update tests with every public API change. New behaviour should be
   covered by both a unit test and a doc test where applicable.
5. Run the full preflight (`cargo test --workspace`, `cargo clippy ... -D warnings`,
   `cargo fmt --all -- --check`, `cargo doc --workspace --no-deps`) before
   pushing.
6. Open a pull request targeting `main` and fill in the PR template.

## Project rules

These are non-negotiable. They are tracked by CI, lint configuration, and the
`AGENTS.md` agent contract.

- **Rust edition 2024, stable toolchain only.** No nightly features. The MSRV
  is declared in each crate's `Cargo.toml` (`rust-version = "1.85"`).
- **No `unwrap()` / `expect()` in library code.** Use `thiserror` for error
  types and propagate with `?`.
- **Every public item must have rustdoc** with at least one example block.
  This is enforced by the workspace `missing_docs` lint.
- **Fault emissions always produce a tracing event** at the appropriate level
  through the `malcolm` target.
- **The core crate (`malcolm-core`) is `#![no_std]`** with `extern crate alloc`.
  It must never depend on `std`, `tokio`, or `tracing`.
- **Seeded RNG only.** Use `rand::SeedableRng` (`SmallRng` is fine). Never
  reach for `thread_rng()` in library code; it would break determinism.
- **Every fault type implements `dry_run()`** that logs what would happen
  without injecting.

## Layout

- `crates/malcolm-core/` — pure math, no I/O, `no_std`.
- `crates/malcolm/` — assembly layer: traits, faults, scenario composition,
  topology, replay, envelope, and the `malcolm!` macro.
- `crates/malcolm-lens/` — optional LLM interpretability layer, feature-gated
  on `ollama` (default) or `anthropic`. Never on the fault injection path.
- `docs/book/` — mdBook source for the public handbook.
- `AGENTS.md` — agent-oriented contract (read by autonomous contributors).

## Reporting issues

Use GitHub Issues. Bug reports should include a minimal reproduction, the
crate versions involved, and the observed vs expected behaviour. Security
issues follow the [SECURITY.md](SECURITY.md) policy — please do not file
them as public issues.

## Code of conduct

Participation in this project is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
