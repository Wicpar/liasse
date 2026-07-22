#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM: the real [`SurfaceHost`] `call` path does not close its §12.1
//! argument object — it silently drops an undeclared member and commits.
//!
//! SPEC.md §12.1: "An argument object presented to a `call` or `view` request is
//! closed: it MUST contain only names that are declared parameters of the targeted
//! mutation or view. A member whose name is not a declared parameter — including
//! any reserved `$`-prefixed name — makes the request malformed; the runtime
//! rejects it during parameter parsing (step 3), before admission, with no partial
//! effect. There is no width subtyping over external argument objects, and an
//! undeclared member is never silently dropped."
//!
//! Commit `53ed5e8` enforced this at the real `SurfaceHost` for the
//! `view`/`watch`/`resume` path (`closed_view_args`), but the `call` path was
//! left out: `build_request` only rejected a *missing* receiver argument and
//! otherwise read just the declared receiver/param names out of the argument map,
//! silently ignoring any extra member — while stuffing the *verbatim* argument map
//! (undeclared members and all) into the §12.3 dedup [`RequestModel`]. So the real
//! `SurfaceHost::call` ACCEPTED a call carrying an undeclared or reserved
//! `$`-prefixed member, dropped it, and committed — a §12.1 violation, and it made
//! the §12.3 dedup identity depend on ignored-but-present members the spec says
//! "can never silently vary between two submissions the runtime would otherwise
//! treat as one operation".
//!
//! Only the testkit adapter closed the shape (the pre-`53ed5e8` "adapter-only"
//! enforcement the corpus rides on), so no corpus scenario reaches the host's gap;
//! this probe drives `SurfaceHost::call` directly, exactly as a transport binding
//! (`liasse-connect`) would.

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{SurfaceCall, SurfaceHost, SurfaceOutcome};
use support::{address, args, host, text};

fn tasks_count(host: &SurfaceHost<MemoryStore>) -> usize {
    host.engine().view_at_head("index").expect("view").expect("declared").rows().len()
}

#[test]
fn call_with_an_undeclared_member_is_rejected_not_silently_committed() {
    let mut host = host();
    host.connect("c1").unwrap();

    // `public.tasks.add` declares exactly one parameter, `title` (no receiver).
    let base = host.call("c1", &SurfaceCall::new(address("public.tasks.add"), args([("title", text("ok"))]))).expect("dispatch");
    assert!(matches!(base, SurfaceOutcome::Committed { .. }), "the declared-only call commits: {base:?}");
    assert_eq!(tasks_count(&host), 1, "one task after the valid add");

    // §12.1: an argument object carrying a member that is NOT a declared parameter
    // (`bogus`) is malformed and MUST be rejected before admission, with no commit
    // and the undeclared member never silently dropped.
    let outcome = host
        .call("c1", &SurfaceCall::new(address("public.tasks.add"), args([("title", text("x")), ("bogus", text("y"))])))
        .expect("dispatch");
    assert!(
        matches!(outcome, SurfaceOutcome::Rejected(_)),
        "§12.1: a `call` argument object with an undeclared member (`bogus`) MUST be rejected as \
         malformed, never silently dropped and committed; got {outcome:?}",
    );
    assert_eq!(tasks_count(&host), 1, "§12.1 'no partial effect': the rejected call committed nothing");
}

#[test]
fn call_with_a_reserved_dollar_member_is_rejected() {
    let mut host = host();
    host.connect("c1").unwrap();
    // §12.1: "including any reserved `$`-prefixed name" — a `$`-prefixed member is
    // never a declared parameter, so it is malformed.
    let outcome = host
        .call("c1", &SurfaceCall::new(address("public.tasks.add"), args([("title", text("x")), ("$sneaky", text("y"))])))
        .expect("dispatch");
    assert!(
        matches!(outcome, SurfaceOutcome::Rejected(_)),
        "§12.1: a reserved `$`-prefixed argument member MUST be rejected as malformed; got {outcome:?}",
    );
    assert_eq!(tasks_count(&host), 0, "the reserved-member call committed nothing");
}

#[test]
fn dedup_identity_is_the_declared_argument_set_not_the_verbatim_map() {
    // §12.3: "a request's identity for this deduplication is its fully-decoded set
    // of DECLARED arguments ... No ignored-but-present member can silently vary
    // between two submissions the runtime would otherwise treat as one operation."
    //
    // Before the fix, an undeclared member reached the dedup `RequestModel` verbatim,
    // so two submissions of ONE operation id that agree on every DECLARED argument
    // but carry different undeclared members were treated as DIFFERENT requests: the
    // first committed (silently dropping the member) and the retry rejected as
    // "reused with different request metadata" — a §12.3 identity that varied on an
    // ignored member. With §12.1 closed on the call path, BOTH are malformed, so an
    // undeclared member can never reach — nor vary — the dedup identity.
    let mut host = host();
    host.connect("c1").unwrap();

    let first = host
        .call(
            "c1",
            &SurfaceCall::new(address("public.tasks.add"), args([("title", text("t")), ("junk", text("a"))]))
                .with_operation_id("op-1"),
        )
        .expect("dispatch");
    assert!(
        matches!(first, SurfaceOutcome::Rejected(_)),
        "the first submission carries an undeclared `junk` member and is malformed, got {first:?}",
    );

    let retry = host
        .call(
            "c1",
            &SurfaceCall::new(address("public.tasks.add"), args([("title", text("t")), ("junk", text("b"))]))
                .with_operation_id("op-1"),
        )
        .expect("dispatch");
    assert!(
        matches!(retry, SurfaceOutcome::Rejected(_)),
        "§12.3/§12.1: the retry (same op id, same declared `title`, different undeclared `junk`) is \
         also malformed — never re-observed as a differing-metadata conflict against a committed \
         first submission; got {retry:?}",
    );
    assert_eq!(tasks_count(&host), 0, "neither malformed submission committed anything");
}
