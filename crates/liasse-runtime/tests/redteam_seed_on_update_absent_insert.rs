#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.13/§4.1 red-team: on update, a `$seed` (`$data`) row at an ABSENT address
//! MUST be inserted. This probes the OTHER half of the rule the corpus pin
//! `target-seed-data-on-update-retained` only exercises the PRESENT half of.
//!
//! # What the spec requires (§13.13)
//!
//! "On update of any package instance — the root application and module instances
//! alike ... A seed value applies only where its address holds no current value:
//! a row newly present in the new seed is inserted when no row exists at that key
//! ... The update report lists added, updated, removed, and locally retained paths
//! (`$seeded`, §13.15)."
//!
//! So §13.13 has two directions on update:
//!   * PRESENT address -> the existing row is never modified (retain local);
//!   * ABSENT address  -> a newly-present seed row is INSERTED.
//!
//! The corpus pin `target-seed-data-on-update-retained` (a `common` case, not in
//! the SKIP-ledger) covers ONLY the PRESENT direction: it seeds items/a="old" at
//! 1.0.0, re-declares `$data` items/a="new" at 1.1.0, and asserts the live value
//! stays "old". That outcome holds even if the entire seed-on-update reconcile is
//! a total no-op — "apply-if-absent" over an occupied address changes nothing.
//!
//! # The probe
//!
//! Version 1.0.0 seeds items/a. Version 1.1.0 (a compatible minor, shape-identical)
//! declares `$data` for a NEW row items/b that never existed. Per §13.13 the update
//! MUST insert items/b (its address is absent). If items/b is missing after the
//! update, the ABSENT->insert direction is not applied.
//!
//! # Root cause
//!
//! `Engine::update` (crates/liasse-runtime/src/migrate.rs) builds migrated state
//! from the §20.1 compatible copy + `$from`/`$as` + the `$migrations` program only.
//! It never reads the target's `$seed`/`$data` and `UpdateReport` carries no
//! `$seeded` list (§13.15). The surface host `update` (host/update.rs) merely
//! delegates to `Engine::update`, so seed-on-update reconcile is unwired on the
//! whole update path.
//!
//! NOTE: this is the ABSENT->insert direction, distinct from the seed
//! reload-vs-diverged path tracked separately. It is surfaced here because the
//! PRESENT-direction pin passes ONLY incidentally, so the normative ABSENT->insert
//! rule currently has no covering test and does not fire.

mod support;

use liasse_runtime::Value;
use support::{generator, load};

/// 1.0.0 seeds items/a. `all` exposes the live rows.
const SEED_V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.seed.absent@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "value": "text" },
    "all": { "$view": ".items { id, value }" }
  },
  "$data": { "items": { "a": { "value": "old" } } }
}"#;

/// 1.1.0 is a shape-identical compatible minor whose `$data` introduces a NEW row
/// items/b at an address the instance has never held. §13.13: an absent seed
/// address is inserted on update.
const SEED_V1_1_ADDS_B: &str = r#"{
  "$liasse": 1,
  "$app": "t.seed.absent@1.1.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "value": "text" },
    "all": { "$view": ".items { id, value }" }
  },
  "$data": { "items": { "b": { "value": "fresh" } } }
}"#;

#[test]
fn seed_row_at_absent_address_is_inserted_on_update() {
    let mut engine = load("seed-absent", SEED_V1);
    let mut generator = generator();

    engine.update(SEED_V1_1_ADDS_B, &mut generator).expect("compatible minor update commits");

    let view = engine.view_at_head("all").expect("view").expect("declared");
    let b = view
        .rows()
        .iter()
        .find(|row| row.field("id").map(Value::to_wire) == Some(serde_json::json!("b")));

    assert!(
        b.is_some(),
        "BUG (§13.13/§4.1): the 1.1.0 `$data` seed row items/b is at an ABSENT address and MUST be \
         inserted on update, but it is missing after the update — live rows are {:?}. \
         `Engine::update` never applies the target's `$seed`/`$data` (no `$seeded` in \
         UpdateReport), so the ABSENT->insert direction of §13.13 does not fire. The pin \
         `target-seed-data-on-update-retained` passes only because its address was already \
         occupied (a no-op for apply-if-absent).",
        view.rows().iter().map(|r| r.field("id").map(Value::to_wire)).collect::<Vec<_>>(),
    );

    // If insertion did happen, the freshly seeded value must be present.
    assert_eq!(
        b.and_then(|row| row.field("value")).map(Value::to_wire),
        Some(serde_json::json!("fresh")),
        "the inserted seed row must carry its seeded value",
    );
}

/// PASSING control: the PRESENT direction — the pinned behaviour. Seeding a value
/// at an already-occupied address on update must NOT overwrite the live value.
/// This holds whether or not seed-on-update is wired, so it is a control, not a
/// bug probe.
#[test]
fn seed_row_at_present_address_is_retained_on_update() {
    let mut engine = load("seed-present", SEED_V1);
    let mut generator = generator();

    // 1.1.0 re-declares items/a with a different value; the address is occupied.
    let target = SEED_V1
        .replace("@1.0.0", "@1.1.0")
        .replace(r#""value": "old""#, r#""value": "new""#);
    engine.update(&target, &mut generator).expect("compatible minor update commits");

    let view = engine.view_at_head("all").expect("view").expect("declared");
    let a = view
        .rows()
        .iter()
        .find(|row| row.field("id").map(Value::to_wire) == Some(serde_json::json!("a")))
        .expect("items/a present");
    assert_eq!(
        a.field("value").map(Value::to_wire),
        Some(serde_json::json!("old")),
        "§13.13: once user data is present at an address, a later seed change does not overwrite it",
    );
}
