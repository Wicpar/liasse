#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM follow-on to Wave-17 Fix 1 (commit efdfb6a, `Builder::base_type` /
//! `scalar_shape`, crates/liasse-model/src/build/fields.rs:117-152). Fix 1 made an
//! expanded field's `$type` accept a type-name string OR an inline
//! `{ $enum: [...] }`, and REJECT anything else with a diagnostic (no more silent
//! `Type::Json`). Fix 1's landed test (`redteam_optional_enum_type_seam`) covered
//! only INITIAL admission of an out-of-set label. This file probes the edges the
//! fix did not directly cover, and confirms every one is handled:
//!
//!   * every OTHER inline shape under `$type` (`$ref`, `$set`, a struct, `$map`)
//!     REJECTS cleanly — invalid load, never a panic, never silent `json`;
//!   * an OPTIONAL inline enum enforces §5.9 on a later UPDATE (not just insert);
//!   * an inline `$type: { $enum }` used as a `$key` orders by §B.5 declaration
//!     order and rejects an out-of-set key (§5.9).
//!
//! Every expectation is deducible from SPEC.md text alone (§5.1/A.3 expanded field,
//! §5.9 closed-set enum, §B.5 declaration-order ordinals). All assertions PASS at
//! HEAD `2bae775`: the file is convergence evidence that Fix 1's rejection and
//! enforcement paths generalize past the single admission case its own test pinned.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, store};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

fn try_load(instance: &str, def: &str) -> Result<Engine<MemoryStore>, String> {
    Engine::load(store(instance), def, &mut generator()).map_err(|e| format!("{e}"))
}

/// §5.1/A.3: an expanded field's `$type` is a type name or an inline `{ $enum }`.
/// Every OTHER inline shape (`$ref`, `$set`, a plain-object struct, `$map`) is a
/// field-level form that is NOT valid inside `$type`; Fix 1 rejects each with a
/// diagnostic rather than silently keeping `Type::Json`. A clean static rejection
/// (not a panic, not an accepted `json` field) is the assertion.
#[test]
fn every_non_enum_inline_type_shape_rejects_cleanly() {
    for (label, shape) in [
        ("ref", r#"{ "$ref": "/things" }"#),
        ("set", r#"{ "$set": "text" }"#),
        ("struct", r#"{ "name": "text" }"#),
        ("map", r#"{ "$map": ["text", "text"] }"#),
    ] {
        let def = format!(
            r#"{{ "$liasse": 1, "$app": "t.r@1.0.0", "$model": {{
                "things": {{ "$key": "id", "id": "text", "f": {{ "$type": {shape} }} }},
                "all": {{ "$view": ".things {{ id }}" }} }} }}"#
        );
        let result = try_load(&format!("inline-type-{label}"), &def);
        assert!(
            result.is_err(),
            "§5.1/A.3: an inline `{{ ${label} }}` under `$type` is not a base type and MUST reject at \
             load, but the definition was accepted (Fix 1 must not silently coerce it to `json`)",
        );
    }
}

/// §5.9: an OPTIONAL inline-enum `$type` field enforces its closed set on a LATER
/// mutation, not only at initial insert. An out-of-set assign rejects; an in-set
/// assign commits and stores the declared label. Fix 1's own test only exercised
/// the insert path, so this pins the update path.
const OPT_APP: &str = r#"{
  "$liasse": 1, "$app": "t.o@1.0.0",
  "$model": {
    "things": {
      "$key": "id", "id": "text",
      "status": { "$type": { "$enum": ["draft","active","closed"] }, "$optional": true },
      "$mut": { "setst": ".status = @v" }
    },
    "all": { "$view": ".things { id, status }" },
    "$mut": { "add": ".things + { id: @id }" }
  }
}"#;

#[test]
fn optional_inline_enum_enforces_closed_set_on_update() {
    let mut engine = try_load("opt-enum-update", OPT_APP).expect("loads");
    let mut g = generator();
    assert!(
        matches!(
            engine.call(&CallRequest::new("add").arg("id", text("t1")), &mut g).expect("add"),
            CallOutcome::Committed { .. }
        ),
        "the insert (status omitted, optional -> none) commits",
    );

    // §5.9: an out-of-set label on a later UPDATE must reject.
    let mut g2 = generator();
    let bad = engine
        .call(&CallRequest::new("setst").receiver(text("t1")).arg("v", text("archived")), &mut g2)
        .expect("call");
    assert!(
        matches!(bad, CallOutcome::Rejected(_)),
        "§5.9: assigning an out-of-set label to an optional inline-enum field on UPDATE must reject, got {bad:?}",
    );

    // An in-set label commits and reads back as the declared enum label.
    let mut g3 = generator();
    assert!(
        matches!(
            engine.call(&CallRequest::new("setst").receiver(text("t1")).arg("v", text("active")), &mut g3).expect("call"),
            CallOutcome::Committed { .. }
        ),
        "an in-set label on UPDATE commits",
    );
    let view = engine.view_at_head("all").expect("view").expect("declared");
    let status = view.rows()[0].field("status").cloned();
    assert!(
        matches!(status, Some(Value::Enum(ref e)) if e.label() == "active"),
        "the updated field reads back as the declared enum label `active`, got {status:?}",
    );
}

/// §B.5/§5.9: an inline `$type: { $enum }` used as a `$key` behaves exactly like a
/// direct `{ $enum }` key — rows enumerate in declaration order (draft<active<closed),
/// and an out-of-set key rejects. Isolates that Fix 1 routes the inline `$type`
/// through the same `enum_node` (identical `Type::Enum`) a direct key uses.
const KEY_APP: &str = r#"{
  "$liasse": 1, "$app": "t.k@1.0.0",
  "$model": {
    "things": { "$key": "status", "status": { "$type": { "$enum": ["draft","active","closed"] } }, "note": "text" },
    "all": { "$view": ".things { status, note, $sort: [status] }" },
    "$mut": { "add": ".things + { status: @s, note: @n }" }
  }
}"#;

#[test]
fn inline_enum_as_key_orders_by_declaration_and_rejects_out_of_set() {
    let mut engine = try_load("inline-enum-key", KEY_APP).expect("loads");
    let mut g = generator();
    for (s, n) in [("closed", "c"), ("draft", "d"), ("active", "a")] {
        assert!(
            matches!(
                engine.call(&CallRequest::new("add").arg("s", text(s)).arg("n", text(n)), &mut g).expect("add"),
                CallOutcome::Committed { .. }
            ),
            "insert of key `{s}` commits",
        );
    }
    let view = engine.view_at_head("all").expect("view").expect("declared");
    let order: Vec<String> = view
        .rows()
        .iter()
        .filter_map(|r| match r.field("status") {
            Some(Value::Enum(e)) => Some(e.label().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        vec!["draft", "active", "closed"],
        "§B.5: an inline-enum key sorts in declaration order, not insertion order",
    );

    // §5.9: an out-of-set key rejects.
    let mut g2 = generator();
    let bad = engine
        .call(&CallRequest::new("add").arg("s", text("archived")).arg("n", text("x")), &mut g2)
        .expect("call");
    assert!(
        matches!(bad, CallOutcome::Rejected(_)),
        "§5.9: an out-of-set inline-enum KEY must reject, got {bad:?}",
    );
}
