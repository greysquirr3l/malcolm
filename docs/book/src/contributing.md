# Contributing

## Quality bar

All contributions should be complete, tested, and documented.

## Local commands

```bash
cargo fmt --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Docs commands

Build the book from source under `docs/book` and emit HTML to `./book`:

```bash
mdbook build docs/book
```

Serve locally:

```bash
mdbook serve docs/book -p 3000
```

## Contribution guidance

- Preserve deterministic behavior in tests and examples.
- Avoid broad refactors when making focused fixes.
- Keep public API changes documented with examples.
- Prefer explicit error handling over panics in library code.
