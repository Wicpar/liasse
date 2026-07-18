#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! One real-socket smoke over the reference std-http binding, end to end on the
//! ephemeral-stream-session transport: open a connection, open the anonymous SSE stream
//! and read its `liasse-session` announcement, bind a subscription to that session with
//! an authenticated `view` POST, read the §12.2 init on the OPEN socket, mutate, and read
//! the reconciled §12.2 patch over the same socket — no cookie, no URL token anywhere.

mod support;

use std::net::TcpListener;

use liasse_wire::serde_json::Value as Json;

use support::{app, http_request, SseSocket};

/// The connection capability the `hello` body reported.
fn connection_of(body: &str) -> String {
    let hello: Json = liasse_wire::serde_json::from_str(body).expect("hello body");
    hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned()
}

#[test]
fn subscribe_mutate_and_read_over_the_ephemeral_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // Open a connection (POST, no cookie is ever set).
    let (status, headers, body) = http_request(addr, "POST", "/", &[], r#"{"type":"hello"}"#);
    assert_eq!(status, 200);
    assert!(!headers.contains_key("set-cookie"), "the ephemeral transport sets no cookie");
    let connection = connection_of(&body);

    // Open the anonymous SSE stream. It announces a fresh ephemeral session on its first
    // event and carries NO wire data — data flows only after an authenticated bind.
    let mut socket = SseSocket::open(addr);
    let stream = socket.session().expect("the stream announces its ephemeral session");
    assert!(socket.wire_events().is_empty(), "the anonymous stream yields no data before a bind");

    // Bind a subscription to that session with an authenticated `view` (C + stream).
    let bind_headers: &[(&str, &str)] =
        &[("Liasse-Connection", connection.as_str()), ("Liasse-Stream", stream.as_str())];
    let (status, _, _) =
        http_request(addr, "POST", "/", bind_headers, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);

    // The §12.2 init flows on the open socket; the view starts empty.
    socket.pump();
    assert!(!socket.wire_events().is_empty(), "the bound stream delivered the init");
    assert!(socket.replay().rows("w1").is_empty(), "the initial view is empty");

    // Mutate over an authenticated POST.
    let (status, _, body) = http_request(
        addr,
        "POST",
        "/",
        bind_headers,
        r#"{"type":"call","address":"public.tasks.add","args":{"title":"socket"}}"#,
    );
    assert_eq!(status, 200);
    assert!(body.contains("committed"), "the add committed: {body}");

    // The reconciled §12.2 patch arrives on the same open socket.
    socket.pump();
    assert_eq!(
        socket.replay().titles("w1"),
        ["socket"],
        "the reconciled patch delivered the added task over the bound socket",
    );
}
