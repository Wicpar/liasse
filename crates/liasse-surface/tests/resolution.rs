#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §10 surface resolution and role gating: only exposed members are callable,
//! internal declarations are unreachable, and a `denied` exposure/authorization
//! failure is a distinct outcome from a runtime `rejected` admission failure
//! (`tests/10-interfaces-roles/NOTES.md` outcome-mapping convention).

mod support;

use liasse_surface::{DenialReason, SurfaceCall, SurfaceOutcome};
use support::{call, host, text};

/// A denied call's reason, or a panic naming the actual outcome.
fn denial(outcome: &SurfaceOutcome) -> DenialReason {
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial, got {outcome:?}"),
    }
}

#[test]
fn public_surface_admits_unauthenticated_call() {
    let mut host = host();
    host.connect("c1");
    let outcome = host.call("c1", &call("public.tasks.add", [("title", text("x"))])).expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "public call commits: {outcome:?}");
}

#[test]
fn internal_mutation_is_not_addressable() {
    // `disable` and `revoke` are declared mutations, but `public.tasks` exposes
    // only add/rename/remove — an internal name resolves to nothing (§10.1).
    let mut host = host();
    host.connect("c1");
    let outcome = host.call("c1", &call("public.tasks.disable", [("id", text("alice"))])).expect("call");
    assert_eq!(denial(&outcome), DenialReason::Unresolved);
}

#[test]
fn nonexistent_surface_and_call_are_unresolved() {
    let mut host = host();
    host.connect("c1");
    let ghost = host.call("c1", &call("public.ghost.add", [("title", text("x"))])).expect("call");
    assert_eq!(denial(&ghost), DenialReason::Unresolved);
    let ghost_call = host.call("c1", &call("public.tasks.frobnicate", [("title", text("x"))])).expect("call");
    assert_eq!(denial(&ghost_call), DenialReason::Unresolved);
}

#[test]
fn case_variant_names_do_not_resolve() {
    // §2.5 names are ASCII and exact; no folding is implied.
    let mut host = host();
    host.connect("c1");
    let surface = host.call("c1", &call("public.Tasks.add", [("title", text("x"))])).expect("call");
    assert_eq!(denial(&surface), DenialReason::Unresolved);
    let member = host.call("c1", &call("public.tasks.Add", [("title", text("x"))])).expect("call");
    assert_eq!(denial(&member), DenialReason::Unresolved);
}

#[test]
fn unknown_role_is_unresolved() {
    let mut host = host();
    host.connect("c1");
    let outcome = host.call("c1", &call("admin.tasks.complete", [("id", text("x")), ("title", text("y"))])).expect("call");
    assert_eq!(denial(&outcome), DenialReason::Unresolved);
}

#[test]
fn role_surface_requires_authentication() {
    // A role surface addressed with no authenticated actor is denied (§10.2/§11).
    // §10.4: the denial is the uniform `unresolved`, byte-identical to a
    // nonexistent role (see `unknown_role_is_unresolved`) — an `unauthenticated`
    // reason here would leak that `member` exists to an anonymous enumerator.
    let mut host = host();
    host.connect("c1");
    let outcome = host.call("c1", &call("member.tasks.complete", [("id", text("x")), ("title", text("y"))])).expect("call");
    assert_eq!(denial(&outcome), DenialReason::Unresolved);
}

#[test]
fn denied_is_distinct_from_rejected() {
    // Exposure/authorization failure is `denied`; a well-addressed request that
    // the admission pipeline refuses is `rejected` — the two never collapse.
    let mut host = host();
    host.connect("c1");

    let unexposed = host.call("c1", &call("public.tasks.disable", [("id", text("alice"))])).expect("call");
    assert!(unexposed.denial().is_some(), "unexposed name denies");
    assert!(unexposed.rejection().is_none(), "an unexposed name is not a runtime rejection");

    // rename of an absent task is a well-addressed request the runtime refuses.
    let rejected = host
        .call("c1", &call("public.tasks.rename", [("id", text("00000000-0000-0000-0000-000000000000")), ("title", text("z"))]))
        .expect("call");
    assert!(rejected.rejection().is_some(), "an absent target is rejected: {rejected:?}");
    assert!(rejected.denial().is_none(), "an admission refusal is not a denial");
}

#[test]
fn view_address_cannot_be_called_and_call_cannot_be_watched() {
    let mut host = host();
    host.connect("c1");
    // `public.tasks` (two segments) is the view target; calling it is unresolved.
    let as_call = host.call("c1", &SurfaceCall::new(support::address("public.tasks"), support::args([]))).expect("call");
    assert_eq!(denial(&as_call), DenialReason::Unresolved);
}
