//! Wiring the `$on_delete` cascade planner (§21.1) into mutation admission.
//!
//! A `collection - key` delete statement removes a row, but §21.1 makes that a
//! graph operation: every inbound reference's declared policy — `restrict`,
//! `cascade`, `none`, or a `= { … }` patch — decides the fate of the rows that
//! point at a deleted one. An inbound ref is a scalar `$ref` field or a member of
//! a `$set` of `$ref` (§5.5/§5.6); a set member's `cascade` drops the member from
//! its set rather than deleting the containing row. This module reads the
//! prospective state into the
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
        // §5.3/§21.1: a `$ref` is a legal static-struct member and carries an
        // `$on_delete` policy the model gate enforces, so walk every ref field of
        // the row's struct tree (recursively) — not just `collection.fields` — or a
        // struct-nested ref escapes deletion planning entirely. The site set is
        // data-independent, so compute it once per collection and reuse per row.
        let sites = crate::refwalk::ref_sites(collection);
        for address in prospective.addresses_in(&path) {
            let Some(fields) = prospective.get(&address) else { continue };
            let Some(step) = address.steps().last() else { continue };
            let from = RowRef::new(collection.name.clone(), materialize::key_identity(model, step.key()));
            for site in &sites {
                let field = site.field;
                let value = site.value(fields);
                if let Some(info) = &field.reference {
                    let Some(target_key) = ref_key(value) else { continue };
                    let to = RowRef::new(info.target.clone(), target_key);
                    // A struct-nested ref's ROW-LEVEL policy (`restrict` blocks,
                    // `cascade` deletes the containing row) applies without any nested
                    // field addressing; a surviving-row FIELD effect fails closed
                    // (`nested_scalar_policy`).
                    let policy = if site.container.is_empty() {
                        resolve_policy(compiled, ctx, prospective, &info.on_delete, &from, &to)?
                    } else {
                        Some(nested_scalar_policy(&info.on_delete))
                    };
                    let Some(policy) = policy else { continue };
                    graph.add_edge(RefEdge { from: from.clone(), field: site.display_name(), to, policy });
                }
                // §5.5/§5.6: every member of a `$set` of `$ref` is a governed
                // inbound ref (§21.1). Build one edge per live member so its
                // policy decides the member's fate when the target is deleted — a
                // set member's `cascade` drops the member (not the whole row).
                if let Some(info) = &field.element_reference
                    && let Some(Value::Set(members)) = value
                {
                    for member in members {
                        let Some(target_key) = ref_key(Some(member)) else { continue };
                        let to = RowRef::new(info.target.clone(), target_key);
                        let policy = if site.container.is_empty() {
                            resolve_member_policy(
                                compiled,
                                ctx,
                                prospective,
                                &info.on_delete,
                                &from,
                                &to,
                                member,
                            )?
                        } else {
                            Some(nested_member_policy(&info.on_delete))
                        };
                        let Some(policy) = policy else { continue };
                        graph.add_edge(RefEdge {
                            from: from.clone(),
                            field: site.display_name(),
                            to,
                            policy,
                        });
                    }
                }
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
/// the deleted target row. An undecided ref resolves to the fail-closed
/// [`DeletePolicy::Undecided`] backstop edge (§22.1): the planner rejects rather
/// than skips if such an edge's target is actually removed. `None` still skips an
/// edge whose policy cannot apply — a patch whose target row is not live.
fn resolve_policy(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    policy: &OnDelete,
    from: &RowRef,
    to: &RowRef,
) -> Result<Option<DeletePolicy>, Rejection> {
    Ok(match policy {
        OnDelete::Undecided => Some(DeletePolicy::Undecided),
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

/// Resolve a `$set`-of-`$ref` member's `$on_delete` policy (§5.6/§21.1). For a
/// set member the policy names differ from a scalar ref in the removing cases:
/// `cascade` deletes the containing **set member** (not the whole row), and
/// `none`/clear removes that membership — both are a [`DeletePolicy::DropMember`]
/// on the surviving referencing row. `restrict`, `undecided`, and a `= patch`
/// (which patches the containing row) resolve exactly as for a scalar ref.
fn resolve_member_policy(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    policy: &OnDelete,
    from: &RowRef,
    to: &RowRef,
    member: &Value,
) -> Result<Option<DeletePolicy>, Rejection> {
    match policy {
        OnDelete::Cascade | OnDelete::Clear => Ok(Some(DeletePolicy::DropMember(member.clone()))),
        _ => resolve_policy(compiled, ctx, prospective, policy, from, to),
    }
}

/// The delete policy of a STRUCT-NESTED scalar `$ref` (§5.3/§21.1). The ROW-LEVEL
/// outcomes — `restrict` (block the delete) and `cascade` (delete the containing
/// row) — apply correctly regardless of nesting, so they map straight through:
/// this is what gives a struct-nested `restrict`/`cascade` its §21.1 effect. A
/// surviving-row FIELD effect (`none`/clear or `= patch`) would need the nested
/// field addressing the deletion plan does not carry (a documented seam, matching
/// the model's own struct seams), so it maps to the fail-closed [`DeletePolicy::Undecided`]
/// backstop: a target deletion that would strand such a ref is REJECTED (§22.1)
/// rather than mis-applied at the wrong (top-level) field or left dangling. No
/// patch is evaluated here, so a nested `= patch` never runs against the wrong scope.
fn nested_scalar_policy(policy: &OnDelete) -> DeletePolicy {
    match policy {
        OnDelete::Restrict => DeletePolicy::Restrict,
        OnDelete::Cascade => DeletePolicy::Cascade,
        OnDelete::Undecided | OnDelete::Clear | OnDelete::Patch(_) => DeletePolicy::Undecided,
    }
}

/// The delete policy of a STRUCT-NESTED `$set`-of-`$ref` member (§5.3/§5.6/§21.1).
/// Only `restrict` is a row-level block that applies without nested addressing;
/// every removing/patching member effect (a member `cascade`/`none` drop, a
/// `= patch`) needs the nested set addressing the plan does not carry, so it fails
/// closed to [`DeletePolicy::Undecided`] (reject rather than mis-apply) — a
/// documented seam.
fn nested_member_policy(policy: &OnDelete) -> DeletePolicy {
    match policy {
        OnDelete::Restrict => DeletePolicy::Restrict,
        _ => DeletePolicy::Undecided,
    }
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
/// `$key`, or the positional [`Value::Composite`] tuple in `$key` order for a
/// composite one (the `from` edge
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

/// The target-key application identity a reference field value points at (§21.1),
/// if it is a live ref: a scalar-keyed ref exposes its bare key, a composite-keyed
/// ref its positional [`Value::Composite`] tuple — the same identity the target
/// row's `materialize::key_identity` produces, so the `$on_delete` edge finds its
/// target node in the graph by value equality.
fn ref_key(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::Ref(reference)) => match reference.key() {
            RefKey::Scalar(key) => Some((**key).clone()),
            RefKey::Composite(components) => Some(Value::Composite(components.clone())),
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
        // §22.1 fail-closed backstop: an undecided inbound ref whose target is
        // being removed would dangle, so the transition is refused (§21.1).
        DeleteError::DanglingUndecided { referencing, field, target } => Rejection::new(
            RejectionReason::DanglingRef,
            format!(
                "cannot delete {:?}: {:?} references it via `{field}` with an undecided `$on_delete`",
                target.key, referencing.key
            ),
        ),
        other => Rejection::new(RejectionReason::Evaluation, other.to_string()),
    }
}
