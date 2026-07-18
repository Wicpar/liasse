#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! The pure client core, exercised natively (the wasm-bindgen boundary is a thin
//! marshaling layer over exactly this logic). The core is the §12.2 replica the
//! untrusted web client keeps: it folds downstream frames — produced here through the
//! shared `liasse-wire` schema, so the assertions run against the real wire form — and
//! serializes the upstream requests the shell POSTs.
//!
//! Each expected state is deduced from §12.2 by hand (init sets the rows; a patch
//! advances them in current-result index order; a frontier-only patch moves only the
//! frontier; close/reset terminate), never from the core's own output.

use liasse_connect_wasm::{AppliedKind, ClientReplica, CoreError, request};
use liasse_wire::serde_json::json;
use liasse_wire::{
    CloseReason, Downstream, Occ, OperationId, PatchOp, ResetReason, Sub, Upstream, WireAnchor,
    WireRow, WireWindow, decode, encode,
};

/// The wire JSON of a downstream frame, exactly as the server would send it.
fn wire(frame: &Downstream) -> String {
    encode(frame).expect("encode downstream")
}

fn row(id: &str, n: i64) -> WireRow {
    WireRow::new(Occ::new(id), json!(n))
}

fn ids(rows: &[WireRow]) -> Vec<String> {
    rows.iter().map(|r| r.occ().as_str().to_owned()).collect()
}

fn ft_string(ft: Option<liasse_wire::Ft>) -> Option<String> {
    ft.map(liasse_wire::Ft::into_inner)
}

#[test]
fn a_connection_replica_folds_a_row_subscription_lifecycle() {
    let mut r = ClientReplica::new();
    let s = Sub::new("tasks");

    // init at f0 establishes the row stream.
    let applied = r
        .apply(&wire(&Downstream::Init { sub: s.clone(), rows: vec![row("a", 1), row("b", 2)] }), "f0")
        .expect("init");
    assert_eq!(applied.kind, AppliedKind::Init);
    assert_eq!(applied.sub.as_deref(), Some("tasks"));
    assert_eq!(applied.frontier.as_deref(), Some("f0"));
    assert_eq!(ids(&r.rows_of(&s)), ["a", "b"]);
    assert_eq!(r.connection_frontier().map(|f| f.as_str().to_owned()), Some("f0".to_owned()));

    // patch at f1: insert c at the end.
    r.apply(
        &wire(&Downstream::Patch {
            sub: s.clone(),
            ops: vec![PatchOp::Insert { at: 2, occ: Occ::new("c"), value: json!(3) }],
        }),
        "f1",
    )
    .expect("insert patch");
    assert_eq!(ids(&r.rows_of(&s)), ["a", "b", "c"]);
    assert_eq!(ft_string(r.frontier_of(&s)), Some("f1".to_owned()));

    // patch at f2: remove a.
    r.apply(&wire(&Downstream::Patch { sub: s.clone(), ops: vec![PatchOp::Remove { occ: Occ::new("a") }] }), "f2")
        .expect("remove patch");
    assert_eq!(ids(&r.rows_of(&s)), ["b", "c"]);

    // frontier-only patch (empty ops) at f3: rows unchanged, frontier advances.
    r.apply(&wire(&Downstream::Patch { sub: s.clone(), ops: vec![] }), "f3").expect("empty patch");
    assert_eq!(ids(&r.rows_of(&s)), ["b", "c"]);
    assert_eq!(ft_string(r.frontier_of(&s)), Some("f3".to_owned()));

    // a connection-level frontier ping at f4 advances the frontier only.
    let ping = r.apply(&wire(&Downstream::Frontier), "f4").expect("frontier ping");
    assert_eq!(ping.kind, AppliedKind::Frontier);
    assert!(ping.sub.is_none(), "a frontier ping names no subscription");
    assert_eq!(ft_string(r.frontier_of(&s)), Some("f4".to_owned()));
    assert_eq!(ids(&r.rows_of(&s)), ["b", "c"]);

    // close terminates; the sub exposes no rows and refuses further frames.
    let closed = r
        .apply(&wire(&Downstream::Close { sub: s.clone(), reason: CloseReason::Unauthorized }), "f5")
        .expect("close");
    assert_eq!(closed.kind, AppliedKind::Close);
    assert_eq!(closed.close_reason, Some(CloseReason::Unauthorized));
    assert!(r.is_closed(&s));
    assert!(r.rows_of(&s).is_empty(), "a closed subscription exposes no rows");
    assert!(matches!(
        r.apply(&wire(&Downstream::Patch { sub: s.clone(), ops: vec![] }), "f6"),
        Err(CoreError::Store(_))
    ));

    // reset drops the replica; the client re-views from scratch.
    let reset = r.apply(&wire(&Downstream::Reset { reason: ResetReason::UnknownConnection }), "").expect("reset");
    assert_eq!(reset.kind, AppliedKind::Reset);
    assert_eq!(reset.reset_reason, Some(ResetReason::UnknownConnection));
    assert!(r.subs().is_empty());
    assert!(r.connection_frontier().is_none(), "a reset invalidates the retained frontier");
}

#[test]
fn move_and_update_land_the_changed_value_at_the_moved_position() {
    let mut r = ClientReplica::new();
    let s = Sub::new("s");
    r.apply(&wire(&Downstream::Init { sub: s.clone(), rows: vec![row("a", 1), row("b", 2), row("c", 3)] }), "f0")
        .expect("init");

    // update b -> 20, then move c to the front (indices read in the current result).
    r.apply(
        &wire(&Downstream::Patch {
            sub: s.clone(),
            ops: vec![
                PatchOp::Update { occ: Occ::new("b"), value: json!(20) },
                PatchOp::Move { occ: Occ::new("c"), to: 0 },
            ],
        }),
        "f1",
    )
    .expect("update+move patch");

    let rows = r.rows_of(&s);
    assert_eq!(ids(&rows), ["c", "a", "b"]);
    let b = rows.iter().find(|x| x.occ().as_str() == "b").expect("b present");
    assert_eq!(b.value(), &json!(20), "the updated value travels with the moved order");
}

#[test]
fn a_scalar_subscription_holds_its_value_and_refuses_row_patches() {
    let mut r = ClientReplica::new();
    let s = Sub::new("count");

    let applied = r.apply(&wire(&Downstream::Scalar { sub: s.clone(), value: json!(41) }), "f0").expect("scalar");
    assert_eq!(applied.kind, AppliedKind::Scalar);
    assert_eq!(applied.scalar, Some(json!(41)));
    assert_eq!(r.scalar_of(&s), Some(json!(41)));

    r.apply(&wire(&Downstream::Scalar { sub: s.clone(), value: json!(42) }), "f1").expect("scalar update");
    assert_eq!(r.scalar_of(&s), Some(json!(42)));
    assert_eq!(ft_string(r.frontier_of(&s)), Some("f1".to_owned()));

    // a row patch onto a scalar subscription is a shape mismatch.
    assert!(matches!(
        r.apply(&wire(&Downstream::Patch { sub: s.clone(), ops: vec![] }), "f2"),
        Err(CoreError::Store(_))
    ));
}

#[test]
fn a_patch_for_an_unopened_subscription_is_refused() {
    let mut r = ClientReplica::new();
    let err = r.apply(&wire(&Downstream::Patch { sub: Sub::new("ghost"), ops: vec![] }), "f0").unwrap_err();
    assert!(matches!(err, CoreError::NotSubscribed(name) if name == "ghost"));
}

#[test]
fn a_rejected_patch_leaves_the_replica_unchanged() {
    let mut r = ClientReplica::new();
    let s = Sub::new("s");
    r.apply(&wire(&Downstream::Init { sub: s.clone(), rows: vec![row("a", 1)] }), "f0").expect("init");

    // removing an occurrence the replica does not hold does not apply.
    let err = r
        .apply(&wire(&Downstream::Patch { sub: s.clone(), ops: vec![PatchOp::Remove { occ: Occ::new("x") }] }), "f1")
        .unwrap_err();
    assert!(matches!(err, CoreError::Store(_)));
    assert_eq!(ids(&r.rows_of(&s)), ["a"], "a failed patch does not mutate the rows");
    assert_eq!(ft_string(r.frontier_of(&s)), Some("f0".to_owned()), "nor the frontier");
}

#[test]
fn malformed_frames_are_refused_never_panicked() {
    let mut r = ClientReplica::new();
    let nasty = [
        "",
        " ",
        "\0",
        "not json at all",
        "{",
        "{}",
        r#"{"type":"nonesuch"}"#,
        r#"{"type":"init","sub":"s"}"#,                                                  // missing rows
        r#"{"type":"init","sub":"s","rows":{}}"#,                                        // rows not an array
        r#"{"type":"close","sub":"s","reason":"made-up"}"#,                              // unknown reason
        r#"{"type":"patch","sub":"s","ops":[{"op":"insert","at":-1,"id":"a","value":1}]}"#, // negative position
        r#"{"type":"patch","sub":"s","ops":[{"op":"unknown","id":"a"}]}"#,               // unknown op
        "true",
        "123",
        "null",
    ];
    for input in nasty {
        assert!(r.apply(input, "f0").is_err(), "must reject: {input:?}");
    }
    // A long garbage string never aborts the process either.
    assert!(r.apply(&"a".repeat(10_000), "f0").is_err());
}

#[test]
fn upstream_requests_round_trip_through_the_wire_schema() {
    // view with params and a window.
    let body = request::view(
        "tasks",
        "public.tasks",
        Some(json!({ "q": "milk" })),
        Some(WireWindow { size: 10, anchor: WireAnchor::First, slide: false }),
        None,
        None,
    )
    .expect("encode view");
    assert_eq!(
        decode::<Upstream>(&body).expect("decode view"),
        Upstream::View {
            sub: Sub::new("tasks"),
            address: "public.tasks".to_owned(),
            params: Some(json!({ "q": "milk" })),
            window: Some(WireWindow { size: 10, anchor: WireAnchor::First, slide: false }),
            auth: None,
            context: None,
        }
    );

    // call.
    let body = request::call("public.tasks.add", json!({ "title": "buy milk" }), None, None).expect("encode call");
    assert_eq!(
        decode::<Upstream>(&body).expect("decode call"),
        Upstream::Call {
            address: "public.tasks.add".to_owned(),
            args: json!({ "title": "buy milk" }),
            auth: None,
            context: None,
        }
    );

    // operation status query.
    let body = request::operation("op-123").expect("encode operation");
    assert_eq!(
        decode::<Upstream>(&body).expect("decode operation"),
        Upstream::Operation { operation: OperationId::new("op-123") }
    );

    // the remaining bodies.
    assert_eq!(
        decode::<Upstream>(&request::unsubscribe("s").expect("encode")).expect("decode"),
        Upstream::Unsubscribe { sub: Sub::new("s") }
    );
    assert_eq!(decode::<Upstream>(&request::manifest().expect("encode")).expect("decode"), Upstream::Manifest);
    assert_eq!(
        decode::<Upstream>(&request::hello(None, None).expect("encode")).expect("decode"),
        Upstream::Hello { auth: None, context: None }
    );
    assert_eq!(
        decode::<Upstream>(&request::fetch("public.tasks", None).expect("encode")).expect("decode"),
        Upstream::Fetch { address: "public.tasks".to_owned(), params: None }
    );
}

#[test]
fn an_sse_stream_drives_the_replica_like_the_server_emits_it() {
    // The frontier rides the SSE `id:`, not the frame body — parse it out, then apply.
    let init = wire(&Downstream::Init { sub: Sub::new("s"), rows: vec![row("a", 1)] });
    let patch = wire(&Downstream::Patch {
        sub: Sub::new("s"),
        ops: vec![PatchOp::Insert { at: 1, occ: Occ::new("b"), value: json!(2) }],
    });
    let stream = format!("id: f0\ndata: {init}\n\nid: f1\ndata: {patch}\n\n");

    let mut r = ClientReplica::new();
    for line in request::parse_sse(&stream) {
        r.apply(&line.data, line.id.as_deref().unwrap_or_default()).expect("apply streamed frame");
    }
    assert_eq!(ids(&r.rows_of(&Sub::new("s"))), ["a", "b"]);
    assert_eq!(r.connection_frontier().map(|f| f.as_str().to_owned()), Some("f1".to_owned()));
}
