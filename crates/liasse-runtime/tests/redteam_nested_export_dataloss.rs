#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Regression: `Engine::export` (§19.5) of an instance holding nested keyed-
//! collection rows (§5.4) MUST carry them into the artifact, and a restore MUST
//! reproduce them — an artifact that describes only the top level would silently
//! lose live data on every disaster-recovery restore.
//!
//! # The history this pins
//!
//! `StateSection::capture` once carried top-level collections and the §8.2
//! singleton only. Exporting anyway serialized a state section that had silently
//! lost every nested row, so a later restore reconstituted an instance missing
//! that data — silent loss violating §20.1 ("the compatible value is copied"),
//! §22.1 (committed-state integrity), and AGENTS.md's fail-closed rule. That was
//! first made fail-CLOSED (export refused), and is now genuinely fixed: the
//! capture carries the whole committed row tree, so the artifact is complete and
//! the round trip is lossless at every depth.
//!
//! The assertions below are deliberately about the RESTORED instance, not about
//! the artifact bytes: a capture that merely serialized nested rows without
//! re-addressing them on restore would still lose them, and only reading them back
//! through the engine proves otherwise.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load, store};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A companies → offices → desks model (§5.4) with mutations that create a row at
/// each level, and views projecting every level so a restore is observable.
const OFFICES: &str = r#"{
  "$liasse": 1
  "$app": "t.nestexport@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "offices": {
        "$key": "id"
        "id": "text"
        "name": "text"
        "desks": { "$key": "id", "id": "text", "label": "text" }
      }
    }
    "all_offices": { "$view": ".companies[:c].offices[:o] { company: c.id, office: o.id, name: o.name }" }
    "all_desks": { "$view": ".companies[:c].offices[:o].desks[:d] { desk: d.id, label: d.label }" }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_office": ".companies[@company].offices + { id: @id, name: @name }"
      "add_desk": ".companies[@company].offices[@office].desks + { id: @id, label: @label }"
    }
  }
}"#;

/// A top-level-only model (the control): no nested collection at all.
const TOPLEVEL: &str = r#"{
  "$liasse": 1
  "$app": "t.toplvlexport@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "all_notes": { "$view": ".notes { id, body }" }
    "$mut": { "add_note": ".notes + { id: @id, body: @body }" }
  }
}"#;

fn call(engine: &mut Engine<MemoryStore>, request: CallRequest) {
    let mut g = generator();
    let outcome = engine.call(&request, &mut g).expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the mutation must commit");
}

/// Every row a view projects, as `(field, value)` pairs, so two instances are
/// compared by their observable state rather than by internal addresses.
fn projection(engine: &Engine<MemoryStore>, view: &str) -> Vec<Vec<(String, Value)>> {
    let rows = engine.view_at_head(view).expect("view").expect("the view is declared");
    rows.rows()
        .iter()
        .map(|row| row.fields().map(|(name, value)| (name.clone(), value.clone())).collect())
        .collect()
}

/// An instance holding one company, two offices, and a desk under each.
fn populated(instance: &str) -> Engine<MemoryStore> {
    let mut engine = load(instance, OFFICES);
    call(&mut engine, CallRequest::new("add_company").arg("id", text("acme")));
    for (office, name) in [("hq", "Acme HQ"), ("lab", "Acme Lab")] {
        call(
            &mut engine,
            CallRequest::new("add_office")
                .arg("company", text("acme"))
                .arg("id", text(office))
                .arg("name", text(name)),
        );
        call(
            &mut engine,
            CallRequest::new("add_desk")
                .arg("company", text("acme"))
                .arg("office", text(office))
                .arg("id", text("d1"))
                .arg("label", text(name)),
        );
    }
    engine
}

/// §19.5/§19.10/§22.1: an export/restore round trip reproduces every nested row,
/// at depth 2 and depth 3 alike — the artifact describes the whole instance.
#[test]
fn export_restore_round_trips_nested_collection_rows() {
    let engine = populated("nestexport");
    let offices = projection(&engine, "all_offices");
    let desks = projection(&engine, "all_desks");
    assert_eq!(offices.len(), 2, "the fixture holds two depth-2 rows");
    assert_eq!(desks.len(), 2, "the fixture holds two depth-3 rows");

    let artifact = engine.export().expect("an instance holding nested rows exports");
    let mut generator = generator();
    let restored = Engine::restore(store("nestrestored"), &artifact, &mut generator).expect("restore");

    assert_eq!(projection(&restored, "all_offices"), offices, "every depth-2 row survives the round trip");
    assert_eq!(projection(&restored, "all_desks"), desks, "every depth-3 row survives the round trip");
}

/// A nested-collection schema whose nested collections hold NO rows round-trips
/// too, and its parent rows are unaffected — the carry keys on actual rows.
#[test]
fn export_restore_handles_a_nested_schema_with_no_nested_rows() {
    let mut engine = load("nestempty", OFFICES);
    call(&mut engine, CallRequest::new("add_company").arg("id", text("acme")));
    let artifact = engine.export().expect("export");
    let mut generator = generator();
    let restored = Engine::restore(store("nestemptyrestored"), &artifact, &mut generator).expect("restore");
    assert!(projection(&restored, "all_offices").is_empty(), "no nested row is invented on restore");
    assert!(restored.view_at_head("all_offices").is_ok(), "the restored instance is usable");
}

/// CONTROL: a top-level-only instance round-trips exactly as before, so the
/// nested carry-through changed nothing for a package that declares none.
#[test]
fn export_restore_top_level_instance_control() {
    let mut engine = load("toplvlexport", TOPLEVEL);
    call(&mut engine, CallRequest::new("add_note").arg("id", text("k1")).arg("body", text("hello")));
    let before = projection(&engine, "all_notes");
    let artifact = engine.export().expect("a top-level-only export must succeed");
    let mut generator = generator();
    let restored = Engine::restore(store("toplvlrestored"), &artifact, &mut generator).expect("restore");
    assert_eq!(projection(&restored, "all_notes"), before, "the top-level round trip is unchanged");
}
