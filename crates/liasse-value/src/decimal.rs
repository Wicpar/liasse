//! `decimal` — exact base-10 value (A.1, A.6).

use core::cmp::Ordering;
use core::str::FromStr;

use bigdecimal::BigDecimal;
use bigdecimal::num_bigint::BigInt;

use crate::error::ValueError;

/// An exact decimal.
///
/// The stored value may carry any scale (fractional digit count), but A.1 pins
/// one canonical wire spelling per mathematical value: the **minimal-scale**
/// plain form (SPEC-ISSUES item 1). `1.0`, `1.00`, and `1` are one value that
/// renders to one string, `1`. [`Ord`]/[`Eq`] are scale-insensitive (numeric)
/// and [`Decimal::to_canonical_text`] is likewise value-determined, so equal
/// decimals are indistinguishable on the wire as well as in order.
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
    /// The canonical output form is pinned by A.1: plain, no exponent, one sign,
    /// and **minimal scale** — every trailing fractional zero stripped, so one
    /// spelling per mathematical value (SPEC-ISSUES item 1). The parsed scale is
    /// retained in the stored value (arithmetic reads it), but it is not part of
    /// identity and never survives [`to_canonical_text`](Self::to_canonical_text);
    /// exponent input is accepted then rendered in plain minimal-scale form.
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

    /// The canonical plain-notation string (A.1): no exponent, **minimal scale**
    /// (every trailing fractional zero stripped, the point dropped when no
    /// fractional digit remains), one leading `-` for negatives, no negative
    /// zero. Normalizing first (`BigDecimal::normalized`, the same path json
    /// numbers use) makes the result a total function of the mathematical value,
    /// so numerically equal decimals share one spelling (SPEC-ISSUES item 1).
    /// Integer-part trailing zeros are magnitude, not scale, and survive.
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        let (mantissa, scale) = self.0.normalized().as_bigint_and_exponent();
        if mantissa.sign() == bigdecimal::num_bigint::Sign::NoSign {
            // Zero of any input scale is `0`; `-0` is never produced.
            return "0".to_owned();
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
