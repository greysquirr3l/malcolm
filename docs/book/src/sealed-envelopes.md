# Sealed Envelopes

Use sealed envelopes when replay artifacts must be encrypted at rest and intentionally opened.

## Security model

- Payload encryption and authentication via ChaCha20-Poly1305.
- Key derivation with Argon2id.
- Explicit open policy to avoid accidental decryption in automation.

## Example

```rust
use malcolm::replay::envelope::{EnvPassphraseProvider, ScenarioEnvelope};

let provider = EnvPassphraseProvider::new("MALCOLM_ENVELOPE_PASSPHRASE");
let envelope = ScenarioEnvelope::seal(&record, &provider)?;
let bytes = envelope.to_bytes()?;

let decoded = ScenarioEnvelope::from_bytes(&bytes)?;
let opened = decoded.open_interactive(true, &provider)?;
assert_eq!(opened.seed, record.seed);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Operational guidance

- Use non-interactive open only in explicitly approved automation paths.
- Treat passphrase providers as sensitive configuration.
- Keep envelope metadata and policy with release artifacts.
