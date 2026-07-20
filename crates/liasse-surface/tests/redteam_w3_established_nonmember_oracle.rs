//! RED TEAM (Wave 3) — §10.4 role-existence enumeration oracle through the
//! `established` predicate for a caller that AUTHENTICATED to a role it is NOT a
//! member of.
//!
//! The wave-2 fix (commit 69f8242) scopes §10.4's denial-hiding exception to the
//! role a bound connection context authenticated against:
//!
//! ```ignore
//! let established = call.auth().is_none()
//!     && self.connections.get(id).and_then(|c| c.context_role(call.context()))
//!         == Some(role.as_str());
//! ```
//!
//! That closes the CROSS-role leak (established over `alpha`, probe `beta`). But
//! it keys establishment on the role NAMED at `authenticate`, recorded by
//! `SurfaceHost::authenticate` -> `Connection::set_context` — and `authenticate`
//! binds that context whenever the selection merely VERIFIES (the role accepts the
//! authenticator and the credential resolves an actor/session), *without* checking
//! role membership (`host/mod.rs::verify_selection`: "Membership is *not* checked
//! here"). So a caller whose actor is NOT a member of the role — a disabled
//! account whose session still resolves — is nonetheless recorded with
//! `context_role == "member"`, and the fix then reads it back as
//! `established == true` for that role.
//!
//! §10.4 forbids exactly this. Its FIRST sentence is a flat MUST scoped to
//! non-members:
//!
//!   "A runtime therefore evaluates role membership before revealing whether a
//!    named surface or call exists, so a caller who is not a member of the
//!    targeted role cannot enumerate that role's surface catalog."
//!
//! and the guarantee it enforces:
//!
//!   "the observable denial — its class and any diagnostic code — for a name that
//!    does not exist MUST be identical to that for a name that exists but is not
//!    granted to that caller."
//!
//! The exception (second-to-last sentence) is for "a caller that has already
//! established authority over the target" — i.e. a MEMBER/holder of the role, the
//! subject of the immediately preceding sentence. A non-member has NOT established
//! authority over the role, so the exception cannot apply to it; the flat MUST
//! governs and the two denials must be identical.
//!
//! Concretely: `bob` is a disabled account (excluded from `members_view`, so a
//! non-member of role `member`) whose live session `s_bob` still resolves an
//! actor. `bob` authenticates the default context to `member` (verifies -> `Bound`;
//! membership never checked), so `context_role("default") == "member"` and the
//! fix's `established` is TRUE for `member`. His session is then revoked. Probing
//! `member.tasks` now denies `session-invalid` (established -> NOT hidden) while a
//! nonexistent role `ghost.tasks` denies `unresolved`: the two differ in
//! diagnostic code, so a NON-MEMBER learns `member` is a real role — the enumeration
//! §10.4's first sentence forbids for a non-member.
//!
//! Root cause: `crates/liasse-surface/src/host/call.rs` `resolve_call` (line ~235)
//! and `resolve_view` (line ~713) derive `established` from `context_role == role`
//! alone; `context_role` is set by `authenticate` on mere verification, never
//! gated on membership. "Established authority over the target" (§10.4) is
//! membership, not "named this role at authenticate".
//!
//! Every expectation is hand-derived from SPEC.md §10.4 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_surface::{
    AuthResult, AuthSelection, Authenticate, Credential, DenialReason, Subscription, SurfaceCall,
    SurfaceHost, SurfaceWatch,
};
use liasse_store::MemoryStore;
use support::{address, args, call, host, text};

/// The denial reason of a bound-context call outcome, or a panic naming what
/// happened.
fn call_denial(host: &mut SurfaceHost<MemoryStore>, address_text: &str) -> DenialReason {
    let call = SurfaceCall::new(
        address(address_text),
        args([("id", text("x")), ("title", text("y"))]),
    );
    let outcome = host.call("c1", &call).expect("call runs");
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial for `{address_text}`, got {outcome:?}"),
    }
}

/// The denial reason of a call outcome carrying a PER-REQUEST `auth` selection
/// (an unestablished, fresh probe — no bound context is consulted).
fn call_denial_per_request(
    host: &mut SurfaceHost<MemoryStore>,
    address_text: &str,
    credential: &str,
) -> DenialReason {
    let call = SurfaceCall::new(
        address(address_text),
        args([("id", text("x")), ("title", text("y"))]),
    )
    .with_auth(AuthSelection::new("token", Credential::new(text(credential))));
    let outcome = host.call("c1", &call).expect("call runs");
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial for `{address_text}`, got {outcome:?}"),
    }
}

/// The denial reason of a bound-context `watch` over `<address>`, or a panic.
fn watch_denial(host: &mut SurfaceHost<MemoryStore>, address_text: &str) -> DenialReason {
    let watch = SurfaceWatch::new(address(address_text), "w1");
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Denied(denial) => denial.reason(),
        other => panic!("expected a denial for `{address_text}`, got {other:?}"),
    }
}

/// Connect `c1`, authenticate the default context to role `member` with the live
/// session `s_bob` — whose account `bob` is DISABLED, so `bob` is NOT a member of
/// `member` (`members_view` = enabled accounts). `authenticate` verifies the
/// selection (the role accepts `token`, the session resolves an actor) and binds
/// the context WITHOUT checking membership, so `context_role("default") ==
/// "member"` even though `bob` holds no authority over the role.
fn host_established_nonmember() -> SurfaceHost<MemoryStore> {
    let mut host = host();
    host.connect("c1").unwrap();
    let request = Authenticate::new(
        "member",
        AuthSelection::new("token", Credential::new(text("s_bob"))),
    );
    assert!(
        matches!(host.authenticate("c1", &request).expect("authenticate"), AuthResult::Bound),
        "a non-member whose session resolves still binds the context (membership is \
         not checked at authenticate)",
    );
    host
}

/// Revoke session `s_bob` through the public `session.revoke` surface, so a later
/// request using it fails authentication (§11.7). This commits on `c1`.
fn revoke_s_bob(host: &mut SurfaceHost<MemoryStore>) {
    let outcome = host
        .call("c1", &call("public.session.revoke", [("id", text("s_bob"))]))
        .expect("revoke runs");
    assert!(
        matches!(outcome, liasse_surface::SurfaceOutcome::Committed { .. }),
        "revoking s_bob commits: {outcome:?}",
    );
}

/// FINDING (§10.4) — a caller ESTABLISHED (per the fix) over role `member` but who
/// is NOT a member of it leaks the role's existence on the `call` path once its
/// session goes invalid.
///
/// The single fixed caller (bound "default" context recorded against `member`, but
/// a non-member: disabled account `bob`) probes two addresses differing only in the
/// role name after its session is revoked: `member.tasks.complete` (a real role the
/// caller is NOT a member of) vs `ghost.tasks.complete` (no such role). §10.4's
/// first sentence requires a NON-MEMBER's two denials to be identical in class AND
/// diagnostic code. The impl denies `member` with `session-invalid` (its
/// `established` predicate is true because the context named `member` at
/// authenticate, so the reason is not hidden) and `ghost` with `unresolved`: an
/// oracle revealing `member` is real to a non-member.
#[test]
fn established_nonmember_leaks_role_existence_on_call() {
    let mut host = host_established_nonmember();
    revoke_s_bob(&mut host);

    let member = call_denial(&mut host, "member.tasks.complete");
    let ghost = call_denial(&mut host, "ghost.tasks.complete");

    assert_eq!(
        member, ghost,
        "§10.4: a NON-MEMBER of `member` (its exception is for established authority = \
         a member) must see the IDENTICAL diagnostic code for the existing-but-ungranted \
         role `member` and the nonexistent role `ghost` — member={member:?} ghost={ghost:?}",
    );
}

/// FINDING (§10.4) — the SAME oracle on the `watch` (subscription) pipeline.
///
/// `resolve_view` carries the identical `established = inline.is_none() &&
/// context_role(context) == Some(role)` predicate as `resolve_call`, so the
/// established-non-member leak is not call-specific: a bound-context subscription on
/// `member.tasks` denies `session-invalid` while `ghost.tasks` denies `unresolved`,
/// violating §10.4's identical-code requirement for a non-member on the
/// view/watch/resume path too.
#[test]
fn established_nonmember_leaks_role_existence_on_watch() {
    let mut host = host_established_nonmember();
    revoke_s_bob(&mut host);

    let member = watch_denial(&mut host, "member.tasks");
    let ghost = watch_denial(&mut host, "ghost.tasks");

    assert_eq!(
        member, ghost,
        "§10.4: a bound-context subscription probe by a NON-MEMBER of an \
         existing-but-ungranted role `member` and a nonexistent role `ghost` must deny \
         with the IDENTICAL diagnostic code — member={member:?} ghost={ghost:?}",
    );
}

/// PASSING CONTROL — the SAME revoked session probed with a PER-REQUEST `auth`
/// selection (no bound context, `established = false`) is correctly uniform.
///
/// Here the caller attaches the revoked `s_bob` credential inline to each call
/// instead of relying on a stored context, so `call.auth().is_some()` and the fix's
/// `established` is false. The §10.4 remap therefore collapses the `session-invalid`
/// reason to `unresolved` for the existing role, matching the nonexistent one. This
/// isolates the defect to the bound-context establishment predicate: the ONLY
/// difference from the failing `call` test is establishment, and this path is
/// already correct.
#[test]
fn per_request_nonmember_probe_is_uniform_control() {
    let mut host = host();
    host.connect("c1").unwrap();
    revoke_s_bob(&mut host);

    let member = call_denial_per_request(&mut host, "member.tasks.complete", "s_bob");
    let ghost = call_denial_per_request(&mut host, "ghost.tasks.complete", "s_bob");

    assert_eq!(
        member, DenialReason::Unresolved,
        "a fresh per-request probe of `member` with a revoked session denies `unresolved`",
    );
    assert_eq!(
        member, ghost,
        "control: a per-request-auth probe sees the uniform denial for an existing and a \
         nonexistent role — member={member:?} ghost={ghost:?}",
    );
}

/// PASSING CONTROL — the established non-member with a STILL-VALID session is
/// correctly uniform, because the failure is MEMBERSHIP (already the uniform
/// `unresolved` in `authorize_role`), not authentication.
///
/// Without the revoke, probing `member.tasks` runs the full authenticate ->
/// membership pipeline: the session resolves, then membership fails (`bob` disabled)
/// and denies the uniform `unresolved` regardless of `established`. So an established
/// non-member is correctly hidden on the MEMBERSHIP path; the leak in the failing
/// tests is specific to the authentication-FAILURE path that `established` exposes.
#[test]
fn established_nonmember_membership_path_is_uniform_control() {
    let mut host = host_established_nonmember();
    // No revoke: s_bob is still live, so authentication succeeds and the pipeline
    // reaches the membership check.
    let member = call_denial(&mut host, "member.tasks.complete");
    let ghost = call_denial(&mut host, "ghost.tasks.complete");

    assert_eq!(
        member, DenialReason::Unresolved,
        "an established non-member with a valid session is denied `unresolved` at membership",
    );
    assert_eq!(
        member, ghost,
        "control: the membership-path denial is uniform for an existing and a nonexistent \
         role — member={member:?} ghost={ghost:?}",
    );
}
