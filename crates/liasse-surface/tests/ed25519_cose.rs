#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17.7/§17.8 end-to-end over the real software Ed25519 provider.
//!
//! The de-hardwired [`SurfaceHost::register_keyring`] accepts a
//! `CoseKeyring<Ed25519KeyProvider>` (not just the sim double); a keyring
//! bootstrapped over it holds a genuine Ed25519 active version; `cose.sign` mints
//! a token through that version and `cose.verify` accepts it, while a tampered
//! claim set, a foreign ring, and a revoked version are each denied.

mod support;

use std::collections::BTreeSet;

use liasse_host::{CoseToken, KeyOperation, ProtectionClass};
use liasse_key_ed25519::{Ed25519KeyProvider, ALGORITHM};
use liasse_surface::{
    CoseClaims, CoseKeyring, CoseVerifyError, Keyring, KeyringAdmin, KeyringPolicy, Precision,
    RotationMode, RotationSchedule, SurfaceHost, VerifyErrorOr, VirtualClock,
};
use liasse_value::{Duration, Text, Value};

const NOW: i128 = support::NOW;

/// A session-keys ring backed by the real Ed25519 provider, rotating on a 30-day
/// cadence with a 45-day acceptance window — the §11.3 native-auth shape.
fn ed25519_keyring() -> CoseKeyring<Ed25519KeyProvider> {
    let policy = KeyringPolicy {
        algorithm: ALGORITHM.to_owned(),
        usage: [KeyOperation::Sign].into_iter().collect::<BTreeSet<_>>(),
        rotate: Some(RotationSchedule {
            every: Duration::parse("P30D").expect("cadence"),
            overlap: Duration::parse("P2D").expect("overlap"),
            mode: RotationMode::Automatic,
        }),
        retain: Some(Duration::parse("P45D").expect("retain")),
        protection: Some(ProtectionClass::Software),
    };
    let ring = Keyring::load("session_keys", Ed25519KeyProvider::new(), policy).expect("ring loads");
    CoseKeyring::new(KeyringAdmin::new(ring, VirtualClock::new(NOW, Precision::Micros)))
}

fn session_claims(session: &str) -> CoseClaims {
    CoseClaims::new([
        (Text::new("auth"), Value::Text(Text::new("session"))),
        (Text::new("session"), Value::Text(Text::new(session))),
    ])
}

/// A host built over the real Ed25519 provider by rebuilding the shared test
/// engine with an explicit provider type.
fn ed25519_host() -> SurfaceHost<liasse_store::MemoryStore, Ed25519KeyProvider> {
    let (engine, router, clock) = support::host().into_parts();
    SurfaceHost::new(engine, router, clock)
}

/// §17.8: `register_keyring` accepts the real provider, the ring bootstraps a
/// genuine Ed25519 version, `cose.sign` mints a token through it, and
/// `cose.verify` accepts the token and reports the signing version.
#[test]
fn ed25519_register_sign_verify_end_to_end() {
    let mut host = ed25519_host();
    host.register_keyring("session_keys", ed25519_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap generates an Ed25519 version");

    // The active version carries a raw 32-byte Ed25519 public key (§17.2).
    let current_id = {
        let current =
            host.keyring_admin("session_keys").expect("ring").current().expect("active version");
        let Value::Bytes(public) = current.public_key() else {
            panic!("public key material must be bytes");
        };
        assert_eq!(public.as_slice().len(), 32, "a raw Ed25519 public key is 32 bytes");
        current.id()
    };

    // §17.8 cose.sign through the provider's active version.
    let token = host.keyring_sign("session_keys", session_claims("s1")).expect("sign mints a token");
    assert_eq!(token.ring(), "session_keys");

    // §17.7 cose.verify against the accepted public versions.
    let (claims, version) = host.keyring_verify("session_keys", &token).expect("token verifies");
    assert_eq!(claims.auth(), Some("session"));
    assert_eq!(version, current_id, "verification reports the signing version identity");
}

/// §17.7: a tampered claim set no longer matches the signed payload, and a token
/// naming a foreign ring is denied by identity — both before any acceptance check.
#[test]
fn ed25519_tampered_and_foreign_tokens_are_denied() {
    let mut host = ed25519_host();
    host.register_keyring("session_keys", ed25519_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap");

    let token = host.keyring_sign("session_keys", session_claims("s1")).expect("sign");

    // Re-package the genuine signature under a forged claim set.
    let forged = CoseToken::new(
        "session_keys",
        token.version(),
        session_claims("s2-forged"),
        token.signature().to_vec(),
    );
    assert_eq!(
        host.keyring_verify("session_keys", &forged),
        Err(VerifyErrorOr::Verify(CoseVerifyError::ClaimsTampered)),
    );

    // A token minted for a different ring cannot pass here.
    let foreign = CoseToken::new(
        "other_ring",
        token.version(),
        session_claims("s1"),
        token.signature().to_vec(),
    );
    assert_eq!(
        host.keyring_verify("session_keys", &foreign),
        Err(VerifyErrorOr::Verify(CoseVerifyError::WrongRing)),
    );
}

/// §17.3/§17.7: revoking the signing version denies a previously valid token —
/// verification consults the accepted set, so the same bytes stop authenticating.
#[test]
fn ed25519_revoked_version_denies_a_valid_token() {
    let mut host = ed25519_host();
    host.register_keyring("session_keys", ed25519_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap");

    let token = host.keyring_sign("session_keys", session_claims("s1")).expect("sign");
    assert!(host.keyring_verify("session_keys", &token).is_ok());

    let version = host.keyring_admin("session_keys").expect("ring").current().expect("active").id();
    host.keyring_revoke("session_keys", version).expect("revoke");

    assert_eq!(
        host.keyring_verify("session_keys", &token),
        Err(VerifyErrorOr::Verify(CoseVerifyError::VersionNotAccepted)),
    );
}
