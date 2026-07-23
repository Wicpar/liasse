//! [`PgStore`]: one package instance's durable state on PostgreSQL.
//!
//! The store owns one writer connection (one writer per instance, so one
//! connection suffices) and an r2d2 [`ReadPool`] of read connections (§5 of
//! `DESIGN-pure-pg.md`), built by the factory after `reconcile` succeeds. Every
//! mutating contract call maps to exactly one SQL transaction on the writer.
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
//! 1, nothing to pin — §5.4; `snapshot`'s frontier log prefix is immutable, case 2,
//! and its head fast path is a single `nodes` scan, case 1), decodes it with the
//! existing codecs, and returns.
//!
//! The store holds **no in-memory read model of durable state** — the projection is
//! gone (Phase 3, the "no in-memory projection" mandate). The staging read base a
//! [`PgTransition`] overlays is the committed state read live from SQL via
//! `row`/`scan`: during staging nothing is written to PostgreSQL, so that pooled
//! base-read sees exactly the committed pre-transition state.

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_store::{
    CollectionPath, CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition, Composition,
    DefinitionText, InstanceStore, RowAddress, Snapshot, StoreError, StoredRow,
};
use liasse_value::{Sha512, Timestamp};
use postgres::{Client, NoTls};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use serde_json::Value as J;
use sha2::{Digest as _, Sha512 as Sha512Hasher};

use crate::backend::{backend, cell, corrupt, pool};
use crate::jsonb_text;
use crate::node_load;
use crate::node_write::NodeWriter;
use crate::read;
use crate::record_codec::{
    decode_composition, decode_log_row, encode_composition, encode_op, seq_from,
};
use crate::schema::Schema;
use crate::transition::PgTransition;

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
    /// The single writer connection (one writer per instance, §5.2): the admission
    /// transaction, `alloc_incarnation`, `put_blob`, `record_point`, and open-time
    /// reconcile all run on it.
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
        // is what a diagnostic actually wants. The head is no longer a projection
        // field — it lives only in the durable `instance_meta.head` (§6.2), which a
        // `&self` non-fallible `Debug` cannot query — so it is not shown here.
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
    /// burn-on-allocate (§6.3). One AUTOCOMMIT statement on the writer advances the
    /// persisted `instance_meta.next_incarnation` and returns the pre-increment
    /// value; the token is `row-{that}`. Staging holds no open SQL transaction (the
    /// overlay is pure in-memory), so this commits by itself, immediately. The
    /// counter therefore advances whether or not the staging later commits — matching
    /// [`liasse_store::MemoryStore`]'s abort-visible, no-reuse allocation in-process,
    /// and (strictly more faithful than the old commit-time persist) never reusing a
    /// burned token across a reopen.
    pub(crate) fn alloc_incarnation(&mut self) -> Result<RowIncarnation, StoreError> {
        let s = self.schema.quoted();
        let row = self
            .writer
            .query_one(
                &format!(
                    "UPDATE {s}.instance_meta SET next_incarnation = next_incarnation + 1 \
                     WHERE id = 1 RETURNING next_incarnation - 1 AS allocated"
                ),
                &[],
            )
            .map_err(backend)?;
        let allocated = cell::<i64>(&row, "instance_meta", "allocated")?;
        let token = u64::try_from(allocated)
            .map_err(|_| corrupt(format!("incarnation counter is negative ({allocated})")))?;
        Ok(RowIncarnation::new(format!("row-{token}")))
    }

    /// Atomically admit a staged transition in one SQL transaction. Empty
    /// transitions consume no position (§22.2). The serial position comes from
    /// the per-instance `instance_meta.head` counter, locked `FOR UPDATE`: it is
    /// gapless and monotone because it is a value we increment, never a
    /// PostgreSQL `SEQUENCE` (which gaps on rollback).
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
        // Neither `jsonb` nor a raw `text` column can hold a `U+0000`, which a valid
        // `text` value/key or an unvalidated D.5 token (transaction id) or D.4 source
        // may carry; NUL-safe-encode every string leaf before it reaches a column.
        let transaction_id = transaction.as_ref().map(|t| jsonb_text::encode_text(t.as_str()));
        let ops_wire = jsonb_text::to_jsonb(&J::Array(ops.iter().map(encode_op).collect()));
        // §22.5/§22.6: the commit's fixed `now` — the `$created` every inserted row
        // records — persisted so a log-fold replay reconstructs it (§14.1).
        let created_wire = jsonb_text::to_jsonb(&crate::value_codec::encode_created(created));
        let definition_source = definition.as_ref().map(|d| jsonb_text::encode_text(d.source()));
        let definition_id = definition.as_ref().map(|d| d.identity().to_canonical_text());
        let composition_wire =
            composition.as_ref().map(|c| jsonb_text::to_jsonb(&encode_composition(c)));

        let mut txn = self.writer.transaction().map_err(backend)?;
        // Take the per-instance write lock and read the authoritative head.
        let locked = txn
            .query_one(&format!("SELECT head FROM {s}.instance_meta WHERE id = 1 FOR UPDATE"), &[])
            .map_err(backend)?;
        // Pure PG: the locked durable head is the sole truth (§6.2); the next serial
        // position is its immediate successor. There is no second (projection) head
        // to cross-check any more, so the old divergence guard is gone.
        let durable_head: i64 = cell(&locked, "instance_meta", "head")?;
        let seq = seq_from(durable_head, "instance_meta.head")?.next();
        let seq_num = i64::try_from(seq.get()).map_err(|_| corrupt("serial position exceeds i64"))?;
        txn.execute(
            &format!(
                "INSERT INTO {s}.commit_log (seq, transaction_id, ops, created) VALUES ($1, $2, $3, $4)"
            ),
            &[&seq_num, &transaction_id, &ops_wire, &created_wire],
        )
        .map_err(backend)?;
        // Every op lands in the `nodes` adjacency tree — the sole durable row
        // representation — in this one admission transaction. `NodeWriter` resolves
        // each address to its surrogate id by an in-transaction SQL point lookup
        // (§6.1), so nodes inserted earlier in this same admission are visible; there
        // is no `by_id` projection to advance afterward. It carries the commit's
        // `now` so a fresh insert stamps the row's `$created` (§14.1, §22.6).
        let mut node_writer = NodeWriter::new(&s, created);
        for op in &ops {
            node_writer.apply(&mut txn, op)?;
        }
        // The commit no longer writes `next_incarnation`: the counter is advanced at
        // allocation time, durably (§6.3). Only the head and any definition/
        // composition are stamped here.
        txn.execute(
            &format!(
                "UPDATE {s}.instance_meta SET \
                 head = $1, \
                 definition_source = COALESCE($2, definition_source), \
                 definition_id = COALESCE($3, definition_id), \
                 composition = COALESCE($4, composition) WHERE id = 1"
            ),
            &[&seq_num, &definition_source, &definition_id, &composition_wire],
        )
        .map_err(backend)?;
        txn.commit().map_err(backend)?;

        // Pure PG: the durable tables the transaction just wrote *are* the committed
        // state. There is no projection to advance — a later read folds the log or
        // hits `nodes` directly (Phase 3).
        Ok(CommitOutcome::Committed(seq))
    }
}

impl InstanceStore for PgStore {
    type Transition<'s> = PgTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    fn head(&self) -> Result<CommitSeq, StoreError> {
        // §4.4: one single-statement pooled read of the durable head. The
        // `instance_meta` table is single-row (`CHECK (id = 1)`), so this is
        // index-gate-exempt (pinned, `meta_tables_are_single_row`).
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        let row = conn
            .query_one(&format!("SELECT head FROM {s}.instance_meta WHERE id = 1"), &[])
            .map_err(backend)?;
        seq_from(cell::<i64>(&row, "instance_meta", "head")?, "instance_meta.head")
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
        // The frontier-past-head check reads the durable head first (§4.3). The
        // one-writer-per-instance invariant plus Rust exclusivity (a commit needs
        // `&mut`, a reader holds `&`) means no commit interleaves this `&self` read,
        // so the head read and the materialization below observe the same state
        // (§5.4 case 3; the §5.4-case-2 in-statement head recheck is the seam wired
        // only if that premise is ever relaxed).
        let head = self.head()?;
        if frontier > head {
            return Err(corrupt(format!(
                "snapshot frontier {} is past head {}",
                frontier.get(),
                head.get()
            )));
        }
        let s = self.schema.quoted();
        let mut conn = self.reads.get().map_err(pool)?;
        if frontier == head {
            // Phase-6 head fast path (§4.3): the `nodes` tree holds exactly head
            // state, so materialize the live-row set directly in ONE full read of
            // `nodes` (O(state)) instead of folding the whole `commit_log`
            // (O(history)). The reconstructed `Snapshot` is byte-identical to the
            // log fold at head — the tree-≡-log-fold equivalence — because
            // `materialize_head` reuses the same value/key codecs the parity-gated
            // `row`/`scan` reads use and walks the same tombstone-through adjacency
            // chain (`node_tree_consistency::head_fast_path_equals_log_fold`). That
            // one statement legitimately scans the whole table (a full-state
            // materialization has no selective plan); it is the pinned no-Seq-Scan
            // exemption (`index_coverage_pg::head_fast_path_is_single_full_scan_exempt`).
            let rows = node_load::materialize_head(&mut *conn, &s)?;
            return Ok(Snapshot::from_rows(head, rows));
        }
        // §4.3 log fold for a historical frontier (`frontier < head`): fold the
        // append-only `commit_log` prefix `≤ frontier`, index-ordered by the PK
        // (index gate 4), decoded by the shared `record_codec` path and replayed by
        // the same `Snapshot::replay` MemoryStore uses — so parity is by
        // construction. The log `≤ frontier` is immutable, so this needs no SQL
        // transaction for coherence (§5.4 case 2): interleaved commits append *past*
        // the frontier and are invisible to the `WHERE seq <= $1` filter.
        let frontier_num =
            i64::try_from(frontier.get()).map_err(|_| corrupt("serial position exceeds i64"))?;
        let log = conn
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
        // §4.4: pooled range read of the append-only commit log from `from`, in seq
        // order (index gate 3), each row decoded by the shared `record_codec` path.
        // The log is immutable, so this single statement needs no pin (§5.4 case 1).
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
        let mut hasher = Sha512Hasher::new();
        hasher.update(bytes);
        let hex = data_encoding::HEXLOWER.encode(&hasher.finalize());
        let digest = Sha512::parse(&hex)
            .map_err(|error| corrupt(format!("computed SHA-512 did not round-trip: {error}")))?;
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
