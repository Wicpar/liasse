#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §14.1–§14.2 temporal selectors over bucketed collections: `.$at(t)` returns
//! the rows whose half-open interval `[from, until)` contains `t`,
//! `.$between(a, b)` the rows intersecting the non-empty range `[a, b)`, and
//! `.$all` every extant row regardless of current activity. Sessions here are
//! short-form buckets (`$bucket: ".expires_at"`), so a session is active while
//! `now < expires_at` and unbounded below. Every expectation is re-derived from
//! §14.1/§14.2 text, independent of the virtual clock (each selector carries its
//! own instants).

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Timestamp, Value};
use liasse_value::{Precision, Text};
use support::{generator, load, BUCKETS, NOW_MICROS};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn at(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// Open a session `id` expiring at `NOW + delta` micros.
fn open(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, id: &str, delta: i128) {
    let mut generator = generator();
    let outcome = engine
        .call(
            &CallRequest::new("open_session").arg("id", text(id)).arg("expires_at", at(NOW_MICROS + delta)),
            &mut generator,
        )
        .expect("open_session call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the session commits");
}

/// The `id` values a read-only temporal mutation returned, in result order.
fn ids(engine: &mut liasse_runtime::Engine<liasse_store::MemoryStore>, request: CallRequest) -> Vec<String> {
    let mut generator = generator();
    let outcome = engine.call(&request, &mut generator).expect("temporal call");
    let response = outcome.response().expect("the read-only mutation returns a value");
    match response.to_wire() {
        serde_json::Value::Array(rows) => rows
            .into_iter()
            .map(|row| row["id"].as_str().expect("id is a text field").to_owned())
            .collect(),
        other => panic!("expected an array of rows, got {other}"),
    }
}

/// A fixture of two short-form sessions: `s1` expires at `NOW + 1000`, `s2` at
/// `NOW + 3000`. Both are active at load time (`NOW`).
fn two_sessions() -> liasse_runtime::Engine<liasse_store::MemoryStore> {
    let mut engine = load("temporal", BUCKETS);
    open(&mut engine, "s1", 1_000);
    open(&mut engine, "s2", 3_000);
    engine
}

/// §14.1: `.$at(t)` selects exactly the rows active at `t` — `from <= t < until`.
/// With `from` unbounded, a session is active while `t < expires_at`; the bound
/// itself is excluded (half-open). Asserted at three instants the clock never
/// visits, so the result follows only from the selector argument.
#[test]
fn at_selects_rows_active_at_the_queried_instant() {
    let mut engine = two_sessions();

    // Before either expiry: both active.
    let mid = CallRequest::new("sessions_at").arg("t", at(NOW_MICROS + 500));
    assert_eq!(ids(&mut engine, mid), vec!["s1".to_owned(), "s2".to_owned()]);

    // Between the two expiries: s1 has ended (2000 >= 1000), s2 still active.
    let after_s1 = CallRequest::new("sessions_at").arg("t", at(NOW_MICROS + 2_000));
    assert_eq!(ids(&mut engine, after_s1), vec!["s2".to_owned()]);

    // At s1's exact bound: excluded, because [from, until) omits until.
    let boundary = CallRequest::new("sessions_at").arg("t", at(NOW_MICROS + 1_000));
    assert_eq!(ids(&mut engine, boundary), vec!["s2".to_owned()]);

    // Past both expiries: none active.
    let after_both = CallRequest::new("sessions_at").arg("t", at(NOW_MICROS + 5_000));
    assert!(ids(&mut engine, after_both).is_empty());
}

/// §14.2: `.$all` exposes every extant row independent of current activity, so an
/// already-expired session still appears — and it appears even after the clock
/// has advanced past its bound, where a bare read hides it (§14.1). Expiry is
/// view membership, never deletion.
#[test]
fn all_exposes_extant_rows_including_expired() {
    let mut engine = two_sessions();

    // At load both are extant and active.
    assert_eq!(
        ids(&mut engine, CallRequest::new("sessions_all")),
        vec!["s1".to_owned(), "s2".to_owned()],
    );

    // Advance past s1's bound: a bare read drops it, but `.$all` still lists it.
    engine.set_time(Timestamp::new(NOW_MICROS + 2_000, Precision::Micros));
    let active = engine.view_at_head("active_sessions").expect("view").expect("declared");
    assert_eq!(active.len(), 1, "the bare active view hides the expired session");
    assert_eq!(
        ids(&mut engine, CallRequest::new("sessions_all")),
        vec!["s1".to_owned(), "s2".to_owned()],
        "`.$all` still exposes the expired session (§14.2)",
    );
}

/// §14.1: `.$between(a, b)` selects the rows whose interval intersects the
/// non-empty range `[a, b)`. With `from` unbounded, a session intersects `[a, b)`
/// iff `until > a`. A session whose `until` equals `a` does not intersect
/// (half-open). Verified with the clock past both bounds, so activity plays no
/// part — only the queried window does.
#[test]
fn between_selects_intersecting_intervals() {
    let mut engine = two_sessions();
    engine.set_time(Timestamp::new(NOW_MICROS + 9_000, Precision::Micros));

    // [1500, 2500): s1 (until 1000) does not reach 1500; s2 (until 3000) does.
    let window = CallRequest::new("sessions_between").arg("a", at(NOW_MICROS + 1_500)).arg("b", at(NOW_MICROS + 2_500));
    assert_eq!(ids(&mut engine, window), vec!["s2".to_owned()]);

    // [0, 4000): both intervals intersect.
    let wide = CallRequest::new("sessions_between").arg("a", at(NOW_MICROS)).arg("b", at(NOW_MICROS + 4_000));
    assert_eq!(ids(&mut engine, wide), vec!["s1".to_owned(), "s2".to_owned()]);

    // Lower bound exactly at s2.until = NOW+3000: half-open, so s2 is excluded
    // (and s1 ended long before), leaving no intersection.
    let touch = CallRequest::new("sessions_between").arg("a", at(NOW_MICROS + 3_000)).arg("b", at(NOW_MICROS + 3_200));
    assert!(ids(&mut engine, touch).is_empty(), "an interval touching only at `until` does not intersect");
}
