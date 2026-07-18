#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team regression: an `optional<T>` set-element / map-value shape is a
//! STATIC error with a rustc-like diagnostic.
//!
//! SPEC.md §5.5 (line 489): "the member shape of a set is never `optional<T>`."
//! Annex A.1 (line 4400): "A map never stores a `none` value; absence is the key
//! not being present." `none` is absence, not a value, so it can never be a set
//! member or a map value. Enforcing that at the type level (rather than only
//! dropping a stored `none`) is the clean fix: the offending shape cannot be
//! declared. This test pins the rejection and its diagnostic quality; a `map`/`set`
//! of a STRUCT that merely carries an optional member stays valid (only a DIRECT
//! `optional` element / value is the error).
//!
//! Diagnostics are inspected at the model layer, where the message is accessible
//! (the runtime `Engine::load` surfaces only a summary error).

mod common;

use common::build;

fn field_model(field: &str) -> String {
    format!(
        r#"{{ "$liasse": 1, "$app": "rt.optinner@1.0.0",
             "$model": {{ "docs": {{ "$key": "id", "id": "text", {field} }} }} }}"#
    )
}

#[test]
fn map_value_optional_string_form_is_rejected() {
    let built = build(&field_model(r#""meta": "map<text, optional<text>>""#));
    let rendered = built.rendered();
    assert!(built.has_code("M-TYPE"), "a `map<K, optional<V>>` value shape is a type error (A.1):\n{rendered}");
    assert!(
        rendered.contains("map value") && rendered.contains("optional"),
        "A.1 line 4400: the diagnostic must explain a map value type is never `optional<V>`:\n{rendered}"
    );
}

#[test]
fn set_element_optional_string_form_is_rejected() {
    let built = build(&field_model(r#""tags": "set<optional<text>>""#));
    let rendered = built.rendered();
    assert!(built.has_code("M-TYPE"), "a `set<optional<T>>` element shape is a type error (§5.5):\n{rendered}");
    assert!(
        rendered.contains("set element") && rendered.contains("optional"),
        "§5.5 line 489: the diagnostic must explain a set element type is never `optional<T>`:\n{rendered}"
    );
    // The fix advice ("declare the element as `T`, and a missing member expresses
    // absence") is carried inline in the message, matching `map_type`'s plain-string
    // diagnostic style.
    assert!(
        rendered.contains("declare the element as"),
        "§5.5 line 489: the diagnostic must tell the user how to fix it:\n{rendered}"
    );
}

#[test]
fn set_element_optional_inline_form_is_rejected() {
    // The inline `{ $set: "optional<text>" }` element resolves through
    // `scalar_shape` (build/shapes.rs `shape_or_type`), a distinct path from the
    // `set<optional<text>>` string form (which `map_type` catches). It needs its
    // own guard — pinned here.
    let built = build(&field_model(r#""tags": { "$set": "optional<text>" }"#));
    let rendered = built.rendered();
    assert!(built.has_code("M-TYPE"), "an inline `{{ $set: optional<T> }}` element is a type error (§5.5):\n{rendered}");
    assert!(
        rendered.contains("set element") && rendered.contains("optional"),
        "§5.5 line 489: the inline diagnostic must explain a set element type is never `optional<T>`:\n{rendered}"
    );
}

#[test]
fn map_and_set_of_a_struct_with_an_optional_member_still_load() {
    // Only a DIRECT `optional` element / value is the error. A `map`/`set` whose
    // element is a STRUCT that happens to carry an optional member is fine — the
    // element type is `Type::Struct`, not `Type::Optional`.
    build(&field_model(r#""tags": "set<{ a: text, b?: text }>""#)).expect_ok();
    build(&field_model(r#""meta": "map<text, { a: text, b?: text }>""#)).expect_ok();
    // And the plain non-optional collections remain valid.
    build(&field_model(r#""tags": "set<text>""#)).expect_ok();
    build(&field_model(r#""meta": "map<text, text>""#)).expect_ok();
}
