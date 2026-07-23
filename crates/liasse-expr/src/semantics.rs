//! The package's observable decimal-division rounding policy (SPEC §4.4, Annex
//! A.6).
//!
//! A.6 fixes the default division rounding to `half_away_from_zero`: a halfway
//! value resolves away from zero, matching PostgreSQL `numeric`. A package MAY
//! override the mode through `$semantics.decimal_division.rounding`. This enum is
//! the *resolved* policy the runtime environment hands the evaluator, so `/` and
//! `avg` round the quotient at its A.6 scale under the declared mode instead of
//! always applying the default.

use liasse_value::bigdecimal::RoundingMode;

/// A decimal-division rounding mode (Annex A.6). The [`Default`] is A.6's
/// `half_away_from_zero`, so an environment that declares no policy keeps the
/// PostgreSQL-`numeric` default behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DivisionRounding {
    /// `half_away_from_zero` — a halfway value rounds away from zero (A.6 default).
    #[default]
    HalfAwayFromZero,
    /// `half_even` — a halfway value rounds to the nearest even digit (banker's).
    HalfEven,
    /// `toward_zero` — always truncate toward zero.
    TowardZero,
    /// `away_from_zero` — always round away from zero.
    AwayFromZero,
    /// `floor` — always round toward −∞.
    Floor,
    /// `ceiling` — always round toward +∞.
    Ceiling,
}

impl DivisionRounding {
    /// Resolve the A.6 rounding mode the `$semantics.decimal_division.rounding`
    /// text names, or `None` when it is not one of the six supported values. The
    /// spellings are exactly A.6's `half_even`, `half_away_from_zero`,
    /// `toward_zero`, `away_from_zero`, `floor`, `ceiling`.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "half_away_from_zero" => Self::HalfAwayFromZero,
            "half_even" => Self::HalfEven,
            "toward_zero" => Self::TowardZero,
            "away_from_zero" => Self::AwayFromZero,
            "floor" => Self::Floor,
            "ceiling" => Self::Ceiling,
            _ => return None,
        })
    }

    /// The `bigdecimal` rounding mode this policy applies when the quotient is
    /// rounded at its A.6 significant-digit scale. `half_away_from_zero` is
    /// `bigdecimal`'s `HalfUp` (round half away from zero, the A.6 default).
    pub(crate) fn mode(self) -> RoundingMode {
        match self {
            Self::HalfAwayFromZero => RoundingMode::HalfUp,
            Self::HalfEven => RoundingMode::HalfEven,
            Self::TowardZero => RoundingMode::Down,
            Self::AwayFromZero => RoundingMode::Up,
            Self::Floor => RoundingMode::Floor,
            Self::Ceiling => RoundingMode::Ceiling,
        }
    }
}
