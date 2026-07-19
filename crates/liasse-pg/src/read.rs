//! The pure-PG read path: one indexed SQL statement per contract read
//! (§4.1/§4.2 of `DESIGN-pure-pg.md`).
//!
//! Every read is a single statement built here and run on any
//! [`postgres::GenericClient`] — a pooled read connection for the `&self`
//! [`crate::store::PgStore::row`]/`scan`, or the admission `Transaction` for the
//! write path's in-transaction id resolution (§6.1). Sharing the builder keeps the
//! chained point lookup identical on both sides.
//!
//! # The chained-InitPlan resolution
//!
//! An address of depth *k* is resolved by *k* nested scalar subqueries hopping
//! `(parent_id, step_name, key_enc)` from the root sentinel (`0`). PostgreSQL plans
//! each hop as an `Index Scan using node_key_lookup` (an `InitPlan`), so the whole
//! walk is index-served with no `Seq Scan` at any depth (EXPLAIN gates 7/8).
//!
//! **Tombstone rule (§4.1, §5.4).** Intermediate hops do NOT filter
//! `value IS NOT NULL`: a resolve walks *through* a tombstoned ancestor to its
//! orphan descendants. Only the outermost level of a live-row read
//! ([`row`]/[`scan`]) adds the `value IS NOT NULL` gate — a tombstone is not a row.
//! The write path's structural [`resolve_id`] omits the gate entirely: it resolves
//! a node's identity whether the node is live or a tombstone.
//!
//! Result addresses are rebuilt from the caller-supplied path plus each row's
//! decoded `key_wire` (via [`crate::node_load::decode_key_wire`]) — never a
//! parent-chain walk.

use liasse_ident::{NameSegment, RowIncarnation};
use liasse_store::{AddressStep, CollectionPath, KeyValue, RowAddress, StoreError, StoredRow};
use liasse_value::Value;
use postgres::GenericClient;
use postgres::Row;
use postgres::types::ToSql;
use serde_json::Value as J;

use crate::backend::{backend, cell, corrupt};
use crate::jsonb_text;
use crate::key_enc;
use crate::value_codec;

/// The self-referential root sentinel: the `parent_id` a depth-1 hop resolves
/// against and the terminus of every parent chain.
const ROOT_SENTINEL_ID: i64 = 0;

/// A boxed bound parameter — every generated statement binds only `step_name`
/// (`text`) and `key_enc` (`bytea`) values, so an owned box over `ToSql` carries
/// them without a bespoke parameter enum.
type Bind = Box<dyn ToSql + Sync>;

/// Push one bound value and return its 1-based placeholder index (`$n`).
fn push(binds: &mut Vec<Bind>, value: impl ToSql + Sync + 'static) -> usize {
    binds.push(Box::new(value));
    binds.len()
}

/// Borrow every bind as the `&[&(dyn ToSql + Sync)]` the driver's `query` API takes.
fn bind_refs(binds: &[Bind]) -> Vec<&(dyn ToSql + Sync)> {
    binds.iter().map(|bind| &**bind as &(dyn ToSql + Sync)).collect()
}

/// Build the chained scalar-subquery (InitPlan) that resolves the surrogate id of
/// the parent whose child levels are `ancestors`, hopping from the root sentinel.
/// No hop filters tombstones — the walk must reach orphans under a tombstoned
/// ancestor (§4.1). Binds each hop's `step_name` then `key_enc` in order.
fn parent_chain(schema: &str, ancestors: &[&AddressStep], binds: &mut Vec<Bind>) -> String {
    let mut chain = ROOT_SENTINEL_ID.to_string();
    for step in ancestors {
        let name = push(binds, step.name().as_str().to_owned());
        let key = push(binds, key_enc::encode_key_value(step.key()));
        chain = format!(
            "(SELECT id FROM {schema}.nodes \
             WHERE parent_id = {chain} AND step_name = ${name} AND key_enc = ${key})"
        );
    }
    chain
}

/// The §4.1 point lookup over an address's full `steps`: the chained InitPlan over
/// the ancestor levels, then the final level's `(step_name, key_enc)`. `live_only`
/// adds the outermost `value IS NOT NULL` tombstone gate — set for a row read,
/// cleared for the write path's structural id resolution.
fn point_lookup(
    schema: &str,
    steps: &[&AddressStep],
    select: &str,
    live_only: bool,
) -> Result<(String, Vec<Bind>), StoreError> {
    let (final_step, ancestors) =
        steps.split_last().ok_or_else(|| corrupt("row address has no steps"))?;
    let mut binds = Vec::new();
    let chain = parent_chain(schema, ancestors, &mut binds);
    let name = push(&mut binds, final_step.name().as_str().to_owned());
    let key = push(&mut binds, key_enc::encode_key_value(final_step.key()));
    let live = if live_only { " AND value IS NOT NULL" } else { "" };
    let sql = format!(
        "SELECT {select} FROM {schema}.nodes \
         WHERE parent_id = {chain} AND step_name = ${name} AND key_enc = ${key}{live}"
    );
    Ok((sql, binds))
}

/// The live row at `address` (§4.1): the chained point lookup with the outermost
/// `value IS NOT NULL` gate, run as one pooled statement. `None` when the address
/// is free or a tombstone.
pub(crate) fn row<C: GenericClient>(
    client: &mut C,
    schema: &str,
    address: &RowAddress,
) -> Result<Option<StoredRow>, StoreError> {
    let steps: Vec<&AddressStep> = address.steps().collect();
    let (sql, binds) = point_lookup(schema, &steps, "incarnation, value", true)?;
    let found = client.query_opt(&sql, &bind_refs(&binds)).map_err(backend)?;
    found.map(|row| stored_row(&row)).transpose()
}

/// The direct live rows of `collection` in Annex B key order (§4.2): the *k−1*
/// ancestor hops via the chained InitPlan, then the ordered child range over the
/// final level. `key_enc` is `BYTEA`, so for a fixed `(parent_id, step_name)` the
/// `node_key_lookup` index order *is* Annex B order — the plan carries no `Sort`
/// (EXPLAIN gate 8). The scalar-subquery parent form is deliberate: a flat JOIN
/// makes the planner insert a `Sort` above the nested loop (§4.2).
pub(crate) fn scan<C: GenericClient>(
    client: &mut C,
    schema: &str,
    collection: &CollectionPath,
) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
    let ancestors = ancestor_steps(collection);
    let ancestor_refs: Vec<&AddressStep> = ancestors.iter().collect();
    let mut binds = Vec::new();
    let chain = parent_chain(schema, &ancestor_refs, &mut binds);
    let name = push(&mut binds, collection.name().as_str().to_owned());
    let sql = format!(
        "SELECT key_wire, incarnation, value FROM {schema}.nodes \
         WHERE parent_id = {chain} AND step_name = ${name} AND value IS NOT NULL ORDER BY key_enc"
    );
    client
        .query(&sql, &bind_refs(&binds))
        .map_err(backend)?
        .iter()
        .map(|row| {
            let wire = jsonb_text::from_jsonb(&cell::<J>(row, "nodes", "key_wire")?);
            let key = crate::node_load::decode_key_wire(&wire)?;
            Ok((collection.row(key), stored_row(row)?))
        })
        .collect()
}

/// Depth cap for the subtree descent (§11, risk 9). A well-formed `nodes`
/// adjacency tree is finite and far shallower than this; reaching it means the
/// table holds a `parent_id` cycle (corruption), which the recursive term would
/// otherwise follow forever. It is a corruption tripwire, not a schema limit —
/// legitimate data never approaches it.
const MAX_SUBTREE_DEPTH: i64 = 10_000;

/// Every live row of the subtree rooted at `address` (excluding the root itself),
/// reached through the declared nested-collection `steps`, in Annex B address
/// order (§7.6 shape-directed descent). One pooled `WITH RECURSIVE` statement:
///
/// - the **anchor** resolves the root by the §4.1 chained-InitPlan point lookup
///   (an `Index Scan using node_key_lookup`); it does NOT filter `value IS NOT
///   NULL`, because a tombstoned root still has orphan descendants to reach and
///   the root row is never emitted anyway;
/// - the **recursive term** joins children on `c.parent_id = p.id AND c.step_name
///   = ANY($steps)`, which keeps the probe on `node_key_lookup` with the `ANY`
///   inside the Index Cond — index-served, no Seq Scan (the `parent_id`-only join
///   plans a Seq Scan + Hash Join and is the pinned anti-pattern, gate 9). It does
///   NOT filter `value IS NOT NULL` either, so it TRAVERSES tombstoned
///   intermediates to their live orphans (the opposite of the coverage CTE, whose
///   tombstone barrier blocks a branch);
/// - each step carries the relative `(step_name, key_wire)` path so the caller
///   rebuilds the descendant's full address without a parent-chain walk.
///
/// Only live rows (`value IS NOT NULL`) below the root are emitted; ordering is
/// done in Rust over the reconstructed [`RowAddress`] (no `Sort` in the plan), so
/// the delivered order is byte-identical to the in-memory oracle's `BTreeMap`
/// order. A `steps`-empty shape needs no query.
pub(crate) fn scan_subtree<C: GenericClient>(
    client: &mut C,
    schema: &str,
    root: &RowAddress,
    steps: &[String],
) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    let root_steps: Vec<&AddressStep> = root.steps().collect();
    let (final_step, ancestors) =
        root_steps.split_last().ok_or_else(|| corrupt("subtree root has no steps"))?;
    let mut binds = Vec::new();
    let chain = parent_chain(schema, ancestors, &mut binds);
    let root_name = push(&mut binds, final_step.name().as_str().to_owned());
    let root_key = push(&mut binds, key_enc::encode_key_value(final_step.key()));
    let steps_param = push(&mut binds, steps.to_vec());
    let depth_param = push(&mut binds, MAX_SUBTREE_DEPTH);
    let sql = format!(
        "WITH RECURSIVE sub AS ( \
           SELECT n.id, '[]'::jsonb AS rel_path, 0::bigint AS depth, n.incarnation, n.value \
           FROM {schema}.nodes n \
           WHERE n.parent_id = {chain} AND n.step_name = ${root_name} AND n.key_enc = ${root_key} \
         UNION ALL \
           SELECT c.id, \
                  p.rel_path || jsonb_build_array(jsonb_build_array(to_jsonb(c.step_name), c.key_wire)), \
                  p.depth + 1, c.incarnation, c.value \
           FROM sub p \
           JOIN {schema}.nodes c ON c.parent_id = p.id AND c.step_name = ANY(${steps_param}) \
           WHERE p.depth < ${depth_param} \
         ) \
         SELECT rel_path, depth, incarnation, value FROM sub WHERE depth > 0 AND value IS NOT NULL"
    );
    let rows = client.query(&sql, &bind_refs(&binds)).map_err(backend)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        if cell::<i64>(row, "sub", "depth")? >= MAX_SUBTREE_DEPTH {
            return Err(corrupt(format!(
                "subtree descent from `{}` reached depth {MAX_SUBTREE_DEPTH}; `nodes` holds a parent_id cycle",
                root.render()
            )));
        }
        let rel_path = jsonb_text::from_jsonb(&cell::<J>(row, "sub", "rel_path")?);
        out.push((rebuild_address(root, &rel_path)?, stored_row(row)?));
    }
    // Order in Rust by the reconstructed address (Annex B / `RowAddress` order),
    // matching the in-memory oracle exactly and keeping the plan sort-free.
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(out)
}

/// Rebuild a descendant's full address from `root` plus the relative
/// `(step_name, key_wire)` pairs the subtree CTE carried, decoding each level's
/// key with the same `key_wire` inverter the scan uses.
fn rebuild_address(root: &RowAddress, rel_path: &J) -> Result<RowAddress, StoreError> {
    let pairs = rel_path.as_array().ok_or_else(|| corrupt("subtree rel_path is not an array"))?;
    let mut address = root.clone();
    for pair in pairs {
        let pair = pair.as_array().ok_or_else(|| corrupt("subtree rel_path level is not a pair"))?;
        let name = pair
            .first()
            .and_then(J::as_str)
            .ok_or_else(|| corrupt("subtree rel_path level has no step name"))?;
        let wire = pair.get(1).ok_or_else(|| corrupt("subtree rel_path level has no key_wire"))?;
        let key = crate::node_load::decode_key_wire(wire)?;
        address = address.child(AddressStep::new(NameSegment::new(name), key));
    }
    Ok(address)
}

/// Resolve the surrogate id of the node at `address` — live row OR tombstone — as
/// one statement on the admission transaction (§6.1). No level filters
/// `value IS NOT NULL`, so a tombstoned ancestor resolves to its id exactly as the
/// former `by_id` map did. `None` when no node exists there.
pub(crate) fn resolve_id<C: GenericClient>(
    client: &mut C,
    schema: &str,
    address: &RowAddress,
) -> Result<Option<i64>, StoreError> {
    let steps: Vec<&AddressStep> = address.steps().collect();
    let (sql, binds) = point_lookup(schema, &steps, "id", false)?;
    match client.query_opt(&sql, &bind_refs(&binds)).map_err(backend)? {
        Some(row) => Ok(Some(cell::<i64>(&row, "nodes", "id")?)),
        None => Ok(None),
    }
}

/// Decode a live node row's `(incarnation, value)` columns into a [`StoredRow`]
/// with the same NUL-safe codecs the write path used.
fn stored_row(row: &Row) -> Result<StoredRow, StoreError> {
    let incarnation = cell::<String>(row, "nodes", "incarnation")?;
    let value = value_codec::decode(&jsonb_text::from_jsonb(&cell::<J>(row, "nodes", "value")?))?;
    Ok(StoredRow::new(RowIncarnation::new(incarnation), value))
}

/// The *k−1* ancestor address steps of `collection` — the parent levels the scan
/// resolves through. `CollectionPath` exposes its ancestors only through a built
/// row address, so probe with a throwaway key and drop the own-collection step,
/// whose key never enters the parent chain (only its declaration name does).
fn ancestor_steps(collection: &CollectionPath) -> Vec<AddressStep> {
    let mut steps: Vec<AddressStep> =
        collection.row(KeyValue::single(Value::Bool(false))).steps().cloned().collect();
    steps.pop();
    steps
}
