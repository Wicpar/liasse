#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module composition: an installed child instance keeps its private state
//! across disable/enable, its surfaces are unavailable while disabled, distinct
//! installations are isolated, and the §13.13 seed three-way merge follows its
//! rule.

mod support;

use std::collections::BTreeMap;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, ModuleError, ModuleHost, SeedMerge, Value,
};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_value::Text;
use support::{generator, TASKS};

/// A small module package with private `notes` state and an `add` mutation.
const NOTES: &str = r#"{
  "$liasse": 1
  "$module": "example.notes@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "all_notes": { "$view": ".notes { id, body } " }
    "$mut": { "add": ".notes + { id: @id, body: @body }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("root")), TASKS, &mut generator)
        .expect("root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn add_note(host: &mut ModuleHost<MemoryStoreFactory>, instance: &str, id: &str, body: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("body", text(body));
    let outcome = host.child_call(instance, &request, &mut generator).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

fn note_count(host: &ModuleHost<MemoryStoreFactory>, instance: &str) -> usize {
    host.child_view(instance, "all_notes").expect("view").expect("declared").len()
}

#[test]
fn install_then_disable_enable_preserves_state() {
    let mut host = host();
    let mut generator = generator();

    host.install("sales", NOTES, &mut generator).expect("install");
    add_note(&mut host, "sales", "n1", "first");
    assert_eq!(note_count(&host, "sales"), 1, "the note is stored");

    // §13.3/§13.12: disabling removes the active surfaces but retains state.
    host.disable("sales").expect("disable");
    assert!(!host.is_enabled("sales"), "the instance is disabled");
    match host.child_view("sales", "all_notes") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no surfaces, got {other:?}"),
    }

    // Enabling restores the surfaces over the exact preserved private state.
    host.enable("sales").expect("enable");
    assert_eq!(note_count(&host, "sales"), 1, "state survived disable/enable");
}

#[test]
fn installations_are_isolated() {
    let mut host = host();
    let mut generator = generator();
    host.install("sales", NOTES, &mut generator).expect("install sales");
    host.install("support", NOTES, &mut generator).expect("install support");

    add_note(&mut host, "sales", "n1", "only in sales");
    assert_eq!(note_count(&host, "sales"), 1);
    assert_eq!(note_count(&host, "support"), 0, "sibling instance state is independent");

    // Distinct installations of the same package are distinct incarnations.
    assert_ne!(host.incarnation("sales"), host.incarnation("support"));
}

#[test]
fn duplicate_install_name_rejected() {
    let mut host = host();
    let mut generator = generator();
    host.install("sales", NOTES, &mut generator).expect("install");
    match host.install("sales", NOTES, &mut generator) {
        Err(ModuleError::DuplicateName(_)) => {}
        other => panic!("a duplicate instance name must be rejected, got {other:?}"),
    }
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let mut host = host();
    let mut generator = generator();
    host.install("sales", NOTES, &mut generator).expect("install");
    let incarnation = host.incarnation("sales").expect("installed").clone();
    add_note(&mut host, "sales", "n1", "kept");

    host.rename("sales", "revenue").expect("rename");
    assert!(!host.is_installed("sales"), "the old name no longer addresses the instance");
    assert_eq!(host.incarnation("revenue"), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(note_count(&host, "revenue"), 1, "rename preserves state");
}

#[test]
fn uninstall_removes_instance() {
    let mut host = host();
    let mut generator = generator();
    host.install("sales", NOTES, &mut generator).expect("install");
    host.uninstall("sales").expect("uninstall");
    assert!(!host.is_installed("sales"));
    match host.child_view("sales", "all_notes") {
        Err(ModuleError::Unknown(_)) => {}
        other => panic!("an uninstalled instance is unknown, got {other:?}"),
    }
}

#[test]
fn seed_three_way_merge_retains_local_edits() {
    // §13.13: the new seed replaces a field only when the current value still
    // equals the old seed value; a locally edited field is retained.
    let mut old_seed = BTreeMap::new();
    old_seed.insert("title".to_owned(), text("Welcome"));
    old_seed.insert("body".to_owned(), text("v1 body"));

    let mut new_seed = BTreeMap::new();
    new_seed.insert("title".to_owned(), text("Welcome"));
    new_seed.insert("body".to_owned(), text("v2 body"));

    let mut current = BTreeMap::new();
    // `title` is still the old seed value; `body` was edited locally.
    current.insert("title".to_owned(), text("Welcome"));
    current.insert("body".to_owned(), text("edited by user"));

    // Make the new seed's `title` observably different so "takes the new seed"
    // is externally deducible rather than coincidental.
    new_seed.insert("title".to_owned(), text("Welcome v2"));
    let merged = SeedMerge { old_seed: &old_seed, new_seed: &new_seed, current: &current }.merge();
    assert_eq!(merged.get("title"), Some(&text("Welcome v2")), "unchanged field takes the new seed");
    assert_eq!(merged.get("body"), Some(&text("edited by user")), "locally edited field is retained");
}
