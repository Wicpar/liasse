#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module lifecycle over the surface [`ModuleDeployment`]: an installed child
//! keeps its private state across disable/enable, a disabled child exposes no
//! surfaces, a duplicate name is a rejection observation (not a fault), a rename
//! preserves the incarnation, and an uninstall removes the instance.

use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_surface::{
    Engine, ModuleDeployment, ModuleError, ModuleHost, ModuleObservation, ModuleUpdate, Precision,
    Value, VirtualClock,
};
use liasse_value::Text;

const NOW: i128 = 1_700_000_000_000_000;

const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "example.root@1.0.0"
  "$model": { "flags": { "$key": "id", "id": "text" } }
}"#;

const NOTES: &str = r#"{
  "$liasse": 1
  "$module": "example.notes@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "all_notes": { "$view": ".notes { id, body }" }
    "$mut": { "add": ".notes + { id: @id, body: @body }" }
  }
}"#;

/// A compatible successor of [`NOTES`] adding a defaulted `pinned` field.
const NOTES_V2: &str = r#"{
  "$liasse": 1
  "$module": "example.notes@1.1.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text", "pinned": "bool = false" }
    "all_notes": { "$view": ".notes { id, body }" }
    "$mut": { "add": ".notes + { id: @id, body: @body }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn deployment() -> ModuleDeployment<MemoryStoreFactory> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let root = Engine::load(MemoryStore::new(InstanceId::new("root")), ROOT, &mut clock).expect("root loads");
    ModuleDeployment::new(ModuleHost::new(MemoryStoreFactory::new(), root), clock)
}

fn add_note(deployment: &mut ModuleDeployment<MemoryStoreFactory>, instance: &str, id: &str, body: &str) {
    let request = CallRequest::new("add").arg("id", text(id)).arg("body", text(body));
    let outcome = deployment.child_call(instance, &request).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

fn note_count(deployment: &ModuleDeployment<MemoryStoreFactory>, instance: &str) -> usize {
    deployment.child_view(instance, "all_notes").expect("view").expect("declared").len()
}

#[test]
fn install_disable_enable_preserves_state() {
    let mut deployment = deployment();
    assert_eq!(deployment.install("sales", NOTES).expect("install"), ModuleObservation::Applied);
    add_note(&mut deployment, "sales", "n1", "first");
    assert_eq!(note_count(&deployment, "sales"), 1);

    assert_eq!(deployment.disable("sales").expect("disable"), ModuleObservation::Applied);
    assert!(!deployment.is_enabled("sales"));
    match deployment.child_view("sales", "all_notes") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no surfaces, got {other:?}"),
    }

    assert_eq!(deployment.enable("sales").expect("enable"), ModuleObservation::Applied);
    assert_eq!(note_count(&deployment, "sales"), 1, "state survived disable/enable");
}

#[test]
fn duplicate_install_is_a_rejection_observation() {
    let mut deployment = deployment();
    deployment.install("sales", NOTES).expect("install");
    assert_eq!(
        deployment.install("sales", NOTES).expect("second install is an observation, not a fault"),
        ModuleObservation::DuplicateName("sales".to_owned()),
    );
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let mut deployment = deployment();
    deployment.install("sales", NOTES).expect("install");
    let incarnation = deployment.incarnation("sales").expect("installed").clone();
    add_note(&mut deployment, "sales", "n1", "kept");

    assert_eq!(deployment.rename("sales", "revenue").expect("rename"), ModuleObservation::Applied);
    assert!(!deployment.is_installed("sales"));
    assert_eq!(deployment.incarnation("revenue"), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(note_count(&deployment, "revenue"), 1, "rename preserves state");
}

#[test]
fn update_migrates_a_single_instance() {
    let mut deployment = deployment();
    deployment.install("sales", NOTES).expect("install");
    add_note(&mut deployment, "sales", "n1", "kept");

    match deployment.update("sales", NOTES_V2).expect("update") {
        ModuleUpdate::Updated(_) => {}
        other => panic!("a compatible update migrates, got {other:?}"),
    }
    // The migrated instance keeps its row; the added field defaults in.
    assert_eq!(note_count(&deployment, "sales"), 1, "the note survived the migration");
}

#[test]
fn uninstall_removes_instance() {
    let mut deployment = deployment();
    deployment.install("sales", NOTES).expect("install");
    assert_eq!(deployment.uninstall("sales").expect("uninstall"), ModuleObservation::Applied);
    assert!(!deployment.is_installed("sales"));
    assert_eq!(
        deployment.uninstall("sales").expect("second uninstall observes unknown"),
        ModuleObservation::Unknown("sales".to_owned()),
    );
}
