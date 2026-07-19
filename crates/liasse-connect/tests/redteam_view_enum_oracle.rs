//! RED-TEAM finding — GitHub #39 enumeration leak via view-params ordering.
//!
//! # CONFIRMED BUG
//!
//! §10.4 (SPEC.md) and §12.1 require that a subscription refusal "MUST NOT reveal
//! whether a surface of the named address exists": an ungranted-but-existing role
//! `view` and a nonexistent role `view` MUST be INDISTINGUISHABLE to a non-member.
//! The `call` pipeline was fixed for exactly this oracle (item #8, `authorize_call`
//! runs the resolution/membership probe BEFORE the closed-shape argument decode).
//! The SAME decode-before-authz ordering was still latent in connect's `view` and
//! `fetch` handlers, which decoded the closed-shape `$params` (§10.1) BEFORE the
//! subscription reached the host's authorization.
//!
//! ## Root cause
//!
//! At the connect boundary the closed-shape view-params decode ran BEFORE the watch
//! was dispatched to the host (where §10.3 membership is evaluated):
//! `decode::decode_args(self.schema.view_params(address), …)` returned a
//! `Rejected`(malformed) outcome for any parameter the address's declared contract
//! does not list, and only then did `self.host.watch(...)` run — which is where a
//! non-member is `Denied`.
//!
//! For an EXISTING parameterized role view the schema carries the real `$params`
//! contract, so a declared-param-shaped payload decodes cleanly and reaches the
//! host, which `Denied`s the non-member. For a NONEXISTENT view the schema has no
//! contract (`view_params` returns the empty slice), so the SAME parameter names
//! are "unknown" and the boundary returns `Rejected`(malformed) BEFORE
//! authorization ever runs. The two responses differ by outcome CLASS (`Denied` vs
//! `Rejected`) purely as a function of whether the named surface view exists —
//! exactly the existence oracle §10.4/§12.1 forbid.
//!
//! It is NOT demonstrable with a parameter-free role view: an empty (or absent)
//! params object clears the closed-shape decode for BOTH the real and the empty
//! contract, so both reach the host's membership denial. The fixture therefore
//! declares a PARAMETERIZED role view (`member.owned`, `$params: { owner }`) to
//! carry a valid-param probe down the non-empty path.
//!
//! ## The property
//!
//! The headline assertions state the §10.4/§12.1-required property (existing and
//! nonexistent probes are indistinguishable in class AND wire code). They FAIL on
//! the pre-fix boundary and hold once the `view`/`fetch` handlers authorize first.
//! The controls scope the finding: the empty-param path was already uniform, and an
//! AUTHORIZED caller still gets the closed-shape `rejected` for an unknown parameter
//! (the #6/#10 reveal is preserved for a caller that has established authority).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_connect::{ConnectCore, Reply};
use liasse_store::MemoryStore;
use liasse_wire::serde_json::{Value as Json, json};
use liasse_wire::{ConnectionToken, Outcome, Sub, Upstream};

use support::{app, hello, hello_member};

/// The externally observable shape of a probe reply: whether it was served, and if
/// refused, the refusal class plus its stable wire code — the §10.4/§12.1 observables
/// a caller can read. The sanitized message is deliberately excluded: §10.4 pins the
/// class and the stable code, and a leak hides in exactly those, never in the prose.
#[derive(Debug, PartialEq, Eq)]
enum Observed {
    /// The read was served — an opened subscription (`view`) or a value (`fetch`).
    Served,
    /// The read was refused with this outcome class and stable wire code.
    Refused { class: &'static str, code: String },
}

impl Observed {
    /// Reduce a boundary reply to what a client can observe about it.
    fn of(reply: Reply) -> Self {
        match reply {
            Reply::Opened { .. } | Reply::Fetched(_) => Self::Served,
            Reply::Outcome(outcome) => Self::Refused { class: class(&outcome), code: code(&outcome) },
            other => panic!("a view/fetch probe never replies {other:?}"),
        }
    }
}

/// The outcome class token §10.4 pins, so an existing-vs-nonexistent comparison
/// compares the observable class rather than the (sanitized) message.
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

/// The stable wire code of a refusal (the taxonomy token behind the class), so the
/// property proves the two probes are identical to the byte, not merely same-class.
fn code(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Rejected { code, .. } | Outcome::Denied { code, .. } => code.as_str().to_owned(),
        Outcome::Failed { code } => format!("{code:?}"),
        _ => String::new(),
    }
}

/// Open a `view` over `address` carrying `params`, returning what the client observes.
fn probe_view(
    core: &mut ConnectCore<MemoryStore>,
    conn: &ConnectionToken,
    address: &str,
    params: Option<Json>,
) -> Observed {
    let frame = Upstream::View {
        sub: Sub::new("probe"),
        address: address.to_owned(),
        params,
        window: None,
        auth: None,
        context: None,
    };
    match core.submit(Some(conn), None, frame) {
        Ok(reply) => Observed::of(reply),
        Err(error) => panic!("view probe faulted: {error:?}"),
    }
}

/// Read `address` once with `fetch`, carrying `params`, returning the observation.
fn probe_fetch(
    core: &mut ConnectCore<MemoryStore>,
    conn: &ConnectionToken,
    address: &str,
    params: Option<Json>,
) -> Observed {
    let frame = Upstream::Fetch { address: address.to_owned(), params };
    match core.submit(Some(conn), None, frame) {
        Ok(reply) => Observed::of(reply),
        Err(error) => panic!("fetch probe faulted: {error:?}"),
    }
}

#[test]
fn a_non_member_cannot_distinguish_a_role_view_by_a_valid_param_probe() {
    // bob authenticates as `member` (his session + account resolve) but is NOT a
    // member — his account is disabled, so `members_view` excludes him. The probe
    // carries a payload shaped like the REAL `member.owned` `$params` (`owner: text`),
    // so the existing view decodes cleanly and is denied at membership, while a
    // nonexistent view's empty contract rejects the same name as unknown at the
    // boundary before authorization.
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");

    let param = json!({ "owner": "alice" });
    let existing = probe_view(&mut core, &conn, "member.owned", Some(param.clone()));
    let ghost = probe_view(&mut core, &conn, "member.ghost", Some(param));

    // §10.4 / §12.1 / #39: both probes MUST be indistinguishable in class AND wire
    // code, so a non-member cannot tell that `owned` exists but `ghost` does not.
    // Pre-fix the existing view is `denied`/`unresolved` and the ghost is
    // `rejected`/`malformed` — the classes differ, revealing existence.
    assert_eq!(
        existing, ghost,
        "enumeration oracle: an existing ungranted role view ({existing:?}) is distinguishable \
         from a nonexistent one ({ghost:?}) when the probe carries a valid declared-param payload \
         (§10.4, §12.1, GitHub #39)"
    );
    assert_eq!(
        existing,
        Observed::Refused { class: "denied", code: "unresolved".to_owned() },
        "a non-member's parameterized role-view probe is the uniform unresolvable-name denial"
    );
}

#[test]
fn a_non_member_cannot_distinguish_a_role_view_by_fetch() {
    // `fetch` runs over a fresh unauthenticated scratch connection, so every role
    // view is refused there — but pre-fix the closed-shape params decode still ran
    // first, so an existing view (valid param decodes, then unauthenticated at the
    // host) and a nonexistent one (unknown param, rejected at the boundary) diverged
    // by class. The reorder makes both the uniform unauthenticated denial.
    let mut core = app();
    let conn = hello(&mut core);

    let param = json!({ "owner": "alice" });
    let existing = probe_fetch(&mut core, &conn, "member.owned", Some(param.clone()));
    let ghost = probe_fetch(&mut core, &conn, "member.ghost", Some(param));

    assert_eq!(
        existing, ghost,
        "fetch enumeration oracle: an existing role view ({existing:?}) is distinguishable from a \
         nonexistent one ({ghost:?}) via a valid declared-param payload (§10.4, §12.1, GitHub #39)"
    );
    assert_eq!(
        existing,
        Observed::Refused { class: "denied", code: "unauthenticated".to_owned() },
        "a role-view fetch is the uniform unauthenticated denial, not a params rejection"
    );
}

#[test]
fn control_empty_param_probes_are_indistinguishable() {
    // CONTROL (passes before and after the fix): an empty (absent) params payload
    // clears the closed-shape decode for BOTH the real and the empty contract, so
    // both probes reach the host's membership denial. Included so the finding is
    // precisely scoped to the non-empty-payload path.
    let mut core = app();
    let conn = hello_member(&mut core, "s_bob");

    let existing = probe_view(&mut core, &conn, "member.owned", None);
    let ghost = probe_view(&mut core, &conn, "member.ghost", None);

    assert_eq!(existing, ghost, "empty-param probes must be indistinguishable (control)");
    assert_eq!(
        existing,
        Observed::Refused { class: "denied", code: "unresolved".to_owned() },
        "a non-member is denied, not rejected, on the empty-param path"
    );
}

#[test]
fn an_authorized_member_still_rejects_an_unknown_view_param() {
    // CONTROL (the #6/#10 closed-shape reveal must be preserved): alice IS a member
    // (her account is enabled), so she has established authority over `member.owned`.
    // An unknown parameter is then a genuine closed-shape `rejected`(malformed) —
    // the fix must NOT collapse an authorized caller's malformed request into a
    // denial.
    let mut core = app();
    let conn = hello_member(&mut core, "s_alice");

    let observed = probe_view(&mut core, &conn, "member.owned", Some(json!({ "bogus": "x" })));
    assert_eq!(
        observed,
        Observed::Refused { class: "rejected", code: "malformed".to_owned() },
        "an authorized member's unknown view param is still the closed-shape rejection (§10, #10)"
    );
}

#[test]
fn an_authorized_member_opens_the_parameterized_role_view() {
    // CONTROL (authorized access must survive the reorder): alice is a member and
    // supplies a valid `owner` parameter, so the parameterized role view opens.
    let mut core = app();
    let conn = hello_member(&mut core, "s_alice");

    let observed = probe_view(&mut core, &conn, "member.owned", Some(json!({ "owner": "alice" })));
    assert_eq!(observed, Observed::Served, "an authorized member opens the parameterized role view");
}
