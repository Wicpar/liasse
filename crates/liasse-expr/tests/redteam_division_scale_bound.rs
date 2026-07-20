#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! FINDING (A.6 / A.7): decimal division emits a quotient whose canonical scale
//! (fractional-digit count) EXCEEDS the pinned implementation scale bound of
//! 16383 digits, even though A.6 caps a quotient's internal rounding precision at
//! exactly that bound.
//!
//! A.6 (verbatim): "a non-terminating quotient is computed to an internal
//! rounding precision of at least sixteen significant fractional digits ... that
//! internal rounding precision is bounded by the implementation limits defined
//! for the Liasse language version — the same scale bound A.7 applies to a `json`
//! number".
//!
//! A.7 bounds a number's scale magnitude by "the same implementation scale limit
//! as an A.6 `decimal`", and this crate pins that limit to
//! `Decimal::MAX_SCALE_MAGNITUDE == 16383` (`liasse-value/src/decimal.rs`), the
//! same boundary `Decimal::parse` enforces on wire input. So a quotient's emitted
//! scale MUST be <= 16383: a value with more fractional digits than that is
//! rejected on the wire (A.7) and refused by `Decimal::parse`.
//!
//! But `eval/decimal.rs::division_scale` caps the rounding scale at
//! `i64::from(u16::MAX) == 65535`, four times the real limit. A quotient whose
//! most-significant digit sits far below the decimal point (a small dividend over
//! a huge divisor) is therefore rounded to a scale between 16384 and 65535 —
//! producing a decimal the value domain forbids and that could never be admitted
//! from the wire.
//!
//! Root cause: `crates/liasse-expr/src/eval/decimal.rs::division_scale` (~L48) —
//! the guard `if scale > i64::from(u16::MAX)` uses 65535 instead of
//! `Decimal::MAX_SCALE_MAGNITUDE` (16383).

mod common;

use common::{as_scalar, eval, row_type, scalar, scell, vdec, FixedEnv, FixedScope};
use liasse_expr::{Cell, ExprType};
use liasse_value::{Decimal, Type, Value};

/// A root struct exposing two decimal fields `a` and `b`, with the given wire
/// texts, evaluated as `.a / .b`.
fn quotient_scale(a_text: &str, b_text: &str) -> usize {
    let root_ty = row_type(
        vec![("a", scalar(Type::Decimal)), ("b", scalar(Type::Decimal))],
        None,
    );
    let scope = FixedScope::new(ExprType::Row(root_ty));
    let root = common::keyless_row(
        0,
        vec![("a", scell(vdec(a_text))), ("b", scell(vdec(b_text)))],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);
    let result = eval(&scope, &env, &dot, ".a / .b");
    let text = match as_scalar(&result) {
        Value::Decimal(d) => d.to_canonical_text(),
        other => panic!("quotient is not a decimal: {other:?}"),
    };
    // The canonical scale is the count of digits after the decimal point (0 when
    // the value is integral). A.1 renders in minimal-scale plain form, so this is
    // the value's true scale magnitude.
    text.split_once('.').map_or(0, |(_, frac)| frac.chars().count())
}

/// CONTROL — an ordinary non-terminating quotient `1 / 3 = 0.3333...` rounds to
/// sixteen significant fractional digits, well within the bound. This proves the
/// division path and the scale measurement are sound.
#[test]
fn ordinary_quotient_scale_is_within_bound() {
    let scale = quotient_scale("1", "3");
    assert!(
        scale as u64 <= Decimal::MAX_SCALE_MAGNITUDE,
        "1/3 must round within the {}-digit bound, got scale {scale}",
        Decimal::MAX_SCALE_MAGNITUDE,
    );
    // Externally deduced: sixteen significant fractional digits for a magnitude-<1
    // quotient starting at the first decimal place.
    assert_eq!(scale, 16, "1/3 rounds to sixteen significant fractional digits");
}

/// FINDING — dividing `1` by `3E16383` (a divisor at the magnitude boundary, so
/// admissible via `Decimal::parse`) yields a non-terminating quotient near
/// `3.33e-16384`. A.6 bounds the emitted rounding scale by the pinned 16383-digit
/// limit, so the canonical result MUST have at most 16383 fractional digits.
///
/// This test FAILS against the current implementation, which rounds to scale
/// `16 - 1 - e = 16399` (with `e = -16384`) — 16 digits past the bound —
/// producing a decimal `Decimal::parse` would itself reject as out of range.
#[test]
fn quotient_scale_must_not_exceed_pinned_bound() {
    // The divisor is admissible: its normalized magnitude is exactly the bound.
    assert!(
        Decimal::parse("3E16383").is_ok(),
        "3E16383 is at the magnitude boundary and must parse",
    );
    let scale = quotient_scale("1", "3E16383");
    assert!(
        scale as u64 <= Decimal::MAX_SCALE_MAGNITUDE,
        "A.6/A.7: a quotient's scale must not exceed the pinned {}-digit limit, \
         but 1 / 3E16383 emitted scale {scale} (division_scale caps at u16::MAX = 65535, \
         not MAX_SCALE_MAGNITUDE)",
        Decimal::MAX_SCALE_MAGNITUDE,
    );
}
