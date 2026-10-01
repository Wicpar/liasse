# PostgreSQL backend

The architecture of `liasse-pg`, the PostgreSQL store of the alpha
([ALPHA.md](ALPHA.md)), and of `liasse-pg-ext`, the extension that runs Liasse
inside PostgreSQL. [SPEC.md](SPEC.md), [LANGUAGE.md](LANGUAGE.md), and
[BUILTINS.md](BUILTINS.md) state what Liasse does; this document states how this
backend does it. Every behavior that a client or the host observes
follows those documents, and the conformance corpus checks it against
`liasse-store-memory`.

---

## 1. Principles

- **PostgreSQL is the engine.** Persisted state lives in PostgreSQL, and every
  evaluation over it runs there: state, history, claims, subscriptions and their
  retained results, the live-view diff, and blob bookkeeping. The host's process
  pilots: it carries connections, holds the host key, performs the host calls
  that leave PostgreSQL and the effects, runs the procedures of native
  transactions, and drives the runs of each transaction.
- **Pure PostgreSQL.** The process holds only what belongs to one connection
  or one run: the routing from connections to subscriptions, and the host calls
  of a run while it performs them.
- **Few round trips.** A call of a mutation is one statement between `BEGIN`
  and `COMMIT`, sent as one pipelined flight, plus one exchange for each round
  of host calls that the process performs (4), and one statement before it for
  each upload place that it verifies. A view read is one statement.
- **A decoupled physical representation.** Surrogate keys that stay in
  PostgreSQL, one table per stored set lineage, owners by reference, and
  Liasse identities translated by index.
- **Own synchronization primitives.** Liasse transactions run under
  `READ COMMITTED` and synchronize through primitives built for the mutation
  model (chapter 5).
- **A self-reconciling schema.** The database holds exactly the tables,
  indexes, and claims that the active versions use (chapter 13).
- **Index use and overhead are correctness gates** (chapter 15).

## 2. Components

| Component | Role |
| --- | --- |
| `liasse-pg` | Implements `liasse-store`. Sends each operation as one statement, runs the loop of runs and reruns, has the runtime perform the host calls, performs the effects, reconciles the schema, runs the background workers, and listens for commits. |
| `liasse-pg-ext` | A pgrx extension, loaded through `shared_preload_libraries`. It links the core evaluator (`liasse-eval`, with `liasse-model`, `liasse-std`, and `liasse-value`) and holds the record and key encodings, the transaction functions, the conflict tracker in shared memory, the signature builtins, a CSPRNG, and the time zone database. `unsafe` is confined to its FFI layer. |
| postgres image | The deploy artifact: PostgreSQL with `liasse-pg-ext` installed, `shared_preload_libraries = 'liasse_pg_ext'`, the tracker's shared memory sized, and `deadlock_timeout = 20ms`. |

```text
host process (liasse, liasse-pg)               PostgreSQL (liasse-pg-ext)
connections and their routing                  state, history, claims, touches
the host key, host calls, effects        <->   evaluation of views, programs, constraints
procedures of native transactions              the conflict tracker (shared memory)
the loop of runs                               subscriptions, retained results, the diff
LISTEN liasse                                  blob bookkeeping
```

---

## 3. Physical representation

An application lives in one PostgreSQL schema.

| Table | Holds |
| --- | --- |
| `module` | each module: its name, its target, and the packages of its migration chain |
| `instance` | each instance: its incarnation, module, active version, the package that activated it, the target that `modules.update` gave it, and mount point (the owner's table and row, and the mount point's declaration), with `ver`, and the diagnostics of a blocked target |
| `member` | each member of an instance's root that holds a value, an object, or an instance: `(instance, member)`, `ver`, `struct`, `value` |
| `c<n>` | one table for each stored set lineage of a module (3.1) |
| `claim` | each key that a row gives to a unique key: `(uniq, owner, key)`, and the row that gives it (3.3) |
| `history` | the changes of each commit that changes state (chapter 7) |
| `touch` | the write marks of each commit (6.2) |
| `subscription`, `occurrence`, `sub_mark`, `frame` | live views (chapter 8) |
| `effect` | the effects of commits, until the process performs them (chapter 11) |
| `keyring` | each instance's keyring of the keyring library, wrapped under the host key (chapter 10) |
| `blob`, `blob_copy`, `blob_use`, `blob_upload` | blob content, its verified copies, its uses in live state, and the places of upload plans (chapter 10) |
| `struct` | each record layout: module, version, declaration, and members |
| `host_dependency` | the host dependencies of each version that an instance holds: module, version, declaration name, and the signature of an opaque or the definition written in Liasse (SPEC §3.7) |
| `update_change` | the changes of each update's per-owner parts, by place (chapter 9) |
| `target_mark` | the read marks of the checks that fail each blocked target, indexed by key (chapter 9) |

### 3.1 Stored set tables

A **lineage** is a stored set declaration followed through the renames of its
module's migrations. Each lineage of each module has one table, which every
instance of the module shares:

```text
id        bigint   the surrogate key, from a sequence with CACHE
instance  bigint   the instance, referencing instance(id)
owner     bigint   the owner row of a nested stored set, referencing its table ON DELETE CASCADE; null at the root
commit    xid8     the identity of the inserting commit          } the row identity (SPEC Annex C.1)
ordinal   int      the row's place among that commit's insertions }
ver       xid8     the identity of the last commit that changed the row's own stored members
struct    int      the layout that value is encoded in
value     bytea    the row's own stored members and objects, or the row itself when it is a scalar, in the record encoding (chapter 12)
```

- A nested row references its owner row, and ancestry is a join: each row
  holds its own members alone, so keys stay small and a rename touches one row.
- `(instance, commit, ordinal)` is unique, and translates a Liasse identity to
  `id`.
- `ver` and `struct` are Liasse's own. A physical rewrite of a row, such as
  mapping it to a newer layout, keeps `ver`: the row's stored members stay as
  they were.
- Removing a row removes its nested rows by cascade, and the transaction
  unmounts the instances mounted in them (SPEC §11.5).
- A stored mount point holds the id of its instance in `value`.

### 3.2 Descriptors and indexes

A **descriptor** is an access path that a module reads through: a lineage, the
members that it compares by equality, then the members that it compares by
range or sorts by. The build derives the descriptors from the footprints of
every view, selection, pipeline, constraint, and unique key (ALPHA.md). Each
descriptor has one expression index:

```sql
CREATE INDEX CONCURRENTLY c7_d3
    ON c7 (instance, owner, liasse_key(struct, value, 3), commit, ordinal)
```

`liasse_key` is the order-preserving key encoding (12.2) of the descriptor's
members, read from `value` through the layout `struct`. A layout is fixed once
it is written, so `liasse_key` is deterministic and declared `IMMUTABLE`.
`commit` and `ordinal` end the index in insertion order (SPEC §4.3), so rows of
equal keys read oldest first. The index on `(instance, owner, commit, ordinal)`
reads the rows of one owner in insertion order.

### 3.3 Claims

`claim` holds one row for each key that a row gives to a unique key, with the
primary key `(uniq, owner, key)`, where `owner` is the owner row, or the
instance for a stored set of the root, and `key` is the key encoding of the
value that the unique key gives the row. A row gives a key when that value holds
an element, each part of a record or a tuple included. Claims, and no unique
index of a stored set table, enforce unique keys:

- a unique key holds of the complete result of a transaction, and a unique index
  of the stored set table judges each row as it is written;
- a new unique key in an update fills `claim` in the background, where a unique
  index would be built on a table in use;
- one index arbitrates concurrent keys for every unique key of the application
  (5.3).

### 3.4 Places

A row of a stored set table and a row of `member` are each one **place** (SPEC
§6.6), the members of their objects included, and each carries its `ver`. Two
rows of one owner's stored set are two places, and the rows of different owners
share no row, page, or lock that a Liasse transaction waits on.

---

## 4. Evaluation

- **One evaluator.** `liasse-pg-ext` evaluates every view, selection,
  initializer, constraint, program, and builtin with the core's evaluator, so a
  PostgreSQL result equals the core's result by construction. The core's
  arithmetic, Unicode tables, and order apply alone.
- **One statement per operation.** `liasse_call(…)` resolves the targets, parses
  the arguments, judges the constraints and unique keys of the input types, runs
  the programs, and judges the complete result, in one call. `liasse_read(…)`
  evaluates one view.
- **Reads under the pinned snapshot.** Every read of a run uses the snapshot
  that `liasse_begin` registered: the extension pushes it as the active snapshot
  and runs read-only SPI queries, which read under that active snapshot. Each
  read is an index scan of a descriptor, and the extension decodes the values it
  reads.
- **Backend functions.** The extension implements the backend functions of
  ALPHA.md over the descriptors (3.2): `sort` reads its elements through the
  descriptor of its key, in index order, `desc` reads a key or a place of a
  tuple key in reverse, and `skip` and `take` bound that scan.
- **The intermediate state.** A run's writes are held in the backend's memory,
  and every read of the run sees the snapshot overlaid with them. They reach the
  tables at commit (5.1).
- **Host calls.** The extension performs the drawn host calls that run within
  the backend: `uuid()`, `random.bytes`, and the key generations of `crypto`
  read the operating system's CSPRNG in the backend, and the time zone database
  is compiled into the extension. The process performs every other host call
  (SPEC §6.4): the package reads of initializations and updates, the history
  reads of a past-row load whose commits lie in cold storage (7), and the drawn
  opaque mutations of the host's own libraries (SPEC §3.7), those of the
  keyring library (10) among them.
- **Rounds.** The extension runs a program by the dependencies between its
  statements, which the build derives from footprints and bindings (ALPHA.md): a
  statement runs once the statements that it depends on have run, so a host call
  starts as soon as its arguments are evaluated. When every statement that
  remains waits on the process, `liasse_call` returns every pending host call of
  the run at once, each with its arguments and, for the keyring library, the
  wrapped keyring of the calling instance (10). The process performs them
  concurrently, and `liasse_resume(results)` continues the run in the same
  backend with every result, and with the new wrapped keyring when a call sealed
  the instance's first key. A run takes one exchange for each round, however
  many calls the round holds.
- **A waiting run holds its snapshot and its backend.** A run that waits on the
  process holds its snapshot and its backend alone, and takes its row locks and
  its claims in `liasse_commit()`.
- **Changes.** The instant is `clock_timestamp()` at `liasse_begin`. Drawn
  results, and the values computed from them on the way, live in the backend's
  memory until the run ends, and the tables receive the places that the run
  writes and, in `history`, its changes (7).
- **Native transactions.** The procedure runs in the process, and each of its
  reads, writes, and calls is one statement of the same PostgreSQL transaction,
  over the same pinned snapshot and intermediate state. This path takes more
  round trips than a call, and keeps the same precision of conflicts.

---

## 5. Transactions and synchronization

The backend detects each ground of SPEC §6.6 on which a transaction waits for
another or runs again, one place, one key, and a cycle, at the precision that
the module gives, and on these grounds alone.

### 5.1 A run

Each run of a Liasse transaction is one PostgreSQL transaction at
`READ COMMITTED`:

1. `liasse_begin()` takes and registers the run's snapshot, then takes the
   commit identity `pg_current_xact_id()`, which follows the identity of every
   commit in the snapshot, then takes the instant, and registers the run in the
   conflict tracker.
2. `liasse_call(…)` runs the transaction (chapter 4), recording read marks as
   it reads.
3. `liasse_commit()` writes the places (5.2), the claims (5.3), the history
   record, the touches, and the effects (chapter 11), each in a canonical order;
   the tracker's commit test (5.4) runs in the pre-commit callback; then
   `COMMIT`.

The three steps travel in one pipelined flight. A run that must run again rolls
back, and the next run is a new PostgreSQL transaction, with a new snapshot and
a new identity. A transaction that reaches the host's bound on runs is rejected
with the code `contention`. A call that closes before its commit (SPEC §10.3)
rolls its run back, and its transaction ends there.

### 5.2 One place

A run writes a place that it read at version `v` only while the place is still
at `v`:

```sql
UPDATE c7 SET value = $2, struct = $3, ver = $me WHERE id = $1 AND ver = $v
DELETE FROM c7 WHERE id = $1 AND ver = $v
```

At `READ COMMITTED`, a write to a row that a concurrent transaction is changing
waits for that transaction to end, then evaluates its condition again on the
newest version (EvalPlanQual). When the other committed a logical change, `ver`
differs, the statement writes zero rows, and the run runs again. When the other
rolled back, or only rewrote the row physically, the write proceeds. This is
SPEC's "MAY wait for the other to end" and "runs again", at the precision of
one place.

- An insert takes a new `id`, a place of its own.
- An insert into a nested stored set whose owner a concurrent transaction
  removes waits for it through the foreign key, and fails with 23503 when it
  commits; the run, which read the owner on its path, runs again.
- Writes apply in the order of `(table, id)`, so runs that change the same
  places wait in one order and never deadlock on them. A deadlock that another
  lock gives, such as a cascade, aborts one run with 40P01 after
  `deadlock_timeout`, and that run runs again.

### 5.3 One key

A run judges each unique key that reads a member that the run changed, over its
snapshot and its own writes, looking up each key that it gives in `claim`. A key
that the snapshot holds rejects the transaction with the code `uniqueness`. At
commit, the run deletes the claims of the keys that its rows stop giving and
inserts the claims of the keys that they start giving, in key order. An insert
of a key that a concurrent transaction claimed waits for it: when it commits,
the insert fails with 23505 and the run runs again over a snapshot that holds
the key; when it rolls back, the insert proceeds.

### 5.4 Cycles

Places and keys leave the anomalies of snapshot isolation: write skew, and a
run that reads the absence of rows that a concurrent run inserts. The
**conflict tracker** closes them. It is Serializable Snapshot Isolation
(Cahill, Röhm, Fekete, SIGMOD 2008) with the exact cycle test of PSSI (Revilak,
O'Neil, O'Neil, ICDE 2011), over marks at the precision of the module:

- **Read marks.** Each read of a run records a mark, which lists the members
  that the read depends on:
  - a place: `(table, id)`;
  - a selection by equality on a descriptor: `(descriptor, owner, hash of the
    key)`, which covers the rows of that key and their absence;
  - a range or a sort on a descriptor: `(descriptor, owner, low, high)`;
  - every other read of a stored set: `(table, owner)`, with the predicate that
    the read evaluated.
- **Write marks.** At commit, each change registers `(table, id)` and, for each
  descriptor of its table, the key of its before-image and of its after-image.
  An insert has an after-image only, and a removal a before-image only.
- **Matching.** A write matches a mark on its place, its key, its range, or its
  `(table, owner)` when it inserts, removes, or changes a member that the mark
  depends on, and, for a mark with a predicate, when its before- or after-image
  satisfies the predicate. This is precision locking (Jordan, Banerjee, Batman,
  SIGMOD 1981). Registering a mark probes the write marks, and registering a
  write probes the read marks, under the lock of the mark's partition, so each
  pair is found whichever comes first.
- **Edges.** A match between a reader `R` and a writer `W` is an
  rw-antidependency `R → W` when `W`'s commit is outside `R`'s snapshot, and a
  wr-dependency `W → R` when it is inside. The `ver` of each place read gives
  the wr-dependency on its writer, and the `ver` of each place written gives
  the ww-dependency on the previous writer.
- **Commit test.** In the pre-commit callback (pgrx
  `register_xact_callback(PgXactCallbackEvent::PreCommit, …)`), the tracker
  adds the committing run's edges and tests for a cycle through it. A cycle
  aborts the committing run, which runs again. A run with zero edges touches its
  own entry alone.
- **Cleanup.** A committed run's entry and marks stay while they can close a
  cycle, and PSSI's rule releases them.
- **Capacity.** The tracker's shared memory has a fixed size. When it fills,
  the oldest committed entries are summarized, as PostgreSQL's SSI does (Ports,
  Grittner, VLDB 2012), which can abort a run spuriously. Every
  summarization is counted, and the gates require zero at the configured size.

A view read runs over a snapshot, alongside every transaction, and takes zero
marks (SPEC §5.1). A native transaction records the same precise marks as a
call.

### 5.5 Why these primitives

PostgreSQL's `SERIALIZABLE` level gives serializability, and reruns
transactions that SPEC §6.6 lets commit:

- its predicate locks cover B-tree pages and whole relations: two runs that
  read and insert disjoint keys of one leaf page conflict, a sequential scan
  conflicts with every write of its relation, and lock promotion under memory
  pressure adds more;
- its dangerous-structure test aborts on two consecutive rw-antidependencies,
  cycle or not;
- `SERIALIZABLE` and `REPEATABLE READ` tie conflicts to physical tuple
  versions: a background rewrite of a row, such as mapping it to a newer layout
  (chapter 9), fails every concurrent writer of the row with 40001.

The mutation model gives what a general database cannot know, and these
primitives use it:

- places, owners, and descriptors come from the module, so marks are keys and
  predicates of the module;
- `ver` separates logical changes from physical ones, so maintenance and
  updates never conflict with transactions;
- unique keys are judged on the complete result and arbitrated by one index;
- the cycle test aborts only on a cycle.

---

## 6. Commit records

### 6.1 History record

Each commit that changes state inserts its `history` row in the PostgreSQL
transaction that commits it (chapter 7), so a commit and its changes are atomic.

### 6.2 Touches

Each commit inserts its write marks into `touch`, keyed by its identity. Live
views (chapter 8), blocked targets, and the checks around an update (chapter 9)
find what a commit changed through them. This is the transactional outbox
pattern: the record of a change commits with the change.

---

## 7. History

The `history` row of a commit that changes state holds exactly SPEC §14.1:

```text
commit    xid8          its identity
instant   timestamptz   its instant, to the microsecond
changes   bytea         its changes, each naming its rows by their identities
```

- A run collects its changes as it writes its places (5.2), from its
  intermediate state, and `liasse_commit` inserts them with the places.
- An update's changes continue in `update_change`, which holds the changes of
  its per-owner parts (chapter 9).
- Recomputation (SPEC §14.2) applies the changes of the `history` rows in the
  order of their identities. Export writes each change with the position paths
  of its rows (SPEC Annex C.5), and restore gives each inserted row a new
  identity.
- **Checkpoints**, the recomputed state of the commits before one commit, MAY be
  kept at an interval that the host sets, to speed past-row reads, export, and
  restore. They are derived: dropping them leaves every result as it is.
- Old history MAY move to cold storage and be read back on demand; a read of an
  old state then waits for it (SPEC §14.1).
- A past-row read (SPEC §14.4) starts from the nearest checkpoint before its
  instant and applies the changes of its snapshot's commits after it, in the
  order of their identities, up to the first whose instant is later.

---

## 8. Live views

### 8.1 Subscription state

| Table | Holds |
| --- | --- |
| `subscription` | each subscription: its connection, name, target, frontier (a snapshot and an instant), and `next_instant`, the earliest instant at which the clock alone changes its result (SPEC §5.2) |
| `occurrence` | the retained result: each occurrence's token, path, and value |
| `sub_mark` | the read marks of the subscription's last evaluation, indexed by key |
| `frame` | the frames sent, kept for the host's resumption grace period |

### 8.2 Advancing

To advance a subscription to a snapshot `S`:

1. The commits in `S` outside the subscription's frontier are a range scan of
   `touch` on the commit identity, from the frontier's `xmin`, filtered by
   visibility in the two snapshots.
2. A subscription whose marks match none of those commits' touches, and whose
   `next_instant` is still ahead, keeps its result. One statement joins the
   touches with `sub_mark` for every subscription of a process.
3. Otherwise the extension evaluates the view at `S`, and a SQL routine diffs
   the result against `occurrence`: removals, insertions, and updates, by path,
   set-based. It writes the new result and the frame, and returns the frame.

The diff is a plain SQL routine, and the memory store's diff is its oracle
(chapter 15).

### 8.3 Commit notification

A Liasse transaction never issues `NOTIFY`. PostgreSQL's `NOTIFY` takes one
lock that every notifying transaction of the server shares, in
`PreCommit_Notify`, and holds it until its commit is done, so a `NOTIFY` inside
each data transaction would serialize every commit. After `COMMIT`, the process
sends `NOTIFY liasse, '<commit>'` in its own transaction with
`synchronous_commit = off`, pipelined behind the commit, and coalesces: at most
one notification of a process is in flight, naming its newest commit. Only
these small transactions queue on that lock.

Each process holds one `LISTEN liasse` session. A notification wakes the
process to advance the subscriptions of its connections to a fresh snapshot.
The advance reads its commits from `touch` (8.2), and a notification only
wakes it: a lost one, such as from a process that stopped between its `COMMIT`
and its `NOTIFY`, delays the advance to the next notification or to the
process's tick, whose period the host sets.

### 8.4 Barrier and resumption

- Before replying `committed`, the committing process advances its
  connection's subscriptions to a snapshot that holds the commit and sends
  their frames before the reply (CONNECT.md 2.4).
- An index on `next_instant` finds the subscriptions that the clock advances.
- Subscription state lives in PostgreSQL, so any process resumes a
  subscription: it sends the frames after the presented frontier from `frame`,
  or a fresh `init` once they are released (CONNECT.md 2.4).

---

## 9. Updates in parallel

An update (SPEC §13.5) never takes a lock that waits for or blocks the
transactions of its instances, and never runs `ALTER TABLE` on a table in use:
its `ACCESS EXCLUSIVE` lock queues behind every running transaction and blocks
every new one.

- **Targets.** `update` writes a module's target, and `modules.update` an
  instance's, in the transaction that gives it. After the commit, the committing
  process searches for an update that holds each instance whose target changed
  (ALPHA.md), and an update that passes starts at once. A search that leaves a
  target blocked writes the diagnostics into the instance's row and the read
  marks of the failing checks into `target_mark`; the advance of chapter 8
  matches each commit's touches against `target_mark` as it matches `sub_mark`,
  and a match searches again. A process that stops before its search leaves the
  target to `open()` and to the tick of every other process (8.3), which search
  each target that no search has judged since it changed.
- **Joint updates.** A joint update sets the update flag of each member in the
  order of the instances' ids, so two updates that share members wait in one
  order and never deadlock, and its commit writes the version of every member in
  one PostgreSQL transaction. Each member meets the runs of its transactions
  through the handshake below.
- **Host calls.** The process performs the host calls of the per-owner parts in
  rounds (4), for the background work and for the update's commit.
- **Before the update commits**, in the background: the tables of new lineages
  are created; the indexes of new descriptors are built with
  `CREATE INDEX CONCURRENTLY`; the claims of new unique keys are filled from the
  current rows; and the extension evaluates the per-owner parts at each existing
  place and writes their changes into `update_change`, by place. The update
  takes its instant when it starts, before this work, and every per-owner part
  reads that instant as the time.
- **The update's commit** writes the new version into the row of each instance
  that it moves (one place, so two updates that move one instance conflict
  there), the new layouts into `struct`, and the reads and writes of the routes'
  file-level parts. It brings the claims of new unique keys and the rows of
  `update_change` up to date with the commits since the backfill, through
  `touch`: a key that two rows give rejects the update, and the update's commit
  evaluates the per-owner parts of each place that those commits inserted or
  changed, and replaces its rows of `update_change`.
- **Rows keep their layout.** A read of a row whose `struct` precedes the
  instance's version takes the row's new values from `update_change`, and a
  write stores the new layout. The update's commit stays one statement however
  many rows it maps: the rows of `update_change` are the changes of its history
  record, which export writes after its moves (SPEC Annex D.2), and restore
  applies.
- **Compaction.** A background worker rewrites the rows of older layouts in
  small batches with `FOR UPDATE SKIP LOCKED` and `synchronous_commit = off`,
  keeping `ver`, so it conflicts with no transaction (5.2).
- **Transactions that did not observe the update.** The update and the runs of
  its instance meet through a store-then-load handshake (Dekker): the update
  sets the instance's update flag in shared memory, then reads the state of
  every run; a run marks itself committing in its own entry, then reads the
  flag. At least one of them sees the other:
  - a run that sees the flag waits for the update to end, maps its changes to
    the new layouts, which its `history` row then holds, inserts the claims of
    the new unique keys, and judges the new constraints and unique keys that its
    changes touch over a fresh snapshot that holds the update; a failure makes
    it run again under the new version;
  - the update waits for the runs that it sees committing, then takes a fresh
    snapshot and judges its new constraints and unique keys on the changes of
    every commit outside its own snapshot, found through `touch`; a failure
    rejects the update.

  Only a run that commits a change of the instance reads the flag, and the
  commit path of a run writes its own entry alone.
- **Removed lineages and descriptors.** Their tables and indexes are dropped,
  `DROP INDEX CONCURRENTLY` for the indexes, once every run whose snapshot
  precedes the update has ended.

---

## 10. Blobs and keys

- Blob content lives at the locations of its storage classes, through
  connectors that run in the process (`liasse-host`). PostgreSQL holds the
  bookkeeping: `blob` (digest and size), `blob_copy` (each verified copy and
  its location), and `blob_use` (each descriptor in live state, maintained at
  commit).
- **Upload plans.** `blob_upload` holds each entry of a plan: its connection,
  digest, size, location, place, the expiry of its access data, and whether
  the place is verified. A connection resumes on any process (chapter 14), so
  its plans live in PostgreSQL. Issuing a plan is one statement, which takes a
  transaction-level advisory lock on the connection's identifier, checks the
  sum of its entries' sizes against the host's limit, and inserts the entries.
- **Verification.** Step 4 of a call (SPEC §10.3) finds its connection's entries
  by `(connection, digest, size)`. For each unverified one, the process reads
  the place back through the connector and hashes it, outside any PostgreSQL
  transaction; on a match, it calls `keep` and records the copy in `blob` and
  `blob_copy` and the entry as verified, in one statement, before the run
  begins. A missing entry, a missing place, or a mismatch rejects the call with
  the code `malformed`.
- A background worker keeps each descriptor's class satisfied, moves content
  when its class changes, and deletes the copies beyond what live state
  requires, keeping at least one verified copy of everything that history
  reaches (SPEC §12.5). It deletes each place once its access data has
  expired, the entries of each closed connection, and the copies that only
  those entries reached.
- **Keyrings.** `keyring` holds each instance's keyring of the keyring library
  (ALPHA.md), wrapped under the host key, keyed by the instance. It stays
  outside every module's table, history record, and export. The run that seals
  an instance's first key inserts its row; a concurrent first seal in the same
  instance meets the primary key and reruns (23505), as a claim does. Rotating
  the host key rewraps the rows in batches with `SKIP LOCKED`.
- Sealed keys are `bytea` inside values. The host key stays in the process's
  memory and reaches no SQL text, parameter, or table: pgcrypto's inline keys
  reach the server log and `pg_stat_statements`.
- Every host call and every connector call has a bound, and a call past it
  fails with the code `host`.

---

## 11. Effects

`effect` holds each effect of a commit until the process has performed it (SPEC
§6.4): its identifier, the commit's identity with the effect's ordinal in that
commit; the name of its mutation; the arguments, in the alpha's encoding; the
number of attempts; and `lease`, the instant from which any process takes it:
the commit's instant, then the end of the hold of the process that takes it,
then after a failure the instant of the next attempt.

- A run keeps its effects in the backend's memory with its writes, and
  `liasse_commit` inserts them in the PostgreSQL transaction that commits, so an
  effect row exists exactly when its commit does. This is the transactional
  outbox, as `touch` is (6.2).
- The committing process leases its new effects right after the commit,
  pipelined behind it as its notification is (8.3), and a worker in each
  process leases the others that are due: one statement,
  `UPDATE effect SET lease = … WHERE id IN (SELECT id FROM effect WHERE lease <= $now ORDER BY lease LIMIT $n FOR UPDATE SKIP LOCKED) RETURNING …`,
  in a transaction of its own. `lease` is indexed, so the statement is an index
  range scan.
- The process performs each leased effect outside any PostgreSQL transaction,
  concurrently with the others, and passes the identifier to its performer. A
  success deletes the row. A failure counts the attempt and moves `lease` to the
  next attempt, with exponential backoff.
- A lease that passes, as when its process stopped during an attempt, lets any
  process lease the effect again, under the same identifier: every effect is
  performed at least once, and the identifier makes a receiver's action once.
- An effect past the host's bound on attempts is reported to the host, then
  deleted.
- These statements alone read `effect`. History, export, and recomputation read
  `history` and `update_change`.

---

## 12. Values and encodings

### 12.1 Record encoding

A row's `value` is its layout's members in the layout's order, each value in an
exact binary form: `Int` of any size, `Decimal` in minimal-scale form, `Text` as
UTF-8, U+0000 included (PostgreSQL's `text` and `jsonb` refuse it; `bytea` holds
it), and `Blob` and composite values by their members.

### 12.2 Key encoding

`liasse_key` writes values so that the unsigned bytewise order of the encodings
is the order of Annex B, and two values have equal encodings exactly when they
are equal: the memcomparable encoding of the FoundationDB tuple layer,
CockroachDB, and MyRocks.

- `Int` and `Decimal`: a sign, the position of the leading digit, then the
  digits, so that numerically equal values have one encoding.
- `Text` and `Bytes`: the bytes, each `0x00` escaped as `0x00 0xFF`, then the
  terminator `0x00 0x00`, so that a proper prefix sorts first; UTF-8 byte order
  is scalar-value order.
- `Uuid`: its 16 bytes. `Date`, `Timestamp`, and `Duration`: big-endian
  integers with the sign bit flipped.
- A set of cardinality `?`, `*`, or `+`: for each element in ascending order,
  the marker `0x02` and the element, then `0x01`, so that `none` is `0x01` and
  a proper prefix sorts first.
- A descending key: every byte of its encoding complemented.
- Records, objects, and tuples: their members in the order of Annex B.2, one
  after the other.

Where PostgreSQL compares `text` itself, it uses the `C` collation.

### 12.3 Messages and history

`changes` in `history` and in `update_change`, the arguments in `effect`, and
the frames in `frame` are in the alpha's encoding (ALPHA.md).

---

## 13. Schema reconciliation

- On open and at each update, the desired schema is derived from the active
  versions of every instance: the table of each lineage, the index of each
  descriptor, the claims of each unique key, and the extension's version, which
  is locked to the core's.
- What is missing is created, concurrently where a table is in use, and what
  the active versions leave unused is dropped once every snapshot that reads it
  has ended.
- Several processes reconcile under one advisory lock that reconciliation alone
  takes.

## 14. Several processes

Every process runs the same code against one database, and the processes are
interchangeable: commits notify every process, subscriptions are shared, and a
connection resumes on any process. The tracker lives in the database server's
shared memory, so it sees the runs of every process.

---

## 15. Gates

Each gate is a count or an invariant, exact under any load.

| Gate | Holds when |
| --- | --- |
| Plans | `EXPLAIN` of every query pattern (view reads, selections by key, sorted windows, claim lookups, touch scans, the `sub_mark` join, history lookups) on tables of 100 000 rows per lineage shows index scans and zero sequential scans. |
| Disjoint work | 32 parallel clients run 10 000 sign-ups, sign-ins, task additions, and completions on distinct accounts over populated tables: zero reruns, zero summarizations, zero deadlocks. |
| Tampers | Lost update on one place, a duplicate value of a unique key, write skew across two places under one constraint, and a phantom under a non-unique predicate: each concurrent pair ends with exactly one rerun, and a result equal to one serial order. |
| Updates | A task-addition workload keeps committing through an update of its instance: zero reruns, zero summarizations, and zero `ALTER TABLE` statements in the statement log. |
| Targets | A blocked target whose failing check reads one row searches again after the first notification of the commit that changes that row, and zero searches run for commits whose touches match none of its marks. |
| Joint updates | A joint update of a parent and 1 000 children, with a transaction workload on every member: zero deadlocks, zero summarizations, and the versions of all 1 001 instances change in one commit. |
| Notifications | The statement log of a workload holds zero `NOTIFY` statements inside a transaction that writes state. |
| History | The state recomputed from history at the final point hashes equal to the live state, identities and incarnations included; export then restore gives the same hash over rows, insertion order, values, and row histories. |
| Schema | After migrations that remove a stored set, a selection, and a unique key, the catalog holds exactly the desired schema: zero orphan tables, indexes, or claims. |
| Parity | Every corpus case gives the same result on `liasse-pg` and `liasse-store-memory`, and the SQL diff's patches equal the memory store's results. |
| Key encoding | Over 1 000 000 generated pairs, bytewise order equals Annex B order and equal bytes equal values: zero counterexamples. |
| Changes | A mutation that stores the SHA-256 digest of 32 drawn bytes and returns the bytes, called 10 000 times: zero occurrences of any returned byte string in `history` and in the export. An update that adds a member initialized to such a digest over 100 000 rows: `update_change` holds exactly 100 000 changes, each equal to the digest that its row reads. A call that calls an effect and changes no state, 10 000 times: zero `history` rows. In each, the recomputed state hashes equal to the live state. |
| Effects | A transaction that calls one effect and runs again 3 times before it commits leaves exactly 1 `effect` row, and its performer sees 1 identifier; a rejected one leaves 0 rows; a process killed after the commit and before the attempt has the effect performed after restart, under the same identifier; `effect` holds 0 rows once every effect succeeded. |
| Host call rounds | A call whose runs sign 100 rows with sealed keys, each run independent of the others' writes, takes exactly 1 `liasse_resume` exchange. |
| Overhead | Each path is timed against the equivalent hand-written SQL as matched pairs on a quiet machine, and the ratio is reported. |

---

## 16. Prior art

| Mechanism | Pattern and source |
| --- | --- |
| Version-conditioned writes (5.2) | optimistic concurrency control (Kung, Robinson, TODS 1981), as in Silo (SOSP 2013) and Hekaton (PVLDB 2011), with PostgreSQL's EvalPlanQual waiting on the concurrent writer |
| Claims (5.3) | arbitration by one unique index: PostgreSQL's B-tree uniqueness check waits for a concurrent inserter of the same key, then fails or proceeds |
| The tracker (5.4) | Serializable Snapshot Isolation (Cahill, Röhm, Fekete, SIGMOD 2008); PSSI's exact cycle test (Revilak, O'Neil, O'Neil, ICDE 2011); PostgreSQL's SSI and its summarization (Ports, Grittner, VLDB 2012); precision locks (Jordan, Banerjee, Batman, SIGMOD 1981) |
| Static exemption, not used | robustness of transaction programs against weaker isolation (Fekete et al., TODS 2005; Vandevoort et al., PVLDB 2021 and arXiv 2501.18377): exempting mutations would need a proof that holds beside native transactions, mounts, and updates |
| Touches (6.2) | the transactional outbox |
| Live views (chapter 8) | poll and diff, as Meteor's observe driver does, triggered by touched keys, with `LISTEN`/`NOTIFY` as the doorbell |
| Updates in parallel (chapter 9) | online asynchronous schema change (Rae et al., F1, VLDB 2013); lazy per-row migration by layout version; PostgreSQL 11's catalog-only `ADD COLUMN` with a constant default; `CREATE INDEX CONCURRENTLY`'s wait for older transactions |
| The update handshake (chapter 9) | Dekker's store-then-load handshake |
| Targets and joint updates (chapter 9) | apt's `upgrade`, which moves every package that moves alone and keeps back the rest, and `full-upgrade`, which moves co-dependent packages together; Kubernetes' level-triggered reconciliation, which acts again at each change of what it watches and reports why an object differs from its desired state; package installability, NP-complete in general (Mancinelli et al., ASE 2006), which the host's bound on the search answers |
| Ordered update flags (chapter 9) | ordered resource acquisition, which rules out deadlock (Havender, IBM Systems Journal, 1968) |
| Host call rounds (4) | the implicit batching and concurrency of independent fetches in Haxl (Marlow, Brandy, Coens, Purdy, "There is no fork: an implicitly concurrent abstraction for efficient data access", ICFP 2014) |
| History (chapter 7) | row-based change logs: MySQL's row-based binary log and PostgreSQL's logical decoding, which record the changed rows of each committed transaction and nothing for a transaction that changes none |
| Effects (chapter 11) | the transactional outbox, drained by `FOR UPDATE SKIP LOCKED` workers; at-least-once delivery made once by an idempotency key, as Stripe's `Idempotency-Key` header; Convex's scheduled functions, which a mutation schedules when it commits |
| Key encoding (12.2) | the memcomparable format of the FoundationDB tuple layer, CockroachDB, and MyRocks |
