//! The `module` value is move-only at load time (SPEC §8.5/§13.16): `=`-copying a
//! module value is a static error, while a program that carries a `module`-typed
//! parameter without copying it loads. These derive from the §8.5 copy rule now
//! that `module` is the real move-only value type — the enforcement in
//! `reject_copy_of_move_only` fires for it, not merely a synthetic marker.
//!
//! (Moving a module INTO a slot, `unpack`, and dispatch through a module value are
//! later pieces; this piece proves the type is move-only.)

mod common;

use common::build;
use liasse_model::code;

/// A one-mutation package whose mutation `m` takes a `module` parameter `mod` and
/// whose body is `statements` (a JSON array).
fn model(statements: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.modmove@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text" }},
            "$mut": {{ "m({{ mod: module }})": {statements} }}
          }}
        }}"#
    )
}

#[test]
fn copying_a_module_with_equals_is_rejected() {
    // §8.5: `=` COPIES, and a module is move-only, so `x = @mod` is a static error.
    let built = build(&model(r#"["x = @mod"]"#));
    assert!(built.has_code(code::MUTATION), "codes: {:?}", built.codes());
    assert!(
        built.rendered().contains("cannot copy a move-only value"),
        "copying a module with `=` must be rejected as a move-only copy:\n{}",
        built.rendered()
    );
}

#[test]
fn a_module_parameter_is_accepted_when_not_copied() {
    // A `module`-typed parameter is a first-class type: a program that declares one
    // and does not copy it loads. (A borrow/dispatch through it arrives in a later
    // piece; here the point is the type parses and threads through the checker.)
    let built = build(&model(r#"["return .accounts { id }"]"#));
    built.expect_ok();
}
