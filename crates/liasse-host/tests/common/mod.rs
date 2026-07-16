//! Shared fixtures for the host-contract tests: builders that assemble the
//! simulated namespace/provider/connector doubles used across suites. Each
//! expectation in the suites is externally deducible from these fixtures.

// Each integration-test binary pulls in this shared module and uses only a
// subset of the fixtures, so the rest read as dead code per binary.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use liasse_host::sim::{Behavior, SimConnector, SimKeyProvider, SimNamespace};
use liasse_host::{
    Capability, ConnectorCapabilities, ContractName, EffectClass, InterfaceHash, KeyCapabilities,
    KeyOperation, OpSignature, ProtectionClass, Version,
};
use liasse_value::{Type, Value};

/// A contract name, panicking on a malformed literal (test-only).
#[must_use]
pub fn name(text: &str) -> ContractName {
    ContractName::parse(text).expect("valid contract name")
}

/// The `test.util` namespace at `version` with a pure `double : (int) -> int`.
#[must_use]
pub fn util_namespace(version: Version, interface_hash: &str) -> SimNamespace {
    SimNamespace::builder(name("test.util"), version, InterfaceHash::new(interface_hash))
        .function(
            "double",
            OpSignature::new([Type::Int], Type::Int),
            EffectClass::Pure,
            Behavior::Double,
        )
        .build()
}

/// A namespace whose `double : (int) -> int` in fact returns a `text`
/// (`off_type`, SPEC-ISSUES item 15).
#[must_use]
pub fn off_type_namespace() -> SimNamespace {
    SimNamespace::builder(name("test.util"), Version::new(1, 0, 0), InterfaceHash::new("ih"))
        .function(
            "double",
            OpSignature::new([Type::Int], Type::Int),
            EffectClass::Pure,
            Behavior::OffType,
        )
        .build()
}

/// A namespace with a declared-`pure` `drift : (int) -> int` that returns a
/// different value each phase (`drifting`, SPEC-ISSUES items 15/16).
#[must_use]
pub fn drifting_namespace() -> SimNamespace {
    SimNamespace::builder(name("test.util"), Version::new(1, 0, 0), InterfaceHash::new("ih"))
        .function(
            "drift",
            OpSignature::new([Type::Int], Type::Int),
            EffectClass::Pure,
            Behavior::Drifting,
        )
        .build()
}

/// A namespace with a `token : () -> text` generated function.
#[must_use]
pub fn token_namespace() -> SimNamespace {
    SimNamespace::builder(name("test.token"), Version::new(1, 0, 0), InterfaceHash::new("ih-token"))
        .function(
            "token",
            OpSignature::new([], Type::Text),
            EffectClass::Generated,
            Behavior::Token,
        )
        .build()
}

/// A namespace with an `accept : (text) -> text` verifier that accepts one
/// credential mapped to a proof.
#[must_use]
pub fn verifier_namespace(credential: &str, proof: &str) -> SimNamespace {
    use liasse_value::Text;
    SimNamespace::builder(name("test.auth"), Version::new(1, 0, 0), InterfaceHash::new("ih-auth"))
        .function(
            "accept",
            OpSignature::new([Type::Text], Type::Text),
            EffectClass::Verifier,
            Behavior::Accept,
        )
        .accepts(credential, Value::Text(Text::new(proof)))
        .build()
}

/// An Ed25519 signing provider that generates, disables, and destroys, with one
/// external key `ext1` for manual bind.
#[must_use]
pub fn signing_provider() -> SimKeyProvider {
    let capabilities = KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .generates()
        .binds()
        .disables()
        .destroys()
        .build();
    SimKeyProvider::new(capabilities).with_external_key("ext1", "Ed25519")
}

/// A filesystem-style connector advertising the full capability set.
#[must_use]
pub fn fs_connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::RangeReads,
        Capability::ServerSideCopy,
        Capability::Checksum,
        Capability::Delete,
        Capability::PhysicalUsage,
    ]))
}
