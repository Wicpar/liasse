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
   is never fetched. This **overrules** the earlier §7 recommendation (traverse in
   PG, prune in Rust): predicates are compiled to SQL, and the oracle-fork risk is
   solved, not dodged (§7).

This document is the implementation plan an agent fleet builds from, phase by
phase. Every claim about SQL plan shape below was **prototyped against PostgreSQL
17.10** on the real v4 `nodes` DDL (`schema.rs`), populated with a fanout-4
depth-5 recursive `companies`/`subcompanies` tree carrying real tagged-wire
values (NUL-escaped text, scale-variant decimals, absent/present-none optionals)
plus 40 000 noise nodes, `ANALYZE`d. Prototype plans and result cross-checks are
reproduced verbatim in §4 and §7.6.

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
  physical addition is a **managed schema function** (`liasse_text_key`, §7.5),
  reconciled exactly like a managed index.

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

Phases 7–8 add the **coverage read** (mandate 3) — a store-level predicate IR plus
one semantics-carrying scan, both defined in `liasse-store` (which already depends
on `liasse-value`, so the IR's evaluator uses the same `Value::cmp` the runtime's
interpreter bottoms out in):

```rust
/// One operand of a §10.5 coverage predicate over a candidate row.
pub enum PredOperand {
    /// A stored (non-computed) scalar field of the candidate, through
    /// non-optional static-struct members: `child.a.b`.
    Field { path: Vec<String>, optional: bool },
    /// The candidate's own level key (`child.$key` / a ref-vs-row coercion).
    Key,
    /// A hoisted candidate-free subexpression, already evaluated to a value
    /// by the caller (a literal, `$actor.…`, `@param`, a pure host call, …).
    Const(Value),
}

/// The §10.5 predicate fragment, lowered from the checked `TypedExpr`.
/// `holds(&StoredRow, &KeyValue) -> bool` is defined HERE, once, over
/// `liasse-value` semantics (int/decimal promotion then `Value::cmp`, truthiness
/// as `matches!(v, Value::Bool(true))`) — the single truth definition both
/// backends must realize.
pub enum RowPredicate {
    Compare { op: CmpOp, class: CompareClass, lhs: PredOperand, rhs: PredOperand },
    Has(PredOperand),               // `has(child.f)` — optional presence
    All(Vec<RowPredicate>),         // `&&` chain (truthiness-consumed)
    Any(Vec<RowPredicate>),         // `||` chain
    Not(Box<RowPredicate>),
    Truth(PredOperand),             // a bare bool operand, `Bool(true)` test
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
    where_: Option<&RowPredicate>,
    except: Option<&RowPredicate>,
) -> Result<Vec<(Vec<KeyValue>, StoredRow)>, StoreError>;
```

`MemoryStore` implements `scan_coverage` as a `BTreeMap` range descent calling
`RowPredicate::holds` — **pruning in Rust**. `PgStore` compiles the same IR into
the recursive CTE of §7 — **pruning in SQL**. The 0-divergence gate compares their
results (§9). `CompareClass` is the checker-resolved static class that picks the
SQL form (§7.5): `Text`, `Numeric` (int/decimal, promoted), `Timestamp`, `Bool`,
`Uuid`, `Date`, `Bytes`, `Enum`, `Duration`, `WireEq` (jsonb-injective tagged
forms: refs/composites free of decimal/timestamp), `KeyEnc`.

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
or a primary key already in the DDL. `Schema::indexes()` is untouched; the
reconciler grows one *function* entry (§7.5), managed by the same
create-if-needed/drop-if-orphan lifecycle as indexes.

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

## 7. The scope call (revised): reads + traversal + `$where`/`$except` pruning ALL in PG

### 7.1 What changed and why

The earlier revision recommended (A+): Postgres executes the §10.5 *traversal*
(`scan_subtree`), Rust keeps the *predicates*. Mandate 3 overrules it: A+ pulls the
whole covered subtree and then discards the pruned part in Rust — on a hierarchy
where `$where`/`$except` cut deep branches, that is exactly the performance killer
the directive names. The revised scope compiles the §10.5 predicate fragment to SQL
and evaluates it **inside** the recursive term, so a pruned candidate never enters
the worktable and its subtree is never joined, fetched, or decoded.

The earlier rejection was grounded in a real risk: §10.5 predicates are arbitrary
`TypedExpr`s, and a naive SQL translation (e.g. `value->>'f'` string compares)
diverges from the interpreter on canonical decimal equality, NUL-escaped text,
none-as-absence, and typed dispatch. The resolution is not to translate naively but
to (a) define a small store-level predicate IR whose truth is specified once in
Rust over `liasse-value` (§3), (b) compile only a statically checked fragment to
it, hoisting everything candidate-free into pre-evaluated parameters, and (c) give
every compiled operator an exact-by-construction SQL form (§7.5), cross-checked by
prototype (§7.6) and permanently by the parity gate (§9).

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

### 7.3 Architecture: who compiles, who evaluates, who falls back

```
liasse-runtime (recursion compiler, new module beside recursion.rs)
  TypedExpr ──lower──▶ Option<RowPredicate>          (None ⇒ not in fragment)
     • candidate-free subtrees: evaluated ONCE by the interpreter against the
       same frontier state + session env, folded to PredOperand::Const(Value)
     • candidate-dependent structure: must fit the §7.4 fragment

liasse-store
  RowPredicate::holds(&StoredRow, &KeyValue) -> bool  (THE truth definition:
     int/decimal promotion + Value::cmp + Bool(true) truthiness)
  scan_coverage(root, field, where_, except)
     MemoryStore: BTreeMap descent + holds()          — prunes in Rust
     PgStore:     compiled WITH RECURSIVE (§7.5–7.6)  — prunes in SQL

liasse-runtime (CompiledScope::materialize)
  head-frontier view read  ──▶ scan_coverage + project each returned row through $view
  non-head frontier (§19.2 replay/resume) ──▶ today's path: snapshot(frontier)
       hydration + interpreter pruning (correct by construction, off the live path)
  hoisted-eval error ──▶ same fallback (reproduces interpreter short-circuit
       behavior exactly: if the interpreter would not have reached the erroring
       subtree, the fallback does not either)
  admission receiver walk (resolve_receiver) ──▶ stays interpreter-based: it
       checks ONE key path with point reads under a staged overlay the nodes
       table does not yet contain — not the perf killer, and CTE-over-nodes would
       be unsound mid-staging
```

**Hoisting rule.** A maximal subexpression that does not reference `$bind`/`.`
(the candidate) is constant across the whole descent. The compiler evaluates it
once in Rust — same interpreter, same frontier state, same session env — and binds
the resulting `Value` as a parameter. This is semantics-preserving because the
fragment's host calls are `pure` (§16.3: "same logical inputs produce the same
output"; `now()` is the fixed transaction sample) — one evaluation equals N. The
one observable difference is error timing: the interpreter, short-circuiting per
candidate, might never evaluate a subtree that the hoist evaluates eagerly. So a
hoist-eval **error never surfaces**: it routes to the interpreter fallback, which
reproduces the exact per-candidate behavior, error or not. Session values
(`$actor`, `$session`, `@params`) are exactly hoisted `Const`s — the session-
relative values crossing into the query as parameters, per mandate 2.

**Frontier scope.** The `nodes` table holds head state, so the compiled CTE serves
reads at `frontier == head` — which is every live view materialization and every
§12 watch advance (a watch always advances to the committed head). Historical
frontiers (§19.2 replay, resume of a stale client) fold the log and prune in Rust,
as today: correct, off the hot path, and unavoidable without versioned rows.

**What the pushdown actually saves.** The covered subtree no longer round-trips:
pruned branches are never fetched, included rows arrive in one statement already in
depth-first order. State the `$view` projection needs *outside* the coverage tree
(root references, other collections) hydrates as today — the pushdown targets the
recursive tree, which is the unbounded part.

### 7.4 The compilable fragment — exact definition

A `$where`/`$except` predicate is compilable iff its **candidate-dependent
structure** consists only of:

1. **Candidate field access** — `child.f` / `.f` where `f` is a *stored*
   (non-computed, non-meter) scalar field of the candidate row, optionally through
   **non-optional** static-struct members (`child.a.b`). Computed fields (§5.2)
   are folded at materialization, not stored — out of fragment (statically known
   from `CompiledCollection.computed`). Optional *struct members* are out (an
   absent member errors in `eval_field` on a bare struct value but would read as
   `none` in SQL — excluded rather than risk divergence; top-level row fields are
   safe: `materialize` gives every declared field a cell, absent → `none`).
2. **`child.$key`** — the candidate's level key (also reached implicitly by the
   checker's ref-vs-row coercion), compiled onto the `key_enc` column.
3. **Comparisons** `== != < <= > >=` whose operands are (1), (2), or hoisted
   constants, with a checker-resolved `CompareClass` in: text, int/decimal,
   timestamp, bool, uuid, date, bytes, enum, duration, key, or a wire-injective
   ref/composite (one whose static type transitively contains **no** decimal and
   no timestamp — their value-column wire forms preserve scale/precision, so jsonb
   equality would be finer than Annex-B equality; such refs fall out of fragment).
   `json`-typed comparisons are out of fragment (jsonb numeric normalization vs
   `Json` `Ord` is a semantic minefield with no payoff).
4. **`has(child.f)`** — optional presence.
5. **`&& || !`** over compilable operands, and **bare `bool`** operands
   (truthiness-consumed).
6. **`in`** with a compilable needle and a hoisted set/list constant — lowered to
   the `Any`-of-`Compare` chain, inheriting each class's exact `Eq` form.

Everything else **on the candidate side** — host calls, keyring/temporal/blob
selectors, aggregates, view combinators/filters/`::` traversal, nested-collection
reads, `size`/`string.*` builtins, arithmetic — is out of fragment. (Candidate-free
subtrees may use all of it; they hoist.) Arithmetic is excluded deliberately even
where it could compile: it introduces error paths (division by zero) whose
evaluation-order semantics SQL does not share; hoisted arithmetic is fine.

### 7.5 Exact-semantics SQL, operator by operator — the crux

Ground rules the table below is built on, each traced to the implementation:

- **Truthiness**: the interpreter consumes every boolean via
  `matches!(v, Value::Bool(true))` (`recursion.rs::predicate`, `eval_logic`,
  `eval_not`). SQL form: wrap every consumption point as `(e) IS TRUE`, so SQL
  `NULL` (= `none`) collapses to `false` exactly like the interpreter. `&&` →
  `(a IS TRUE) AND (b IS TRUE)`; `||` → `… OR …`; `!e` → `NOT (e IS TRUE)`
  (note: `!none` must be **true** — `NOT (NULL IS TRUE)` is true; a naive
  `NOT e` would yield `NULL` → false. Divergence killed by construction).
  Short-circuit differences are unobservable: every compiled form below is total
  (no cast can fail on codec-produced payloads, no division exists in-fragment).
- **`none` is absence, and MAX**: a candidate field that is `none` reaches SQL as
  `NULL` under *both* durable spellings — an absent struct member (extraction
  finds no pair) and a present `{"none":true}` (the typed payload projection
  `->'s'`/`->>'d'`/… of the `none` tag is `NULL`). `Value::cmp` makes `none`
  equal to `none` and **greater than every present value** (`rank = u8::MAX`,
  B.2 "present values ascending, then none").
- **Field extraction** (the compiled `child.f`): the stored row value is the
  tagged wire form `{"st": [[name, tagged], …]}` (`value_codec::encode_struct`,
  names in `BTreeMap` order, NUL-escaped by `jsonb_text`):

  ```sql
  fld(f) := (SELECT p.pair->1
             FROM jsonb_array_elements(c.value->'st') AS p(pair)
             WHERE p.pair->>0 = $f LIMIT 1)          -- $f = escape(field name)
  ```

  Nested static structs chain the same form over the member's `->'st'` payload.
  This is a per-candidate SubPlan over the already-fetched row value — no table
  access, so it cannot introduce a Seq Scan (verified in the §7.6 plan).
- **Parameters**: every hoisted `Const(Value)` is encoded by the *same* codecs the
  stored side used — `jsonb_text::escape` for text, canonical text for numerics,
  `key_enc::encode_key_value` for keys — so both sides of every comparison live in
  the same encoding.

| Class | `==` / `!=` | `<` `<=` `>` `>=` | Why it is exact |
|---|---|---|---|
| `text` | `fld->>'s' IS [NOT] DISTINCT FROM $p` ($p escaped) | `liasse_text_key(fld->>'s') <op> liasse_text_key($p)` (+ none-rank CASE if optional) | the escape is a bijection ⇒ escaped equality ⇔ decoded equality; ordering must be over **decoded** text — `liasse_text_key` (below) emits the decoded UTF-8 bytes as `BYTEA` (which *can* hold `0x00`), and UTF-8 byte order = Unicode scalar order = `Text::Ord` |
| `int`, `decimal` (promoted) | `(fld->>'i'│'d')::numeric IS [NOT] DISTINCT FROM ($p)::numeric` | `::numeric` compare (+ none-rank CASE) | canonical payload text always casts; PG `numeric` compares by value ⇒ `1.0 = 1.00 = 1` (Annex A.1 canonical equality) and mixed int/decimal promotes exactly like `eval_compare`'s `to_big_decimal`. Documented bound: values beyond PG `numeric` limits (>131 072 int digits / >16 383 frac digits) error as `StoreError::Backend` rather than diverge silently (§11) |
| `timestamp` | on `nanos(x) := (x->'ts'->>0)::numeric * CASE x->'ts'->>1 WHEN 'seconds' THEN 1e9 WHEN 'millis' THEN 1e6 WHEN 'micros' THEN 1e3 ELSE 1 END` — `IS [NOT] DISTINCT FROM` | same `nanos` compare | the value column preserves count/precision incarnation; B.1 equality is after exact-precision normalization — common-scale nanos as `numeric` (i128 counts fit) reproduces `Timestamp::cmp` exactly |
| `bool` | `(fld->>'b')::boolean IS [NOT] DISTINCT FROM $p` | (`false < true` via boolean compare; rare) | payload is jsonb true/false; bare-operand truthiness is `(fld->>'b')::boolean IS TRUE` |
| `uuid` | canonical-text `IS [NOT] DISTINCT FROM` | text compare `COLLATE "C"` | canonical lowercase-hyphenated hex is injective and fixed-width ⇒ byte order = 16-byte order = `Uuid::Ord` |
| `date` | canonical-text `IS [NOT] DISTINCT FROM` | on `datekey(t) := y*10000 + m*100 + d` parsed by `regexp_match(t, '^(-?\d+)-(\d{2})-(\d{2})$')` | canonical `YYYY-MM-DD` is injective (Eq safe); plain text order breaks for signed years (−9999..9999), the numeric key is monotone over (y, m, d) = `Date::Ord` |
| `bytes` | base64-text `IS [NOT] DISTINCT FROM` | `decode(fld->>'y', 'base64') <op> …` | canonical padded base64 is injective (Eq safe); base64 alphabet is not order-preserving, `BYTEA` memcmp of the decoded bytes = `Bytes::Ord` |
| `enum` | `fld->'enum' IS [NOT] DISTINCT FROM $p::jsonb` (the `[ordinal, label]` pair) | `(fld->'enum'->>0)::numeric` | within one declared enum type ordinal↔label is a bijection; `EnumValue::Ord` is ordinal-first (labels escaped identically on both sides) |
| `duration` | `(fld->>'dur')::numeric IS [NOT] DISTINCT FROM …` | same | nanos `i128` as canonical text ⇒ numeric compare = `Duration::Ord` |
| wire-injective `ref`/composite | jsonb equality of the tagged form (`fld->'ref' IS [NOT] DISTINCT FROM $p::jsonb`) | out of fragment | injective only when the key type has no decimal/timestamp (enforced statically, §7.4); ref-vs-key compares the inner tagged key per `ref_target_key` |
| `$key` | `c.key_enc IS [NOT] DISTINCT FROM $p_enc` | `c.key_enc <op> $p_enc` | `key_enc` is proptest-gated to `sign(memcmp) == sign(cmp)` and `Equal ⇔ byte-identical`, with Annex-B canonicalization (decimal scale, timestamp precision) built in — exact for equality *and* ordering, and it is the index column |
| `has(child.f)` | `fld IS NOT NULL AND fld <> '{"none":true}'::jsonb` — equivalently `fld->'…tag…' IS NOT NULL`; compile as `NOT (fld IS NULL OR fld = '{"none":true}'::jsonb)` | — | presence ⇔ not `none` under either durable spelling |

**None-rank CASE** (ordering with a statically `optional` operand): `none` is the
maximum, and `none` vs `none` is `Equal`, so for `a <op> b` with either side
possibly `NULL`:

```sql
CASE WHEN a IS NULL AND b IS NULL THEN {Eq-verdict of op}     -- ≤,≥ → true; <,> → false
     WHEN a IS NULL THEN {op-verdict of Greater}              -- a = none = MAX
     WHEN b IS NULL THEN {op-verdict of Less}
     ELSE {class compare} END
```

— all three verdicts are compile-time constants per operator. Equality never needs
the CASE: `IS [NOT] DISTINCT FROM` already realizes `None == None → true`,
`None == present → false`.

**`liasse_text_key`** — the one new schema object, an `IMMUTABLE STRICT` SQL
function managed by the reconciler exactly like an index (created on open, dropped
when no longer declared; the reconciler's declared-set grows a `functions()`
alongside `indexes()`):

```sql
CREATE FUNCTION {s}.liasse_text_key(e text) RETURNS bytea
LANGUAGE sql IMMUTABLE STRICT AS $$
  SELECT CASE
    WHEN strpos(e, '\') = 0 THEN convert_to(e, 'UTF8')       -- fast path: no escapes
    ELSE (SELECT COALESCE(string_agg(
            CASE r.m[1] WHEN '\\' THEN '\x5c'::bytea
                        WHEN '\0' THEN '\x00'::bytea
                        ELSE convert_to(r.m[1], 'UTF8') END,
            ''::bytea ORDER BY r.ord), ''::bytea)
          FROM regexp_matches(e, '\\\\|\\0|[^\\]+|\\', 'g') WITH ORDINALITY AS r(m, ord))
  END
$$;
```

The regexp tokenizes the escaped text left-to-right (`\\`, `\0`, plain runs, and a
lenient lone trailing `\` mirroring `jsonb_text::unescape`'s totality); each token
maps to its decoded bytes. Prototyped (§7.6): over an 11-string adversarial corpus
(`""`, bare NUL, bare `\`, `a\0…` variants, `a\…` variants) the key order equals
the hand-computed decoded scalar order exactly, while the naive escaped-form
`COLLATE "C"` order misplaces 3 of 11 — the naive compare is proven wrong, the key
proven necessary. Sequential `replace()` chains cannot decode correctly (escape
tokens overlap: `a\\0` vs `a\0`); the single-pass regexp is the correct form. A
plain decode-to-`text` is impossible in PostgreSQL — a `text` value cannot hold
`U+0000`, which is the reason `jsonb_text` exists — so `BYTEA` is not a choice but
the only correct target.

### 7.6 The compiled coverage CTE — prototyped end to end

Shape (for `$where` compiled to `W(c)`, `$except` to `X(c)`; `included(c) :=
(W(c) IS TRUE) AND NOT (X(c) IS TRUE)` with absent predicates dropping their
conjunct — default include, deny overrides):

```sql
WITH RECURSIVE cover AS (
    SELECT n.id, jsonb_build_array() AS key_path, ARRAY[]::bytea[] AS sort_path, n.value
    FROM {s}.nodes n
    WHERE n.parent_id = (…chained InitPlan, §4.1…)
      AND n.step_name = $root_step AND n.key_enc = $root_key
      AND n.value IS NOT NULL                       -- root: live check ONLY, no predicate
  UNION ALL
    SELECT c.id, p.key_path || jsonb_build_array(c.key_wire),
           p.sort_path || c.key_enc, c.value
    FROM cover p
    JOIN {s}.nodes c ON c.parent_id = p.id AND c.step_name = $field
    WHERE c.value IS NOT NULL                       -- tombstone blocks the branch (§7.2)
      AND included(c)                               -- hereditary pruning, during descent
)
SELECT key_path, value FROM cover ORDER BY sort_path
```

(`ORDER BY sort_path` — arrays of memcmp-ordered `key_enc` — yields depth-first
Annex-B order; Rust may instead sort on decoded rel-paths, dropping the Sort node.)

**Result parity, prototyped.** PostgreSQL 17.10, real v4 DDL + `node_key_lookup`,
341-node tree (values via `value_codec` tagged wire + `jsonb_text` escapes) +
40 000 noise rows, `ANALYZE`d. Seven compiled predicates ran against an
independent *decoded oracle*: a parallel table holding each row's decoded field
values in native SQL types (`NUMERIC` balance, `BYTEA` NUL-bearing tag,
`NULL`-collapsed optional), populated by the same generator — i.e. the
interpreter's view of the data — with the reference recursion evaluated over it:

| Predicate (compiled form under test) | compiled | oracle | missing | extra |
|---|---|---|---|---|
| P1 `child.status != 'closed'` (text Ne) | 222 | 222 | 0 | 0 |
| P2 `child.tag == "a\0b"` (NUL-escaped text Eq, escaped param) | 2 | 2 | 0 | 0 |
| P3 `child.balance == 1.0` (stored `'1'`/`'1.0'`/`'1.00'` all match; `'2.5'` not) | 121 | 121 | 0 | 0 |
| P4 `child.owner == $actor` (hoisted session param) | 5 | 5 | 0 | 0 |
| P5 `status != 'closed' && (balance == 1.0 \|\| !(owner == $actor))` | 181 | 181 | 0 | 0 |
| P6 `child.plan != 'basic'` (optional: absent AND present-`{"none":true}` both included — `none != text` is **true** under `Value::cmp`) | 49 | 49 | 0 | 0 |
| P7 `$except: child.status == 'closed'` (deny-list compilation) | 222 | 222 | 0 | 0 |

Zero divergence on every axis the earlier design feared: decimal scale, NUL
escapes, none-as-absence (both durable spellings), bound session values, deny-list
override, unfiltered root.

**Plan, verbatim** (P5, the full and/or/not combination):

```
Sort (actual … rows=181)
  Sort Key: cover.sort_path
  CTE cover
    ->  Recursive Union (actual … rows=181)
          ->  Index Scan using node_key_lookup on nodes n (rows=1)
                Index Cond: ((parent_id = 0) AND (step_name = 'companies') AND (key_enc = '\x0001'))
                Filter: (value IS NOT NULL)
          ->  Nested Loop (actual … rows=36 loops=5)
                ->  WorkTable Scan on cover p (rows=36 loops=5)
                ->  Index Scan using node_key_lookup on nodes c (loops=181)
                      Index Cond: ((parent_id = p.id) AND (step_name = 'subcompanies'))
                      Filter: ((value IS NOT NULL) AND ((SubPlan 1 …) IS TRUE) AND …)
                      SubPlan 1 -> Function Scan on jsonb_array_elements (loops=232)
                      SubPlan 2 -> … (loops=195)
                      SubPlan 3 -> … (loops=49)
Execution Time: 1.833 ms
```

Anchor and recursive term are both `Index Scan using node_key_lookup`; **no Seq
Scan anywhere** (the Function Scans are per-row jsonb extraction over the in-hand
value, not table access). Hereditary pruning is visible in the plan itself: the
worktable carries 181 rows — only survivors are re-joined, so the 160 pruned
nodes' subtrees were never probed (`loops=181`, not 341). Even SQL's non-guaranteed
evaluation order behaves: SubPlan 2/3 loop counts (195/49 < 232) show the AND/OR
collapsing early, and since every compiled form is total, order can only affect
speed, never truth.

### 7.7 Fragment boundary — decision and recommendation

Three candidate policies for a `$where`/`$except` outside the fragment:

- **(a) Silent Rust fallback per view** — maximal expressiveness, but one predicate
  quietly re-introduces the exact perf killer the mandate forbids, invisibly. Under
  AGENTS.md ("`liasse-pg` performance is a correctness gate") a silent per-view
  regression is a correctness hole, not a degradation. Rejected as the *general*
  mechanism.
- **(b) Restrict §10.5 to the fragment at load** — a static check where
  `compile_recursive` already validates predicates, emitting a rustc-like
  diagnostic (offending subexpression span, why it cannot push down, what to
  rewrite — e.g. "hoist the host call out of the candidate comparison, or filter
  in the surface `$view`"), plus **one normative line in SPEC.md §10.5**, e.g.:
  *"A `$where`/`$except` predicate MUST be a storage-executable candidate
  predicate: comparisons, logic, and presence tests over the candidate's stored
  scalar fields and key, with any candidate-independent subexpression evaluated
  once per read; a predicate outside this fragment is a load-time error."*
  Reachability check: predicates are role-scoped, checker-typed `bool` over one
  candidate — the spec's own §10.5 examples (`child.plan != 'closed'`,
  `child.id == 'hr'`) and every plausible ACL/tenancy/status predicate sit inside
  the fragment; candidate-free subtrees (where host calls and `/`-reads
  legitimately appear) stay unrestricted via hoisting.
- **(c) Hybrid** — (b)'s static guarantee for the common path, with the
  interpreter fallback retained for the two *dynamic* cases that no static rule
  removes: reads at a non-head frontier (§19.2 replay — the log-fold path is the
  semantics there anyway) and a hoisted-parameter evaluation error (rare; the
  fallback reproduces exact interpreter behavior including short-circuit).

**Recommendation: (c), with (b)'s SPEC note landed in the same phase.** Pruning is
*always* in PG on the live path — guaranteed statically, not hoped — while
expressiveness lost is limited to candidate-side host calls/aggregates/traversals,
which the diagnostic teaches the author to restructure. The corpus keeps a case
asserting the load-time rejection (per AGENTS.md, the SPEC edit and corpus case
precede the implementation). If the maintainer prefers full expressiveness later,
policy (a) can be re-admitted per-view behind an explicit opt-in — the fallback
machinery exists regardless for the two dynamic cases.

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
- **Predicate parity is three-layered** (new, Phases 7–8):
  1. *Lowering parity* (runtime unit level): for a corpus of §10.5 predicates ×
     candidate rows, `CompiledRecursive::included` (interpreter) must agree with
     `RowPredicate::holds` on the lowered IR — catches compiler bugs independently
     of any store.
  2. *Store parity* (the gate): `scan_coverage` on MemoryStore (Rust pruning) vs
     PgStore (SQL pruning) over the **same IR** — the mandate's 0-divergence
     comparison, run inside `scenarios_gate_against_pg_store` and a dedicated
     redteam-style suite.
  3. *Adversarial coverage*: the gate corpus MUST include, at minimum — a decimal
     field stored at three scales compared for equality (`1` = `1.0` = `1.00`);
     text carrying `U+0000` and `\` (both as stored values and as bound
     parameters); an optional field exercised in all three durable spellings
     (absent member, present `{"none":true}` — writable through the contract
     directly even if the runtime normalizes — and present value), under `==`,
     `!=`, and an ordering op (the none-rank CASE); a hoisted `$actor` parameter;
     `$except` overriding `$where`; a `closed` root (must still appear — anchor
     unfiltered); a tombstoned intermediate (branch blocked); `child.$key`
     compares including a scale-variant decimal key (key_enc canonicalization);
     an `in` set; `has()`. Each case's expected verdict is hand-derived from
     Annex A.1/B — externally deducible, per AGENTS.md.
- **Index gates** (`index_coverage_pg.rs`) — the gate becomes the READ gate. Keep
  (1)–(6); add, on the populated tree:
  - (7) depth-3 `row` chained-InitPlan point lookup → index-only, no Seq Scan;
  - (8) depth-3 `scan` in the §4.2 form → index-**ordered** (no Sort, no Seq Scan) —
    this pins the scalar-subquery formulation against the join formulation regressing in;
  - (9) `scan_subtree` recursive CTE → no Seq Scan anywhere; anchor and recursive
    term each use `node_key_lookup` (walk the `Recursive Union` children);
  - (10) `has_blob` EXISTS probe → index-only;
  - (11) `scan_coverage` compiled CTE (a P5-class predicate) → anchor and
    recursive term each `Index Scan using node_key_lookup`, no Seq Scan; the
    predicate appears only as a Filter/SubPlan (never a scan source). This is the
    EXPLAIN tripwire for mandate 3.
  - Pinned-exemption tests: single-row `instance_meta` reads (exists), the
    `alloc_incarnation` single-row UPDATE (§6.3), and the Phase-6 head fast path
    (a full-state materialization has no selective plan; assert instead that it is
    *one* statement and equals the log fold).
- **Benchmarks — the current numbers are void.** They measured `BTreeMap` reads (the
  forbidden projection). Re-run the criterion suite against pure PG with the
  overhead axis defined as *contract read vs the identical hand-written SQL on the
  same pool* (the AGENTS.md "near-raw-PostgreSQL overhead" gate — near **raw SQL**,
  not near RAM). Axes: `row` at depth 1/3/5; `scan` of 64/4 096 rows; `scan_subtree`
  of ~1 000 nodes; `scan_coverage` of a ~1 000-node tree at 10 %/50 %/90 % pruned
  (vs the same tree via `scan_subtree`+Rust pruning — the number that justifies
  mandate 3); `snapshot(head)` fast path vs log fold at 10³/10⁵ commits; `head`;
  `alloc_incarnation`; commit (dual-write projection upkeep is gone, but each
  staged insert now carries one allocation round trip — measure both directions).
  Record results in the crate before closing Phase 6.

## 10. Migration plan — every phase lands green (corpus + parity + index gates)

| Phase | Content | Exit criteria |
|---|---|---|
| **0** | Contract surgery (§3 signature table) in `liasse-store`; MemoryStore + battery + runtime/surface/testkit ripple; add `r2d2`/`r2d2_postgres`; `PgStore` gains the pool (built post-reconcile) — **reads still projection-served**; AGENTS.md pool clarification | workspace compiles; all gates green; zero behavior change |
| **1** | Leaf reads → pooled SQL: `head`, `get_blob`, `has_blob`, `point_position`, `definition`, `composition`, `log_from`; delete projection fields `blobs`, `points`, `definition`, `composition`, `head` | parity + corpus green; gate (10) added |
| **2** | `row`/`scan` → §4.1/§4.2 SQL; `PgTransition` overlays the SQL base; `NodeWriter` resolves via in-txn SQL (§6.1); commit trusts durable head and stops writing `next_incarnation` (§6.2); **`alloc_incarnation` → durable burn-on-allocate `UPDATE … RETURNING` (§6.3)**; delete `by_id`, the `new_ids` plumbing, and the projection's incarnation counter | gates (7)(8) added and green; parity green incl. abort-then-commit token scenarios |
| **3** | `snapshot` → §4.3 log fold; delete `projection.log`; **delete `projection.rs`**; gut `node_load.rs` to the address-reconstruction helper Phase 6 will reuse; `PgStore` fields = §2 exactly | grep-provable: no durable-state field on `PgStore`; reopen test still passes (now trivially) |
| **4** | §12/read-path hygiene: hydrate once per (instance, frontier), share across watches; engine read paths prefer `snapshot(head)` hydration over N live `scan`s where committed-state reads suffice | parity + corpus green; watch tests green |
| **5** | `scan_subtree`: contract + MemoryStore range impl + PG CTE + adoption in `gather_tree`/`rows_at`/`materialize_row_cell` (semantics-free hydration: admission gathers, receiver walks, non-recursive scoped views, fallback path); depth guard | gate (9) green; hydration round trips measured before/after |
| **6** | `snapshot(head)` fast path from `nodes` + tree≡log-fold equivalence test; full benchmark re-run + recorded numbers | bench report committed; overhead within gate |
| **7** | Predicate IR (`RowPredicate` + `holds` in `liasse-store`, §3) + `scan_coverage` contract read + MemoryStore descent impl + runtime lowering compiler (fragment check §7.4, hoisting §7.3) + lowering-parity unit suite; §10.5 head-frontier reads served via `scan_coverage` on **both** stores (MemoryStore prunes in Rust — behavior identical, architecture in place); SPEC.md §10.5 normative fragment line + load-time diagnostic + corpus case for the rejection (corpus first, per AGENTS.md) | parity green; lowering-parity suite green; corpus case for load rejection red→green |
| **8** | PgStore `scan_coverage` → compiled `WITH RECURSIVE` (§7.5–7.6); `liasse_text_key` managed function + reconciler `functions()` set (create/drop lifecycle + orphan test); fallback wiring (non-head frontier, hoist-eval error); gate (11) + §9 adversarial predicate-parity corpus; `scan_coverage` bench axis | gate (11) green; 0-divergence on the adversarial corpus; pruned-tree bench recorded |

Ordering rationale: reads convert one contract method at a time **behind the parity
gate** (each phase's diff is small enough to bisect a divergence); the projection
dies only when its last reader does (Phase 3); optimizations (5, 6) come only after
the pure-PG semantics are locked by the gates; the predicate pushdown (7, 8) splits
so the IR, the compiler, and the *Rust* realization land — gated — before the SQL
realization, making any Phase-8 divergence attributable to the SQL lowering alone.

## 11. Risks and judgment calls

**Risks, hardest first**
1. **Compiler-fork drift**: the §7 pushdown intentionally creates a second
   evaluator for a fragment. Containment: the truth definition lives once in
   `RowPredicate::holds` over `liasse-value`; the fragment is closed and enumerated
   (§7.4/§7.5); three-layer parity (§9) catches lowering bugs and SQL bugs
   separately; any future fragment growth requires a new table row + gate cases,
   never ad-hoc SQL.
2. **Planner drift**: the no-Sort scan plan and the index-served recursive terms are
   plan shapes, not guarantees; a PG major version could regress them. Mitigation:
   the EXPLAIN gates ((7)–(11)) are deterministic CI tripwires.
3. **Round-trip inflation**: pure PG turns RAM reads into network reads; the
   gather-heavy runtime multiplies them. Phases 4–5 collapse the multiplier, §7
   removes the biggest single source (coverage over-fetch), and the benchmark gate
   (vs raw SQL, same trip count) keeps the *backend's* overhead honest. Per-alloc
   incarnation round trips are the one new cost (§6.3; batching seam designed).
4. **Ripple breadth** of fallible `head()`/owned `definition()` through
   engine/surface/testkit — wide but mechanical; contained in Phase 0's single commit.
5. **Prepared-statement churn**: sync `postgres::Client::query(&str)` re-prepares per
   call; per-depth and per-predicate generated SQL multiplies distinct texts. If
   benches flag it, cache prepared statements per (connection, shape) via r2d2's
   customizer — an infrastructure cache, not a data projection.
6. **Recursive-CTE cycle on corrupt data**: bounded by the depth guard (§7.2-era
   `scan_subtree` guard, reused by the coverage CTE), reported as corruption.
7. **Pool exhaustion/failure**: small pool + short reads; checkout timeout maps to
   `StoreError::Backend` (fail loud, never block forever).

**Judgment calls the mandates did not fully specify** (flagged for maintainer review)
- **Fragment policy (c)** — restrict-at-load plus a retained fallback for non-head
  frontiers and hoist-eval errors (§7.7). The SPEC.md §10.5 wording above is a
  proposal; the maintainer words the normative line.
- **Numeric bound**: predicate comparisons cast through PG `numeric`; a stored
  value beyond its limits errors (`StoreError::Backend`) instead of answering —
  documented bound, precedent: the i64 serial-position bound. The alternative
  (order-preserving text tricks for unbounded ints) was judged not worth the
  complexity for >10⁵-digit numbers.
- **Optional static-struct members** are out of fragment (interpreter `eval_field`
  errors on an absent bare-struct member where SQL would read `none` — excluded
  conservatively rather than pinning that corner, §7.4).
- **`liasse_text_key` as a managed schema function** — the reconciler grows a
  declared-functions set (create-if-needed, drop-orphans), same lifecycle as
  indexes; the inline-expression alternative avoids reconciler growth but bloats
  every compiled text-ordering site. Function chosen.
- `snapshot()` still returns the materialized `Snapshot` value — a session-relative
  computed result under mandate 2, not a read model.
- `scan_subtree` + `scan_coverage` are contract *extensions* (both stores + runtime
  adoption) — the mandates ordered pure-PG reads and PG-side pruning; these are the
  minimal cuts that deliver both. `scan_subtree` remains the semantics-free
  hydration primitive; `scan_coverage` is deliberately semantics-carrying, with its
  semantics defined store-side (`RowPredicate::holds`) so "semantics-free contract"
  degrades in one named, gated place rather than diffusely.
- The Phase-6 head fast path deliberately full-scans `nodes` (exempted, pinned) —
  a full-state materialization has no selective plan.
- **Incarnation burn-on-allocate** (§6.3) makes aborted-staging token burn durable —
  observable only as *larger gaps* in an opaque token space, matching the oracle
  in-process and strictly more monotone across reopens than the old design. If the
  maintainer ever wants gapless tokens, that is a spec change, not a storage one.
- AGENTS.md's interior-mutability rule vs the pooled reads: resolved by maintainer
  directive; Phase 0 lands the clarifying AGENTS.md sentence so rule and code agree.
