#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! The software Ed25519 provider produces real signatures a `ed25519-dalek`
//! verifier accepts, advertises the §17.6 capabilities an Ed25519 keyring needs,
//! and retires handles through disable/destroy (§17.3).

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use liasse_host::{
    KeyOperation, KeyProvider, KeySpec, ProtectionClass, ProviderFailure, ProviderRequirement,
};
use liasse_key_ed25519::{Ed25519KeyProvider, ALGORITHM};
use liasse_value::Value;

fn ed25519_spec() -> KeySpec {
    KeySpec {
        algorithm: ALGORITHM.to_owned(),
        operations: [KeyOperation::Sign].into_iter().collect(),
        protection: Some(ProtectionClass::Software),
    }
}

/// §17.5/§17.7: a generated key signs a message and the detached signature
/// verifies under the same key's public material with the real Ed25519 backend —
/// the externally deducible check that this is genuine crypto, not a stand-in.
#[test]
fn generate_sign_verify_raw_ed25519() {
    let mut provider = Ed25519KeyProvider::new();
    let handle = provider.generate(&ed25519_spec()).expect("generate");
    let message = b"auth=session;session=s1";

    let signature = provider.sign(&handle, ALGORITHM, message).expect("sign");
    assert_eq!(signature.len(), 64, "Ed25519 detached signatures are 64 bytes");

    let public = provider.public_key(&handle).expect("public key");
    assert_eq!(public.algorithm(), ALGORITHM);
    let Value::Bytes(public_bytes) = public.material() else {
        panic!("public key material must be bytes (§17.2)");
    };

    let verifying_bytes: [u8; 32] =
        public_bytes.as_slice().try_into().expect("raw Ed25519 public key is 32 bytes");
    let verifying = VerifyingKey::from_bytes(&verifying_bytes).expect("valid verifying key");
    let signature_bytes: [u8; 64] = signature.as_slice().try_into().expect("64-byte signature");
    let signature = Signature::from_bytes(&signature_bytes);

    verifying.verify(message, &signature).expect("the genuine signature verifies");
    assert!(
        verifying.verify(b"tampered payload", &signature).is_err(),
        "a signature must not verify over a different message",
    );
}

/// §17.6: the advertised capabilities satisfy the requirement an automatic
/// Ed25519 signing keyring (`$algorithm: Ed25519`, `$usage: [sign]`, rotating so
/// disable is needed, software protection) places on its provider.
#[test]
fn capabilities_satisfy_ed25519_signing_keyring() {
    let capabilities = Ed25519KeyProvider::new().capabilities();

    let requirement = ProviderRequirement {
        algorithm: ALGORITHM.to_owned(),
        operations: [KeyOperation::Sign].into_iter().collect::<BTreeSet<_>>(),
        automatic: true,
        external_binding: false,
        protection: Some(ProtectionClass::Software),
        needs_disable: true,
        needs_destroy: false,
        needs_attestation: false,
    };
    capabilities.satisfies(&requirement).expect("Ed25519 provider satisfies the signing keyring");

    assert!(capabilities.supports_algorithm(ALGORITHM));
    assert!(!capabilities.supports_algorithm("RSA"));
    assert!(capabilities.supports_operation(KeyOperation::Sign));
    assert_eq!(capabilities.protection(), ProtectionClass::Software);
}

/// §17.6: a manual (external-binding) keyring is rejected — a software provider
/// mints its own keys and advertises no external binding.
#[test]
fn capabilities_reject_manual_binding() {
    let capabilities = Ed25519KeyProvider::new().capabilities();
    let manual = ProviderRequirement {
        algorithm: ALGORITHM.to_owned(),
        operations: [KeyOperation::Sign].into_iter().collect::<BTreeSet<_>>(),
        automatic: false,
        external_binding: true,
        protection: None,
        needs_disable: false,
        needs_destroy: false,
        needs_attestation: false,
    };
    assert!(capabilities.satisfies(&manual).is_err(), "no external binding is advertised");
}

/// §17.3: a disabled key can no longer sign, and destroy removes its material so
/// even public reads fail afterward.
#[test]
fn disable_and_destroy_retire_a_handle() {
    let mut provider = Ed25519KeyProvider::new();
    let handle = provider.generate(&ed25519_spec()).expect("generate");

    provider.disable(&handle).expect("disable");
    assert!(
        matches!(provider.sign(&handle, ALGORITHM, b"x"), Err(ProviderFailure::UnknownKey(_))),
        "a disabled key cannot sign",
    );

    provider.destroy(&handle).expect("destroy removes the disabled key's material");
    assert!(
        matches!(provider.public_key(&handle), Err(ProviderFailure::UnknownKey(_))),
        "a destroyed key has no public material",
    );
    assert!(
        matches!(provider.destroy(&handle), Err(ProviderFailure::UnknownKey(_))),
        "destroying an already-destroyed key is a clean rejection",
    );
}

/// §17.5: a non-Ed25519 spec or sign algorithm is refused so a mismatched keyring
/// never signs under this provider.
#[test]
fn wrong_algorithm_is_refused() {
    let mut provider = Ed25519KeyProvider::new();
    let handle = provider.generate(&ed25519_spec()).expect("generate");

    assert!(matches!(provider.sign(&handle, "RSA", b"x"), Err(ProviderFailure::Algorithm(_))));

    let non_ed25519 = KeySpec {
        algorithm: "RSA".to_owned(),
        operations: BTreeSet::new(),
        protection: None,
    };
    assert!(matches!(provider.generate(&non_ed25519), Err(ProviderFailure::Algorithm(_))));
}

/// §17.8: the `EdDSA` COSE label names the same algorithm, so signing under it
/// succeeds and the signature still verifies.
#[test]
fn eddsa_alias_signs() {
    let mut provider = Ed25519KeyProvider::new();
    let handle = provider.generate(&ed25519_spec()).expect("generate");
    let message = b"aliased";
    let signature = provider.sign(&handle, "EdDSA", message).expect("sign under EdDSA");

    let public = provider.public_key(&handle).expect("public key");
    let Value::Bytes(public_bytes) = public.material() else { panic!("bytes") };
    let verifying_bytes: [u8; 32] = public_bytes.as_slice().try_into().expect("32 bytes");
    let verifying = VerifyingKey::from_bytes(&verifying_bytes).expect("valid key");
    let signature_bytes: [u8; 64] = signature.as_slice().try_into().expect("64 bytes");
    verifying
        .verify(message, &Signature::from_bytes(&signature_bytes))
        .expect("EdDSA-aliased signature verifies");
}
