//! A software Ed25519 [`KeyProvider`] (SPEC.md §17.5) backed by `ed25519-dalek`.
//!
//! This is the real-crypto counterpart to the scriptable `SimKeyProvider` double:
//! [`Ed25519KeyProvider::generate`] mints an Ed25519 keypair, [`sign`] returns a
//! genuine 64-byte detached signature over the message, and [`public_key`]
//! exposes the raw 32-byte verifying key as application-safe bytes (§17.2). It
//! advertises the §17.6 capability surface a keyring declaring `"$algorithm":
//! "Ed25519"` requires — the Ed25519 algorithm, the sign/verify operation set,
//! automatic generation, key disable/destroy, and a software protection class —
//! so `Keyring::load` accepts it in place of a hardware provider.
//!
//! # Restricted signing (§17.7)
//!
//! The provider never hands out private bytes or an unrestricted key object. A
//! caller signs by naming an opaque [`KeyHandle`]; the private scalar stays
//! behind this boundary. The managed `Keyring` drives exactly one active version
//! at a time and only ever asks this provider to sign under that handle, which is
//! precisely the "restricted signer for the current active version" §17.7
//! mandates.
//!
//! # Key storage (§17.6 "wrap local encrypted storage")
//!
//! Live signing keys are held in process memory for the lifetime of the provider.
//! Durability is a pluggable seam — the [`KeyVault`] trait:
//!
//! - [`Ed25519KeyProvider::new`] uses an [`EphemeralVault`]: keys live only in
//!   memory and are gone at process exit (the dev default).
//! - [`Ed25519KeyProvider::with_vault`] persists every generated seed through a
//!   vault and recovers the vault's keys at construction. The provided
//!   [`EncryptedFileVault`] seals each private seed with XChaCha20-Poly1305 under
//!   a key derived from a constructor master key and writes it to a keystore
//!   directory — never plaintext on disk.
//!
//! # Registration
//!
//! The workspace `KeyProvider` is synchronous and owned by value (see
//! `liasse_host::provider`), so the SPEC's `context.key_provider(name,
//! Arc::new(..))` is expressed as by-value ownership inside a managed ring:
//!
//! ```ignore
//! use liasse_key_ed25519::Ed25519KeyProvider;
//! let ring = Keyring::load("session_keys", Ed25519KeyProvider::new(), policy)?;
//! host.register_keyring("session_keys", CoseKeyring::new(KeyringAdmin::new(ring, clock)));
//! ```

mod vault;

pub use vault::{EncryptedFileVault, EphemeralVault, KeyVault, VaultError};

use std::collections::BTreeMap;

use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use zeroize::Zeroize;

use liasse_host::{
    Attestation, ExternalKeyRef, KeyCapabilities, KeyHandle, KeyOperation, KeyProvider, KeySpec,
    ProtectionClass, ProviderFailure, PublicKey,
};
use liasse_value::{Bytes, Value};

/// The `$algorithm` label this provider generates and signs under (§17.1).
pub const ALGORITHM: &str = "Ed25519";
/// The COSE `EdDSA` label a namespace MAY name the same algorithm by (§17.8).
const ALGORITHM_ALIAS: &str = "EdDSA";

/// One in-memory signing key and whether it has been retired (§17.3 disable).
struct StoredKey {
    signing: SigningKey,
    disabled: bool,
}

/// A software [`KeyProvider`] that signs with in-process Ed25519 keys and
/// persists them through a [`KeyVault`] (§17.5).
pub struct Ed25519KeyProvider {
    keys: BTreeMap<u64, StoredKey>,
    next_id: u64,
    vault: Box<dyn KeyVault>,
}

impl Ed25519KeyProvider {
    /// A provider whose keys live only in process memory (the dev default). Keys
    /// are not persisted and are lost at process exit; use [`with_vault`] for
    /// durability.
    ///
    /// [`with_vault`]: Ed25519KeyProvider::with_vault
    #[must_use]
    pub fn new() -> Self {
        Self { keys: BTreeMap::new(), next_id: 1, vault: Box::new(EphemeralVault) }
    }

    /// A provider whose private seeds are persisted through `vault`, recovering
    /// every key the vault already holds so signing survives a process restart.
    ///
    /// # Errors
    /// [`VaultError`] if the vault cannot be read or a stored entry cannot be
    /// unsealed (e.g. a wrong master key).
    pub fn with_vault(vault: Box<dyn KeyVault>) -> Result<Self, VaultError> {
        let mut keys = BTreeMap::new();
        let mut next_id = 1u64;
        for (id, mut seed) in vault.load()? {
            keys.insert(id, StoredKey { signing: SigningKey::from_bytes(&seed), disabled: false });
            next_id = next_id.max(id.saturating_add(1));
            seed.zeroize();
        }
        Ok(Self { keys, next_id, vault })
    }

    /// The capability set every Ed25519 software provider advertises (§17.6).
    #[must_use]
    fn ed25519_capabilities() -> KeyCapabilities {
        KeyCapabilities::builder(ProtectionClass::Software)
            .algorithm(ALGORITHM)
            .operation(KeyOperation::Sign)
            .operation(KeyOperation::Verify)
            .generates()
            .disables()
            .destroys()
            .build()
    }

    /// The live (non-disabled, non-destroyed) key behind `handle` (§17.3).
    fn live(&self, handle: &KeyHandle) -> Result<&StoredKey, ProviderFailure> {
        match self.keys.get(&handle.get()) {
            Some(stored) if !stored.disabled => Ok(stored),
            _ => Err(ProviderFailure::UnknownKey(handle.get())),
        }
    }
}

impl Default for Ed25519KeyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyProvider for Ed25519KeyProvider {
    fn capabilities(&self) -> KeyCapabilities {
        Self::ed25519_capabilities()
    }

    fn generate(&mut self, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        if spec.algorithm != ALGORITHM && spec.algorithm != ALGORITHM_ALIAS {
            return Err(ProviderFailure::Algorithm(spec.algorithm.clone()));
        }
        let mut seed = [0u8; SECRET_KEY_LENGTH];
        getrandom::fill(&mut seed)
            .map_err(|error| ProviderFailure::Failed(format!("csprng unavailable: {error}")))?;
        let signing = SigningKey::from_bytes(&seed);
        let id = self.next_id;
        // §17.9: persist before the handle is minted, so a keystore write failure
        // commits no key and the caller retries cleanly.
        let persisted = self.vault.store(id, &seed);
        seed.zeroize();
        persisted.map_err(|error| ProviderFailure::Failed(format!("keystore persist failed: {error}")))?;
        self.next_id = self.next_id.saturating_add(1);
        self.keys.insert(id, StoredKey { signing, disabled: false });
        Ok(KeyHandle::new(id))
    }

    fn bind(
        &mut self,
        external: &ExternalKeyRef,
        _spec: &KeySpec,
    ) -> Result<KeyHandle, ProviderFailure> {
        // A software provider owns no external hardware/OS key registry, so it
        // does not advertise `binds` and §17.6 rejects a manual-mode keyring at
        // load. A direct bind therefore resolves no external key (§17.9).
        Err(ProviderFailure::UnknownExternal(external.as_str().to_owned()))
    }

    fn public_key(&self, key: &KeyHandle) -> Result<PublicKey, ProviderFailure> {
        let stored = self.live(key)?;
        let verifying = stored.signing.verifying_key();
        Ok(PublicKey::new(ALGORITHM, Value::Bytes(Bytes::new(verifying.to_bytes().to_vec()))))
    }

    fn sign(
        &self,
        key: &KeyHandle,
        algorithm: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, ProviderFailure> {
        if algorithm != ALGORITHM && algorithm != ALGORITHM_ALIAS {
            return Err(ProviderFailure::Algorithm(algorithm.to_owned()));
        }
        let stored = self.live(key)?;
        Ok(stored.signing.sign(message).to_bytes().to_vec())
    }

    fn disable(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        let stored = self
            .keys
            .get_mut(&key.get())
            .ok_or_else(|| ProviderFailure::UnknownKey(key.get()))?;
        stored.disabled = true;
        Ok(())
    }

    fn destroy(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        if self.keys.remove(&key.get()).is_none() {
            return Err(ProviderFailure::UnknownKey(key.get()));
        }
        self.vault
            .remove(key.get())
            .map_err(|error| ProviderFailure::Failed(format!("keystore remove failed: {error}")))
    }

    fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>, ProviderFailure> {
        // A software key carries no hardware attestation (§17.2); the handle must
        // still name a live key.
        self.live(key).map(|_| None)
    }
}
