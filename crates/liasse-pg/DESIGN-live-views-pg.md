# DESIGN — §12 live views as PostgreSQL engine state (Rust pilots)

Status: **design, not implemented**. This document is the revised **Phase 10**
of `DESIGN-pure-pg.md` (§10) and **supersedes that document's §8** ("§12 live
views and windowing over pure PG") in full: §8 kept the §12.2 diff, the window
partition, and the frozen-gap anchor in Rust as "session-relative" state and
implicitly rejected PostgreSQL notifications as the event basis. Both calls are
**reversed** here by maintainer directive. `DESIGN-pure-pg.md` itself is not
edited; wherever the two disagree about §12, this document wins. Everything
else in `DESIGN-pure-pg.md` — the pure-PG read path (§4), the pool (§5), the
write path (§6), the general in-PG evaluator (§7), the parity/gate discipline
(§9), the phase structure (§10) — stands and is built on, not repeated.

## 1. The binding model, read precisely

Maintainer directives, verbatim:

1. *"there should be as little back and forth as possible between the system
   and the backend."*
2. *"so please use postgres based notifications as the event basis"*
3. *"rust shouldn't even diff them, all state happens in postgres, rust only
   pilots"*
4. *"i.e. rust pilots the generic backend."*

And the maintainer's own reconciliation with `DESIGN-pure-pg.md`'s mandate 2
("session-relative → Rust"): this is **not a reversal but a
reclassification**. A subscription's retained result, its frontier, its
window/frozen-gap coordinate, and the §12.2 diff between two frontiers are now
classified **backend state and backend computation** — they live in whatever
the store is (PostgreSQL for `PgStore`, process memory for `MemoryStore`) and
are advanced by the store, never by the layer above it. What remains
Rust-resident is **client transport only**: the socket, the LISTEN handle, the
client↔subscription routing map, and the credential-bearing
auth-re-check-on-forward. Rust's only genuine *computation* is executing app
procedures inside mutation bodies (mandate 7 of `DESIGN-pure-pg.md` §7.5 —
the one framework-run context); everything read, validate, diff, and event is
the backend's.

Three consequences, drawn now so the rest of the document can be mechanism:

- **The diff moves into PostgreSQL — as the same linked code.** The §12.2
  ordered patch must match `ViewDelta::between` / `patch::diff`
  (`crates/liasse-runtime/src/{view.rs,patch.rs}`) *exactly* — it is the
  parity oracle. The design therefore does for the diff precisely what
  mandate 4 did for the evaluator: it does **not** re-derive the diff in SQL
  (a second implementation, a permanent divergence surface — the mistake v2's
  operator table already taught us); it **links the one Rust diff/window
  implementation into the extension** as a new face, `liasse.sub_advance`
  (§4). SQL supplies the inputs (stored prior, fresh `scan_view` result) and
  stores the outputs; the computation is the same function on both stores.
- **The event basis is NOTIFY/LISTEN** (§5): the commit itself announces
  "which frontier, which collections" transactionally; pilots wake instead of
  being called.
- **"Generic backend" is literal** (§7): the store contract grows
  subscription methods with the same shape discipline as `scan_view` — an
  opaque, single-implementation semantics carrier — and `MemoryStore`
  implements the identical surface in-process, so ONE pilot code path drives
  either store and the §9 parity gate has a live oracle.

Client-observable §12.1/§12.2/§12.3 behavior is **unchanged**: the same
`init`/`patch`/`close` frames, the same five ops with the same positions, the
same window semantics, the same completion barrier, the same denial shapes.
This document moves where the computation and state live, never what the
client sees; the §12 conformance corpus (`tests/12-clients-live-views/`) is
the arbiter and none of its cases is recast (§9, with one behavior note the
corpus already admits — §6.3).

## 2. What Rust keeps — the transport-only inventory, and the proof

The pilot (the re-shaped `liasse-surface` host) holds, per process:

| Datum | Why it is transport, not logical state |
|---|---|
| socket / stream handles per client connection | the wire itself |
| routing map: `(connection, watch_id) → SubscriptionId` + shape tag | addressing of frames to sockets; reconstructible from nothing (a lost map = a lost socket = the client resumes, §12.2) |
| per-connection `AuthSelection`s (authenticator name + credential) | §11.3: "the credential is retained only in transport state, never written to application state" — the one datum that MUST NOT be stored (§8.1) |
| the LISTEN handle (the events connection) | the wire to the backend |
| bounded per-socket outbound buffers | transport flow control (§6.5) |
| the virtual/wall clock it drives time with | time is an *input* to the system, not state of it (§22.5) |

Nothing else. The proof obligation "no logical §12 state leaks into Rust" is
discharged structurally: every datum the *next patch* depends on — prior
result, frontier, window parameters, frozen gap, bound `$params`, scope path,
authz coordinates (role, actor/session keys — not the credential), the
compiled dependency set — is a column of §3's tables (or `MemoryStore`'s
equivalent), and the pilot's advance call passes **no prior state in**: it
passes a `SubscriptionId` and receives a delta out. A pilot that crashes and
restarts can, given only its sockets' resume requests, reconstruct every
routing entry and continue every stream (§6.4) — which is the operational
definition of "holds transport only". Two flagged near-misses are resolved in
§8.1 (credentials stay out of PG, deliberately, with the auth split that
makes that sound) and §11 (operation records — logical §12.3 state that must
ALSO move into PG, an adjacent relocation this design includes).

What is **deleted** from Rust (no legacy, per the pre-release rule):
`crates/liasse-surface/src/watch.rs` (`Watch`, `WatchAuthz` — the retained
`last`/`windowed` results, the per-watch frontier, the close latch: all
backend state now), the advance path of `host/barrier.rs` (hydrate → re-eval →
`ViewDelta` diff), and `connection.rs`'s `watches: BTreeMap<String, Watch>`
(becomes the routing map). `window.rs`'s and `patch.rs`/`view.rs`'s diff and
window *logic* is not deleted but **relocated** into the shared crate the
extension links (§4.4). What is **kept**: `crates/liasse-wire/` in full — the
client-side fold (`WireStore`), the wire `PatchOp`/`apply` — clients are not
this design's concern; and `ViewRow`/`ViewResult`/`PatchOp` as decode/wire
types for plain (non-live) `view` reads and frame encoding. Inventory in
§10.2.

## 3. Subscription state in PostgreSQL

### 3.1 Placement: per-instance schema, alongside `nodes`

Subscription state is **per package instance**: a subscription reads one
instance's views, its frontier is that instance's `CommitSeq`, and the diff
joins against that instance's `nodes`. The tables live in the instance schema
(`schema.rs`), so `drop_instance` (a schema drop) removes them with everything
else and the reconciler's single-schema orphan sweep governs them like any
fixed table. They join `Schema::tables()` as declared fixed tables — the
self-reconciling story needs no new mechanism, only two more entries in the
enumerable desired set (and `SCHEMA_VERSION` bumps to 5; the stamp gate
already handles old databases).

### 3.2 The tables

Physical-representation rules carried over from the decoupled schema
([[pg-decoupled-physical-representation]]): surrogate keys, no
liasse-structure flattening, wire columns decoded by the shared codecs.

```sql
-- one row per open subscription (UNLOGGED — §3.3)
CREATE UNLOGGED TABLE {s}.subscriptions (
    sub_id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- the compiled read: program wires (§7.4 of DESIGN-pure-pg), stable per view
    view_addr     TEXT   NOT NULL,          -- compiled view identity (diagnostics, sharing key)
    admit_wire    BYTEA,                    -- NULL = no filter
    project_wire  BYTEA  NOT NULL,
    sort_wire     BYTEA,                    -- NULL = source key order
    fold_wire     BYTEA,                    -- NULL = row stream; else the §4.6 aggregate fold
    env_stable    BYTEA  NOT NULL,          -- hoisted env, STABLE class only (§4.7): $actor/$session/@params
    scope_wire    JSONB  NOT NULL,          -- §10.5 scope-row key path ('[]' when unscoped)
    args_wire     JSONB  NOT NULL,          -- bound $params, for sharing-key identity & resume
    bounds        JSONB  NOT NULL,          -- {skip, limit} — the surface's own §7.3 bounds
    -- authorization coordinates (NEVER the credential — §8.1)
    role_name     TEXT,                     -- NULL = public subscription
    actor_wire    JSONB,                    -- resolved $actor key (wire form)
    session_wire  JSONB,                    -- resolved $session key, when the authenticator binds one
    members_admit BYTEA,                    -- the lowered §10.3 membership check program (§8.2); NULL = public
    -- the compiled dependency set (§5.4): view deps ∪ members deps ∪ session deps
    deps          TEXT[] NOT NULL,
    -- live position
    frontier      BIGINT NOT NULL,
    shape         TEXT   NOT NULL,          -- 'rows' | 'scalar' (fixed at init, §12.2)
    scalar_wire   JSONB,                    -- retained value of a scalar subscription
    -- window (§12.2); all NULL for an unwindowed subscription
    win_size      INT,
    win_anchor    TEXT,                     -- 'first' | 'last' | 'at'
    win_anchor_occ TEXT,                    -- D.1 canonical occurrence text for 'at'
    win_slide     BOOLEAN,
    gap_sort      JSONB,                    -- frozen gap: sort tuple (wire values)…
    gap_occ       TEXT,                     -- …plus occurrence identity (§12.2, B.5)
    -- lifecycle (§3.3)
    pilot_id      TEXT   NOT NULL,
    lease_until   TIMESTAMPTZ NOT NULL
);
CREATE INDEX {s}.sub_by_pilot ON {s}.subscriptions (pilot_id, frontier);
CREATE INDEX {s}.sub_deps     ON {s}.subscriptions USING GIN (deps);

-- the retained result of a row-stream subscription: the full authorized
-- (post-$skip/$limit) view at `frontier`, one row per occurrence
CREATE UNLOGGED TABLE {s}.sub_rows (
    sub_id     BIGINT NOT NULL REFERENCES {s}.subscriptions(sub_id) ON DELETE CASCADE,
    occ        TEXT   NOT NULL,   -- D.1 canonical occurrence-identity text (§4.3)
    pos        INT    NOT NULL,   -- zero-based position in the full retained result
    win_pos    INT,               -- zero-based position in the client-visible window slice; NULL = outside
    sort_wire  JSONB  NOT NULL,   -- the evaluated $sort tuple, decodable wire values ('[]' unsorted)
    value_wire JSONB  NOT NULL,   -- the projected output struct (tagged wire form)
    PRIMARY KEY (sub_id, occ)
);
CREATE UNIQUE INDEX {s}.sub_rows_pos ON {s}.sub_rows (sub_id, pos);
```

Notes, each load-bearing:

- **The full result is retained, not just the window.** §12.2's frozen-gap
  coordinate and `$last`/slide placement are positions in the *full* view
  order; `DESIGN-pure-pg.md` §8 already established that a bounded window
  needs the full (post-`$limit`) ordered stream for neighbor tracking. The
  client-visible slice is the `win_pos IS NOT NULL` sub-rows in `win_pos`
  order; the windowed diff (§4.2) runs over exactly that slice.
- **`occ` is the D.1 canonical occurrence-identity text** — the `RowId`
  rendered canonically (§4.3). It is the join key of the diff and the `$id` of
  every wire op, so it is stored in its identity-bearing text form, not a
  surrogate. `pos` is derived state (the retained order), stored because the
  diff needs the *prior* order and positions must not be recomputed from a
  later state.
- **Program wires are per-subscription copies.** Many subscriptions of one
  view store identical wires; normalizing them into a shared `sub_programs`
  table keyed by a wire hash is a designed compression seam, not v1 — the
  wires are small (hoisting keeps them expression-sized) and the copy keeps
  open/close single-table. (Judgment call, §11.)
- **`env_stable` holds only the STABLE env class** — `$actor`, `$session`,
  `@params` values, which are fixed for the subscription's lifetime. The
  state-dependent env classes are handled per §4.7, NOT stored (stale state
  is exactly what a stored env would become).

### 3.3 Lifecycle, durability class, and GC

A subscription is ephemeral: created at `view`, gone at `close`/disconnect/
expiry. Three mechanisms, layered:

- **UNLOGGED tables.** Subscription state is *reconstructible* — a client can
  always be re-served with a fresh `init` (§12.2: "or a fresh `init` when the
  runtime has released the required range"). UNLOGGED removes WAL from the
  highest-churn write path in the system (every advance rewrites result rows)
  and gives exactly the right crash semantics: a PostgreSQL crash truncates
  the tables, every resume finds no retained stream, and every client
  re-inits — loud, correct, and §12.2-conformant. This is deliberately NOT
  application state; nothing in `commit_log`/`nodes` is UNLOGGED.
- **Pilot leases.** Every subscription carries `(pilot_id, lease_until)`. A
  live pilot re-arms its leases on a timer (one `UPDATE … WHERE pilot_id = $1`
  statement); any pilot's open/wake path opportunistically reaps expired rows
  (`DELETE … WHERE lease_until < now()` — `ON DELETE CASCADE` clears
  `sub_rows`). A crashed pilot's subscriptions therefore vanish within one
  lease period; its clients reconnect to a surviving pilot and resume (§6.4).
- **Open-time sweep.** Single-process deployments (the whole test suite, the
  embedded-library case) get a simpler guarantee for free: `reconcile` (or
  store open) deletes every subscription row of the opening pilot's id —
  a restart is a volatile-state reset (§22, `SurfaceHost::into_parts`
  semantics), and the lease machinery is the multi-pilot extension of the
  same rule, not a parallel mechanism.

The reconciler treats both tables as fixed tables (create side); there is no
new orphan class — rows are data, and the lease reaper is their GC. The §12.3
**operation-record** table (`{s}.operations` — key, request-model hash,
outcome wire, expiry) joins the same schema but **LOGGED**: at-most-once
execution must survive crashes (§12.3: a committed transition remains final;
replaying it because the dedup record evaporated would be a double
execution). Its host-policy expiry (`unknown` after expiry) is a TTL column
plus the same opportunistic reaper. This is the §11-flagged adjacent
relocation: it is §12.3 logical state and cannot stay in pilot memory under
the binding model.

## 4. The in-PG advance: one statement, same linked diff

### 4.1 Shape

Per affected subscription per commit (or temporal advance), the backend runs
**one statement** that: reads the head, recomputes the view (the §7.6
`scan_view` statement of `DESIGN-pure-pg.md`, with the sort-tuple column),
re-evaluates membership (§8.2), hands (stored prior, fresh, window state,
membership verdict) to the linked diff — `liasse.sub_advance` — and writes
back the new retained result, window coordinate, and frontier, returning the
wire delta. Sketch (flat-view source; the coverage CTE composes identically):

```sql
WITH head AS (SELECT head FROM {s}.instance_meta WHERE id = 1),
fresh AS (      -- §7.6 scan_view + position + decodable sort tuple
  SELECT (row_number() OVER (ORDER BY ord, key_enc)) - 1 AS pos,
         key_wire, ord, sort_vals, projected
  FROM (
    SELECT c.key_wire, c.key_enc,
           liasse.eval($P, c.value, c.key_wire, $E, $T)      AS projected,
           liasse.eval($SV, c.value, c.key_wire, $E, $T)     AS sort_vals,
           liasse.eval_sort($S, c.value, c.key_wire, $E, $T) AS ord
    FROM {s}.nodes c  -- … §7.6 chained-InitPlan parent, step, live filter, admit …
    ORDER BY ord, c.key_enc OFFSET $skip LIMIT $limit
  ) f
),
member AS (     -- §8.2: the lowered §10.3/§10.5 membership probe (public: TRUE)
  SELECT EXISTS ( … §7.6 statement of the members view, actor in env … ) AS ok
),
prior AS (
  SELECT COALESCE(jsonb_agg(jsonb_build_array(occ, pos, win_pos, sort_vals, value_wire)
                            ORDER BY pos), '[]'::jsonb) AS rows
  FROM {s}.sub_rows WHERE sub_id = $id
),
verdict AS (    -- THE relocated Rust advance: diff + window + scalar, one call
  SELECT liasse.sub_advance(
           s.win_state,                 -- shape, win_*, gap_*, scalar_wire (row of `subscriptions`)
           prior.rows,
           (SELECT COALESCE(jsonb_agg(jsonb_build_array(pos, key_wire, sort_vals, projected)
                                      ORDER BY pos), '[]'::jsonb) FROM fresh),
           (SELECT ok FROM member)
         ) AS out
  FROM {s}.subscriptions s, prior WHERE s.sub_id = $id
),
put AS (        -- upsert surviving + new occurrences (disjoint from `del` — §4.5)
  INSERT INTO {s}.sub_rows (sub_id, occ, pos, win_pos, sort_vals, value_wire)
  SELECT $id, r->>0, (r->>1)::int, (r->>2)::int, r->3, r->4
  FROM verdict, jsonb_array_elements(out->'retain') r
  WHERE (out->>'live')::bool
  ON CONFLICT (sub_id, occ) DO UPDATE
    SET pos = EXCLUDED.pos, win_pos = EXCLUDED.win_pos,
        sort_vals = EXCLUDED.sort_vals, value_wire = EXCLUDED.value_wire
),
del AS (        -- departed occurrences only (disjoint from `put`)
  DELETE FROM {s}.sub_rows
  WHERE sub_id = $id
    AND occ IN (SELECT jsonb_array_elements_text(out->'evict') FROM verdict)
),
bump AS (
  UPDATE {s}.subscriptions
  SET frontier = (SELECT head FROM head),
      gap_sort = v.out->'gap'->'sort', gap_occ = v.out->'gap'->>'occ',
      scalar_wire = v.out->'scalar'
  FROM verdict v
  WHERE sub_id = $id AND frontier = $prev   -- optimistic single-advancer guard (§8.4)
  RETURNING 1
)
SELECT (SELECT head FROM head) AS frontier,
       v.out->'delta'          AS delta,     -- the wire ops / scalar / close / no-op
       (SELECT count(*) FROM bump) AS bumped
FROM verdict v;
```

Facts that make this the right shape:

- **One MVCC statement snapshot = one coherent frontier** (`DESIGN-pure-pg.md`
  §5.4 case 1): the head read, the fresh view, and the membership probe all
  observe the same committed state, so the returned `frontier` is exactly the
  frontier the rows and the verdict were computed at — the §12.2 coherence
  obligation, discharged by MVCC rather than by Rust exclusivity. This is why
  the single-statement form is a *correctness* choice, not only the
  fewest-round-trips one; the multi-statement fallback (§4.5) must wrap the
  same reads in one transaction to say the same thing.
- **The recompute-and-diff floor is unchanged** — §12.2's "after applying,
  client == authorized view" makes recompute-and-diff the safe baseline
  (`DESIGN-pure-pg.md` §8, mitigation 4 — still true). What moved is
  everything around it: no rows leave PostgreSQL except the wire delta.
- **The commit-scoped skip is a separate bulk statement** (§5.4): dependency-
  disjoint subscriptions never run this statement at all.

### 4.2 What `liasse.sub_advance` computes — the relocated machine

The face is the current Rust logic, moved verbatim (§4.4), not redesigned:

- **Row-stream, unwindowed**: `patch::diff(prior, fresh)` — pass 1 removes
  departed occurrences and updates changed-value survivors
  (position-preserving); pass 2 places left-to-right, `insert{at}` for new,
  `move{to}` for out-of-place survivors, positions interpreted mid-application
  exactly as the client applies them (`patch.rs`). `update` vs `rekey`: the
  diff **never synthesizes `rekey`** — occurrence identity is key-derived
  (D.1), so an atomic rekey presents as a distinct identity and diffs as
  `remove` + `insert`, which yields the correct ordered result; `rekey`
  remains wire vocabulary for a future rekey-stable-identity layer. This is
  the pinned `patch.rs` behavior and the oracle's, so the in-PG diff matches
  by construction.
- **Row-stream, windowed**: re-select the window over the fresh full result
  (`window.rs::select`, relocated): first/last/anchored placement, `$slide`
  centering clamped to bounds, anchor located by occurrence identity; when a
  concrete anchor is present, (re)freeze the gap at its current
  `(sort tuple, occurrence)` pair; when absent, resume at the frozen
  coordinate by `partition_point` through the view's own total order
  (`SortOrder::compare` — each `$sort` key with direction, occurrence as the
  B.5 tiebreak). Then diff **the prior window slice against the refreshed
  slice** (`ViewDelta::between_rows`): positions window-relative, evictions
  as `remove` — applying the delta to the client's prior window reproduces
  the new authorized window exactly (§12.2). The full fresh result is still
  what `retain` writes back (with `win_pos` marking the slice).
- **Scalar/aggregate** (§7.5): value comparison — the new value when changed,
  the frontier-only no-op when not (`ViewDelta::between`, scalar arm). The
  fresh scalar comes from §4.6.
- **Membership false** (§8.2): `delta = close`, `live = false` — no writeback,
  the pilot deletes the subscription and emits `close(frontier, reason)`
  (§12.2: state removed the subscription's authority).
- **Init** is the same face with an empty prior and an `init` disposition:
  full rows (or window-open, which reports the §12.2 `AbsentAnchor` refusal
  when a concrete anchor names no current occurrence — surfaced as a typed
  member of the result, never a silent empty window), or the scalar value.

Face signature (extension, beside the §7.4 faces):

```sql
FUNCTION liasse.sub_advance(state jsonb, prior jsonb, fresh jsonb, authorized boolean)
RETURNS jsonb   -- { live, delta, retain, evict, gap, scalar }
IMMUTABLE STRICT PARALLEL SAFE COST 1000
```

`IMMUTABLE` is truthful: a pure function of its four arguments (the machine
reads no tables — the statement feeds it; the same argument-passing discipline
that kept the §7.4 faces `IMMUTABLE` and the plan gateable). The delta member
is the canonical wire encoding of the §12.2 frame payload — the pilot
forwards it byte-for-byte; it never re-encodes, which is part of the
no-computation-in-Rust proof.

### 4.3 Occurrence identity and sort tuples — where they come from

The fresh CTE's `key_wire` (flat) or accumulated `key_path` (coverage) decodes
through the shared codecs to the per-level `KeyValue`s; the machine builds the
`RowId` from their canonical key text exactly as the runtime's
`ViewResult` construction does today (one `Key` part per level, D.1/D.2), and
renders it to the canonical `occ` text used as the join/identity key. The
decodable evaluated `$sort` tuple — required by the frozen-gap coordinate —
is the `sort_vals` column (`liasse.eval` over the sort-key list, the
`DESIGN-pure-pg.md` §7.6 "sort_tuple column", selected only by subscription
statements); `ord` (`eval_sort`'s `sort_enc` bytes) drives the SQL `ORDER BY`
and is not retained. Both come from the §7.4 extension faces — no new
identity or ordering machinery is introduced by this design.

### 4.4 The relocation: crate `liasse-sub`

The diff and window logic leave `liasse-surface`/`liasse-runtime` and land in
one new pgrx-free crate, **`liasse-sub`** (one concern per crate: the §12.2
subscription advance machine):

- moved in: `patch::diff` + `PatchOp`, `ViewDelta::between`/`between_rows`,
  `ViewRow` (with `same_value`), the `Window`/`FrozenGap`/`Anchor` machinery
  of `window.rs`, the advance/init state machine of `watch.rs` (minus
  `WatchAuthz`, which dissolves into pilot routing + §3.2 columns), and the
  §12.2 wire encoding of a delta;
- dependencies: `liasse-expr` (RowId, SortOrder), `liasse-value` — nothing
  heavier; linked by `liasse-runtime` (re-export for plain `view` reads and
  frame types), `liasse-store`'s `MemoryStore` path via the opaque trait
  (§7.2), and `liasse-pg-ext` (the `.so`);
- the existing watch/window unit suites move with the code and keep passing
  unchanged — the relocation is behavior-neutral by construction, and those
  suites become the layer-1 gate (§9).

This is the same containment pattern as `liasse-pred`: the semantics live in
ONE pgrx-free crate; the extension and the in-memory store link it; parity
between stores is same-linked-code plus a transport gate.

### 4.5 Writeback discipline

The single statement's `put`/`del` CTEs touch **disjoint row sets** (upsert
survivors + inserts; delete departed only) — never the same `(sub_id, occ)`
twice in one statement, which is the one thing data-modifying CTEs forbid.
This disjointness must be pinned by a test (it is an easy regression to
introduce by "simplifying" to delete-all + insert-all, which has
undefined-order unique-violation behavior). If prototyping (§10, phase 10d)
finds any planner surprise in the composed statement, the recorded fallback
is the same computation as one explicit transaction on the pooled connection
(`BEGIN; SELECT compute…; writeback…; COMMIT`) — semantically identical
(the optimistic `frontier = $prev` guard, §8.4, carries the
single-advancer coherence either way), one round trip more.

### 4.6 Scalar/aggregate subscriptions: the fold face

`DESIGN-pure-pg.md` §7.5 left the aggregate *fold* in Rust over the pushed
stream — fine for a one-shot read, but a subscribed aggregate would make
every advance a Rust computation, which mandate 3 forbids. This design pulls
the documented fold-in-SQL seam forward for the subscription path only, as
one more linked face:

```sql
FUNCTION liasse.eval_fold(fold bytea, rows jsonb, env bytea) RETURNS jsonb
IMMUTABLE STRICT PARALLEL SAFE COST 100
```

— the same interpreter's aggregate fold (`count`/`sum`/`avg`/`min`/`max`,
Annex-A decimal semantics and all) over the `jsonb_agg`-collected pushed
stream, producing the scalar the advance compares. A SQL-native aggregate
(`sum((…)::numeric)`) is rejected for the same reason v2's operator table was:
a second arithmetic. Aggregates whose *source* does not lower, and every
other scope-deferred source class, take the fallback of §4.8. One-shot
`view` reads keep the Rust fold — no behavior change there.

### 4.7 The env problem, stated honestly

A pushed program's hoisted env has three classes with different lifetimes:

1. **Stable** — `$actor`/`$session`/`@params`: fixed at open, stored
   (`env_stable`), always available to the advance. The overwhelmingly common
   case; a subscription whose program hoists nothing else is fully
   PG-advanced with zero Rust evaluation.
2. **Stored-state-dependent** — a hoisted `/`-read (an `in /admins` haystack,
   a `/accounts[.owner]` deref base): its value changes with commits, so a
   stored copy would be stale — the §12.2 violation would be silent and
   ugly. Design: an **env-prefetch CTE** — the compiler emits, per such
   entry, the §4.2/§7.6 scan that materializes it, `jsonb_agg`ed and passed
   to the faces as a per-statement env-extension argument the extension
   decodes with the same codecs (exactly the subtree-prefetch pattern, §7.6
   of `DESIGN-pure-pg.md`, applied to `/`-reads). This keeps the advance
   one statement and the faces `IMMUTABLE`.
3. **Engine-state and temporal** — keyring metadata, meter accessors,
   `now()`: engine values, supplied as advance *parameters* (the pilot's
   clock is an input, §2; engine-state values are read via their own §4
   reads by the statement assembler in `liasse-pg`, not by the pilot).

**Flag for the maintainer** (§11): class 2's env-prefetch CTE is new compiler
surface (each hoisted `/`-read must itself be a lowerable scan). v1 scope
choice: build the prefetch for the plain `/`-collection haystack (the common
`in`/deref shapes); a subscription whose program hoists a *non-lowerable*
candidate-free read (a view-combinator result, say) takes the §4.8 fallback,
reported. This mirrors §7.5's scope-not-restriction discipline.

### 4.8 The fallback class: Rust-evaluated fresh, PG-diffed and PG-stored

Scope-deferred views (`DESIGN-pure-pg.md` §7.5: combinators, non-composing
view-refs, bucketed sources in v1, engine-state sources, `json` sort keys)
and **non-head-frontier reads** (§19.2 replay/resume, §6.4) cannot have their
*evaluation* pushed — that is a scope fact this design inherits, not a new
tension. Disposition under the binding model: the pilot's engine evaluates
the fresh `ViewResult` in Rust (today's interpreter path, hydration-shared),
encodes it, and hands it to the SAME advance statement as the `fresh`
argument (a jsonb parameter replacing the fresh CTE) — **state and diff stay
in PostgreSQL**; only the evaluation ran outside, exactly as `scan_view`'s
own fallback does for one-shot reads. The pushdown report names every
subscription on this route; strict mode refuses them. This is the honest
boundary of "ALL computation in PG": it holds for every lowerable view (the
§7.5 claim: the overwhelmingly common case), and degrades loudly, never
silently, elsewhere.

## 5. Events: the NOTIFY/LISTEN protocol

### 5.1 Who fires: the writer, transactionally

`PgStore::commit_transition` appends one statement to its existing admission
transaction:

```sql
SELECT pg_notify($channel, $payload)
```

PostgreSQL delivers NOTIFY only if the transaction commits, after it commits,
to every listening connection, **in commit order** — which is precisely the
§12.2 frontier-monotonicity requirement: a pilot observes seq n's
notification before seq n+1's, and the head it reads on wake is ≥ the
notifying seq. Writer-issued beats trigger-based on every axis that matters
here: the writer already holds the decoded ops (the touched set is a
by-product of the codec work it just did), no per-row trigger fires N times
per commit, and the reconciler manages no trigger objects. The
trigger-on-`commit_log` alternative (an AFTER INSERT statement trigger
computing the payload in SQL) was considered and rejected: its only advantage
— catching a hypothetical writer that bypasses `PgStore` — guards a case the
one-writer contract already excludes, at the price of teaching the database
to parse the ops wire (`record_codec` deliberately stays out of the
extension). Recorded as a seam if out-of-contract writers ever exist.

Temporal advances (§14.1, §22.6) produce **no commit and no NOTIFY**: time is
a pilot input (§2). The pilot that moves time runs the advance sweep itself
(§6.2); in a multi-pilot deployment the time-mover additionally notifies its
peers on the same channel with a temporal payload (`{v,t:<instant>}`) so
their subscriptions observe bucket transitions — same channel, same wake
path, no new mechanism.

### 5.2 Channel granularity

One channel **per instance schema**, named by the schema name
(`liasse_<ns>_<label>_<hash>` — already a valid identifier under the 63-byte
limit, §schema.rs). Options weighed:

- *per-collection channels*: LISTEN has no wildcard; a pilot would LISTEN/
  UNLISTEN as watched-collection sets churn, and the commit would notify K
  channels. All cost, no filtering benefit over a payload filter. Rejected.
- *one global channel*: every pilot wakes for every instance's every commit.
  Rejected — the instance is the natural unit (its schema, its head, its
  subscriptions).
- *per-instance channel + payload filter* (chosen): a pilot LISTENs exactly
  the instances it serves; the payload (or `commit_log`) narrows to
  subscriptions.

### 5.3 Payload, the 8 KB limit, and the overflow path

Payload (canonical JSON, versioned):

```json
{"v":1,"seq":417,"touched":["projects","companies/*/members"]}
```

`touched` is the commit's collection-path set — the writer derives it from
the ops it just encoded (one path per op's address, deduplicated to declared
collection paths). NOTIFY payloads are capped a bit under 8 KB; a commit
touching pathologically many collections overflows. Overflow path: the writer
sends `{"v":1,"seq":417}` (no `touched`) and the wake path treats the touched
set as *unknown* ⇒ the dependency filter degrades conservatively (every
subscription with `frontier < seq` is advanced; a dependency-disjoint one
computes a frontier-only no-op — correct, just not skipped). To keep the skip
exact even for huge commits, the durable truth backs the hint: **judgment
call (§11)** — recommended: `commit_log` gains a `touched TEXT[]` column
written by the same insert (cheap: the writer already has the set; rides the
SCHEMA_VERSION 5 bump), so the wake path's bulk statements (§5.4) join
`commit_log` for the exact range-union of touched sets when the payload
omitted it, and reconnect catch-up (§6.4) has the same source. The
alternative — payload-only, conservative on overflow — is one column lighter
and strictly less exact; it is acceptable if the maintainer prefers zero
schema growth.

Coalescing is inherent: notifications carry no delta, only "the head moved
past seq with these collections". A pilot waking late (or after processing a
burst) advances **to head, once** — the diff between stored prior and current
head collapses any number of intervening commits into one §12.2 patch, which
is exactly what the retained-state design buys (§6.5's backpressure story is
the same property used deliberately). Identical payloads sent within one
transaction are deduplicated by PostgreSQL; distinct seqs are distinct
payloads, so nothing is lost to that rule.

### 5.4 From NOTIFY to "which subscriptions advance"

On wake with `(seq, touched)`, per owned instance, the pilot issues two bulk
statements *before* any per-subscription advance:

```sql
-- (a) the commit-scoped skip: dependency-disjoint ⇒ frontier-only no-op
UPDATE {s}.subscriptions
SET frontier = $head
WHERE pilot_id = $me AND frontier < $head AND NOT (deps && $touched)
RETURNING sub_id, frontier;

-- (b) the advance set: dependency-overlapping, mine, behind
SELECT sub_id FROM {s}.subscriptions
WHERE pilot_id = $me AND frontier < $head AND (deps && $touched);
```

(a)'s returned rows become frontier-only `patch(frontier, [])` frames —
§12.3's "unaffected views MAY receive only a frontier advancement",
implemented as one statement for the whole population. (b)'s rows each run
the §4.1 statement. `$touched` comes from the payload, or from
`commit_log[frontier+1 ..= head]` (§5.3) when omitted.

**Why the skip is sound against §12.2's re-evaluate-at-every-frontier rule**
(load-bearing): `deps` is compiled as the union of (i) the view's collection-
dependency set — the lowered source path plus every hoisted `/`-read and
env-prefetch source (a by-product of lowering, `DESIGN-pure-pg.md` §8
mitigation 3), (ii) the role's `$members` view dependencies, and (iii) the
authenticator's session/account collection dependencies. Membership, session
validity, and the view's rows can only change through a commit touching one
of those collections — so a dependency-disjoint commit provably cannot have
changed the authorization verdict *or* the rows, and re-using the prior
verdict for the frontier-only frame is exact, not approximate. Temporal
advances get no such shortcut: a session can expire and a bucket row can
leave with **no commit at all**, so a temporal sweep always runs the full
§4.1 advance (with its membership probe) for every owned subscription — the
bucket-boundary index ("advance only subscriptions whose next temporal
boundary ≤ now", a computed column maintained at advance time) is the
designed optimization seam, not v1.

### 5.5 Multi-pilot broadcast

Every pilot LISTENs the channels of the instances it serves; each filters to
`pilot_id = $me` (the routing fact "whose socket holds this subscription" is
transport, mirrored into the lease column so the *backend* can filter — the
column is ownership metadata, not logical §12 state). The committing pilot's
own NOTIFY arrives on its own LISTEN connection too; by then its synchronous
barrier (§6.3) has already advanced its connection's subscriptions past
`seq`, so the wake finds `frontier ≥ head` and no-ops — self-suppression by
monotonicity, no special case.

## 6. The Rust pilot loop

### 6.1 Connections

The events handle is a **dedicated** connection per (pilot, database) — not a
pool checkout (LISTEN registrations are per-connection and a parked pool
member would starve reads), owned by the store's events object (§7.1). All
advance statements run on the ordinary r2d2 pool. The sync `postgres` crate's
`Client::notifications()` iterator with timeout is the wait primitive.

### 6.2 The loop

```text
loop:
  events = store.events.wait(timeout)          -- NOTIFY batch, or timeout
  for each (instance, seq|temporal) in events:
    run bulk skip (§5.4a) → forward frontier-only frames
    for sub_id in advance set (§5.4b):
      if socket for sub_id is congested: defer (mark; §6.5)
      out = store.sub_advance(sub_id, now)     -- the §4.1 statement
      forward per §6.6 (auth re-check → frame → socket)
  on timeout: re-arm leases; reap expired subscriptions (§3.3);
              temporal sweep if the clock crossed a boundary
```

`advance_time` (the driver moving the virtual clock) runs the same body
inline with a temporal event; `sweep_all` (operator transitions, §19 import)
runs it with the head as the event. Both lose their hydrate-and-diff bodies
and become event injections.

### 6.3 The §12.3 completion barrier under a PG-resident diff

`call` on connection C, after `commit_transition` returns `seq`:

1. run the §5.4 pair **restricted to C's subscriptions**, synchronously;
2. for each: the §4.1 advance (whose statement snapshot is ≥ `seq` — the
   single writer already committed, and reads on the pool observe committed
   state immediately, §5.2 of `DESIGN-pure-pg.md`), forward the frame;
3. only then return `committed { frontier, commit }`.

That is §12.3's "advance every still-authorized subscription on the same
logical connection through the commit before returning" — the committing
path and the diff location decoupled cleanly because the calling connection's
sockets live, by construction, on the pilot that ran the call. Subscriptions
on *other* connections (same pilot or peers) advance on the NOTIFY wake —
asynchronously but promptly. Two conformance notes, checked against SPEC and
corpus:

- §12.3 binds only the caller's connection; §12.2 requires re-evaluation "at
  every **outgoing** frontier" — a peer emits no frontier between the commit
  and its wake, so nothing stale ever leaves.
- Today's in-process host advances peer *authority* (close) synchronously
  and peer *rows* never (only on the peer's own commit). Under NOTIFY, peer
  rows advance on wake — live views become actually live for idle peers. The
  corpus was checked for a pin on the old behavior:
  `frontier-at-least-own-commit-under-concurrency` deliberately admits both
  serializations of the peer's row (`expect_one_of`), and
  `unaffected-watch-advances-frontier` / `authority-loss-emits-close` are
  same-connection. **No corpus case pins peer-row staleness**; the change is
  conformant (§12.2 is exactly this) and strictly more useful. Flagged in
  §11 for the maintainer's awareness all the same, since it is
  client-observable timing.

### 6.4 Reconnect and resumable frontiers (§12.2, §12.3, §19.2)

A client resumes subscription `id` from retained frontier `F`:

- **`F` == the stored subscription frontier** (the common reconnect within
  the lease window, possibly to a *different pilot*): the new pilot adopts
  the subscription — one `UPDATE … SET pilot_id = $me, lease_until = …
  WHERE sub_id = … AND lease_until …` (taking over an expired or
  released lease), inserts the routing entry, then runs one ordinary §4.1
  advance to head: the client receives exactly "the later authorized patches
  in that stream" (§12.2) with zero re-init. This is a capability the
  Rust-resident design could never offer (state died with the process) and
  is the concrete payoff of PG-resident subscription state for
  [[web-client-sync-connector]].
- **`F` behind the stored frontier, or the subscription reaped/truncated**:
  the retained range is released ⇒ fresh `init` (§12.2's second arm), which
  is today's universal behavior — still conformant, now the exception
  instead of the rule. The *upgrade seam*, designed not built: rebuild the
  prior at `F` via the §19.2 primitive — `snapshot(F)` (the O(history)
  commit-log fold, served from PG) + interpreter evaluation (the non-head
  fallback, §4.8) — seed `sub_rows` with it, then advance to head; the
  client gets `patch(F→head)` instead of an init. Correct by construction,
  off the hot path, priced at O(history) exactly as §19.2 says replay is.

Resume authorization is unchanged: `resolve_view` re-runs membership before
any row flows (`resume-after-authority-loss-denied` stays green).

### 6.5 Backpressure

A slow client's socket buffer fills. The pilot must never drop a patch (the
stream would corrupt — `WireStore` would reject or, worse, diverge), so the
choices are buffer-unbounded (forbidden), reset (today's only lever), or —
what PG-resident state makes natural — **defer the advance**: a congested
subscription is skipped by the wake loop (its PG state stays at its old
frontier; the frontier-only bulk statement excludes deferred subs too) and
advanced when the socket drains, at which point the single §4.1 statement
diffs prior→head across every missed commit — bounded memory, one coalesced
patch, no protocol violation. A subscription deferred past a configured
horizon (or past a reaped lease) is closed with `reset`; the client
re-views. The deferral mark is transport (per-socket congestion state), not
logical state — the subscription row itself is simply *behind*, which is a
legal state of the stream.

### 6.6 Forward-time authorization (the pilot's close decision)

Before forwarding any computed frame, the pilot performs the §12.2 re-check
that CANNOT live in the database: credential verification. The split (§8.1):
the advance statement already re-evaluated **membership and scope coverage**
(state facts, DB-run); the pilot re-verifies **the credential** — the
retained `AuthSelection` (connection context or per-request selection, §11.4)
against committed state via the authenticator's `$verify`/resolve (v1
framework-executed per `DESIGN-pure-pg.md` §7.5, reading keyring engine
state + the credential it alone holds). Either failing ⇒ the pilot closes:
`sub_close(id)` + `close(frontier, reason)` frame (§12.2, §11.7 — revocation
and expiry deny the very next outgoing frontier). The CLOSE decision is thus
the pilot's, on backend-computed facts plus transport-held credentials —
exactly the maintainer's reconciliation.

### 6.7 Failure modes, all loud

- **LISTEN connection drops**: the events handle surfaces a `StoreError`;
  the pilot re-establishes, re-LISTENs, then runs a **catch-up sweep**
  (advance every owned subscription behind head — notifications during the
  gap are unrecoverable by design, NOTIFY is not a queue; the sweep is the
  recovery primitive and is gated, §9). No silent resubscribe-and-hope.
- **Pilot crashes**: leases expire; another pilot (or its own restart) reaps;
  clients resume (§6.4 arm 1 within the lease window, else fresh init).
- **PostgreSQL crashes**: UNLOGGED truncation (§3.3) ⇒ every resume takes
  the fresh-init arm; `operations` (LOGGED) preserves §12.3 at-most-once.
- **PG unavailable mid-advance**: the advance statement errors ⇒
  `StoreError::Backend` ⇒ the pilot resets affected connections (clients
  re-view when the store returns). Never a half-forwarded frame: forwarding
  happens only after the statement returned its delta.

## 7. The generic-backend contract surface

### 7.1 New `liasse-store` methods (sketch — shapes, not final signatures)

Same discipline as `scan_view`: semantics-carrying, backend-agnostic, the
semantics entering through opaque single-implementation carriers.

```rust
/// Opaque subscription handle, allocated by the store.
pub struct SubscriptionId(u64);

/// The §12.2 advance machine, opaque to the contract — ONE implementor,
/// liasse_sub::SubPrograms (§4.4): init/advance faces for MemoryStore, the
/// serialized machine-state wire for a pushdown backend. Mirrors ViewProgram.
pub trait SubMachine { /* init / advance faces + state & window wires */ }

/// What a subscription is: the evaluated read (reused from scan_view) plus
/// the §12.2 machine, the §8.2 membership probe, and the §5.4 dependency set.
pub struct SubscriptionSpec<'a> {
    pub source: ViewSource<'a>,
    pub program: &'a dyn ViewProgram,
    pub skip: Option<u64>, pub limit: Option<u64>,
    pub machine: &'a dyn SubMachine,          // window params, shape, fold
    pub membership: Option<&'a dyn ViewProgram>, // lowered §10.3/§10.5 probe (None = public)
    pub deps: Vec<CollectionPath>,
    pub authz: SubAuthz,                      // role name, actor/session keys — no credential
}

fn sub_open(&mut self, spec: SubscriptionSpec<'_>) -> Result<SubOpened, StoreError>;
    // SubOpened { id, frontier, first: SubDelta }  — init rows / scalar /
    // window rows, or the typed AbsentAnchor refusal (§12.2)
fn sub_advance(&mut self, id: SubscriptionId, input: AdvanceInput<'_>)
    -> Result<SubAdvanced, StoreError>;
    // AdvanceInput { now: Timestamp, fresh_override: Option<&ViewResultWire> }
    //   — fresh_override is the §4.8 fallback seam (Rust-evaluated fresh)
    // SubAdvanced { frontier, delta: SubDelta, authorized: bool }
fn sub_skip_disjoint(&mut self, pilot: &PilotId, head: CommitSeq, touched: &Touched)
    -> Result<Vec<(SubscriptionId, CommitSeq)>, StoreError>;   // §5.4 (a)
fn sub_pending(&self, pilot: &PilotId, head: CommitSeq, touched: &Touched)
    -> Result<Vec<SubscriptionId>, StoreError>;                // §5.4 (b)
fn sub_close(&mut self, id: SubscriptionId) -> Result<(), StoreError>;
fn sub_adopt(&mut self, id: SubscriptionId, pilot: &PilotId)
    -> Result<Option<CommitSeq>, StoreError>;                  // §6.4 reconnect

/// The event basis. Backend-defined transport behind one blocking surface.
fn events(&self) -> Result<Self::Events, StoreError>;
pub struct StoreEvent { pub frontier: CommitSeq, pub touched: Touched }
pub enum Touched { Collections(Vec<CollectionPath>), Unknown }   // Unknown = §5.3 overflow
pub trait StoreEvents { fn wait(&mut self, timeout: Duration)
    -> Result<Vec<StoreEvent>, StoreError>; }
```

`SubDelta` is contract-level *data* (the five §12.2 ops + init/scalar/close
forms — the wire vocabulary, semantics-free as data, computed only inside
`liasse-sub`). The delta the store returns is what the pilot forwards;
`liasse-wire` encodes frames from it without recomputation.

### 7.2 `MemoryStore` — the same surface, in-process

- Subscription state: an owned map `SubscriptionId → SubState` (prior rows,
  frontier, machine state) **inside the store** — backend state held by the
  backend, exactly the reclassification; no interior mutability (all methods
  taking state are `&mut self`, matching the existing contract style —
  `sub_open`/`sub_advance`/`sub_close` are `&mut` on both stores since they
  write backend state).
- Advance: `scan_view` on itself (the in-Rust evaluation) → the SAME
  `liasse-sub` machine faces → delta; membership via the same lowered probe
  program evaluated in-process. Byte-identical computation to the extension
  by shared linkage.
- Events: commit pushes `StoreEvent` onto a `std::sync::mpsc` channel whose
  receiver is the `Events` handle (a std primitive conveying events out of
  `&mut` commit — not an interior-mutability structure of our own state;
  noted for the AGENTS.md sentence Phase 10b lands, §10.1). Temporal events
  are injected by the pilot on both stores identically.

With this, "Rust pilots the generic backend" is literal: the pilot loop of §6
compiles once against `InstanceStore` and runs unmodified over either store —
which is also what makes the §9 parity gate a true end-to-end oracle.

## 8. Authorization and coherence

### 8.1 The §12.2 re-evaluation, split by what each side can know

§12.2 requires re-evaluating, at every outgoing frontier: authentication,
session validity, scoped role membership, surface availability, output
projection. The split:

| Check | Where | Why |
|---|---|---|
| output projection | in-PG | it IS the fresh `scan_view` (the projection wire) |
| role membership (§10.3) | in-PG (§8.2) | the `$members` view is a DB-run, built-in-only read — lowerable like any view |
| scoped coverage (§10.5) | in-PG (§8.2) | the coverage re-walk is the recursive source shape, already in-PG |
| session validity via state (`$bucket` activity, revocation rows, §11.7) | in-PG | state facts the membership/verify reads observe |
| credential verification (authenticator resolve / `$verify`) | pilot | the credential exists ONLY in transport (§11.3); v1 executes `$verify` framework-side (`DESIGN-pure-pg.md` §7.5) |
| surface availability | pilot (compile-time) + in-PG (definition head) | a redefinition is a commit; the advance statement's snapshot observes it |
| the CLOSE decision | pilot | maintainer reconciliation; it composes the two verdicts and owns the socket |

Deliberate consequence: **credentials are never written to PostgreSQL** —
§11.3's transport-only rule outranks "all state in Postgres", and the
maintainer's own reclassification lists auth-re-check-on-forward as
Rust-resident. The stored authz columns (§3.2) are the *resolved
coordinates* (role, actor key, session key) the DB-side membership probe
needs — derived identities, not secrets. Flagged in §11 for confirmation.

### 8.2 The in-PG membership probe

The role's `$members` view (§10.3) is lowered once at open into
`members_admit` (a `ViewProgram` whose consumption is "does the actor's exact
row identity occur at least once"): the advance statement's `member` CTE is
an `EXISTS` over the §7.6 statement with the actor key in env — one indexed
probe, short-circuiting on the first occurrence. A scoped subscription
additionally re-walks its §10.5 path: the coverage CTE constrained to the
stored `scope_wire` key path (each step `$where`-included, non-`$except`,
strict descendant — the admission re-walk, §10.5), an `EXISTS` again. A
`$members` view that does not lower (a combinator, §7.5) puts the
*subscription* on the §4.8 fallback route for its membership half — the
pilot evaluates membership in Rust as today's `Barrier::authorized` does —
reported, strict-refusable. Fail-closed: a NULL/absent probe result closes,
never admits (mirroring `authorize_role`'s fail-closed membership read).

### 8.3 Consistency class of an advance

The §4.1 statement is `DESIGN-pure-pg.md` §5.4 **case 1** — one MVCC
statement snapshot covering head + fresh + membership + prior + writeback.
No `read_session` pinning is needed on the happy path. The one multi-reader
hazard is *two advances of one subscription racing* (a wake and a barrier,
or two pilots during lease takeover):

### 8.4 Single-advancer coherence

The `bump` CTE's `WHERE frontier = $prev` optimistic guard makes the
statement's writeback conditional on the subscription still being where the
advancer read it; `bumped = 0` means a concurrent advance won — the loser
discards its delta (forwarding it would double-apply ops the winner already
shipped) and re-reads the subscription's frontier: if ≥ head, done (the
winner covered it); else retry. Within one pilot this race is precluded by
the single-threaded loop; across pilots by `pilot_id` ownership + lease
takeover, so the guard is defence in depth — but it is what makes the design
safe to state rather than hope. (MemoryStore: `&mut self` makes the race
unrepresentable — the guard is PG-only mechanics below the contract.)

### 8.5 Single writer, many pilots, many nodes

The write path keeps **one writer per instance** (`DESIGN-pure-pg.md` §5.2,
§6): mutation `call`s are admitted by the pilot owning the writer; other
pilots serve `view`/subscription/read traffic over the pool and their own
LISTEN connections. This is the [[web-client-sync-connector]] topology: N
stateless-ish connector pilots fanning out sockets, one admission owner, one
PostgreSQL. Full multi-writer admission (the head `FOR UPDATE` lock already
serializes commits — §22.3 explicitly admits "database serialization" as the
admission mechanism) is a real future option but re-opens `Prospective::
gather`'s coherence premise (§5.4 case 3 relies on in-process exclusivity ⇒
the `read_session()` REPEATABLE READ seam must be wired) — recorded as the
one prerequisite, deliberately out of scope here.

## 9. Parity, gates, benchmarks

- **Layer 1 — the relocation is behavior-neutral**: the existing watch/window
  unit suites move to `liasse-sub` with the code and stay green unmodified
  (diff order, window placement, gap freeze/resume, slide clamp, scalar
  no-op, size-0 window, eviction-as-remove — all pinned already).
- **Layer 2 — store parity, the headline gate**: one scripted driver runs
  identical scenario scripts (open/commit/advance/window/reauth/resume/close
  interleavings) against `MemoryStore` and `PgStore` **through the identical
  pilot code** (§7.2), asserting byte-identical frame streams per
  subscription. Since the machine is same-linked code, what this actually
  guards is the transport: jsonb prior/fresh encode–decode, `pos`/`win_pos`
  round-trip, occ-text identity, `sort_vals` decode for the gap, the
  writeback CTEs, the optimistic guard, the dep-filter statements, the
  fold face, NOTIFY payload encode/decode. Corpus: the §9 adversarial view
  corpus of `DESIGN-pure-pg.md` (NUL text, scale-variant decimals, absent
  optionals under mixed-direction sorts, ties) × subscription edges: windowed
  first/last/anchored/slide; anchor-gap freeze, absent-anchor resume across
  descending sorts, reappearance; rekey ⇒ remove+insert; scalar incl.
  `Value::None`-carrying present scalar vs the no-op; frontier-only skips;
  membership-revoked close at exact frontier; lease-expiry reap; reconnect
  same-frontier patch continuation; PG-crash truncation ⇒ fresh init.
  Expected frames hand-derived (AGENTS.md: externally deducible).
- **The §12 conformance corpus is the outer gate**, unmodified: every
  `tests/12-clients-live-views/` case must stay green over the rewritten
  host on both stores — `patch-coherence-equals-declared-view` is §12.2's
  own oracle restated.
- **Event gates**: one commit ⇒ exactly one event with the correct seq and
  touched set on a listening handle; payload-overflow commit ⇒ `Unknown`
  touched ⇒ conservative advance still frame-correct; killed LISTEN
  connection ⇒ loud error, catch-up sweep converges to the same frames the
  uninterrupted run produced; NOTIFY self-suppression (committing pilot's
  own wake no-ops); temporal event with no commit advances bucketed views
  (`temporal-observation-advances-live-view` on the new path).
- **EXPLAIN gates** (extending `index_coverage_pg.rs`, on *populated*
  subscription tables — hundreds of subscriptions, so the planner has
  something to choose): (14) the §4.1 advance statement — fresh CTE inherits
  gates (11)/(12)/(13) verbatim; `prior` served by `sub_rows` PK / `sub_rows_pos`
  (no Seq Scan, no Sort — `pos` index order); the writeback upsert on the PK.
  (15) the §5.4 pair — GIN `sub_deps` for the `&&` filter, `sub_by_pilot`
  for the pilot+frontier range. (16) lease reap — `sub_by_pilot` or a pinned
  small-table exemption if the planner prefers it at realistic sizes
  (measured, then pinned, not guessed).
- **Fail-loud gates**: advance during simulated PG outage ⇒ `StoreError`,
  reset frames, no partial stream; `bumped = 0` race ⇒ delta discarded,
  retry converges; disjoint-writeback regression test (§4.5).
- **Benchmarks** (Phase 10e closes on recorded numbers): end-to-end
  commit→frame latency vs the Phase-4/8 hydrate-and-diff baseline (the
  number that justifies this design); the §4.1 statement vs the identical
  hand-written SQL (the near-raw gate, extension faces on both sides);
  advance cost vs subscriber count on one tuple (§11 sharing seam trigger);
  wake fan-out under a commit storm with 90 % dependency-disjoint
  subscriptions (the skip's payoff); coalesced advance after an N-commit
  deferral vs N sequential advances.

## 10. Phase plan — the revised Phase 10

Prerequisites unchanged from `DESIGN-pure-pg.md` §10: Phase 7 (evaluator
stack + the mandate-7 load rule — "7b"), Phase 8 (extension + image), Phase 9
(`scan_view` → SQL) land first; this phase consumes them.

| Phase | Content | Exit criteria |
|---|---|---|
| **10a** | Extract `liasse-sub` (diff/window/machine + wire types moved from `liasse-runtime`/`liasse-surface`, §4.4); runtime/surface re-point; watch/window suites move with it | workspace green; zero behavior change; layer-1 gate = the moved suites |
| **10b** | Contract surface (§7.1) + `MemoryStore` implementation (§7.2) + the pilot rewrite of `liasse-surface` (loop §6.2, barrier §6.3, resume §6.4, backpressure §6.5, close §6.6) over the contract; **DELETE** `watch.rs`, the barrier advance path, `Connection.watches` (§10.2); `operations` relocation designed-in behind the contract | full §12 corpus + surface suites green on MemoryStore through the new pilot; grep-provable: no retained result/frontier/window field outside stores |
| **10c** | PG subscription schema (SCHEMA_VERSION 5: `subscriptions`, `sub_rows`, `operations`, `commit_log.touched`), reconcile entries, leases + reap, open-time sweep; `sub_open`/`sub_close`/`sub_adopt`/bulk statements; EXPLAIN gates (15)(16) | corpus green on PgStore with advances still fresh-computed via §4.8 override (state PG-resident, evaluation not yet pushed); reap/lease gates green |
| **10d** | `liasse.sub_advance` + `liasse.eval_fold` faces (extension links `liasse-sub`); the §4.1 statement incl. env-prefetch CTE (§4.7) and writeback discipline (§4.5); writer NOTIFY + `PgStore` events (§5); pilot on real events | layer-2 parity green (identical frames both stores, adversarial corpus); event gates green; EXPLAIN gate (14) green; §12 corpus green end-to-end on the image |
| **10e** | Multi-pilot hardening (lease takeover §6.4, catch-up sweep §6.7, optimistic-guard race gate §8.4); benchmarks recorded | race + catch-up gates green; bench report committed; pushdown report lists every §4.8-routed subscription |

Each sub-phase lands green (corpus + parity + gates), per the standing
discipline. 10c before 10d is the same attribution ordering as Phases 7→9:
state relocation is gated before computation relocation, so a 10d divergence
is attributable to the statement/faces, never to the schema or lifecycle.

### 10.2 Deleted vs kept — the no-legacy inventory

**Deleted outright** (pre-release, no deprecation): `liasse-surface/src/
watch.rs` (`Watch`, `WatchAuthz`); `host/barrier.rs`'s hydrate→re-eval→diff
advance body (the file becomes the pilot's forward/close step; the Phase-4
hydration sharing survives only inside the §4.8 fallback evaluation);
`Connection::watches` and the `Watch` accessors on `SurfaceHost`
(`read_view`/`read_window` re-serve from the store — a test convenience read,
now `sub_`-backed); `window.rs` and the diff halves of `view.rs`/`patch.rs`
*as locations* (logic moves to `liasse-sub`, §4.4 — moved, not forked; no
copy remains behind).

**Kept**: `crates/liasse-wire/` in full (`WireStore` — the client fold stays
the client's, untouched); `ViewResult`/`ViewRow` as the result type of plain
`view` reads and the decode vocabulary; `PatchOp` as wire vocabulary
(re-exported from `liasse-sub`); the §19.2 replay/`snapshot(frontier)`
machinery (it is the §6.4/§4.8 fallback's substrate); `resolve_view`/
`authorize_view`/denial-uniformity logic in `host/call.rs` (open-time
authorization is untouched by this design).

## 11. Judgment calls (flagged for the maintainer) and tensions

Judgment calls where the directives underdetermine — recommendation stated,
alternatives preserved:

1. **Diff-in-PG mechanism = the linked Rust machine (`liasse.sub_advance`),
   not a SQL re-derivation.** A FULL OUTER JOIN + window-function diff was
   evaluated: removes/updates fall out naturally, but pass-2's
   mid-application positions are an imperative fold a pure-SQL rendering
   would re-implement — a second diff, permanently gated against the first.
   Rejected on the same grounds v2's operator table died. The SQL's job is
   feeding and storing; the computation is one linked function. (If the
   maintainer intended "the diff as a SQL query", this is the deliberate
   deviation to review.)
2. **NOTIFY channel granularity: per-instance schema channel** (§5.2), not
   per-collection, not global.
3. **Payload: seq + touched, degrading to seq-only on ~8 KB overflow, with
   the durable `commit_log.touched TEXT[]` column backing exactness**
   (§5.3). The zero-schema-growth alternative (conservative advance on
   overflow) is workable if the column is unwanted.
4. **Writer-issued NOTIFY, not triggers** (§5.1); trigger seam recorded.
5. **Subscription durability class: UNLOGGED + leases + open-time sweep**
   (§3.3); PG-crash ⇒ fresh-init resumes (conformant). The LOGGED
   alternative buys patch-continuity across a PG crash at WAL cost on the
   hottest write path — judged not worth it while §12.2 blesses re-init.
   `operations` is LOGGED (at-most-once must survive).
6. **GC policy**: lease period (suggested 30 s re-arm / 90 s expiry) and the
   deferral horizon (§6.5) are host policy knobs, defaults boring, flagged.
7. **Credentials never enter PostgreSQL** (§8.1): §11.3's transport rule is
   read as outranking "all state in Postgres"; the advance carries resolved
   identity coordinates only, and credential re-verification stays on the
   pilot (also matching the maintainer's own "auth re-check-on-forward is
   Rust-resident"). Confirm.
8. **Peer connections now receive row patches on wake** (§6.3) — conformant
   (§12.2), corpus-compatible (checked), strictly more "live", but an
   observable timing change from the current reference behavior. Confirm.
9. **Env classes** (§4.7): stable env stored; state-dependent hoisted
   `/`-reads served by an env-prefetch CTE (v1: plain collection reads);
   engine/temporal values as advance parameters; non-lowerable hoisted
   entries ⇒ §4.8 fallback, reported. The prefetch is new compiler surface —
   the one place this design grows Phase-7 scope.
10. **`liasse.eval_fold`** (§4.6): the aggregate fold-in-SQL seam is pulled
    forward for *subscribed* aggregates only, as a linked face — otherwise
    every scalar-aggregate advance would be Rust computation. One-shot reads
    keep the Rust fold.
11. **Per-subscription program-wire copies** (§3.2), with a shared-programs
    normalization seam if profiles show duplication mattering; likewise the
    per-tuple fresh-CTE sharing across subscriptions of one
    `(view, args, scope)` (`DESIGN-pure-pg.md` §8 mitigation 2's successor)
    is a designed optimization — one statement advancing all subscriptions
    of a tuple via a lateral over their priors — deliberately not v1 (the
    bench axis decides).
12. **Operation records move to PG** (§3.3) — logical §12.3 state; included
    here though the directive named only live views. Confirm scope.

Tensions found — none infeasible, each documented at its section:

- **Non-head frontiers, scope-deferred views, non-lowerable membership/env**
  cannot have their *evaluation* in PG (a scope fact inherited from
  `DESIGN-pure-pg.md` §7.5, not created here). Disposition: Rust-evaluated
  fresh handed into the PG diff/state path (§4.8) — state and diff remain
  backend-resident; reported, strict-refusable. "ALL computation in PG"
  holds for every lowerable view and degrades loudly elsewhere.
- **Credential verification** structurally cannot move (§8.1) — and the
  maintainer's reconciliation already assigns it to Rust.
- **Temporal events have no commit to NOTIFY from** (§5.1) — time is a pilot
  input; the pilot injects temporal events, peers are notified explicitly.
  No §14.1 conformance impact (`temporal-observation-advances-live-view`
  gates it).
- **NOTIFY is not a queue** (§6.7): a disconnected listener's notifications
  are gone. The catch-up sweep — enabled precisely because frontiers are
  PG-resident — is the recovery primitive; loss is therefore a latency
  event, never a correctness one.
