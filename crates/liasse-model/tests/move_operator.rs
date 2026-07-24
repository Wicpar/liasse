//! Move-operator type-checking (SPEC §8.5): the move operator `<-`/`->` consumes
//! its source binding, `=` copies and leaves the source live, a read of a
//! moved-from binding is a use-after-move rejection, and reassigning revives it.
//! Each expectation is derived from the §8.5 "Copy, move, and affine values" rule,
//! not from prior implementation output.

mod common;

use common::build;
use liasse_model::code;

/// Build a one-mutation package whose program is `statements` (a JSON array body),
/// over an `accounts` collection a local binding can select a row from.
fn model(statements: &str) -> String {
    format!(
        r#"{{
          "$liasse": 1,
          "$app": "t.move@1.0.0",
          "$model": {{
            "accounts": {{ "$key": "id", "id": "text", "balance": "int" }},
            "$mut": {{ "m": {statements} }}
          }}
        }}"#
    )
}

#[test]
fn move_from_consumes_the_source_binding() {
    // §8.5: `held <- x` transfers `x` and leaves it moved-from, so the later
    // `return x { id }` reads a moved-from binding — a use-after-move.
    let built = build(&model(
        r#"["x = .accounts[@id]", "held <- x", "return x { id }"]"#,
    ));
    assert!(built.has_code(code::MUTATION));
    assert!(
        built.rendered().contains("use-after-move"),
        "the read after a move must be a use-after-move rejection:\n{}",
        built.rendered()
    );
}

#[test]
fn move_to_spelling_consumes_the_source_the_same_way() {
    // §8.5: `x -> held` is the mirror spelling of `held <- x`; it consumes `x`
    // identically, so reading `x` afterwards is a use-after-move.
    let built = build(&model(
        r#"["x = .accounts[@id]", "x -> held", "return x { id }"]"#,
    ));
    assert!(built.has_code(code::MUTATION));
    assert!(
        built.rendered().contains("use-after-move"),
        "{}",
        built.rendered()
    );
}

#[test]
fn equals_copies_and_leaves_the_source_live() {
    // §8.5: `=` COPIES — a read (`held = x`) borrows and does not consume — so `x`
    // is still live for the later `return x { id }`. The program must load; if a
    // copy were wrongly treated as a move this would be a use-after-move.
    let built = build(&model(
        r#"["x = .accounts[@id]", "held = x", "return x { id }"]"#,
    ));
    built.expect_ok();
}

#[test]
fn a_plain_read_borrows_and_does_not_consume() {
    // §8.5 move-versus-borrow: reading `x` (here inside an `assert`) borrows it, so
    // a subsequent `return x { id }` still sees a live binding — no move occurred.
    let built = build(&model(
        r#"["x = .accounts[@id]", "assert(x.balance >= 0, 'check')", "return x { id }"]"#,
    ));
    built.expect_ok();
}

#[test]
fn reassigning_a_moved_binding_clears_the_error() {
    // §8.5: a moved-from binding is readable again once reassigned. Here `x` is
    // moved, then rebound, so the final `return x { id }` is a live read.
    let built = build(&model(
        r#"["x = .accounts[@id]", "held <- x", "x = .accounts[@id]", "return x { id }"]"#,
    ));
    built.expect_ok();
}

#[test]
fn a_move_source_must_be_a_local_binding() {
    // Honesty: moving out of stored state (a selector, a field path) is out of
    // scope until a move-only type exists, so it is refused LOUDLY rather than
    // silently copied under a move spelling.
    let built = build(&model(r#"["held <- .accounts[@id]"]"#));
    assert!(built.has_code(code::MUTATION));
    assert!(
        built
            .rendered()
            .contains("a move source must be a local binding"),
        "a non-binding move source must be rejected loudly:\n{}",
        built.rendered()
    );
}
