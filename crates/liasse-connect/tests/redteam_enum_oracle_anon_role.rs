#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM finding — §10.4 role-catalog enumeration oracle survives the #8 fix
//! for an UNAUTHENTICATED caller.
//!
//! # The rule
//!
//! SPEC.md §10.4 (line 1427): "for a fixed caller and authentication context, the
//! observable denial — its class AND any diagnostic code — for a name that does
//! not exist MUST be identical to that for a name that exists but is not granted
//! to that caller." A runtime "evaluates role membership before revealing whether
//! a named surface or call exists, so a caller who is not a member of the targeted
//! role cannot enumerate that role's surface catalog."
//!
//! # What the #8 fix (b2a643a) covered — and what it did not
//!
//! `authz.rs::a_non_member_cannot_distinguish_...` and
//! `redteam_enum_oracle_argshape.rs` both fix the caller/context to an
//! AUTHENTICATED non-member (bob: a resolving session + account, but disabled, so
//! `members_view` excludes him). For that caller `resolve_call`
//! (crates/liasse-surface/src/host/call.rs:186) passes `call_selection` (step 2 —
//! bob has a stored context) and fails at `authorize_role` membership (step 3),
//! which returns the uniform `unresolved` denial for BOTH an existing-ungranted
//! surface and a nonexistent one. That axis is closed.
//!
//! The UNAUTHENTICATED axis is not. Fix the caller/context to an ANONYMOUS
//! connection (`hello`, no authenticator). `resolve_call`'s Role arm checks role
//! EXISTENCE first (step 1, call.rs:207) and only then runs `call_selection`
//! (step 2, call.rs:209):
//!
//! - `member.tasks.complete` — role `member` EXISTS, so step 1 passes and step 2
//!   fails (no authenticated actor) → `DenialReason::Unauthenticated` → wire code
//!   `unauthenticated` (crates/liasse-connect/src/encode.rs:166).
//! - `ghost.tasks.complete` — role `ghost` does NOT exist, so step 1 fails →
//!   `DenialReason::Unresolved` → wire code `unresolved` (encode.rs:165).
//!
//! Same class (`denied`), DIFFERENT wire code and message, purely as a function of
//! whether the named role exists — exactly the existence oracle §10.4 forbids. The
//! `DenialReason` doc (crates/liasse-surface/src/outcome.rs:126-128) explicitly
//! claims the auth-context reasons "fire before surface resolution — so they leak
//! no catalog either"; that safety argument is FALSE, because `Unauthenticated`
//! fires AFTER the step-1 role-existence check, so it is emitted only for existing
//! roles while nonexistent roles short-circuit to `Unresolved`.
//!
//! ## Impact
//!
//! An unauthenticated peer enumerates the set of exposed ROLE names: POST a call
//! to `<guess>.x.y` with no authenticator; `unauthenticated` means the role
//! `<guess>` exists, `unresolved` means it does not. No credential is needed.
//!
//! Every expectation is derived from §10.4 alone. The finding test asserts the
//! §10.4-required identity: it FAILS on the pre-fix host and holds once
//! `resolve_call`/`resolve_view` collapse the actor-required denial over a role
//! (unenumerable) target to the uniform `unresolved`. The controls isolate the leak
//! to the unauthenticated axis, prove the harness reads wire codes faithfully, keep
//! the shared `view`/`fetch` pipeline honest, and prove the collapse does not
//! over-hide the enumerable (public) path.

mod support;

use liasse_connect::Reply;
use liasse_wire::serde_json::json;
use liasse_wire::Outcome;
use support::{app, call, hello, hello_member, view_reply, view_request};

/// The observable `(class, code, message)` a §10.4 comparison must hold constant
/// between a nonexistent name and an existing-ungranted one.
fn observable(outcome: &Outcome) -> (&'static str, String, String) {
    match outcome {
        Outcome::Committed { .. } => ("committed", String::new(), String::new()),
        Outcome::Unchanged { .. } => ("unchanged", String::new(), String::new()),
        Outcome::Rejected { code, message } => ("rejected", code.as_str().to_owned(), message.clone()),
        Outcome::Denied { code, message } => ("denied", code.as_str().to_owned(), message.clone()),
        Outcome::Failed { .. } => ("failed", String::new(), String::new()),
        Outcome::Unknown => ("unknown", String::new(), String::new()),
    }
}

/// FINDING (fails today). Fixed caller/authentication context: an ANONYMOUS
/// connection. §10.4 requires the denial for a nonexistent name (role `ghost`) to
/// be identical — class AND diagnostic code — to the denial for an existing name
/// that is not granted (role `member`, which the anonymous caller cannot access).
/// The engine denies the existing role `unauthenticated` and the nonexistent role
/// `unresolved`, so the wire code reveals that `member` is a real role.
#[test]
fn an_anonymous_caller_cannot_distinguish_an_existing_role_from_a_nonexistent_one() {
    let mut core = app();
    let conn = hello(&mut core); // anonymous: no authenticator

    // A receiver-shaped payload — irrelevant here: an anonymous caller is refused
    // before any argument decode, so the payload cannot change the disposition.
    let payload = json!({ "id": "aaaaaaaa-0000-4000-8000-000000000001", "title": "y" });
    let existing = call(&mut core, &conn, "member.tasks.complete", payload.clone(), None);
    let ghost = call(&mut core, &conn, "ghost.tasks.complete", payload, None);

    let existing_obs = observable(&existing);
    let ghost_obs = observable(&ghost);
    assert_eq!(
        existing_obs.0, "denied",
        "an existing ungranted role call must be denied, got {existing:?}"
    );
    assert_eq!(ghost_obs.0, "denied", "a nonexistent role call must be denied, got {ghost:?}");
    // §10.4: class AND diagnostic code (and the sanitized message) MUST be identical
    // for a fixed caller/authentication context. They are not — the existing role
    // leaks `unauthenticated` while the nonexistent one is `unresolved`.
    assert_eq!(
        existing_obs.1, ghost_obs.1,
        "§10.4: the wire code MUST NOT reveal that role `member` exists while `ghost` does not \
         (existing={existing:?}, ghost={ghost:?})"
    );
    assert_eq!(
        existing_obs.2, ghost_obs.2,
        "§10.4: the sanitized message must be identical too (existing={existing:?}, ghost={ghost:?})"
    );
}

// ---------------------------------------------------------------------------
// PASSING CONTROLS — isolate the leak to the UNAUTHENTICATED axis and prove the
// observation is faithful.
// ---------------------------------------------------------------------------

/// CONTROL (passes): with the SAME anonymous caller, two DIFFERENT nonexistent
/// roles deny identically — the harness reads the wire code faithfully and the
/// nonexistent-name denial is stable. This rules out a harness artifact: the
/// finding above is a real existing-vs-nonexistent divergence, not observation
/// noise.
#[test]
fn control_two_nonexistent_roles_are_indistinguishable() {
    let mut core = app();
    let conn = hello(&mut core);
    let a = observable(&call(&mut core, &conn, "ghost.tasks.complete", json!({}), None));
    let b = observable(&call(&mut core, &conn, "phantom.other.thing", json!({}), None));
    assert_eq!(a, b, "two nonexistent role names must deny identically");
    assert_eq!(a.1, "unresolved", "a nonexistent role denies with the uniform `unresolved` code");
}

/// CONTROL (passes): an AUTHENTICATED non-member (bob, disabled account) is the
/// axis the #8 fix already closed. He passes `call_selection` and fails at
/// membership, so an existing-ungranted ROLE and a nonexistent ROLE both deny
/// `unresolved` — identical class, code, and message. This proves the leak is
/// specific to the unauthenticated caller: for an authenticated non-member the
/// role-existence axis is already hidden.
#[test]
fn control_authenticated_non_member_role_existence_is_hidden() {
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");
    let existing = observable(&call(&mut core, &conn, "member.tasks.complete", json!({}), None));
    let ghost = observable(&call(&mut core, &conn, "ghost.tasks.complete", json!({}), None));
    assert_eq!(
        existing, ghost,
        "an authenticated non-member must not distinguish an existing role from a nonexistent one"
    );
    assert_eq!(existing.1, "unresolved", "the fix routes an authenticated non-member to `unresolved`");
}

/// The `(class, code, message)` a subscription probe presents: a refusal is a
/// `denied`/`rejected` outcome, a served view opens. Reduces a `view` reply to the
/// same observable the `call` probes compare, so the shared `resolve_view` pipeline
/// is checked on the SAME §10.4 identity as the `call` finding.
fn view_observable(
    core: &mut liasse_connect::ConnectCore<liasse_store::MemoryStore>,
    conn: &liasse_wire::ConnectionToken,
    address: &str,
) -> (&'static str, String, String) {
    match view_reply(core, conn, view_request("probe", address, None)) {
        Reply::Outcome(outcome) => observable(&outcome),
        Reply::Opened { .. } => ("opened", String::new(), String::new()),
        other => panic!("a view probe never replies {other:?}"),
    }
}

/// CONTROL (fails pre-fix, passes after): the finding's `call` leak also lived on
/// the shared `resolve_view` pipeline the connect `view`/`fetch` handlers run. For
/// the SAME anonymous caller, a subscription over an EXISTING role view
/// (`member.owned`) and over a NONEXISTENT role (`phantom.owned`) MUST be
/// indistinguishable — class AND code. Pre-fix the existing role denied
/// `unauthenticated` and the nonexistent one `unresolved`; the shared remap makes
/// both the uniform `unresolved`. (The pre-existing #39 view test only compared two
/// surfaces UNDER THE SAME role, so it never exercised the role-existence axis.)
#[test]
fn control_anonymous_role_view_matches_a_nonexistent_role() {
    let mut core = app();
    let conn = hello(&mut core); // anonymous
    let existing = view_observable(&mut core, &conn, "member.owned");
    let ghost = view_observable(&mut core, &conn, "phantom.owned");
    assert_eq!(
        existing, ghost,
        "§10.4: an anonymous role subscription must not reveal the role exists \
         (existing={existing:?}, ghost={ghost:?})"
    );
    assert_eq!(
        existing.1, "unresolved",
        "an anonymous role view is the uniform unresolvable-name denial, got {existing:?}"
    );
}

/// CONTROL (passes): the collapse is scoped to UNENUMERABLE (role) targets and must
/// NOT over-hide a PUBLIC surface, which is enumerable via `manifest`. §10.2 forbids
/// a public surface from ever *requiring* an actor — a `$actor`/`$session` read in a
/// public program is rejected at load, and an indirect one faults at admission as a
/// `rejected`, never a `denied` — so no public target ever yields `unauthenticated`
/// to collapse in the first place; `unauthenticated` is preserved structurally for
/// an enumerable target (the remap stays predicated on authority) even though this
/// valid app can never load one. This control proves the legitimate public path is
/// untouched: an anonymous caller still fully invokes a public surface, and a
/// nonexistent public surface still denies the same `unresolved` the fix did not
/// change.
#[test]
fn control_public_surface_access_is_not_over_hidden() {
    let mut core = app();
    let conn = hello(&mut core); // anonymous

    // A public surface an anonymous caller is entitled to use is SERVED, not hidden.
    let served = observable(&call(&mut core, &conn, "public.intake.add", json!({ "title": "hi" }), None));
    assert_eq!(served.0, "committed", "an anonymous caller still invokes a public surface, got {served:?}");

    // A nonexistent public surface still denies the uniform `unresolved`: only a
    // role's actor-required denial is remapped, so a public denial is unchanged.
    let missing = observable(&call(&mut core, &conn, "public.ghost.add", json!({ "title": "hi" }), None));
    assert_eq!(missing.0, "denied", "a nonexistent public surface is denied, got {missing:?}");
    assert_eq!(
        missing.1, "unresolved",
        "a nonexistent public surface still denies `unresolved`, got {missing:?}"
    );
}
