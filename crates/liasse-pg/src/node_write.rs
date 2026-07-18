//! Node-adjacency dual-write: mirror every committed row op into the `nodes`
//! adjacency tree by surrogate id, and rebuild the address→id map on open.
//!
//! Phase 1 keeps the flat `rows` table authoritative for reads; the `nodes` tree
//! is written in lock-step inside the *same* admission transaction so a later
//! phase can flip reads onto it without a schema migration. Every op is applied
//! **by the surrogate node id**, resolved in O(1) from a per-transaction map
//! ([`NodeWriter::staged`]) layered over the committed [`by_id`](crate::projection)
//! side map:
//!
//! - **Insert** allocates a fresh node under its parent (the root sentinel `0` at
//!   depth 1, else the parent row's node) and returns its id.
//! - **Update** rewrites the resolved node's value/incarnation in place.
//! - **Delete** removes the resolved node AND its descendant subtree by id (§5.4):
//!   the runtime does not always emit a `Delete` per nested row (a top-level drop
//!   leaves them as logical orphans the flat `rows` table keeps), but an adjacency
//!   tree cannot dangle a child off a deleted parent, so the subtree is cascaded by
//!   id — keeping the tree a walkable fold of reachable state, and tolerating a
//!   parent-first delete via the `DEFERRABLE` self-FK.
//! - **Rekey** moves the *same* id to a new parent/key, so the row's descendants —
//!   whose `parent_id` still names that stable id — move with it untouched. This is
//!   the whole reason for a surrogate id.
//!
//! `key_enc` (the order-preserving `BYTEA`) is written for lookup/scan order;
//! `key_wire` (canonical, self-describing JSONB) is written so a load can decode
//! the level key back into a [`KeyValue`] and reconstruct the address.

use std::collections::BTreeMap;

use liasse_ident::NameSegment;
use liasse_store::{
    AddressStep, CommittedRowOp, KeyValue, RowAddress, StoreError, key_from_components,
};
use postgres::{Client, Transaction};
use serde_json::Value as J;

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::key_enc;
use crate::value_codec;

/// The self-referential root sentinel: the `parent_id` of every depth-1 node and
/// the terminus of every parent-walk.
const ROOT_SENTINEL_ID: i64 = 0;

/// Applies a commit's row ops to the `nodes` table by surrogate id, resolving each
/// address to its node id from the committed `by_id` map plus the ids this
/// transaction has itself just created or moved.
pub(crate) struct NodeWriter<'a> {
    /// The quoted schema name, e.g. `"liasse_…"`.
    schema: &'a str,
    /// The committed address→id map — the durable node identities before this txn.
    committed: &'a BTreeMap<RowAddress, i64>,
    /// Addresses inserted or rekeyed-into during THIS transaction, and their ids;
    /// consulted before `committed` so a nested child sees its just-inserted parent.
    staged: BTreeMap<RowAddress, i64>,
    /// The id of each freshly inserted node, in op order — handed to the projection
    /// so it can advance `by_id` after the commit succeeds.
    new_ids: Vec<i64>,
}

impl<'a> NodeWriter<'a> {
    /// Open a writer over the committed identities for one admission transaction.
    pub(crate) fn new(schema: &'a str, committed: &'a BTreeMap<RowAddress, i64>) -> Self {
        Self { schema, committed, staged: BTreeMap::new(), new_ids: Vec::new() }
    }

    /// The ids of the nodes this transaction inserted, one per `Insert` op in op
    /// order — the projection replays the same op order to advance `by_id`.
    pub(crate) fn into_new_ids(self) -> Vec<i64> {
        self.new_ids
    }

    /// Mirror one committed op into the node tree.
    pub(crate) fn apply(
        &mut self,
        txn: &mut Transaction<'_>,
        op: &CommittedRowOp,
    ) -> Result<(), StoreError> {
        match op {
            CommittedRowOp::Insert { address, incarnation, value } => {
                let parent = self.resolve_parent(address)?;
                let step = last_step(address)?;
                let row = txn
                    .query_one(
                        &format!(
                            "INSERT INTO {}.nodes \
                             (parent_id, step_name, key_enc, key_wire, incarnation, value) \
                             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                            self.schema
                        ),
                        &[
                            &parent,
                            &step.name().as_str(),
                            &key_enc::encode_key_value(step.key()),
                            &jsonb_text::to_jsonb(&encode_key_wire(step.key())),
                            &incarnation.as_str(),
                            &jsonb_text::to_jsonb(&value_codec::encode(value)),
                        ],
                    )
                    .map_err(backend)?;
                let id = cell::<i64>(&row, "nodes", "id")?;
                self.staged.insert(address.clone(), id);
                self.new_ids.push(id);
            }
            CommittedRowOp::Update { address, incarnation, value } => {
                let id = self.resolve_id(address)?;
                txn.execute(
                    &format!(
                        "UPDATE {}.nodes SET value = $1, incarnation = $2 WHERE id = $3",
                        self.schema
                    ),
                    &[
                        &jsonb_text::to_jsonb(&value_codec::encode(value)),
                        &incarnation.as_str(),
                        &id,
                    ],
                )
                .map_err(backend)?;
            }
            CommittedRowOp::Delete { address, .. } => {
                let id = self.resolve_id(address)?;
                // Delete the node AND its whole descendant subtree, by id. A row's
                // nested rows are its descendants (§5.4), so removing a row removes
                // them — a proper hierarchy. The runtime does not always emit an
                // explicit `Delete` per descendant: a top-level row drop (§21.1)
                // removes only the row itself and leaves its nested rows as logical
                // orphans (the flat `rows` table keeps them, unreachable). An
                // adjacency tree cannot: an orphaned child node would dangle off a
                // deleted parent and violate the self-FK. Cascading by id here keeps
                // the tree a valid, walkable fold of *reachable* state — issued by id,
                // not by SQL `ON DELETE CASCADE`, so the delete set stays explicit. A
                // node already gone (a child whose parent was deleted first) makes the
                // recursion match nothing, so a redundant per-row delete is a no-op.
                txn.execute(
                    &format!(
                        "WITH RECURSIVE subtree(id) AS (\
                           SELECT id FROM {schema}.nodes WHERE id = $1 \
                           UNION ALL \
                           SELECT n.id FROM {schema}.nodes n JOIN subtree s ON n.parent_id = s.id\
                         ) DELETE FROM {schema}.nodes WHERE id IN (SELECT id FROM subtree)",
                        schema = self.schema
                    ),
                    &[&id],
                )
                .map_err(backend)?;
                self.staged.remove(address);
            }
            CommittedRowOp::Rekey { from, to, incarnation: _, value } => {
                // A rekey keeps the row's incarnation (unchanged here) and its
                // surrogate id, so descendants that reference the id move with it.
                let id = self.resolve_id(from)?;
                let parent = self.resolve_parent(to)?;
                let step = last_step(to)?;
                txn.execute(
                    &format!(
                        "UPDATE {}.nodes SET \
                         parent_id = $1, step_name = $2, key_enc = $3, key_wire = $4, value = $5 \
                         WHERE id = $6",
                        self.schema
                    ),
                    &[
                        &parent,
                        &step.name().as_str(),
                        &key_enc::encode_key_value(step.key()),
                        &jsonb_text::to_jsonb(&encode_key_wire(step.key())),
                        &jsonb_text::to_jsonb(&value_codec::encode(value)),
                        &id,
                    ],
                )
                .map_err(backend)?;
                self.staged.remove(from);
                self.staged.insert(to.clone(), id);
            }
        }
        Ok(())
    }

    /// The node id of `address`: staged (this txn) first, then committed. A missing
    /// id means an op referenced a row with no node — a durable inconsistency.
    fn resolve_id(&self, address: &RowAddress) -> Result<i64, StoreError> {
        self.staged
            .get(address)
            .or_else(|| self.committed.get(address))
            .copied()
            .ok_or_else(|| corrupt(format!("no node for row address {}", address.render())))
    }

    /// The parent node id of `address`: the root sentinel for a top-level row, else
    /// the id of the row one level up (which must already be a node).
    fn resolve_parent(&self, address: &RowAddress) -> Result<i64, StoreError> {
        match parent_address(address) {
            None => Ok(ROOT_SENTINEL_ID),
            Some(parent) => self.resolve_id(&parent),
        }
    }
}

/// Rebuild the address→id map from the durable node tree — the write path's O(1)
/// parent/id resolver — by reading every node and walking each one's parent chain
/// to the root sentinel. Run once on open, beside the `rows` load.
pub(crate) fn load_by_id(
    client: &mut Client,
    schema: &str,
) -> Result<BTreeMap<RowAddress, i64>, StoreError> {
    // id -> (parent_id, this level's step); the sentinel is dropped, so a walk that
    // reaches parent_id == 0 has left the tree.
    let mut steps: BTreeMap<i64, (i64, AddressStep)> = BTreeMap::new();
    for row in client
        .query(&format!("SELECT id, parent_id, step_name, key_wire FROM {schema}.nodes"), &[])
        .map_err(backend)?
    {
        let id = cell::<i64>(&row, "nodes", "id")?;
        if id == ROOT_SENTINEL_ID {
            continue;
        }
        let parent_id = cell::<i64>(&row, "nodes", "parent_id")?;
        let name = cell::<String>(&row, "nodes", "step_name")?;
        let key = decode_key_wire(&jsonb_text::from_jsonb(&cell::<J>(&row, "nodes", "key_wire")?))?;
        steps.insert(id, (parent_id, AddressStep::new(NameSegment::new(name), key)));
    }

    let mut by_id = BTreeMap::new();
    for &id in steps.keys() {
        by_id.insert(reconstruct(id, &steps)?, id);
    }
    Ok(by_id)
}

/// Walk `start`'s parent chain to the root sentinel, assembling its full address.
/// A chain longer than the whole node set cannot terminate at the root — that is a
/// cyclic corruption, reported rather than looped on forever.
fn reconstruct(
    start: i64,
    steps: &BTreeMap<i64, (i64, AddressStep)>,
) -> Result<RowAddress, StoreError> {
    let mut chain = Vec::new();
    let mut id = start;
    loop {
        let (parent, step) = steps.get(&id).ok_or_else(|| corrupt("node parent chain is broken"))?;
        chain.push(step.clone());
        if *parent == ROOT_SENTINEL_ID {
            break;
        }
        if chain.len() > steps.len() {
            return Err(corrupt("node parent chain does not terminate at the root sentinel"));
        }
        id = *parent;
    }
    chain.reverse();
    let mut levels = chain.into_iter();
    let first = levels.next().ok_or_else(|| corrupt("node has no address steps"))?;
    let mut address = RowAddress::root(first);
    for step in levels {
        address = address.child(step);
    }
    Ok(address)
}

/// The final (own-collection) level of an address — the step this node stores.
/// A [`RowAddress`] is non-empty by construction, so this only fails on corruption.
fn last_step(address: &RowAddress) -> Result<&AddressStep, StoreError> {
    address.steps().last().ok_or_else(|| corrupt("row address has no steps"))
}

/// The address of a row's parent — itself minus its final level — or `None` when
/// the row is top-level (its parent is the root sentinel).
fn parent_address(address: &RowAddress) -> Option<RowAddress> {
    let mut steps: Vec<AddressStep> = address.steps().cloned().collect();
    steps.pop();
    let mut levels = steps.into_iter();
    let first = levels.next()?;
    let mut parent = RowAddress::root(first);
    for step in levels {
        parent = parent.child(step);
    }
    Some(parent)
}

/// The `key_wire` column for a level: the key's components in the same canonical,
/// self-describing form the flat `rows` address key uses, so a load can decode it.
fn encode_key_wire(key: &KeyValue) -> J {
    J::Array(key.components().map(value_codec::encode_key).collect())
}

/// Invert [`encode_key_wire`]: rebuild a [`KeyValue`] from a node's `key_wire`.
fn decode_key_wire(wire: &J) -> Result<KeyValue, StoreError> {
    let components = wire.as_array().ok_or_else(|| corrupt("node key_wire is not an array"))?;
    let values = components.iter().map(value_codec::decode).collect::<Result<Vec<_>, _>>()?;
    key_from_components(values)
}
