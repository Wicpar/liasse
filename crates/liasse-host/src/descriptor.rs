//! The load-time namespace descriptor (§16.2): named value types, function
//! names with typed signatures, an effect class per function, and a semantic
//! interface hash. The package install record pins the resolved descriptor.
//!
//! The descriptor's op signatures are ordinary typed values (built from
//! [`liasse_value::Type`]), so the model can compare a call site's argument
//! types against the pinned signature (`namespace-signature-type-mismatch`).

use std::collections::BTreeMap;

use liasse_value::Type;

use crate::version::{ContractName, Version};

/// The §16.3 effect class a namespace function declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectClass {
    /// Same logical inputs produce the same output. MAY run in views, checks,
    /// and replay.
    Pure,
    /// Validates untrusted input against declared keys/config and returns a
    /// typed proof or diagnostic. Runs during external request admission.
    Verifier,
    /// May use randomness, clocks, or provider operations; one successful
    /// result is fixed for the admitted operation. Runs write-time.
    Generated,
}

impl EffectClass {
    /// Whether a function of this class MAY run during a view/check/replay
    /// (§16.3: only pure functions may).
    #[must_use]
    pub const fn runs_in_view(self) -> bool {
        matches!(self, Self::Pure)
    }

    /// The spelling used in `$requires` descriptors and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::Verifier => "verifier",
            Self::Generated => "generated",
        }
    }
}

/// A function's typed signature: positional parameter types and a result type
/// (§16.2 "function names and typed signatures").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSignature {
    params: Vec<Type>,
    result: Type,
}

impl OpSignature {
    /// Build a signature from its parameter and result types.
    #[must_use]
    pub fn new(params: impl IntoIterator<Item = Type>, result: Type) -> Self {
        Self {
            params: params.into_iter().collect(),
            result,
        }
    }

    /// The positional parameter types, in call order.
    #[must_use]
    pub fn params(&self) -> &[Type] {
        &self.params
    }

    /// The result type.
    #[must_use]
    pub fn result(&self) -> &Type {
        &self.result
    }
}

/// A pinned namespace function: its typed signature and effect class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDescriptor {
    signature: OpSignature,
    effect: EffectClass,
}

impl FunctionDescriptor {
    /// Assemble a function descriptor.
    #[must_use]
    pub fn new(signature: OpSignature, effect: EffectClass) -> Self {
        Self { signature, effect }
    }

    /// The typed signature.
    #[must_use]
    pub fn signature(&self) -> &OpSignature {
        &self.signature
    }

    /// The effect class.
    #[must_use]
    pub const fn effect(&self) -> EffectClass {
        self.effect
    }
}

/// A namespace-defined named value type (§16.4): its canonical codec name and
/// whether it is key-eligible (§16.2 "key eligibility for namespace types when
/// provided").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceType {
    codec: String,
    key_eligible: bool,
}

impl NamespaceType {
    /// Declare a namespace type.
    #[must_use]
    pub fn new(codec: impl Into<String>, key_eligible: bool) -> Self {
        Self {
            codec: codec.into(),
            key_eligible,
        }
    }

    /// The canonical codec name.
    #[must_use]
    pub fn codec(&self) -> &str {
        &self.codec
    }

    /// Whether the type MAY serve as a collection key component.
    #[must_use]
    pub const fn is_key_eligible(&self) -> bool {
        self.key_eligible
    }
}

/// An opaque semantic-interface identity (§16.2 "semantic interface hash").
/// Equal strings mean equal interfaces; a different string is a different
/// interface even at the same version — this is what a pinned descriptor
/// checks against on reopen.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceHash(String);

impl InterfaceHash {
    /// Wrap an interface-hash token.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The hash token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The complete load-time descriptor a namespace advertises (§16.2). A
/// registered component supplies one; the package install record pins the
/// resolved value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceDescriptor {
    id: ContractName,
    version: Version,
    interface_hash: InterfaceHash,
    types: BTreeMap<String, NamespaceType>,
    functions: BTreeMap<String, FunctionDescriptor>,
}

impl NamespaceDescriptor {
    /// Assemble a descriptor from its parts.
    #[must_use]
    pub fn new(
        id: ContractName,
        version: Version,
        interface_hash: InterfaceHash,
        types: impl IntoIterator<Item = (String, NamespaceType)>,
        functions: impl IntoIterator<Item = (String, FunctionDescriptor)>,
    ) -> Self {
        Self {
            id,
            version,
            interface_hash,
            types: types.into_iter().collect(),
            functions: functions.into_iter().collect(),
        }
    }

    /// The semantic contract name (§16.2).
    #[must_use]
    pub fn id(&self) -> &ContractName {
        &self.id
    }

    /// The resolved descriptor version.
    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    /// The semantic interface hash.
    #[must_use]
    pub fn interface_hash(&self) -> &InterfaceHash {
        &self.interface_hash
    }

    /// A declared function's pinned descriptor, if the namespace declares it.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&FunctionDescriptor> {
        self.functions.get(name)
    }

    /// The declared functions in name order.
    pub fn functions(&self) -> impl Iterator<Item = (&String, &FunctionDescriptor)> {
        self.functions.iter()
    }

    /// A declared named value type, if any.
    #[must_use]
    pub fn named_type(&self, name: &str) -> Option<&NamespaceType> {
        self.types.get(name)
    }

    /// The declared named value types in name order.
    pub fn named_types(&self) -> impl Iterator<Item = (&String, &NamespaceType)> {
        self.types.iter()
    }
}
