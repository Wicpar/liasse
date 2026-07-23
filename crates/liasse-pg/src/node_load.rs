//! Materialize head committed state from the durable `nodes` adjacency tree — the
//! `snapshot(head)` fast path (`DESIGN-pure-pg.md` §4.3, Phase 6) — and decode a
//! node's `key_wire` column back into a typed [`KeyValue`].
//!
//! `key_wire` is the canonical, self-describing key form the write path stores
//! ([`crate::node_write`]); [`decode_key_wire`] inverts it — the SQL scan
//! ([`crate::read`]) decodes each child row's `key_wire` to rebuild that row's
//! address, since the read path never walks a parent chain. `key_enc` is the
//! order-preserving companion column and is never inverted (it exists only for the
//! lookup/scan index).
//!
//! # The head fast path (§4.3)
//!
//! `snapshot(frontier)` normally folds the append-only `commit_log` prefix
//! `≤ frontier`, which is O(history). But at `frontier == head` the `nodes` tree
//! already **is** head state, so [`materialize_head`] reconstructs the live-row set
//! straight from it in ONE full read of `nodes` (O(state), not O(history)). This is
//! the whole-tree reconstruction the in-memory projection used on open, minus the
//! `by_id` structural index the projection carried (Phase 3 retired it); the
//! resulting [`BTreeMap`] is exactly a [`liasse_store::Snapshot`]'s live-row map, so
//! the store wraps it with [`liasse_store::Snapshot::from_rows`]. The reconstruction
//! reuses the *same* value/key codecs the parity-gated `row`/`scan` reads use and
//! walks the *same* tombstone-through adjacency chain, so its `Snapshot` is
//! byte-identical to the log fold at head (the tree-≡-log-fold equivalence, gated by
//! `node_tree_consistency::head_fast_path_equals_log_fold`).
//!
//! The one statement legitimately reads the whole table: a full-state
//! materialization has no selective plan, so it is EXEMPT from the no-Seq-Scan index
//! gate, pinned like the single-row `instance_meta` reads
//! (`index_coverage_pg::head_fast_path_is_single_full_scan_exempt`).
//!
//! # Rows vs tombstones
//!
//! A node is a structural *position*; a *row* is a node carrying a value. The
//! materialized map holds only **live** nodes (`value IS NOT NULL`). A **tombstone**
//! (`value IS NULL`) is a deleted non-leaf ancestor retained so its descendant rows
//! stay addressable (§5.4 logical orphans); it is not itself a row, so it is never
//! emitted. The parent-chain walk MUST still traverse tombstones: a tombstone
//! contributes its `step_name` + decoded `key_wire` to a descendant's address even
//! though it is not a row. This is what makes the head fast path reproduce the exact
//! log-fold live set — a top-level drop leaves its nested rows as orphans and the
//! walk reconstructs their addresses through the tombstoned ancestor.

use std::collections::BTreeMap;

use liasse_ident::{NameSegment, RowIncarnation};
use liasse_store::{AddressStep, KeyValue, RowAddress, StoreError, StoredRow, key_from_components};
use postgres::GenericClient;
use serde_json::Value as J;

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::value_codec;

/// The self-referential root sentinel: the `parent_id` of every depth-1 node and
/// the terminus of every parent-walk. It is not itself a row.
const ROOT_SENTINEL_ID: i64 = 0;

/// One decoded node: its parent link, its own address level, and — for a *row*, as
/// opposed to a tombstone — its stored value. `row` is `None` for a tombstone: it
/// still contributes its level to descendants' addresses (so it is kept in the map
/// the parent-walk indexes) but is never emitted as a row.
struct Node {
    parent: i64,
    step: AddressStep,
    row: Option<StoredRow>,
}

/// Reconstruct the live committed row set at head straight from `schema`'s `nodes`
/// table — the §4.3 head fast path. One full read of `nodes`, then a Rust
/// parent-chain walk (through tombstones) that rebuilds each live node's
/// [`RowAddress`]. The returned map is a [`liasse_store::Snapshot`]'s live-row map;
/// its `BTreeMap` self-sorts to Annex-B order, so a downstream scan enumerates a
/// collection in key order with no explicit sort.
pub(crate) fn materialize_head<C: GenericClient>(
    client: &mut C,
    schema: &str,
) -> Result<BTreeMap<RowAddress, StoredRow>, StoreError> {
    // ONE statement, the whole table (the pinned no-Seq-Scan exemption). Kept
    // byte-for-byte in sync with `index_coverage_pg::head_fast_path...`, which
    // EXPLAINs this exact SQL to pin the exemption rationale.
    let mut nodes: BTreeMap<i64, Node> = BTreeMap::new();
    for row in client
        .query(
            &format!(
                "SELECT id, parent_id, step_name, key_wire, incarnation, value, created \
                 FROM {schema}.nodes"
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
        // A live row has both `value` and `incarnation` (and a `created` admission
        // instant); a tombstone has neither (the schema's `CHECK` keeps value and
        // incarnation co-NULL, and a tombstone's `created` is NULL too). Decode a row,
        // or record a tombstone that still carries its address level for the walk.
        let value = cell::<Option<J>>(&row, "nodes", "value")?;
        let incarnation = cell::<Option<String>>(&row, "nodes", "incarnation")?;
        let node = match (value, incarnation) {
            (Some(value), Some(incarnation)) => {
                let created = match cell::<Option<J>>(&row, "nodes", "created")? {
                    Some(wire) => value_codec::decode_created(&jsonb_text::from_jsonb(&wire))?,
                    None => return Err(corrupt("live node is missing its `created` admission instant")),
                };
                Some(StoredRow::new(
                    RowIncarnation::new(incarnation),
                    created,
                    value_codec::decode(&jsonb_text::from_jsonb(&value))?,
                ))
            }
            (None, None) => None,
            _ => return Err(corrupt("node has exactly one of value/incarnation set")),
        };
        nodes.insert(id, Node { parent, step: AddressStep::new(NameSegment::new(name), key), row: node });
    }

    // Emit only *live* nodes, reconstructing each address by walking the parent chain
    // through ALL nodes (tombstones included, since a tombstoned ancestor still
    // contributes an address level).
    let mut rows = BTreeMap::new();
    for (&id, node) in &nodes {
        if let Some(row) = &node.row {
            rows.insert(reconstruct(id, &nodes)?, row.clone());
        }
    }
    Ok(rows)
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
/// Shared by the SQL scan ([`crate::read`]), which decodes each child row's
/// `key_wire` to rebuild its address, and the head-fast-path materialization above.
pub(crate) fn decode_key_wire(wire: &J) -> Result<KeyValue, StoreError> {
    let components = wire.as_array().ok_or_else(|| corrupt("node key_wire is not an array"))?;
    let values = components.iter().map(value_codec::decode).collect::<Result<Vec<_>, _>>()?;
    key_from_components(values)
}
