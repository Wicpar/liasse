# AGENTS.md

Binding instructions for anyone — human or agent — writing code in this repository.

## Writing Rust code

- Structure state so that invalid state is unrepresentable in the type system.
- Do not validate; parse. Turn raw input into a richer type once, at the boundary; past that boundary the type is proof the data is well-formed.
- Use semantic types: types that have meaning and group reusable functionality, instead of abusing meaningless scalars.
- Do not use interior-mutability structures. Avoid reference counts. They are a sign of a bad design; normally everything is expressible as owned values, `&`, and `&mut`. This prohibition targets smuggling mutability into a crate's *own* state types; a mature third-party connection pool managing an *external* resource (e.g. `liasse-pg`'s r2d2 read pool over PostgreSQL connections) is internally synchronised by design and is exempt.
- Code must never panic. The exception is unrecoverable states, like poisoned locks in some cases. Tests are expected to panic on failed cases.
- The workspace forbids `unsafe` (`unsafe_code = "forbid"`); the one sanctioned exception is the forthcoming `liasse-pg-ext` crate (the `pgrx` PostgreSQL extension), permitted `unsafe` solely for the `pgrx` FFI boundary, confined to the FFI shim and justified with `// SAFETY:` comments.
- Keep it simple: if it's long and complicated, you are missing an abstraction and should research better representations. Leverage the Rust type system to create safe abstractions that have pre-validated states instead of propagating `Result`/`Option` everywhere.
- Do not reinvent the wheel; try to find existing crates that do the job.
- When parsing a language, emit rustc-like helpful diagnostics. A diagnostic MUST allow a new user to understand what went wrong, why, and how to fix it (when a hint is available).
- Do not write noisy code. Noisy code is code with a lot of busy work that does not contribute to the core logic of what is being programmed. Keep things in abstractions that allow functionality to be implemented at homogeneous abstraction levels. Good code is code where each statement truly contributes to the understanding of what is being computed.
- Avoid creating bare `fn` helpers; most of the time there is a type representing state on which to `impl` the functions, or traits to implement.
- When multiple structures implement common functionality, and code relies on that functionality without caring about the backing structure, use traits.
- Code files should never be longer than a few hundred lines. A file growing past that is a missing module boundary or a missing abstraction.

## Writing tests

- Tests verify that logical invariants are not broken.
- Never write tests for invariants the type system already enforces.
- Never write tests that measure performance.
- Tests must verify that parts of the program correctly interact.
- Never write tautological tests that rely on the program's own answer not changing. The expected result must be externally deducible for the test to be useful.
- Do not put tests in the same file as the functionality; create separate test files containing only tests.

## Benchmarks

- Benchmark functionality with criterion at every level, low and high.
- Every non-trivial custom abstraction, algorithm, or data structure should benchmark all the different performance axes it has.
- Do not benchmark trivial or crate-imported functionality. The only exception is when choosing between multiple candidate crates.

## Project constraints

- The parser MUST be built on `pest`.
- The storage backend is PostgreSQL (`liasse-pg` is the only crate that talks to it; everything else goes through the `liasse-store` contract).
- **`liasse-pg` performance is a correctness gate, not a nice-to-have.** Every SQL query pattern the backend runs (row-by-canonical-key lookup, collection scan in Annex B key order, commit-log read from a seq, snapshot-at-frontier, blob-by-digest, instance-meta head/version, history points) MUST be backed by an appropriate index in the embedded DDL, and the backend's overhead must be near that of the equivalent raw PostgreSQL request. Gate index coverage DETERMINISTICALLY with `EXPLAIN` assertions on populated tables (each query must use an Index/Index-Only Scan, never a Seq Scan) — a plan-based index-use assertion is a correctness property, so this is the one place the "never write performance tests" rule above does not apply. Add criterion benchmarks comparing backend ops to raw SQL for the throughput axes.
- **`liasse-pg` is self-reconciling.** On every load/migration the backend brings the physical database into exact correspondence with what the CURRENT active model needs: it auto-creates required structures (tables, indexes, roles, …) and auto-DROPS anything no longer used. Migrations must NEVER pollute the database — no orphaned tables, indexes, columns, roles, or other structures may accumulate. The physical DB is a pure reflection of the current logical needs; only used structures persist and the rest is cleaned up when it stops being useful. Gate this: a migration removing a collection/field/index must leave the corresponding table/index/role GONE (assert no orphan), and a fresh load must create exactly what is needed and nothing more.
- **Client-sync connector (deferred deliverable, build after spec-completion).** Beyond the Rust library embedding, the implementation ships a connector for a self-hosted web client that manages client-side sync automatically per SPEC §12 (manifest/view/call/fetch/operation; resumable `init`/`patch`/`close` frontiers; bounded windows; §12.3 completion barrier). The §12 semantics already live in `liasse-runtime`/`liasse-surface` (`ViewDelta`, `watch.rs`, `window.rs`); the connector adds a canonical wire codec, a transport binding (one logical connection = the §12.3 coherence unit), and a WASM-core + thin-TS-shell web client that applies patch ops and resumes from a retained frontier. **Transport: NO WebSockets** — use either SSE + HTTP requests (default: SSE carries the `init`/`patch`/`close` frontier stream with `Last-Event-ID` giving §12.2 resume for free; POSTs carry `manifest`/`view`/`call`/`fetch`/`operation`) or QUIC/WebTransport (one session per connection, one stream per subscription). **The web client is UNTRUSTED**: it is a convenience layer for view-state sync and request ergonomics only and holds NO authority. All authorization, per-frontier role/session re-checks, output projection, normalization, and admission stay server-side; the client receives only the already-authorized projection and never unexposed fields, internal identities, or another actor's data. Expose nothing sensitive over the wire (opaque occurrence ids; operation ids are per-client capabilities; no executable source; no internal model detail), and parse every inbound client message as hostile input at the boundary.
- The normative reference is [SPEC.md](SPEC.md) (Liasse v0.5). Every observable behavior traces to a spec rule.
- The conformance corpus lives in `tests/` (file-based; format defined in [tests/FORMAT.md](tests/FORMAT.md)). The corpus is written **before** implementation: implementation work makes existing cases pass — never rewrite a case to match implementation behavior unless the case itself contradicts SPEC.md.
