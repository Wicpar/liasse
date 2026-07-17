//! [`PgStore`]: one package instance's durable state on PostgreSQL.
//!
//! The store owns one connection (one writer per instance, so one connection
//! suffices) and an in-memory [`Projection`] of committed state for the `&self`
//! read path. Every mutating contract call maps to exactly one SQL transaction;
//! reads are served from the projection, which the write path keeps equal to the
//! durable tables.

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_store::{
    CollectionPath, CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition, Composition,
    DefinitionText, InstanceStore, RowAddress, Snapshot, StoreError, StoredRow,
};
use liasse_value::Sha512;
use postgres::Client;
use serde_json::Value as J;
use sha2::{Digest as _, Sha512 as Sha512Hasher};

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::projection::{Projection, encode_composition};
use crate::record_codec::{address_key, encode_op};
use crate::schema::Schema;
use crate::transition::PgTransition;
use crate::value_codec;

/// A PostgreSQL-backed store for one package instance.
pub struct PgStore {
    client: Client,
    schema: Schema,
    instance: InstanceId,
    projection: Projection,
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
    /// Adopt an opened connection whose `schema` is created and current, loading
    /// the read model from its durable tables.
    pub(crate) fn open(
        mut client: Client,
        schema: Schema,
        instance: InstanceId,
    ) -> Result<Self, StoreError> {
        let projection = Projection::load(&mut client, &schema)?;
        Ok(Self { client, schema, instance, projection })
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
        for op in &ops {
            Self::apply_op_sql(&mut txn, &s, op)?;
        }
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
        self.projection.apply_committed(committed, definition, composition);
        Ok(CommitOutcome::Committed(self.projection.head()))
    }

    fn apply_op_sql(
        txn: &mut postgres::Transaction<'_>,
        schema: &str,
        op: &CommittedRowOp,
    ) -> Result<(), StoreError> {
        match op {
            CommittedRowOp::Insert { address, incarnation, value } => {
                let key = address_key(address)?;
                txn.execute(
                    &format!(
                        "INSERT INTO {schema}.rows (addr_key, incarnation, value) VALUES ($1, $2, $3)"
                    ),
                    &[&key, &incarnation.as_str(), &jsonb_text::to_jsonb(&value_codec::encode(value))],
                )
                .map_err(backend)?;
            }
            CommittedRowOp::Update { address, incarnation, value } => {
                let key = address_key(address)?;
                txn.execute(
                    &format!(
                        "UPDATE {schema}.rows SET incarnation = $2, value = $3 WHERE addr_key = $1"
                    ),
                    &[&key, &incarnation.as_str(), &jsonb_text::to_jsonb(&value_codec::encode(value))],
                )
                .map_err(backend)?;
            }
            CommittedRowOp::Delete { address, .. } => {
                let key = address_key(address)?;
                txn.execute(
                    &format!("DELETE FROM {schema}.rows WHERE addr_key = $1"),
                    &[&key],
                )
                .map_err(backend)?;
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                let from_key = address_key(from)?;
                let to_key = address_key(to)?;
                txn.execute(
                    &format!("DELETE FROM {schema}.rows WHERE addr_key = $1"),
                    &[&from_key],
                )
                .map_err(backend)?;
                txn.execute(
                    &format!(
                        "INSERT INTO {schema}.rows (addr_key, incarnation, value) VALUES ($1, $2, $3)"
                    ),
                    &[&to_key, &incarnation.as_str(), &jsonb_text::to_jsonb(&value_codec::encode(value))],
                )
                .map_err(backend)?;
            }
        }
        Ok(())
    }
}

impl InstanceStore for PgStore {
    type Transition<'s> = PgTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    fn head(&self) -> CommitSeq {
        self.projection.head()
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

    fn point_position(&self, point: &HistoryPoint) -> Option<CommitSeq> {
        self.projection.point_position(point)
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

    fn has_blob(&self, digest: &Sha512) -> bool {
        self.projection.has_blob(digest)
    }

    fn definition(&self) -> Option<&DefinitionText> {
        self.projection.definition()
    }

    fn composition(&self) -> Option<&Composition> {
        self.projection.composition()
    }
}
