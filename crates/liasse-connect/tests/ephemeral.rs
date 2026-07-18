#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Real-socket proof of the ephemeral-stream-session transport that pairs with the
//! secure `@liasse/connect` browser client (§12.2). The SSE stream is opened
//! ANONYMOUSLY; the server mints a fresh single-socket stream-session and announces it
//! on the socket's first event; a subscription's frames flow only after an authenticated
//! `view` POST binds that session to the connection. There is no cookie and no URL token
//! anywhere, so nothing presentable grants access to a stream.
//!
//! Proven here, all over real loopback sockets:
//! 1. an anonymous GET receives a `liasse-session` first event and NO wire data;
//! 2. one SSE multiplexes all of a connection's subscriptions (demuxed by `sub`);
//! 3. a WINDOWED subscribe delivers only its window;
//! 4. `unsubscribe` stops that sub's frames while other subs keep flowing (data-min);
//! 5. reconnect → a new session → re-bind → the §12.2 init re-establishes the rows;
//! 6. theft resistance: a second connection presenting the victim's stream id with a
//!    different/absent connection cannot bind or receive the victim's frames, and opening
//!    the anonymous URL alone yields no data.

mod support;

use std::net::{SocketAddr, TcpListener};

use liasse_wire::serde_json::Value as Json;

use support::{app, http_request, SseSocket};

/// Start a fresh reference server on a loopback port.
fn server() -> (SocketAddr, liasse_connect::bind::Server) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();
    (addr, server)
}

/// Open a connection and return its capability `C`.
fn hello(addr: SocketAddr) -> String {
    let (status, _, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    assert_eq!(status, 200);
    let hello: Json = liasse_wire::serde_json::from_str(&body).expect("hello body");
    hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned()
}

/// The `Liasse-Connection` + `Liasse-Stream` bind headers for a POST.
fn bind_headers<'a>(connection: &'a str, stream: &'a str) -> [(&'a str, &'a str); 2] {
    [("Liasse-Connection", connection), ("Liasse-Stream", stream)]
}

#[test]
fn an_anonymous_stream_announces_a_session_and_carries_no_data() {
    let (addr, _server) = server();

    let first = SseSocket::open(addr);
    let second = SseSocket::open(addr);
    let a = first.session().expect("the first anonymous stream announces a session");
    let b = second.session().expect("the second anonymous stream announces a session");

    // (1) Each anonymous socket gets its OWN fresh ephemeral session, and neither carries
    // any wire data — opening the URL yields only an empty unbound session.
    assert_ne!(a, b, "each anonymous connect mints a distinct ephemeral session");
    assert!(first.wire_events().is_empty(), "an unbound stream carries no data");
    assert!(second.wire_events().is_empty(), "an unbound stream carries no data");
}

#[test]
fn a_windowed_bind_delivers_only_its_window() {
    let (addr, _server) = server();
    let connection = hello(addr);
    let mut socket = SseSocket::open(addr);
    let stream = socket.session().expect("session");
    let headers = bind_headers(&connection, &stream);

    // Seed three tasks (calls need only the connection; no subscription is open yet).
    for title in ["a", "b", "c"] {
        let body = format!(r#"{{"type":"call","address":"public.tasks.add","args":{{"title":"{title}"}}}}"#);
        let (status, _, reply) = http_request(addr, "POST", "/", &headers, &body);
        assert_eq!(status, 200);
        assert!(reply.contains("committed"), "seed add committed: {reply}");
    }

    // (3) A windowed view (size 1, anchored at the first row in `title` order) delivers
    // exactly ONE row — its window — not the whole three-row view.
    let (status, _, _) = http_request(
        addr,
        "POST",
        "/",
        &headers,
        r#"{"type":"view","sub":"win","address":"public.tasks","window":{"size":1}}"#,
    );
    assert_eq!(status, 200);
    socket.pump();
    assert_eq!(socket.replay().titles("win"), ["a"], "the window is exactly its one row, not [a, b, c]");
}

#[test]
fn one_socket_multiplexes_all_subs_and_unsubscribe_minimizes_data() {
    let (addr, _server) = server();
    let connection = hello(addr);
    let mut socket = SseSocket::open(addr);
    let stream = socket.session().expect("session");
    let headers = bind_headers(&connection, &stream);

    // (2) Two subscriptions ride the ONE socket, demuxed by `sub`.
    for sub in ["w1", "w2"] {
        let body = format!(r#"{{"type":"view","sub":"{sub}","address":"public.tasks"}}"#);
        let (status, _, _) = http_request(addr, "POST", "/", &headers, &body);
        assert_eq!(status, 200);
    }
    // A commit reconciles BOTH subs on the single stream.
    http_request(addr, "POST", "/", &headers, r#"{"type":"call","address":"public.tasks.add","args":{"title":"x"}}"#);
    socket.pump();
    let applied = socket.replay();
    assert_eq!(applied.titles("w1"), ["x"], "w1 sees the add on the shared socket");
    assert_eq!(applied.titles("w2"), ["x"], "w2 sees the add on the shared socket");

    // (4) Unsubscribe w1; a later commit then patches ONLY w2 — w1's frames stop.
    http_request(addr, "POST", "/", &headers, r#"{"type":"unsubscribe","sub":"w1"}"#);
    http_request(addr, "POST", "/", &headers, r#"{"type":"call","address":"public.tasks.add","args":{"title":"y"}}"#);
    socket.pump();
    let applied = socket.replay();
    assert!(applied.closed("w1"), "w1 was closed by the unsubscribe");
    assert!(!applied.titles("w1").contains(&"y".to_owned()), "w1 receives no frames after unsubscribe (data-min)");
    assert!(applied.titles("w2").contains(&"y".to_owned()), "w2 keeps flowing on the shared socket");
}

#[test]
fn a_reconnect_gets_a_new_session_rebinds_and_reestablishes_rows() {
    let (addr, _server) = server();
    let connection = hello(addr);

    // First socket: bind, init, mutate, patch.
    let mut first = SseSocket::open(addr);
    let stream_one = first.session().expect("session one");
    let headers_one = bind_headers(&connection, &stream_one);
    http_request(addr, "POST", "/", &headers_one, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    http_request(addr, "POST", "/", &headers_one, r#"{"type":"call","address":"public.tasks.add","args":{"title":"a"}}"#);
    first.pump();
    assert_eq!(first.replay().titles("w1"), ["a"], "the first socket carried the row");

    // (5) Reconnect: a NEW socket announces a DISTINCT session; re-binding the same
    // subscription to it delivers a fresh init that re-establishes the current rows.
    let mut second = SseSocket::open(addr);
    let stream_two = second.session().expect("session two");
    assert_ne!(stream_one, stream_two, "each (re)connect is a distinct ephemeral session");
    let headers_two = bind_headers(&connection, &stream_two);
    let (status, _, _) =
        http_request(addr, "POST", "/", &headers_two, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);
    second.pump();
    assert_eq!(second.replay().titles("w1"), ["a"], "the re-bind re-established the rows on the new socket");
}

#[test]
fn a_stolen_session_or_connection_cannot_attach_to_a_victims_socket() {
    let (addr, _server) = server();

    // Victim binds a subscription and receives a private row on its own socket.
    let victim = hello(addr);
    let mut victim_socket = SseSocket::open(addr);
    let victim_stream = victim_socket.session().expect("victim session");
    let victim_headers = bind_headers(&victim, &victim_stream);
    http_request(addr, "POST", "/", &victim_headers, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    http_request(addr, "POST", "/", &victim_headers, r#"{"type":"call","address":"public.tasks.add","args":{"title":"secret"}}"#);
    victim_socket.pump();
    assert_eq!(victim_socket.replay().titles("w1"), ["secret"], "the victim received its own row");

    // Attacker has its OWN connection and somehow learned the victim's stream id.
    let attacker = hello(addr);

    // (6a) Binding the victim's session with a DIFFERENT connection is rejected (theft).
    let stolen = bind_headers(&attacker, &victim_stream);
    let (status, _, body) =
        http_request(addr, "POST", "/", &stolen, r#"{"type":"view","sub":"steal","address":"public.tasks"}"#);
    assert_eq!(status, 403, "a different connection cannot bind the victim's session");
    assert!(body.contains("fault") || body.contains("bad-token"), "the rejection is a fault: {body}");

    // (6b) Presenting the victim's session with NO connection is rejected too.
    let anon_stream: &[(&str, &str)] = &[("Liasse-Stream", victim_stream.as_str())];
    let (status, _, _) =
        http_request(addr, "POST", "/", anon_stream, r#"{"type":"view","sub":"steal","address":"public.tasks"}"#);
    assert_eq!(status, 404, "an unauthenticated bind of the victim's session is refused");

    // (6c) The attacker's own anonymous socket carries no data (never bound to anything).
    let attacker_socket = SseSocket::open(addr);
    let _ = attacker; // the attacker's connection never reached the victim's socket
    assert!(attacker_socket.wire_events().is_empty(), "the attacker's socket receives no victim frames");

    // The victim's stream was untouched by the attempts.
    victim_socket.pump();
    assert_eq!(victim_socket.replay().titles("w1"), ["secret"], "the victim's stream is unaffected by the theft attempts");
}
