#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1/§22.1/§5.9 red-team: a migration that narrows a `$enum`'s closed label
//! set must not strand an out-of-domain value in committed target state.
//!
//! §5.9 makes `$enum` a CLOSED label set: an insert or assignment of an
//! undeclared label is rejected (see `enums.rs`). §20.1 requires the complete
//! prospective target to be "checked under ordinary keys, refs, uniqueness,
//! checks ... before the package update commits", and §22.1 lists "field and
//! shape types" among the state constraints that hold in EVERY committed state.
//!
//! A major release narrows `status` from {draft, active, archived} to
//! {draft, active}. The live row `t1` holds `archived`; the §20.1 compatible
//! same-identity copy carries that value into the prospective target, where it is
//! no longer a declared label of the target enum. The migration MUST therefore be
//! rejected, exactly as a migrated value failing a target `$check`
//! (`evolution.rs`) or a migrated dangling ref (`migrated-state-dangling-ref`) is.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

const V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.enum@1.0.0",
  "$model": {
    "things": { "$key": "id", "id": "text", "status": { "$enum": ["draft", "active", "archived"] } },
    "all": { "$view": ".things { id, status }" }
  },
  "$data": { "things": { "t1": { "status": "archived" } } }
}"#;

/// Major release narrowing the `status` enum: `archived` is dropped.
const V2_NARROWED: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.enum@2.0.0",
  "$model": {
    "things": { "$key": "id", "id": "text", "status": { "$enum": ["draft", "active"] } },
    "all": { "$view": ".things { id, status }" }
  }
}"#;

#[test]
fn enum_narrowing_migration_rejects_out_of_domain_value() {
    let mut engine = load("mig-enum", V1);
    let mut generator = generator();

    let result = engine.update(V2_NARROWED, &mut generator);

    // §20.1/§22.1/§5.9: the live label `archived` is outside the target enum's
    // closed set, so the prospective target violates the enum field type and the
    // migration MUST be rejected (1.0.0 stays active).
    match result {
        Err(UpdateError::Rejected(_)) => {} // spec-correct
        Err(other) => panic!("expected a §20.1 migration rejection, got a different error: {other}"),
        Ok(report) => {
            // Surface the actual stranded value so the failure is self-explaining.
            let view = engine.view_at_head("all").expect("view").expect("declared");
            let status = view.rows()[0].field("status").cloned();
            panic!(
                "BUG (§5.9/§22.1): the enum-narrowing migration committed ({report:?}) and left an \
                 out-of-domain value in committed state: status={status:?}. `archived` is not a \
                 declared label of the target enum {{draft, active}}, yet the compatible copy carried \
                 it through unchecked."
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
