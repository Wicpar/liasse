//! `decimal` — exact base-10 value (A.1, A.6).

use core::cmp::Ordering;
use core::str::FromStr;

use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::BigInt;

use crate::error::ValueError;

/// An exact decimal.
///
/// The value keeps its *scale* (fractional digit count) because A.1 pins a
/// plain wire spelling; but B.1 orders decimals by mathematical value, so
/// `1.0` and `1.00` are equal *values* that nonetheless render to distinct
/// wire strings. [`Ord`]/[`Eq`] here are therefore scale-insensitive
/// (numeric), while [`Decimal::to_canonical_text`] preserves scale.
#[derive(Debug, Clone)]
pub struct Decimal(BigDecimal);

impl Decimal {
    /// The largest scale magnitude accepted from wire input, in digits.
    ///
    /// A decimal's scale is its power-of-ten exponent: a positive scale places
    /// that many fractional digits, a negative scale that many trailing zeros.
    /// [`to_canonical_text`](Self::to_canonical_text) materializes those digits,
    /// so an unbounded scale is a denial-of-service vector — `BigDecimal`
    /// happily parses `1E-2000000000` (scale two billion), whose plain form is a
    /// multi-gigabyte string. A.6 requires only "at least sixteen fractional
    /// digits" (SPEC.md line 4437) and bounds the result scale by an
    /// implementation limit (line 4438); the language version pins no exact
    /// value, so this crate picks a generous `2^14` — orders of magnitude beyond
    /// any real decimal yet far below any allocation hazard.
    pub const MAX_SCALE_MAGNITUDE: u64 = 1 << 14;

    /// Parse an exact decimal.
    ///
    /// The canonical output form is pinned by A.1 (plain, no exponent, one
    /// sign); the exact trailing-zero spelling is *not* pinned (SPEC-ISSUES
    /// item 1). We take the least-surprising "preserve the operation scale"
    /// reading: the parsed scale is retained and rendered verbatim, and
    /// exponent input is accepted then normalized to plain notation (item 2).
    ///
    /// The scale magnitude is bounded ([`MAX_SCALE_MAGNITUDE`](Self::MAX_SCALE_MAGNITUDE)):
    /// an adversarial wire value with an extreme exponent is rejected here, at
    /// the untrusted boundary, so it can never reach the canonical-text encoder
    /// and force an unbounded allocation.
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        let value =
            BigDecimal::from_str(text).map_err(|_| ValueError::MalformedDecimal(text.to_owned()))?;
        let magnitude = value.as_bigint_and_exponent().1.unsigned_abs();
        if magnitude > Self::MAX_SCALE_MAGNITUDE {
            return Err(ValueError::DecimalScaleOutOfRange {
                magnitude,
                limit: Self::MAX_SCALE_MAGNITUDE,
            });
        }
        Ok(Self(value))
    }

    /// Build from an already-typed [`BigDecimal`] (e.g. an arithmetic result).
    #[must_use]
    pub fn from_big_decimal(value: BigDecimal) -> Self {
        Self(value)
    }

    /// Borrow the underlying decimal for arithmetic in downstream crates.
    #[must_use]
    pub fn as_big_decimal(&self) -> &BigDecimal {
        &self.0
    }

    /// The canonical plain-notation string (A.1): no exponent, one leading `-`
    /// for negatives, no negative zero. The stored scale is preserved
    /// (SPEC-ISSUES item 1, "preserve operation scale" reading).
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        let (mantissa, scale) = self.0.as_bigint_and_exponent();
        if mantissa.sign() == bigdecimal::num_bigint::Sign::NoSign {
            // Canonical zero keeps its scale ("0", "0.00", ...) but never `-0`.
            return Self::assemble(false, "0", scale);
        }
        let negative = mantissa.sign() == bigdecimal::num_bigint::Sign::Minus;
        let digits = mantissa.magnitude().to_string();
        Self::assemble(negative, &digits, scale)
    }

    /// Place the decimal point `scale` digits from the right of `digits`,
    /// padding with leading zeros when the integer part is empty. A negative
    /// scale multiplies by a power of ten (trailing zeros, no point).
    fn assemble(negative: bool, digits: &str, scale: i64) -> String {
        let sign = if negative { "-" } else { "" };
        if scale <= 0 {
            let zeros = "0".repeat(scale.unsigned_abs() as usize);
            return format!("{sign}{digits}{zeros}");
        }
        let scale = scale as usize;
        if digits.len() > scale {
            let point = digits.len() - scale;
            let (int_part, frac_part) = digits.split_at(point);
            format!("{sign}{int_part}.{frac_part}")
        } else {
            let pad = "0".repeat(scale - digits.len());
            format!("{sign}0.{pad}{digits}")
        }
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        // BigDecimal's own comparison is value-based (scale-insensitive).
        self.0.cmp(&other.0)
    }
}

impl From<BigInt> for Decimal {
    fn from(value: BigInt) -> Self {
        Self(BigDecimal::from(value))
    }
}
