#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Operator and arithmetic semantics with externally-computed expectations
//! (§6, Annex A.6, Annex B). Expected values are derived from the spec text and
//! independent computation (`bc`), never from the implementation's own output.

mod common;

use common::{as_scalar, check, eval, try_eval, vbig, vdec, vint, vtext, FixedEnv, FixedScope};
use liasse_expr::{Cell, EvalError, ExprType};
use liasse_value::{Type, Value};

fn scope() -> FixedScope {
    FixedScope::new(ExprType::scalar(Type::Int))
}

fn dot() -> Cell {
    Cell::Scalar(vint(0))
}

fn run(source: &str) -> Value {
    let scope = scope();
    let env = FixedEnv::new(common::keyless_row(0, vec![]));
    as_scalar(&eval(&scope, &env, &dot(), source))
}

#[test]
fn int_addition_is_arbitrary_precision() {
    // i64::MAX + 1, verified with bc: 9223372036854775808.
    assert_eq!(run("9223372036854775807 + 1"), vbig("9223372036854775808"));
    // (i64::MAX)^2 = 85070591730234615847396907784232501249.
    assert_eq!(
        run("9223372036854775807 * 9223372036854775807"),
        vbig("85070591730234615847396907784232501249")
    );
}

#[test]
fn integer_division_truncates_toward_zero() {
    // A.6: integer division truncates toward zero, not floors.
    assert_eq!(run("-7 / 2"), vint(-3));
    assert_eq!(run("7 / -2"), vint(-3));
    assert_eq!(run("7 / 2"), vint(3));
}

#[test]
fn integer_remainder_takes_dividend_sign() {
    // SPEC-ISSUES item 3 documented choice: remainder follows the dividend.
    assert_eq!(run("-7 % 2"), vint(-1));
    assert_eq!(run("7 % -2"), vint(1));
}

#[test]
fn decimal_remainder_is_the_remainder_not_the_quotient() {
    // SPEC-ISSUES item 3 extends the truncated-toward-zero choice to decimals:
    // `%` is the exact remainder `a - trunc(a/b)*b`, carrying the dividend sign,
    // NOT the division result. 5.5 % 2.0 = 1.5 (bc: trunc(5.5/2.0)=2, 5.5-4.0).
    assert_eq!(run("5.5 % 2.0"), vdec("1.5"));
    // Dividend sign, verified with bc: trunc(-5.5/2.0) = -2, -5.5 - (-4.0).
    assert_eq!(run("-5.5 % 2.0"), vdec("-1.5"));
    // Exact even division leaves a zero remainder.
    assert_eq!(run("6.0 % 1.5"), vdec("0"));
}

#[test]
fn decimal_remainder_is_exact_at_large_magnitude() {
    // A.6: `%` is the EXACT remainder `a - trunc(a/b)*b` for every magnitude,
    // matching PostgreSQL `mod`; it must satisfy `(a/b)*b + (a%b) = a` and, for a
    // positive dividend, lie in `[0, |b|)`. It must NOT be computed by truncating
    // `a/b`: the `/` operator rounds the quotient to a bounded significant-digit
    // precision, so a quotient one ulp below an integer (a long trailing run of
    // 9s) rounds UP to the next integer and truncating it gives the wrong result.
    //
    // a = 3*10^100 - 1 = "2" then one hundred "9"; b = 10^100 = "1" then one
    // hundred "0". a = 2*b + (b - 1) and b - 1 < b, so a % b = b - 1 = 10^100 - 1
    // = one hundred "9" (derived by hand: 10^100 - 1). The intermediate quotient
    // a/b = 2.999...9 (one hundred 9s) rounds up to 3 under `/`, so a
    // truncate-the-quotient reading would wrongly yield a - 3*b = -1.
    let a = format!("2{}", "9".repeat(100));
    let b = format!("1{}", "0".repeat(100));
    let expected = "9".repeat(100);
    assert_eq!(run(&format!("{a}.0 % {b}.0")), vdec(&expected));
    // Dividend sign is preserved (truncated division): -(3*10^100-1) % 10^100 =
    // -(10^100 - 1) = negative one hundred "9".
    assert_eq!(run(&format!("-{a}.0 % {b}.0")), vdec(&format!("-{expected}")));

    // A smaller run (30 nines) that already exceeds the display scale but stays
    // within the quotient's significant-digit budget, and a mixed-scale case.
    let a30 = format!("2{}", "9".repeat(30));
    let b30 = format!("1{}", "0".repeat(30));
    assert_eq!(run(&format!("{a30}.0 % {b30}.0")), vdec(&"9".repeat(30)));
    // 100.5 % 0.7: bc gives trunc(100.5/0.7)=143, 100.5 - 143*0.7 = 100.5 - 100.1.
    assert_eq!(run("100.5 % 0.7"), vdec("0.4"));
}

#[test]
fn decimal_add_sub_mul_are_exact() {
    // A.6: decimal +,-,* are exact — no binary-float drift.
    assert_eq!(run("0.1 + 0.2"), vdec("0.3"));
    assert_eq!(run("0.1 * 0.1"), vdec("0.01"));
    assert_eq!(run("1.50 - 0.50"), vdec("1.0"));
}

#[test]
fn decimal_division_selects_scale_and_normalizes() {
    // 10.0 / 4.0 = 2.5 exactly; item 1 choice normalizes trailing zeros.
    assert_eq!(run("10.0 / 4.0"), vdec("2.5"));
    // 1/3 rounded half-away-from-zero at 16 fractional digits.
    assert_eq!(run("1.0 / 3.0"), vdec("0.3333333333333333"));
}

#[test]
fn division_by_zero_is_a_typed_error_not_a_panic() {
    let scope = scope();
    let env = FixedEnv::new(common::keyless_row(0, vec![]));
    assert_eq!(
        try_eval(&scope, &env, &dot(), "1 / 0"),
        Err(EvalError::DivisionByZero)
    );
    assert_eq!(
        try_eval(&scope, &env, &dot(), "5.0 / 0.0"),
        Err(EvalError::DivisionByZero)
    );
}

#[test]
fn text_concatenation_with_plus() {
    assert_eq!(run(r#""ab" + "cd""#), vtext("abcd"));
}

#[test]
fn comparisons_use_total_order_and_promote_numerics() {
    assert_eq!(run("2 == 2"), Value::Bool(true));
    assert_eq!(run("1 < 2"), Value::Bool(true));
    // int/decimal compare numerically after promotion, not by type rank.
    assert_eq!(run("2 == 2.0"), Value::Bool(true));
    assert_eq!(run("3 > 2.5"), Value::Bool(true));
    assert_eq!(run(r#""a" < "b""#), Value::Bool(true));
}

#[test]
fn logical_operators_short_circuit() {
    // `false && (1/0 == 0)` must not evaluate the divisor and must be false.
    let scope = scope();
    let env = FixedEnv::new(common::keyless_row(0, vec![]));
    assert_eq!(
        try_eval(&scope, &env, &dot(), "false && (1 / 0 == 0)"),
        Ok(Cell::Scalar(Value::Bool(false)))
    );
    assert_eq!(
        try_eval(&scope, &env, &dot(), "true || (1 / 0 == 0)"),
        Ok(Cell::Scalar(Value::Bool(true)))
    );
}

#[test]
fn checked_expression_is_typed_int() {
    // A well-typed sum reports its result type without evaluating.
    let typed = check(&scope(), "1 + 2");
    assert_eq!(typed.ty(), &ExprType::scalar(Type::Int));
}
