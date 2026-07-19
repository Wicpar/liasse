//! [`PgStore`]: one package instance's durable state on PostgreSQL.
//!
//! The store owns one writer connection (one writer per instance, so one
//! connection suffices) and an r2d2 [`ReadPool`] of read connections (§5 of
//! `DESIGN-pure-pg.md`), built by the factory after `reconcile` succeeds. Every
//! mutating contract call maps to exactly one SQL transaction on the writer.
//!
//! **Phase 1 (§4.4)** serves the leaf reads — `head`, `log_from`,
//! `point_position`, `get_blob`, `has_blob`, `definition`, `composition` — from
//! the pool: each checks a connection out of `reads`, runs one single-statement
//! autocommit SQL query (consistency case 1, nothing to pin — §5.4), decodes the
//! result with the existing codecs, and returns. The remaining reads
//! (`row`/`scan`/`snapshot`) are still answered from the in-memory
//! [`Projection`], which the write path keeps equal to the durable tables until
//! Phases 2–3 convert them too.

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_store::{
    CollectionPath, CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition, Composition,
    DefinitionText, InstanceStore, RowAddress, Snapshot, StoreError, StoredRow,
};
use liasse_value::Sha512;
use postgres::{Client, NoTls};
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;
use serde_json::Value as J;
use sha2::{Digest as _, Sha512 as Sha512Hasher};

use crate::backend::{backend, cell, corrupt, pool};
use crate::jsonb_text;
use crate::node_write::NodeWriter;
use crate::projection::{
    Projection, decode_composition, decode_log_row, encode_composition, seq_from,
};
use crate::record_codec::encode_op;
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
pub struct PgStore {
    client: Client,
    schema: Schema,
    instance: InstanceId,
    projection: Projection,
    /// The `&self` read pool (§5), built post-`reconcile` by the factory. UNUSED
    /// in Phase 0: no contract read consults it yet — every read is still served
    /// from `projection` — and Phase 1 is the first reader ([`PgStore::pool`]).
    reads: ReadPool,
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
    /// Adopt an opened writer connection whose `schema` is created and current,
    /// loading the read model from its durable tables. `reads` is the read pool
    /// the factory built against the same DSN *after* `reconcile` succeeded, so
    /// every pooled connection observes the reconciled schema (§5.3).
    pub(crate) fn open(
        mut client: Client,
        schema: Schema,
        instance: InstanceId,
        reads: ReadPool,
    ) -> Result<Self, StoreError> {
        let projection = Projection::load(&mut client, &schema)?;
        Ok(Self { client, schema, instance, projection, reads })
    }

    /// The `&self` read-connection pool (§5). Doc-hidden and stable in shape only
    /// for the internal read path: Phase 1 checks a connection out of it to serve
    /// leaf reads from SQL. It exists now (and is exercised here) so the field is
    /// live before its first real reader lands — no `#[allow(dead_code)]` needed.
    #[doc(hidden)]
    #[must_use]
    pub fn pool(&self) -> &ReadPool {
        &self.reads
    }

    /// The live current row at `address` — the base a staged read overlays.
    pub(crate) fn resolve_current(&self, address: &RowAddress) -> Option<&StoredRow> {
        self.projection.current().get(address)
    }

    /// The committed direct rows of `collection` — the base a staged scan
    /// overlays.
    pub(crate) fn resolve_collection(
        &self,
        collection: &CollectionPath,
    ) -> Vec<(RowAddress, StoredRow)> {
        self.projection
            .current()
            .iter()
            .filter(|(address, _)| collection.contains(address))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect()
    }

    /// Allocate the next opaque incarnation token (D.1) during staging.
    pub(crate) fn alloc_incarnation(&mut self) -> RowIncarnation {
        self.projection.alloc_incarnation()
    }

    /// Atomically admit a staged transition in one SQL transaction. Empty
    /// transitions consume no position (§22.2). The serial position comes from
    /// the per-instance `instance_meta.head` counter, locked `FOR UPDATE`: it is
    /// gapless and monotone because it is a value we increment, never a
    /// PostgreSQL `SEQUENCE` (which gaps on rollback).
    pub(crate) fn commit_transition(
        &mut self,
        ops: Vec<CommittedRowOp>,
        transaction: Option<TransactionId>,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
    ) -> Result<CommitOutcome, StoreError> {
        if ops.is_empty() && definition.is_none() && composition.is_none() {
            return Ok(CommitOutcome::Unchanged);
        }
        let s = self.schema.quoted();
        let next_incarnation = i64::try_from(self.projection.next_incarnation())
            .map_err(|_| corrupt("incarnation counter exceeds i64"))?;
        // Neither `jsonb` nor a raw `text` column can hold a `U+0000`, which a valid
        // `text` value/key or an unvalidated D.5 token (transaction id) or D.4 source
        // may carry; NUL-safe-encode every string leaf before it reaches a column.
        let transaction_id = transaction.as_ref().map(|t| jsonb_text::encode_text(t.as_str()));
        let ops_wire = jsonb_text::to_jsonb(&J::Array(ops.iter().map(encode_op).collect()));
        let definition_source = definition.as_ref().map(|d| jsonb_text::encode_text(d.source()));
        let definition_id = definition.as_ref().map(|d| d.identity().to_canonical_text());
        let composition_wire =
            composition.as_ref().map(|c| jsonb_text::to_jsonb(&encode_composition(c)));

        let mut txn = self.client.transaction().map_err(backend)?;
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
            &format!("INSERT INTO {s}.commit_log (seq, transaction_id, ops) VALUES ($1, $2, $3)"),
            &[&seq_num, &transaction_id, &ops_wire],
        )
        .map_err(backend)?;
        // Every op lands in the `nodes` adjacency tree — the sole durable row
        // representation — in this one admission transaction. The freshly inserted
        // node ids are collected so the projection can advance its `by_id` map once
        // the commit succeeds.
        let mut node_writer = NodeWriter::new(&s, self.projection.by_id());
        for op in &ops {
            node_writer.apply(&mut txn, op)?;
        }
        let new_node_ids = node_writer.into_new_ids();
        txn.execute(
            &format!(
                "UPDATE {s}.instance_meta SET \
                 head = $1, next_incarnation = $2, \
                 definition_source = COALESCE($3, definition_source), \
                 definition_id = COALESCE($4, definition_id), \
                 composition = COALESCE($5, composition) WHERE id = 1"
            ),
            &[&seq_num, &next_incarnation, &definition_source, &definition_id, &composition_wire],
        )
        .map_err(backend)?;
        txn.commit().map_err(backend)?;

        let committed = CommittedTransition::new(seq, ops, transaction);
        self.projection.apply_committed(committed, new_node_ids);
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
        Ok(self.projection.current().get(address).cloned())
    }

    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        Ok(self
            .projection
            .current()
            .iter()
            .filter(|(address, _)| collection.contains(address))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect())
    }

    fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        // The frontier-past-head check reads the durable head first (§4.3); the fold
        // itself still replays the projection log until Phase 3.
        let head = self.head()?;
        if frontier > head {
            return Err(corrupt(format!(
                "snapshot frontier {} is past head {}",
                frontier.get(),
                head.get()
            )));
        }
        self.projection.snapshot(frontier)
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
                "SELECT seq, transaction_id, ops FROM {s}.commit_log WHERE seq >= $1 ORDER BY seq"
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
        self.client
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
        self.client
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
