#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Real-socket proof that the reference binding pairs with the secure `@liasse/connect`
//! browser client: the SSE stream is bound to its connection by an HttpOnly cookie the
//! `hello` response sets — NOT a URL token, NOT a header a native `EventSource` cannot
//! send — while the `Liasse-Connection` header stays a fallback for a header-capable /
//! injected transport (connect S2, §12.2).
//!
//! The connection capability never rides the URL (a capability there leaks via history,
//! access logs, and `Referer`, and would let a stream be stolen); only the non-secret
//! resume cursor may, and the resume cursor is honoured from either the `Last-Event-ID`
//! header (browser auto-reconnect) or the `last-event-id` query (manual rebuild).

mod support;

use std::net::TcpListener;

use liasse_wire::serde_json::Value as Json;
use liasse_wire::{Downstream, Ft, SseEvent, WireStore};

use support::{app, http_request};

/// The cookie name the reference binding uses to bind the SSE stream (the wire contract
/// the browser client depends on).
const COOKIE: &str = "liasse_connection";

#[test]
fn hello_sets_a_secure_httponly_cookie_and_the_stream_binds_by_it() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // (1) `hello` sets the connection as an HttpOnly, Secure, SameSite cookie.
    let (status, headers, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    assert_eq!(status, 200);
    let connection = connection_of(&body);
    let set_cookie = headers.get("set-cookie").expect("hello sets a connection cookie");
    let (name, value) = set_cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .expect("Set-Cookie carries a name=value");
    assert_eq!(name, COOKIE, "the cookie is the connection cookie");
    assert_eq!(value, connection, "the cookie carries the connection capability");
    let lower = set_cookie.to_ascii_lowercase();
    assert!(lower.contains("httponly"), "the cookie is HttpOnly (untrusted JS cannot read it): {set_cookie}");
    assert!(lower.contains("secure"), "the cookie is Secure (never crosses plaintext): {set_cookie}");
    assert!(lower.contains("samesite"), "the cookie is SameSite (resists CSRF/theft): {set_cookie}");

    // The browser resends this ambient cookie; nothing else identifies the connection.
    let cookie = format!("{COOKIE}={connection}");
    let cookie_only: &[(&str, &str)] = &[("Cookie", cookie.as_str())];

    // Subscribe over the cookie alone — a browser POST carries the cookie, no header.
    let (status, _, _) = http_request(addr, "POST", "/", cookie_only, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);

    // (2) The SSE GET sends ONLY the cookie: no `liasse-connection` header, no URL token.
    let (status, sse_headers, sse) = http_request(addr, "GET", "/", cookie_only, "");
    assert_eq!(status, 200);
    assert!(sse_headers.get("content-type").is_some_and(|v| v.contains("text/event-stream")));
    let init = SseEvent::parse_stream(&sse);
    assert!(kinds(&init).contains(&"init"), "the cookie-bound stream opens with an init");
    let last_id = init.last().and_then(|event| event.id.clone()).expect("init carries a frontier id");

    // Mutate over the cookie POST.
    let (status, _, body) = http_request(addr, "POST", "/", cookie_only, r#"{"type":"call","address":"public.tasks.add","args":{"title":"cookie"}}"#);
    assert_eq!(status, 200);
    assert!(body.contains("committed"), "the add committed: {body}");

    // (3a) Resume via the `Last-Event-ID` header (a browser's automatic auto-reconnect).
    let mut via_header = WireStore::new();
    feed(&mut via_header, &init);
    let header_resume: &[(&str, &str)] = &[("Cookie", cookie.as_str()), ("Last-Event-ID", last_id.as_str())];
    let (status, _, sse) = http_request(addr, "GET", "/", header_resume, "");
    assert_eq!(status, 200);
    feed(&mut via_header, &SseEvent::parse_stream(&sse));
    assert_eq!(titles(&via_header), ["cookie"], "the header resume replayed the §12.2 patch");

    // (3b) Resume via the `last-event-id` URL query (the client's manual rebuild). The
    // frontier token is not a credential, so it may ride the URL; the connection still
    // comes only from the cookie.
    let mut via_query = WireStore::new();
    feed(&mut via_query, &init);
    let query_target = format!("/?last-event-id={last_id}");
    let (status, _, sse) = http_request(addr, "GET", &query_target, cookie_only, "");
    assert_eq!(status, 200);
    feed(&mut via_query, &SseEvent::parse_stream(&sse));
    assert_eq!(titles(&via_query), ["cookie"], "the query resume replayed the same §12.2 patch");
}

#[test]
fn an_injected_header_transport_still_binds_the_stream_without_a_cookie() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    let (_, _, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    let connection = connection_of(&body);
    let header: &[(&str, &str)] = &[("Liasse-Connection", connection.as_str())];

    let (status, _, _) = http_request(addr, "POST", "/", header, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);

    // A header-capable transport (no cookie, no URL token) still opens the stream.
    let (status, sse_headers, sse) = http_request(addr, "GET", "/", header, "");
    assert_eq!(status, 200);
    assert!(sse_headers.get("content-type").is_some_and(|v| v.contains("text/event-stream")));
    assert!(kinds(&SseEvent::parse_stream(&sse)).contains(&"init"), "the header fallback opens the stream");
}

#[test]
fn the_connection_header_takes_precedence_over_the_cookie() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    let (_, _, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    let connection = connection_of(&body);
    let cookie = format!("{COOKIE}={connection}");
    let header: &[(&str, &str)] = &[("Liasse-Connection", connection.as_str())];
    let (status, _, _) = http_request(addr, "POST", "/", header, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);

    // A valid header with a bogus cookie opens the stream: the header is used.
    let valid_header: &[(&str, &str)] = &[("Liasse-Connection", connection.as_str()), ("Cookie", "liasse_connection=deadbeef")];
    let (status, _, sse) = http_request(addr, "GET", "/", valid_header, "");
    assert_eq!(status, 200);
    assert!(kinds(&SseEvent::parse_stream(&sse)).contains(&"init"), "the explicit header opened the live stream");

    // A bogus header with the VALID cookie yields a reset for the unknown header token —
    // never the valid connection's init. The header wins, so the cookie is ignored.
    let bogus_header: &[(&str, &str)] = &[("Liasse-Connection", "deadbeef"), ("Cookie", cookie.as_str())];
    let (status, _, sse) = http_request(addr, "GET", "/", bogus_header, "");
    assert_eq!(status, 200);
    let ks = kinds(&SseEvent::parse_stream(&sse));
    assert!(ks.contains(&"reset"), "the bogus header is used, not the valid cookie: {ks:?}");
    assert!(!ks.contains(&"init"), "the valid cookie was ignored — the header takes precedence: {ks:?}");
}

/// The connection capability the `hello` body reported.
fn connection_of(body: &str) -> String {
    let hello: Json = liasse_wire::serde_json::from_str(body).expect("hello body");
    hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned()
}

/// Fold decoded SSE `init`/`patch` events into a single-subscription store.
fn feed(store: &mut WireStore, events: &[SseEvent]) {
    for event in events {
        let ft = Ft::new(event.id.clone().unwrap_or_default());
        let Ok(frame) = liasse_wire::decode::<Downstream>(&event.data) else { continue };
        match frame {
            Downstream::Init { rows, .. } => store.init(rows, ft).expect("init"),
            Downstream::Patch { ops, .. } => store.patch(&ops, ft).expect("patch"),
            _ => {}
        }
    }
}

/// The `event:` kind of each decodable downstream frame in a stream.
fn kinds(events: &[SseEvent]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| liasse_wire::decode::<Downstream>(&event.data).ok())
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

/// The `title` field of each row a store holds, in order.
fn titles(store: &WireStore) -> Vec<String> {
    store
        .rows()
        .iter()
        .filter_map(|row| row.value().get("title").and_then(Json::as_str).map(str::to_owned))
        .collect()
}
