# Bilani blocker: committed blobs become unknown after engine reopen

> **Delete this file in the fixing commit once the regression test passes.**

## Status

This is an actionable Liasse runtime bug blocking Bilani's migration of generated-document metadata to the PostgreSQL-backed Liasse store. It is not a Bilani routing or storage-configuration issue.

After a blob mutation commits, application rows and history survive `Engine::reopen_with_hosts`, and the connector still contains the verified bytes. The reopened engine nevertheless refuses to serve the committed descriptor:

```text
no committed blob is known for that descriptor
```

## Reproduction from Bilani

Bilani currently has the failing restart gate at `server/tests/postgres_documents.rs`. It uses a unique PostgreSQL Liasse instance plus an on-disk `FsConnector`, so both application state and physical bytes survive engine destruction.

From the Bilani checkout:

```bash
cd /home/frederic/IdeaProjects/bilani
BILANI_TEST_PG_DSN=$(sed -n 's/^DATABASE_URL=//p' .env) \
  cargo test -p server --test postgres_documents -- --nocapture
```

Observed sequence:

1. Boot the documents package with `PgStore` and `FsConnector`.
2. Stage and commit a PDF through `call_with_blob`.
3. Read and retain the committed head and SHA-512 descriptor.
4. Drop the engine.
5. Reopen the same package instance, PostgreSQL namespace, and filesystem root.
6. Confirm that the installed release and committed head were recovered.
7. Call `fetch_blob` with the committed digest.
8. The call fails with `no committed blob is known for that descriptor`.

The essential assertion is:

```rust
let head = first.documents().head().await?;
drop(first);

let (reopened, provisioning) =
    templates::boot(documents, &dsn, &namespace, blobs.path()).await?;
assert!(provisioning.installed.is_some());
assert_eq!(reopened.documents().head().await?, head);

let digest = Sha512::parse(&landed.sha512)?;
assert_eq!(reopened.documents().fetch_blob(digest).await?, bytes);
```

The first two assertions pass. The final fetch fails.

## Liasse-local regression

Add the durable-reopen case beside `package_declared_ingress_round_trips_through_serve_store` in `crates/liasse-runtime/tests/blob_connector_registry.rs`:

1. Load `PACKAGE` with a store whose state can be handed to a second engine.
2. Register a connector whose physical object map is shared by both registries, or use an on-disk connector rooted in one temporary directory.
3. Commit `add(..., bytes, ...)` and confirm `fetch_blob` works before restart.
4. Recover the store with `Engine::into_store()` and destroy the first engine/registry.
5. Build a fresh registry over the same connector bytes.
6. Reopen with `Engine::reopen_with_hosts(store, PACKAGE, ...)`.
7. Assert the reopened head equals the previous head and `fetch_blob(&descriptor)` returns the exact original bytes.

In compact form, the missing invariant is:

```rust
let mut first = Engine::load_with_hosts(store, PACKAGE, &mut generator(), registry_a)?;
let (outcome, descriptor) = add(&mut first, "d1", bytes, "payload.bin")?;
assert!(matches!(outcome, CallOutcome::Committed { .. }));
assert_eq!(first.fetch_blob(&descriptor)?.bytes(), bytes);
let head = first.head()?;
let store = first.into_store();

let reopened =
    Engine::reopen_with_hosts(store, PACKAGE, &mut generator(), registry_b_same_bytes)?;
assert_eq!(reopened.head()?, head);
assert_eq!(reopened.fetch_blob(&descriptor)?.bytes(), bytes);
```

`SimConnector` currently owns a private, non-shared object map. A small test-only `BlobConnector` backed by `Arc<Mutex<BTreeMap<Sha512, Vec<u8>>>>`, or a `liasse-blob-fs` dev dependency, makes `registry_b_same_bytes` model a real connector across process restart.

## Cause

`Engine` keeps two pieces of blob authority only in memory:

- `blob_catalog`, which distinguishes blobs committed into application state from merely staged/orphaned connector objects and maps logical stores to connectors;
- `blob_placements`, which supplies `$stored`, `$satisfied`, and `$surplus` facts.

`Engine::reopen_with_hosts` reconstructs neither. In `crates/liasse-runtime/src/engine.rs` it creates both as empty defaults:

```rust
blob_placements: crate::env::BlobPlacements::default(),
blob_catalog: BlobCatalog::default(),
```

During a live admission, `BlobCatalog::commit(staged)` is the only path that populates the serve catalog. `fetch_blob_by_digest` then delegates to `BlobCatalog::fetch_digest`, whose empty-catalog result is `FetchError::Unknown`. Persisted rows can therefore contain valid committed blob descriptors that become unserveable solely because the host process restarted.

## Required behavior

On durable reopen, Liasse must reconstruct or restore the committed blob catalog and placement facts before the engine becomes active. The implementation must preserve the existing security boundary:

- do not authorize every object merely because a connector reports it;
- only blobs that crossed the application-state commit boundary become serveable;
- verify connector content against the descriptor digest before serving;
- restore connector/store routing and `$stored`, `$satisfied`, and `$surplus` consistently;
- rejected/staged/orphaned objects remain absent from the committed catalog.

The durable representation may persist catalog/placement facts atomically with the application transition, or reopen may deterministically rebuild them from committed blob-bearing state and re-verify registered connectors. A Bilani-side cache or retry would conceal the invariant violation and is not an acceptable fix.

## Acceptance criteria

- A Liasse integration test reproduces the failure with a fresh engine and fresh registry over durable/shared connector bytes.
- The test passes after the runtime fix and proves exact-byte fetch after reopen.
- Existing rejected-ingress, deduplication, tamper, unavailable-connector, and placement-expression tests remain green.
- The Bilani command above passes without a Bilani workaround.
- **This handoff file is deleted in the same commit that fixes the bug.**
