#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! CHARACTERIZATION probe (Target 1, singleton type-change edge; also the
//! collection analogue to scope it): a §20 migration that CHANGES a scalar
//! field's TYPE across a version bump — with no `$as` transform — and a
//! populated, type-incompatible live value.
//!
//! §20.1 pins the migration order as "compatible same-identity copy" then checks
//! "the complete prospective target ... under ordinary keys, refs, uniqueness,
//! checks". §22.1 lists "field and shape types" among the constraints that hold
//! in EVERY committed state. A live `text` value "Alpha" is NOT a valid `int`, so
//! a text->int change with that value cannot yield a well-typed target state:
//! either the migration REJECTS (the value is unrepresentable in the target
//! type) or the copy simply is not "compatible same-identity" and the required
//! int field is unpopulated -> reject. Committing a `Value::Text` into an `int`
//! field violates §22.1.
//!
//! `coerce_and_require` (migrate.rs:419-492) only coerces ref-typed and
//! enum-bearing fields; a plain scalar type change (text->int, int->bool, ...) is
//! neither, so the verbatim-copied value passes straight through the final check.
//! This probe records the OBSERVED outcome for the singleton and the collection
//! so the scope (singleton-specific vs general) is documented, and asserts the
//! spec-mandated no-strand outcome.

mod support;

use liasse_runtime::{Engine, UpdateError, Value};
use liasse_store::MemoryStore;
use support::{generator, load};

/// True iff `wire` is a canonical base-10 integer wire value (Annex A.1): a JSON
/// string of digits (optionally leading `-`). "Alpha" is not.
fn is_int_wire(wire: &serde_json::Value) -> bool {
    wire.as_str().is_some_and(|s| {
        let digits = s.strip_prefix('-').unwrap_or(s);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

fn characterize(engine: &mut Engine<MemoryStore>, target: &str, gen_label: &str) -> String {
    let mut generator = generator();
    let _ = gen_label;
    match engine.update(target, &mut generator) {
        Err(UpdateError::Rejected(_)) => "REJECTED (spec-correct: unrepresentable value)".to_owned(),
        Err(other) => format!("ERROR: {other}"),
        Ok(_) => "COMMITTED".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Singleton scalar text -> int with a non-numeric live value.
// ---------------------------------------------------------------------------

const SINGLETON_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.typechange@1.0.0",
  "$model": {
    "flag": "text",
    "readout": { "$view": ". { flag }" }
  },
  "$data": { "flag": "Alpha" }
}"#;

const SINGLETON_V2_INT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.typechange@2.0.0",
  "$model": {
    "flag": "int",
    "readout": { "$view": ". { flag }" }
  }
}"#;

#[test]
fn singleton_scalar_text_to_int_incompatible_value_must_not_strand() {
    let mut engine = load("mig-singleton-typechange", SINGLETON_V1);
    let observed = characterize(&mut engine, SINGLETON_V2_INT, "singleton");

    // If the migration committed, read the field back and check it is a valid int.
    let stranded = if observed == "COMMITTED" {
        match engine.view_at_head("readout") {
            Ok(Some(view)) => {
                let w = view.rows()[0].field("flag").map(Value::to_wire);
                match &w {
                    Some(wire) if !is_int_wire(wire) => Some(format!("{wire:?}")),
                    _ => None,
                }
            }
            _ => None,
        }
    } else {
        None
    };

    assert!(
        stranded.is_none(),
        "BUG (§20.1/§22.1): the singleton `flag` changed text->int with a non-numeric live value \
         \"Alpha\"; the migration {observed} and left a non-int value {} stranded in the int field. \
         A plain scalar type change is neither ref nor enum, so `coerce_and_require` never \
         re-validates it, and the verbatim `Value::Text` survives the final check.",
        stranded.as_deref().unwrap_or("<none>"),
    );
}

// ---------------------------------------------------------------------------
// Collection scalar text -> int (same, to scope singleton-specific vs general).
// ---------------------------------------------------------------------------

const COLLECTION_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.collection.typechange@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "flag": "text" },
    "readout": { "$view": ".items { id, flag }" }
  },
  "$data": { "items": { "a": { "flag": "Alpha" } } }
}"#;

const COLLECTION_V2_INT: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.collection.typechange@2.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "flag": "int" },
    "readout": { "$view": ".items { id, flag }" }
  }
}"#;

#[test]
fn collection_scalar_text_to_int_incompatible_value_must_not_strand() {
    let mut engine = load("mig-collection-typechange", COLLECTION_V1);
    let observed = characterize(&mut engine, COLLECTION_V2_INT, "collection");

    let stranded = if observed == "COMMITTED" {
        match engine.view_at_head("readout") {
            Ok(Some(view)) => {
                let w = view.rows()[0].field("flag").map(Value::to_wire);
                match &w {
                    Some(wire) if !is_int_wire(wire) => Some(format!("{wire:?}")),
                    _ => None,
                }
            }
            _ => None,
        }
    } else {
        None
    };

    assert!(
        stranded.is_none(),
        "BUG (§20.1/§22.1): the collection `items.flag` changed text->int with a non-numeric live \
         value \"Alpha\"; the migration {observed} and left a non-int value {} stranded in the int \
         field — the SAME `coerce_and_require` scalar-type gap, showing it is NOT singleton-specific.",
        stranded.as_deref().unwrap_or("<none>"),
    );
}

// ---------------------------------------------------------------------------
// CONTROL (passing): the migration final check DOES enforce a field-type
// violation when the type is an ENUM — an out-of-domain label rejects
// (`coerce_and_require`'s enum re-validation, the landed
// `redteam_enum_narrowing_migration.rs` precedent). This proves the final check
// is MEANT to reject field-type violations; the scalar text->int cases above show
// that guarantee holds ONLY for enum/ref fields, not for a plain scalar type
// change — the exact inconsistency the bug exposes.
// ---------------------------------------------------------------------------

const ENUM_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.enumctl@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "status": { "$enum": ["draft", "active", "archived"] } },
    "readout": { "$view": ".items { id, status }" }
  },
  "$data": { "items": { "a": { "status": "archived" } } }
}"#;

const ENUM_V2_NARROWED: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.enumctl@2.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "status": { "$enum": ["draft", "active"] } },
    "readout": { "$view": ".items { id, status }" }
  }
}"#;

#[test]
fn migration_final_check_rejects_enum_field_type_violation_control() {
    let mut engine = load("mig-enumctl", ENUM_V1);
    let observed = characterize(&mut engine, ENUM_V2_NARROWED, "enum");
    assert!(
        observed.starts_with("REJECTED"),
        "control: a migrated ENUM field-type violation must reject (it does), so the final check is \
         meant to enforce field types — the scalar text->int strand above is the gap. Observed: {observed}",
    );
}
