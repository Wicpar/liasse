//! Annex A.4 `overflow: reject` calendar-period policy.
//!
//! A.4 (normative), policy table:
//!
//! | Field      | Values           | Meaning                          |
//! |------------|------------------|----------------------------------|
//! | `overflow` | `clamp`, `reject`| destination calendar date missing|
//!
//! and §14.7: "`overflow` controls dates absent from the destination month;
//! `clamp` chooses its final valid day." The two policies are the *alternatives*
//! for the single condition "destination calendar date missing": `clamp` returns
//! the last valid day of the destination month, `reject` MUST fail the boundary
//! computation. `reject` therefore MUST NOT silently produce a clamped instant.
//!
//! This is the timing-independent core of SPEC-ISSUES #13: that item leaves the
//! *runtime layer* at which a reject surfaces unpinned (eager at the source-row
//! transition vs. lazy at a temporal read) but states the OUTCOME — a rejection —
//! is pinned. `overflow` is a policy on the boundary *calculation* itself, so the
//! calculation (`Period::advance` / `recurring_intervals`) is where the policy is
//! honored; if the calculation never rejects, no runtime layer can, under any
//! timing choice.
//!
//! Anchor 2025-01-31 monthly: the b1 destination is "February 31", which is absent
//! from February. Under `clamp` this yields 2025-02-28; under `reject` the boundary
//! computation MUST fail.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_value::{
    recurring_intervals, CalendarPeriodBuilder, Overflow, Period, Precision, Timestamp,
};

fn secs(n: i128) -> Timestamp {
    Timestamp::new(n, Precision::Seconds)
}

// 2025-01-31T00:00:00Z anchor; 2025-02-28T00:00:00Z is the clamped b1.
const JAN31: i128 = 1_738_281_600;
const FEB28: i128 = 1_740_700_800;
const MAR31: i128 = 1_743_379_200;

fn monthly(overflow: Overflow) -> Period {
    Period::Calendar(
        CalendarPeriodBuilder {
            months: 1,
            zone: Some("UTC".to_owned()),
            overflow,
            ..CalendarPeriodBuilder::default()
        }
        .build()
        .unwrap(),
    )
}

/// Control: the b1 destination ("February 31") is genuinely missing, so `clamp`
/// yields the last valid February day (2025-02-28). This fixes the premise that
/// the `reject` case below is exercising the "destination date missing" condition.
#[test]
fn clamp_control_lands_on_last_valid_february_day() {
    let clamp_b1 = monthly(Overflow::Clamp).advance(secs(JAN31)).unwrap();
    assert_eq!(
        clamp_b1,
        secs(FEB28),
        "A.4 clamp: Jan 31 + 1 month = Feb 31 (missing) clamps to Feb 28",
    );
}

/// A.4 (normative): a calendar step whose destination day is absent under
/// `overflow: reject` MUST fail the boundary computation, NOT silently clamp.
///
/// Current runtime returns `Ok(2025-02-28)` — byte-identical to `clamp` — because
/// `advance_calendar` (crates/liasse-value/src/recur.rs) never consults the
/// period's `overflow` policy (`CalendarPeriod::policies()` is read only by the
/// PostgreSQL key codec, never by the recurrence arithmetic), so the `reject`
/// policy is dead code. This assertion fails against that behavior.
#[test]
fn reject_policy_fails_the_missing_boundary_computation() {
    let advanced = monthly(Overflow::Reject).advance(secs(JAN31));
    assert!(
        advanced.is_err(),
        "A.4 overflow: reject MUST reject the missing 'Feb 31' boundary, not clamp \
         it; got {advanced:?} (identical to the clamp result Feb 28)",
    );
}

/// The two policies are the two *alternatives* for the same "destination date
/// missing" condition, so on a missing boundary they MUST differ: `clamp` produces
/// an instant, `reject` rejects. Currently they are indistinguishable — the direct
/// evidence that `overflow` is ignored.
#[test]
fn reject_and_clamp_must_differ_on_a_missing_boundary() {
    let clamp_b1 = monthly(Overflow::Clamp).advance(secs(JAN31));
    let reject_b1 = monthly(Overflow::Reject).advance(secs(JAN31));
    assert_ne!(
        clamp_b1.is_ok(),
        reject_b1.is_ok(),
        "A.4: on a missing destination date, clamp yields a value and reject fails; \
         the two policies must not produce the same outcome (clamp={clamp_b1:?}, \
         reject={reject_b1:?})",
    );
}

/// The series generator (`recurring_intervals`, the path §14.5 buckets drive) must
/// propagate the `reject` failure: a monthly `overflow: reject` series anchored on
/// Jan 31 crosses the missing "Feb 31" boundary (b1), so generating it MUST fail.
/// Currently it returns a clean three-interval series with a clamped Feb 28 / Mar 31.
#[test]
fn recurring_series_rejects_when_a_boundary_is_missing() {
    let series = recurring_intervals(secs(JAN31), None, Some(&monthly(Overflow::Reject)), secs(MAR31));
    assert!(
        series.is_err(),
        "A.4/§14.5: a reject-policy series crossing the missing 'Feb 31' boundary \
         MUST fail generation, not emit clamped intervals; got {series:?}",
    );
}

// 2025-02-15T00:00:00Z: a finite series bound that clips BEFORE the missing b1 = Feb 31.
const FEB15: i128 = 1_739_577_600;

/// §14.5/§14.7: a finite series bound that clips the last interval BEFORE the first
/// missing boundary is admitted, because the missing boundary is not "within the
/// enumerable series". The series `[Jan 31, Feb 15)` monthly `overflow: reject` has
/// b1 = "Feb 31" (missing), whose clamped position Feb 28 is at or after the bound
/// Feb 15 — so b1 is clipped away and never an interval endpoint. Enumeration yields
/// exactly one interval `[Jan 31, Feb 15)`; it MUST NOT fail on the clipped-away
/// missing boundary. (Contrast `recurring_series_rejects_when_a_boundary_is_missing`,
/// where the bound is past b1 so the missing boundary is a real endpoint.)
#[test]
fn finite_bound_clipping_before_missing_boundary_is_admitted() {
    let series = recurring_intervals(
        secs(JAN31),
        Some(secs(FEB15)),
        Some(&monthly(Overflow::Reject)),
        secs(FEB15),
    )
    .expect("a finite bound that clips before the missing b1 must generate, not reject");
    let got: Vec<_> = series.iter().map(|i| (i.index, i.from, i.until)).collect();
    assert_eq!(
        got,
        vec![(0, secs(JAN31), Some(secs(FEB15)))],
        "the clipped final interval [Jan 31, Feb 15) is included; the missing 'Feb 31' \
         boundary lies past the bound and takes no part",
    );
}

/// The complement of the case above: a finite bound PAST the first missing boundary
/// leaves that boundary strictly inside the enumerable series, so it is a genuine
/// interval endpoint and generation MUST reject (§14.7). Series `[Jan 31, Mar 31)`
/// monthly `overflow: reject`: b1 = "Feb 31" (missing) sits well inside the series.
#[test]
fn finite_bound_past_missing_interior_boundary_rejects() {
    let series = recurring_intervals(
        secs(JAN31),
        Some(secs(MAR31)),
        Some(&monthly(Overflow::Reject)),
        secs(MAR31),
    );
    assert!(
        series.is_err(),
        "a missing boundary strictly inside a finite series is a real endpoint and \
         MUST fail generation; got {series:?}",
    );
}
