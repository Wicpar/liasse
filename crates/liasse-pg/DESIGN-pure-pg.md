# DESIGN — Pure-PostgreSQL `liasse-pg` (no in-memory projection)

Status: **design, not implemented**. Mandate: *"IN MEMORY PROJECTION IS FORBIDDEN IN
PG BACKEND. PG backend must be pure PG."* Every contract read must be served by a
PostgreSQL query; the backend may hold **no** in-memory read model of durable state.

This document is the implementation plan an agent fleet builds from, phase by phase.
Every claim about SQL plan shape below was **prototyped against PostgreSQL 17.10** on
the real v4 `nodes` DDL (`schema.rs`), populated with a fanout-4 depth-5 recursive
`companies`/`subcompanies` tree (1 365 nodes incl. tombstones) plus 40 000 noise
nodes, `ANALYZE`d. Prototype plans are reproduced verbatim in §4 and §7.

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
    next_incarnation: u64                                      // allocator cursor (§6.3)
}
```

- **No field holds durable state.** No row map, no log copy, no blob cache, no point
  map, no cached head/definition/composition. (`next_incarnation` is an allocator
  cursor, not a read model — reads never consult it; see the §6.3 judgment call.)
- Every `&self` read checks a connection out of `reads`, runs **one SQL statement**
  (or, for `snapshot`, one statement plus a Rust fold), and returns it.
- Every `&mut` write path (`begin`→`commit`, `put_blob`, `record_point`, open-time
  reconcile) uses `writer`, exactly as today.
- The decoupled physical schema is **kept unchanged**: the `nodes` adjacency tree with
  surrogate `id`/`parent_id`/`step_name`/`key_enc`/`key_wire`/`incarnation`/`value`,
  the `commit_log`, `history_points`, `blobs`, `instance_meta`, `schema_version`
  tables, and the `node_key_lookup` unique index. No hierarchy flattening, no
  per-collection tables, no new columns. `SCHEMA_VERSION` does **not** bump: the
  re-architecture changes only who answers reads, not what is durable.

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
query **result**, not a read model: it is derived on demand from a Postgres query and
dropped with the caller. (Judgment call — recorded in §11.)

**MemoryStore** adapts mechanically (`Ok(...)`, `.cloned()`), staying the oracle.
**Ripple**: ~28 call sites in `liasse-runtime`/`liasse-surface`/testkit gain `?` or
error mapping; `Engine::head`, `Engine::definition_source` and the few engine reads
that consume the five methods become fallible. Mechanical, wide, one commit (Phase 0).

Phase 5 adds one method (see §7 for why and the MemoryStore body):

```rust
/// Every row of the subtree rooted at `root` (excluding `root` itself), i.e. all
/// rows whose address strictly extends `root`'s, in Annex B address order.
/// Semantics-free: no predicates; tombstoned intermediates are traversed so
/// logical orphans (§5.4) are included.
fn scan_subtree(&self, root: &RowAddress) -> Result<Vec<(RowAddress, StoredRow)>, StoreError>;
```

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
or a primary key already in the DDL. `Schema::indexes()` is untouched, so the
reconciler story is untouched.

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

- **Writer**: the one `postgres::Client` the store owns (one writer per instance is
  a given). Used by: the admission SQL transaction (`commit_transition`, including
  `NodeWriter`), `put_blob`, `record_point`, and open-time reconcile/DDL. Unchanged.
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

1. **Single-statement reads** — `row`, `scan`, `scan_subtree`, `head`, `log_from`,
   `point_position`, `get_blob`, `has_blob`, `definition`, `composition`: one SQL
   statement = one MVCC statement snapshot. Internally consistent on any pooled
   connection, autocommit. Nothing to pin.
2. **Logically pinned reads** — `snapshot(frontier)`: reads only the append-only,
   immutable `commit_log` prefix `≤ frontier`. Interleaved commits append *past* the
   frontier and are invisible by construction. No SQL transaction needed. (The Phase-6
   head fast path is again a single statement over `nodes` → case 1; its
   `frontier == head` precondition is checked in the same statement's CTE by reading
   `instance_meta.head`, falling back to the log fold on mismatch.)
3. **Multi-statement sequences above the contract** — `Prospective::gather` issues
   many `scan`s expecting one coherent state. In-process, coherence is guaranteed by
   Rust exclusivity: a commit needs `&mut Engine` while a reader holds `&Engine`, so
   no commit can interleave; out-of-process writers are excluded by
   one-writer-per-instance. **Defence-in-depth seam** (designed now, wired only if the
   one-writer premise is ever relaxed): `PgStore::read_session()` checks out a pooled
   connection, opens `BEGIN READ ONLY ISOLATION LEVEL REPEATABLE READ`, serves a
   whole multi-read sequence on one MVCC snapshot, then commits and returns the
   connection. The §12 path additionally *prefers* `snapshot(head)`-based hydration
   (case 2) over N live scans — see §8.

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

### 6.2 `commit_transition` trusts the durable head

Today it locks `instance_meta.head FOR UPDATE` and cross-checks the projection.
Pure PG: the locked durable head **is** the truth; `seq = head + 1`. The
projection-divergence corruption check disappears (there is no second head to
disagree). `record_point`/`put_blob` drop their projection mirrors; nothing else
changes.

### 6.3 Incarnation allocation — judgment call

`alloc_incarnation` needs a monotone counter across transitions. Keeping
`next_incarnation: u64` in the struct (loaded from `instance_meta` at open, advanced
per alloc, persisted at commit — exactly today's behavior minus the projection
around it) is **allocator bookkeeping, not a read model**: no contract read is
served from it. The alternative — re-reading the durable counter per transition —
changes observable abort behavior (tokens reused after an aborted staging, where
MemoryStore does not reuse them in-process) and would risk parity-gate divergence.
Decision: keep the in-memory cursor; document it in `PgStore`'s docs as the one
deliberate non-durable field and why it is mandate-compatible. Flagged in §11.

## 7. The scope call: (A+) — pure-PG storage now, PG-executed *traversal* next; full (B) rejected

### 7.1 The analysis

**How the runtime actually reads.** Every admission and every view read hydrates the
whole instance state: `Prospective::gather` (admissions) recursively `scan`s every
collection and each row's nested collections (`gather_tree`); view reads call
`store.snapshot(frontier)` and rebuild a `Prospective` from it; §10.5 coverage
(`recursion.rs`) then walks the *already-materialized* nested `Cell` tree from
`materialize_row_cell` — it never queries the store during the walk. Views, filters,
sorts, aggregates, and the §10.5 recursion are evaluated by the expression
interpreter over that tree.

**Why full (B) — predicates/filters/aggregates in SQL — is rejected.** The
predicates are arbitrary `TypedExpr`s: they may call resolved *pure host namespaces*
(§16.3), read keyring snapshots, bind `$actor`/`$session` context, and rely on
Annex-B semantics (canonical decimal equality, NUL-escaped text in JSONB — a SQL
`value->>'f'` comparison would compare the *escaped* form) that PostgreSQL does not
natively reproduce. Executing them in SQL requires a Liasse-expression→SQL compiler
covering only a fragment, forking evaluation semantics into two engines whose
disagreement the 0-divergence parity gate exists to catch. That is a large,
high-risk project with modest payoff, because the runtime's evaluation model is
whole-tree hydration regardless — a pushed-down filter does not change what
`Prospective::gather` needs. (The §7.3 prototype shows the *mechanics* of hereditary
pruning in SQL work fine; the blocker is predicate semantics, not SQL.)

**What actually hurts under pure PG, and the (A+) fix.** Scope (A) alone makes
`gather_tree`/`rows_at` an N+1 round-trip storm: one `scan` per nested collection
*per row* (the depth-5 prototype tree is ~1 400 scans for one hydration). The fix is
one **semantics-free** contract primitive — `scan_subtree` (§3) — that Postgres
serves with a single index-served `WITH RECURSIVE` query and MemoryStore serves as
an ordered prefix range over its address `BTreeMap`. Postgres then **executes the
§10.5/§5.4 recursive *traversal*** (the structural descent, the expensive part);
Rust keeps executing the *predicates* (the semantic part). This is the recommended
target: **A+ = A (Phases 0–4) + `scan_subtree` (Phase 5)**.

### 7.2 `scan_subtree` — prototyped

Anchor at the root row (chained hop, §4.1), descend through **every** child level
(no `step_name` filter — any nested collection), traverse tombstones structurally,
emit only live rows, carry the relative `(step_name, key_wire)` path:

```sql
WITH RECURSIVE subtree AS (
    SELECT n.id, jsonb_build_array() AS rel_path, n.incarnation, n.value
    FROM {s}.nodes n
    WHERE n.parent_id = (…chained InitPlan for root's parent…)
      AND n.step_name = $k AND n.key_enc = $k_enc
  UNION ALL
    SELECT c.id, p.rel_path || jsonb_build_array(jsonb_build_array(c.step_name, c.key_wire)),
           c.incarnation, c.value
    FROM subtree p JOIN {s}.nodes c ON c.parent_id = p.id
    WHERE jsonb_array_length(p.rel_path) < $depth_guard
)
SELECT rel_path, incarnation, value FROM subtree
WHERE value IS NOT NULL AND jsonb_array_length(rel_path) > 0
```

Prototype results (41 365-row table): subtree of `/companies/1` = 341 nodes, 340
live + 1 traversed tombstone, and the orphan rows **under** the tombstoned mid-level
node are all reached with correct full relative paths. Plan: anchor and recursive
term are both `Index Scan using node_key_lookup` (`Nested Loop` over the WorkTable);
no Seq Scan anywhere. Ordering is done in Rust (addresses sort to Annex-B order in
the existing `BTreeMap` materialization), so the plan carries no Sort node.
`$depth_guard` bounds a hypothetical cyclic-corruption loop (mirroring `node_load`'s
chain-length check): pass a generous bound (e.g. the node count from a cheap
estimate) and report corruption if any emitted path hits it.

Adoption: `gather_tree` and `materialize`'s nested `rows_at` recursion become — per
top-level collection — one `scan` plus one `scan_subtree` per row *only when the
collection's shape declares nested collections*, or better, one `scan_subtree` per
top-level collection root… Note the primitive is rooted at a **row**; for a whole
top-level collection the runtime issues `scan` then batches `scan_subtree` per row.
If profiling still shows N round trips, a follow-up widening (`scan_subtree` rooted
at a `CollectionPath`) is the same CTE with the anchor's final `key_enc` equality
dropped — same plan shape, one call per collection. Start with the row-rooted form
(it also directly serves `materialize_row_cell` for §10.5 scoped views).

### 7.3 §10.5 coverage as `WITH RECURSIVE` — proven, and where it lands

Mandate item: show the recursive coverage view as a recursive query with hereditary
`$where`/`$except` pruning and §5.4 ancestor+local identity. Prototyped end-to-end
(SQL-expressible predicate `status != 'closed'` standing in for `$except`):

```sql
WITH RECURSIVE cover AS (
    SELECT n.id, n.key_wire, n.value,
           jsonb_build_array(n.key_wire) AS key_path, ARRAY[n.key_enc] AS sort_path
    FROM {s}.nodes n
    WHERE n.parent_id = 0 AND n.step_name = 'companies' AND n.key_enc = $1
      AND n.value IS NOT NULL AND (n.value->>'status') IS DISTINCT FROM 'closed'
  UNION ALL
    SELECT c.id, c.key_wire, c.value,
           p.key_path || jsonb_build_array(c.key_wire), p.sort_path || c.key_enc
    FROM cover p JOIN {s}.nodes c ON c.parent_id = p.id AND c.step_name = 'subcompanies'
    WHERE c.value IS NOT NULL AND (c.value->>'status') IS DISTINCT FROM 'closed'
)
SELECT key_path, value FROM cover ORDER BY sort_path;
```

Verified: hereditary pruning falls out of the recursion (a pruned node contributes
no row and is never descended into — the excluded key's whole subtree vanished; count
law held exactly: 1+3+9+27+81 = 121 of 341); `key_path` accumulates ancestor+local
identity per §5.4; `sort_path` (array of memcmp-ordered `key_enc`) yields depth-first
tree order; plan is index-served (anchor + recursive term both
`Index Scan using node_key_lookup`, no Seq Scan; the final Sort is over the 121-row
*result*, or is dropped by ordering in Rust).

**Where it lands**: this query is *not* wired into the runtime in this
re-architecture, because real `$where`/`$except` predicates are full `TypedExpr`s
(§7.1). §10.5 is served by `scan_subtree` (structural descent in PG) + Rust
pruning/projection. The prototype is recorded as the proven pattern for an *opt-in
future* fast path restricted to statically provably SQL-compilable predicates —
explicitly out of scope now.

## 8. §12 live views and windowing over pure PG

**Mechanics that stay in Rust (and why).** `watch.rs`/`window.rs` diff recomputed
`ViewResult`s and slice windows over the view's total sort order — evaluated sort
tuples plus `RowId` occurrence tiebreak (§12.2, §7.3, B.5). Those tuples come from
expression evaluation (possibly via host calls), not stored columns, so a "PG-side
window" (pushing `$size`/`$anchor` down as `LIMIT`/range) is **rejected**: the window
is defined over the view order, which PostgreSQL cannot compute. The frozen-gap
anchor semantics likewise live on evaluated tuples. No change to either file's logic.

**What changes: where each frontier's state comes from.** Per commit, per
subscription, the engine runs `store.snapshot(head)` → `Prospective::from_snapshot`
→ re-evaluate → `Watch::advance` diff. Under pure PG that snapshot is a real query.
Honest cost accounting and the mitigations, in order:

1. **Until Phase 6**, `snapshot(head)` is an O(history) log fold *per advance*. This
   is correct but the dominant §12 cost on long histories. **Phase 6's head fast
   path** makes it O(state) — one indexed/full-tree statement.
2. **Per-frontier sharing (Phase 4)**: today every watch on an instance re-hydrates
   the same frontier independently. Hydrate **once per (instance, frontier)** and
   share the `Prospective` across all subscriptions advancing to that frontier
   (surface-level change; semantically invisible — all watches read the same
   committed frontier). Turns N-subscription cost into 1 hydration + N evaluations.
3. **Commit-scoped skip (seam, phase-later)**: `log_from(prior_frontier)` yields
   exactly the committed ops between two frontiers (already SQL-served). A
   subscription whose compiled view's collection-dependency set is disjoint from the
   touched addresses can skip re-evaluation and emit a frontier-only no-op. Requires
   static dependency extraction from the compiled view — designed as a seam, not
   built now (it must be conservative: any host-call/keyring-reading view is
   "depends on everything").
4. **True incremental maintenance** (per-op delta → per-view patch without
   re-evaluation) is out of scope — it belongs with the deferred `liasse-connect`
   deliverable, and the §12.2 contract ("after applying every patch the client
   result MUST equal the authorized declared view") is exactly what makes
   recompute-and-diff the safe baseline.

**Hard parts, honestly**: (a) O(state) per commit per instance is the floor for this
architecture — acceptable while instances are modest, and the §12.3 coherence unit
(one connection) bounds it; measure in Phase 6 benches before optimizing further.
(b) The window's full-view neighbor tracking means a bounded window still needs the
full recomputed row stream, so windows do not reduce hydration cost — only wire
cost. (c) Resumable frontiers (`init`/`patch` replay for reconnecting clients) read
old frontiers → O(history) log folds by design; that is the §19.2 replay primitive
working as specified, not a regression.

## 9. Parity, gates, benchmarks

- **Parity**: `MemoryStore` stays the oracle; the `scenarios_gate_against_pg_store`
  0-divergence gate and the shared `contract_tests` battery must be green after
  *every phase* (the battery is updated once, in Phase 0, for the §3 signatures).
  `snapshot` parity is by construction (shared `Snapshot::replay`).
- **Index gates** (`index_coverage_pg.rs`) — the gate becomes the READ gate. Keep
  (1)–(6); add, on the populated tree:
  - (7) depth-3 `row` chained-InitPlan point lookup → index-only, no Seq Scan;
  - (8) depth-3 `scan` in the §4.2 form → index-**ordered** (no Sort, no Seq Scan) —
    this pins the scalar-subquery formulation against the join formulation regressing in;
  - (9) `scan_subtree` recursive CTE → no Seq Scan anywhere; anchor and recursive
    term each use `node_key_lookup` (walk the `Recursive Union` children);
  - (10) `has_blob` EXISTS probe → index-only.
  - Pinned-exemption tests: single-row `instance_meta` reads (exists) and the
    Phase-6 head fast path (new — a full-state materialization has no selective
    plan; assert instead that it is *one* statement and equals the log fold).
- **Benchmarks — the current numbers are void.** They measured `BTreeMap` reads (the
  forbidden projection). Re-run the criterion suite against pure PG with the
  overhead axis defined as *contract read vs the identical hand-written SQL on the
  same pool* (the AGENTS.md "near-raw-PostgreSQL overhead" gate — near **raw SQL**,
  not near RAM). Axes: `row` at depth 1/3/5; `scan` of 64/4 096 rows; `scan_subtree`
  of ~1 000 nodes; `snapshot(head)` fast path vs log fold at 10³/10⁵ commits;
  `head`; commit (should be *faster*: the dual-write projection upkeep is gone).
  Record results in the crate before closing Phase 6.

## 10. Migration plan — every phase lands green (corpus + parity + index gates)

| Phase | Content | Exit criteria |
|---|---|---|
| **0** | Contract surgery (§3) in `liasse-store`; MemoryStore + battery + runtime/surface/testkit ripple; add `r2d2`/`r2d2_postgres`; `PgStore` gains the pool (built post-reconcile) — **reads still projection-served**; AGENTS.md pool clarification | workspace compiles; all gates green; zero behavior change |
| **1** | Leaf reads → pooled SQL: `head`, `get_blob`, `has_blob`, `point_position`, `definition`, `composition`, `log_from`; delete projection fields `blobs`, `points`, `definition`, `composition`, `head` | parity + corpus green; gate (10) added |
| **2** | `row`/`scan` → §4.1/§4.2 SQL; `PgTransition` overlays the SQL base; `NodeWriter` resolves via in-txn SQL (§6.1); delete `by_id` and the `new_ids` plumbing; commit trusts durable head (§6.2) | gates (7)(8) added and green; parity green |
| **3** | `snapshot` → §4.3 log fold; delete `projection.log`; **delete `projection.rs`**; gut `node_load.rs` to the address-reconstruction helper Phase 6 will reuse; `PgStore` fields = §2 exactly | grep-provable: no durable-state field on `PgStore`; reopen test still passes (now trivially) |
| **4** | §12/read-path hygiene: hydrate once per (instance, frontier), share across watches; engine read paths prefer `snapshot(head)` hydration over N live `scan`s where committed-state reads suffice | parity + corpus green; watch tests green |
| **5** | `scan_subtree`: contract + MemoryStore range impl + PG CTE (§7.2) + adoption in `gather_tree`/`rows_at`/`materialize_row_cell`; depth guard | gate (9) green; hydration round trips measured before/after |
| **6** | `snapshot(head)` fast path from `nodes` + tree≡log-fold equivalence test; full benchmark re-run + recorded numbers | bench report committed; overhead within gate |

Ordering rationale: reads convert one contract method at a time **behind the parity
gate** (each phase's diff is small enough to bisect a divergence); the projection
dies only when its last reader does (Phase 3); optimizations (5, 6) come only after
the pure-PG semantics are locked by the gates.

## 11. Risks and judgment calls

**Risks, hardest first**
1. **Planner drift**: the no-Sort scan plan and the index-served recursive term are
   plan shapes, not guarantees; a PG major version could regress them. Mitigation:
   the EXPLAIN gates are deterministic CI tripwires; the §4.2 scalar-subquery form is
   pinned by gate (8).
2. **Round-trip inflation**: pure PG turns RAM reads into network reads; the
   gather-heavy runtime multiplies them. Phases 4–5 exist precisely to collapse the
   multiplier; the benchmark gate (vs raw SQL, same trip count) keeps the *backend's*
   overhead honest, and the recorded Phase-6 numbers make the *architecture's* cost
   visible rather than hidden.
3. **Ripple breadth** of fallible `head()`/owned `definition()` through
   engine/surface/testkit — wide but mechanical; contained in Phase 0's single commit.
4. **Prepared-statement churn**: sync `postgres::Client::query(&str)` re-prepares per
   call, and per-depth generated SQL multiplies distinct texts. If benches flag it,
   cache prepared statements per (connection, depth) via r2d2's customizer — an
   infrastructure cache, not a data projection.
5. **Recursive-CTE cycle on corrupt data**: bounded by the depth guard (§7.2),
   reported as corruption — mirroring `node_load`'s existing check.
6. **Pool exhaustion/failure**: small pool + short reads; checkout timeout maps to
   `StoreError::Backend` (fail loud, never block forever).

**Judgment calls the mandate did not fully specify** (flagged for maintainer review)
- `next_incarnation` stays an in-memory allocator cursor (§6.3) — reads never touch
  it; re-reading per transition would change abort-visible token allocation vs the
  oracle.
- `snapshot()` still returns the materialized `Snapshot` value — interpreted as a
  query result, not a read model; changing the contract's snapshot shape was judged
  out of mandate scope.
- `scan_subtree` is a contract *extension* (both stores + runtime adoption) — the
  mandate ordered pure-PG reads, not contract growth; it is the minimal cut that
  makes pure PG viable at depth, and Postgres executing the §10.5 *traversal* is as
  far as (B) can go without forking predicate semantics (§7.1). Full (B) is
  rejected, with the §7.3 prototype recorded as the future opt-in pattern.
- The Phase-6 head fast path deliberately full-scans `nodes` (exempted, pinned) —
  a full-state materialization has no selective plan.
- AGENTS.md's interior-mutability rule vs the pooled reads: resolved by maintainer
  directive; Phase 0 lands the clarifying AGENTS.md sentence so rule and code agree.
