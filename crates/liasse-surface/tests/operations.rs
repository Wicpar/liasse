#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.3 operation identifiers: at-most-once execution for an equivalent retry,
//! rejection of a divergent reuse, per-target scope distinctness, a new operation
//! for every identifier-less call, and retained operation status as a capability.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{OperationKey, OperationStatus, Subscription, SurfaceHost, SurfaceOutcome, SurfaceWatch};
use support::{address, call, host, text};

/// The number of rows currently in the public `index` view.
fn row_count(host: &SurfaceHost<MemoryStore>) -> usize {
    host.engine().view_at_head("index").expect("view").expect("declared").len()
}

/// Open an unwindowed subscription over `target` on `conn`.
fn watch(host: &mut SurfaceHost<MemoryStore>, conn: &str, target: &str, id: &str) {
    match host.watch(conn, &SurfaceWatch::new(address(target), id)).expect("watch") {
        Subscription::Init(_) => {}
        other => panic!("expected an init, got {other:?}"),
    }
}

#[test]
fn equivalent_retry_executes_once() {
    // §12.3: reusing the identifier with an equivalent request is at-most-once.
    let mut host = host();
    host.connect("c1").unwrap();
    let first = host
        .call("c1", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op-1"))
        .expect("call");
    assert!(matches!(first, SurfaceOutcome::Committed { .. }), "first submission commits");

    let retry = host
        .call("c1", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op-1"))
        .expect("call");
    assert!(retry.is_ok(), "the retry is accepted: {retry:?}");
    assert_eq!(row_count(&host), 1, "the retry did not execute a second time");
}

#[test]
fn divergent_reuse_is_rejected() {
    // §12.3: reusing the identifier with different request metadata rejects.
    let mut host = host();
    host.connect("c1").unwrap();
    host.call("c1", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op-2")).expect("call");

    let reuse = host
        .call("c1", &call("public.tasks.add", [("title", text("b"))]).with_operation_id("op-2"))
        .expect("call");
    assert!(reuse.rejection().is_some(), "the divergent reuse is rejected: {reuse:?}");
    assert!(reuse.denial().is_none(), "a burned-identifier conflict is a rejection, not a denial");
    assert_eq!(row_count(&host), 1, "the mismatched reuse neither executed nor damaged the record");
}

#[test]
fn identifier_less_call_is_new_every_time() {
    let mut host = host();
    host.connect("c1").unwrap();
    host.call("c1", &call("public.tasks.add", [("title", text("a"))])).expect("call");
    host.call("c1", &call("public.tasks.add", [("title", text("a"))])).expect("call");
    assert_eq!(row_count(&host), 2, "a call with no operation id is a new operation each time");
}

#[test]
fn identifier_scope_is_distinct_per_target() {
    // The same identifier against a *different* surface is an independent scope,
    // so both execute (§12.3, `red/operation-id-scope-distinct-per-target`).
    let mut host = host();
    host.connect("c1").unwrap();
    let one = host
        .call("c1", &call("public.tasks.add", [("title", text("x"))]).with_operation_id("op-3"))
        .expect("call");
    let two = host
        .call("c1", &call("public.intake.add", [("title", text("x"))]).with_operation_id("op-3"))
        .expect("call");
    assert!(matches!(one, SurfaceOutcome::Committed { .. }), "first target commits");
    assert!(matches!(two, SurfaceOutcome::Committed { .. }), "second target commits independently");
    assert_eq!(row_count(&host), 2, "distinct scopes both executed");
}

#[test]
fn retained_status_reports_the_committed_operation() {
    let mut host = host();
    host.connect("c1").unwrap();
    let outcome = host
        .call("c1", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op-4"))
        .expect("call");
    let commit = outcome.commit().expect("committed");

    let key = OperationKey::new("public.tasks", None, "op-4");
    match host.operation_status(&key) {
        OperationStatus::Committed { commit: at, .. } => assert_eq!(at, commit, "status reports the commit position"),
        other => panic!("expected a committed status, got {other:?}"),
    }
}

#[test]
fn replay_settles_the_replaying_connection() {
    // §12.3: receiving `committed` — even on an operation-id replay — proves the
    // *replaying* connection's authorized live results have advanced through that
    // commit. A retry from a lagging connection must therefore sweep it, not just
    // hand back the stored outcome.
    let mut host = host();
    host.connect("c1").unwrap();
    host.connect("c2").unwrap();
    watch(&mut host, "c2", "public.tasks", "w2");

    // c1 commits the operation; c2 is not on c1's connection, so it lags.
    let first = host
        .call("c1", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op"))
        .expect("first");
    let commit = first.commit().expect("first commits");
    assert!(host.read_view("c2", "w2").expect("view").is_empty(), "c2's watch has not seen c1's commit");

    // c2 replays the equivalent operation: at-most-once (no second row), yet its
    // own frontier and subscription now reflect the commit.
    let replay = host
        .call("c2", &call("public.tasks.add", [("title", text("a"))]).with_operation_id("op"))
        .expect("replay");
    assert!(replay.is_ok(), "the replay is accepted: {replay:?}");
    assert_eq!(replay.commit(), Some(commit), "the replay re-observes the stored commit");
    assert_eq!(row_count(&host), 1, "the replay did not execute a second time");
    assert_eq!(host.frontier("c2"), Some(commit), "the replaying connection advanced through the commit");
    assert_eq!(host.read_view("c2", "w2").expect("view").len(), 1, "the replay swept c2's subscription");
}

#[test]
fn status_is_unknown_for_an_unheld_identifier() {
    // The high-entropy identifier is the capability to read a record; an unknown
    // one yields `unknown` (§12.3).
    let host = host();
    let key = OperationKey::new("public.tasks", None, "op-never-submitted");
    assert_eq!(host.operation_status(&key), OperationStatus::Unknown);
}
