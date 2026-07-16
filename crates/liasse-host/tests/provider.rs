//! Key-provider double behaviour observed through the [`KeyProvider`] contract:
//! generation/sign/lifecycle, the §17.4 public-key validation step on the
//! `invalid_public_key` double, capability checks (§17.6), clean failures and
//! unavailability (§17.9), and the typed budget-exhausting stand-in for a hang.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;

use liasse_host::sim::ProviderOp;
use liasse_host::{
    CapabilityShortfall, ExternalKeyRef, KeyOperation, KeyProvider, KeySpec, ProtectionClass,
    ProviderFailure, ProviderRequirement, PublicKeyError,
};

use common::signing_provider;

fn ed25519_spec() -> KeySpec {
    KeySpec {
        algorithm: "Ed25519".to_owned(),
        operations: BTreeSet::from([KeyOperation::Sign]),
        protection: None,
    }
}

/// A generated key exposes valid public material and can sign; the signature is
/// deterministic in the key and message.
#[test]
fn generate_public_key_and_sign() {
    let mut provider = signing_provider();
    let handle = provider.generate(&ed25519_spec()).expect("generated");

    let public = provider.public_key(&handle).expect("public key");
    public.validate().expect("valid public key");
    assert_eq!(public.algorithm(), "Ed25519");

    let signature = provider.sign(&handle, "Ed25519", b"msg").expect("signed");
    assert!(!signature.is_empty());
}

/// §17.4 step 2: an `invalid_public_key` provider returns a structurally
/// invalid public key that fails validation, so a rotation would reject the
/// replacement and keep the current version (§17.9).
#[test]
fn invalid_public_key_fails_validation() {
    let mut provider = signing_provider();
    provider.set_invalid_public_key([ProviderOp::Generate]);

    let handle = provider.generate(&ed25519_spec()).expect("generated");
    let public = provider.public_key(&handle).expect("public key returned");
    // The call *succeeds* (the component lies with a value) but validation
    // rejects the material.
    assert!(matches!(
        public.validate(),
        Err(PublicKeyError::WrongType(_))
    ));
}

/// A manual bind resolves a registered external key and activates through the
/// same public-key read.
#[test]
fn manual_bind_of_external_key() {
    let mut provider = signing_provider();
    let handle = provider
        .bind(&ExternalKeyRef::new("ext1"), &ed25519_spec())
        .expect("bound");
    provider
        .public_key(&handle)
        .expect("public key")
        .validate()
        .expect("valid");
}

/// A binding of an unregistered external reference is a clean rejection.
#[test]
fn bind_unknown_external_rejects() {
    let mut provider = signing_provider();
    assert!(matches!(
        provider.bind(&ExternalKeyRef::new("nope"), &ed25519_spec()),
        Err(ProviderFailure::UnknownExternal(_))
    ));
}

/// A scripted clean failure on `sign` rejects the operation with no partial
/// effect (§17.9).
#[test]
fn scripted_sign_failure_rejects() {
    let mut provider = signing_provider();
    let handle = provider.generate(&ed25519_spec()).expect("generated");
    provider.set_fail([ProviderOp::Sign]);
    assert!(matches!(
        provider.sign(&handle, "Ed25519", b"msg"),
        Err(ProviderFailure::Failed(_))
    ));
}

/// An unavailable provider fails every operation (§17.9).
#[test]
fn unavailable_provider_fails_every_op() {
    let mut provider = signing_provider();
    let handle = provider.generate(&ed25519_spec()).expect("generated");
    provider.set_available(false);
    assert!(matches!(
        provider.public_key(&handle),
        Err(ProviderFailure::Unavailable)
    ));
    assert!(matches!(
        provider.sign(&handle, "Ed25519", b"msg"),
        Err(ProviderFailure::Unavailable)
    ));
}

/// A hanging operation is the typed budget-exhausting outcome, not a real loop:
/// the double returns `WouldNotReturn` deterministically.
#[test]
fn hanging_op_is_budget_exhausting_value() {
    let mut provider = signing_provider();
    let handle = provider.generate(&ed25519_spec()).expect("generated");
    provider.set_hang([ProviderOp::Sign]);
    assert!(matches!(
        provider.sign(&handle, "Ed25519", b"msg"),
        Err(ProviderFailure::WouldNotReturn)
    ));
}

/// A destroyed key is no longer usable.
#[test]
fn destroyed_key_is_unusable() {
    let mut provider = signing_provider();
    let handle = provider.generate(&ed25519_spec()).expect("generated");
    provider.destroy(&handle).expect("destroyed");
    assert!(matches!(
        provider.public_key(&handle),
        Err(ProviderFailure::UnknownKey(_))
    ));
}

/// §17.6 capability checks: the provider satisfies a matching requirement and
/// reports the specific shortfall for an unsupported algorithm.
#[test]
fn capability_checks() {
    let provider = signing_provider();
    let capabilities = provider.capabilities();

    let ok = ProviderRequirement {
        algorithm: "Ed25519".to_owned(),
        operations: BTreeSet::from([KeyOperation::Sign]),
        automatic: true,
        external_binding: true,
        protection: Some(ProtectionClass::Software),
        needs_disable: true,
        needs_destroy: true,
        needs_attestation: false,
    };
    capabilities.satisfies(&ok).expect("satisfied");

    let wrong_algorithm = ProviderRequirement {
        algorithm: "ES256".to_owned(),
        ..ok.clone()
    };
    assert!(matches!(
        capabilities.satisfies(&wrong_algorithm),
        Err(CapabilityShortfall::Algorithm(_))
    ));

    let needs_attestation = ProviderRequirement {
        needs_attestation: true,
        ..ok.clone()
    };
    assert!(matches!(
        capabilities.satisfies(&needs_attestation),
        Err(CapabilityShortfall::Attestation)
    ));

    // A provider that does not advertise disable cannot satisfy a policy that
    // requires key retirement (§17.6).
    let no_disable = liasse_host::KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .generates()
        .binds()
        .destroys()
        .build();
    assert!(matches!(
        no_disable.satisfies(&ok),
        Err(CapabilityShortfall::Disable)
    ));
}
