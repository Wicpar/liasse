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

use liasse_ident::RowIncarnation;
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
