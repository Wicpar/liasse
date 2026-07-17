#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §18.5 blob placement members read end-to-end through the engine: a mutation
//! `return` that projects `.file.$satisfied`/`.file.$stored`/`.file.$surplus`
//! resolves against the placement facts the engine records
//! ([`Engine::record_blob_placement`], §18.5).
//!
//! Physical placement lives outside application state, so the engine cannot
//! derive these facts; a driver records them from the blob subsystem's
//! `blob_placement_state`. Each expectation here is the fact the test itself
//! recorded — never the program's own answer — and the "no facts recorded →
//! rejected" case proves the value is read from the ledger, not fabricated.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, PlacementState, StoreId, Value};
use liasse_store::MemoryStore;
use liasse_value::{BlobDescriptor, MediaType, Sha512, Text};
use serde_json::json;
use support::{generator, load};

/// A `docs` collection with a blob `file` field placed in `/stores['primary']`,
/// and a root `add` mutation whose `return` reads every §18.5 placement member.
const PLACEMENT_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.blobplacement@1.0.0"
  "$model": {
    "stores": { "$key": "id", "id": "text", "connector": "text", "enabled": "bool = true" }
    "docs": {
      "$key": "id"
      "$blob_storage": { "$in": "/stores['primary']" }
      "id": "text"
      "file": { "$type": "blob", "$max_bytes": "1000", "$media": ["text/plain"] }
    }
    "$mut": {
      "add": [
        "doc = .docs + { id: @id, file: @file }"
        "return doc { id, satisfied: .file.$satisfied, stored: .file.$stored { id }, surplus: .file.$surplus { id } }"
      ]
    }
  }
  "$data": { "stores": { "primary": { "connector": "fs" } } }
}"#;

/// The canonical 128-hex digest of the seeded descriptor (64 `0xab` bytes).
fn digest_hex() -> String {
    "ab".repeat(64)
}

/// A `text/plain` blob descriptor bound as the `@file` mutation parameter.
fn descriptor() -> Value {
    Value::Blob(Box::new(BlobDescriptor::new(
        Sha512::parse(&digest_hex()).expect("hash"),
        4,
        MediaType::new("text/plain"),
        None,
    )))
}

fn state(stored: &[&str], satisfied: bool, surplus: &[&str]) -> PlacementState {
    PlacementState {
        stored: stored.iter().map(|s| StoreId::new(*s)).collect(),
        satisfied,
        surplus: surplus.iter().map(|s| StoreId::new(*s)).collect(),
    }
}

/// Call the `add` mutation with the fixed descriptor as its `@file` parameter.
fn add(engine: &mut liasse_runtime::Engine<MemoryStore>) -> CallOutcome {
    let mut g = generator();
    engine
        .call(
            &CallRequest::new("add")
                .arg("id", Value::Text(Text::new("d1")))
                .arg("file", descriptor()),
            &mut g,
        )
        .expect("call")
}

/// §18.5: with a single verified copy in `primary` satisfying the `$in` policy,
/// the return reads `$satisfied` = true, `$stored` = [{id: primary}], and an
/// empty `$surplus` — the facts the driver recorded, surfaced through the hook.
#[test]
fn return_reads_recorded_satisfied_stored_and_surplus() {
    let mut engine = load("blobplacement", PLACEMENT_APP);
    engine.record_blob_placement(digest_hex(), &state(&["primary"], true, &[]));

    let outcome = add(&mut engine);
    let value = outcome.response().expect("committed response").to_wire();
    assert_eq!(
        value,
        json!({ "id": "d1", "satisfied": true, "stored": [{ "id": "primary" }], "surplus": [] })
    );
}

/// §18.5: the members report the *recorded* facts, not a fabricated satisfied
/// state — an unsatisfied policy over two verified stores with one surplus copy
/// surfaces `$satisfied` = false, `$stored` = both, `$surplus` = the extra copy.
#[test]
fn return_reflects_unsatisfied_policy_and_surplus() {
    let mut engine = load("blobplacement", PLACEMENT_APP);
    engine.record_blob_placement(digest_hex(), &state(&["a", "b"], false, &["b"]));

    let outcome = add(&mut engine);
    let value = outcome.response().expect("committed response").to_wire();
    assert_eq!(
        value,
        json!({
            "id": "d1",
            "satisfied": false,
            "stored": [{ "id": "a" }, { "id": "b" }],
            "surplus": [{ "id": "b" }]
        })
    );
}

/// §18.5: re-recording a digest replaces its facts, so a policy change that
/// shifts `$satisfied`/`$surplus` without moving bytes is reflected on the next
/// read (the surplus-after-shrink observation).
#[test]
fn re_recording_updates_the_facts() {
    let mut engine = load("blobplacement", PLACEMENT_APP);
    engine.record_blob_placement(digest_hex(), &state(&["primary"], true, &[]));
    // A later policy shrink leaves `primary` surplus while `secondary` alone is
    // now required and present.
    engine.record_blob_placement(digest_hex(), &state(&["primary", "secondary"], true, &["primary"]));

    let outcome = add(&mut engine);
    let value = outcome.response().expect("committed response").to_wire();
    assert_eq!(
        value,
        json!({
            "id": "d1",
            "satisfied": true,
            "stored": [{ "id": "primary" }, { "id": "secondary" }],
            "surplus": [{ "id": "primary" }]
        })
    );
}

/// §18.5: a placement member is engine-recorded state. With no facts recorded
/// for the descriptor, the `return` cannot resolve `.file.$satisfied`, so the
/// whole mutation is rejected (a fail-closed contract breach) — proof the value
/// is read from the ledger, not invented.
#[test]
fn return_without_recorded_placement_is_rejected() {
    let mut engine = load("blobplacement", PLACEMENT_APP);
    let outcome = add(&mut engine);
    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "reading a placement member with no recorded facts must reject, got {outcome:?}",
    );
}
