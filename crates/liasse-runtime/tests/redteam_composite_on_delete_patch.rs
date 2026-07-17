#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21.1 `$on_delete = { … }` patch policy for a ref to a COMPOSITE-keyed target.
//!
//! §21.1 makes a `collection - key` delete a graph operation: every inbound
//! reference's policy decides the fate of the rows that point at a deleted one.
//! A `= { … }` patch rewrites the surviving referencing row. The single-key
//! baseline of this exact policy is `on_delete::patch_rewrites_the_surviving_row_and_clears_the_ref`.
//!
//! The target's key arity is not part of §21.1's policy semantics: whether the
//! deleted row has a single-field `$key` or a composite one, the inbound patch
//! must be applied to the surviving referencing row all the same. This exercises
//! the composite-target case with an otherwise identical model and policy, so the
//! expectation is re-derived from §21.1, not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Value};
use liasse_value::{Ref, Struct, Text};
use support::{generator, load};

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compdel@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "label": "text = ''" },
    "holders": {
      "$key": "id",
      "id": "text",
      "status": "text = 'active'",
      "loc": { "$ref": "/regions", "$optional": true, "$on_delete": "= { loc: none, status: 'orphaned' }" }
    },
    "holders_view": { "$view": ".holders { id, status, loc, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, label: @label }",
      "add_holder": ".holders + { id: @id, loc: @loc }",
      "delete_region": "-.regions[{ region: @region, code: @code }]"
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

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

#[test]
fn on_delete_patch_rewrites_holder_of_composite_target() {
    let mut engine = load("compdel", M);
    let mut g = generator();
    commit(
        engine
            .call(
                &CallRequest::new("add_region").arg("region", text("eu")).arg("code", text("x")).arg("label", text("EU-X")),
                &mut g,
            )
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("add_holder").arg("id", text("h1")).arg("loc", loc("eu", "x")), &mut g).expect("call"));

    // Delete the composite region [eu, x]. §21.1: the inbound `= { … }` patch on
    // holder h1's `loc` ref must rewrite the surviving holder — exactly as the
    // single-key baseline does — clearing `loc` and setting `status: 'orphaned'`.
    commit(
        engine
            .call(&CallRequest::new("delete_region").arg("region", text("eu")).arg("code", text("x")), &mut g)
            .expect("call"),
    );

    let view = engine.view_at_head("holders_view").expect("view").expect("declared");
    let row = &view.rows()[0];
    assert_eq!(row.field("id"), Some(&text("h1")), "the holder survives the delete");
    assert_eq!(
        row.field("status"),
        Some(&text("orphaned")),
        "§21.1: the `= {{ … }}` patch must rewrite the surviving holder's status when its composite target is deleted"
    );
    // §21.1 clears the optional ref to `none`; a `none` optional field is an
    // absent optional value, so it is omitted from the projected view row.
    assert_eq!(
        row.field("loc"),
        None,
        "§21.1: the patch must clear the ref to the deleted composite region (else it dangles)"
    );
}

const R: &str = r#"{
  "$liasse": 1,
  "$app": "t.compdelr@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "label": "text = ''" },
    "holders": {
      "$key": "id",
      "id": "text",
      "loc": { "$ref": "/regions", "$on_delete": "restrict" }
    },
    "holders_view": { "$view": ".holders { id, loc, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, label: @label }",
      "add_holder": ".holders + { id: @id, loc: @loc }",
      "delete_region": "-.regions[{ region: @region, code: @code }]"
    }
  }
}"#;

/// Companion probe (PASSES): the `restrict` policy over the SAME composite target
/// is enforced. §21.1 `restrict` finds the inbound composite ref by identity and
/// blocks the delete, which proves composite-target `$on_delete` is a supported,
/// working feature — the patch defect above is an inconsistency, not a blanket
/// unsupported case.
#[test]
fn on_delete_restrict_blocks_delete_of_composite_target() {
    let mut engine = load("compdelr", R);
    let mut g = generator();
    commit(
        engine
            .call(
                &CallRequest::new("add_region").arg("region", text("eu")).arg("code", text("x")).arg("label", text("EU-X")),
                &mut g,
            )
            .expect("call"),
    );
    commit(engine.call(&CallRequest::new("add_holder").arg("id", text("h1")).arg("loc", loc("eu", "x")), &mut g).expect("call"));

    let outcome = engine
        .call(&CallRequest::new("delete_region").arg("region", text("eu")).arg("code", text("x")), &mut g)
        .expect("call");
    assert_eq!(
        outcome.rejection().map(liasse_runtime::Rejection::reason),
        Some(RejectionReason::Restricted),
        "§21.1: a `restrict` ref to a live composite region must block its deletion; got {outcome:?}"
    );
}
