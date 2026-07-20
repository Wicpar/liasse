#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17.5 keyring provider CONTINUITY across the reconstruction boundaries a
//! `$provider`-backed ring must survive without ever silently downgrading to the
//! forgeable sim double (Task 5-upstream FIX F1/F2): a §20 migration that adds a
//! ring, a §19.10 restore, and a §19.8 import movement.
//!
//! The honesty rule: a `$provider`-named ring is REAL-provisioned when its
//! provider is available, and refuses LOUDLY otherwise — never a quiet sim
//! fallback. Only a name the application registered nothing for keeps the sim
//! default. Each test would pass silently on the pre-fix engine (the new/restored
//! ring self-provisioned the publicly-reconstructable sim key, so a forged token
//! verified), so each asserts the discriminating property: a sim-signed token for
//! the reconstructed ring is DENIED.

use std::collections::BTreeMap;

use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use liasse_expr::Cell;
use liasse_host::{
    Attestation, ExternalKeyRef, KeyCapabilities, KeyHandle, KeyOperation, KeyProvider, KeySpec,
    ProtectionClass, ProviderFailure, PublicKey,
};
use liasse_ident::InstanceId;
use liasse_key_ed25519::Ed25519KeyProvider;
use liasse_runtime::{
    CallOutcome, CallRequest, CoseVerifyError, Engine, EngineError, FixedGenerators, ImportError,
    ImportRelation, Precision, Registry, UpdateError,
};
use liasse_store::MemoryStore;
use liasse_value::{Bytes, Text, Value};
use sha2::{Digest, Sha512};

const NOW: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

// ---------------------------------------------------------------------------
// A deterministic REAL Ed25519 provider: a fresh instance built with the same
// secret reproduces the same key per handle, so a restored ring re-bootstraps the
// SAME signing key and pre-export tokens keep verifying — while the secret keeps
// the keys non-forgeable (unlike the sim double, whose seed is the public handle
// id). This is a disaster-recovery KMS (re-supplied at restore) reduced to a test.
// ---------------------------------------------------------------------------

struct DeterministicEd25519 {
    secret: [u8; 32],
    keys: BTreeMap<u64, (SigningKey, bool)>,
    next_id: u64,
}

impl DeterministicEd25519 {
    fn new(secret: [u8; 32]) -> Self {
        Self { secret, keys: BTreeMap::new(), next_id: 1 }
    }

    fn derive(&self, id: u64) -> SigningKey {
        let mut hasher = Sha512::new();
        hasher.update(self.secret);
        hasher.update(id.to_le_bytes());
        let digest = hasher.finalize();
        let mut seed = [0u8; SECRET_KEY_LENGTH];
        seed.copy_from_slice(&digest[..SECRET_KEY_LENGTH]);
        SigningKey::from_bytes(&seed)
    }

    fn live(&self, key: &KeyHandle) -> Result<&SigningKey, ProviderFailure> {
        match self.keys.get(&key.get()) {
            Some((signing, false)) => Ok(signing),
            _ => Err(ProviderFailure::UnknownKey(key.get())),
        }
    }
}

impl KeyProvider for DeterministicEd25519 {
    fn capabilities(&self) -> KeyCapabilities {
        KeyCapabilities::builder(ProtectionClass::Software)
            .algorithm("Ed25519")
            .operation(KeyOperation::Sign)
            .operation(KeyOperation::Verify)
            .generates()
            .disables()
            .destroys()
            .build()
    }

    fn generate(&mut self, spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        if spec.algorithm != "Ed25519" && spec.algorithm != "EdDSA" {
            return Err(ProviderFailure::Algorithm(spec.algorithm.clone()));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.keys.insert(id, (self.derive(id), false));
        Ok(KeyHandle::new(id))
    }

    fn bind(&mut self, external: &ExternalKeyRef, _spec: &KeySpec) -> Result<KeyHandle, ProviderFailure> {
        Err(ProviderFailure::UnknownExternal(external.as_str().to_owned()))
    }

    fn public_key(&self, key: &KeyHandle) -> Result<PublicKey, ProviderFailure> {
        let signing = self.live(key)?;
        Ok(PublicKey::new("Ed25519", Value::Bytes(Bytes::new(signing.verifying_key().to_bytes().to_vec()))))
    }

    fn sign(&self, key: &KeyHandle, algorithm: &str, message: &[u8]) -> Result<Vec<u8>, ProviderFailure> {
        if algorithm != "Ed25519" && algorithm != "EdDSA" {
            return Err(ProviderFailure::Algorithm(algorithm.to_owned()));
        }
        Ok(self.live(key)?.sign(message).to_bytes().to_vec())
    }

    fn disable(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        match self.keys.get_mut(&key.get()) {
            Some(entry) => {
                entry.1 = true;
                Ok(())
            }
            None => Err(ProviderFailure::UnknownKey(key.get())),
        }
    }

    fn destroy(&mut self, key: &KeyHandle) -> Result<(), ProviderFailure> {
        self.keys.remove(&key.get()).map(|_| ()).ok_or(ProviderFailure::UnknownKey(key.get()))
    }

    fn attest(&self, key: &KeyHandle) -> Result<Option<Attestation>, ProviderFailure> {
        self.live(key).map(|_| None)
    }
}

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

/// v1: one `session_keys` ring naming `test-kp`, a `login` minting through it.
const V1: &str = r#"{
  "$liasse": 1,
  "$app": "t.krmig@1.0.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ]
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

/// v2 adding a SECOND ring `audit_keys` under a DISTINCT provider `audit-kp`, plus
/// a `login_audit` minting through it (a minor, additive successor of v1).
const V2_DISTINCT: &str = r#"{
  "$liasse": 1,
  "$app": "t.krmig@1.1.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "audit_keys": { "$keyring": { "$provider": "audit-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ],
      "login_audit": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/audit_keys, { auth: 'audit', session: session.$key })",
        "return { token }"
      ]
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

/// v2 adding `audit_keys` under the SAME provider `test-kp` session_keys already
/// consumed at load — the source genuinely lacks a second `test-kp` to back it.
const V2_SAME_PROVIDER: &str = r#"{
  "$liasse": 1,
  "$app": "t.krmig@1.1.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "audit_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P45D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ]
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

/// v2 that TIGHTENS `$retain` on the existing `session_keys` ring (F2).
const V2_TIGHTEN_RETAIN: &str = r#"{
  "$liasse": 1,
  "$app": "t.krmig@1.1.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "Ed25519", "$rotate": "P30D", "$retain": "P10D" } },
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "sessions": { "$key": "id", "id": "uuid = uuid()", "account": { "$ref": "/accounts" }, "revoked": "bool = false" },
    "$mut": {
      "login": [
        "session = /sessions + { account: @account }",
        "token = cose.sign(/session_keys, { auth: 'session', session: session.$key })",
        "return { token }"
      ]
    }
  },
  "$data": { "accounts": { "alice": { "name": "Alice" } } }
}"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn provider(name: &str, provider: impl KeyProvider + 'static) -> Registry {
    let mut registry = Registry::new();
    registry.register_provider(name, Box::new(provider) as Box<dyn KeyProvider>);
    registry
}

fn ed25519(name: &str) -> Registry {
    provider(name, Ed25519KeyProvider::new())
}

/// A registry backing two distinct provider names with real Ed25519 providers.
fn ed25519_two(a: &str, b: &str) -> Registry {
    let mut registry = Registry::new();
    registry.register_provider(a, Box::new(Ed25519KeyProvider::new()) as Box<dyn KeyProvider>);
    registry.register_provider(b, Box::new(Ed25519KeyProvider::new()) as Box<dyn KeyProvider>);
    registry
}

fn store(tag: &str) -> MemoryStore {
    MemoryStore::new(InstanceId::new(tag))
}

/// Run a login-style mutation `name` and return the minted cose token value.
fn mint(engine: &mut Engine<MemoryStore>, name: &str, g: &mut FixedGenerators) -> Value {
    let request = CallRequest::new(name).arg("account", Value::Text(Text::new("alice")));
    let CallOutcome::Committed { response, .. } = engine.call(&request, g).expect("no engine fault") else {
        panic!("{name} must commit");
    };
    let response = response.expect("a return value");
    let Cell::Scalar(Value::Struct(fields)) = response.cell() else {
        panic!("{name} returns a `{{ token }}` struct");
    };
    fields.get("token").cloned().expect("a `token` member")
}

/// A sim-signed token for `ring`, minted by a fresh SIM-backed engine over `pkg`.
fn sim_token(pkg: &str, ring_mut: &str, tag: &str) -> Value {
    let mut g = generator();
    let mut sim = Engine::load(store(tag), pkg, &mut g).expect("sim load");
    mint(&mut sim, ring_mut, &mut g)
}

// ---------------------------------------------------------------------------
// F1 — migration adding a `$provider`-named ring
// ---------------------------------------------------------------------------

/// TEST 1 — a migration that adds a `$provider`-named ring whose provider the
/// engine still retains REAL-provisions it: minting through it verifies, and a
/// sim-signed token for that ring is DENIED. On the pre-fix engine the new ring
/// self-provisioned the sim double, so the sim token would have been ACCEPTED.
#[test]
fn migration_adds_ring_real_provisioned_when_source_retained() {
    let mut g = generator();
    // `audit-kp` is registered at load but backs no v1 ring, so it is retained.
    let mut engine = Engine::load_with_hosts(store("mig-real"), V1, &mut g, ed25519_two("test-kp", "audit-kp"))
        .expect("v1 load");

    engine.update(V2_DISTINCT, &mut g).expect("additive migration adding a retained ring succeeds");

    // The new ring signs through the retained real provider: its token verifies.
    let real = mint(&mut engine, "login_audit", &mut g);
    let (claims, _) = engine.cose_verify("audit_keys", &real).expect("the real audit token verifies");
    let Value::Struct(fields) = &claims else { panic!("claims are a struct") };
    assert_eq!(fields.get("auth"), Some(&Value::Text(Text::new("audit"))));

    // The discriminating property: a sim-minted token for `audit_keys` is DENIED —
    // the migrated ring is NOT backed by the reconstructable sim key.
    let forged = sim_token(V2_DISTINCT, "login_audit", "mig-real-sim");
    assert!(
        matches!(engine.cose_verify("audit_keys", &forged), Err(CoseVerifyError::ClaimsTampered)),
        "a sim-signed token must be denied by the REAL-provisioned migrated ring",
    );
}

/// TEST 2 — the same migration when the provider source genuinely lacks a provider
/// for the new ring (it reuses `test-kp`, already consumed by `session_keys`)
/// REFUSES loudly, leaving the old head and keyrings intact. The pre-fix engine
/// silently self-provisioned the sim double and the migration succeeded.
#[test]
fn migration_refused_when_registered_provider_unavailable() {
    let mut g = generator();
    let mut engine = Engine::load_with_hosts(store("mig-refuse"), V1, &mut g, ed25519("test-kp")).expect("v1 load");
    let head_before = engine.head().expect("head");

    let error = engine.update(V2_SAME_PROVIDER, &mut g).expect_err("migration must refuse, not silently sim");
    assert!(
        matches!(error, UpdateError::Engine(EngineError::Keyring(_))),
        "a $provider that the source cannot supply must be a loud keyring refusal, got {error:?}",
    );

    // Engine unchanged: old head intact, no `audit_keys` ring, session_keys still works.
    assert_eq!(engine.head().expect("head"), head_before, "a refused migration commits nothing");
    assert!(engine.keyring("audit_keys").is_none(), "the refused ring was not provisioned");
    let token = mint(&mut engine, "login", &mut g);
    assert!(engine.cose_verify("session_keys", &token).is_ok(), "the original ring is untouched");
}

// ---------------------------------------------------------------------------
// F1 — §19.10 restore
// ---------------------------------------------------------------------------

/// TEST 3 — `restore_with_hosts` re-provisions the ring against the supplied
/// deterministic provider, so a token minted BEFORE export still verifies after
/// restore, and a sim-signed token is DENIED. The pre-fix restore rebuilt over a
/// bare registry (sim double), so the pre-export Ed25519 token no longer verified.
#[test]
fn restore_with_hosts_reprovisions_and_verifies_pre_export_tokens() {
    let secret = [7u8; 32];
    let mut g = generator();
    let mut engine =
        Engine::load_with_hosts(store("restore-real"), V1, &mut g, provider("test-kp", DeterministicEd25519::new(secret)))
            .expect("v1 load over the deterministic provider");

    let pre_export = mint(&mut engine, "login", &mut g);
    engine.cose_verify("session_keys", &pre_export).expect("the token verifies before export");
    let artifact = engine.export().expect("export");

    // Restore over a FRESH provider instance with the SAME secret (a re-supplied
    // KMS): the ring re-bootstraps the same key, so the pre-export token verifies.
    let mut rg = generator();
    let restored = Engine::restore_with_hosts(
        store("restore-real"),
        &artifact,
        provider("test-kp", DeterministicEd25519::new(secret)),
        &mut rg,
    )
    .expect("restore_with_hosts re-provisions the ring");
    restored.cose_verify("session_keys", &pre_export).expect("the pre-export token still verifies after restore");

    // And the restored ring is REAL, not sim: a sim-signed token is denied.
    let forged = sim_token(V1, "login", "restore-real-sim");
    assert!(
        matches!(restored.cose_verify("session_keys", &forged), Err(CoseVerifyError::ClaimsTampered)),
        "a sim-signed token must be denied by the restored real ring",
    );
}

/// TEST 4 — a provider-less `restore` of state declaring a `$provider`-named ring
/// REFUSES loudly rather than silently self-provisioning the forgeable sim double.
/// The pre-fix `restore` rebuilt every ring as sim and succeeded.
#[test]
fn bare_restore_of_provider_ring_refuses() {
    let mut g = generator();
    let engine =
        Engine::load_with_hosts(store("restore-refuse"), V1, &mut g, ed25519("test-kp")).expect("v1 load");
    let artifact = engine.export().expect("export");

    let mut rg = generator();
    let error = match Engine::restore(store("restore-refuse"), &artifact, &mut rg) {
        Ok(_) => panic!("a provider-less restore of a $provider ring must refuse, but it restored"),
        Err(error) => error,
    };
    assert!(
        matches!(error, ImportError::Engine(EngineError::Keyring(_))),
        "a bare restore of a $provider ring must be a loud keyring refusal, got {error:?}",
    );
}

// ---------------------------------------------------------------------------
// F1 — §19.8 import / reinstall
// ---------------------------------------------------------------------------

/// TEST 5 — the §19.8 import variant of the restore case, over the DISTINCT
/// `reinstall_point` path: a follower fast-forwards onto a continuation whose
/// definition adds a `$provider`-named ring, and that ring is REAL-provisioned from
/// the follower's retained registry (mint+verify), while a sim-signed token for it
/// is DENIED. The pre-fix import re-provisioned a non-retained ring over a
/// consumed/bare registry as the forgeable sim double.
#[test]
fn import_fast_forward_real_provisions_added_ring() {
    // A base advances v1 -> (migrate) v2, which adds `audit_keys` under the retained
    // `audit-kp`; both points are exported.
    let mut g = generator();
    let mut base =
        Engine::load_with_hosts(store("import-ff"), V1, &mut g, ed25519_two("test-kp", "audit-kp")).expect("v1 load");
    let early = base.export().expect("export v1 point");
    base.update(V2_DISTINCT, &mut g).expect("migrate base to v2");
    let ahead = base.export().expect("export v2 point");

    // A follower restored at the v1 point (its registry still holds the retained
    // `audit-kp`) fast-forwards onto the v2 continuation.
    let mut fg = generator();
    let mut follower = Engine::restore_with_hosts(
        store("import-ff"),
        &early,
        ed25519_two("test-kp", "audit-kp"),
        &mut fg,
    )
    .expect("restore follower at v1 point");
    assert_eq!(follower.classify(&ahead).expect("classify"), ImportRelation::FastForward);
    let report = follower.import(&ahead, &[ImportRelation::FastForward]).expect("import");
    assert!(report.applied, "the continuation fast-forwards");

    // The ring the incoming definition added is REAL-provisioned through the import
    // path (reinstall_point), not the forgeable sim double.
    let real = mint(&mut follower, "login_audit", &mut fg);
    follower.cose_verify("audit_keys", &real).expect("the imported real audit token verifies");
    let forged = sim_token(V2_DISTINCT, "login_audit", "import-ff-sim");
    assert!(
        matches!(follower.cose_verify("audit_keys", &forged), Err(CoseVerifyError::ClaimsTampered)),
        "a sim-signed token must be denied by the REAL-provisioned imported ring",
    );
}

// ---------------------------------------------------------------------------
// F2 — policy change on a retained ring
// ---------------------------------------------------------------------------

/// F2 — a migration that tightens `$retain` on an EXISTING ring is not a silent
/// no-op: the version lifecycle cannot soundly hot-apply the change, so the
/// migration REFUSES loudly and the live ring keeps its original retention window.
/// The pre-fix engine dropped the new policy silently and the migration succeeded.
#[test]
fn migration_tightening_retain_on_live_ring_refuses() {
    let mut g = generator();
    let mut engine =
        Engine::load_with_hosts(store("retain"), V1, &mut g, ed25519("test-kp")).expect("v1 load");
    let head_before = engine.head().expect("head");

    let error = engine.update(V2_TIGHTEN_RETAIN, &mut g).expect_err("a live-ring policy change must refuse");
    assert!(
        matches!(error, UpdateError::Engine(EngineError::Keyring(_))),
        "a policy change on a live ring must be a loud keyring refusal, got {error:?}",
    );
    assert_eq!(engine.head().expect("head"), head_before, "a refused migration commits nothing");
}
