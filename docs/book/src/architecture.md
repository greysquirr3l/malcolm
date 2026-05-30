# Architecture

malcolm follows a layered workspace design to keep math, composition, and optional AI analysis separate.

## Layers

## Domain layer: malcolm-core

Responsibilities:

- Distribution primitives and correlated noise generation.
- Bifurcation profiles and regime classification.
- Value objects used by higher layers.

Constraints:

- no_std compatibility.
- No runtime-specific dependencies.

## Assembly layer: malcolm

Responsibilities:

- Fault traits and concrete fault implementations.
- Scenario composition and execution.
- Topology cascade propagation.
- Recording, replay, and envelope handling.

Constraints:

- Runtime integration is optional and feature-gated.
- Fault emissions produce structured tracing events.

## Infrastructure advisory layer: malcolm-lens

Responsibilities:

- Provider-agnostic interfaces for narrative and anomaly analysis.
- Prompt construction and structured response parsing.
- Optional provider adapters.

Constraints:

- Advisory only. Never on the fault injection path.

## Design rules worth preserving

- Keep trait ownership close to the consuming handler.
- Prefer explicit typed boundaries over free-form strings.
- Require deterministic seeds for reproducible behavior.
- Keep each module focused; avoid giant multi-purpose files.
