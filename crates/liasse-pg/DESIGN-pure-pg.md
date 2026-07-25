# DESIGN — Pure-PostgreSQL `liasse-pg` (no in-memory projection)

Status: **Phases 0–4 landed** (projection deleted, all contract reads SQL,
§12 hydration shared per (instance, frontier)); **Phase 5 (`scan_subtree`) in
flight — note its §3/§7.6 shape-directed revision**; **Phases 6–10 are design,
not implemented**. Mandates, in force together:

1. *"IN MEMORY PROJECTION IS FORBIDDEN IN PG BACKEND. PG backend must be pure PG."*
   Every contract read must be served by a PostgreSQL query; the backend may hold
   **no** in-memory read model of durable state.
2. *"IF PERSISTED → POSTGRES ONLY. IF SESSION-RELATIVE → IN RUST SESSION CODE."*
   Persisted/durable state (rows, the tree, the incarnation counter) lives
   in and is read from Postgres — no in-memory copy or cache, allocator cursors
   included (this **overrules** the earlier §6.3 judgment call). Session-relative
   state (a computed `Snapshot` result, window/anchor state, the *values* of
   `$actor`/`$session`/`$params`) lives in Rust session code.
3. *"`$where`/`$except` MUST be in PG or it will be a performance killer."* The
   §10.5 recursive-coverage hereditary pruning executes **inside** the
   `WITH RECURSIVE` query, pruning during descent — a pruned node's whole subtree
   is never fetched.
4. **Maintainer decision, superseding v2's §7 SQL lowering**: the §10.5 pruning
   predicate is evaluated inside PostgreSQL by **the actual liasse interpreter**,
   linked into a [`pgrx`](https://github.com/pgcentralfoundation/pgrx) extension
   and shipped in a **custom PostgreSQL Docker image** (PG is self-hosted, so the
   image is the deployment artifact). There is **no predicate→SQL compiler, no
   exact-semantics operator table, no compilable-fragment restriction, and no
   `liasse_text_key` schema function** — v2's §7.4/§7.5/§7.7 machinery and its
   §10.5 SPEC-change proposal are **replaced** by §7 and §12 below. Everything
   else in v2 stands: the SQL read path (§4), the r2d2 pool (§5), the write path
   incl. the durable incarnation counter (§6), `scan_subtree` and the §7.2 coverage
   semantics, the §12-watch treatment (§8), the parity/gate discipline (§9), and
   the phase plan structure (§10).
5. **Maintainer scope expansion, revising this document (v4)**: *"pgrx extension
   is not only for `$where` and `$except`, it's for ANY cel expression."* The
   extension is a **general Liasse-expression evaluator** — the same checked
   `TypedExpr` interpreter, run in-database — and read-side expression
   evaluation **over persisted rows pushes down into PostgreSQL through it**:
   a `$view`'s filter, projection (computed fields included), and `$sort` are
   served by ONE index-served SQL statement whose per-row evaluation is the
   extension, and the §10.5 coverage read of mandates 3+4 becomes a **special
   case** of that general mechanism (the recursive source with a hereditary
   admit program). What stays in Rust is enumerated, not implied: admission and
   staging (the in-memory overlay SQL cannot see, §5.4 case 3), `$normalize`
   and every write-side evaluation, the MemoryStore oracle (always in-Rust —
   the parity target), the §12 diff/window/anchor machinery, and the
   scope-deferred forms (§7.5 — narrowed by mandate 6 below to scope choices,
   no per-row exclusion).
6. **Maintainer decisions, revising this document (v5) — the evaluator becomes
   FULLY general; no per-view restriction remains.** Two decisions remove the
   two per-row exclusions v4's §7.5 carried. (a) *App-registered host
   namespaces are REMOVED from the language* — **superseded by mandate 7
   below: app namespaces are KEPT; the built-in-only property of DB-run
   programs is obtained by a load-time language rule instead of removal.**
   (v5's original rationale, retained for the record: with only the fixed
   built-in set — `string`, `time`, `convert`, `hex`, `base64`, `sha`, plus
   the language operators, SPEC.md §6.5/§16.1 — deterministic and in-crate,
   every function a program can call is **bundled into the extension**, so a
   candidate-dependent host call always evaluates in-PG. Mandate 7 preserves
   exactly that property for the programs the extension actually receives,
   without deleting §16.) (b) *Candidate-subtree reads push down*: a program that
   reads through the candidate's own nested collections
   (`size(child.subcompanies)`, aggregates/filters over the candidate's
   subtree) is served by a **compiler-prefetched subtree lateral** (§7.6) — the
   query materializes the candidate's live subtree index-served and passes it
   to the evaluator as one more argument, so the faces stay `IMMUTABLE` and
   the plan stays index-served; a guarded depth bound makes corrupt/cyclic
   data a loud error, never a hang. With both edges gone, ANY read-side
   expression over persisted rows pushes down; what remains outside v1 is
   **scope** (deferred combinator/aggregate-fold/json-sort and non-stored
   sources, §7.5), not restriction, and the v4 load-time coverage-rejection
   policy dissolves (§7.5).
7. **Maintainer decision, revising this document (v6) — SEPARATE expressions
   by WHERE they run; supersedes mandate 6(a), mandate 6(b) stands.** App-
   registered host namespaces (§16: registration, `Registry`, `$requires`,
   descriptors) are **KEPT** — they are the substrate for §11 custom
   authentication (webauthn/oidc/password verifier namespaces the runtime
   cannot natively perform), §17 cose, and §20 migration codecs. Instead, the
   language separates expression contexts by execution locus, and the split
   is maximal (maintainer correction, binding: *"verify, check, normalize
   should also be in db. the only place an app provided procedure can run is
   in a mutation, since it's modeled as a transaction"*).
   **Framework-run** is exactly ONE context: the **mutation program body** —
   the §8 sequential atomic program (including §11.5 login/auth mutations,
   §21.1 delete patches, and the §20.1 `$up`/`$down` delta programs, which
   are programs of mutation statements) — because a mutation is modeled as a
   transaction, and the transaction is where app procedures' effects,
   recording, and replay fixing are defined. Only there may an
   app-registered namespace be called. **Database-run** is everything
   else — every read AND every validation: a `$view`'s filter/selection,
   projection, and `$sort`; §10.5 coverage `$where`/`$except`; computed
   values; **`$verify` (§11), `$check` (§5.10/§8.8), `$normalize` (§8.8)**;
   defaults; bucket/meter/placement expressions; migration `$as`/`$back` —
   all restricted to the **built-in namespaces only** (§6.5 + the language
   operators; native keyring verification, §17.7, remains available in
   `$verify` — it is runtime-dispatched, not an app namespace). An
   app-registered call in ANY non-mutation-body position is a **load-time
   error** with a rustc-like diagnostic; no runtime fallback exists for that
   case. Custom credential verification is re-modeled onto the §11.5 pattern:
   an **auth mutation** invokes the verifier namespace inside its transaction
   and mints a native token that later requests verify through a built-in
   `$verify` (§7.5). The extension therefore holds **every function a DB-run
   program can contain** — by construction at load, not by removing §16 —
   and v5's §16-removal workstream is **CANCELLED**. Enforcement lands in
   the checker (`liasse-expr`), §7.5; the SPEC.md amendments (§16.5, §11.3,
   §16.3, §8.8) are drafted there.

This document is the implementation plan an agent fleet builds from, phase by
phase. Every claim about SQL plan shape in §4 and §6 was **prototyped against
PostgreSQL 17.10** on the real v4 `nodes` DDL (`schema.rs`), populated with a
fanout-4 depth-5 recursive `companies`/`subcompanies` tree carrying real
tagged-wire values plus 40 000 noise nodes, `ANALYZE`d. The v2 §7 lowering
prototypes (operator table, `liasse_text_key` corpus) are superseded with the
machinery they validated; their adversarial corpus survives as the §9 regression
backstop. The v3 extension mechanics — a pgrx `#[pg_extern]` function pruning a
recursive CTE with an index-served plan, packaged into the two-stage Docker
image — are prototyped end to end in §7.8, and the v4 general-evaluator
mechanics — a one-statement filter+projection+sort `$view` read through three
extension faces, plus the shape-directed recursive descent — extend the same
prototype (§7.8). The v5 subtree-prefetch mechanics — a candidate-subtree-
reading predicate served through an index-served recursive lateral (flat AND
composed into the coverage CTE's recursive term), verdict-identical to a
native-SQL oracle, with cyclic/corrupt data bailing at the depth guard as an
error rather than a hang — extend it again (§7.8).

---

## 1. What is forbidden today (current architecture)

`PgStore` (`store.rs`) holds a `Projection` (`projection.rs`): `BTreeMap`s of the
whole committed row set (`current`), the structural node index (`by_id`), the full
commit log, history points, **all blob bytes**, and instance metadata — rebuilt from
the durable tables on open (`node_load.rs`) and advanced in lockstep by every commit.
**Every** contract read (`row`, `scan`, `snapshot`, `log_from`, `head`, `get_blob`,
`has_blob`, `point_position`, `definition`, `composition`) is answered from that
projection; PostgreSQL is only written to. The projection exists for two reasons:

1. the contract's reads are `&self` while the synchronous `postgres::Client` needs
   `&mut self` for every query, and interior mutability is forbidden (AGENTS.md);
2. several read signatures are only implementable from memory: they are infallible
   (`head`, `point_position`, `has_blob`) or return borrows (`definition`,
   `composition`).

Both causes are removed below: (1) by a read-connection **pool** (maintainer
directive, §5), (2) by minimal contract surgery (§3).

## 2. Target architecture at a glance

```
PgStore {
    writer:  postgres::Client                                  // one writer per instance
    reads:   r2d2::Pool<PostgresConnectionManager<NoTls>>      // &self read checkouts
    schema:  Schema
    instance: InstanceId
}
```

- **No field holds durable state — and no allocator cursor either.** No row map,
  no log copy, no blob cache, no point map, no cached head/definition/composition,
  and no `next_incarnation` field: incarnation tokens are
  drawn from the durable `incarnations` sequence
  counter (§6.3).
- Every `&self` read checks a connection out of `reads`, runs **one SQL statement**
  (or, for `snapshot`, one statement plus a Rust fold), and returns it.
- Every `&mut` write path (`begin`→`commit`, `put_blob`, `record_point`,
  `alloc_incarnation`, open-time reconcile) uses `writer`, exactly as today.
- The decoupled physical schema is **kept unchanged**: the `nodes` adjacency tree with
  surrogate `id`/`parent_id`/`step_name`/`key_enc`/`key_wire`/`incarnation`/`value`,
  the `commit_log`, `history_points`, `blobs`, `instance_meta`, `schema_version`
  tables, and the `node_key_lookup` unique index. No hierarchy flattening, no
  per-collection tables, no new columns. `SCHEMA_VERSION` does **not** bump: the
  re-architecture changes only who answers reads, not what is durable. The one
  physical addition (revised, mandate 4) is the **`liasse` PostgreSQL extension**
  — database-scoped, shipped in the deployment image, required and
  version-checked at open (§7.7, §12); v2's managed `liasse_text_key` schema
  function is eliminated with the SQL lowering it served.

## 3. Contract surgery (`liasse-store`) — unavoidable, minimal

Pure PG makes a read effectful and unable to hand out borrows of durable state. Four
signatures change; the pool keeps every read `&self`.

| Method | Today | Pure-PG target | Why |
|---|---|---|---|
| `head` | `&self -> CommitSeq` | `&self -> Result<CommitSeq, StoreError>` | SQL can fail |
| `point_position` | `&self -> Option<CommitSeq>` | `&self -> Result<Option<CommitSeq>, StoreError>` | SQL can fail |
| `has_blob` | `&self -> bool` | `&self -> Result<bool, StoreError>` | SQL can fail |
| `definition` | `&self -> Option<&DefinitionText>` | `&self -> Result<Option<DefinitionText>, StoreError>` | owned: nothing to borrow from |
| `composition` | `&self -> Option<&Composition>` | `&self -> Result<Option<Composition>, StoreError>` | owned: nothing to borrow from |

Unchanged: `instance() -> &InstanceId` (identity lives in the struct, not in durable
state); `row`/`scan`/`snapshot`/`log_from`/`get_blob` (already fallible + owned);
all of `Transition` (its reads stay `&self` — `PgTransition` reaches the pool through
its `&mut PgStore` reborrowed as `&`); `StoreFactory`.

`Snapshot` stays a materialized value type. Building one per `snapshot()` call is a
query **result**, not a read model — under mandate 2 it is exactly the
"session-relative computed result" that belongs in Rust. (Judgment call — §11.)

**MemoryStore** adapts mechanically (`Ok(...)`, `.cloned()`), staying the oracle.
**Ripple**: ~28 call sites in `liasse-runtime`/`liasse-surface`/testkit gain `?` or
error mapping; `Engine::head`, `Engine::definition_source` and the few engine reads
that consume the five methods become fallible. Mechanical, wide, one commit (Phase 0).

Phase 5 adds one semantics-free method (see §7 for how it now divides labor with
the evaluated read). Revised for the shape-directed-descent finding (§7.6): the
caller supplies the **step universe** — the declared nested-collection names
occurring anywhere in the subtree's compiled shape — because a recursive join on
`parent_id` alone plans as a Seq Scan + hash join (prototyped, §7.8); joining
`(parent_id, step_name = ANY($steps))` rides `node_key_lookup`.

```rust
/// Every row of the subtree rooted at `root` (excluding `root` itself), i.e. all
/// rows whose address strictly extends `root`'s, in Annex B address order.
/// Semantics-free: no predicates; tombstoned intermediates are traversed so
/// logical orphans (§5.4) are included. `steps` is the set of declared nested
/// collection names occurring in the subtree's shape — the descent visits only
/// child rows under those step names, which is every row a well-formed store
/// holds there (the caller derives `steps` from the compiled shape; §7.6).
fn scan_subtree(&self, root: &RowAddress, steps: &[String]) -> Result<Vec<(RowAddress, StoredRow)>, StoreError>;
```

Phases 7–10 add the **evaluated read** (mandates 3+4+5+6) — the one
semantics-carrying read of the contract. The store carries the evaluation
**opaquely**, behind a trait, so `liasse-store` stays semantics-free and gains
no dependency on the expression layer. The semantics live in ONE implementation
(`liasse-pred`, §7.3) that the in-memory store calls directly and the PG
extension calls after deserialization — v2's `RowPredicate`/`PredOperand`/
`CompareClass` store-level IR is **eliminated**, and v3's single-purpose
`CoveragePredicate`/`scan_coverage` pair is **generalized** into it (coverage is
now one *source shape* of the same read).

```rust
/// The candidate's live subtree, prefetched by the store when the program's
/// read-set (`subtree_steps`) is non-empty: every live row strictly under the
/// candidate through the read-set's nested collection names — each with its
/// rel path from the candidate (one (step, key) component per descended
/// level) and stored value, in Annex B address order. Live rows only: a
/// tombstone blocks its branch, exactly as `materialize_row_cell` builds the
/// interpreter's candidate tree (§7.2) — deliberately NOT `scan_subtree`'s
/// traverse-through-tombstones hydration semantics. Empty for the common
/// shallow program.
pub struct CandidateSubtree(pub Vec<(Vec<(String, KeyValue)>, Value)>);

/// A compiled per-row evaluation program, opaque to the store contract: the
/// admit filter, the projection, and the sort-tuple evaluation of one lowered
/// view read. The single implementor is `liasse_pred::RowPrograms` (§7.3); the
/// trait exists so `liasse-store` carries programs without depending on the
/// expression layer. Each face is total over (stored payload, typed key,
/// prefetched subtree); an evaluation fault is an error, never a silent
/// verdict or a guessed value.
pub trait ViewProgram {
    /// The nested-collection step names the program reads through the
    /// candidate — the compiler-extracted candidate-subtree read-set
    /// (§7.5/§7.6), transitively closed over carried computed expressions.
    /// Empty for the common shallow program. Non-empty directs the store to
    /// prefetch each candidate's live subtree over exactly these steps
    /// (MemoryStore: a depth-guarded BTreeMap descent; PgStore: the §7.6
    /// lateral) before calling the faces.
    fn subtree_steps(&self) -> &[String];
    /// The admit verdict over one row. For a flat view this is the lowered
    /// filter; for §10.5 coverage it is the composed hereditary
    /// `$where && !$except` (§7.2). Truthiness is strict `Bool(true)`.
    /// `None`-program (no filter) admits everything — the store skips the call.
    fn admits(&self, value: &Value, key: &KeyValue, subtree: &CandidateSubtree)
        -> Result<bool, EvalFault>;
    /// The projected output row: the scalar/struct output cells of the `$view`
    /// projection (§7.1) with computed fields (§5.2) folded, as one
    /// `Value::Struct` in output-name order. Keyed sub-view cells are not part
    /// of this scalar projection (they are separate streams, §12.2).
    fn project(&self, value: &Value, key: &KeyValue, subtree: &CandidateSubtree)
        -> Result<Value, EvalFault>;
    /// The evaluated `$sort` tuple (§7.3), highest priority first; empty for an
    /// unsorted view (order = source key order).
    fn sort_tuple(&self, value: &Value, key: &KeyValue, subtree: &CandidateSubtree)
        -> Result<Vec<Value>, EvalFault>;
    /// The version-locked serialized faces (§7.4) a pushdown backend ships to
    /// its in-database twin: admit / project / sort expression wires and the
    /// shared hoisted-env wire. MemoryStore never calls these.
    fn admit_wire(&self) -> Option<&[u8]>;
    fn project_wire(&self) -> &[u8];
    fn sort_wire(&self) -> Option<&[u8]>;
    fn env_wire(&self) -> &[u8];
}

/// Where the evaluated read draws its candidate rows.
pub enum ViewSource<'a> {
    /// A collection's direct rows (§4.2's scan, evaluated): the common `$view`.
    Collection(&'a CollectionPath),
    /// The §10.5 coverage tree under `root` through nested keyed collection
    /// `field`: depth-first in Annex B key order, live rows only (a tombstone
    /// blocks its branch), each DESCENDANT admitted hereditarily by
    /// `program.admits`. The root row itself is NOT filtered — predicates admit
    /// candidates; the covered row is admitted by scope membership (§10.3),
    /// which the caller has already resolved. The root IS projected.
    Coverage { root: &'a RowAddress, field: &'a str },
}

/// One evaluated result row: its rel key path from the source (one component
/// for a flat scan, one per descended level for coverage), its incarnation,
/// the projected output struct, and the evaluated sort tuple.
pub struct EvaluatedRow {
    pub key_path: Vec<KeyValue>,
    pub incarnation: RowIncarnation,
    pub projected: Value,
    pub sort: Vec<Value>,
}

/// The evaluated view read (§7): admit, project, and sort-evaluate the source's
/// rows through `program`, returning rows in the view's delivered order — the
/// Annex-B sort-tuple order with the key path as final tiebreak when the
/// program sorts, else source order (flat: key order; coverage: depth-first key
/// order). `skip`/`limit` (§7.3 bounds) apply after ordering; `Coverage`
/// ignores them (§10.5 has no bounds) and delivers depth-first key order.
fn scan_view(
    &self,
    source: ViewSource<'_>,
    program: &dyn ViewProgram,
    skip: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<EvaluatedRow>, StoreError>;
```

`MemoryStore` implements `scan_view` as a `BTreeMap` range scan/descent calling
`admits`/`project`/`sort_tuple` and sorting with the shared Annex-B tuple
comparison — **evaluating in Rust, through the same evaluator**; for a program
with a non-empty `subtree_steps` it builds each candidate's `CandidateSubtree`
by its own depth-guarded live descent over the same `BTreeMap`. `PgStore` ships
the wires into the §7.6 SQL, where the extension deserializes them and runs
**the same evaluator inside PostgreSQL** — filter in `WHERE`, projection in the
`SELECT` list, sort key in `ORDER BY`, bounds as `OFFSET`/`LIMIT`, and the
candidate subtree prefetched by the §7.6 lateral. Parity is by construction:
same faces, same interpreter — only where the subtree rows come from differs
(BTreeMap vs indexed lateral). An `EvalFault` maps to a new `StoreError::Eval`
variant, which the runtime answers with the interpreter fallback (§7.5) so
fault behavior is interpreter-exact; a depth-guard trip (cyclic or corrupt
over-deep data, §7.6) maps on both stores to the same corruption-classed
`StoreError` — a loud bail, never a hang and never a silent truncation.

## 4. The read path: one indexed SQL statement per contract read

All SQL is schema-qualified (as today); `$n` are bound parameters. `key_enc` is
computed in Rust by the existing `key_enc::encode_key_value` — unchanged.
Result addresses are rebuilt from the **caller-supplied** path plus each row's decoded
`key_wire` — no parent-chain walk is ever needed on the read path.

### 4.1 `row(address)` — chained-InitPlan point lookup

For an address of depth *k*, generate *k* nested scalar subqueries hopping
`(parent_id, step_name, key_enc)` from the root sentinel (`0`); the outermost adds
`value IS NOT NULL` (a tombstone is not a row). Depth-3 prototype plan (verbatim):

```
Index Scan using node_key_lookup on nodes c
  Index Cond: ((parent_id = (InitPlan 2).col1) AND (step_name = '…') AND (key_enc = '…'))
  Filter: (value IS NOT NULL)
  InitPlan 2 -> Index Scan using node_key_lookup on nodes a2 …
    InitPlan 1 -> Index Scan using node_key_lookup on nodes a1 …
```

Pure Index Scans at every hop, no Seq Scan, at any depth. An intermediate hop does
**not** filter tombstones (`resolve` must walk through a tombstoned ancestor to its
orphan descendants — prototyped: children of a tombstoned `/companies/2` are found).

### 4.2 `scan(collection)` — resolved parent, ordered child range

Same chained-InitPlan resolution for the *k−1* ancestor hops, then the already-gated
ordered pattern over the final level:

```sql
SELECT c.key_wire, c.incarnation, c.value FROM {s}.nodes c
WHERE c.parent_id = (…chained InitPlan…)
  AND c.step_name = $name AND c.value IS NOT NULL
ORDER BY c.key_enc
```

Prototype plan: a single `Index Scan using node_key_lookup` with the InitPlan chain —
**no Sort node** (`key_enc` is `BYTEA`; for fixed `(parent_id, step_name)` the index
order *is* Annex-B order, memcmp). Beware the tempting flat `JOIN` formulation: the
prototype showed the planner inserts a `Sort` above a join (it cannot push ordering
through nested-loop parameterization); the scalar-subquery form is the one that plans
clean and is what the EXPLAIN gate must pin.

Top-level collections (*k* = 1) degenerate to `parent_id = 0 AND step_name = $1` —
the existing gated pattern (2).

### 4.3 `snapshot(frontier)` — indexed log read + shared Rust fold

```sql
SELECT seq, transaction_id, ops FROM {s}.commit_log WHERE seq <= $1 ORDER BY seq
```

(index-ordered by the `commit_log` PK — existing gate (4)), decoded by
`record_codec::decode_op`, folded by the **same** `Snapshot::replay` MemoryStore
uses — parity by construction, and the frontier-past-head check reads the durable
head first. The log is append-only and immutable, so this read is *logically pinned*
by `frontier`: no SQL transaction is needed for coherence (§5.4).

Cost is O(history) per call. Phase 6 adds a **head fast path**: when
`frontier == head`, materialize from `nodes` instead — one full read (today's
`node_load` reconstruction minus `by_id`), O(state). That query legitimately reads
the whole table; it is exempt from the no-Seq-Scan gate with a pinned rationale
test, exactly like `meta_tables_are_single_row` pins the single-row exemption. Its
correctness is cross-checked by a tree-equals-log-fold equivalence test (the existing
`node_tree_consistency` approach) plus the parity gate.

### 4.4 The rest — direct point/range reads

| Read | SQL | Index | Gate |
|---|---|---|---|
| `head` | `SELECT head FROM instance_meta WHERE id = 1` | single-row table | exempt (pinned) |
| `log_from(from)` | `… WHERE seq >= $1 ORDER BY seq` | `commit_log` PK | existing (3) |
| `point_position` | `SELECT seq FROM history_points WHERE lineage=$1 AND point=$2` | PK | existing (6) |
| `get_blob` | `SELECT bytes FROM blobs WHERE digest=$1` | PK | existing (5) |
| `has_blob` | `SELECT EXISTS(SELECT 1 FROM blobs WHERE digest=$1)` | PK | new (10) |
| `definition`/`composition` | `SELECT … FROM instance_meta WHERE id = 1` | single-row table | exempt (pinned) |

All existing NUL-safe codecs (`jsonb_text`), `value_codec`, and `key_wire` decoding
are reused unchanged; decode happens per read on the query result.

**No new secondary index is required.** Every read pattern rides `node_key_lookup`
or a primary key already in the DDL. `Schema::indexes()` is untouched. (Revised,
mandate 4: the reconciler grows an *extension* requirement — `CREATE EXTENSION IF
NOT EXISTS liasse` plus the ABI handshake, §7.7 — instead of v2's managed
`functions()` set, which is eliminated along with `liasse_text_key`.)

## 5. The `&self` connection model: one writer + an r2d2 read pool

Per maintainer directive: reads are served from a **pool**; no interior-mutability
hand-rolling, no contract-wide `&mut`-ification, no async rewrite.

### 5.1 Crates and fit

`r2d2` + `r2d2_postgres` (`PostgresConnectionManager<NoTls>`). `r2d2_postgres 0.18`
wraps exactly the workspace's sync `postgres 0.19` (`Cargo.lock`: 0.19.14) — the
standard, battle-tested sync pool for rust-postgres. New workspace dependencies:
`r2d2 = "0.8"`, `r2d2_postgres = "0.18"`. `deadpool`/`bb8` are async
(tokio-postgres) and are rejected: the whole crate is sync by contract design
("the traits are synchronous and `&mut`-based") and nothing here needs async.

**AGENTS.md tension, surfaced not buried**: AGENTS.md forbids interior-mutability
structures; an r2d2 pool is internally synchronized (and `Arc`-shared). Resolution:
the maintainer directed the pool; the rule's target (per `projection.rs`'s own
reading) is smuggling mutability into *our own* state types, and "do not reinvent
the wheel" points at a mature resource manager for an external resource. **Phase 0
must land a one-sentence AGENTS.md clarification** exempting third-party connection
pools for external resources, so the rule and the code stop disagreeing.

### 5.2 Writer vs pool

- **Writer**: the one `postgres::Client` a store handle owns. Used by: the admission
  SQL transaction (`commit_transition`, including `NodeWriter`), the history pass
  that follows it (§6.4), `alloc_incarnation` (§6.3), `put_blob`, `record_point`, and
  open-time reconcile/DDL. **One writer per handle, not per instance**: the factory
  will open as many handles onto one instance as asked, and admission takes no
  instance-wide lock, so their admissions overlap.
- **Pool**: read-only in usage (not enforced by role — same DSN, same rights). Every
  `&self` read: `self.reads.get()` → run statement(s) → drop guard (auto-return).

**Read-your-committed-writes**: writer and pool hit the same server/database. A
commit returns only after its SQL transaction commits; any later pooled statement's
READ COMMITTED snapshot includes every committed transaction. So a read after
`commit()` observes the new head — no lag, no fencing needed. There is no window
where a pooled read sees a *partial* commit (single SQL txn, atomic visibility).

### 5.3 Configuration, lifecycle, failure

- **Placement**: one pool per `PgStore` (per instance), built by the factory **after**
  `reconcile` succeeds, so every pooled connection sees the reconciled schema. All
  SQL is schema-qualified; no `search_path` per connection.
- **Sizing**: `max_size = 4`, `min_idle = Some(0)` (lazy — a test suite opens many
  instances; idle floors would multiply connections), `connection_timeout = 5s`,
  `test_on_check_out = false` — a validity ping per checkout is a full round trip,
  which the near-raw-overhead gate cannot afford; a dead connection surfaces as a
  query error mapped to `StoreError::Backend` (fail loud; r2d2 discards broken
  connections on return and re-establishes on demand — that is the reconnect story).
  `PgStoreFactory` gains an optional pool-size knob; defaults stay boring.
- **Teardown**: dropping `PgStore` drops the pool and its connections. `drop_instance`
  is unaffected.

### 5.4 Consistency taxonomy (which read needs what)

1. **Single-statement reads** — `row`, `scan`, `scan_subtree`, `scan_view`,
   `head`, `log_from`, `point_position`, `get_blob`, `has_blob`, `definition`,
   `composition`: one SQL statement = one MVCC statement snapshot. Internally
   consistent on any pooled connection, autocommit. Nothing to pin.
2. **Pinned multi-statement reads** — `snapshot(frontier)`. Its positioned
   `commit_log` prefix `≤ frontier` is immutable, but the *head* it compares against
   and the `nodes` tree its fast path materializes are not, so the whole read runs on
   one `REPEATABLE READ READ ONLY` transaction: head, the unpositioned-admission
   check, and the materialization or fold all come from one MVCC snapshot. The
   `nodes` fast path is taken only when that snapshot sees **no unpositioned
   admission** — state can be briefly ahead of built history (§6.4), and a snapshot
   must not show state its own frontier does not cover.
3. **Multi-statement sequences above the contract** — `Prospective::gather` issues
   many `scan`s expecting one coherent state, and it runs *outside* the admission
   transaction, on a pooled connection. With overlapping admissions this is the
   backend's remaining isolation gap: PostgreSQL is never shown the read set, so it
   cannot detect a read-write conflict, and the durable write path catches only what
   it can see by itself (§6.5). Closing it means opening the admission transaction
   before the first read and serving the gather through it — the `read_session()` /
   transaction-boundary work, which is **not** done. The §12 path additionally
   *prefers* `snapshot(head)`-based hydration (case 2) over N live scans — see §8.

## 6. Write-path changes

### 6.1 `NodeWriter` loses the projection

`NodeWriter` currently resolves parents/ids against `projection.by_id`. Replace with
in-transaction SQL resolution: `resolve_id(address)` = the same
`(parent_id, step_name, key_enc)` hop chain as §4.1 but **executed on the admission
transaction** (`&mut Transaction`), so nodes inserted earlier in the same admission
are visible; results memoized in the existing per-transaction `staged` map. The
`new_ids`/`apply_node_id` plumbing (which existed only to advance `by_id`) is
deleted. Tombstone auto-creation (`resolve_or_create`) is unchanged in behavior —
it already runs SQL; only its map lookups change source.

### 6.2 `commit_transition` takes no position and no lock

The admission transaction writes **state only**: the ops into the `nodes` tree, and a
*residual* into `commit_log` — a row with no `seq`, keyed by `pg_current_xact_id()`.
It reads no counter, locks no row, and touches `instance_meta` only when the
transition actually changes the definition or composition. There is no head counter
left to lock: the head is the built history's tip (§6.4). `record_point`/`put_blob`
have no projection mirrors.

### 6.3 Incarnation allocation — durable, burn-on-allocate, lock-free

The counter is persisted state, so it lives in and is read from Postgres only.
`alloc_incarnation` is one autocommit statement on the writer connection:

```sql
SELECT nextval('{s}."incarnations"')
```

— the token is `row-{returned}`. Staging is a pure in-memory overlay
(`transition.rs`: "Nothing touches PostgreSQL until commit"), so this statement
never runs inside an open SQL transaction; it commits by itself, immediately.

It is a **`SEQUENCE`, not a counter row**, and the reason is the same one that
governs §6.2: a counter row is a per-instance meeting point, and every staged insert
would queue on it. A sequence has none of that — `nextval` takes no row lock — and it
loses nothing, because the property this counter must have is *no reuse*, not *no
gaps*. `nextval` is non-transactional, which delivers no-reuse exactly.

**Abort-visibility parity — why burn-on-allocate is the correct form.**
`MemoryStore::alloc_incarnation` advances its counter *at allocation time*; an
aborted staging does not roll it back, so in-process tokens are never reused
("gaps from aborted transitions are harmless" — `memory.rs`). Two durable designs
were considered:

- *Allocate inside the admission transaction* — rolls the counter back on abort,
  **reusing** tokens the oracle would not reuse: an observable divergence on any
  abort-then-commit scenario the parity gate runs. Rejected.
- *Burn on allocate* (the autocommit `UPDATE … RETURNING` above) — the counter
  advances durably whether or not the staging later commits. In-process behavior is
  **identical** to the oracle. Across a reopen it is *strictly more faithful* than
  the old projection design, which persisted the counter only at commit and so
  reused burned tokens of aborted stagings after a reopen. Accepted.

Cost: one extra round trip per allocated token during staging (per staged insert).
A **batching seam** is designed but not built: when staging knows it needs *k*
tokens, `nextval('…', $k)` — or a `setval`-free `SELECT nextval FROM
generate_series(1, $k)` — allocates the range in one statement. The returned range is
*consumed durable state* handed to the session, not a cached read model, so it stays
mandate-compatible; wire it only if the benches show allocation dominating admission.

### 6.4 History construction — where serial positions come from

§22.1: *"History construction follows committed transitions independently of write
admission. Independent writes may be captured concurrently."* So positions are not
part of writing. A **history pass** (`history.rs`) stamps them afterwards:

```sql
WITH settled AS (
  SELECT xid, row_number() OVER (ORDER BY xid) AS offset_from_tip
  FROM {s}.commit_log
  WHERE seq IS NULL AND xid < pg_snapshot_xmin(pg_current_snapshot())
), tip AS (SELECT coalesce(max(seq), 0) AS at FROM {s}.commit_log)
UPDATE {s}.commit_log AS log SET seq = tip.at + settled.offset_from_tip
FROM settled, tip WHERE log.xid = settled.xid
```

**The watermark is the correctness.** `pg_snapshot_xmin` is the oldest transaction id
still running; every transaction below it has ended and no future one can be numbered
below it. A pass therefore positions a *complete final prefix* in `xid` order, and
stops dead at the first still-in-flight admission — so history never publishes a
later admission ahead of an earlier one that has not settled. That is the anomaly a
`nextval`-during-admission design would introduce, removed by construction rather
than mitigated.

**Where it runs.** Inline at the end of every admission (`PgStore::settle`), which
also resolves that admission's own position before `commit_transition` returns — so
`CommitOutcome::Committed` still carries a final position, never a reservation. It is
also callable on its own (`PgStore::build_history`) for a background builder. If an
older admission is in flight, `settle` waits for it: not a lock, and not a wait any
*state write* is behind — every concurrent admission's writing has already happened
in parallel by then.

**Its one lock.** A pass takes a transaction-scoped advisory lock on the schema, so
two passes cannot both read the same tip and hand out the same positions. It
serializes *history construction*, which is the building of one linear order and is
inherently serial. No admission ever takes it.

**Gaps.** Positions come out gapless because a residual is written inside the
admitting transaction, so an aborted admission leaves nothing to skip. That is a
consequence, not a requirement — §22.3 asks only for monotonicity, and nothing here
works to preserve contiguity.

**The observable gap.** Between an admission committing and a pass positioning it,
its state is live (`row`/`scan` see it) while history does not carry it (`head`,
`log_from`, `snapshot` do not). This is deliberate and §22.1 is what makes it legal.
Every read states which side it is on: current-state reads are head-independent;
positional reads are built history; and `snapshot`'s head fast path checks for
unpositioned admissions rather than assume the tree matches the head (§5.4 case 2).

### 6.5 What the durable write path can and cannot catch

Admission's occupancy and identity checks run during staging, against a read taken
before the transaction opens and on another connection (§5.4 case 3). With
overlapping admissions those reads can go stale, so the durable write re-checks what
it is able to see for itself:

- **Placing a row** (`Insert`, and a `Rekey`'s target) is `INSERT … ON CONFLICT DO
  UPDATE … WHERE nodes.value IS NULL`: it revives a tombstone and refuses a live row.
  A conflicting live row returns no row, and the admission fails
  `StoreError::Conflict`.
- **Rewriting or removing a row** (`Update`, `Delete`, a `Rekey`'s source) matches on
  the incarnation the op carries: `WHERE id = $1 AND incarnation = $2`. Zero rows
  touched means the row named is no longer the row that is there, and the admission
  fails `StoreError::Conflict`.

**Not caught: two concurrent updates of the same row.** §22.6 makes an update
preserve the row's incarnation, so both writers' guards match and the later one wins
silently — and because the engine materializes the whole row, a concurrent write to a
*different field* is what gets lost. Catching it needs a per-row version observed by
the read and asserted by the write, which requires the read set to be inside the
admission transaction (§5.4 case 3). Until that lands, this backend does not detect
update/update conflicts and must not be described as if it does.

## 7. The general in-PG evaluator: ANY read-side expression over persisted rows

### 7.1 What changed and why — from a predicate function to the general evaluator

v2 satisfied mandate 3 by compiling a statically checked predicate *fragment* to
exact-semantics SQL: a store-level `RowPredicate` IR, an operator-by-operator SQL
table (decimal-through-`numeric`, `liasse_text_key` for NUL-escaped text order, a
none-rank CASE), and a load-time restriction of §10.5 predicates to the
compilable fragment, with a proposed normative SPEC line. That design carried an
honest, permanent cost: **a second evaluator** for the fragment, an enumerated
fragment boundary users hit, and a parity obligation renewed with every operator
the fragment ever grows.

Mandate 4 removed the second evaluator instead of containing it: **link the one
interpreter into PostgreSQL.** A `pgrx` extension crate builds the same
`liasse-expr` evaluator the runtime and MemoryStore execute into a `.so` loaded
by the self-hosted PostgreSQL. Because it is literally the same Rust code
evaluating the same `TypedExpr` against the same truthiness contract, parity with
the MemoryStore oracle is **by construction**, and expressiveness over the
candidate is the full core language — field access (including nested static
structs), computed fields, arithmetic (including its error paths), string
builtins, `has`/`in`/`size`, comparisons, logic, ternary, `$key` — with **no
fragment table and no per-operator SQL**.

Mandate 5 draws the conclusion v3 stopped short of: if the real interpreter is
in the database, restricting it to `$where`/`$except` is arbitrary. The
extension becomes a **general evaluator** — `liasse.eval` takes any serialized
checked expression, a stored row, its key, and a hoisted env, and returns the
serialized result value; a boolean predicate is the special case whose result is
consumed as a SQL boolean (§7.4). On top of it, **read-side evaluation over
persisted rows pushes down**: a `$view` over a stored collection — source scan
`+` optional filter `+` projection `+` `$sort` (§7.1/§7.3 of SPEC.md) — is
served by ONE index-served SQL statement in which the extension evaluates the
filter in `WHERE`, the projection (computed fields folded, §5.2) in the `SELECT`
list, and the sort key in `ORDER BY`, with `$skip`/`$limit` as `OFFSET`/`LIMIT`.
The evaluated view rows come straight from PostgreSQL — no
hydrate-everything-then-evaluate-in-Rust for the covered cases. The §10.5
coverage read of mandates 3+4 is re-derived as the **recursive source shape** of
this one mechanism (§3 `ViewSource::Coverage`): same programs, same faces, same
CTE — pruning-during-descent is what "the filter runs in `WHERE`" means when the
source is recursive.

What does NOT push down in v1 is **scope, not restriction** (mandates 6+7):
§7.5 enumerates the deferred source dispositions (combinators, view-refs that
do not compose, bucketed and engine-state sources, the aggregate fold) — the
per-row expression universe of a *checked* DB-run program has no excluded
class: every core-language operator and every built-in namespace function over
the candidate's own row AND its nested subtree evaluates in-PG (§7.5, §7.6),
and an app-registered call cannot occur in such a program because the checker
rejected it at load (mandate 7, §7.5). What does not push down in v1 by
EXECUTION-MODEL choice is admission/staging evaluation: it runs over the
staged in-memory overlay that `nodes` does not yet contain (§5.4 case 3), so
`$check`/`$normalize`/defaults and the mutation program itself stay
interpreter-executed in Rust — the mutation body because it is the one
framework-run context (mandate 7), and `$check`/`$normalize`/defaults as the
v1 executor choice for DB-run-classified, built-in-only expressions (§7.5
"admission under the mutation-only rule": classification restricts the
expression, not the executor; the in-transaction PG admission seam is now
unlocked by construction but deliberately not v1). MemoryStore always
evaluates in Rust — it is the oracle that makes the 0-divergence gate
meaningful.

**Eliminated from v2, explicitly**: the predicate→SQL compiler; the
`RowPredicate`/`PredOperand`/`CompareClass` store-level IR; the §7.4 fragment
definition; the §7.5 exact-semantics operator table (and with it the `numeric`
bound, the none-rank CASE, the wire-injective-ref rule, and the
optional-struct-member exclusion); the `liasse_text_key` managed function and the
reconciler's `functions()` declared-set; the §7.7 fragment policy and its
proposed SPEC.md §10.5 fragment line (replaced in v4 by a narrower §7.5 note,
withdrawn in v5, and re-founded in v6 as the mandate-7 language rule: the one
restriction on DB-run programs is built-in-only namespaces, enforced by the
checker at load, not by a lowering-time fragment policy — §7.5).
What v2 pinned and this design keeps: the §7.2 execution semantics, the anchor-
unfiltered and tombstone-barrier rules, the CTE shape and its index-served plan
discipline, `scan_subtree` as the distinct semantics-free primitive, and the
adversarial predicate corpus (now a regression backstop, §9).

### 7.2 How read-side evaluation executes today — the semantics to reproduce

**The general view path.** Every read evaluation is a pure function of an
environment whose root is the materialized package-root `Row`
(`materialize.rs`): `EvalCtx::root` builds the WHOLE root — every collection's
rows with nested collections (`build_row`), computed values folded to a fixed
point (`fold_computed`, §5.2: fault ⇒ `none`, non-scalar ⇒ skip), keyrings,
source buckets, module folds, meter accessors, then every declared view folded
onto the root as a same-named cell (`expose_views`, to a fixed point) — and the
view expression evaluates against it (`TypedExpr::evaluate`). A surface `$view`
(`Engine::view_with`) additionally binds `@params` and `$actor`/`$session`; the
result becomes a `ViewResult` (`view.rs`): per row the projected scalar/struct
output fields, the `$sort` tuple, and the key-derived `RowId` (D.1) — exactly
the triple `EvaluatedRow` carries (§3). Two structural facts the pushdown must
mirror:

- **Projection output** (`view.rs::cell_field_value`): a `none` optional output
  is an omitted member; a keyless nested row is carried inline as a
  `Value::Struct`; a keyed nested cell (sub-view) is NOT part of the row's
  scalar projection — it is a separately-addressed stream. So `project` (§3)
  returning one `Value::Struct` of scalar/struct outputs is the complete §12
  row payload, not an approximation.
- **Order** (`§7.3`, `order.rs`): the delivered order is the `$sort` tuple
  compared per key direction under the Annex-B total order, with
  PostgreSQL-style absence placement (ascending: present then `none`;
  descending: `none` then present reversed) and occurrence identity as the
  final tiebreak. An unsorted view delivers source (key) order.

**The §10.5 coverage path.**
`recursion.rs` (`CompiledRecursive`): the covered row is materialized with its full
nested tree (`materialize_row_cell`), then `cover` walks it: **the covered root is
projected unconditionally** (predicates admit *candidates*, i.e. descendants — the
root is admitted by §10.3 scope membership), and at each level a candidate is
included iff `$where` holds (default include) and `$except` does not (deny-list
overrides); recursion descends only into included candidates — both hereditary.
`predicate()` evaluates with the candidate bound to `$bind` *and* as `.`
(`eval_with(prospective, pred, candidate, {bind: candidate})`), and consumes the
result as `matches!(v, Value::Bool(true))` — anything not literally `true` reads
as `false`. Compile-time scope (`compile_recursive`): `.` = the candidate row type,
`/` = the package root, `$bind`, surface `@params`, structurals
(`$actor`/`$session`), host ops. Two structural facts the SQL must mirror:

- **The anchor is unfiltered.** The earlier §7.3 prototype applied the predicate to
  the anchor row — that was wrong against `recursion.rs`: the covered root always
  appears. The compiled CTE applies predicates only in the recursive term.
- **A tombstone blocks its subtree.** Coverage candidates are the materialized
  *live* rows of `$field`; a deleted intermediate has no row cell, so its retained
  orphans are unreachable through coverage. Hence the coverage CTE keeps
  `value IS NOT NULL` in the recursive term as a *traversal barrier* — the opposite
  of `scan_subtree`, which traverses tombstones for semantics-free hydration. The
  two primitives are deliberately distinct.

One more implementation fact the in-PG evaluation must reproduce exactly: the
candidate `Row` the interpreter binds is built by `build_row` + `fold_computed`
(`materialize.rs`, `eval.rs`) — every declared non-collection member gets a cell
(absent stored field ⇒ `none`), nested keyed collections get materialized
`Cell::Collection`s, and computed values (§5.2) fold in at fixed point with a
faulting or non-scalar computed left as `none`. §7.4 defines how the extension
rebuilds precisely this candidate (minus the parts an audited predicate can never
read).

### 7.3 Architecture: who lowers, who evaluates, who falls back

```
liasse-expr  (feature `eval-wire`, new `wire` module)
  • serde derives (postcard) on the closed set: TypedExpr/TypedKind, ExprType,
    Value, Cell/Row/RowId — version-locked, never a public wire format (§7.4)
  • candidate-dependence classification + HOIST: every maximal candidate-free
    subtree is evaluated ONCE (callback into the runtime interpreter) and
    replaced by a synthetic binding; its value ships as an env entry
  • SUBTREE READ-SET extraction: the candidate-relative paths that descend
    through nested keyed collections (and those reached by carried computed
    expressions, transitively) are collected into the program's step universe
    — the `subtree_steps` that direct the §7.6 prefetch lateral. Always
    statically extractable: the checker types every candidate-relative path,
    and a recursive shape (§5.8) yields a FINITE step universe with unbounded
    depth, which the guarded recursive lateral serves
  • RESIDUAL AUDIT: confirms the source lowers and no scope-deferred form
    (§7.5 dispositions) remains, reporting a typed, span-carrying reason
    otherwise; per mandates 6+7 the per-row expression universe of a checked
    DB-run program is total — the checker's load-time DbRead rule (§7.5) has
    already excluded app-registered calls, so the audit classifies sources
    and scope, it never excludes expressions

liasse-pred  (new crate: the ONE row-program implementation)
  RowPrograms {
      admit:      Option<hoisted TypedExpr>  (filter; for coverage the composed
                                              `$where && !$except`, §7.2 order)
      project:    hoisted projection outputs (name → TypedExpr, dep-ordered)
      sort:       Vec<(hoisted TypedExpr, direction)>   ($sort keys, §7.3)
      env:        Vec<(SyntheticName, Cell)> (hoisted candidate-free values)
      bind:       Option<String>              ($bind / filter binding name)
      candidate:  CandidateDescriptor         (declared scalar/struct members,
                                               carried computed exprs in fold
                                               order, key arity, and the
                                               subtree read-set: the nested-
                                               collection steps + shapes the
                                               program reads)
  }
  implements liasse-store's ViewProgram (§3):
  subtree_steps() = the descriptor's read-set step universe (empty ⇒ shallow)
  admits / project / sort_tuple (value, key, subtree: &CandidateSubtree)
     = build candidate Row (descriptor-driven, absent ⇒ none; the read-set's
       nested Cell::Collections materialized from the prefetched subtree rows
       by rel path — the DEEP candidate when the program reads through it)
       → fold carried computed exprs (the shared fixed-point fold)
       → TypedExpr::evaluate(ProgEnv{env, bind→candidate}, candidate)
       → consume per face: strict Bool(true) | output Value::Struct | tuple
  to_wire()/from_wire() per face + env: postcard; EVAL_ABI: the version-lock
  constant (§7.7); sort-tuple ordering via the shared Annex-B comparison and,
  for the pushdown side, the order-preserving `sort_enc` (liasse-pg-codec §7.4)

liasse-store
  ViewProgram trait + CandidateSubtree + ViewSource + scan_view (opaque; §3),
  scan_subtree(steps)
     MemoryStore: range scan / BTreeMap descent calling the faces, sorting by
                  the shared tuple comparison; per-candidate depth-guarded
                  live descent for subtree programs  — evaluates in Rust
     PgStore:     ONE SQL statement calling liasse.eval_bool / liasse.eval /
                  liasse.eval_sort, with the §7.6 prefetch lateral when
                  subtree_steps is non-empty         — evaluates in SQL

liasse-pg-ext  (new pgrx cdylib crate; §12.1) — ONE evaluator, three SQL faces
  #[pg_extern] liasse.eval(expr, value, key_wire, env, subtree)      -> jsonb
  #[pg_extern] liasse.eval_bool(expr, value, key_wire, env, subtree) -> boolean
  #[pg_extern] liasse.eval_sort(expr, value, key_wire, env, subtree) -> bytea
  #[pg_extern] liasse.guard_depth(depth, max)                        -> boolean
     = from_wire(expr ⊕ env)  [per-backend LRU-cached]
       → value_codec::decode(value), key from key_wire, subtree rows decoded
         by the same codecs (jsonb array of (rel path, value), §7.4)
       → THE SAME RowPrograms face → tagged-wire jsonb | strict truthiness |
         sort_enc order-preserving bytes
     guard_depth: true below the bound, reserved-SQLSTATE error past it —
       every recursive descent's cycle/depth bail (§7.6)

liasse-runtime (view/coverage lowering, new module beside recursion.rs)
  compile: lower each surface/declared $view and each $recursive block:
     Lowered::Pushdown(RowPrograms + ViewSource + bounds)  — the live path
     Lowered::Fallback(reason)                             — per §7.5 policy
  read path (Engine::view_with / CompiledScope::materialize / watch advance):
     head-frontier read of a lowered view ──▶ store.scan_view → ViewResult
          (rows already filtered, projected, sorted; coverage rows nested into
          the §10.5 keyed tree by key_path)
     non-head frontier (§19.2 replay/resume) ──▶ today's path: snapshot(frontier)
          hydration + interpreter evaluation (correct by construction, off the
          live path)
     hoist-eval error, or StoreError::Eval from the store ──▶ same fallback
          (reproduces interpreter behavior exactly, including which error and
          the per-candidate short-circuit timing)
     scope-deferred view shape (§7.5 dispositions — the ONLY static
          fallback class left) ──▶ per-view interpreter fallback, recorded
          in the load-time pushdown report (§7.5 policy)
     admission receiver walk (resolve_receiver) ──▶ stays interpreter-based: it
          checks ONE key path with point reads under a staged overlay the nodes
          table does not yet contain — not the perf killer, and CTE-over-nodes
          would be unsound mid-staging
     admission/staging evaluation ($check/$normalize/defaults/programs,
          Prospective::gather) ──▶ interpreter-executed in Rust in v1: the
          mutation program because it is THE framework context (mandate 7);
          $check/$normalize/defaults by executor choice — they are DB-run
          classified, built-in-only, so the in-txn PG admission seam exists
          by construction (§7.5), deliberately not built in v1
```

The composed coverage admit (`$where && !$except` over the bound candidate) is
semantics-preserving against `CompiledRecursive::included`: the interpreter
short-circuits `&&` left-to-right, so `$where` evaluates first and a failing
candidate never reaches `$except` — the same order and the same fault surface as
the two-call form, in one program and one SQL call per row.

**Hoisting rule (kept from v2, now feeding an env instead of SQL parameters).** A
maximal subexpression that does not reference `$bind`/`.` (the candidate) is
constant across the whole descent. The compiler evaluates it once in Rust — same
interpreter, same frontier state, same session env — and binds the resulting
`Cell` (scalar, row, or whole collection — e.g. an `in /admins` haystack) under a
synthetic name outside the identifier grammar (NUL-prefixed, so no source binding
can collide). This is semantics-preserving because every built-in namespace
function is deterministic ("same logical inputs produce the same output",
§16.3 `pure` — and a DB-run program contains no app-registered call at all,
by the mandate-7 load rule, so hoisting never has to reason about one),
`now()` is the fixed per-operation sample (A.5), and `uuid()` is
pinned per call site per request — one evaluation equals N. Hoisting remains
an *optimization* for candidate-free subtrees (evaluate once, not per row),
never a necessity: a candidate-dependent built-in call evaluates in-PG. The one observable difference is error timing:
the interpreter, short-circuiting per candidate, might never evaluate a subtree
the hoist evaluates eagerly. So a hoist-eval **error never surfaces**: it routes
to the interpreter fallback, which reproduces the exact per-candidate behavior,
error or not. Session values (`$actor`, `$session`, `@params`) are exactly
hoisted env entries — the session-relative values crossing into the query as
parameters, per mandate 2.

**The one seam that is NOT literally shared code, named**: `admits`'s shallow
candidate construction (descriptor-driven member cells + the computed fold)
reconstructs what the runtime's `build_row`/`fold_computed` produce. The computed
fold itself moves into `liasse-pred` and the runtime delegates to it (literally
shared); the member-cell construction is a few descriptor-driven lines whose
equivalence the layer-1 lowering-parity gate (§9) checks against the real
`CompiledRecursive::included` over the corpus. Everything downstream — the
evaluator, `Value::cmp`, truthiness — is the same linked code in all three
executors (runtime, MemoryStore, extension).

**Frontier scope (unchanged from v2).** The `nodes` table holds head state, so
the pushdown serves reads at `frontier == head` — every live view materialization
and every §12 watch advance. Historical frontiers (§19.2 replay, resume of a
stale client) fold the log and prune in Rust, as today: correct, off the hot
path, and unavoidable without versioned rows.

**What the pushdown actually saves (extended by mandate 5).** For coverage, the
covered subtree no longer round-trips: pruned branches are never fetched,
included rows arrive in one statement already projected and in depth-first
order. For a flat pushed `$view`, the collection no longer round-trips as whole
rows to be materialized and evaluated in Rust: filtered-out rows are dropped at
the heap, the wire carries only projected output columns, and a `$limit` view
transfers only its bounded top-N. State a program needs *outside* the candidate
(the `$actor` row, hoisted `/`-reads) arrives via `env` from ordinary §4
point/scan reads — never a full hydration.

### 7.4 The extension function contract — one evaluator, three SQL faces

The extension installs ONE evaluator behind three SQL faces, in its own schema
(never the instance schema — it is shared by every instance in the database).
The faces exist because SQL types the call site: a `WHERE` clause consumes
`boolean`, a `SELECT` list consumes the result value, an `ORDER BY` consumes a
memcmp-orderable key. All three deserialize the same wire, build the same
candidate, and run the same interpreter; they differ only in how the result
`Cell` is consumed — and each consumption is the *same* consumption the Rust
side performs (§7.3), so the faces add no semantics.

```sql
-- the general face: the serialized result Cell, tagged wire form (value_codec)
FUNCTION liasse.eval(expr bytea, value jsonb, key_wire jsonb, env bytea,
                     subtree jsonb)
RETURNS jsonb
IMMUTABLE STRICT PARALLEL SAFE COST 100

-- the predicate face: strict truthiness, consumed as `… IS TRUE`
FUNCTION liasse.eval_bool(expr bytea, value jsonb, key_wire jsonb, env bytea,
                          subtree jsonb)
RETURNS boolean
IMMUTABLE STRICT PARALLEL SAFE COST 100

-- the ordering face: the order-preserving sort_enc bytes of the evaluated
-- $sort tuple (direction-folded), consumed in ORDER BY
FUNCTION liasse.eval_sort(expr bytea, value jsonb, key_wire jsonb, env bytea,
                          subtree jsonb)
RETURNS bytea
IMMUTABLE STRICT PARALLEL SAFE COST 100

-- the descent guard: true while depth <= max, a reserved-SQLSTATE (LQ002)
-- error past it — placed in EVERY recursive term (§7.6) so cyclic/corrupt
-- data bails loudly instead of hanging or truncating
FUNCTION liasse.guard_depth(depth integer, max integer)
RETURNS boolean
IMMUTABLE STRICT PARALLEL SAFE
```

*Deviation from the mandate sketch, deliberate*: the sketch was one
`eval(…) -> jsonb` with the bool case "consumed as `IS TRUE`", but `IS TRUE`
does not type-check over `jsonb` — the faithful realization is a typed boolean
face over the same evaluator (`eval_bool(…) IS TRUE`), and the ordering
consumption needs bytes whose memcmp order IS the Annex-B tuple order, which a
jsonb result cannot provide (PostgreSQL's jsonb ordering is not Annex B's).
A composite-returning `eval_row` (admit + projected + ord in one call) is a
documented optimization seam, not the design: separate faces keep each call in
its natural clause, where the planner places it (§7.6/§7.8), and the decode
cache makes the shared prefix work (wire decode, candidate build) cheap.

- **`expr`** — `postcard`-serialized program face *minus* its env: the hoisted
  `TypedExpr` (for `eval_sort`, the sort-key list with directions), the bind
  name, and the `CandidateDescriptor`. Stable for the lifetime of a compiled
  view, so its deserialization is cached (below).
- **`value`** — the candidate row's stored tagged wire form, exactly the `nodes.value`
  column (`value_codec`). The extension decodes it with the *same* codec the
  store uses (`liasse-pg-codec`, §12.1) and builds the shallow candidate per the
  descriptor: every declared non-collection member a cell, absent ⇒ `none`;
  computed members folded by the shared fold.
- **`key_wire`** — the candidate's key in its decodable wire form (the
  `nodes.key_wire` column), decoded to the typed `KeyValue` and exposed as the
  candidate's key identity (lone scalar or positional composite, per the
  descriptor's key arity) — what `child.$key` and the ref-vs-row coercion read.
  *Deviation from the mandate sketch, deliberate*: the sketch passed `key_enc`,
  but `key_enc` is the order-preserving memcmp form the store never decodes;
  `key_wire` is the decode source the read path already relies on (§4). The CTE
  has both columns in hand; passing the decodable one is strictly simpler than
  teaching the extension to invert `key_enc`.
- **`env`** — `postcard`-serialized `Vec<(SyntheticName, Cell)>`: the hoisted
  candidate-free values, shared by every face of one lowered view. Separate
  from `expr` because it varies per *read* (session `$actor`, `@params`,
  `/`-read collections at this frontier) while `expr` varies only per *view* —
  so each blob caches at its own rate.
- **`subtree`** — the candidate's prefetched live subtree when the program's
  read-set is non-empty (§7.6): a jsonb array of `[path, value]` elements, one
  per live row strictly under the candidate through the read-set steps, where
  `path` is the array of `[step_name, key_wire]` pairs from the candidate down
  to the row. Assembled **by the SQL side** (the §7.6 lateral's `jsonb_agg`) —
  which is why it is a separate jsonb argument and not an `env` entry: `env`
  is Rust-built postcard, and SQL cannot produce postcard. The extension
  decodes each row with the same `value_codec`/`key_wire` codecs and
  materializes the read-set's nested `Cell::Collection`s under the candidate
  by rel path (collection cells are key-ordered maps, so no SQL-side ordering
  is required of the aggregate). For a shallow program the SQL passes the
  literal `'[]'::jsonb` (no lateral is planned at all), keeping `STRICT`
  honest — the argument is never NULL. No per-backend caching: unlike
  `expr`/`env` the blob is per-candidate by nature.
- **Result consumption, per face** — `eval`: the result `Cell` in tagged wire
  form as jsonb — a projected row is its output `Value::Struct` (a `none`
  output an omitted member, §7.2), a scalar its tagged value; the store decodes
  it with the same `value_codec`. `eval_bool`: the truthiness contract
  verbatim, `matches!(result, Cell::Scalar(Value::Bool(true)))`; anything else
  is `false`. `eval_sort`: the evaluated sort tuple encoded by `sort_enc`
  (below). An evaluation fault (an `EvalError` — e.g. division by zero on the
  candidate's values) is reported identically by all faces via `pgrx`'s error
  path as a PG error with a reserved SQLSTATE (`LQ001`) and the sanitized
  message; `PgStore` maps that SQLSTATE to `StoreError::Eval`, and the runtime
  answers with the interpreter fallback (§7.3) so the surfaced error is the
  interpreter's own — including *which* candidate errors first, which SQL
  evaluation order does not promise. (The §5.2 computed-fold exception stands:
  a *computed member's* fault folds to `none` inside the candidate build, by
  the shared fold — only the program's own evaluation faults escape.)
- **`sort_enc` (new, in `liasse-pg-codec`)** — an order-preserving byte
  encoding of an evaluated `$sort` tuple: per key, a rank byte placing `none`
  per §7.3 (ascending: after every present value; descending: before), then
  the value's Annex-B-order-preserving bytes (the `key_enc` machinery — sign-
  flipped big-endian numerics through the shared decimal normalization,
  NUL-escaped text — extended with the non-key-eligible scalar classes in
  their Annex-B class rank), with every byte inverted for a descending key so
  one ascending memcmp realizes mixed directions. The occurrence tiebreak is
  NOT encoded — the SQL appends the key-path columns (`key_enc` / the coverage
  `sort_path`) as trailing `ORDER BY` terms, which IS the D.1 occurrence
  identity order. **Honest limit**: a sort key of static type `json` would need
  the full Annex-B JSON internal order encoded byte-comparably; that is
  designable (the JSON order is total) but is NOT built in v1 — a view sorting
  on a `json`-typed key falls back per §7.5, recorded in the pushdown report.
- **Volatility** — `IMMUTABLE` is truthful for all three faces: each is a pure
  function of its five arguments (the interpreter is pure; every
  nondeterminism source was hoisted into `env` by construction, and a
  candidate-subtree read arrives AS an argument — the prefetched `subtree` —
  rather than as a table access, which is exactly why option (a) of §7.6
  preserves `IMMUTABLE` where the SPI alternative could not). `STRICT`
  makes a NULL `value` (a tombstone, if the planner ever reorders around the
  `value IS NOT NULL` barrier) yield NULL — filtered/last-ordered, never
  evaluated. `PARALLEL SAFE` is truthful (no state beyond a per-backend
  cache); recursive CTEs do not parallelize today, so it is future-proofing,
  not a load-bearing claim. `COST 100` tells the planner these calls are
  expensive relative to `c.value IS NOT NULL`, so the cheap conditions order
  first.
- **Serialization** — `postcard` over feature-gated `serde` derives
  (`liasse-expr` feature `eval-wire`; `liasse-value` feature `serde`) on the
  closed type set. This is an **internal, version-locked wire**: producer
  (runtime) and consumer (extension) are required to be the same build (§7.7
  handshake), so no cross-version stability is promised or needed, and the
  derives impose no public-format obligation. A proptest round-trip gate (§9)
  pins encode∘decode = id.
- **Per-backend decode cache** — deserializing `expr` per row would be
  O(rows × |expr|). The extension keeps a small per-backend (thread-local — a PG
  backend is single-threaded; parallel workers each get their own) LRU, keyed by
  a 128-bit hash of the blob, holding deserialized programs and env tables. One
  query passes byte-identical blobs — and the three faces of one view share the
  candidate descriptor and env — so every call after the first is a
  hash-lookup. This is an infrastructure cache over immutable inputs, not a
  data projection — same footing as the §11 prepared-statement cache and the
  r2d2 pool. The remaining per-row duplicated work across faces (each face
  re-decodes `value` and rebuilds the shallow candidate) is the price of the
  three-face shape; the composite `eval_row` seam (above) exists if the §9
  bench axis shows it dominating.

### 7.5 The two execution contexts, the load rule, and the hoisting boundary

With the full interpreter linked in, the boundary is no longer "which operators
compile" but "which *inputs* exist inside PostgreSQL" — and under mandates 6+7
**every input a checked read-side program can need exists there**: the
candidate's stored row and key (the CTE columns), the hoisted candidate-free
environment (`env`), the candidate's own live subtree (the §7.6 prefetch), and
— because the checker enforced it at load — only functions the extension
links: the built-in namespaces. The lowering audit (§7.3) classifies sources
and scope; it excludes no expression.

**The two execution contexts (mandate 7).** Every SPEC expression position is
classified by WHERE its evaluation runs when a conforming engine serves it,
and the discriminator is maximally simple (maintainer correction, binding):
**framework-run = the mutation program body, nothing else** — a mutation is
modeled as a transaction, and the transaction is the only place an
app-provided procedure can run. Everything that *reads or validates* is
database-run and built-in-only. The complete classification, every
expression-bearing SPEC context:

| SPEC context | Position | Class |
|---|---|---|
| §7.1/§6.4/§10.1/§10.2 | `$view` source selection + `[:x \| …]` filter (declared, surface, and public views) | **DB-run** |
| §7.1/§7.2 | `$view` projection outputs, synthetic `$key` groupings, `group` aggregates | **DB-run** |
| §7.3 | `$sort` keys (on views AND on collections), `$skip`/`$limit` values | **DB-run** |
| §7.4/§7.5/§7.6 | view-combinator operands, aggregate arguments, ref traversal in view positions | **DB-run** (the merge/fold machinery is engine code, not an expression) |
| §10.3 | role `$members` views | **DB-run** (admission *evaluates* it framework-side — see the artifact/executor rule below) |
| §10.5 | coverage `$where`/`$except`, `$through` | **DB-run** (the admission re-walk is framework *execution* of the same artifact) |
| §10.1 | surface `$mut` receiver references (`.projects[@project].rename` — the selector outside the body) | **DB-run** (a read/selection; the *named mutation's body* is framework) |
| §5.2 | computed values | **DB-run** |
| §5.10/§8.8 | `$check` (field, struct, row) | **DB-run** (built-in-only; v1 executor = the interpreter in the admission transaction — mechanism block below) |
| §8.8 | `$normalize` | **DB-run** (same; its output is stored state) |
| §11.3 | auth `$verify`, `$actor`, `$session`, auth `$check` | **DB-run** (built-in-only + native §17.7 keyring verification; custom verification re-models as an auth mutation — below) |
| §5.1 | field defaults (incl. generated keys) | **DB-run** for namespace calls (language `uuid()`/`now()` remain legal per §8.8 — they are language functions, not namespaces; app-generated calls move into the mutation body — flagged below) |
| §14.1–14.5 | bucket `$from`/`$until`/`$repeat`, `$order`, source-backed `$source` views | **DB-run** |
| §15.1–15.4 | meter `$sources` pool views, `$quantity`, spend `$amount`/`$time`/`$eligible` | **DB-run** (built-in-only; allocation is engine machinery) |
| §18.4 | blob placement `$in`/`$serve` store views | **DB-run** |
| §20.1/§20.2 | migration field transforms `$as`/`$back` | **DB-run** (an app codec conversion moves into the delta's `$up`/`$down` program) |
| §9.1/§13 | seed/bundle data, module `$config` | **DB-run** (data/expressions outside any mutation body; seeds are values) |
| §8, §8.10, §8.11 | **the mutation program body**: statements, their embedded values and selectors, patches, local bindings, `assert`, internal calls, `return` | **framework** — THE app-procedure context (all §16.3 effect classes: pure, verifier, generated) |
| §11.5/§11.6 | login/SSO **auth mutations** (webauthn/oidc/password/api-key verification, session open, `cose.sign` token mint) | **framework** (they ARE mutation bodies — the §11.5 pattern is the re-model target) |
| §17.8/§17 | keyring operations in mutation statements; rotation transitions (system commits) | **framework** (mutation-class transitions) |
| §20.1/§20.2 | migration `$up`/`$down` delta programs | **framework** (programs of mutation statements, one atomic transaction; §20.1's own pure+deterministic constraint still applies, so app *pure* codecs are legal here and only here) |
| §21.1 | `$on_delete` delete patches (`$target`) | **framework** (mutation programs) |

Positions with no expressions (§5.7 `$unique` field lists, §12 wire ops, §19
history) classify vacuously. Engine-state *values* readable inside DB-run
expressions (§17.2 keyring metadata, §15.6 meter accessors, §18.5 placement
members) are not namespace calls and are untouched by the rule: candidate-free
uses hoist into `env`, and the candidate-addressed accessor keeps its
scope-deferred disposition below.

**The rule for an artifact read in BOTH contexts.** Classification restricts
the *artifact*; it never dictates the *executor*. A DB-run-classified
expression must satisfy the built-in-only restriction because SOME evaluation
may run it in the database — but the framework remains free to evaluate it
too (MemoryStore, fallback routes, §10.5 admission re-walks, §10.3 membership
checks, and — the v1 executor choice — `$check`/`$normalize`/`$verify` at
admission). So "read in both a view and a check" collapses to: the artifact
carries the union of its contexts' restrictions — which is exactly the DB
restriction, since framework-run adds none — and each evaluation chooses its
executor freely. The converse never arises: the one framework-only artifact,
the mutation body, is never handed to the extension, so its app calls never
need a DB story.

**The computed-value question — subsumed by the uniform rule.** The v6 draft
deliberated whether computed values (§5.2) should be built-in-only globally
or only when transitively projectable in a view; the corrected mandate
dissolves the question: computed values are a non-mutation-body position like
every other, hence built-in-only globally — declaration-local, one-span
diagnostic, no transitive contains-app-call analysis, no
action-at-a-distance (a pass-through `$view` never trips over a distant
computed declaration, because that declaration was already rejected on its
own). The deliberation's conclusion is retained because it independently
reached the same rule and supplies the escape the diagnostic suggests:
compute the app-fn value in a mutation body into a stored field — also the
right modeling for expensive host crypto (store once, don't recompute per
read).

**The §11 auth re-model (custom verification becomes an auth MUTATION).**
`$verify` is now built-in-only, so it covers exactly what is native and
deterministic: token verification through the built-ins and §17.7 keyring
acceptance (`cose.verify(/session_keys, $credential)` — runtime-dispatched
against accepted key versions, no provider call, no app namespace). Custom
credential verification — webauthn, OIDC, password hashing, api-key
exchange: precisely the §16.3 *verifier* namespaces — can no longer sit in
`$verify`; it re-models onto the pattern SPEC.md §11.5 already canonizes:
a public **auth mutation** invokes the verifier namespace inside its atomic
program (`identity = webauthn.verify(@response)`), maps the verified
identity to application login/account state, records the session row, mints
a native token (`cose.sign`, §17.8), and returns it; every subsequent
request authenticates through an authenticator whose `$verify` is the
built-in/native token check. The primary SPEC flow (§11.5 `passkey_login`,
§11.6 `oidc_callback`) already IS this shape — what the re-model removes is
only the *per-request* app-verifier authenticator (§11.3's `api_key`
example, `$verify: "api_keys.verify($credential)"`): such a credential is
either exchanged once for a native token via an auth mutation, or verified
natively (store `sha.sha256` of the key in state and compare in `$verify` —
built-ins suffice). App verifier namespaces are **KEPT**; only their
position narrows to the mutation body, where the transaction gives their
result the §8.12 recording and replay-fixing semantics — which a
per-request `$verify` (re-evaluated at every admission, never recorded)
could not.

**Admission under the mutation-only rule (`$check`/`$normalize` mechanism,
honest).** The mutation body (framework; may call app procedures) *proposes*
the transition into the staged overlay; `$normalize` then `$check`
(built-in-only) *validate* the prospective state (§8.8 order). Are they now
evaluated in-PG? Two readings, resolved: **classification restricts the
expression, not the executor** — being DB-run-classified means the database
CAN evaluate them, not that it must. v1 keeps the existing admission
mechanism unchanged: the interpreter evaluates `$normalize`/`$check`/
defaults inside the admission transaction over the staged in-memory overlay
(§5.4 case 3) — the same linked code, the same results, and consistent with
mandate 2 (the prospective overlay is session-relative working state, not
persisted state), so **Phases 0–6's in-memory-staging model is untouched**.
What the rule *unlocks*, by construction, is the in-transaction PG admission
seam that was previously impossible: because no `$check`/`$normalize` can
contain an app call, the writer connection could stage the proposed rows
inside its open transaction and evaluate validation through the §7.4 faces
over in-txn `nodes` state, rolling back on rejection — validation next to
the data, one round trip. That seam is recorded, NOT built: it inverts §5.4
case 3 and re-opens the §6 write path, a material re-architecture. **Flag
for the maintainer**: if "should also be in db" intends *mandated in-PG
execution* of `$normalize`/`$check` (not just the built-in-only
restriction), the admission model of Phases 0–6 changes materially
(staged-overlay evaluation → stage-into-PG-transaction evaluation) and that
is a separate, phased decision this document does not take unilaterally.
`$verify` execution likewise stays framework in v1 (it reads keyring engine
state plus the request credential — engine values, not `nodes` rows).

**No hoisting exemption (decided by the mandate's wording).** An
app-registered call in a DB-run expression is rejected even when it is
candidate-free and would hoist ("an app-registered-namespace call in ANY of
these positions is a load-time error"). Deliberately so: hoistability is a
pushdown implementation property, not a language property — tying legality to
"whether this subtree happens to be candidate-free under the current
compiler" would leak §7.3's hoist machinery into the language and make
programs break or un-break as the lowering evolves. The uniform rule keeps
hoisting a pure optimization (§7.3) and the language rule storage-agnostic.
Consequence, stated honestly: a candidate-free scalar view such as
`{ $view: "util.double(.n)" }` — legal today, exercised by the corpus — is
rejected under mandate 7 even though the engine would evaluate it in Rust;
the §9 corpus note lists the cases this flips.

**The load-time check (the checker design — `liasse-expr`).** The enforcement
extends the §16.3 effect-class machinery that already rejects a generated
function in a view at load (`check/views.rs::check_host_call`, corpus-pinned
by `generated-function-in-view-rejected.hjson`) — the identical mechanism,
one more axis:

- **`HostOrigin` on `HostOp`** (`host.rs`): `Core | Registered`. The
  runtime's `HostBinding::expr_signatures()` (liasse-runtime `host.rs`) tags
  every `$requires`-resolved descriptor `Registered`; the §6.5 namespaces the
  checker resolves itself (`BuiltinFn`, `core_string_fn`, and the core
  surfaces as they land) are `Core`, as is the §20 codec binding
  (`HostBinding::codecs()` — hex/base64/string-bytes ARE core). The partition
  is total and unambiguous by §16.2's own rule: a `$requires` local key MUST
  be distinct from every core namespace name.
- **`HostPosition` collapses to the mandate-7 shape** (`host.rs`): the
  current `Pure`/`Write`/`Admission` triple becomes

  ```rust
  enum HostPosition {
      /// Database-evaluated position (everything but a mutation body):
      /// built-in namespaces only, pure effect only. The kind names the
      /// sub-position for the diagnostic.
      DbRead(DbReadPosition),
      /// A field default: still built-in-only for NAMESPACE calls, but the
      /// language generated functions (uuid(), now()) stay legal (§8.8) —
      /// they are typed as language calls and never reach check_host_call.
      Default,
      /// The mutation program body — the transaction. The only position
      /// where an app-registered namespace may be called, with any declared
      /// effect class (pure, verifier, generated — §16.3 as amended).
      Mutation,
  }
  // DbReadPosition: ViewFilter | ViewProjection | SortKey | Coverage |
  //   Computed | Check | Normalize | Verify | ActorSession | BucketBound |
  //   MeterSource | Placement | MigrationTransform | Receiver
  ```

  `permits(effect, origin)`: `DbRead(_)`/`Default` ⇒ `(Pure, Core)` only;
  `Mutation` ⇒ any effect, any origin. The old `Admission` variant (which
  admitted verifier effects in `$verify`) is DELETED — verifier functions
  now run in `Mutation` position, and `$verify` is `DbRead(Verify)` (native
  cose verification is runtime-dispatched, not a `HostOp` call, so it is
  untouched by `check_host_call`).
- **Position wiring** (liasse-model `check.rs`/`scope.rs`, liasse-runtime
  `compiled.rs`/`recursion.rs`): the same `with_host_position` mechanism that
  today selects `Write` for defaults and mutation values now selects
  `Mutation` exactly where a §8 program body is checked (mutation programs,
  §11.5 auth mutations, §20.1 delta programs, §21.1 delete patches),
  `Default` for field defaults, and `DbRead(kind)` everywhere else — view
  sources/filters/projections/sorts (`check_tree`, view compilation),
  coverage predicates (`compile_recursive`), computed values, **`$check`,
  `$normalize`, and the §11.3 authenticator expressions** (today all
  `Pure`/`Admission`), bucket bounds, meter expressions, placement views,
  migration `$as`/`$back`.
- **The check, in `check_host_call`, AFTER the effect check** (order matters:
  a generated fn in a view keeps its §16.3 effect diagnostic — the stronger,
  position-class-wide violation — and the origin check fires only for
  otherwise-admissible pure app calls), and before arity/argument typing:

  ```text
  error: app-registered function `pki.check` cannot be called outside a
         mutation program
    --> package.hjson:41:23 (the $view filter of surface `documents`)
     |
  41 |   "$view": ".docs[:d | pki.check(d.sig, d.payload)] { id }"
     |                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^
     |            `pki` is an app-registered namespace (§16.2 `$requires`)
     |
     = note: every position outside a mutation body — a view's filter,
             projection, or `$sort`, a `$recursive` `$where`/`$except`, a
             computed value, a `$check`, a `$normalize`, an auth `$verify`,
             a bucket bound — is database-evaluated, and only the built-in
             namespaces exist there (§6.5: hex, base64, sha, string,
             convert, time)
     = help: an application procedure runs inside a mutation — the
             transaction: compute the value in a mutation statement and
             store it, then read the stored field here; for credential
             verification, use an auth mutation that mints a native token
             (§11.5)
  ```

  No runtime fallback exists for this case: the rejection is a load error,
  so `RowPrograms` — and every `$check`/`$normalize`/`$verify` program — can
  never contain an app-registered call and the extension never sees one:
  the v5 "every function a program can call exists inside PostgreSQL"
  property, obtained by construction.
- **Prototype disposition**: not separately prototyped — the seam is the
  shipped effect-class rejection at the same call site, already exercised
  end-to-end by the static corpus (`generated-function-in-view-rejected`,
  `verifier-function-in-view-rejected`); the new check is one enum variant,
  one field, one conjunct, and position wiring over the existing
  `with_host_position` plumbing.

**The SPEC.md amendments this design depends on (replace v5's §16-removal
note).** §16's registration machinery stays as written; one normative rule is
ADDED and three passages are AMENDED to match. Draft normative text:

> **§16.5 Execution contexts (new).** A call to a `$requires`-registered
> namespace (§16.2) is legal only within a mutation program — the atomic
> sequential program of §8, including auth mutations (§11.5), delete patches
> (§21.1), and migration delta programs (§20.1). Every other expression
> position is *database-evaluated*: a `$view`'s source selection, filter,
> projection, and `$sort` (§7), a `$recursive` `$where`/`$except` (§10.5), a
> computed value (§5.2), a field default (§5.1), `$normalize` and every
> `$check` (§8.8, §5.10), the authenticator expressions `$verify`, `$actor`,
> `$session`, and `$check` (§11.3), bucket, meter, and placement expressions
> (§14, §15, §18.4), and migration field transforms (§20.1). A
> database-evaluated expression is restricted to the language operators and
> the built-in namespaces (§6.5); a registered-namespace call there is a
> static (load-time) error. This is a language rule, not a storage choice:
> it guarantees a conforming engine can evaluate everything that reads or
> validates inside its storage engine, which hosts only the built-in
> surface, while application procedures run exactly where atomicity,
> result recording, and replay fixing are defined — the transaction (§8.12).

> **§11.3 (amended).** Replace "`$verify` MAY use a registered verifier
> namespace and performs no application-state mutation" with: "`$verify` is
> a database-evaluated expression (§16.5): it is restricted to the language
> operators, the built-in namespaces (§6.5), and native keyring verification
> (§17.7), and performs no application-state mutation. Application-defined
> credential verification — webauthn, OIDC, password hashing, API-key
> exchange — runs inside a mutation program (§11.5): the mutation invokes
> the registered verifier namespace, maps the verified identity to
> application login and account state, and constructs a native token
> (§17.8) that subsequent requests verify through `$verify`."

> **§16.3 (amended).** Replace "Pure functions MAY run during views, checks,
> and replay. Verifiers run during external request admission. Generated
> functions run in mutation/write-time positions." with: "A built-in pure
> function MAY run in any expression position. A registered namespace
> function of any effect class is callable only within a mutation program
> (§16.5): a verifier validates untrusted input inside the mutation that
> admits it; a generated function produces its recorded result inside the
> mutation that commits it."

> **§8.8 "Expression effects" (amended).** Replace with: "Computed fields,
> views, `$normalize`, and `$check` are database-evaluated (§16.5) and use
> built-in pure functions only. Defaults MAY additionally use the language
> generated functions (`uuid()`, `now()`). Mutation programs MAY use
> registered namespaces of every effect class. The checker rejects an
> effect class — or a registered namespace — used in the wrong position
> while loading the package."

(§7.1 and §10.5 each gain one informative cross-reference sentence to
§16.5.)

**First, the SOURCE classification — which reads push down at all.** The
per-row boundary below applies to the filter/projection/sort programs of a read
whose *source* is servable by the store. Lowerable sources:

- **a stored collection scan** — the view chain `Field{Root/Current-of-root,
  name}` resolving to a stored keyed collection (NOT a folded view cell),
  optionally wrapped in a `Select::Bind` filter and/or a `Project` block:
  `ViewSource::Collection`. This is SPEC.md's own canonical `$view` shape
  (`.projects[:p | …] { … }`), the overwhelmingly common case;
- **§10.5 coverage** — the `$recursive` descent: `ViewSource::Coverage`.

Everything else keeps its runtime treatment, each with a defined disposition
rather than a vague "unsupported":

- **a reference to another view** (`.other_view` as source): lowered iff the
  referenced view lowers — the lowering resolves the reference at compile time
  and pushes the *composition* when the result is still one collection chain;
  otherwise fallback. (Recursive view-through-view chains bottom out exactly as
  `view_order_of`'s fuel does.)
- **combinators** (`a | b`, `a & b`, `a - b`, `?:`, `??`): each operand that
  lowers is served by its own pushed query; the combinator itself (an
  identity-keyed merge honoring SPEC §7.4's order rules) runs in Rust over the
  two evaluated row streams. Designed as a seam, NOT built in v1 — v1 falls
  back for the whole combinator expression.
- **aggregates over a lowerable view** (`count`/`sum`/… SPEC §7.5): the source
  pushes (filter/projection in-PG); the fold runs in Rust over the returned
  stream. (Pushing the fold itself into SQL aggregation over `liasse.eval`
  results is a bench-driven seam — correctness is identical, only the
  transferred row count differs.)
- **keyed selection** (`.coll[k]`): already a §4.1 point read; the projection
  program applies to the one row in Rust — no pushdown needed to avoid
  hydration.
- **temporal/bucketed sources** (§14): a bucket's activity test is itself a
  row-level expression over the row's fields at the request clock — it is
  *composable into the pushed filter* (hoist `now`, conjoin the compiled
  `$from`/`$until` interval test). Designed as a seam; v1 falls back for
  bucketed sources.
- **engine-state sources** — keyring version views (§17.2), meter accessors
  (§15.6), module aggregation (§13.9), source-backed buckets (§14.4), blob
  placement members (§18.5): their rows/values are engine-derived, not stored
  `nodes` rows; as *sources* they fall back, and as *subexpressions inside a
  pushed program* they are candidate-free and hoist into `env` like any other
  engine value. The one candidate-DEPENDENT member of this class — an
  engine-state accessor addressed *by* the candidate (a meter accessor on the
  candidate row) — is neither hoistable (candidate-dependent) nor served by
  the §7.6 subtree prefetch (its value is engine-derived, not a stored subtree
  row); it takes this class's disposition: per-view fallback + report, with
  the same lateral-prefetch mechanism (joining the engine's durable state per
  candidate) as its designed upgrade seam. This is a *scope* line, not an
  expressiveness one — and the only candidate-dependent form v1 defers at all.

**In-PG (the full core language plus every built-in namespace, over the
candidate's row AND subtree):**
`Literal`, the candidate itself (`Current`, the `$bind`/filter binding),
synthetic hoisted bindings, `Field` chains resolving to the candidate's stored
scalar members, nested *static-struct* members, carried computed members, **or
the candidate's nested keyed collections and sets** (served from the §7.6
prefetched subtree — `size(child.subcompanies)`, an aggregate or filtered
selection over the candidate's subtree at any depth of the read-set);
`Key` (the candidate's `$key`, from `key_wire`); `Compare` (full Annex-B
`Value::cmp` — all types, canonical decimal equality, `none` ranking,
NUL-bearing text — because it *is* `Value::cmp`); `Logic`/`Not` (interpreter
short-circuit and strict truthiness); `In` (hoisted set/collection haystacks);
`Ternary`; `Arith`/`Neg` (including error paths — a fault maps per §7.4);
`Builtin` — **the complete host surface a checked DB-run program can contain**
(mandate 7): `size`/`has` and the whole
`string`/`time`/`convert`/`hex`/`base64`/`sha` namespaces (SPEC.md §6.5/§16.1),
deterministic and linked into the extension with the interpreter,
so a candidate-dependent call (`sha.sha256(child.doc)`,
`time.add(child.created, @grace)`) evaluates in-PG like any operator — and an
app-registered call is not in this list because it cannot reach a DB-run
program at all (the load rule above);
`Struct`/`List`/`Composite` literals (composite-key operands, struct outputs);
`Select`/`Traverse`/`Aggregate`/`Project` whose base is a *hoisted* collection
cell (e.g. `/accounts[.owner]` — the candidate-dependent selection runs in-PG
over the hoisted `/accounts` haystack, like `In` does; weigh the env size,
below) **or a candidate-subtree collection** (base served from the prefetch).
Computed fields the program reads are carried in the descriptor with
their own hoisted expressions and audited recursively; their fold reproduces
`fold_computed` (fault ⇒ `none`, non-scalar ⇒ skip) via the shared
implementation, and a carried computed that itself reads through nested
collections extends the read-set transitively (§7.3) — the fold then runs
over rows already present in the prefetched subtree, so evaluation never needs
data the arguments do not carry. Projection outputs may reference earlier
outputs (§7.1 dependency order) — the lowering inlines them in the checker's
own output order, which is already cycle-free.

**Hoisted (candidate-free, evaluated once in Rust, shipped in `env`):** literals
aside — session structurals (`$actor`/`$session`), surface `@params`, `#imports`,
`/`-reads of any collection, `now()`/`uuid()`, aggregates/views/traversals over
*other* state, keyring/meter/placement selectors, and any built-in call whose
arguments are candidate-free (an optimization — one evaluation instead of one
per row — not a necessity, since the same call would evaluate in-PG).
*Env-size caveat, honest*: hoisting a `/`-collection
(an `in /admins` haystack, a `/accounts[.owner]` deref base) ships that
collection's cells in `env` per read — bounded by the referenced collection,
not the source, but a large deref target makes the env blob large; the §9
bench axis measures env-heavy programs, and a per-view fallback remains
available if a hoisted haystack is pathological. (A future alternative — the
extension resolving the deref by its own indexed point-read via SPI — is the
same SPI seam as below, not designed in.)

**The excluded set is empty for a CHECKED program (mandates 6+7).** v4 carved
out two candidate-dependent classes the extension could not serve; both are
gone — one by a load-time language rule, one by the prefetch:

1. **App-registered host calls — excluded at LOAD, kept in the language
   (mandate 7, superseding v5's removal).** §16 app-registered host
   namespaces, `$requires`, the `Registry`, descriptors, §11 verifier
   namespaces, §17 cose, and §20 codecs are all **KEPT** — they are how an
   application does webauthn/oidc/password verification, token signing, and
   migration codecs, crypto the runtime cannot perform natively — but their
   one legal position is the mutation program body (the transaction). What
   guarantees the extension never meets one is the mandate-7 rule above: ANY
   position outside a mutation body (view filter/projection/sort, coverage
   predicate, computed value, `$check`, `$normalize`, `$verify`, default,
   bucket bound, meter/placement expression, migration `$as`/`$back`) that
   calls an app-registered namespace is a **load-time error** in the
   checker, so every program that reaches lowering — and every admission
   validation program — is built-in-only *by construction*. The complete host surface of a checked
   DB-run program is the fixed built-in set — `string`, `time`, `convert`,
   `hex`, `base64`, `sha`, plus the language operators (SPEC.md §6.5/§16.1) —
   deterministic, implemented in-crate, and linked into runtime, MemoryStore
   path, and extension alike, so the in-database evaluator has **every
   function it can ever encounter**. No app-fn runtime fallback exists, and
   none is needed: the case is statically impossible, not dynamically
   routed. v5's §16-removal workstream (registration-API deletion, SPEC.md
   §16 rewrite) is **cancelled**; the SPEC edit is the additive §16.5 rule
   drafted above.
2. **Reads through the candidate's own subtree — now served by the prefetch.**
   A program reading the candidate's nested keyed collections
   (`size(child.subcompanies)`, aggregates/filters over the candidate's
   subtree) is served by the **candidate-subtree prefetch** (§7.6, option (a)):
   the compiler extracts the read-set, the statement materializes each
   candidate's live subtree through an index-served recursive lateral, and the
   rows arrive at the evaluator as the `subtree` argument (§7.4) — the faces
   stay `IMMUTABLE`, the plan stays index-served and EXPLAIN-gateable, and
   parity holds by construction (MemoryStore builds the same
   `CandidateSubtree` from its `BTreeMap`; same faces, same interpreter). A
   data cycle or corrupt over-deep chain trips the shared depth guard as a
   loud error — never a hang, never a silent truncation (§7.6). The v4 SPI
   escape is superseded as the default and remains a documented seam only
   (§7.6 records why it lost).

**Policy — uniform at lowering time, because the checker already did the
rejecting.** The v3/v4 split — load-time rejection for coverage predicates,
automatic fallback for general views — existed because a coverage predicate
could contain a non-pushable *expression*, and an unpushed coverage read is
the mandate-3 perf killer. Under mandates 6+7 no such expression survives to
lowering: an app-registered call in a DB-run position died in the CHECKER
(the mandate-7 language rule — a rejection with a rustc-like diagnostic, but
a §16.5 language error over every DB-run position uniformly, not v4's
engine-policy carve-out for coverage with a per-surface opt-in escape; that
v4 policy and its SPEC.md §10.5 restriction line stay withdrawn), and every
checked predicate and program over the candidate's row, key, subtree, and
hoisted environment pushes. Coverage predicates and general `$view` programs
are governed by ONE lowering policy, and **no app-fn fallback exists** — the
interpreter fallback list below contains no expression-restriction entry.
What still routes to the interpreter, exhaustively:

- **Dynamic, correctness routes (unchanged, both automatic):** non-head
  frontiers (§19.2 replay / stale-client resume) — the `nodes` table holds
  head state only, so historical reads fold the log and evaluate in Rust; and
  hoist-eval errors / `StoreError::Eval` faults — the interpreter fallback
  reproduces the exact per-candidate error surface (§7.3/§7.4). Depth-guard
  trips are NOT a fallback route: corrupt data is a loud `StoreError` on both
  paths, never silently re-served.
- **Scope-deferred forms (v1 scope choices with designed upgrade seams, not
  restrictions):** view combinators and non-composing view-references (whole
  expression falls back), bucketed sources, engine-state sources including the
  candidate-addressed accessor case (disposition above), `json`-typed sort
  keys (`sort_enc` gap, §7.4), and the aggregate *fold* (source pushes, fold
  in Rust — not a fallback of the read itself). Each keeps **automatic
  per-view interpreter fallback, never silent**: load emits the **pushdown
  report** (per view: pushed, or the typed span-carrying reason), queryable
  through engine introspection, and the engine-configuration **strict mode**
  promotes any reported residual to a load error for deployments that want
  the everything-pushes guarantee. Mandate-3 exposure under the uniform
  policy is exactly "a scope-deferred form inside a coverage predicate" —
  practically only the candidate-addressed engine accessor — which the report
  names and strict mode refuses; the calculus that justified load-rejection
  for a whole *class* of ordinary predicates no longer applies when the class
  is one exotic accessor with a designed upgrade seam.

**Does ANY restriction remain? Exactly one — and it is a language rule, not
an engine policy.** Every expression outside a mutation body is built-in-only
(mandate 7, the §16.5 draft above), checked at load with a precise
diagnostic; the mutation body — the transaction — keeps the full
app-registered surface, every effect class. Within that rule, every
expression over the candidate's own data — the spec's §10.5 examples, every
ACL/tenancy/status/plan predicate, computed fields, arithmetic, the full
built-in namespaces, `in`-sets, nested structs, `$key`, and reads through the
candidate's nested collections at any depth — runs in-PG. v2's fragment
table, v3/v4's engine-level excluded classes, and the v4 coverage rejection
policy are all gone; what is deferred in v1 is scope (the bullet above),
reported and strict-promotable, never a silent route. The SPEC.md edits this
design depends on are the **§16.5 execution-contexts rule plus the
§11.3/§16.3/§8.8 amendments** (drafted above; §16's registration machinery
stays as written — the v5 removal note is void).

### 7.6 The SQL shapes: flat view, coverage CTE, and the shape-directed descent rule

**The flat `$view` statement** (`ViewSource::Collection`), for admit wire `$A`
(absent ⇒ conjunct dropped), projection wire `$P`, sort wire `$S` (absent ⇒
`ORDER BY c.key_enc` — source key order), env wire `$E`, over the §4.2 resolved
parent chain. `$T` is the subtree argument: the literal `'[]'::jsonb` for a
shallow program (no lateral is planned at all), or `st.subtree` from the
prefetch lateral below when the program's read-set is non-empty:

```sql
SELECT c.key_wire, c.incarnation,
       liasse.eval($P, c.value, c.key_wire, $E, $T)  AS projected,
       liasse.eval($S, c.value, c.key_wire, $E, $T)  AS sort_tuple  -- only when sorted
FROM {s}.nodes c
  -- CROSS JOIN LATERAL (…subtree prefetch, below…) st   -- only when read-set ≠ ∅
WHERE c.parent_id = (…chained InitPlan, §4.1…)
  AND c.step_name = $name AND c.value IS NOT NULL
  AND liasse.eval_bool($A, c.value, c.key_wire, $E, $T) IS TRUE
ORDER BY liasse.eval_sort($S, c.value, c.key_wire, $E, $T), c.key_enc
OFFSET $skip LIMIT $limit
```

- The filter runs in the scan's **Filter line** (never a scan source): rows the
  admit program excludes are dropped at the heap, before projection or sort
  evaluation — SQL's clause order does what the interpreter's
  filter-then-project does.
- The **`ORDER BY` is the evaluated `$sort`** in `sort_enc` order (§7.4), with
  `c.key_enc` as the trailing occurrence tiebreak — Annex-B/§7.3 order by
  construction of the encoding. `$skip`/`$limit` become `OFFSET`/`LIMIT`, and
  with a `LIMIT` the sort is a bounded top-N heapsort (prototyped, §7.8) — the
  first time a `$limit` view's cost stops being O(collection) transfer.
- The `sort_tuple` column carries the decodable evaluated tuple for §12's
  window gap coordinate (§8); it is selected only when a subscription needs it
  (a plain `view` read skips the column). This is the one place a sort
  expression evaluates twice per row (`eval_sort` + `eval`); the per-backend
  cache amortizes the decode, and the composite `eval_row` seam (§7.4) exists
  if benches flag it.
- **Plan shape** (prototyped, §7.8): an index-served scan of the collection's
  `(parent_id, step_name)` range — `Index Scan` or `Bitmap Index Scan +
  Bitmap Heap Scan` `using node_key_lookup`, the planner's call for wide
  ranges; both are index-served, and the gate accepts both — with `eval_bool`
  **only in the Filter/Recheck line**, `eval_sort` **only in the Sort Key**, a
  `Sort` (or top-N) node above, and **no Seq Scan anywhere**.

**The coverage CTE** (`ViewSource::Coverage`), for the composed admit wire `$A`
(= `$where && !$except`, §7.3; absent ⇒ conjunct dropped — default include),
projection wire `$P`, env wire `$E`, subtree argument `$T` as above (the
recursive term gains the same prefetch lateral when the admit reads the
candidate's subtree — prototyped legal and index-served, §7.8). The recursive
term also carries the depth guard — every recursive descent does (below):

```sql
WITH RECURSIVE cover AS (
    SELECT n.id, jsonb_build_array() AS key_path,
           ARRAY[]::bytea[] AS sort_path, 0 AS depth, n.incarnation, n.value
    FROM {s}.nodes n
    WHERE n.parent_id = (…chained InitPlan, §4.1…)
      AND n.step_name = $root_step AND n.key_enc = $root_key
      AND n.value IS NOT NULL                       -- root: live check ONLY, no predicate
  UNION ALL
    SELECT c.id, p.key_path || jsonb_build_array(c.key_wire),
           p.sort_path || c.key_enc, p.depth + 1, c.incarnation, c.value
    FROM cover p
    JOIN {s}.nodes c ON c.parent_id = p.id AND c.step_name = $field
    -- CROSS JOIN LATERAL (…subtree prefetch, below…) st   -- only when read-set ≠ ∅
    WHERE c.value IS NOT NULL                       -- tombstone blocks the branch (§7.2)
      AND liasse.guard_depth(p.depth + 1, $max_depth)
      AND liasse.eval_bool($A, c.value, c.key_wire, $E, $T) IS TRUE
)
SELECT key_path, incarnation, value,
       liasse.eval($P, value, (…last key_path element…), $E, $T) AS projected
FROM cover ORDER BY sort_path
```

- The `IS TRUE` wrapper collapses a STRICT-NULL (only reachable if the planner
  evaluates the call before the `value IS NOT NULL` barrier) to *excluded*,
  never admitted.
- **Pruning during descent**: a candidate failing the admit never enters the
  worktable, so its subtree is never joined, fetched, or decoded — the
  recursion itself is the pruning, identical to v3 (§7.8's 139-of-781 plan).
  The **projection runs in the outer SELECT** — evaluated once per *included*
  row, after pruning, never on a pruned candidate.
- **Plan shape**: anchor = the §4.1 chained-InitPlan `Index Scan using
  node_key_lookup`; recursive term = `Nested Loop` of `WorkTable Scan` +
  `Index Scan using node_key_lookup` with the extension calls appearing **only
  in the Filter line** — a per-worktable-row function call, never a scan
  source, so it cannot introduce a Seq Scan. This is EXPLAIN gate (11): anchor
  + recursive term index-served, no Seq Scan anywhere, worktable row count =
  included count (the pruning proof).
- Ordering: `sort_path` (arrays of memcmp-ordered `key_enc`) yields depth-first
  Annex-B order — §10.5's keyed-tree order; coverage has no `$sort`/bounds.
  `key_path` decodes to the per-level `KeyValue` rel path via the shared codec,
  and the runtime nests the projected rows into the §10.5 keyed tree by path.
- The recursion depth guard (`liasse.guard_depth`, below) is shared with
  `scan_subtree` and the prefetch lateral; a cycle in corrupt data is reported
  as corruption, not an infinite descent.

**The candidate-subtree prefetch (mandate 6b, option (a)).** When a program's
read-set (`subtree_steps`, §7.3) is non-empty, the statement — flat or the
coverage CTE's recursive term — gains one lateral per candidate row:

```sql
CROSS JOIN LATERAL (
    WITH RECURSIVE sub AS (
        SELECT s.id,
               jsonb_build_array(jsonb_build_array(s.step_name, s.key_wire)) AS path,
               s.value, 1 AS depth
        FROM {s}.nodes s
        WHERE s.parent_id = c.id AND s.step_name = ANY($steps)
          AND s.value IS NOT NULL              -- LIVE rows: tombstone blocks (§7.2)
      UNION ALL
        SELECT t.id,
               sub.path || jsonb_build_array(jsonb_build_array(t.step_name, t.key_wire)),
               t.value, sub.depth + 1
        FROM sub JOIN {s}.nodes t
          ON t.parent_id = sub.id AND t.step_name = ANY($steps)
        WHERE t.value IS NOT NULL
          AND liasse.guard_depth(sub.depth + 1, $max_depth)
    )
    SELECT COALESCE(jsonb_agg(jsonb_build_array(sub.path, sub.value)),
                    '[]'::jsonb) AS subtree
    FROM sub
) st
```

`st.subtree` is the faces' fifth argument (§7.4). Facts that make this the
right mechanism, in order of weight:

- **The evaluator stays `IMMUTABLE` and the plan stays gateable.** The subtree
  arrives as an argument, so the faces remain pure functions of their inputs,
  and every per-candidate probe is IN the plan — anchor and recursive term of
  the lateral are `Index Scan using node_key_lookup` with the `= ANY` inside
  the Index Cond (shape-directed rule below; prototyped, §7.8) — so the
  EXPLAIN gate can pin index service deterministically, which AGENTS.md's
  performance-as-correctness discipline requires.
- **Live rows only, deliberately.** The interpreter's candidate tree is the
  *materialized* subtree (`materialize_row_cell`): a tombstone blocks its
  branch. The prefetch reproduces exactly that (`value IS NOT NULL` as a
  traversal barrier) — it shares `scan_subtree`'s shape-directed machinery but
  NOT its traverse-through-tombstones hydration semantics; the two tombstone
  rules stay deliberately distinct (§7.2).
- **The read-set is always statically extractable** (§7.3): the checker types
  every candidate-relative path, and a recursive shape (§5.8) yields a finite
  step universe whose unbounded *depth* the recursive lateral serves under the
  guard. There is no "read-set unknown at compile time" case in the typed
  language, so option (b) below is never *needed* — it stays a documented
  seam, not a fallback in use.
- **No predicate-in-predicate regress.** The prefetch is semantics-free: it
  applies no program to subtree rows (matching the interpreter, whose
  candidate tree is raw live rows plus the computed fold). A carried computed
  on a subtree row folds over rows already inside the prefetched tree
  (transitive read-set, §7.3). So "candidate reads its subtree whose rows
  read THEIR subtrees" is evaluation over one finite prefetched tree — the
  only recursion is the data descent itself, and the guard bounds it.
- **Cost, honest.** The lateral runs once per candidate row that survives the
  cheap conjuncts (Index Cond + `IS NOT NULL`) — including candidates the
  admit then rejects, because the predicate needs the subtree to reject them;
  the interpreter path pays the same materialization per candidate, so this is
  the floor relocated, not new work. In the §7.8 prototype the per-invocation
  cost of the lateral's recursive subplan (~1.4–1.7 ms on the demo tree,
  dominated by rescan setup + per-row guard/agg calls, not probe count) is
  visible and is a §9 bench axis; hereditary pruning still bounds coverage
  (a rejected candidate's subtree is prefetched once for its own verdict but
  never descended for coverage). When only the admit reads the subtree, the
  projection passes `'[]'`; a subtree-reading projection carries `st.subtree`
  as a CTE column to the outer `SELECT` instead of re-running the lateral.

**Option (b), evaluated and not chosen: the evaluator reads via SPI.** The
faces would query `nodes` for the candidate's children on demand (pgrx SPI),
becoming `STABLE` (`IMMUTABLE` would be a lie for a table-reading function).
Honest accounting: at these call sites volatility itself changes little —
every call already takes per-row arguments, so constant-folding was never in
play. What actually loses the comparison: (i) the per-candidate reads
*disappear from the plan* — a Filter line hiding N invisible probes that the
EXPLAIN gate can no longer pin as index-served, breaking the deterministic
plan-gate discipline; (ii) per-row SPI re-enters the executor with its own
prepare/execute overhead, unamortizable and invisible to the planner's cost
model; (iii) `PARALLEL SAFE` must drop (SPI in parallel workers is
restricted); (iv) the extension would gain a query surface into the instance
schema (search-path and schema-qualification obligations inside the `.so`),
where today it touches no table at all. Compilation would be marginally
simpler (no lateral assembly), and that is the whole upside. **Recommendation:
option (a) everywhere** — the read-set is always static (above); (b) remains
the recorded seam if a future language form ever produces a truly unbounded
read-set.

**The cycle/depth guard — every recursive descent, both stores.**
`liasse.guard_depth(depth, max)` (§7.4) rides in the recursive term of the
coverage CTE, the `scan_subtree` CTE, and the prefetch lateral: `true` below
the shared bound (`MAX_SUBTREE_DEPTH`, one constant in `liasse-store`, sized
beyond any sane tree — e.g. 1024 — and shared by both stores), a
reserved-SQLSTATE (`LQ002`) error past it, which `PgStore` maps to the same
corruption-classed `StoreError` the MemoryStore/interpreter descent returns
from its own counter. An actual data cycle therefore BAILS loudly — a
detected error, never a hang and never a silently truncated result — and a
legitimately over-deep tree is refused identically rather than half-served.
Structural note, stated so the guard is not mistaken for the only defence:
under the single-parent adjacency schema a parent-cycle is necessarily a
*detached ring* (every ring member's parent is a ring member), which no
address-resolved descent can enter — prototyped: a planted ring left the
sound read's 780-row result untouched. The guard's live targets are corrupt
over-deep chains, descents anchored on corrupt roots (the prototyped
forced-start-in-ring case errors at the bound), and schema-invariant
violations we have not imagined — defence in depth over an invariant that
already holds by construction.

**The shape-directed descent rule (binding for EVERY recursive descent).** A
recursive term that joins children by `parent_id = p.id` **alone** does not use
`node_key_lookup` — `parent_id` without `step_name` is not a usable prefix
selective enough for the planner, which chooses **Seq Scan + Hash Join**
(prototyped, §7.8; independently hit by the Phase-5 `scan_subtree` work). Every
descent — the coverage CTE, `scan_subtree`, any future subtree read — MUST name
the step(s) it descends: `c.parent_id = p.id AND c.step_name = $field` for the
single-relation coverage descent, and `c.step_name = ANY($steps)` for a
multi-collection subtree walk, where `$steps` is the set of nested-collection
names declared anywhere in the subtree's compiled shape (the §3 `steps`
parameter; a stored child row's step name is always a declared one, so the walk
is complete). PostgreSQL keeps `= ANY` inside the **Index Cond** — one probe
per (parent, step) — so the plan stays index-served (prototyped, §7.8).
PostgreSQL permits exactly one self-reference in a recursive term, so the
K-collection descent is one term with a K-element array, not K terms. A
multi-collection walk's `sort_path` accumulates `(step_name, key_enc)` PAIRS
(not bare `key_enc`) so the final `ORDER BY` yields Annex-B **address** order —
sibling collections order by name segment before key — where the
single-relation coverage CTE can keep bare `key_enc` (one step name per level). Two
consequences, stated: (a) `scan_subtree`'s step universe comes from the CURRENT
compiled shape — a migration walking rows of a *previous* model must derive its
universe from that model's shape (or the physical `SELECT DISTINCT step_name`,
one indexed statement), not the new one; (b) a truly shape-free all-children
descent, if one is ever needed, is servable by adding a dedicated
`nodes(parent_id)` btree — prototyped: with that index the planner DOES switch
to a parameterized index nested-loop — but no current read needs it, and the
index is NOT added (it would be dead weight the reconciler must justify;
revisit only with a concrete consumer).

### 7.7 Extension presence and the version lock

Two invariants, both enforced at open, both failing loud:

1. **Presence.** `reconcile` (§12.3) runs `CREATE EXTENSION IF NOT EXISTS liasse`
   before the DDL step. In the shipped image the extension is already created at
   initdb time (in `template1`, so every database inherits it), making this a
   no-op that needs no superuser; on a database where it is genuinely absent and
   the role cannot create it, reconcile refuses with an actionable message
   ("this deployment requires the liasse PostgreSQL image — see
   crates/liasse-pg-ext — or a manually installed matching extension"). If a
   *newer* packaged version is available (image upgraded under an existing
   database), reconcile runs `ALTER EXTENSION liasse UPDATE` — the
   self-reconciling story extended to the extension.
2. **Version identity.** The runtime and the `.so` must be the same build of the
   evaluation semantics and wire. `liasse-pred` exports a single
   `EVAL_ABI: &str` — its crate version plus a wire-revision component — which
   the extension exposes as `liasse.abi_version()` and the store compares
   against its own linked constant on every open, refusing on mismatch
   ("extension ABI `X`, store ABI `Y`: deploy the matching image"). Discipline
   backing the constant: any change to the serialized types or their semantics
   bumps the wire revision (review-enforced, plus the §9 round-trip and parity
   gates that fail on drift in practice); CI builds the image from the same
   commit as the test binaries, so the lock holds by construction there, and the
   handshake catches operational skew (old image, new binary — or the reverse)
   at open rather than as corruption later.

The extension is **database-scoped and shared** by every instance schema: it is
never part of a per-instance declared set, `drop_instance` (a schema drop) does
not touch it, and the reconciler's orphan sweep — which inspects only the
instance schema — cannot see it, by construction. Uninstalling it is an
operational act, not reconciliation.

### 7.8 Prototype — the mechanics, proven end to end

The load-bearing v3 claims — a pgrx `#[pg_extern]` Rust function is callable
from the recursive term, prunes during descent, keeps the index-served plan, and
ships in a two-stage Docker image on stock `postgres:17` — were prototyped in
this workspace's sandbox (Docker 29.4, no local pgrx toolchain: the build runs
*inside* the image build, which is itself the design being validated).

- **Image**: stage 1 `rust:1-bookworm` + PGDG `postgresql-server-dev-17` +
  `cargo install cargo-pgrx` (resolved 0.19.1) + `cargo pgrx init --pg17`
  against the distro `pg_config`; a minimal extension crate whose
  `#[pg_extern(immutable, parallel_safe, strict)] fn liasse_eval_demo(pred:
  &[u8], row: JsonB) -> bool` evaluates a stand-in predicate
  (`{"field": f, "ne": s}`) over the real tagged wire form with the real
  truthiness rules (`none != text` ⇒ true, strict `Bool(true)`); `cargo pgrx
  package` emits `.so` + control + SQL. Stage 2 `postgres:17-bookworm` copies
  them into `pkglibdir`/`sharedir` and an initdb-time script runs
  `CREATE EXTENSION` in `template1` and the default DB. The image built clean
  (exit 0) and booted; `pg_extension` lists `liasse_demo 0.0.0` at first
  connection and `SELECT liasse_demo_abi()` answered `liasse-demo 0.0.0` — the
  §7.7 handshake mechanism works as designed.
- **Data**: the v4-shaped `nodes` DDL + `node_key_lookup`, a fanout-5 depth-4
  `companies`/`subcompanies` tree (781 stored subtree nodes, ~⅓ `closed`, with
  children stored *under* closed nodes so pruning has subtrees to skip) plus
  40 000 noise rows, `ANALYZE`d.
- **Result parity**: the §7.6-shaped CTE filtering via `liasse_eval_demo(…) IS
  TRUE` returned **139** included rows; the reference recursion with the
  predicate hand-written in native SQL returned **139**; the unpruned stored
  subtree is **781** — the extension path skipped 642 nodes' fetches during
  descent, verdict-identical to the native oracle.
- **Plan** (`EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF)`, verbatim minus the CTE
  Scan tail):

  ```
  Sort (actual rows=139 loops=1)
    Sort Key: cover.sort_path
    CTE cover
      ->  Recursive Union (actual rows=139 loops=1)
            ->  Index Scan using node_key_lookup on nodes n (actual rows=1 loops=1)
                  Index Cond: ((parent_id = 0) AND (step_name = 'companies'::text) AND (key_enc = '\x0001'::bytea))
                  Filter: (value IS NOT NULL)
            ->  Nested Loop (actual rows=28 loops=5)
                  ->  WorkTable Scan on cover p (actual rows=28 loops=5)
                  ->  Index Scan using node_key_lookup on nodes c (actual rows=1 loops=139)
                        Index Cond: ((parent_id = p.id) AND (step_name = 'subcompanies'::text))
                        Filter: ((value IS NOT NULL) AND (liasse_eval_demo(…, value) IS TRUE))
                        Rows Removed by Filter: 1
  Execution Time: 0.549 ms
  ```

  Anchor and recursive term are both `Index Scan using node_key_lookup`; **no
  Seq Scan anywhere**; the extension call appears only in the Filter line; the
  inner index scan runs `loops=139` — once per *included* row, never per stored
  row (781): hereditary pruning is visible in the plan itself, exactly as gate
  (11) will pin it.
**The v4 general-evaluator claims, prototyped in the same image** (extension
extended with stand-in `liasse_eval_demo_project` → jsonb and
`liasse_eval_demo_ord` → order-preserving bytea faces; `demo-eval.sql`, run
after `demo.sql`; the v3 numbers above re-reproduced first — 139/139/781,
index-served):

- **ONE statement serves a `$view`** over a 2 000-row `projects` collection
  (plus the 40 000 noise rows and the companies tree): filter
  (`status != 'closed'`), projection with a computed-style output
  (`margin = revenue - cost`, evaluated per row in the SELECT list, returned as
  the tagged-wire jsonb row), and a mixed-direction `$sort`
  (`[-priority, name]`) where `priority` is ABSENT (`none`) on every fifth row
  — the §7.3 placement (descending ⇒ none first) realized purely by the
  `sort_enc` rank byte, `ORDER BY <ord bytea>, key_enc`.
- **Full-stream parity**: `jsonb_agg` of the extension-evaluated, extension-
  ordered row stream **equals** the same view hand-written in native SQL
  (field extraction, native `revenue - cost`, `ORDER BY priority DESC NULLS
  FIRST, name, key_enc`) — `parity = t`, 1 715 view rows of 2 000 stored (285
  filtered), none-block ordering included.
- **Plan**: `Sort` (Sort Key = the `eval_ord` call + `key_enc`) over a
  `Bitmap Heap Scan` + `Bitmap Index Scan on node_key_lookup`
  (`Index Cond: parent_id = 0 AND step_name = 'projects'`), the filter face
  only in the Filter line (`Rows Removed by Filter: 285`), **no Seq Scan**.
  The planner chose the bitmap form over a plain Index Scan for the 2 000-row
  range — still index-served; gate (12) accepts either. With `LIMIT 10` the
  Sort becomes **top-N heapsort** (26 kB), halving execution time — `$limit`
  pushdown observed working.
- **Shape-directed descent** (the rule in §7.6): a two-collection recursive
  descent (`step_name = ANY('{subcompanies,offices}')`) plans as
  `Nested Loop(WorkTable Scan, Index Scan using node_key_lookup)` with the
  `ANY` **inside the Index Cond** — 843 rows, no Seq Scan. The all-children
  form (`parent_id = p.id` alone) plans as **Seq Scan + Hash Join** —
  the Phase-5 trap, reproduced and pinned. With a dedicated
  `nodes(parent_id)` index the planner DOES switch to a parameterized
  `Index Scan using nodes_parent` nested-loop (answering the Phase-5 open
  question) — kept as the documented rescue, not built (§7.6).
**The v5 subtree-prefetch claims, prototyped in the same image** (extension
extended with a stand-in subtree face `liasse_eval_demo_subtree(pred, row,
subtree) -> bool` — candidate open AND ≥ N open rows in its prefetched
subtree — and the guard `liasse_demo_guard(depth, max)`; `demo-subtree.sql`,
run after `demo.sql`; the v3 and v4 numbers re-reproduced first — 139/139/781
index-served, `parity = t` 1 715/2 000, top-N):

- **The flat form**: the 5 children of `/companies['1']` each evaluated
  against their own 155-row live `subcompanies` subtree through the §7.6
  lateral — verdicts `t,f,t,t,f` (3 admitted), **full parity** with the
  native-SQL oracle (per-candidate recursive counts hand-written in SQL):
  `parity = t`.
- **Plan (flat)**: Nested Loop of the candidate `Index Scan using
  node_key_lookup` (5 rows) with a per-candidate `Subquery Scan … Aggregate …
  Recursive Union` — the lateral's anchor AND recursive term each `Index Scan
  using node_key_lookup` with the `= ANY($steps)` **inside the Index Cond**
  (775 inner probes across 5 subtrees), the guard on the WorkTable Scan's
  Filter line, the subtree face **only in the Filter line** — **no Seq Scan
  anywhere**, 6.9 ms end to end.
- **The coverage composition is legal and index-served**: a `WITH RECURSIVE`
  prefetch lateral inside the recursive term of the outer coverage CTE
  (nested recursive CTEs, self-reference used once) planned and ran:
  included count **42 = 42** against the native-SQL oracle, both recursions
  entirely on `node_key_lookup`, hereditary pruning visible (rejected
  candidates' subtrees prefetched once for their own verdict, never descended
  for coverage). Honest cost, recorded: 210 lateral invocations (once per
  *examined* candidate — 5 children per each of 42 included rows) at ~1.6 ms
  fixed rescan cost each ⇒ 348 ms total on the demo tree; the per-invocation
  overhead (recursive-subplan rescan + per-row guard/`jsonb_agg` calls, not
  probe count) is the §9 subtree bench axis, and the interpreter path pays
  the same per-candidate materialization — the floor relocated, not added.
- **Cyclic data bails, never hangs**: a planted parent-ring (two rows
  parenting each other) left the sound address-resolved read untouched
  (780 live subtree rows — the ring is detached, §7.6 structural note), and a
  descent forced to start INSIDE the ring errored at the bound —
  `ERROR: liasse: recursive descent exceeded depth 64: data cycle or corrupt
  over-deep tree` — in milliseconds, exactly the loud-bail contract the
  guard promises.
- **Not covered by the prototype, honestly**: linking the full `liasse-expr`
  interpreter (the demo links serde_json only — but a cdylib linking pure-Rust
  rlibs is a plain Cargo property, not a pgrx risk), the postcard wire + decode
  cache, the real `sort_enc` (the demo encodes int/text with a NUL-terminator
  shortcut; the real encoding reuses `key_enc`'s NUL-escaping and decimal
  normalization), coverage-projection-in-outer-SELECT (composed from proven
  parts: the v3 CTE + the v4 jsonb face), per-row cost at scale (a §9
  bench axis), and two subtree details: the demo lateral aggregates flat
  `[step_name, key_wire, value]` triples rather than accumulated rel-path
  arrays (the accumulation is the same `||` mechanics the CTE's
  `key_path`/`sort_path` columns already prove), and the stand-in face counts
  subtree entries rather than rebuilding nested `Cell::Collection`s (the real
  build is descriptor-driven decode through the shared codecs, §7.4). The
  prototype artifacts — Dockerfile, extension source, initdb script,
  `demo.sql`, `demo-eval.sql`, `demo-subtree.sql` — are committed at
  `crates/liasse-pg/design-prototype/` (design collateral, not implementation)
  and are the templates §12 builds from; `docker build` + the three demo runs
  reproduce every number above.

## 8. §12 live views and windowing over pure PG

**Mechanics that stay in Rust (and why) — confirmed against mandate 2.**
`watch.rs`/`window.rs` diff recomputed `ViewResult`s and slice windows over the
view's total sort order — evaluated sort tuples plus `RowId` occurrence tiebreak
(§12.2, §7.3, B.5). The §12.2 diff (`patch.rs`), the window partition, and the
frozen-gap anchor state are *session-relative* — exactly what mandate 2 assigns
to Rust session code — and they stay there unchanged under pushdown: what
changes is where the `ViewResult` they consume comes from. A "PG-side window"
(pushing `$size`/`$anchor` down as its own `LIMIT`/range) remains **rejected**:
the window is defined over the view order at a frozen gap coordinate with
neighbor tracking, which needs the full ordered row stream regardless (below).
The pushed query DOES apply the surface's own `$skip`/`$limit` (§7.6) — the
view's declared bounds, which cap what any window can see (§12.2).

**What changes under mandate 5: a pushed view's advance is one query, not a
hydration.** Per commit, per subscription, the engine today runs
`store.snapshot(head)` → `Prospective::from_snapshot` → re-evaluate →
`Watch::advance` diff. The pushdown splits subscriptions in two:

- **Pushed views** (the §7.5-lowerable ones — flat filtered/projected/sorted
  views and §10.5 coverage): the advance calls `scan_view` at the committed
  head — ONE SQL statement returning the evaluated, ordered view rows — decodes
  them into the `ViewResult`, and diffs. No snapshot, no `Prospective`, no root
  materialization for this subscription. The per-read hoisted env (the `$actor`
  row, `@params`, hoisted `/`-reads at this frontier) is rebuilt per advance —
  its `/`-read entries are themselves §4 point/scan reads, so an env with k
  hoisted collection reads costs k+1 statements, not a hydration. The §12.2
  re-authorization at each frontier (role membership, actor liveness) already
  runs on point reads and is unchanged.
- **Fallback views** (§7.5 residuals, non-head frontiers): today's path —
  `snapshot(head)` hydration + interpreter — with the Phase-4/6 mitigations.

Honest cost accounting and the mitigations, in order:

1. **Until Phase 6**, `snapshot(head)` is an O(history) log fold *per advance*
   for FALLBACK views. **Phase 6's head fast path** makes it O(state). A pushed
   view never pays either — its floor is O(its own result), the §7.6 statement.
2. **Sharing (Phase 4, generalized)**: hydration sharing stays **once per
   (instance, frontier)** for fallback subscriptions. Pushed subscriptions
   share at a finer grain: one `scan_view` per distinct
   **(view address, args, scope, frontier)** serves every subscription on that
   tuple (the common fan-out — many clients watching the same surface — is
   exactly this case); distinct args/scopes are genuinely distinct queries.
3. **Commit-scoped skip (seam, phase-later)**: `log_from(prior_frontier)`
   yields exactly the committed ops between two frontiers (already SQL-served).
   A subscription whose compiled view's collection-dependency set is disjoint
   from the touched addresses can skip its re-query/re-evaluation and emit a
   frontier-only no-op. For a pushed view the dependency set is a byproduct of
   lowering (the source path + every hoisted `/`-read), so the conservative
   analysis is *easier* there; still a seam, not built now.
4. **True incremental maintenance** (per-op delta → per-view patch without
   re-evaluation) is out of scope — it belongs with the deferred
   `liasse-connect` deliverable, and the §12.2 contract ("after applying every
   patch the client result MUST equal the authorized declared view") is exactly
   what makes recompute-and-diff the safe baseline. The pushed re-query IS
   recompute-and-diff — recompute moved to PG, diff unchanged.

**Coherence.** A pushed advance evaluates "at head" as one SQL statement (case-1
consistency, §5.4); in-process writer exclusivity (`&mut Engine` to commit vs
`&Engine` to advance) guarantees the head cannot move between the commit that
triggered the advance and the advance's query — the same argument that already
covers `Prospective::gather`'s multi-scan sequences, now needed for at most
1 + k statements (query + env reads). The env reads and the view query thus see
the same frontier; the §5.4 `read_session()` seam remains the defence-in-depth
if one-writer is ever relaxed.

**Hard parts, honestly**: (a) per commit, the engine now runs one query per
distinct watched (view, args, scope) tuple — N distinct watched views = N
indexed queries per commit. That is the recompute-and-diff floor relocated, not
removed; it beats O(state) hydration when views are selective or bounded
(`$limit` becomes top-N in-PG, §7.8) and loses nothing when they are not — but
a commit storm over many distinct watched views is still N×, and only seam 3 /
IVM reduce that. Measure in Phase 10 benches. (b) The window's full-view
neighbor tracking means a bounded window still consumes the full (post-`$limit`)
recomputed row stream — the pushdown reduces what leaves PostgreSQL to the
view's own result (projected columns, not whole rows; pruned coverage subtrees
never fetched), not below it. (c) The §12.2 `sort_tuple` gap coordinate
requires the decodable evaluated tuple per row — the `eval` sort-tuple column
(§7.6), a second per-row sort evaluation; watch queries pay it, plain `view`
reads skip it. (d) Resumable frontiers (`init`/`patch` replay for reconnecting
clients) read old frontiers → O(history) log folds plus Rust-side evaluation by
design; that is the §19.2 replay primitive working as specified, not a
regression (§7.3 "frontier scope").

## 9. Parity, gates, benchmarks

- **Parity**: `MemoryStore` stays the oracle; the `scenarios_gate_against_pg_store`
  0-divergence gate and the shared `contract_tests` battery must be green after
  *every phase* (the battery is updated once, in Phase 0, for the §3 signatures).
  `snapshot` parity is by construction (shared `Snapshot::replay`).
- **Evaluation parity (revised for mandates 4+5)** — v2's three layers collapse
  to two, because the store-vs-store layer is now the same linked code:
  1. *Lowering parity* (runtime unit level): for a corpus of views × states,
     the interpreter path (materialize root → evaluate → `ViewResult`) must
     agree — rows, exposed values, order, sort tuples — with the lowered path
     (`scan_view` on MemoryStore → `ViewResult`); and for §10.5, for a corpus
     of predicates × candidate rows, `CompiledRecursive::included` must agree
     with the composed `RowPrograms::admits`. This is the gate on the ONLY
     reimplemented seam (§7.3): hoisting, the residual audit, the source
     classification, descriptor-driven candidate construction, the computed
     fold, projection-output ordering, and the sort-tuple comparison.
  2. *Store parity as a regression backstop*: `scan_view` on MemoryStore vs
     PgStore over the same `RowPrograms`. Agreement is by construction (same
     faces), so what this actually guards is the machinery *around* it: the
     postcard wire (also pinned directly by an encode∘decode proptest), the
     jsonb `value_codec`/`key_wire` decode inside the extension, the projected
     jsonb result decode, **`sort_enc` vs the shared Annex-B tuple comparison**
     (also pinned directly by a proptest: for random tuple pairs and direction
     vectors, memcmp of encodings ≡ the tuple comparison), `OFFSET`/`LIMIT` vs
     Rust bounds, the decode cache keying, the CTE's traversal semantics, and
     the SQLSTATE→`StoreError::Eval` fault mapping (a division-by-zero program
     must surface, through the fallback, as the interpreter's own error on both
     stores).
  3. *Adversarial corpus — kept, verbatim, as the backstop's teeth*: decimal
     stored at three scales compared for equality (`1` = `1.0` = `1.00`); text
     carrying `U+0000` and `\` (stored and bound); an optional field in all
     three durable spellings under `==`, `!=`, and an ordering op; a hoisted
     `$actor`; `$except` overriding `$where`; a `closed` root (anchor
     unfiltered); a tombstoned intermediate (branch blocked); `child.$key`
     compares including a scale-variant decimal key; an `in` set; `has()`; a
     computed field; candidate arithmetic including a division-by-zero fault
     case; `string.lower` on a NUL-bearing candidate field; a
     nested-static-struct member; an `in` over a hoisted `/`-collection. Plus
     the mandate-5 surface: a projection with a computed output and an omitted
     `none` optional output; `$sort` on an absent-optional key ascending AND
     descending (placement per §7.3); a mixed-direction two-key sort; a sort
     key of NUL-bearing text; a descending decimal key at mixed scales;
     `$skip`/`$limit` over ties; an unsorted view (key order); a projected
     static-struct output; a `/accounts[.owner]`-style hoisted-haystack deref
     in an output. Plus the mandate-6 surface: `size(child.subcompanies)`
     compared at a boundary value; an aggregate over a filtered selection of
     the candidate's subtree; a subtree read through TWO nested collection
     names (the multi-step `= ANY` universe); a candidate whose read-set
     collections are empty (`'[]'` subtree); a tombstoned intermediate inside
     the candidate's subtree (its branch invisible to the predicate — the
     live-rows rule); a carried computed field that itself reads a nested
     collection (transitive read-set); candidate-dependent built-in calls
     (`string.lower` composed with `time`/`convert`/`sha` members) now
     admitted in-PG; and the depth-guard error case (an over-deep chain past
     `MAX_SUBTREE_DEPTH` → the corruption error on both stores). Expected
     results hand-derived from Annex A.1/B and §7.3 —
     externally deducible, per AGENTS.md.
- **New refusal gates** (§7.7/§12): open against a database without the
  extension → actionable `StoreError`, nothing partially reconciled; open
  against a skewed ABI (simulated by installing a stub `liasse.abi_version()`
  returning a different string on a bare PG) → actionable refusal; the
  load-time **pushdown report** naming a scope-deferred view with its typed
  reason, and strict mode promoting it to an error (§7.5 — the v4 gate for
  the app-host-call load rejection was deleted with that rejection under
  mandate 6; mandate 7's replacement is a CHECKER rule with its own static
  corpus cases, below, not a store gate); the **cycle/depth bail gate**: a
  corrupt over-deep chain (and a
  forced descent on a planted ring, per the §7.8 construction) surfaces the
  reserved-SQLSTATE guard error as the corruption-classed `StoreError` on
  PgStore AND the identical error from MemoryStore's counter — never a hang,
  never a truncated result (corpus case, written before the mechanism lands).
- **Mandate-7 corpus (the mutation-only load rule; corpus first, per
  AGENTS.md).** NEW static (`suite: static`, `outcome: invalid`) cases
  modeled on
  `tests/16-host-namespaces/common/generated-function-in-view-rejected.hjson`,
  one per position family — an app-registered fn in: a `$view` filter
  (`app-function-in-view-filter-rejected`), a projection output, a `$sort`
  key, a §10.5 `$where` (`app-function-in-coverage-where-rejected`), a
  computed value (`app-function-in-computed-value-rejected`), a `$check`
  (`app-function-in-check-rejected`), a `$normalize`
  (`app-function-in-normalize-rejected`), an auth `$verify`
  (`app-verifier-in-dollar-verify-rejected`), and a field default
  (`app-function-in-default-rejected`); each pins the load rejection and
  its §16.5 citation. UNAFFECTED and kept verbatim: every
  mutation-body app-fn case — the w2 worked examples
  (`webauthn.verify`/`oidc.verify` invoked in §11.5 login MUTATION bodies:
  `w2-passkey-login-opens-session-and-authenticates` and siblings), the
  §17 cose/keyring suite (`cose.sign` in mutation statements, `$verify:
  "cose.verify(…)"` — native), and §20 delta-program codec cases. MUST BE
  RECAST — cases that today run an app fn OUTSIDE a mutation body as their
  carrier and would contradict the amended SPEC:
  `required-namespace-pure-function-runs-in-view` (asserts the very behavior
  §16.5 forbids — recast the pure call into a mutation body, or flip it to
  the static rejection), `verifier-namespace-runs-at-admission` (an app
  verifier in `$verify` — recast onto the §11.5 auth-mutation pattern: the
  scenario's per-request app verification becomes a login mutation minting
  a native token), `generated-default-fixed-and-recorded`
  (`token: "text = util.token()"` — the app generated call moves into the
  creating mutation's body; the §8.12 recording semantics under test are
  unchanged there), `compatible-minor-resolves-within-major`,
  `pinned-descriptor-drift-fails-reopen`,
  `required-namespace-removed-fails-reopen` (their initial load must
  succeed, so their `util.double(.n)` view carrier moves into a mutation
  body; the resolution/drift mechanics under test are
  position-independent), and `namespace-signature-type-mismatch-rejected`
  (still invalid, but its pinned detail names the signature mismatch — move
  the carrier into a mutation body so that diagnostic, not the position
  one, stays the asserted error). Effect-class cases
  (`generated-function-in-view-rejected`, `verifier-function-in-view-rejected`)
  keep their diagnostics: the checker tests effect class BEFORE origin
  (§7.5 check order). The SPEC's own §11.3 `api_key` authenticator example
  is amended with the §11.3 draft (its pattern re-models per §7.5).
- **Index gates** (`index_coverage_pg.rs`) — the gate becomes the READ gate. Keep
  (1)–(6); add, on the populated tree:
  - (7) depth-3 `row` chained-InitPlan point lookup → index-only, no Seq Scan;
  - (8) depth-3 `scan` in the §4.2 form → index-**ordered** (no Sort, no Seq Scan) —
    this pins the scalar-subquery formulation against the join formulation regressing in;
  - (9) `scan_subtree` recursive CTE (shape-directed, `step_name = ANY`) → no
    Seq Scan anywhere; anchor and recursive term each use `node_key_lookup`
    with the `ANY` **inside the Index Cond** (walk the `Recursive Union`
    children) — this pins the §7.6 shape-directed rule against the
    all-children `parent_id`-only join regressing in (which plans Seq Scan +
    Hash Join, §7.8);
  - (10) `has_blob` EXISTS probe → index-only;
  - (11) the coverage `scan_view` CTE (a composed and/or/not admit with a
    hoisted parameter) → anchor and recursive term each `Index Scan using
    node_key_lookup`, no Seq Scan; `liasse.eval_bool` appears **only in a
    Filter line** (never a scan source); the recursive term's inner-scan loop
    count equals the included-row count (pruning-during-descent, pinned from
    the plan). This is the EXPLAIN tripwire for mandates 3+4, matching the
    §7.8 prototype plan.
  - (12) the flat `scan_view` statement (filter + projection + two-key mixed
    sort) → the collection range served by `Index Scan` OR
    `Bitmap Index Scan`+`Bitmap Heap Scan` `using node_key_lookup` (both
    index-served; the planner picks by range width, §7.8), **no Seq Scan**;
    `eval_bool` only in the Filter/Recheck line; `eval_sort` only in the Sort
    Key; with `$limit`, a `Limit` over a top-N sort. This is the EXPLAIN
    tripwire for mandate 5.
  - (13) the subtree-prefetch lateral (§7.6), flat AND composed into the
    coverage CTE's recursive term → **no Seq Scan anywhere**; the candidate
    scan and the lateral's anchor and recursive term each index-served on
    `node_key_lookup` with the `= ANY($steps)` **inside the Index Cond**;
    the subtree face only in a Filter line; `liasse.guard_depth` present in
    every recursive term (walk the plan tree for it — its absence is the
    hang-risk regression). This is the EXPLAIN tripwire for mandate 6b,
    matching the §7.8 v5 plans.
  - Pinned-exemption tests: single-row `instance_meta` reads (exists), the
    `alloc_incarnation` single-row UPDATE (§6.3), and the Phase-6 head fast path
    (a full-state materialization has no selective plan; assert instead that it is
    *one* statement and equals the log fold).
- **Benchmarks — the current numbers are void.** They measured `BTreeMap` reads (the
  forbidden projection). Re-run the criterion suite against pure PG with the
  overhead axis defined as *contract read vs the identical hand-written SQL on the
  same pool* (the AGENTS.md "near-raw-PostgreSQL overhead" gate — near **raw SQL**,
  not near RAM). For `scan_view` the "raw SQL" comparator is the §7.6
  statement/CTE itself run by hand on the pooled connection — the extension
  calls are part of the raw cost on both sides, so the gate keeps measuring the
  *backend's* overhead, not the interpreter's. Axes: `row` at depth 1/3/5;
  `scan` of 64/4 096 rows; `scan_subtree` of ~1 000 nodes; coverage
  `scan_view` of a ~1 000-node tree at 10 %/50 %/90 % pruned (vs the same tree
  via `scan_subtree`+Rust pruning — the number that justifies mandate 3);
  **the headline mandate-5 axis: a watched flat `$view` (filter+projection+
  sort, 10 %/90 % selectivity, with and without `$limit`) served by pushed
  `scan_view` vs the same view by `snapshot(head)`-hydrate-then-evaluate — the
  number that justifies the pushdown**, recorded per state size 10³/10⁵ rows;
  **per-row face cost** (the statement with the extension faces vs the same
  statement with hand-lowered native-SQL expressions from the retained corpus —
  the measured price of generality, with the decode cache on and off, and the
  `eval_sort`+`eval` double-evaluation overhead recorded to judge the
  `eval_row` seam); env-heavy programs (a hoisted 10³-row `/`-collection
  haystack — the §7.5 env-size caveat, measured); **the subtree-prefetch
  axis (mandate 6b): a subtree-reading view/coverage at fanout 4/16 and
  depth 2/5, per-candidate lateral cost isolated (the §7.8 ~1.6 ms/rescan
  fixed overhead, re-measured with the real faces) vs the same read on the
  interpreter path — the number that justifies option (a), and the trigger
  for its optimization seams if it regresses**; `snapshot(head)` fast path
  vs log fold at 10³/10⁵ commits; `head`; `alloc_incarnation`; commit. Record
  results in the crate before closing Phase 6 (core axes), Phase 9 (extension
  axes), and Phase 10 (pushdown/§12 axes).

## 10. Migration plan — every phase lands green (corpus + parity + index gates)

| Phase | Content | Exit criteria |
|---|---|---|
| **0** | Contract surgery (§3 signature table) in `liasse-store`; MemoryStore + battery + runtime/surface/testkit ripple; add `r2d2`/`r2d2_postgres`; `PgStore` gains the pool (built post-reconcile) — **reads still projection-served**; AGENTS.md pool clarification | workspace compiles; all gates green; zero behavior change |
| **1** | Leaf reads → pooled SQL: `head`, `get_blob`, `has_blob`, `point_position`, `definition`, `composition`, `log_from`; delete projection fields `blobs`, `points`, `definition`, `composition`, `head` | parity + corpus green; gate (10) added |
| **2** | `row`/`scan` → §4.1/§4.2 SQL; `PgTransition` overlays the SQL base; `NodeWriter` resolves via in-txn SQL (§6.1); commit stops writing any counter (§6.2); **`alloc_incarnation` → durable burn-on-allocate `nextval` (§6.3)**; delete `by_id`, the `new_ids` plumbing, and the projection's incarnation counter | gates (7)(8) added and green; parity green incl. abort-then-commit token scenarios |
| **3** | `snapshot` → §4.3 log fold; delete `projection.log`; **delete `projection.rs`**; gut `node_load.rs` to the address-reconstruction helper Phase 6 will reuse; `PgStore` fields = §2 exactly | grep-provable: no durable-state field on `PgStore`; reopen test still passes (now trivially) |
| **4** | §12/read-path hygiene: hydrate once per (instance, frontier), share across watches; engine read paths prefer `snapshot(head)` hydration over N live `scan`s where committed-state reads suffice | parity + corpus green; watch tests green |
| **5** | `scan_subtree`: contract (**with the §3 `steps` parameter — shape-directed per §7.6; the `parent_id`-only join is a pinned anti-pattern**) + MemoryStore range impl + PG CTE + adoption in `gather_tree`/`rows_at`/`materialize_row_cell` (semantics-free hydration: admission gathers, receiver walks, fallback-path views); depth guard | gate (9) green (incl. the `= ANY` Index Cond pin); hydration round trips measured before/after |
| **6** | `snapshot(head)` fast path from `nodes` + tree≡log-fold equivalence test; core benchmark re-run + recorded numbers | bench report committed; overhead within gate |
| **7** | The evaluator stack, Rust side: `liasse-expr` `eval-wire` feature (serde derives, hoist + residual audit, postcard wire, **subtree read-set extraction — transitive over carried computeds, §7.3**); **`liasse-pred`** crate (`RowPrograms` with the three faces incl. the `CandidateSubtree` argument and deep-candidate build, descriptor with read-set, shared computed fold, composed coverage admit, `EVAL_ABI`, round-trip proptests); `sort_enc` in the codec (+ the memcmp≡tuple-cmp proptest); `liasse-store` `ViewProgram`/`CandidateSubtree`/`ViewSource`/`EvaluatedRow` + `StoreError::Eval` + the corruption-classed depth-guard error + `MAX_SUBTREE_DEPTH` + `scan_view` + MemoryStore impl (scan + descent + per-candidate depth-guarded live subtree build); runtime lowering (source classification, hoist, audit, read-set) + head-frontier reads of §10.5 coverage AND lowerable `$view`s served via `scan_view` on **both** stores (MemoryStore evaluates in Rust — behavior identical, architecture in place); pushdown report + strict mode + corpus cases for the report, the depth-guard bail, and the layer-1 parity corpus incl. the mandate-6 subtree cases (corpus first, per AGENTS.md). **Prerequisite folded in (mandate 7 — the v5 §16-removal workstream is CANCELLED, §16 stays): the mutation-only load rule lands with or before this phase — `HostOrigin` on `HostOp` + the collapsed `HostPosition` (`DbRead(kind)`/`Default`/`Mutation`, §7.5) + the `check_host_call` origin conjunct (liasse-expr), position wiring at every site (liasse-model `check_tree`, liasse-runtime `compiled.rs`/`recursion.rs` — incl. `$check`/`$normalize`/`$verify` moving to `DbRead`), the §16.5/§11.3/§16.3/§8.8 SPEC amendments, and the mandate-7 static corpus incl. the §9 carrier-case recasts (auth-mutation re-model) — so this phase's lowering, and every admission validation program, receives built-in-only expressions by construction** | parity green; lowering-parity suite green (views + coverage + subtree programs); wire + sort_enc proptests green; corpus report/guard cases red→green; mandate-7 static cases red→green |
| **8** | The extension + image: **`liasse-pg-codec`** split out of `liasse-pg` (`value_codec`, `jsonb_text`, `key_enc*`, `sort_enc` + their test files; mechanical, liasse-pg re-exports); **`liasse-pg-ext`** pgrx cdylib (the five-argument `liasse.eval` + `liasse.eval_bool` + `liasse.eval_sort` incl. subtree decode, `liasse.guard_depth`, `liasse.abi_version`, decode cache, lint carve-out §12.1); the two-stage Dockerfile + image build (§12.2, from the §7.8 template); test-harness container path + `LIASSE_PG_IMAGE` (§12.4); CI image job | extension unit tests (`cargo pgrx test`) green; image builds in CI; harness boots it; abi handshake round-trips |
| **9** | PgStore `scan_view` → the §7.6 SQL (flat statement + coverage CTE + the subtree-prefetch lateral with `liasse.guard_depth` in every recursive term) calling the three faces; reconcile extension step + ABI handshake + refusal gates (§7.7) incl. the cycle/depth bail gate; fallback wiring (non-head frontier, hoist-eval error, `StoreError::Eval`, scope-deferred views); gates (11)(12)(13) + §9 adversarial corpus (incl. the mandate-6 subtree cases) over the extension path; coverage + per-row-face + subtree-prefetch bench axes | gates (11)(12)(13) green; 0-divergence on the adversarial corpus; refusal + bail gates green; pruned-tree + face-cost + subtree-lateral benches recorded |
| **10** | §12 adoption: watch/window advance over pushed views (`scan_view` at head → `ViewResult` → diff, §8); per-(view, args, scope, frontier) query sharing beside the Phase-4 hydration sharing; sort-tuple column wiring for windowed subscriptions; **the pushed-read benchmark axes — the whole point: pushed `scan_view` vs hydrate-then-evaluate, watch-advance end to end** | watch/window suites green on both paths; §8 sharing observable in tests (one query per tuple per frontier); pushdown bench report committed |

Ordering rationale: reads convert one contract method at a time **behind the parity
gate**; the projection dies only when its last reader does (Phase 3); optimizations
(5, 6) come only after the pure-PG semantics are locked by the gates. The evaluator
pushdown splits four ways (7, 8, 9, 10) so that the semantics (lowering + the
program faces + MemoryStore realization) land gated **before** any PG artifact
exists, the build artifacts (codec split, extension crate, image, harness) land as
pure infrastructure with their own unit gates, then PgStore switches its evaluated
read onto the extension — making any Phase-9 divergence attributable to transport
(wire/jsonb/sort_enc/SQL), never to evaluation semantics — and only then does the
§12 loop consume the pushed path, so a watch regression is attributable to the §8
wiring, never to the read itself. The one dependency that formerly lived
outside this table — v5's §16 removal workstream — is **cancelled by
mandate 7**: §16, the registration API, and `$requires` all stay. Its
replacement, the mutation-only load rule (§7.5), is small enough to live
INSIDE Phase 7 (checker enum + origin field + one conjunct + position
wiring + static corpus + the SPEC.md §16.5/§11.3/§16.3/§8.8 amendments) and
must land with or before that phase's lowering, which assumes checked
DB-run programs are built-in-only. The §11 auth re-model (custom
verification as auth mutations) rides the same workstream: it is a SPEC +
corpus change, not an engine one — the §11.5 login-mutation machinery
already exists.

## 11. Risks and judgment calls

**Risks, hardest first**

1. **Toolchain risk — pgrx** (replaces v2's compiler-fork drift, which is
   eliminated with the second evaluator): pgrx is the de-facto standard for Rust
   PG extensions (PostgreSQL-org adjacent, actively maintained, used in
   production by pgvectorscale/plrust/ZomboDB lineage) but it is a large macro
   framework with its own release cadence coupled to PG majors. Containment:
   pgrx appears in exactly ONE leaf crate; the wire and semantics live in
   pgrx-free crates (`liasse-pred`); the extension surface is two functions, so
   a pgrx major bump or (worst case) a rewrite against raw C FFI touches ~200
   lines. Versions are pinned (workspace `pgrx`, Dockerfile `cargo-pgrx`, both
   upgraded deliberately together).
2. **Version skew** between the `.so` and the runtime: two artifacts now share
   one semantics. Contained by the §7.7 double lock — CI builds both from one
   commit; the `abi_version` handshake refuses operational skew at open, before
   any read. Residual risk is a wire change without an `EVAL_ABI` bump slipping
   through review *and* the round-trip/parity gates on one commit — accepted as
   comparable to any same-repo protocol.
3. **Panic and unsafe containment**: the workspace forbids `unsafe` and denies
   panics; a pgrx crate cannot inherit either lint (its generated FFI glue is
   `unsafe`; its error path converts Rust panics to PG `ERROR`s via the guarded
   boundary, longjmp-safe by pgrx's design). Resolution, surfaced not buried:
   `liasse-pg-ext` opts out of the workspace lint set with a crate-level comment
   pinning the rules — no *hand-written* `unsafe`, no panicking code of our own;
   every fallible path returns `Result` and converts to a single
   `pgrx::error!`/SQLSTATE site (§7.4). A panic that does occur (a bug) aborts
   the transaction with a PG ERROR — never the postmaster — and surfaces as
   `StoreError`, satisfying "fail loud". AGENTS.md gets one clarifying sentence
   in Phase 8, like the Phase-0 pool sentence.
4. **Per-row evaluation cost**: the interpreter per row replaces v2's native
   SQL operators per row — generality has a price (jsonb decode + candidate
   build + tree walk per row *per face*, §7.4; the expr/env decode is amortized
   by the cache). The §9 face-cost bench axis measures it against hand-lowered
   native-SQL expressions on the same data, the double sort evaluation
   (`eval_sort` + the tuple column) is recorded separately to judge the
   `eval_row` composite seam, and the near-raw gate keeps the *backend*
   overhead honest. The subtree prefetch adds its own instance of this risk:
   the §7.8 v5 measurement shows ~1.6 ms *fixed* cost per lateral invocation
   (recursive-subplan rescan + per-row guard/agg calls) — once per examined
   candidate of a subtree-reading program; the §9 subtree axis re-measures it
   with the real faces, and the interpreter path's identical per-candidate
   materialization is the honest comparator. If the price is ever intolerable,
   the v2 lowering could
   return as a *transparent optimization* for expressions it can serve — the
   architecture (opaque `ViewProgram` behind the contract) leaves that door
   open without another contract change.
5. **Planner drift**: the no-Sort scan plan and the index-served recursive terms
   are plan shapes, not guarantees; a PG major could regress them. Mitigation:
   EXPLAIN gates (7)–(11) are deterministic CI tripwires, and the deployment
   image pins the PG major/minor, so plan-affecting upgrades arrive only with an
   image bump CI has already gated.
6. **Round-trip inflation**: unchanged from v2 — Phases 4–5 collapse the
   multiplier, §7 removes the biggest single source (coverage over-fetch), and
   the benchmark gate keeps the backend's overhead honest. Per-alloc incarnation
   round trips remain the one new write-path cost (§6.3; batching seam designed).
7. **Ripple breadth** of fallible `head()`/owned `definition()` — wide but
   mechanical; contained in Phase 0's single commit.
8. **Prepared-statement churn**: sync `postgres::Client::query(&str)` re-prepares
   per call; per-depth generated SQL multiplies distinct texts. If benches flag
   it, cache prepared statements per (connection, shape) via r2d2's customizer —
   an infrastructure cache, not a data projection.
9. **Recursive-CTE cycle on corrupt data**: bounded by `liasse.guard_depth` in
   every recursive term (coverage CTE, `scan_subtree`, the prefetch lateral)
   and by the same shared constant in MemoryStore's descents — a loud
   corruption error, never a hang or a truncation. Structurally, single-parent
   adjacency confines any parent-cycle to a detached ring no address-resolved
   descent can enter (§7.6, prototyped §7.8); the guard is defence in depth
   over that invariant, plus the refusal of legitimately over-deep data.
10. **Pool exhaustion/failure**: small pool + short reads; checkout timeout maps
    to `StoreError::Backend` (fail loud, never block forever).
11. **Deployment surface**: self-hosting a custom PG image means PG minor/CVE
    updates arrive through *our* image rebuilds, not the distro. The Dockerfile
    tracks `postgres:17-bookworm` (PGDG), so a rebuild is `docker build` + the
    CI gate; the §7.7 handshake makes a forgotten runtime upgrade a refusal, not
    a divergence. Operational cost accepted with the maintainer's self-hosted
    premise.

**Judgment calls the mandates did not fully specify** (flagged for maintainer review)

- **Serialized form = the hoisted `TypedExpr` itself** (postcard, feature-gated
  derives), not a new expression IR: a second IR would reintroduce exactly the
  lowering-divergence surface mandate 4 exists to kill. Cost: serde derives on
  internal expression types — accepted as a private, version-locked wire (§7.4),
  never a public format.
- **Three typed SQL faces over one evaluator, not the sketch's single
  `eval(…) -> jsonb`** (§7.4): `IS TRUE` does not type-check over jsonb, and
  `ORDER BY` needs memcmp-Annex-B bytes a jsonb result cannot provide. The
  faces share wire, cache, candidate build, and interpreter; the composite
  `eval_row` (one call returning admit+projected+ord) is a bench-driven seam.
- **`sort_enc` is a NEW order-preserving encoding** (direction-folded, §7.3
  none placement) rather than sorting in Rust after an unordered fetch: SQL
  ordering is what makes `$limit` a top-N and keeps the statement "one query".
  Its equivalence to the shared tuple comparison is proptest-pinned (§9). The
  admitted gap: a `json`-typed sort key is not encodable in v1 → per-view
  fallback, reported. Encoding the total Annex-B JSON internal order is
  designable if a real package needs it.
- **`key_wire` instead of the sketch's `key_enc`** as the function's key
  parameter — `key_enc` is one-way by design; the decodable identity is
  `key_wire` (§7.4).
- **Coverage admit composed as `$where && !$except`** in ONE program/call
  (§7.3), not two calls: interpreter short-circuit reproduces the two-step
  `included()` order and fault surface exactly; one face call per worktable row
  instead of two.
- **Policy, uniform at lowering (v5+v6, §7.5)**: nothing is left for the
  LOWERING to reject — coverage predicates and general `$view` programs are
  equally pushable, so the v4 split (coverage load-time rejection + opt-in
  vs view fallback) stays collapsed to ONE rule: automatic per-view
  interpreter fallback only for scope-deferred forms and dynamic routes,
  never silent (pushdown report), with strict mode promoting any reported
  residual to a load error. The one load-time rejection that exists is the
  CHECKER's mandate-7 rule (a §16.5 language error, uniform over every
  non-mutation-body position — not an engine pushdown policy); the v4 §10.5
  SPEC note stays withdrawn, and the report/strict knob stays engine
  configuration, not SPEC surface. Mandate-3 exposure narrows to "a
  scope-deferred form inside a coverage predicate" (practically: a
  candidate-addressed engine accessor) — reported, strict-refusable, with a
  designed upgrade seam.
- **§16 app-registered host namespaces: KEPT; position narrowed to the
  mutation body (mandate 7, superseding v5's removal)** — registration API,
  `$requires`, descriptors, §11 verifier namespaces, §17 cose, §20 codecs
  all stay; the v5 removal workstream is cancelled. The extension bundles
  only built-ins and never needs an app namespace because no DB-run program
  can contain one (load-checked). The v4
  app-namespaces-linked-into-a-downstream-image seam stays void — not
  because the class is gone, but because it can never reach the extension.
- **Mandate-7 judgment calls and flags (maintainer review):**
  - **Field defaults**: classified DB-run for namespace calls per the
    binding operationalization ("the only framework-run row is the mutation
    body"), so an app *generated/provider* call in a `$default` is now a
    load error and moves into the creating mutation's body
    (`generated-default-fixed-and-recorded` recast, §9). FLAGGED: the
    correction's *rationale* (transactionality) would equally cover a
    default, since defaults evaluate inside the admitted transaction —
    confirm the strict body-only reading. Language `uuid()`/`now()` in
    defaults are unaffected (language functions, not namespaces).
  - **Migration `$up`/`$down` vs `$as`/`$back`**: delta programs are
    classified framework-run (they are programs of mutation statements run
    as one atomic transaction, §20.1 — and §20.1's own pure+deterministic
    constraint still limits them to *pure* app codecs); the per-field
    `$as`/`$back` transforms are declaration-level, hence DB-run — an app
    codec conversion is written in the delta program instead. Judged, not
    explicitly stated by the correction.
  - **Surface `$mut` receiver references** (`.projects[@project].rename`):
    the selector outside the named mutation's body is DB-run (a
    read/selection); selectors INSIDE body statements are body. The
    boundary is the program array, exactly the parser's own boundary.
    Judged.
  - **Admission execution model**: `$check`/`$normalize`/`$verify` are
    built-in-only (classification) but v1 keeps interpreter execution over
    the staged overlay inside the admission transaction — Phases 0–6
    untouched; the in-txn PG admission seam is unlocked by construction and
    recorded, not built (§7.5 mechanism block). FLAGGED: if "should also be
    in db" mandates in-PG *execution* of admission validation, §5.4 case 3
    and the §6 write path re-open — a separate phased decision.
  - **Per-request app-verifier authenticators are gone by design** (the
    §11.3 `api_key` shape): re-modeled as token exchange through an auth
    mutation, or native hash comparison (`sha.sha256`) in `$verify`. The
    §8.12/§16.3 recording semantics actually *improve*: a verifier result
    that matters durably now always lives in a transaction that records it.
  - **Diagnostic ordering**: effect-class violation reports before the
    origin/position violation in `check_host_call`, so the existing
    effect-class corpus diagnostics stay pinned (§7.5, §9).
  - **Candidate-free app calls in DB-run positions are errors too** (no
    hoisting exemption, §7.5): legality never depends on the compiler's
    hoist analysis; the corpus carrier cases this flips are listed in §9.
- **Candidate-subtree reads: prefetch lateral (option (a)) over SPI (option
  (b))** (§7.6): (a) keeps the faces `IMMUTABLE` and every per-candidate probe
  plan-visible and EXPLAIN-gateable; (b) hides per-row table access from the
  plan, pays per-row executor re-entry, drops `PARALLEL SAFE`, and gives the
  `.so` a query surface it otherwise never needs. (b) remains a documented
  seam only, reserved for a hypothetical non-static read-set the typed
  language cannot currently produce.
- **The subtree crosses the SQL boundary as a jsonb argument, not an `env`
  entry**: `env` is Rust-built postcard; the subtree is assembled per
  candidate BY SQL (`jsonb_agg` in the lateral), and jsonb is the natural
  SQL-side aggregate the extension decodes with the codecs it already links.
- **The prefetch is live-rows-only** (tombstone blocks its branch) — it
  reproduces `materialize_row_cell`, the interpreter's candidate tree, and is
  deliberately NOT `scan_subtree`'s traverse-through-tombstones hydration;
  the two tombstone rules remain distinct primitives with distinct consumers.
- **Candidate-addressed engine-state accessors** (a meter accessor on the
  candidate) are classified with the engine-state *scope* class — per-view
  fallback + report, upgrade seam = the same lateral joining the engine's
  durable state — rather than blocking the "no restriction" claim: they are
  engine-derived values, not persisted-row expressions. Flagged as the one
  disposition the mandate wording did not explicitly settle.
- **Aggregates fold in Rust over the pushed stream; combinators/view-refs
  fall back in v1** (§7.5) — dispositions chosen to keep v1's lowering surface
  small and gateable; both have designed upgrade paths that change no contract.
- **The shallow-candidate seam** (§7.3): descriptor-driven candidate
  construction + the shared computed fold are the one reimplemented sliver,
  gated by lowering parity; judged far smaller than v2's per-operator SQL
  surface it replaces.
- **Shape-directed descent everywhere** (§7.6): the `steps` universe comes from
  the CURRENT compiled shape; a migration walking a previous model's rows must
  use that model's universe (or one indexed `SELECT DISTINCT step_name`). The
  dedicated `nodes(parent_id)` index that would rescue a shape-free descent is
  prototyped but NOT added — no consumer, and the reconciler would carry dead
  weight.
- **Env-size**: hoisting a `/`-collection haystack ships it per read (§7.5);
  accepted with a bench axis and the per-view fallback as the relief valve —
  not silently truncated, never partially hoisted.
- **`EVAL_ABI` = crate version + wire revision**, not a build-time content hash:
  a hash would also lock semantically neutral refactors and needs reproducible
  hashing of type layouts; the revision constant plus the gates was judged
  proportionate. Revisit if skew ever bites.
- **Extension objects live in their own `liasse` schema** (control file pins
  it), so instance-schema reconciliation and the orphan sweep never interact
  with them; presence/version are checked per open, cheaply.
- **Test-harness dependency on Docker** for extension-bearing suites (§12.4):
  judged acceptable because the image IS the deployment artifact and CI always
  has it; the initdb-cluster path stays for non-extension development but now
  fails loudly (with the §7.7 message) once a suite needs `liasse.eval`.
  PostgreSQL 18's `extension_control_path` would let the harness inject the
  extension into a disposable cluster without root — noted for when the image
  moves to PG 18.
- `snapshot()` still returns the materialized `Snapshot` value — a
  session-relative computed result under mandate 2, not a read model.
- `scan_subtree` + `scan_view` remain contract *extensions*; `scan_view` stays
  the single semantics-carrying read (both source shapes), but its semantics
  enter through an opaque trait object (§3) rather than a store-defined IR —
  the contract itself is *more* semantics-free than v2's, while serving
  strictly more of the read path.
- The Phase-6 head fast path deliberately full-scans `nodes` (exempted, pinned).
- **Incarnation burn-on-allocate** (§6.3) — unchanged; gap-free tokens are not a
  property anything needs, which is what lets it be a `SEQUENCE`.
- AGENTS.md's interior-mutability rule vs pooled reads: resolved by maintainer
  directive; Phase 0 lands the clarifying sentence. Phase 8 lands the analogous
  sentence for the extension crate's lint carve-out (risk 3).

## 12. Deployment: the extension crate, the image, CI, and the test model

### 12.1 Workspace crates

- **`crates/liasse-pg-codec`** (new, mechanical split): `value_codec`,
  `jsonb_text`, `key_enc`/`key_enc_num` and their test files move out of
  `liasse-pg`, which depends on and re-exports them. Motivation: the extension
  must decode the stored wire (`value`, `key_wire`) with the *same* code, and
  must not link `postgres`/`r2d2` into a `.so` that lives inside a PostgreSQL
  backend. The split is dependency-clean today (those modules import only
  `liasse-value`, `liasse-store`, `serde_json`) and honors "one concern per
  crate": the physical wire representation is now a named concern shared by the
  store and its in-database twin. `record_codec` (commit-op wire) stays in
  `liasse-pg` — the extension never reads the log.
- **`crates/liasse-pred`** (new): `RowPrograms` (the three faces: `admits` /
  `project` / `sort_tuple`, each taking the `CandidateSubtree`) + descriptor
  with read-set + deep-candidate build + shared computed fold + composed
  coverage admit + postcard wire + `EVAL_ABI` (§7.3–§7.4). Depends on
  `liasse-expr` (with `eval-wire`), `liasse-value`, `liasse-store` (for
  `KeyValue`/`Value` types and the `ViewProgram` trait), `liasse-ident`,
  `postcard`. No pgrx, no postgres — fully testable on the host. (The name
  predates mandate 5; it now carries programs, not just predicates — renaming
  to `liasse-prog` is a maintainer's one-word call, nothing else changes.)
- **`crates/liasse-pg-ext`** (new, the ONLY pgrx crate): `crate-type =
  ["cdylib", "rlib"]` (pgrx convention; the rlib serves `cargo pgrx test`).
  Depends on `pgrx`, `liasse-pred`, `liasse-pg-codec`, `serde_json`. Exposes
  the three five-argument evaluator faces `liasse.eval` / `liasse.eval_bool` /
  `liasse.eval_sort` (subtree decode included), `liasse.guard_depth`, and
  `liasse.abi_version` (§7.4, §7.7) and the per-backend
  decode cache; control file: `schema = liasse`, `relocatable = false`,
  `default_version` = the workspace version. Does **not** opt into the workspace
  lint set (risk 3): pgrx's generated glue is `unsafe`, and its boundary
  converts panics; the crate-level header documents the containment rules. It is
  excluded from the plain `cargo build` dev loop only in the sense that nothing
  depends on it; it builds with the workspace and its host-runnable unit tests
  (wire decode, cache) run in plain CI, while `cargo pgrx test` runs in the
  image job.

### 12.2 The image — the deployment artifact

Two-stage Dockerfile at `crates/liasse-pg-ext/Dockerfile` (validated end to end
by the §7.8 prototype, which is its template):

```dockerfile
# build: pinned rust + PGDG server headers + pinned cargo-pgrx
FROM rust:1.93-bookworm AS build
RUN … apt.postgresql.org … postgresql-server-dev-17 postgresql-17 clang libclang-dev …
RUN cargo install cargo-pgrx --version <pinned> --locked
RUN cargo pgrx init --pg17 /usr/lib/postgresql/17/bin/pg_config
COPY . /src                                  # the workspace; .dockerignore trims it
RUN cd /src && cargo pgrx package -p liasse-pg-ext \
      --pg-config /usr/lib/postgresql/17/bin/pg_config

# runtime: stock postgres + the packaged extension + initdb-time CREATE EXTENSION
FROM postgres:17-bookworm
COPY --from=build /src/target/release/liasse-pg-ext-pg17/usr/lib/postgresql/17/lib/  /usr/lib/postgresql/17/lib/
COPY --from=build /src/target/release/liasse-pg-ext-pg17/usr/share/postgresql/17/extension/ /usr/share/postgresql/17/extension/
COPY crates/liasse-pg-ext/initdb.sql /docker-entrypoint-initdb.d/10-liasse.sql
```

- `initdb.sql` creates the extension in `template1` (every later database
  inherits it) and in the default database — so `CREATE EXTENSION IF NOT
  EXISTS` at reconcile is a privilege-free no-op in this image (§7.7).
- The PG major is pinned by the image (17 today); the extension `.so` is built
  against the same PGDG minor the runtime stage ships. A PG major bump is an
  image change gated by the whole suite, never an in-place surprise.
- Tags: `liasse-postgres:<workspace-version>` plus the git SHA as an OCI label;
  `latest` only from `main`. The image is what production deploys and what CI
  tests — one artifact, one lock (§7.7).

### 12.3 Store-side integration

`reconcile` (§4.4, §7.7) prepends, inside the same open transaction where
possible and with actionable refusals where not:

1. `CREATE EXTENSION IF NOT EXISTS liasse` — no-op in the image; refusal with
   the §7.7 message when absent and uncreatable.
2. `ALTER EXTENSION liasse UPDATE` when `pg_available_extensions` shows a newer
   packaged default than the installed version — the self-reconciling lifecycle
   applied to the extension.
3. `SELECT liasse.abi_version()` compared to `liasse_pred::EVAL_ABI` — refuse on
   mismatch, before any program is ever shipped.

`PgStoreFactory` is unchanged in shape: it still takes a DSN; what that DSN
points at is now expected to be the liasse image (or a manually provisioned
equivalent — the handshake, not the image, is the contract).

### 12.4 CI and the test harness

- **CI pipeline**: (a) the host jobs — workspace build, unit + contract tests,
  `liasse-pred` proptests — unchanged and image-free; (b) the **image job**:
  `docker build` (layer-cached; the toolchain layers change only on pin bumps),
  `cargo pgrx test -p liasse-pg-ext` in the build stage, then the full
  `liasse-pg` integration suite — conformance corpus, 0-divergence parity,
  EXPLAIN gates (1)–(12), refusal gates — pointed at a container of the freshly
  built image. Same commit builds the binaries and the image, closing the
  version lock in CI (§7.7).
- **Harness** (`tests/support/mod.rs`): the resolution order grows one rung —
  (1) `LIASSE_PG_TEST_DSN` (must reach a PG satisfying the handshake; loud
  failure otherwise, as today); (2) the default local socket, same condition;
  (3) **new**: `docker run` of `LIASSE_PG_IMAGE` (default: the locally built
  tag) with a private port, health-waited, torn down by the last `PgHandle`
  drop exactly like today's disposable cluster; (4) the `initdb` disposable
  cluster — which now serves only extension-free development and fails loudly
  at reconcile (§7.7's actionable message) for anything touching
  `scan_view`. No suite ever silently skips (AGENTS.md).
- **The near-raw-overhead gate keeps its meaning** (§9): contract read vs the
  identical hand-written SQL on the same pooled connection of the same
  extension-equipped PG — for evaluated reads the hand-written SQL includes the
  same extension-face calls, so the gate isolates backend overhead from
  interpreter cost, and the separate eval-cost bench axis (§9) tracks the
  interpreter itself.
