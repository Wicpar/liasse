//! `period` — fixed or calendar recurrence step (A.4). Ordering per B.1.

use crate::duration::Duration;
use crate::error::ValueError;

/// Destination-date-missing policy. Declaration order (A.4/B.1): `clamp < reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Overflow {
    Clamp,
    Reject,
}

/// Local-time-occurs-twice policy. Order: `earlier < later < reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ambiguous {
    Earlier,
    Later,
    Reject,
}

/// Local-time-does-not-occur policy. Order: `forward < backward < reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Missing {
    Forward,
    Backward,
    Reject,
}

impl Overflow {
    fn parse(text: &str) -> Result<Self, ValueError> {
        match text {
            "clamp" => Ok(Self::Clamp),
            "reject" => Ok(Self::Reject),
            other => Err(ValueError::UnknownPolicy {
                field: "overflow",
                value: other.to_owned(),
            }),
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Clamp => "clamp",
            Self::Reject => "reject",
        }
    }
}

impl Ambiguous {
    fn parse(text: &str) -> Result<Self, ValueError> {
        match text {
            "earlier" => Ok(Self::Earlier),
            "later" => Ok(Self::Later),
            "reject" => Ok(Self::Reject),
            other => Err(ValueError::UnknownPolicy {
                field: "ambiguous",
                value: other.to_owned(),
            }),
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Earlier => "earlier",
            Self::Later => "later",
            Self::Reject => "reject",
        }
    }
}

impl Missing {
    fn parse(text: &str) -> Result<Self, ValueError> {
        match text {
            "forward" => Ok(Self::Forward),
            "backward" => Ok(Self::Backward),
            "reject" => Ok(Self::Reject),
            other => Err(ValueError::UnknownPolicy {
                field: "missing",
                value: other.to_owned(),
            }),
        }
    }

    const fn keyword(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
            Self::Reject => "reject",
        }
    }
}

/// The raw magnitudes and policies of a calendar period, before the
/// non-zero-magnitude invariant is enforced.
#[derive(Debug, Clone)]
pub struct CalendarPeriodBuilder {
    pub years: i64,
    pub months: i64,
    pub weeks: i64,
    pub days: i64,
    pub time: Duration,
    pub zone: Option<String>,
    pub overflow: Overflow,
    pub ambiguous: Ambiguous,
    pub missing: Missing,
}

impl Default for CalendarPeriodBuilder {
    fn default() -> Self {
        Self {
            years: 0,
            months: 0,
            weeks: 0,
            days: 0,
            time: Duration::ZERO,
            zone: None,
            overflow: Overflow::Clamp,
            ambiguous: Ambiguous::Earlier,
            missing: Missing::Forward,
        }
    }
}

impl CalendarPeriodBuilder {
    /// Resolve the `overflow`/`ambiguous`/`missing` keyword members.
    pub fn set_overflow(&mut self, text: &str) -> Result<(), ValueError> {
        self.overflow = Overflow::parse(text)?;
        Ok(())
    }
    pub fn set_ambiguous(&mut self, text: &str) -> Result<(), ValueError> {
        self.ambiguous = Ambiguous::parse(text)?;
        Ok(())
    }
    pub fn set_missing(&mut self, text: &str) -> Result<(), ValueError> {
        self.missing = Missing::parse(text)?;
        Ok(())
    }

    /// Enforce A.4's "at least one magnitude component MUST be non-zero" and
    /// freeze into an immutable [`CalendarPeriod`].
    pub fn build(self) -> Result<CalendarPeriod, ValueError> {
        let has_magnitude = self.years != 0
            || self.months != 0
            || self.weeks != 0
            || self.days != 0
            || !self.time.is_zero();
        if !has_magnitude {
            return Err(ValueError::EmptyCalendarPeriod);
        }
        Ok(CalendarPeriod {
            years: self.years,
            months: self.months,
            weeks: self.weeks,
            days: self.days,
            time: self.time,
            zone: self.zone,
            overflow: self.overflow,
            ambiguous: self.ambiguous,
            missing: self.missing,
        })
    }
}

/// A calendar recurrence step (A.4).
///
/// Field order here is deliberately the B.1 comparison order —
/// `(years, months, weeks, days, time, zone, overflow, ambiguous, missing)` —
/// so a derived lexicographic `Ord` is exactly the specified order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalendarPeriod {
    years: i64,
    months: i64,
    weeks: i64,
    days: i64,
    time: Duration,
    zone: Option<String>,
    overflow: Overflow,
    ambiguous: Ambiguous,
    missing: Missing,
}

impl CalendarPeriod {
    /// The `(years, months, weeks, days)` calendar magnitudes.
    #[must_use]
    pub const fn calendar_magnitudes(&self) -> (i64, i64, i64, i64) {
        (self.years, self.months, self.weeks, self.days)
    }

    /// The elapsed time magnitude.
    #[must_use]
    pub const fn time(&self) -> Duration {
        self.time
    }

    /// The time zone name, if any.
    #[must_use]
    pub fn zone(&self) -> Option<&str> {
        self.zone.as_deref()
    }

    /// The three recurrence policies.
    #[must_use]
    pub const fn policies(&self) -> (Overflow, Ambiguous, Missing) {
        (self.overflow, self.ambiguous, self.missing)
    }

    /// The keyword spellings of the three policies, for wire encoding.
    #[must_use]
    pub const fn policy_keywords(&self) -> (&'static str, &'static str, &'static str) {
        (
            self.overflow.keyword(),
            self.ambiguous.keyword(),
            self.missing.keyword(),
        )
    }
}

/// A period value (A.4): a fixed elapsed step or a calendar recurrence.
///
/// Ordering (B.1): fixed periods sort before calendar periods; fixed by exact
/// duration; calendar by the field tuple above. The variant order (`Fixed`
/// first) plus the derived `Ord` realize this directly.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Period {
    Fixed(Duration),
    Calendar(CalendarPeriod),
}
