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

**Revision (maintainer corrections 1–3).** Three of §11's flagged judgment
calls were decided by the maintainer — two **against** this document's own
recommendation, one confirming it — and this revision re-derives every affected
mechanism. Verbatim:

1. *"the diffing should not be rust based, it must be purely handled by the
   backend (i.e. pure postgres routine), not pgrx. and remove the rust code
   (except if needed in the test memory backend, keep it in there in that
   case)."* — overrides judgment call 1 (the linked-Rust `liasse.sub_advance`
   face). The §12.2 diff/window/fold machine is now a **pure PostgreSQL
   routine** (SQL + PL/pgSQL, §4); the Rust machine survives only inside
   `MemoryStore`, as the in-process **test oracle** (§4.4, §7.2); parity is
   no longer by-construction but **gate-enforced** (§9). The pgrx extension
   is still used for expression *evaluation* — the `eval`/`eval_bool`/
   `eval_sort` faces that produce the fresh result — plus the codec faces of
   §4.3; only the diff stops being Rust.
2. *"Credentials MUST enter postgres, it DOES the validation. but rust may
   terminate invalid connections early by caching session checks (no actual
   secrets) so you may need a dual layer arch for that."* — overrides
   judgment call 7 ("credentials never enter PostgreSQL"). Authorization is
   now **dual-layer** (§8.1): PostgreSQL is the authoritative validator —
   the credential enters the advance/validation statement as a transient
   parameter and the built-in `$verify` (§11.3, §17.7) runs through the eval
   faces in-PG — while the Rust pilot keeps a secret-free session-check
   cache that can only reject-fast or defer, never authorize.
3. Confirmations, no longer flagged: peer connections receive row patches on
   NOTIFY wake (judgment call 8 — and the notify→advance→forward path is to
   be treated as **latency-critical**, §5.1/§6.2), and §12.3 operation
   records move into PostgreSQL (judgment call 12, §3.3).

Superseded passages below are marked, not silently rewritten, where the
original argued *for* an overridden choice; the argument is replaced by the
decision plus the mitigation that now carries its weight.

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
Rust-resident is **client transport only**: the socket, the LISTEN handle,
the client↔subscription routing map, and the credential *held* in transport
between requests — the auth re-check itself now runs IN PostgreSQL
(correction 2, §8.1; the maintainer's earlier reconciliation listed the
re-check as Rust-resident, and correction 2 supersedes that: Rust keeps the
credential and a secret-free fast-reject cache, PG does the validation).
Rust's only genuine *computation* is executing app procedures inside
mutation bodies (mandate 7 of `DESIGN-pure-pg.md` §7.5 — the one
framework-run context); everything read, validate, diff, and event is the
backend's.

Three consequences, drawn now so the rest of the document can be mechanism:

- **The diff moves into PostgreSQL — as a native PostgreSQL routine.**
  (Revised per correction 1; the first draft linked the Rust machine into the
  extension as a `liasse.sub_advance` pgrx face, arguing a SQL re-derivation
  was a second implementation and a permanent divergence surface. The
  maintainer decided the opposite: the diff is *purely handled by the
  backend*, not pgrx.) The §12.2 ordered patch must still match
  `ViewDelta::between` / `patch::diff`
  (`crates/liasse-runtime/src/{view.rs,patch.rs}`) *exactly* — that Rust
  machine remains the parity **oracle**, retained only inside `MemoryStore`
  — but the production computation is `{s}.sub_advance`, a PL/pgSQL routine
  (§4.2) invoked by the advance statement. SQL supplies the inputs (stored
  prior, fresh `scan_view` result) and stores the outputs; the accepted
  divergence surface — two implementations of one algorithm — is carried by
  the §9 gate stack (direct routine-vs-oracle property tests, not only
  end-to-end frames). Expression *evaluation* stays on the pgrx faces.
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
| per-connection `AuthSelection`s (authenticator name + credential) | §11.3: "the credential is retained only in transport state, never written to application state" — retained here, never *persisted* anywhere; per correction 2 it now also **transits into PostgreSQL** as a bind parameter of the validation statement (§8.1), which is call-lifetime existence (§11.3), not storage |
| the session-check cache: per-`AuthSelection` PG verdicts + expiries, **no secrets** (§8.1 layer 2) | derived booleans/instants PG computed; reconstructible from nothing (an empty cache only costs a PG revalidation); can reject-fast or defer, never authorize |
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
definition of "holds transport only". The two near-misses the first draft
flagged are now both decided: credentials **do** enter PostgreSQL — PG is the
authoritative validator, the pilot keeps only a secret-free fast-reject cache
(correction 2, §8.1) — and operation records move into PG (correction 3,
§3.3), so nothing auth- or §12.3-logical remains Rust-resident.

What is **deleted** from Rust (no legacy, per the pre-release rule):
`crates/liasse-surface/src/watch.rs` (`Watch`, `WatchAuthz` — the retained
`last`/`windowed` results, the per-watch frontier, the close latch: all
backend state now), the advance path of `host/barrier.rs` (hydrate → re-eval →
`ViewDelta` diff), and `connection.rs`'s `watches: BTreeMap<String, Watch>`
(becomes the routing map). `window.rs`'s and `patch.rs`/`view.rs`'s diff and
window *logic* is not deleted but **relocated** into `liasse-sub` — which,
per correction 1, only `MemoryStore` and the parity harness link (§4.4); the
production diff is the native PostgreSQL routine (§4.2). What is **kept**:
`crates/liasse-wire/` in full — the
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
    -- authorization coordinates (the credential is never STORED — it enters
    -- per statement as a bind parameter, §8.1 correction 2)
    role_name     TEXT,                     -- NULL = public subscription
    actor_wire    JSONB,                    -- resolved $actor key (wire form)
    session_wire  JSONB,                    -- resolved $session key, when the authenticator binds one
    members_admit BYTEA,                    -- the lowered §10.3 membership check program (§8.2); NULL = public
    -- the compiled dependency set (§5.4): view deps ∪ members deps ∪ session deps
    deps          TEXT[] NOT NULL,
    -- live position
    frontier      BIGINT NOT NULL,
    shape         TEXT   NOT NULL,          -- 'rows' | 'scalar' (fixed at init, §12.2)
    scalar_out    TEXT,                     -- retained scalar, canonical A.1 client-wire JSON text
    -- window (§12.2); all NULL for an unwindowed subscription
    win_size      INT,
    win_anchor    TEXT,                     -- 'first' | 'last' | 'at'
    win_anchor_occ TEXT,                    -- D.1 canonical occurrence text for 'at'
    win_slide     BOOLEAN,
    gap_sort_enc  BYTEA,                    -- frozen gap: direction-folded sort_enc of the
    gap_occ_enc   BYTEA,                    --   anchor's tuple + its occ-identity enc (§4.3)
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
    value_out  TEXT   NOT NULL,   -- the projected row in canonical A.1 client-wire JSON text (§4.3)
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
- **`value_out` is the canonical A.1 client-wire JSON text, and it is the
  equality form** (revised per correction 1). The pure-SQL diff's `update`
  test is the oracle's `same_value` — `Value` equality, which is
  *normalization-coarse*: scale-insensitive decimals, precision-insensitive
  timestamps, mathematically-compared `json` numbers. The internal tagged
  wire (`value_codec`) is incarnation-preserving for timestamps, so jsonb
  equality over it would diverge from the oracle (an extra `update` on a
  precision-variant rewrite of the same instant). The canonical **external**
  A.1 form pins exactly one spelling per value — so byte equality of
  `value_out` ≡ `Value` equality (a §9 proptest pins this equivalence per
  value class) — and it is simultaneously the exact `$value` payload the
  frame carries, produced once by the `liasse.val_out` codec face (§4.3).
  Stored as TEXT, not JSONB: jsonb storage would reject NUL-bearing
  text and normalize numbers/member order, mangling the byte-for-byte frame
  payload. No sort tuple is retained per row: the diff never orders prior
  rows (`pos` IS the prior order), the gap freezes from the *fresh* result,
  and the resume partition runs over the fresh result (§4.2) — so the first
  draft's per-row sort-tuple column in `sub_rows` is dropped (the compiled
  `$sort` *program* wire in `subscriptions` is unrelated and stays).
- **The frozen gap is stored in enc form** (`gap_sort_enc`, `gap_occ_enc`):
  the resume comparison "each `$sort` key with its direction, then the
  occurrence as the B.5 tiebreak" is, in enc space, one ascending memcmp of
  the `(sort_enc, occ_enc)` pair — precisely what `sort_enc` (§7.4 of
  `DESIGN-pure-pg.md`) and `key_enc` were built for — so the pure-SQL
  routine compares bytes instead of re-implementing Annex-B tuple
  comparison. The gap's occurrence *text* needs no column: it is always the
  anchor's own identity, already in `win_anchor_occ`.
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
plus the same opportunistic reaper. This adjacent relocation is **decided**
(correction 3 confirmed the flagged judgment call 12): operation records are
§12.3 logical state and live in PostgreSQL, never in pilot memory.

## 4. The in-PG advance: one statement, native diff

### 4.1 Shape

Per affected subscription per commit (or temporal advance), the backend runs
**one statement** that: reads the head, recomputes the view (the §7.6
`scan_view` statement of `DESIGN-pure-pg.md`, through the pgrx *evaluation*
faces), re-evaluates authorization — membership (§8.2) **and** credential
validity (§8.1, correction 2) — hands (stored prior, fresh, window state,
authorization verdict) to the **native diff routine** — `{s}.sub_advance`, a
PL/pgSQL function (§4.2, correction 1) — and writes back the new retained
result, window coordinate, and frontier, returning the wire delta. Sketch
(flat-view source; the coverage CTE composes identically):

```sql
WITH head AS (SELECT head FROM {s}.instance_meta WHERE id = 1),
fresh AS (      -- §7.6 scan_view + position + the §4.3 identity/order/output codecs
  SELECT (row_number() OVER (ORDER BY ord, occ_enc)) - 1 AS pos,
         occ, occ_enc, ord, value_out
  FROM (
    SELECT liasse.occ_text(c.key_wire)                    AS occ,      -- D.1 identity text
           c.key_enc                                      AS occ_enc,  -- identity enc (§4.3)
           liasse.eval_sort($S, c.value, c.key_wire, $E, $T) AS ord,
           liasse.val_out(                                             -- A.1 client-wire text
             liasse.eval($P, c.value, c.key_wire, $E, $T))  AS value_out
    FROM {s}.nodes c  -- … §7.6 chained-InitPlan parent, step, live filter, admit …
    ORDER BY ord, c.key_enc OFFSET $skip LIMIT $limit
  ) f
),
authz AS (      -- §8.2 membership/coverage probe ∧ §8.1 in-PG credential validation
  SELECT (EXISTS ( … §7.6 statement of the members view, actor in env … ))
     AND ( … §8.1 $verify/$check/$session statement; $credential is a bind
           parameter of THIS statement, stored nowhere … )       AS ok
),
prior AS (      -- the retained result as parallel arrays, pos order
  SELECT COALESCE(array_agg(occ       ORDER BY pos), '{}') AS occs,
         COALESCE(array_agg(win_pos   ORDER BY pos), '{}') AS wins,
         COALESCE(array_agg(value_out ORDER BY pos), '{}') AS vals
  FROM {s}.sub_rows WHERE sub_id = $id
),
verdict AS (    -- THE native advance: diff + window + scalar, one PL/pgSQL call
  SELECT ({s}.sub_advance(
           s.win_state,                  -- shape, win_*, disposition (jsonb from `subscriptions`)
           s.gap_sort_enc, s.gap_occ_enc, s.scalar_out,
           NULL,                         -- scalar_fresh: the §4.6 fold CTE on the scalar shape
           prior.occs, prior.wins, prior.vals,
           (SELECT COALESCE(array_agg(occ       ORDER BY pos), '{}') FROM fresh),
           (SELECT COALESCE(array_agg(value_out ORDER BY pos), '{}') FROM fresh),
           (SELECT COALESCE(array_agg(ord       ORDER BY pos), '{}') FROM fresh),
           (SELECT COALESCE(array_agg(occ_enc   ORDER BY pos), '{}') FROM fresh),
           COALESCE((SELECT ok FROM authz), false)    -- fail-closed on NULL (§8.2)
         )).*      -- live, delta, retain_{occ,win,val}, evict, gap_sort_enc, gap_occ_enc, scalar_out
  FROM {s}.subscriptions s, prior WHERE s.sub_id = $id
),
put AS (        -- upsert surviving + new occurrences (disjoint from `del` — §4.5)
  INSERT INTO {s}.sub_rows (sub_id, occ, pos, win_pos, value_out)
  SELECT $id, r.occ, (r.ord - 1)::int, r.win, r.val
  FROM verdict v,
       unnest(v.retain_occ, v.retain_win, v.retain_val)
         WITH ORDINALITY AS r(occ, win, val, ord)
  WHERE v.live
  ON CONFLICT (sub_id, occ) DO UPDATE
    SET pos = EXCLUDED.pos, win_pos = EXCLUDED.win_pos,
        value_out = EXCLUDED.value_out
),
del AS (        -- departed occurrences only (disjoint from `put`)
  DELETE FROM {s}.sub_rows
  WHERE sub_id = $id AND occ = ANY ((SELECT v.evict FROM verdict v))
),
bump AS (
  UPDATE {s}.subscriptions
  SET frontier = (SELECT head FROM head),
      gap_sort_enc = v.gap_sort_enc, gap_occ_enc = v.gap_occ_enc,
      scalar_out = v.scalar_out
  FROM verdict v
  WHERE sub_id = $id AND frontier = $prev   -- optimistic single-advancer guard (§8.4)
  RETURNING 1
)
SELECT (SELECT head FROM head) AS frontier,
       v.live, v.delta,                     -- frame-payload text; NULL = frontier-only/close (§4.2)
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

### 4.2 What `{s}.sub_advance` computes — the native §12.2 machine

**Superseded**: the first draft's §4.2 defined this as a pgrx face over the
relocated Rust machine ("the current Rust logic, moved verbatim, matching by
construction"). Correction 1 overrides that: the routine below is a **pure
PostgreSQL re-derivation** — PL/pgSQL, table-free, installed per instance
schema by the reconciler (§4.4) — whose semantics are pinned to the Rust
oracle by the §9 gates rather than by shared linkage. The behaviors it must
reproduce are unchanged and are restated here as *the algorithm*, not as a
pointer to Rust code, because the PL/pgSQL body is now a second
implementation and the doc must say exactly what it computes.

Signature (a reconciler-managed function, NOT an extension face):

```sql
FUNCTION {s}.sub_advance(
    state        jsonb,     -- shape ('rows'|'scalar'), win_size, win_anchor,
                            --   win_anchor_occ, win_slide, disposition ('init'|'advance')
    gap_sort_enc bytea,     -- frozen gap coordinate; NULL = none frozen yet
    gap_occ_enc  bytea,
    scalar_prior text,      -- prior scalar (A.1 client-wire text); NULL = no prior
    scalar_fresh text,      -- the §4.6 SQL fold's result (A.1 text); NULL for row shape
    prior_occ    text[],    -- prior full result, pos order (parallel arrays)
    prior_win    int[],     --   win_pos per prior row; NULL = outside the slice
    prior_val    text[],    --   value_out per prior row
    fresh_occ    text[],    -- fresh full result, pos order (parallel arrays)
    fresh_val    text[],
    fresh_sort   bytea[],   --   direction-folded sort_enc per fresh row (§4.3)
    fresh_enc    bytea[],   --   occurrence-identity enc per fresh row (§4.3)
    authorized   boolean)
RETURNS {s}.sub_out         -- (live bool, delta text,
                            --  retain_occ text[], retain_win int[], retain_val text[],
                            --  evict text[], gap_sort_enc bytea, gap_occ_enc bytea,
                            --  scalar_out text)
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE COST 1000
```

`IMMUTABLE` is truthful and load-bearing exactly as it was for the pgrx face:
the routine reads no tables — the statement feeds it — so the plan stays
gateable and the routine is directly drivable with literal arguments, which
is what the §9 direct-parity gate does. `delta` is TEXT, not jsonb: it is the
byte-exact §12.2 frame payload fragment (compact serialization, pinned member
order), built by string assembly — jsonb construction would re-sort object
members and normalize numbers, breaking byte-parity with the oracle's
`liasse-wire` rendering. `delta IS NULL` with `live` distinguishes the two
op-free forms: `(true, NULL)` = frontier-only, `(false, NULL)` = close.

**The arms**, each the oracle's pinned behavior (`patch.rs`, `window.rs`,
`view.rs` — now MemoryStore-only, §4.4):

- **`authorized = false`** (§8.1/§8.2): `live = false`, `delta = NULL`, no
  writeback arrays — the pilot deletes the subscription and emits
  `close(frontier, reason)` (§12.2: state removed the subscription's
  authority). The statement passes `COALESCE(ok, false)`: a NULL probe
  closes, never admits (fail-closed, §8.2), which is also why the routine is
  not `STRICT`.
- **Scalar/aggregate** (`scalar_fresh` = the §4.6 SQL fold's result):
  first observation or `disposition = 'init'` → `delta` = the value's A.1
  text; changed (`scalar_prior` byte-unequal to `scalar_fresh`) → the new
  value; unchanged → `(true, NULL)`. Byte equality of A.1 text ≡ `Value` equality (§3.2 note,
  §9-pinned). A present `none` scalar is the JSON `null` text — distinct
  from SQL `NULL` (= no prior), so the `Value::None`-vs-no-op distinction
  costs nothing.
- **Row-stream, windowed — window selection first** (`window.rs::select`,
  re-derived): over the FRESH full result compute the slice start:
  - `first` → 0; `last` → `greatest(n_fresh - win_size, 0)`;
  - concrete anchor: locate `win_anchor_occ` in `fresh_occ` (identity text
    equality — D.1 text is injective on identities). Present at index `a` →
    **(re)freeze the gap**: `gap := (fresh_sort[a], fresh_enc[a])`; start =
    `a`, or with `$slide` the centering clamp
    `least(greatest(a - win_size/2, 0), greatest(n_fresh - win_size, 0))`
    (integer division, the oracle's `center`). Absent → **resume at the
    frozen coordinate**: start = the count of fresh rows strictly before
    the gap in the view's total order —
    `count(*) WHERE (fresh_sort[i], fresh_enc[i]) < (gap_sort_enc,
    gap_occ_enc)` — one ascending memcmp pair-compare, because `sort_enc`
    direction-folds the `$sort` keys and `occ_enc` IS the B.5 tiebreak
    (§4.3); this is `FrozenGap::resume`'s `partition_point` expressed
    set-wise. Absent with no gap frozen is reachable only at open: the
    typed §12.2 `AbsentAnchor` refusal (a distinct routine outcome at
    `disposition = 'init'`, never a silent empty window).
  - `win_size = 0` → a valid, permanently empty slice.
  The slice is `fresh[start .. start + win_size)` saturating; the diff below
  runs prior-slice (rows with `prior_win` NOT NULL, in `prior_win` order)
  against fresh-slice, positions window-relative, evictions as `remove`.
  Unwindowed subscriptions take the same path with slice = full result.
- **Row-stream diff — the position-fold, shown** (this is the imperative
  algorithm the first draft said pure SQL could not express cleanly; PL/pgSQL
  is imperative, and the fold is ~40 lines):

  ```text
  -- inputs: P = prior slice occs (order), PV = their values,
  --         N = fresh slice occs (order), NV = their values
  -- pass 0: index the fresh slice
  pos_in_next := map occ -> slice index over N          -- one jsonb object build
  -- pass 1: identity-addressed remove/update, ONE walk over P in order
  working := empty text[]
  FOR k IN 1 .. |P| LOOP
    IF pos_in_next has P[k] THEN
      working := working || P[k]                        -- survivor, prior order kept
      IF PV[k] <> NV[pos_in_next(P[k])] THEN            -- byte equality ≡ Value eq
        emit update { $id: P[k], $value: NV[...] }      -- position-preserving
      END IF
    ELSE
      emit remove { $id: P[k] }                         -- departed
    END IF
  END LOOP
  -- pass 2: left-to-right placement over N, positions mid-application.
  -- invariant: after index i, working[1..i+1] == N[1..i+1]; an out-of-place
  -- survivor therefore sits at some j > i+1, so moving it to i never
  -- disturbs the settled prefix.
  FOR i IN 0 .. |N| - 1 LOOP
    t := N[i + 1]
    CONTINUE WHEN working[i + 1] = t                    -- already in place
    j := array_position(working, t)                     -- NULL = new occurrence
    IF j IS NULL THEN
      working := working[1:i] || t || working[i+1:]     -- insert at i (0-based)
      emit insert { $at: i, $id: t, $value: NV[i + 1] }
    ELSE
      working := working[1:j-1] || working[j+1:]        -- lift from j
      working := working[1:i] || t || working[i+1:]     -- place at i
      emit move { $id: t, $to: i }
    END IF
  END LOOP
  ```

  Removes and updates interleave in *prior* order within pass 1 (one walk,
  not two sub-passes) — that is `patch::diff`'s emission order, and byte
  parity of frames means the PL/pgSQL must reproduce the op ORDER, not just
  an equivalent op set; the §9 direct gate pins it op-for-op. `update` vs
  `rekey`: unchanged — the diff **never synthesizes `rekey`**; occurrence
  identity is key-derived (D.1), an atomic rekey presents as a distinct
  identity and diffs as `remove` + `insert`, and `rekey` remains wire
  vocabulary for a future rekey-stable-identity layer.
- **Retain/evict** (writeback, full result — not the slice): `retain_*` =
  every fresh row with its full-result `pos` (the array index) and its
  `win_pos` (slice position or NULL); `evict` = prior occurrences absent
  from the fresh result. A row that merely left the *window* stays retained
  with `win_pos = NULL` — the §4.5 disjointness discipline is unchanged.
- **Init** (`disposition = 'init'`, empty prior): `delta` = the init rows
  (the slice, in order) or the scalar value; gap frozen when a concrete
  anchor is present; `AbsentAnchor` as above.

**Cost honesty.** Pass 2 is O(|slice|) when nothing moves (the common
append/update advance: the `CONTINUE` arm is one array index per row) and
O(|slice| × moves) when rows are displaced — the same asymptotics as the
oracle's `Vec` fold, with a worse constant (interpreted PL/pgSQL, copy-on-
slice arrays). Windowed subscriptions bound |slice| by `$size`; large
UNWINDOWED subscriptions are the exposed axis, measured by the §9 bench
(advance cost vs result size) with a recorded number, not an assumption. The
designed escape, if the bench demands it, is a set-based fast path for the
no-move case (detect `working = N` after pass 1 with one array equality,
emitting updates only) — an optimization *inside* the routine, never a
second diff.

### 4.3 Occurrence identity, order, and output — the codec faces

Correction 1 removes Rust from the *diff*; it explicitly keeps the pgrx
extension for *evaluation* — and the production of the fresh result's
identity, order, and output columns is evaluation/codec work, not diffing.
Two small codec faces join the §7.4 eval faces (same discipline: `IMMUTABLE
STRICT PARALLEL SAFE`, pure functions of their arguments, shared-codec
implementations — never re-derived in SQL, which would be exactly the
second-codec mistake the diff correction consciously accepts *only* for the
diff):

```sql
-- D.1 canonical occurrence-identity text from the decodable key wire
-- (flat: the key_wire column; coverage: the accumulated per-level path)
FUNCTION liasse.occ_text(key_path jsonb) RETURNS text
IMMUTABLE STRICT PARALLEL SAFE COST 25

-- the canonical A.1 client-wire JSON text of an evaluated result cell —
-- the exact §12.2 $value / scalar payload, rendered by the SAME linked
-- `liasse-wire` renderer MemoryStore's frames go through
FUNCTION liasse.val_out(cell jsonb) RETURNS text
IMMUTABLE STRICT PARALLEL SAFE COST 50
```

Provenance of each fresh-CTE column:

- **`occ`** — `liasse.occ_text` over `key_wire` (flat) or the accumulated
  key path (coverage): the `RowId` built and rendered exactly as the
  runtime's `ViewResult` construction does today (one `Key` part per level,
  D.1/D.2). The join/identity key and the `$id` of every wire op.
- **`occ_enc`** — the memcmp-orderable identity: the `key_enc` column (flat)
  or the per-level `key_enc` bytes concatenated (coverage) — `key_enc`'s
  terminator/escape discipline makes each level self-delimiting, so
  concatenation preserves the lexicographic path order, which IS the D.1
  occurrence order the §7.6 `ORDER BY` already realizes. Pinned by a §9
  proptest (concatenated-enc order ≡ `RowId` `Ord`). Pure column work — no
  face needed.
- **`ord`** — `eval_sort`'s direction-folded `sort_enc` bytes, as in §7.6.
  Revision vs the first draft: `ord` is now also what the frozen gap
  *retains* (`gap_sort_enc` — §3.2), so the first draft's decodable
  `sort_vals` column (`liasse.eval` over the sort-key list) is **dropped
  entirely** — the §12.2 "retains its last complete sort tuple" obligation
  is satisfied in enc form, which is client-invisible and order-exact, and
  the pure-SQL routine compares it by memcmp instead of re-implementing
  Annex-B tuple comparison.
- **`value_out`** — `liasse.val_out` over `liasse.eval`'s projected cell:
  the A.1 client-wire text (equality form + ship form, §3.2). Because the
  renderer is the SAME linked `liasse-wire` code both stores use, the
  `$value` bytes inside frames are identical **by construction** on both
  stores; the dual-implementation surface correction 1 accepts is thereby
  scoped to *op selection, order, and positions* — the §4.2 fold — and
  nothing else. (A composed `eval_out` face that fuses `eval` + `val_out`
  into one call is the same optimization seam as §7.4's `eval_row`.)

### 4.4 The relocation: crate `liasse-sub` (oracle) + the routine set (production)

**Superseded in part**: the first draft relocated the Rust machine into
`liasse-sub` *so the extension could link it*. Correction 1 removes that
linkage — "remove the rust code (except if needed in the test memory
backend, keep it in there in that case)" — but the crate extraction itself
survives, because `MemoryStore` needs the machine as its contract
implementation and the §9 gates need it as the oracle:

- **`liasse-sub`** (pgrx-free, one concern: the §12.2 subscription advance
  machine) still receives: `patch::diff` + `PatchOp`,
  `ViewDelta::between`/`between_rows`, `ViewRow` (with `same_value`), the
  `Window`/`FrozenGap`/`Anchor` machinery of `window.rs`, the advance/init
  state machine of `watch.rs` (minus `WatchAuthz`, which dissolves into
  pilot routing + §3.2 columns), and the §12.2 wire encoding of a delta.
  Dependencies: `liasse-expr` (RowId, SortOrder), `liasse-value`.
- **Linked by**: `liasse-runtime` (re-export for plain `view` reads and
  frame types), `liasse-store`'s `MemoryStore` path via the opaque trait
  (§7.2), and the §9 parity harness (test-dependency, driving the oracle).
  **NOT linked by `liasse-pg-ext`** — the extension carries evaluation and
  codec faces only (§4.3), never the diff. On the production PgStore path
  the Rust machine is dead code by construction, which is what the
  maintainer ordered; it is *live* code on the MemoryStore path, so the
  no-legacy rule is satisfied — nothing retained is unreachable.
- The existing watch/window unit suites move with the code and keep passing
  unchanged — they pin the ORACLE, which is now the §9 layer-0 gate.

**The production machine** is the reconciler-managed routine set, created in
each instance schema alongside the §3.2 tables (a `Schema::routines()`
entry in the enumerable desired set, versioned by the same `SCHEMA_VERSION`
stamp, dropped with the schema — no new reconciliation mechanism, no
extension-upgrade coupling, and per-instance placement means no cross-
instance version skew):

- `{s}.sub_advance` — the §4.2 machine (PL/pgSQL);
- `{s}.sub_out` — its composite return type;
- `{s}.enc_before(a_sort bytea, a_occ bytea, b_sort bytea, b_occ bytea)` —
  the strict `(sort_enc, occ_enc)` pair compare (one SQL expression;
  bytea comparison is memcmp, collation-free);
- the §4.6 fold helpers (`{s}.dec_div`, the fold assembly).

Deliberately NOT in the extension's install script even though extensions
can carry plain SQL: the routine's version must move with `liasse-pg`'s
statements and schema (they are one design), not with the evaluator ABI —
and the reconciler already owns exactly that lifecycle. (Judgment call,
§11.)

The containment pattern shifts accordingly: `liasse-pred`-style "one crate,
two linkers, parity by construction" applies now to *evaluation and value
rendering* (§4.3); the diff is "one algorithm, two implementations, parity
by gate" (§9) — the trade the maintainer decided.

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

### 4.6 Scalar/aggregate subscriptions: the pure-SQL fold

**Superseded**: the first draft made the subscribed-aggregate fold a linked
pgrx face (`liasse.eval_fold`), rejecting SQL-native aggregation as "a
second arithmetic" on the v2-operator-table precedent. Correction 1 decides
the opposite: *the fold likewise becomes pure Postgres*. The per-row values
entering the fold still come from the eval faces (`liasse.eval` over the
aggregated field — evaluation stays pgrx); the aggregation across rows, and
the compare that yields the value delta, are SQL:

- **`count`** — `count(*)` over the admitted stream. No semantics to
  diverge.
- **`min` / `max`** — NOT SQL comparison semantics: the fresh CTE orders the
  evaluated field values by their `eval_sort` enc bytes and takes the
  first/last value — `(array_agg(v ORDER BY v_enc))[1]` and `[array_upper]`
  — so the ordering IS Annex B via the existing enc codec, and no second
  comparator exists. Ties are invisible: Annex-B-equal values render to one
  canonical wire spelling (§3.2), so the oracle's first-vs-last tie pick
  cannot differ on the wire. Empty input → `none`.
- **`distinct`** — `array_agg(DISTINCT …)` keyed on the canonical wire form
  (distinctness ≡ `Value` equality by the §3.2 injectivity), ordered by the
  enc bytes into the canonical Annex-B set order.
- **`sum`** — PG `numeric` addition over the decoded payloads. `numeric`
  addition is exact, as is the oracle's `BigDecimal` fold, so agreement is
  arithmetic identity, not luck; the result re-renders to the canonical
  minimal-scale text via `trim_scale`. The result *tag* (int vs decimal)
  follows the oracle's rule — decimal iff any input was decimal
  (`bool_or(tag = 'd')`), the empty sum is the zero of the statically-known
  field type carried in `fold_wire`.
- **`avg`** — the one genuine arithmetic re-derivation, so its rule is
  stated and gate-pinned rather than assumed: the oracle divides the exact
  sum by the count under the A.6 rule — round the exact quotient to the
  scale exposing **sixteen significant fractional digits**, `HalfUp` (ties
  away from zero), then normalize to minimal scale. PG's own `/` picks its
  result scale differently, so `{s}.dec_div(a, b)` computes it explicitly:
  derive the quotient's leading-digit place from a cheap estimate, compute
  the scaled integer quotient `trunc((a * 10^s) / b)` with its exact
  remainder (multiply-back — every step exact in `numeric`), apply the
  ties-away-from-zero correction by comparing `2·|r|` against `|b|`, and
  `trim_scale` the result. Bounded and deterministic; byte-agreement with
  `eval::decimal::divide` is a §9 proptest gate. Empty input → `none`.
  Division by zero cannot arise (`count > 0` on the non-empty arm).

Absent inputs are skipped before the fold (`FILTER (WHERE …)` on the `none`
tag), matching §7.5. The folded scalar renders through `liasse.val_out` into
the A.1 text the routine compares (§4.2's scalar arm) — so the *compare* is
byte equality, shared with the row path.

**Boundary pinned, honestly**: `Decimal::MAX_SCALE_MAGNITUDE` is 2¹⁴ =
16384, PG `numeric`'s dscale ceiling is 16383 — a stored decimal at the
exact boundary scale cannot cross the `numeric` bridge. Storage is
unaffected (payloads are text); only a fold over such a value errors — loud
(`StoreError::Backend`), never wrong, and flagged in §11 for a one-digit
bound alignment in `liasse-value` rather than a silent clamp here.

A `sum`/`avg` whose inputs are not numeric cannot type-check upstream, so
the SQL fold's numeric bridge is total over its reachable inputs. Aggregates
whose *source* does not lower, and every other scope-deferred source class,
take the fallback of §4.8. One-shot `view` reads keep the Rust fold — no
behavior change there, and no linked `eval_fold` face exists anywhere.

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
renders it through the same codecs the extension links (§4.3: `occ` text,
enc bytes, A.1 `value_out` text), and hands it to the SAME advance statement
as the fresh parallel-array arguments (parameters replacing the fresh CTE) —
the diff still runs in `{s}.sub_advance`, so **state and diff stay in
PostgreSQL**; only the evaluation ran outside, exactly as `scan_view`'s own
fallback does for one-shot reads. The Rust diff is never a fallback: on
PgStore there is exactly one diff, the native routine, on every route. The pushdown report names every
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

Correction 3 makes the *purpose* of this channel explicit: views update
**as soon as possible** — NOTIFY is not a convenience but the head of the
latency-critical notify→advance→forward path (§6.2), and its delivery
semantics (transactional, post-commit, commit-ordered, push not poll) are
exactly what makes single-digit-millisecond peer updates achievable with
zero polling anywhere.

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
mitigation 3), (ii) the role's `$members` view dependencies, (iii) the
authenticator's session/account collection dependencies, and (iv — added
for correction 2, §8.1) the keyring engine-state dependency of the
authenticator's `$verify` (accepted key versions change only through §17
rotation commits). Membership, session validity, credential validity
against state, and the view's rows can only change through a commit
touching one of those — so a dependency-disjoint commit provably cannot
have changed the authorization verdict *or* the rows, and re-using the
prior verdict (including the prior in-PG credential verdict) for the
frontier-only frame is exact, not approximate. Temporal
advances get no such shortcut: a session can expire and a bucket row can
leave with **no commit at all**, so a temporal sweep always runs the full
§4.1 advance (with its whole authz CTE — membership AND the §8.1
credential/expiry validation; the §8.1 cache may pre-empt it only in the
close direction) for every owned subscription — the
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
      out = store.sub_advance(sub_id, now, auth) -- the §4.1 statement (§8.1 validation inside)
      forward per §6.6 (fast-reject → PG verdict → frame → socket)
  on timeout: re-arm leases; reap expired subscriptions (§3.3);
              temporal sweep if the clock crossed a boundary
```

`advance_time` (the driver moving the virtual clock) runs the same body
inline with a temporal event; `sweep_all` (operator transitions, §19 import)
runs it with the head as the event. Both lose their hydrate-and-diff bodies
and become event injections.

**The notify→advance→forward path is the latency-critical path** (correction
3 confirmed judgment call 8 *and* raised immediacy to a design requirement:
views update ASAP). The loop is shaped for minimal delay, each point
deliberate: the events connection **blocks** in `wait` — no polling
interval anywhere on the event path (the timeout arm exists only for
housekeeping, and lease re-arm/reap/temporal sweep run exclusively there,
never delaying an event); a wake drains the whole pending NOTIFY batch in
one call and coalesces per instance before touching PG; the bulk skip
(§5.4a) goes first because it resolves the entire no-op population in one
round trip; per-subscription frames are forwarded **as each §4.1 statement
returns** — there is no batch barrier between advance and socket write; and
the congestion check (§6.5) runs *before* the advance so a slow socket
never adds compute latency to its neighbors. Everything between commit and
frame is then: NOTIFY delivery (sub-millisecond locally) + one bulk
statement + one advance statement per affected subscription + a socket
write. The §9 bench pins this with a recorded commit→peer-frame number
against a single-digit-millisecond budget on the reference image — a gate,
not an aspiration.

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
  conformant (§12.2 is exactly this) and strictly more useful. **Decided**:
  correction 3 confirmed the flagged call — peers receive row patches on
  wake, and the wake path is designed and gated as latency-critical (§6.2).

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

### 6.6 Forward-time authorization (fast-reject, PG verdict, close)

**Superseded**: the first draft placed credential verification on the pilot
("the §12.2 re-check that CANNOT live in the database"). Correction 2
reverses this — credentials enter PostgreSQL and PG does the validation
(§8.1) — so the forward path becomes:

1. **Fast-reject (Rust, layer 2, optional)**: before running the advance at
   all, the pilot consults its secret-free session-check cache (§8.1): a
   subscription whose cached expiry instant has passed, or whose last PG
   verdict within TTL was *invalid*, is closed/terminated immediately — no
   PG round trip, which is the cache's entire purpose ("rust may terminate
   invalid connections early"). Anything else — cache miss, TTL lapse,
   valid-so-far — proceeds; the cache never green-lights.
2. **The verdict (PostgreSQL, layer 1, authoritative)**: the §4.1 statement
   itself re-evaluated everything §12.2 lists — membership, scope coverage,
   session validity, AND credential verification (`$verify`/`$check`
   through the eval faces, the credential a bind parameter, §8.1) — at the
   statement's own MVCC snapshot, i.e. at exactly the frontier the frame
   reports. The returned `live`/`authorized` IS the §12.2 re-check.
3. **The close** (`live = false`, or step 1 fired): `sub_close(id)` + the
   `close(frontier, reason)` frame (§12.2, §11.7 — revocation and expiry
   deny the very next outgoing frontier); the cache records the verdict.

The CLOSE decision thus *executes* on the pilot (it owns the socket) but
*decides* in PostgreSQL — every forwarded frame is covered by an in-PG
validation at its own frontier, and the pilot's only unilateral power is to
refuse or terminate early, never to serve.

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
/// liasse_sub::SubPrograms (§4.4): init/advance faces for MemoryStore; for
/// PgStore only the DECLARATIVE parameters (shape, window config, fold spec —
/// the §3.2 columns) cross the boundary, never a serialized Rust machine —
/// the PG-side machine is the native routine (§4.2). Mirrors ViewProgram.
pub trait SubMachine { /* init / advance faces + declarative window/fold params */ }

/// What a subscription is: the evaluated read (reused from scan_view) plus
/// the §12.2 machine, the §8.2 membership probe, and the §5.4 dependency set.
pub struct SubscriptionSpec<'a> {
    pub source: ViewSource<'a>,
    pub program: &'a dyn ViewProgram,
    pub skip: Option<u64>, pub limit: Option<u64>,
    pub machine: &'a dyn SubMachine,          // window params, shape, fold
    pub membership: Option<&'a dyn ViewProgram>, // lowered §10.3/§10.5 probe (None = public)
    pub verify: Option<&'a dyn ViewProgram>,  // compiled §11.3 $verify/$check (§8.1; None = public)
    pub deps: Vec<CollectionPath>,
    pub authz: SubAuthz,                      // role name, actor/session keys — no stored credential
}

fn sub_open(&mut self, spec: SubscriptionSpec<'_>) -> Result<SubOpened, StoreError>;
    // SubOpened { id, frontier, first: SubDelta }  — init rows / scalar /
    // window rows, or the typed AbsentAnchor refusal (§12.2)
fn sub_advance(&mut self, id: SubscriptionId, input: AdvanceInput<'_>)
    -> Result<SubAdvanced, StoreError>;
    // AdvanceInput { now: Timestamp, auth: &AuthSelection,
    //                fresh_override: Option<&ViewResultWire> }
    //   — `auth` carries the transport-held credential PER CALL for the §8.1
    //     in-PG validation (a bind parameter downstream, never stored;
    //     correction 2); fresh_override is the §4.8 fallback seam
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

`SubDelta` is contract-level *data*, and — revised for correction 1 — it
carries the **rendered** §12.2 frame payload (byte-exact text), not
re-encodable structure: PgStore returns the routine's `delta` text verbatim,
MemoryStore renders `liasse-sub`'s structured ops through `liasse-wire` at
the store boundary. Both pilots forward bytes without recomputation, and the
§9 byte-identity gate compares exactly what ships. (The structured op
vocabulary remains `liasse-sub`-internal + oracle-facing.)

### 7.2 `MemoryStore` — the same surface, in-process

- Subscription state: an owned map `SubscriptionId → SubState` (prior rows,
  frontier, machine state) **inside the store** — backend state held by the
  backend, exactly the reclassification; no interior mutability (all methods
  taking state are `&mut self`, matching the existing contract style —
  `sub_open`/`sub_advance`/`sub_close` are `&mut` on both stores since they
  write backend state).
- Advance: `scan_view` on itself (the in-Rust evaluation) → the
  `liasse-sub` machine — which after correction 1 lives HERE and only here
  (§4.4): MemoryStore is the Rust machine's home and the parity oracle's
  substrate; membership and `$verify` via the same lowered probe programs
  evaluated in-process. Byte-identity with PgStore's native routine is no
  longer by shared linkage — it is enforced by the §9 gate stack (value
  rendering still IS shared linkage, `liasse-wire`; op selection is the
  gated dual implementation).
- Events: commit pushes `StoreEvent` onto a `std::sync::mpsc` channel whose
  receiver is the `Events` handle (a std primitive conveying events out of
  `&mut` commit — not an interior-mutability structure of our own state;
  noted for the AGENTS.md sentence Phase 10b lands, §10.1). Temporal events
  are injected by the pilot on both stores identically.

With this, "Rust pilots the generic backend" is literal: the pilot loop of §6
compiles once against `InstanceStore` and runs unmodified over either store —
which is also what makes the §9 parity gate a true end-to-end oracle.

## 8. Authorization and coherence

### 8.1 The §12.2 re-evaluation: dual-layer, PostgreSQL authoritative

**Superseded**: the first draft ruled "credentials never enter PostgreSQL",
reading §11.3's transport-only rule as outranking "all state in Postgres",
and kept `$verify` on the pilot. Correction 2 decides the opposite:
*"Credentials MUST enter postgres, it DOES the validation. but rust may
terminate invalid connections early by caching session checks (no actual
secrets)."* The architecture is dual-layer.

**Layer 1 — PostgreSQL, authoritative.** §12.2 requires re-evaluating, at
every outgoing frontier: authentication, session validity, scoped role
membership, surface availability, output projection. ALL of them are now
Postgres computations inside the §4.1 statement's single MVCC snapshot:

| Check | How, in-PG |
|---|---|
| output projection | the fresh `scan_view` (the projection wire) — unchanged |
| role membership (§10.3) | the lowered `$members` probe (§8.2) — unchanged |
| scoped coverage (§10.5) | the coverage re-walk `EXISTS` (§8.2) — unchanged |
| session validity via state (`$bucket` activity, revocation rows, §11.7) | the `$session` row resolution + auth `$check` conditions, evaluated through the eval faces over the same snapshot |
| **credential verification** | **the authz CTE evaluates the authenticator's `$verify`** — built-in-only + native §17.7 keyring verification (`cose.verify(/session_keys, $credential)`), through the eval faces: the built-in namespaces are linked into the extension (mandate 6), the accepted public key versions are committed engine state readable in-PG, and the credential enters as a **bind parameter** of the statement |
| surface availability | definition head observed by the same snapshot (a redefinition is a commit); compile-time half stays on the pilot — it is compilation, not authorization |

This is exactly what Phase 7b's built-in-only `$verify` rule bought (SPEC
§11.3: "`$verify` is a database-evaluated expression … restricted to the
language operators, the built-in namespaces, and native keyring
verification"): because no app code can appear in `$verify`, the database
CAN run it — and now it does, for every open, every advance, every resume.
The one v1-executor choice `DESIGN-pure-pg.md` §7.5 left framework-side
("`$verify` execution likewise stays framework in v1") is hereby revised
for this path: per-frontier and per-request session validation executes
in-PG. The credential is *never stored*: it exists as a statement parameter
for the statement's lifetime — which is §11.3's own model ("external request
arguments, including `$credential`, exist for the call lifetime"; the
prohibition is on writing it to application state, and nothing writes it).
Deployment note, load-bearing: the image config must keep bind parameters
out of the log surface (`log_parameter_max_length = 0`,
`log_parameter_max_length_on_error = 0`), so "enters Postgres" never becomes
"enters the log files".

**No conflict with mandate 7** (state it, so the two rules cannot be read
against each other): custom credential verification — webauthn, OIDC,
password hashing, API-key exchange — still runs in mutation BODIES (§11.5:
the auth mutation verifies once through the app verifier namespace, records
the session, mints a native token). "PG does the validation" refers to the
*ongoing* built-in/native validation of that native token and its session
state at every subsequent request and frontier — precisely the part §11.3
restricts to built-ins so that it CAN be a database evaluation. App crypto
in pure SQL is neither required nor permitted; the two rules are two halves
of one design: framework verifies *once* (mutation), Postgres validates
*always* (built-in `$verify`).

**Layer 2 — the Rust pilot's session-check cache, an optimization that can
only refuse.** Per retained `AuthSelection` the pilot caches **results**,
never material: `{ verdict: bool, verdict_frontier, expires_at, checked_at,
ttl }` — `expires_at` is the token/session expiry instant PG reported (a
derived, non-secret instant), `verdict` the last in-PG validation outcome.
No credential bytes, no token, no key material, no claims. Uses:

- **terminate early**: a connection/subscription whose `expires_at` has
  passed on the pilot's clock is closed without a PG round trip — sound
  unilaterally, because expiry is *monotone* (an expired token never
  re-validates; §11.7 "a later request using that session fails");
- **reject fast**: within `ttl` of a PG *invalid* verdict, new frames/
  requests on that selection are refused without PG — sound because the
  worst staleness direction is refusing a since-revalidated client
  (availability, retried through PG), never serving an invalid one;
- **defer to PG**: everything else — cache miss, TTL lapse, valid-so-far —
  runs the in-PG validation (which, riding the §4.1 statement, costs no
  extra round trip on the advance path anyway).

Invalidation needs no new machinery: state-driven revocation reaches the
pilot as the NOTIFY it already receives — the §5.4 `deps` union includes
the authenticator's session/account collections and the keyring engine
state, so a revoking or rotating commit forces the full advance (with its
in-PG validation) and the cache entry is overwritten by the fresh verdict;
temporal expiry is the pilot's own clock against `expires_at`; and the
short TTL bounds every other staleness. **The guarantee, stated as an
invariant**: the cache can reject-fast or defer — it can never authorize.
Every row-carrying frame is computed by a §4.1 statement that validated the
credential at that frame's own frontier; a frontier-only frame (the §5.4
skip) rides the skip's exactness argument — the commit provably touched
nothing the verdict depends on (deps clause (iv)), so the prior in-PG
verdict still IS the verdict at the new frontier. PostgreSQL remains
authoritative unconditionally; no Rust code path ever upgrades a
subscription's authority.

The stored authz columns (§3.2) are unchanged: *resolved coordinates*
(role, actor key, session key) — derived identities the probes need, still
never the credential. The optimization seam if per-advance `$verify` crypto
ever dominates a bench: a per-backend memo inside the extension keyed by
(credential hash, keyring version epoch) — still in-PG, still
PG-authoritative, recorded not built.

### 8.2 The in-PG membership probe (one conjunct of the authz CTE)

The role's `$members` view (§10.3) is lowered once at open into
`members_admit` (a `ViewProgram` whose consumption is "does the actor's exact
row identity occur at least once"): within the advance statement's `authz`
CTE (§4.1 — membership ∧ coverage ∧ the §8.1 credential validation, one
conjunction) the membership conjunct is an `EXISTS` over the §7.6 statement
with the actor key in env — one indexed probe, short-circuiting on the first
occurrence. A scoped subscription
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

**Re-founded for correction 1.** The first draft's parity story was
"same linked code, so the gates guard only transport". That premise is gone:
the production diff (`{s}.sub_advance`, PL/pgSQL) and the oracle
(`liasse-sub`, MemoryStore) are now two implementations of one pinned
algorithm — exactly the divergence surface the first draft argued against
and the maintainer accepted. The parity layer is therefore the LOAD-BEARING
guarantee of this design, structured so that a divergence cannot hide:

- **Layer 0 — the oracle is pinned**: the existing watch/window/diff unit
  suites move to `liasse-sub` with the code and stay green unmodified (diff
  order, window placement, gap freeze/resume, slide clamp, scalar no-op,
  size-0 window, eviction-as-remove). The oracle's meaning never floats.
- **Layer 1 — DIRECT routine-vs-oracle parity, the headline gate** (new):
  the routine is `IMMUTABLE` and table-free precisely so it can be driven
  with literal arguments — a proptest harness generates `(state, gap,
  scalar, prior, fresh, authorized)` instances, runs `liasse_sub`'s machine
  in-process and `SELECT {s}.sub_advance(…)` on the image with the SAME
  inputs, and asserts **byte-identical** outputs: the delta text op-for-op
  (order included — pass 1's prior-order interleave and pass 2's greedy
  placement are behavior, not implementation detail), `retain_*`/`evict`
  sets, gap coordinate, scalar. Generators cover the full §9 adversarial
  value corpus of `DESIGN-pure-pg.md` (NUL text, scale-variant decimals,
  absent optionals, mixed-direction sorts, ties) × structural edges
  (moves, rekey-as-remove+insert, mass eviction, frozen-gap freeze/resume
  across descending sorts, reappearance, slide clamps at both bounds,
  size-0, empty↔nonempty, `Value::None` scalar vs no-op, unauthorized).
  Found divergences become pinned regression cases. **Plus the intrinsic
  property, checked independently on BOTH implementations**: applying the
  emitted ops to the prior slice (positions interpreted mid-application)
  reproduces the fresh slice exactly — the §12.2 obligation itself — so
  even an oracle bug mirrored by the routine cannot pass silently.
  Sub-pins, one per SQL-side re-derivation or equivalence the design leans
  on: `{s}.enc_before` ≡ `SortOrder::is_before` (random tuple/direction/
  occurrence pairs); `{s}.dec_div` ≡ `eval::decimal::divide` (the A.6
  16-significant-fractional-digit HalfUp rule); each §4.6 fold ≡
  `aggregate::combine` (incl. empty-input identities and the int/decimal
  tag rule); `val_out` byte equality ≡ `Value` equality per value class
  (the §3.2 injectivity — decimals at variant scales, timestamps at
  variant precisions, `json` number spellings); concatenated occ-enc order
  ≡ `RowId` `Ord` (coverage paths at mixed depths).
- **Layer 2 — end-to-end store parity**: one scripted driver runs identical
  scenario scripts (open/commit/advance/window/reauth/resume/close
  interleavings) against `MemoryStore` and `PgStore` **through the identical
  pilot code** (§7.2), asserting byte-identical frame streams per
  subscription. With layer 1 owning the machine, this layer guards the
  composition: the §4.1 statement's array assembly and `pos` round-trip,
  occ-text identity through the codec faces, the writeback CTEs, the
  optimistic guard, the dep-filter statements, the SQL fold assembly, the
  §8.1 authz CTE and its close timing, NOTIFY payload encode/decode.
  Scenario edges: windowed first/last/anchored/slide; frontier-only skips;
  credential-expiry and membership-revoked close at the exact frontier;
  lease-expiry reap; reconnect same-frontier patch continuation; PG-crash
  truncation ⇒ fresh init. Expected frames hand-derived (AGENTS.md:
  externally deducible).
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
  number that justifies this design), split same-connection vs
  **commit→peer-frame over NOTIFY against the single-digit-millisecond
  budget of §6.2** (correction 3's immediacy requirement, recorded as a
  gate); the §4.1 statement vs the identical hand-written SQL (the near-raw
  gate, extension faces on both sides); **the PL/pgSQL routine vs the Rust
  machine on identical inputs across result sizes and displacement ratios**
  (the §4.2 cost-honesty axis — the number that decides whether the no-move
  fast path is built); the SQL fold vs the Rust fold across input sizes;
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
| **10a** | Extract `liasse-sub` (diff/window/machine + wire types moved from `liasse-runtime`/`liasse-surface`, §4.4); runtime/surface re-point; watch/window suites move with it | workspace green; zero behavior change; layer-0 gate = the moved suites (the oracle pinned, §9) |
| **10b** | Contract surface (§7.1) + `MemoryStore` implementation (§7.2) + the pilot rewrite of `liasse-surface` (loop §6.2, barrier §6.3, resume §6.4, backpressure §6.5, close §6.6) over the contract; **DELETE** `watch.rs`, the barrier advance path, `Connection.watches` (§10.2); `operations` relocation designed-in behind the contract | full §12 corpus + surface suites green on MemoryStore through the new pilot; grep-provable: no retained result/frontier/window field outside stores |
| **10c** | PG subscription schema (SCHEMA_VERSION 5: `subscriptions`, `sub_rows`, `operations`, `commit_log.touched`), reconcile entries incl. **the routine set** (`{s}.sub_advance` + `sub_out` + `enc_before` + `dec_div` + fold helpers — `Schema::routines()`, §4.4), leases + reap, open-time sweep; `sub_open`/`sub_close`/`sub_adopt`/bulk statements; **the §9 layer-1 DIRECT routine-parity gate** (the routine is table-free, so it gates before any statement composes it); EXPLAIN gates (15)(16) | layer-1 parity green (routine ≡ oracle, adversarial generators + intrinsic property); corpus green on PgStore with advances fresh-computed via the §4.8 route (state and DIFF PG-resident, evaluation not yet pushed); reap/lease gates green |
| **10d** | The §4.3 codec faces (`occ_text`, `val_out` — the extension's only growth; it never links `liasse-sub`); the §4.1 statement incl. the §8.1 authz CTE (in-PG `$verify`), env-prefetch CTE (§4.7), the §4.6 SQL fold assembly, and writeback discipline (§4.5); writer NOTIFY + `PgStore` events (§5); pilot on real events | layer-2 parity green (identical frames both stores, adversarial corpus); event gates green; EXPLAIN gate (14) green; §12 corpus green end-to-end on the image |
| **10e** | Multi-pilot hardening (lease takeover §6.4, catch-up sweep §6.7, optimistic-guard race gate §8.4); the §8.1 layer-2 session-check cache (fast-reject/defer + TTL); benchmarks recorded incl. the §6.2 latency budget | race + catch-up gates green; cache-invariant gate green (§8.1: the cache only rejects or defers — every row-carrying frame traces to an in-PG validation at its frontier); bench report committed; pushdown report lists every §4.8-routed subscription |

Each sub-phase lands green (corpus + parity + gates), per the standing
discipline. 10c before 10d is the same attribution ordering as Phases 7→9,
now with a sharper cut: the ROUTINE is gated directly against the oracle in
10c, before any statement composes it — so a 10d divergence is attributable
to statement assembly, codec faces, or events, never to the diff algorithm,
and a 10c divergence is the diff algorithm by construction. The layer-2
cache lands last (10e) because it is pure optimization: every earlier phase
is correct with an empty cache.

### 10.2 Deleted vs kept — the no-legacy inventory

**Deleted outright** (pre-release, no deprecation): `liasse-surface/src/
watch.rs` (`Watch`, `WatchAuthz`); `host/barrier.rs`'s hydrate→re-eval→diff
advance body (the file becomes the pilot's forward/close step; the Phase-4
hydration sharing survives only inside the §4.8 fallback evaluation);
`Connection::watches` and the `Watch` accessors on `SurfaceHost`
(`read_view`/`read_window` re-serve from the store — a test convenience read,
now `sub_`-backed); `window.rs` and the diff halves of `view.rs`/`patch.rs`
*as locations* (logic moves to `liasse-sub`, §4.4 — one Rust home, no copy
left behind in runtime/surface).

**Kept — with the correction-1 relocation stated precisely**: the Rust
diff/window machine (`liasse-sub`) is **MemoryStore-only** — linked by
`MemoryStore`'s contract implementation and the §9 parity harness, NOT by
the extension, NOT reachable from any PgStore path (grep-provable: no
`liasse-sub` dependency edge from `liasse-pg`/`liasse-pg-ext`). The
production diff is the reconciler-installed PL/pgSQL routine set (§4.4).
The algorithm therefore exists **twice, by maintainer decision**: the
PL/pgSQL implementation ships, the Rust implementation is the in-process
test oracle and MemoryStore's §12.2 engine — both live code with a
load-bearing job, which is how the no-legacy rule is satisfied (nothing
retained is unreachable; nothing shipped is duplicated *within* one store's
path). Also kept: `crates/liasse-wire/` in full (`WireStore` — the client
fold stays the client's, untouched; the frame renderer additionally links
into the extension for `val_out`, §4.3); `ViewResult`/`ViewRow` as the
result type of plain `view` reads and the decode vocabulary; `PatchOp` as
wire vocabulary (re-exported from `liasse-sub`); the §19.2
replay/`snapshot(frontier)` machinery (it is the §6.4/§4.8 fallback's
substrate); `resolve_view`/`authorize_view`/denial-uniformity logic in
`host/call.rs` (open-time authorization is untouched by this design).

## 11. Judgment calls and tensions

### 11.1 Decided by the maintainer (corrections 1–3; no longer flagged)

- **1 — Diff-in-PG mechanism — DECIDED, against the draft's recommendation.**
   The draft chose the linked Rust machine, arguing pass-2's mid-application
   positions make a pure-SQL rendering "a second diff, permanently gated
   against the first" (the v2-operator-table precedent). The maintainer
   ruled: *pure postgres routine, not pgrx; remove the Rust code except in
   the test memory backend*. The design now carries that: the PL/pgSQL
   position-fold (§4.2 — PL/pgSQL is imperative, so the fold is expressed
   directly, not contorted into window functions), the Rust machine as
   MemoryStore-only oracle (§4.4), and the divergence risk owned by the §9
   layer-1 direct gate plus the intrinsic apply-property. The reasoning
   shifted, not vanished: the diff is a small, closed, frozen algorithm
   with a machine-checkable correctness property — unlike an open-ended
   operator table, its dual implementation is exhaustively property-testable,
   which is what makes the maintainer's trade (backend purity over
   single-implementation) safe to carry.
- **7 — Credentials — DECIDED, against the draft's recommendation.** The draft
   kept credentials out of PostgreSQL; the maintainer ruled they MUST enter
   and PG does the validation, with Rust limited to secret-free early
   termination. Carried as the §8.1 dual-layer architecture: in-PG
   `$verify`/`$check`/`$session` in the advance statement's authz CTE
   (reinforcing Phase-7b's built-in-only `$verify` — the restriction that
   makes in-PG execution possible), a results-only pilot cache that can
   reject-fast or defer but never authorize, and the explicit
   reconciliation with mandate 7 (app verifiers stay in mutation bodies;
   PG validates the native token, always).
- **8 — Peer connections receive row patches on NOTIFY wake — CONFIRMED**
   (§6.3), with immediacy promoted to a requirement: the
   notify→advance→forward path is latency-critical, designed for minimal
   delay (§6.2) and gated by a recorded commit→peer-frame budget (§9).
- **10 — The subscribed-aggregate fold — superseded by correction 1.** The
    draft's linked `eval_fold` face is gone; the fold is pure Postgres
    (§4.6: SQL aggregation over eval-face values, enc-ordered min/max,
    explicit A.6 division for avg), gate-pinned against the Rust fold.
- **12 — Operation records move to PG — CONFIRMED** (§3.3): §12.3 logical
    state, LOGGED, TTL-reaped.

### 11.2 Still flagged (unchanged by the corrections)

- **2 — NOTIFY channel granularity: per-instance schema channel** (§5.2), not
   per-collection, not global.
- **3 — Payload: seq + touched, degrading to seq-only on ~8 KB overflow, with
   the durable `commit_log.touched TEXT[]` column backing exactness**
   (§5.3). The zero-schema-growth alternative (conservative advance on
   overflow) is workable if the column is unwanted.
- **4 — Writer-issued NOTIFY, not triggers** (§5.1); trigger seam recorded.
- **5 — Subscription durability class: UNLOGGED + leases + open-time sweep**
   (§3.3); PG-crash ⇒ fresh-init resumes (conformant). The LOGGED
   alternative buys patch-continuity across a PG crash at WAL cost on the
   hottest write path — judged not worth it while §12.2 blesses re-init.
   `operations` is LOGGED (at-most-once must survive).
- **6 — GC policy**: lease period (suggested 30 s re-arm / 90 s expiry) and the
   deferral horizon (§6.5) are host policy knobs, defaults boring, flagged.
- **9 — Env classes** (§4.7): stable env stored; state-dependent hoisted
   `/`-reads served by an env-prefetch CTE (v1: plain collection reads);
   engine/temporal values as advance parameters; non-lowerable hoisted
   entries ⇒ §4.8 fallback, reported. The prefetch is new compiler surface —
   the one place this design grows Phase-7 scope.
- **11 — Per-subscription program-wire copies** (§3.2), with a shared-programs
    normalization seam if profiles show duplication mattering; likewise the
    per-tuple fresh-CTE sharing across subscriptions of one
    `(view, args, scope)` (`DESIGN-pure-pg.md` §8 mitigation 2's successor)
    is a designed optimization — one statement advancing all subscriptions
    of a tuple via a lateral over their priors — deliberately not v1 (the
    bench axis decides).

(Entries 7, 8, 10, and 12 of the first draft moved to §11.1 — decided;
numbering is kept stable so cross-references stay valid.)

### 11.3 Newly raised by the corrections (flagged)

13. **Where the routine set lives**: reconciler-managed per-instance-schema
    functions (chosen, §4.4 — versioned with `SCHEMA_VERSION`, dropped with
    the instance, no extension-upgrade coupling) vs the extension's SQL
    install script (one copy per database, but couples the diff's version
    to the evaluator ABI). Confirm the reconciler placement.
14. **The `occ_text`/`val_out` codec faces stay pgrx** (§4.3). Correction 1
    says the *diffing* must not be Rust; identity rendering (D.1) and
    canonical client-wire output (A.1) are codec/evaluation work feeding
    the fresh result, kept on the extension exactly so they are never
    re-derived in SQL. If the maintainer intends "not pgrx" to cover these
    too, each becomes a third implementation of a canonical codec — flagged
    as the one boundary call this revision takes, not silently.
15. **The SQL fold's arithmetic pins** (§4.6): `{s}.dec_div` must mirror
    A.6's 16-significant-fractional-digit HalfUp division exactly
    (gate-pinned); and the `numeric` bridge has a one-digit boundary —
    `MAX_SCALE_MAGNITUDE` 16384 vs PG dscale 16383 — that errs loudly today
    and deserves a bound alignment in `liasse-value` (a separate one-line
    change, not taken here).
16. **Byte-equality-as-`Value`-equality is now load-bearing** (§3.2/§4.3):
    the diff's `update` test and the scalar no-op ride the A.1 canonical
    form's injectivity per value class, pinned by a dedicated §9 proptest.
    Any future value class whose canonical form is looser than its equality
    must extend that gate before it ships.
17. **Layer-2 cache policy** (§8.1): the TTL default (suggested: one lease
    period), and the deployment requirement that bind parameters stay out
    of PG logs (`log_parameter_max_length* = 0` in the image). Confirm
    both.
18. **PL/pgSQL diff cost on large unwindowed views** (§4.2): same
    asymptotics as the oracle, worse constant; the §9 bench decides whether
    the in-routine no-move fast path is built. Flagged so a latency
    regression is a known trade, not a surprise.

### 11.4 Tensions

Tensions found — none infeasible, each documented at its section:

- **Non-head frontiers, scope-deferred views, non-lowerable membership/env**
  cannot have their *evaluation* in PG (a scope fact inherited from
  `DESIGN-pure-pg.md` §7.5, not created here). Disposition: Rust-evaluated
  fresh handed into the PG diff/state path (§4.8) — state and diff remain
  backend-resident; reported, strict-refusable. "ALL computation in PG"
  holds for every lowerable view and degrades loudly elsewhere.
- **Credential verification** — the first draft listed this as structurally
  Rust-bound; correction 2 dissolved the tension the other way: verification
  moves in-PG (§8.1), which is *possible* precisely because Phase-7b made
  `$verify` built-in-only. What remains genuinely Rust-bound is only the
  socket (executing the close) and the once-per-login app verifier inside a
  mutation body (mandate 7) — neither is per-frontier validation.
- **The dual-implementation diff** (correction 1) is the one accepted
  standing tension: two implementations of the §12.2 fold, held equal by
  the §9 layer-1 gate + intrinsic property rather than by construction. It
  is bounded (a closed ~40-line algorithm), loud (byte-level gates on every
  CI run), and deliberate (the maintainer's trade for a pure-backend diff).
  No §12.2 semantics was found that the PL/pgSQL rendering cannot express
  identically to the oracle — the two candidate blockers dissolved under
  scrutiny: Annex-B order comparisons reduce to memcmp over the existing
  enc codecs (§4.3), and normalization-coarse value equality reduces to
  byte equality of the canonical A.1 form (§3.2). No infeasibility.
- **Temporal events have no commit to NOTIFY from** (§5.1) — time is a pilot
  input; the pilot injects temporal events, peers are notified explicitly.
  No §14.1 conformance impact (`temporal-observation-advances-live-view`
  gates it).
- **NOTIFY is not a queue** (§6.7): a disconnected listener's notifications
  are gone. The catch-up sweep — enabled precisely because frontiers are
  PG-resident — is the recovery primitive; loss is therefore a latency
  event, never a correctness one.
