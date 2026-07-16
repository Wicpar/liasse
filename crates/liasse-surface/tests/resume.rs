#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12.2 resume-from-retained-frontier: a client that retains a frontier, sees
//! later commits, drops its connection, and resumes reconstructs the authorized
//! declared view at the current frontier — and a resume that has since lost
//! authority delivers no rows.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CommitSeq, Subscription, SurfaceHost, SurfaceResume, SurfaceWatch, Value, ViewResult,
};
use support::{add_task, address, authenticate_member, call, host, text};

/// Open an unwindowed subscription and return its retained init frontier.
fn watch_frontier(host: &mut SurfaceHost<MemoryStore>, conn: &str, target: &str, id: &str) -> CommitSeq {
    match host.watch(conn, &SurfaceWatch::new(address(target), id)).expect("watch") {
        Subscription::Init(_) => host.frontier(conn).expect("connection open"),
        other => panic!("expected an init, got {other:?}"),
    }
}

/// The `title` column of a view result, in order.
fn titles(result: &ViewResult) -> Vec<String> {
    result
        .rows()
        .iter()
        .map(|row| match row.field("title") {
            Some(Value::Text(t)) => t.as_str().to_owned(),
            other => panic!("unexpected title cell {other:?}"),
        })
        .collect()
}

#[test]
fn resume_reconstructs_the_current_authorized_view() {
    // §12.2: resuming yields the authorized declared view at the current frontier.
    // The client retains the init frontier, commits happen after it, the
    // connection drops, and the resumed subscription reconstructs the result.
    let mut host = host();
    host.connect("c1");
    let from = watch_frontier(&mut host, "c1", "public.tasks", "w1");
    add_task(&mut host, "c1", "a");
    add_task(&mut host, "c1", "b");
    host.disconnect("c1");

    host.connect("c2");
    let resume = SurfaceResume::new(address("public.tasks"), "w2", from);
    match host.resume("c2", &resume).expect("resume") {
        Subscription::Init(result) => {
            assert_eq!(titles(&result), ["a", "b"], "the resume reconstructs the current view");
        }
        other => panic!("expected a reconstructed init, got {other:?}"),
    }
}

#[test]
fn resume_after_authority_loss_delivers_no_rows() {
    // §12.2: a resume yields only *authorized* patches; membership/session are
    // re-evaluated at resume. Revoking the session removes authority, so the
    // retained frontier may not be replayed into fresh data.
    let mut host = host();
    host.connect("c1");
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), liasse_surface::AuthResult::Bound));
    let from = watch_frontier(&mut host, "c1", "member.tasks", "m1");

    // A second connection revokes alice's session without sweeping c1.
    host.connect("c2");
    host.call("c2", &call("public.session.revoke", [("id", text("s_alice"))])).expect("revoke").commit().unwrap();

    // Resuming the member subscription on c1 re-authorizes against the revoked
    // session and is refused before any row flows.
    let resume = SurfaceResume::new(address("member.tasks"), "m2", from);
    match host.resume("c1", &resume).expect("resume") {
        Subscription::Denied(_) => {}
        other => panic!("a resume after authority loss must be denied, got {other:?}"),
    }
}
