#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17 keyring dynamic semantics over a host [`KeyProvider`] double: the version
//! lifecycle, rotation scheduling on the virtual clock, sealed public-only
//! metadata, acceptance/`$retain` policy, revocation/destruction, and §17.9
//! failure keep-current. Every expectation is re-derived from §17 text, and the
//! sharpest red-case shapes (wrong-key/wrong-ring access, rotation failure,
//! revocation overriding retain) are included.

use std::collections::BTreeSet;

use liasse_host::sim::{ProviderOp, SimKeyProvider};
use liasse_host::{
    ConformanceGuard, GuardError, KeyCapabilities, KeyOperation, ProtectionClass,
};
use liasse_runtime::{
    KeyState, Keyring, KeyringError, KeyringPolicy, RotationMode, RotationOutcome, RotationSchedule,
    VerifyError,
};
use liasse_value::{Duration, Precision, Timestamp, Value};

const NOW: i128 = 1_700_000_000_000_000;
/// P30D in microseconds.
const P30D: i128 = 30 * 24 * 3600 * 1_000_000;
/// P45D in microseconds.
const P45D: i128 = 45 * 24 * 3600 * 1_000_000;

fn at(micros: i128) -> Timestamp {
    Timestamp::new(micros, Precision::Micros)
}

fn dur(text: &str) -> Duration {
    Duration::parse(text).expect("duration parses")
}

fn sign_usage() -> BTreeSet<KeyOperation> {
    [KeyOperation::Sign].into_iter().collect()
}

/// A provider that can generate, disable, destroy, and sign `Ed25519`.
fn full_provider() -> SimKeyProvider {
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

/// An automatic-rotation policy: rotate every 30 days with a 2-day overlap and a
/// 45-day retain window.
fn auto_policy() -> KeyringPolicy {
    KeyringPolicy {
        algorithm: "Ed25519".to_owned(),
        usage: sign_usage(),
        rotate: Some(RotationSchedule {
            every: dur("P30D"),
            overlap: dur("P2D"),
            mode: RotationMode::Automatic,
        }),
        retain: Some(dur("P45D")),
        protection: None,
    }
}

/// §17.3: automatic mode generates and activates the first version at bootstrap,
/// so a dependent surface becomes available.
#[test]
fn automatic_bootstrap_activates_first_version() {
    let mut ring = Keyring::load("session_keys", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let current = ring.current().expect("an active version");
    assert_eq!(current.state(), KeyState::Active);
    assert_eq!(ring.versions().len(), 1);
    assert_eq!(current.activated_at(), Some(at(NOW)));
}

/// §17.3/§17.4: manual mode activates nothing at bootstrap (the dependent
/// surface stays unavailable) until an operator binds and activates a version.
#[test]
fn manual_activation_enables_dependent_surface() {
    let provider = {
        let capabilities = KeyCapabilities::builder(ProtectionClass::Software)
            .algorithm("Ed25519")
            .operation(KeyOperation::Sign)
            .binds()
            .disables()
            .build();
        SimKeyProvider::new(capabilities).with_external_key("ext1", "Ed25519")
    };
    let policy = KeyringPolicy {
        rotate: Some(RotationSchedule {
            every: dur("P30D"),
            overlap: Duration::ZERO,
            mode: RotationMode::Manual,
        }),
        ..auto_policy()
    };
    let mut ring = Keyring::load("session_keys", provider, policy).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    assert!(ring.current().is_none(), "manual bootstrap activates nothing");

    let id = ring
        .bind_activate(&liasse_host::ExternalKeyRef::new("ext1"), at(NOW))
        .expect("bind+activate");
    assert_eq!(ring.current().map(|v| v.id()), Some(id), "the bound version is now active");
}

/// §17.6: an automatic policy over a provider that cannot generate keys fails
/// the capability check at load, before activation.
#[test]
fn automatic_rotation_requires_generation_capability() {
    let capabilities = KeyCapabilities::builder(ProtectionClass::Software)
        .algorithm("Ed25519")
        .operation(KeyOperation::Sign)
        .build();
    match Keyring::load("ring", SimKeyProvider::new(capabilities), auto_policy()) {
        Err(KeyringError::Capability(_)) => {}
        _ => panic!("expected a capability rejection for missing generation"),
    }
}

/// §17.6: a declared hardware protection class is unmet by a software provider.
#[test]
fn protection_class_unmet_rejects_load() {
    let policy = KeyringPolicy { protection: Some(ProtectionClass::Hardware), ..auto_policy() };
    match Keyring::load("ring", full_provider(), policy) {
        Err(KeyringError::Capability(_)) => {}
        _ => panic!("expected a capability rejection for unmet protection"),
    }
}

/// §17.4: a due rotation performed before the next operation activates a new
/// version and retires the prior active one; at most one version is active.
#[test]
fn scheduled_rotation_retires_prior_active() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();

    // One microsecond past the cadence: a rotation is due.
    let outcome = ring.ensure_current(at(NOW + P30D + 1));
    assert!(matches!(outcome, RotationOutcome::Rotated(_)));

    let current = ring.current().expect("v2");
    assert_ne!(current.id(), first, "a new version is active");
    // §17.4: the lazy cutover is placed at the scheduled cadence boundary
    // (activated_at + $every), not the late trigger instant, so the result is
    // identical to a runtime that rotated on schedule.
    assert_eq!(
        current.activated_at(),
        Some(at(NOW + P30D)),
        "the replacement activates at the scheduled boundary, not the late trigger",
    );
    let retired = ring.versions().iter().find(|v| v.id() == first).expect("v1 retained");
    assert_eq!(retired.state(), KeyState::Retired, "the prior version retired");
    assert_eq!(
        retired.retired_at(),
        Some(at(NOW + P30D)),
        "the prior version retires atomically at the same boundary instant",
    );
    assert_eq!(
        ring.versions().iter().filter(|v| v.state() == KeyState::Active).count(),
        1,
        "exactly one active version",
    );
}

/// §17.4 step 3: `$overlap` exposes the next version as `pending` ahead of the
/// atomic cutover, while the current version is still the active one.
#[test]
fn rotation_overlap_exposes_pending_version() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let active = ring.current().expect("v1").id();

    // Inside the 2-day overlap window before the 30-day cutover.
    let overlap = dur("P2D").as_nanos() / 1_000; // P2D in micros
    let outcome = ring.ensure_current(at(NOW + P30D - overlap + 1));
    assert!(matches!(outcome, RotationOutcome::NotDue), "cutover has not arrived");
    assert_eq!(ring.current().map(|v| v.id()), Some(active), "the prior version is still active");
    assert!(
        ring.versions().iter().any(|v| v.state() == KeyState::Pending),
        "a pending version is exposed during overlap",
    );
}

/// §17.9: when a scheduled rotation cannot create a replacement, the current
/// version remains active and the ring is reported overdue.
#[test]
fn rotation_failure_keeps_current_active() {
    // Bootstrap succeeds (v1 generated), then a generate failure is injected so
    // the due rotation cannot create a replacement (§17.9).
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    ring.provider_mut().set_fail([ProviderOp::Generate]);

    let outcome = ring.ensure_current(at(NOW + P30D + 1));
    assert!(matches!(outcome, RotationOutcome::KeptCurrentOverdue));
    assert_eq!(ring.current().map(|v| v.id()), Some(first), "the current version stays active");
    assert!(ring.is_overdue(), "the overdue rotation is reported");
}

/// §17.1: a retired version is accepted for verification within `$retain` and
/// rejected past it.
#[test]
fn retain_window_bounds_retired_acceptance() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    ring.ensure_current(at(NOW + P30D + 1));

    // §17.4: the version retires at the scheduled cadence boundary, so the
    // `$retain` window is measured from that boundary, not the late trigger.
    let retired_at = ring
        .versions()
        .iter()
        .find(|v| v.id() == first)
        .and_then(|v| v.retired_at())
        .expect("v1 retired")
        .count();

    // Just inside the retain window after retirement: still accepted.
    let within = retired_at + P45D - 1;
    assert!(ring.accepted(at(within)).iter().any(|v| v.id() == first), "accepted within retain");
    // Past the retain window: no longer accepted.
    let beyond = retired_at + P45D + 1;
    assert!(!ring.accepted(at(beyond)).iter().any(|v| v.id() == first), "expired past retain");
}

/// §17.1: an omitted `$retain` keeps a retired version accepted until explicit
/// revocation or destruction.
#[test]
fn retain_omitted_accepts_until_revoked() {
    let policy = KeyringPolicy { retain: None, ..auto_policy() };
    let mut ring = Keyring::load("ring", full_provider(), policy).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    ring.ensure_current(at(NOW + P30D + 1));

    let far_future = NOW + 100 * P30D;
    assert!(
        ring.accepted(at(far_future)).iter().any(|v| v.id() == first),
        "retired version accepted indefinitely without a retain window",
    );
    ring.revoke(first, at(far_future)).expect("revoke");
    assert!(
        !ring.accepted(at(far_future)).iter().any(|v| v.id() == first),
        "revocation ends acceptance",
    );
}

/// §17.3 (red): revocation rejects a version immediately, overriding a retain
/// window it would otherwise still be inside.
#[test]
fn revocation_overrides_retain_window() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    let rotate_at = NOW + P30D + 1;
    ring.ensure_current(at(rotate_at));
    let within = rotate_at + 1; // well inside retain
    ring.revoke(first, at(within)).expect("revoke");
    assert!(
        !ring.accepted(at(within)).iter().any(|v| v.id() == first),
        "a revoked version is not accepted even inside its retain window",
    );
}

/// §17.3 (red): a destroyed version is no longer accepted for verification.
#[test]
fn destroyed_version_no_longer_accepted() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    let rotate_at = NOW + P30D + 1;
    ring.ensure_current(at(rotate_at));
    ring.destroy(first, at(rotate_at + 1)).expect("destroy");
    assert!(
        !ring.accepted(at(rotate_at + 1)).iter().any(|v| v.id() == first),
        "a destroyed version is not accepted",
    );
    assert_eq!(
        ring.versions().iter().find(|v| v.id() == first).map(|v| v.state()),
        Some(KeyState::Destroyed),
    );
}

/// §17.7 (red): a token signed by one keyring does not verify against another —
/// a cross-keyring token is denied.
#[test]
fn wrong_keyring_token_denied() {
    let mut ring_a = Keyring::load("ring_a", full_provider(), auto_policy()).expect("loads a");
    ring_a.bootstrap(at(NOW)).expect("bootstrap a");
    let mut ring_b = Keyring::load("ring_b", full_provider(), auto_policy()).expect("loads b");
    ring_b.bootstrap(at(NOW)).expect("bootstrap b");

    let token = ring_a.sign(b"claims", at(NOW)).expect("sign");
    assert_eq!(ring_a.verify(&token, at(NOW)), Ok(token.version()));
    assert_eq!(ring_b.verify(&token, at(NOW)), Err(VerifyError::WrongRing));
}

/// §17.7/§17.8 (red): a token from a retired version keeps verifying across
/// rotations while it stays accepted, but revoking that version denies it — the
/// stolen-token-until-session-revoked shape.
#[test]
fn stolen_token_survives_rotations_until_revoked() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let token = ring.sign(b"claims", at(NOW)).expect("sign");
    let signer = token.version();

    let rotate_at = NOW + P30D + 1;
    ring.ensure_current(at(rotate_at));
    // The signing version is retired but still accepted, so the token verifies.
    assert_eq!(ring.verify(&token, at(rotate_at + 1)), Ok(signer));

    ring.revoke(signer, at(rotate_at + 2)).expect("revoke");
    assert_eq!(
        ring.verify(&token, at(rotate_at + 3)),
        Err(VerifyError::VersionNotAccepted),
        "revoking the session's version denies the stolen token",
    );
}

/// §17.9: an unavailable signing operation rejects the requesting operation and
/// commits no effect — the ring state is unchanged.
#[test]
fn sign_failure_rejects_without_effect() {
    let mut provider = full_provider();
    provider.set_fail([ProviderOp::Sign]);
    let mut ring = Keyring::load("ring", provider, auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let before: Vec<_> = ring.versions().iter().map(|v| (v.id(), v.state())).collect();

    let error = ring.sign(b"claims", at(NOW)).expect_err("sign fails");
    assert!(matches!(error, KeyringError::Provider(_)));
    let after: Vec<_> = ring.versions().iter().map(|v| (v.id(), v.state())).collect();
    assert_eq!(before, after, "a failed signing operation leaves the ring unchanged");
}

/// §17.4 step 2 / §17.9: a provider that returns a structurally invalid public
/// key on rotation fails the read-and-validate step, so the rotation is
/// abandoned and the current version stays active.
#[test]
fn invalid_public_key_keeps_current_active() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let first = ring.current().expect("v1").id();
    // From now on, generated keys carry invalid public material, so the §17.4
    // read-and-validate step fails and the rotation is abandoned.
    ring.provider_mut().set_invalid_public_key([ProviderOp::Generate]);

    let outcome = ring.ensure_current(at(NOW + P30D + 1));
    assert!(matches!(outcome, RotationOutcome::KeptCurrentOverdue));
    assert_eq!(ring.current().map(|v| v.id()), Some(first));
}

/// §17.4 (red): manual binding of an external key whose algorithm disagrees with
/// the policy is rejected at the validate-public-metadata step.
#[test]
fn bind_algorithm_mismatch_rejected() {
    let provider = {
        let capabilities = KeyCapabilities::builder(ProtectionClass::Software)
            .algorithm("Ed25519")
            .operation(KeyOperation::Sign)
            .binds()
            .disables()
            .build();
        // The external handle advertises a different algorithm than the policy.
        SimKeyProvider::new(capabilities).with_external_key("ext1", "ES256")
    };
    let policy = KeyringPolicy {
        rotate: Some(RotationSchedule { every: dur("P30D"), overlap: Duration::ZERO, mode: RotationMode::Manual }),
        ..auto_policy()
    };
    let mut ring = Keyring::load("ring", provider, policy).expect("loads");
    let error = ring
        .bind_activate(&liasse_host::ExternalKeyRef::new("ext1"), at(NOW))
        .expect_err("algorithm mismatch");
    assert!(matches!(error, KeyringError::AlgorithmMismatch { .. }));
    assert!(ring.current().is_none(), "no version was activated");
}

/// §17.2: version metadata carries only public material. The double's public
/// key is the `pk-...` public form; the signing operation's `sig-...` output is
/// transport-only and never appears in version metadata.
#[test]
fn public_metadata_carries_only_public_material() {
    let mut ring = Keyring::load("ring", full_provider(), auto_policy()).expect("loads");
    ring.bootstrap(at(NOW)).expect("bootstrap");
    let public_bytes = match ring.current().expect("v1").public_key() {
        Value::Bytes(bytes) => bytes.as_slice().to_vec(),
        other => panic!("public key is not bytes: {other:?}"),
    };
    assert!(
        public_bytes.starts_with(b"pk-"),
        "version metadata exposes the public key form, not private/signature bytes",
    );
    let token = ring.sign(b"claims", at(NOW)).expect("sign");
    assert_eq!(
        ring.verify(&token, at(NOW)),
        Ok(token.version()),
        "the private signing operation stays behind the provider boundary",
    );
}

/// §16.2/§17.7: a signing host namespace is invoked through the conformance
/// guard, so a component returning an off-contract token type is caught rather
/// than trusted — the guard the runtime wraps provider-backed namespace ops in.
#[test]
fn cose_namespace_off_contract_token_is_caught() {
    use liasse_host::sim::{Behavior, SimNamespace};
    use liasse_host::{EffectClass, InterfaceHash, OpSignature, Version};
    use liasse_value::Type;

    // A cose-like `sign : (int) -> bytes` whose implementation in fact returns a
    // `text` — a nonconforming signer.
    let namespace = SimNamespace::builder(
        liasse_host::ContractName::parse("liasse.cose").expect("name"),
        Version::new(1, 0, 0),
        InterfaceHash::new("ih-cose"),
    )
    .function("sign", OpSignature::new([Type::Int], Type::Bytes), EffectClass::Generated, Behavior::OffType)
    .build();

    let mut guard = ConformanceGuard::new();
    match guard.invoke(&namespace, "sign", &[Value::Int(liasse_value::Integer::from(1))]) {
        Err(GuardError::Violation(_)) => {}
        other => panic!("expected a conformance violation, got {other:?}"),
    }
}

/// §17.2/§17.3: a declared keyring materializes a version view under its name,
/// so `/ring.$current`/`.$accepted`/`.$versions` selectors resolve through the
/// engine at load. The bootstrapped ring exposes exactly one active version with
/// present `activated_at` and no `retired_at`/`revoked_at`.
#[test]
fn keyring_selectors_materialize_bootstrapped_version() {
    use liasse_runtime::{Engine, FixedGenerators};
    use liasse_ident::InstanceId;
    use liasse_store::MemoryStore;

    // No `$requires`: this ring is never signed against (only its selector views
    // are read), so §16.2 (SPEC-ISSUES #17) forbids declaring an unused `cose`.
    let def = r#"{
      "$liasse": 1,
      "$app": "t.keyrings@1.0.0",
      "$model": {
        "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
        "ring_current": { "$view": "/session_keys.$current" },
        "ring_accepted": { "$view": "/session_keys.$accepted" },
        "ring_versions": { "$view": "/session_keys.$versions" }
      }
    }"#;
    let store = MemoryStore::new(InstanceId::new("i1"));
    let mut generator = FixedGenerators::at(at(NOW));
    let mut engine = Engine::load(store, def, &mut generator).expect("load");

    let current = engine.view_at_head("ring_current").expect("view ok").expect("declared");
    assert_eq!(current.len(), 1, "exactly one active version (§17.3)");
    let row = &current.rows()[0];
    assert_eq!(row.field("id"), Some(&Value::Int(liasse_value::Integer::from(1))));
    assert_eq!(row.field("algorithm"), Some(&Value::Text(liasse_value::Text::new("Ed25519"))));
    assert!(row.field("activated_at").is_some(), "an active version has an activation instant");
    assert_eq!(row.field("retired_at"), None, "a bootstrapped active version is not retired");
    assert_eq!(row.field("revoked_at"), None, "a bootstrapped active version is not revoked");

    assert_eq!(engine.view_at_head("ring_accepted").expect("ok").expect("declared").len(), 1);
    assert_eq!(engine.view_at_head("ring_versions").expect("ok").expect("declared").len(), 1);

    // §17.4: past the 30-day cadence a due rotation retires v1 and activates a
    // new version, so the version view grows to two while `.$current` stays one.
    engine.advance(P30D + 1);
    let versions = engine.view_at_head("ring_versions").expect("ok").expect("declared");
    assert_eq!(versions.len(), 2, "a due rotation adds a version (§17.4)");
    let current = engine.view_at_head("ring_current").expect("ok").expect("declared");
    assert_eq!(current.len(), 1, "still exactly one active version after rotation");
    assert_eq!(current.rows()[0].field("id"), Some(&Value::Int(liasse_value::Integer::from(2))));
}
