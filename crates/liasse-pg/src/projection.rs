//! The in-memory read model still backing the staging read base and `snapshot`,
//! loaded from the durable tables and kept in lockstep with them.
//!
//! The contract's read methods take `&self`, but the synchronous PostgreSQL
//! client needs `&mut self` for every query. The pure-PG re-architecture
//! (`DESIGN-pure-pg.md`) is replacing this projection read-by-read behind the
//! parity gate. **Phase 1 (§4.4)** moved the leaf reads — `head`, `log_from`,
//! `point_position`, `get_blob`, `has_blob`, `definition`, `composition` — onto
//! pooled SQL; **Phase 2 (§4.1/§4.2)** moved the contract's `row`/`scan` onto SQL
//! ([`crate::read`]) and the write path's parent/id resolution onto in-transaction
//! SQL (§6.1), retiring the structural `by_id` index, and made the incarnation
//! counter durable (§6.3), retiring the in-memory allocator cursor. What this
//! projection still holds is exactly what the not-yet-converted paths need:
//!
//! - `current`: address → [`StoredRow`] — the live-row base a staged read overlays
//!   (`resolve_current`/`resolve_collection`, §22.2) and the occupancy oracle;
//! - `log`: the replayable commit log `snapshot` folds (Phase 3 moves this to a
//!   durable log read, then deletes this file).
//!
//! It is loaded from the tables when the store opens and advanced by exactly the
//! operations each commit writes to PostgreSQL in the same SQL transaction, so it
//! is equal to durable state by construction; the current rows are reconstructed
//! from the `nodes` adjacency tree — the sole durable row representation
//! ([`crate::node_load`]) — while the log comes from its own table.

use std::collections::BTreeMap;

use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};
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

/// The committed-state read model still backing the staging base and `snapshot`.
#[derive(Debug)]
pub struct Projection {
    current: BTreeMap<RowAddress, StoredRow>,
    log: Vec<CommittedTransition>,
}

impl Projection {
    /// Load the read model still backing the staging base and `snapshot` from the
    /// durable tables of `schema`: the commit log, and the current live rows
    /// reconstructed from the `nodes` tree. The head, history points, blobs,
    /// definition/composition (Phase 1) and the incarnation counter (Phase 2, now
    /// durable §6.3) are no longer mirrored — SQL serves them straight (§4.4, §6.3).
    pub fn load(client: &mut Client, schema: &Schema) -> Result<Self, StoreError> {
        let s = schema.quoted();
        let mut log = Vec::new();
        for row in client
            .query(&format!("SELECT seq, transaction_id, ops FROM {s}.commit_log ORDER BY seq"), &[])
            .map_err(backend)?
        {
            log.push(decode_log_row(&row)?);
        }

        // Reconstruct the current live rows from the node tree — the sole durable row
        // representation — walking each node's parent chain to its address and
        // decoding its `key_wire`/`value` (§node_load).
        let current = crate::node_load::load(client, &s)?;

        Ok(Self { current, log })
    }

    /// The whole current row map — the base a staged read/scan overlays.
    pub fn current(&self) -> &BTreeMap<RowAddress, StoredRow> {
        &self.current
    }

    /// A frontier snapshot folded from the durable log (§22.7, §19.2).
    pub fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        Snapshot::replay(&self.log, frontier)
    }

    /// Advance the projection by a committed transition — the exact operations the
    /// same SQL transaction just wrote to PostgreSQL. Applied only after the commit
    /// succeeds. The head is no longer mirrored here — it is the durable
    /// `instance_meta.head` the commit locked and wrote (§6.2) — and node identities
    /// are resolved by in-transaction SQL (§6.1), so no `by_id` index is advanced.
    pub fn apply_committed(&mut self, transition: CommittedTransition) {
        for op in transition.ops() {
            self.apply_op(op);
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
