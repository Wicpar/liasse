//! Period recurrence arithmetic (SPEC.md §14.5, §14.7): advancing a timestamp by
//! a fixed or calendar period, and generating a half-open interval series.
//!
//! A fixed period adds an exact elapsed duration. A calendar period applies its
//! `(years, months, weeks, days)` magnitudes in its declared time zone — so a
//! monthly step lands on the same day-of-month across months of different length,
//! clamping an absent destination day (`overflow: clamp`) to the last valid day —
//! then adds its exact `time` component. The zone rules come from `jiff`; a build
//! without a configured time-zone database can still advance UTC and fixed
//! periods, and reports a named zone it cannot resolve rather than guessing.
//!
//! [`recurring_intervals`] turns a `[from, series_until)` window plus a repeat
//! period into the consecutive `[bi, bi+1)` intervals of §14.5: each boundary is
//! obtained by advancing the prior one, the step must advance strictly, a finite
//! series bound must sit above the start, and an unbounded series is generated
//! only up to a caller-supplied horizon.

use jiff::tz::TimeZone;
use jiff::{Span, Timestamp as JiffTimestamp};

use crate::error::ValueError;
use crate::period::{CalendarPeriod, Period};
use crate::temporal::Timestamp;

/// Nanoseconds per one tick at `precision`. Every declared precision divides one
/// second evenly, so this is exact.
fn nanos_per_tick(precision: crate::Precision) -> i128 {
    1_000_000_000 / precision.ticks_per_second()
}

/// A conservative bound on a single calendar magnitude, well inside `jiff`'s
/// per-unit `Span` limits, so building the span never panics; a period carrying a
/// larger magnitude is reported out of range instead.
const CALENDAR_MAGNITUDE_LIMIT: i64 = 4_000_000;

impl Period {
    /// The next recurrence boundary after `from` (§14.7): `from` plus this period,
    /// at `from`'s precision.
    ///
    /// A fixed period adds its exact elapsed duration. A calendar period shifts by
    /// its calendar magnitudes in its zone (clamping an overflowing day), then adds
    /// its exact `time`. Fails if the shift leaves the representable range or names
    /// a time zone this build cannot resolve.
    pub fn advance(&self, from: Timestamp) -> Result<Timestamp, ValueError> {
        match self {
            Self::Fixed(duration) => {
                let per_tick = nanos_per_tick(from.precision());
                let added = duration.as_nanos() / per_tick;
                let count = from
                    .count()
                    .checked_add(added)
                    .ok_or(ValueError::PeriodOutOfRange)?;
                Ok(Timestamp::new(count, from.precision()))
            }
            Self::Calendar(calendar) => advance_calendar(calendar, from),
        }
    }
}

/// Advance `from` by a calendar period through `jiff`'s zoned arithmetic.
fn advance_calendar(calendar: &CalendarPeriod, from: Timestamp) -> Result<Timestamp, ValueError> {
    let per_tick = nanos_per_tick(from.precision());
    let nanos = from
        .count()
        .checked_mul(per_tick)
        .ok_or(ValueError::PeriodOutOfRange)?;
    let instant = JiffTimestamp::from_nanosecond(nanos).map_err(|_| ValueError::PeriodOutOfRange)?;

    let zone = resolve_zone(calendar.zone())?;
    let zoned = instant.to_zoned(zone);

    let (years, months, weeks, days) = calendar.calendar_magnitudes();
    for magnitude in [years, months, weeks, days] {
        if magnitude.unsigned_abs() > CALENDAR_MAGNITUDE_LIMIT.unsigned_abs() {
            return Err(ValueError::PeriodOutOfRange);
        }
    }
    // `overflow: clamp` (§14.7) is jiff's default day handling: a calendar step
    // whose destination day is absent lands on the last valid day of the month.
    let span = Span::new()
        .years(years)
        .months(months)
        .weeks(weeks)
        .days(days);
    let shifted = zoned
        .checked_add(span)
        .map_err(|_| ValueError::PeriodOutOfRange)?;

    // The `time` component is exact elapsed duration added after the calendar shift.
    let result_nanos = shifted
        .timestamp()
        .as_nanosecond()
        .checked_add(calendar.time().as_nanos())
        .ok_or(ValueError::PeriodOutOfRange)?;
    let count = result_nanos.div_euclid(per_tick);
    Ok(Timestamp::new(count, from.precision()))
}

/// Resolve a calendar period's zone name to a `jiff` time zone. An absent name or
/// `UTC` needs no database; any other name requires a configured time-zone
/// database and reports its absence rather than silently falling back.
fn resolve_zone(name: Option<&str>) -> Result<TimeZone, ValueError> {
    match name {
        None | Some("UTC") => Ok(TimeZone::UTC),
        Some(name) => {
            TimeZone::get(name).map_err(|_| ValueError::PeriodZoneUnavailable(name.to_owned()))
        }
    }
}

/// One generated half-open interval of a source-backed bucket series (§14.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    /// The zero-based recurrence index (`$index`).
    pub index: i64,
    /// The interval start (`$from`).
    pub from: Timestamp,
    /// The interval end (`$until`), or `None` for the unbounded single interval a
    /// non-repeating series with no series bound produces.
    pub until: Option<Timestamp>,
}

/// The generation cap that guards an unbounded recurring series against runaway
/// enumeration when its horizon is far from its start.
const SERIES_CAP: usize = 200_000;

/// Generate the half-open interval series of a source-backed bucket (§14.5).
///
/// `from` is the series start (`$from`); `series_until` its optional upper bound
/// (`$until`); `repeat` the recurrence period (`$repeat`), absent for a single
/// interval; `horizon` bounds generation of an otherwise-unbounded series (the
/// caller's read/evaluation instant), so an omitted `series_until` still yields a
/// finite prefix covering the horizon.
///
/// Rejects a finite `series_until` at or before `from`, and a `repeat` that fails
/// to advance strictly from a boundary (a zero, negative, or otherwise
/// non-advancing period).
pub fn recurring_intervals(
    from: Timestamp,
    series_until: Option<Timestamp>,
    repeat: Option<&Period>,
    horizon: Timestamp,
) -> Result<Vec<Interval>, ValueError> {
    if let Some(bound) = series_until
        && bound <= from
    {
        return Err(ValueError::SeriesBoundNotAfterStart);
    }
    let Some(repeat) = repeat else {
        // §14.5: an absent `$repeat` produces one interval using the series bounds.
        return Ok(vec![Interval { index: 0, from, until: series_until }]);
    };

    let mut intervals = Vec::new();
    let mut boundary = from;
    let mut index: i64 = 0;
    loop {
        let next = repeat.advance(boundary)?;
        if next <= boundary {
            return Err(ValueError::NonAdvancingPeriod);
        }
        match series_until {
            Some(bound) => {
                // §14.5: `$until = min(bi+1, series-until)`; a clipped final interval
                // is included when its start is below its end.
                let until = if next < bound { next } else { bound };
                intervals.push(Interval { index, from: boundary, until: Some(until) });
                if next >= bound {
                    break;
                }
            }
            None => {
                // §14.5: an unbounded series generates indefinitely; each period has a
                // finite `$until` from its next boundary. Generate up to the horizon.
                intervals.push(Interval { index, from: boundary, until: Some(next) });
                if next > horizon {
                    break;
                }
            }
        }
        boundary = next;
        index += 1;
        if intervals.len() > SERIES_CAP {
            return Err(ValueError::SeriesTooLong(SERIES_CAP));
        }
    }
    Ok(intervals)
}
