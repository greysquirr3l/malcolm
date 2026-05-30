# Release and Publish

This repository uses a chained workflow model:

1. `CI` validates build, tests, lints, no_std checks, and docs build.
2. `Auto Tag` runs after successful `CI` on `main` and creates `vX.Y.Z` tags for release commits.
3. `Release` resolves the tag/SHA, verifies CI status, publishes crates, and creates a GitHub release.

## crates.io publish order

Crates are published in dependency order:

1. `malcolm-core`
2. `malcolm`
3. `malcolm-lens`

The release workflow is idempotent and skips already-published versions.

## Required release secret

- `CARGO_REGISTRY_TOKEN`

## Why CI verification exists

Tag-triggered workflows can race with CI completion. The release workflow polls the Actions API and proceeds only after CI is completed and successful for the release commit.

## Manual release run

Use `workflow_dispatch` on the Release workflow and pass a `vX.Y.Z` tag.
