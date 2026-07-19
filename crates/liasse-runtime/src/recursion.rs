//! §10.5 recursive surface coverage materialization.
//!
//! A scoped role MAY propagate one surface through a checked descendant relation
//! (§10.5): the same `$view` projection re-applies to the covered row and to every
//! included descendant, and the coverage output appears under `$field` as a nested
//! keyed view — a keyed tree in which every node's ancestors are all included.
//!
//! The covered row is already materialized with its self-referential nested
//! collections in full (§5.4/§5.8, the F1 landing): [`materialize_row_cell`] hands
//! back the whole `subcompanies` tree as nested [`Cell::Collection`]s bounded by the
//! stored data, not the (type-level-infinite) shape. So coverage is a walk over that
//! in-hand tree: project each node through `$view`, and at each level keep only the
//! candidates the hereditary `$where` allow-list admits and the hereditary `$except`
//! deny-list does not prune — a pruned or excluded node contributes no output slot
//! and none of its descendants are surfaced or reparented.
//!
//! [`materialize_row_cell`]: crate::eval::EvalCtx::materialize_row_cell

use std::collections::BTreeMap;

use liasse_expr::{Cell, Row, TypedExpr};
use liasse_store::KeyValue;
use liasse_value::{Json, Value};
use serde_json::{Map as JsonMap, Value as J};

use crate::error::EngineError;
use crate::eval::EvalCtx;
use crate::materialize::top_address;
use crate::state::Prospective;
use crate::view::ViewResult;

/// A compiled `$recursive` coverage block (§10.5): the nested `$field` the keyed
/// tree lives under, the `$bind` candidate name, and the optional hereditary
/// `$where` allow-list / `$except` deny-list predicates over the bound candidate.
pub(crate) struct CompiledRecursive {
    /// The covered row's keyed-collection field the nested keyed view nests under.
    pub(crate) field: String,
    /// The name each descendant candidate is bound to for the predicates (§10.5).
    pub(crate) bind: String,
    /// The `$where` allow-list predicate (default include when absent).
    pub(crate) where_pred: Option<TypedExpr>,
    /// The `$except` deny-list predicate (default no pruning when absent).
    pub(crate) except_pred: Option<TypedExpr>,
}

/// The receiver row a scoped-role addressed call mutates (§10.3/§10.5): the
/// collection declaration path and the key components (in `$key` order across
/// every level) that address it. The role-holding row is the empty descendant
/// path (`path` = the scope collection, `key` = the request scope); a covered
/// descendant extends both with each `$field` step the addressing walk descends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedReceiver {
    /// The collection declaration path of the addressed row.
    pub path: Vec<String>,
    /// The key components of the addressed row, in `$key` order across levels.
    pub key: Vec<Value>,
}

/// How a call address resolves against the scoped-role coverage machinery
/// (§10.3/§10.5) — the read-only disposition the surface admission gates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedResolution {
    /// The address is not a scoped-role surface; its receiver comes from the
    /// call's own arguments (an ordinary public or package-level-role call).
    Unscoped,
    /// A scoped-role surface, but the addressed scope row does not live or the
    /// covered descendant is not a strict, `$where`-included, non-`$except` step —
    /// denied uniformly, indistinguishable from a nonexistent address (§10.4).
    Denied,
    /// The resolved receiver of a scoped-role addressed call.
    Receiver(ScopedReceiver),
}

/// The scope binding of a scoped-role surface view (§10.3/§10.5): the declaration
/// path of the collection whose row `.` (the role-holding row) resolves to, and —
/// when the surface declares one — the recursive coverage that nests the same
/// projection through the checked descendant relation.
pub(crate) struct CompiledScope {
    /// The declaration path of the scope collection (`["companies"]`), whose row
    /// keyed by the request scope is the covered `.`.
    pub(crate) collection_path: Vec<String>,
    /// The recursive coverage, when the surface declares `$recursive` (§10.5).
    pub(crate) recursive: Option<CompiledRecursive>,
}

impl CompiledScope {
    /// Materialize the scoped-role surface view (§10.5) at `scope_key`: resolve the
    /// covered row, project it through `$view`, and — under `$recursive` — nest the
    /// same projection through the descendant relation as a keyed tree. The result
    /// is delivered as one JSON object (the singular covered view). `None` when the
    /// scope names no live row, so the read faults closed exactly like an absent
    /// view (§6.3).
    pub(crate) fn materialize(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        view_expr: &TypedExpr,
        scope_key: &[Value],
    ) -> Result<Option<ViewResult>, EngineError> {
        let Some(key) = key_of(scope_key) else { return Ok(None) };
        let Some(name) = self.collection_path.last() else { return Ok(None) };
        let address = top_address(name, key);
        let Some(covered) = ctx.materialize_row_cell(prospective, &self.collection_path, &address)
        else {
            return Ok(None);
        };
        let object = match &self.recursive {
            Some(recursive) => recursive.cover(ctx, prospective, view_expr, &covered)?,
            None => project(ctx, prospective, view_expr, &covered)?,
        };
        let json = Json::from_wire(&J::Object(object))
            .map_err(|error| EngineError::Internal(error.to_string()))?;
        Ok(Some(ViewResult::Scalar(Value::Json(json))))
    }

    /// Resolve the receiver a scoped-role addressed call mutates (§10.5): the
    /// role-holding row keyed by `scope_key` (the empty `descendant` path), or a
    /// covered descendant addressed by its key path down through `$field`. The walk
    /// re-evaluates the recursive relation at every step — a strict, `$where`-included,
    /// non-`$except` descendant — reusing the same admit logic the coverage view
    /// materializes with ([`CompiledRecursive::included`]), so a step that is absent,
    /// pruned, or excluded yields `None` (denied uniformly, §10.4). `None` too when
    /// the scope names no live row, or a descendant is addressed on a surface that
    /// declares no `$recursive` coverage — only the role-holding row is then
    /// addressable.
    pub(crate) fn resolve_receiver(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        scope_key: &[Value],
        descendant: &[Value],
    ) -> Result<Option<ScopedReceiver>, EngineError> {
        let Some(key) = key_of(scope_key) else { return Ok(None) };
        let Some(name) = self.collection_path.last() else { return Ok(None) };
        let address = top_address(name, key);
        let Some(mut current) = ctx.materialize_row_cell(prospective, &self.collection_path, &address)
        else {
            return Ok(None);
        };
        let mut path = self.collection_path.clone();
        let mut receiver_key = scope_key.to_vec();
        if descendant.is_empty() {
            return Ok(Some(ScopedReceiver { path, key: receiver_key }));
        }
        // §10.5: a covered descendant is reachable only through the declared
        // recursive relation; a non-recursive scoped surface addresses only its
        // role-holding row.
        let Some(recursive) = &self.recursive else { return Ok(None) };
        for component in descendant {
            let Some(child) = recursive.included_child(ctx, prospective, &current, component)? else {
                return Ok(None);
            };
            path.push(recursive.field.clone());
            receiver_key.push(component.clone());
            current = Cell::Row(Box::new(child));
        }
        Ok(Some(ScopedReceiver { path, key: receiver_key }))
    }
}

impl CompiledRecursive {
    /// The covered node projected through `$view`, with its `$field` set to the
    /// nested keyed view of its INCLUDED descendants (§10.5) — recursively, so the
    /// keyed tree carries every node whose ancestors are all included. A candidate
    /// the `$where` allow-list excludes, or the `$except` deny-list prunes, drops
    /// with its whole subtree: both predicates are hereditary.
    fn cover(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        view_expr: &TypedExpr,
        node: &Cell,
    ) -> Result<JsonMap<String, J>, EngineError> {
        let mut object = project(ctx, prospective, view_expr, node)?;
        let mut children = Vec::new();
        if let Some(Cell::Collection(rows)) = node.as_row().and_then(|row| row.cell(&self.field)) {
            for candidate in rows {
                if self.included(ctx, prospective, candidate)? {
                    let child = Cell::Row(Box::new(candidate.clone()));
                    children.push(J::Object(self.cover(ctx, prospective, view_expr, &child)?));
                }
            }
        }
        object.insert(self.field.clone(), J::Array(children));
        Ok(object)
    }

    /// Whether `candidate` is an INCLUDED descendant (§10.5): it satisfies the
    /// `$where` allow-list (default include) and does not satisfy the `$except`
    /// deny-list (default none), which overrides `$where`. Recursion descends only
    /// into included candidates.
    fn included(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        candidate: &Row,
    ) -> Result<bool, EngineError> {
        let current = Cell::Row(Box::new(candidate.clone()));
        if let Some(pred) = &self.where_pred
            && !self.predicate(ctx, prospective, pred, &current)?
        {
            return Ok(false);
        }
        if let Some(pred) = &self.except_pred
            && self.predicate(ctx, prospective, pred, &current)?
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// The INCLUDED descendant of `node` keyed by `component` under `$field` (§10.5),
    /// or `None` when no child carries that key, or the child is not included (fails
    /// the `$where` allow-list, or matched by the `$except` deny-list). The §10.5
    /// addressing walk admits exactly the set the coverage view surfaces — both go
    /// through [`Self::included`] — so a descendant is addressable as a mutation
    /// receiver iff it appears in the covered keyed tree.
    fn included_child(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        node: &Cell,
        component: &Value,
    ) -> Result<Option<Row>, EngineError> {
        let Some(Cell::Collection(rows)) = node.as_row().and_then(|row| row.cell(&self.field)) else {
            return Ok(None);
        };
        for candidate in rows {
            if candidate.key() == component {
                return Ok(self
                    .included(ctx, prospective, candidate)?
                    .then(|| candidate.clone()));
            }
        }
        Ok(None)
    }

    /// Evaluate a `$where`/`$except` predicate with the candidate bound to `$bind`
    /// (§10.5). A non-`bool`/`none` result reads as `false` — the predicate is
    /// type-checked `bool` at load, so this is the fail-closed guard, not a path a
    /// well-formed package reaches.
    fn predicate(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        pred: &TypedExpr,
        candidate: &Cell,
    ) -> Result<bool, EngineError> {
        let bindings = BTreeMap::from([(self.bind.clone(), candidate.clone())]);
        let cell = ctx
            .eval_with(prospective, pred, candidate, bindings)
            .map_err(|rejection| EngineError::Internal(rejection.message().to_owned()))?;
        Ok(matches!(cell.as_scalar(), Some(Value::Bool(true))))
    }
}

/// Project a covered/descendant `node` through the surface `$view` (§10.5) into a
/// JSON object of its output fields. The same projection applies at every level.
fn project(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    view_expr: &TypedExpr,
    node: &Cell,
) -> Result<JsonMap<String, J>, EngineError> {
    let cell = ctx
        .eval(prospective, view_expr, node)
        .map_err(|rejection| EngineError::Internal(rejection.message().to_owned()))?;
    let row = cell
        .as_row()
        .ok_or_else(|| EngineError::Internal("a `$recursive` `$view` must project a row (§10.5)".to_owned()))?;
    Ok(row_object(row))
}

/// The scalar output fields of a projected row as a JSON object (Annex A wire
/// table). A `none` optional field is an omitted member; a nested cell (a row or
/// collection) is not part of the surface `$view`'s scalar projection here — the
/// nested keyed view is added by the recursion under `$field`, not the projection.
fn row_object(row: &Row) -> JsonMap<String, J> {
    row.cells()
        .filter_map(|(name, cell)| match cell {
            Cell::Scalar(Value::None) => None,
            Cell::Scalar(value) => Some((name.clone(), value.to_wire())),
            _ => None,
        })
        .collect()
}

/// The store [`KeyValue`] a scope key path denotes (§5.4): a single component is a
/// single-field key, several are a composite in `$key` order. `None` for an empty
/// scope — a scoped surface addressed without its containing row identity.
fn key_of(scope: &[Value]) -> Option<KeyValue> {
    let (first, rest) = scope.split_first()?;
    Some(KeyValue::composite(first.clone(), rest.iter().cloned()))
}
