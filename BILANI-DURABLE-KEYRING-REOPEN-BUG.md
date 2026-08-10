# Bilani blocker: a managed keyring changes identity after engine reopen

> **Delete this file in the fixing commit once the regression test passes.**

## Status

This is an actionable Liasse runtime and storage-boundary bug blocking Bilani from booting a generic host from a serialized `.liasse` state with only a Liasse PostgreSQL backing connection.

A package keyring is declared and managed by Liasse, but its live lifecycle and private key material are not durable Liasse state. `Engine::reopen_with_hosts` bootstraps the declared ring again. With a fresh `Ed25519KeyProvider`, the reopened ring receives a new key while reusing version identity `1`. Tokens minted before restart no longer verify.

Bilani currently hides this by implementing its own PostgreSQL key vault in `server/src/engine/pg_vault.rs`. That makes the application framework access PostgreSQL outside the Liasse store contract. It also means a serialized Liasse state is not sufficient to restore the application. The workaround must be deleted, not preserved as the architecture.

## Minimal reproduction

Add this test to `crates/liasse-runtime/tests/injected_keyring.rs`, where `LOGIN`, `generator`, `ed25519_registry`, `injected_engine`, and `login_token` already exist:

```rust
#[test]
fn injected_keyring_identity_survives_reopen() {
    let mut first = injected_engine();
    let mut g = generator();
    let before = first
        .keyring("session_keys")
        .expect("ring")
        .current()
        .expect("active version")
        .public_key()
        .clone();
    let token = login_token(&mut first, &mut g);
    assert!(first.cose_verify("session_keys", &token).is_ok());

    let store = first.into_store();
    let reopened = Engine::reopen_with_hosts(store, LOGIN, &mut g, ed25519_registry())
        .expect("reopen");
    let after = reopened
        .keyring("session_keys")
        .expect("ring")
        .current()
        .expect("active version")
        .public_key()
        .clone();

    assert_eq!(after, before, "restart preserves the managed signing identity");
    assert!(
        reopened.cose_verify("session_keys", &token).is_ok(),
        "a token minted before restart remains valid"
    );
}
```

Run it with:

```bash
cd /home/frederic/IdeaProjects/liasse-bilani-unblock
cargo test -p liasse-runtime --test injected_keyring injected_keyring_identity_survives_reopen -- --nocapture
```

The public-key equality fails today. If that assertion is temporarily removed, verification of the pre-restart token fails because the reopened ring was bootstrapped over a different key.

The same behavior was independently reproduced from Bilani with its real platform package and two fresh ephemeral provider registries. `Engine::reopen_with_hosts` succeeded, but the active public key changed across the restart.

## Cause

Keyring state is reconstructed as process state instead of restored as durable engine state:

- `Engine` owns `Vec<Keyring<EngineKeyProvider>>` in memory.
- package loading calls `Keyring::load` and `bootstrap` for each declaration;
- `Engine::into_store` returns only the application store;
- `Engine::reopen_with_hosts` loads and bootstraps the keyrings again;
- `Ed25519KeyProvider::new` uses `EphemeralVault`, so its private material is gone after process exit;
- `PgStore` persists application state and history, but Liasse exposes no store-backed key provider or managed keyring persistence path.

This makes the public version number misleading after restart. Version `1` exists before and after, but it identifies different cryptographic material.

## Required behavior

A Liasse application restored from an artifact or reopened from a durable Liasse store must preserve its managed keyring lifecycle and signing identity. The generic host must be able to provide its PostgreSQL backing connection to Liasse without implementing a second private PostgreSQL schema or reading and writing that database itself.

The fix may persist managed keyring state and protected provider material through the Liasse storage abstraction, or expose a Liasse-owned durable provider constructed from the same backing store. The security design must preserve these invariants:

- private key material is never exposed as application-readable state;
- a restart preserves active and retained key versions exactly;
- pre-restart tokens remain verifiable until their declared retention or revocation boundary;
- rotation, disable, revoke, and destroy survive restart;
- the same numeric version never silently names different key material;
- restoration refuses missing or corrupt protected key material instead of bootstrapping a replacement;
- the generic host does not issue SQL or own a parallel PostgreSQL schema.

## Acceptance criteria

- The Liasse regression above passes with a fresh engine and fresh host registry.
- A PostgreSQL-backed integration test destroys the first engine and all provider objects, reopens through a Liasse-owned API, and verifies a token minted before restart.
- The test proves active version identity, public key, retention state, and provider handle continuity.
- Missing or corrupt durable provider material is a loud reopen failure and never creates a replacement key.
- Bilani can delete `server/src/engine/pg_vault.rs` and provide only its PostgreSQL connection to Liasse.
- Existing keyring rotation, provider failure, artifact restoration, and store conformance suites remain green.
- **This handoff file is deleted in the same commit that fixes the bug.**
