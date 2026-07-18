#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe: the direct `collection - keys` delete form (§8.5) fails to
//! reconcile a composite-key **object operand** to the positional
//! `Value::Composite` carrier a composite row's key now uses (commit f3a21bc,
//! §5.4 / A.9), so it silently deletes nothing.
//!
//! §8.5 (`collection - keys   delete rows by key`) removes a row by its key.
//! §6.3 (line 698: "A composite-key lookup uses one object operand naming each
//! key component") makes `{ region, code }` the composite-key operand, and A.9
//! normalizes that authoring object to the target's `$key`-order tuple. So
//! `.regions - { region: 'eu', code: 'x' }` MUST delete the region whose key is
//! `[eu, x]`, exactly as the selection form `-.regions[{ region, code }]` does.
//!
//! Since f3a21bc a composite row's application-visible key is the positional
//! `Value::Composite` (`materialize::key_identity`), and the §21.1 cascade
//! planner keys its graph nodes by that identity (`cascade.rs` step 1). But the
//! `collection - keys` interpreter path (`interp::exec_delete`) evaluates the key
//! operand with `scalar_value`, which yields the bare authoring object as a
//! `Value::Struct` — it is never reconciled to `Value::Composite`. The resulting
//! `RowRef` therefore matches no graph node and the deletion closes over nothing:
//! the transition commits `Unchanged` and the row survives. The identical object
//! operand deletes the same row through the selection form, and the direct form
//! works for a single-field key, so this is a composite-carrier reconciliation
//! defect, not an unsupported form. Expectations are re-derived from §8.5 / §6.3,
//! not the implementation.

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

/// A composite-keyed `regions` collection with three ways to remove a row: the
/// direct `collection - { object }` form, the `-collection[{ object }]` selection
/// form, and — on a single-field-keyed sibling — the direct `collection - scalar`
/// form. The two literal object operands address the identical row `[eu, x]`.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compdirdel@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text" },
    "accounts": { "$key": "id", "id": "text" },
    "regions_view": { "$view": ".regions { region, code, $sort: [region, code] }" },
    "accounts_view": { "$view": ".accounts { id, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code }",
      "add_account": ".accounts + { id: @id }",
      "del_region_direct": ".regions - { region: 'eu', code: 'x' }",
      "del_region_select": "-.regions[{ region: 'eu', code: 'x' }]",
      "del_account_direct": ".accounts - @id"
    }
  }
}"#;

fn region_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("regions_view").expect("view").expect("declared").rows().len()
}

fn account_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("accounts_view").expect("view").expect("declared").rows().len()
}

fn with_region(label: &str) -> Engine<MemoryStore> {
    let mut g = generator();
    let mut engine = Engine::load(store(label), M, &mut g).expect("load");
    commit(
        engine
            .call(&CallRequest::new("add_region").arg("region", text("eu")).arg("code", text("x")), &mut g)
            .expect("call"),
    );
    assert_eq!(region_count(&engine), 1, "fixture seeds exactly one region [eu, x]");
    engine
}

#[test]
fn direct_minus_deletes_composite_keyed_row() {
    // §8.5: `.regions - { region: 'eu', code: 'x' }` deletes the row whose
    // composite key is [eu, x]. That row exists, so the collection MUST be empty
    // afterwards. (Currently the transition commits `Unchanged` and the row
    // survives — the object operand is carried as a `Value::Struct` that never
    // reconciles to the row's `Value::Composite` key.)
    let mut engine = with_region("direct");
    let mut g = generator();
    let outcome = engine.call(&CallRequest::new("del_region_direct"), &mut g).expect("call");
    assert_eq!(
        region_count(&engine),
        0,
        "§8.5/§6.3: the direct `collection - {{ region, code }}` form must delete the composite \
         row [eu, x]; it survived (delete outcome: {outcome:?})"
    );
}

#[test]
fn selection_form_deletes_the_same_composite_row() {
    // CONTROL (passes): the identical composite object operand, applied through
    // the selection form, deletes the row — so [eu, x] is genuinely addressable
    // by { region, code } and the direct-form failure above is a reconciliation
    // defect, not an addressing impossibility.
    let mut engine = with_region("select");
    let mut g = generator();
    commit(engine.call(&CallRequest::new("del_region_select"), &mut g).expect("call"));
    assert_eq!(region_count(&engine), 0, "the selection form deletes the composite row [eu, x]");
}

#[test]
fn direct_minus_deletes_single_field_keyed_row() {
    // CONTROL (passes): the direct `collection - key` form itself works for a
    // single-field key (`.accounts - @id`), isolating the defect to the composite
    // object-operand carrier rather than the direct-minus form as such.
    let mut g = generator();
    let mut engine = Engine::load(store("account"), M, &mut g).expect("load");
    commit(engine.call(&CallRequest::new("add_account").arg("id", text("a1")), &mut g).expect("call"));
    assert_eq!(account_count(&engine), 1, "one account seeded");
    commit(engine.call(&CallRequest::new("del_account_direct").arg("id", text("a1")), &mut g).expect("call"));
    assert_eq!(account_count(&engine), 0, "the direct `collection - key` form deletes a single-field-keyed row");
}
