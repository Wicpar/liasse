#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.4 prepared updates: an update computed in full but not applied.
//!
//! The two properties that make a dry run worth trusting:
//!
//! 1. a dry run of an update that would be REJECTED reports exactly the rejection
//!    the effecting update reports — same variant, same rendered diagnostic;
//! 2. a dry run of a VALID update leaves the instance byte-identical — the export
//!    of the whole boundary before and after is the same bytes, so nothing was
//!    applied.
//!
//! Plus the staleness rule (§20.4): a plan whose basis has moved is refused by
//! `apply_update`, and refusing it commits nothing.

mod support;

use liasse_runtime::{
    CallRequest, ConflictCoordinate, ConflictKind, Engine, PreparedUpdate, UpdateError,
    UpdateRelation, Value,
};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

const PEOPLE_V1: &str = r#"{
  "$liasse": 1
  "$app": "example.dryrun@1.0.0"
  "$model": {
    "people": { "$key": "id", "id": "text", "name": "text" }
    "all_people": { "$view": ".people { id, name }" }
  }
  "$data": { "people": { "p1": { "name": "Ada" } } }
}"#;

/// A compatible minor release adding a defaulted `tier` — it commits cleanly.
const PEOPLE_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "example.dryrun@1.1.0"
  "$model": {
    "people": { "$key": "id", "id": "text", "name": "text", "tier": "int = 3" }
    "all_people": { "$view": ".people { id, name, tier }" }
  }
}"#;

/// A major release whose `$check` the live value "Ada" fails, so the whole
/// migration is rejected by the §20.1 final admission suite.
const PEOPLE_V2_REJECTED: &str = r#"{
  "$liasse": 1
  "$app": "example.dryrun@2.0.0"
  "$model": {
    "people": {
      "$key": "id"
      "id": "text"
      "name": { "$type": "text", "$check": ["size(.) > 20", "name must be long"] }
    }
    "all_people": { "$view": ".people { id, name }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The `name` of person `p1` as the instance currently holds it.
fn live_name(engine: &Engine<MemoryStore>) -> Value {
    let view = engine.view_at_head("all_people").expect("view").expect("declared");
    view.rows()[0].field("name").cloned().expect("name")
}

/// Property 2: a dry run of a VALID update applies nothing.
///
/// The exported boundary — active definition, committed state, and history index
/// — is compared byte for byte across the prepare. Anything the prepare had
/// written would move those bytes.
#[test]
fn dry_run_of_a_valid_update_leaves_the_instance_byte_identical() {
    let engine = load("dryrun", PEOPLE_V1);
    let mut generator = generator();

    let before = engine.export().expect("export before");
    let basis_before = engine.update_basis().expect("basis before");

    // `prepare_update` takes `&self`: preparing cannot mutate the instance, and
    // this binding is the whole dry run — the plan is dropped at the end of scope.
    let prepared: PreparedUpdate = engine.prepare_update(PEOPLE_V1_1, &mut generator).expect("plan computes");
    assert_eq!(prepared.relation(), UpdateRelation::Minor, "1.0.0 -> 1.1.0 is a minor update");
    assert_eq!(prepared.basis(), &basis_before, "the plan names the position it was computed against");
    assert_eq!(prepared.target().version.minor, 1, "the plan names the target it would adopt");
    assert!(prepared.reconciliation().is_clean(), "no `$bundle` divergence in this update");
    assert_eq!(prepared.reconciliation().merged.len(), 1, "the proposed result is the one migrated row");
    drop(prepared);

    let after = engine.export().expect("export after");
    assert_eq!(before, after, "a dry run applies nothing: the exported boundary is byte-identical");
    assert_eq!(
        engine.update_basis().expect("basis after"),
        basis_before,
        "the instance is still at the same commit, clock, and package version"
    );
    assert_eq!(
        engine.model().header().identity.version.minor,
        0,
        "the prior package is still the active one"
    );
}

/// Property 1: a dry run of an update that would be REJECTED reports exactly the
/// rejection the effecting update reports.
///
/// Both messages come from the same `prepare_update` call, which is the point —
/// but the test drives them through the two public entry points so a future
/// divergence between them would fail here.
#[test]
fn dry_run_reports_exactly_the_rejection_the_real_update_produces() {
    let mut engine = load("dryrun", PEOPLE_V1);
    let mut generator = generator();

    let dry = engine.prepare_update(PEOPLE_V2_REJECTED, &mut generator).expect_err("rejected");
    let real = engine.update(PEOPLE_V2_REJECTED, &mut generator).expect_err("rejected");

    assert!(matches!(dry, UpdateError::Rejected(_)), "the dry run rejects: {dry}");
    assert!(matches!(real, UpdateError::Rejected(_)), "the update rejects: {real}");
    assert_eq!(dry.to_string(), real.to_string(), "the dry run reports the update's own diagnostic");
    assert_eq!(
        engine.model().header().identity.version.major,
        1,
        "a rejected update leaves the prior package active (§20.3)"
    );
}

/// A prepared plan that IS applied commits exactly the update `Engine::update`
/// commits — same relation, same resulting state — because `update` is these two
/// steps.
#[test]
fn preparing_then_applying_is_the_update() {
    let mut prepared_engine = load("dryrun", PEOPLE_V1);
    let mut direct_engine = load("dryrun", PEOPLE_V1);
    let mut generator = generator();

    let plan = prepared_engine.prepare_update(PEOPLE_V1_1, &mut generator).expect("plan computes");
    let via_plan = prepared_engine.apply_update(plan).expect("apply commits");
    let direct = direct_engine.update(PEOPLE_V1_1, &mut generator).expect("update commits");

    assert_eq!(via_plan, direct, "prepare-then-apply reports the same update as `update`");
    assert_eq!(
        prepared_engine.export().expect("export"),
        direct_engine.export().expect("export"),
        "and lands the same committed boundary"
    );
}

/// §20.4 staleness: a commit landing between prepare and apply invalidates the
/// plan, and applying it is refused loudly with nothing committed.
#[test]
fn applying_a_plan_whose_basis_moved_is_refused_and_commits_nothing() {
    let mut engine = load("dryrun", PEOPLE_V1);
    let mut generator = generator();

    let plan = engine.prepare_update(PEOPLE_V1_1, &mut generator).expect("plan computes");
    let planned_basis = plan.basis().clone();

    // A concurrent movement: the clock advances, so `now()` no longer resolves to
    // the instant the plan's rows were computed at (§14, §22.5).
    engine.advance(1_000);
    let moved_basis = engine.update_basis().expect("basis");
    assert_ne!(moved_basis, planned_basis, "the basis really did move");

    let refused = engine.apply_update(plan).expect_err("a stale plan must not commit");
    match &refused {
        UpdateError::Stale { prepared, current } => {
            assert_eq!(**prepared, planned_basis);
            assert_eq!(**current, moved_basis);
        }
        other => panic!("expected a stale-plan refusal, got {other}"),
    }
    assert!(refused.to_string().contains("stale prepared update"), "the refusal is loud: {refused}");
    assert_eq!(
        engine.model().header().identity.version.minor,
        0,
        "the refused apply committed nothing: the prior package is still active"
    );

    // Re-preparing against the moved position produces a plan that does apply.
    let fresh = engine.prepare_update(PEOPLE_V1_1, &mut generator).expect("re-prepare");
    engine.apply_update(fresh).expect("the re-prepared plan applies");
    assert_eq!(engine.model().header().identity.version.minor, 1);
}

/// A plan prepared for one instance never applies to another (§20.4 basis).
#[test]
fn a_plan_never_crosses_instances() {
    let engine_a = load("dryrun-a", PEOPLE_V1);
    let mut engine_b = load("dryrun-b", PEOPLE_V1);
    let mut generator = generator();

    let plan = engine_a.prepare_update(PEOPLE_V1_1, &mut generator).expect("plan computes");
    let refused = engine_b.apply_update(plan).expect_err("another instance's plan must not apply");
    assert!(matches!(refused, UpdateError::Stale { .. }), "{refused}");
    assert_eq!(engine_b.model().header().identity.version.minor, 0);
}

const BUNDLED_V1: &str = r#"{
  "$liasse": 1
  "$app": "example.bundle@1.0.0"
  "$model": {
    "settings": {
      "$key": "id"
      "id": "text"
      "value": "text"
    }
    "all_settings": { "$view": ".settings { id, value, $sort: [id] }" }
    "$mut": { "set": ".settings[@id] { value = @to }" }
  }
  "$bundle": { "settings": { "theme": { "value": "dark" }, "locale": { "value": "fr" } } }
}"#;

/// The release moves BOTH bundled values; the instance has locally moved only
/// `theme`.
const BUNDLED_V1_1: &str = r#"{
  "$liasse": 1
  "$app": "example.bundle@1.1.0"
  "$model": {
    "settings": {
      "$key": "id"
      "id": "text"
      "value": "text"
    }
    "all_settings": { "$view": ".settings { id, value, $sort: [id] }" }
    "$mut": { "set": ".settings[@id] { value = @to }" }
  }
  "$bundle": { "settings": { "theme": { "value": "blue" }, "locale": { "value": "en" } } }
}"#;

/// Set the setting `id` to `to` through the package's own mutation.
fn set_setting(engine: &mut Engine<MemoryStore>, id: &str, to: &str) {
    let mut generator = generator();
    let request = CallRequest::new("set").arg("id", text(id)).arg("to", text(to));
    engine.call(&request, &mut generator).expect("the local edit commits");
}

/// §13.13/§20.4: the plan reports exactly the bundled coordinates the release and
/// the instance both moved, and the update it models resolves each the way §13.13
/// says — the local value is retained, the untouched one takes the new release's.
#[test]
fn a_plan_reports_the_bundled_values_a_local_edit_will_override() {
    let mut engine = load("bundle", BUNDLED_V1);
    let mut generator = generator();
    set_setting(&mut engine, "theme", "light");

    let plan = engine.prepare_update(BUNDLED_V1_1, &mut generator).expect("plan computes");
    let conflicts = &plan.reconciliation().conflicts;
    assert_eq!(conflicts.len(), 1, "only the locally-edited coordinate diverges: {conflicts:?}");
    assert_eq!(conflicts[0].kind, ConflictKind::IncompatibleValue);
    assert_eq!(
        conflicts[0].coordinate,
        ConflictCoordinate::Row {
            collection: "settings".to_owned(),
            key: text("theme"),
            field: Some("value".to_owned()),
        },
        "reported at the §D.3 coordinate a host would correct"
    );

    engine.apply_update(plan).expect("the update commits");
    let view = engine.view_at_head("all_settings").expect("view").expect("declared");
    let value = |id: &str| {
        view.rows()
            .iter()
            .find(|row| row.field("id") == Some(&text(id)))
            .and_then(|row| row.field("value").cloned())
    };
    assert_eq!(value("theme"), Some(text("light")), "§13.13 retains the local edit the plan flagged");
    assert_eq!(value("locale"), Some(text("en")), "and takes the release's value where no edit diverged");
}

/// A dry run of the bundled update is likewise inert: the local edit, the old
/// bundled value, and the active package all survive it untouched.
#[test]
fn dry_run_of_a_bundled_update_changes_no_bundled_value() {
    let mut engine = load("bundle", BUNDLED_V1);
    let mut generator = generator();
    set_setting(&mut engine, "theme", "light");

    let before = engine.export().expect("export before");
    drop(engine.prepare_update(BUNDLED_V1_1, &mut generator).expect("plan computes"));
    assert_eq!(engine.export().expect("export after"), before, "the dry run applied nothing");
    assert_eq!(live_bundled(&engine, "locale"), text("fr"), "the old bundled value is still in force");
}

/// The `value` of the setting `id` as the instance currently holds it.
fn live_bundled(engine: &Engine<MemoryStore>, id: &str) -> Value {
    let view = engine.view_at_head("all_settings").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("id") == Some(&text(id)))
        .and_then(|row| row.field("value").cloned())
        .expect("setting")
}

/// A dry run over a rejected update leaves the live value the rejected migration
/// would have replaced exactly as it was.
#[test]
fn a_rejected_dry_run_leaves_live_state_untouched() {
    let engine = load("dryrun", PEOPLE_V1);
    let mut generator = generator();
    let before = live_name(&engine);
    engine.prepare_update(PEOPLE_V2_REJECTED, &mut generator).expect_err("rejected");
    assert_eq!(live_name(&engine), before);
}
