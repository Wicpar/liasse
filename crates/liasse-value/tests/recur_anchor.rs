//! Annex A.4 end-of-month anchor rule for calendar-period recurrence series.
//!
//! A.4 (normative): "Boundary `i` is calculated from the original series anchor
//! using `i × period`, rather than repeatedly adding to the previous clipped
//! boundary. This preserves end-of-month anchors: January 31, monthly, clamp ->
//! January 31; February 28/29; March 31; ...".

#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_value::{recurring_intervals, CalendarPeriodBuilder, Period, Precision, Timestamp};

fn secs(n: i128) -> Timestamp {
    Timestamp::new(n, Precision::Seconds)
}

// 2025-01-31T00:00:00Z anchor; monthly, clamp, UTC.
const JAN31: i128 = 1_738_281_600;
const FEB28: i128 = 1_740_700_800;
const MAR31: i128 = 1_743_379_200; // Jan 31 + 2 calendar months, no clamp (March has 31 days)
const APR30: i128 = 1_745_971_200;

#[test]
fn monthly_series_third_boundary_is_anchored_march_31_not_previous_clip() {
    let monthly = Period::Calendar(
        CalendarPeriodBuilder {
            months: 1,
            zone: Some("UTC".to_owned()),
            ..CalendarPeriodBuilder::default()
        }
        .build()
        .unwrap(),
    );

    // Unbounded series with a horizon into late April so at least b0..b3 exist.
    let series = recurring_intervals(secs(JAN31), None, Some(&monthly), secs(APR30)).unwrap();

    // A.4 anchor rule: b0=Jan31, b1=Jan31+1mo=Feb28 (clamp), b2=Jan31+2mo=Mar31.
    // Interval 1 is [b1, b2) = [Feb 28, Mar 31); interval 2 begins at b2 = Mar 31.
    assert_eq!(series[1].from, secs(FEB28));
    assert_eq!(
        series[1].until,
        Some(secs(MAR31)),
        "A.4: third boundary must be anchor + 2 months = March 31, not Feb 28 + 1 month = March 28",
    );
    assert_eq!(
        series[2].from,
        secs(MAR31),
        "A.4: the third interval starts at the anchored March 31",
    );
}
