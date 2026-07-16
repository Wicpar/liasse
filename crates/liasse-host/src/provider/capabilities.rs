//! The provider capability surface (§17.6): what protected operations a
//! provider advertises, the protection class it backs keys with, and the
//! typed check of an advertised set against a keyring's declared requirement.

use std::collections::BTreeSet;

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
