#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! The §6.5 `string` search predicates: `string.starts_with`,
//! `string.ends_with`, `string.contains`.
//!
//! Every expectation below is deduced from the §6.5 definitions alone —
//! `starts_with(s, n)` is true exactly when `s = n ++ r`, `ends_with(s, n)` when
//! `s = p ++ n`, `contains(s, n)` when `s = p ++ n ++ r`, over the Unicode
//! *scalar sequence* a `text` value is (Annex A.1), with no normalization,
//! folding, trimming, or locale — so no expected value here could only be
//! obtained by running the implementation.

mod common;

use common::{
    check_rejects, eval, keyless_row, row_type, scalar, scell, try_eval, vint, vtext, FixedEnv,
    FixedScope,
};
use liasse_diag::Diagnostics;
use liasse_expr::{Cell, EvalError, ExprType};
use liasse_value::{Type, Value};

/// A row exposing the text field the cases search over, plus an absent
/// `optional<text>` (`parent`) and a non-text `count`.
fn scope() -> FixedScope {
    FixedScope::new(ExprType::Row(row_type(
        vec![
            ("code", scalar(Type::Text)),
            ("parent", scalar(Type::Optional(Box::new(Type::Text)))),
            ("count", scalar(Type::Int)),
        ],
        None,
    )))
}

fn env() -> FixedEnv {
    FixedEnv::new(keyless_row(
        1,
        vec![
            ("code", scell(vtext("512100"))),
            ("parent", scell(Value::None)),
            ("count", scell(vint(3))),
        ],
    ))
}

/// Evaluate `source` against the fixture row and return the boolean it answered.
fn verdict(source: &str) -> bool {
    let scope = scope();
    let env = env();
    let dot = Cell::Row(Box::new(env.root.clone()));
    match eval(&scope, &env, &dot, source) {
        Cell::Scalar(Value::Bool(verdict)) => verdict,
        other => panic!("expected a bool from `{source}`, got {other:?}"),
    }
}

/// Evaluate `source`, expecting the evaluation to be refused.
fn refusal(source: &str) -> EvalError {
    let scope = scope();
    let env = env();
    let dot = Cell::Row(Box::new(env.root.clone()));
    match try_eval(&scope, &env, &dot, source) {
        Ok(cell) => panic!("expected `{source}` to be refused, got {cell:?}"),
        Err(error) => error,
    }
}

fn mentions(diags: &Diagnostics, needle: &str) -> bool {
    diags.iter().any(|d| d.message().contains(needle))
}

#[test]
fn prefix_suffix_and_substring_decompose_the_subject() {
    // §6.5: "512100" = "512" ++ "100" = "5121" ++ "00" = "51" ++ "21" ++ "00".
    assert!(verdict("string.starts_with('512100', '512')"));
    assert!(verdict("string.ends_with('512100', '100')"));
    assert!(verdict("string.contains('512100', '21')"));
    // A prefix that is not one: "512100" has no decomposition `"513" ++ r`.
    assert!(!verdict("string.starts_with('512100', '513')"));
    // A suffix that is not one, and an absent infix.
    assert!(!verdict("string.ends_with('512100', '101')"));
    assert!(!verdict("string.contains('512100', '2 11')"));
    // A prefix is a substring but generally not a suffix.
    assert!(verdict("string.contains('512100', '512')"));
    assert!(!verdict("string.ends_with('512100', '512')"));
}

#[test]
fn empty_needle_holds_for_every_subject_including_the_empty_one() {
    // §6.5: the empty sequence decomposes every subject (`s = '' ++ s`).
    assert!(verdict("string.starts_with('512100', '')"));
    assert!(verdict("string.ends_with('512100', '')"));
    assert!(verdict("string.contains('512100', '')"));
    assert!(verdict("string.starts_with('', '')"));
    assert!(verdict("string.ends_with('', '')"));
    assert!(verdict("string.contains('', '')"));
}

#[test]
fn needle_longer_than_subject_never_matches() {
    // §6.5: a longer needle admits no decomposition, so all three are false.
    assert!(!verdict("string.starts_with('512', '512100')"));
    assert!(!verdict("string.ends_with('512', '512100')"));
    assert!(!verdict("string.contains('512', '512100')"));
    // The empty subject is the extreme case of the same rule.
    assert!(!verdict("string.starts_with('', '5')"));
    assert!(!verdict("string.ends_with('', '5')"));
    assert!(!verdict("string.contains('', '5')"));
}

#[test]
fn a_subject_is_its_own_prefix_suffix_and_substring() {
    // §6.5: `n = s` decomposes with an empty `p`/`r`.
    assert!(verdict("string.starts_with('512100', '512100')"));
    assert!(verdict("string.ends_with('512100', '512100')"));
    assert!(verdict("string.contains('512100', '512100')"));
}

#[test]
fn comparison_is_over_scalars_without_normalization() {
    // §6.5/Annex A.1: text is preserved exactly, so an NFC needle does not match
    // an NFD subject: "cafe\u{301}s" (5 scalars) against the needle "caf\u{e9}".
    assert!(!verdict("string.starts_with('cafe\u{301}s', 'caf\u{e9}')"));
    assert!(verdict("string.starts_with('caf\u{e9}s', 'caf\u{e9}')"));
    // The NFD subject does match its own NFD prefix — nothing is renormalized in
    // either direction.
    assert!(verdict("string.starts_with('cafe\u{301}s', 'cafe\u{301}')"));
    // A bare combining acute is a scalar like any other: it is the fifth scalar
    // of the NFD form, so it is contained but is not a prefix.
    assert!(verdict("string.contains('cafe\u{301}s', '\u{301}')"));
    assert!(!verdict("string.starts_with('cafe\u{301}s', '\u{301}')"));
}

#[test]
fn multibyte_scalars_never_match_at_a_byte_boundary() {
    // §6.5: the predicates agree with a byte search because UTF-8 is
    // self-synchronizing and injective. "ü" is U+00FC = 0xC3 0xBC and "é" is
    // U+00E9 = 0xC3 0xA9: they share a lead byte but no scalar, so neither
    // contains the other and no match starts inside a scalar.
    assert!(!verdict("string.contains('\u{fc}', '\u{e9}')"));
    assert!(!verdict("string.starts_with('\u{fc}\u{e9}', '\u{e9}')"));
    assert!(verdict("string.ends_with('\u{fc}\u{e9}', '\u{e9}')"));
    // An astral scalar (U+1F600, four UTF-8 bytes) behaves the same way.
    assert!(verdict("string.starts_with('\u{1f600}a', '\u{1f600}')"));
    assert!(!verdict("string.starts_with('a\u{1f600}', '\u{1f600}')"));
    assert!(verdict("string.contains('a\u{1f600}b', '\u{1f600}')"));
}

#[test]
fn neither_case_nor_whitespace_is_folded_away() {
    // §6.5: no case folding and no trimming happen implicitly.
    assert!(!verdict("string.starts_with('Maße', 'MASSE')"));
    assert!(!verdict("string.starts_with(' 512', '512')"));
    assert!(verdict("string.contains(' 512', '512')"));
    // The case-insensitive test composes explicitly; the Unicode default full
    // fold maps ß to "ss" and MASSE to "masse", so the folded subject "maße"
    // — folded further to "masse" — starts with the folded needle.
    assert!(verdict(
        "string.starts_with(string.casefold('Maße'), string.casefold('MASSE'))"
    ));
    // The trimmed test likewise composes.
    assert!(verdict("string.starts_with(string.trim(' 512'), '512')"));
}

#[test]
fn an_absent_optional_argument_is_refused_not_answered_false() {
    // §6.5: an absent (`none`) argument is a typed evaluation error, never a
    // plausible `false`. The fixture row's `parent` is `none`.
    let error = refusal("string.starts_with(.code, .parent)");
    assert!(
        error.message().contains("text"),
        "the refusal must name the text argument it could not compare, got: {}",
        error.message()
    );
    // Symmetric in the subject position, and a non-text argument is refused the
    // same way rather than compared by some other rule.
    let _ = refusal("string.ends_with(.parent, .code)");
    let _ = refusal("string.contains(.code, .count)");
}

#[test]
fn a_guarded_optional_never_reaches_the_predicate() {
    // §6.5/Annex A.6: `||` does not evaluate an unreached right operand, so the
    // guarded form a chart-of-accounts parent invariant uses is total.
    assert!(verdict("!has(.parent) || string.starts_with(.code, .parent)"));
    // Coalescing is the other spelling: the empty needle matches everything.
    assert!(verdict("string.starts_with(.code, .parent ?? '')"));
}

#[test]
fn a_wrong_argument_count_is_rejected_at_load() {
    // §6.5: the arity is part of the signature package loading validates.
    for source in [
        "string.starts_with('512100')",
        "string.ends_with('512100')",
        "string.contains('512100')",
        "string.starts_with('512100', '512', '1')",
        // The one-argument utilities are pinned the same way.
        "string.trim('a', 'b')",
        "string.lower()",
    ] {
        let diags = check_rejects(&scope(), source);
        assert!(
            mentions(&diags, "argument(s), but"),
            "`{source}` must be rejected for its arity"
        );
    }
}

#[test]
fn the_predicates_are_typed_bool() {
    // §6.5: the result type is `bool`, so a predicate composes with the logical
    // operators and has no type where a `text` operand is required.
    assert!(verdict(
        "string.starts_with('512100', '512') && !string.ends_with('512100', '512')"
    ));
    let diags = check_rejects(&scope(), ".code + string.contains('a', 'a')");
    assert!(mentions(&diags, "no type for operands"));
}

#[test]
fn an_unknown_string_function_is_still_rejected() {
    // §6.5: adding to the roster must not open the namespace to any name.
    let diags = check_rejects(&scope(), "string.startswith('512100', '512')");
    assert!(mentions(&diags, "unknown function"));
}
