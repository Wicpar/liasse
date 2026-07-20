#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §17.5 real-provider injection into the engine's keyrings, driven end-to-end
//! through mutation admission and native-cose verification (Task 5-upstream).
//!
//! `Engine::load_with_hosts` resolves each declared `$keyring`'s `$provider`
//! against the host [`Registry`]'s registered providers: a ring whose provider
//! the application registered signs with *that* provider (here the software
//! [`Ed25519KeyProvider`]), while a ring with no registered provider keeps the
//! deterministic sim double. This asserts the injected path is genuinely used —
//! not silently downgraded to the forgeable sim keys — through the version
//! lifecycle, a `cose.sign` login, `cose.verify` acceptance, forgery/foreign/sim-
//! signed denials, clock-driven rotation, and a loud capability-shortfall load
//! failure.

use liasse_expr::Cell;
use liasse_host::KeyProvider;
use liasse_ident::InstanceId;
use liasse_key_ed25519::Ed25519KeyProvider;
use liasse_runtime::{
    CallOutcome, CallRequest, CoseClaims, CoseToken, CoseVerifyError, Engine, EngineError,
    FixedGenerators, KeyState, Precision, Registry,
};
use liasse_store::MemoryStore;
use liasse_value::{Text, Timestamp, Value};

const NOW: i128 = 1_700_000_000_000_000;
/// P30D in microseconds (the LOGIN ring's rotation cadence).
const P30D: i128 = 30 * 24 * 3600 * 1_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW, Precision::Micros)
}

/// A `session_keys` keyring naming provider `test-kp`, an Ed25519 login mutation
/// minting a token through `cose.sign`, and the mutation core the runtime admits.
const LOGIN: &str = r#"{
  "$liasse": 1,
  "$app": "t.keyrings@1.0.0",
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

/// The same package declaring `$algorithm: "ES256"`, which the software Ed25519
/// provider does not advertise (§17.6). The package COMPILES (the algorithm is a
/// provider concern, not a static one), so an injected `test-kp` must fail the
/// capability check loudly *at provision* rather than sign with an unsupported
/// algorithm or silently downgrade to sim.
const LOGIN_ES256: &str = r#"{
  "$liasse": 1,
  "$app": "t.keyrings@1.0.0",
  "$requires": { "cose": "liasse.cose@1" },
  "$model": {
    "session_keys": { "$keyring": { "$provider": "test-kp", "$algorithm": "ES256", "$rotate": "P30D", "$retain": "P45D" } },
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

/// A registry backing `test-kp` with the real software Ed25519 provider (§17.5).
fn ed25519_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_provider("test-kp", Box::new(Ed25519KeyProvider::new()) as Box<dyn KeyProvider>);
    registry
}

/// An engine over the LOGIN package whose `session_keys` ring is backed by the
/// injected Ed25519 provider.
fn injected_engine() -> Engine<MemoryStore> {
    let store = MemoryStore::new(InstanceId::new("injected"));
    let mut g = generator();
    Engine::load_with_hosts(store, LOGIN, &mut g, ed25519_registry()).expect("injected load resolves")
}

/// Run the login mutation and return the minted cose token value.
fn login_token(engine: &mut Engine<MemoryStore>, g: &mut FixedGenerators) -> Value {
    let request = CallRequest::new("login").arg("account", Value::Text(Text::new("alice")));
    let CallOutcome::Committed { response, .. } = engine.call(&request, g).expect("no engine fault") else {
        panic!("login must commit");
    };
    let response = response.expect("a return value");
    let Cell::Scalar(Value::Struct(fields)) = response.cell() else {
        panic!("the login returns a `{{ token }}` struct");
    };
    fields.get("token").cloned().expect("a `token` member")
}

/// §17.5/§17.2: the ring is bootstrapped over the INJECTED provider — its active
/// version carries a genuine 32-byte Ed25519 public key (the sim double it
/// replaces would never be reached), and a `cose.sign` token verifies against the
/// ring's accepted versions.
#[test]
fn injected_ed25519_signs_and_verifies() {
    let mut engine = injected_engine();
    let mut g = generator();

    let active = {
        let current = engine.keyring("session_keys").expect("ring").current().expect("active version");
        let Value::Bytes(public) = current.public_key() else {
            panic!("an Ed25519 public key is raw bytes");
        };
        assert_eq!(public.as_slice().len(), 32, "a raw Ed25519 public key is 32 bytes (real provider, not sim)");
        current.id()
    };

    let token = login_token(&mut engine, &mut g);
    let (claims, version) = engine.cose_verify("session_keys", &token).expect("the injected token verifies");
    assert_eq!(version, active, "verification reports the signing version identity");
    let Value::Struct(fields) = &claims else { panic!("claims are a struct") };
    assert_eq!(fields.get("auth"), Some(&Value::Text(Text::new("session"))));
}

/// §17.7: a token minted by a SIM-backed engine is DENIED by the injected ring —
/// its version ordinal is accepted, but its signature does not verify under the
/// injected ring's Ed25519 public key. This is the security property Task 5
/// restores: the injected key is not the publicly-reconstructable sim key, so a
/// sim-minted (forgeable) token cannot authenticate against a real deployment.
#[test]
fn sim_signed_token_is_denied_by_injected_ring() {
    let injected = injected_engine();

    // A sim-backed engine over the identical package (no registered provider).
    let mut sim_g = generator();
    let mut sim = Engine::load(MemoryStore::new(InstanceId::new("sim")), LOGIN, &mut sim_g).expect("sim load");
    let sim_token = login_token(&mut sim, &mut sim_g);

    // Both rings bootstrapped version 1, so the ordinal is accepted — only the
    // signature check separates them.
    assert!(matches!(
        injected.cose_verify("session_keys", &sim_token),
        Err(CoseVerifyError::ClaimsTampered),
    ));
    // Sanity: the sim ring verifies its own sim-signed token.
    assert!(sim.cose_verify("session_keys", &sim_token).is_ok());
}

/// §17.7: a tampered claim set (the genuine signature repackaged under altered
/// claims) no longer matches the signed payload over the injected ring's key.
#[test]
fn tampered_injected_token_is_denied() {
    let mut engine = injected_engine();
    let mut g = generator();
    let token = login_token(&mut engine, &mut g);

    // Repackage the genuine signature under a forged claim set (§17.7).
    let parsed = CoseToken::from_value(&token).expect("a valid cose token");
    let forged = CoseToken::new(
        parsed.ring(),
        parsed.version(),
        CoseClaims::new([(Text::new("auth"), Value::Text(Text::new("forged")))]),
        parsed.signature().to_vec(),
    );
    assert!(matches!(
        engine.cose_verify("session_keys", &forged.to_value()),
        Err(CoseVerifyError::ClaimsTampered),
    ));
}

/// §17.3/§17.4: clock-driven rotation works identically over the injected Ed25519
/// provider — advancing past the P30D cadence retires version 1 and activates a
/// freshly GENERATED Ed25519 version 2, while v1 stays accepted inside `$retain`.
#[test]
fn rotation_over_injected_ed25519() {
    let mut engine = injected_engine();

    let v1 = engine.keyring("session_keys").expect("ring").current().expect("active").id();

    // Cross the cadence boundary: the lazy rotation runs before the next read.
    engine.set_time(Timestamp::new(NOW + P30D + 1, Precision::Micros));

    let ring = engine.keyring("session_keys").expect("ring");
    let v2 = ring.current().expect("a new active version after rotation").id();
    assert_ne!(v2, v1, "rotation activated a new version over the injected provider");

    // The new active version is a genuine 32-byte Ed25519 key (a real generate).
    let Value::Bytes(public) = ring.current().unwrap().public_key() else {
        panic!("the rotated version's public key is bytes");
    };
    assert_eq!(public.as_slice().len(), 32);

    // v1 retired but still accepted inside the 45-day retain window (§17.4).
    let now = Timestamp::new(NOW + P30D + 1, Precision::Micros);
    let v1_meta = ring.versions().iter().find(|v| v.id() == v1).expect("v1 retained");
    assert_eq!(v1_meta.state(), KeyState::Retired);
    assert!(ring.accepted(now).iter().any(|v| v.id() == v1), "retired v1 stays accepted within $retain");
}

/// Honesty rule: an injected provider that cannot fulfil the declared policy is a
/// LOUD load failure — never a silent downgrade to the sim double. The software
/// Ed25519 provider advertises only `Ed25519`, so backing an `ES256` ring rejects
/// loading with an [`EngineError::Keyring`] (§17.6).
#[test]
fn incompatible_injected_provider_fails_load_loudly() {
    let store = MemoryStore::new(InstanceId::new("incompatible"));
    let mut g = generator();
    let result = Engine::load_with_hosts(store, LOGIN_ES256, &mut g, ed25519_registry());
    let error = match result {
        Ok(_) => panic!("an incompatible injected provider must reject loading, but it loaded"),
        Err(error) => error,
    };
    assert!(
        matches!(error, EngineError::Keyring(_)),
        "capability shortfall must be a loud keyring load error, got {error}",
    );
}

/// The default path is unchanged: a package loaded with no registered provider
/// still self-provisions the sim double and signs/verifies exactly as before.
#[test]
fn unregistered_provider_keeps_sim_default() {
    let mut g = generator();
    let mut engine = Engine::load(MemoryStore::new(InstanceId::new("sim-default")), LOGIN, &mut g).expect("sim load");
    assert!(engine.keyring("session_keys").expect("ring").current().is_some());
    let token = login_token(&mut engine, &mut g);
    assert!(engine.cose_verify("session_keys", &token).is_ok());
}
