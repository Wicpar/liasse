//! The key provider an engine-managed keyring is backed by (§17.5).
//!
//! An engine self-provisions a declared `$keyring` in one of two ways, decided at
//! load ([`build`](crate::Engine::load) / [`build_with_hosts`](crate::Engine::load_with_hosts)):
//!
//! - **injected** — the application registered a real §17.5
//!   [`KeyProvider`](liasse_host::KeyProvider) under the declaration's
//!   `$provider` name (`Registry::register_provider`), so the ring signs with
//!   production keys (an HSM, a KMS, the software `Ed25519KeyProvider`);
//! - **sim** — no provider was registered under that name, so the engine falls
//!   back to its deterministic in-process `SimKeyProvider` double (the corpus/dev
//!   default, unchanged).
//!
//! Both live in the *same* `Vec<Keyring<EngineKeyProvider>>`, so the engine's
//! keyring index stores a heterogeneous mix without genericizing [`Engine`] over a
//! provider parameter. [`KeyProvider`] is object-safe and `Send + Sync`, so the
//! injected arm is a plain `Box<dyn KeyProvider>`; the sim arm stays a concrete
//! `SimKeyProvider` so the §17.9 fault-injection accessor
//! ([`Engine::keyring_provider_mut`](crate::Engine::keyring_provider_mut)) reaches
//! it without a downcast. The type is opaque: its variants are constructed only by
//! the runtime and never matched by an application, so a caller sees only the
//! [`KeyProvider`] contract every ring resolves `cose.sign`/`cose.verify`/rotation
//! against, with no change to their language semantics between the two backings.

use liasse_host::sim::SimKeyProvider;
use liasse_host::{
    Attestation, ExternalKeyRef, KeyCapabilities, KeyHandle, KeyProvider, KeySpec, ProviderFailure,
    PublicKey,
};

/// The key provider backing one engine keyring (§17.5): either the engine's own
/// deterministic sim double or an application-injected real provider. See the
/// [module docs](self).
pub struct EngineKeyProvider(Backing);

/// Which backing an engine keyring resolves its protected operations against.
enum Backing {
    /// The engine's self-provisioned deterministic double (no registered provider).
    Sim(SimKeyProvider),
    /// A real §17.5 provider the application registered under the `$provider` name.
    Injected(Box<dyn KeyProvider>),
}

impl EngineKeyProvider {
    /// Back a ring with the engine's self-provisioned sim double (the default when
    /// no provider is registered under the declaration's `$provider`).
    pub(crate) fn sim(provider: SimKeyProvider) -> Self {
        Self(Backing::Sim(provider))
    }

    /// Back a ring with an application-registered real provider (§17.5).
    pub(crate) fn injected(provider: Box<dyn KeyProvider>) -> Self {
        Self(Backing::Injected(provider))
    }

    /// The backing as a `&dyn` for a read-only protected operation.
    fn as_dyn(&self) -> &dyn KeyProvider {
        match &self.0 {
            Backing::Sim(provider) => provider,
            Backing::Injected(provider) => provider.as_ref(),
        }
    }

    /// The backing as a `&mut dyn` for a keystore-mutating lifecycle operation.
    fn as_dyn_mut(&mut self) -> &mut dyn KeyProvider {
        match &mut self.0 {
            Backing::Sim(provider) => provider,
            Backing::Injected(provider) => provider.as_mut(),
        }
    }

    /// The sim double backing this ring, for the §17.9 `provider_set`
    /// fault-injection vocabulary. `None` for an injected real provider, which
    /// exposes no scriptable fault surface — a real deployment's keys cannot be
    /// scripted to fail on demand.
    pub(crate) fn as_sim_mut(&mut self) -> Option<&mut SimKeyProvider> {
        match &mut self.0 {
            Backing::Sim(provider) => Some(provider),
            Backing::Injected(_) => None,
        }
    }
}

impl KeyProvider for EngineKeyProvider {
    fn capabilities(&self) -> KeyCapabilities {
        self.as_dyn().capabilities()
    }

    fn generate(&mut self, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        self.as_dyn_mut().generate(spec)
    }

    fn bind(&mut self, external: &ExternalKeyRef, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        self.as_dyn_mut().bind(external, spec)
    }

    fn public_key(&self, key: &KeyHandle) -> Result<PublicKey, ProviderFailure> {
        self.as_dyn().public_key(key)
    }

    fn sign(&self, key: &KeyHandle, algorithm: &str, message: &[u8]) -> Result<Vec<u8>, ProviderFailure> {
        self.as_dyn().sign(key, algorithm, message)
    }

    fn disable(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        self.as_dyn_mut().disable(key)
    }

    fn destroy(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        self.as_dyn_mut().destroy(key)
    }

    fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>, ProviderFailure> {
        self.as_dyn().attest(key)
    }
}
