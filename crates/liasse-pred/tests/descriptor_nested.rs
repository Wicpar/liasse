//! RED TEAM (Phase 7a): the NESTED-collection candidate build
//! (`CandidateDescriptor::build_nested` / `Member::nested`) against an independent
//! §5.4/§7.2/D.1 oracle.
//!
//! `crates/liasse-pred/src/descriptor.rs` is "the one seam that is not literally
//! shared code" (module doc): it re-derives `materialize::build_row` /
//! `materialize::rows_at` from a prefetched `CandidateSubtree`. The shipped
//! lowering-parity and coverage gates BOTH pass only `subtree_steps: Vec::new()`
//! and a flat scalar-only descriptor — so the entire nested path (`build_nested`,
//! the relative-path filter, the child-identity chain, the defensive key sort, the
//! per-level `key_identity`) is exercised NOWHERE. This file drives it and compares
//! the rebuilt `Row` to a hand-built one derived directly from §5.4 (nested keyed
//! collection materialized from the rows under this row's address, extending its
//! D.1 identity) and D.1 (key-derived `RowId` chain). A structural mismatch = the
//! faces would see a different candidate than the interpreter = HIGH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use liasse_expr::{Cell, Row, RowId};
use liasse_ident::KeyText;
use liasse_pred::{CandidateDescriptor, Member};
use liasse_store::{CandidateSubtree, KeyValue};
use liasse_value::{Integer, Struct, Text, Value};

fn text(s: &str) -> Value {
    Value::Text(Text::new(s))
}
fn int(n: i64) -> Value {
    Value::Int(Integer::from(n))
}
fn tkey(s: &str) -> KeyValue {
    KeyValue::single(text(s))
}
/// The canonical D.2 key text of a single-component key — the identity oracle the
/// whole runtime shares (`materialize::row_id_text`).
fn kt(key: &KeyValue) -> String {
    KeyText::from_key_values(&key.components().cloned().collect::<Vec<_>>()).unwrap().as_str().to_owned()
}

fn company(id: &str, plan: &str) -> Value {
    Value::Struct(Struct::new([(Text::new("id"), text(id)), (Text::new("plan"), text(plan))]))
}
/// A company with `plan` absent (an omitted optional, §A.1).
fn company_no_plan(id: &str) -> Value {
    Value::Struct(Struct::new([(Text::new("id"), text(id))]))
}

/// A path component `(step, key)` for a `CandidateSubtree` relative path.
fn step(name: &str, key: &str) -> (String, KeyValue) {
    (name.to_owned(), tkey(key))
}

/// A leaf company descriptor `{ id, plan }`.
fn leaf_desc() -> CandidateDescriptor {
    CandidateDescriptor::new(true, vec![Member::scalar("id"), Member::scalar("plan")])
}

/// A company that nests `subs` of `inner`.
fn nesting_desc(inner: CandidateDescriptor) -> CandidateDescriptor {
    CandidateDescriptor::new(
        true,
        vec![Member::scalar("id"), Member::scalar("plan"), Member::nested("subs", inner)],
    )
}

fn field(value: &Value, name: &str) -> Value {
    match value {
        Value::Struct(s) => s.get(name).cloned().unwrap_or(Value::None),
        _ => Value::None,
    }
}

/// Independently build the expected row for a NESTING descriptor level (`{ id,
/// plan, subs }`) per §5.4/D.1: the lone key component is the identity, and the
/// nested keyed collection is materialized from the rows under this row.
fn expect_nesting_row(value: &Value, key: &KeyValue, id: &RowId, subs: Vec<Row>) -> Row {
    Row::new(
        id.clone(),
        key.components().next().cloned().unwrap_or(Value::None),
        [
            ("id".to_owned(), Cell::scalar(field(value, "id"))),
            ("plan".to_owned(), Cell::scalar(field(value, "plan"))),
            ("subs".to_owned(), Cell::Collection(subs)),
        ],
    )
}

/// The expected row for a LEAF descriptor level (`{ id, plan }`) — no nested
/// member, so no `subs` cell exists.
fn expect_leaf_row(value: &Value, key: &KeyValue, id: &RowId) -> Row {
    Row::new(
        id.clone(),
        key.components().next().cloned().unwrap_or(Value::None),
        [
            ("id".to_owned(), Cell::scalar(field(value, "id"))),
            ("plan".to_owned(), Cell::scalar(field(value, "plan"))),
        ],
    )
}

#[test]
fn two_level_self_ref_nesting_with_absent_optional() {
    // acme
    //  ├─ eng ── t1
    //  └─ hr (plan absent)          [hr sorts after eng by key]
    // t1 and hr have no children (empty `subs`).
    let descriptor = nesting_desc(nesting_desc(leaf_desc()));

    let acme_v = company("acme", "active");
    let acme_key = tkey("acme");
    let subtree = CandidateSubtree(vec![
        // Deliberately OUT of key order (hr before eng, grandchild before child)
        // to exercise the defensive sort and depth filter.
        (vec![step("subs", "hr")], company_no_plan("hr")),
        (vec![step("subs", "eng"), step("subs", "t1")], company("t1", "active")),
        (vec![step("subs", "eng")], company("eng", "active")),
    ]);

    let built = descriptor.build_row(&acme_v, &acme_key, &subtree);

    // Expected identity chain (D.1): acme → child(eng) → child(t1); acme → child(hr).
    let acme_id = RowId::keyed(kt(&acme_key));
    let eng_id = acme_id.child_keyed(kt(&tkey("eng")));
    let t1_id = eng_id.child_keyed(kt(&tkey("t1")));
    let hr_id = acme_id.child_keyed(kt(&tkey("hr")));

    // t1 is built with the LEAF descriptor (no `subs` member); eng/hr with the MID
    // descriptor (a `subs` member, empty for hr and for t1's absent grandchildren).
    let t1 = expect_leaf_row(&company("t1", "active"), &tkey("t1"), &t1_id);
    let eng = expect_nesting_row(&company("eng", "active"), &tkey("eng"), &eng_id, vec![t1]);
    let hr = expect_nesting_row(&company_no_plan("hr"), &tkey("hr"), &hr_id, Vec::new());
    // Children of acme in Annex-B key order: eng < hr.
    let expected = expect_nesting_row(&acme_v, &acme_key, &acme_id, vec![eng, hr]);

    assert_eq!(built, expected, "\nbuilt:    {built:#?}\nexpected: {expected:#?}");
}

#[test]
fn sibling_step_names_do_not_cross_contaminate() {
    // A descriptor with TWO nested members `subs` and `notes`. A row through
    // `notes` must not leak into the `subs` collection and vice versa.
    let descriptor = CandidateDescriptor::new(
        true,
        vec![
            Member::scalar("id"),
            Member::scalar("plan"),
            Member::nested("subs", leaf_desc()),
            Member::nested("notes", leaf_desc()),
        ],
    );
    let acme_v = company("acme", "active");
    let acme_key = tkey("acme");
    let subtree = CandidateSubtree(vec![
        (vec![step("subs", "eng")], company("eng", "active")),
        (vec![step("notes", "n1")], company("n1", "active")),
    ]);

    let built = descriptor.build_row(&acme_v, &acme_key, &subtree);
    let subs = built.cell("subs").and_then(Cell::as_collection).unwrap();
    let notes = built.cell("notes").and_then(Cell::as_collection).unwrap();

    assert_eq!(subs.len(), 1, "subs must contain only the `eng` row");
    assert_eq!(subs.first().unwrap().key(), &text("eng"));
    assert_eq!(notes.len(), 1, "notes must contain only the `n1` row");
    assert_eq!(notes.first().unwrap().key(), &text("n1"));
}

#[test]
fn composite_key_row_identity() {
    // single_field_key = false: the row's application-visible key identity is the
    // positional `Value::Composite` of the key components (§5.4/B.4), NOT the lone
    // component. A rekeyed nested composite child chains its D.1 identity likewise.
    let child = CandidateDescriptor::new(false, vec![Member::scalar("id")]);
    let descriptor = CandidateDescriptor::new(
        false,
        vec![Member::scalar("id"), Member::nested("subs", child)],
    );

    let root_v = Value::Struct(Struct::new([(Text::new("id"), text("acme"))]));
    let root_key = KeyValue::composite(text("acme"), [int(7)]);
    let child_key = KeyValue::composite(text("eng"), [int(3)]);
    let subtree = CandidateSubtree(vec![(
        vec![("subs".to_owned(), child_key.clone())],
        Value::Struct(Struct::new([(Text::new("id"), text("eng"))])),
    )]);

    let built = descriptor.build_row(&root_v, &root_key, &subtree);

    // The composite key identity is the positional tuple.
    assert_eq!(built.key(), &Value::Composite(vec![text("acme"), int(7)]));
    let subs = built.cell("subs").and_then(Cell::as_collection).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs.first().unwrap().key(), &Value::Composite(vec![text("eng"), int(3)]));

    // Identity chain uses the typed composite key VALUE (§5.4/B.4/D.1): the
    // positional `Value::Composite` tuple, ordered component-wise by value (int
    // second component by §B.1), not the D.2 text join.
    let root_id = RowId::keyed_value(Value::Composite(vec![text("acme"), int(7)]));
    let child_id = root_id.child_keyed_value(Value::Composite(vec![text("eng"), int(3)]));
    assert_eq!(built.id(), &root_id);
    assert_eq!(subs.first().unwrap().id(), &child_id);
}

#[test]
fn empty_subtree_yields_empty_nested_collection() {
    // A nesting descriptor over an EMPTY subtree: the nested member must still be a
    // present, empty `Cell::Collection`, not absent and not a scalar `none`.
    let descriptor = nesting_desc(leaf_desc());
    let built = descriptor.build_row(&company("solo", "active"), &tkey("solo"), &CandidateSubtree::default());
    match built.cell("subs") {
        Some(Cell::Collection(rows)) => assert!(rows.is_empty(), "subs must be an empty collection"),
        other => panic!("subs must be an empty Cell::Collection, got {other:?}"),
    }
}
