# malcolm Handbook

malcolm is a Rust chaos engineering workspace focused on deterministic fault injection and adversarial simulation. It is designed for distributed systems teams that need reproducible failure conditions, realistic jitter patterns, and auditable experiment outputs.

This book gives you one path from first run to production release operations.

## What you will learn

- How the three crates are layered and why that separation matters.
- How to build scenarios with deterministic seeds and realistic failure distributions.
- How to record, replay, and seal scenario outputs.
- How to use the optional lens layer for post-mortem analysis.
- How tracing and CI pipelines support safe operation and release.

## Crates in this workspace

| Crate | Role |
|---|---|
| `malcolm-core` | Pure domain and math primitives, no_std compatible |
| `malcolm` | Assembly layer: faults, scenarios, replay, topology |
| `malcolm-lens` | Optional advisory analysis layer |

## Read this book in order

If you are new, read:

1. Quick Start
2. Architecture
3. Scenario Design
4. Deterministic Replay

Then move to Sealed Envelopes and Lens Guide for advanced operation.
