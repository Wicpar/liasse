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
//!
//! The current rows are reconstructed from the `nodes` adjacency tree — the sole
//! durable row representation ([`crate::node_load`]) — while the log, points,
//! blobs, and metadata come from their own tables.

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
use crate::record_codec::decode_op;
use crate::schema::Schema;

/// A committed-state read model for one instance.
#[derive(Debug)]
pub struct Projection {
    head: CommitSeq,
    next_incarnation: u64,
    current: BTreeMap<RowAddress, StoredRow>,
    /// Each *live* row's surrogate node id in the `nodes` adjacency tree (tombstones
    /// carry no entry). Kept out of the shared [`StoredRow`] (the id is a
    /// backend-private handle, never observed through the contract) and used only by
    /// the dual-write path to apply node ops by id. Maintained in lock-step with
    /// `current` after every commit succeeds.
    by_id: BTreeMap<RowAddress, i64>,
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

        // Reconstruct the current rows AND the address→id map from the node tree —
        // the sole durable row representation — in one pass: each node's parent chain
        // gives its address, its `key_wire`/`value` decode to the level key and stored
        // value, and its surrogate id feeds the write path's `by_id` resolver.
        let crate::node_load::NodeTree { current, by_id } = crate::node_load::load(client, &s)?;

        Ok(Self {
            head,
            next_incarnation,
            current,
            by_id,
            log,
            points,
            blobs,
            definition,
            composition,
        })
    }

    /// The live rows' surrogate node ids — the dual-write path's parent/id resolver.
    pub fn by_id(&self) -> &BTreeMap<RowAddress, i64> {
        &self.by_id
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

    /// Advance the projection by a committed transition — the exact operations the
    /// same SQL transaction just wrote to PostgreSQL — and to the `nodes` tree.
    /// `new_node_ids` holds the surrogate id each op established at a live address, one
    /// per `Insert` and one per `Rekey` (target), in op order — the `RETURNING id`s the
    /// node dual-write collected — so `by_id` stays in lock-step with the durable node
    /// identities. Applied only after the commit succeeds.
    pub fn apply_committed(
        &mut self,
        transition: CommittedTransition,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
        new_node_ids: Vec<i64>,
    ) {
        let mut fresh_ids = new_node_ids.into_iter();
        for op in transition.ops() {
            self.apply_op(op);
            self.apply_node_id(op, &mut fresh_ids);
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

    /// Advance `by_id` (which holds only *live* rows) by one op, mirroring the node
    /// dual-write's effect on node identity: an insert takes the next established id
    /// (a fresh node, or a revived tombstone keeping its id); a delete drops ONLY the
    /// addressed id (the node write tombstones just that node, leaving descendants —
    /// still live rows — in place, so their ids stay); a rekey drops the source id
    /// (its node is tombstoned) and takes the next established id for the target (a
    /// fresh or revived node); an update leaves identity untouched.
    fn apply_node_id(&mut self, op: &CommittedRowOp, fresh_ids: &mut impl Iterator<Item = i64>) {
        match op {
            CommittedRowOp::Insert { address, .. } => {
                if let Some(id) = fresh_ids.next() {
                    self.by_id.insert(address.clone(), id);
                }
            }
            CommittedRowOp::Update { .. } => {}
            CommittedRowOp::Delete { address, .. } => {
                self.by_id.remove(address);
            }
            CommittedRowOp::Rekey { from, to, .. } => {
                self.by_id.remove(from);
                if let Some(id) = fresh_ids.next() {
                    self.by_id.insert(to.clone(), id);
                }
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
