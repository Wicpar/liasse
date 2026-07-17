#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe for §9.1 seed key agreement.
//!
//! §9.1: "The map member supplies the local key. A repeated key field MUST agree
//! with it." A seed row whose in-body key field disagrees with its `$data` map
//! member key is invalid input and, since §9.3 admits the whole load as one
//! atomic transition, must reject the load (conformance case
//! `tests/09-loading-bootstrap/common/seed-repeated-key-field-must-agree.hjson`,
//! `outcome: rejected`).

mod support;

use liasse_runtime::{Engine, Value};
use support::{generator, store};

/// Map member key `acme` disagrees with the repeated `id` field `acmeinc`.
const SEED_DISAGREE: &str = r#"{
  "$liasse": 1,
  "$app": "t.seedkeyagree@1.0.0",
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" },
    "all_companies": { "$view": ".companies { id, name }" }
  },
  "$data": {
    "companies": { "acme": { "id": "acmeinc", "name": "Acme SAS" } }
  }
}"#;

#[test]
fn repeated_key_field_disagreeing_with_map_member_rejects_load() {
    let mut generator = generator();
    let result = Engine::load(store("seed-key-disagree"), SEED_DISAGREE, &mut generator);

    // §9.1 + corpus `seed-repeated-key-field-must-agree`: a disagreeing repeated
    // key field is invalid seed input and the whole atomic load MUST reject.
    match result {
        Err(_) => { /* conforming: the disagreement rejected the load */ }
        Ok(engine) => {
            // Non-conforming. Surface exactly what the load did with the row so
            // the divergence is unambiguous: it silently keyed the row by the
            // in-body value and ignored the map member key.
            let view = engine
                .view_at_head("all_companies")
                .expect("view ok")
                .expect("view present");
            let ids: Vec<String> = view
                .rows()
                .iter()
                .filter_map(|r| match r.field("id") {
                    Some(Value::Text(t)) => Some(t.as_str().to_owned()),
                    _ => None,
                })
                .collect();
            panic!(
                "§9.1 violated: a disagreeing repeated key field admitted the load \
                 instead of rejecting it. Admitted {} company row(s) with id(s) {:?} \
                 (the map member key `acme` was silently discarded in favor of the \
                 in-body `acmeinc`).",
                view.len(),
                ids,
            );
        }
    }
}

/// Control (§9.1): a repeated key field that AGREES with its `$data` map member
/// key is valid — the load must still succeed and the row keep the shared key.
const SEED_AGREE: &str = r#"{
  "$liasse": 1,
  "$app": "t.seedkeyagree.ok@1.0.0",
  "$model": {
    "companies": { "$key": "id", "id": "text", "name": "text" },
    "all_companies": { "$view": ".companies { id, name }" }
  },
  "$data": {
    "companies": { "acme": { "id": "acme", "name": "Acme SAS" } }
  }
}"#;

#[test]
fn repeated_key_field_agreeing_with_map_member_loads() {
    let mut generator = generator();
    let engine = match Engine::load(store("seed-key-agree"), SEED_AGREE, &mut generator) {
        Ok(engine) => engine,
        Err(error) => panic!("an agreeing repeated key field must load, got: {error}"),
    };
    let view = engine.view_at_head("all_companies").expect("view ok").expect("view present");
    assert_eq!(view.len(), 1, "the agreeing seed row is admitted");
    assert_eq!(
        view.rows()[0].field("id"),
        Some(&Value::Text(liasse_value::Text::new("acme"))),
        "the shared key `acme` is kept",
    );
}
