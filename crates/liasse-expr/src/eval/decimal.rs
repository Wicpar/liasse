//! Package decimal-division semantics (A.6). Division selects a result scale
//! that exposes at least sixteen *significant* fractional digits of the
//! quotient — never fewer than either operand's display scale — following
//! PostgreSQL `numeric`. "Significant" counts from the quotient's first nonzero
//! fractional digit, so a quotient with leading fractional zeros (e.g.
//! `1/700000 = 0.00000142857…`) gets extra scale rather than losing precision.
//! The value is then rounded half-away-from-zero and its trailing-zero spelling
//! normalized (the spelling itself is unpinned — SPEC-ISSUES item 1).

use liasse_value::bigdecimal::{BigDecimal, RoundingMode, Zero};

use crate::error::EvalError;

/// The count of significant fractional digits A.6 requires of a quotient.
const SIGNIFICANT_FRACTIONAL_DIGITS: i64 = 16;

/// Divide `a` by `b` under the package decimal semantics (A.6). Callers reject
/// division by zero before calling; a zero `b` here is a caller bug and yields
/// [`EvalError::DivisionByZero`] rather than panicking.
pub(crate) fn divide(a: &BigDecimal, b: &BigDecimal) -> Result<BigDecimal, EvalError> {
    if b.is_zero() {
        return Err(EvalError::DivisionByZero);
    }
    let raw = a / b;
    let scale = division_scale(&raw, a, b)?;
    Ok(raw.with_scale_round(scale, RoundingMode::HalfUp).normalized())
}

/// The A.6 result scale for a quotient: enough fractional places to expose
/// sixteen significant fractional digits of `raw`, but never fewer than either
/// operand's display scale. `raw` is the exact (high-precision) quotient, read
/// only to locate its leading fractional zeros.
fn division_scale(raw: &BigDecimal, a: &BigDecimal, b: &BigDecimal) -> Result<i64, EvalError> {
    let significant = if raw.is_zero() {
        SIGNIFICANT_FRACTIONAL_DIGITS
    } else {
        // e = floor(log10(|raw|)) is the place of the most significant digit
        // (0 for 1..10, -1 for 0.1..1, -6 for 1e-6..). The first significant
        // fractional digit sits at place min(e, -1); sixteen of them from there
        // reach scale SIGNIFICANT_FRACTIONAL_DIGITS - 1 - e, floored at
        // SIGNIFICANT_FRACTIONAL_DIGITS for quotients of magnitude >= 1.
        let e = decimal_exponent(raw);
        SIGNIFICANT_FRACTIONAL_DIGITS.max(SIGNIFICANT_FRACTIONAL_DIGITS - 1 - e)
    };
    let scale = significant
        .max(a.fractional_digit_count())
        .max(b.fractional_digit_count());
    if scale > i64::from(u16::MAX) {
        return Err(EvalError::DecimalScale);
    }
    Ok(scale)
}

/// `floor(log10(|value|))` for a nonzero decimal: the place of its most
/// significant digit. `value = mantissa * 10^-scale` with `d` mantissa digits
/// lies in `[10^(d-1-scale), 10^(d-scale))`, so the exponent is `d - 1 - scale`
/// — independent of any trailing zeros, which raise `d` and `scale` together.
fn decimal_exponent(value: &BigDecimal) -> i64 {
    let (_, scale) = value.as_bigint_and_exponent();
    let digits = value.digits() as i64;
    digits - 1 - scale
}
