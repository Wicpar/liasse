//! RED TEAM — §11.4 public-address-with-authenticator-selection on the `view`
//! (subscription) pipeline.
//!
//! §11.4 (verbatim MUST): "Public surfaces use their public address and carry no
//! authenticator selection. A request to a public address that nonetheless carries
//! an authenticator selection or credential is malformed and is rejected — the
//! runtime does not drop the selection and serve the request actor-less, and it
//! refuses the request before verifying the attached credential."
//!
//! This rule binds every external request to a public address (§12.1 lists `view`
//! alongside `call`). The `call` pipeline honours it: `resolve_call`'s
//! `Authority::Public` arm rejects a call carrying `auth` as malformed
//! (`SurfaceOutcome::Rejected`), pinned by the corpus case
//! `11-auth-sessions/red/public-surface-authenticator-selection`.
//!
//! The `view`/`watch` pipeline does NOT. `resolve_view`'s `Authority::Public` arm
//! (`crates/liasse-surface/src/host/call.rs`) ignores the `selection` argument
//! entirely — it resolves the public view and returns it — so a `SurfaceWatch`
//! carrying `.with_auth(...)` over a public address is SERVED (an `Init`
//! subscription), exactly the "drop the selection and serve the request
//! actor-less" behaviour the spec forbids. The extraneous credential is silently
//! ignored rather than making the request malformed.
//!
//! A conforming runtime must refuse such a subscription (never open it). This test
//! asserts the watch is not served; the impl opens it, so the assertion fails —
//! that is the finding.
//!
//! Expectations are deducible from SPEC.md §11.4 alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_surface::{AuthSelection, Credential, Subscription, SurfaceWatch};
use support::{address, host, text};

/// CONTROL — a public `view` with NO authenticator selection opens normally. This
/// establishes the address and view are live, so the finding below is a boundary
/// violation, not a dead surface.
#[test]
fn public_view_without_auth_opens() {
    let mut host = host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.tasks"), "w0");
    match host.watch("c1", &watch).expect("watch runs") {
        Subscription::Init(_) => {}
        other => panic!("a plain public view must open: {other:?}"),
    }
}

/// FINDING (§11.4) — a public `view` carrying an authenticator selection is served
/// actor-less instead of being refused as malformed.
///
/// The subscription attaches a real, valid session credential (`s_alice`) to the
/// public `public.tasks` view. §11.4 makes the extraneous selection malformed: the
/// runtime must refuse the request and must NOT drop the selection and serve it.
/// A conforming outcome is therefore any refusal (`Denied` / `Failed`) — never a
/// served subscription. The impl returns `Subscription::Init`, having ignored the
/// credential and served the public view, which is exactly the forbidden
/// "drop the selection and serve the request actor-less" behaviour.
#[test]
fn public_view_carrying_auth_is_refused() {
    let mut host = host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.tasks"), "w1")
        .with_auth(AuthSelection::new("token", Credential::new(text("s_alice"))));
    let outcome = host.watch("c1", &watch).expect("watch runs");

    assert!(
        !matches!(outcome, Subscription::Init(_) | Subscription::Window(_)),
        "§11.4: a public-address subscription carrying an authenticator selection is \
         malformed and MUST be refused, not served actor-less — got {outcome:?}",
    );
}

/// FINDING (§11.4) — the refusal must also precede credential verification, so a
/// public view carrying a FORGED credential is likewise malformed, not served.
///
/// §11.4: the runtime "refuses the request before verifying the attached
/// credential." A forged credential (`Value::Bool` — not a token) attached to a
/// public view must not cause verification and must not be served; the request is
/// malformed on the mere presence of a selection. The impl serves the public view
/// regardless (the selection is never inspected on this path), so the request is
/// wrongly served rather than refused.
#[test]
fn public_view_carrying_forged_auth_is_refused() {
    let mut host = host();
    host.connect("c1").unwrap();
    let watch = SurfaceWatch::new(address("public.tasks"), "w2")
        .with_auth(AuthSelection::new("token", Credential::new(liasse_surface::Value::Bool(true))));
    let outcome = host.watch("c1", &watch).expect("watch runs");

    assert!(
        !matches!(outcome, Subscription::Init(_) | Subscription::Window(_)),
        "§11.4: a public-address subscription carrying a (forged) credential is \
         malformed and MUST be refused before verification, not served — got {outcome:?}",
    );
}
