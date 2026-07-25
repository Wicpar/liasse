//! [`PgStore`]: one package instance's durable state on PostgreSQL.
//!
//! The store owns one writer connection and an r2d2 [`ReadPool`] of read
//! connections (§5 of `DESIGN-pure-pg.md`), built by the factory after `reconcile`
//! succeeds. Every mutating contract call maps to exactly one SQL transaction on the
//! writer. One writer per *handle*, not per instance: the factory opens as many
//! handles onto one instance as asked, and their admissions overlap.
//!
//! **Phase 1 (§4.4)** serves the leaf reads — `head`, `log_from`,
//! `point_position`, `get_blob`, `has_blob`, `definition`, `composition` — from
//! the pool; **Phase 2 (§4.1/§4.2)** adds the `row`/`scan` node reads
//! ([`crate::read`]); and **Phase 3 (§4.3)** serves `snapshot` from a pooled
//! `commit_log` read folded by the shared [`Snapshot::replay`], with **Phase 6**
//! adding the `snapshot(head)` fast path — at `frontier == head` the live-row set is
//! materialized directly from the `nodes` tree ([`crate::node_load`]) in O(state)
//! rather than folding the whole log in O(history). Each read checks a connection
//! out of `reads`, runs one single-statement autocommit SQL query (consistency case
//! 1, nothing to pin — §5.4), decodes it with the existing codecs, and returns.
//! `snapshot` is the one exception: it needs head, the unpositioned-admission check
//! and its materialization to agree, so it runs them on one `REPEATABLE READ READ
//! ONLY` transaction (case 2).
//!
//! **Admission writes state, never history.** A commit transaction records the ops
//! in the `nodes` tree and a residual in `commit_log`, takes no serial position and
//! no instance-wide lock, and commits. Positions are stamped afterwards, over settled
//! admissions, by [`crate::history`] (§22.1). So `head`, `log_from` and `snapshot`
//! all read *built* history, while `row`/`scan` read current state — and current
//! state can be briefly ahead of history, which is the one gap this decoupling
//! creates and which every read here answers deliberately.
//!
//! The store holds **no in-memory read model of durable state** — the projection is
//! gone (Phase 3, the "no in-memory projection" mandate). The staging read base a
//! [`PgTransition`] overlays is the committed state read live from SQL via
//! `row`/`scan`: during staging nothing is written to PostgreSQL, so that pooled
//! base-read sees exactly the committed pre-transition state.

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_store::{
    CollectionPath, CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition, Composition,
    DefinitionText, GroupMember, InstanceStore, PendingCommit, RowAddress, Snapshot, StoreError,
    StoredRow,
};
use liasse_value::{Sha512, Timestamp};
use postgres::{Client, NoTls};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use serde_json::Value as J;

use crate::admit::{commit_body, commit_member};
use crate::backend::{backend, cell, corrupt, pool};
use crate::history;
use crate::jsonb_text;
use crate::node_load;
use crate::read;
use crate::record_codec::{decode_composition, decode_log_row, seq_from};
use crate::schema::Schema;
use crate::transition::PgTransition;

/// How long [`PgStore::settle`] waits between history passes while an older
/// admission is still in flight. Short enough that the common case (nothing else in
/// flight, positioned on the first pass) never sleeps at all, and the uncommon one
/// resolves as soon as the straggler ends.
const SETTLE_POLL: core::time::Duration = core::time::Duration::from_millis(1);

/// The `&self` read-connection pool: r2d2 over the same synchronous `postgres`
/// client the writer uses (§5.1). A pool is the maintainer-directed answer to
/// serving the contract's `&self` reads without contract-wide `&mut`-ification
/// or hand-rolled interior mutability; it manages an *external* resource
/// (database connections), which AGENTS.md's interior-mutability prohibition
/// (aimed at the crate's own state types) explicitly exempts.
#[doc(hidden)]
pub type ReadPool = Pool<PostgresConnectionManager<NoTls>>;

/// A PostgreSQL-backed store for one package instance.
///
/// The four fields are exactly the pure-PG target (`DESIGN-pure-pg.md` §2): the one
/// `writer` connection, the `reads` pool, the `schema`, and the `instance` identity.
/// **No field holds durable or read-model state** — no row map, no log copy, no blob
/// cache, no point map, no cached head/definition/composition, and no incarnation
/// cursor (durable since Phase 2, §6.3). Every contract read is a SQL query; the
/// projection this struct once carried was deleted in Phase 3.
pub struct PgStore {
    /// This handle's writer connection (§5.2): the admission transaction, the
    /// history pass that follows it, `alloc_incarnation`, `put_blob`, `record_point`,
    /// and open-time reconcile all run on it.
    writer: Client,
    /// The `&self` read pool (§5), built post-`reconcile` by the factory. Every
    /// contract read checks a connection out of it and serves one indexed SQL
    /// statement — `snapshot` additionally folds the returned log (§4.1–§4.4).
    reads: ReadPool,
    schema: Schema,
    instance: InstanceId,
}

impl core::fmt::Debug for PgStore {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `postgres::Client` is not `Debug`; name the instance and schema, which
        // is what a diagnostic actually wants. The head is not a field at all — it is
        // the built history's tip (§6.4), which a `&self` non-fallible `Debug` cannot
        // query — so it is not shown here.
        f.debug_struct("PgStore")
            .field("instance", &self.instance.as_str())
            .field("schema", &self.schema.name())
            .finish_non_exhaustive()
    }
}

impl PgStore {
    /// Adopt an opened `writer` connection whose `schema` is created and current.
    /// Nothing is loaded into memory: there is no projection to rebuild (Phase 3), so
    /// a fresh or reopened store answers every read straight from the durable tables.
    /// `reads` is the read pool the factory built against the same DSN *after*
    /// `reconcile` succeeded, so every pooled connection observes the reconciled
    /// schema (§5.3).
    pub(crate) fn open(
        writer: Client,
        schema: Schema,
        instance: InstanceId,
        reads: ReadPool,
    ) -> Result<Self, StoreError> {
        Ok(Self { writer, reads, schema, instance })
    }

    /// Allocate the next opaque incarnation token (D.1) during staging — durable
    /// burn-on-allocate (§6.3). One `nextval` on the schema's `incarnations`
    /// sequence returns the token number; the token is `row-{that}`.
    ///
    /// A sequence is the exactly-right shape here: `nextval` is non-transactional,
    /// so the counter advances whether or not the staging later commits (matching
    /// [`liasse_store::MemoryStore`]'s abort-visible, no-reuse allocation, and never
    /// reusing a burned token across a reopen), and it takes **no row lock**, so
    /// allocating a token during staging cannot serialize concurrent admissions.
    /// Gaps are meaningless for an opaque token, which is why the counter row this
    /// replaced had no reason to exist.
    pub(crate) fn alloc_incarnation(&mut self) -> Result<RowIncarnation, StoreError> {
        let [incarnations] = self.schema.sequences();
        let row = self
            .writer
            .query_one(
                &format!("SELECT nextval('{}') AS allocated", incarnations.qualified(&self.schema)),
                &[],
            )
            .map_err(backend)?;
        let allocated = cell::<i64>(&row, "incarnations", "allocated")?;
        let token = u64::try_from(allocated)
            .map_err(|_| corrupt(format!("incarnation counter is negative ({allocated})")))?;
        Ok(RowIncarnation::new(format!("row-{token}")))
    }

    /// Atomically admit a staged transition in one SQL transaction, then resolve the
    /// serial position history gives it. Empty transitions admit nothing (§22.2).
    ///
    /// The transaction writes **state and a residual only** — no position, no lock,
    /// nothing another instance-wide writer has to wait for — so two admissions to
    /// this instance overlap freely. The position is then stamped by a history pass
    /// over *settled* admissions ([`crate::history`]); the pass runs here, and this
    /// call returns once it has reached this admission, so the contract's "admission
    /// at one final serial position" still holds at the call boundary.
    ///
    /// The wait is not a lock: it ends when the oldest transaction that was in flight
    /// when this one committed has ended, and every concurrent admission's *state
    /// write* has already happened in parallel by then.
    pub(crate) fn commit_transition(
        &mut self,
        ops: Vec<CommittedRowOp>,
        created: Timestamp,
        transaction: Option<TransactionId>,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
    ) -> Result<CommitOutcome, StoreError> {
        if ops.is_empty() && definition.is_none() && composition.is_none() {
            return Ok(CommitOutcome::Unchanged);
        }
        let s = self.schema.quoted();
        let mut txn = self.writer.transaction().map_err(backend)?;
        let admission = commit_body(
            &mut txn,
            &s,
            &ops,
            created,
            transaction.as_ref(),
            definition.as_ref(),
            composition.as_ref(),
        )?;
        txn.commit().map_err(backend)?;

        // Pure PG: the durable tables the transaction just wrote *are* the committed
        // state. There is no projection to advance — a later read folds the log or
        // hits `nodes` directly (Phase 3).
        Ok(CommitOutcome::Committed(self.settle(&admission)?))
    }

    /// Drive history construction until `admission` has its position.
    ///
    /// The first pass positions it unless an *older* transaction was still in flight
    /// when it committed; then the wait is for that transaction to end, which is
    /// exactly the settlement the design is built on. Polling is the honest shape:
    /// PostgreSQL offers no way to wait on the snapshot horizon advancing, and a
    /// deadline here would be worse than a wait — it would mean returning an error
    /// for state that is already durably committed.
    fn settle(&mut self, admission: &history::AdmissionId) -> Result<CommitSeq, StoreError> {
        let s = self.schema.quoted();
        loop {
            self.build_history()?;
            if let Some(seq) = history::position_of(&mut self.writer, &s, admission)? {
                return Ok(seq);
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    /// Run one history-construction pass over this instance and report how many
    /// admissions it positioned (`DESIGN-pure-pg.md` §6.4).
    ///
    /// Every admission drives this itself, so it is public for the two callers that
    /// need a pass *without* admitting: a background builder, and a test holding an
    /// admission in flight to observe what a pass does and does not position.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if the pass cannot reach PostgreSQL.
    pub fn build_history(&mut self) -> Result<u64, StoreError> {
        history::build(&mut self.writer, &self.schema)
    }
}

impl InstanceStore for PgStore {
    type Transition<'s> = PgTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// PostgreSQL commits an all-or-none multi-instance transition durably: every
    /// touched instance's schema is on the SAME database, so the coordinator commits
    /// them together in ONE SQL transaction ([`Self::commit_pending_group`]) — atomic
    /// by construction, all schemas commit or the transaction rolls back entirely.
    fn multi_instance_atomic_commit(&self) -> bool {
        true
    }

    /// Commit one already-staged payload on this instance's own writer, in its own
    /// SQL transaction (§22.2) — the single-participant durable commit. The
    /// already-resolved ops are admitted directly; nothing is re-staged.
    fn commit_pending(&mut self, pending: PendingCommit) -> Result<CommitOutcome, StoreError> {
        let PendingCommit { ops, created, transaction, definition, composition } = pending;
        self.commit_transition(ops, created, transaction, definition, composition)
    }

    /// Commit every participant's payload as ONE durable all-or-none transition
    /// (§13.10). All instances of one deployment share a single database (one schema
    /// each), so a folded multi-engine transition is committed in ONE SQL transaction
    /// spanning every touched schema, on a single coordinating connection: atomicity
    /// is inherent — every schema commits, or the transaction rolls back entirely, so
    /// no surviving committed child under a rejected parent and no parent commit with
    /// an uncommitted child.
    ///
    /// The whole group shares ONE admitting transaction, so every participant's
    /// residual carries the same transaction id and history positions each instance's
    /// share of the fold together with everything else that settled — the fold is one
    /// event per instance, at each instance's own next position. Nothing here takes a
    /// per-instance lock, so two folded commits over overlapping instances neither
    /// serialize nor deadlock, and the fixed schema ordering the old head-lock
    /// protocol needed is gone with it. Outcomes are returned in the order the members
    /// were given.
    fn commit_pending_group(
        mut members: Vec<GroupMember<'_, PgStore>>,
    ) -> Result<Vec<CommitOutcome>, StoreError> {
        let admissions = {
            let Some((first, rest)) = members.split_first_mut() else {
                return Ok(Vec::new());
            };
            // The first member lends its writer as the single coordinating
            // connection; every touched schema is written through it in ONE
            // transaction.
            let first_pending = &first.pending;
            let PgStore { writer, schema, .. } = &mut *first.store;
            let first_schema = schema.quoted();
            let mut txn = writer.transaction().map_err(backend)?;
            let mut admissions = Vec::with_capacity(rest.len() + 1);
            admissions.push(commit_member(&mut txn, &first_schema, first_pending)?);
            for member in rest.iter() {
                let s = member.store.schema.quoted();
                admissions.push(commit_member(&mut txn, &s, &member.pending)?);
            }
            // ONE commit lands every touched schema atomically; any error above
            // dropped `txn` uncommitted, so nothing was written.
            txn.commit().map_err(backend)?;
            admissions
        };

        // Each participant now settles into its OWN instance's history; the fold is
        // durable either way, so a participant that has to wait for an unrelated
        // in-flight writer delays only its reported position, never the commit.
        members
            .iter_mut()
            .zip(admissions)
            .map(|(member, admission)| match admission {
                Some(admission) => Ok(CommitOutcome::Committed(member.store.settle(&admission)?)),
                None => Ok(CommitOutcome::Unchanged),
            })
            .collect()
    }

    fn head(&self) -> Result<CommitSeq, StoreError> {
        // §4.4: one single-statement pooled read of the built history's tip. There is
        // no write-side head counter to consult — history IS the head (§22.1), so the
        // authoritative answer is the highest position a history pass has handed out.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        history::tip(&mut *conn, &s)
    }

    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError> {
        // §4.1: one pooled chained-InitPlan point lookup (index gate 7). Intermediate
        // hops walk through tombstoned ancestors; only the outermost level filters
        // `value IS NOT NULL` (a tombstone is not a row).
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        read::row(&mut *conn, &s, address)
    }

    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        // §4.2: one pooled statement — the k−1 ancestor hops via the same chained
        // InitPlan, then the ordered child range over the final level, index-ordered
        // by `key_enc` with no `Sort` (index gate 8, scalar-subquery form).
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        read::scan(&mut *conn, &s, collection)
    }

    fn scan_subtree(
        &self,
        root: &RowAddress,
        steps: &[String],
    ) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        // §7.6: one pooled shape-directed `WITH RECURSIVE` statement — the anchor
        // resolves `root` via the chained InitPlan, the recursive term descends
        // `c.step_name = ANY($steps)` (staying on `node_key_lookup`, no Seq Scan,
        // index gate 9), traversing tombstones and emitting live descendants only.
        // Ordering is done in Rust over the reconstructed address, so the plan
        // carries no `Sort` and the order is byte-identical to the memory oracle.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        read::scan_subtree(&mut *conn, &s, root, steps)
    }

    fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        // Every decision here — is the frontier past the head, is the `nodes` tree
        // exactly head state, which rows does the fold see — must come from ONE
        // coherent view, because a concurrent admission can now land between two
        // statements. So the whole read runs on one `REPEATABLE READ READ ONLY`
        // snapshot: `DESIGN-pure-pg.md` §5.4's read session, wired now that the
        // one-writer premise it was conditioned on is gone.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let mut read = conn
            .build_transaction()
            .isolation_level(postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .map_err(backend)?;
        let head = history::tip(&mut read, &s)?;
        if frontier > head {
            return Err(corrupt(format!(
                "snapshot frontier {} is past head {}",
                frontier.get(),
                head.get()
            )));
        }
        // Phase-6 head fast path (§4.3): the `nodes` tree holds current state, so at
        // the head it can be materialized directly in ONE full read (O(state))
        // instead of folding the whole `commit_log` (O(history)). It is only *equal*
        // to head state while history has positioned everything committed — an
        // admission whose state has landed but whose position is still pending would
        // otherwise show up in a snapshot that claims not to include it. That gap is
        // real and observable (§22.1 decouples the two), so the fast path is taken
        // only when this snapshot sees no unpositioned admission, and the log fold —
        // always exact — serves the rest.
        if frontier == head && !history::has_unpositioned(&mut read, &s)? {
            // The reconstructed `Snapshot` is byte-identical to the log fold at head
            // — the tree-≡-log-fold equivalence — because `materialize_head` reuses
            // the same value/key codecs the parity-gated `row`/`scan` reads use and
            // walks the same tombstone-through adjacency chain
            // (`node_tree_consistency::head_fast_path_equals_log_fold`). That one
            // statement legitimately scans the whole table (a full-state
            // materialization has no selective plan); it is the pinned no-Seq-Scan
            // exemption (`index_coverage_pg::head_fast_path_is_single_full_scan_exempt`).
            let rows = node_load::materialize_head(&mut read, &s)?;
            return Ok(Snapshot::from_rows(head, rows));
        }
        // §4.3 log fold: fold the positioned `commit_log` prefix `≤ frontier`,
        // index-ordered by `seq` (index gate 4), decoded by the shared `record_codec`
        // path and replayed by the same `Snapshot::replay` MemoryStore uses — so
        // parity is by construction. Unpositioned residuals carry a NULL `seq` and
        // are excluded by the comparison itself, which is exactly right: they are not
        // yet part of history.
        let frontier_num =
            i64::try_from(frontier.get()).map_err(|_| corrupt("serial position exceeds i64"))?;
        let log = read
            .query(
                &format!(
                    "SELECT seq, transaction_id, ops, created FROM {s}.commit_log \
                     WHERE seq <= $1 ORDER BY seq"
                ),
                &[&frontier_num],
            )
            .map_err(backend)?
            .iter()
            .map(decode_log_row)
            .collect::<Result<Vec<_>, _>>()?;
        Snapshot::replay(&log, frontier)
    }

    fn log_from(&self, from: CommitSeq) -> Result<Vec<CommittedTransition>, StoreError> {
        // §4.4: pooled range read of built history from `from`, in position order
        // (index gate 3), each row decoded by the shared `record_codec` path. A
        // positioned row never changes again, so this single statement needs no pin
        // (§5.4 case 1); an unpositioned residual carries a NULL `seq` and is excluded
        // by the comparison, so the stream is history, never a preview of it.
        let s = self.schema.quoted();
        let from = i64::try_from(from.get()).map_err(|_| corrupt("serial position exceeds i64"))?;
        let mut conn = self.reads.get().map_err(pool)?;
        conn.query(
            &format!(
                "SELECT seq, transaction_id, ops, created FROM {s}.commit_log WHERE seq >= $1 ORDER BY seq"
            ),
            &[&from],
        )
        .map_err(backend)?
        .iter()
        .map(decode_log_row)
        .collect()
    }

    fn begin(&mut self) -> Self::Transition<'_> {
        PgTransition::new(self)
    }

    fn record_point(&mut self, at: CommitSeq, point: HistoryPoint) -> Result<(), StoreError> {
        // The position bound must not outrun the durable head, read from SQL (§4.4).
        let head = self.head()?;
        if at > head {
            return Err(corrupt(format!(
                "history point at {} is past head {}",
                at.get(),
                head.get()
            )));
        }
        let at_num = i64::try_from(at.get()).map_err(|_| corrupt("position exceeds i64"))?;
        // Lineage and point are unvalidated opaque D.5 tokens that may carry a
        // `U+0000` a `text` column rejects; NUL-safe-encode both. The escape is a
        // bijection, so equal points still collide on the `(lineage, point)` key.
        let lineage = jsonb_text::encode_text(point.lineage().as_str());
        let point_id = jsonb_text::encode_text(point.point().as_str());
        self.writer
            .execute(
                &format!(
                    "INSERT INTO {}.history_points (lineage, point, seq) VALUES ($1, $2, $3) \
                     ON CONFLICT (lineage, point) DO UPDATE SET seq = EXCLUDED.seq",
                    self.schema.quoted()
                ),
                &[&lineage, &point_id, &at_num],
            )
            .map_err(backend)?;
        // The durable `history_points` table is now the sole source — no projection
        // mirror to maintain (the leaf `point_position` read serves it from SQL).
        Ok(())
    }

    fn point_position(&self, point: &HistoryPoint) -> Result<Option<CommitSeq>, StoreError> {
        // §4.4: pooled PK lookup on `history_points` (index gate 6). NUL-safe-encode
        // the lineage/point tokens exactly as the write path does, so an equal point
        // collides on the same `(lineage, point)` key.
        let s = self.schema.quoted();
        let lineage = jsonb_text::encode_text(point.lineage().as_str());
        let point_id = jsonb_text::encode_text(point.point().as_str());
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_opt(
                &format!("SELECT seq FROM {s}.history_points WHERE lineage = $1 AND point = $2"),
                &[&lineage, &point_id],
            )
            .map_err(backend)?;
        row.map(|row| seq_from(cell::<i64>(&row, "history_points", "seq")?, "history_points.seq"))
            .transpose()
    }

    fn put_blob(&mut self, bytes: &[u8]) -> Result<Sha512, StoreError> {
        // §18.1: the content digest comes from the one shared hasher on `Sha512`,
        // so this backend and the memory reference address identical bytes by an
        // identical digest by construction rather than by two matching copies.
        let digest = Sha512::of(bytes);
        self.writer
            .execute(
                &format!(
                    "INSERT INTO {}.blobs (digest, bytes) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    self.schema.quoted()
                ),
                &[&digest.to_canonical_text(), &bytes],
            )
            .map_err(backend)?;
        // The durable `blobs` table is now the sole source — no projection cache to
        // maintain (the leaf `get_blob`/`has_blob` reads serve it from SQL).
        Ok(digest)
    }

    fn get_blob(&self, digest: &Sha512) -> Result<Option<Vec<u8>>, StoreError> {
        // §4.4: pooled PK lookup on `blobs` (index gate 5).
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_opt(
                &format!("SELECT bytes FROM {s}.blobs WHERE digest = $1"),
                &[&digest.to_canonical_text()],
            )
            .map_err(backend)?;
        row.map(|row| cell::<Vec<u8>>(&row, "blobs", "bytes")).transpose()
    }

    fn has_blob(&self, digest: &Sha512) -> Result<bool, StoreError> {
        // §4.4: pooled existence probe on the `blobs` PK — new index gate 10
        // (index-only, no Seq Scan). The `EXISTS` collapses the match to one boolean.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_one(
                &format!("SELECT EXISTS(SELECT 1 FROM {s}.blobs WHERE digest = $1) AS present"),
                &[&digest.to_canonical_text()],
            )
            .map_err(backend)?;
        cell::<bool>(&row, "blobs", "present")
    }

    fn definition(&self) -> Result<Option<DefinitionText>, StoreError> {
        // §4.4: single-statement pooled read of the durable definition source
        // (single-row table, index-gate-exempt/pinned), NUL-decoded to an owned
        // `DefinitionText` — nothing to borrow from a durable table.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_one(
                &format!("SELECT definition_source FROM {s}.instance_meta WHERE id = 1"),
                &[],
            )
            .map_err(backend)?;
        Ok(cell::<Option<String>>(&row, "instance_meta", "definition_source")?
            .map(|source| DefinitionText::new(jsonb_text::decode_text(&source))))
    }

    fn composition(&self) -> Result<Option<Composition>, StoreError> {
        // §4.4: single-statement pooled read of the durable composition JSONB
        // (single-row table, index-gate-exempt/pinned), decoded to an owned
        // `Composition`.
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_one(&format!("SELECT composition FROM {s}.instance_meta WHERE id = 1"), &[])
            .map_err(backend)?;
        cell::<Option<J>>(&row, "instance_meta", "composition")?
            .map(|wire| decode_composition(&jsonb_text::from_jsonb(&wire)))
            .transpose()
    }
}
