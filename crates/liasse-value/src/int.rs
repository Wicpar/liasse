//! `int` — arbitrary-precision integer (A.1).

use core::str::FromStr;

use bigdecimal::num_bigint::BigInt;

use crate::error::ValueError;

/// An arbitrary-precision integer.
///
/// Canonical wire form (A.1) is a JSON string of canonical base-10 digits:
/// no leading zeros, a single leading `-` for negatives, and `0` for zero —
/// exactly [`BigInt`]'s `Display`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Integer(BigInt);

impl Integer {
    /// Parse the canonical base-10 text of an integer.
    ///
    /// Input acceptance beyond the canonical form is unpinned (SPEC-ISSUES
    /// item 2); we take the least-surprising decoder stance: accept any
    /// standard signed base-10 spelling and normalize to the canonical form on
    /// output. Non-integer spellings (decimal point, exponent) are rejected.
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        BigInt::from_str(text)
            .map(Integer)
            .map_err(|_| ValueError::MalformedInt(text.to_owned()))
    }

    /// The canonical base-10 string (A.1 / D.2).
    #[must_use]
    pub fn to_canonical_text(&self) -> String {
        self.0.to_string()
    }

    /// Borrow the underlying big integer for arithmetic in downstream crates.
    #[must_use]
    pub fn as_bigint(&self) -> &BigInt {
        &self.0
    }
}

impl From<BigInt> for Integer {
    fn from(value: BigInt) -> Self {
        Self(value)
    }
}

impl From<i64> for Integer {
    fn from(value: i64) -> Self {
        Self(BigInt::from(value))
    }
}
