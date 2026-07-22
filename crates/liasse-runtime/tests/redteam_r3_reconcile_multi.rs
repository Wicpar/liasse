#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18.6 red team (round 3): a reconcile pass that observes, simultaneously, a
//! GENUINELY tampered copy (bytes hash wrong) and a TEMPORARILY unavailable copy
//! (delivers no bytes) must demote only the tampered one and repair it from a
//! clean verified holder.
//!
//! §18.6: "A corrupt observation demotes that copy and triggers repair from
//! another verified holder." §18.12: temporary failure "rejects or delays the
//! affected operation while preserving committed application state." The tampered
//! copy's repair reads from a clean holder and writes to the tampered store; the
//! unavailable store is not a participant in that repair.

use liasse_host::sim::SimConnector;
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_runtime::{
    AcceptedType, BlobEngine, CopyState, DeclaredDescriptor, Placement, PlacementPolicy, Store,
    StoreId,
};
use liasse_value::MediaType;

fn connector() -> SimConnector {
    SimConnector::new(ConnectorCapabilities::new([
        Capability::StreamUpload,
        Capability::StreamDownload,
        Capability::Delete,
    ]))
}

fn declared(content: &[u8]) -> DeclaredDescriptor {
    DeclaredDescriptor {
        sha512: BlobIntegrity::digest_hex(content),
        bytes: content.len() as u64,
        media: "text/plain".to_owned(),
        name: None,
    }
}

fn accepted() -> AcceptedType {
    AcceptedType { max_bytes: 10_000, media: vec![MediaType::new("text/plain")] }
}

/// Three stores on distinct connectors, each holding a verified copy.
fn three_store_engine(content: &[u8]) -> (BlobEngine<SimConnector>, liasse_runtime::Blob) {
    let mut engine = BlobEngine::new();
    engine.register("c1", connector());
    engine.register("c2", connector());
    engine.register("c3", connector());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c1".to_owned(), enabled: true });
    engine.add_store(Store { id: StoreId::new("s2"), connector: "c2".to_owned(), enabled: true });
    engine.add_store(Store { id: StoreId::new("s3"), connector: "c3".to_owned(), enabled: true });
    let all: PlacementPolicy =
        Placement::View(vec![StoreId::new("s1"), StoreId::new("s2"), StoreId::new("s3")]).into();
    let blob = engine.upload(&declared(content), &accepted(), &all, content).expect("upload lands 3");
    assert_eq!(blob.stored().len(), 3, "three verified copies to start");
    (engine, blob)
}

/// §18.6: with a `$copies{2}` policy, one tampered + one unavailable copy during
/// the SAME reconcile — the tampered copy is demoted and repaired from the clean
/// holder, while the unavailable copy is left verified (not demoted). This should
/// PASS: the repair does not depend on the unavailable store.
#[test]
fn copies_policy_repairs_tampered_while_a_sibling_is_unavailable() {
    let content = b"durable payload";
    let (mut engine, mut blob) = three_store_engine(content);
    let digest = *blob.descriptor().sha512();

    // s1's physical object is corrupt (reads hash wrong); s2 is a temporary outage.
    engine.connector_mut("c1").expect("c1").corrupt(digest);
    engine.connector_mut("c2").expect("c2").set_available(false);

    let policy: PlacementPolicy = Placement::Copies {
        n: 2,
        of: vec![StoreId::new("s1"), StoreId::new("s2"), StoreId::new("s3")],
    }
    .into();
    engine.reconcile(&mut blob, &policy);

    assert_eq!(
        blob.placement(&StoreId::new("s1")),
        Some(CopyState::Verified),
        "§18.6: the tampered copy is repaired from the clean holder even while a sibling is down",
    );
    assert_ne!(
        blob.placement(&StoreId::new("s2")),
        Some(CopyState::Corrupt),
        "§18.12: a temporary outage must not demote s2 to corrupt",
    );
    assert_eq!(
        blob.placement(&StoreId::new("s3")),
        Some(CopyState::Verified),
        "the clean holder stays verified",
    );
}

/// §18.6 (sharper): under an `$all` policy that lists a store which is temporarily
/// unavailable, a DIFFERENT store's genuinely tampered copy must still be repaired
/// from a clean verified holder. The tampered store is reachable and writable and
/// a clean source exists, so §18.6 "triggers repair from another verified holder"
/// applies independently of the unrelated outage.
#[test]
fn all_policy_repairs_tampered_despite_unrelated_outage() {
    let content = b"durable payload";
    let (mut engine, mut blob) = three_store_engine(content);
    let digest = *blob.descriptor().sha512();

    // s1 tampered (reachable, writable, but reads hash wrong); s2 temporarily down;
    // s3 clean and verified — a perfectly good repair source for s1.
    engine.connector_mut("c1").expect("c1").corrupt(digest);
    engine.connector_mut("c2").expect("c2").set_available(false);

    let policy: PlacementPolicy = Placement::All(vec![
        Placement::View(vec![StoreId::new("s1")]),
        Placement::View(vec![StoreId::new("s2")]),
        Placement::View(vec![StoreId::new("s3")]),
    ])
    .into();
    engine.reconcile(&mut blob, &policy);

    // s1 is corrupt, reachable, writable; s3 holds clean bytes. §18.6 requires the
    // corrupt observation to trigger repair from s3.
    assert_eq!(
        blob.placement(&StoreId::new("s1")),
        Some(CopyState::Verified),
        "§18.6: a corrupt copy with a clean source and a writable destination must be \
         repaired; an unrelated store's outage must not block it (s1 = {:?})",
        blob.placement(&StoreId::new("s1")),
    );
}

/// §18.6 control: the `$all` repair works fine when NO sibling is unavailable —
/// isolating that the ONLY difference in the failing case is the unrelated outage.
#[test]
fn all_policy_repairs_tampered_when_all_reachable() {
    let content = b"durable payload";
    let (mut engine, mut blob) = three_store_engine(content);
    let digest = *blob.descriptor().sha512();

    engine.connector_mut("c1").expect("c1").corrupt(digest);
    // s2 stays reachable this time.

    let policy: PlacementPolicy = Placement::All(vec![
        Placement::View(vec![StoreId::new("s1")]),
        Placement::View(vec![StoreId::new("s2")]),
        Placement::View(vec![StoreId::new("s3")]),
    ])
    .into();
    engine.reconcile(&mut blob, &policy);

    assert_eq!(
        blob.placement(&StoreId::new("s1")),
        Some(CopyState::Verified),
        "control: with every store reachable the tampered copy is repaired",
    );
}
