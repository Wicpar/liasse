#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17 keyring administration over the surface [`KeyringAdmin`]: automatic
//! bootstrap activates a first version against the bundled clock, a signed token
//! verifies while the version is accepted, and revocation makes the identical
//! token bytes fail verification — the acceptance set is re-evaluated at the
//! clock's instant.

use std::collections::BTreeSet;

use liasse_host::sim::SimKeyProvider;
use liasse_host::{KeyCapabilities, KeyOperation, ProtectionClass};
use liasse_surface::{
    KeyState, Keyring, KeyringAdmin, KeyringPolicy, Precision, RotationMode, RotationSchedule,
    VerifyError, VirtualClock,
};
use liasse_value::Duration;

const NOW: i128 = 1_700_000_000_000_000;

fn provider() -> SimKeyProvider {
    let capabilities = KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .generates()
        .binds()
        .disables()
        .destroys()
        .build();
    SimKeyProvider::new(capabilities)
}

fn auto_policy() -> KeyringPolicy {
    KeyringPolicy {
        algorithm: "Ed25519".to_owned(),
        usage: [KeyOperation::Sign].into_iter().collect::<BTreeSet<_>>(),
        rotate: Some(RotationSchedule {
            every: Duration::parse("P30D").expect("dur"),
            overlap: Duration::parse("P2D").expect("dur"),
            mode: RotationMode::Automatic,
        }),
        retain: Some(Duration::parse("P45D").expect("dur")),
        protection: None,
    }
}

fn admin() -> KeyringAdmin<SimKeyProvider> {
    let ring = Keyring::load("session_keys", provider(), auto_policy()).expect("loads");
    KeyringAdmin::new(ring, VirtualClock::new(NOW, Precision::Micros))
}

/// §17.3: automatic bootstrap activates the first version against the bundled
/// clock instant.
#[test]
fn bootstrap_activates_first_version() {
    let mut admin = admin();
    admin.bootstrap().expect("bootstrap");
    let current = admin.current().expect("an active version");
    assert_eq!(current.state(), KeyState::Active);
    assert_eq!(admin.accepted().len(), 1);
}

/// §17.7: a token signed by the active version verifies while accepted, and the
/// identical bytes fail once the version is revoked — the acceptance set drives
/// the outcome, not the token.
#[test]
fn revocation_rejects_a_previously_valid_token() {
    let mut admin = admin();
    admin.bootstrap().expect("bootstrap");
    let token = admin.sign(b"session-payload").expect("sign");
    assert!(admin.verify(&token).is_ok(), "a fresh token from the active version verifies");

    let version = admin.current().expect("active").id();
    admin.revoke(version).expect("revoke");
    assert_eq!(admin.verify(&token), Err(VerifyError::VersionNotAccepted), "revocation overrides retain");
}
