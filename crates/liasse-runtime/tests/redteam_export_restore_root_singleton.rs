#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §19.10 / §8.2 round-trip — RED TEAM against commit `a0e1f36`.
//!
//! DEFECT: a `.liasse` artifact export SILENTLY DROPS all package-root **singleton
//! state** (§8.2). The export succeeds, the restore succeeds, and the restored
//! instance is missing every non-collection root field — so restore does NOT
//! reproduce the exported owned logical state.
//!
//! §8.2 root singleton state is a package root's non-collection writable members —
//! scalar fields and static (possibly nested) structs declared directly under
//! `$model`. They are durable state: seeded from `$data`, mutated by root
//! mutations, stored in one reserved row, and materialized onto the package root
//! so a view reads `.field` / `.struct.member` (see `tests/root_singletons.rs`).
//! The normal store persist/restart path keeps them — `Prospective::gather_from`
//! scans the singleton row explicitly (`state.rs`, "the package root's singleton
//! fields live in one reserved row").
//!
//! The artifact/export path does not. Root cause is in the runtime's portable
//! state codec `crates/liasse-runtime/src/portable.rs`:
//!
//!   * `StateSection::capture` (portable.rs:49-51) iterates `model.root().members`
//!     and `continue`s past every member that is not a `Node::Collection`. The
//!     §8.2 singleton reserved row is a set of `Node::Field`/struct members, never
//!     a collection, so it is never captured.
//!   * `to_bytes` / `from_bytes` / `working` / `Engine::install_state` likewise
//!     handle only the captured collections, so nothing re-materializes the
//!     singleton row on restore.
//!
//! Net effect: after export/restore the root singleton fields are absent (they
//! read `none` / are dropped), losing both seeded and mutated singleton values.
//! This violates §19.10 "Restoring an artifact and exporting the same instance
//! boundary ... reproduces the same definitions, resources, owned logical states,
//! ...".
//!
//! This is distinct from the acknowledged nested-collection CORE seam (portable.rs
//! documents nested *collections* as out of scope): a root singleton is
//! TOP-LEVEL, non-nested state, and the top-level `notes` collection here restores
//! correctly — the control assertion isolates the loss to the singleton row, not
//! the restore machinery. The value is externally deducible: `flag` was mutated to
//! `changed` and `company.name` seeded to `Acme`; both must read back identically.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

/// A package whose root carries §8.2 singleton state (`flag`, and the nested
/// struct `company`) ALONGSIDE a top-level `notes` collection. `readout` projects
/// the root singleton fields; `all_notes` projects the collection. The `notes`
/// collection is the control: it round-trips and proves the restore itself works.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.singleton.export@1.0.0",
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

/// The ids in the top-level `notes` collection (the round-trip control).
fn notes(engine: &Engine<MemoryStore>) -> Vec<Value> {
    let view = engine.view_at_head("all_notes").expect("view ok").expect("all_notes declared");
    view.rows().iter().filter_map(|row| row.field("id").cloned()).collect()
}

#[test]
fn export_restore_preserves_root_singleton_state() {
    let mut engine = Engine::load(store("singleton-export"), M, &mut generator()).expect("app loads");
    let mut g = generator();

    // Mutate the singleton scalar away from its seed (so a dropped value and a
    // re-seeded value are both distinguishable from a genuine round trip) and add
    // a collection row as the restore control.
    commit(engine.call(&CallRequest::new("set_flag").arg("v", text("changed")), &mut g).expect("set_flag"));
    commit(engine.call(&CallRequest::new("add_note").arg("id", text("n1")), &mut g).expect("add_note"));

    assert_eq!(singleton(&engine, "flag"), Some(text("changed")), "the mutated singleton reads `changed`");
    assert_eq!(singleton(&engine, "cname"), Some(text("Acme")), "the seeded singleton struct member reads `Acme`");
    assert_eq!(notes(&engine), vec![text("n1")], "the note is present");

    // §19.5/§19.7: a verified `.liasse` artifact of the current boundary.
    let artifact = engine.export().expect("export");

    // §19.10: restore into a fresh instance. The restore itself SUCCEEDS.
    let restored = match Engine::restore(store("singleton-export"), &artifact, &mut generator()) {
        Ok(restored) => restored,
        Err(error) => panic!("restore unexpectedly failed: {error}"),
    };

    // Control: the top-level `notes` collection round-trips, so the restore
    // machinery works — the loss below is specific to the §8.2 singleton row.
    assert_eq!(notes(&restored), vec![text("n1")], "the top-level collection round-trips (restore works)");

    // §19.10 / §8.2: the root singleton state MUST survive the round trip. It does
    // not — `StateSection::capture` (portable.rs:49-51) skips every non-collection
    // root member, so the singleton reserved row is never written to the artifact
    // and reads back absent after restore.
    assert_eq!(
        singleton(&restored, "flag"),
        Some(text("changed")),
        "§19.10/§8.2: the mutated root singleton scalar `flag` must survive export/restore; it is \
         dropped because portable.rs `StateSection::capture` serializes only `Node::Collection` root \
         members and never the §8.2 singleton reserved row",
    );
    assert_eq!(
        singleton(&restored, "cname"),
        Some(text("Acme")),
        "§19.10/§8.2: the seeded root singleton struct member `company.name` must survive \
         export/restore; it too is dropped by the collection-only state capture",
    );
}
