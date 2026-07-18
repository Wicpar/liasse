#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 attack battery item 3 (stream transport security) and invariant #5
//! (capability confinement). REGRESSION GUARD for the fixed CRITICAL.
//!
//! WAS (CRITICAL): the connection was a single nonce that was simultaneously the
//! bearer credential AND the plaintext body of every frontier/occurrence token
//! (`Nonce::connection_token` returned the nonce; `Nonce::frontier`/`occurrence`
//! embedded it). Because a frontier token legitimately rides the SSE `id:` — through
//! browser history, access logs, and the `Referer` header — an attacker who saw ONE
//! split the nonce out and presented it as `Liasse-Connection`, stealing the victim's
//! authorized stream and POSTing as them.
//!
//! NOW (FIXED): a connection carries TWO independent values (crates/liasse-connect/
//! src/token.rs `ConnKeys`): a SECRET credential `C` (the registry key and the
//! `Liasse-Connection` value) and a non-secret PUBLIC id `P`. Only `P` is embedded in
//! ft/occ; `C` never appears in any token. So a leaked frontier/occurrence token
//! reveals only `P`, and `P` opens nothing: reaching a connection at all requires
//! presenting `C` at the registry, and `P` is not `C`.
//!
//! The downstream stream is now bound by an anonymous in-band ephemeral session (no
//! cookie, no resume URL), so the SSE `id:` frontier token is the only token that still
//! rides a channel an attacker can observe — which is exactly what these tests probe.
//!
//! Security property GUARDED: a frontier/occurrence token is not a credential and,
//! alone, yields no authority over the connection (§12.2; AGENTS.md untrusted-frontend
//! / opaque-identity). These tests assert the token never embeds `C` and that whatever
//! an attacker can extract from it grants nothing — including binding a victim's socket.

mod support;

use std::net::TcpListener;

use liasse_wire::serde_json::{json, Value as Json};
use liasse_wire::{ConnectionToken, Downstream, Upstream};

use liasse_connect::ConnectError;
use support::{app, call, drain, hello, http_request, view, SseSocket};

/// The value an attacker can pull out of a leaked ft/occ token body by splitting on
/// the token's separators — the per-connection PUBLIC id `P`. The point of the guard
/// is that this is NOT the connection credential `C`, so wrapping it as a
/// [`ConnectionToken`] and presenting it opens nothing.
fn embedded_id_of(token: &str) -> ConnectionToken {
    let body = token
        .strip_prefix("f.")
        .or_else(|| token.strip_prefix("o."))
        .expect("a default-minter ft/occ token starts with `f.`/`o.`");
    let (public_id, _tail) = body.rsplit_once('.').expect("token body is `public_id.position`");
    ConnectionToken::new(public_id)
}

#[test]
fn a_frontier_token_does_not_reveal_the_connection_capability() {
    let mut core = app();
    let victim = hello(&mut core);
    view(&mut core, &victim, "w1", "public.tasks");
    call(&mut core, &victim, "public.tasks.add", json!({ "title": "secret" }), None);

    // The SSE `id:` on any downstream frame is the frontier token the design lets ride
    // the resume URL. Take one exactly as a proxy/log/Referer would see it.
    let leaked_ft = drain(&mut core, &victim)
        .iter()
        .find_map(|event| event.id.clone())
        .expect("a downstream frame carries a frontier id");

    // The connection credential is nowhere in the token — not even as a substring, so
    // no splitting scheme recovers it.
    assert!(
        !leaked_ft.contains(victim.as_str()),
        "the frontier token must not embed the connection credential anywhere: {leaked_ft}",
    );

    // What an attacker CAN extract (the public id) is not the credential...
    let extracted = embedded_id_of(&leaked_ft);
    assert_ne!(
        extracted.as_str(),
        victim.as_str(),
        "the value carried by a frontier token is the public id, never the credential",
    );

    // ...and it opens nothing: presenting it as the connection is an unknown connection.
    let hijack = core.submit(Some(&extracted), None, Upstream::Manifest);
    assert!(
        matches!(hijack, Err(ConnectError::NoConnection)),
        "the value extracted from a frontier token grants no connection authority: {hijack:?}",
    );

    // Resuming as that value yields only an unknown-connection reset, never the
    // victim's authorized init stream.
    let stream = core.resume(&extracted, None).expect("resume tolerates an unknown connection");
    assert!(
        stream
            .iter()
            .all(|e| !matches!(liasse_wire::decode::<Downstream>(&e.data), Ok(Downstream::Init { .. }))),
        "the extracted value cannot replay the victim's stream",
    );
    assert!(
        stream
            .iter()
            .any(|e| matches!(liasse_wire::decode::<Downstream>(&e.data), Ok(Downstream::Reset { .. }))),
        "an unknown connection is only told to re-establish",
    );
}

#[test]
fn an_occurrence_token_does_not_reveal_the_connection_capability() {
    let mut core = app();
    let victim = hello(&mut core);
    view(&mut core, &victim, "w1", "public.tasks");
    call(&mut core, &victim, "public.tasks.add", json!({ "title": "row" }), None);

    // Occurrence tokens ride the SSE body (init rows / patch inserts) and are echoed
    // back by the client as window anchors, so they too reach the untrusted frontend.
    let leaked_occ = drain(&mut core, &victim)
        .iter()
        .filter_map(|e| liasse_wire::decode::<Downstream>(&e.data).ok())
        .find_map(|frame| match frame {
            Downstream::Init { rows, .. } => rows.first().map(|r| r.occ().clone()),
            Downstream::Patch { ops, .. } => ops.first().map(|op| op.occ().clone()),
            _ => None,
        })
        .expect("a row occurrence token");

    assert!(
        !leaked_occ.as_str().contains(victim.as_str()),
        "the occurrence token must not embed the connection credential anywhere: {}",
        leaked_occ.as_str(),
    );

    let extracted = embedded_id_of(leaked_occ.as_str());
    assert_ne!(
        extracted.as_str(),
        victim.as_str(),
        "the value carried by an occurrence token is the public id, never the credential",
    );
    assert!(
        matches!(core.submit(Some(&extracted), None, Upstream::Manifest), Err(ConnectError::NoConnection)),
        "an occurrence token alone grants no connection authority",
    );
}

#[test]
fn a_leaked_frontier_token_cannot_steal_the_stream_over_a_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // Victim opens a connection, binds a subscription to its anonymous stream-session,
    // and mutates — exactly as the secure browser client does.
    let (_, _, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    let connection = connection_of(&body);
    let mut socket = SseSocket::open(addr);
    let stream = socket.session().expect("the anonymous stream announces a session");
    let headers: &[(&str, &str)] =
        &[("Liasse-Connection", connection.as_str()), ("Liasse-Stream", stream.as_str())];
    http_request(addr, "POST", "/", headers, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    http_request(addr, "POST", "/", headers, r#"{"type":"call","address":"public.tasks.add","args":{"title":"private"}}"#);

    // The SSE `id:` frontier token is the one token that still rides an observable
    // channel (history/logs/Referer of the stream URL, or a proxy). Take one as leaked.
    socket.pump();
    let leaked_ft = socket
        .wire_events()
        .iter()
        .find_map(|event| event.id.clone())
        .expect("a downstream frame stamps a frontier id");

    // The frontier token does NOT contain the connection credential — the C/P split
    // holds on the real wire, so no splitting scheme reconstructs `C`.
    assert!(
        !leaked_ft.contains(&connection),
        "the leaked frontier token must not carry the connection credential: {leaked_ft}",
    );
    let extracted = embedded_id_of(&leaked_ft);
    assert_ne!(extracted.as_str(), connection, "the leaked frontier token yields only the public id");

    // Presenting the extracted value as the connection opens NOTHING — it is not a
    // registered credential, so a POST as it is an unknown connection.
    let stolen: &[(&str, &str)] = &[("Liasse-Connection", extracted.as_str())];
    let (status, _, body) =
        http_request(addr, "POST", "/", stolen, r#"{"type":"call","address":"public.tasks.add","args":{"title":"forged"}}"#);
    assert_eq!(status, 404, "the forged connection cannot POST: {status}");
    assert!(!body.contains("committed"), "the attacker cannot mutate the victim's data: {body}");

    // And it cannot bind the victim's still-open stream-session either: binding requires
    // a registered `C`, and the extracted public id is not one.
    let stolen_bind: &[(&str, &str)] =
        &[("Liasse-Connection", extracted.as_str()), ("Liasse-Stream", stream.as_str())];
    let (status, _, _) =
        http_request(addr, "POST", "/", stolen_bind, r#"{"type":"view","sub":"steal","address":"public.tasks"}"#);
    assert_eq!(status, 404, "the extracted value cannot bind the victim's socket");

    // The victim's stream is untouched: it still holds only its own authorized row.
    socket.pump();
    let applied = socket.replay();
    assert_eq!(applied.titles("w1"), ["private"], "the victim's stream is unaffected by the theft attempts");
}

fn connection_of(body: &str) -> String {
    let hello: Json = liasse_wire::serde_json::from_str(body).expect("hello body");
    hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned()
}
