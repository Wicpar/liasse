#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.1/§20.3/Annex E.1/E.9 red-team: an OFF-LINEAGE package that happens to be
//! shape-compatible with the active instance must be REJECTED for in-place update
//! (no connected §20.1 delta path), not silently committed.
//!
//! # What the spec requires
//!
//! §20.3: "A package is compatible for an in-place update exactly when it shares
//! the instance's application identity AND a connected §20.1 delta path exists
//! between the instance's current version and the package's version. An
//! off-lineage package of the same name ... is never migrated in place."
//!
//! §20.1 route resolution: "When no connected delta path exists between the two
//! versions — the instance's version lies strictly between declared keys with no
//! delta from it, or off the declared lineage entirely — the in-place update is
//! rejected and the active package remains active (§9.4, Annex E.9)." The runtime
//! "MUST NOT synthesize an undeclared intermediate version." The single implicit
//! (structural-diff) delta is available only "between two versions with no declared
//! `$migrations` key strictly between them, and where both endpoint models are
//! held."
//!
//! # The scenario (a shape-compatible twin of the pinned off-lineage case)
//!
//! Active source is 1.0.0. The target is 3.0.0 and declares its chain with a single
//! `$migrations` key "2.0.0" (the delta 2.0.0 -> 3.0.0). 1.0.0 lies strictly below
//! that declared key with NO delta from it; the implicit delta is unavailable
//! because a declared key (2.0.0) sits strictly between 1.0.0 and 3.0.0 and the
//! 2.0.0 model is not held. So 1.0.0 is OFF the declared lineage and there is no
//! connected delta path — exactly the reasoning the corpus pin
//! `sequence-composition-off-lineage-rejected` cites.
//!
//! The ONE difference from that pin: here the 3.0.0 model is SHAPE-COMPATIBLE with
//! 1.0.0 (`items { id, name }` in both), so the §20.1 compatible same-identity copy
//! populates every target field. The pin's target instead added a required `tag`
//! field with no fill, so it was rejected INCIDENTALLY as "required field
//! unpopulated" (§5.1) — never by an actual lineage check.
//!
//! This test removes that incidental rejection. If the engine COMMITS, off-lineage
//! composition is not enforced at all: `crates/liasse-runtime/src/migrate.rs`
//! `Engine::update` performs a single-hop compatible copy plus only the
//! `$migrations` program keyed to the EXACT active version (line 357) and never
//! resolves or validates a connected delta route. The pinned rejection then holds
//! only by luck of a shape mismatch.

mod support;

use liasse_runtime::{UpdateError, Value};
use support::{generator, load};

/// Active application 1.0.0, seeded with one row so a committed migration is
/// observable in state.
const ACTIVE_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.offlineage@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "name": "text" },
    "all": { "$view": ".items { id, name }" }
  },
  "$data": { "items": { "a": { "name": "seed" } } }
}"#;

/// Off-lineage target 3.0.0: the declared chain starts at key "2.0.0" (delta
/// 2.0.0 -> 3.0.0), so the active 1.0.0 is off the declared lineage. The model is
/// deliberately SHAPE-COMPATIBLE with 1.0.0 (`items { id, name }`), so the §20.1
/// compatible copy needs no fill and no incidental "unpopulated field" rejection
/// can fire — only an actual lineage/route check could reject this.
const OFFLINEAGE_V3: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.offlineage@3.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "name": "text" },
    "$migrations": {
      "2.0.0": [ ".items = $old.items { id, name }" ]
    },
    "all": { "$view": ".items { id, name }" }
  }
}"#;

#[test]
fn offlineage_shape_compatible_update_is_rejected() {
    let mut engine = load("mig-offlineage", ACTIVE_V1);
    let mut generator = generator();

    let result = engine.update(OFFLINEAGE_V3, &mut generator);

    match result {
        // §20.1/§20.3/E.9 spec-correct: no connected delta path from 1.0.0 to
        // 3.0.0 (the declared chain starts at 2.0.0 and the runtime never
        // synthesizes intermediates), so the in-place update is rejected and the
        // active package stays in force.
        Err(UpdateError::Rejected(_)) | Err(UpdateError::Incompatible(_)) => {}
        Err(UpdateError::Engine(other)) => panic!(
            "expected a §20.1 off-lineage rejection, got a load/engine error instead: {other}"
        ),
        Ok(report) => {
            let view = engine.view_at_head("all").expect("view").expect("declared");
            let name = view.rows()[0].field("name").cloned();
            panic!(
                "BUG (§20.1/§20.3/Annex E.1/E.9): an OFF-LINEAGE 3.0.0 package committed in place \
                 ({report:?}). The active 1.0.0 is not on the target's declared lineage {{2.0.0, \
                 3.0.0}} and no connected delta path exists, yet the engine ran a single-hop \
                 compatible copy and committed (post-update items/a.name = {name:?}). \
                 `Engine::update` (migrate.rs) never resolves a §20.1 route — the corpus pin \
                 `sequence-composition-off-lineage-rejected` is enforced only incidentally, by the \
                 required-field-unpopulated check, not by any lineage check."
            );
        }
    }

    // §9.4/E.9: whatever the verdict, a rejected update must leave 1.0.0 active and
    // its seeded row intact. (Only reached when the update is correctly refused.)
    let view = engine.view_at_head("all").expect("view").expect("declared");
    assert_eq!(
        view.rows()[0].field("name").map(Value::to_wire),
        Some(serde_json::json!("seed")),
        "a refused off-lineage update must leave the 1.0.0 instance untouched",
    );
}
