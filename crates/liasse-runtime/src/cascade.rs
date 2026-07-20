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
use liasse_value::{RefKey, Struct, Text, Value};

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
                    // `cascade` deletes the containing row) applies without nested field
                    // addressing; its SURVIVING-ROW effect (`none` clear / `= patch`) is
                    // applied at the nested field by `nested_scalar_policy`.
                    let policy = if site.container.is_empty() {
                        resolve_policy(compiled, ctx, prospective, &info.on_delete, &from, &to)?
                    } else {
                        nested_scalar_policy(compiled, ctx, prospective, &info.on_delete, &from, &to, site)?
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
                            nested_member_policy(
                                compiled, ctx, prospective, &info.on_delete, &from, &to, member, site,
                            )?
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
            let (Some(from_cell), Some(target_cell)) = (
                row_cell_at(ctx, compiled, prospective, &from.collection, from),
                row_cell_at(ctx, compiled, prospective, &to.collection, to),
            ) else {
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

/// Resolve a STRUCT-NESTED scalar `$ref`'s `$on_delete` policy (§5.3/§21.1),
/// applying its SURVIVING-ROW effect AT the nested field. The row-level outcomes —
/// `restrict` (block the delete), `cascade` (delete the containing row) — and a
/// whole-row-authored `= patch` (whose keys are top-level fields, §21.1 binds `.`
/// to the whole row) need no nested addressing and resolve through the shared
/// [`resolve_policy`]. `none` (clear this optional ref) is the one nested-addressed
/// effect: it is encoded as a top-level struct patch that clears the nested leaf
/// (`nested_clear_patch`), so the existing flat plan application writes it back. An
/// undecided ref stays the fail-closed backstop. This CLOSES the wave-2 seam that
/// rejected every struct-nested survivor effect (§21.1: no spec-valid policy may be
/// refused).
fn nested_scalar_policy(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    policy: &OnDelete,
    from: &RowRef,
    to: &RowRef,
    site: &crate::refwalk::RefSite<'_>,
) -> Result<Option<DeletePolicy>, Rejection> {
    match policy {
        // §21.1 "none — clear this optional ref": assign `none` to the nested
        // referencing field, carried as a top-level struct patch.
        OnDelete::Clear => Ok(nested_leaf_patch(compiled, prospective, from, site, &|_| Value::None)),
        _ => resolve_policy(compiled, ctx, prospective, policy, from, to),
    }
}

/// Resolve a STRUCT-NESTED `$set`-of-`$ref` member's `$on_delete` policy
/// (§5.3/§5.6/§21.1), applying its SURVIVING-ROW effect at the nested set. A
/// `cascade`/`none` member effect drops the deleted target from the nested set —
/// encoded as a top-level struct patch removing that member; `restrict`, an
/// undecided ref, and a whole-row `= patch` resolve as for a scalar ref through
/// [`resolve_policy`].
///
/// Combination granularity note (§21.1 "patches … combine when they touch disjoint
/// fields or assign the same resulting value"): because interp applies the flat
/// plan by TOP-LEVEL field name, a nested effect is carried as a whole-struct
/// assignment, so its combination unit is the top-level struct field — two nested
/// effects on the SAME struct in ONE transition would collide. That case needs
/// leaf-granular plan application (an interp/deletion seam outside this crate's
/// row-materialization scope); a single nested effect per struct — the reported and
/// overwhelmingly common case — applies correctly.
#[allow(clippy::too_many_arguments)]
fn nested_member_policy(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    policy: &OnDelete,
    from: &RowRef,
    to: &RowRef,
    member: &Value,
    site: &crate::refwalk::RefSite<'_>,
) -> Result<Option<DeletePolicy>, Rejection> {
    match policy {
        // §21.1 "cascade — delete the containing row or set member"; `none`/clear
        // likewise drops the membership. Remove the deleted target from the nested set.
        OnDelete::Cascade | OnDelete::Clear => {
            let member = member.clone();
            let drop = move |value: &Value| match value {
                Value::Set(members) => {
                    let mut kept = members.clone();
                    kept.remove(&member);
                    Value::Set(kept)
                }
                other => other.clone(),
            };
            Ok(nested_leaf_patch(compiled, prospective, from, site, &drop))
        }
        _ => resolve_policy(compiled, ctx, prospective, policy, from, to),
    }
}

/// Build the top-level struct patch a struct-nested survivor effect induces on the
/// referencing row `from` (§21.1): read the row's fields, apply `transform` to the
/// nested leaf `site` addresses, and return a single `(top_field, new_struct)`
/// assignment — the flat form interp's `apply_deletion` writes back. `None` when
/// the referencing row or its struct path is not materialized (a live ref always
/// materializes it, so the edge that produced this call already proved it present).
fn nested_leaf_patch(
    compiled: &Compiled,
    prospective: &Prospective,
    from: &RowRef,
    site: &crate::refwalk::RefSite<'_>,
    transform: &dyn Fn(&Value) -> Value,
) -> Option<DeletePolicy> {
    let compiled = compiled.collection(&from.collection)?;
    let address = address_of(prospective, compiled.key.as_slice(), from)?;
    let fields = prospective.get(&address)?;
    let (top, rest) = site.container.split_first()?;
    let new_top = rebuild_struct(fields.get(*top)?, rest, &site.field.name, transform)?;
    Some(DeletePolicy::Patch(vec![((*top).to_owned(), new_top)]))
}

/// Descend the static-struct name `path` into `value` and apply `transform` to
/// member `leaf`, rebuilding each struct level (§5.3). `None` when a level is not a
/// struct — the ref cannot live there, so there is nothing to change.
fn rebuild_struct(
    value: &Value,
    path: &[&str],
    leaf: &str,
    transform: &dyn Fn(&Value) -> Value,
) -> Option<Value> {
    let Value::Struct(existing) = value else { return None };
    let mut members: Vec<(Text, Value)> = Vec::new();
    for (name, member) in existing.fields() {
        let rebuilt = match path.split_first() {
            Some((head, tail)) if name.as_str() == *head => rebuild_struct(member, tail, leaf, transform)?,
            None if name.as_str() == leaf => transform(member),
            _ => member.clone(),
        };
        members.push((name.clone(), rebuilt));
    }
    Some(Value::Struct(Struct::new(members)))
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

/// The COMPLETE logical row cell of the live row `row`, if it exists (§21.1/§5.2):
/// its computed values folded (in dependency order) and its nested keyed
/// collections descended, so an `$on_delete` patch binding this cell to `.` or
/// `$target` reads a computed member (`.tag`, `$target.badge`) like any stored
/// field. `materialize_row_cell` is the ONE canonical complete builder; the bare
/// `row_cell` is the fallback for a row that no longer materializes.
fn row_cell_at(
    ctx: &EvalCtx<'_>,
    compiled: &Compiled,
    prospective: &Prospective,
    collection: &str,
    row: &RowRef,
) -> Option<liasse_expr::Cell> {
    let comp = compiled.collection(collection)?;
    let address = address_of(prospective, comp.key.as_slice(), row)?;
    let decl = [collection.to_owned()];
    ctx.materialize_row_cell(prospective, &decl, &address)
        .or_else(|| prospective.get(&address).map(|fields| row_cell(comp, fields)))
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
