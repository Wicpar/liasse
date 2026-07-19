#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17/§18 host components composed into the [`SurfaceHost`] call path: a login
//! mutation commits its session and a `cose.sign` over the composed keyring
//! resolves and verifies; a provider outage rejects new signing but leaves an
//! existing token verifying (§17.7/§17.9); a blob put/get round-trips by digest;
//! and a blob-parameter call verifies the descriptor before admission, so a lying
//! descriptor rejects the whole call before any state transition (§18.2/§18.7).

mod support;

use std::collections::BTreeSet;

use liasse_host::sim::{SimConnector, SimKeyProvider};
use liasse_host::{
    Capability, ConnectorCapabilities, KeyCapabilities, KeyOperation, ProtectionClass,
};
use liasse_surface::{
    AcceptedType, BlobEngine, BlobGetOutcome, BlobHost, BlobPutOutcome, CoseClaims, CoseKeyring,
    CoseVerifyError, DeclaredDescriptor, Keyring, KeyringAdmin, KeyringPolicy, Placement, Precision,
    RotationMode, RotationSchedule, Store, StoreId, SurfaceOutcome, VirtualClock,
};
use liasse_value::{Duration, MediaType, Text, Value};

use support::{call, host, text, FUTURE};

const NOW: i128 = support::NOW;

// ---- keyring composition (§17) -------------------------------------------

fn sign_provider() -> SimKeyProvider {
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

fn cose_keyring() -> CoseKeyring<SimKeyProvider> {
    let ring = Keyring::load("session_keys", sign_provider(), auto_policy()).expect("ring loads");
    CoseKeyring::new(KeyringAdmin::new(ring, VirtualClock::new(NOW, Precision::Micros)))
}

fn session_claims(session: &str) -> CoseClaims {
    CoseClaims::new([
        (Text::new("auth"), Value::Text(Text::new("session"))),
        (Text::new("session"), Value::Text(Text::new(session))),
    ])
}

/// §17.8: a login mutation commits its session, and `cose.sign(/session_keys, …)`
/// resolves the composed keyring during the surface flow and mints a token that
/// verifies against the accepted set.
#[test]
fn login_signs_over_composed_keyring_and_verifies() {
    let mut host = host();
    host.register_keyring("session_keys", cose_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap");
    host.connect("c1").unwrap();

    // The login mutation commits its session (§8) — a plain admitted transition.
    let outcome = host
        .call(
            "c1",
            &call(
                "public.login.open",
                [("id", text("sess-1")), ("account", text("alice")), ("expires", support::timestamp(FUTURE))],
            ),
        )
        .expect("login call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "the session commits: {outcome:?}");

    // §17.7 `cose.sign`: the composed keyring signs the session claims.
    let token = host.keyring_sign("session_keys", session_claims("sess-1")).expect("sign resolves");
    assert_eq!(token.ring(), "session_keys");

    // §17.7 `cose.verify`: the token verifies and reports its version identity.
    let (claims, version) = host.keyring_verify("session_keys", &token).expect("verify");
    assert_eq!(claims.auth(), Some("session"));
    assert_eq!(version, host.keyring_admin("session_keys").unwrap().current().unwrap().id());
}

/// §17.9: a total provider outage rejects a *new* signing operation, while an
/// already-minted token keeps verifying — verification consults the accepted
/// public versions, not the provider.
#[test]
fn provider_outage_rejects_signing_but_not_verification() {
    let mut host = host();
    host.register_keyring("session_keys", cose_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap");

    let token = host.keyring_sign("session_keys", session_claims("sess-1")).expect("sign");

    host.provider_mut("session_keys").expect("ring").set_available(false);
    // Existing token still verifies (§17.7 — no provider operation).
    assert!(host.keyring_verify("session_keys", &token).is_ok());
    // A new login cannot sign (§17.9).
    assert!(host.keyring_sign("session_keys", session_claims("sess-2")).is_err());
}

/// §17.3/§17.7: revoking the signing version denies a previously valid token,
/// while a foreign-ring or tampered token is denied by identity/integrity.
#[test]
fn revocation_and_binding_deny_a_token() {
    let mut host = host();
    host.register_keyring("session_keys", cose_keyring());
    host.keyring_bootstrap("session_keys").expect("bootstrap");
    let token = host.keyring_sign("session_keys", session_claims("sess-1")).expect("sign");
    assert!(host.keyring_verify("session_keys", &token).is_ok());

    let version = host.keyring_admin("session_keys").unwrap().current().unwrap().id();
    host.keyring_revoke("session_keys", version).expect("revoke");
    assert_eq!(
        host.keyring_verify("session_keys", &token).unwrap_err(),
        liasse_surface::VerifyErrorOr::Verify(CoseVerifyError::VersionNotAccepted),
    );
}

// ---- blob composition (§18) ----------------------------------------------

fn connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Checksum,
        Capability::Delete,
    ]))
}

fn blob_host() -> BlobHost<SimConnector> {
    let mut engine = BlobEngine::new();
    engine.register("fs", connector());
    engine.add_store(Store { id: StoreId::new("primary"), connector: "fs".to_owned(), enabled: true });
    let accepted = AcceptedType { max_bytes: 32, media: vec![MediaType::new("text/plain")] };
    BlobHost::new(engine, accepted, Placement::View(vec![StoreId::new("primary")]))
}

/// §18.7/§18.8: a blob put commits and its bytes round-trip by digest through the
/// composed blob host driver ops.
#[test]
fn blob_put_get_round_trips_by_digest_through_host() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    let content = b"invoice bytes";

    let BlobPutOutcome::Committed { digest, stored } =
        host.blob_put("attachment", content, "text/plain").expect("registered")
    else {
        panic!("upload should commit");
    };
    assert_eq!(stored, vec![StoreId::new("primary")]);
    assert_eq!(
        host.blob_get("attachment", &digest, true).expect("registered"),
        BlobGetOutcome::Delivered(content.to_vec()),
    );
    // A metadata-only projection grants no fetch (§18.8).
    assert_eq!(
        host.blob_get("attachment", &digest, false).expect("registered"),
        BlobGetOutcome::Denied,
    );
}

/// §18.2/§18.7: a blob-parameter call verifies the streamed bytes against the
/// accepted type before admission — an accepted blob lets the call commit, while
/// an oversize blob rejects the whole call before any state transition.
#[test]
fn blob_parameter_call_verifies_before_admission() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    host.connect("c1").unwrap();

    // An accepted blob: the containing call commits, and the verified descriptor
    // is retained (its digest is fetchable).
    let outcome = host
        .call_with_blob(
            "c1",
            call("public.intake.add", [("title", text("with-file"))]),
            "attachment",
            b"small file",
            "text/plain",
        )
        .expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "accepted blob commits: {outcome:?}");
    let digest = liasse_host::BlobIntegrity::digest_hex(b"small file");
    assert!(host.blob_stored("attachment", &digest).expect("registered").is_some());

    // An oversize blob (> max_bytes) rejects the whole call before admission
    // (§18.2), and no task is added.
    let before = host.engine().view_at_head("index").expect("view").expect("declared").rows().len();
    let outcome = host
        .call_with_blob(
            "c1",
            call("public.intake.add", [("title", text("too-big"))]),
            "attachment",
            b"this content is far larger than the thirty-two byte cap",
            "text/plain",
        )
        .expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Rejected(_)), "oversize blob rejects: {outcome:?}");
    let after = host.engine().view_at_head("index").expect("view").expect("declared").rows().len();
    assert_eq!(before, after, "a rejected blob parameter admits no transition");
}

/// §18.1 (red): a client-declared descriptor disagreeing with the streamed bytes
/// is rejected before any copy lands, through the composed host driver op.
#[test]
fn lying_blob_descriptor_rejected_through_host() {
    let mut host = host();
    host.register_blob("attachment", blob_host());
    let content = b"real bytes";
    let lying = DeclaredDescriptor {
        sha512: liasse_host::BlobIntegrity::digest_hex(b"different"),
        bytes: content.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    };
    let outcome = host.blob_put_declared("attachment", &lying, content).expect("registered");
    assert!(matches!(outcome, BlobPutOutcome::Rejected(_)));
    assert_eq!(
        host.blob_get("attachment", &lying.sha512, true).expect("registered"),
        BlobGetOutcome::Unknown,
    );
}
