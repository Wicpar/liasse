#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! The encrypted keystore (§17.6) seals private seeds at rest: a provider
//! reopened over the same directory and master key recovers its keys and signs
//! identically, while a wrong master key cannot unseal the store.

use std::collections::BTreeSet;

use liasse_host::{KeyOperation, KeyProvider, KeySpec, ProtectionClass};
use liasse_key_ed25519::{Ed25519KeyProvider, EncryptedFileVault, ALGORITHM};

fn ed25519_spec() -> KeySpec {
    KeySpec {
        algorithm: ALGORITHM.to_owned(),
        operations: [KeyOperation::Sign].into_iter().collect::<BTreeSet<_>>(),
        protection: Some(ProtectionClass::Software),
    }
}

fn provider_over(master: &[u8], dir: &std::path::Path) -> Ed25519KeyProvider {
    let vault = EncryptedFileVault::open(master, dir).expect("open keystore");
    Ed25519KeyProvider::with_vault(Box::new(vault)).expect("load keystore")
}

/// A key generated through the encrypted vault survives a "restart": a fresh
/// provider over the same directory and master recovers the same key (same public
/// material) and produces a valid signature with it.
#[test]
fn encrypted_keystore_round_trips_across_reopen() {
    let dir = tempfile::tempdir().expect("temp dir");
    let master = b"a-high-entropy-master-key-from-the-kms";

    let handle;
    let public_before;
    {
        let mut provider = provider_over(master, dir.path());
        handle = provider.generate(&ed25519_spec()).expect("generate");
        public_before = provider.public_key(&handle).expect("public key");
    }

    // Reopen: the key is recovered from the sealed keystore, not regenerated.
    let reopened = provider_over(master, dir.path());
    let public_after = reopened.public_key(&handle).expect("recovered public key");
    assert_eq!(public_before, public_after, "the recovered key reproduces the same public key");

    let signature = reopened.sign(&handle, ALGORITHM, b"post-restart").expect("sign with recovered key");
    assert_eq!(signature.len(), 64);
}

/// A wrong master key cannot unseal the keystore: opening a provider over it fails
/// rather than silently producing garbage keys.
#[test]
fn wrong_master_key_cannot_unseal() {
    let dir = tempfile::tempdir().expect("temp dir");
    {
        let mut provider = provider_over(b"correct-master", dir.path());
        provider.generate(&ed25519_spec()).expect("generate");
    }

    let wrong = EncryptedFileVault::open(b"the-wrong-master", dir.path()).expect("open");
    assert!(
        Ed25519KeyProvider::with_vault(Box::new(wrong)).is_err(),
        "a wrong master key must fail to unseal the sealed seeds",
    );
}

/// Destroying a key removes its sealed file, so a reopened provider no longer
/// recovers it (§17.3 destroy).
#[test]
fn destroy_removes_the_sealed_seed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let master = b"master";

    let survivor;
    let destroyed;
    {
        let mut provider = provider_over(master, dir.path());
        survivor = provider.generate(&ed25519_spec()).expect("generate survivor");
        destroyed = provider.generate(&ed25519_spec()).expect("generate destroyed");
        provider.destroy(&destroyed).expect("destroy");
    }

    let reopened = provider_over(master, dir.path());
    assert!(reopened.public_key(&survivor).is_ok(), "the surviving key is recovered");
    assert!(reopened.public_key(&destroyed).is_err(), "the destroyed key is gone from the keystore");
}
