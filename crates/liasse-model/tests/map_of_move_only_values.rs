//! A map whose `$value` type is move-only (SPEC §5.4, §8.5, §13.16).
//!
//! §5.4: "That per-entry ownership slot is what lets a map value be a move-only
//! value (§8.5) such as a `module` (§13.16): each entry owns its value." Two
//! consequences follow, and both are load-time facts:
//!
//! - A map of `module` DECLARES. The value slot is a real row member, so it holds
//!   an affine value the way any owning slot does — nothing about the map form
//!   weakens the ownership.
//! - `m { $value }` over a move-only value type does NOT. The projection builds a
//!   SET of the values, which would duplicate one handle per entry; §5.4 makes
//!   that a static error rather than a silent copy. `m { $key }` is unaffected —
//!   a key is data, not an owned handle.
//!
//! Reading one entry's value (`m[k].$value`) BORROWS it (§8.5), so it stays legal;
//! the lifecycle operators that MOVE it are §13.16's and are not built here.

mod common;

use common::build;

/// A package whose `slots` map holds `value_type`, plus `views` (JSON members).
fn model(value_type: &str, views: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.mapmove@1.0.0",
          "$model": {{
            "slots": {{ "$key": "text", "$value": "{value_type}" }}
            {views}
          }}
        }}"#
    )
}

#[test]
fn a_map_of_modules_declares() {
    // §5.4: a map's `$value` may be any present value type, including a move-only
    // one — the entry row IS the ownership slot.
    build(&model("module", "")).expect_ok();
}

#[test]
fn the_key_projection_over_a_move_only_map_still_loads() {
    // §5.4: `{ $key }` reads the entry keys, which are data — the value type's
    // affinity has nothing to say about them.
    build(&model("module", r#", "names": { "$view": ".slots { $key }" }"#)).expect_ok();
}

#[test]
fn the_value_projection_over_a_move_only_map_is_rejected() {
    // §5.4/§8.5: `{ $value }` collapses the entries to a SET of their values, so a
    // move-only value type would be copied once per entry. Loud static error, not
    // a silent duplication of a handle the type forbids duplicating.
    let built = build(&model("module", r#", "all": { "$view": ".slots { $value }" }"#));
    let rendered = built.rendered();
    assert!(
        built.has_code("E-EXPR"),
        "a `{{ $value }}` set of a move-only value is a static error; codes: {:?}\n{rendered}",
        built.codes()
    );
    assert!(
        rendered.contains("move-only"),
        "§8.5: the diagnostic must say the value is move-only:\n{rendered}"
    );
}

#[test]
fn the_value_projection_over_a_copyable_map_loads() {
    // The control: the same projection over a copyable value type is ordinary.
    build(&model("text", r#", "all": { "$view": ".slots { $value }" }"#)).expect_ok();
}

#[test]
fn reading_one_entrys_move_only_value_borrows_it() {
    // §5.4/§8.5: `.$value` READS the entry's value; a read borrows and never
    // transfers, so it is legal even for a move-only value type.
    build(&model(
        "module",
        r#", "one": { "$view": ".slots[:e] { k: e.$key, held: e.$value }" }"#,
    ))
    .expect_ok();
}
