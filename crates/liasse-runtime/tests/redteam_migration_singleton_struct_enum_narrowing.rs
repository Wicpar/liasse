#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe of the just-landed §8.2 root-singleton migration fix
//! (commit 1c11d1e, `build_migrated` in crates/liasse-runtime/src/migrate.rs):
//! a §20 migration that NARROWS a `$enum` nested inside a root-singleton STATIC
//! STRUCT member strands the dropped label in committed target state.
//!
//! # BACKGROUND
//!
//! Wave-14 (commit 80fac2c) taught the migration final check to re-validate a
//! migrated enum against the TARGET's closed label set, and a later fix extended
//! it to enums one struct layer down for a keyed COLLECTION via the
//! `for struct_meta in &collection.structs` loop in
//! `coerce_and_require` (migrate.rs:477-488). The landed test
//! `redteam_struct_nested_enum_narrowing_migration.rs` confirms the COLLECTION
//! path REJECTS such a narrowing (it passes at HEAD).
//!
//! Commit 1c11d1e then carried the §8.2 root singleton reserved row through a
//! migration and claimed to cover "nested static-struct" singleton members.
//!
//! # DEFECT
//!
//! `coerce_and_require` re-validates a migrated row's enums through two loops
//! keyed off the row's compiled collection: `collection.fields` and
//! `collection.structs`. For the singleton reserved row, `collection_at(["$root"])`
//! resolves to the `root_singleton` pseudo-collection, and
//! `compile_root_singleton` (compiled.rs:1091-1101) builds it with
//! `structs: Vec::new()` — a root static-struct member returns `Ok(None)` from
//! `compile_field` (compiled.rs:880), so it is neither a compiled field NOR a
//! compiled struct of the singleton pseudo-collection. Both re-validation loops
//! therefore skip it: the compatible same-identity copy of the `profile` struct
//! (staged verbatim by the singleton carry loop, migrate.rs:297-309) is never
//! re-checked against the target's narrowed enum.
//!
//! Net effect: after the migration commits, the singleton struct member carries a
//! label that is NOT a declared label of the target enum. §5.9 makes `$enum` a
//! CLOSED set; §20.1 requires the complete prospective target to be "checked under
//! ordinary keys, refs, uniqueness, checks ... before the package update commits";
//! §22.1 lists "field and shape types" among the constraints that hold in EVERY
//! committed state. The dropped ordinal is out of range for the 2-label target
//! set, so B.1 ordering/equality over it are undefined — the exact corruption the
//! collection fix guards against, reachable one struct layer down in the singleton.
//!
//! Expected (spec-correct): the migration is REJECTED and 1.0.0 stays active,
//! EXACTLY as the collection analogue is.
//! Actual (bug): the migration COMMITS, stranding `archived` inside the singleton
//! struct member.
//!
//! # ISOLATION
//!
//! `singleton_top_level_enum_narrowing_rejects` is the passing control: a
//! narrowing of a TOP-LEVEL singleton enum field (which DOES land in
//! `root_singleton.fields`) is correctly rejected. The only difference between the
//! control and the bug case is one struct layer of nesting, isolating the defect
//! to the missing `structs` re-validation of the singleton pseudo-collection.
//! Both expected outcomes are re-derived from SPEC.md text alone (§5.9 closed
//! enum, §20.1 pre-commit check, §22.1 field types), never from implementation
//! behavior.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

// ---------------------------------------------------------------------------
// THE BUG: enum narrowed one struct layer down in the ROOT SINGLETON.
// ---------------------------------------------------------------------------

/// v1: a ROOT SINGLETON static struct member `profile` (§8.2) carries an enum
/// member `status` in {draft, active, archived}, seeded to `archived`.
const STRUCT_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.structenum@1.0.0",
  "$model": {
    "profile": {
      "status": { "$enum": ["draft", "active", "archived"] },
      "note": "text"
    },
    "readout": { "$view": ". { s: .profile.status, n: .profile.note }" }
  },
  "$data": { "profile": { "status": "archived", "note": "keep" } }
}"#;

/// v2 (major release, 2.0.0): narrows the struct-nested `status` enum — `archived`
/// is dropped. The struct member is otherwise byte-identical, so §20.1's
/// compatible same-identity copy carries the stale `archived` into the target.
const STRUCT_V2_NARROWED: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.structenum@2.0.0",
  "$model": {
    "profile": {
      "status": { "$enum": ["draft", "active"] },
      "note": "text"
    },
    "readout": { "$view": ". { s: .profile.status, n: .profile.note }" }
  }
}"#;

#[test]
fn singleton_struct_nested_enum_narrowing_strands_out_of_domain_value() {
    let mut engine = load("mig-singleton-structenum", STRUCT_V1);
    let mut generator = generator();

    // Pre-migration sanity: the seeded singleton struct member reads `archived`.
    let before = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        before.rows()[0].field("s").map(Value::to_wire),
        Some(serde_json::json!("archived")),
        "pre-migration singleton struct enum member reads its seeded label",
    );

    let result = engine.update(STRUCT_V2_NARROWED, &mut generator);

    // §20.1/§22.1/§5.9: `archived` is outside the target enum's closed set, so the
    // prospective target violates the enum field type and the migration MUST be
    // rejected (1.0.0 stays active) — exactly as the COLLECTION analogue is
    // (`redteam_struct_nested_enum_narrowing_migration.rs` passes).
    match result {
        Err(UpdateError::Rejected(_)) => {} // spec-correct: singleton path caught it
        Err(other) => panic!("expected a §20.1 migration rejection, got a different error: {other}"),
        Ok(report) => {
            let read_back = match engine.view_at_head("readout") {
                Ok(Some(view)) => format!("readable, profile.status={:?}", view.rows()[0].field("s")),
                Ok(None) => "view not declared".to_owned(),
                Err(error) => format!("UNREADABLE committed state: {error}"),
            };
            panic!(
                "BUG (§5.9/§22.1/§20.1): the singleton STRUCT-nested enum-narrowing migration \
                 committed ({report:?}) instead of rejecting. `archived` is not a declared label of \
                 the target enum {{draft, active}}, yet the compatible copy of the singleton `profile` \
                 struct carried it through unchecked: `root_singleton.structs` is empty, so \
                 `coerce_and_require` never re-validates a singleton struct's enum leaves. \
                 Resulting state: {read_back}."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CONTROL (passing): the SAME narrowing on a TOP-LEVEL singleton enum field is
// correctly rejected, because a top-level scalar enum DOES compile into
// `root_singleton.fields` and `coerce_and_require`'s field loop re-validates it.
// The only difference from the bug case is one struct layer of nesting.
// ---------------------------------------------------------------------------

/// v1: a TOP-LEVEL singleton enum field `state` in {draft, active, archived}.
const TOP_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.topenum@1.0.0",
  "$model": {
    "state": { "$enum": ["draft", "active", "archived"] },
    "readout": { "$view": ". { state }" }
  },
  "$data": { "state": "archived" }
}"#;

/// v2 (major release): narrows the top-level enum — `archived` is dropped.
const TOP_V2_NARROWED: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton.topenum@2.0.0",
  "$model": {
    "state": { "$enum": ["draft", "active"] },
    "readout": { "$view": ". { state }" }
  }
}"#;

#[test]
fn singleton_top_level_enum_narrowing_rejects() {
    let mut engine = load("mig-singleton-topenum", TOP_V1);
    let mut generator = generator();

    let result = engine.update(TOP_V2_NARROWED, &mut generator);

    match result {
        Err(UpdateError::Rejected(_)) => {} // spec-correct
        Err(other) => panic!("expected a §20.1 migration rejection, got a different error: {other}"),
        Ok(report) => panic!(
            "control regressed: a top-level singleton enum narrowing must reject, committed {report:?}"
        ),
    }

    // The rejected migration leaves 1.0.0 active: `state` still reads `archived`.
    let view = engine.view_at_head("readout").expect("view").expect("declared");
    assert_eq!(
        view.rows()[0].field("state").map(Value::to_wire),
        Some(serde_json::json!("archived")),
        "the rejected migration must leave 1.0.0 active with the singleton unchanged",
    );
}
