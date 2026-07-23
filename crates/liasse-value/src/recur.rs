//! Period recurrence arithmetic (SPEC.md §14.5, §14.7): advancing a timestamp by
//! a fixed or calendar period, and generating a half-open interval series.
//!
//! A fixed period adds an exact elapsed duration. A calendar period applies its
//! `(years, months, weeks, days)` magnitudes in its declared time zone — so a
//! monthly step lands on the same day-of-month across months of different length.
//! An absent destination day is governed by `overflow` (§14.7/A.4): `clamp` lands on
//! the last valid day of the month, `reject` fails the boundary computation. The
//! exact `time` component is then added. The zone rules come from `jiff`; a build
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

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp as JiffTimestamp};

use crate::error::ValueError;
use crate::period::{CalendarPeriod, Overflow, Period};
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
    /// its calendar magnitudes in its zone (clamping an overflowing day under
    /// `overflow: clamp`), then adds its exact `time`. Fails if the shift leaves the
    /// representable range, names a time zone this build cannot resolve, or lands on
    /// a day absent from its destination month under `overflow: reject`.
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
    /// overflowing day under `overflow: clamp`). Fails if the shift leaves the
    /// representable range, names a time zone this build cannot resolve, or lands on
    /// a day absent from its destination month under `overflow: reject`.
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

    /// The recurrence boundary `steps` after `anchor` for the interval-series
    /// generator: the CLAMPED boundary, plus whether that boundary would be rejected
    /// under this period's own `overflow: reject` policy (its destination day is
    /// absent from its month, §14.7/A.4). A fixed period never overflows a calendar
    /// date, so it never "would reject".
    ///
    /// Distinct from [`Period::advance_from`], which collapses a would-reject boundary
    /// into the error and returns no instant. [`recurring_intervals`] instead needs
    /// the clamped position to decide whether a missing boundary lies WITHIN a finite
    /// series or is clipped away past its `$until` bound (§14.5): a boundary at or
    /// after the bound is never an interval endpoint, so its overflow must not fail
    /// admission — only a missing boundary strictly inside the series does.
    fn advance_reporting(&self, anchor: Timestamp, steps: i64) -> Result<(Timestamp, bool), ValueError> {
        match self {
            Self::Fixed(_) => Ok((self.advance_from(anchor, steps)?, false)),
            Self::Calendar(calendar) => advance_calendar_checked(calendar, anchor, steps),
        }
    }
}

/// Advance `anchor` by `steps` calendar periods through `jiff`'s zoned arithmetic,
/// scaling every magnitude by `steps` so the shift is anchored (Annex A.4). A
/// boundary landing on a day absent from its destination month under
/// `overflow: reject` fails; every other case yields the clamped instant.
fn advance_calendar(
    calendar: &CalendarPeriod,
    anchor: Timestamp,
    steps: i64,
) -> Result<Timestamp, ValueError> {
    let (boundary, would_reject) = advance_calendar_checked(calendar, anchor, steps)?;
    if would_reject {
        return Err(ValueError::CalendarOverflowRejected);
    }
    Ok(boundary)
}

/// Advance `anchor` by `steps` calendar periods, returning the CLAMPED boundary
/// together with whether it would be rejected under the period's `overflow: reject`
/// policy (§14.7/A.4). The clamped instant is returned regardless of policy so the
/// series generator can position a would-be-rejected boundary for the §14.5
/// within-series clip test; [`advance_calendar`] turns the flag back into the error.
fn advance_calendar_checked(
    calendar: &CalendarPeriod,
    anchor: Timestamp,
    steps: i64,
) -> Result<(Timestamp, bool), ValueError> {
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

    // §14.7 / Annex A.4: `overflow` governs the single "destination calendar date
    // missing" condition — a year/month shift whose day-of-month is absent from the
    // target month (a Jan-31 monthly anchor's "Feb 31"). `clamp` and `reject` are the
    // two alternatives for that one condition: `clamp` lands on the last valid day
    // (jiff's default day-constraining, applied below), `reject` MUST fail the
    // boundary computation rather than silently produce the clamped instant.
    //
    // The sibling zone-resolution policies (`ambiguous`/`missing`) would hook in at
    // this same site, but a build without a time-zone database never resolves a named
    // IANA zone (`resolve_zone` errors first), so their branches are unreachable here.
    // The caller decides what a would-reject boundary means (fail, or — past a finite
    // series bound — clip away, §14.5); here we only report it alongside the clamped
    // instant, which is computed unconditionally below.
    let would_reject = calendar.policies().0 == Overflow::Reject
        && calendar_day_is_missing(zoned.date(), years, months)?;

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
    Ok((Timestamp::new(count, anchor.precision()), would_reject))
}

/// Whether a scaled year/month calendar shift from `anchor` lands on a day-of-month
/// absent from its destination month — A.4's "destination calendar date missing".
///
/// The condition is decided *before* any clamp: the scaled `years`/`months` are added
/// from the first of `anchor`'s month (a shift that never itself clamps, since day 1
/// exists in every month), then the anchor's own day-of-month is tested for
/// constructibility in the destination month. Reading the intent this way — rather
/// than inferring it from jiff's post-clamp day — keeps a boundary that legitimately
/// lands from being falsely rejected. Weeks, days, and the exact `time` component are
/// elapsed offsets that never remove a calendar date, so they take no part.
fn calendar_day_is_missing(anchor: Date, years: i64, months: i64) -> Result<bool, ValueError> {
    let ym_span = Span::new()
        .try_years(years)
        .and_then(|s| s.try_months(months))
        .map_err(|_| ValueError::PeriodOutOfRange)?;
    let destination = anchor
        .first_of_month()
        .checked_add(ym_span)
        .map_err(|_| ValueError::PeriodOutOfRange)?;
    Ok(Date::new(destination.year(), destination.month(), anchor.day()).is_err())
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
/// interval; `horizon` is the EXCLUSIVE upper bound on the interval starts of an
/// otherwise-unbounded series: generation yields every interval whose start lies
/// below it and computes exactly the one boundary that closes the last such interval
/// — never the following boundary — so an omitted `series_until` still yields a finite
/// prefix, and a horizon coinciding with a boundary stops there rather than stepping
/// one further (which, under §14.7/A.4 `reject`, could needlessly fail on a boundary
/// outside the window).
///
/// Rejects a finite `series_until` at or before `from`, and a `repeat` that fails
/// to advance strictly from a boundary (a zero, negative, or otherwise
/// non-advancing period).
///
/// Under a calendar `overflow: reject` `repeat`, a boundary landing on a missing
/// calendar date rejects only when it lies WITHIN the enumerable series (§14.5/§14.7):
/// a finite `series_until` clips its last interval to the bound, so a missing boundary
/// at or after the bound is never an interval endpoint and does not fail generation —
/// only a missing boundary strictly inside `[from, series_until)` does. An unbounded
/// series clips nothing, so a missing boundary that closes an in-horizon interval
/// rejects. This mirrors the horizon rule: a reject boundary outside the enumerated
/// window is never surfaced.
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
        // `would_reject` flags a boundary landing on a calendar date absent from its
        // destination month under `overflow: reject`; `next` is then that boundary's
        // CLAMPED position, used only to place it against a finite `series_until`.
        let (next, would_reject) = repeat.advance_reporting(from, index + 1)?;
        match series_until {
            Some(bound) => {
                // §14.5/§14.7: the final interval is CLIPPED to the series bound, so a
                // boundary at or after `bound` is never an interval endpoint and is not
                // "within the enumerable series". Clip to `bound` and stop — even when
                // this clipped-away boundary would overflow under `overflow: reject`:
                // that missing date lies outside [from, bound) and must not fail
                // admission. `next >= bound > boundary`, so the clipped interval is
                // non-empty. A missing `reject` boundary strictly inside [from, bound)
                // is a genuine endpoint and rejects the transition (checked below).
                if next >= bound {
                    intervals.push(Interval { index, from: boundary, until: Some(bound) });
                    break;
                }
                if would_reject {
                    return Err(ValueError::CalendarOverflowRejected);
                }
                if next <= boundary {
                    return Err(ValueError::NonAdvancingPeriod);
                }
                intervals.push(Interval { index, from: boundary, until: Some(next) });
            }
            None => {
                // §14.5: an unbounded series generates indefinitely; each period has a
                // finite `$until` from its next boundary. Intervals are NOT clipped, so
                // a would-reject boundary that closes an in-horizon interval is a real
                // endpoint and rejects (caught the moment a selector enumerates it).
                if would_reject {
                    return Err(ValueError::CalendarOverflowRejected);
                }
                if next <= boundary {
                    return Err(ValueError::NonAdvancingPeriod);
                }
                // `horizon` is the exclusive upper bound on interval starts: stop as soon
                // as a boundary REACHES it, so the boundary closing the last in-window
                // interval is computed but the following one — a start at or past the
                // horizon, and possibly a §A.4 `reject` outside the window — is not.
                intervals.push(Interval { index, from: boundary, until: Some(next) });
                if next >= horizon {
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
