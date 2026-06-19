# Security Policy

## Supported versions

| Crate           | Version | Supported           |
|-----------------|---------|---------------------|
| `malcolm-core`  | 0.5.x   | yes                 |
| `malcolm`       | 0.5.x   | yes                 |
| `malcolm-lens`  | 0.5.x   | yes                 |
| `< 0.5.0`       | any     | no — please upgrade |

Older releases receive security fixes only at the maintainers' discretion and
only if the fix can be applied without breaking the public API contract.

## Reporting a vulnerability

Please **do not** file public GitHub issues for suspected vulnerabilities.

Report privately via GitHub's [private vulnerability reporting][private-report]
on the repository. Include:

- A short description of the issue and its impact.
- Reproduction steps or a proof-of-concept.
- The crate versions and commit SHA affected.
- Whether you would like public credit for the report.

We aim to acknowledge new reports within five business days and to ship a fix
or mitigation within thirty days for high-severity issues.

## Threat model

malcolm is a developer-time library. It is intentionally designed to inject
faults into a developer's own test, simulation, or staging environment. The
threat model is the boundary between the developer's host and any service the
host can reach.

- **Sealed envelope payloads** are encrypted with `ChaCha20-Poly1305` using a
  key derived from a passphrase via `Argon2id`. Confidentiality and integrity
  depend on the passphrase's entropy and on the secrecy of the per-envelope
  salt and nonce stored alongside the ciphertext.
- **Ollama base URL** is restricted to loopback hosts by default. Remote hosts
  require `MALCOLM_LENS_ALLOW_REMOTE_OLLAMA=true`. Cloud metadata endpoints
  (`169.254.169.254` and similar) are always rejected.
- **Passphrase sources** must never log secret material. Provider `Debug`
  implementations are written to redact secrets and the `malcolm` tracing
  target is the audit trail.

## Audit and supply chain

- `cargo deny check` runs on every CI build against the rules in `deny.toml`.
- `cargo audit` runs on every CI build to catch known RustSec advisories.
- Reproducible builds: `Cargo.lock` is committed and the release workflow
  publishes with `--locked`.

[private-report]: https://github.com/greysquirr3l/malcom/security/advisories/new
