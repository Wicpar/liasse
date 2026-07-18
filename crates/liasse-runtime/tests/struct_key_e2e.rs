#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! End-to-end flow of a struct-typed `$key` (SPEC.md A.8: "structs composed
//! solely of key-eligible required fields"). The model now ACCEPTS such a key
//! (see `liasse-model` `build/keys.rs` and `redteam_struct_key_eligibility`);
//! this test proves the acceptance is not a load-only fix that lets in data the
//! runtime cannot handle: a struct-keyed collection compiles, insert stores a
//! `struct`-valued key, and a scan reads the rows back in Annex-B key-ascending
//! order — which for a struct key is field-name (text) order (B.4), NOT the
//! declaration order of the struct's members.
//!
//! Every expected order is re-derived from the spec, not the engine: with key
//! `loc = { x: int, y: int }`, B.4 compares the struct's fields in canonical
//! field-name order (`x` before `y`), so rows sort by `x` then `y` ascending.
//!
//! The durable rekey-and-reopen leg, and memory-vs-PostgreSQL agreement, are
//! exercised at the semantics-free store layer (the value-keyed layer that
//! actually holds a struct key) in `liasse-pg`'s `struct_key_divergence`. Note a
//! known model limitation this test deliberately does not depend on: a struct
//! `$key`'s *declared* key type still resolves to `json` (the resolver /
//! runtime-schema key-type fallback treats a non-scalar key node as `json`), so a
//! struct-key *selector* operand (`.cells[@k]`) cannot yet be typed in a mutation
//! prototype — insert and read do not need the key type and work regardless.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::{CollectionPath, InstanceStore, MemoryStore};
use liasse_value::{Integer, Struct, Text};
use support::{generator, load};

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.structkey.e2e@1.0.0",
  "$model": {
    "cells": {
      "$key": "loc",
      "loc": { "x": "int", "y": "int" },
      "value": "text"
    },
    "cells_view": { "$view": ".cells { loc, value }" },
    "$mut": {
      "add": ".cells + { loc: { x: @x, y: @y }, value: @v }"
    }
  }
}"#;

fn int(n: i64) -> Value {
    Value::Int(Integer::parse(&n.to_string()).expect("valid int"))
}

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// The `struct` key value `{ x, y }` as stored: a `Value::Struct` whose members
/// are held in canonical field-name order.
fn loc(x: i64, y: i64) -> Value {
    Value::Struct(Struct::new([(Text::new("x"), int(x)), (Text::new("y"), int(y))]))
}

fn add(engine: &mut Engine<MemoryStore>, x: i64, y: i64, v: &str) {
    let mut g = generator();
    let request = CallRequest::new("add").arg("x", int(x)).arg("y", int(y)).arg("v", text(v));
    let outcome = engine.call(&request, &mut g).expect("call add");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "insert of a struct-keyed row commits, got {outcome:?}");
}

/// The `(loc, value)` pairs a `cells_view` scan reads, in the order returned.
fn view_rows(engine: &Engine<MemoryStore>) -> Vec<(Value, Value)> {
    let view = engine.view_at_head("cells_view").expect("view ok").expect("view declared");
    view.rows()
        .iter()
        .map(|row| (row.field("loc").expect("loc").clone(), row.field("value").expect("value").clone()))
        .collect()
}

/// The row payloads a store-level scan reads, straight from the committed store,
/// in Annex-B key order (the store's own ordering, independent of the view).
fn store_scan_values(engine: &Engine<MemoryStore>) -> Vec<Value> {
    engine
        .store()
        .scan(&CollectionPath::top(liasse_ident::NameSegment::new("cells")))
        .expect("scan")
        .into_iter()
        .map(|(_, row)| row.value().clone())
        .collect()
}

/// A.8 accept + B.4 order end-to-end: four struct-keyed rows read back sorted by
/// `x` then `y` ascending (field-name order), not by insertion or declaration
/// order.
#[test]
fn struct_keyed_rows_scan_in_field_name_order() {
    let mut engine = load("structkey-e2e", M);
    // Insert in a deliberately scrambled order.
    add(&mut engine, 2, 1, "a");
    add(&mut engine, 1, 5, "b");
    add(&mut engine, 1, 2, "c");
    add(&mut engine, 2, 0, "d");

    // B.4: compare `x` first (`x` < `y` as field names), then `y`. So the
    // ascending order is (1,2), (1,5), (2,0), (2,1).
    let expected = vec![
        (loc(1, 2), text("c")),
        (loc(1, 5), text("b")),
        (loc(2, 0), text("d")),
        (loc(2, 1), text("a")),
    ];
    assert_eq!(view_rows(&engine), expected, "a struct key scans in B.4 field-name order");

    // The store's own scan agrees: four distinct rows, key-ascending. The stored
    // payload is the whole row struct `{ loc, value }`; assert the `value` cells in
    // the store's scan order match the same B.4 sequence.
    let store_values = store_scan_values(&engine);
    let store_value_cells: Vec<Value> = store_values
        .iter()
        .map(|row| match row {
            Value::Struct(fields) => fields.get("value").expect("value cell").clone(),
            other => panic!("a stored row is a struct, got {other:?}"),
        })
        .collect();
    assert_eq!(
        store_value_cells,
        vec![text("c"), text("b"), text("d"), text("a")],
        "the store scans struct-keyed rows in the same B.4 order the view does"
    );
}
