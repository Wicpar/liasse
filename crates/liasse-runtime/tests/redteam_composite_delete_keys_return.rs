#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team probe (§8.4 / §8.5 / §6.3): the delete-and-return local binding over
//! the `collection - keys` **key form** with a COMPOSITE object operand
//! (`removed = .regions - { region: @region, code: @code }`) neither deletes the
//! row nor returns it — a composite-carrier defect the Wave-11 `key_value_of`
//! sweep (f4a645e) MISSED on the capture path.
//!
//! ## What the spec mandates
//!
//! §8.5 (SPEC.md line 1022): `collection - keys` deletes rows by key. §6.3
//! (line 698): "A composite-key lookup uses one object operand naming each key
//! component", and A.9 normalizes that authoring object `{ region, code }` to the
//! target's positional `$key`-order tuple. §8.4 (line 1014): "Deletion returns
//! the deleted rows as they existed immediately before removal"; the local-binding
//! form (`name = <deletion>`, then `return name { … }`) projects that removed-rows
//! view. So `removed = .regions - { region: @region, code: @code };
//! return removed { region, code, secret }` MUST (a) delete the composite row
//! `[eu, x]` and (b) return it once, carrying its pre-delete payload — identically
//! to how the single-field key form and the composite *selection* form behave.
//!
//! ## The defect
//!
//! Two sibling interpreter paths evaluate a `collection - keys` operand, and only
//! one normalizes a composite object operand to the row's positional
//! `Value::Composite` key:
//!
//! * The delete STATEMENT path `interp::exec_delete` (interp.rs:1323-1328) builds
//!   `normalize = |v| materialize::normalize_key_operand(&key_fields, v)` and maps
//!   every operand through it before addressing rows. That is the f3a21bc/f4a645e
//!   composite fix; the statement form works (`redteam_composite_direct_delete`
//!   and `redteam_composite_in_param_delete` pass).
//! * The delete-and-return CAPTURE path `interp::delete_key_values`
//!   (interp.rs:498-504) does NOT normalize: it returns the bare authoring
//!   `Value::Struct({ region, code })` straight from `scalar_value`.
//!
//! That un-normalized `Value::Struct` then flows to `bind_deleted`
//! (interp.rs:521), whose capture addresses each row with
//! `materialize::key_value_of(key)` (interp.rs:542). `key_value_of` only
//! decomposes a `Value::Composite`; on a `Value::Struct` it takes the
//! `other => KeyValue::single(other)` arm (materialize.rs:111), building a
//! one-component `KeyValue::single(Value::Struct)` that never equals the stored
//! N-component `KeyValue::composite`. The `prospective.get(address)` lookup misses,
//! so the returned view is EMPTY. The same `Value::Struct` is handed to
//! `delete_rows` as `RowRef::new(name, Value::Struct)` (interp.rs:558); the §21.1
//! cascade planner keys its graph nodes by `materialize::key_identity` (the
//! positional `Value::Composite`, cascade.rs:53), so the `RowRef` matches no node
//! and the removal closes over nothing — the row SURVIVES and the call reports
//! `unchanged` (§8.9).
//!
//! The Wave-11 sweep routed `bind_deleted`'s capture through `key_value_of`, but
//! `key_value_of` is downstream of the missing `normalize_key_operand` step: the
//! authoring `Value::Struct` must be reconciled to `Value::Composite` FIRST (as
//! `exec_delete` does), and `delete_key_values` omits it. So the composite object
//! operand is dropped before `key_value_of` ever sees a shape it can decompose.
//!
//! ## Isolation
//!
//! Both controls below pass, isolating the defect to the composite object operand
//! on the key-form capture path — not the delete-and-return machinery, not the
//! `- keys` form, and not the addressability of `[eu, x]` by `{ region, code }`:
//!   * `single_field_delete_keys_and_return` — the identical key-form capture over
//!     a single-field key returns its deleted row (`key_value_of` on a scalar is
//!     `single`, which matches);
//!   * `composite_delete_selection_and_return` — the SAME composite object operand,
//!     applied through the SELECTION form (`removed = -.regions[{ … }]`, which
//!     `selection_key_values` evaluates to materialized-row keys), deletes and
//!     returns the row.
//!
//! Expectations are re-derived from §8.4 / §8.5 / §6.3 / A.9, not the
//! implementation.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, ResponseValue, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

/// A composite-keyed `regions` collection carrying a non-key `secret` payload, and
/// a single-field-keyed `accounts` control. Each `drop_*` mutation is a §8.4
/// delete-and-return binding. `drop_region_keys` uses the `collection - keys` KEY
/// form with a composite object operand (the site under test); `drop_account_keys`
/// the same KEY form over a single-field key (control); `drop_region_select` the
/// SELECTION form over the identical composite object operand (control).
const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.compdelkeysret@1.0.0",
  "$model": {
    "regions": { "$key": ["region", "code"], "region": "text", "code": "text", "secret": "text" },
    "accounts": { "$key": "id", "id": "text", "secret": "text" },
    "regions_view": { "$view": ".regions { region, code, secret, $sort: [region, code] }" },
    "accounts_view": { "$view": ".accounts { id, secret, $sort: [id] }" },
    "$mut": {
      "add_region": ".regions + { region: @region, code: @code, secret: @secret }",
      "add_account": ".accounts + { id: @id, secret: @secret }",
      "drop_region_keys": [
        "removed = .regions - { region: @region, code: @code }",
        "return removed { region, code, secret }"
      ],
      "drop_account_keys": [
        "removed = .accounts - @id",
        "return removed { id, secret }"
      ],
      "drop_region_select": [
        "removed = -.regions[{ region: @region, code: @code }]",
        "return removed { region, code, secret }"
      ]
    }
  }
}"#;

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// Extract the `return` response from a delete-and-return call under EITHER
/// completion. §8.9: if the delete removes nothing the call reports `unchanged`
/// (with the `return` evaluated from the unchanged state); a genuine removal
/// reports `committed`. Both carry a `return` response, and the test asserts the
/// deleted-rows view on it regardless, so a wrong `unchanged` still surfaces as a
/// row-count / empty-view failure rather than a helper panic.
fn response(outcome: CallOutcome) -> ResponseValue {
    match outcome {
        CallOutcome::Committed { response, .. } | CallOutcome::Unchanged { response } => {
            response.expect("a delete-and-return mutation carries a `return` response")
        }
        CallOutcome::Rejected(rejection) => panic!("delete-and-return unexpectedly rejected: {rejection:?}"),
    }
}

fn region_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("regions_view").expect("view").expect("declared").rows().len()
}

fn account_count(engine: &Engine<MemoryStore>) -> usize {
    engine.view_at_head("accounts_view").expect("view").expect("declared").rows().len()
}

#[test]
fn composite_delete_keys_and_return_yields_the_deleted_row() {
    // The site under test: `removed = .regions - { region, code }` then
    // `return removed { … }`. §8.5/§6.3: the composite object operand deletes the
    // row keyed [eu, x]; §8.4: the binding returns that deleted row.
    let mut g = generator();
    let mut engine = Engine::load(store("comp-del-keys-ret"), M, &mut g).expect("load");
    let add = CallRequest::new("add_region")
        .arg("region", text("eu"))
        .arg("code", text("x"))
        .arg("secret", text("alpha-secret"));
    assert!(matches!(engine.call(&add, &mut g).expect("add_region"), CallOutcome::Committed { .. }));
    assert_eq!(region_count(&engine), 1, "the fixture seeds one composite region [eu, x]");

    let drop = CallRequest::new("drop_region_keys").arg("region", text("eu")).arg("code", text("x"));
    let response = response(engine.call(&drop, &mut g).expect("drop_region_keys"));

    // §8.5/§6.3: the composite row [eu, x] must be removed from live state. Under
    // the defect it survives — `delete_key_values` never normalizes the object
    // operand, so the §21.1 planner matches no node and the removal no-ops.
    assert_eq!(
        region_count(&engine),
        0,
        "§8.5/§6.3: `removed = .regions - {{ region, code }}` (key form) must delete the composite \
         row [eu, x]; it survived because `delete_key_values` (interp.rs:498) omits the \
         `normalize_key_operand` step `exec_delete` (interp.rs:1324) applies, so the operand stays \
         a `Value::Struct` the §21.1 planner never matches"
    );

    // §8.4: the binding returns the deleted row — a one-element view carrying its
    // full pre-delete payload. Under the defect the captured view is EMPTY, because
    // `bind_deleted` addresses the row with `key_value_of(Value::Struct)`, a
    // one-component `KeyValue::single` that never matches the stored composite key.
    let wire = response.to_wire();
    let rows = wire.as_array().expect("the deleted-rows return is a view (a JSON array)");
    assert_eq!(
        rows.len(),
        1,
        "§8.4: the delete-and-return binding must return the deleted composite row [eu, x]; the \
         returned view is empty ({wire}) because the un-normalized `Value::Struct` operand \
         addressed a non-existent one-component row (interp.rs:498 -> 542, materialize.rs:111)"
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
fn single_field_delete_keys_and_return_yields_the_deleted_row() {
    // CONTROL (must pass): the identical key-form capture over a single-field key
    // deletes and returns its row. `key_value_of` on a scalar is `single`, which
    // matches the stored single-field key, so the capture and removal both succeed —
    // isolating the defect above to the composite object-operand carrier.
    let mut g = generator();
    let mut engine = Engine::load(store("scalar-del-keys-ret"), M, &mut g).expect("load");
    let add = CallRequest::new("add_account").arg("id", text("a1")).arg("secret", text("beta-secret"));
    assert!(matches!(engine.call(&add, &mut g).expect("add"), CallOutcome::Committed { .. }));
    assert_eq!(account_count(&engine), 1, "one account seeded");

    let drop = CallRequest::new("drop_account_keys").arg("id", text("a1"));
    let response = response(engine.call(&drop, &mut g).expect("drop_account_keys"));
    assert_eq!(account_count(&engine), 0, "§8.5: the single-field key form deletes the row");

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

#[test]
fn composite_delete_selection_and_return_is_the_working_control() {
    // CONTROL (must pass): the SAME composite object operand `{ region, code }`,
    // applied through the SELECTION form `removed = -.regions[{ … }]`, deletes and
    // returns the row. `selection_key_values` evaluates the selection to its
    // materialized rows and captures each `row.key()` — already the positional
    // `Value::Composite` — so the object operand IS addressable and the defect
    // above is confined to the KEY-form capture path, not an addressing
    // impossibility.
    let mut g = generator();
    let mut engine = Engine::load(store("comp-del-select-ret"), M, &mut g).expect("load");
    let add = CallRequest::new("add_region")
        .arg("region", text("eu"))
        .arg("code", text("x"))
        .arg("secret", text("alpha-secret"));
    assert!(matches!(engine.call(&add, &mut g).expect("add_region"), CallOutcome::Committed { .. }));
    assert_eq!(region_count(&engine), 1, "the fixture seeds one composite region [eu, x]");

    let drop = CallRequest::new("drop_region_select").arg("region", text("eu")).arg("code", text("x"));
    let response = response(engine.call(&drop, &mut g).expect("drop_region_select"));
    assert_eq!(region_count(&engine), 0, "the selection form deletes the composite row [eu, x]");

    let wire = response.to_wire();
    let rows = wire.as_array().expect("the deleted-rows return is a view (a JSON array)");
    assert_eq!(rows.len(), 1, "§8.4: the selection-form delete-and-return returns the deleted row");
    let row = rows[0].as_object().expect("a deleted row projects to an object");
    assert_eq!(row.get("region").and_then(|v| v.as_str()), Some("eu"), "the deleted row's region");
    assert_eq!(row.get("code").and_then(|v| v.as_str()), Some("x"), "the deleted row's code");
    assert_eq!(
        row.get("secret").and_then(|v| v.as_str()),
        Some("alpha-secret"),
        "§8.4: the returned deleted row carries its pre-delete payload",
    );
}
