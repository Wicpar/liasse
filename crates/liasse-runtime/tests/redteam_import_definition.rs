#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §19.8/§19.5/§20.2 regression: an import movement (rollback / fast-forward)
//! restores the DEFINITION active at the selected point together with its state.
//!
//! A point captured before or after a migration carries a different shape than
//! the currently active model. Before the fix, [`Engine::import`] discarded the
//! artifact's definition and reinstalled the captured bytes under the still-active
//! (post-migration) model, so a rollback across a migration silently lost the
//! point's values and left the instance incoherent (v1 data under a v2 schema).
//! §19.8 requires a movement to restore the selected point; §19.5 has the artifact
//! carry the definition active at that point; §20.2 keeps the prior values
//! available. The movement must therefore adopt the point's own definition.

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::{CallRequest, Engine, ImportRelation, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// v1: `items { id, name }`, exposed by the `all` view, with an `add` mutation.
const ITEMS_V1: &str = r#"{
  "$liasse": 1,
  "$app": "example.hm@1.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "name": "text" },
    "all": { "$view": ".items { id, name }" },
    "$mut": { "add": ".items + { id: @id, name: @name }" }
  }
}"#;

/// v2 (major): renames `name` -> `title` via `$from`, exposed by a new `all2`
/// view. The `all`/`add` v1 surfaces are dropped (permitted across a major).
const ITEMS_V2: &str = r#"{
  "$liasse": 1,
  "$app": "example.hm@2.0.0",
  "$model": {
    "items": { "$key": "id", "id": "text", "title": { "$type": "text", "$from": "name" } },
    "all2": { "$view": ".items { id, title }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn add_item(engine: &mut Engine<MemoryStore>, id: &str, name: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("name", text(name));
    engine.call(&request, &mut generator).expect("add commits");
}

#[test]
fn rollback_across_migration_restores_point_definition_and_value() {
    let mut engine = load("hm-rb", ITEMS_V1);
    add_item(&mut engine, "i1", "orig");

    // Capture the v1 point: its artifact carries the v1 definition + i1{name:"orig"}.
    let a1 = engine.export().expect("export v1 point");

    // Migrate to v2: name -> title. Live state is now i1{title:"orig"} under v2.
    let mut migrate_gen = generator();
    engine.update(ITEMS_V2, &mut migrate_gen).expect("migrate to v2 commits");
    let v2 = engine.view_at_head("all2").expect("view").expect("v2 view declared");
    assert_eq!(v2.rows()[0].field("title"), Some(&text("orig")), "v2 holds the migrated value");
    assert!(engine.view_at_head("all").expect("view").is_none(), "the v1 view is gone under v2");

    // Roll back to the v1 point. §19.8: the earlier point precedes local.
    assert_eq!(engine.classify(&a1).expect("classify"), ImportRelation::Rollback);
    let report = engine.import(&a1, &[ImportRelation::Rollback]).expect("rollback import");
    assert!(report.applied, "the rollback is permitted by the policy");

    // After the movement the SELECTED POINT is restored: the v1 definition is
    // active again and the prior value survives (§19.8/§19.5/§20.2).
    let restored = engine.view_at_head("all").expect("view").expect("v1 view active after rollback");
    assert_eq!(restored.len(), 1);
    assert_eq!(
        restored.rows()[0].field("name"),
        Some(&text("orig")),
        "the pre-migration value is readable at the restored point (§20.2)"
    );
    assert_eq!(restored.rows()[0].field("title"), None, "the restored row has the v1 shape");
    assert!(
        engine.view_at_head("all2").expect("view").is_none(),
        "the v2 view is no longer active after the rollback"
    );

    // The rolled-back instance re-exports as a coherent v1 artifact (§19.10): a
    // fresh restore of it reproduces the v1 shape and value.
    let reexport = engine.export().expect("re-export after rollback");
    let mut restore_gen = generator();
    let store = MemoryStore::new(InstanceId::new("hm-rb2"));
    let round = Engine::restore(store, &reexport, &mut restore_gen).expect("restore re-export");
    let round_view = round.view_at_head("all").expect("view").expect("v1 view in re-export");
    assert_eq!(round_view.rows()[0].field("name"), Some(&text("orig")));
    assert!(round.view_at_head("all2").expect("view").is_none());
}

#[test]
fn fast_forward_across_migration_adopts_incoming_definition_and_value() {
    // Base advances v1 -> (migrate) v2; a follower restored at the v1 point
    // fast-forwards onto the v2 continuation and must adopt v2's definition.
    let mut base = load("hm-ff", ITEMS_V1);
    add_item(&mut base, "i1", "orig");
    let early = base.export().expect("export v1 point");

    let mut migrate_gen = generator();
    base.update(ITEMS_V2, &mut migrate_gen).expect("migrate base to v2");
    let ahead = base.export().expect("export v2 point");

    // A follower restored at the earlier v1 point sees the v1 shape.
    let mut restore_gen = generator();
    let store = MemoryStore::new(InstanceId::new("hm-ff"));
    let mut follower = Engine::restore(store, &early, &mut restore_gen).expect("restore v1 point");
    assert_eq!(
        follower.view_at_head("all").expect("view").expect("v1 view").rows()[0].field("name"),
        Some(&text("orig")),
    );

    // The v2 continuation is ahead on the same lineage -> fast-forward available.
    assert_eq!(follower.classify(&ahead).expect("classify"), ImportRelation::FastForward);
    let report = follower.import(&ahead, &[ImportRelation::FastForward]).expect("fast-forward import");
    assert!(report.applied, "the continuation fast-forwards");

    // The follower adopts the incoming v2 definition and value.
    let v2 = follower.view_at_head("all2").expect("view").expect("v2 view active after fast-forward");
    assert_eq!(v2.len(), 1);
    assert_eq!(v2.rows()[0].field("title"), Some(&text("orig")), "the migrated value carried forward");
    assert!(follower.view_at_head("all").expect("view").is_none(), "the v1 view is gone after fast-forward");
}
