#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Annex E.5 explicit-ordering narrowing on a minor package update.
//!
//! A public view's declared `$sort` is a boundary contract: Annex E.2 lists
//! "view parameter, output-shape, identity, and explicit ordering contracts"
//! among the promises an independently versioned client relies on. Annex E.3
//! lists "row identity and explicit ordering" among the mechanically decidable
//! checks and states the checker "MUST reject every narrowing it can establish."
//! Annex E.5 lists "changing explicit sort semantics" among the BREAKING output
//! changes, and Annex E.1/§20.3 require a minor or patch release to "preserve or
//! widen the compatibility surface", using a new major for breaking changes.
//!
//! Each case re-derives its outcome from the Annex E text alone.

mod support;

use liasse_runtime::{Engine, RejectionReason, UpdateError};
use liasse_store::MemoryStore;
use support::{generator, load};

const SORT_V1: &str = r#"{
  "$liasse": 1
  "$app": "t.compat.sort@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "n": "int" }
    "$public": { "byn": { "$view": ".items { id, n, $sort: [\"n\"] }" } }
  }
  "$data": { "items": { "a": { "n": "10" }, "b": { "n": "30" }, "c": { "n": "20" } } }
}"#;

/// E.5: reversing a public view's explicit `$sort` on a minor is a breaking
/// change and MUST be rejected before activation as a boundary narrowing
/// (E.1/E.3/E.9). Ascending `["n"]` -> descending `["-n"]` reorders every row a
/// paginating client observes.
#[test]
fn minor_reverses_explicit_view_sort_rejected() {
    let mut engine: Engine<MemoryStore> = load("sortcompat", SORT_V1);
    let target = SORT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#"$sort: [\"n\"]"#, r#"$sort: [\"-n\"]"#);
    assert_ne!(target, SORT_V1.replace("@1.0.0", "@1.1.0"), "the $sort direction must actually flip");

    let mut generator = generator();
    match engine.update(&target, &mut generator) {
        Err(UpdateError::Rejected(rejection)) => {
            assert_eq!(
                rejection.reason(),
                RejectionReason::Compatibility,
                "an ordering narrowing is a compatibility rejection, got: {}",
                rejection.message()
            );
            assert!(
                rejection.message().contains("ordering"),
                "the diagnostic reports the ordering change: {}",
                rejection.message()
            );
        }
        other => panic!(
            "SPEC VIOLATION (Annex E.5): reversing the exposed view `$sort` from ascending to \
             descending on a minor update MUST be rejected as an explicit-ordering narrowing, \
             but the engine returned {other:?}"
        ),
    }
    // E.9: the prior release stays active.
    assert_eq!(engine.model().header().identity.version.minor, 0, "1.0.0 stays active");
}

/// Control (no over-rejection): a minor that PRESERVES the `$sort` while widening
/// the projection with an optional output field is substitutable and MUST
/// commit. Proves the ordering check flags a change, not the mere presence of a
/// `$sort`.
#[test]
fn minor_preserves_explicit_view_sort_committed() {
    let mut engine: Engine<MemoryStore> = load("sortkeep", SORT_V1);
    let target = SORT_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""n": "int""#, r#""n": "int", "label": "text?""#)
        .replace(r#"{ id, n, $sort: [\"n\"] }"#, r#"{ id, n, label, $sort: [\"n\"] }"#);
    let mut generator = generator();
    engine.update(&target, &mut generator).expect("an unchanged $sort with a widened projection commits");
    assert_eq!(engine.model().header().identity.version.minor, 1, "1.1.0 is active");
}
