#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! The wire invariants that keep the connector safe and coherent (§12): frontier
//! tokens are monotone, §12.3 patches are enqueued before the committed reply,
//! occurrence tokens are stable and never reused, and every forged or malformed input
//! is a fault — never a panic, never a leak.

mod support;

use liasse_wire::serde_json::json;
use liasse_wire::{
    ConnectionToken, Downstream, Occ, Outcome, SseEvent, Upstream, WireAnchor, WireWindow,
};
use support::{Client, app, call, drain, hello, view, view_request};

/// The occurrence token of the row titled `title` in subscription `sub`.
fn occ_of(client: &Client, sub: &str, title: &str) -> Occ {
    let rows = client.rows(sub);
    let occs = client.occ(sub);
    rows.iter()
        .zip(occs)
        .find(|(row, _)| row.get("title") == Some(&json!(title)))
        .map(|(_, occ)| occ)
        .expect("row present")
}

#[test]
fn frontier_tokens_are_monotone_per_connection() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    let mut ids: Vec<String> = Vec::new();
    let mut collect = |events: &[SseEvent]| {
        for event in events {
            if let Some(id) = &event.id {
                ids.push(id.clone());
            }
        }
    };
    collect(&drain(&mut core, &conn));
    for title in ["m", "a", "z", "b"] {
        call(&mut core, &conn, "public.tasks.add", json!({ "title": title }), None);
        collect(&drain(&mut core, &conn));
    }

    // Each id decodes to a frontier position; the sequence never regresses.
    let positions: Vec<u64> = ids
        .iter()
        .map(|id| core.frontier_position(&conn, &liasse_wire::Ft::new(id.clone())).expect("own frontier token"))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] <= pair[1]), "frontier positions are monotone: {positions:?}");
    assert!(positions.iter().any(|&p| p > 0), "the frontier actually advanced");
}

#[test]
fn patch_frames_are_enqueued_before_the_committed_reply() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");
    let _ = drain(&mut core, &conn);

    let outcome = call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);
    let Outcome::Committed { frontier, .. } = outcome else {
        panic!("the add committed: {outcome:?}");
    };

    // The moment the committed reply is in hand, the §12.2 patch is already buffered on
    // the stream, stamped with the very frontier the reply reports.
    let events = drain(&mut core, &conn);
    let patch = events
        .iter()
        .find(|event| matches!(liasse_wire::decode::<Downstream>(&event.data), Ok(Downstream::Patch { .. })))
        .expect("a patch was enqueued before the reply returned");
    assert_eq!(patch.id.as_deref(), Some(frontier.as_str()), "the patch carries the committed frontier");
}

#[test]
fn occurrence_tokens_are_stable_and_never_reused() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");
    let mut client = Client::new();

    call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);
    client.feed(&drain(&mut core, &conn));
    let m_first = occ_of(&client, "w1", "m");

    // Inserting another row before "m" must not disturb "m"'s occurrence token.
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "a" }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(occ_of(&client, "w1", "m"), m_first, "the occurrence token is stable across neighbor inserts");

    // Removing "m" and adding a fresh task must mint a new token — never reuse the
    // departed one for a different occurrence.
    let id = support::task_id_json(&core, "m");
    call(&mut core, &conn, "public.tasks.remove", json!({ "id": id }), None);
    client.feed(&drain(&mut core, &conn));
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "n" }), None);
    client.feed(&drain(&mut core, &conn));
    let n_occ = occ_of(&client, "w1", "n");
    assert_ne!(n_occ, m_first, "a new occurrence never reuses a departed token");
}

#[test]
fn a_forged_connection_token_is_a_fault() {
    let mut core = app();
    let _ = hello(&mut core);
    let forged = ConnectionToken::new("forged-connection");
    let error = core
        .submit(Some(&forged), None, Upstream::Manifest)
        .expect_err("a forged connection is refused");
    assert!(matches!(error, liasse_connect::ConnectError::NoConnection));
}

#[test]
fn a_forged_occurrence_anchor_is_a_fault() {
    let mut core = app();
    let conn = hello(&mut core);
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);

    let window = WireWindow { size: 2, anchor: WireAnchor::At { occ: Occ::new("forged-occ") }, slide: false };
    let error = core
        .submit(Some(&conn), None, view_request("bad", "public.tasks", Some(window)))
        .expect_err("a forged anchor token is refused");
    assert!(matches!(error, liasse_connect::ConnectError::BadToken));
}

#[test]
fn a_mistyped_or_unknown_argument_is_rejected_not_panicked() {
    let mut core = app();
    let conn = hello(&mut core);

    // `title` is `text`; a number does not decode against it.
    let mistyped = call(&mut core, &conn, "public.tasks.add", json!({ "title": 123 }), None);
    assert!(matches!(mistyped, Outcome::Rejected { .. }), "a mistyped argument is rejected: {mistyped:?}");

    // An argument the schema does not declare is refused, not silently admitted.
    let unknown = call(&mut core, &conn, "public.tasks.add", json!({ "title": "ok", "bogus": 1 }), None);
    assert!(matches!(unknown, Outcome::Rejected { .. }), "an unknown argument is rejected: {unknown:?}");
}

#[test]
fn a_forged_frontier_token_resumes_without_panic() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), None);

    // A garbage Last-Event-ID is not this connection's token: it falls back to a fresh
    // init at the current frontier rather than replaying a gap or panicking.
    let events = core.resume(&conn, Some("forged-frontier")).expect("resume tolerates a forged id");
    let mut client = Client::new();
    client.feed(&events);
    assert_eq!(client.titles("w1"), ["m"], "the fresh init reproduces the current state");
}

#[test]
fn a_malformed_frame_fails_to_decode() {
    // The codec boundary rejects a truncated or ill-typed frame; the binding turns this
    // into a `fault`, never a panic.
    assert!(liasse_wire::decode::<Upstream>("{ not json").is_err());
    assert!(liasse_wire::decode::<Upstream>(r#"{ "type": "no_such_frame" }"#).is_err());
    // A frame that does decode still round-trips cleanly.
    let ok: Result<Upstream, _> = liasse_wire::decode(r#"{ "type": "manifest" }"#);
    assert!(matches!(ok, Ok(Upstream::Manifest)));
}
