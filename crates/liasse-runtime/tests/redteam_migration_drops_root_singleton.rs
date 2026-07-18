#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM cross-cutting probe (Target B round 4 — migration × §8.2 singleton):
//! a §20 package update (`Engine::update`) SILENTLY DROPS all package-root
//! **singleton state** (§8.2).
//!
//! # DEFECT
//!
//! §20.1 pins the migration order as "compatible same-identity copy, local `$from`
//! mappings ..., then the selected package-level statements". A root singleton
//! field (a scalar / static-struct / ref / set declared directly under `$model`,
//! §8.2) that is present unchanged in both the active and the target model is a
//! compatible same-identity member — exactly like a keyed-collection field — so it
//! MUST be carried forward into the migrated live state. It is not.
//!
//! Root cause, two cooperating sites:
//!
//!   * `build_migrated` (crates/liasse-runtime/src/migrate.rs:254) builds the
//!     prospective migrated state by iterating `target.compiled.collections` ONLY.
//!     The §8.2 singleton reserved row (captured in `old_state.singleton`) is never
//!     read here, so it never enters the `migrated` row map.
//!   * `apply_migration` (crates/liasse-runtime/src/engine.rs:460-467) gathers the
//!     live working copy — which DOES include the singleton row at
//!     `singleton::address()` (§8.2) — then removes EVERY live address and
//!     re-inserts only the `migrated` rows. The singleton address is removed and
//!     never re-inserted, so `diff()` emits a delete for it: the migration commit
//!     wipes the durable root state from the store.
//!
//! Net effect: after ANY committed package update the root singleton fields read
//! back absent (`none`), losing both seeded and mutated singleton values. This
//! violates §20.1 (compatible same-identity copy) and §8.2 (the singleton is
//! durable owned state), and — because the wiped state is what a later export
//! captures — §19.10 (a restored/exported boundary reproduces the owned logical
//! state) transitively.
//!
//! # ISOLATION
//!
//! The migration itself commits and the CONTROL collection `notes` round-trips
//! through it (its seeded row survives and the newly added `tag` field takes its
//! default), so the migration machinery works — the loss is specific to the §8.2
//! singleton row. The expected values are externally deducible: `flag` was mutated
//! to `changed` and `company.name` seeded to `Acme`; §20.1's compatible copy keeps
//! both. Nothing here encodes implementation behavior.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, UpdateError, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

/// v1: a root carrying §8.2 singleton state (`flag` scalar, nested struct
/// `company`) ALONGSIDE a control collection `notes`. `readout` projects the
/// singleton; `all_notes` projects the collection.
const V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton@1.0.0",
  "$model": {
    "flag": "text",
    "company": { "name": "text" },
    "notes": { "$key": "id", "id": "text" },
    "readout": { "$view": ". { flag, cname: .company.name }" },
    "all_notes": { "$view": ".notes { id, $sort: [id] }" },
    "$mut": { "set_flag": ".flag = @v", "add_note": ".notes + { id: @id }" }
  },
  "$data": { "flag": "seed", "company": { "name": "Acme" } }
}"#;

/// v2 (minor forward, 1.1.0): KEEPS `flag`, `company`, and `notes` exactly, and
/// makes ONE compatible change — a new optional field `tag` on `notes` with a
/// default. The singleton members are byte-identical, so §20.1's compatible
/// same-identity copy must carry them forward; the new `tag` proves the migration
/// actually ran over the surviving `notes` row.
const V2: &str = r#"{
  "$liasse": 1,
  "$app": "t.mig.singleton@1.1.0",
  "$model": {
    "flag": "text",
    "company": { "name": "text" },
    "notes": { "$key": "id", "id": "text", "tag": "text = 'default'" },
    "readout": { "$view": ". { flag, cname: .company.name }" },
    "all_notes": { "$view": ".notes { id, tag, $sort: [id] }" },
    "$mut": { "set_flag": ".flag = @v", "add_note": ".notes + { id: @id }" }
  },
  "$data": { "flag": "seed", "company": { "name": "Acme" } }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn commit(o: CallOutcome) {
    assert!(matches!(o, CallOutcome::Committed { .. }), "expected a commit, got {o:?}");
}

/// A field of the single `readout` row (a root-singleton projection).
fn singleton(engine: &Engine<MemoryStore>, field: &str) -> Option<Value> {
    let view = engine.view_at_head("readout").expect("view ok").expect("readout declared");
    view.rows().first().and_then(|row| row.field(field).cloned())
}

/// The `(id, tag)` pairs in the `notes` collection (the migration control).
fn notes(engine: &Engine<MemoryStore>) -> Vec<(Option<Value>, Option<Value>)> {
    let view = engine.view_at_head("all_notes").expect("view ok").expect("all_notes declared");
    view.rows()
        .iter()
        .map(|row| (row.field("id").cloned(), row.field("tag").cloned()))
        .collect()
}

#[test]
fn migration_preserves_root_singleton_state() {
    let mut engine = Engine::load(store("mig-singleton"), V1, &mut generator()).expect("v1 loads");
    let mut g = generator();

    // Mutate the singleton scalar away from its seed and add the control note.
    commit(engine.call(&CallRequest::new("set_flag").arg("v", text("changed")), &mut g).expect("set_flag"));
    commit(engine.call(&CallRequest::new("add_note").arg("id", text("n1")), &mut g).expect("add_note"));

    // Pre-migration: the singleton and the collection hold what we put in.
    assert_eq!(singleton(&engine, "flag"), Some(text("changed")), "pre-migration singleton scalar");
    assert_eq!(singleton(&engine, "cname"), Some(text("Acme")), "pre-migration singleton struct member");
    assert_eq!(notes(&engine), vec![(Some(text("n1")), None)], "pre-migration note (no tag field yet)");

    // §20: a minor forward update. It commits (a new field is a compatible add).
    let report = match engine.update(V2, &mut generator()) {
        Ok(report) => report,
        Err(UpdateError::Rejected(r)) => panic!("migration unexpectedly rejected: {}", r.message()),
        Err(error) => panic!("migration unexpectedly failed: {error}"),
    };
    let _ = report;

    // CONTROL (§20.1): the `notes` collection survives the migration and the new
    // `tag` field takes its default on the surviving row — so migration works.
    assert_eq!(
        notes(&engine),
        vec![(Some(text("n1")), Some(text("default")))],
        "§20.1 control: the collection row survives and the added field defaults — migration ran",
    );

    // THE DEFECT (§20.1 compatible same-identity copy × §8.2): the root singleton
    // members are byte-identical across v1/v2, so they MUST survive the migration.
    // They are dropped: `build_migrated` (migrate.rs:254) copies only collection
    // rows, and `apply_migration` (engine.rs:460-467) removes the singleton
    // reserved row without re-inserting it.
    assert_eq!(
        singleton(&engine, "flag"),
        Some(text("changed")),
        "§20.1/§8.2: the root singleton scalar `flag` must survive the compatible same-identity \
         migration; it is dropped because `build_migrated` copies only `target.compiled.collections` \
         and `apply_migration` wipes the singleton reserved row",
    );
    assert_eq!(
        singleton(&engine, "cname"),
        Some(text("Acme")),
        "§20.1/§8.2: the root singleton struct member `company.name` must survive the migration; it \
         is dropped by the same collection-only migration staging",
    );
}

/// The §19.10 downstream consequence: an export taken AFTER the migration cannot
/// carry the singleton, because the migration already wiped it from live state.
/// Restore therefore does not reproduce the exported owned logical state. This is
/// the migration × artifact-export cross-cut (Target B) sharing the root cause
/// above; the control collection round-trips, isolating the loss to the singleton.
#[test]
fn export_after_migration_loses_root_singleton() {
    let mut engine = Engine::load(store("mig-singleton-export"), V1, &mut generator()).expect("v1 loads");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("set_flag").arg("v", text("changed")), &mut g).expect("set_flag"));
    commit(engine.call(&CallRequest::new("add_note").arg("id", text("n1")), &mut g).expect("add_note"));

    engine.update(V2, &mut generator()).expect("migration commits");

    // §19.5/§19.7: export the post-migration boundary and restore it fresh.
    let artifact = engine.export().expect("export");
    let restored =
        Engine::restore(store("mig-singleton-export"), &artifact, &mut generator()).expect("restore ok");

    // CONTROL: the collection round-trips through migration + export + restore.
    assert_eq!(
        notes(&restored),
        vec![(Some(text("n1")), Some(text("default")))],
        "§19.10 control: the migrated collection row round-trips through export/restore",
    );

    // §19.10/§20.1/§8.2: the singleton must be reproduced; it is absent because the
    // migration dropped it before the export could capture it.
    assert_eq!(
        singleton(&restored, "flag"),
        Some(text("changed")),
        "§19.10/§20.1/§8.2: a boundary exported after a migration must still reproduce the root \
         singleton scalar on restore; the migration wiped it first",
    );
}
