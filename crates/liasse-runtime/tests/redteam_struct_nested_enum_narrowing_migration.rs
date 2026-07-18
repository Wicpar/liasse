#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1/§22.1/§5.9 red-team: a migration that narrows a `$enum` closed label set
//! must not strand an out-of-domain value in committed target state — and this must
//! hold when the enum is a member of a STRUCT field, not only a top-level field.
//!
//! This is the struct-nested analogue of `redteam_enum_narrowing_migration.rs`.
//! Wave-14 commit 80fac2c ("Re-validate migrated enum values against the target's
//! closed set") closed the gap for a top-level enum field: `map_row` (migrate.rs)
//! copies the source value verbatim, so `coerce_and_require` re-validates every
//! migrated enum field against the TARGET's closed set. But that re-validation is
//! gated on `rules::is_enum_field(&field.ty)`, and `rules::enum_of` (rules.rs)
//! unwraps only `Type::Optional` — NOT `Type::Struct` (nor `Type::Set`). A field
//! whose declared type is a struct carrying an enum member is therefore skipped:
//! the verbatim-copied struct is never re-validated, so a narrowing release that
//! drops the live label strands it in committed target state.
//!
//! §5.9 makes `$enum` a CLOSED label set; §20.1 requires the complete prospective
//! target to be "checked under ordinary keys, refs, uniqueness, checks ... before
//! the package update commits"; §22.1 lists "field and shape types" among the
//! state constraints that hold in EVERY committed state. The dropped ordinal is
//! out of range for the target's 2-label set, so B.1 ordering/equality become
//! undefined and the value is unreadable under the target shape — the very
//! corruption 80fac2c's message calls out, reachable one struct layer down.
//!
//! Expected (spec-correct): the migration is REJECTED and 1.0.0 stays active.
//! Actual (bug): the migration COMMITS, stranding `archived` inside the struct.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

/// A struct field `profile` carries an enum member `status` in {draft, active,
/// archived}. The live row `t1` holds `archived` inside that struct.
const V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.structenum@1.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "profile": {
        "status": { "$enum": ["draft", "active", "archived"] },
        "note": "text"
      }
    },
    "all": { "$view": ".things { id, status: .profile.status }" }
  },
  "$data": { "things": { "t1": { "profile": { "status": "archived", "note": "n" } } } }
}"#;

/// Major release narrowing the struct-nested `status` enum: `archived` is dropped.
const V2_NARROWED: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.structenum@2.0.0",
  "$model": {
    "things": {
      "$key": "id",
      "id": "text",
      "profile": {
        "status": { "$enum": ["draft", "active"] },
        "note": "text"
      }
    },
    "all": { "$view": ".things { id, status: .profile.status }" }
  }
}"#;

#[test]
fn struct_nested_enum_narrowing_migration_rejects_out_of_domain_value() {
    let mut engine = load("mig-structenum", V1);
    let mut generator = generator();

    let result = engine.update(V2_NARROWED, &mut generator);

    // §20.1/§22.1/§5.9: the live label `archived` inside `profile` is outside the
    // target enum's closed set, so the prospective target violates the enum field
    // type and the migration MUST be rejected (1.0.0 stays active).
    match result {
        Err(UpdateError::Rejected(_)) => {} // spec-correct
        Err(other) => panic!("expected a §20.1 migration rejection, got a different error: {other}"),
        Ok(report) => {
            // The out-of-domain label (ordinal 2 in a 2-label target set) makes the
            // committed state unreadable: materializing the view over the stranded
            // enum errors. Probe it WITHOUT unwrapping, so the diagnostic is clean.
            let read_back = match engine.view_at_head("all") {
                Ok(Some(view)) => format!("readable, profile.status={:?}", view.rows()[0].field("status")),
                Ok(None) => "view not declared".to_owned(),
                Err(error) => format!("UNREADABLE committed state: {error}"),
            };
            panic!(
                "BUG (§5.9/§22.1/§20.1): the struct-nested enum-narrowing migration committed \
                 ({report:?}) instead of rejecting. `archived` is not a declared label of the target \
                 enum {{draft, active}}, yet the compatible copy of the `profile` struct carried it \
                 through unchecked (rules::enum_of does not unwrap Type::Struct). Resulting state: {read_back}."
            );
        }
    }

    // The rejected migration leaves the v1 state intact: t1 still reads `archived`.
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(
        view.rows()[0].field("status").map(Value::to_wire),
        Some(serde_json::json!("archived")),
        "the rejected migration must leave 1.0.0 active with t1 unchanged",
    );
}
