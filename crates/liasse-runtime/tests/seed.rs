#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §9.1 genesis seed admission: valid `$data` is admitted through the full rule
//! pipeline (defaults, normalization, checks); an invalid seed rejects the load.

mod support;

use liasse_runtime::{Engine, EngineError, RejectionReason, Value};
use liasse_value::{Integer, Text};
use support::{generator, load, store, SEEDED, SEEDED_INVALID};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

#[test]
fn valid_seed_is_admitted_through_the_pipeline() {
    let engine = load("seeded", SEEDED);
    let view = engine.view_at_head("all_companies").expect("view").expect("declared");
    assert_eq!(view.len(), 2, "both seeded companies are present");

    // Rows are in Annex B key order: acme, then globex.
    let acme = &view.rows()[0];
    assert_eq!(acme.field("name"), Some(&text("Acme")), "seed name is trimmed");
    assert_eq!(acme.field("tier"), Some(&int(1)), "omitted tier takes its default");

    let globex = &view.rows()[1];
    assert_eq!(globex.field("name"), Some(&text("Globex")));
    assert_eq!(globex.field("tier"), Some(&int(3)), "supplied tier wins over the default");
}

/// §9.1: all seeded identities and supplied values form one prospective state
/// before defaults resolve, so a default reading another seeded collection
/// observes it regardless of `$data` member order. `audits` is listed before the
/// `companies` its default counts, yet must still see both.
const SEED_PROSPECTIVE: &str = r#"{
  "$liasse": 1
  "$app": "t.prospective@1.0.0"
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" }
    "audits": { "$key": "id", "id": "text", "companies_seen": "int = count(/companies)" }
    "all_audits": { "$view": ".audits { id, companies_seen }" }
  }
  "$data": {
    "audits": { "boot": {} }
    "companies": { "a": { "name": "Alpha" }, "b": { "name": "Beta" } }
  }
}"#;

#[test]
fn a_seed_default_observes_the_whole_prospective_state() {
    let engine = load("prospective", SEED_PROSPECTIVE);
    let view = engine.view_at_head("all_audits").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "one audit row");
    assert_eq!(
        view.rows()[0].field("companies_seen"),
        Some(&int(2)),
        "the default counts both seeded companies even though `audits` is seeded first",
    );
}

#[test]
fn invalid_seed_rejects_the_load() {
    let mut generator = generator();
    let result = Engine::load(store("seeded-bad"), SEEDED_INVALID, &mut generator);
    match result {
        Err(EngineError::Seed(rejection)) => {
            assert_eq!(rejection.reason(), RejectionReason::Check, "the blank name fails its check");
        }
        Err(other) => panic!("expected a seed rejection, got {other}"),
        Ok(_) => panic!("an invalid seed must not activate"),
    }
}

/// §4.2 / Annex C.4: a `$data` value is a literal-or-expression position. A
/// string beginning with `=` is an expression evaluated against the seed state;
/// a leading `'` escapes exactly one quote and stores the remainder as a literal
/// that is never evaluated.
const SEED_ESCAPE: &str = r#"{
  "$liasse": 1
  "$app": "t.escape@1.0.0"
  "$model": {
    "docs": {
      "$key": "id"
      "id": "text"
      "formula": "text"
      "n": "int"
    }
    "all_docs": { "$view": ".docs { id, formula, n }" }
  }
  "$data": {
    "docs": {
      "d1": { "formula": "'= total + tax", "n": "= 1 + 1" }
    }
  }
}"#;

#[test]
fn seed_honors_the_literal_escape_and_the_expression_form() {
    let engine = load("escape", SEED_ESCAPE);
    let view = engine.view_at_head("all_docs").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "one seeded doc");
    let row = &view.rows()[0];
    // "'= total + tax" is an escaped literal: one leading ' removed, NOT evaluated.
    assert_eq!(
        row.field("formula"),
        Some(&text("= total + tax")),
        "a leading ' escape stores the literal `= total + tax`, not an evaluated expression",
    );
    // "= 1 + 1" is an expression evaluated at seed time to the int 2.
    assert_eq!(
        row.field("n"),
        Some(&int(2)),
        "a `= 1 + 1` seed value evaluates to the int 2",
    );
}

/// The leading-`'` escape removes exactly one quote (Annex C.4): `"'x"` stores
/// `"x"`, `"''x"` stores `"'x"`, and a lone `"'"` stores the empty string.
const SEED_QUOTE_BOUNDARY: &str = r#"{
  "$liasse": 1
  "$app": "t.quoteboundary@1.0.0"
  "$model": {
    "docs": { "$key": "id", "id": "text", "label": "text" }
    "all_docs": { "$view": ".docs { id, label }" }
  }
  "$data": {
    "docs": {
      "plain": { "label": "'plain" }
      "double": { "label": "''x" }
      "lone": { "label": "'" }
    }
  }
}"#;

#[test]
fn seed_literal_escape_removes_exactly_one_leading_quote() {
    let engine = load("quote-boundary", SEED_QUOTE_BOUNDARY);
    let view = engine.view_at_head("all_docs").expect("view").expect("declared");
    let label = |id: &str| {
        view.rows()
            .iter()
            .find(|r| r.field("id") == Some(&text(id)))
            .and_then(|r| r.field("label"))
            .cloned()
    };
    assert_eq!(label("plain"), Some(text("plain")), "one leading ' removed");
    assert_eq!(label("double"), Some(text("'x")), "exactly one of two leading quotes removed");
    assert_eq!(label("lone"), Some(text("")), "a lone ' stores the empty string");
}

/// The exact §04 `data-expression-and-literal-escape` corpus package, served
/// through its `$public` surface view (which reads another view) — proving the
/// seed materialization is observable end to end at the runtime boundary.
const SEED_ESCAPE_SURFACE: &str = r#"{
  "$liasse": 1
  "$app": "t.pkg.escape@1.0.0"
  "$model": {
    "doc": { "formula": "text", "n": "int" }
    "doc_view": { "$view": ".doc { formula, n }" }
    "$public": { "doc": { "$view": ".doc_view" } }
  }
  "$data": {
    "doc": { "formula": "'= total + tax", "n": "= 1 + 1" }
  }
}"#;

#[test]
fn seed_escape_is_observable_through_a_public_surface_view() {
    use liasse_runtime::ViewQuery;
    let engine = load("escape-surface", SEED_ESCAPE_SURFACE);
    let head = engine.head();
    let result = engine
        .view_with("public.doc", head, &ViewQuery::new())
        .expect("surface view")
        .expect("declared");
    let row = &result.rows()[0];
    assert_eq!(row.field("formula"), Some(&text("= total + tax")), "escaped literal survives the surface read");
    assert_eq!(row.field("n"), Some(&int(2)), "the `= 1 + 1` seed evaluates to 2 through the surface read");
}
