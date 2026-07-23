use std::fmt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};

use super::ScenarioRecord;

const ENVELOPE_MAGIC: [u8; 4] = *b"MENV";
const ENVELOPE_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// Binary encrypted telemetry envelope for one scenario record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioEnvelope {
    metadata: EnvelopeMetadata,
    ciphertext: Vec<u8>,
    nonce: [u8; NONCE_LEN],
    salt: [u8; SALT_LEN],
}

/// Minimal cleartext metadata attached to an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeMetadata {
    /// Scenario name.
    pub scenario_name: String,
    /// Deterministic scenario seed.
    pub seed: u64,
    /// Event count in encrypted payload.
    pub event_count: usize,
    /// Envelope creation timestamp in milliseconds.
    pub sealed_at_ms: u64,
}

/// Errors produced by [`ScenarioEnvelope`] operations and passphrase providers.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// Envelope magic bytes do not match the `MENV` prefix.
    #[error("invalid envelope magic")]
    InvalidMagic,

    /// Envelope version is not understood by this build.
    #[error("unsupported envelope version: {0}")]
    UnsupportedVersion(u8),

    /// Envelope payload is shorter than the minimum header.
    #[error("envelope payload is truncated")]
    Truncated,

    /// Envelope payload could not be serialized to bytes.
    #[error("failed to serialize envelope payload: {0}")]
    Serialize(String),

    /// Envelope payload could not be parsed from bytes.
    #[error("failed to deserialize envelope payload: {0}")]
    Deserialize(String),

    /// Argon2id key derivation failed (out of memory or invalid parameters).
    #[error("failed to derive encryption key")]
    KeyDerivation,

    /// ChaCha20-Poly1305 encryption failed.
    #[error("envelope encryption failed")]
    Encrypt,

    /// ChaCha20-Poly1305 authentication failed (tamper or wrong key).
    #[error("envelope authentication failed")]
    Decrypt,

    /// Deliberate-open policy denied the operation before decryption.
    #[error("policy denied open operation: {0}")]
    PolicyDenied(&'static str),

    /// Non-interactive open requested without a configured passphrase source.
    #[error("missing passphrase source")]
    MissingPassphraseSource,

    /// Configured passphrase source returned no usable material.
    #[error("passphrase source returned empty material")]
    EmptyPassphrase,

    /// Required environment variable for a passphrase provider was not set.
    #[error("failed to read passphrase from environment variable: {0}")]
    MissingEnvVar(String),

    /// External command for a passphrase provider exited with a non-zero status.
    #[error("passphrase command failed with status: {0}")]
    CommandFailed(String),

    /// Underlying keystore backend returned an error.
    #[error("keystore error: {0}")]
    Keystore(String),

    /// The system entropy source (OS RNG) was unavailable when generating
    /// nonce or salt material for the sealed envelope.
    #[error("os entropy source unavailable")]
    EntropyUnavailable,

    /// Decrypted payload could not be parsed as a [`ScenarioRecord`].
    #[error("failed to decode scenario record: {0}")]
    RecordDecode(String),
}

/// Distinguishes deliberate-open policy modes for [`ScenarioEnvelope::open_interactive`]
/// and [`ScenarioEnvelope::open_non_interactive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Open requested with explicit operator confirmation.
    Interactive,
    /// Open requested in automation, without confirmation.
    NonInteractive,
}

#[derive(Serialize, Deserialize)]
struct EnvelopeBody {
    metadata: EnvelopeMetadata,
    nonce: [u8; NONCE_LEN],
    salt: [u8; SALT_LEN],
    ciphertext: Vec<u8>,
}

/// Source of passphrase material for envelope operations.
pub trait PassphraseProvider: Send + Sync {
    /// Read passphrase bytes without logging secret material.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured passphrase source cannot be read,
    /// returns invalid data, or resolves to an empty passphrase.
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError>;

    /// Stable provider label for telemetry.
    fn label(&self) -> &'static str;
}

/// Passphrase source backed by an environment variable.
pub struct EnvPassphraseProvider {
    env_var: String,
}

impl EnvPassphraseProvider {
    /// Build a provider that reads the passphrase from the named environment variable.
    #[must_use]
    pub fn new(env_var: impl Into<String>) -> Self {
        Self {
            env_var: env_var.into(),
        }
    }
}

impl fmt::Debug for EnvPassphraseProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvPassphraseProvider")
            .field("env_var", &self.env_var)
            .finish()
    }
}

impl PassphraseProvider for EnvPassphraseProvider {
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
        let value = std::env::var(&self.env_var)
            .map_err(|_error| EnvelopeError::MissingEnvVar(self.env_var.clone()))?;
        let bytes = value.into_bytes();
        if bytes.is_empty() {
            return Err(EnvelopeError::EmptyPassphrase);
        }
        Ok(bytes)
    }

    fn label(&self) -> &'static str {
        "env"
    }
}

/// Passphrase source backed by a command invocation.
pub struct CommandPassphraseProvider {
    program: String,
    args: Vec<String>,
}

impl CommandPassphraseProvider {
    /// Build a provider that runs `program` with `args` and reads the trimmed
    /// stdout as the passphrase material.
    #[must_use]
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
        }
    }
}

impl fmt::Debug for CommandPassphraseProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandPassphraseProvider")
            .field("program", &self.program)
            .field("args", &self.args)
            .finish()
    }
}

impl PassphraseProvider for CommandPassphraseProvider {
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
        let output = Command::new(&self.program)
            .args(&self.args)
            .output()
            .map_err(|error| EnvelopeError::CommandFailed(error.to_string()))?;

        if !output.status.success() {
            return Err(EnvelopeError::CommandFailed(output.status.to_string()));
        }

        let value = String::from_utf8_lossy(&output.stdout);
        let trimmed = value.trim().as_bytes().to_vec();
        if trimmed.is_empty() {
            return Err(EnvelopeError::EmptyPassphrase);
        }
        Ok(trimmed)
    }

    fn label(&self) -> &'static str {
        "command"
    }
}

/// Trait for keystore-backed secret retrieval.
pub trait KeystoreSecretProvider: Send + Sync {
    /// Retrieve raw secret bytes by key identifier.
    ///
    /// # Errors
    ///
    /// Returns an error if the keystore backend cannot resolve `key_name` or
    /// cannot return the secret material.
    fn fetch_secret(&self, key_name: &str) -> Result<Vec<u8>, EnvelopeError>;
}

/// Passphrase source backed by a keystore provider.
pub struct KeystorePassphraseProvider<K: KeystoreSecretProvider> {
    key_name: String,
    keystore: K,
}

impl<K: KeystoreSecretProvider> KeystorePassphraseProvider<K> {
    /// Build a provider that asks the supplied keystore for the secret under
    /// `key_name` and uses the returned bytes as passphrase material.
    #[must_use]
    pub fn new(key_name: impl Into<String>, keystore: K) -> Self {
        Self {
            key_name: key_name.into(),
            keystore,
        }
    }
}

impl<K: KeystoreSecretProvider> fmt::Debug for KeystorePassphraseProvider<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeystorePassphraseProvider")
            .field("key_name", &self.key_name)
            .field("keystore_type", &std::any::type_name::<K>())
            .finish_non_exhaustive()
    }
}

impl<K: KeystoreSecretProvider> PassphraseProvider for KeystorePassphraseProvider<K> {
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
        let secret = self.keystore.fetch_secret(&self.key_name)?;
        if secret.is_empty() {
            return Err(EnvelopeError::EmptyPassphrase);
        }
        Ok(secret)
    }

    fn label(&self) -> &'static str {
        "keystore"
    }
}

impl ScenarioEnvelope {
    /// Seal one scenario record into an authenticated encrypted envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization, key derivation, passphrase retrieval,
    /// or encryption fails.
    pub fn seal(
        record: &ScenarioRecord,
        passphrase_provider: &dyn PassphraseProvider,
    ) -> Result<Self, EnvelopeError> {
        let plaintext = record
            .to_bytes()
            .map_err(|error| EnvelopeError::Serialize(error.to_string()))?;

        let metadata = EnvelopeMetadata {
            scenario_name: record.scenario_name.clone(),
            seed: record.seed,
            event_count: record.events.len(),
            sealed_at_ms: now_ms(),
        };

        let mut nonce = [0_u8; NONCE_LEN];
        SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_error| EnvelopeError::EntropyUnavailable)?;

        let mut salt = [0_u8; SALT_LEN];
        SysRng
            .try_fill_bytes(&mut salt)
            .map_err(|_error| EnvelopeError::EntropyUnavailable)?;

        let mut passphrase = passphrase_provider.get_passphrase()?;
        let mut key_bytes = derive_key(&passphrase, &salt)?;

        let key = Key::try_from(&key_bytes[..]).map_err(|_error| EnvelopeError::KeyDerivation)?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce_ref = Nonce::try_from(&nonce[..]).map_err(|_error| EnvelopeError::Encrypt)?;
        let ciphertext = cipher
            .encrypt(&nonce_ref, plaintext.as_ref())
            .map_err(|_error| EnvelopeError::Encrypt)?;

        key_bytes.fill(0);
        passphrase.fill(0);

        tracing::info!(
            target: "malcolm",
            fault_type = "envelope_sealed",
            node_id = "n/a",
            seed = record.seed,
            intensity = 0.0_f64,
            dry_run = false,
            scenario_name = %record.scenario_name,
            provider = passphrase_provider.label(),
            payload_size = plaintext.len(),
            sealed_size = ciphertext.len(),
            "scenario envelope sealed",
        );

        Ok(Self {
            metadata,
            ciphertext,
            nonce,
            salt,
        })
    }

    /// Open an envelope via interactive confirmation policy.
    ///
    /// # Errors
    ///
    /// Returns an error when confirmation is denied, passphrase retrieval fails,
    /// decryption fails, or the decrypted record cannot be decoded.
    pub fn open_interactive(
        &self,
        confirmation_granted: bool,
        passphrase_provider: &dyn PassphraseProvider,
    ) -> Result<ScenarioRecord, EnvelopeError> {
        if !confirmation_granted {
            tracing::warn!(
                target: "malcolm",
                fault_type = "envelope_open_denied",
                node_id = "n/a",
                seed = self.metadata.seed,
                intensity = 0.0_f64,
                dry_run = false,
                mode = "interactive",
                scenario_name = %self.metadata.scenario_name,
                "interactive envelope open denied by confirmation gate",
            );
            return Err(EnvelopeError::PolicyDenied(
                "interactive confirmation was not granted",
            ));
        }

        self.open_with_provider(OpenMode::Interactive, passphrase_provider)
    }

    /// Open an envelope in non-interactive mode.
    ///
    /// # Errors
    ///
    /// Returns an error when no passphrase source is provided, passphrase
    /// retrieval fails, decryption fails, or the decrypted record is invalid.
    pub fn open_non_interactive(
        &self,
        passphrase_provider: Option<&dyn PassphraseProvider>,
    ) -> Result<ScenarioRecord, EnvelopeError> {
        let Some(provider) = passphrase_provider else {
            tracing::warn!(
                target: "malcolm",
                fault_type = "envelope_open_denied",
                node_id = "n/a",
                seed = self.metadata.seed,
                intensity = 0.0_f64,
                dry_run = false,
                mode = "non_interactive",
                scenario_name = %self.metadata.scenario_name,
                "non-interactive envelope open denied without passphrase source",
            );
            return Err(EnvelopeError::MissingPassphraseSource);
        };

        self.open_with_provider(OpenMode::NonInteractive, provider)
    }

    /// Serialize this envelope to a binary payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the envelope body cannot be serialized.
    pub fn to_bytes(&self) -> Result<Vec<u8>, EnvelopeError> {
        let body = EnvelopeBody {
            metadata: self.metadata.clone(),
            nonce: self.nonce,
            salt: self.salt,
            ciphertext: self.ciphertext.clone(),
        };

        let encoded = serde_json::to_vec(&body)
            .map_err(|error| EnvelopeError::Serialize(error.to_string()))?;

        let mut output = Vec::with_capacity(ENVELOPE_MAGIC.len() + 1 + encoded.len());
        output.extend_from_slice(&ENVELOPE_MAGIC);
        output.push(ENVELOPE_VERSION);
        output.extend_from_slice(&encoded);
        Ok(output)
    }

    /// Deserialize one envelope from binary payload.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` are truncated, use an unsupported format, or
    /// cannot be deserialized into an envelope body.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() < ENVELOPE_MAGIC.len() + 1 {
            return Err(EnvelopeError::Truncated);
        }

        let Some(magic) = bytes.get(..ENVELOPE_MAGIC.len()) else {
            return Err(EnvelopeError::Truncated);
        };
        if magic != ENVELOPE_MAGIC {
            return Err(EnvelopeError::InvalidMagic);
        }

        let Some(version) = bytes.get(ENVELOPE_MAGIC.len()).copied() else {
            return Err(EnvelopeError::Truncated);
        };
        if version != ENVELOPE_VERSION {
            return Err(EnvelopeError::UnsupportedVersion(version));
        }

        let Some(body_bytes) = bytes.get((ENVELOPE_MAGIC.len() + 1)..) else {
            return Err(EnvelopeError::Truncated);
        };
        let body: EnvelopeBody = serde_json::from_slice(body_bytes)
            .map_err(|error| EnvelopeError::Deserialize(error.to_string()))?;

        Ok(Self {
            metadata: body.metadata,
            ciphertext: body.ciphertext,
            nonce: body.nonce,
            salt: body.salt,
        })
    }

    /// Read cleartext metadata without opening the encrypted payload.
    #[must_use]
    pub const fn metadata(&self) -> &EnvelopeMetadata {
        &self.metadata
    }

    fn open_with_provider(
        &self,
        mode: OpenMode,
        passphrase_provider: &dyn PassphraseProvider,
    ) -> Result<ScenarioRecord, EnvelopeError> {
        let mut passphrase = passphrase_provider.get_passphrase()?;
        let mut key_bytes = derive_key(&passphrase, &self.salt)?;

        let key = Key::try_from(&key_bytes[..]).map_err(|_error| EnvelopeError::KeyDerivation)?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce_ref =
            Nonce::try_from(&self.nonce[..]).map_err(|_error| EnvelopeError::Decrypt)?;
        let plaintext = cipher
            .decrypt(&nonce_ref, self.ciphertext.as_ref())
            .map_err(|_error| EnvelopeError::Decrypt)?;

        key_bytes.fill(0);
        passphrase.fill(0);

        let record = ScenarioRecord::from_bytes(&plaintext)
            .map_err(|error| EnvelopeError::RecordDecode(error.to_string()))?;

        tracing::info!(
            target: "malcolm",
            fault_type = "envelope_opened",
            node_id = "n/a",
            seed = self.metadata.seed,
            intensity = 0.0_f64,
            dry_run = false,
            mode = ?mode,
            scenario_name = %self.metadata.scenario_name,
            provider = passphrase_provider.label(),
            "scenario envelope opened",
        );

        Ok(record)
    }
}

fn derive_key(passphrase: &[u8], salt: &[u8; SALT_LEN]) -> Result<[u8; KEY_LEN], EnvelopeError> {
    let params =
        Params::new(19_456, 2, 1, Some(KEY_LEN)).map_err(|_error| EnvelopeError::KeyDerivation)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0_u8; KEY_LEN];
    argon2
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|_error| EnvelopeError::KeyDerivation)?;
    Ok(key)
}

fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        EnvelopeError, KeystorePassphraseProvider, KeystoreSecretProvider, PassphraseProvider,
        ScenarioEnvelope,
    };
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::replay::RecordingHarness;
    use crate::scenario::ChaosScenario;
    use malcolm_core::bifurcation::BifurcationProfile;

    struct StaticPassphraseProvider {
        secret: Vec<u8>,
    }

    impl PassphraseProvider for StaticPassphraseProvider {
        fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
            Ok(self.secret.clone())
        }

        fn label(&self) -> &'static str {
            "static"
        }
    }

    #[derive(Clone)]
    struct MockKeystore {
        secret: Vec<u8>,
    }

    impl KeystoreSecretProvider for MockKeystore {
        fn fetch_secret(&self, _key_name: &str) -> Result<Vec<u8>, EnvelopeError> {
            Ok(self.secret.clone())
        }
    }

    fn sample_record() -> crate::replay::ScenarioRecord {
        let scenario = ChaosScenario::builder()
            .name("sealed-envelope")
            .seed(73)
            .add_fault(PacketLoss::builder().seed(9).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = FaultContext {
            seed: 73,
            timestamp_ms: 123,
            node_id: "node-a".to_owned(),
            profile: BifurcationProfile::network_partition(),
        };
        RecordingHarness::new(&scenario).record(&mut ctx)
    }

    #[test]
    fn envelope_round_trip_restores_original_record() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticPassphraseProvider {
            secret: b"correct horse battery staple".to_vec(),
        };

        let source = sample_record();
        let envelope = ScenarioEnvelope::seal(&source, &provider)?;
        let encoded = envelope.to_bytes()?;
        let decoded = ScenarioEnvelope::from_bytes(&encoded)?;

        let opened = decoded.open_interactive(true, &provider)?;
        assert_eq!(opened, source);
        Ok(())
    }

    #[test]
    fn tampered_envelope_fails_authentication() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticPassphraseProvider {
            secret: b"passphrase-123".to_vec(),
        };

        let mut envelope = ScenarioEnvelope::seal(&sample_record(), &provider)?;
        if let Some(last) = envelope.ciphertext.last_mut() {
            *last ^= 0b0000_0001;
        }

        let opened = envelope.open_non_interactive(Some(&provider));
        assert!(matches!(opened, Err(EnvelopeError::Decrypt)));
        Ok(())
    }

    #[test]
    fn non_interactive_open_without_provider_is_denied() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticPassphraseProvider {
            secret: b"passphrase-123".to_vec(),
        };

        let envelope = ScenarioEnvelope::seal(&sample_record(), &provider)?;
        let opened = envelope.open_non_interactive(None);

        assert!(matches!(
            opened,
            Err(EnvelopeError::MissingPassphraseSource)
        ));
        Ok(())
    }

    #[test]
    fn interactive_open_requires_confirmation() -> Result<(), Box<dyn std::error::Error>> {
        let provider = StaticPassphraseProvider {
            secret: b"passphrase-123".to_vec(),
        };

        let envelope = ScenarioEnvelope::seal(&sample_record(), &provider)?;
        let opened = envelope.open_interactive(false, &provider);
        assert!(matches!(opened, Err(EnvelopeError::PolicyDenied(_))));
        Ok(())
    }

    #[test]
    fn keystore_provider_does_not_expose_secret_in_debug() -> Result<(), Box<dyn std::error::Error>>
    {
        let secret = b"keystore-secret-material".to_vec();
        let provider = KeystorePassphraseProvider::new(
            "prod/malcolm/envelope",
            MockKeystore {
                secret: secret.clone(),
            },
        );

        let loaded = provider.get_passphrase()?;
        assert_eq!(loaded, secret);

        let debug = format!("{provider:?}");
        assert!(!debug.contains("keystore-secret-material"));
        Ok(())
    }
}
