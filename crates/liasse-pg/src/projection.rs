//! The in-memory read model, loaded from the durable tables and kept in lockstep
//! with them.
//!
//! The contract's read methods take `&self`, but the synchronous PostgreSQL
//! client needs `&mut self` for every query. Rather than smuggle in interior
//! mutability — forbidden for our own types — the store keeps a projection of
//! committed state in memory: current rows, the replayable commit log, history
//! points, blob bytes, and instance metadata. It is loaded from the tables when
//! the store opens and advanced by exactly the operations each commit writes to
//! PostgreSQL in the same SQL transaction, so it is equal to durable state by
//! construction. Reopening a store rebuilds an identical projection from the
//! tables, which is what makes durability observable (the crate's reopen test
//! proves it) and lets frontier snapshots fold the durable log (§19.2, §22.7).

use std::collections::BTreeMap;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId, RowIncarnation};
use liasse_store::{
    CommitSeq, CommittedRowOp, CommittedTransition, Composition, DefinitionText, Mount, RowAddress,
    Snapshot, StoreError, StoredRow,
};
use liasse_value::Sha512;
use postgres::Client;
use serde_json::{Map, Value as J};

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::record_codec::{decode_address, decode_op};
use crate::schema::Schema;
use crate::value_codec;

/// A committed-state read model for one instance.
#[derive(Debug)]
pub struct Projection {
    head: CommitSeq,
    next_incarnation: u64,
    current: BTreeMap<RowAddress, StoredRow>,
    log: Vec<CommittedTransition>,
    points: BTreeMap<HistoryPoint, CommitSeq>,
    blobs: BTreeMap<Sha512, Vec<u8>>,
    definition: Option<DefinitionText>,
    composition: Option<Composition>,
}

impl Projection {
    /// Load the whole read model from the durable tables of `schema`.
    pub fn load(client: &mut Client, schema: &Schema) -> Result<Self, StoreError> {
        let s = schema.quoted();
        let meta = client
            .query_one(
                &format!(
                    "SELECT head, next_incarnation, definition_source, composition \
                     FROM {s}.instance_meta WHERE id = 1"
                ),
                &[],
            )
            .map_err(backend)?;
        let head = seq_from(cell::<i64>(&meta, "instance_meta", "head")?, "instance_meta.head")?;
        let next_incarnation = counter(
            cell::<i64>(&meta, "instance_meta", "next_incarnation")?,
            "instance_meta.next_incarnation",
        )?;
        let definition = cell::<Option<String>>(&meta, "instance_meta", "definition_source")?
            .map(|source| DefinitionText::new(jsonb_text::decode_text(&source)));
        let composition = cell::<Option<J>>(&meta, "instance_meta", "composition")?
            .map(|wire| decode_composition(&jsonb_text::from_jsonb(&wire)))
            .transpose()?;

        let mut log = Vec::new();
        for row in client
            .query(&format!("SELECT seq, transaction_id, ops FROM {s}.commit_log ORDER BY seq"), &[])
            .map_err(backend)?
        {
            let seq = seq_from(cell::<i64>(&row, "commit_log", "seq")?, "commit_log.seq")?;
            let transaction = cell::<Option<String>>(&row, "commit_log", "transaction_id")?
                .map(|id| liasse_ident::TransactionId::new(jsonb_text::decode_text(&id)));
            let ops_wire = jsonb_text::from_jsonb(&cell::<J>(&row, "commit_log", "ops")?);
            let ops = ops_wire
                .as_array()
                .ok_or_else(|| corrupt("commit_log ops is not an array"))?
                .iter()
                .map(decode_op)
                .collect::<Result<Vec<_>, _>>()?;
            log.push(CommittedTransition::new(seq, ops, transaction));
        }

        // Current state is the authoritative `rows` table; it must agree with a
        // fold of the log, which the store's snapshot path exercises.
        let mut current = BTreeMap::new();
        for row in client
            .query(&format!("SELECT addr_key, incarnation, value FROM {s}.rows"), &[])
            .map_err(backend)?
        {
            let addr_key: String = cell(&row, "rows", "addr_key")?;
            let wire: J = serde_json::from_str(&addr_key)
                .map_err(|error| corrupt(format!("stored address key is not JSON: {error}")))?;
            let address = decode_address(&wire)?;
            let incarnation = RowIncarnation::new(cell::<String>(&row, "rows", "incarnation")?);
            let value = value_codec::decode(&jsonb_text::from_jsonb(&cell::<J>(&row, "rows", "value")?))?;
            current.insert(address, StoredRow::new(incarnation, value));
        }

        let mut points = BTreeMap::new();
        for row in client
            .query(&format!("SELECT lineage, point, seq FROM {s}.history_points"), &[])
            .map_err(backend)?
        {
            let point = HistoryPoint::new(
                LineageId::new(jsonb_text::decode_text(&cell::<String>(&row, "history_points", "lineage")?)),
                PointId::new(jsonb_text::decode_text(&cell::<String>(&row, "history_points", "point")?)),
            );
            let at = seq_from(cell::<i64>(&row, "history_points", "seq")?, "history_points.seq")?;
            points.insert(point, at);
        }

        let mut blobs = BTreeMap::new();
        for row in
            client.query(&format!("SELECT digest, bytes FROM {s}.blobs"), &[]).map_err(backend)?
        {
            let digest = Sha512::parse(&cell::<String>(&row, "blobs", "digest")?).map_err(corrupt_digest)?;
            blobs.insert(digest, cell::<Vec<u8>>(&row, "blobs", "bytes")?);
        }

        Ok(Self { head, next_incarnation, current, log, points, blobs, definition, composition })
    }

    /// The current head position.
    pub fn head(&self) -> CommitSeq {
        self.head
    }

    /// The whole current row map — the base a scan filters.
    pub fn current(&self) -> &BTreeMap<RowAddress, StoredRow> {
        &self.current
    }

    /// The committed log at positions `>= from`, ascending.
    pub fn log_from(&self, from: CommitSeq) -> Vec<CommittedTransition> {
        self.log.iter().filter(|t| t.seq() >= from).cloned().collect()
    }

    /// A frontier snapshot folded from the durable log (§22.7, §19.2).
    pub fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        Snapshot::replay(&self.log, frontier)
    }

    /// The position a recorded point names, if any.
    pub fn point_position(&self, point: &HistoryPoint) -> Option<CommitSeq> {
        self.points.get(point).copied()
    }

    /// Record a point-to-position mapping in the projection.
    pub fn insert_point(&mut self, point: HistoryPoint, at: CommitSeq) {
        self.points.insert(point, at);
    }

    /// Blob bytes by digest.
    pub fn blob(&self, digest: &Sha512) -> Option<&Vec<u8>> {
        self.blobs.get(digest)
    }

    /// Whether a blob is held.
    pub fn has_blob(&self, digest: &Sha512) -> bool {
        self.blobs.contains_key(digest)
    }

    /// Record blob bytes in the projection.
    pub fn insert_blob(&mut self, digest: Sha512, bytes: Vec<u8>) {
        self.blobs.entry(digest).or_insert(bytes);
    }

    /// The active definition and composition.
    pub fn definition(&self) -> Option<&DefinitionText> {
        self.definition.as_ref()
    }
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }

    /// Allocate the next opaque incarnation token (D.1). Aborted allocations are
    /// harmless: only serial positions must be gapless.
    pub fn alloc_incarnation(&mut self) -> RowIncarnation {
        let token = format!("row-{}", self.next_incarnation);
        self.next_incarnation += 1;
        RowIncarnation::new(token)
    }

    /// The incarnation counter to persist alongside a commit.
    pub fn next_incarnation(&self) -> u64 {
        self.next_incarnation
    }

    /// Advance the projection by a committed transition — the exact operations
    /// the same SQL transaction just wrote to PostgreSQL.
    pub fn apply_committed(
        &mut self,
        transition: CommittedTransition,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
    ) {
        for op in transition.ops() {
            self.apply_op(op);
        }
        if let Some(definition) = definition {
            self.definition = Some(definition);
        }
        if let Some(composition) = composition {
            self.composition = Some(composition);
        }
        self.head = transition.seq();
        self.log.push(transition);
    }

    fn apply_op(&mut self, op: &CommittedRowOp) {
        match op {
            CommittedRowOp::Insert { address, incarnation, value }
            | CommittedRowOp::Update { address, incarnation, value } => {
                self.current
                    .insert(address.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
            CommittedRowOp::Delete { address, .. } => {
                self.current.remove(address);
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                self.current.remove(from);
                self.current
                    .insert(to.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
        }
    }
}

/// Encode a composition into the `instance_meta.composition` JSONB.
#[must_use]
pub fn encode_composition(composition: &Composition) -> J {
    let mut obj = Map::new();
    for (name, mount) in composition.mounts() {
        let mut entry = Map::new();
        entry.insert("instance".to_owned(), J::String(mount.instance().as_str().to_owned()));
        entry.insert("lineage".to_owned(), J::String(mount.selected().lineage().as_str().to_owned()));
        entry.insert("point".to_owned(), J::String(mount.selected().point().as_str().to_owned()));
        obj.insert(name.to_owned(), J::Object(entry));
    }
    J::Object(obj)
}

fn decode_composition(wire: &J) -> Result<Composition, StoreError> {
    let obj = wire.as_object().ok_or_else(|| corrupt("composition is not an object"))?;
    let mut composition = Composition::new();
    for (name, entry) in obj {
        let entry = entry.as_object().ok_or_else(|| corrupt("mount is not an object"))?;
        let field = |key: &str| {
            entry.get(key).and_then(J::as_str).ok_or_else(|| corrupt(format!("mount missing `{key}`")))
        };
        let mount = Mount::new(
            InstanceId::new(field("instance")?),
            HistoryPoint::new(LineageId::new(field("lineage")?), PointId::new(field("point")?)),
        );
        composition = composition.with(name.clone(), mount);
    }
    Ok(composition)
}

fn corrupt_digest(error: liasse_value::ValueError) -> StoreError {
    corrupt(format!("stored blob digest is malformed: {error}"))
}

/// Rebuild the serial position stored as the durable `BIGINT` `raw` (from column
/// `what`). A position is minted by [`CommitSeq::next`] and can never be
/// negative; a negative durable value is a corruption to report, never a value
/// to silently coerce to genesis. Reconstruction is O(1) via
/// [`CommitSeq::from_stored`], which is what keeps [`Projection::load`] linear in
/// the commit count rather than quadratic (it previously replayed `next` from
/// genesis once per stored position).
fn seq_from(raw: i64, what: &str) -> Result<CommitSeq, StoreError> {
    let n = u64::try_from(raw).map_err(|_| corrupt(format!("{what} is negative ({raw})")))?;
    Ok(CommitSeq::from_stored(n))
}

/// Rebuild a durable non-negative counter stored as the `BIGINT` `raw` (from
/// column `what`). Like a position it can never legitimately be negative; a
/// negative durable value is corruption rather than a silent zero.
fn counter(raw: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(raw).map_err(|_| corrupt(format!("{what} is negative ({raw})")))
}
