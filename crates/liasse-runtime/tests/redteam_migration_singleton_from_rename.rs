#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe of the §8.2 root-singleton migration fix (commit 1c11d1e,
//! `build_migrated` in crates/liasse-runtime/src/migrate.rs): a `$from` RENAME of
//! a root-singleton field is silently ignored, so the old value is not carried
//! into the renamed target field.
//!
//! # DEFECT
//!
//! §20.1: "A target field MAY identify its previous source ... `$from` names the
//! old field or collection, `$as` transforms its old value bound as `.` ...
//! Without `$as`, the compatible value is copied." A root singleton field (a
//! scalar declared directly under `$model`, §8.2) is a target field, so a
//! `$from`-declared rename MUST carry the old value forward under the new name.
//!
//! The migration path applies `$from`/`$as` mappings ONLY inside the
//! keyed-collection loop of `build_migrated` (migrate.rs:254-281, via `map_row`).
//! The §8.2 singleton carry loop (migrate.rs:297-309) never consults the
//! `MigrationPlan`: it copies `old_singleton.get(member.name)` keyed by the
//! TARGET member's own name. For a renamed member the target name is absent from
//! the source singleton row, so nothing is carried and the `$from` mapping is a
//! no-op. (`MigrationPlan::read` even mis-files the renamed singleton field under
//! `plan.collections`, since a `{ $type, $from }` object parses as a
//! collection-with-`$from`; that entry is then only consulted for compiled
//! collections, never for the singleton — so the rename is dropped twice over.)
//!
//! Net effect: the renamed singleton value is LOST (violating §20.1's compatible
//! copy under `$from`), whereas the identical rename on a keyed COLLECTION field
//! copies the value correctly (control below + corpus
//! `rename-field-via-from-copies-value`).
//!
//! Both expected values are re-derived from §20.1 text ("Without `$as`, the
//! compatible value is copied") alone — never from implementation behavior.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

// ---------------------------------------------------------------------------
// THE BUG: a `$from` rename of a ROOT SINGLETON field carries nothing.
// ---------------------------------------------------------------------------

/// v1: a root singleton scalar `name`, seeded "Alpha".
const SINGLETON_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.rename@1.0.0",
  "$model": {
    "name": "text",
    "readout": { "$view": ". { name }" }
  },
  "$data": { "name": "Alpha" }
}"#;

/// v2 (major, 2.0.0): renames the singleton `name` to `label` via `$from`, no
/// `$as`. §20.1 requires the compatible value "Alpha" to be copied into `label`.
/// `label` is OPTIONAL so the required-field gate cannot fire — isolating the
/// observable to "was the value carried?" rather than a population rejection.
const SINGLETON_V2_RENAME: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.rename@2.0.0",
  "$model": {
    "label": { "$type": "text", "$optional": true, "$from": "name" },
    "readout": { "$view": ". { label }" }
  }
}"#;

#[test]
fn singleton_field_rename_via_from_loses_value() {
    let mut engine = load("mig-singleton-rename", SINGLETON_V1);
    let mut generator = generator();

    // Pre-migration: the seeded singleton reads "Alpha".
    let before = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        before.rows()[0].field("name").map(Value::to_wire),
        Some(serde_json::json!("Alpha")),
        "pre-migration singleton scalar reads its seed",
    );

    match engine.update(SINGLETON_V2_RENAME, &mut generator) {
        Ok(_) => {}
        Err(UpdateError::Rejected(r)) => panic!(
            "the `$from` rename migration was rejected ({}); it should COMMIT and carry the value",
            r.message()
        ),
        Err(other) => panic!("migration failed unexpectedly: {other}"),
    }

    // §20.1: without `$as`, the compatible value is copied — so `label` must read
    // "Alpha". The singleton carry loop ignores the `$from`, so it reads `none`.
    let after = engine.view_at_head("readout").expect("view").expect("declared");
    let got = after.rows()[0].field("label").map(Value::to_wire);
    assert_eq!(
        got,
        Some(serde_json::json!("Alpha")),
        "BUG (§20.1): the renamed singleton field `label` must carry the source `name` value \
         \"Alpha\" (\"Without `$as`, the compatible value is copied\"), but the §8.2 singleton \
         carry loop ignores the `$from` mapping, so the value is LOST. Observed: {got:?}",
    );
}

// ---------------------------------------------------------------------------
// CONTROL (passing): the IDENTICAL `$from` rename on a keyed COLLECTION field
// copies the value, proving the mapping machinery works and the singleton is the
// specific gap.
// ---------------------------------------------------------------------------

/// v1: a keyed collection `items` with a scalar `name`, seeded "Alpha".
const COLLECTION_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.collection.rename@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "name": "text" },
    "readout": { "$view": ".items { id, name }" }
  },
  "$data": { "items": { "a": { "name": "Alpha" } } }
}"#;

/// v2 (major, 2.0.0): renames `items.name` to `items.label` via `$from`.
const COLLECTION_V2_RENAME: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.collection.rename@2.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "label": { "$type": "text", "$from": "name" } },
    "readout": { "$view": ".items { id, label }" }
  }
}"#;

#[test]
fn collection_field_rename_via_from_copies_value_control() {
    let mut engine = load("mig-collection-rename", COLLECTION_V1);
    let mut generator = generator();

    engine.update(COLLECTION_V2_RENAME, &mut generator).expect("collection rename migration commits");

    let after = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        after.rows()[0].field("label").map(Value::to_wire),
        Some(serde_json::json!("Alpha")),
        "control: a collection `$from` rename copies the value (the singleton path must too)",
    );
}
