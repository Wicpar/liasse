//! RED TEAM — Annex A.7 / A.6 decimal-scale bound is checked against the RAW
//! un-normalized parsed scale, not the value's CANONICAL (minimal-scale)
//! magnitude, so admission depends on the input SPELLING rather than the value.
//!
//! The scale bound exists to bound "a number's scale magnitude (its base-ten
//! exponent — its fractional-digit or trailing-zero count)" (SPEC.md A.7,
//! line 4637). A.1 (line 4496) pins that this magnitude is a property of the
//! value: the canonical form is minimal-scale, "a total function of the
//! decimal's mathematical value ... scale is not part of a decimal's identity".
//! So two spellings of ONE value share ONE scale magnitude and MUST get ONE
//! admission verdict.
//!
//! `decimal.rs::Decimal::parse` instead reads `value.as_bigint_and_exponent().1`
//! — the un-normalized parsed exponent — and bounds THAT. But
//! `to_canonical_text` (decimal.rs:88) normalizes FIRST, so the bound-check and
//! the digit-string materialization it is meant to guard use DIFFERENT scales.
//! The mismatch produces two spec violations, in opposite directions:
//!
//!   over-accept  `10E16383` == 10^16384 has canonical trailing-zero count
//!                16384 (> 16383), yet is ACCEPTED — A.7 says a conforming impl
//!                "MUST NOT accept it or attempt to canonicalize it" (line 4637).
//!                Worse, `to_canonical_text` then materializes the 16385-char
//!                string the bound was meant to prevent — the DoS guard defeated.
//!   over-reject  `1.0E-16383` == 1E-16383 has canonical fractional-digit count
//!                16383 (== the limit, IN bound), yet is REJECTED — A.7 requires
//!                an in-bound json number be accepted and canonicalized.
//!
//! Both surface at the `json` decode boundary, where A.7 is normative and json
//! numbers are always canonicalized (no separate spelling gate). Every expected
//! is hand-derived from A.7 + A.1; none is echoed from the implementation.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_value::{Decimal, Type, ValueError};

/// The pinned bound (wire.rs already pins this constant to PostgreSQL numeric
/// dscale). Re-derived here only to state the boundary each case straddles.
const LIMIT: u64 = 16383;

/// The count of trailing `'0'` characters of a canonical decimal integer text —
/// its canonical trailing-zero count, i.e. the A.7 negative-scale magnitude.
fn trailing_zeros(canonical: &str) -> usize {
    canonical.chars().rev().take_while(|c| *c == '0').count()
}

// ===========================================================================
// HIGH — A.7 MUST-NOT-accept violated (over-accept + DoS guard defeated).
// ===========================================================================

/// `10E16383` is the value `10^16384`: its canonical minimal-scale form is `1`
/// followed by 16384 zeros, a trailing-zero count of 16384 > 16383. A.7: "a
/// `json` value containing a number whose scale magnitude exceeds that limit is
/// rejected ... a conforming implementation MUST NOT accept it or attempt to
/// canonicalize it" (line 4637). Expected: rejected. Actual: accepted.
#[test]
fn json_number_over_canonical_scale_must_be_rejected() {
    let over_scale = serde_json::from_str::<serde_json::Value>("10E16383").unwrap();
    let decoded = Type::Json.decode(&over_scale);
    assert!(
        matches!(decoded, Err(ValueError::DecimalScaleOutOfRange { .. })),
        "A.7 line 4637: 10^16384 has canonical scale magnitude 16384 (> {LIMIT}); a \
         conforming impl MUST NOT accept it. Got: {decoded:?}"
    );
}

/// The teeth behind the previous case: the accepted value canonicalizes to the
/// very digit string the bound exists to prevent. A.7: the impl must not "attempt
/// to canonicalize it". Here it both accepts AND materializes 16384 trailing
/// zeros — the guard is fully defeated.
#[test]
fn accepted_over_scale_value_does_not_materialize_over_bound_string() {
    let d = Decimal::parse("10E16383");
    if let Ok(value) = d {
        let canonical = value.to_canonical_text();
        assert!(
            trailing_zeros(&canonical) as u64 <= LIMIT,
            "A.7 line 4637: the bound must prevent canonicalizing a number whose scale \
             magnitude exceeds {LIMIT}, but a {}-char string with {} trailing zeros was \
             produced from an accepted value",
            canonical.len(),
            trailing_zeros(&canonical)
        );
    }
}

// ===========================================================================
// The value's identity, not its spelling, must decide admission (A.1 line 4496).
// ===========================================================================

/// The SAME value `10^16384` spelled `1E16384` (raw scale 16384) and `10E16383`
/// (raw scale 16383) MUST get the SAME verdict — A.1 line 4496: "numerically
/// equal decimals share exactly one canonical spelling — scale is not part of a
/// decimal's identity." The impl rejects the first and accepts the second.
#[test]
fn same_value_two_spellings_get_same_verdict() {
    let via_1e = Type::Json.decode(&serde_json::from_str("1E16384").unwrap());
    let via_10e = Type::Json.decode(&serde_json::from_str("10E16383").unwrap());
    assert_eq!(
        via_1e.is_ok(),
        via_10e.is_ok(),
        "A.1 line 4496: `1E16384` and `10E16383` are ONE value (10^16384); admission must \
         not depend on the spelling. Got 1E16384 => {:?}, 10E16383 => {:?}",
        via_1e.is_ok(),
        via_10e.is_ok()
    );
}

/// An IN-bound value carried with a trailing fractional zero: `1.0E-16383` ==
/// `1E-16383`, whose canonical fractional-digit count is 16383 (== the limit, in
/// bound). A.7 requires an in-bound json number be accepted and canonicalized;
/// the impl rejects it because its RAW parsed scale is 16384. It must decode to
/// the identical value as its minimal spelling.
#[test]
fn in_bound_value_with_trailing_zero_spelling_must_be_accepted() {
    let canonical = Type::Json.decode(&serde_json::from_str("1E-16383").unwrap());
    let with_zero = Type::Json.decode(&serde_json::from_str("1.0E-16383").unwrap());
    assert!(
        canonical.is_ok(),
        "control: minimal spelling of the max in-bound scale must be accepted"
    );
    assert!(
        with_zero.is_ok(),
        "A.7/A.1: `1.0E-16383` is the in-bound value `1E-16383` (canonical scale {LIMIT}); \
         it must be accepted, not rejected for a trailing-zero spelling. Got {with_zero:?}"
    );
    assert_eq!(
        with_zero.unwrap(),
        canonical.unwrap(),
        "the two spellings are one value and must decode identically"
    );
}

// ===========================================================================
// PASSING CONTROLS — the bound works where raw and canonical scale AGREE, so the
// failures above isolate the raw-vs-canonical mismatch, not the guard itself.
// ===========================================================================

/// The minimal spelling exactly AT the limit (`1E-16383`, canonical scale 16383)
/// decodes, and a genuinely astronomical scale (`1E-2000000000`) is rejected —
/// the guard is correct when the parsed scale already equals the canonical scale.
#[test]
fn control_minimal_spelling_bound_holds() {
    assert!(
        Type::Json.decode(&serde_json::from_str("1E-16383").unwrap()).is_ok(),
        "canonical scale exactly at the limit decodes"
    );
    assert!(
        matches!(
            Type::Json.decode(&serde_json::from_str("1E-2000000000").unwrap()),
            Err(ValueError::DecimalScaleOutOfRange { .. })
        ),
        "a scale magnitude far past the limit is rejected"
    );
    // `1E16383` == 10^16383: canonical trailing-zero count 16383 (== limit, in
    // bound). Raw and canonical scale agree here, so it is correctly accepted.
    assert!(
        Type::Json.decode(&serde_json::from_str("1E16383").unwrap()).is_ok(),
        "10^16383 (canonical scale magnitude 16383) is in bound and decodes"
    );
}

/// Ordinary decimals — where the parsed scale already IS minimal — are unaffected,
/// confirming the defect is confined to the raw-vs-canonical scale mismatch at the
/// boundary rather than a general decode regression.
#[test]
fn control_ordinary_decimals_unaffected() {
    for spelling in ["1", "1.5", "0.001", "100", "-1.23"] {
        assert!(
            Decimal::parse(spelling).is_ok(),
            "ordinary decimal `{spelling}` must decode"
        );
    }
}
