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
