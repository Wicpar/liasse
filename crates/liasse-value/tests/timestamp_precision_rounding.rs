//! `Timestamp::to_precision` — coarser fractional-second conversion rounding
//! (SPEC §A.5, §A.6). §A.5 fixes the default to half-away-from-zero, but the
//! same `$semantics.decimal_division.rounding` selector may pick any A.6 mode,
//! and that selector governs this timestamp field-write boundary too.
//!
//! Every expected count is hand-derived from the rounding definitions, never read
//! off the implementation. A finer or equal precision is an exact power-of-ten
//! upscale (no rounding); a coarser precision divides the tick count by the exact
//! ratio, and a value that does not divide evenly rounds under the selected mode.
//! The us→s cases below all coarsen by ratio 1_000_000, so the whole-second
//! quotient is `count / 1_000_000` truncated toward zero and the discarded
//! fraction is `|count mod 1_000_000| / 1_000_000`, compared against one half
//! (500_000).

use liasse_value::bigdecimal::RoundingMode;
use liasse_value::{Precision, Timestamp};

/// Coarsen `count` micro-seconds to whole seconds under `mode` and return the
/// resulting second count.
fn us_to_s(count: i128, mode: RoundingMode) -> i128 {
    let out = Timestamp::new(count, Precision::Micros).to_precision(Precision::Seconds, mode);
    assert_eq!(out.precision(), Precision::Seconds, "conversion retargets precision");
    out.count()
}

// 1.5 s — a positive exact half; the truncated quotient 1 is odd.
#[test]
fn positive_half_odd_quotient() {
    let c = 1_500_000;
    assert_eq!(us_to_s(c, RoundingMode::Down), 1); // toward_zero: truncate
    assert_eq!(us_to_s(c, RoundingMode::Up), 2); // away_from_zero
    assert_eq!(us_to_s(c, RoundingMode::Floor), 1); // toward -inf (positive)
    assert_eq!(us_to_s(c, RoundingMode::Ceiling), 2); // toward +inf (positive)
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), 2); // half away from zero (A.6 default)
    assert_eq!(us_to_s(c, RoundingMode::HalfDown), 1); // half toward zero
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), 2); // to even: 1 odd -> 2
}

// 2.5 s — a positive exact half whose truncated quotient 2 is even.
#[test]
fn positive_half_even_quotient() {
    let c = 2_500_000;
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), 3);
    assert_eq!(us_to_s(c, RoundingMode::HalfDown), 2);
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), 2); // to even: 2 even -> 2
}

// 1.4 s — below one half.
#[test]
fn positive_below_half() {
    let c = 1_400_000;
    assert_eq!(us_to_s(c, RoundingMode::Down), 1);
    assert_eq!(us_to_s(c, RoundingMode::Up), 2);
    assert_eq!(us_to_s(c, RoundingMode::Floor), 1);
    assert_eq!(us_to_s(c, RoundingMode::Ceiling), 2);
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), 1);
    assert_eq!(us_to_s(c, RoundingMode::HalfDown), 1);
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), 1);
}

// 1.6 s — above one half.
#[test]
fn positive_above_half() {
    let c = 1_600_000;
    assert_eq!(us_to_s(c, RoundingMode::Down), 1);
    assert_eq!(us_to_s(c, RoundingMode::Up), 2);
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), 2);
    assert_eq!(us_to_s(c, RoundingMode::HalfDown), 2);
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), 2);
}

// -1.5 s — a negative exact half; the truncated quotient -1 is odd. The
// remainder shares the dividend's sign, so half-away lands at -2.
#[test]
fn negative_half_odd_quotient() {
    let c = -1_500_000;
    assert_eq!(us_to_s(c, RoundingMode::Down), -1); // toward zero
    assert_eq!(us_to_s(c, RoundingMode::Up), -2); // away from zero
    assert_eq!(us_to_s(c, RoundingMode::Floor), -2); // toward -inf
    assert_eq!(us_to_s(c, RoundingMode::Ceiling), -1); // toward +inf
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), -2); // half away from zero
    assert_eq!(us_to_s(c, RoundingMode::HalfDown), -1); // half toward zero
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), -2); // to even: -1 odd -> -2
}

// -2.5 s — a negative exact half whose truncated quotient -2 is even.
#[test]
fn negative_half_even_quotient() {
    assert_eq!(us_to_s(-2_500_000, RoundingMode::HalfEven), -2);
}

// -1.6 s — above one half in magnitude.
#[test]
fn negative_above_half() {
    let c = -1_600_000;
    assert_eq!(us_to_s(c, RoundingMode::Down), -1);
    assert_eq!(us_to_s(c, RoundingMode::Up), -2);
    assert_eq!(us_to_s(c, RoundingMode::Floor), -2);
    assert_eq!(us_to_s(c, RoundingMode::Ceiling), -1);
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), -2);
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), -2);
}

// -1.4 s — below one half in magnitude.
#[test]
fn negative_below_half() {
    let c = -1_400_000;
    assert_eq!(us_to_s(c, RoundingMode::Floor), -2);
    assert_eq!(us_to_s(c, RoundingMode::Ceiling), -1);
    assert_eq!(us_to_s(c, RoundingMode::HalfUp), -1);
    assert_eq!(us_to_s(c, RoundingMode::HalfEven), -1);
}

// An exact multiple has no discarded fraction, so every mode agrees.
#[test]
fn exact_multiple_is_mode_invariant() {
    for mode in [
        RoundingMode::Down,
        RoundingMode::Up,
        RoundingMode::Floor,
        RoundingMode::Ceiling,
        RoundingMode::HalfUp,
        RoundingMode::HalfDown,
        RoundingMode::HalfEven,
    ] {
        assert_eq!(us_to_s(3_000_000, mode), 3);
        assert_eq!(us_to_s(-3_000_000, mode), -3);
    }
}

// Finer and equal precision are exact upscales the mode never touches.
#[test]
fn finer_and_equal_precision_are_exact() {
    let ts = Timestamp::new(5, Precision::Seconds);
    // s -> ms multiplies by 1_000 exactly, regardless of mode.
    assert_eq!(ts.to_precision(Precision::Millis, RoundingMode::Down).count(), 5_000);
    assert_eq!(ts.to_precision(Precision::Nanos, RoundingMode::HalfUp).count(), 5_000_000_000);
    // Equal precision returns the same count.
    assert_eq!(ts.to_precision(Precision::Seconds, RoundingMode::Ceiling).count(), 5);
}
