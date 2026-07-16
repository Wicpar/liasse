#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §22 restart/durability: [`SurfaceHost::into_parts`] hands the engine, router,
//! and clock back so a driver can drop the running host — losing its volatile
//! connection, subscription, and operation state — and rebuild a fresh host over
//! the same engine. Committed state survives the handoff unchanged; no seed is
//! re-applied and no generated value is re-rolled.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    OperationKey, OperationStatus, SurfaceHost, SurfaceOutcome, SurfaceRouter, VirtualClock,
};
use support::{add_task, call, host, text};

/// Rebuild a fresh host over the parts a restart hands back — the exact sequence
/// a durability driver performs.
fn restart(host: SurfaceHost<MemoryStore>) -> SurfaceHost<MemoryStore> {
    let (engine, router, clock): (_, SurfaceRouter, VirtualClock) = host.into_parts();
    SurfaceHost::new(engine, router, clock)
}

#[test]
fn committed_state_survives_a_restart_unchanged() {
    // A committed task — including its generated `uuid` key — must be exactly the
    // same after the host is torn down and rebuilt over the same engine, and a
    // fresh connection must read it. Rebuilding reuses the engine, so no `$data`
    // seed is re-applied and no key is re-rolled.
    let mut host = host();
    host.connect("c1");
    let id = add_task(&mut host, "c1", "durable");

    let mut host = restart(host);
    host.connect("c2");

    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    let row = view
        .rows()
        .iter()
        .find(|row| row.field("title") == Some(&text("durable")))
        .expect("the task survives the restart");
    assert_eq!(row.field("id"), Some(&id), "the generated key is not re-rolled by a restart");
}

#[test]
fn a_restart_drops_volatile_operation_records() {
    // An operation record is at-most-once *within a run*, not durable: a restart
    // clears the retained log, so the identifier's status is `Unknown` afterwards.
    let mut host = host();
    host.connect("c1");
    let op = "op-restart-1";
    let outcome = host
        .call("c1", &call("public.tasks.add", [("title", text("logged"))]).with_operation_id(op))
        .expect("add");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "add commits: {outcome:?}");
    let key = OperationKey::new("public.tasks", None, op);
    assert!(
        !matches!(host.operation_status(&key), OperationStatus::Unknown),
        "the operation is retained before the restart",
    );

    let host = restart(host);
    assert!(
        matches!(host.operation_status(&key), OperationStatus::Unknown),
        "the volatile operation log does not survive a restart",
    );
}

#[test]
fn a_restart_resets_connections_to_the_retained_head() {
    // Connections and subscriptions are volatile: after a restart the rebuilt
    // host has none, and a freshly opened connection's frontier is the engine's
    // retained head — the same committed position the torn-down host left.
    let mut host = host();
    host.connect("c1");
    add_task(&mut host, "c1", "before");
    let head = host.engine().head();
    assert!(host.frontier("c1").is_some(), "the pre-restart connection is open");

    let mut host = restart(host);
    assert!(host.frontier("c1").is_none(), "the restart drops the old connection");
    host.connect("c2");
    assert_eq!(host.frontier("c2"), Some(head), "a fresh connection starts at the retained head");
}
