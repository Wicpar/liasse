#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 attack battery item 3 (resume/cookie transport security) and
//! invariant #5 (capability confinement). REGRESSION GUARD for the fixed CRITICAL.
//!
//! WAS (CRITICAL): the connection was a single nonce that was simultaneously the
//! bearer credential AND the plaintext body of every frontier/occurrence token
//! (`Nonce::connection_token` returned the nonce; `Nonce::frontier`/`occurrence`
//! embedded it). Because a frontier token legitimately rides the SSE `id:` and the
//! `?last-event-id=` resume URL — through browser history, access logs, and the
//! `Referer` header — an attacker who saw ONE such URL split the nonce out and
//! presented it as `Liasse-Connection`, stealing the victim's authorized stream and
//! POSTing as them. The HttpOnly cookie defence was illusory.
//!
//! NOW (FIXED): a connection carries TWO independent values (crates/liasse-connect/
//! src/token.rs `ConnKeys`): a SECRET credential `C` (the registry key, the cookie /
//! `Liasse-Connection` value) and a non-secret PUBLIC id `P`. Only `P` is embedded in
//! ft/occ; `C` never appears in any token. So a leaked frontier/occurrence token — or
//! a resume URL that embeds one — reveals only `P`, and `P` opens nothing: reaching a
//! connection at all requires presenting `C` at the registry, and `P` is not `C`.
//!
//! Security property GUARDED: a frontier/occurrence token is not a credential and,
//! alone, yields no authority over the connection (§12.2; AGENTS.md untrusted-frontend
//! / opaque-identity). These tests assert the token never embeds `C` and that whatever
//! an attacker can extract from it grants nothing.

mod support;

use std::net::TcpListener;

use liasse_wire::serde_json::{Value as Json, json};
use liasse_wire::{ConnectionToken, Downstream, SseEvent, Upstream};

use liasse_connect::ConnectError;
use support::{app, call, drain, hello, http_request, view};

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
fn a_leaked_resume_url_cannot_steal_the_stream_over_a_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // Victim opens a connection (HttpOnly cookie), subscribes, and mutates over the
    // cookie exactly as the secure browser client does.
    let (_, _, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    let connection = connection_of(&body);
    let cookie = format!("liasse_connection={connection}");
    let cookie_only: &[(&str, &str)] = &[("Cookie", cookie.as_str())];
    http_request(addr, "POST", "/", cookie_only, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    http_request(addr, "POST", "/", cookie_only, r#"{"type":"call","address":"public.tasks.add","args":{"title":"private"}}"#);

    // Victim's browser auto-reconnects the SSE stream; the frontier cursor appears in
    // the resume URL, which is logged / kept in history. That is all the attacker sees.
    let (_, _, sse) = http_request(addr, "GET", "/", cookie_only, "");
    let leaked_ft = SseEvent::parse_stream(&sse)
        .iter()
        .find_map(|e| e.id.clone())
        .expect("the stream stamps a frontier id");
    let leaked_resume_url = format!("/?last-event-id={leaked_ft}");

    // The binding refuses a resume URL that carries no connection: the frontier cursor
    // alone opens nothing (the front-door mitigation).
    let (status, _, _) = http_request(addr, "GET", &leaked_resume_url, &[], "");
    assert_eq!(status, 400, "a bare resume URL with no connection is refused at the front door");

    // And the mitigation is now real: the frontier token in that URL does NOT contain
    // the connection credential, so the attacker cannot reconstruct it.
    assert!(
        !leaked_ft.contains(&connection),
        "the leaked resume URL must not carry the connection credential: {leaked_ft}",
    );
    let extracted = embedded_id_of(&leaked_ft);
    assert_ne!(extracted.as_str(), connection, "the leaked resume URL yields only the public id");

    // Presenting the extracted value as the connection opens NOTHING: the GET returns
    // no authorized rows, only an unknown-connection reset.
    let stolen_header: &[(&str, &str)] = &[("Liasse-Connection", extracted.as_str())];
    let (_status, _headers, sse) = http_request(addr, "GET", "/", stolen_header, "");
    let kinds = kinds(&SseEvent::parse_stream(&sse));
    assert!(!kinds.contains(&"init"), "the forged connection receives no authorized rows: {kinds:?}");
    assert!(kinds.contains(&"reset"), "an unknown connection is only told to re-establish: {kinds:?}");

    // And it cannot write as the victim.
    let (status, _, body) = http_request(addr, "POST", "/", stolen_header, r#"{"type":"call","address":"public.tasks.add","args":{"title":"forged"}}"#);
    assert_ne!(status, 200, "the forged connection cannot POST: {status}");
    assert!(!body.contains("committed"), "the attacker cannot mutate the victim's data: {body}");
}

fn connection_of(body: &str) -> String {
    let hello: Json = liasse_wire::serde_json::from_str(body).expect("hello body");
    hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned()
}

fn kinds(events: &[SseEvent]) -> Vec<&'static str> {
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
