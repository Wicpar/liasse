#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 per-frontier re-authorization: a commit that removes a subscription's
//! authority closes it at the next outgoing frontier — on its own connection and, for
//! the peer whose commit revoked the grant, across connections.

mod support;

use liasse_wire::serde_json::json;
use support::{Client, app, call, drain, hello, hello_member, view};

#[test]
fn revoking_a_session_closes_the_member_subscription() {
    let mut core = app();
    let conn = hello_member(&mut core, "s_alice");
    view(&mut core, &conn, "m1", "member.tasks");

    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    assert!(!client.closed("m1"), "the member subscription opens live");

    // A revoke on the same connection sweeps the member subscription at the new
    // frontier; re-authorization fails, so it closes.
    call(&mut core, &conn, "public.session.revoke", json!({ "id": "s_alice" }), None);
    client.feed(&drain(&mut core, &conn));

    assert!(client.closed("m1"), "the client observed the close");
    assert!(core.host().close_reason(conn.as_str(), "m1").is_some(), "the server closed the subscription");
}

#[test]
fn a_peer_commit_closes_a_cross_connection_subscription() {
    let mut core = app();
    let alice = hello_member(&mut core, "s_alice");
    view(&mut core, &alice, "m1", "member.tasks");
    let mut client = Client::new();
    client.feed(&drain(&mut core, &alice));

    // A second, public connection revokes alice's session. Alice issued no request,
    // yet her subscription's authority is re-evaluated at that outgoing frontier and
    // removed, so it closes on her connection.
    let peer = hello(&mut core);
    call(&mut core, &peer, "public.session.revoke", json!({ "id": "s_alice" }), None);

    client.feed(&drain(&mut core, &alice));
    assert!(client.closed("m1"), "the cross-connection subscription closed at the peer's frontier");
}
