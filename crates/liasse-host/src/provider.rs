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

use std::collections::BTreeSet;

use liasse_value::Value;

/// A protected operation a key may perform (§17.5). Beyond `sign`/`verify`,
/// providers MAY advertise capability-gated operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyOperation {
    /// Produce a signature.
    Sign,
    /// Verify a signature (public operation).
    Verify,
    /// Decrypt ciphertext.
    Decrypt,
    /// Perform key agreement.
    KeyAgreement,
    /// Wrap another key.
    Wrap,
    /// Produce a message authentication code.
    Mac,
}

/// A provider's declared protection class (§17.1 `$protection`, §17.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtectionClass {
    /// In-process or OS-keystore software protection.
    Software,
    /// Hardware-backed protection (HSM, secure element, PKCS#11 device).
    Hardware,
}

impl ProtectionClass {
    /// Whether this class satisfies a `required` protection class. Hardware
    /// satisfies a software requirement, not vice versa.
    #[must_use]
    pub const fn satisfies(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Hardware, _) | (Self::Software, Self::Software)
        )
    }
}

/// The capability set a provider advertises for the §17.6 load-time checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCapabilities {
    algorithms: BTreeSet<String>,
    operations: BTreeSet<KeyOperation>,
    can_generate: bool,
    can_bind: bool,
    protection: ProtectionClass,
    can_disable: bool,
    can_destroy: bool,
    can_attest: bool,
}

impl KeyCapabilities {
    /// Start building a capability set for a provider of `protection` class.
    /// Every lifecycle flag defaults off and every set starts empty; the
    /// builder turns on only what the provider advertises.
    #[must_use]
    pub fn builder(protection: ProtectionClass) -> KeyCapabilitiesBuilder {
        KeyCapabilitiesBuilder {
            algorithms: BTreeSet::new(),
            operations: BTreeSet::new(),
            can_generate: false,
            can_bind: false,
            protection,
            can_disable: false,
            can_destroy: false,
            can_attest: false,
        }
    }

    /// Whether the provider advertises `algorithm`.
    #[must_use]
    pub fn supports_algorithm(&self, algorithm: &str) -> bool {
        self.algorithms.contains(algorithm)
    }

    /// Whether the provider advertises `operation`.
    #[must_use]
    pub fn supports_operation(&self, operation: KeyOperation) -> bool {
        self.operations.contains(&operation)
    }

    /// The declared protection class.
    #[must_use]
    pub const fn protection(&self) -> ProtectionClass {
        self.protection
    }

    /// Check these capabilities against a keyring's declared requirement
    /// (§17.6). Returns the first shortfall found, or `Ok(())` when the
    /// provider satisfies every declared demand.
    pub fn satisfies(&self, requirement: &ProviderRequirement) -> Result<(), CapabilityShortfall> {
        if !self.supports_algorithm(&requirement.algorithm) {
            return Err(CapabilityShortfall::Algorithm(requirement.algorithm.clone()));
        }
        for operation in &requirement.operations {
            if !self.supports_operation(*operation) {
                return Err(CapabilityShortfall::Operation(*operation));
            }
        }
        if requirement.automatic && !self.can_generate {
            return Err(CapabilityShortfall::Generation);
        }
        if requirement.external_binding && !self.can_bind {
            return Err(CapabilityShortfall::Binding);
        }
        if let Some(protection) = requirement.protection
            && !self.protection.satisfies(protection)
        {
            return Err(CapabilityShortfall::Protection {
                required: protection,
                advertised: self.protection,
            });
        }
        if requirement.needs_disable && !self.can_disable {
            return Err(CapabilityShortfall::Disable);
        }
        if requirement.needs_destroy && !self.can_destroy {
            return Err(CapabilityShortfall::Destroy);
        }
        if requirement.needs_attestation && !self.can_attest {
            return Err(CapabilityShortfall::Attestation);
        }
        Ok(())
    }
}

/// Builds a [`KeyCapabilities`] set, turning on only advertised behaviour.
#[derive(Debug, Clone)]
pub struct KeyCapabilitiesBuilder {
    algorithms: BTreeSet<String>,
    operations: BTreeSet<KeyOperation>,
    can_generate: bool,
    can_bind: bool,
    protection: ProtectionClass,
    can_disable: bool,
    can_destroy: bool,
    can_attest: bool,
}

impl KeyCapabilitiesBuilder {
    /// Advertise support for an algorithm.
    #[must_use]
    pub fn algorithm(mut self, algorithm: impl Into<String>) -> Self {
        self.algorithms.insert(algorithm.into());
        self
    }

    /// Advertise support for a protected operation.
    #[must_use]
    pub fn operation(mut self, operation: KeyOperation) -> Self {
        self.operations.insert(operation);
        self
    }

    /// Advertise automatic key generation.
    #[must_use]
    pub const fn generates(mut self) -> Self {
        self.can_generate = true;
        self
    }

    /// Advertise external key binding.
    #[must_use]
    pub const fn binds(mut self) -> Self {
        self.can_bind = true;
        self
    }

    /// Advertise key disable.
    #[must_use]
    pub const fn disables(mut self) -> Self {
        self.can_disable = true;
        self
    }

    /// Advertise key destruction.
    #[must_use]
    pub const fn destroys(mut self) -> Self {
        self.can_destroy = true;
        self
    }

    /// Advertise attestation.
    #[must_use]
    pub const fn attests(mut self) -> Self {
        self.can_attest = true;
        self
    }

    /// Finish the capability set.
    #[must_use]
    pub fn build(self) -> KeyCapabilities {
        KeyCapabilities {
            algorithms: self.algorithms,
            operations: self.operations,
            can_generate: self.can_generate,
            can_bind: self.can_bind,
            protection: self.protection,
            can_disable: self.can_disable,
            can_destroy: self.can_destroy,
            can_attest: self.can_attest,
        }
    }
}

/// The capability demands a keyring declaration places on its provider (§17.6):
/// algorithm, operation set, generation/binding mode, protection class, and
/// disable/destroy/attestation behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequirement {
    /// The declared `$algorithm`.
    pub algorithm: String,
    /// The `$usage` operation set (or the inferred minimal set).
    pub operations: BTreeSet<KeyOperation>,
    /// Whether the policy needs automatic generation.
    pub automatic: bool,
    /// Whether the policy needs external binding (manual mode).
    pub external_binding: bool,
    /// The `$protection` class, if declared.
    pub protection: Option<ProtectionClass>,
    /// Whether policy requires provider key disable (retirement).
    pub needs_disable: bool,
    /// Whether policy requires provider destruction.
    pub needs_destroy: bool,
    /// Whether policy requires attestation.
    pub needs_attestation: bool,
}

/// A specific way a provider fails a keyring's capability check (§17.6).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityShortfall {
    /// The algorithm is not advertised.
    #[error("provider does not advertise algorithm `{0}`")]
    Algorithm(String),
    /// A required operation is not advertised.
    #[error("provider does not advertise operation `{0:?}`")]
    Operation(KeyOperation),
    /// Automatic generation is required but unsupported.
    #[error("provider does not support automatic key generation")]
    Generation,
    /// External binding is required but unsupported.
    #[error("provider does not support external key binding")]
    Binding,
    /// Key disable is required but unsupported.
    #[error("provider does not support key disable")]
    Disable,
    /// The advertised protection class is weaker than required.
    #[error("provider protection `{advertised:?}` does not satisfy required `{required:?}`")]
    Protection {
        /// The class the keyring demands.
        required: ProtectionClass,
        /// The class the provider advertises.
        advertised: ProtectionClass,
    },
    /// Provider destruction is required but unsupported.
    #[error("provider does not support key destruction")]
    Destroy,
    /// Attestation is required but unsupported.
    #[error("provider does not support attestation")]
    Attestation,
}

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
