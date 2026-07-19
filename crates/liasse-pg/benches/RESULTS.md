# `liasse-pg` benchmark — pure-PostgreSQL substrate

`liasse-pg` holds **no in-memory projection**: every contract read is a PostgreSQL
query served from the r2d2 read pool (see `DESIGN-pure-pg.md`). The prior
projection-era matrix (`backend_vs_native_matrix`) and its "projection vs SQL"
numbers measured an architecture that no longer exists and were deleted.

## `backend_vs_raw` — per-op overhead over raw SQL

```
cargo bench -p liasse-pg --bench backend_vs_raw
```

Each contract op — `row` (point), `scan` (collection), `scan_subtree`,
`snapshot` (log-fold, and the `frontier == head` fast path materialized from
`nodes`), `head`, and `commit` — is timed against the **identical hand-written SQL
on the same pooled connection**. This is the AGENTS.md *"near-raw-PostgreSQL
overhead"* gate: both sides are now SQL, so it measures the backend's per-op
overhead over hand-issued SQL — a same-substrate race, not the old
projection-vs-RAM comparison.

Expectations that hold by construction:
- reads are one indexed statement each (the `index_coverage_pg` gate pins no
  Seq Scan), so per-op overhead is decode + pool-checkout, near parity with raw SQL;
- `snapshot(head)` uses the O(state) `nodes` fast path, not the O(history) log fold
  (crossover recorded by the bench; equivalence pinned by
  `node_tree_consistency::head_fast_path_equals_log_fold`);
- `commit` is faster than the projection era — no dual-write projection upkeep.

## The headline benchmark — Phase 10

The performance comparison that actually matters — **pushed-down query evaluation
inside Postgres (the extension) vs hydrate-then-evaluate-in-Rust** — lands with the
extension in Phase 10 (`DESIGN-pure-pg.md` §10), together with the subtree-lateral
cost axis (§9). This substrate benchmark is the pre-pushdown baseline it is measured
against.
