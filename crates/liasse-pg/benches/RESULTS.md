# `liasse-pg` vs native PostgreSQL — comparative benchmark matrix

Produced by `benches/backend_vs_native_matrix.rs`. Medians of a self-timed pass
(alongside the criterion groups), one machine, warm projection. Reproduce with:

```
cargo bench -p liasse-pg --bench backend_vs_native_matrix
```

## What is being compared (read this before the numbers)

`PgStore` answers every contract read (`row`, `scan`) from an **in-memory
projection** rebuilt from the durable `nodes` tree on open; PostgreSQL is the
durable **write** path. The store is **semantics-free** — it has no `$view`,
filter, join, or aggregate of its own — so the runtime serves a complex query by
`scan`ning the collection(s) from the projection and doing the filter / join /
aggregate / view-projection **in Rust**. That whole path is the "backend" column.

So the axes are not symmetric, and each row says which it is:

- **Read classes (1–7): `projection vs SQL`.** Backend = a RAM `BTreeMap` scan +
  in-Rust computation; raw SQL = the round trip a hand-written query would issue,
  index-served where an index applies. The ratio shows the projection's **saving
  or overhead**, *not* a same-substrate race.
- **Write class (8): `like-for-like SQL`.** Backend = the admission transaction
  (head lock, log append, node insert, head bump); raw = the identical four
  statements by hand on an equally-populated twin schema. This is the one genuinely
  apples-to-apples axis.

Native-SQL plans were captured at the large size to confirm fair baselines:
point/ordered/range = `Index Scan`; non-key filter = parallel `Seq Scan` (no index
on that column, as PG itself would); reference join = `Hash Join` + `Index Only
Scan`. No accidental seq-scan inflates a comparison.

## Results

`ratio = backend ÷ raw`. `<1` backend faster; `>1` backend slower.

### Small — 1,000 rows

| class | basis | backend | raw SQL | ratio | verdict |
|---|---|---:|---:|---:|---|
| 1 point lookup | projection vs SQL | 231 ns | 83.30 µs | 0.003 | **projection-win 360×** |
| 2 ordered scan | projection vs SQL | 260 µs | 1.036 ms | 0.25 | projection-win 4.0× |
| 3 range scan | projection vs SQL | 319 µs | 252 µs | 1.26 | overhead 1.3× |
| 4 non-key filter | projection vs SQL | 316 µs | 291 µs | 1.09 | near-parity |
| 5 reference join | projection vs SQL | 380 µs | 676 µs | 0.56 | projection-win 1.8× |
| 6 aggregate group-by | projection vs SQL | 372 µs | 357 µs | 1.04 | near-parity |
| 7 view filter+project | projection vs SQL | 332 µs | 292 µs | 1.14 | near-parity |
| 8 commit insert | like-for-like SQL | 12.47 ms | 11.97 ms | 1.04 | near-parity |

### Large — 100,000 rows

| class | basis | backend | raw SQL | ratio | verdict |
|---|---|---:|---:|---:|---|
| 1 point lookup | projection vs SQL | 331 ns | 90.45 µs | 0.004 | **projection-win 273×** |
| 2 ordered scan | projection vs SQL | 76.53 ms | 94.82 ms | 0.81 | projection-win 1.2× |
| 3 range scan | projection vs SQL | 82.78 ms | 217 µs | **380×** | **overhead 380×** |
| 4 non-key filter | projection vs SQL | 83.83 ms | 14.23 ms | 5.89 | overhead 5.9× |
| 5 reference join | projection vs SQL | 111.6 ms | 26.31 ms | 4.24 | overhead 4.2× |
| 6 aggregate group-by | projection vs SQL | 89.34 ms | 27.59 ms | 3.24 | overhead 3.2× |
| 7 view filter+project | projection vs SQL | 100.4 ms | 15.36 ms | 6.54 | overhead 6.5× |
| 8 commit insert | like-for-like SQL | 2.50 ms | 2.55 ms | 0.98 | near-parity |

## Reading the results

**Where the backend wins, decisively and at any scale:**

- **Point lookups** are ~**270–360× faster** than a native query — a `BTreeMap`
  hit in RAM (231–331 ns, ~flat in dataset size) versus an ~85 µs round trip to
  PostgreSQL. This is the projection's whole reason to exist and it delivers.
- **Full ordered scans** favor the projection (no round trip / no result
  serialization): 4× at 1k, 1.2× at 100k.
- **Writes (the like-for-like axis) are at parity** — the admission transaction is
  within ~4% of hand-written SQL at 1k and ~2% *faster* at 100k. **The backend adds
  no meaningful overhead over raw PostgreSQL on the write path.**

**Where the projection loses at scale — the real limitation:**

At 1k every class is near-parity, so the projection is "free" on small data. At
100k the **selective / structured read classes invert**: range scan **380×**,
view **6.5×**, filter **5.9×**, join **4.2×**, aggregate **3.2×** slower than
native PG. The cause is structural: the projection is a single key-ordered
`BTreeMap` with **no secondary indexes**, so a filter/range/join/aggregate is an
**O(n) full scan in Rust**, whereas PostgreSQL answers the same query with a
B-tree range scan, hash join, or parallel scan. The range-scan case is the
sharpest illustration — PG serves a small key range from its index in 217 µs while
the projection walks all 100k rows in 83 ms.

## Takeaway

- The architecture is **excellent for its design point**: key-addressed reads and
  live views over working-set-sized collections (point access ~constant-time,
  writes at raw-PG parity).
- It is **not a query engine**: selective filtered/range/join/aggregate reads over
  large collections do not get index acceleration and run several-fold (up to
  orders-of-magnitude) slower than native PostgreSQL at 100k.
- The natural optimization, if large selective queries become a target, is to push
  such predicates down to the **durable indexed PG tables** (which the
  `index_coverage_pg` gate already keeps index-served) instead of scanning the
  projection — a runtime read-planning change, not a schema change.

## How to extend

`SIZES` in the bench is `[1_000, 100_000]`; add `1_000_000` for the next tier
(fixtures are bulk-built with `INSERT … SELECT generate_series`, so setup stays
cheap; the criterion sample size is already reduced for the large tier).
