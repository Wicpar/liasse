//! Package decimal-division semantics (A.6). Division computes the quotient to
//! an internal rounding precision of at least sixteen *significant* fractional
//! digits, following PostgreSQL `numeric`. "Significant" counts from the
//! quotient's first nonzero fractional digit, so a quotient with leading
//! fractional zeros (e.g. `1/700000 = 0.00000142857…`) gets extra precision
//! rather than losing it. The value is rounded half-away-from-zero and then
//! normalized to its minimal-scale canonical spelling (A.1/SPEC-ISSUES item 1),
//! which subsumes any operand-display-scale floor — a terminating quotient such
//! as `10 / 4` is `2.5`, never zero-padded.

use liasse_value::bigdecimal::{BigDecimal, RoundingMode, Zero};
use liasse_value::Decimal;

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
    let scale = division_scale(&raw);
    Ok(raw.with_scale_round(scale, RoundingMode::HalfUp).normalized())
}

/// The A.6 internal rounding precision for a quotient: enough fractional places
/// to expose sixteen significant fractional digits of `raw`, but bounded above by
/// the implementation scale limit. The operand display scale imposes no floor —
/// minimal-scale rendering (A.1/SPEC-ISSUES item 1) subsumes it, since the result
/// is normalized after rounding. `raw` is the exact (high-precision) quotient,
/// read only to locate its leading fractional zeros.
fn division_scale(raw: &BigDecimal) -> i64 {
    let scale = if raw.is_zero() {
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
    // A.6/A.7: "that internal rounding precision is bounded by the implementation
    // limits ... the same scale bound A.7 applies to a `json` number." So the
    // rounding precision is CAPPED at `Decimal::MAX_SCALE_MAGNITUDE` (the ceiling
    // `Decimal::parse` and A.7 enforce), never rounded past it. A quotient whose
    // most-significant digit sits below place 10^-limit — a tiny dividend over a
    // huge divisor, `1 / 3E16383` — would otherwise emit a scale (16399) the value
    // domain forbids on the wire; capping the precision keeps the emitted value
    // admissible (here it rounds to 0) instead of producing an out-of-range
    // decimal. `SIGNIFICANT_FRACTIONAL_DIGITS` (16) is far under the limit, so a
    // terminating quotient like `10 / 4` = `2.5` is unaffected.
    let limit = i64::try_from(Decimal::MAX_SCALE_MAGNITUDE).unwrap_or(i64::MAX);
    scale.min(limit)
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
