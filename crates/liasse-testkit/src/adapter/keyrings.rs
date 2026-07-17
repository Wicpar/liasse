//! Driving the §17 keyring op families against the engine's internal keyrings.
//!
//! A `$keyring` declaration is self-provisioned by the runtime [`Engine`]: it
//! bootstraps one live [`Keyring`](liasse_runtime::Keyring) per declaration over a
//! deterministic in-process key provider, rotates it as the virtual clock moves,
//! and answers `cose.sign(/ring, …)` in a mutation and `cose.verify` at
//! authentication against *that* ring (crate::adapter::runtime drives the clock
//! and reads the ring through the engine accessors). The version metadata a case
//! reads through `/ring.$current`/`.$accepted`/`.$versions` is a snapshot of the
//! same engine ring.
//!
//! This module parses the two chapter-local step keys (tests/17-keyrings/NOTES.md)
//! into typed specs and drives them against the engine ring: the `provider_set`
//! fault-injection vocabulary (§17.9) onto the ring's backing [`SimKeyProvider`]
//! ([`Engine::keyring_provider_mut`](liasse_runtime::Engine::keyring_provider_mut)),
//! and the `keyring_admin` lifecycle transitions (§17.3/§17.4) — `bind_activate`,
//! `revoke`, `destroy` — onto the ring itself through
//! [`Engine::keyring_admin`](liasse_runtime::Engine::keyring_admin), the mutable
//! keyring-lifecycle accessor.

use liasse_host::sim::{ProviderOp, SimKeyProvider};
use liasse_host::ExternalKeyRef;
use liasse_runtime::{Engine, Keyring, VersionId, MANUAL_EXTERNAL_KEY};
use liasse_store::InstanceStore;
use serde_json::Value as J;

use crate::contract::Observation;
use crate::outcome::Outcome;

/// A parsed `provider_set` step (§17.9 fault injection): the provider name it
/// targets and the reconfiguration it applies from this step onward. Each field
/// is applied only when the step names it, so a `{ fail: [...] }` leaves
/// availability untouched and a `{ available: false }` leaves the fail set alone.
pub(super) struct ProviderSetSpec {
    /// `available: false` fails every provider operation (a total outage).
    pub(super) available: Option<bool>,
    /// Operations that fail cleanly (`fail`).
    pub(super) fail: Vec<ProviderOp>,
    /// Operations that never return (`hang`), modelled as the typed
    /// budget-exhausting failure the double raises — never an actual loop.
    pub(super) hang: Vec<ProviderOp>,
    /// Operations that return structurally invalid public-key material
    /// (`invalid_public_key`): the call succeeds but §17.4 validation rejects the
    /// resulting key, so §17.9 keeps the current version.
    pub(super) invalid_public_key: Vec<ProviderOp>,
}

/// A parsed `keyring_admin` step (§17.3/§17.4 lifecycle transition).
pub(super) struct KeyringAdminSpec {
    /// The addressed ring (`/name` or `name`).
    ring: String,
    /// The transition: `bind_activate`, `revoke`, or `destroy`.
    op: String,
    /// The version ordinal a `revoke`/`destroy` addresses (the `.$versions` `id`,
    /// carried as an `int` wire string).
    version: Option<String>,
}

impl ProviderSetSpec {
    /// Parse a `provider_set` step's payload, or `None` when it is malformed.
    pub(super) fn parse(target: &J) -> Option<Self> {
        let object = target.as_object()?;
        // The `provider` name the step carries is not read: the corpus backs every
        // ring of a case with the one declared provider, so the fault applies to
        // each engine ring uniformly.
        Some(Self {
            available: object.get("available").and_then(J::as_bool),
            fail: provider_ops(object.get("fail")),
            hang: provider_ops(object.get("hang")),
            invalid_public_key: provider_ops(object.get("invalid_public_key")),
        })
    }

    /// Apply the reconfiguration to `provider` (§17.9): a total outage, then the
    /// per-operation clean-failure / hang / invalid-public-key scripts the step
    /// names. Each list, when non-empty, replaces that script; an unnamed field
    /// leaves the corresponding provider state as it was.
    fn apply_to(&self, provider: &mut SimKeyProvider) {
        if let Some(available) = self.available {
            provider.set_available(available);
        }
        if !self.fail.is_empty() {
            provider.set_fail(self.fail.iter().copied());
        }
        if !self.hang.is_empty() {
            provider.set_hang(self.hang.iter().copied());
        }
        if !self.invalid_public_key.is_empty() {
            provider.set_invalid_public_key(self.invalid_public_key.iter().copied());
        }
    }

    /// Reconfigure the provider backing every engine ring (§17.9). A case backs
    /// all its rings with the single declared provider, so the fault applies to
    /// each; a ring whose provider the engine self-provisions is reached through
    /// [`Engine::keyring_provider_mut`].
    pub(super) fn apply<S: InstanceStore>(&self, engine: &mut Engine<S>) {
        let rings: Vec<String> = engine.keyring_names().map(ToOwned::to_owned).collect();
        for ring in rings {
            if let Some(provider) = engine.keyring_provider_mut(&ring) {
                self.apply_to(provider);
            }
        }
    }
}

impl KeyringAdminSpec {
    /// Parse a `keyring_admin` step's payload, or `None` when it names no ring/op.
    pub(super) fn parse(target: &J) -> Option<Self> {
        let object = target.as_object()?;
        let ring = object.get("ring").and_then(J::as_str)?.trim_start_matches('/').to_owned();
        let op = object.get("op").and_then(J::as_str)?.to_owned();
        let version = object.get("version").and_then(J::as_str).map(ToOwned::to_owned);
        Some(Self { ring, op, version })
    }

    /// Drive the lifecycle transition against the engine's self-provisioned ring
    /// (§17.3/§17.4), mapping its result to the harness outcome vocabulary:
    ///
    /// - `bind_activate` binds the engine provider's manual external handle
    ///   ([`MANUAL_EXTERNAL_KEY`]) and activates it through the same transition as
    ///   automatic rotation (§17.4), atomically retiring any prior active version;
    /// - `revoke`/`destroy` address the version whose `.$versions` `id` ordinal the
    ///   step names, resolved to its logical [`VersionId`] from the ring.
    ///
    /// A provider/capability/metadata failure (§17.4/§17.6/§17.9) — an algorithm
    /// mismatch, a missing external handle, an unknown version — is a `rejected`
    /// transition that activates or revokes nothing.
    pub(super) fn apply<S: InstanceStore>(&self, engine: &mut Engine<S>) -> Observation {
        let now = engine.now();
        let Some(ring) = engine.keyring_admin(&self.ring) else {
            return Observation::outcome(Outcome::Rejected);
        };
        let result = match self.op.as_str() {
            "bind_activate" => {
                ring.bind_activate(&ExternalKeyRef::new(MANUAL_EXTERNAL_KEY), now).map(|_| ())
            }
            "revoke" | "destroy" => {
                let Some(version) = self.version_id(ring) else {
                    return Observation::outcome(Outcome::Rejected);
                };
                if self.op == "revoke" {
                    ring.revoke(version, now)
                } else {
                    ring.destroy(version, now)
                }
            }
            _ => return Observation::outcome(Outcome::Error),
        };
        match result {
            Ok(()) => Observation::ok(None),
            Err(_) => Observation::outcome(Outcome::Rejected),
        }
    }

    /// The logical [`VersionId`] the step's version ordinal names, resolved from
    /// the ring's retained versions (§17.2 `.$versions.id`). `None` when the step
    /// carries no version or the ring has no version of that ordinal.
    fn version_id(&self, ring: &Keyring<SimKeyProvider>) -> Option<VersionId> {
        let ordinal: u64 = self.version.as_ref()?.parse().ok()?;
        ring.versions().iter().find(|version| version.id().get() == ordinal).map(|version| version.id())
    }
}

/// Parse a list of provider-operation tokens (§17.5), skipping any unknown token.
fn provider_ops(value: Option<&J>) -> Vec<ProviderOp> {
    value
        .and_then(J::as_array)
        .map(|list| list.iter().filter_map(|token| provider_op(token.as_str()?)).collect())
        .unwrap_or_default()
}

/// One `provider_set` operation token (§17.5), or `None` when unrecognized.
fn provider_op(token: &str) -> Option<ProviderOp> {
    Some(match token {
        "generate" => ProviderOp::Generate,
        "bind" => ProviderOp::Bind,
        "public_key" => ProviderOp::PublicKey,
        "sign" => ProviderOp::Sign,
        "disable" => ProviderOp::Disable,
        "destroy" => ProviderOp::Destroy,
        "attest" => ProviderOp::Attest,
        _ => return None,
    })
}
