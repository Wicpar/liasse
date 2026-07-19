//! Decomposing a checked flat `$view` into its faces (§7.1/§7.3), so a lowering
//! pass can build a row-program without reaching into the `pub(crate)` typed tree.
//!
//! A flat `$view` is `Project { source, projection }` where `source` is a bare
//! top-level collection (`.coll` / `/coll`) or that collection under a
//! `[:name | condition]` filter. This module recovers the filter, the projection
//! outputs, the `$sort` keys with their directions, and the `$skip`/`$limit`
//! bounds — every part a residual face needs — as public [`TypedExpr`]s. A grouped
//! (`$key`) projection, a `$quantity` pool source, or any non-flat source shape
//! yields `None`: it does not lower and the caller falls back to the interpreter
//! (§7.5).

use crate::typed::{TypedExpr, TypedKind, TypedSelector};

/// A decomposed flat `$view` — the parts a lowering pass turns into a row program.
#[derive(Debug, Clone)]
pub struct FlatView {
    /// The source collection's addressing name (`.coll`).
    pub collection: String,
    /// The `[:name | …]` filter bind name, when the source is filtered.
    pub bind: Option<String>,
    /// The filter condition, when the source is filtered.
    pub filter: Option<TypedExpr>,
    /// The projection outputs in dependency order (name → expression).
    pub outputs: Vec<(String, TypedExpr)>,
    /// The `$sort` keys, highest priority first, each with its descending flag.
    pub sort: Vec<(TypedExpr, bool)>,
    /// The `$skip` bound.
    pub skip: Option<u64>,
    /// The `$limit` bound.
    pub limit: Option<u64>,
}

/// Decompose a checked flat `$view` expression, or `None` when it is not a flat
/// collection projection (a grouped/meter projection, a combinator, a traversal, a
/// nested source) — those do not lower in v1.
#[must_use]
pub fn lower_flat_view(expr: &TypedExpr) -> Option<FlatView> {
    let TypedKind::Project { source, projection } = expr.kind() else {
        return None;
    };
    // §7.2/§15.1: a synthetic-`$key` grouping and a `$quantity` pool source are not
    // flat row projections — they do not lower in v1.
    if !projection.key.is_empty() || projection.quantity.is_some() {
        return None;
    }
    let (collection, bind, filter) = lower_source(source)?;
    Some(FlatView {
        collection,
        bind,
        filter,
        outputs: projection.outputs.iter().map(|o| (o.name.clone(), o.expr.clone())).collect(),
        sort: projection.sort.iter().map(|k| (k.expr.clone(), k.descending)).collect(),
        skip: projection.skip,
        limit: projection.limit,
    })
}

/// Recover the source collection name, the filter bind name, and the filter
/// condition from a projection's source.
fn lower_source(source: &TypedExpr) -> Option<(String, Option<String>, Option<TypedExpr>)> {
    match source.kind() {
        TypedKind::Select { base, selector: TypedSelector::Bind { name, condition } } => {
            let collection = collection_name(base)?;
            Some((collection, Some(name.clone()), condition.as_ref().map(|c| (**c).clone())))
        }
        _ => {
            let collection = collection_name(source)?;
            Some((collection, None, None))
        }
    }
}

/// The single top-level collection a bare `.coll`/`/coll` field addresses.
fn collection_name(expr: &TypedExpr) -> Option<String> {
    match expr.kind() {
        TypedKind::Field { base, name } if is_root_receiver(base) => Some(name.clone()),
        _ => None,
    }
}

/// Whether an expression is the root receiver (`.` or `/`) a top-level collection
/// is read off.
fn is_root_receiver(expr: &TypedExpr) -> bool {
    matches!(expr.kind(), TypedKind::Current | TypedKind::Root)
}
