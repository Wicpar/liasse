//! §14.5/§14.7 period advancement and interval-series generation.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_value::{
    recurring_intervals, CalendarPeriodBuilder, Duration, Overflow, Period, Precision, Timestamp,
    ValueError,
};

fn secs(n: i128) -> Timestamp {
    Timestamp::new(n, Precision::Seconds)
}

// 2026-01-01T00:00:00Z .. weekly boundaries, in Unix seconds.
const JAN01: i128 = 1_767_225_600;
const JAN08: i128 = 1_767_830_400;
const JAN15: i128 = 1_768_435_200;
const JAN22: i128 = 1_769_040_000;

#[test]
fn fixed_period_advances_by_exact_duration() {
    let weekly = Period::Fixed(Duration::parse("P7D").unwrap());
    assert_eq!(weekly.advance(secs(JAN01)).unwrap(), secs(JAN08));
}

#[test]
fn bounded_weekly_series_clips_at_series_bound() {
    let weekly = Period::Fixed(Duration::parse("P7D").unwrap());
    let series = recurring_intervals(secs(JAN01), Some(secs(JAN22)), Some(&weekly), secs(JAN01))
        .unwrap();
    let got: Vec<_> = series.iter().map(|i| (i.index, i.from, i.until)).collect();
    assert_eq!(
        got,
        vec![
            (0, secs(JAN01), Some(secs(JAN08))),
            (1, secs(JAN08), Some(secs(JAN15))),
            (2, secs(JAN15), Some(secs(JAN22))),
        ]
    );
}

#[test]
fn series_bound_equal_to_start_rejects() {
    let weekly = Period::Fixed(Duration::parse("P7D").unwrap());
    let err = recurring_intervals(secs(JAN01), Some(secs(JAN01)), Some(&weekly), secs(JAN01))
        .unwrap_err();
    assert!(matches!(err, ValueError::SeriesBoundNotAfterStart));
}

#[test]
fn series_bound_one_tick_above_start_yields_single_clipped_interval() {
    let weekly = Period::Fixed(Duration::parse("P7D").unwrap());
    let series =
        recurring_intervals(secs(JAN01), Some(secs(JAN01 + 1)), Some(&weekly), secs(JAN01)).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].from, secs(JAN01));
    assert_eq!(series[0].until, Some(secs(JAN01 + 1)));
}

#[test]
fn zero_fixed_period_is_non_advancing() {
    let zero = Period::Fixed(Duration::parse("PT0S").unwrap());
    let err = recurring_intervals(secs(JAN01), None, Some(&zero), secs(JAN22)).unwrap_err();
    assert!(matches!(err, ValueError::NonAdvancingPeriod));
}

#[test]
fn unbounded_series_covers_the_horizon() {
    let weekly = Period::Fixed(Duration::parse("P7D").unwrap());
    // Horizon inside the third week: [JAN15, JAN22) must be generated, then stop.
    let series = recurring_intervals(secs(JAN01), None, Some(&weekly), secs(JAN15 + 10)).unwrap();
    let last = series.last().unwrap();
    assert_eq!(last.from, secs(JAN15));
    assert_eq!(last.until, Some(secs(JAN22)));
    // The interval containing the horizon is the final one generated.
    assert!(series.iter().all(|i| i.from <= secs(JAN22)));
}

#[test]
fn non_repeating_series_is_a_single_interval() {
    let series = recurring_intervals(secs(JAN01), Some(secs(JAN22)), None, secs(JAN01)).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].from, secs(JAN01));
    assert_eq!(series[0].until, Some(secs(JAN22)));
}

#[test]
fn unbounded_reject_series_stops_at_boundary_coincident_horizon() {
    // A monthly `overflow: reject` series anchored 2025-10-30, unbounded. Boundaries
    // b0=Oct 30, b1=Nov 30, b2=Dec 30, b3=Jan 30 are real days; b4 = anchor + 4 months
    // = "Feb 30" 2026 is absent -> §14.7/A.4 reject. A read window ending exactly on
    // b3 has an exclusive-start horizon of b3: interval 2 = [b2, b3) is the last one
    // whose start is below b3, and closing it computes b3 — but NOT b4. Generation
    // therefore succeeds with exactly {0, 1, 2}; it must never step to b4 (which would
    // fail on the missing boundary and discard the whole in-window series).
    let oct30: i128 = 1_761_782_400;
    let nov30: i128 = 1_764_460_800;
    let dec30: i128 = 1_767_052_800;
    let jan30: i128 = 1_769_731_200;
    let monthly_reject = Period::Calendar(
        CalendarPeriodBuilder {
            months: 1,
            zone: Some("UTC".to_owned()),
            overflow: Overflow::Reject,
            ..CalendarPeriodBuilder::default()
        }
        .build()
        .unwrap(),
    );

    let series = recurring_intervals(secs(oct30), None, Some(&monthly_reject), secs(jan30))
        .expect("a horizon coinciding with the valid boundary b3 must not compute (and reject) b4");
    let got: Vec<_> = series.iter().map(|i| (i.index, i.from, i.until)).collect();
    assert_eq!(
        got,
        vec![
            (0, secs(oct30), Some(secs(nov30))),
            (1, secs(nov30), Some(secs(dec30))),
            (2, secs(dec30), Some(secs(jan30))),
        ],
        "the window's exclusive-start horizon b3 stops generation at interval 2 = [b2, b3), \
         closing it with b3 and never stepping to the rejected b4",
    );
}

#[test]
fn calendar_month_clamps_absent_destination_day() {
    // 2025-01-31T00:00:00Z + 1 month, UTC, clamp -> 2025-02-28T00:00:00Z.
    let jan31: i128 = 1_738_281_600;
    let feb28: i128 = 1_740_700_800;
    let builder = CalendarPeriodBuilder {
        months: 1,
        zone: Some("UTC".to_owned()),
        ..CalendarPeriodBuilder::default()
    };
    let period = Period::Calendar(builder.build().unwrap());
    assert_eq!(period.advance(secs(jan31)).unwrap(), secs(feb28));
}
