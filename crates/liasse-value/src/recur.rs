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
//! period into the consecutive `[bi, bi+1)` intervals of §14.5: boundary `i` is
//! the series anchor advanced by `i × period` (Annex A.4), computed from the
//! anchor rather than chained from the previous — possibly clamped — boundary, so
//! end-of-month anchors survive (Jan 31 monthly clamp -> Feb 28, then Mar 31, not
//! Mar 28). The step must advance strictly, a finite series bound must sit above
//! the start, and an unbounded series is generated only up to a caller-supplied
//! horizon.

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

impl Period {
    /// The next recurrence boundary after `from` (§14.7): `from` plus this period,
    /// at `from`'s precision. Equivalent to [`Period::advance_from`] with one step.
    ///
    /// A fixed period adds its exact elapsed duration. A calendar period shifts by
    /// its calendar magnitudes in its zone (clamping an overflowing day), then adds
    /// its exact `time`. Fails if the shift leaves the representable range or names
    /// a time zone this build cannot resolve.
    pub fn advance(&self, from: Timestamp) -> Result<Timestamp, ValueError> {
        self.advance_from(from, 1)
    }

    /// The recurrence boundary `steps` periods after `anchor` (Annex A.4): the
    /// anchor advanced by `steps × period`, at `anchor`'s precision.
    ///
    /// Boundary `i` is computed from the series anchor with `steps = i`, not by
    /// chaining from the previous — possibly clamped — boundary. A fixed period
    /// adds `steps` times its exact duration. A calendar period scales each of its
    /// `(years, months, weeks, days)` magnitudes and its `time` component by
    /// `steps`, then applies the single scaled shift in its zone (clamping an
    /// overflowing day, `overflow: clamp`). Fails if the shift leaves the
    /// representable range or names a time zone this build cannot resolve.
    pub fn advance_from(&self, anchor: Timestamp, steps: i64) -> Result<Timestamp, ValueError> {
        match self {
            Self::Fixed(duration) => {
                let per_tick = nanos_per_tick(anchor.precision());
                let added = (duration.as_nanos() / per_tick)
                    .checked_mul(i128::from(steps))
                    .ok_or(ValueError::PeriodOutOfRange)?;
                let count = anchor
                    .count()
                    .checked_add(added)
                    .ok_or(ValueError::PeriodOutOfRange)?;
                Ok(Timestamp::new(count, anchor.precision()))
            }
            Self::Calendar(calendar) => advance_calendar(calendar, anchor, steps),
        }
    }
}

/// Advance `anchor` by `steps` calendar periods through `jiff`'s zoned arithmetic,
/// scaling every magnitude by `steps` so the shift is anchored (Annex A.4).
fn advance_calendar(
    calendar: &CalendarPeriod,
    anchor: Timestamp,
    steps: i64,
) -> Result<Timestamp, ValueError> {
    let per_tick = nanos_per_tick(anchor.precision());
    let nanos = anchor
        .count()
        .checked_mul(per_tick)
        .ok_or(ValueError::PeriodOutOfRange)?;
    let instant = JiffTimestamp::from_nanosecond(nanos).map_err(|_| ValueError::PeriodOutOfRange)?;

    let zone = resolve_zone(calendar.zone())?;
    let zoned = instant.to_zoned(zone);

    let (years, months, weeks, days) = calendar.calendar_magnitudes();
    let scaled = |magnitude: i64| magnitude.checked_mul(steps).ok_or(ValueError::PeriodOutOfRange);
    let (years, months, weeks, days) = (scaled(years)?, scaled(months)?, scaled(weeks)?, scaled(days)?);
    // `overflow: clamp` (§14.7) is jiff's default day handling: a calendar step
    // whose destination day is absent lands on the last valid day of the month.
    // The fallible `try_*` builders reject a scaled magnitude beyond jiff's
    // per-unit `Span` limit as out of range rather than panicking.
    let span = Span::new()
        .try_years(years)
        .and_then(|s| s.try_months(months))
        .and_then(|s| s.try_weeks(weeks))
        .and_then(|s| s.try_days(days))
        .map_err(|_| ValueError::PeriodOutOfRange)?;
    let shifted = zoned
        .checked_add(span)
        .map_err(|_| ValueError::PeriodOutOfRange)?;

    // The `time` component is exact elapsed duration added after the calendar
    // shift, likewise scaled by `steps`.
    let time_total = calendar
        .time()
        .as_nanos()
        .checked_mul(i128::from(steps))
        .ok_or(ValueError::PeriodOutOfRange)?;
    let result_nanos = shifted
        .timestamp()
        .as_nanosecond()
        .checked_add(time_total)
        .ok_or(ValueError::PeriodOutOfRange)?;
    let count = result_nanos.div_euclid(per_tick);
    Ok(Timestamp::new(count, anchor.precision()))
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
        // Annex A.4: boundary `i+1` is the anchor advanced by `(i+1) × period`,
        // computed from `from` rather than from the prior (possibly clamped)
        // `boundary`, so end-of-month anchors are preserved across the series.
        let next = repeat.advance_from(from, index + 1)?;
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
