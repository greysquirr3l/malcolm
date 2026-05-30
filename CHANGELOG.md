# Changelog

All notable changes to this project are documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

## [0.5.1] - 2026-05-30

### Fixed

- Hardened release publishing workflow for crates.io by adding retry logic,
  already-published detection, and index propagation waits between crate
  publishes.
- Reduced false-negative release failures caused by crates.io index lag after
  successful publishes.

### Changed

- Bumped workspace crates to `0.5.1`:
  - `malcolm-core`
  - `malcolm`
  - `malcolm-lens`
- Updated quick-start dependency examples to `malcolm = "0.5.1"`.

## [0.5.0] - 2026-05-30

This release establishes `0.5.0` as the first feature-complete milestone for the malcolm workspace. It is based on the repository commit history through `chore: finalize implementation and cleanup`, plus the release automation and handbook integration shipped in this change set.

### Added

- Cargo workspace scaffold with three crates: `malcolm-core`, `malcolm`, and `malcolm-lens`.
- Math and distribution primitives for chaos simulation including power-law, Pareto, and log-normal families.
- Lyapunov sensitivity scoring for instability amplification analysis.
- Bifurcation profile and threshold modeling for regime shifts.
- Fault-core infrastructure including deterministic handle/registry wiring and dry-run support.
- Network fault primitives for packet loss, latency disruption, and partition-style behaviors.
- Resource fault primitives for memory, CPU, and I/O degradation modeling.
- Clock fault primitives for skew, freeze, and jump-style temporal disturbances.
- Comprehensive mdBook handbook source under `docs/book` with architecture, replay, envelope, lens, observability, and release-operation guides.
- Wiki publication automation that syncs handbook pages from `docs/book` to the repository wiki.

### Changed

- Consolidated the implementation into a stable, workspace-first architecture suitable for CI/release automation.
- Updated all published crate versions to `0.5.0`:
  - `malcolm-core`
  - `malcolm`
  - `malcolm-lens`
- Extended CI with mdBook build validation to prevent doc regressions.
- Added release-chain workflows for OSSF Scorecard, Auto Tag, Release, and crates.io publishing on release.

### Commit Guide (selected)

- `ff46a63` feat(workspace-scaffold): initialize cargo workspace with malcolm crates and CI
- `efb28df` feat(distributions): implement power-law/pareto/log-normal distributions
- `6da2fca` feat(lyapunov-scorer): add sensitivity scoring primitives
- `dbe9758` feat(bifurcation-profiles): add threshold profiles and regime support
- `edf4060` feat(fault-core): add core fault traits, handle, registry, and dry-run scaffolding
- `2e9caa9` feat(network-faults): add network fault modeling
- `38dc8c1` feat(resource-faults): add resource pressure/degradation faults
- `a7ba88d` feat(clock-faults): add clock skew/freeze/jump faults
- `7c517ca` chore: finalize implementation and cleanup
