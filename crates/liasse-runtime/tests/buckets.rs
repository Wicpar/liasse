#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §14 lifecycle buckets over the engine's virtual clock: a bucketed row is
//! active only within its half-open interval `[from, until)`, ordinary reads
//! expose exactly the active rows at the clock instant, activity is view
//! membership rather than deletion, and an invalid finite interval is rejected
//! at admission. Every expectation is re-derived from §14.1/§14.2 text.

mod support;

use liasse_runtime::{CallOutcome, CallRequest, RejectionReason, Timestamp, Value};
use liasse_value::{Precision, Text};
use support::{generator, load, BUCKETS, NOW_MICROS};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn at(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// §14.1: `.sessions` returns the rows active at the evaluation time. The clock
/// starts at load; the row is active while `now < expires_at` and leaves every
/// active view at the exact `expires_at` instant because the interval is
/// half-open. The row remains extant (it reappears when the clock returns before
/// the bound), so expiry is view membership, not deletion (§14.2).
#[test]
fn upper_bound_is_half_open_at_the_exact_instant() {
    let mut engine = load("buckets-upper", BUCKETS);
    let mut generator = generator();
    let expiry = NOW_MICROS + 1_000;

    let outcome = engine
        .call(
            &CallRequest::new("open_session").arg("id", text("s1")).arg("expires_at", at(expiry)),
            &mut generator,
        )
        .expect("call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the session commits");

    // At load time the session is active (now < expiry).
    let view = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "an unexpired session is active");

    // One tick before the bound: still active.
    engine.set_time(Timestamp::new(expiry - 1, Precision::Micros));
    let view = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "the session is active up to but not including its bound");

    // At the exact bound instant: inactive, because [from, until) excludes until.
    engine.set_time(Timestamp::new(expiry, Precision::Micros));
    let view = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert!(view.is_empty(), "the session leaves the active view at the exact expiry instant");

    // Past the bound: still inactive.
    engine.advance(1_000_000);
    let view = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert!(view.is_empty(), "an expired session stays out of the active view");

    // Return the clock before the bound: the row is still extant, so it is
    // active again — expiry never deleted it (§14.2).
    engine.set_time(Timestamp::new(NOW_MICROS, Precision::Micros));
    let view = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "the expired-then-current session reappears; it was never deleted");
}

/// §14.2: an omitted `$until` is an unbounded upper interval, and the lower
/// bound is inclusive — a row with a future `$from` is inactive until the clock
/// reaches it, then active forever after.
#[test]
fn lower_bound_activation_is_inclusive_and_unbounded_above() {
    let mut engine = load("buckets-lower", BUCKETS);
    let mut generator = generator();
    let start = NOW_MICROS + 5_000;

    engine
        .call(
            &CallRequest::new("open_license").arg("id", text("l1")).arg("starts_at", at(start)),
            &mut generator,
        )
        .expect("call");

    // Before the start: not yet active.
    let view = engine.view_at_head("active_licenses").expect("view").expect("declared");
    assert!(view.is_empty(), "a not-yet-started license is inactive");

    // Exactly at the start instant: active (the lower bound is inclusive).
    engine.set_time(Timestamp::new(start, Precision::Micros));
    let view = engine.view_at_head("active_licenses").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "the license activates at its exact start instant");

    // Far in the future: still active — no upper bound.
    engine.advance(1_000_000_000);
    let view = engine.view_at_head("active_licenses").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "an unbounded-above license never expires");
}

/// §14.2: a finite interval MUST satisfy `$until > $from`; a transition
/// producing an invalid interval rejects, and the whole admission is refused
/// (committed state is untouched — the valid reservation admitted first survives).
#[test]
fn invalid_finite_interval_is_rejected() {
    let mut engine = load("buckets-invalid", BUCKETS);
    let mut generator = generator();
    let head = engine.head();

    // A well-formed reservation admits.
    let ok = engine
        .call(
            &CallRequest::new("reserve")
                .arg("id", text("r1"))
                .arg("starts_at", at(NOW_MICROS))
                .arg("ends_at", at(NOW_MICROS + 1_000)),
            &mut generator,
        )
        .expect("call");
    assert!(matches!(ok, CallOutcome::Committed { .. }), "a forward interval admits");
    assert_ne!(engine.head(), head, "the valid reservation advanced the frontier");
    let committed_head = engine.head();

    // An empty interval (ends_at == starts_at) is rejected: until is not strictly
    // after from.
    let empty = engine
        .call(
            &CallRequest::new("reserve")
                .arg("id", text("r2"))
                .arg("starts_at", at(NOW_MICROS))
                .arg("ends_at", at(NOW_MICROS)),
            &mut generator,
        )
        .expect("call");
    let rejection = empty.rejection().expect("empty interval is rejected");
    assert_eq!(rejection.reason(), RejectionReason::Evaluation, "an invalid interval is an evaluation refusal");

    // A reversed interval (ends_at < starts_at) is likewise rejected.
    let reversed = engine
        .call(
            &CallRequest::new("reserve")
                .arg("id", text("r3"))
                .arg("starts_at", at(NOW_MICROS + 2_000))
                .arg("ends_at", at(NOW_MICROS + 1_000)),
            &mut generator,
        )
        .expect("call");
    assert!(reversed.rejection().is_some(), "a reversed interval is rejected");

    // Neither rejection touched committed state.
    assert_eq!(engine.head(), committed_head, "rejected reservations left the frontier intact");
    let view = engine.view_at_head("active_reservations").expect("view").expect("declared");
    assert_eq!(view.len(), 1, "only the one valid reservation exists");
}
