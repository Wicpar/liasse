#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17.7/§17.8 red team: COSE session-token forgery through signature-blind
//! verification.
//!
//! Normative model:
//! - §17.7 "Verification uses the accepted public versions: `cose.verify(...)`.
//!   The namespace result includes the verified key-version identity so
//!   authentication policy can reject revoked or disallowed versions." Verifying
//!   against the accepted PUBLIC versions is a cryptographic signature check
//!   against each accepted version's public key material (§17.2 `$public` is the
//!   "public key values for accepted versions").
//! - §17.8 "The registered namespace controls the pinned token format and
//!   cryptographic encoding. The provider controls the private operation."
//!
//! Together these fix the security invariant that a verifying token can only be
//! produced by the provider's private signing operation for an accepted version;
//! a party holding only PUBLIC information (the ring name and an accepted version
//! ordinal, both observable via `/ring.$current`) must not be able to mint a
//! token that verifies.
//!
//! The implementation violates this. `cose_sign`
//! (`crates/liasse-runtime/src/host.rs:388`) stores the plaintext canonical claim
//! bytes (`claims.signing_bytes()`) as the token's `$sig`, DISCARDING the
//! keyring's real signature; `cose_verify` (`host.rs:455`) then only checks
//! `token.claims().signing_bytes() == token.signature()` — a tautology for any
//! well-formed token — plus version acceptance. No cryptographic signature is
//! ever verified against the version's public key. The identical signature-blind
//! logic is in `liasse-surface` (`crates/liasse-surface/src/cose.rs:99`).
//!
//! Consequence: an off-line attacker forges a token for ARBITRARY claims with no
//! access to the private signer. These tests hand-derive the spec-correct
//! rejection, so the forgery case FAILS against the current implementation while
//! the two controls (foreign ring, non-accepted version) PASS — proving the test
//! reaches a real verify and only the signature check is missing.

use liasse_host::{CoseClaims, CoseToken};
use liasse_ident::InstanceId;
use liasse_runtime::{Engine, FixedGenerators};
use liasse_store::MemoryStore;
use liasse_value::{Precision, Text, Timestamp, Value};

const NOW: i128 = 1_700_000_000_000_000;

fn at(micros: i128) -> Timestamp {
    Timestamp::new(micros, Precision::Micros)
}

/// A package declaring an automatic Ed25519 `session_keys` keyring. On load the
/// engine provisions and bootstraps it, so version ordinal 1 is the active,
/// accepted version (§17.3) — exactly the ordinal an attacker reads off the
/// public `/session_keys.$current` view.
fn keyring_engine() -> Engine<MemoryStore> {
    let def = r#"{
      "$liasse": 1,
      "$app": "t.forge@1.0.0",
      "$model": {
        "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
        "ring_current": { "$view": "/session_keys.$current" }
      }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut generator = FixedGenerators::at(at(NOW));
    Engine::load(store, def, &mut generator).expect("keyring package loads")
}

/// Claims as name/value text pairs.
fn claims(members: &[(&str, &str)]) -> CoseClaims {
    CoseClaims::new(
        members
            .iter()
            .map(|(k, v)| (Text::new(*k), Value::Text(Text::new(*v)))),
    )
}

/// A token value built exactly the way an OFF-LINE attacker would: pick arbitrary
/// claims, name the target ring and an accepted version ordinal, and set `$sig`
/// to the canonical claim bytes — which is precisely what the runtime's own
/// `cose_sign` stores. No private signing operation is ever performed.
fn forged_token(ring: &str, version: u64, claims: CoseClaims) -> Value {
    let sig = claims.signing_bytes();
    CoseToken::new(ring, version, claims, sig).to_value()
}

/// PASSING CONTROL (§17.7 wrong-ring): a token naming a different ring is denied.
/// Confirms the harness reaches a real verify that rejects on the checks it does
/// perform.
#[test]
fn control_foreign_ring_token_is_denied() {
    let engine = keyring_engine();
    let forged = forged_token("other_ring", 1, claims(&[("auth", "session"), ("session", "s1")]));
    assert!(
        engine.cose_verify("session_keys", &forged).is_err(),
        "a token naming a foreign ring must be denied (§17.7)",
    );
}

/// PASSING CONTROL (§17.7 acceptance): a token naming a version the ring never
/// accepted (ordinal 999) is denied. Confirms version acceptance is enforced.
#[test]
fn control_unaccepted_version_token_is_denied() {
    let engine = keyring_engine();
    let forged = forged_token("session_keys", 999, claims(&[("auth", "session"), ("session", "s1")]));
    assert!(
        engine.cose_verify("session_keys", &forged).is_err(),
        "a token naming a non-accepted version must be denied (§17.7)",
    );
}

/// FINDING (§17.7/§17.8): a token forged from PUBLIC information alone — the ring
/// name and the accepted version ordinal, with `$sig` set to the plaintext
/// canonical claim bytes — verifies successfully even though no private signing
/// operation ever ran and the attacker never held the private key. The forged
/// claims impersonate a session the attacker never authenticated as.
///
/// Spec-correct outcome: rejected. §17.7 "verification uses the accepted public
/// versions" (a cryptographic check against the version's public key) and §17.8
/// "the provider controls the private operation" together make a token
/// unforgeable without the private key. This asserts the rejection and therefore
/// FAILS against the signature-blind implementation, which returns the forged
/// claims as a verified proof.
#[test]
fn forged_token_without_private_key_must_not_verify() {
    let engine = keyring_engine();
    let forged = forged_token(
        "session_keys",
        1,
        claims(&[("auth", "session"), ("session", "victim-session-the-attacker-never-held")]),
    );
    let result = engine.cose_verify("session_keys", &forged);
    assert!(
        result.is_err(),
        "a token forged from public metadata alone (no private signing operation) \
         must NOT verify — §17.7 verification uses the accepted public versions; \
         §17.8 the provider controls the private operation. Got Ok proof: {result:?}",
    );
}
