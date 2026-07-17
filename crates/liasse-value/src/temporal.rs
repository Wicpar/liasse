//! `date` and `timestamp` (A.1, A.5).

use core::cmp::Ordering;
use core::str::FromStr;

use jiff::civil::Date as CivilDate;

use crate::error::ValueError;

/// Declared timestamp precision (A.5). The package default is [`Precision::Micros`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Precision {
    Seconds,
    Millis,
    Micros,
    Nanos,
}

impl Precision {
    /// The PostgreSQL-matching package default (A.5).
    pub const DEFAULT: Self = Self::Micros;

    /// Ticks of this precision per second.
    #[must_use]
    pub const fn ticks_per_second(self) -> i128 {
        match self {
            Self::Seconds => 1,
            Self::Millis => 1_000,
            Self::Micros => 1_000_000,
            Self::Nanos => 1_000_000_000,
        }
    }

    /// The `$precision` keyword spelling (A.5).
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Seconds => "s",
            Self::Millis => "ms",
            Self::Micros => "us",
            Self::Nanos => "ns",
        }
    }

    /// Parse a `$precision` keyword.
    pub fn parse(keyword: &str) -> Option<Self> {
        match keyword {
            "s" => Some(Self::Seconds),
            "ms" => Some(Self::Millis),
            "us" => Some(Self::Micros),
            "ns" => Some(Self::Nanos),
            _ => None,
        }
    }
}

/// A signed Unix-time count at a declared precision (A.5).
///
/// The wire form (A.1) is the base-10 count only; precision is a property of
/// the declared type, retained here so cross-precision ordering (B.1) is exact
/// without ever fabricating a wall-clock rendering.
#[derive(Debug, Clone, Copy)]
pub struct Timestamp {
    count: i128,
    precision: Precision,
}

impl Timestamp {
    /// Build from a signed count of ticks at the given precision.
    #[must_use]
    pub const fn new(count: i128, precision: Precision) -> Self {
        Self { count, precision }
    }

    /// Parse a canonical base-10 signed count at the declared precision.
    pub fn parse(text: &str, precision: Precision) -> Result<Self, ValueError> {
        let count: i128 = text
            .parse()
            .map_err(|_| ValueError::MalformedTimestamp(text.to_owned()))?;
        Ok(Self { count, precision })
    }

    /// The signed tick count.
    #[must_use]
    pub const fn count(self) -> i128 {
        self.count
    }

    /// The declared precision.
    #[must_use]
    pub const fn precision(self) -> Precision {
        self.precision
    }

    /// The canonical base-10 string of the count (A.1).
    #[must_use]
    pub fn to_canonical_text(self) -> String {
        self.count.to_string()
    }

    /// Decompose into whole seconds and a sub-second nanosecond remainder,
    /// each within `i128` range regardless of precision, so comparison never
    /// overflows and negatives use floor semantics (correct signed order).
    fn seconds_and_subnanos(self) -> (i128, i128) {
        let ticks = self.precision.ticks_per_second();
        let seconds = self.count.div_euclid(ticks);
        let sub_ticks = self.count.rem_euclid(ticks);
        let sub_nanos = sub_ticks * (1_000_000_000 / ticks);
        (seconds, sub_nanos)
    }
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Timestamp {}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        // B.1: signed order after exact precision normalization.
        self.seconds_and_subnanos()
            .cmp(&other.seconds_and_subnanos())
    }
}

/// A Gregorian calendar date (A.1). Parsing rejects impossible calendar dates,
/// so a [`Date`] is proof of a real day; ordering is chronological (B.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date(CivilDate);

impl Date {
    /// Parse a `YYYY-MM-DD` date.
    pub fn parse(text: &str) -> Result<Self, ValueError> {
        CivilDate::from_str(text)
            .map(Date)
            .map_err(|_| ValueError::MalformedDate(text.to_owned()))
    }

    /// The canonical `YYYY-MM-DD` string (A.1).
    #[must_use]
    pub fn to_canonical_text(self) -> String {
        self.0.to_string()
    }

    /// The proleptic-Gregorian year (`-9999..=9999`). Delegates to the backing
    /// [`CivilDate`]; exposed so an order-preserving key codec can lay out the
    /// `(year, month, day)` tuple B.1 orders a date by.
    #[must_use]
    pub fn year(self) -> i16 {
        self.0.year()
    }

    /// The month of the year (`1..=12`).
    #[must_use]
    pub fn month(self) -> i8 {
        self.0.month()
    }

    /// The day of the month (`1..=31`, valid for the month by construction).
    #[must_use]
    pub fn day(self) -> i8 {
        self.0.day()
    }
}
