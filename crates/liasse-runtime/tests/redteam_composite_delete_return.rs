#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§8.4): the delete-and-return local binding
//! (`removed = -.coll[…]` then `return removed { … }`) fails to CAPTURE a
//! COMPOSITE-keyed collection's deleted rows, so the mandated deleted-rows view
//! comes back empty even though the row is genuinely removed.
//!
//! §8.4 (SPEC.md line 1014): "Deletion returns the deleted rows as they existed
//! immediately before removal, in selector order after duplicate target
//! identities are removed by first occurrence." §8.4's local-binding form binds
//! that removed-rows view to a name so a `return name { … }` projects it. For a
//! single-field key this works (the corpus's
//! `21-deletion-erasure/deletion-returns-rows-as-they-existed` passes). It MUST
//! work identically for a composite key: the delete removes the row, and the
//! bound view carries that row's pre-delete payload.
//!
//! The capture path (`interp::bind_deleted`) addresses each captured row with
//! `materialize::top_address(coll, KeyValue::single(key.clone()))`
//! (crates/liasse-runtime/src/interp.rs:536). When `key` is the positional
//! `Value::Composite([region, code])` a composite row's key now uses (commit
//! f3a21bc, §5.4 / A.9), `KeyValue::single` wraps the WHOLE tuple as a
//! ONE-component key, which never equals the stored row's N-component
//! `RowAddress` (`{ region } :: [code]`). The `prospective.get(address)` lookup
//! therefore misses, the row is dropped from the captured collection, and
//! `return removed { … }` projects an EMPTY view. This is the identical
//! composite-key addressing defect that was fixed for `erase` (commit 3fdb601,
//! which switched `exec_erase` to `materialize::key_value_of`, interp.rs:1420),
//! left unfixed on the delete-and-return capture path.
//!
//! The delete ITSELF succeeds here: the removal at interp.rs:553 keys its
//! `RowRef` by the positional `Value::Composite`, which the §21.1 cascade planner
//! matches (the same path the passing `redteam_composite_direct_delete` control
//! exercises). So this isolates the defect to the CAPTURE, not the removal, and
//! not the delete form as such — the single-field control below captures and
//! returns its deleted row correctly. Expectations are re-derived from §8.4, not
//! the implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, ResponseValue, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

/// A composite-keyed `regions` collection carrying a non-key `secret` payload,
/// and a single-field-keyed `accounts` control. Each `drop_*` mutation is the
/// §8.4 delete-and-return binding: capture the removed rows into `removed`, then
/// return that removed-rows view. Both use the selection delete form so the
/// captured key is the exact stored row key (a positional `Value::Composite` for
/// `regions`, a scalar for `accounts`) — isolating the capture step.
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compdelret@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "secret": "text" },
    "accounts": { "$key": "id", "id": "text", "secret": "text" },
    "regions_view": { "$view": ".regions { region, code, secret, $sort: [region, code] }" },
    "accounts_view": { "$view": ".accounts { id, secret, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, secret: @secret }",
      "add_account": ".accounts + { id: @id, secret: @secret }",
      "drop_region": [
        "removed = -.regions[{ region: @region, code: @code }]",
        "return removed { region, code, secret }"
      ],
      "drop_account": [
        "removed = -.accounts[@id]",
        "return removed { id, secret }"
      ]
    }
  }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn commit(outcome: CallOutcome) -> ResponseValue {
    match outcome {
        CallOutcome::Committed { response, .. } => {
            response.expect("a delete-and-return mutation carries a response")
        }
        other => panic!("expected a committed state change, got {other:?}"),
    }
}

fn region_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("regions_view").expect("view").expect("declared").rows().len()
}

fn account_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("accounts_view").expect("view").expect("declared").rows().len()
}

#[test]
fn composite_delete_and_return_yields_the_deleted_row() {
    let mut g = generator();
    let mut engine = Engine::load(store("comp-del-ret"), M, &mut g).expect("load");
    let add = CallRequest::new("add_region")
        .arg("region", text("eu"))
        .arg("code", text("x"))
        .arg("secret", text("alpha-secret"));
    assert!(matches!(engine.call(&add, &mut g).expect("add"), CallOutcome::Committed { .. }));
    assert_eq!(region_count(&engine), 1, "the fixture seeds one composite region [eu, x]");

    let drop = CallRequest::new("drop_region").arg("region", text("eu")).arg("code", text("x"));
    let response = commit(engine.call(&drop, &mut g).expect("drop_region"));

    // The removal itself succeeds: the composite row is gone from live state.
    // (This is the control that the delete path handles the positional composite
    // key; the defect below is the CAPTURE, not the removal.)
    assert_eq!(region_count(&engine), 0, "the composite row [eu, x] is removed from live state");

    // §8.4: the delete-and-return binding returns the deleted row as it existed
    // immediately before removal — a one-element view carrying its full payload.
    let wire = response.to_wire();
    let rows = wire.as_array().expect("the deleted-rows return is a view (a JSON array)");
    assert_eq!(
        rows.len(),
        1,
        "§8.4: `removed = -.regions[{{ region, code }}]; return removed {{ … }}` must return the \
         deleted composite row [eu, x]; the returned view is empty ({wire}) because the capture \
         addressed the row with a one-component `KeyValue::single(Value::Composite)` that never \
         matched the stored N-component key (interp.rs:536)"
    );
    let row = rows[0].as_object().expect("a deleted row projects to an object");
    assert_eq!(row.get("region").and_then(|v| v.as_str()), Some("eu"), "the deleted row's region");
    assert_eq!(row.get("code").and_then(|v| v.as_str()), Some("x"), "the deleted row's code");
    assert_eq!(
        row.get("secret").and_then(|v| v.as_str()),
        Some("alpha-secret"),
        "§8.4: the returned deleted row carries its pre-delete payload (the non-key `secret`)"
    );
}

#[test]
fn single_field_delete_and_return_yields_the_deleted_row() {
    // CONTROL (must pass): the identical delete-and-return binding over a
    // single-field key captures and returns its deleted row, proving the
    // `bind_deleted` capture path works for a scalar key and isolating the defect
    // above to the composite-key carrier.
    let mut g = generator();
    let mut engine = Engine::load(store("scalar-del-ret"), M, &mut g).expect("load");
    let add = CallRequest::new("add_account").arg("id", text("a1")).arg("secret", text("beta-secret"));
    assert!(matches!(engine.call(&add, &mut g).expect("add"), CallOutcome::Committed { .. }));
    assert_eq!(account_count(&engine), 1, "one account seeded");

    let drop = CallRequest::new("drop_account").arg("id", text("a1"));
    let response = commit(engine.call(&drop, &mut g).expect("drop_account"));
    assert_eq!(account_count(&engine), 0, "the account is removed from live state");

    let wire = response.to_wire();
    let rows = wire.as_array().expect("the deleted-rows return is a view (a JSON array)");
    assert_eq!(rows.len(), 1, "§8.4: the single-field delete-and-return returns the deleted row");
    let row = rows[0].as_object().expect("a deleted row projects to an object");
    assert_eq!(row.get("id").and_then(|v| v.as_str()), Some("a1"), "the deleted account id");
    assert_eq!(
        row.get("secret").and_then(|v| v.as_str()),
        Some("beta-secret"),
        "§8.4: the returned deleted row carries its pre-delete payload",
    );
}
