#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! End-to-end: a struct-typed `$key` (A.8) addressed by a *selector* in a
//! mutation. Accepting the struct key (commit a4495e9) carried the data — insert,
//! scan, rekey — but a struct-key selector operand could not be typed, because
//! the declared key type fell back to `json`: a parameter declared with the
//! struct type and used as `.cells[@k]` inferred two incompatible types and the
//! package failed to load. With the key type computed as the struct itself, the
//! selector types and works.
//!
//! This proves the completion end-to-end: a mutation declaring `@k` as the struct
//! key and using it as `.cells[@k]` loads, selects the row whose key equals `@k`,
//! and returns exactly that row — with the key value carried straight through
//! from the call argument to the stored row. Every expectation is derived from
//! the data inserted, not from the engine's own answer.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value};
use liasse_store::{CollectionPath, InstanceStore, MemoryStore};
use liasse_value::{Integer, Struct, Text};
use support::{generator, load};

const M: &str = r#"{
  "$liasse": 1,
  "$app": "t.structkey.selector.e2e@1.0.0",
  "$model": {
    "cells": {
      "$key": "loc",
      "loc": { "x": "int", "y": "int" },
      "value": "text"
    },
    "cells_view": { "$view": ".cells { loc, value }" },
    "$mut": {
      "add": ".cells + { loc: { x: @x, y: @y }, value: @v }",
      "take({ k: { x: int, y: int } })": ["removed = .cells[@k]", ".cells - @k", "return removed"]
    }
  }
}"#;

fn int(n: i64) -> Value {
    Value::Int(Integer::parse(&n.to_string()).expect("valid int"))
}

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// The struct key value `{ x, y }` as stored and as passed to a selector.
fn loc(x: i64, y: i64) -> Value {
    Value::Struct(Struct::new([(Text::new("x"), int(x)), (Text::new("y"), int(y))]))
}

fn add(engine: &mut Engine<MemoryStore>, x: i64, y: i64, v: &str) {
    let mut g = generator();
    let request = CallRequest::new("add").arg("x", int(x)).arg("y", int(y)).arg("v", text(v));
    let outcome = engine.call(&request, &mut g).expect("call add");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "insert commits, got {outcome:?}");
}

/// The number of rows the store currently holds under `cells`.
fn cell_count(engine: &Engine<MemoryStore>) -> usize {
    engine
        .store()
        .scan(&CollectionPath::top(liasse_ident::NameSegment::new("cells")))
        .expect("scan")
        .len()
}

/// A.8/§6.3 end-to-end: `take` declares `@k` as the struct key and selects
/// `.cells[@k]`. The package loads (the selector types), and calling it with a
/// struct key value selects and returns exactly the row keyed by that struct,
/// then removes it.
#[test]
fn struct_key_selector_selects_and_returns_the_keyed_row() {
    // The load itself is the first assertion: pre-fix this package is rejected
    // ("@k used with two incompatible types") because the struct-key selector
    // could not be typed.
    let mut engine = load("structkey-selector-e2e", M);

    // Four rows at distinct struct keys, inserted scrambled.
    add(&mut engine, 2, 1, "a");
    add(&mut engine, 1, 5, "b");
    add(&mut engine, 1, 2, "c");
    add(&mut engine, 2, 0, "d");
    assert_eq!(cell_count(&engine), 4);

    // Select the row keyed by the struct { x: 1, y: 2 } — the "c" row.
    let mut g = generator();
    let request = CallRequest::new("take").arg("k", loc(1, 2));
    let outcome = engine.call(&request, &mut g).expect("call take");

    let response = match &outcome {
        CallOutcome::Committed { response, .. } => response.as_ref().expect("take returns a row"),
        other => panic!("take commits with a returned row, got {other:?}"),
    };
    // The returned row is exactly the one keyed by { x: 1, y: 2 }: its stored
    // `value` is "c" and its `loc` is the struct we selected by.
    // Canonical wire (Annex A): an `int` projects as a decimal string.
    assert_eq!(
        response.to_wire(),
        serde_json::json!({ "loc": { "x": "1", "y": "2" }, "value": "c" }),
        "the struct-key selector returns the row whose key equals the operand"
    );

    // And the selected row is gone; the other three remain.
    assert_eq!(cell_count(&engine), 3, "the selected struct-keyed row was removed");

    // The selector resolves by struct-key equality: a key no row carries selects
    // zero rows, which the single-row `[@k]` selector rejects (§6.3) — proving the
    // match is by the struct value, not a blanket pass-through.
    let mut g = generator();
    let miss = engine
        .call(&CallRequest::new("take").arg("k", loc(9, 9)), &mut g)
        .expect("call take (absent)");
    assert!(
        miss.rejection().is_some(),
        "selecting an absent struct key selects no row and is rejected, got {miss:?}"
    );
    assert_eq!(cell_count(&engine), 3, "an absent struct-key selection changes no rows");
}
