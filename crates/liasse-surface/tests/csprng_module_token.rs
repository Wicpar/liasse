//! Security regression: a MODULE-admitted generated `uuid()` — a token minted by a
//! module instance's §13.11 direct-surface mutation (or a §13.10 interface-routed
//! mutation) — MUST be unpredictable (SPEC.md §5.1/§8.12), exactly as a base
//! `SurfaceHost`-admitted token is (see `csprng_surface_token.rs`).
//!
//! [`ModuleDeployment`] drives the §13 module lifecycle in the production surface
//! layer. Before the fix it admitted every child mutation under its owned
//! counter-seeded [`VirtualClock`] as the [`liasse_runtime::Generators`], so a
//! module-minted `uuid()` derived from the seed sequence `…, K, K+1, …`. Two fresh
//! deployments of the same module therefore minted the SAME token for the identical
//! first request — an attacker who guesses the counter reconstructs a victim's
//! module token. This is the very defect the base-host CSPRNG seam closed, left open
//! on the module path.
//!
//! The fix gives [`ModuleDeployment`] the same injectable [`Entropy`] seam the base
//! host has: production defaults to the OS CSPRNG (unpredictable module tokens),
//! while `now()` stays the request-fixed virtual-clock instant (Annex A.5).
//!
//! Externally-deducible expectations:
//!
//! * two OS-seeded deployments mint DISTINCT tokens for the identical first module
//!   request — the counter seam's signature (identical tokens) is gone;
//! * the injectable seam still admits a deterministic RNG, so equal seeds replay the
//!   same token and different seeds diverge — reproducibility without sacrificing
//!   production unpredictability.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest, InstallRequest};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_surface::{
    Engine, Entropy, ModuleDeployment, ModuleHost, ModuleObservation, ModuleSpace, Precision,
    Value, VirtualClock,
};
use liasse_value::Text;

const NOW: i128 = 1_700_000_000_000_000;

/// A minimal root exposing one row-scoped module space `/orgs/acme/modules`.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "example.root@1.0.0"
  "$model": {
    "orgs": { "$key": "id", "id": "text", "modules": { "$modules": {} } }
  }
  "$data": { "orgs": { "acme": {} } }
}"#;

/// A module whose `mint` mutation inserts a `grants` row carrying a
/// security-critical generated `token` (and a generated surrogate `id` key). The
/// `all` view surfaces the token so a §13.11 direct-surface read recovers it.
const GRANTS: &str = r#"{
  "$liasse": 1
  "$module": "example.grants@1.0.0"
  "$model": {
    "grants": { "$key": "id", "id": "uuid = uuid()", "label": "text", "token": "uuid = uuid()" }
    "all": { "$view": ".grants { id, label, token }" }
    "$mut": { "mint": ".grants + { label: @label }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn space() -> ModuleSpace {
    ModuleSpace::new("/orgs/acme/modules").expect("mount path")
}

/// Build a deployment over [`ROOT`] with `entropy` as its module-admission seed
/// source (the injectable seam under test). The root loads over the clock exactly
/// as a production driver loads it before wrapping the deployment.
fn deployment(instance: &str, entropy: Entropy) -> ModuleDeployment<MemoryStoreFactory> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let root = Engine::load(MemoryStore::new(InstanceId::new(instance)), ROOT, &mut clock)
        .expect("root loads");
    ModuleDeployment::new(ModuleHost::new(MemoryStoreFactory::new(), root), clock).with_entropy(entropy)
}

/// Install `example.grants` as `svc`, mint one grant for `label`, and return the
/// generated `token` read back from committed state via the §13.11 direct surface.
fn mint_token(deployment: &mut ModuleDeployment<MemoryStoreFactory>, label: &str) -> Value {
    let space = space();
    assert_eq!(
        deployment.install(&space, InstallRequest::new("svc", GRANTS)).expect("install"),
        ModuleObservation::Applied,
    );
    let request = CallRequest::new("mint").arg("label", text(label));
    let outcome = deployment.child_call(&space, "svc", &request).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "mint commits: {outcome:?}");

    let view = deployment.child_view(&space, "svc", "all").expect("view").expect("declared");
    let row = view
        .rows()
        .iter()
        .find(|row| row.field("label") == Some(&text(label)))
        .expect("grant row present");
    row.field("token").cloned().expect("token")
}

/// Two independently constructed OS-seeded deployments mint DIFFERENT tokens for the
/// byte-identical first module request. Under the pre-fix counter seam both seeded
/// from the same clock counter and minted the SAME token — the predictability
/// defect. Distinct tokens prove the seed is high-entropy CSPRNG output.
#[test]
fn os_entropy_makes_the_first_module_token_unpredictable() {
    let mut a = deployment("root-a", Entropy::os());
    let mut b = deployment("root-b", Entropy::os());

    let token_a = mint_token(&mut a, "victim@example.test");
    let token_b = mint_token(&mut b, "victim@example.test");

    assert!(matches!(token_a, Value::Uuid(_)), "token is a uuid: {token_a:?}");
    assert_ne!(
        token_a, token_b,
        "two fresh OS-seeded deployments must NOT mint the same first module token (counter-seed signature)"
    );
}

/// The injectable seam still admits a deterministic RNG: two deployments seeded
/// alike replay the identical module token, and a differently seeded one diverges.
/// This is the reproducibility a conformance harness relies on, obtained WITHOUT
/// weakening the production OS default.
#[test]
fn injected_seeded_module_entropy_replays_identically_and_diverges_on_a_different_seed() {
    let mut one = deployment("root-1", Entropy::seeded(42));
    let mut two = deployment("root-2", Entropy::seeded(42));
    let mut other = deployment("root-3", Entropy::seeded(43));

    let t_one = mint_token(&mut one, "a@example.test");
    let t_two = mint_token(&mut two, "a@example.test");
    let t_other = mint_token(&mut other, "a@example.test");

    assert_eq!(t_one, t_two, "the same injected seed must replay the same module token");
    assert_ne!(t_one, t_other, "a different injected seed must produce a different module token");
}
