#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 per-frontier re-authorization: a commit that removes a subscription's
//! authority closes it at the next outgoing frontier — on its own connection and, for
//! the peer whose commit revoked the grant, across connections.

mod support;

use liasse_connect::Reply;
use liasse_wire::serde_json::json;
use liasse_wire::{Outcome, Upstream};
use support::{Client, app, call, drain, hello, hello_member, view};

#[test]
fn a_non_member_cannot_distinguish_an_ungranted_surface_from_a_nonexistent_one() {
    // SPEC-ISSUES item 8: bob authenticates as `member` (his session and account
    // resolve) but is not a member — his account is disabled. Probing an EXISTING
    // ungranted role surface (`member.tasks.complete`) and a NONEXISTENT one
    // (`member.ghost.complete`) under the same role MUST yield an identical denial
    // — same `denied` class AND wire code and message — so a non-member can never
    // enumerate the role's surface catalog. The probe carries no arguments: a
    // non-member is denied at membership resolution, before any argument shape is
    // considered, so this isolates the name-resolution axis item 8 pins.
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");

    let existing = call(&mut core, &conn, "member.tasks.complete", json!({}), None);
    let ghost = call(&mut core, &conn, "member.ghost.complete", json!({}), None);

    match (existing, ghost) {
        (
            Outcome::Denied { code: real_code, message: real_message },
            Outcome::Denied { code: ghost_code, message: ghost_message },
        ) => {
            assert_eq!(real_code, ghost_code, "the wire code must not reveal that `tasks` exists");
            assert_eq!(real_message, ghost_message, "the sanitized message must be identical too");
        }
        other => panic!("both probes must be denied, got {other:?}"),
    }
}

#[test]
fn a_public_call_carrying_an_authenticator_selection_is_rejected() {
    // SPEC-ISSUES item 8 / §10.2 / §11.4: a public address carries no authenticator
    // selection. Attaching a credential to a public call is malformed — the runtime
    // rejects it rather than dropping the selection and serving the call actor-less.
    let mut core = app();
    let conn = hello(&mut core);
    let frame = Upstream::Call {
        address: "public.intake.add".to_owned(),
        args: json!({ "title": "hi" }),
        auth: Some(json!({ "auth": "token", "credential": "s_alice" })),
        context: None,
    };
    match core.submit(Some(&conn), None, frame) {
        Ok(Reply::Outcome(outcome)) => {
            assert!(
                matches!(outcome, Outcome::Rejected { .. }),
                "a public call carrying a selection is rejected as malformed: {outcome:?}"
            );
        }
        other => panic!("expected a call outcome, got {other:?}"),
    }
}

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
