#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM — S5 attack battery item 2 (malformed / oversized / adversarial frames).
//! Hostile request bodies and values at the connect boundary must yield a typed
//! fault or a handled spec outcome, never a panic and never state corruption
//! (AGENTS.md no-panic). This is a ROBUST sign-off: every case below is confined.
//!
//! Coverage:
//!   * Deep nesting is capped by the codec (serde recursion limit) before it can
//!     reach the recursive value decoder — proven at the boundary.
//!   * Max-wire-depth, wrong-typed, huge, and unknown arguments/params → Rejected.
//!   * Adversarial addresses (empty, NUL, unicode, oversized) → Denied `unresolved`.
//!   * Extreme window sizes → no OOM.
//!   * Frames requiring a connection with none → typed `NoConnection`.
//!   * The reference HTTP reader: oversized body → 413, non-UTF-8 body → 400,
//!     unknown tag → 400, malformed request line → 400.

mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use liasse_wire::serde_json::{Value as Json, json};
use liasse_wire::Upstream;

use liasse_connect::{ConnectError, Reply};
use support::{app, hello};

/// An `n`-deep nested JSON array value, built without recursion in the test.
fn nested_value(n: usize) -> Json {
    let mut v = json!(0);
    for _ in 0..n {
        v = Json::Array(vec![v]);
    }
    v
}

#[test]
fn deep_nesting_is_rejected_by_the_codec_before_it_reaches_the_decoder() {
    // The realistic attack arrives as bytes: the codec (serde_json) caps nesting, so a
    // pathologically deep body is a decode error, never a stack overflow. It never
    // reaches `core.submit`.
    let nest = format!("{}0{}", "[".repeat(20_000), "]".repeat(20_000));
    let deep = format!(r#"{{"type":"call","address":"public.tasks.add","args":{{"title":{nest}}}}}"#);
    assert!(liasse_wire::decode::<Upstream>(&deep).is_err(), "deep nesting is a codec error, not a panic");
}

#[test]
fn max_wire_depth_arguments_are_rejected_without_panicking() {
    let mut core = app();
    let conn = hello(&mut core);

    // A value nested as deeply as the wire codec allows (well within serde's limit),
    // handed to the recursive type decoder: `title: text` cannot decode an array, so
    // it is Rejected — total decoding, no stack overflow.
    let frame = Upstream::Call {
        address: "public.tasks.add".to_owned(),
        args: json!({ "title": nested_value(100) }),
        auth: None,
        context: None,
    };
    match core.submit(Some(&conn), None, frame).expect("hostile args are handled") {
        Reply::Outcome(liasse_wire::Outcome::Rejected { .. }) => {}
        other => panic!("deep args should be rejected, got {other:?}"),
    }
}

#[test]
fn wrong_typed_huge_and_unknown_arguments_are_all_confined() {
    let mut core = app();
    let conn = hello(&mut core);

    let cases: Vec<(&str, Json)> = vec![
        ("number-for-text", json!({ "title": 123 })),
        ("bool-for-text", json!({ "title": true })),
        ("array-for-text", json!({ "title": [1, 2, 3] })),
        ("object-for-text", json!({ "title": { "x": 1 } })),
        ("huge-number", json!({ "title": 1e308 })),
        ("unknown-argument", json!({ "title": "ok", "smuggled": 1 })),
        ("args-not-an-object", json!([1, 2, 3])),
    ];
    for (name, args) in cases {
        let frame = Upstream::Call { address: "public.tasks.add".to_owned(), args, auth: None, context: None };
        let reply = core.submit(Some(&conn), None, frame).unwrap_or_else(|e| panic!("{name}: hostile args faulted: {e:?}"));
        match reply {
            // Mistyped/unknown/ill-shaped args are all Rejected (mirroring the runtime's
            // Malformed), never admitted and never a panic.
            Reply::Outcome(liasse_wire::Outcome::Rejected { .. }) => {}
            other => panic!("{name}: expected Rejected, got {other:?}"),
        }
    }

    // A huge string is a VALID text and commits — the point is decoding is correct, not
    // that everything is refused.
    let big = "x".repeat(200_000);
    let frame = Upstream::Call { address: "public.tasks.add".to_owned(), args: json!({ "title": big }), auth: None, context: None };
    assert!(matches!(core.submit(Some(&conn), None, frame), Ok(Reply::Outcome(liasse_wire::Outcome::Committed { .. }))));
}

#[test]
fn params_for_a_parameter_free_view_are_rejected() {
    let mut core = app();
    let conn = hello(&mut core);
    // `public.tasks` declares no params; any supplied member is unknown → rejected.
    let frame = Upstream::View {
        sub: liasse_wire::Sub::new("w"),
        address: "public.tasks".to_owned(),
        params: Some(json!({ "smuggled": nested_value(80) })),
        window: None,
        auth: None,
        context: None,
    };
    match core.submit(Some(&conn), None, frame).expect("hostile params handled") {
        Reply::Outcome(liasse_wire::Outcome::Rejected { .. }) => {}
        other => panic!("unknown params should be rejected, got {other:?}"),
    }
}

#[test]
fn adversarial_addresses_resolve_to_a_denial_never_panic() {
    let mut core = app();
    let conn = hello(&mut core);
    let addresses = ["", " ", "\0", "public..tasks", "a.b.c.d.e.f", "público.tásks", &"x.".repeat(50_000)];
    for address in addresses {
        let frame = Upstream::Call { address: address.to_owned(), args: json!({}), auth: None, context: None };
        match core.submit(Some(&conn), None, frame).unwrap_or_else(|e| panic!("address {address:?} faulted: {e:?}")) {
            // A name that does not parse, or names nothing exposed, is an
            // indistinguishable `unresolved` denial (§10.1), never a panic.
            Reply::Outcome(liasse_wire::Outcome::Denied { .. } | liasse_wire::Outcome::Rejected { .. }) => {}
            other => panic!("address {address:?} should be denied/rejected, got {other:?}"),
        }
    }
}

#[test]
fn extreme_window_sizes_do_not_exhaust_memory() {
    let mut core = app();
    let conn = hello(&mut core);
    for title in ["a", "b", "c"] {
        let frame = Upstream::Call { address: "public.tasks.add".to_owned(), args: json!({ "title": title }), auth: None, context: None };
        let _ = core.submit(Some(&conn), None, frame);
    }
    for size in [0usize, 1, usize::MAX] {
        let frame = support::view_request("win", "public.tasks", Some(liasse_wire::WireWindow {
            size,
            anchor: liasse_wire::WireAnchor::First,
            slide: false,
        }));
        // A window of `usize::MAX` must slice the view, not pre-allocate it — no OOM.
        let reply = core.submit(Some(&conn), None, frame).unwrap_or_else(|e| panic!("window size {size} faulted: {e:?}"));
        assert!(matches!(reply, Reply::Opened { .. }), "window size {size} opens: {reply:?}");
    }
}

#[test]
fn every_frame_requiring_a_connection_faults_typed_when_none_is_presented() {
    let mut core = app();
    let frames = [
        Upstream::Manifest,
        support::view_request("w", "public.tasks", None),
        Upstream::Unsubscribe { sub: liasse_wire::Sub::new("w") },
        Upstream::Call { address: "public.tasks.add".to_owned(), args: json!({ "title": "t" }), auth: None, context: None },
        Upstream::Fetch { address: "public.tasks".to_owned(), params: None },
        Upstream::Operation { operation: liasse_wire::OperationId::new("op") },
    ];
    for frame in frames {
        let error = core.submit(None, None, frame).expect_err("a connection-bound frame with no connection is a fault");
        assert!(matches!(error, ConnectError::NoConnection), "expected NoConnection, got {error:?}");
    }
}

// --- reference HTTP reader robustness (real socket) ---------------------------------

/// Send raw bytes to the binding and return the response status and body.
fn raw(addr: SocketAddr, bytes: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(bytes).expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .expect("status");
    (status, body.to_owned())
}

#[test]
fn the_http_reader_confines_oversized_non_utf8_and_malformed_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let server = liasse_connect::bind::serve(listener, app).expect("serve");
    let addr = server.local_addr();

    // Oversized: a Content-Length past the 1 MiB bound is rejected before the body is
    // read (no unbounded allocation), as a 413 fault.
    let oversized = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1048577\r\n\r\n";
    let (status, body) = raw(addr, oversized);
    assert_eq!(status, 413, "oversized body is a 413");
    assert!(body.contains("oversized") || body.contains("fault") || body.contains("size"), "413 carries a fault frame: {body}");

    // Non-UTF-8 body: raw bytes that are not text are a 400, not a panic.
    let mut non_utf8 = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\n".to_vec();
    non_utf8.extend_from_slice(&[0xff, 0xfe, 0xfd]);
    let (status, _) = raw(addr, &non_utf8);
    assert_eq!(status, 400, "non-UTF-8 body is a 400");

    // A body that is valid UTF-8 but an unknown frame tag is a 400 malformed fault.
    let unknown_tag = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 16\r\n\r\n{\"type\":\"nope\"}\n";
    let (status, body) = raw(addr, unknown_tag);
    assert_eq!(status, 400, "unknown tag is a 400");
    assert!(body.contains("malformed") || body.contains("parse"), "carries a malformed fault: {body}");

    // A malformed request line (no target) is a 400, never a panic.
    let (status, _) = raw(addr, b"GARBAGE\r\n\r\n");
    assert_eq!(status, 400, "a malformed request line is a 400");
}
