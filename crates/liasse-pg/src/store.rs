//! [`PgStore`]: one package instance's durable state on PostgreSQL.
//!
//! The store owns one writer connection (one writer per instance, so one
//! connection suffices) and an in-memory [`Projection`] of committed state for
//! the `&self` read path. Every mutating contract call maps to exactly one SQL
//! transaction; reads are served from the projection, which the write path keeps
//! equal to the durable tables.
//!
//! It also owns an r2d2 [`ReadPool`] of read connections (§5 of
//! `DESIGN-pure-pg.md`), built by the factory after `reconcile` succeeds. In
//! Phase 0 the pool is **unused** — every contract read is still projection-served
//! — and exists so the later phases can move each `&self` read onto a pooled SQL
//! statement without another contract change; [`PgStore::pool`] is the seam they
//! reach it through.

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

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::node_write::NodeWriter;
use crate::projection::{Projection, encode_composition};
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
        // is what a diagnostic actually wants.
        f.debug_struct("PgStore")
            .field("instance", &self.instance.as_str())
            .field("schema", &self.schema.name())
            .field("head", &self.projection.head().get())
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
        let seq = self.projection.head().next();
        let seq_num = i64::try_from(seq.get()).map_err(|_| corrupt("serial position exceeds i64"))?;
        let next_incarnation =
            i64::try_from(self.projection.next_incarnation()).map_err(|_| corrupt("incarnation counter exceeds i64"))?;
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
        let durable_head: i64 = cell(&locked, "instance_meta", "head")?;
        if durable_head != seq_num - 1 {
            return Err(corrupt(format!(
                "projection head {} disagrees with durable head {durable_head}",
                seq_num - 1
            )));
        }
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
        self.projection.apply_committed(committed, definition, composition, new_node_ids);
        Ok(CommitOutcome::Committed(self.projection.head()))
    }
}

impl InstanceStore for PgStore {
    type Transition<'s> = PgTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    fn head(&self) -> Result<CommitSeq, StoreError> {
        // Phase 0: still projection-served (the pool is unused). Phase 1 replaces
        // this body with a pooled `SELECT head FROM instance_meta` (§4.4).
        Ok(self.projection.head())
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
        if frontier > self.projection.head() {
            return Err(corrupt(format!(
                "snapshot frontier {} is past head {}",
                frontier.get(),
                self.projection.head().get()
            )));
        }
        self.projection.snapshot(frontier)
    }

    fn log_from(&self, from: CommitSeq) -> Result<Vec<CommittedTransition>, StoreError> {
        Ok(self.projection.log_from(from))
    }

    fn begin(&mut self) -> Self::Transition<'_> {
        PgTransition::new(self)
    }

    fn record_point(&mut self, at: CommitSeq, point: HistoryPoint) -> Result<(), StoreError> {
        if at > self.projection.head() {
            return Err(corrupt(format!(
                "history point at {} is past head {}",
                at.get(),
                self.projection.head().get()
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
        self.projection.insert_point(point, at);
        Ok(())
    }

    fn point_position(&self, point: &HistoryPoint) -> Result<Option<CommitSeq>, StoreError> {
        Ok(self.projection.point_position(point))
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
        self.projection.insert_blob(digest, bytes.to_vec());
        Ok(digest)
    }

    fn get_blob(&self, digest: &Sha512) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.projection.blob(digest).cloned())
    }

    fn has_blob(&self, digest: &Sha512) -> Result<bool, StoreError> {
        Ok(self.projection.has_blob(digest))
    }

    fn definition(&self) -> Result<Option<DefinitionText>, StoreError> {
        // Owned per the contract: clone the projection's copy (Phase 1 decodes it
        // per read from `instance_meta` instead).
        Ok(self.projection.definition().cloned())
    }

    fn composition(&self) -> Result<Option<Composition>, StoreError> {
        Ok(self.projection.composition().cloned())
    }
}
