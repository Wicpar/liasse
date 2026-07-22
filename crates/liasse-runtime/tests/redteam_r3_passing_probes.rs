#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18/§21 red team (round 3) — probes that are expected to HOLD, isolating the
//! one genuine gap (in `redteam_r3_reconcile_multi`) from adjacent behaviour that
//! is correct. Each asserts an externally-deducible §18/§21 property.

use std::collections::BTreeMap;

use liasse_host::sim::SimConnector;
use liasse_host::{BlobIntegrity, Capability, ConnectorCapabilities};
use liasse_runtime::{
    AcceptedType, BlobEngine, CopyState, DeclaredDescriptor, Erasure, FetchError, Occurrence,
    Placement, PlacementPolicy, Store, StoreId, Value,
};
use liasse_value::{MediaType, Text};

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

/// §18.9 (destination verification after demote): repair to a store whose READ
/// transport tampers must NOT promote that store to verified — the destination is
/// verified only after its bytes hash clean (§18.6 step 3 / §18.9 copy).
#[test]
fn tampering_destination_is_not_promoted_after_demote() {
    let content = b"payload";
    let mut engine = BlobEngine::new();
    engine.register("c1", connector());
    engine.register("c2", connector());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c1".to_owned(), enabled: true });
    engine.add_store(Store { id: StoreId::new("s2"), connector: "c2".to_owned(), enabled: true });

    let policy: PlacementPolicy = Placement::View(vec![StoreId::new("s1"), StoreId::new("s2")]).into();
    let mut blob = engine.upload(&declared(content), &accepted(), &policy, content).expect("upload");
    let digest = *blob.descriptor().sha512();

    // s1 genuinely tampered (demotes); s2 clean source. But s1's read transport is
    // ALSO compromised, so a re-upload+verify to s1 can never hash clean.
    engine.connector_mut("c1").expect("c1").corrupt(digest);
    engine.connector_mut("c1").expect("c1").set_tamper_download(true);

    engine.reconcile(&mut blob, &policy);

    assert_ne!(
        blob.placement(&StoreId::new("s1")),
        Some(CopyState::Verified),
        "§18.9: a destination whose read transport tampers must not be marked verified",
    );
    assert_eq!(
        blob.placement(&StoreId::new("s2")),
        Some(CopyState::Verified),
        "the clean holder stays verified",
    );
}

/// §18.8/§18.12 (fetch during a partial outage): when the first `$serve` holder is
/// temporarily unavailable, the fetch falls through to a clean verified holder and
/// returns exactly the descriptor bytes.
#[test]
fn fetch_falls_through_partial_outage_to_clean_holder() {
    let content = b"served bytes";
    let mut engine = BlobEngine::new();
    engine.register("c1", connector());
    engine.register("c2", connector());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c1".to_owned(), enabled: true });
    engine.add_store(Store { id: StoreId::new("s2"), connector: "c2".to_owned(), enabled: true });

    let policy: PlacementPolicy = Placement::View(vec![StoreId::new("s1"), StoreId::new("s2")]).into();
    let blob = engine.upload(&declared(content), &accepted(), &policy, content).expect("upload");

    // s1 (first in serve order) goes dark; s2 is clean.
    engine.connector_mut("c1").expect("c1").set_available(false);

    assert_eq!(
        engine.fetch(&blob, true).expect("fetch via the reachable holder"),
        content,
        "§18.8: a fetch skips the unavailable holder and returns exact bytes from a clean one",
    );
}

/// §18.8/§18.12 control: if EVERY holder is unavailable (a full outage, no bytes
/// delivered anywhere) the fetch yields no result rather than a wrong one.
#[test]
fn fetch_under_full_outage_yields_no_result() {
    let content = b"served bytes";
    let mut engine = BlobEngine::new();
    engine.register("c1", connector());
    engine.add_store(Store { id: StoreId::new("s1"), connector: "c1".to_owned(), enabled: true });
    let policy: PlacementPolicy = Placement::View(vec![StoreId::new("s1")]).into();
    let blob = engine.upload(&declared(content), &accepted(), &policy, content).expect("upload");

    engine.connector_mut("c1").expect("c1").set_available(false);
    assert_eq!(engine.fetch(&blob, true), Err(FetchError::NoCleanHolder));
}

// ---- erasure (§21.2/§21.3) ------------------------------------------------

fn key(text: &str) -> Value {
    Value::Text(Text::new(text))
}

fn blob_leaf(digest_hex: &str) -> Value {
    // A struct standing in for a row whose payload carries a shared blob digest.
    Value::Struct(liasse_value::Struct::new([(Text::new("sha512"), key(digest_hex))]))
}

/// §21.2 (shared digest / co-resident): erasing one occurrence scrubs only that
/// occurrence's leaf; a co-resident occurrence whose payload carries the SAME blob
/// digest keeps its retained payload intact.
#[test]
fn erase_scrubs_only_the_named_occurrence_not_a_digest_sharing_sibling() {
    let shared_digest = BlobIntegrity::digest_hex(b"the shared blob bytes");
    let mut history = Erasure::new();
    // Two distinct occurrences whose payloads both reference the same blob digest.
    let a = Occurrence::new("rows/a");
    let b = Occurrence::new("rows/b");
    history.record(a.clone(), blob_leaf(&shared_digest));
    history.record(b.clone(), blob_leaf(&shared_digest));

    history.erase(std::slice::from_ref(&a)).expect("erase a");

    assert!(history.payload(&a).is_none(), "the erased occurrence's leaf is scrubbed");
    assert_eq!(
        history.payload(&b),
        Some(&blob_leaf(&shared_digest)),
        "§21.2: a co-resident occurrence sharing the blob digest keeps its payload",
    );
    assert!(history.stub(&a).is_some(), "a verifiable stub remains for the erased leaf");
}

/// §21.3 (reinsertion after an intervening change elsewhere): a reinsertion
/// restores only where the exact expected stub remains, and is unaffected by an
/// intervening modification of an UNRELATED occurrence.
#[test]
fn reinsert_targets_only_its_occurrence_across_an_intervening_change() {
    let mut history = Erasure::new();
    let target = Occurrence::new("rows/target");
    let other = Occurrence::new("rows/other");
    history.record(target.clone(), key("secret"));
    history.record(other.clone(), key("original"));

    let extract = history.erase(std::slice::from_ref(&target)).expect("erase target");

    // An intervening change to an unrelated occurrence (models a reconcile/mutation
    // touching other state between erase and reinsert).
    history.record(other.clone(), key("changed"));

    history.reinsert(&extract).expect("reinsert restores against the untouched stub");
    assert_eq!(
        history.payload(&target),
        Some(&key("secret")),
        "§21.3: the target is restored where its stub still matches",
    );
    assert_eq!(
        history.payload(&other),
        Some(&key("changed")),
        "the unrelated occurrence keeps its intervening value",
    );
}

// Silence an unused warning for the map alias if the toolchain flags it.
#[allow(dead_code)]
fn _touch() -> BTreeMap<String, Value> {
    BTreeMap::new()
}
