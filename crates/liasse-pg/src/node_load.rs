//! Decode a node's `key_wire` column back into a typed [`KeyValue`].
//!
//! `key_wire` is the canonical, self-describing key form the write path stores
//! ([`crate::node_write`]); [`decode_key_wire`] inverts it — the SQL scan
//! ([`crate::read`]) decodes each child row's `key_wire` to rebuild that row's
//! address, since the read path never walks a parent chain (`DESIGN-pure-pg.md`
//! §4). `key_enc` is the order-preserving companion column and is never inverted
//! (it exists only for the lookup/scan index).
//!
//! The whole-tree reconstruction that once rebuilt the in-memory projection on open
//! is gone with the projection (Phase 3): reads are served directly from `nodes` by
//! indexed SQL. Phase 6's `snapshot(head)` fast path (§4.3) will reintroduce a
//! `nodes`-to-address materialization when it lands; until then, this single-level
//! key decoder is all the module holds.

use liasse_store::{KeyValue, StoreError, key_from_components};
use serde_json::Value as J;

use crate::backend::corrupt;
use crate::value_codec;

/// Invert the `key_wire` column: rebuild a level's [`KeyValue`] from its canonical,
/// self-describing JSON components (the same form [`crate::node_write`] writes).
/// Used by the SQL scan ([`crate::read`]) to rebuild each child row's address.
pub(crate) fn decode_key_wire(wire: &J) -> Result<KeyValue, StoreError> {
    let components = wire.as_array().ok_or_else(|| corrupt("node key_wire is not an array"))?;
    let values = components.iter().map(value_codec::decode).collect::<Result<Vec<_>, _>>()?;
    key_from_components(values)
}
