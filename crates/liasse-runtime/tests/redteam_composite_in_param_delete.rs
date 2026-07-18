#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probes for two composite-object-operand positions the f3a21bc
//! positional rework left broken (§6.3 / §8.5 / A.9):
//!
//!  1. `object in composite-keyed-view` (§6.3): an authoring object naming each
//!     `$key` component (`{ region, code }`) is the target's `$key`-order tuple.
//!     Membership MUST normalize it to the row's `Value::Composite` key and
//!     evaluate TRUE when that row exists — before the fix it evaluated FALSE
//!     because the object was carried as a bare `Value::Struct` that never
//!     matched the row's positional composite key.
//!
//!  2. Parameterized `collection - { comp: @p, ... }` delete (§8.5): a composite
//!     delete whose components are parameters MUST load (the delete param-inference
//!     must recognize the object operand and infer each `@p` from its named key
//!     component, mirroring the `[{..}]` selector path) and delete exactly the
//!     addressed row.
//!
//! Expectations are re-derived from §6.3/§8.5/A.9, not the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn commit(outcome: CallOutcome) {
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "expected a commit, got {outcome:?}");
}

/// A composite-keyed `regions` collection plus an `accounts` sink so a membership
/// probe's verdict is observable (a passing `assert` lets the following insert
/// commit; a failing one rejects the whole program). `del_region_param` removes a
/// composite row addressed entirely by parameters.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compinparam@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text" },
    "accounts": { "$key": "id", "id": "text" },
    "regions_view": { "$view": ".regions { region, code, $sort: [region, code] }" },
    "accounts_view": { "$view": ".accounts { id, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code }",
      "guarded_present": ["assert({ region: 'eu', code: 'x' } in .regions, 'region [eu, x] must be a member')", ".accounts + { id: @id }"],
      "guarded_absent": ["assert({ region: 'zz', code: 'zz' } in .regions, 'region [zz, zz] must be a member')", ".accounts + { id: @id }"],
      "del_region_param": ".regions - { region: @region, code: @code }"
    }
  }
}"#;

fn regions(engine: &Engine<MemoryStore>) -> Vec<(String, String)> {
    engine
        .view_at_head("regions_view")
        .expect("view")
        .expect("declared")
        .rows()
        .iter()
        .map(|row| {
            let region = match row.field("region") {
                Some(Value::Text(t)) => t.as_str().to_owned(),
                other => panic!("region field: {other:?}"),
            };
            let code = match row.field("code") {
                Some(Value::Text(t)) => t.as_str().to_owned(),
                other => panic!("code field: {other:?}"),
            };
            (region, code)
        })
        .collect()
}

fn account_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("accounts_view").expect("view").expect("declared").rows().len()
}

fn seed(label: &str, rows: &[(&str, &str)]) -> Engine<MemoryStore> {
    let mut g = generator();
    let mut engine = Engine::load(store(label), M, &mut g).expect("load");
    for (region, code) in rows {
        commit(
            engine
                .call(
                    &CallRequest::new("add_region").arg("region", text(region)).arg("code", text(code)),
                    &mut g,
                )
                .expect("call"),
        );
    }
    engine
}

#[test]
fn membership_of_a_present_composite_key_evaluates_true() {
    // §6.3/A.9: `{ region: 'eu', code: 'x' } in .regions` denotes the composite
    // key [eu, x], which exists — the `assert` passes and the following insert
    // commits. (Before the fix the object stayed a `Value::Struct`, membership was
    // FALSE, the assert failed, and the program rejected.)
    let mut engine = seed("in-present", &[("eu", "x")]);
    let mut g = generator();
    let outcome =
        engine.call(&CallRequest::new("guarded_present").arg("id", text("a1")), &mut g).expect("call");
    commit(outcome);
    assert_eq!(account_count(&engine), 1, "membership was TRUE, so the guarded insert committed");
}

#[test]
fn membership_of_an_absent_composite_key_evaluates_false() {
    // CONTROL: the same form for a key that does NOT exist evaluates FALSE, so the
    // assert fails and nothing commits — proving the TRUE result above is a genuine
    // key match, not membership collapsing to always-true.
    let mut engine = seed("in-absent", &[("eu", "x")]);
    let mut g = generator();
    let outcome =
        engine.call(&CallRequest::new("guarded_absent").arg("id", text("a1")), &mut g).expect("call");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "absent membership must reject: {outcome:?}");
    assert_eq!(account_count(&engine), 0, "the guarded insert did not run");
}

#[test]
fn parameterized_composite_delete_removes_the_addressed_row() {
    // §8.5/§6.3/A.9: `.regions - { region: @region, code: @code }` must LOAD (each
    // `@p` inferred from its named key component) and delete exactly [eu, x],
    // leaving [us, y]. Before the fix the delete param-inference ignored the object
    // operand, so `@region`/`@code` could not be inferred and the package failed to
    // load.
    let mut engine = seed("del-param", &[("eu", "x"), ("us", "y")]);
    let mut g = generator();
    assert_eq!(regions(&engine), vec![("eu".to_owned(), "x".to_owned()), ("us".to_owned(), "y".to_owned())]);
    commit(
        engine
            .call(
                &CallRequest::new("del_region_param").arg("region", text("eu")).arg("code", text("x")),
                &mut g,
            )
            .expect("call"),
    );
    assert_eq!(
        regions(&engine),
        vec![("us".to_owned(), "y".to_owned())],
        "the parameterized composite delete removed exactly [eu, x]"
    );
}

#[test]
fn parameterized_composite_delete_of_an_absent_key_is_a_no_op() {
    // CONTROL: deleting a key that does not exist changes nothing (commits
    // Unchanged), so the delete is precise rather than clearing the collection.
    let mut engine = seed("del-param-absent", &[("eu", "x")]);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("del_region_param").arg("region", text("zz")).arg("code", text("zz")), &mut g)
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Unchanged { .. }), "absent-key delete is a no-op: {outcome:?}");
    assert_eq!(regions(&engine), vec![("eu".to_owned(), "x".to_owned())], "the extant row survives");
}
