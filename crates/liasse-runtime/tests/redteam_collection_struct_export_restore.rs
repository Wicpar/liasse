#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM (Target 3 — §19.10 round-trip closure of the §20.1 migration fix,
//! commit 622eca8): a committed state carrying a §5.3 STATIC STRUCT member of a
//! keyed collection EXPORTS successfully but CANNOT BE RESTORED — the produced
//! `.liasse` artifact is un-restorable, violating §19.10.
//!
//! # DEFECT
//!
//! §19.10: "Restoring an artifact and exporting the same instance boundary …
//! reproduces the same definitions, resources, **owned logical states**, …". A
//! keyed-collection static struct member (§5.3, `"pt": { "b": "int", "a": "int" }`)
//! is owned logical state. It admits, views, and — after commit 622eca8 —
//! MIGRATES: `migrate.rs::coerce_and_require` re-validates each struct member of a
//! migrated row (`collection.structs`, crates/liasse-runtime/src/migrate.rs:526)
//! through `struct_ty.decode(value.to_wire())` and COMMITS it. Commit 622eca8's
//! stated invariant is that "the committable states are exactly those the §19
//! codec can round-trip". That invariant is FALSE for a collection static struct:
//! the §19 portable codec drops it.
//!
//! Root cause: the state-section decode type omits static struct members.
//! `StateSection::to_bytes` serializes EVERY field of a row, struct members
//! included (`materialize::struct_of(fields)`, crates/liasse-runtime/src/
//! portable.rs:87-93). But the decode type `StateSection::row_type`
//! (crates/liasse-runtime/src/portable.rs:174-180) is built ONLY from
//! `collection.fields` — a static struct member compiles into `collection.structs`,
//! not `collection.fields` (crates/liasse-runtime/src/compiled.rs:646-649), so it
//! is absent from the decode struct type. `Type::Struct::decode` then rejects the
//! serialized `pt` as an UNEXPECTED member, so `Engine::restore` errors
//! ("state row in `<c>`: unexpected member `pt`") and nothing is instantiated.
//!
//! The §8.2 SINGLETON path does this correctly: `singleton::row_type` →
//! `optional_struct_type` iterates ALL root members including `Node::Struct`
//! (crates/liasse-runtime/src/singleton.rs:77 + `decodable_member_type`), so a
//! singleton static struct round-trips (proved by `control_singleton_static_struct_
//! round_trips` below, and by the landed `redteam_singleton_roundtrip_families`).
//! Only the keyed-COLLECTION path drops structs — an asymmetry no test covered
//! (`redteam_export_restore_root_singleton` deliberately uses a scalar-only `notes`
//! collection as its control, never a collection static struct).
//!
//! # WHY IT IS A CONCRETE VIOLATION
//!
//! Nothing here is malformed: every model loads, every value admits, every view
//! reads. `export()` returns Ok — the engine claims a valid boundary artifact —
//! yet `restore()` of that very artifact FAILS. An export that cannot be restored
//! is a §19.10 violation regardless of whether restore rejects or silently drops.
//! Reachable two independent ways, each a `#[test]` below:
//!   * `collection_static_struct_export_cannot_restore` — plain load + seed, no
//!     migration: the general §19.5/§19.10 codec gap.
//!   * `migration_commits_collection_struct_then_fails_restore` — the Target-3
//!     framing: a migrated boundary commits (coerce_and_require validates the
//!     struct) but does not round-trip, falsifying 622eca8's own invariant.
//!
//! # SEVERITY: HIGH
//!
//! An entire class of valid, loadable models (any keyed collection with a §5.3
//! static struct member) produces exports that cannot be restored — the instance
//! cannot be backed up, moved across points (§19.8 import/rollback), or
//! reconciled (§19.9). Silent at export time; a hard failure at restore time.
//!
//! # ISOLATION (passing controls in this file)
//!
//!   * `control_singleton_static_struct_round_trips` — a struct at the SINGLETON
//!     level round-trips, so the restore machinery works; the defect is the
//!     collection decode type.
//!   * `control_scalar_only_collection_round_trips` — a scalar-only collection
//!     round-trips, so the defect is the struct member specifically, not
//!     collections in general.
//!
//! All expectations are re-derived from SPEC.md text alone (§19.10 owned logical
//! state, §19.5 state section, §5.3 static struct, §20.1 migration meters/checks,
//! §8.2 singleton). At HEAD `84de2c0` the two RED tests FAIL and the two controls
//! PASS.

mod support;

use liasse_runtime::{Engine, UpdateError, Value, ViewQuery};
use liasse_store::MemoryStore;
use support::{generator, store};

/// Read the first row of a projection view.
fn first_row_fields(engine: &Engine<MemoryStore>, view: &str) -> Option<liasse_runtime::ViewRow> {
    engine
        .view_with(view, engine.head().unwrap(), &ViewQuery::new())
        .expect("view ok")
        .and_then(|v| v.rows().first().cloned())
}

fn restore_result(engine: &Engine<MemoryStore>, tag: &str) -> Result<Engine<MemoryStore>, String> {
    let artifact = engine.export().expect("§19.5 export of a committed boundary must succeed");
    Engine::restore(store(tag), &artifact, &mut generator()).map_err(|e| e.to_string())
}

// A keyed collection whose row carries a §5.3 static struct member `pt`.
const V1_COLLECTION_STRUCT: &str = r#"{
  "$liasse": 1,
  "$app": "t.colstruct@1.0.0",
  "$model": {
    "configs": { "$key": "id", "id": "text", "pt": { "b": "int", "a": "int" } },
    "$public": { "all": { "$view": ".configs { id, pt }" } }
  },
  "$data": { "configs": { "p1": { "pt": { "b": 1, "a": 2 } } } }
}"#;

/// §19.10/§19.5/§5.3: a committed keyed-collection static struct member exports
/// but the artifact does not restore. The export claims success; the restore of
/// that artifact fails, so the exported boundary does not reproduce its own owned
/// logical state.
#[test]
fn collection_static_struct_export_cannot_restore() {
    let engine = Engine::load(store("colstruct"), V1_COLLECTION_STRUCT, &mut generator()).expect("v1 loads");
    // Precondition: the struct member is live, owned logical state.
    let row = first_row_fields(&engine, "public.all").expect("the seeded row is present");
    assert!(matches!(row.field("pt"), Some(Value::Struct(_))), "precondition: `pt` is a committed static struct");

    let restored = restore_result(&engine, "colstruct");
    assert!(
        restored.is_ok(),
        "§19.10/§19.5/§5.3: export of a keyed collection carrying a static struct member SUCCEEDS but the \
         artifact CANNOT be restored ({}). `StateSection::to_bytes` serializes the struct member (portable.rs:87), \
         but `StateSection::row_type` builds the decode type from `collection.fields` only (portable.rs:174), \
         omitting `collection.structs`, so `Type::Struct::decode` rejects the serialized member as unexpected. \
         An exported boundary that cannot be restored violates §19.10.",
        restored.err().unwrap_or_default(),
    );
}

// The same collection, a MAJOR-bump migration adding an unrelated scalar. The
// migration copies the static struct forward and (per 622eca8) re-validates it.
const V2_COLLECTION_STRUCT: &str = r#"{
  "$liasse": 1,
  "$app": "t.colstruct@2.0.0",
  "$model": {
    "configs": { "$key": "id", "id": "text", "pt": { "b": "int", "a": "int" }, "note": "text = 'x'" },
    "$public": { "all": { "$view": ".configs { id, pt, note }" } }
  }
}"#;

/// §20.1/§19.10: the Target-3 framing. A §20.1 migration COMMITS a state carrying
/// a collection static struct (commit 622eca8's `coerce_and_require` re-validates
/// `collection.structs` and admits them), but that committed boundary does NOT
/// round-trip through export/restore — directly falsifying the fix's stated
/// invariant that committable states are exactly those the §19 codec round-trips.
#[test]
fn migration_commits_collection_struct_then_fails_restore() {
    let mut engine = Engine::load(store("colstruct-mig"), V1_COLLECTION_STRUCT, &mut generator()).expect("v1 loads");

    match engine.update(V2_COLLECTION_STRUCT, &mut generator()) {
        // Spec-acceptable outcome: the migration refuses to commit a state its own
        // §20.1 "checked under … before the update commits" pipeline cannot later
        // round-trip. Then there is nothing un-restorable — the defect did not occur.
        Err(UpdateError::Rejected(_)) => return,
        Ok(_) => {}
        Err(other) => panic!("migration failed unexpectedly: {other}"),
    }

    // The migration committed. The struct member is in committed state.
    let row = first_row_fields(&engine, "public.all").expect("migrated row present");
    assert!(matches!(row.field("pt"), Some(Value::Struct(_))), "the migration carried the static struct forward");
    assert!(matches!(row.field("note"), Some(Value::Text(_))), "the migration added `note`");

    let restored = restore_result(&engine, "colstruct-mig");
    assert!(
        restored.is_ok(),
        "§20.1/§19.10: the §20.1 migration COMMITTED a keyed-collection static struct (622eca8's \
         `coerce_and_require` validated `collection.structs`, migrate.rs:526), but the migrated boundary \
         does NOT round-trip: export succeeds, restore FAILS ({}). Commit 622eca8's invariant — committable \
         states are exactly those the §19 codec round-trips — is false for a collection static struct.",
        restored.err().unwrap_or_default(),
    );
}

// CONTROL 1 (must pass): a §8.2 singleton static struct round-trips — the
// singleton decode type includes struct members (singleton.rs:77). Isolates the
// defect to the keyed-COLLECTION decode type.
const S_SINGLETON_STRUCT: &str = r#"{
  "$liasse": 1,
  "$app": "t.singstruct@1.0.0",
  "$model": {
    "cfg": { "b": "int", "a": "int" },
    "notes": { "$key": "id", "id": "text" },
    "$public": { "readout": { "$view": ". { a: .cfg.a, b: .cfg.b }" } }
  },
  "$data": { "cfg": { "b": 7, "a": 9 } }
}"#;

#[test]
fn control_singleton_static_struct_round_trips() {
    let engine = Engine::load(store("singstruct"), S_SINGLETON_STRUCT, &mut generator()).expect("loads");
    let before = first_row_fields(&engine, "public.readout").expect("readout row");
    let a_before = before.field("a").cloned();
    assert!(a_before.is_some(), "precondition: singleton struct member `cfg.a` reads");

    let restored = restore_result(&engine, "singstruct").expect("§19.10: singleton struct MUST round-trip");
    let after = first_row_fields(&restored, "public.readout").expect("restored readout row");
    assert_eq!(after.field("a").cloned(), a_before, "§19.10: the singleton struct member survives restore");
}

// CONTROL 2 (must pass): a scalar-only keyed collection round-trips — the defect
// is the static struct member specifically, not keyed collections in general.
const C_SCALAR_ONLY: &str = r#"{
  "$liasse": 1,
  "$app": "t.scalarcol@1.0.0",
  "$model": {
    "configs": { "$key": "id", "id": "text", "b": "int", "a": "int" },
    "$public": { "all": { "$view": ".configs { id, a, b }" } }
  },
  "$data": { "configs": { "p1": { "b": 1, "a": 2 } } }
}"#;

#[test]
fn control_scalar_only_collection_round_trips() {
    let engine = Engine::load(store("scalarcol"), C_SCALAR_ONLY, &mut generator()).expect("loads");
    let restored = restore_result(&engine, "scalarcol").expect("§19.10: a scalar-only collection MUST round-trip");
    let row = first_row_fields(&restored, "public.all").expect("restored row present");
    assert_eq!(row.field("a").cloned(), Some(Value::Int(liasse_value::Integer::parse("2").unwrap())), "value survives");
}
