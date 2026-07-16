//! The virtual clock and its ISO-8601 duration input.
//!
//! FORMAT.md pins determinism: "The virtual clock starts at
//! `2026-01-01T00:00:00Z` and only `advance_time` moves it." This module owns
//! the two types that realize that rule: [`Iso8601Duration`], the parsed form of
//! an `advance_time` argument, and [`VirtualClock`], which starts at the fixed
//! epoch and advances by whole calendar units (years/months) plus a fixed
//! micro-second span (weeks/days/time), at micro-second resolution (the corpus
//! uses durations as fine as `PT0.000001S`).
//!
//! Calendar arithmetic is proleptic-Gregorian: the days-from-civil algorithm
//! (H. Hinnant) converts between a civil date and a day count, so month/year
//! addition clamps an overlong day (Jan 31 + P1M → Feb 28/29) the same way the
//! runtime's temporal layer must.

use std::fmt;

/// Micro-seconds in one calendar day.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// The largest calendar magnitude (years or months) a duration may name. A
/// billion years dwarfs any conceivable test yet keeps proleptic-Gregorian
/// arithmetic inside `i64`, so an adversarial `advance_time` string is a clean
/// [`DurationParseError`] rather than an overflow panic.
const CALENDAR_LIMIT: i64 = 1_000_000_000;

/// A parsed ISO-8601 duration, split into the calendar part (`years`/`months`,
/// whose length depends on where they land) and a fixed micro-second span
/// (weeks/days/time, whose length is constant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iso8601Duration {
    years: i64,
    months: i64,
    fixed_micros: i64,
}

/// Why an `advance_time` string is not a well-formed ISO-8601 duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationParseError {
    /// The offending text.
    pub input: String,
    /// What is wrong with it.
    pub reason: String,
}

impl fmt::Display for DurationParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not an ISO-8601 duration: {}", self.input, self.reason)
    }
}

impl std::error::Error for DurationParseError {}

impl Iso8601Duration {
    /// Parse an ISO-8601 duration (`P[nY][nM][nW][nD][T[nH][nM][nS]]`). Integer
    /// components except the seconds field, which admits a fractional part down
    /// to micro-second precision.
    pub fn parse(input: &str) -> Result<Self, DurationParseError> {
        let err = |reason: &str| DurationParseError { input: input.to_owned(), reason: reason.to_owned() };
        let body = input.strip_prefix('P').ok_or_else(|| err("must begin with `P`"))?;
        if body.is_empty() {
            return Err(err("carries no components"));
        }
        let (date_part, time_part) = match body.split_once('T') {
            Some((d, t)) => (d, Some(t)),
            None => (body, None),
        };

        let mut acc = Self { years: 0, months: 0, fixed_micros: 0 };
        for (value, unit) in components(date_part) {
            match unit {
                'Y' => acc.years = checked_add(acc.years, whole(value, unit, input)?, input)?,
                'M' => acc.months = checked_add(acc.months, whole(value, unit, input)?, input)?,
                'W' => acc.add_fixed(whole(value, unit, input)?, 7 * MICROS_PER_DAY, input)?,
                'D' => acc.add_fixed(whole(value, unit, input)?, MICROS_PER_DAY, input)?,
                other => return Err(err(&format!("`{other}` is not a valid date-part unit"))),
            }
        }
        if let Some(time) = time_part {
            if time.is_empty() {
                return Err(err("`T` is present but names no time component"));
            }
            for (value, unit) in components(time) {
                match unit {
                    'H' => acc.add_fixed(whole(value, unit, input)?, 3_600_000_000, input)?,
                    'M' => acc.add_fixed(whole(value, unit, input)?, 60_000_000, input)?,
                    'S' => acc.add_fixed(seconds_to_micros(value, input)?, 1, input)?,
                    other => return Err(err(&format!("`{other}` is not a valid time-part unit"))),
                }
            }
        }
        if !(-CALENDAR_LIMIT..=CALENDAR_LIMIT).contains(&acc.years)
            || !(-CALENDAR_LIMIT..=CALENDAR_LIMIT).contains(&acc.months)
        {
            return Err(overflow(input));
        }
        Ok(acc)
    }

    /// Accumulate `units * scale` micro-seconds into the fixed span, rejecting a
    /// duration whose magnitude overflows the micro-second accumulator.
    fn add_fixed(&mut self, units: i64, scale: i64, input: &str) -> Result<(), DurationParseError> {
        let contribution = units.checked_mul(scale).ok_or_else(|| overflow(input))?;
        self.fixed_micros = self.fixed_micros.checked_add(contribution).ok_or_else(|| overflow(input))?;
        Ok(())
    }
}

/// Split a duration segment into `(digits, unit)` pairs left to right. A trailing
/// run with no unit letter is yielded with a `'\0'` sentinel unit so the caller
/// reports it as malformed.
fn components(segment: &str) -> Vec<(&str, char)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in segment.char_indices() {
        if ch.is_ascii_alphabetic() {
            out.push((segment.get(start..idx).unwrap_or_default(), ch));
            start = idx + ch.len_utf8();
        }
    }
    if let Some(trailing) = segment.get(start..)
        && !trailing.is_empty()
    {
        out.push((trailing, '\0'));
    }
    out
}

fn whole(value: &str, unit: char, input: &str) -> Result<i64, DurationParseError> {
    value.parse::<i64>().map_err(|_| DurationParseError {
        input: input.to_owned(),
        reason: format!("`{value}{unit}` is not an integer count"),
    })
}

fn checked_add(acc: i64, add: i64, input: &str) -> Result<i64, DurationParseError> {
    acc.checked_add(add).ok_or_else(|| overflow(input))
}

/// The diagnostic for a duration whose magnitude cannot be represented.
fn overflow(input: &str) -> DurationParseError {
    DurationParseError { input: input.to_owned(), reason: "duration magnitude is out of range".to_owned() }
}

/// Parse a seconds field with up to six fractional digits into micro-seconds.
fn seconds_to_micros(value: &str, input: &str) -> Result<i64, DurationParseError> {
    let err = |reason: String| DurationParseError { input: input.to_owned(), reason };
    let (whole_part, frac_part) = match value.split_once('.') {
        Some((w, f)) => (w, f),
        None => (value, ""),
    };
    let whole: i64 = whole_part.parse().map_err(|_| err(format!("`{whole_part}S` is not an integer")))?;
    if frac_part.len() > 6 || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(format!("`{value}S` has more than micro-second precision")));
    }
    let mut micros = 0i64;
    let mut scale = 100_000i64;
    for byte in frac_part.bytes() {
        micros += i64::from(byte - b'0') * scale;
        scale /= 10;
    }
    whole
        .checked_mul(1_000_000)
        .and_then(|w| w.checked_add(micros))
        .ok_or_else(|| err(format!("`{value}S` is out of range")))
}

/// A point on the virtual timeline, at micro-second resolution, as micro-seconds
/// since 1970-01-01T00:00:00Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    micros: i64,
}

impl Instant {
    /// Micro-seconds since the Unix epoch.
    #[must_use]
    pub fn unix_micros(self) -> i64 {
        self.micros
    }

    fn from_civil(year: i64, month: i64, day: i64, intraday_micros: i64) -> Self {
        let day_micros = days_from_civil(year, month, day).saturating_mul(MICROS_PER_DAY);
        Self { micros: day_micros.saturating_add(intraday_micros) }
    }

    /// Advance by a duration: apply the calendar part to the civil date (clamping
    /// an overlong day), then add the fixed micro-second span.
    #[must_use]
    fn advanced(self, by: &Iso8601Duration) -> Self {
        let day_index = self.micros.div_euclid(MICROS_PER_DAY);
        let intraday = self.micros.rem_euclid(MICROS_PER_DAY);
        let (mut year, month, day) = civil_from_days(day_index);

        let total_months = (month - 1).saturating_add(by.years.saturating_mul(12)).saturating_add(by.months);
        year = year.saturating_add(total_months.div_euclid(12));
        let new_month = total_months.rem_euclid(12) + 1;
        let new_day = day.min(days_in_month(year, new_month));

        Self::from_civil(year, new_month, new_day, intraday).offset(by.fixed_micros)
    }

    fn offset(self, micros: i64) -> Self {
        Self { micros: self.micros.saturating_add(micros) }
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.micros.div_euclid(MICROS_PER_DAY));
        let intraday = self.micros.rem_euclid(MICROS_PER_DAY);
        let hour = intraday / 3_600_000_000;
        let minute = (intraday / 60_000_000) % 60;
        let second = (intraday / 1_000_000) % 60;
        let micro = intraday % 1_000_000;
        write!(f, "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micro:06}Z")
    }
}

/// The virtual clock: an [`Instant`] fixed at the FORMAT.md epoch, moved only by
/// [`VirtualClock::advance`], plus a count of how many advances have applied.
#[derive(Debug, Clone, Copy)]
pub struct VirtualClock {
    now: Instant,
    advances: usize,
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualClock {
    /// A clock at the fixed epoch `2026-01-01T00:00:00Z`.
    #[must_use]
    pub fn new() -> Self {
        Self { now: Instant::from_civil(2026, 1, 1, 0), advances: 0 }
    }

    /// The current virtual instant.
    #[must_use]
    pub fn now(&self) -> Instant {
        self.now
    }

    /// The number of `advance_time` steps applied so far.
    #[must_use]
    pub fn advance_count(&self) -> usize {
        self.advances
    }

    /// Move the clock forward by `duration`, returning the new instant.
    pub fn advance(&mut self, duration: &Iso8601Duration) -> Instant {
        self.now = self.now.advanced(duration);
        self.advances += 1;
        self.now
    }
}

/// Days from 1970-01-01 to the civil date `(year, month, day)` (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: civil `(year, month, day)` from a day count.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 30,
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
