#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 / §22.6 temporal observations on a live view: advancing the virtual
//! clock past a bucketed row's active interval removes it from the live result
//! without any commit, and — because the session-expiry clock advances with it —
//! a role subscription whose session has expired closes at the advanced instant.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, Subscription, SurfaceBinding, SurfaceHost, SurfaceRouter,
    SurfaceRouterBuilder, SurfaceWatch, Timestamp, Value, ViewBinding, VirtualClock,
};
use support::{address, authenticate_member, call, host, text};

/// The virtual-clock start these temporal tests run at (micro-second precision).
const START: i128 = 1_767_225_600_000_000;

/// Offers keyed by id, bucketed on `expires_at`, with an `active` view that lists
/// only the rows active at the virtual clock (§14.1) — the shape §22.6 needs.
const OFFERS_APP: &str = r#"{
  "$liasse": 1,
  "$app": "t.offers@1.0.0",
  "$model": {
    "offers": { "$key": "id", "$bucket": ".expires_at", "id": "text", "expires_at": "timestamp" },
    "$mut": { "add": ".offers + { id: @id, expires_at: @expires_at }" },
    "active": { "$view": ".offers { id, $sort: [id] }" },
    "$public": { "offers": { "$view": ".active", "$mut": { "add": ".add" } } }
  }
}"#;

fn offers_host() -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(START, Precision::Micros);
    let engine = Engine::load(support::store("offers"), OFFERS_APP, &mut clock).expect("offers loads");
    let router = offers_router(engine.model());
    SurfaceHost::new(engine, router, clock)
}

fn offers_router(model: &liasse_model::Model) -> SurfaceRouter {
    let offers = SurfaceBinding::new()
        .with_view(ViewBinding::new("active"))
        .with_call("add", CallBinding::root("add", ["id".to_owned(), "expires_at".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("offers", offers)
        .build(model)
        .expect("offers router validates against the model")
}

/// A micro-precision timestamp value at absolute `micros`.
fn at(micros: i128) -> Value {
    Value::Timestamp(Timestamp::new(micros, Precision::Micros))
}

/// The number of rows the live subscription `id` on `conn` currently tracks.
fn live_len(host: &SurfaceHost<MemoryStore>, conn: &str, id: &str) -> usize {
    host.read_view(conn, id).expect("live view present").len()
}

#[test]
fn temporal_expiry_removes_a_row_from_the_live_view_without_a_commit() {
    // §22.6: when a row leaves its half-open active interval the runtime reflects
    // the new current view and advances the live frontier; §14.1 makes the
    // interval half-open, so the row is inactive at and after `expires_at`. No
    // application commit is involved — only the clock moved.
    let mut host = offers_host();
    host.connect("c1").unwrap();
    // Active until START + 1h.
    let expires = START + 3_600_000_000;
    host.call("c1", &call("public.offers.add", [("id", text("o1")), ("expires_at", at(expires))]))
        .expect("add")
        .commit()
        .expect("the add commits");

    match host.watch("c1", &SurfaceWatch::new(address("public.offers"), "w1")).expect("watch") {
        Subscription::Init(result) => assert_eq!(result.len(), 1, "the offer is active at open"),
        other => panic!("expected an init, got {other:?}"),
    }

    // Advance two hours — past the offer's half-open active interval.
    host.advance_time(Timestamp::new(START + 2 * 3_600_000_000, Precision::Micros)).expect("advance");
    assert_eq!(live_len(&host, "c1", "w1"), 0, "the expired offer left the live view after the temporal advance");
}

#[test]
fn temporal_advance_before_expiry_keeps_the_row() {
    // The half-open interval is [start, expires_at): the row is still active at any
    // instant strictly before `expires_at`, so a sub-expiry advance keeps it.
    let mut host = offers_host();
    host.connect("c1").unwrap();
    let expires = START + 3_600_000_000;
    host.call("c1", &call("public.offers.add", [("id", text("o1")), ("expires_at", at(expires))]))
        .expect("add")
        .commit()
        .expect("the add commits");
    host.watch("c1", &SurfaceWatch::new(address("public.offers"), "w1")).expect("watch");

    // Advance to one minute before expiry.
    host.advance_time(Timestamp::new(expires - 60_000_000, Precision::Micros)).expect("advance");
    assert_eq!(live_len(&host, "c1", "w1"), 1, "the still-active offer remains in the live view");
}

#[test]
fn session_expiry_at_an_advanced_instant_closes_a_role_subscription() {
    // §11.7: the surface layer judges session validity against the virtual clock.
    // Advancing it past a member's `expires_at` must re-evaluate the subscription's
    // authority (§12.2) and close it — the same authority-loss path a revoke takes,
    // reached here by the clock rather than a commit.
    let mut host = host();
    host.connect("c1").unwrap();
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), liasse_surface::AuthResult::Bound));
    match host.watch("c1", &SurfaceWatch::new(address("member.tasks"), "m1")).expect("watch") {
        Subscription::Init(_) => {}
        other => panic!("the member watch opens, got {other:?}"),
    }

    // `s_alice` expires at 2_000_000_000_000_000 (support::FUTURE). Advance past it.
    host.advance_time(Timestamp::new(support::FUTURE + 1, Precision::Micros)).expect("advance");
    assert!(
        host.close_reason("c1", "m1").is_some(),
        "the member subscription closes once its session has expired at the advanced instant"
    );
    assert!(host.read_view("c1", "m1").is_none(), "a closed subscription releases its cached result");
}
