# malcolm

> "Your scientists were so preoccupied with whether or not they could, they didn't stop to think if they should."
> — Dr. Ian Malcolm

malcolm is a standalone Rust chaos engineering library for fault injection and adversarial simulation. It provides mathematically-grounded primitives for testing distributed systems, async services, and simulation layers under real-world failure conditions — seeded deterministic replay, power-law and Pareto fault distributions, Lyapunov sensitivity scoring, Byzantine fault primitives, and correlated noise generators, all with zero unsafe code and a no_std-compatible core.

## Crates

| Crate | Description |
|-------|-------------|
| `malcolm-core` | Pure math domain layer — no I/O, no_std compatible |
| `malcolm` | Assembly layer — fault traits, fault types, scenario composition |
| `malcolm-lens` | Optional LLM interpretability layer for post-mortem analysis |

## Quick Start

```toml
[dev-dependencies]
malcolm = "0.1"
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
