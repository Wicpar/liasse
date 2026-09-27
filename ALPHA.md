# Alpha scope

The alpha is the first lean core release. [SPEC.md](SPEC.md) and [LANGUAGE.md](LANGUAGE.md) define its core, and SPEC Annex D lists what is not in it. This file maps the crates of the alpha, core and outside the core, and states what each part must do.

## The boundary

The **core** is the engine that SPEC and LANGUAGE define: the language, values and the artifact format, the runtime semantics, the prelude, the standard namespaces (SPEC §13.1), and three contracts. Everything else is **outside the core** and plugs into one of the contracts:

| Contract | What plugs in |
| --- | --- |
| `liasse-store` | storage backends |
| `liasse-host` | components that a module names in `requires`, key providers, blob connectors |
| `liasse-wire` | client bindings |

A core crate depends only on core crates. A crate outside the core depends on the core, never the reverse. The core states what happens; how a backend makes it happen, such as indexes, query plans, and where evaluation runs, belongs to the backend.

## Core

```text
liasse                   root client (SPEC §20.1)
├── language
│   ├── liasse-diag      source spans, rustc-like errors, runtime diagnostics (SPEC §20.3)
│   ├── liasse-syntax    the pest grammar of LANGUAGE Annex A
│   ├── liasse-expr      typing, evaluation, operators, standard namespaces (LANGUAGE chapters 5 to 9, BUILTINS, SPEC §13.1)
│   └── liasse-model     elaboration into the checked model, the prelude (LANGUAGE chapters 2 to 4 and 10 to 13, BUILTINS §8)
├── values
│   ├── liasse-value     types, CBOR values, total order (SPEC Annex A, B)
│   ├── liasse-ident     identity text, display and declaration paths (SPEC Annex C.1, C.2)
│   └── liasse-artifact  the .liasse.zip format (SPEC §3.1, Annex C.3, C.4)
├── engine
│   ├── liasse-runtime   transactions, modules, migrations, history, blobs (SPEC §4 to §8, §12, §15 to §19)
│   ├── liasse-pred      the row-program evaluator of views, selections, and assertions
│   └── liasse-surface   public members, sessions, live views (SPEC §9 to §11)
└── contracts
    ├── liasse-store     storage contract
    ├── liasse-host      component, key provider, and blob connector contracts (SPEC §13.4, §14.5, §15.6)
    └── liasse-wire      wire messages and patch apply (SPEC §11)
```

Requirements:

- Every observable behavior follows SPEC and LANGUAGE, and the conformance corpus passes on every backend.
- The parser is built on `pest`, and every build error is a rustc-like diagnostic that says what went wrong, why, and how to fix it.
- Every stored value and every wire value is CBOR, each type's encoding fixed by SPEC Annex A.4.
- A transaction runs as a sequence over one snapshot, reads its own writes, and applies its final state atomically; readers and writers never block each other (SPEC §7, §19).
- Every rejection carries a diagnostic: code, message, path, and a trace across the transaction (SPEC §20.3).
- Templates, mixins, slots, and applications are elaborated when a module is built, and a backend receives the elaborated declarations. Elaboration composes typed declarations: `factor = 1 + 2` substituted into `10 * factor` is 30. Every elaborated declaration keeps its origin, the definition, the application, and the slot it comes from, for diagnostics and documentation, and a template's documentation stays distinct from an application's. Contracts are compared after elaboration.
- The root client builds one context: it picks one backend, registers components, key providers, and blob connectors, and binds key classes and storage classes (SPEC §13.4, §14.1, §15.3, §20.1).

## Outside the core

```text
backends                     plug into liasse-store
├── liasse-pg                PostgreSQL, the only production store
├── liasse-pg-ext            pgrx extension: Liasse evaluation inside PostgreSQL
├── postgres image           PostgreSQL with liasse-pg-ext installed
└── liasse-store-memory      in-process store, the test oracle
components                   plug into liasse-host, named in `requires`
├── liasse-keyring           liasse.keyring@1
└── liasse-cose              liasse.cose@1
host implementations         plug into liasse-host
├── liasse-key-ed25519       key provider
└── liasse-blob-fs           blob connector
client bindings              plug into liasse-wire
├── liasse-connect           SSE + HTTP binding
├── liasse-connect-wasm      web client core
└── clients/connect          TypeScript shell over the web client core
tools
└── liasse-cli               the `liasse` command
verification
├── tests/                   conformance corpus
└── liasse-testkit           corpus runner
```

A deployed application composes them:

```text
application process                    embeds liasse, the root client
├── backend                            liasse-pg ──── PostgreSQL (postgres image, liasse-pg-ext)
├── components                         liasse.keyring@1, liasse.cose@1
├── key provider                       liasse-key-ed25519
├── blob connector                     liasse-blob-fs
└── client binding                     liasse-connect ──── web client (liasse-connect-wasm, clients/connect)
```

### Backends

- **`liasse-pg`** implements the store contract over PostgreSQL and meets the PostgreSQL checklist below. It keeps the complete history and MAY move old parts to cold storage.
- **`liasse-pg-ext`** is a pgrx extension that evaluates Liasse expressions inside PostgreSQL with the core's own evaluator, so a PostgreSQL result equals the core's result by construction. It is the one crate that uses `unsafe`, confined to the FFI shim.
- **postgres image** is the backend's deploy artifact: PostgreSQL with `liasse-pg-ext` installed. `liasse-pg` creates the extension when it reconciles the schema.
- **`liasse-store-memory`** implements the store contract in process, for tests only. It is the oracle that the corpus runs against beside `liasse-pg`, and both results must match an independent expected result.

### Components

Each component publishes a namespace through a descriptor (SPEC §13.2), checked when a module that requires it activates.

| Crate | Contract | Publishes | Specified in |
| --- | --- | --- | --- |
| `liasse-keyring` | `liasse.keyring@1` | the `Keyring` template, its views `current`, `accepted`, `public`, `versions`, and the key version lifecycle, rotation, capability checks, and provider failures | SPEC §14.1 to §14.4, §14.6, §14.9 |
| `liasse-cose` | `liasse.cose@1` | signing through a keyring, the keyring verifier, direct session tokens | SPEC §14.7, §14.8 |

### Host implementations

- **`liasse-key-ed25519`** is a software key provider: it generates Ed25519 key versions, keeps private key material behind opaque handles, and signs (SPEC §14.5).
- **`liasse-blob-fs`** is a blob connector over a local filesystem, usable as a location of a storage class (SPEC §15.6).

### Client bindings

- **`liasse-connect`** carries sessions over HTTP: requests as HTTP POSTs, frames on one Server-Sent Events stream per session, each message one CBOR item, in unpadded base64url on the stream, and resumption from the last frontier the client received (SPEC §11). It parses every inbound message as hostile input.
- **`liasse-connect-wasm`** and **`clients/connect`** are the web client: a WASM core that applies patches to a replica of each subscription and resumes from its retained frontier, and a thin TypeScript shell. The web client holds no authority: it receives only the projection that the server authorized, and every check stays on the server.

### Tools

- **`liasse-cli`** is the `liasse` command. It stays outside SPEC and LANGUAGE, and is built so that agentic tools can drive it:
  - `liasse build <folder>` builds a definition folder into a `.liasse.zip` (SPEC §3.1): the definition with its library files, every library it requires under `children/`, each a module, and every earlier version's module file and library files and every migration under `migrations/` (LANGUAGE 13.1), and reports every build error as a diagnostic.
  - `liasse migrate apply <module.liasse> <migration.liasse>`:
    1. checks that the migration's `name` is the module's `meta.name`, that its `from` is the module's `meta.version`, and that every entry applies to the current definition (LANGUAGE 13.4, 13.5); a failed check changes no file;
    2. copies the current module file to `migrations/<from>/`, and the migration to `migrations/<from>-<to>.migration.liasse`;
    3. applies the structural entries to the definition (`add`, `remove`, `rename`, `retype`) and sets `meta.version` to `to`, keeping the comments and layout of untouched declarations.

### Verification

- **`tests/`** holds the conformance cases, one directory per SPEC chapter, written before the implementation. A case reaches the system only through client paths (views, mutations, reflection), and its expected result comes from a SPEC or LANGUAGE rule, never from the implementation.
- **`liasse-testkit`** runs every case against `liasse-store-memory` and `liasse-pg`.
- Backend properties that no client can observe, such as index use and schema reconciliation, are Rust tests of `liasse-pg`.

## PostgreSQL checklist

SPEC leaves these to the backend. `liasse-pg` does them all.

- [ ] Serve every selection, `filter`, `unique` lookup and enforcement, and `order:` default order from indexes. Gate each query pattern with `EXPLAIN` on populated tables: an index scan, never a sequential scan.
- [ ] Views are known queries: shape and index the stored data to accelerate every view, selection, and pipeline that the model declares, materializing where it pays, as long as each read equals the view's expression.
- [ ] Equality is by value (LANGUAGE 6.3): row comparison, `in`, `|`, `&`, `-`, and removal. Implement it with any equivalent mechanism, such as comparing identities or `unique` keys where they decide equality, and constrain nothing beyond that meaning.
- [ ] Scope every lookup to its owner: no cost grows with unrelated owners or with rows outside a window.
- [ ] Evaluate views, initializers, constraints, and functions inside PostgreSQL. No in-memory copy, cache, or projection of persisted state.
- [ ] Run one operation in as few statements as its semantics allow: admission inside the writer's transaction, a view's reads in one statement, incarnations allocated in blocks, and no re-query of a view that a commit does not touch.
- [ ] MVCC: snapshot reads, readers and writers never block each other, serializable commit with retry on SQLSTATE 40001, several service nodes on one database.
- [ ] Live views: subscription state and the patch diff live in PostgreSQL, and `NOTIFY` is the event source.
- [ ] Self-reconciling schema: each load and each migration creates exactly the tables, indexes, and roles that the active model needs, and drops the rest.
- [ ] Stored values are CBOR.
- [ ] Every component call that admission makes has a bound (SPEC §20.2).
- [ ] Each affected end-to-end path is benchmarked against the equivalent raw SQL.

## Open scope decisions

- `liasse.webauthn@1`, `liasse.oidc@1`, `liasse.password@1`: SPEC shows them in examples only. In the alpha, or later?
- A QUIC and WebTransport binding: SPEC §11 allows it. In the alpha, or SSE + HTTP only?
- A blob connector for object storage: in the alpha, or the filesystem connector only?
- The format of a corpus case file: SPEC data is CBOR, and the case files need a format without JSON.
