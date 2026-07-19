//! A scriptable key-provider double covering the corpus `provider_set`
//! vocabulary (tests/17-keyrings/NOTES.md, tests/23-host-contract/NOTES.md):
//! clean failures (`fail`), full unavailability (`available: false`),
//! never-returning operations (`hang`, modelled as the typed budget-exhausting
//! [`ProviderFailure::WouldNotReturn`] — never an actual loop), and
//! structurally invalid public keys (`invalid_public_key`, which succeed at the
//! call but yield material that fails the §17.4 validation step).
//!
//! Each key is a REAL, deterministic-seeded Ed25519 keypair (§17.5): `sign`
//! returns a genuine detached signature and `public_key` the matching verifying
//! key, so a token minted through the double verifies under the §17.8 cose codec
//! for the right cryptographic reason — a signature-blind forgery cannot. The
//! seed is derived from the provider-local handle (not process-random): a
//! simulator needs a stable, reproducible keypair per handle, not secrecy.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use liasse_value::{Bytes, Value};

use crate::provider::{
    Attestation, ExternalKeyRef, KeyCapabilities, KeyHandle, KeyProvider, KeySpec, ProviderFailure,
    PublicKey,
};

/// A provider operation the double can be scripted to fail, hang, or corrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderOp {
    /// [`KeyProvider::generate`].
    Generate,
    /// [`KeyProvider::bind`].
    Bind,
    /// [`KeyProvider::public_key`].
    PublicKey,
    /// [`KeyProvider::sign`].
    Sign,
    /// [`KeyProvider::disable`].
    Disable,
    /// [`KeyProvider::destroy`].
    Destroy,
    /// [`KeyProvider::attest`].
    Attest,
}

struct SimKey {
    algorithm: String,
    /// A real Ed25519 signing key so the double's `sign`/`public_key` are a
    /// genuine keypair whose detached signatures verify under the §17.8 cose
    /// codec — a legitimately-signed token verifies for the RIGHT reason, and a
    /// forgery cannot (the double is deterministic-seeded, which is fine for a
    /// simulator: only the process-local keypair identity matters, not secrecy).
    signing: SigningKey,
    invalid_public_key: bool,
    disabled: bool,
}

/// A deterministic Ed25519 signing key for a provider-local handle `id`. The
/// double needs a stable, unique keypair per handle (never process-random), so a
/// re-read public key and a later signature always belong to the same key; any
/// 32-byte seed is a valid Ed25519 secret, and distinct handles seed distinctly.
fn signing_key_for(id: u64) -> SigningKey {
    let mut seed = [0u8; SECRET_KEY_LENGTH];
    seed[..8].copy_from_slice(&id.to_le_bytes());
    SigningKey::from_bytes(&seed)
}

/// A scriptable [`KeyProvider`] double.
pub struct SimKeyProvider {
    capabilities: KeyCapabilities,
    external_keys: BTreeMap<String, String>,
    keys: BTreeMap<u64, SimKey>,
    next_id: u64,
    available: bool,
    fail: BTreeSet<ProviderOp>,
    hang: BTreeSet<ProviderOp>,
    invalid_public_key: BTreeSet<ProviderOp>,
}

impl SimKeyProvider {
    /// Build a double advertising `capabilities`. External keys usable by
    /// manual `bind` are registered with [`SimKeyProvider::with_external_key`].
    #[must_use]
    pub fn new(capabilities: KeyCapabilities) -> Self {
        Self {
            capabilities,
            external_keys: BTreeMap::new(),
            keys: BTreeMap::new(),
            next_id: 1,
            available: true,
            fail: BTreeSet::new(),
            hang: BTreeSet::new(),
            invalid_public_key: BTreeSet::new(),
        }
    }

    /// Register an externally created key handle (a `hosts.*.external_keys`
    /// entry) so manual `bind` can resolve it.
    #[must_use]
    pub fn with_external_key(
        mut self,
        name: impl Into<String>,
        algorithm: impl Into<String>,
    ) -> Self {
        self.external_keys.insert(name.into(), algorithm.into());
        self
    }

    /// Set whether the provider is available; when false every operation fails
    /// [`ProviderFailure::Unavailable`] (§17.9).
    pub fn set_available(&mut self, available: bool) {
        self.available = available;
    }

    /// Script the operations that fail cleanly (`provider_set { fail }`).
    pub fn set_fail(&mut self, ops: impl IntoIterator<Item = ProviderOp>) {
        self.fail = ops.into_iter().collect();
    }

    /// Script the operations that never return (`provider_set { hang }`).
    pub fn set_hang(&mut self, ops: impl IntoIterator<Item = ProviderOp>) {
        self.hang = ops.into_iter().collect();
    }

    /// Script the operations that return a structurally invalid public key
    /// (`provider_set { invalid_public_key }`). The call still succeeds; the
    /// resulting key's `public_key` fails §17.4 validation.
    pub fn set_invalid_public_key(&mut self, ops: impl IntoIterator<Item = ProviderOp>) {
        self.invalid_public_key = ops.into_iter().collect();
    }

    /// The availability/hang/fail gate every operation passes through first.
    fn gate(&self, op: ProviderOp) -> Result<(), ProviderFailure> {
        if !self.available {
            return Err(ProviderFailure::Unavailable);
        }
        if self.hang.contains(&op) {
            return Err(ProviderFailure::WouldNotReturn);
        }
        if self.fail.contains(&op) {
            return Err(ProviderFailure::Failed(format!("injected failure for {op:?}")));
        }
        Ok(())
    }

    fn live_key(&self, key: &KeyHandle) -> Result<&SimKey, ProviderFailure> {
        match self.keys.get(&key.get()) {
            Some(state) if !state.disabled => Ok(state),
            _ => Err(ProviderFailure::UnknownKey(key.get())),
        }
    }
}

impl KeyProvider for SimKeyProvider {
    fn capabilities(&self) -> KeyCapabilities {
        self.capabilities.clone()
    }

    fn generate(&mut self, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        self.gate(ProviderOp::Generate)?;
        let id = self.next_id;
        self.next_id += 1;
        self.keys.insert(
            id,
            SimKey {
                algorithm: spec.algorithm.clone(),
                signing: signing_key_for(id),
                invalid_public_key: self.invalid_public_key.contains(&ProviderOp::Generate),
                disabled: false,
            },
        );
        Ok(KeyHandle::new(id))
    }

    fn bind(
        &mut self,
        external: &ExternalKeyRef,
        _spec: &KeySpec,
    ) -> Result<KeyHandle, ProviderFailure> {
        self.gate(ProviderOp::Bind)?;
        let algorithm = self
            .external_keys
            .get(external.as_str())
            .cloned()
            .ok_or_else(|| ProviderFailure::UnknownExternal(external.as_str().to_owned()))?;
        let id = self.next_id;
        self.next_id += 1;
        self.keys.insert(
            id,
            SimKey {
                algorithm,
                signing: signing_key_for(id),
                invalid_public_key: self.invalid_public_key.contains(&ProviderOp::Bind),
                disabled: false,
            },
        );
        Ok(KeyHandle::new(id))
    }

    fn public_key(&self, key: &KeyHandle) -> Result<PublicKey, ProviderFailure> {
        self.gate(ProviderOp::PublicKey)?;
        let state = self.live_key(key)?;
        if state.invalid_public_key {
            // Structurally invalid material: not bytes, so §17.4 validation
            // rejects the replacement and §17.9 keeps the current version.
            return Ok(PublicKey::new(state.algorithm.clone(), Value::None));
        }
        // §17.2: the real Ed25519 verifying key (32 bytes) — the public half a
        // token signature is later checked against, never private/signature bytes.
        let material = state.signing.verifying_key().to_bytes().to_vec();
        Ok(PublicKey::new(
            state.algorithm.clone(),
            Value::Bytes(Bytes::new(material)),
        ))
    }

    fn sign(
        &self,
        key: &KeyHandle,
        algorithm: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, ProviderFailure> {
        self.gate(ProviderOp::Sign)?;
        let state = self.live_key(key)?;
        if state.algorithm != algorithm {
            return Err(ProviderFailure::Algorithm(algorithm.to_owned()));
        }
        // §17.7/§17.8: a genuine detached signature by the handle's private key,
        // so the token verifies under the accepted version's public key — never a
        // forgeable placeholder derived from public metadata.
        Ok(state.signing.sign(message).to_bytes().to_vec())
    }

    fn disable(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        self.gate(ProviderOp::Disable)?;
        let state = self
            .keys
            .get_mut(&key.get())
            .ok_or_else(|| ProviderFailure::UnknownKey(key.get()))?;
        state.disabled = true;
        Ok(())
    }

    fn destroy(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        self.gate(ProviderOp::Destroy)?;
        self.keys
            .remove(&key.get())
            .map(|_| ())
            .ok_or_else(|| ProviderFailure::UnknownKey(key.get()))
    }

    fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>, ProviderFailure> {
        self.gate(ProviderOp::Attest)?;
        let state = self.live_key(key)?;
        Ok(Some(Attestation::new(Value::Bytes(Bytes::new(
            format!("attest-{}", state.algorithm).into_bytes(),
        )))))
    }
}
