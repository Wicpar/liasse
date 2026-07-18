#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.3 at-most-once execution: a repeated operation id commits once and replays the
//! identical outcome; the same id with different arguments is rejected; a call with no
//! id is a fresh operation every time. A retained status query reads the same record.

mod support;

use liasse_wire::serde_json::json;
use liasse_wire::{Outcome, Upstream};
use support::{app, call, hello, server_titles, view};

#[test]
fn same_operation_id_commits_once_and_replays() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    let first = call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), Some("op1"));
    assert!(matches!(first, Outcome::Committed { .. }), "the first submission commits: {first:?}");
    assert_eq!(server_titles(&core, &conn, "w1"), ["m"]);

    // The same id with an equivalent request replays the retained outcome — no second
    // commit, and byte-identical positions.
    let replay = call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), Some("op1"));
    assert_eq!(first, replay, "the retry replays the identical committed outcome");
    assert_eq!(server_titles(&core, &conn, "w1"), ["m"], "no duplicate commit");

    // The same id with different arguments is a burned-identifier conflict.
    let conflict = call(&mut core, &conn, "public.tasks.add", json!({ "title": "other" }), Some("op1"));
    assert!(matches!(conflict, Outcome::Rejected { .. }), "reused id + different args is rejected: {conflict:?}");
    assert_eq!(server_titles(&core, &conn, "w1"), ["m"], "the conflict commits nothing");
}

#[test]
fn a_status_query_reads_the_retained_record() {
    let mut core = app();
    let conn = hello(&mut core);
    let committed = call(&mut core, &conn, "public.tasks.add", json!({ "title": "m" }), Some("op7"));

    let status = core
        .submit(Some(&conn), None, Upstream::Operation { operation: liasse_wire::OperationId::new("op7") })
        .expect("operation query");
    match status {
        liasse_connect::Reply::Outcome(outcome) => {
            assert_eq!(outcome, committed, "the retained status matches the committed outcome");
        }
        other => panic!("status query: {other:?}"),
    }

    // An id this connection never issued reads as `unknown` — no cross-client leak.
    let unknown = core
        .submit(Some(&conn), None, Upstream::Operation { operation: liasse_wire::OperationId::new("never") })
        .expect("operation query");
    assert!(matches!(unknown, liasse_connect::Reply::Outcome(Outcome::Unknown)));
}

#[test]
fn no_operation_id_is_a_fresh_operation_each_time() {
    let mut core = app();
    let conn = hello(&mut core);
    view(&mut core, &conn, "w1", "public.tasks");

    call(&mut core, &conn, "public.tasks.add", json!({ "title": "dup" }), None);
    call(&mut core, &conn, "public.tasks.add", json!({ "title": "dup" }), None);
    assert_eq!(server_titles(&core, &conn, "w1"), ["dup", "dup"], "each id-less add is a distinct commit");
}
