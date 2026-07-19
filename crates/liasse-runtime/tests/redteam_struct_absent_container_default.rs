#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM finding — §5.5 empty set/map default is NOT applied to an omitted
//! container member of a §5.3 static STRUCT (a follow-on gap of the #29 empty-map
//! default fix, 4457b44).
//!
//! # The rule
//!
//! SPEC.md §5.5 (line 493): "When a containing row **or struct** is created, an
//! omitted child set or keyed collection starts empty; an omitted non-optional
//! `map<K, V>` field likewise starts as the **empty map**, never `none` — the
//! set-analogous default, so the field's declared shape holds in every committed
//! state." §22.1 requires the declared shape to hold in every committed state.
//!
//! # The gap
//!
//! The #29 fix taught `rules::absent_value` (crates/liasse-runtime/src/rules.rs)
//! to default an omitted non-optional `set`/`map` field to the empty container, and
//! `apply_defaults` applies it to every declared `collection.fields` entry through a
//! final absent-fill loop.
//!
//! A §5.3 static struct member is compiled into `collection.structs`, NOT
//! `collection.fields`, and is built by a SEPARATE path, `Interp::struct_value`
//! (crates/liasse-runtime/src/interp.rs). That path applied each struct member's
//! EXPLICIT default but had **no `absent_value` fill** — the second loop
//! `apply_defaults` runs was simply missing. So a struct member that is a `set`/`map`
//! with no explicit default and no supplied value was left ABSENT from the struct,
//! not defaulted to the empty container. §5.5 names "row **or struct**" precisely to
//! forbid this; the fix adds `rules::complete_struct_containers`.
//!
//! ## Impact
//!
//! `meta: {}` over a struct `{ tags: set<text>, labels: map<text, text> }` committed
//! a struct whose declared `set`/`map` members were absent (project as `none`)
//! rather than empty — the declared shape did NOT hold (§22.1). A later `+`/`-` set
//! write or map-entry write then acts against `none` instead of the existing (empty)
//! membership, exactly the breakage the set-analogous default exists to prevent.
//! Whereas the identical omission on a TOP-LEVEL field of the same row correctly
//! yields the empty container — an asymmetry §5.5 explicitly rules out.
//!
//! The finding assertions state the §5.5-required empty containers; the top-level
//! controls prove the very same omission is handled correctly one level up,
//! isolating the gap to the struct path.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, Value, ViewResult};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

/// A row whose struct member and top-level fields all carry a non-optional
/// `set`/`map` with no explicit default, plus a struct-in-struct so the container
/// default's recursion into a nested static struct is exercised. `add` inserts a
/// row supplying only the key and an EMPTY struct initializer (with an empty nested
/// struct), so every container is omitted.
const MODEL: &str = r#"{
  "$liasse": 1,
  "$app": "redteam.structdefault@1.0.0",
  "$model": {
    "docs": {
      "$key": "id",
      "id": "text",
      "top_tags": "set<text>",
      "top_labels": "map<text, text>",
      "meta": { "tags": "set<text>", "labels": "map<text, text>", "inner": "{ nested_tags: set<text> }" }
    },
    "all": { "$view": ".docs { id, top_tags, top_labels, meta }" },
    "$mut": { "add": ".docs + { id: @id, meta: { inner: {} } }" }
  }
}"#;

/// Evaluate the `all` view and return its single row's projection.
fn one_row(engine: &Engine<MemoryStore>) -> ViewResult {
    let result = engine.view_at_head("all").expect("view evaluates").expect("declared view");
    assert_eq!(result.rows().len(), 1, "exactly one row was inserted");
    result
}

/// The projected `meta` struct value of the single row.
fn meta_of(result: &ViewResult) -> Value {
    let row = result.rows().first().expect("one row");
    row.field("meta").expect("meta struct is projected").clone()
}

/// Insert the single row (omitting every container) and return the engine.
fn seeded() -> Engine<MemoryStore> {
    let mut engine = load("structdefault", MODEL);
    let mut g = generator();
    let outcome = engine
        .call(&CallRequest::new("add").arg("id", Value::Text(Text::new("d1"))), &mut g)
        .expect("call runs");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits: {outcome:?}");
    engine
}

/// FINDING: §5.5 requires an omitted non-optional `set` member of a STRUCT to start
/// as the empty container (like a top-level field).
#[test]
fn omitted_set_member_of_a_struct_defaults_to_the_empty_set() {
    let engine = seeded();
    let result = one_row(&engine);
    let Value::Struct(meta) = meta_of(&result) else { panic!("meta is a struct") };
    match meta.get("tags") {
        Some(Value::Set(members)) => {
            assert!(members.is_empty(), "§5.5: an omitted struct set member starts EMPTY, got {members:?}")
        }
        other => panic!(
            "§5.5 line 493: an omitted non-optional `set` member of a struct MUST default to the \
             empty set (like a top-level field), but `meta.tags` is {other:?}"
        ),
    }
}

/// FINDING: the map half of the same gap — the direct follow-on of the #29 empty-map
/// default, which reached `collection.fields` but not the struct path.
#[test]
fn omitted_map_member_of_a_struct_defaults_to_the_empty_map() {
    let engine = seeded();
    let result = one_row(&engine);
    let Value::Struct(meta) = meta_of(&result) else { panic!("meta is a struct") };
    match meta.get("labels") {
        Some(Value::Map(entries)) => {
            assert!(entries.is_empty(), "§5.5: an omitted struct map member starts EMPTY, got {entries:?}")
        }
        other => panic!(
            "§5.5 line 493 / SPEC-ISSUES item 37: an omitted non-optional `map` member of a struct \
             MUST default to the empty map (like a top-level field), but `meta.labels` is {other:?}"
        ),
    }
}

/// FINDING (recursion): the container default reaches a struct-in-struct — an
/// omitted non-optional `set` member of a NESTED static struct also starts empty.
#[test]
fn omitted_set_member_of_a_nested_struct_defaults_to_the_empty_set() {
    let engine = seeded();
    let result = one_row(&engine);
    let Value::Struct(meta) = meta_of(&result) else { panic!("meta is a struct") };
    let Some(Value::Struct(inner)) = meta.get("inner") else {
        panic!("§5.5: a supplied nested struct `meta.inner` is present, got {:?}", meta.get("inner"))
    };
    match inner.get("nested_tags") {
        Some(Value::Set(members)) => assert!(
            members.is_empty(),
            "§5.5: an omitted set member of a nested struct starts EMPTY, got {members:?}"
        ),
        other => panic!(
            "§5.5 line 493: the container default MUST recurse into a nested static struct, but \
             `meta.inner.nested_tags` is {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// PASSING CONTROLS — the identical omission on a TOP-LEVEL field of the SAME row
// yields the empty container. This isolates the gap to the struct path and proves
// the harness reads container defaults faithfully.
// ---------------------------------------------------------------------------

/// CONTROL: a top-level `set` field omitted from the same insert defaults to the
/// empty set (the #29 fix's `apply_defaults` absent-fill).
#[test]
fn control_top_level_omitted_set_is_empty() {
    let engine = seeded();
    let result = one_row(&engine);
    let row = result.rows().first().expect("one row");
    match row.field("top_tags") {
        Some(Value::Set(members)) => assert!(members.is_empty(), "a top-level omitted set is empty"),
        other => panic!("a top-level omitted `set` field must default to the empty set, got {other:?}"),
    }
}

/// CONTROL: a top-level `map` field omitted from the same insert defaults to the
/// empty map (the #29 fix, SPEC-ISSUES item 37).
#[test]
fn control_top_level_omitted_map_is_empty() {
    let engine = seeded();
    let result = one_row(&engine);
    let row = result.rows().first().expect("one row");
    match row.field("top_labels") {
        Some(Value::Map(entries)) => assert!(entries.is_empty(), "a top-level omitted map is empty"),
        other => panic!("a top-level omitted `map` field must default to the empty map, got {other:?}"),
    }
}
