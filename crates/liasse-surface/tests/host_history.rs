#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §19/§23.5 host operations over the surface: export produces verified `.liasse`
//! bytes a later import consumes, an import moves live committed state under a
//! movement policy, a tampered artifact is refused before any movement, reconcile
//! computes the three-way merge, and a trusted operator transition commits while
//! bypassing surface role authentication.

mod support;

use liasse_surface::{ImportRelation, SurfaceOutcome};
use support::{add_task, call, host, text};

/// The number of rows the `index` view currently reports.
fn task_count(host: &liasse_surface::SurfaceHost<liasse_store::MemoryStore>) -> usize {
    host.engine().view_at_head("index").expect("view").expect("declared").len()
}

/// §19.5/§19.8: an export at the current boundary classifies as the same point;
/// after state advances the earlier export sits behind the head as a rollback,
/// which a permitting policy activates by moving live committed state back.
#[test]
fn export_then_import_rolls_state_back() {
    let mut host = host();
    host.connect("$default");
    add_task(&mut host, "$default", "first");
    let snapshot = host.export().expect("export");
    assert_eq!(host.classify(&snapshot).expect("classify"), ImportRelation::SamePoint);

    add_task(&mut host, "$default", "second");
    assert_eq!(task_count(&host), 2);

    // The earlier snapshot now sits behind the head: a rollback.
    assert_eq!(host.classify(&snapshot).expect("classify"), ImportRelation::Rollback);
    let report = host.import(&snapshot, &[ImportRelation::Rollback]).expect("import rollback");
    assert_eq!(report.relation, ImportRelation::Rollback);
    assert!(report.applied, "a permitted rollback activates");
    assert_eq!(task_count(&host), 1, "state rolled back to the first boundary");
}

/// §19.8: a movement whose relation the policy does not permit is classified but
/// not activated — committed state is untouched.
#[test]
fn import_not_permitted_by_policy_leaves_state() {
    let mut host = host();
    host.connect("$default");
    add_task(&mut host, "$default", "first");
    let snapshot = host.export().expect("export");
    add_task(&mut host, "$default", "second");
    assert_eq!(task_count(&host), 2);

    let report = host.import(&snapshot, &[]).expect("classify only");
    assert_eq!(report.relation, ImportRelation::Rollback);
    assert!(!report.applied, "an empty policy activates nothing");
    assert_eq!(task_count(&host), 2, "state is unchanged");
}

/// §19.8: a tampered artifact fails recursive verification and is refused before
/// anything is classified or moved.
#[test]
fn tampered_artifact_is_refused() {
    let mut host = host();
    host.connect("$default");
    add_task(&mut host, "$default", "first");
    let mut bytes = host.export().expect("export");
    // Corrupt an interior byte so the container checksum no longer holds.
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    assert!(host.classify(&bytes).is_err(), "a corrupt artifact does not classify");
    assert!(host.import(&bytes, &[ImportRelation::Rollback]).is_err(), "and does not import");
}

/// §19.9: reconcile computes a clean three-way merge, and a row inserted only in
/// live local state survives against a base/incoming that lack it.
#[test]
fn reconcile_keeps_local_only_insert() {
    let mut host = host();
    host.connect("$default");
    add_task(&mut host, "$default", "first");
    let base = host.export().expect("export base");
    // The base and the incoming are the same boundary (only `first`); local state
    // then gains a second task.
    add_task(&mut host, "$default", "second");

    let outcome = host.reconcile(&base, &base).expect("reconcile");
    assert!(outcome.is_clean(), "no divergent change conflicts: {:?}", outcome.conflicts);
    let task_rows = outcome.merged.keys().filter(|address| address.render().contains("tasks")).count();
    assert_eq!(task_rows, 2, "the locally-inserted task row is kept alongside the base task row");
}

/// §23.5: a trusted operator transition commits a role-gated mutation while
/// bypassing surface authentication — the very same target a plain client call
/// denies for want of an authenticated actor.
#[test]
fn operator_bypasses_role_authentication() {
    let mut host = host();
    host.connect("$default");
    let id = add_task(&mut host, "$default", "chore");

    // A plain client call to the member-role surface, unauthenticated, is denied.
    let denied = host
        .call("$default", &call("member.tasks.complete", [("id", id.clone()), ("title", text("done"))]))
        .expect("call");
    assert!(matches!(denied, SurfaceOutcome::Denied(_)), "unauthenticated role call is denied: {denied:?}");

    // The operator drives the same mutation without any authentication.
    let committed = host
        .operator_call(&call("member.tasks.complete", [("id", id), ("title", text("operator-done"))]))
        .expect("operator call");
    assert!(matches!(committed, SurfaceOutcome::Committed { .. }), "operator commits: {committed:?}");

    let view = host.engine().view_at_head("index").expect("view").expect("declared");
    let titles: Vec<_> = view.rows().iter().filter_map(|r| r.field("title").cloned()).collect();
    assert!(titles.contains(&text("operator-done")), "the operator transition took effect: {titles:?}");
}
