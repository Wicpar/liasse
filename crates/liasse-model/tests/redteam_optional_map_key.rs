#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! Red-team regression: an `optional<K>` map KEY shape is a STATIC error with a
//! rustc-like diagnostic — the symmetric sibling of the already-landed
//! optional-value and optional-set-element rejections.
//!
//! Annex A.1: `none` is absence in the Liasse type system, not a value. It can
//! never be a set member or a map value; by the same reasoning it can never be a
//! map KEY — an entry keyed on `none` is nonsensical, since `none` names the key's
//! absence rather than any value. `map_type` previously checked only the map VALUE
//! for `optional`, silently accepting `map<optional<K>, V>`. Enforcing the rule at
//! the type level (rather than only dropping a stored `none`) is the clean fix: the
//! offending shape cannot be declared. A `map` whose key is a plain type stays
//! valid; only a DIRECT `optional` key is the error.

mod common;

use common::build;

fn field_model(field: &str) -> String {
    format!(
        r#"{{ "$liasse": 1, "$app": "rt.optmapkey@1.0.0",
             "$model": {{ "docs": {{ "$key": "id", "id": "text", {field} }} }} }}"#
    )
}

#[test]
fn map_key_optional_string_form_is_rejected() {
    let built = build(&field_model(r#""index": "map<optional<text>, text>""#));
    let rendered = built.rendered();
    assert!(built.has_code("M-TYPE"), "a `map<optional<K>, V>` key shape is a type error (A.1):\n{rendered}");
    assert!(
        rendered.contains("map key") && rendered.contains("optional"),
        "A.1: the diagnostic must explain a map key type is never `optional<K>`:\n{rendered}"
    );
    // The fix advice is carried inline in the message, matching `map_type`'s
    // plain-string diagnostic style for the sibling rejections.
    assert!(
        rendered.contains("declare the key as"),
        "A.1: the diagnostic must tell the user how to fix it:\n{rendered}"
    );
}

#[test]
fn map_with_both_key_and_value_optional_reports_the_key_first() {
    // The key check runs before the value check, so a doubly-optional map surfaces
    // the map-key rejection (still a single M-TYPE type error either way).
    let built = build(&field_model(r#""index": "map<optional<text>, optional<text>>""#));
    let rendered = built.rendered();
    assert!(built.has_code("M-TYPE"), "a doubly-optional map is a type error (A.1):\n{rendered}");
    assert!(
        rendered.contains("map key"),
        "A.1: the map-key rejection is reported for an `optional` key:\n{rendered}"
    );
}

#[test]
fn plain_map_keys_still_load() {
    // Only a DIRECT `optional` key is the error; ordinary scalar key types remain
    // valid map keys, and a non-optional value alongside stays valid too.
    build(&field_model(r#""index": "map<text, text>""#)).expect_ok();
    build(&field_model(r#""index": "map<uuid, int>""#)).expect_ok();
}
