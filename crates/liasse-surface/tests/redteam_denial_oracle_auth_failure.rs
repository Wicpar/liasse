//! RED TEAM — §10.4 role-existence enumeration oracle on the AUTHENTICATION-FAILURE
//! path of the `call` pipeline.
//!
//! §10.4 (verbatim MUST): "The denial MUST NOT reveal whether a surface of the
//! named address exists: for a fixed caller and authentication context, the
//! observable denial — its class and any diagnostic code — for a name that does
//! not exist MUST be identical to that for a name that exists but is not granted
//! to that caller." The exception is narrow: "Membership- or existence-specific
//! diagnostics are permitted only toward a caller that has already established
//! authority over the target."
//!
//! The impl already closes this oracle for the `Unauthenticated` reason
//! (`host/call.rs::hide_unenumerable_denial`, documented in `outcome.rs`): an
//! actor-required denial over a role target is collapsed to the uniform
//! `Unresolved`, precisely so an unauthenticated caller cannot enumerate the role
//! catalog by wire code. But the OTHER credential-verification reasons —
//! `Unverified` (forged credential) and `AuthenticatorNotAccepted` (wrong
//! authenticator name) — are passed through verbatim
//! (`authorize_role` -> `verify_selection`, `map_err(SurfaceOutcome::Denied)` with
//! no hiding). A caller who presents a forged credential, or names an authenticator
//! the role does not accept, has established NO authority over the target, yet the
//! distinct reason it receives for an existing role (vs the `Unresolved` a
//! nonexistent role gives) reveals role existence — the very oracle §10.4 forbids
//! and the very oracle the impl explicitly closed for `Unauthenticated`.
//!
//! Root cause: `crates/liasse-surface/src/host/call.rs`, `resolve_call` — the
//! `verify_selection` failure (auth acceptance + `$verify`) fires before membership
//! and is not remapped by `hide_unenumerable_denial`, unlike the `call_selection`
//! (`Unauthenticated`) denial one line above it.
//!
//! Expectations are deducible from SPEC.md §10.4 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_surface::{AuthSelection, Credential, DenialReason, SurfaceCall, SurfaceOutcome, Value};
use support::{address, args, host, text};

/// The denial reason of a call outcome, or a panic naming what happened. Every
/// §10/§11 refusal on the `call` pipeline is a `Denied` outcome.
fn denial_reason(outcome: &SurfaceOutcome) -> DenialReason {
    match outcome.denial() {
        Some(denial) => denial.reason(),
        None => panic!("expected a denial, got {outcome:?}"),
    }
}

/// Probe a `<role>.tasks.complete` address on a fresh host with a fixed
/// authenticator selection, returning the denial reason the caller observes. The
/// arguments are irrelevant: resolution/authorization deny before any admission,
/// so they never reach `build_request`.
fn probe(role: &str, selection: AuthSelection) -> DenialReason {
    let mut host = host();
    host.connect("c1").unwrap();
    let call = SurfaceCall::new(
        address(&format!("{role}.tasks.complete")),
        args([("id", text("x")), ("title", text("y"))]),
    )
    .with_auth(selection);
    let outcome = host.call("c1", &call).expect("call runs");
    denial_reason(&outcome)
}

/// FINDING (§10.4) — a FORGED credential distinguishes an existing role from a
/// nonexistent one by diagnostic code.
///
/// A single fixed caller presents the identical forged credential (`token`,
/// non-text bytes that `$verify` rejects) to two addresses whose only difference
/// is the role name: `member` (exists) vs `ghost` (does not). §10.4 requires the
/// two denials to be identical in class AND diagnostic code, because this caller
/// has established no authority over either target. The impl returns `Unverified`
/// for the existing role and `Unresolved` for the nonexistent one — an oracle:
/// the caller learns that `member` is a real role.
#[test]
fn forged_credential_does_not_leak_role_existence() {
    let forged = || AuthSelection::new("token", Credential::new(Value::Bool(true)));
    let existing = probe("member", forged());
    let nonexistent = probe("ghost", forged());

    assert_eq!(
        existing, nonexistent,
        "§10.4: a forged-credential caller must see the IDENTICAL diagnostic code \
         for an existing role and a nonexistent one, or role existence leaks — \
         member={existing:?} ghost={nonexistent:?}",
    );
}

/// FINDING (§10.4) — naming an authenticator the role does not accept distinguishes
/// an existing role from a nonexistent one by diagnostic code.
///
/// The caller names `api` (a declared authenticator the `member` role does not
/// accept — it accepts only `token`). §10.4 again requires identical denials. The
/// impl returns `AuthenticatorNotAccepted` for `member` (the acceptance check runs
/// only after the role is resolved) and `Unresolved` for `ghost` — the same oracle.
#[test]
fn unaccepted_authenticator_does_not_leak_role_existence() {
    let selection = || AuthSelection::new("api", Credential::new(text("alice")));
    let existing = probe("member", selection());
    let nonexistent = probe("ghost", selection());

    assert_eq!(
        existing, nonexistent,
        "§10.4: naming an unaccepted authenticator must deny with the IDENTICAL \
         diagnostic code for an existing role and a nonexistent one — \
         member={existing:?} ghost={nonexistent:?}",
    );
}

/// PASSING CONTROL — once authentication SUCCEEDS, the oracle is correctly closed.
///
/// `s_bob` is a live session whose account (`bob`) is disabled, so the credential
/// verifies and `$actor`/`$session` resolve (authentication succeeds), but `bob`
/// is not a member of `member` (membership excludes disabled accounts, §10.3). The
/// caller therefore holds a real authentication context but no authority over the
/// target. §10.4 requires the existing-role and nonexistent-role denials to match,
/// and here they DO: both are `Unresolved`. This isolates the finding above — the
/// leak is specific to the authentication-FAILURE path, and vanishes the moment
/// membership (rather than `$verify`/acceptance) is the reason.
#[test]
fn authenticated_nonmember_denials_are_uniform() {
    let selection = || AuthSelection::new("token", Credential::new(text("s_bob")));
    let existing = probe("member", selection());
    let nonexistent = probe("ghost", selection());

    assert_eq!(existing, DenialReason::Unresolved, "an authenticated non-member denies `unresolved`");
    assert_eq!(
        existing, nonexistent,
        "an authenticated non-member sees the same uniform denial for an existing \
         and a nonexistent role (control) — member={existing:?} ghost={nonexistent:?}",
    );
}
