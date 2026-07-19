#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §12 clients and live views: watch init, patch coherence across commits, the
//! same-connection completion barrier, the at-least-own-commit frontier guarantee
//! under a second connection's write, and authority-loss `close`.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{Subscription, SurfaceHost, SurfaceWatch, Value, ViewResult};
use support::{add_task, address, authenticate_member, call, host, text};

/// Open a subscription and return its initial complete result.
fn watch(host: &mut SurfaceHost<MemoryStore>, conn: &str, target: &str, id: &str) -> ViewResult {
    match host.watch(conn, &SurfaceWatch::new(address(target), id)).expect("watch") {
        Subscription::Init(result) => result,
        other => panic!("expected an unwindowed init, got {other:?}"),
    }
}

/// The `title` column of a subscription's current result, in order.
fn titles(host: &SurfaceHost<MemoryStore>, conn: &str, id: &str) -> Vec<String> {
    let view = host.read_view(conn, id).expect("view present");
    view.rows()
        .iter()
        .map(|row| match row.field("title") {
            Some(Value::Text(text)) => text.as_str().to_owned(),
            other => panic!("unexpected title cell {other:?}"),
        })
        .collect()
}

#[test]
fn watch_init_is_the_complete_current_result() {
    let mut host = host();
    host.connect("c1").unwrap();
    add_task(&mut host, "c1", "seed");
    let init = watch(&mut host, "c1", "public.tasks", "w1");
    assert_eq!(init.len(), 1, "init carries the complete current result");
    assert_eq!(init.rows()[0].field("title"), Some(&text("seed")));
}

#[test]
fn patches_stay_coherent_with_the_declared_view() {
    // §12.2: after each commit the client result equals the sorted declared view.
    let mut host = host();
    host.connect("c1").unwrap();
    let empty = watch(&mut host, "c1", "public.tasks", "w1");
    assert!(empty.is_empty());

    let m = add_task(&mut host, "c1", "m");
    assert_eq!(titles(&host, "c1", "w1"), vec!["m"]);

    add_task(&mut host, "c1", "a");
    assert_eq!(titles(&host, "c1", "w1"), vec!["a", "m"], "new row sorts before the existing one");

    let rename = call("public.tasks.rename", [("id", m.clone()), ("title", text("z"))]);
    assert!(host.call("c1", &rename).expect("rename").is_ok());
    assert_eq!(titles(&host, "c1", "w1"), vec!["a", "z"], "the sort-changing update re-orders");

    // remove the first row (title "a"); it must leave the result.
    let a = row_id(&host, "a");
    assert!(host.call("c1", &call("public.tasks.remove", [("id", a)])).expect("remove").is_ok());
    assert_eq!(titles(&host, "c1", "w1"), vec!["z"]);
}

/// The generated id of the task currently titled `title`.
fn row_id(host: &SurfaceHost<MemoryStore>, title: &str) -> Value {
    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    view.rows()
        .iter()
        .find(|row| row.field("title") == Some(&text(title)))
        .and_then(|row| row.field("id").cloned())
        .expect("row present")
}

#[test]
fn committed_call_advances_same_connection_watch() {
    // §12.3: receiving `committed` proves the same-connection watch already
    // reflects that commit.
    let mut host = host();
    host.connect("c1").unwrap();
    let init = watch(&mut host, "c1", "public.tasks", "w1");
    assert!(init.is_empty());
    let outcome = host.call("c1", &call("public.tasks.add", [("title", text("live"))])).expect("call");
    assert!(outcome.commit().is_some(), "the call committed");
    assert_eq!(titles(&host, "c1", "w1"), vec!["live"], "the watch reflects the commit before the call returned");
}

#[test]
fn frontier_covers_at_least_the_callers_own_commit() {
    // A second connection's committed write becomes visible to the first no later
    // than the first's own next returned commit (§12.3, §3.3/§22.3).
    let mut host = host();
    host.connect("c1").unwrap();
    host.connect("c2").unwrap();
    let w1 = watch(&mut host, "c1", "public.tasks", "w1");
    assert!(w1.is_empty());

    // c2 commits a write. c1 made no call, so its watch still lags.
    add_task(&mut host, "c2", "b");
    assert!(titles(&host, "c1", "w1").is_empty(), "c1's watch does not advance on c2's commit");

    // c1 commits its own write; its watch now covers at least its own commit,
    // which includes c2's earlier one.
    add_task(&mut host, "c1", "a");
    assert_eq!(titles(&host, "c1", "w1"), vec!["a", "b"], "c1 now sees its own and the earlier peer commit");
}

#[test]
fn losing_authority_closes_a_role_subscription() {
    // §12.2: when state removes a subscription's authority the runtime emits
    // `close`. Revoking the session revokes the member watch at the next frontier.
    let mut host = host();
    host.connect("c1").unwrap();
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), liasse_surface::AuthResult::Bound));
    let init = watch(&mut host, "c1", "member.tasks", "m1");
    assert!(init.is_empty(), "the member watch opens");

    // A public revoke on the same connection sweeps the member watch at the new
    // frontier; re-auth fails, so the watch closes.
    let revoke = host.call("c1", &call("public.session.revoke", [("id", text("s_alice"))])).expect("revoke");
    assert!(revoke.commit().is_some(), "revoke commits");
    assert!(host.close_reason("c1", "m1").is_some(), "the member subscription is closed after authority loss");
    assert!(host.read_view("c1", "m1").is_none(), "a closed subscription releases its cached result");
}

#[test]
fn unauthenticated_role_watch_is_denied() {
    // §10.4: an unauthenticated watch on an existing role view is denied with the
    // uniform `unresolved` — identical to a nonexistent role — so the wire code does
    // not disclose that `member` exists to an anonymous enumerator.
    let mut host = host();
    host.connect("c1").unwrap();
    match host.watch("c1", &SurfaceWatch::new(address("member.tasks"), "m1")).expect("watch") {
        Subscription::Denied(denial) => {
            assert_eq!(denial.reason(), liasse_surface::DenialReason::Unresolved);
        }
        other => panic!("an unauthenticated role watch must be denied, got {other:?}"),
    }
}

#[test]
fn peer_commit_removing_authority_closes_a_cross_connection_subscription() {
    // §12.2: a commit on one connection is an outgoing frontier for a subscription
    // on another. Revoking alice's session from c2 must close her member
    // subscription on c1, even though c1 issued no request of its own.
    let mut host = host();
    host.connect("c1").unwrap();
    host.connect("c2").unwrap();
    assert!(matches!(authenticate_member(&mut host, "c1", "s_alice"), liasse_surface::AuthResult::Bound));
    let init = watch(&mut host, "c1", "member.tasks", "m1");
    assert!(init.is_empty(), "the member subscription opens");

    // The revoke commits on c2; c1 made no call, yet its subscription's authority
    // is re-evaluated at that outgoing frontier and removed.
    let revoke = host.call("c2", &call("public.session.revoke", [("id", text("s_alice"))])).expect("revoke");
    assert!(revoke.commit().is_some(), "the revoke commits");
    assert!(
        host.close_reason("c1", "m1").is_some(),
        "the cross-connection subscription is closed after its authority is revoked",
    );
}

#[test]
fn a_per_request_auth_selection_opens_and_re_authorizes_a_role_subscription() {
    // §11.4: a subscription may carry its own `auth` selection instead of reusing a
    // connection context, so a client opens a role subscription with no prior
    // `authenticate`; §12.2 then re-authorizes from that retained credential, so a
    // later revocation still closes it.
    let mut host = host();
    host.connect("c1").unwrap();
    let selection = liasse_surface::AuthSelection::new(
        "token",
        liasse_surface::Credential::new(text("s_alice")),
    );
    let request = SurfaceWatch::new(address("member.tasks"), "m1").with_auth(selection);
    match host.watch("c1", &request).expect("watch") {
        Subscription::Init(result) => assert!(result.is_empty(), "the inline-authenticated subscription opens"),
        other => panic!("an inline-auth role watch must open, got {other:?}"),
    }

    // The retained selection is re-verified at the revoke's frontier, closing the
    // subscription even though no connection context ever backed it.
    let revoke = host.call("c1", &call("public.session.revoke", [("id", text("s_alice"))])).expect("revoke");
    assert!(revoke.commit().is_some(), "the revoke commits");
    assert!(
        host.close_reason("c1", "m1").is_some(),
        "a subscription opened under a per-request selection re-authorizes from that credential and closes on revocation",
    );
}
