//! Rebuild the in-memory read model from the durable `nodes` adjacency tree — the
//! sole durable representation of committed rows.
//!
//! Opening (or reopening) a store loads the whole committed row set with one pass
//! over `nodes`: read every node, walk each non-sentinel node's `parent_id` chain
//! to the root sentinel (`id = 0`) assembling its [`RowAddress`] top-down from each
//! level's `step_name` + decoded `key_wire`, and decode its stored `value`. The
//! same pass yields both maps the store keeps:
//!
//! - `current`: address → [`StoredRow`] — the base every `&self` read overlays. The
//!   `BTreeMap` self-sorts to Annex B order ([`RowAddress`]'s `Ord`), so a scan
//!   enumerates a collection in key order with no explicit sort.
//! - `by_id`: address → surrogate node id — the write path's O(1) parent/id
//!   resolver ([`crate::node_write`]).
//!
//! # Rows vs tombstones
//!
//! A node is a structural *position*; a *row* is a node carrying a value. `current`
//! and `by_id` hold only **live** nodes (`value IS NOT NULL`). A **tombstone**
//! (`value IS NULL`) is a deleted non-leaf ancestor retained so its descendant rows
//! stay addressable (§5.4 logical orphans); it is not itself a row, so it is never
//! emitted. But the parent-chain walk MUST still traverse tombstones: a tombstone
//! contributes its `step_name` + decoded `key_wire` to a descendant's address even
//! though it is not a row. All non-sentinel nodes are therefore read into the map
//! that the walk indexes, and the live/tombstone distinction is applied only when
//! deciding what to emit. This is what makes a reopen reproduce the exact live
//! projection: a top-level drop leaves its nested rows as orphans, and the walk
//! reconstructs their addresses through the tombstoned ancestor.
//!
//! `key_wire` is the canonical, self-describing key form (decoded here); `key_enc`
//! is never inverted — it exists only for the lookup/scan index.

use std::collections::BTreeMap;

use liasse_ident::{NameSegment, RowIncarnation};
use liasse_store::{AddressStep, RowAddress, StoreError, StoredRow, key_from_components};
use postgres::Client;
use serde_json::Value as J;

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::value_codec;

/// The self-referential root sentinel: the `parent_id` of every depth-1 node and
/// the terminus of every parent-walk. It is not itself a row.
const ROOT_SENTINEL_ID: i64 = 0;

/// The committed row set reconstructed from the node tree: the current rows keyed
/// by address (the read path's base) and each row's surrogate node id (the write
/// path's resolver).
pub(crate) struct NodeTree {
    pub current: BTreeMap<RowAddress, StoredRow>,
    pub by_id: BTreeMap<RowAddress, i64>,
}

/// One decoded node: its parent link, its own address level, and — for a *row*, as
/// opposed to a tombstone — its stored value. `row` is `None` for a tombstone: it
/// still contributes its level to descendants' addresses (so it is kept in the map
/// the parent-walk indexes) but is never emitted as a row.
struct Node {
    parent: i64,
    step: AddressStep,
    row: Option<StoredRow>,
}

/// Reconstruct the whole committed row set from `schema`'s `nodes` table.
pub(crate) fn load(client: &mut Client, schema: &str) -> Result<NodeTree, StoreError> {
    let mut nodes: BTreeMap<i64, Node> = BTreeMap::new();
    for row in client
        .query(
            &format!(
                "SELECT id, parent_id, step_name, key_wire, incarnation, value FROM {schema}.nodes"
            ),
            &[],
        )
        .map_err(backend)?
    {
        let id = cell::<i64>(&row, "nodes", "id")?;
        if id == ROOT_SENTINEL_ID {
            continue;
        }
        let parent = cell::<i64>(&row, "nodes", "parent_id")?;
        let name = cell::<String>(&row, "nodes", "step_name")?;
        let key = decode_key_wire(&jsonb_text::from_jsonb(&cell::<J>(&row, "nodes", "key_wire")?))?;
        // A live row has both `value` and `incarnation`; a tombstone has neither (the
        // schema's `CHECK` keeps them co-NULL). Decode a row, or record a tombstone
        // that still carries its address level for the walk.
        let value = cell::<Option<J>>(&row, "nodes", "value")?;
        let incarnation = cell::<Option<String>>(&row, "nodes", "incarnation")?;
        let node = match (value, incarnation) {
            (Some(value), Some(incarnation)) => Some(StoredRow::new(
                RowIncarnation::new(incarnation),
                value_codec::decode(&jsonb_text::from_jsonb(&value))?,
            )),
            (None, None) => None,
            _ => return Err(corrupt("node has exactly one of value/incarnation set")),
        };
        nodes.insert(
            id,
            Node { parent, step: AddressStep::new(NameSegment::new(name), key), row: node },
        );
    }

    // Emit only live nodes as rows, but reconstruct each address by walking the
    // parent chain through ALL nodes (tombstones included).
    let mut current = BTreeMap::new();
    let mut by_id = BTreeMap::new();
    for (&id, node) in &nodes {
        let Some(row) = &node.row else { continue };
        let address = reconstruct(id, &nodes)?;
        current.insert(address.clone(), row.clone());
        by_id.insert(address, id);
    }
    Ok(NodeTree { current, by_id })
}

/// Walk `start`'s parent chain to the root sentinel, assembling its full address
/// top-down. A chain longer than the whole node set cannot terminate at the root —
/// that is a cyclic corruption, reported rather than looped on forever.
fn reconstruct(start: i64, nodes: &BTreeMap<i64, Node>) -> Result<RowAddress, StoreError> {
    let mut chain = Vec::new();
    let mut id = start;
    loop {
        let node = nodes.get(&id).ok_or_else(|| corrupt("node parent chain is broken"))?;
        chain.push(node.step.clone());
        if node.parent == ROOT_SENTINEL_ID {
            break;
        }
        if chain.len() > nodes.len() {
            return Err(corrupt("node parent chain does not terminate at the root sentinel"));
        }
        id = node.parent;
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

/// Invert the `key_wire` column: rebuild a level's [`KeyValue`] from its canonical,
/// self-describing JSON components (the same form [`crate::node_write`] writes).
fn decode_key_wire(wire: &J) -> Result<liasse_store::KeyValue, StoreError> {
    let components = wire.as_array().ok_or_else(|| corrupt("node key_wire is not an array"))?;
    let values = components.iter().map(value_codec::decode).collect::<Result<Vec<_>, _>>()?;
    key_from_components(values)
}
