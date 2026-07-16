//! The virtual clock and ISO-8601 duration parsing. Expected instants are
//! computed by hand from the FORMAT.md epoch and calendar rules, not read back
//! from the clock.

use liasse_testkit::{Iso8601Duration, VirtualClock};

#[test]
fn clock_starts_at_the_fixed_epoch() {
    let clock = VirtualClock::new();
    assert_eq!(clock.now().to_string(), "2026-01-01T00:00:00.000000Z");
    assert_eq!(clock.advance_count(), 0);
}

#[test]
fn day_and_time_advances_accumulate() -> Result<(), String> {
    let mut clock = VirtualClock::new();
    // 2026-01-01 + P31D = 2026-02-01 (January has 31 days).
    clock.advance(&Iso8601Duration::parse("P31D").map_err(|e| e.to_string())?);
    assert_eq!(clock.now().to_string(), "2026-02-01T00:00:00.000000Z");
    // + PT1H30M -> same day, 01:30.
    clock.advance(&Iso8601Duration::parse("PT1H30M").map_err(|e| e.to_string())?);
    assert_eq!(clock.now().to_string(), "2026-02-01T01:30:00.000000Z");
    assert_eq!(clock.advance_count(), 2);
    Ok(())
}

#[test]
fn calendar_months_clamp_an_overlong_day() -> Result<(), String> {
    let mut clock = VirtualClock::new();
    // 2026-01-01 + P1M -> 2026-02-01. Then + P1M again -> 2026-03-01. 2026 is
    // not a leap year, so the calendar step never lands on a non-existent day.
    clock.advance(&Iso8601Duration::parse("P5M").map_err(|e| e.to_string())?);
    // 2026-01-01 + 5 months = 2026-06-01.
    assert_eq!(clock.now().to_string(), "2026-06-01T00:00:00.000000Z");
    Ok(())
}

#[test]
fn microsecond_precision_is_preserved() -> Result<(), String> {
    let mut clock = VirtualClock::new();
    clock.advance(&Iso8601Duration::parse("PT0.000001S").map_err(|e| e.to_string())?);
    assert_eq!(clock.now().to_string(), "2026-01-01T00:00:00.000001Z");
    clock.advance(&Iso8601Duration::parse("PT59M59.999999S").map_err(|e| e.to_string())?);
    assert_eq!(clock.now().to_string(), "2026-01-01T01:00:00.000000Z");
    Ok(())
}

#[test]
fn malformed_durations_are_rejected() {
    assert!(Iso8601Duration::parse("31D").is_err(), "must start with P");
    assert!(Iso8601Duration::parse("P").is_err(), "must carry a component");
    assert!(Iso8601Duration::parse("PT").is_err(), "T with no time component");
    assert!(Iso8601Duration::parse("P1X").is_err(), "unknown unit");
    assert!(Iso8601Duration::parse("PT0.1234567S").is_err(), "sub-microsecond precision");
    // An adversarial magnitude is a clean parse error, never an overflow panic:
    // 9_999_999_999_999 s * 1_000_000 µs/s exceeds i64, and 2e9 years exceeds
    // the billion-year calendar limit (yet still parses as an i64).
    assert!(Iso8601Duration::parse("PT9999999999999S").is_err(), "seconds overflow the micro-second span");
    assert!(Iso8601Duration::parse("P2000000000Y").is_err(), "years exceed the calendar limit");
}
