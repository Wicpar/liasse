//! Security regression: a surface-admitted generated `uuid()` — the signup/email
//! challenge token `email_challenges.token` — MUST be unpredictable (SPEC.md
//! §5.1/§8.12).
//!
//! Before the fix, a surface mutation was admitted under the counter-seeded
//! [`VirtualClock`] as its [`liasse_runtime::Generators`], so the per-request seed
//! `uuid()` derives from was `0, 1, 2, …`. Every fresh host therefore minted the
//! SAME first token, letting an attacker who guesses the counter predict a victim's
//! token. The fix routes admission through a CSPRNG [`Entropy`] seam while keeping
//! `now()` request-fixed from the virtual clock (Annex A.5).
//!
//! These cases prove, against externally-deducible expectations:
//!
//! * two OS-seeded hosts mint DISTINCT tokens for the identical first request — the
//!   defect's signature (a counter seam makes them identical) is gone;
//! * the injectable seam still admits a deterministic RNG, so a seeded host replays
//!   identically and two different seeds diverge — reproducibility without
//!   sacrificing production unpredictability;
//! * `now()` stays deterministic and request-fixed across admissions while the token
//!   varies — only the randomness moved to the CSPRNG, not the clock;
//! * §12.3 dedup replay reuses the RECORDED token verbatim rather than drawing a
//!   fresh one (§8.12 "produced once" recording guarantee).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_store::MemoryStore;
use liasse_surface::{
    CallBinding, Engine, Entropy, Precision, SurfaceBinding, SurfaceCall, SurfaceHost,
    SurfaceOutcome, Value, ViewBinding, VirtualClock,
};
use liasse_value::Timestamp;

use support::{store, text, NOW};

/// A signup application whose challenge rows carry two generated `uuid()` values —
/// the surrogate `id` key and the security-critical `token` — plus a `now()`
/// timestamp, mirroring the reported `email_challenges` shape. `request` inserts a
/// challenge from a supplied `email`; every generated member comes from a default.
const SIGNUP_APP: &str = r#"{
  "$liasse": 1
  "$app": "example.signup@1.0.0"
  "$model": {
    "email_challenges": {
      "$key": "id"
      "id": "uuid = uuid()"
      "email": "text"
      "token": "uuid = uuid()"
      "created_at": "timestamp = now()"
    }
    "challenges": { "$view": ".email_challenges { id, email, token, created_at }" }
    "$mut": {
      "request({ email: text })": ".email_challenges + { email: @email }"
    }
    "$public": {
      "signup": {
        "$view": ".challenges"
        "$mut": { "request": ".request" }
      }
    }
  }
}"#;

/// Build a signup host over a fresh in-memory store at [`NOW`], with `entropy` as
/// its generated-value source (the injectable seam under test).
fn host(instance: &str, entropy: Entropy) -> SurfaceHost<MemoryStore> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let engine = Engine::load(store(instance), SIGNUP_APP, &mut clock).expect("load signup app");
    let signup = SurfaceBinding::new()
        .with_view(ViewBinding::new("challenges"))
        .with_call("request", CallBinding::root("request", ["email".to_owned()]));
    let router = liasse_surface::SurfaceRouterBuilder::new()
        .public_surface("signup", signup)
        .build(engine.model())
        .expect("router validates against the signup model");
    SurfaceHost::new(engine, router, clock).with_entropy(entropy)
}

/// A `public.signup.request` call for `email`, optionally carrying an `operation_id`
/// (§12.3 at-most-once dedup).
fn request(email: &str, op: Option<&str>) -> SurfaceCall {
    let call = SurfaceCall::new(
        liasse_surface::SurfaceAddress::parse("public.signup.request").expect("address parses"),
        [("email".to_owned(), text(email))].into_iter().collect(),
    );
    match op {
        Some(id) => call.with_operation_id(id),
        None => call,
    }
}

/// Admit a challenge for `email` on `conn`, asserting it commits, and read the
/// generated `(token, created_at)` back from committed state (§8.12: the value that
/// entered committed state, read verbatim).
fn mint(host: &mut SurfaceHost<MemoryStore>, conn: &str, email: &str) -> (Value, Value) {
    let outcome = host.call(conn, &request(email, None)).expect("call");
    assert!(matches!(outcome, SurfaceOutcome::Committed { .. }), "request commits: {outcome:?}");
    row_for(host, email)
}

/// The committed `(token, created_at)` of the challenge whose `email` matches.
fn row_for(host: &SurfaceHost<MemoryStore>, email: &str) -> (Value, Value) {
    let view = host.engine().view_at_head("challenges").expect("view").expect("declared");
    let row = view
        .rows()
        .iter()
        .find(|row| row.field("email") == Some(&text(email)))
        .expect("challenge row present");
    (
        row.field("token").cloned().expect("token"),
        row.field("created_at").cloned().expect("created_at"),
    )
}

/// Two independently constructed OS-seeded hosts mint DIFFERENT tokens for the
/// byte-identical first request. Under the pre-fix counter seam both would seed
/// from `0` and mint the SAME token — the predictability defect. Distinct tokens
/// are the proof the seed is high-entropy CSPRNG output, not counter-derived.
#[test]
fn os_entropy_makes_the_first_surface_token_unpredictable() {
    let mut a = host("signup-a", Entropy::os());
    let mut b = host("signup-b", Entropy::os());
    a.connect("c").expect("connect a");
    b.connect("c").expect("connect b");

    let (token_a, _) = mint(&mut a, "c", "victim@example.test");
    let (token_b, _) = mint(&mut b, "c", "victim@example.test");

    assert!(matches!(token_a, Value::Uuid(_)), "token is a uuid: {token_a:?}");
    assert_ne!(
        token_a, token_b,
        "two fresh OS-seeded hosts must NOT mint the same first token (counter-seed signature)"
    );
}

/// The injectable seam still admits a deterministic RNG: two hosts seeded alike
/// replay the identical token, and a differently seeded host diverges. This is the
/// reproducibility a test relies on, obtained WITHOUT weakening the production OS
/// default.
#[test]
fn injected_seeded_entropy_replays_identically_and_diverges_on_a_different_seed() {
    let mut one = host("signup-1", Entropy::seeded(42));
    let mut two = host("signup-2", Entropy::seeded(42));
    let mut other = host("signup-3", Entropy::seeded(43));
    for h in [&mut one, &mut two, &mut other] {
        h.connect("c").expect("connect");
    }

    let (t_one, _) = mint(&mut one, "c", "a@example.test");
    let (t_two, _) = mint(&mut two, "c", "a@example.test");
    let (t_other, _) = mint(&mut other, "c", "a@example.test");

    assert_eq!(t_one, t_two, "the same injected seed must replay the same token");
    assert_ne!(t_one, t_other, "a different injected seed must produce a different token");
}

/// `now()` is request-fixed and deterministic (Annex A.5) even as the token is
/// randomized: across two admissions at the un-advanced clock, `created_at` is the
/// SAME fixed instant while the tokens differ. Only the randomness moved to the
/// CSPRNG; time did not.
#[test]
fn now_is_request_fixed_while_the_token_is_random() {
    let mut h = host("signup-now", Entropy::os());
    h.connect("c").expect("connect");

    let (token1, created1) = mint(&mut h, "c", "one@example.test");
    let (token2, created2) = mint(&mut h, "c", "two@example.test");

    let fixed = Value::Timestamp(Timestamp::new(NOW, Precision::Micros));
    assert_eq!(created1, fixed, "created_at is the request-fixed virtual-clock instant");
    assert_eq!(created2, created1, "now() is deterministic across admissions (A.5)");
    assert_ne!(token1, token2, "uuid() draws fresh CSPRNG entropy per admission (§5.1)");
}

/// §12.3 dedup replay reuses the RECORDED token (§8.12 "produced once"): re-issuing
/// the same `operation_id` returns the retained outcome and re-draws NOTHING —
/// committed state still holds exactly one challenge carrying the original token, so
/// a replayed request can never mint a second, fresh token.
#[test]
fn dedup_replay_reuses_the_recorded_token_and_never_redraws() {
    let mut h = host("signup-replay", Entropy::os());
    h.connect("c").expect("connect");

    let first = h.call("c", &request("dup@example.test", Some("op-1"))).expect("first call");
    assert!(matches!(first, SurfaceOutcome::Committed { .. }), "first admits: {first:?}");
    let (recorded, _) = row_for(&h, "dup@example.test");

    // A byte-identical retry under the same operation identifier is at-most-once:
    // the retained outcome is replayed, the transition is NOT re-executed.
    let replay = h.call("c", &request("dup@example.test", Some("op-1"))).expect("replay call");
    assert!(matches!(replay, SurfaceOutcome::Committed { .. }), "replay reports the retained outcome");

    let view = h.engine().view_at_head("challenges").expect("view").expect("declared");
    let rows: Vec<_> =
        view.rows().iter().filter(|row| row.field("email") == Some(&text("dup@example.test"))).collect();
    assert_eq!(rows.len(), 1, "replay must not mint a second challenge row");
    assert_eq!(
        rows[0].field("token"),
        Some(&recorded),
        "the recorded token is reused verbatim on replay, never re-drawn (§8.12)"
    );
}
