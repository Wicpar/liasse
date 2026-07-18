//! RED-TEAM finding — SPEC-ISSUES #8 enumeration leak via argument-shape ordering.
//!
//! # CONFIRMED BUG
//!
//! §10.4 (SPEC.md) and the #8 resolution require that a denial "MUST NOT reveal
//! whether a surface of the named address exists": an ungranted-but-existing role
//! call and a nonexistent role call must be INDISTINGUISHABLE to a non-member.
//! The existing control (`authz.rs::a_non_member_cannot_distinguish_...`) proves
//! this for an EMPTY argument object. It does NOT hold once the probe carries a
//! non-empty, declared-arg-shaped payload.
//!
//! ## Root cause
//!
//! At the connect boundary the closed-shape argument decode runs BEFORE the call
//! is dispatched to the host (where §10.3 membership is evaluated):
//! `decode::decode_args(self.schema.call_args(address), …)` at
//! crates/liasse-connect/src/core/live.rs:117 returns a `Rejected`(malformed)
//! outcome for any argument the address's declared contract does not list, and
//! only then does line 142 call `self.host.call(...)` — which is where a
//! non-member is `Denied` (crates/liasse-surface/src/host/call.rs:176-188,
//! resolving membership before the surface/call binding's existence is revealed).
//!
//! For an EXISTING role call the schema carries the real contract, so a
//! declared-arg-shaped payload decodes cleanly and reaches the host, which
//! `Denied`s the non-member. For a NONEXISTENT call the schema has no contract
//! (`call_args` returns the empty slice, crates/liasse-connect/src/mount.rs), so
//! the SAME payload's argument names are "unknown" and the boundary returns
//! `Rejected`(malformed) BEFORE authorization ever runs. The two responses differ
//! by outcome CLASS (`Denied` vs `Rejected`) purely as a function of whether the
//! named surface/call exists — exactly the existence oracle #8 forbids.
//!
//! ## Impact
//!
//! A non-member enumerates a role's surface/call catalog: send any plausible
//! declared-arg payload (a receiver key like `{ "id": … }` is universal) to
//! `role.<guess>.<call>`; a `Denied` means the call exists (and accepts that
//! argument), a `Rejected` means it does not. The empty-arg path is closed; this
//! non-empty path is not.
//!
//! The assertion below states the #8-required property (the two probes are
//! indistinguishable in class) and FAILS today.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_wire::serde_json::json;
use liasse_wire::Outcome;
use support::{app, call, hello_member};

/// The outcome class token, so an existing-vs-nonexistent comparison ignores the
/// (sanitized) message and compares only the observable class §10.4 pins.
fn class(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Committed { .. } => "committed",
        Outcome::Unchanged { .. } => "unchanged",
        Outcome::Rejected { .. } => "rejected",
        Outcome::Denied { .. } => "denied",
        Outcome::Failed { .. } => "failed",
        Outcome::Unknown => "unknown",
    }
}

#[test]
fn a_non_member_cannot_distinguish_via_a_declared_arg_shaped_probe() {
    // bob authenticates as `member` (his session + account resolve) but is not a
    // member — his account is disabled, so `members_view` excludes him. The probe
    // carries a payload shaped like the REAL `member.tasks.complete` contract
    // (`rename` takes `id` + `title`), so an existing call decodes cleanly and is
    // denied at membership, while a nonexistent call is rejected as malformed at
    // the boundary before authorization.
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");

    // The receiver key `id` of `member.tasks.complete` (bound to `rename`) is a
    // `uuid`, so the probe supplies a VALIDLY-decoding uuid: the existing call
    // then clears the closed-shape decode and reaches the host (which denies the
    // non-member), while the nonexistent call's empty contract rejects the same
    // argument names as unknown at the boundary, before authorization.
    let payload = json!({ "id": "aaaaaaaa-0000-4000-8000-000000000001", "title": "y" });
    let existing = call(&mut core, &conn, "member.tasks.complete", payload.clone(), None);
    let ghost = call(&mut core, &conn, "member.ghost.complete", payload, None);

    // §10.4 / #8: both probes MUST be indistinguishable in outcome class, so a
    // non-member cannot tell that `tasks` exists but `ghost` does not. Today the
    // existing call is `denied` and the ghost is `rejected` (malformed) — the
    // classes differ, revealing existence.
    assert_eq!(
        class(&existing),
        class(&ghost),
        "enumeration oracle: an existing ungranted role call ({existing:?}) is distinguishable \
         from a nonexistent one ({ghost:?}) by outcome class when the probe carries a \
         declared-arg-shaped payload (§10.4, SPEC-ISSUES #8)"
    );
}

/// CONTROL (passes): the empty-argument path is the one the existing suite
/// covers; both probes deny identically there, because an empty object clears the
/// closed-shape decode for BOTH the real and the empty contract and both reach
/// the host's membership denial. Included here so the finding is precisely scoped
/// to the non-empty-payload path.
#[test]
fn control_empty_arg_probes_are_indistinguishable() {
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");

    let existing = call(&mut core, &conn, "member.tasks.complete", json!({}), None);
    let ghost = call(&mut core, &conn, "member.ghost.complete", json!({}), None);

    assert_eq!(
        class(&existing),
        class(&ghost),
        "empty-arg probes must be indistinguishable (control): {existing:?} vs {ghost:?}"
    );
    assert_eq!(class(&existing), "denied", "a non-member is denied, not rejected, on the empty path");
}
