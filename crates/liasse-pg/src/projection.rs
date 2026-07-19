//! The in-memory read model still backing the `row`/`scan`/`snapshot` reads,
//! loaded from the durable tables and kept in lockstep with them.
//!
//! The contract's read methods take `&self`, but the synchronous PostgreSQL
//! client needs `&mut self` for every query. The pure-PG re-architecture
//! (`DESIGN-pure-pg.md`) is replacing this projection read-by-read behind the
//! parity gate; **Phase 1 (§4.4) has already moved the leaf reads** — `head`,
//! `log_from`, `point_position`, `get_blob`, `has_blob`, `definition`,
//! `composition` — onto pooled SQL, so this projection no longer mirrors the
//! durable head, history points, blob bytes, or instance definition/composition.
//! What it still holds is exactly what the not-yet-converted reads need:
//!
//! - `current`: address → [`StoredRow`] — the live-row base `row`/`scan` overlay
//!   (Phase 2) and the occupancy oracle;
//! - `by_id`: the structural node index (every node, live or tombstoned) the write
//!   path resolves parents against (Phase 2 moves this to in-txn SQL);
//! - `log`: the replayable commit log `snapshot` folds (Phase 3 moves this to a
//!   durable log read);
//! - `next_incarnation`: the allocator counter (Phase 2 makes it durable per §6.3).
//!
//! It is loaded from the tables when the store opens and advanced by exactly the
//! operations each commit writes to PostgreSQL in the same SQL transaction, so it
//! is equal to durable state by construction; the current rows are reconstructed
//! from the `nodes` adjacency tree — the sole durable row representation
//! ([`crate::node_load`]) — while the log comes from its own table.

use std::collections::BTreeMap;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId, RowIncarnation};
use liasse_store::{
    CommitSeq, CommittedRowOp, CommittedTransition, Composition, Mount, RowAddress, Snapshot,
    StoreError, StoredRow,
};
use postgres::{Client, Row};
use serde_json::{Map, Value as J};

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::record_codec::decode_op;
use crate::schema::Schema;

/// The committed-state read model still backing `row`/`scan`/`snapshot`.
#[derive(Debug)]
pub struct Projection {
    next_incarnation: u64,
    current: BTreeMap<RowAddress, StoredRow>,
    /// The structural node index: EVERY node's address → its surrogate id in the
    /// `nodes` adjacency tree — live rows AND tombstones (a deleted non-leaf ancestor
    /// retained so its descendants stay addressable, §5.4). The dual-write path
    /// resolves a row's parent against this map, so a child places under a tombstoned
    /// ancestor exactly as the read path walks through it. Occupancy is NEVER read
    /// from here: `current` is the live-row index (reads/occupancy), `by_id` is the
    /// structural index (parent resolution) — a tombstoned address is present here but
    /// absent from `current`. The id is a backend-private handle (never observed
    /// through the contract), kept out of the shared [`StoredRow`]. Maintained in
    /// lock-step with the node tree after every commit succeeds.
    by_id: BTreeMap<RowAddress, i64>,
    log: Vec<CommittedTransition>,
}

impl Projection {
    /// Load the read model still backing `row`/`scan`/`snapshot` from the durable
    /// tables of `schema`: the incarnation counter, the commit log, and the current
    /// rows plus their structural index reconstructed from the `nodes` tree. The
    /// head, history points, blobs, and definition/composition are no longer
    /// mirrored — the leaf reads serve them straight from SQL (§4.4).
    pub fn load(client: &mut Client, schema: &Schema) -> Result<Self, StoreError> {
        let s = schema.quoted();
        let meta = client
            .query_one(
                &format!("SELECT next_incarnation FROM {s}.instance_meta WHERE id = 1"),
                &[],
            )
            .map_err(backend)?;
        let next_incarnation = counter(
            cell::<i64>(&meta, "instance_meta", "next_incarnation")?,
            "instance_meta.next_incarnation",
        )?;

        let mut log = Vec::new();
        for row in client
            .query(&format!("SELECT seq, transaction_id, ops FROM {s}.commit_log ORDER BY seq"), &[])
            .map_err(backend)?
        {
            log.push(decode_log_row(&row)?);
        }

        // Reconstruct the current rows AND the address→id map from the node tree —
        // the sole durable row representation — in one pass: each node's parent chain
        // gives its address, its `key_wire`/`value` decode to the level key and stored
        // value, and its surrogate id feeds the write path's `by_id` resolver.
        let crate::node_load::NodeTree { current, by_id } = crate::node_load::load(client, &s)?;

        Ok(Self { next_incarnation, current, by_id, log })
    }

    /// The structural node index (every node, live or tombstoned) — the dual-write
    /// path's parent/id resolver, so a child places under a tombstoned ancestor.
    pub fn by_id(&self) -> &BTreeMap<RowAddress, i64> {
        &self.by_id
    }

    /// The whole current row map — the base a scan filters.
    pub fn current(&self) -> &BTreeMap<RowAddress, StoredRow> {
        &self.current
    }

    /// A frontier snapshot folded from the durable log (§22.7, §19.2).
    pub fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        Snapshot::replay(&self.log, frontier)
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
    /// identities. Applied only after the commit succeeds. The head is no longer
    /// mirrored here — it is the durable `instance_meta.head` the commit locked and
    /// wrote (§6.2), and the leaf `head` read serves it from SQL (§4.4).
    pub fn apply_committed(&mut self, transition: CommittedTransition, new_node_ids: Vec<i64>) {
        let mut fresh_ids = new_node_ids.into_iter();
        for op in transition.ops() {
            self.apply_op(op);
            self.apply_node_id(op, &mut fresh_ids);
        }
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

    /// Advance `by_id` (the structural node index — EVERY node, live or tombstoned)
    /// by one op, mirroring the node dual-write's effect on node identity: an insert
    /// takes the next established id (a fresh node, or a revived tombstone keeping its
    /// id); a delete LEAVES the entry in place (the node write tombstones the node
    /// rather than removing it, so a later child still resolves the tombstoned
    /// ancestor); a rekey KEEPS the source entry (its node is tombstoned in place) and
    /// takes the next established id for the target (a fresh or revived node); an
    /// update leaves identity untouched. The live-row index `current` is advanced
    /// separately by [`Self::apply_op`], which is where a delete/rekey-source drops
    /// the row — occupancy stays live-only while `by_id` stays structural.
    fn apply_node_id(&mut self, op: &CommittedRowOp, fresh_ids: &mut impl Iterator<Item = i64>) {
        match op {
            CommittedRowOp::Insert { address, .. } => {
                if let Some(id) = fresh_ids.next() {
                    self.by_id.insert(address.clone(), id);
                }
            }
            CommittedRowOp::Update { .. } => {}
            // The node is tombstoned in place, not removed, so its structural entry
            // stays — a later child still resolves this tombstoned ancestor.
            CommittedRowOp::Delete { .. } => {}
            CommittedRowOp::Rekey { to, .. } => {
                // The source node is tombstoned in place (its `from` entry stays in
                // `by_id`); the target establishes a fresh-or-revived live node.
                if let Some(id) = fresh_ids.next() {
                    self.by_id.insert(to.clone(), id);
                }
            }
        }
    }
}

/// Decode one `commit_log` row (`seq`, `transaction_id`, `ops`) into a
/// [`CommittedTransition`]. Shared by [`Projection::load`]'s full-log read and the
/// leaf `log_from` read (§4.4), so both decode a stored transition identically.
pub(crate) fn decode_log_row(row: &Row) -> Result<CommittedTransition, StoreError> {
    let seq = seq_from(cell::<i64>(row, "commit_log", "seq")?, "commit_log.seq")?;
    let transaction = cell::<Option<String>>(row, "commit_log", "transaction_id")?
        .map(|id| liasse_ident::TransactionId::new(jsonb_text::decode_text(&id)));
    let ops_wire = jsonb_text::from_jsonb(&cell::<J>(row, "commit_log", "ops")?);
    let ops = ops_wire
        .as_array()
        .ok_or_else(|| corrupt("commit_log ops is not an array"))?
        .iter()
        .map(decode_op)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CommittedTransition::new(seq, ops, transaction))
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

/// Decode a composition from the `instance_meta.composition` JSONB — the inverse of
/// [`encode_composition`], serving the leaf `composition` read (§4.4).
pub(crate) fn decode_composition(wire: &J) -> Result<Composition, StoreError> {
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

/// Rebuild the serial position stored as the durable `BIGINT` `raw` (from column
/// `what`). A position is minted by [`CommitSeq::next`] and can never be
/// negative; a negative durable value is a corruption to report, never a value
/// to silently coerce to genesis. Reconstruction is O(1) via
/// [`CommitSeq::from_stored`]. Shared by the leaf reads that decode a stored
/// position (`head`, `point_position`) and by the log decode.
pub(crate) fn seq_from(raw: i64, what: &str) -> Result<CommitSeq, StoreError> {
    let n = u64::try_from(raw).map_err(|_| corrupt(format!("{what} is negative ({raw})")))?;
    Ok(CommitSeq::from_stored(n))
}

/// Rebuild a durable non-negative counter stored as the `BIGINT` `raw` (from
/// column `what`). Like a position it can never legitimately be negative; a
/// negative durable value is corruption rather than a silent zero.
fn counter(raw: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(raw).map_err(|_| corrupt(format!("{what} is negative ({raw})")))
}
