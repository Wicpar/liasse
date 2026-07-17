#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §8.2/§8.8/§9.1 root-singleton `$normalize`: a writable singleton root field's
//! normalizer runs at genesis seed and at every `.field = …` write, exactly as a
//! collection field's normalizer does. §8.3: an inferred parameter carries no
//! normalization, but "the assigned target still applies its own normalization",
//! so `.name = @name` yields the normalized committed value.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::load;

/// A package with a normalized singleton root field, a root `rename` mutation
/// that assigns it from an inferred parameter and returns it, and a seed value.
const PROFILE: &str = r#"{
  "$liasse": 1
  "$app": "t.singnorm@1.0.0"
  "$model": {
    "name": { "$type": "text", "$normalize": "string.lower(string.trim(.))" }
    "readout": { "$view": ". { name }" }
    "$mut": { "rename": [".name = @name", "return .name"] }
  }
  "$data": { "name": "  BOB  " }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn singleton_name(engine: &Engine<MemoryStore>) -> serde_json::Value {
    let view = engine.view_at_head("readout").expect("view").expect("declared");
    view.rows()[0].field("name").map(Value::to_wire).expect("name is present")
}

#[test]
fn seed_applies_the_singleton_normalizer() {
    // §9.1: seed data passes through the same normalization a mutation write does,
    // so the seeded "  BOB  " is lowered-and-trimmed to "bob" in committed state.
    let engine = load("singnorm-seed", PROFILE);
    assert_eq!(singleton_name(&engine), serde_json::json!("bob"), "the seed value is normalized");
}

#[test]
fn write_applies_the_singleton_normalizer_and_returns_it() {
    let mut engine = load("singnorm-write", PROFILE);
    let mut generator = support::generator();
    // §8.3: the parameter itself is a plain text, but the assigned target applies
    // its own normalization; the return reads committed state (§8.10).
    let request = CallRequest::new("rename").arg("name", text("  ALICE  "));
    let outcome = engine.call(&request, &mut generator).expect("rename call");
    let CallOutcome::Committed { response, .. } = outcome else {
        panic!("rename must commit, got {outcome:?}");
    };
    assert_eq!(
        response.expect("rename returns the name").to_wire(),
        serde_json::json!("alice"),
        "the return shows the normalized committed value",
    );
    // The committed singleton state is the normalized value, observable by a view.
    assert_eq!(singleton_name(&engine), serde_json::json!("alice"), "the stored value is normalized");
}
