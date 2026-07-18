#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 resume: a `Last-Event-ID` replays the retained tail and reproduces the
//! client's state; when the ring has evicted that range, the connection falls back to
//! a fresh init at the current frontier — always correct, never a gapped replay.

mod support;

use liasse_wire::serde_json::json;
use support::{Client, app, call, drain, hello, server_titles, view};

#[test]
fn last_event_id_replay_reproduces_state() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    // A client applies the init, then disconnects, retaining the init's frontier.
    let init = drain(&mut core, &conn);
    let resume_from = init.last().and_then(|event| event.id.clone()).expect("init carries a frontier id");
    let mut applied = Client::new();
    applied.feed(&init);

    // While it is gone, two commits advance the view.
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "a" }), None);

    // It reconnects with the retained frontier: the server replays the buffered tail,
    // which carries the applied replica to the current authorized state.
    let replayed = core.resume(&conn, Some(&resume_from)).expect("resume");
    applied.feed(&replayed);
    assert_eq!(applied.titles("w1"), ["a", "m"]);
    assert_eq!(applied.titles("w1"), server_titles(&core, &conn, "w1"));
}

#[test]
fn a_released_range_falls_back_to_a_fresh_init() {
    // A tiny outbound bound forces old frames out of the ring; a resume from a frontier
    // whose tail was released re-inits at the current frontier rather than replaying a
    // gap.
    let mut core = app().with_capacity(2);
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    // The client retains the (empty) init frontier, then disconnects.
    let init = drain(&mut core, &conn);
    let stale = init.last().and_then(|event| event.id.clone()).expect("init frontier");

    // Enough commits pass (each delivered, so the ring evicts old delivered frames)
    // that the retained frontier's tail is no longer buffered.
    for title in ["a", "b", "c", "d", "e"] {
        call(&mut core, &conn, "public.tasks.add", json!({ "title": title }), None);
        let _ = drain(&mut core, &conn);
    }

    // Resuming from the released frontier re-inits the current authorized state.
    let events = core.resume(&conn, Some(&stale)).expect("resume");
    let mut client = Client::new();
    client.feed(&events);
    assert_eq!(client.titles("w1"), ["a", "b", "c", "d", "e"]);
    assert_eq!(client.titles("w1"), server_titles(&core, &conn, "w1"));
}

#[test]
fn an_unknown_connection_resets() {
    let mut core = app();
    let phantom = liasse_wire::ConnectionToken::new("not-a-real-connection");
    let events = core.resume(&phantom, Some("whatever")).expect("resume");
    let mut client = Client::new();
    client.feed(&events);
    // The client is told to re-establish from scratch (§22 volatile subscriptions).
    assert!(client.rows("w1").is_empty());
}
