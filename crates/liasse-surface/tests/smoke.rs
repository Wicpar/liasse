#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! Smoke: the fixture package loads, its router validates, and a public view
//! resolves — the baseline every other case builds on.

mod support;

use liasse_surface::{Subscription, SurfaceOutcome};
use support::{call, host, text};

#[test]
fn fixture_loads_and_public_call_commits() {
    let mut host = host();
    host.connect("c1");

    let sub = host.watch("c1", &liasse_surface::SurfaceWatch::new(support::address("public.tasks"), "w1")).expect("watch");
    assert!(matches!(sub, Subscription::Init(ref result) if result.is_empty()), "empty init");

    let outcome = host.call("c1", &call("public.tasks.add", [("title", text("hello"))])).expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "public add commits: {outcome:?}");

    let view = host.read_view("c1", "w1").expect("watch result present");
    assert_eq!(view.len(), 1, "the committed row is reflected on the same connection");
    assert_eq!(view.rows()[0].field("title"), Some(&text("hello")));
    assert_eq!(view.rows()[0].field("owner"), None, "the view projects only id and title");
}
