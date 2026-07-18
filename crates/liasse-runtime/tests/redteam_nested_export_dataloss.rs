#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Regression: `Engine::export` (§19.5) of an instance holding nested keyed-
//! collection rows (§5.4) MUST fail closed — error loudly — rather than emit a
//! `.liasse` artifact with the nested rows silently dropped.
//!
//! `StateSection::capture` carries top-level collections and the §8.2 singleton
//! only; it never carries a nested collection forward. Exporting anyway would
//! serialize a state section that has silently lost every nested row, and a later
//! restore would reconstitute an instance missing that data — silent data loss
//! that violates §20.1 ("the compatible value is copied"), §22.1 (committed-state
//! integrity), and AGENTS.md's fail-closed rule. The capture now refuses when the
//! instance actually holds nested rows, so `export` surfaces
//! [`EngineError::Unsupported`] instead of producing a lossy artifact.
//!
//! The guard is precise — it fires only on committed nested rows, so a nested
//! schema whose nested collections are empty still exports, and a top-level-only
//! instance is unaffected. Faithful nested-collection carry-through remains a
//! tracked feature.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, EngineError, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A companies → offices two-level model (§5.4) with mutations that create the
/// parent row and, separately, a nested office row under it.
const OFFICES: &str = r#"{
  "$liasse": 1
  "$app": "t.nestexport@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "offices": { "$key": "id", "id": "text", "name": "text" }
    }
    "$mut": {
      "add_company": ".companies + { id: @id }"
      "add_office": ".companies[@company].offices + { id: @id, name: @name }"
    }
  }
}"#;

/// A top-level-only model (the control): no nested collection, so export is never
/// at risk of dropping anything.
const TOPLEVEL: &str = r#"{
  "$liasse": 1
  "$app": "t.toplvlexport@1.0.0"
  "$model": {
    "notes": { "$key": "id", "id": "text", "body": "text" }
    "$mut": { "add_note": ".notes + { id: @id, body: @body }" }
  }
}"#;

fn add_company(engine: &mut liasse_runtime::Engine<MemoryStore>, id: &str) {
    let mut g = generator();
    let outcome = engine.call(&CallRequest::new("add_company").arg("id", text(id)), &mut g).expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add_company must commit");
}

fn add_office(engine: &mut liasse_runtime::Engine<MemoryStore>, company: &str, id: &str, name: &str) {
    let mut g = generator();
    let outcome = engine
        .call(
            &CallRequest::new("add_office")
                .arg("company", text(company))
                .arg("id", text(id))
                .arg("name", text(name)),
            &mut g,
        )
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add_office must commit a nested row");
}

/// With a committed nested-collection row present, `export` must refuse rather
/// than emit an artifact that has silently dropped it (§20.1/§22.1, fail-closed).
#[test]
fn export_refuses_instance_holding_nested_collection_rows() {
    let mut engine = load("nestexport", OFFICES);
    add_company(&mut engine, "acme");
    add_office(&mut engine, "acme", "hq", "Acme HQ");
    match engine.export() {
        Err(EngineError::Unsupported(_)) => {}
        Ok(_) => panic!(
            "export of an instance holding nested-collection rows must fail closed, not silently \
             drop them into a lossy artifact (§20.1/§22.1)"
        ),
        Err(other) => panic!("export must refuse with EngineError::Unsupported, got: {other:?}"),
    }
}

/// The guard is precise: a nested-collection schema whose nested collections hold
/// NO rows loses nothing on capture, so export still succeeds. This proves the
/// refusal keys on actual nested data, not merely on the schema shape.
#[test]
fn export_allows_nested_schema_with_no_nested_rows() {
    let mut engine = load("nestempty", OFFICES);
    add_company(&mut engine, "acme"); // parent row only; `offices` stays empty
    engine
        .export()
        .expect("a nested schema with no nested rows drops nothing, so export must succeed");
}

/// CONTROL: a top-level-only instance carries all its state through the portable
/// capture, so export succeeds exactly as before — the guard never fires here.
#[test]
fn export_top_level_instance_succeeds_control() {
    let mut engine = load("toplvlexport", TOPLEVEL);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add_note").arg("id", text("k1")).arg("body", text("hello")), &mut g)
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add_note must commit");
    engine.export().expect("a top-level-only export must succeed");
}
