#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team §17.1/§17.3/§17.6: a keyring that OMITS `$rotate` is **manual**.
//!
//! §17.1: "Omitting `$rotate` disables scheduled rotation and leaves activation
//! manual." §17.3: "manual mode requires the host to bind and activate one
//! before the dependent surface is enabled." §17.6: loading validates the
//! provider's "automatic generation OR external binding mode".
//!
//! A no-`$rotate` ring is therefore driven exactly like an explicit
//! `$mode: "manual"` ring: bootstrap activates nothing, the §17.6 requirement is
//! external binding (never automatic generation), and the dependent surface stays
//! unavailable until an operator [`Keyring::bind_activate`]s a version. The wave
//! bug read a missing `$rotate` as *automatic* (auto-generate + activate at
//! bootstrap), which both (a) enables the dependent auth surface with a key the
//! operator never provisioned, and (b) rejects a bind-only provider at load —
//! exactly the manual-key lifecycle §17 pins.

use std::collections::BTreeSet;

use liasse_host::sim::SimKeyProvider;
use liasse_host::{ExternalKeyRef, KeyCapabilities, KeyOperation, ProtectionClass};
use liasse_runtime::{Keyring, KeyringPolicy};
use liasse_value::{Duration, Precision, Timestamp};

const NOW: i128 = 1_700_000_000_000_000;

fn at(micros: i128) -> Timestamp {
    Timestamp::new(micros, Precision::Micros)
}

fn sign_usage() -> BTreeSet<KeyOperation> {
    [KeyOperation::Sign].into_iter().collect()
}

/// §17.1: a keyring declaration that OMITS `$rotate` — `rotate: None`. Per the
/// spec this is a *manual* activation policy, not an automatic one.
fn no_rotate_policy() -> KeyringPolicy {
    KeyringPolicy {
        algorithm: "Ed25519".to_owned(),
        usage: sign_usage(),
        rotate: None,
        retain: Some(Duration::parse("P45D").expect("duration")),
        protection: None,
    }
}

/// A bind-only provider (external binding, NO automatic generation), carrying one
/// externally created handle — the manual operator key a no-`$rotate` ring is
/// activated from (§17.4). Mirrors the corpus `generate:false, bind:true`
/// provider of `manual-activation-enables-dependent-surface`.
fn bind_only_provider() -> SimKeyProvider {
    let caps = KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .binds()
        .build();
    SimKeyProvider::new(caps).with_external_key("ext1", "Ed25519")
}

/// A provider that both generates and binds, plus an external handle — so the
/// ring LOADS under any reading and the bootstrap behaviour alone is under test.
fn full_provider_with_external() -> SimKeyProvider {
    let caps = KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .generates()
        .binds()
        .disables()
        .build();
    SimKeyProvider::new(caps).with_external_key("ext1", "Ed25519")
}

/// §17.6: a no-`$rotate` ring is manual, so its provider requirement is external
/// binding — NOT automatic generation. A bind-only provider (no generation) must
/// therefore LOAD, exactly as an explicit `$mode: "manual"` ring does.
#[test]
fn no_rotate_ring_requires_binding_not_generation() {
    let ring = Keyring::load("session_keys", bind_only_provider(), no_rotate_policy());
    assert!(
        ring.is_ok(),
        "§17.1/§17.6: a no-$rotate ring is manual — it needs external binding, not \
         automatic generation; a bind-only provider must load: {:?}",
        ring.err()
    );
}

/// §17.6 (contrapositive): a no-`$rotate` (manual) ring over a generate-only
/// provider that CANNOT bind is rejected at load with a binding capability
/// shortfall — the provider cannot perform the external binding the manual policy
/// requires.
#[test]
fn no_rotate_ring_rejects_generate_only_provider() {
    let generate_only = {
        let caps = KeyCapabilities::builder(ProtectionClass::Software)
            .algorithm("Ed25519")
            .operation(KeyOperation::Sign)
            .generates()
            .build();
        SimKeyProvider::new(caps)
    };
    let ring = Keyring::load("session_keys", generate_only, no_rotate_policy());
    assert!(
        ring.is_err(),
        "§17.6: a no-$rotate (manual) ring needs external binding; a generate-only \
         provider must be rejected at load"
    );
}

/// §17.1/§17.3: omitting `$rotate` leaves activation MANUAL — bootstrap activates
/// nothing, so the dependent surface stays unavailable until an operator binds and
/// activates a version through the §17.4 manual transition.
#[test]
fn no_rotate_bootstrap_does_not_auto_activate() {
    let mut ring =
        Keyring::load("session_keys", full_provider_with_external(), no_rotate_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    assert!(
        ring.current().is_none(),
        "§17.1/§17.3: a no-$rotate ring is manual; bootstrap must activate nothing"
    );

    let id = ring
        .bind_activate(&ExternalKeyRef::new("ext1"), at(NOW))
        .expect("operator bind+activate");
    assert_eq!(
        ring.current().map(|v| v.id()),
        Some(id),
        "§17.4: the operator-bound version is now the single active version"
    );
}
