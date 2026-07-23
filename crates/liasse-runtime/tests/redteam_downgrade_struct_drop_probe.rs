#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe of the §20.2 downgrade representability gate
//! (`downgrade_representable` in crates/liasse-runtime/src/migrate.rs): a downgrade
//! that DROPS a populated static-struct member (§5.3) is admitted, silently
//! discarding live data the older shape cannot represent.
//!
//! # DEFECT
//!
//! §20.2: "A downgrade is rejected only when a populated live value cannot be
//! represented in the older shape and no declared `$down`, deduced inverse, or
//! stash restoration preserves it, or when the route crosses a `$one_way` delta."
//! A static struct member (§5.3) is a live value of the row. When the older
//! (downgrade-target) shape declares no struct member of that name, and no
//! declared transform reconstructs it, the value cannot be represented in the
//! older shape and the downgrade MUST be rejected — exactly as the field-drop
//! variant is (corpus `downgrade-drops-unrepresentable-field-rejected`, and the
//! CONTROL below).
//!
//! `downgrade_representable` enforces §20.2 by scanning `active_collection.fields`
//! for a populated field the target neither keeps nor reconstructs. A static
//! struct member compiles into `CompiledCollection::structs`, NOT `fields`
//! (crates/liasse-runtime/src/compiled.rs), so the scan never inspects it: a
//! whole populated struct dropped on downgrade slips past the gate and the
//! migration commits, discarding the struct's live data. This reference
//! implementation does not implement the §20.2 stash, so this rejection IS the
//! only §20.2 preservation mechanism — and it has a struct-shaped hole.
//!
//! Both expected outcomes are derived from §20.2 text alone (a populated live
//! value with no representation in the older shape and no preserving transform is
//! rejected) — never from implementation behavior.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

// ---------------------------------------------------------------------------
// THE BUG: a downgrade dropping a populated STATIC STRUCT member is admitted.
// ---------------------------------------------------------------------------

/// v2 (2.0.0): a keyed collection whose rows carry a scalar `keep` and a static
/// struct `profile` (§5.3). Row `t1` is seeded with `profile` populated.
const STRUCT_V2: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downstruct@2.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "keep": "text",
      "profile": {
        "severity": "int",
        "note": "text"
      }
    },
    "readout": { "$view": ".things { id, keep }" }
  },
  "$data": {
    "things": { "t1": { "keep": "K", "profile": { "severity": "3", "note": "urgent" } } }
  }
}"#;

/// v1 (1.0.0): the older shape DROPS the `profile` struct entirely. The live
/// `profile` value of `t1` cannot be represented here and no transform preserves
/// it, so a downgrade 2.0.0 -> 1.0.0 must be rejected (§20.2).
const STRUCT_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downstruct@1.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "keep": "text"
    },
    "readout": { "$view": ".things { id, keep }" }
  }
}"#;

#[test]
fn downgrade_dropping_populated_struct_is_rejected() {
    let mut engine = load("mig-down-struct", STRUCT_V2);
    let mut generator = generator();

    // Pre-downgrade: the seeded struct-bearing row is live under 2.0.0.
    let before = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        before.rows()[0].field("keep").map(Value::to_wire),
        Some(serde_json::json!("K")),
        "pre-downgrade the seeded row is live",
    );

    // §20.2: the older shape cannot represent the live `profile` and no declared
    // downgrade transform preserves it, so this downgrade MUST be rejected.
    match engine.update(STRUCT_V1, &mut generator) {
        Err(UpdateError::Rejected(_)) => { /* correct: §20.2 rejection */ }
        Ok(_) => panic!(
            "BUG (§20.2): the downgrade 2.0.0 -> 1.0.0 dropped the populated static struct \
             `profile` and COMMITTED, silently discarding live data the older shape cannot \
             represent. `downgrade_representable` only scans `collection.fields`, so a struct \
             member (compiled into `collection.structs`) is never checked. §20.2 requires this \
             downgrade to be rejected with 2.0.0 left active.",
        ),
        Err(other) => panic!("downgrade failed for the wrong reason: {other}"),
    }

    // The rejected downgrade left 2.0.0 active: the row is unchanged and readable.
    let after = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        after.rows()[0].field("keep").map(Value::to_wire),
        Some(serde_json::json!("K")),
        "the rejected downgrade left 2.0.0 active with the row intact",
    );
}

// ---------------------------------------------------------------------------
// CONTROL (passing): the IDENTICAL downgrade dropping a populated SCALAR field
// is correctly rejected, proving the §20.2 gate works and the struct is the gap.
// ---------------------------------------------------------------------------

/// v2 (2.0.0): a scalar `extra` alongside `keep`, both populated.
const FIELD_V2: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downfield@2.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "keep": "text",
      "extra": "text"
    },
    "readout": { "$view": ".things { id, keep }" }
  },
  "$data": {
    "things": { "t1": { "keep": "K", "extra": "live" } }
  }
}"#;

/// v1 (1.0.0): drops the scalar `extra`. §20.2 rejects this downgrade.
const FIELD_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downfield@1.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "keep": "text"
    },
    "readout": { "$view": ".things { id, keep }" }
  }
}"#;

#[test]
fn downgrade_dropping_populated_scalar_field_is_rejected_control() {
    let mut engine = load("mig-down-field", FIELD_V2);
    let mut generator = generator();

    match engine.update(FIELD_V1, &mut generator) {
        Err(UpdateError::Rejected(_)) => { /* correct: §20.2 rejection */ }
        Ok(_) => panic!(
            "control regression: dropping the populated scalar `extra` on downgrade must be \
             rejected (§20.2) — the field-level gate is what the struct case is supposed to mirror",
        ),
        Err(other) => panic!("control downgrade failed for the wrong reason: {other}"),
    }
}

// ---------------------------------------------------------------------------
// THE SINGLETON TWIN: a downgrade dropping a populated §8.2 root-singleton member
// is the same §20.2 loss at a different state shape — the singleton reserved row
// is not a keyed collection, so `active_state.collections()` never yields it.
// ---------------------------------------------------------------------------

/// v2 (2.0.0): a root singleton scalar `title` (declared directly under `$model`,
/// §8.2), seeded "Hello".
const SINGLETON_V2: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downsingleton@2.0.0",
  "$model": {
    "title": "text",
    "readout": { "$view": ". { title }" }
  },
  "$data": { "title": "Hello" }
}"#;

/// v1 (1.0.0): the older shape drops the singleton `title` (its only durable
/// state), carrying instead an unrelated defaulted `note`. The live `title`
/// cannot be represented and no inverse preserves it -> §20.2 rejects.
const SINGLETON_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.downsingleton@1.0.0",
  "$model": {
    "note": "text = 'x'",
    "readout": { "$view": ". { note }" }
  }
}"#;

#[test]
fn downgrade_dropping_populated_singleton_member_is_rejected() {
    let mut engine = load("mig-down-singleton", SINGLETON_V2);
    let mut generator = generator();

    match engine.update(SINGLETON_V1, &mut generator) {
        Err(UpdateError::Rejected(_)) => { /* correct: §20.2 rejection */ }
        Ok(_) => panic!(
            "BUG (§8.2/§20.2): the downgrade dropped the populated root singleton member `title` \
             and COMMITTED, discarding live root state. The singleton reserved row is not a keyed \
             collection, so `downgrade_representable` never inspected it. §20.2 requires this \
             downgrade to be rejected with 2.0.0 left active.",
        ),
        Err(other) => panic!("singleton downgrade failed for the wrong reason: {other}"),
    }
}
