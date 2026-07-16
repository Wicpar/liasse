//! The [`KeyProvider`] contract (§17): a host-supplied implementation that owns
//! opaque private-key handles and performs protected operations, while the
//! application defines rotation and acceptance policy.
//!
//! The spec's representative trait is `async` and behind an `Arc`; this
//! workspace is synchronous with one writer per instance and no reference
//! counting, so the mutating lifecycle operations (`generate`, `bind`,
//! `disable`, `destroy`) take `&mut self` and the read operations take `&self`.
//! Private key bytes never cross this boundary — [`KeyHandle`] is opaque and
//! [`PublicKey`] carries only public material as a typed value (§17.2).
//!
//! The advertised capability surface and its §17.6 requirement check live in
//! [`capabilities`].

mod capabilities;

pub use capabilities::{
    CapabilityShortfall, KeyCapabilities, KeyCapabilitiesBuilder, KeyOperation, ProtectionClass,
    ProviderRequirement,
};

use std::collections::BTreeSet;

use liasse_value::Value;

/// The specification of a key to generate or bind (§17.5 `KeySpec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpec {
    /// The algorithm the key must implement.
    pub algorithm: String,
    /// The operations the key must permit.
    pub operations: BTreeSet<KeyOperation>,
    /// The protection class demanded, if any.
    pub protection: Option<ProtectionClass>,
}

/// A reference to an externally created key handle, bound in manual mode
/// (§17.4, §17.5 `ExternalKeyRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalKeyRef(String);

impl ExternalKeyRef {
    /// Wrap an external reference token.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The reference token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque handle to a private key that lives behind the provider (§17.5).
/// The bytes are never application values; the handle only identifies the key
/// to the provider that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyHandle(u64);

impl KeyHandle {
    /// Mint a handle from a provider-local identifier.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The provider-local identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Public key material and its algorithm, exposed as application-safe values
/// (§17.2). The material is a typed [`Value`] — a well-behaved provider returns
/// `Value::Bytes`; a nonconforming one may return anything, which
/// [`PublicKey::validate`] catches at the §17.4 read-and-validate step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    algorithm: String,
    material: Value,
}

impl PublicKey {
    /// Assemble a public key from its algorithm and material value.
    #[must_use]
    pub fn new(algorithm: impl Into<String>, material: Value) -> Self {
        Self {
            algorithm: algorithm.into(),
            material,
        }
    }

    /// The algorithm this key implements.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// The public material as a typed value.
    #[must_use]
    pub fn material(&self) -> &Value {
        &self.material
    }

    /// The §17.4 step-2 validation: "read and validate its public key". Well-
    /// formed public material is a non-empty byte string under a named
    /// algorithm. A structurally invalid or wrong-typed value (the
    /// `invalid_public_key` nonconforming double) is rejected here, so §17.9
    /// keeps the current active version in place.
    pub fn validate(&self) -> Result<(), PublicKeyError> {
        if self.algorithm.is_empty() {
            return Err(PublicKeyError::MissingAlgorithm);
        }
        match &self.material {
            Value::Bytes(bytes) if !bytes.as_slice().is_empty() => Ok(()),
            Value::Bytes(_) => Err(PublicKeyError::EmptyMaterial),
            other => Err(PublicKeyError::WrongType(other.to_canonical_json_string())),
        }
    }
}

/// Why a public key fails the §17.4 validation step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicKeyError {
    /// The key named no algorithm.
    #[error("public key names no algorithm")]
    MissingAlgorithm,
    /// The material was an empty byte string.
    #[error("public key material is empty")]
    EmptyMaterial,
    /// The material was not byte material at all; the canonical wire form of
    /// the offending value is carried for diagnostics.
    #[error("public key material `{0}` is not bytes")]
    WrongType(String),
}

/// Provider-supplied attestation for a key (§17.2, §17.5), carried as an
/// application-safe value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attestation(Value);

impl Attestation {
    /// Wrap attestation material.
    #[must_use]
    pub fn new(material: Value) -> Self {
        Self(material)
    }

    /// The attestation material.
    #[must_use]
    pub fn material(&self) -> &Value {
        &self.0
    }
}

/// A registered key provider (§17.5).
///
/// Object-safe and synchronous. Lifecycle operations that change the provider's
/// keystore take `&mut self` (one writer per instance); reads take `&self`. A
/// failure before admission commits no application effect (§17.9).
pub trait KeyProvider {
    /// The capabilities this provider advertises (§17.6).
    fn capabilities(&self) -> KeyCapabilities;

    /// Generate a fresh key satisfying `spec`, returning its opaque handle.
    fn generate(&mut self, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure>;

    /// Bind an externally created key, returning its opaque handle (§17.4
    /// manual mode).
    fn bind(
        &mut self,
        external: &ExternalKeyRef,
        spec: &KeySpec,
    ) -> Result<KeyHandle, ProviderFailure>;

    /// Read a key's public material and algorithm (§17.4 step 2).
    fn public_key(&self, key: &KeyHandle) -> Result<PublicKey, ProviderFailure>;

    /// Sign `message` under `algorithm` with the key (§17.5, §17.7).
    fn sign(
        &self,
        key: &KeyHandle,
        algorithm: &str,
        message: &[u8],
    ) -> Result<Vec<u8>, ProviderFailure>;

    /// Disable a key (retirement), leaving it unusable for new operations.
    fn disable(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure>;

    /// Destroy a key's provider material (§17.3 destroyed state).
    fn destroy(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure>;

    /// Produce an attestation for a key, if the provider attests.
    fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>, ProviderFailure>;
}

/// A typed provider failure (§17.9). Every variant is a clean rejection that
/// commits no partial provider effect.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderFailure {
    /// The named handle is not a live key of this provider.
    #[error("unknown or destroyed key handle {0}")]
    UnknownKey(u64),
    /// The requested operation is not among the provider's capabilities.
    #[error("provider does not support operation `{0:?}`")]
    Unsupported(KeyOperation),
    /// The algorithm does not match the key or the provider's capabilities.
    #[error("algorithm `{0}` is not available for this key")]
    Algorithm(String),
    /// No external key is registered under this reference (manual bind).
    #[error("no external key registered as `{0}`")]
    UnknownExternal(String),
    /// The provider is unavailable and can perform no operation (§17.9).
    #[error("provider is unavailable")]
    Unavailable,
    /// A clean, deterministic operation failure the double injects (§17.9).
    #[error("provider operation failed: {0}")]
    Failed(String),
    /// A stand-in for a provider operation that never returns.
    ///
    /// A real hang cannot be represented as a value and cannot be tested
    /// without wall-clock timeouts, so a misbehaving/unresponsive component is
    /// modelled as this typed, budget-exhausting outcome: with any finite
    /// mutation-time budget in force the runtime resolves the enclosing
    /// mutation by budget exhaustion (§23.6), never a partial transition
    /// (SPEC-ISSUES item 16). The double returns this instead of looping.
    #[error("provider operation would not return (budget-exhausting)")]
    WouldNotReturn,
}
