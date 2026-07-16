#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §19 history/artifacts: an export round-trips through a restore, and a tampered
//! artifact is rejected by the recursive artifact verification before any state
//! is instantiated.

mod support;

use liasse_ident::InstanceId;
use liasse_runtime::{CallRequest, ConflictKind, Engine, ImportError, ImportRelation, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load, SEEDED, TASKS};

const NOTES: &str = r#"{
  "$liasse": 1
  "$app": "example.notes@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "all_notes": { "$view": ".notes { id, body } " }
    "$mut": { "add": ".notes + { id: @id, body: @body }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn add_note(engine: &mut Engine<MemoryStore>, id: &str, body: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("body", text(body));
    engine.call(&request, &mut generator).expect("add commits");
}

fn note_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("all_notes").expect("view").expect("declared").len()
}

/// Read `all_companies` and return `(id, name)` pairs for comparison.
fn companies(engine: &Engine<MemoryStore>) -> Vec<(String, String)> {
    let view = engine.view_at_head("all_companies").expect("view").expect("declared");
    view.rows()
        .iter()
        .map(|row| {
            let id = format!("{:?}", row.field("id").expect("id"));
            let name = format!("{:?}", row.field("name").expect("name"));
            (id, name)
        })
        .collect()
}

#[test]
fn export_then_restore_reproduces_state() {
    let engine = load("hist", SEEDED);
    let before = companies(&engine);
    assert_eq!(before.len(), 2, "the seeded fixture has two companies");

    let artifact = engine.export().expect("export");

    // §19.10: restoring the artifact into a fresh runtime reproduces the state.
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new("hist"));
    let restored = Engine::restore(store, &artifact, &mut generator).expect("restore");
    assert_eq!(companies(&restored), before, "restored state equals exported state");
}

#[test]
fn tampered_state_is_rejected_by_artifact_verification() {
    let engine = load("hist", SEEDED);
    let mut artifact = engine.export().expect("export");

    // The state section is stored uncompressed, so a seeded value appears
    // verbatim; flipping a byte inside it makes the entry's sha256 disagree
    // with the manifest (§19.8 / Annex D.5 verification).
    let needle = b"Globex";
    let at = artifact
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("seeded value present in the state section");
    artifact[at] ^= 0x20;

    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new("hist"));
    match Engine::restore(store, &artifact, &mut generator) {
        Err(ImportError::Artifact(_)) => {}
        Err(other) => panic!("expected an artifact-verification failure, got {other}"),
        Ok(_) => panic!("a tampered artifact must not restore"),
    }
}

#[test]
fn same_instance_export_classifies_as_same_point() {
    let engine = load("hist", SEEDED);
    let artifact = engine.export().expect("export");
    // §19.8: an export of the current point, classified against that same
    // instance and lineage, is already synchronized.
    let relation = engine.classify(&artifact).expect("classify");
    assert_eq!(relation, ImportRelation::SamePoint);
}

#[test]
fn different_instance_classifies_as_unrelated() {
    // Two applications with distinct incarnations have no shared history (§19.8).
    let a = load("app-a", TASKS);
    let b = load("app-b", TASKS);
    let artifact = a.export().expect("export");
    assert_eq!(b.classify(&artifact).expect("classify"), ImportRelation::Unrelated);
}

#[test]
fn rollback_import_restores_earlier_point() {
    // Advance to a later point, export it, then export an earlier point, and
    // import the earlier over the later: a permitted rollback moves state back.
    let mut engine = load("notes", NOTES);
    add_note(&mut engine, "n1", "one");
    let early = engine.export().expect("export early");
    add_note(&mut engine, "n2", "two");
    assert_eq!(note_count(&engine), 2);

    // The earlier artifact precedes the current head -> rollback available.
    assert_eq!(engine.classify(&early).expect("classify"), ImportRelation::Rollback);
    let report = engine
        .import(&early, &[ImportRelation::Rollback])
        .expect("import");
    assert!(report.applied, "rollback is permitted by the policy");
    assert_eq!(note_count(&engine), 1, "state moved back to the earlier point");
}

#[test]
fn fast_forward_import_applies_continuation() {
    let mut base = load("notes", NOTES);
    add_note(&mut base, "n1", "one");
    let early = base.export().expect("export early");
    add_note(&mut base, "n2", "two");
    let ahead = base.export().expect("export ahead");

    // Restore the earlier point into a fresh runtime of the same instance, then
    // fast-forward it to the later point.
    let mut generator = generator();
    let store = MemoryStore::new(InstanceId::new("notes"));
    let mut restored = Engine::restore(store, &early, &mut generator).expect("restore");
    assert_eq!(note_count(&restored), 1);
    assert_eq!(restored.classify(&ahead).expect("classify"), ImportRelation::FastForward);
    let report = restored.import(&ahead, &[ImportRelation::FastForward]).expect("import");
    assert!(report.applied);
    assert_eq!(note_count(&restored), 2, "the incoming continuation applied");
}

#[test]
fn policy_gates_activation() {
    let mut engine = load("notes", NOTES);
    add_note(&mut engine, "n1", "one");
    let early = engine.export().expect("export");
    add_note(&mut engine, "n2", "two");

    // A rollback is available but not permitted by an empty policy: classified,
    // not applied (§19.8).
    let report = engine.import(&early, &[]).expect("import");
    assert_eq!(report.relation, ImportRelation::Rollback);
    assert!(!report.applied, "no movement activates outside the policy");
    assert_eq!(note_count(&engine), 2, "state is unchanged");
}

#[test]
fn merge_reports_delete_vs_modify_conflict() {
    // Base has two notes; local deletes one; incoming modifies the same one.
    // §19.9 reports a delete-versus-modify conflict.
    let mut base_engine = load("notes", NOTES);
    add_note(&mut base_engine, "n1", "one");
    add_note(&mut base_engine, "n2", "two");
    let base = base_engine.export().expect("base");

    // The incoming side has n2 with a different body -> modified relative to base.
    let mut incoming_engine = load("notes-i", NOTES);
    add_note(&mut incoming_engine, "n1", "one");
    add_note(&mut incoming_engine, "n2", "edited");
    let incoming = incoming_engine.export().expect("incoming");

    // The local side lacks n2 -> deleted relative to base.
    let mut local = load("notes", NOTES);
    add_note(&mut local, "n1", "one");

    let outcome = local.merge(&base, &incoming).expect("merge");
    assert!(!outcome.is_clean(), "the merge conflicts");
    assert!(
        outcome.conflicts.iter().any(|c| c.kind == ConflictKind::DeleteVsModify),
        "delete-vs-modify is reported: {:?}",
        outcome.conflicts,
    );
}
