#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! One real-socket smoke over the reference std-http binding: bind a loopback port,
//! open a connection, subscribe, read the SSE init, mutate, then resume from
//! `Last-Event-ID` and read the §12.2 patch over the wire — end to end.

mod support;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use liasse_wire::serde_json::Value as Json;
use liasse_wire::{Downstream, SseEvent, WireStore};

use support::app;

/// A raw HTTP/1.1 request over a fresh loopback connection, returning the response
/// status, headers, and body (the server closes after each response).
fn request(
    addr: std::net::SocketAddr,
    method: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, BTreeMap<String, String>, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut request = format!("{method} / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).expect("write");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    let text = String::from_utf8(raw).expect("utf8 response");
    let (head, body) = text.split_once("\r\n\r\n").expect("response has a body separator");
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let mut response_headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            response_headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    (status, response_headers, body.to_owned())
}

#[test]
fn subscribe_mutate_and_resume_over_a_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // Open a connection.
    let (status, _, body) = request(addr, "POST", &[], r#"{"type":"hello"}"#);
    assert_eq!(status, 200);
    let hello: Json = liasse_wire::serde_json::from_str(&body).expect("hello body");
    let connection = hello.get("connection").and_then(Json::as_str).expect("connection token").to_owned();
    let conn_header: &[(&str, &str)] = &[("Liasse-Connection", &connection)];

    // Subscribe.
    let (status, _, _) = request(addr, "POST", conn_header, r#"{"type":"view","sub":"w1","address":"public.tasks"}"#);
    assert_eq!(status, 200);

    // Read the SSE init and retain its frontier id.
    let (status, headers, sse) = request(addr, "GET", conn_header, "");
    assert_eq!(status, 200);
    assert!(headers.get("content-type").is_some_and(|value| value.contains("text/event-stream")));
    let init = SseEvent::parse_stream(&sse);
    let last_id = init.last().and_then(|event| event.id.clone()).expect("init carries a frontier id");
    let mut store = WireStore::new();
    feed(&mut store, &init);
    assert!(store.rows().is_empty(), "the initial view is empty");

    // Mutate.
    let (status, _, body) = request(addr, "POST", conn_header, r#"{"type":"call","address":"public.tasks.add","args":{"title":"socket"}}"#);
    assert_eq!(status, 200);
    assert!(body.contains("committed"), "the add committed: {body}");

    // Resume from the retained frontier and read the §12.2 patch.
    let resume_headers: &[(&str, &str)] = &[("Liasse-Connection", &connection), ("Last-Event-ID", &last_id)];
    let (status, _, sse) = request(addr, "GET", resume_headers, "");
    assert_eq!(status, 200);
    feed(&mut store, &SseEvent::parse_stream(&sse));

    let titles: Vec<String> = store
        .rows()
        .iter()
        .filter_map(|row| row.value().get("title").and_then(Json::as_str).map(str::to_owned))
        .collect();
    assert_eq!(titles, ["socket"], "the resumed patch delivered the added task over the socket");
}

/// Fold decoded SSE events into a single-subscription store.
fn feed(store: &mut WireStore, events: &[SseEvent]) {
    for event in events {
        let ft = liasse_wire::Ft::new(event.id.clone().unwrap_or_default());
        let Ok(frame) = liasse_wire::decode::<Downstream>(&event.data) else { continue };
        match frame {
            Downstream::Init { rows, .. } => {
                store.init(rows, ft).expect("init");
            }
            Downstream::Patch { ops, .. } => {
                store.patch(&ops, ft).expect("patch");
            }
            _ => {}
        }
    }
}
