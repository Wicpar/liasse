#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §5.4 inbound-ref rewrite for a COMPOSITE-keyed target.
//!
//! An atomic rekey of a referenced row must rewrite every inbound reference to
//! the new key in the same transition, so no dangling or stale reference
//! survives. A ref to a composite-keyed target is carried as its target's
//! name-sorted key struct (`Ref::scalar(Struct)`), so the rewrite must match and
//! reissue it by application identity, not positional components. After rekeying
//! the target, the inbound composite ref must resolve to the NEW key and read the
//! target row. Expectations are re-derived from §5.4 (inbound-ref rewrite) and
//! §7.6 (a ref is a target key), not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::{Ref, Struct, Text};
use support::{generator, load};

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.comprekey@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "label": "text" },
    "holders": { "$key": "id", "id": "text", "loc": { "$ref": "/regions" } },
    "holders_view": { "$view": ".holders { id, loc }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, label: @label }",
      "add_holder": ".holders + { id: @id, loc: @loc }",
      "recode_region": ".regions[{ region: @region, code: @code }].code = @new"
    }
  }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// The composite ref value `[region, code]` as decode builds it: `Ref::scalar` of
/// the name-sorted key struct `{ code, region }`.
fn loc(region: &str, code: &str) -> Value {
    Value::Ref(Ref::scalar(Value::Struct(Struct::new([
        (Text::new("code"), text(code)),
        (Text::new("region"), text(region)),
    ]))))
}

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

/// The `loc` value of holder `h1` in the current committed state.
fn holder_loc(engine: &Engine<MemoryStore>) -> Value {
    let view = engine.view_at_head("holders_view").expect("view").expect("declared");
    view.rows()[0].field("loc").expect("loc field").clone()
}

#[test]
fn rekey_rewrites_inbound_composite_ref() {
    let mut engine = load("comprekey", M);
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

    // Rekey the region [eu, x] -> [eu, y]. §5.4: the inbound composite ref must
    // be rewritten to the new key in the same transition (else it dangles, since
    // no region [eu, x] remains).
    commit(
        engine
            .call(&CallRequest::new("recode_region").arg("region", text("eu")).arg("code", text("x")).arg("new", text("y")), &mut g)
            .expect("call"),
    );

    // The rekey COMMITTING (not rejecting) already proves the rewritten inbound
    // ref resolves to a live row: §5.4 marks the referencing holder touched and
    // the final pass re-validates its references. The `loc` now reads the NEW key.
    assert_eq!(
        holder_loc(&engine),
        loc("eu", "y"),
        "§5.4: the inbound composite ref must be rewritten from [eu, x] to [eu, y]"
    );
}
