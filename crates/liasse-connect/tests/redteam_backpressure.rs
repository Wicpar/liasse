#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 attack battery item 4 (slow-client / no-stall, D3 backpressure).
//! A stalled SSE consumer must NOT block the actor or another connection, must NOT
//! grow memory without bound, and must recover losslessly by reconstruction. This is
//! a ROBUST sign-off.
//!
//! Properties proven:
//!   * A connection that never drains its stream overflows a BOUNDED ring: its next
//!     poll returns a small `reset(overflow)` + fresh `init`, NOT the arbitrarily
//!     many dropped frames — so memory is bounded, not accumulated.
//!   * The reconstruction is lossless: the stalled client's replica, after the
//!     reset+init, equals the server's recomputed authorized view.
//!   * A healthy connection keeps being served coherently the whole time the other is
//!     stuck (the actor never blocks). The suite is single-threaded and synchronous,
//!     so completing at all is itself the no-deadlock proof.

mod support;

use liasse_wire::serde_json::json;
use liasse_wire::Downstream;

use support::{Client, app, call, drain, hello, server_titles, view};

/// The `event:` kinds a drained batch carries.
fn kinds(events: &[liasse_wire::SseEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| liasse_wire::decode::<Downstream>(&e.data).ok())
        .map(|frame| match frame {
            Downstream::Init { .. } => "init",
            Downstream::Scalar { .. } => "scalar",
            Downstream::Patch { .. } => "patch",
            Downstream::Close { .. } => "close",
            Downstream::Frontier => "frontier",
            Downstream::Reset { .. } => "reset",
            Downstream::Fault { .. } => "fault",
        })
        .collect()
}

#[test]
fn a_stalled_stream_overflows_a_bounded_ring_and_recovers_losslessly() {
    // Capacity 2: at most two outbound frames are retained per connection.
    let mut core = app().with_capacity(2);
    let stalled = hello(&mut core);
    view(&mut core, &stalled, "s", "public.tasks");

    // The stalled client NEVER drains. Far more than `capacity` commits land, each
    // enqueuing a frame it never reads.
    let mut titles = Vec::new();
    for i in 0..50 {
        let title = format!("t{i:02}");
        call(&mut core, &stalled, "public.tasks.add", json!({ "title": &title }), None);
        titles.push(title);
    }
    titles.sort(); // the `index` view is sorted by title

    // The overflow is observed: the next drain is a small reset+init, NOT 50 patches —
    // proving the ring dropped frames instead of growing.
    let recovered = drain(&mut core, &stalled);
    let ks = kinds(&recovered);
    assert!(ks.contains(&"reset"), "an overflowed stream resets: {ks:?}");
    assert!(ks.contains(&"init"), "then re-inits from scratch: {ks:?}");
    assert!(recovered.len() <= 4, "recovery is bounded, not the ~50 dropped frames: {} frames", recovered.len());

    // The reconstruction is lossless: applying the reset+init reproduces the full
    // current authorized view.
    let mut client = Client::new();
    client.feed(&recovered);
    assert_eq!(client.titles("s"), titles, "the stalled client recovers the complete state by reconstruction");
    assert_eq!(client.titles("s"), server_titles(&core, &stalled, "s"), "and it equals the server's recomputed view");
}

#[test]
fn a_healthy_connection_is_served_while_another_is_stalled() {
    let mut core = app().with_capacity(2);

    let stalled = hello(&mut core);
    let healthy = hello(&mut core);
    view(&mut core, &stalled, "s", "public.tasks");
    view(&mut core, &healthy, "h", "public.tasks");

    let mut healthy_client = Client::new();
    healthy_client.feed(&drain(&mut core, &healthy)); // consume the healthy init

    // Interleave: the stalled connection floods without ever draining; the healthy
    // connection commits AND drains every round and must stay coherent throughout.
    for i in 0..10 {
        // Stalled: enqueue-and-abandon.
        call(&mut core, &stalled, "public.tasks.add", json!({ "title": format!("s{i:02}") }), None);

        // Healthy: a normal commit + drain. The actor must still serve it.
        let outcome = call(&mut core, &healthy, "public.tasks.add", json!({ "title": format!("h{i:02}") }), None);
        assert!(matches!(outcome, liasse_wire::Outcome::Committed { .. }), "healthy commit {i} served: {outcome:?}");
        healthy_client.feed(&drain(&mut core, &healthy));

        // The healthy client's applied replica equals the server's recomputed view at
        // every step — the stalled peer never corrupts or blocks it.
        assert_eq!(
            healthy_client.titles("h"),
            server_titles(&core, &healthy, "h"),
            "the healthy connection stays coherent while the peer is stalled (round {i})",
        );
    }

    // The stalled connection, when it finally reconnects, recovers losslessly — its
    // backpressure never harmed the healthy one.
    let recovered = drain(&mut core, &stalled);
    assert!(kinds(&recovered).contains(&"reset"), "the stalled stream reset on overflow");
    let mut stalled_client = Client::new();
    stalled_client.feed(&recovered);
    assert_eq!(stalled_client.titles("s"), server_titles(&core, &stalled, "s"), "the stalled client recovers fully");
}
