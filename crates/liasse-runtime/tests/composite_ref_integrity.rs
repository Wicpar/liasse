#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! §5.6/§7.6/§22.1 reference integrity for a ref to a COMPOSITE-keyed target.
//!
//! A ref to a collection with a composite `$key` is carried as its target's
//! name-sorted key struct (a ref to a composite target is typed
//! `RefTarget::Scalar(Struct)`; decode builds `Ref::scalar(Struct)`). Admission
//! must resolve it against the target's materialized composite key
//! (`materialize::key_identity`, also a name-sorted struct): a ref to a live
//! composite row commits, a ref to an absent composite row is `DanglingRef`.
//! Expectations are re-derived from §7.6 ("a ref value is a target key") and
//! §5.6 reference validity — not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Value};
use liasse_value::{Ref, Struct, Text};
use support::{generator, load};

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compref@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "label": "text" },
    "holders": { "$key": "id", "id": "text", "loc": { "$ref": "/regions" } },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, label: @label }",
      "add_holder": ".holders + { id: @id, loc: @loc }"
    }
  }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// A composite ref to `[region, code]` as decode builds it: `Ref::scalar` of the
/// name-sorted key struct `{ code, region }`.
fn loc(region: &str, code: &str) -> Value {
    Value::Ref(Ref::scalar(Value::Struct(Struct::new([
        (Text::new("code"), text(code)),
        (Text::new("region"), text(region)),
    ]))))
}

fn with_region() -> liasse_runtime::Engine<liasse_store::MemoryStore> {
    let mut engine = load("compref", M);
    let mut g = generator();
    let r = engine
        .call(
            &CallRequest::new("add_region")
                .arg("region", text("eu"))
                .arg("code", text("x"))
                .arg("label", text("EU-X")),
            &mut g,
        )
        .expect("call");
    assert!(matches!(r, CallOutcome::Committed { .. }), "region insert: {r:?}");
    engine
}

#[test]
fn composite_ref_resolves_to_live_target() {
    let mut engine = with_region();
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add_holder").arg("id", text("h1")).arg("loc", loc("eu", "x")), &mut g)
        .expect("call");
    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "a composite ref to a live region must resolve (§5.6/§7.6), got {outcome:?}"
    );
}

#[test]
fn composite_ref_to_absent_target_is_rejected() {
    // Enforcement, not a blanket pass: a composite ref whose key names no live
    // region is a dangling reference (§5.6/§22.1).
    let mut engine = with_region();
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add_holder").arg("id", text("h2")).arg("loc", loc("eu", "y")), &mut g)
        .expect("call");
    assert_eq!(
        outcome.rejection().map(liasse_runtime::Rejection::reason),
        Some(RejectionReason::DanglingRef),
        "a composite ref to an absent region must be rejected; got {outcome:?}"
    );
}
