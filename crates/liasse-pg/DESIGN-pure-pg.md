# DESIGN — Pure-PostgreSQL `liasse-pg` (no in-memory projection)

Status: **design, not implemented**. Mandates, in force together:

1. *"IN MEMORY PROJECTION IS FORBIDDEN IN PG BACKEND. PG backend must be pure PG."*
   Every contract read must be served by a PostgreSQL query; the backend may hold
   **no** in-memory read model of durable state.
2. *"IF PERSISTED → POSTGRES ONLY. IF SESSION-RELATIVE → IN RUST SESSION CODE."*
   Persisted/durable state (rows, the tree, the `next_incarnation` counter) lives
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
   incl. durable `next_incarnation` (§6), `scan_subtree` and the §7.2 coverage
   semantics, the §12-watch treatment (§8), the parity/gate discipline (§9), and
   the phase plan structure (§10).

This document is the implementation plan an agent fleet builds from, phase by
phase. Every claim about SQL plan shape in §4 and §6 was **prototyped against
PostgreSQL 17.10** on the real v4 `nodes` DDL (`schema.rs`), populated with a
fanout-4 depth-5 recursive `companies`/`subcompanies` tree carrying real
tagged-wire values plus 40 000 noise nodes, `ANALYZE`d. The v2 §7 lowering
prototypes (operator table, `liasse_text_key` corpus) are superseded with the
machinery they validated; their adversarial corpus survives as the §9 regression
backstop. The v3 extension mechanics — a pgrx `#[pg_extern]` function pruning a
recursive CTE with an index-served plan, packaged into the two-stage Docker
image — are prototyped end to end in §7.8.

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
  and (revised, mandate 2) no `next_incarnation` field: incarnation tokens are
  allocated transactionally from the durable `instance_meta.next_incarnation`
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
the coverage read):

```rust
/// Every row of the subtree rooted at `root` (excluding `root` itself), i.e. all
/// rows whose address strictly extends `root`'s, in Annex B address order.
/// Semantics-free: no predicates; tombstoned intermediates are traversed so
/// logical orphans (§5.4) are included.
fn scan_subtree(&self, root: &RowAddress) -> Result<Vec<(RowAddress, StoredRow)>, StoreError>;
```

Phases 7–9 add the **coverage read** (mandates 3+4). Revised for the extension
architecture: the store contract carries the predicate **opaquely**, behind a
trait, so `liasse-store` stays semantics-free and gains no dependency on the
expression layer. The predicate's semantics live in ONE implementation
(`liasse-pred`, §7.3) that the in-memory store calls directly and the PG
extension calls after deserialization — v2's `RowPredicate`/`PredOperand`/
`CompareClass` store-level IR is **eliminated**.

```rust
/// A §10.5 candidate predicate, opaque to the store contract. The single
/// implementor is `liasse_pred::CandidatePredicate` (§7.3); the trait exists so
/// `liasse-store` carries predicates without depending on the expression layer.
pub trait CoveragePredicate {
    /// The admit verdict over one candidate: the stored row payload and its
    /// typed key. Truthiness is strict `Bool(true)` (§7.2); a predicate
    /// evaluation fault is an error, never a silent verdict.
    fn admits(&self, value: &Value, key: &KeyValue) -> Result<bool, PredicateFault>;
    /// The version-locked serialized form (§7.4) a pushdown backend ships to
    /// its in-database twin. MemoryStore never calls this.
    fn wire(&self) -> &[u8];
}

/// The §10.5 coverage tree under `root` through nested keyed collection
/// `field`: depth-first in Annex B key order, live rows only (a tombstone is not
/// a candidate and blocks its subtree), each DESCENDANT admitted by the
/// hereditary `where_`/`except` pair. The root row itself is NOT filtered —
/// §10.5 predicates admit candidates; the covered row is admitted by scope
/// membership (§10.3), which the caller has already resolved.
/// Rel paths are the key components under `field`, one per level.
fn scan_coverage(
    &self,
    root: &RowAddress,
    field: &str,
    where_: Option<&dyn CoveragePredicate>,
    except: Option<&dyn CoveragePredicate>,
) -> Result<Vec<(Vec<KeyValue>, StoredRow)>, StoreError>;
```

`MemoryStore` implements `scan_coverage` as a `BTreeMap` range descent calling
`admits` — **pruning in Rust, through the same evaluator**. `PgStore` ships
`wire()` into the recursive CTE of §7.6, where the extension deserializes it and
calls the same `admits` — **pruning in SQL, through the same evaluator**. A
`PredicateFault` maps to a new `StoreError::Predicate` variant, which the runtime
answers with the interpreter fallback (§7.5) so fault behavior is
interpreter-exact.

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

- **Writer**: the one `postgres::Client` the store owns (one writer per instance is
  a given). Used by: the admission SQL transaction (`commit_transition`, including
  `NodeWriter`), `alloc_incarnation` (§6.3), `put_blob`, `record_point`, and
  open-time reconcile/DDL. Unchanged otherwise.
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

1. **Single-statement reads** — `row`, `scan`, `scan_subtree`, `scan_coverage`,
   `head`, `log_from`, `point_position`, `get_blob`, `has_blob`, `definition`,
   `composition`: one SQL statement = one MVCC statement snapshot. Internally
   consistent on any pooled connection, autocommit. Nothing to pin.
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
disagree). `record_point`/`put_blob` drop their projection mirrors. The commit's
`instance_meta` update **stops writing `next_incarnation`** — the counter is
advanced at allocation time (§6.3), not at commit.

### 6.3 Incarnation allocation — durable, burn-on-allocate (revised per mandate 2)

The earlier revision of this document kept `next_incarnation: u64` as an in-memory
allocator cursor. **Overruled**: the counter is persisted state, so it lives in and
is read from Postgres only. `alloc_incarnation` becomes one autocommit statement on
the writer connection:

```sql
UPDATE {s}.instance_meta SET next_incarnation = next_incarnation + 1
WHERE id = 1 RETURNING next_incarnation - 1
```

— the token is `row-{returned}`. Staging is a pure in-memory overlay
(`transition.rs`: "Nothing touches PostgreSQL until commit"), so this statement
never runs inside an open SQL transaction; it commits by itself, immediately.

**Abort-visibility parity — why burn-on-allocate is the correct form.**
`MemoryStore::alloc_incarnation` advances its counter *at allocation time*; an
aborted staging does not roll it back, so in-process tokens are never reused
("gaps from aborted transitions are harmless; only serial positions must be
gapless" — `memory.rs`). Two durable designs were considered:

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
tokens, `SET next_incarnation = next_incarnation + $k RETURNING …` allocates the
range in one statement. The returned range is *consumed durable state* handed to
the session — not a cached read model — so it stays mandate-compatible; wire it
only if the Phase-6 benches show allocation dominating admission. The single-row
`instance_meta` update plans as an `Index Scan using instance_meta_pkey`
(prototyped; pinned under the existing single-row exemption). Prototyped end to
end: tokens `0`, `1`, `2` allocated across an interleaved `BEGIN …
FOR UPDATE … ROLLBACK` admission — the aborted transaction did not return token
`1`; monotone, no reuse.

## 7. The scope call (revised again): the REAL interpreter prunes inside PostgreSQL

### 7.1 What changed and why — v2's SQL lowering is superseded

v2 satisfied mandate 3 by compiling a statically checked predicate *fragment* to
exact-semantics SQL: a store-level `RowPredicate` IR, an operator-by-operator SQL
table (decimal-through-`numeric`, `liasse_text_key` for NUL-escaped text order, a
none-rank CASE), and a load-time restriction of §10.5 predicates to the
compilable fragment, with a proposed normative SPEC line. That design carried an
honest, permanent cost: **a second evaluator** for the fragment, an enumerated
fragment boundary users hit, and a parity obligation renewed with every operator
the fragment ever grows.

Mandate 4 removes the second evaluator instead of containing it: **link the one
interpreter into PostgreSQL.** A `pgrx` extension crate builds the same
`liasse-expr` evaluator the runtime and MemoryStore execute into a `.so` loaded
by the self-hosted PostgreSQL, exposing one SQL function the coverage CTE calls
per candidate during descent. Because it is literally the same Rust code
evaluating the same `TypedExpr` against the same truthiness contract, parity with
the MemoryStore oracle is **by construction**, and the expressiveness of a §10.5
predicate over the candidate is the full core language — field access (including
nested static structs), computed fields, arithmetic (including its error paths),
string builtins, `has`/`in`/`size`, comparisons, logic, ternary, `$key` — with
**no fragment table and no per-operator SQL**.

**Eliminated from v2, explicitly**: the predicate→SQL compiler; the
`RowPredicate`/`PredOperand`/`CompareClass` store-level IR; the §7.4 fragment
definition; the §7.5 exact-semantics operator table (and with it the `numeric`
bound, the none-rank CASE, the wire-injective-ref rule, and the
optional-struct-member exclusion); the `liasse_text_key` managed function and the
reconciler's `functions()` declared-set; the §7.7 fragment policy and its
proposed SPEC.md §10.5 fragment line (replaced by the far narrower §7.5 note).
What v2 pinned and this design keeps: the §7.2 execution semantics, the anchor-
unfiltered and tombstone-barrier rules, the CTE shape and its index-served plan
discipline, `scan_subtree` as the distinct semantics-free primitive, and the
adversarial predicate corpus (now a regression backstop, §9).

### 7.2 How §10.5 executes today — the semantics to reproduce

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
liasse-expr  (feature `predicate-wire`, new `predicate` module)
  • serde derives (postcard) on the closed set: TypedExpr/TypedKind, ExprType,
    Value, Cell/Row/RowId — version-locked, never a public wire format (§7.4)
  • candidate-dependence classification + HOIST: every maximal candidate-free
    subtree is evaluated ONCE (callback into the runtime interpreter) and
    replaced by a synthetic binding; its value ships as an env entry
  • RESIDUAL AUDIT: the hoisted tree must contain only in-PG-evaluable nodes
    (§7.5 whitelist); anything else reports a typed, span-carrying reason

liasse-pred  (new crate: the ONE candidate-predicate implementation)
  CandidatePredicate {
      expr:       hoisted TypedExpr          (candidate refs only)
      env:        Vec<(SyntheticName, Cell)> (hoisted candidate-free values)
      bind:       String                      ($bind)
      candidate:  CandidateDescriptor         (declared scalar/struct members,
                                               carried computed exprs in fold
                                               order, key arity)
  }
  admits(value: &Value, key: &KeyValue) -> Result<bool, PredicateFault>
     = build shallow candidate Row (descriptor-driven, absent ⇒ none)
       → fold carried computed exprs (the shared fixed-point fold)
       → TypedExpr::evaluate(PredEnv{env, bind→candidate}, candidate)
       → matches!(cell, Cell::Scalar(Value::Bool(true)))
  to_wire()/from_wire(): postcard; PRED_ABI: the version-lock constant (§7.7)

liasse-store
  CoveragePredicate trait (opaque; §3) + scan_coverage
     MemoryStore: BTreeMap descent calling admits()      — prunes in Rust
     PgStore:     WITH RECURSIVE calling liasse.eval()   — prunes in SQL (§7.6)

liasse-pg-ext  (new pgrx cdylib crate; §12.1)
  #[pg_extern] liasse.eval(pred, value, key_wire, env)
     = from_wire(pred ⊕ env)  [per-backend LRU-cached]
       → value_codec::decode(value), key from key_wire
       → THE SAME admits()

liasse-runtime (recursion compiler, new module beside recursion.rs)
  compile_recursive → lower each $where/$except:
     Lowered::Pushdown(CandidatePredicate)   — the live path
     Lowered::Fallback(reason)               — per §7.5 policy
  CompiledScope::materialize
     head-frontier view read ──▶ scan_coverage + project each row through $view
     non-head frontier (§19.2 replay/resume) ──▶ today's path: snapshot(frontier)
          hydration + interpreter pruning (correct by construction, off the live path)
     hoist-eval error, or StoreError::Predicate from the store ──▶ same fallback
          (reproduces interpreter behavior exactly, including which error and
          the per-candidate short-circuit timing)
     admission receiver walk (resolve_receiver) ──▶ stays interpreter-based: it
          checks ONE key path with point reads under a staged overlay the nodes
          table does not yet contain — not the perf killer, and CTE-over-nodes
          would be unsound mid-staging
```

**Hoisting rule (kept from v2, now feeding an env instead of SQL parameters).** A
maximal subexpression that does not reference `$bind`/`.` (the candidate) is
constant across the whole descent. The compiler evaluates it once in Rust — same
interpreter, same frontier state, same session env — and binds the resulting
`Cell` (scalar, row, or whole collection — e.g. an `in /admins` haystack) under a
synthetic name outside the identifier grammar (NUL-prefixed, so no source binding
can collide). This is semantics-preserving because §16.3 restricts view-position
host calls to `pure` ("same logical inputs produce the same output"), `now()` is
the fixed per-operation sample (A.5), and `uuid()` is pinned per call site per
request — one evaluation equals N. The one observable difference is error timing:
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

**What the pushdown actually saves (unchanged).** The covered subtree no longer
round-trips: pruned branches are never fetched, included rows arrive in one
statement already in depth-first order. State the `$view` projection needs
*outside* the coverage tree hydrates as today — the pushdown targets the
recursive tree, which is the unbounded part.

### 7.4 The extension function contract

One SQL function, installed by the extension into its own schema (never the
instance schema — it is shared by every instance in the database):

```sql
FUNCTION liasse.eval(pred bytea, value jsonb, key_wire jsonb, env bytea)
RETURNS boolean
IMMUTABLE STRICT PARALLEL SAFE COST 100
```

- **`pred`** — `postcard`-serialized `CandidatePredicate` *minus* its env: the
  hoisted `TypedExpr`, the bind name, and the `CandidateDescriptor`. Stable for
  the lifetime of a compiled view, so its deserialization is cached (below).
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
  candidate-free values. Separate from `pred` because it varies per *read*
  (session `$actor`, `@params`, `/`-read collections at this frontier) while
  `pred` varies only per *view* — so each blob caches at its own rate.
- **Return** — the truthiness contract verbatim:
  `matches!(result, Cell::Scalar(Value::Bool(true)))`; anything else is `false`.
  A predicate evaluation fault (an `EvalError` — e.g. division by zero on the
  candidate's values) is reported via `pgrx`'s error path as a PG error with a
  reserved SQLSTATE (`LQ001`) and the sanitized message; `PgStore` maps that
  SQLSTATE to `StoreError::Predicate`, and the runtime answers with the
  interpreter fallback (§7.3) so the surfaced error is the interpreter's own —
  including *which* candidate errors first, which SQL evaluation order does not
  promise.
- **Volatility** — `IMMUTABLE` is truthful: the function is a pure function of
  its four arguments (the interpreter is pure; every nondeterminism source was
  hoisted into `env` by construction). `STRICT` makes a NULL `value` (a
  tombstone, if the planner ever reorders around the `value IS NOT NULL`
  barrier) yield NULL — filtered, never evaluated. `PARALLEL SAFE` is truthful
  (no state beyond a per-backend cache); recursive CTEs do not parallelize
  today, so it is future-proofing, not a load-bearing claim. `COST 100` tells
  the planner this filter is expensive relative to `c.value IS NOT NULL`, so the
  cheap conditions order first.
- **Serialization** — `postcard` over feature-gated `serde` derives
  (`liasse-expr` feature `predicate-wire`; `liasse-value` feature `serde`) on
  the closed type set. This is an **internal, version-locked wire**: producer
  (runtime) and consumer (extension) are required to be the same build (§7.7
  handshake), so no cross-version stability is promised or needed, and the
  derives impose no public-format obligation. A proptest round-trip gate (§9)
  pins encode∘decode = id.
- **Per-backend decode cache** — deserializing `pred` per worktable row would be
  O(rows × |pred|). The extension keeps a small per-backend (thread-local — a PG
  backend is single-threaded; parallel workers each get their own) LRU, keyed by
  a 128-bit hash of the blob, holding deserialized `CandidatePredicate`s and env
  tables. One query passes byte-identical blobs, so every row after the first is
  a hash-lookup. This is an infrastructure cache over immutable inputs, not a
  data projection — same footing as the §11 prepared-statement cache and the
  r2d2 pool.

### 7.5 The hoisting boundary: what runs in-PG, what remains outside

With the full interpreter linked in, the boundary is no longer "which operators
compile" but "which *inputs* exist inside PostgreSQL". The lowering audit (§7.3)
enforces exactly that, over the hoisted tree:

**In-PG (the residual whitelist — the full core language over the candidate):**
`Literal`, the candidate itself (`Current`, the `$bind` binding), synthetic
hoisted bindings, `Field` chains resolving to the candidate's stored scalar
members, nested *static-struct* members, or carried computed members; `Key` (the
candidate's `$key`, from `key_wire`); `Compare` (full Annex-B `Value::cmp` — all
types, canonical decimal equality, `none` ranking, NUL-bearing text — because it
*is* `Value::cmp`); `Logic`/`Not` (interpreter short-circuit and strict
truthiness); `In` (hoisted set/collection haystacks); `Ternary`; `Arith`/`Neg`
(including error paths — a fault maps per §7.4); `Builtin` `size`/`has`/
`string.lower`/`string.upper`/`string.trim`; `Struct`/`List`/`Composite`
literals (composite-key operands). Computed fields the predicate reads are
carried in the descriptor with their own hoisted expressions and audited
recursively; their fold reproduces `fold_computed` (fault ⇒ `none`, non-scalar ⇒
skip) via the shared implementation.

**Hoisted (candidate-free, evaluated once in Rust, shipped in `env`):** literals
aside — session structurals (`$actor`/`$session`), surface `@params`, `#imports`,
`/`-reads of any collection, `now()`/`uuid()`, aggregates/views/traversals over
*other* state, and **any host-namespace call whose arguments are candidate-free**
(evaluated through the app's registered namespace, §16.3 `pure`).

**Not in-PG, by nature (candidate-DEPENDENT but needing runtime context the
extension lacks):**

1. **App-registered host calls on the candidate** — e.g.
   `geo.contains($actor.region, child.location)`. §16 namespaces are registered
   by the application at runtime (`liasse-host::Registry` holds
   `Vec<Box<dyn HostNamespace>>`); there is **no compiled-in built-in namespace
   set to bundle** — the only built-ins are the language builtins
   (`size`/`has`/`string.*`), which live in the interpreter and therefore work
   in-PG for free. Bundling app namespaces would mean every application builds
   its own PostgreSQL image linking its Rust namespaces — a designed *seam* (the
   extension crate could expose a static registration point for a downstream
   image build), not a default anyone should need.
2. **Reads through the candidate's own subtree** — the candidate's nested keyed
   collections (`size(child.subcompanies)`), aggregates over them, temporal/
   keyring/blob selectors, and meter accessors on the candidate. The interpreter
   serves these from the materialized subtree / engine indices; the shallow
   candidate the CTE row carries does not contain them. (An SPI escape — the
   extension querying `nodes` for the candidate's children mid-descent — is
   noted as a seam in §11, not designed in: it reintroduces per-row table access
   the plan gate would have to reason about.)

**Policy for these two classes — recommendation.** Same reasoning as v2 §7.7,
now over a far smaller surface: a *silent* per-view interpreter fallback would
quietly reintroduce the exact perf killer mandate 3 forbids, and under AGENTS.md
("performance is a correctness gate") that is a correctness hole. Recommended:
**load-time rejection with a rustc-like diagnostic** (offending span; why —
"this host call takes the candidate as an argument, and the storage-side
evaluator has no app namespaces"; how to fix — hoist the candidate-free part,
restructure as a stored/computed field, or filter in the surface `$view`), plus
an **explicit per-surface opt-in** (an engine-configuration escape, not a SPEC
surface) that re-admits interpreter pruning *visibly* for a view that truly
needs it. The two *dynamic* cases — non-head frontiers (§19.2) and
hoist-eval/predicate-fault errors — keep the automatic interpreter fallback:
they are correctness routes, not silent performance routes.

**Does ANY restriction remain?** For the common case, none: every predicate over
the candidate's own data — the spec's own §10.5 examples, and every plausible
ACL/tenancy/status/plan predicate, now *including* computed fields, arithmetic,
string ops, `in`-sets, nested structs, and `$key` — runs in-PG with no fragment
to fall out of. What remains restricted (by default, with the opt-in escape) is
exactly the two classes above; v2 additionally rejected candidate-side
arithmetic, `size`/`string.*`, computed fields, optional struct members,
`json`-typed and decimal-bearing-ref comparisons — all now admitted. A one-line
SPEC.md §10.5 note remains warranted (corpus-first discipline requires the
load-time rejection to trace to spec); proposed wording for the maintainer to
edit: *"A `$where`/`$except` predicate evaluates over the bound candidate row's
own stored and computed fields; candidate-independent subexpressions are
evaluated once per read. A predicate that applies a host-namespace function to a
candidate-dependent argument, or reads through the candidate's nested
collections, is a load-time error."* The v2 fragment line is withdrawn.

### 7.6 The coverage CTE calling the extension

Shape, for `$where` wire `$W`, `$except` wire `$X`, env wire `$E` (an absent
predicate drops its conjunct — default include, deny overrides):

```sql
WITH RECURSIVE cover AS (
    SELECT n.id, jsonb_build_array() AS key_path,
           ARRAY[]::bytea[] AS sort_path, n.incarnation, n.value
    FROM {s}.nodes n
    WHERE n.parent_id = (…chained InitPlan, §4.1…)
      AND n.step_name = $root_step AND n.key_enc = $root_key
      AND n.value IS NOT NULL                       -- root: live check ONLY, no predicate
  UNION ALL
    SELECT c.id, p.key_path || jsonb_build_array(c.key_wire),
           p.sort_path || c.key_enc, c.incarnation, c.value
    FROM cover p
    JOIN {s}.nodes c ON c.parent_id = p.id AND c.step_name = $field
    WHERE c.value IS NOT NULL                       -- tombstone blocks the branch (§7.2)
      AND liasse.eval($W, c.value, c.key_wire, $E) IS TRUE
      AND NOT (liasse.eval($X, c.value, c.key_wire, $E) IS TRUE)
)
SELECT key_path, incarnation, value FROM cover ORDER BY sort_path
```

- `included(c)` is realized as `(W IS TRUE) AND NOT (X IS TRUE)` — the `IS TRUE`
  wrappers collapse a STRICT-NULL (only reachable if the planner evaluates the
  call before the `value IS NOT NULL` barrier) to *excluded*, never admitted.
- **Pruning during descent**: a candidate failing `included` never enters the
  worktable, so its subtree is never joined, fetched, or decoded — the recursion
  itself is the pruning, identical to v2's compiled CTE and to the §7.8 plan.
- **Plan shape**: anchor = the §4.1 chained-InitPlan `Index Scan using
  node_key_lookup`; recursive term = `Nested Loop` of `WorkTable Scan` +
  `Index Scan using node_key_lookup` with the extension calls appearing **only
  in the Filter line** — a per-worktable-row function call, never a scan source,
  so it cannot introduce a Seq Scan. This is EXPLAIN gate (11), unchanged in
  meaning from v2 (§9): anchor + recursive term index-served, no Seq Scan
  anywhere, worktable row count = included count (the pruning proof).
- Ordering: `sort_path` (arrays of memcmp-ordered `key_enc`) yields depth-first
  Annex-B order; `key_path` decodes to the per-level `KeyValue` rel path via the
  shared codec, and `incarnation`+`value` decode to the `StoredRow`.
- The recursion depth guard (§11) is shared with `scan_subtree`; a cycle in
  corrupt data is reported as corruption, not an infinite descent.

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
   predicate semantics and wire. `liasse-pred` exports a single
   `PRED_ABI: &str` — its crate version plus a wire-revision component — which
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
- **Not covered by the prototype, honestly**: linking the full `liasse-expr`
  interpreter (the demo links serde_json only — but a cdylib linking pure-Rust
  rlibs is a plain Cargo property, not a pgrx risk), the postcard wire + decode
  cache, and per-row cost at scale (a §9 bench axis). The prototype artifacts —
  Dockerfile, extension source, initdb script, demo SQL — are committed at
  `crates/liasse-pg/design-prototype/` (design collateral, not implementation)
  and are the templates §12 builds from; `docker build` + the `demo.sql` run
  reproduce every number above.

## 8. §12 live views and windowing over pure PG

**Mechanics that stay in Rust (and why) — confirmed against mandate 2.**
`watch.rs`/`window.rs` diff recomputed `ViewResult`s and slice windows over the
view's total sort order — evaluated sort tuples plus `RowId` occurrence tiebreak
(§12.2, §7.3, B.5). Those tuples come from expression evaluation (possibly via
host calls), not stored columns, so a "PG-side window" (pushing `$size`/`$anchor`
down as `LIMIT`/range) is **rejected**: the window is defined over the view order,
which PostgreSQL cannot compute. Window and frozen-gap anchor state are
*session-relative* — exactly what mandate 2 assigns to Rust session code. **No §12
window predicate needs the §7 CTE treatment**: there is no store-level predicate in
the window path to push down; the §10.5 coverage *inside* a watched view is pushed
down by §7 on every advance, because a watch always advances to the committed head
(the `frontier == head` case §7.3 serves).

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
cost. The §7 pushdown *does* reduce it for scoped recursive views: the pruned
coverage tree never leaves PostgreSQL. (c) Resumable frontiers (`init`/`patch`
replay for reconnecting clients) read old frontiers → O(history) log folds plus
Rust-side pruning by design; that is the §19.2 replay primitive working as
specified, not a regression (§7.3 "frontier scope").

## 9. Parity, gates, benchmarks

- **Parity**: `MemoryStore` stays the oracle; the `scenarios_gate_against_pg_store`
  0-divergence gate and the shared `contract_tests` battery must be green after
  *every phase* (the battery is updated once, in Phase 0, for the §3 signatures).
  `snapshot` parity is by construction (shared `Snapshot::replay`).
- **Predicate parity (revised for mandate 4)** — v2's three layers collapse to
  two, because the store-vs-store layer is now the same linked code:
  1. *Lowering parity* (runtime unit level): for a corpus of §10.5 predicates ×
     candidate rows, `CompiledRecursive::included` (the interpreter over the
     fully materialized candidate) must agree with
     `CandidatePredicate::admits` (the lowered, hoisted, shallow-candidate
     form). This is the gate on the ONLY reimplemented seam (§7.3): hoisting,
     the residual audit, descriptor-driven candidate construction, and the
     computed fold.
  2. *Store parity as a regression backstop*: `scan_coverage` on MemoryStore vs
     PgStore over the same `CandidatePredicate`. Agreement is by construction
     (same `admits`), so what this actually guards is the machinery *around* it:
     the postcard wire (also pinned directly by an encode∘decode proptest), the
     jsonb `value_codec`/`key_wire` decode inside the extension, the decode
     cache keying, the CTE's traversal semantics, and the SQLSTATE→
     `StoreError::Predicate` fault mapping (a division-by-zero predicate must
     surface, through the fallback, as the interpreter's own error on both
     stores).
  3. *Adversarial corpus — kept, verbatim, as the backstop's teeth*: decimal
     stored at three scales compared for equality (`1` = `1.0` = `1.00`); text
     carrying `U+0000` and `\` (stored and bound); an optional field in all
     three durable spellings under `==`, `!=`, and an ordering op; a hoisted
     `$actor`; `$except` overriding `$where`; a `closed` root (anchor
     unfiltered); a tombstoned intermediate (branch blocked); `child.$key`
     compares including a scale-variant decimal key; an `in` set; `has()`. Plus
     the newly admitted expressiveness: a computed field; candidate arithmetic
     including a division-by-zero fault case; `string.lower` on a NUL-bearing
     candidate field; a nested-static-struct member; an `in` over a hoisted
     `/`-collection. Expected verdicts hand-derived from Annex A.1/B —
     externally deducible, per AGENTS.md.
- **New refusal gates** (§7.7/§12): open against a database without the
  extension → actionable `StoreError`, nothing partially reconciled; open
  against a skewed ABI (simulated by installing a stub `liasse.abi_version()`
  returning a different string on a bare PG) → actionable refusal; the
  load-time diagnostic for a candidate-dependent app-host-call predicate
  (corpus case, written before the lowering lands).
- **Index gates** (`index_coverage_pg.rs`) — the gate becomes the READ gate. Keep
  (1)–(6); add, on the populated tree:
  - (7) depth-3 `row` chained-InitPlan point lookup → index-only, no Seq Scan;
  - (8) depth-3 `scan` in the §4.2 form → index-**ordered** (no Sort, no Seq Scan) —
    this pins the scalar-subquery formulation against the join formulation regressing in;
  - (9) `scan_subtree` recursive CTE → no Seq Scan anywhere; anchor and recursive
    term each use `node_key_lookup` (walk the `Recursive Union` children);
  - (10) `has_blob` EXISTS probe → index-only;
  - (11) `scan_coverage` extension CTE (an and/or/not predicate with a hoisted
    parameter) → anchor and recursive term each `Index Scan using
    node_key_lookup`, no Seq Scan; `liasse.eval` appears **only in a Filter
    line** (never a scan source); the recursive term's inner-scan loop count
    equals the included-row count (pruning-during-descent, pinned from the
    plan). This is the EXPLAIN tripwire for mandates 3+4, matching the §7.8
    prototype plan.
  - Pinned-exemption tests: single-row `instance_meta` reads (exists), the
    `alloc_incarnation` single-row UPDATE (§6.3), and the Phase-6 head fast path
    (a full-state materialization has no selective plan; assert instead that it is
    *one* statement and equals the log fold).
- **Benchmarks — the current numbers are void.** They measured `BTreeMap` reads (the
  forbidden projection). Re-run the criterion suite against pure PG with the
  overhead axis defined as *contract read vs the identical hand-written SQL on the
  same pool* (the AGENTS.md "near-raw-PostgreSQL overhead" gate — near **raw SQL**,
  not near RAM). For `scan_coverage` the "raw SQL" comparator is the §7.6 CTE
  itself run by hand on the pooled connection — the extension call is part of
  the raw cost on both sides, so the gate keeps measuring the *backend's*
  overhead, not the interpreter's. Axes: `row` at depth 1/3/5; `scan` of
  64/4 096 rows; `scan_subtree` of ~1 000 nodes; `scan_coverage` of a
  ~1 000-node tree at 10 %/50 %/90 % pruned (vs the same tree via
  `scan_subtree`+Rust pruning — the number that justifies mandate 3);
  **`liasse.eval` per-row cost** (the CTE with the extension predicate vs the
  same CTE with v2's hand-lowered native-SQL predicate from the retained
  corpus — the measured price of generality, recorded, with the decode cache on
  and off); `snapshot(head)` fast path vs log fold at 10³/10⁵ commits; `head`;
  `alloc_incarnation`; commit. Record results in the crate before closing
  Phase 6 (core axes) and Phase 9 (coverage axes).

## 10. Migration plan — every phase lands green (corpus + parity + index gates)

| Phase | Content | Exit criteria |
|---|---|---|
| **0** | Contract surgery (§3 signature table) in `liasse-store`; MemoryStore + battery + runtime/surface/testkit ripple; add `r2d2`/`r2d2_postgres`; `PgStore` gains the pool (built post-reconcile) — **reads still projection-served**; AGENTS.md pool clarification | workspace compiles; all gates green; zero behavior change |
| **1** | Leaf reads → pooled SQL: `head`, `get_blob`, `has_blob`, `point_position`, `definition`, `composition`, `log_from`; delete projection fields `blobs`, `points`, `definition`, `composition`, `head` | parity + corpus green; gate (10) added |
| **2** | `row`/`scan` → §4.1/§4.2 SQL; `PgTransition` overlays the SQL base; `NodeWriter` resolves via in-txn SQL (§6.1); commit trusts durable head and stops writing `next_incarnation` (§6.2); **`alloc_incarnation` → durable burn-on-allocate `UPDATE … RETURNING` (§6.3)**; delete `by_id`, the `new_ids` plumbing, and the projection's incarnation counter | gates (7)(8) added and green; parity green incl. abort-then-commit token scenarios |
| **3** | `snapshot` → §4.3 log fold; delete `projection.log`; **delete `projection.rs`**; gut `node_load.rs` to the address-reconstruction helper Phase 6 will reuse; `PgStore` fields = §2 exactly | grep-provable: no durable-state field on `PgStore`; reopen test still passes (now trivially) |
| **4** | §12/read-path hygiene: hydrate once per (instance, frontier), share across watches; engine read paths prefer `snapshot(head)` hydration over N live `scan`s where committed-state reads suffice | parity + corpus green; watch tests green |
| **5** | `scan_subtree`: contract + MemoryStore range impl + PG CTE + adoption in `gather_tree`/`rows_at`/`materialize_row_cell` (semantics-free hydration: admission gathers, receiver walks, non-recursive scoped views, fallback path); depth guard | gate (9) green; hydration round trips measured before/after |
| **6** | `snapshot(head)` fast path from `nodes` + tree≡log-fold equivalence test; core benchmark re-run + recorded numbers | bench report committed; overhead within gate |
| **7** | The predicate stack, Rust side: `liasse-expr` `predicate-wire` feature (serde derives, hoist + residual audit, postcard wire); **`liasse-pred`** crate (`CandidatePredicate`, `admits`, descriptor, shared computed fold, `PRED_ABI`, round-trip proptest); `liasse-store` `CoveragePredicate` trait + `StoreError::Predicate` + `scan_coverage` + MemoryStore descent impl; runtime lowering (hoist, audit, computed read-set) + §10.5 head-frontier reads served via `scan_coverage` on **both** stores (MemoryStore prunes in Rust — behavior identical, architecture in place); load-time diagnostic + SPEC.md §10.5 note (§7.5 wording, maintainer-edited) + corpus cases for the rejection and the layer-1 parity corpus (corpus first, per AGENTS.md) | parity green; lowering-parity suite green; wire round-trip proptest green; corpus rejection case red→green |
| **8** | The extension + image: **`liasse-pg-codec`** split out of `liasse-pg` (`value_codec`, `jsonb_text`, `key_enc*` + their test files; mechanical, liasse-pg re-exports); **`liasse-pg-ext`** pgrx cdylib (`liasse.eval`, `liasse.abi_version`, decode cache, lint carve-out §12.1); the two-stage Dockerfile + image build (§12.2, from the §7.8 template); test-harness container path + `LIASSE_PG_IMAGE` (§12.4); CI image job | extension unit tests (`cargo pgrx test`) green; image builds in CI; harness boots it; abi handshake round-trips |
| **9** | PgStore `scan_coverage` → the §7.6 CTE calling `liasse.eval`; reconcile extension step + ABI handshake + refusal gates (§7.7); fallback wiring (non-head frontier, hoist-eval error, `StoreError::Predicate`); gate (11) + §9 adversarial corpus over the extension path; coverage + per-row-cost bench axes | gate (11) green; 0-divergence on the adversarial corpus; refusal gates green; pruned-tree + eval-cost benches recorded |

Ordering rationale: reads convert one contract method at a time **behind the parity
gate**; the projection dies only when its last reader does (Phase 3); optimizations
(5, 6) come only after the pure-PG semantics are locked by the gates. The predicate
pushdown splits three ways (7, 8, 9) so that the semantics (lowering + `admits` +
MemoryStore realization) land gated **before** any PG artifact exists, the build
artifacts (codec split, extension crate, image, harness) land as pure
infrastructure with their own unit gates, and only then does PgStore switch its
coverage read onto the extension — making any Phase-9 divergence attributable to
transport (wire/jsonb/CTE), never to predicate semantics.

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
   any read. Residual risk is a wire change without a `PRED_ABI` bump slipping
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
4. **Per-row evaluation cost**: the interpreter per candidate replaces v2's
   native SQL operators per candidate — generality has a price (jsonb decode +
   tree walk per row; the pred/env decode is amortized by the §7.4 cache). The
   §9 eval-cost bench axis measures it against the v2-style hand-lowered
   predicate on the same tree, and the near-raw gate keeps the *backend*
   overhead honest. If the price is ever intolerable, the v2 lowering could
   return as a *transparent optimization* for predicates it can serve — the
   architecture (opaque `CoveragePredicate` behind the contract) leaves that
   door open without another contract change.
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
9. **Recursive-CTE cycle on corrupt data**: bounded by the shared depth guard,
   reported as corruption.
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
  derives), not a new predicate IR: a second IR would reintroduce exactly the
  lowering-divergence surface mandate 4 exists to kill. Cost: serde derives on
  internal expression types — accepted as a private, version-locked wire (§7.4),
  never a public format.
- **`key_wire` instead of the sketch's `key_enc`** as the function's key
  parameter — `key_enc` is one-way by design; the decodable identity is
  `key_wire` (§7.4).
- **Candidate-dependent app-host-calls and candidate-subtree reads: load-time
  rejection by default with an explicit per-surface interpreter opt-in** (§7.5),
  not a silent fallback; SPEC.md gets the one narrow note (maintainer words it).
  The SPI escape (extension reading the candidate's children mid-descent) and
  the app-namespaces-linked-into-a-downstream-image escape are documented seams,
  built only on demand.
- **The shallow-candidate seam** (§7.3): descriptor-driven candidate
  construction + the shared computed fold are the one reimplemented sliver,
  gated by lowering parity; judged far smaller than v2's per-operator SQL
  surface it replaces.
- **`PRED_ABI` = crate version + wire revision**, not a build-time content hash:
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
- `scan_subtree` + `scan_coverage` remain contract *extensions*; `scan_coverage`
  stays the single semantics-carrying read, but its semantics now enter through
  an opaque trait object (§3) rather than a store-defined IR — the contract
  itself is *more* semantics-free than v2's.
- The Phase-6 head fast path deliberately full-scans `nodes` (exempted, pinned).
- **Incarnation burn-on-allocate** (§6.3) — unchanged; gapless tokens would be a
  spec change, not a storage one.
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
- **`crates/liasse-pred`** (new): `CandidatePredicate` + `admits` + descriptor +
  shared computed fold + postcard wire + `PRED_ABI` (§7.3–§7.4). Depends on
  `liasse-expr` (with `predicate-wire`), `liasse-value`, `liasse-store` (for
  `KeyValue`/`Value` types and the `CoveragePredicate` trait), `liasse-ident`,
  `postcard`. No pgrx, no postgres — fully testable on the host.
- **`crates/liasse-pg-ext`** (new, the ONLY pgrx crate): `crate-type =
  ["cdylib", "rlib"]` (pgrx convention; the rlib serves `cargo pgrx test`).
  Depends on `pgrx`, `liasse-pred`, `liasse-pg-codec`, `serde_json`. Exposes
  `liasse.eval` and `liasse.abi_version` (§7.4, §7.7) and the per-backend decode
  cache; control file: `schema = liasse`, `relocatable = false`,
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
3. `SELECT liasse.abi_version()` compared to `liasse_pred::PRED_ABI` — refuse on
   mismatch, before any predicate is ever shipped.

`PgStoreFactory` is unchanged in shape: it still takes a DSN; what that DSN
points at is now expected to be the liasse image (or a manually provisioned
equivalent — the handshake, not the image, is the contract).

### 12.4 CI and the test harness

- **CI pipeline**: (a) the host jobs — workspace build, unit + contract tests,
  `liasse-pred` proptests — unchanged and image-free; (b) the **image job**:
  `docker build` (layer-cached; the toolchain layers change only on pin bumps),
  `cargo pgrx test -p liasse-pg-ext` in the build stage, then the full
  `liasse-pg` integration suite — conformance corpus, 0-divergence parity,
  EXPLAIN gates (1)–(11), refusal gates — pointed at a container of the freshly
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
  `scan_coverage`. No suite ever silently skips (AGENTS.md).
- **The near-raw-overhead gate keeps its meaning** (§9): contract read vs the
  identical hand-written SQL on the same pooled connection of the same
  extension-equipped PG — for coverage reads the hand-written SQL includes the
  same `liasse.eval` calls, so the gate isolates backend overhead from
  interpreter cost, and the separate eval-cost bench axis (§9) tracks the
  interpreter itself.
