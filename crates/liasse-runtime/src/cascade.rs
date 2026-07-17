//! Wiring the `$on_delete` cascade planner (§21.1) into mutation admission.
//!
//! A `collection - key` delete statement removes a row, but §21.1 makes that a
//! graph operation: every inbound reference's declared policy — `restrict`,
//! `cascade`, `none`, or a `= { … }` patch — decides the fate of the rows that
//! point at a deleted one. This module reads the prospective state into the
//! [`Graph`](crate::deletion::Graph) the planner operates on, resolves each
//! reference's compiled policy (evaluating a patch against the referencing row
//! with the deleted target bound as `$target`), and returns the plan together
//! with the address of every row it names, so the interpreter can apply the
//! removals and surviving-row patches atomically.

use std::collections::BTreeMap;

use liasse_ident::NameSegment;
use liasse_store::{CollectionPath, RowAddress};
use liasse_value::{RefKey, Value};

use crate::compiled::{Compiled, OnDelete};
use crate::deletion::{DeleteError, DeletePolicy, DeletionPlan, Graph, RefEdge, RowRef};
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize;
use crate::refid::identity_of;
use crate::state::Prospective;

/// A planned deletion (§21.1): the fixed-point plan plus the live address of
/// every row it names, so the interpreter can locate rows to remove or patch.
pub(crate) struct PlannedDeletion {
    pub(crate) plan: DeletionPlan,
    pub(crate) addresses: BTreeMap<RowRef, RowAddress>,
}

/// Plan the deletion of `initial` under every inbound `$on_delete` policy,
/// reading the graph out of `prospective`. A `restrict` block or a conflicting
/// patch is a [`Rejection`] (§21.1).
pub(crate) fn plan(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    initial: &[RowRef],
) -> Result<PlannedDeletion, Rejection> {
    let mut graph = Graph::new();
    let mut addresses = BTreeMap::new();

    // 1. Every live row becomes a graph node, keyed by (collection, key).
    for collection in &compiled.collections {
        let path = CollectionPath::top(NameSegment::new(collection.name.clone()));
        let Some(model) = ctx.schema.top_collection(&collection.name) else { continue };
        for address in prospective.addresses_in(&path) {
            let Some(fields) = prospective.get(&address) else { continue };
            let Some(step) = address.steps().last() else { continue };
            let key = materialize::key_identity(model, step.key());
            let row = RowRef::new(collection.name.clone(), key);
            graph.add_row(row.clone(), fields.clone());
            addresses.insert(row, address.clone());
        }
    }

    // 2. Every live reference becomes an inbound edge under its compiled policy.
    for collection in &compiled.collections {
        let path = CollectionPath::top(NameSegment::new(collection.name.clone()));
        let Some(model) = ctx.schema.top_collection(&collection.name) else { continue };
        for address in prospective.addresses_in(&path) {
            let Some(fields) = prospective.get(&address) else { continue };
            let Some(step) = address.steps().last() else { continue };
            let from = RowRef::new(collection.name.clone(), materialize::key_identity(model, step.key()));
            for field in &collection.fields {
                let Some(info) = &field.reference else { continue };
                let Some(target_key) = ref_key(fields.get(&field.name)) else { continue };
                let to = RowRef::new(info.target.clone(), target_key);
                let Some(policy) = resolve_policy(compiled, ctx, prospective, &info.on_delete, &from, &to)?
                else {
                    continue;
                };
                graph.add_edge(RefEdge { from: from.clone(), field: field.name.clone(), to, policy });
            }
        }
    }

    match graph.plan(initial) {
        Ok(plan) => Ok(PlannedDeletion { plan, addresses }),
        Err(error) => Err(delete_rejection(&error)),
    }
}

/// Resolve a compiled `$on_delete` policy to the planner's [`DeletePolicy`],
/// evaluating a patch object against the referencing row with `$target` bound to
/// the deleted target row. `None` skips an edge whose policy never applies (an
/// undecided ref, or a patch whose target row is not live).
fn resolve_policy(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    policy: &OnDelete,
    from: &RowRef,
    to: &RowRef,
) -> Result<Option<DeletePolicy>, Rejection> {
    Ok(match policy {
        OnDelete::Undecided => None,
        OnDelete::Restrict => Some(DeletePolicy::Restrict),
        OnDelete::Cascade => Some(DeletePolicy::Cascade),
        OnDelete::Clear => Some(DeletePolicy::Clear),
        OnDelete::Patch(patch) => {
            let (Some(from_cell), Some(target_cell)) =
                (row_cell_at(compiled, prospective, &from.collection, from), row_cell_at(compiled, prospective, &to.collection, to))
            else {
                return Ok(None);
            };
            let structurals = BTreeMap::from([("target".to_owned(), target_cell)]);
            let cell = ctx.eval_full(prospective, patch, &from_cell, BTreeMap::new(), structurals)?;
            Some(DeletePolicy::Patch(patch_assignments(&cell)))
        }
    })
}

/// The `(field, value)` assignments a patch object evaluated to (§21.1). A patch
/// that did not evaluate to a struct contributes nothing.
fn patch_assignments(cell: &liasse_expr::Cell) -> Vec<(String, Value)> {
    match cell {
        liasse_expr::Cell::Scalar(Value::Struct(fields)) => {
            fields.fields().map(|(name, value)| (name.as_str().to_owned(), value.clone())).collect()
        }
        _ => Vec::new(),
    }
}

/// The logical row cell of the live row `row`, if it exists.
fn row_cell_at(
    compiled: &Compiled,
    prospective: &Prospective,
    collection: &str,
    row: &RowRef,
) -> Option<liasse_expr::Cell> {
    let compiled = compiled.collection(collection)?;
    let address = address_of(prospective, compiled.key.as_slice(), row)?;
    let fields = prospective.get(&address)?;
    Some(row_cell(compiled, fields))
}

/// The live address of `row`, located by scanning its collection for the row
/// whose application-visible key identity (§5.4) equals `row.key`.
///
/// `row.key` is already an application identity — a bare scalar for a single-field
/// `$key`, or the name-sorted key struct for a composite one (the `from` edge
/// carries `materialize::key_identity`, the `to` edge the ref's typed key). So the
/// stored row's positional `$key`-order `components` must be normalized through the
/// SAME `refid::identity_of` the reference-resolution and restrict/cascade/clear
/// paths use before comparing; a positional/arity-limited match would resolve only
/// single-field keys and silently drop a composite-keyed `$on_delete` patch edge
/// (§21.1).
fn address_of(prospective: &Prospective, key_names: &[String], row: &RowRef) -> Option<RowAddress> {
    let path = CollectionPath::top(NameSegment::new(row.collection.clone()));
    prospective.addresses_in(&path).into_iter().find(|address| {
        address.steps().last().is_some_and(|step| {
            let components: Vec<Value> = step.key().components().cloned().collect();
            identity_of(key_names, &components) == row.key
        })
    })
}

/// The scalar target key a reference field value points at, if it is a live
/// single-key ref. Composite-key refs are a documented CORE seam here.
fn ref_key(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::Ref(reference)) => match reference.key() {
            RefKey::Scalar(key) => Some((**key).clone()),
            RefKey::Composite(_) => None,
        },
        _ => None,
    }
}

/// Map a planner failure to the admission rejection it is (§21.1).
fn delete_rejection(error: &DeleteError) -> Rejection {
    match error {
        DeleteError::Restricted { referencing, field, target } => Rejection::new(
            RejectionReason::Restricted,
            format!(
                "cannot delete {:?}: {:?} still references it via `{field}` (restrict)",
                target.key, referencing.key
            ),
        ),
        DeleteError::ConflictingPatch { row, field } => Rejection::new(
            RejectionReason::Restricted,
            format!("conflicting `$on_delete` patches on `{field}` of {:?}", row.key),
        ),
        other => Rejection::new(RejectionReason::Evaluation, other.to_string()),
    }
}
