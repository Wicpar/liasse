#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 bounded windows over the SSE stream: first/last/anchored/sliding slices, an
//! absent concrete anchor, and a scalar/aggregate view — the last two delivering
//! `failed` frames rather than opening.

mod support;

use liasse_connect::{ConnectCore, Reply, Schema};
use liasse_ident::InstanceId;
use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Precision, SurfaceBinding, SurfaceHost, SurfaceRouter, SurfaceRouterBuilder,
    ViewBinding, VirtualClock,
};
use liasse_value::Type;
use liasse_wire::serde_json::json;
use liasse_wire::{FailedCode, Occ, Outcome, WireAnchor, WireWindow};

use support::{Client, app, call, drain, hello, view, view_frame, view_reply, view_request, NOW};

/// Add tasks titled a..e (the `index` view sorts by title).
fn seed(core: &mut ConnectCore<MemoryStore>, conn: &liasse_wire::ConnectionToken) {
    for title in ["a", "b", "c", "d", "e"] {
        call(core, conn, "public.tasks.add", json!({ "title": title }), None);
    }
}

fn windowed(sub: &str, size: usize, anchor: WireAnchor, slide: bool) -> liasse_wire::Upstream {
    view_request(sub, "public.tasks", Some(WireWindow { size, anchor, slide }))
}

#[test]
fn first_and_last_windows_slice_the_view_edges() {
    let mut core = app();
    let conn = hello(&mut core);
    seed(&mut core, &conn);

    view_frame(&mut core, &conn, windowed("first", 2, WireAnchor::First, false));
    view_frame(&mut core, &conn, windowed("last", 2, WireAnchor::Last, false));

    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.titles("first"), ["a", "b"]);
    assert_eq!(client.titles("last"), ["d", "e"]);
}

#[test]
fn a_concrete_anchor_becomes_the_first_row_and_slides_when_asked() {
    let mut core = app();
    let conn = hello(&mut core);
    seed(&mut core, &conn);

    // A full subscription lets the client learn occurrence tokens; the anchor is the
    // token of the row titled "c".
    view(&mut core, &conn, "full", "public.tasks");
    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    let anchor: Occ = client.occ("full")[2].clone();

    view_frame(&mut core, &conn, windowed("anc", 2, WireAnchor::At { occ: anchor.clone() }, false));
    view_frame(&mut core, &conn, windowed("slide", 3, WireAnchor::At { occ: anchor }, true));
    client.feed(&drain(&mut core, &conn));

    assert_eq!(client.titles("anc"), ["c", "d"], "the anchor becomes the first row");
    assert_eq!(client.titles("slide"), ["b", "c", "d"], "a sliding window centers the anchor");
}

#[test]
fn an_absent_anchor_fails_to_open() {
    let mut core = app();
    let conn = hello(&mut core);
    seed(&mut core, &conn);

    view(&mut core, &conn, "full", "public.tasks");
    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    let departed: Occ = client.occ("full")[1].clone(); // the row titled "b"

    // Remove "b": its occurrence token is still well-formed and known to the
    // connection, but names no current occurrence, so a window anchored on it fails.
    let id = support::task_id_json(&core, "b");
    call(&mut core, &conn, "public.tasks.remove", json!({ "id": id }), None);

    let reply = view_reply(&mut core, &conn, windowed("gone", 2, WireAnchor::At { occ: departed }, false));
    assert!(
        matches!(reply, Reply::Outcome(Outcome::Failed { code: FailedCode::AbsentAnchor })),
        "an absent anchor is a failed frame, not an authorization refusal: {reply:?}",
    );
}

// --- a scalar/aggregate view, which no window can bound (§7.5) ----------------------

const SCALAR_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.scalar@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text" }
    "count": { "$view": "= size(.items)" }
    "$mut": { "add": ".items + { id: @id }" }
    "$public": {
      "counter": { "$view": ".count" }
      "items": { "$mut": { "add": ".add" } }
    }
  }
}"#;

fn scalar_router(model: &liasse_model::Model) -> SurfaceRouter {
    let counter = SurfaceBinding::new().with_view(ViewBinding::new("count"));
    let items = SurfaceBinding::new().with_call("add", CallBinding::root("add", ["id".to_owned()]));
    SurfaceRouterBuilder::new()
        .public_surface("counter", counter)
        .public_surface("items", items)
        .build(model)
        .expect("scalar router validates")
}

fn scalar_core() -> ConnectCore<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(MemoryStore::new(InstanceId::new("scalar")), SCALAR_APP, &mut clock)
        .expect("scalar app loads");
    let schema = Schema::builder()
        .view("public.counter", engine.surface_view_params("public.counter"))
        .call("public.items.add", vec![("id".to_owned(), Type::Text)])
        .build();
    let router = scalar_router(engine.model());
    ConnectCore::mount(SurfaceHost::new(engine, router, clock), schema)
}

#[test]
fn a_scalar_view_delivers_a_value_and_refuses_a_window() {
    let mut core = scalar_core();
    let conn = hello(&mut core);

    // Unwindowed: the aggregate delivers a scalar value that advances on commit.
    view(&mut core, &conn, "n", "public.counter");
    let mut client = Client::new();
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.scalar("n"), Some(json!("0")), "the initial count is zero");

    call(&mut core, &conn, "public.items.add", json!({ "id": "x" }), None);
    client.feed(&drain(&mut core, &conn));
    assert_eq!(client.scalar("n"), Some(json!("1")), "the scalar advances on commit");

    // Windowed: a scalar view has no rows to bound, so the window fails to open.
    let reply = view_reply(
        &mut core,
        &conn,
        view_request("bad", "public.counter", Some(WireWindow { size: 2, anchor: WireAnchor::First, slide: false })),
    );
    assert!(
        matches!(reply, Reply::Outcome(Outcome::Failed { code: FailedCode::ScalarView })),
        "a window over a scalar view is a scalar-view failure: {reply:?}",
    );
}
