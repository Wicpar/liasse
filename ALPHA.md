# Alpha scope

The alpha is the first lean core release. [SPEC.md](SPEC.md), [LANGUAGE.md](LANGUAGE.md), and [BUILTINS.md](BUILTINS.md) define its behavior, independent of any implementation. This file maps the crates of the alpha, core and outside the core, states what each part must do, and gives the alpha's implementation choices: the Rust API of the host, CBOR as the encoding, and the host implementations. [POSTGRES.md](POSTGRES.md) is the architecture of the PostgreSQL backend, and [CONNECT.md](CONNECT.md) the client binding and the web client.

## The boundary

The **core** is the engine that SPEC, LANGUAGE, and BUILTINS define: the language, values and the artifact format, the runtime semantics, the builtins, and three contracts. Everything else is **outside the core** and plugs into one of the contracts:

| Contract | What plugs in |
| --- | --- |
| `liasse-store` | storage backends |
| `liasse-host` | host-key providers, blob connectors, the implementations of the mutations of the host's own libraries |
| `liasse-wire` | client bindings |

A core crate depends only on core crates. A crate outside the core depends on the core. The core states what happens; how a backend makes it happen, such as indexes, query plans, isolation levels, and where evaluation runs, belongs to the backend.

The crates, and the web client's TypeScript shell, stand in bands from the applications down to the foundation. Each depends on the crates after its arrow, all in bands below it:

```text
┌─ applications ─────────────────────────────────────────────────────────────┐
│ host                 the program that embeds Liasse                        │
│                      → liasse, the implementations that it picks           │
│ liasse-cli           builds and migrates source folders                    │
│                      → model, artifact                                     │
│ liasse-testkit       runs the conformance corpus                           │
│                      → liasse, liasse-pg, liasse-store-memory              │
├─ implementations of the contracts ─────────────────────────────────────────┤
│ liasse-pg            the PostgreSQL store                                  │
│                      → store, host, model, cbor                            │
│ liasse-pg-ext        Liasse evaluation inside PostgreSQL                   │
│                      → eval, cbor                                          │
│ liasse-store-memory  the in-process store, the test oracle                 │
│                      → store, host, eval                                   │
│ liasse-keys          the keyring library: keyrings, sealed keys, transfer  │
│                      → store, host                                         │
│ liasse-key-file      a host-key provider of liasse-keys                    │
│                      → host                                                │
│ liasse-blob-fs       the filesystem blob connector                         │
│                      → host                                                │
│ liasse-connect       the SSE and HTTPS binding                             │
│                      → wire, cbor                                          │
│ liasse-connect-wasm  the web client core                                   │
│                      → wire, cbor                                          │
│ clients/connect      the web client's TypeScript shell                     │
│                      → liasse-connect-wasm                                 │
└────────────────────────────────────────────────────────────────────────────┘
                             ▼  core  ▼
┌─ host ─────────────────────────────────────────────────────────────────────┐
│ liasse               the context, native access, reflection                │
│                      → surface, runtime, blob, artifact, store, host       │
├─ clients ──────────────────────────────────────────────────────────────────┤
│ liasse-surface       connections, entry points, reads, calls               │
│                      → runtime, blob, store, wire                          │
├─ runtime ──────────────────────────────────────────────────────────────────┤
│ liasse-runtime       the lifecycle, targets and updates, host calls        │
│                      → model, artifact, store, host, blob                  │
├─ services ─────────────────────────────────────────────────────────────────┤
│ liasse-blob          descriptors, uploads, storage classes, retention      │
│                      → store, host                                         │
├─ contracts ────────────────────────────────────────────────────────────────┤
│ liasse-store         the store contract and the backend functions          │
│                      → model                                               │
│ liasse-host          the host-key, connector, host-call, effect contracts  │
│                      → value                                               │
│ liasse-wire          operations, outcomes, frames, patches                 │
│                      → value                                               │
├─ language ─────────────────────────────────────────────────────────────────┤
│ liasse-eval          evaluation of views, programs, and constraints        │
│                      → model, std                                          │
│ liasse-model         resolution, elaboration, typing, and checks           │
│                      → syntax, std, path                                   │
│ liasse-std           the builtins of each language version                 │
│                      → value                                               │
│ liasse-syntax        the grammar of LANGUAGE Annex A                       │
│                      → diag                                                │
├─ formats ──────────────────────────────────────────────────────────────────┤
│ liasse-artifact      packages, exports, and history records                │
│                      → cbor, path                                          │
│ liasse-cbor          the alpha's encoding of values                        │
│                      → value                                               │
├─ foundation ───────────────────────────────────────────────────────────────┤
│ liasse-value         values, their order, limits, and arithmetic           │
│ liasse-path          declaration, display, and position paths              │
│ liasse-diag          diagnostics: spans, codes, and rendering              │
│                      → path                                                │
└────────────────────────────────────────────────────────────────────────────┘
```

- The stores evaluate, with `liasse-eval`: `liasse-pg-ext` inside PostgreSQL, and `liasse-store-memory` in process. The runtime pilots them.
- The web client reaches the core through `liasse-wire` and `liasse-cbor` alone.

| Crate | Implements |
| --- | --- |
| `liasse` | SPEC chapters 8 and 9 |
| `liasse-surface` | SPEC chapter 10, CONNECT chapter 2 |
| `liasse-runtime` | SPEC §3.7, the host calls of §6.4, chapter 9, §11.2, chapter 13, §14.2, §14.5 |
| `liasse-keys` | the keyring library (below) |
| `liasse-blob` | SPEC chapter 12 |
| `liasse-store` | the store contract, and the declarations of the backend functions (below) |
| `liasse-host` | SPEC §3.7, §6.4, §12.3 |
| `liasse-wire` | the messages of CONNECT chapter 2 |
| `liasse-eval` | LANGUAGE chapters 5 to 10 and 15, SPEC chapters 4 to 6, §11.3, §11.4, §13.4, §14.4 |
| `liasse-model` | LANGUAGE chapters 2 to 4 and 9 to 16, SPEC §3.6, §3.7, §6.4 |
| `liasse-std` | BUILTINS, and the further host libraries (below) |
| `liasse-syntax` | LANGUAGE Annex A |
| `liasse-artifact` | SPEC §3.5, §14.5, Annex D |
| `liasse-cbor` | the CBOR encoding (below) |
| `liasse-value` | SPEC Annex A and B, BUILTINS §1 |
| `liasse-path` | SPEC Annex C |
| `liasse-diag` | SPEC chapter 7 |

## Core

Requirements:

- Every observable behavior follows SPEC, LANGUAGE, and BUILTINS, and the conformance corpus passes on every backend.
- The parser is built on `pest`, and every build error (SPEC §3.6) is a rustc-like diagnostic that also says why.
- The alpha's encoding is CBOR (below): the records of a dehydrated application and the messages of `liasse-connect` are CBOR.
- The build derives the **footprint** of every mutation, view, constraint, and unique key: the places that it reads and writes, as paths from its receiver, its arguments, and the values it reads. In SPEC §1.2, `complete()` changes `done` of its receiver; `add(title)` inserts one row into `/tasks`; and `member(session)` reads the `/sessions` row by `public_key` and the `.accounts` row that it names. Backends use footprints to derive their access paths and indexes, to track conflicts at key precision, and to advance only the live views that a commit touches.
- The alpha declares `history.at`, `modules.init`, and `modules.update` asynchronous, and the other builtins synchronous (SPEC §6.4).
- The build infers the class of each call of a `fn` and of a `mut` (SPEC §6.4), and checks that each call stands where its class does. A call placed elsewhere is a build error that names the call inside the `fn` or the `mut` that gives it its class, and so is a migration program's call of a mutation of its own module that reaches an effect.
- The runtime searches for the update that holds each instance whose active version differs from its target (SPEC §13.1), and starts the updates in parallel: an update whose checks read an instance that an update nearer the root moves starts after that one. For an instance that no update moves to its target, it searches the versions of its route from the target back toward its active version, and moves it to the first that passes. It judges first the checks that read versions alone: builds, routes, host dependencies, shapes, provided contexts, and the graph of calls. A check between two instances that fails with one of them at its target adds the other, when the other's target differs from the package of its active version. The set that these checks close runs its routes and judges its result once. A failing check that reads further instances adds those whose targets differ; a member whose failing checks read it alone leaves the set, with the members that need it; and a set that passes is reduced, one member at a time, to the members that it needs. When these steps end on a failing set, the runtime judges the subsets of the instances that its failing checks read, smallest first, up to the host's bound on the search.
- The core computes the results that the language version fixes (SPEC §6.4) with its own Unicode 16.0.0 tables (BUILTINS §4.1) and its own decimal arithmetic (BUILTINS §1.3) alone.
- A commit that changes state writes its changes (SPEC §14.1) with its places, in one transaction. Restore and a past-row load apply the changes of history in the order of the commits' identities (SPEC §14.2), and a past-row load evaluates the routes that carry the past state to the instance's active version alone (SPEC §14.4).
- Programs run by their dependencies. The build derives, from footprints and bindings, which statements of a program depend on which: one reads a binding or a result that the other gives, reads a place that the other writes, or writes a place that the other reads or writes. A run starts each host call once its arguments are evaluated, runs every statement whose dependencies have run, and waits only where a pending result is needed, so the independent host calls of one run are performed together, in one round (SPEC §6.4). When several statements fail, the run reports the first in statement order, as a run in order does.
- The build warns when a constraint's condition reads nothing through `.` or `^`, and names the body that it belongs in.
- A backend receives the elaborated declarations (SPEC §3.6). Elaboration composes typed declarations: `factor = 1 + 2` substituted into `10 * factor` is 30. A template's documentation stays distinct from an application's, and shapes are compared after elaboration.
- The host builds one context: it picks one backend, registers the blob connectors and its own host libraries, each with what it serves and one implementation of each of its opaque mutations, whose opaque functions the store implements, defines storage classes, and binds them (SPEC §3.7, §8.1, §12.3). It sets the expiry of the access data in blob plans, the bytes that the upload plans of one connection place ([CONNECT.md](CONNECT.md) 2.5), and the bound on the work of each read of a view that a client writes (SPEC §10.2).
- A blob's digest (SPEC §12.1) is the 64-byte SHA-512 digest of its content; a digest of another length makes the descriptor malformed.

### The host in Rust

The host of SPEC §1.2:

```rust
let blobs = liasse_blob_fs::Connector::new("/var/lib/acme/blobs")
    .presigned(blob_base_url, blob_signing_key);

let liasse = Liasse::builder()
    .store(liasse_pg::connect(&database_url).await?)
    .blob_connector("fs", blobs.clone())
    .storage_class("default", StorageClass::copies(1).location("fs"))
    .access_expiry(Duration::from_secs(900))
    .upload_limit(1 << 30)
    .build()
    .await?;

let app = match liasse.open().await? {
    Some(app) => app,
    None => liasse.create(Package::read("acme.tasks.liasse.zip")?).await?,
};
tokio::try_join!(
    liasse_connect::serve(&app, listen_address, authorize),
    blobs.serve(blob_listen_address),
)?;
```

`authorize` is the host's own function: it sees each request that opens a connection, and returns the views of the root instance, each through a shape of the module, that the connection starts with, as `Guest` for a client of SPEC §1.2 (SPEC §10.1). A native transaction (SPEC §8.2) is a closure, which the host runs once for each run of the transaction. It calls an effect of its host libraries with `tx.effect(name, arguments)`, which keeps the effect with the run; the effect is committed with the run that commits, and dropped with every other run.

### Backend functions

The store evaluates every function, and implements the opaque functions of the host's own libraries (SPEC §3.7). `liasse-pg` and `liasse-store-memory` implement the **backend functions**, which `liasse-store` declares as a host library that serves every module and clients, each with its signature:

| Signature | Description |
| --- | --- |
| `fn filter<V>(items: V*, keep: V -> Bool): items?!` | The elements for which `keep` holds: the selection `items[keep(.)]`. |
| `fn group<V, K, R>(items: V*, key: V -> K, each: { key: K; items: V+ } -> R): items \|> R` | One element for each distinct `key`: the value of `each` for the record of that key and its elements, with the key as identity: `group(.customer) { customer: .key, total: sum(.items.total) }`. |
| `fn distinct<V, A>(items: V*, by: V -> A = .): items` | One element of each group of elements of equal `by`, the first in the order of the `sort` before it, else in insertion order: `distinct(.id)`. |
| `fn union<V, A>(items: V*, other: V*, by: V -> A = .): V*` | The elements, and the elements of `other` whose `by` equals that of none of them. |
| `fn intersection<V, A>(items: V*, other: V*, by: V -> A = .): items?!` | The elements whose `by` equals that of an element of `other`. |
| `fn difference<V, A>(items: V*, other: V*, by: V -> A = .): items?!` | The elements whose `by` equals that of no element of `other`. |
| `fn count<V>(items: V*): Int` | The number of elements. |
| `fn sum<A + A: A>(items: A+): A`, `fn sum(none): none` | The elements added with `+`, in the order of the `sort` before it, else in insertion order: `sum(.lines.amount) ?? 0.0` is zero for no lines. |
| `fn avg(items: Decimal+): Decimal`, `fn avg(none): none` | The mean: the sum of the elements divided by their number as BUILTINS §1.3 divides a `Decimal`. |
| `fn min<V>(items: V+): V`, `fn min(none): none` | The least element. |
| `fn max<V>(items: V+): V`, `fn max(none): none` | The greatest element. |
| `fn one<V>(items: V*): V` | The one element of `items`; any other number of elements rejects the evaluation with the code `evaluation` (SPEC chapter 7). |
| `fn sort<V, A>(items: V*, key: V -> A = .): items` | The elements, ordered by their `key` in the order of its type (SPEC Annex B), and elements of equal keys in insertion order. Several keys form one tuple key (SPEC Annex B.2): `sort((desc(.created_at), .id))`. |
| `fn desc<V>(x: V): V` | `x`, as a `sort` key, or a place of a tuple key, that orders in reverse. |
| `fn skip<V>(items: V*, n: Int): items?!` | The elements after the first `n`, in the order of the `sort` before it, else in insertion order. |
| `fn take<V>(items: V*, n: Int): items?!` | The first `n` elements, in the order of the `sort` before it, else in insertion order. |

- `filter`, `distinct`, `union`, `intersection`, `difference`, `skip`, and `take` keep every element as it is.
- A `group` key is of a key-eligible type (SPEC Annex A.3), or a record or a tuple of them: `.customer`, `{ day: time.date(.at), .kind }`.
- `by` is written at the level of each element of the operands, which share what it reads.
- A call after a `sort` that keeps every element as it is keeps its order, and a read gives the elements of its result in that order.
- **Insertion order** (SPEC §4.3) orders elements by the rows that they come from, oldest first, and the elements that come from no row by their values (SPEC Annex B).

### Further host libraries

`liasse-std` also provides these host libraries, which the host injects into every module and which serve clients (SPEC §3.7). Each signature reads as in BUILTINS, and a time zone is an IANA name that the host's time zone database holds; another name rejects the evaluation with the code `evaluation`.

| Signature | Description |
| --- | --- |
| `time.unix(t: Timestamp): Int` | The whole seconds from `1970-01-01T00:00:00Z` to `t`, rounded toward negative infinity. |
| `time.from_unix(seconds: Int): Timestamp` | The instant `seconds` after `1970-01-01T00:00:00Z`. |
| `time.date(t: Timestamp): Date` | The date of `t` in UTC. |
| `time.start(date: Date): Timestamp` | The instant at which `date` starts in UTC. |
| `time.add_days(date: Date, days: Int): Date` | The date `days` days after `date`, or before it for a negative `days`. |
| `time.seconds(d: Duration): Int` | The whole seconds of `d`, rounded toward zero. |
| `time.from_seconds(seconds: Int): Duration` | The duration of `seconds` seconds. |
| `mut time.local_date(t: Timestamp, zone: Text): Date` | The date of `t` in the time zone `zone`, read from the host's time zone database. |
| `mut time.local_start(date: Date, zone: Text): Timestamp` | The instant at which `date` starts in the time zone `zone`, read from the host's time zone database. |
| `hex.encode(data: Bytes): Text` | The two lower-case hexadecimal digits of each byte of `data`. |
| `hex.decode(text: Text): Bytes?` | The bytes that `text` spells with two hexadecimal digits of either case for each byte; `none` for any other text. |
| `base64.encode(data: Bytes): Text` | `data` in base64 with padding (RFC 4648 §4). |
| `base64.decode(text: Text): Bytes?` | The bytes that the padded base64 text `text` spells; `none` for any other text. |
| `base64.url_encode(data: Bytes): Text` | `data` in base64url without padding (RFC 4648 §5). |
| `base64.url_decode(text: Text): Bytes?` | The bytes that the unpadded base64url text `text` spells; `none` for any other text. |
| `ecdsa.p256_from_der(der: Bytes): Bytes?` | The 64-byte P-256 signature that the DER encoding `der` of an `ECDSA-Sig-Value` (RFC 3279 §2.2.3) holds; `none` when `der` is not the DER encoding of two integers from 1 to the group order minus 1. |
| `ecdsa.p384_from_der(der: Bytes): Bytes?` | The 96-byte P-384 signature that the DER encoding `der` of an `ECDSA-Sig-Value` holds; `none` otherwise, as for P-256. |
| `rsa.pkcs1_sha256_verify(modulus: Bytes, exponent: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an RSASSA-PKCS1-v1_5 signature (RFC 8017 §8.2.2) with SHA-256 of `message` under the public key `(modulus, exponent)`. |
| `rsa.pss_sha256_verify(modulus: Bytes, exponent: Bytes, message: Bytes, signature: Bytes): Bool` | `signature` is an RSASSA-PSS signature (RFC 8017 §8.1.2) with SHA-256, MGF1 with SHA-256, and a 32-byte salt, of `message` under the public key `(modulus, exponent)`. |
| `webauthn.credential(rp_id: Text, origins: Text*, challenge: Bytes, client_data: Bytes, attestation_object: Bytes): { credential: Bytes; public_key: Bytes; counter: Int; user_verified: Bool }?` | The credential that a WebAuthn registration creates (WebAuthn Level 3 §7.1): its credential ID, its public key as a COSE_Key, its signature counter, and whether the authenticator verified the user; `none` when the registration fails to verify. |
| `webauthn.verify(public_key: Bytes, rp_id: Text, origins: Text*, challenge: Bytes, client_data: Bytes, authenticator_data: Bytes, signature: Bytes): { counter: Int; user_verified: Bool }?` | The signature counter of a WebAuthn assertion under the credential public key `public_key` (WebAuthn Level 3 §7.2), and whether the authenticator verified the user; `none` when the assertion fails to verify. |

Every verification gives `false`, or `none` for WebAuthn, for every invalid signature, a malformed key or signature included. The private keys, ECDSA public keys, and ECDSA signatures are those of BUILTINS §4.2.

- **RSA keys** are the modulus and the public exponent as big-endian unsigned integers, the form of JWK and COSE: a modulus of 2048 to 4096 bits, and an odd exponent from 3 to 2^32 − 1. A signature has the byte length of the modulus.
- **WebAuthn** registrations and assertions verify when:
  - `client_data` is UTF-8 JSON whose `type` is `webauthn.create` for a registration and `webauthn.get` for an assertion, whose `challenge` is `challenge` in unpadded base64url, whose `origin` is one of `origins`, and whose `crossOrigin`, when present, is `false`;
  - the authenticator data, `authenticator_data` or the `authData` of the CBOR map `attestation_object`, starts with the SHA-256 digest of `rp_id` in UTF-8, and its flags have user presence set; a registration reads the attested credential data of `authData`, and passes over `fmt` and `attStmt`;
  - the credential public key is a COSE_Key (RFC 9052 §7) of the algorithm ES256 (ECDSA over P-256 with SHA-256), EdDSA (Ed25519), or RS256 (RSASSA-PKCS1-v1_5 with SHA-256, under the RSA keys above);
  - an assertion's `signature` signs `authenticator_data` followed by the SHA-256 digest of `client_data`: in DER for ES256, as authenticators give it, in 64 bytes for EdDSA, and in the modulus's length for RS256.

  `counter` is the signature counter of the authenticator data, and `user_verified` its user verification flag.

### Host libraries

The host gives the builder its own host libraries (SPEC §3.7), each with what it serves and one implementation of each opaque mutation that it declares:

```rust
let liasse = Liasse::builder()
    // the store and the blob connectors, as above
    .host_library(
        Library::read("acme.sys.liasse")?
            .host_call("rates.quote", rates)
            .effect_performer("mail.send", mailer),
        Serves::modules(),
    )
    .host_library(
        Library::read("billing.sys.liasse")?.host_call("tax.rate", tax),
        Serves::matching(["acme.billing@>=2.1.0, <3.0.0", "acme.invoices"])?,
    )
    .effect_attempts(8)
    .build()
    .await?;
```

An opaque mutation whose signature has a result type is drawn, and its implementation is a `HostCall`; one whose signature ends with its parameters is an effect, and its implementation is an `EffectPerformer` (SPEC §6.4). Both are `liasse-host` traits:

```rust
#[async_trait]
pub trait HostCall: Send + Sync {
    async fn call(&self, arguments: &Value) -> Result<Value>;
}

#[async_trait]
pub trait EffectPerformer: Send + Sync {
    async fn perform(&self, id: &EffectId, arguments: &Value) -> Result<()>;
}
```

- `Serves::modules()` makes a library serve every module of the application, `Serves::modules_and_clients()` every module and clients, and `Serves::matching(selectors)` the modules that one of its selectors names: `acme.invoices` at every version, `acme.invoices@1.2.0` at one version, and `acme.billing@>=2.1.0, <3.0.0` at the versions that every comparator of the list accepts, among `>`, `>=`, `<`, and `<=`.
- `build()` checks that each library parses, that each of its opaque mutations has one implementation of its kind, that each implementation names an opaque mutation of that library, and that the store implements each of its opaque functions with its signature, and reports each failed check as a diagnostic.
- `arguments` is the record of a call's arguments, by parameter name. `call` gives a value of the mutation's result type; any other result, and an error, reject the transaction with the code `host`.
- `open()` and each activation check the host dependencies of every version that they build (SPEC §3.7), and the diagnostic of a failed check gives the recorded declaration. The check compares a recorded definition with the library's by their syntax trees, and leaves documentation comments out of the comparison.
- `id` is the same at each attempt of one effect and distinct from every other effect's. A performer passes it to its receiver as the receiver's idempotency key, such as an HTTP `Idempotency-Key` header, so that the receiver acts once.
- An error fails the attempt, and the host attempts again after a backoff, up to the bound that `.effect_attempts(n)` sets. An effect past the bound on attempts is reported on `liasse.failed_effects()`, a stream of each effect's identifier, mutation name, arguments, and last error, and leaves the store.

A blob connector (SPEC §12.3) stores content under its digest, delivers it whole or by byte range, gives access data with which a client reads or writes content directly, copies, deletes, and reports what a location holds, through the `liasse-host` trait:

```rust
#[async_trait]
pub trait BlobConnector: Send + Sync {
    async fn put(&self, at: &Location, digest: &Digest, content: ContentStream) -> Result<()>;
    async fn get(&self, at: &Location, key: &Key, range: Option<ByteRange>)
        -> Result<ContentStream>;
    async fn read_access(&self, at: &Location, digest: &Digest, served: &Served,
        expires_in: Duration) -> Result<Option<AccessData>>;
    async fn write_access(&self, at: &Location, place: &Place, size: u64,
        expires_in: Duration) -> Result<Option<AccessData>>;
    async fn keep(&self, at: &Location, place: &Place, digest: &Digest) -> Result<()>;
    async fn copy(&self, from: &Location, to: &Location, digest: &Digest) -> Result<()>;
    async fn delete(&self, at: &Location, key: &Key) -> Result<()>;
    async fn usage(&self, at: &Location) -> Result<Usage>;
}

pub enum Key { Content(Digest), Place(Place) }
pub struct Served { pub media: String, pub name: Option<String> }
pub struct AccessData { pub url: Url, pub headers: Vec<(String, String)>, pub expires: Timestamp }
```

- A **place** is 16 random bytes that the server draws for one entry of an upload plan. `write_access` returns access data for one `PUT` of exactly `size` bytes to that place, and `None` for a size beyond what the location takes in one write. A place is written once: a second `PUT` answers 412 and leaves the place as it is. Write access reaches its own place only.
- `read_access` returns access data for `GET`s of the content until its expiry, served with the descriptor's media type and, with its file name, as an attachment.
- The server verifies a place by reading it back through `get` and computing its size and SHA-512 digest, before the call that names it runs. Once verified, `keep` stores the place's content under its digest, unless the location holds a verified copy already, and the place is deleted after its access data expires.

### The keyring library

`liasse-keys` gives the host `keyring`, a host library (SPEC §3.7) whose mutations hide private keys under the system keyring and sign with them:

```liasse
//! Private keys hidden under the system keyring.
mut keyring.seal(key: Bytes): Bytes
mut keyring.ed25519_sign(sealed_key: Bytes, message: Bytes): Bytes
mut keyring.p256_sign(sealed_key: Bytes, message: Bytes): Bytes
mut keyring.p384_sign(sealed_key: Bytes, message: Bytes): Bytes
```

`keyring.seal(key)` gives `key` sealed for the calling instance. Each signature of `keyring` opens `sealed_key` in the host's process and signs as the `crypto` signature of its algorithm does (BUILTINS §4.2). A private key that a module stores reaches state, history, and exports; a sealed key opens in its own instance alone:

```liasse
signing: { public_key: Bytes; sealed_key: Bytes }?

mut new_signing_key() {
    let pair = crypto.ed25519_generate()
    .signing = { public_key: pair.public_key, sealed_key: keyring.seal(pair.private_key) }
}
```

The commit of `new_signing_key` holds the public key and the sealed key, and the private key stays in its run. The host gives the library, with a host-key provider, to the modules that it chooses:

```rust
let keys = liasse_keys::Keyring::new(liasse_key_file::HostKey::open(&key_path)?);

let liasse = Liasse::builder()
    // the store and the blob connectors, as above
    .host_library(keys.library(), Serves::modules())
    .build()
    .await?;
```

`liasse-keys` keeps each keyring in the backend, and protects keyrings through a host-key provider, the `liasse-host` trait:

```rust
#[async_trait]
pub trait HostKey: Send + Sync {
    async fn wrap(&self, plaintext: &[u8], context: &[u8]) -> Result<Wrapped>;
    async fn unwrap(&self, wrapped: &Wrapped, context: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}
```

- A keyring holds an instance's keys, each 32 random bytes with a 16-byte identifier, the last one current. It is wrapped whole, with the instance's incarnation as the context, so a wrapped keyring opens for its own instance only. An instance's keyring starts with its first sealed key.
- A sealed key is the identifier of an instance key (16 bytes), a 24-byte random nonce, and the XChaCha20-Poly1305 ciphertext of the private key, with the key identifier as associated data. A signature looks the identifier up in the keyring of the calling instance alone, and a sealed key that it fails to open rejects the transaction with the code `host`.
- Instance keys and opened private keys stay in the host's process memory, zeroed after use, and reach no SQL text, parameter, or log.
- `keys.rotate_host_key()` wraps every keyring under the provider's current host key, and reports how many keyrings each earlier host key still wraps; an earlier host key leaves the configuration once that count is zero.
- `liasse-keys` holds an X25519 transfer key pair, whose private key it wraps under the host key. `keys.transfer_key()` gives the public key. `keys.export_keys(&app, recipient, instances)` seals, at one snapshot, the keyrings of the named instances and of the instances mounted in them, or of every instance, to the recipient's transfer key with HPKE (RFC 9180, base mode: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305), each keyring with its instance's position path at that snapshot (SPEC Annex C.5) as associated data, and names the snapshot by the identity of its newest commit and the identities of the older commits that it lacks. `keys.import_keys(&app, bundle)` opens a bundle, finds each keyring's instance at its position path in the state of that snapshot, and wraps the keyring under the host key for that instance.

## Outside the core

A deployed application composes them:

```text
host process                           the host, which embeds liasse
├── backend                            liasse-pg ──── PostgreSQL (postgres image, liasse-pg-ext)
├── keyring library                    liasse-keys ──── liasse-key-file: a key file or a systemd credential
├── blob connector                     liasse-blob-fs ──── presigned requests from the web client
└── client binding                     liasse-connect ──── web client (liasse-connect-wasm, clients/connect)
```

### Backends

- **`liasse-pg`**, **`liasse-pg-ext`**, and the **postgres image** are the PostgreSQL backend, whose architecture and gates [POSTGRES.md](POSTGRES.md) gives. `liasse-pg-ext` is the one crate that uses `unsafe`.
- **`liasse-store-memory`** implements the store contract in process, for tests only. It is the oracle that the corpus runs against beside `liasse-pg`, and both results must match an independent expected result.

### Host implementations

- **`liasse-key-file`** is a host-key provider of `liasse-keys`. It reads the host keys from a file, or from a systemd credential (`LoadCredentialEncrypted=`, which `systemd-creds` seals with the host's TPM2 when present), and holds them in the process's memory only. Each host key is 32 random bytes with a 16-byte identifier; the configuration names the current key and lists the earlier ones that still wrap a keyring. A wrap is the host key's identifier, a 24-byte random nonce, and the XChaCha20-Poly1305 ciphertext, with the identifier and the context as associated data.
- **`liasse-blob-fs`** is a blob connector over a local filesystem, usable as a location of a storage class (SPEC §12.3). It keeps content under its digest's hexadecimal name and places under their own, and it serves presigned requests itself, on an origin of its own that the host sets:
  - a presigned URL carries its expiry and an HMAC-SHA-256 over the method, the digest or place, the size, the expiry, and the served media type and name, under a 32-byte signing key from the host's configuration;
  - a `PUT` whose body differs from the signed size leaves the place empty; a complete body lands in a temporary file that `link(2)` then names as the place, so a second write finds the name taken and answers 412;
  - `keep` links the place's file under the digest;
  - a `GET` answers with the signed media type, `Content-Disposition: attachment` with the file name when there is one, `X-Content-Type-Options: nosniff`, and `Content-Security-Policy: sandbox`;
  - it answers CORS requests from the application origins that the host lists.

### Client bindings

- **`liasse-connect`** carries connections over HTTPS, with one Server-Sent Events stream of frames per connection and each message one CBOR item. It parses every inbound message as hostile input, within the host's size limits. [CONNECT.md](CONNECT.md) gives its transport and messages.
- **`liasse-connect-wasm`** and **`clients/connect`** are the web client: a WASM core and a thin TypeScript shell. [CONNECT.md](CONNECT.md) gives its behavior and its key functions.

### Tools

- **`liasse-cli`** is the `liasse` command, built so that agentic tools can drive it:
  - `liasse build <folder>` builds a source folder into a `.liasse.zip` (SPEC §3.4, §3.5): the current version with its library files, each immediate submodule under `submodules/` as a package, and every earlier version and every migration under `migrations/`, and reports every build error as a diagnostic.
  - `liasse migrate apply <module.liasse> <migration.liasse>`:
    1. checks that the migration's `name` is the module's `meta.name`, that its `from` is the module's `meta.version`, and that every entry applies to the current version (LANGUAGE 15.2, 15.4); a failed check leaves every file as it is;
    2. copies the current module file and its library files to `migrations/<from>/`, and the migration to `migrations/<from>-<to>.migration.liasse`;
    3. applies the structural entries to the current version (`add`, `remove`, `rename`, `retype`) and sets `meta.version` to `to`, keeping the comments and layout of untouched declarations.

### Verification

- **`tests/`** holds the conformance cases, one directory per SPEC chapter, written before the implementation. A case reaches the system only through client paths (views, mutations, reflection), and its expected result comes from a SPEC or LANGUAGE rule.
- **`liasse-testkit`** runs every case against `liasse-store-memory` and `liasse-pg`.
- Backend properties, such as index use, schema reconciliation, and rerun counts, are Rust tests of `liasse-pg`: the gates of POSTGRES.md chapter 15.
- The keyring library has tamper cases, Rust tests that must stay refused: a sealed key that another instance presents, a wrapped keyring copied into another instance's row, a bundle opened with another transfer key, and a keyring or sealed key altered by one byte each fail with the code `host`. After `rotate_host_key` and the removal of the earlier host key, every sealed key still signs.
- History has leak cases, Rust tests on both stores: a mutation that stores the SHA-256 digest of 32 drawn bytes and returns the bytes, and `new_signing_key` above. The returned bytes and the private key occur zero times in the exported history, each commit's changes hold exactly the values that it stores, and restore recomputes a state equal to the live one.
- Effects have cases on both stores: a transaction that calls an effect and runs again before it commits hands its performer one identifier; a rejected transaction hands it zero; a transaction that calls an effect and changes no state adds zero history entries and has its effect performed; a process stopped between the commit and the attempt performs the effect after it starts again, under the same identifier; and an effect whose performer keeps failing reaches `failed_effects()` after exactly the bound on attempts.
- Host libraries have cases on both stores: an application whose module calls the opaque `rates.get` fails `open()` with the code `build`, naming `rates.get` and its signature, once the library drops the mutation or changes its signature, and opens again once the library defines `rates.get` in Liasse through `rates.quote` with that signature. A module that calls a function that the library defines fails `open()` with the code `build`, naming the function and its recorded definition, once the library changes the function's body, and opens again once the library holds the recorded definition.
- Targets have cases on both stores: a child's target that passes alone moves the child and leaves its parent's version as it is; a parent's target whose shape lists a member that a child's version lacks stays blocked, reflection names the child's display path and the member, and it moves in the first commit after the child's update, with zero further `update` calls; a parent and a child whose targets pass only together move in exactly one commit; an instance with a blocked target commits 1 000 transactions under its active version; a target that a row blocks by failing its migration moves after a transaction corrects the row, with zero further `update` calls; a blocked instance moves to the furthest version of its route that passes; an instance initialized after an `update` from an older package of that module moves to its module's target; a package that disagrees with its module's chain is rejected with the code `build`; and an instance whose mount point's type has a `target` stays at that target while its module's target moves on. Over generated trees of instances, with targets whose shapes, contexts, and migrations pass or fail, an instance ends blocked exactly when no set of targets that holds it passes, checked by brute force: zero counterexamples.
- Provided mutations have cases on both stores: a child's mutation that calls a mutation that its parent provides commits the child's and the parent's writes in one commit, and a parent's write that breaks a constraint rejects both; and a mount whose provided mutation calls, through the mount point, the child mutation that calls it is rejected with the code `build`.
- Each blob connector has tamper cases, Rust tests that must stay refused: a second `PUT` to a place, a `PUT` of another size, a request under an expired or altered signature, and a `PUT` toward a digest each leave every stored byte unchanged; and a place whose content differs from its descriptor rejects the call that names it with the code `malformed`.

## CBOR encoding

The alpha's encoding (SPEC Annex A.4) is CBOR (RFC 8949): each value in a record of a dehydrated application and in a message of `liasse-connect` is one CBOR data item. A writer encodes each value in the form that the table gives. A reader decodes every well-formed item of that form to its value, whatever the widths of its integers, lengths, and tag arguments, the order of its map keys and of the elements of a set, and the scale of its decimals.

The items are integers, byte strings, text strings, arrays, maps with text keys, the simple values `false`, `true`, and `null` where the table places it, and the tags 2, 3, 4, 37, and 1004 where the table places them.

| Value | CBOR |
| --- | --- |
| `Text` | text string |
| `Bytes` | byte string |
| `Bool` | `true` / `false` |
| `Int` | integer (major type 0 or 1) from −2^64 to 2^64 − 1; beyond, a tag 2 (non-negative) or tag 3 (negative) bignum |
| `Decimal` | tag 4 decimal fraction `[exponent, mantissa]`, written in minimal-scale form (BUILTINS §1) |
| `Uuid` | tag 37 over a 16-byte byte string, the bytes in RFC 9562 order |
| `Date` | tag 1004 over an RFC 3339 full-date text string |
| `Timestamp` | integer count of microseconds since the Unix epoch |
| `Duration` | integer count of microseconds |
| `Enum` label | text string |
| record, object, row | map with text keys, one member per member that holds an element |
| tuple | array of its places in position order; an empty place is `null` |
| set of cardinality `*` or `+`, and set of sets of cardinality `?` | array of its elements, each with its occurrence token where a message carries one |
| `Blob` | map `{ "digest": <64-byte byte string>, "media": <text>, "name": <text>, "size": <unsigned integer> }`, `name` omitted when empty |
| opaque token (entry point handle, frontier, commit, occurrence token) | byte string |
| `none` in a place that cannot be omitted, such as one result of a call list | `null` |

A received item that decodes to no value of its type is malformed (SPEC Annex A.4). In CBOR, these items decode to no value: a CBOR map with two equal keys, a floating-point number, a simple value or tag outside the list, a tag 37 over a byte string whose length differs from 16, a tag 1004 over an invalid date, and a text string that is not valid UTF-8.

The decimal `2.5` is `4([-1, 25])`; the `Uuid` `3f2c7a1e-0b4d-4c6e-9a8f-1d2e3f405162` is `37(h'3f2c7a1e0b4d4c6e9a8f1d2e3f405162')`; the `Date` `2026-09-22` is `1004("2026-09-22")`; the instant `2026-09-22T10:00:00.250Z` is `1790071200250000`; the `Map<Text, Int>` holding `daily` with 100 is `[{"key": "daily", "value": 100}]`; and `(none, 5)` of the type `(Text?, Int)` is `[null, 5]`.

## Open scope decisions

- A QUIC and WebTransport binding: SPEC chapter 10 allows it. In the alpha, or SSE + HTTPS only?
- A blob connector for S3-compatible object storage: in the alpha, or the filesystem connector only? It would presign SigV4 `PUT`s that sign `content-length` and `if-none-match: *` (a place written once), presign `GET`s with `response-content-type` and `response-content-disposition`, and implement `keep` as a server-side copy.
- A host-key provider backed by a KMS or a PKCS#11 HSM: in the alpha, or key file and systemd credential only?
- The format of a corpus case file.
- Ordered live views: `liasse-connect` and the backends add an order key to each occurrence of a sorted view, by which a client keeps its order. In the alpha, or later?
