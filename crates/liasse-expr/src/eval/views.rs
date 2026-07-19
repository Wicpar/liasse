//! Evaluation of views: row streams with binding context, projection (plain and
//! grouped), sorting and bounds, aggregates, and set combinators
//! (§7.1–§7.5, Annex B.2/B.5).

use std::collections::{BTreeMap, BTreeSet};

use liasse_value::Value;

use crate::env::{Cell, Row, RowId};
use crate::error::EvalError;
use crate::eval::{Evaluator, RowScope};
use crate::order::SortOrder;
use crate::ty::ExprType;
use crate::typed::{CombineOp, Projection, SortKey, TypedExpr, TypedKind, TypedSelector};

impl Evaluator<'_> {
    /// A view expression as an ordered stream of rows with their binding context.
    pub(crate) fn eval_view(&mut self, expr: &TypedExpr) -> Result<Vec<RowScope>, EvalError> {
        match expr.kind() {
            TypedKind::Traverse { base, member } => self.eval_traverse(base, member),
            TypedKind::Select { base, selector: TypedSelector::Bind { name, condition } } => {
                // §6.4: evaluate the base as a view so the bindings it introduced
                // (an outer `[:name]` or a `::`/`.` traversal level) stay visible
                // while this selector filters and binds each row.
                let base_scopes = self.eval_view(base)?;
                let kept = self.select_bind_scopes(base_scopes, name, condition)?;
                Ok(kept)
            }
            TypedKind::Select { base, selector: selector @ TypedSelector::Keys(_) } => {
                // §6.3: a key selector denotes a row view — zero or one row for a
                // scalar/composite key, one row per existing set/ref occurrence.
                // As a view it never coerces to a single row, so an absent key is
                // an empty stream rather than a cardinality error.
                let rows = self.select_rows(base, selector)?;
                Ok(rows.into_iter().map(RowScope::bare).collect())
            }
            TypedKind::Project { source, projection } => {
                // §7.1 in view context: project over the source's row view. A
                // scalar/composite-key source that statically types as a single
                // `Row` is still projected over its 0/1-row view here, so an absent
                // key yields no rows rather than the one-row cardinality rejection
                // `eval_select` raises. This is the shape a `$view` declaration
                // (§12.2) delivers — a collection, never a coerced single row.
                let rows = self.project_view(source, projection)?;
                Ok(rows.into_iter().map(RowScope::bare).collect())
            }
            _ => match self.eval(expr)? {
                Cell::Collection(rows) => Ok(rows.into_iter().map(RowScope::bare).collect()),
                Cell::Row(row) => Ok(vec![RowScope::bare(*row)]),
                _ => Err(EvalError::ShapeMismatch { expected: "a view" }),
            },
        }
    }

    /// Collect a view expression into a plain [`Cell::Collection`].
    pub(crate) fn collect_view(&mut self, expr: &TypedExpr) -> Result<Cell, EvalError> {
        let scopes = self.eval_view(expr)?;
        Ok(Cell::Collection(scopes.into_iter().map(|s| s.row).collect()))
    }

    pub(super) fn select_bind_scopes(
        &mut self,
        base_scopes: Vec<RowScope>,
        name: &str,
        condition: &Option<Box<TypedExpr>>,
    ) -> Result<Vec<RowScope>, EvalError> {
        // §6.4: inside `[:name | condition]`, `.` stays the enclosing receiver and
        // `name` binds the row under test — a meter source `/pools[:p | p.owner == .]`
        // compares each pool against the enforcing row `.` (§15.3). Preserve the
        // outer `.` for the filter rather than rebinding it to the row.
        let outer = self.current_at(0).unwrap_or_else(|_| Cell::Row(Box::new(Row::keyless(RowId::leaf(0), []))));
        let mut kept = Vec::new();
        for scope in base_scopes {
            let row = scope.row;
            if let Some(cond) = condition {
                self.push(outer.clone());
                // The filter sees every binding the base contributed, then the
                // new `[:name]` binding for the row under test (§6.4).
                for (bound, cell) in &scope.binds {
                    self.bind(bound.clone(), cell.clone());
                }
                self.bind(name.to_owned(), Cell::Row(Box::new(row.clone())));
                let verdict = self.eval(cond);
                self.pop();
                if !matches!(verdict?, Cell::Scalar(Value::Bool(true))) {
                    continue;
                }
            }
            // A `[:name]` filter keeps the row unchanged, so its source-chain
            // identity (any outer `::` prefix included) passes through untouched.
            let identity = scope.identity;
            let mut binds = scope.binds;
            binds.push((name.to_owned(), Cell::Row(Box::new(row.clone()))));
            kept.push(RowScope { row, binds, identity });
        }
        Ok(kept)
    }

    /// `base::member` (§6.4): flatten `member` across the rows of `base`,
    /// binding each traversed collection to its own field name.
    fn eval_traverse(&mut self, base: &TypedExpr, member: &str) -> Result<Vec<RowScope>, EvalError> {
        let outer = self.eval_view(base)?;
        let base_bind = bind_name_of(base);
        let mut out = Vec::new();
        for scope in outer {
            let inner = match scope.row.cell(member) {
                Some(Cell::Collection(rows)) => rows.clone(),
                Some(_) => return Err(EvalError::ShapeMismatch { expected: "a nested collection" }),
                None => continue,
            };
            for row in inner {
                let mut binds = scope.binds.clone();
                if let Some(name) = &base_bind {
                    binds.push((name.clone(), Cell::Row(Box::new(scope.row.clone()))));
                }
                binds.push((member.to_owned(), Cell::Row(Box::new(row.clone()))));
                // §7.2/§13.9: a `::` level inherits `outer.$key + inner.$key`, so
                // the outer (e.g. module-instance) component prefixes the exposed
                // row's identity — keeping same-keyed rows of distinct outer rows
                // apart. `scope.identity` already carries any further-out prefix.
                let identity = scope.identity.join(row.id());
                out.push(RowScope { row, binds, identity });
            }
        }
        Ok(out)
    }

    pub(crate) fn eval_project(
        &mut self,
        expr: &TypedExpr,
        source: &TypedExpr,
        projection: &Projection,
    ) -> Result<Cell, EvalError> {
        if matches!(expr.ty(), ExprType::Row(_)) {
            let row = match self.eval(source)? {
                Cell::Row(row) => *row,
                _ => return Err(EvalError::ShapeMismatch { expected: "a row" }),
            };
            let projected = self.project_row(&RowScope::bare(row.clone()), projection, None)?;
            return Ok(Cell::Row(Box::new(projected)));
        }
        Ok(Cell::Collection(self.project_view(source, projection)?))
    }

    /// Project over the source evaluated as a row view (§7.1), producing the
    /// output rows without coercing to a single row. Shared by the view-typed
    /// [`eval_project`] path and the view-context [`eval_view`] projection arm, so
    /// a scalar/composite-key source projects to its 0/1-row view rather than
    /// rejecting an absent key.
    fn project_view(
        &mut self,
        source: &TypedExpr,
        projection: &Projection,
    ) -> Result<Vec<Row>, EvalError> {
        let scopes = self.eval_view(source)?;
        self.project_scopes(scopes, projection)
    }

    /// Project a stream of already-evaluated source scopes (§7.1), dispatching to
    /// the plain or grouped path by whether a `$key` is declared. Shared by
    /// [`Self::project_view`] and the temporal rebase, which projects a bucketed
    /// base's recovered extant rather than re-reading the live collection.
    pub(super) fn project_scopes(
        &mut self,
        scopes: Vec<RowScope>,
        projection: &Projection,
    ) -> Result<Vec<Row>, EvalError> {
        if projection.key.is_empty() {
            self.project_plain(scopes, projection)
        } else {
            self.project_grouped(scopes, projection)
        }
    }

    fn project_plain(
        &mut self,
        scopes: Vec<RowScope>,
        projection: &Projection,
    ) -> Result<Vec<Row>, EvalError> {
        let mut ranked = Vec::with_capacity(scopes.len());
        for scope in scopes {
            let row = self.project_row(&scope, projection, None)?;
            // §7.3: sort keys see the projected outputs and the source row.
            let keys = if projection.sort.is_empty() {
                Vec::new()
            } else {
                self.eval_keys(&scope, &row, &projection.sort)?
            };
            ranked.push((row, keys));
        }
        Ok(bound(self.order(ranked, &projection.sort), projection))
    }

    fn project_grouped(
        &mut self,
        scopes: Vec<RowScope>,
        projection: &Projection,
    ) -> Result<Vec<Row>, EvalError> {
        // A BTreeMap keys the groups in synthetic-key ascending order (B.5).
        let mut groups: BTreeMap<Vec<Value>, Vec<RowScope>> = BTreeMap::new();
        for scope in scopes {
            let key = self.group_key(&scope, projection)?;
            groups.entry(key).or_default().push(scope);
        }
        let mut ranked = Vec::with_capacity(groups.len());
        for (key, members) in groups {
            let Some(first) = members.first() else { continue };
            let group_cell = Cell::Collection(members.iter().map(|s| s.row.clone()).collect());
            let identity = synthetic_key_value(&key);
            let row = self.project_row(first, projection, Some((group_cell, identity)))?;
            let keys = if projection.sort.is_empty() {
                Vec::new()
            } else {
                self.eval_keys(first, &row, &projection.sort)?
            };
            ranked.push((row, keys));
        }
        Ok(bound(self.order(ranked, &projection.sort), projection))
    }

    /// Order projected rows: preserve the source stream order when no `$sort` is
    /// declared (§6.3 selection order / §7.3 default), otherwise sort by the
    /// declared keys with occurrence identity as the final tiebreak (B.5).
    fn order(&self, ranked: Vec<(Row, Vec<Value>)>, sort: &[SortKey]) -> Vec<Row> {
        if sort.is_empty() {
            return ranked.into_iter().map(|(row, _)| row).collect();
        }
        order_rows(ranked, sort)
    }

    /// Evaluate a projection's outputs over one source row, producing the output
    /// row. `group` supplies the grouped-view context: the `group` binding, the
    /// synthetic key value, and an id seed.
    fn project_row(
        &mut self,
        scope: &RowScope,
        projection: &Projection,
        group: Option<(Cell, Value)>,
    ) -> Result<Row, EvalError> {
        self.push(Cell::Row(Box::new(scope.row.clone())));
        // §7.1/§6.4 (pinned): the in-scope row/loop bindings are never shadowed by a
        // same-named sibling output member, so track their names and never let an
        // output value overwrite one — mirrors the checker's resolution exactly.
        let mut loop_binds: BTreeSet<String> = BTreeSet::new();
        for (name, cell) in &scope.binds {
            loop_binds.insert(name.clone());
            self.bind(name.clone(), cell.clone());
        }
        if let Some((group_cell, _)) = &group {
            loop_binds.insert("group".to_owned());
            self.bind("group".to_owned(), group_cell.clone());
        }
        let mut cells: BTreeMap<String, Cell> = BTreeMap::new();
        for output in &projection.outputs {
            match self.eval(&output.expr) {
                Ok(cell) => {
                    if !loop_binds.contains(&output.name) {
                        self.bind(output.name.clone(), cell.clone());
                    }
                    cells.insert(output.name.clone(), cell);
                }
                Err(err) => {
                    self.pop();
                    return Err(err);
                }
            }
        }
        // §15.1: `$quantity` assigns the pool-capacity structural role. It is
        // evaluated in the same frame as the outputs and exposed as a structural
        // cell the runtime allocates against.
        if let Some(quantity) = &projection.quantity {
            match self.eval(quantity) {
                Ok(cell) => {
                    cells.insert("$quantity".to_owned(), cell);
                }
                Err(err) => {
                    self.pop();
                    return Err(err);
                }
            }
        }
        self.pop();
        let (id, key) = match group {
            // §7.2/§12.4: a synthetic group's identity is its group key, not its
            // position — rendered to canonical key text (D.2) so it is stable.
            Some((_, identity)) => (group_row_id(&identity), identity),
            // §7.2/§13.9: a plain projection inherits the source-chain identity —
            // the row's own key for a single collection, or the composed
            // `outer.$key + inner.$key` a `::` traversal contributed.
            None => (scope.identity.clone(), scope.row.key().clone()),
        };
        Ok(Row::new(id, key, cells))
    }

    /// Evaluate the sort keys for one row: `.` is the source row, and the
    /// projected outputs plus any `::` binds are in scope (§7.3).
    fn eval_keys(
        &mut self,
        scope: &RowScope,
        projected: &Row,
        sort: &[SortKey],
    ) -> Result<Vec<Value>, EvalError> {
        self.push(Cell::Row(Box::new(scope.row.clone())));
        // §7.1/§6.4 (pinned): a projected output never shadows a same-named row/loop
        // binding — a sort key referencing such a name reads the row binding, not the
        // like-named output, matching the projection-body resolution.
        let mut loop_binds: BTreeSet<String> = BTreeSet::new();
        for (name, cell) in &scope.binds {
            loop_binds.insert(name.clone());
            self.bind(name.clone(), cell.clone());
        }
        for (name, cell) in projected.cells() {
            if !loop_binds.contains(name) {
                self.bind(name.clone(), cell.clone());
            }
        }
        let mut keys = Vec::with_capacity(sort.len());
        for key in sort {
            match self.eval(&key.expr) {
                Ok(Cell::Scalar(value)) => keys.push(value),
                Ok(_) => {
                    self.pop();
                    return Err(EvalError::ShapeMismatch { expected: "a scalar sort key" });
                }
                Err(err) => {
                    self.pop();
                    return Err(err);
                }
            }
        }
        self.pop();
        Ok(keys)
    }

    fn group_key(&mut self, scope: &RowScope, projection: &Projection) -> Result<Vec<Value>, EvalError> {
        let mut key = Vec::with_capacity(projection.key.len());
        for name in &projection.key {
            let output = projection.outputs.iter().find(|o| &o.name == name);
            let Some(output) = output else { continue };
            self.push(Cell::Row(Box::new(scope.row.clone())));
            let value = self.eval(&output.expr);
            self.pop();
            match value? {
                Cell::Scalar(value) => key.push(value),
                _ => return Err(EvalError::ShapeMismatch { expected: "a scalar key value" }),
            }
        }
        Ok(key)
    }

    pub(crate) fn eval_combine(
        &mut self,
        op: CombineOp,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let left = self.eval_view(lhs)?;
        let right = self.eval_view(rhs)?;
        // §7.2/§7.4/B.5: a combinator identifies rows by their composed occurrence
        // identity — a `::` level's `outer.$key + inner.$key` (§13.9, `RowId::join`)
        // or a synthetic group's `$key` — not the bare inner key. Two rows sharing an
        // inner key under distinct parents are distinct identities and must not merge.
        // `RowScope::identity` carries that composed identity; for a plain collection
        // it is the row's own key-derived id, so plain-view behavior is unchanged.
        let right_ids: BTreeSet<&RowId> = right.iter().map(|scope| &scope.identity).collect();
        let result: Vec<Row> = match op {
            // §7.4: union keeps left order, then right identities not already present.
            CombineOp::Union => {
                let mut left_ids: BTreeSet<RowId> =
                    left.iter().map(|scope| scope.identity.clone()).collect();
                let mut rows: Vec<Row> = left.iter().map(|scope| scope.row.clone()).collect();
                for scope in right {
                    if left_ids.insert(scope.identity.clone()) {
                        rows.push(scope.row);
                    }
                }
                rows
            }
            CombineOp::Intersect => left
                .into_iter()
                .filter(|scope| right_ids.contains(&scope.identity))
                .map(|scope| scope.row)
                .collect(),
            CombineOp::Difference => left
                .into_iter()
                .filter(|scope| !right_ids.contains(&scope.identity))
                .map(|scope| scope.row)
                .collect(),
        };
        Ok(Cell::Collection(result))
    }
}

/// The single collection a view base ADDRESSES by name (§7.1): the field or
/// traversal member it ranges over, recovered through the row-narrowing and
/// row-reshaping operators that leave that source collection intact.
///
/// A bare `Field`/`Traverse` names the collection directly. A `Select` (a
/// `[:name | …]` filter or a `[key]` selection) and a `Project` still range over
/// the ONE collection their inner base names — the operator narrows or reshapes
/// rows but never changes which collection they come from — so the name is
/// recovered by recursing into that inner base. A multi-source operator
/// (`Combine`, `Fallback`, `Ternary`) ranges over more than one collection and
/// names none: the recursion reaches it through the `_` arm and yields `None`,
/// even wrapped in a filter (`(.a ∪ .b)[:x | …]`), since the multi-source node
/// answers `None` at its own level. The recursion is bounded by the syntax
/// nesting cap that bounds every structural walk in this crate, so it cannot
/// overflow.
///
/// Two callers read this:
/// - [`Evaluator::eval_temporal`] resolves a bucketed temporal read against the
///   collection the selector names (§14.1). A dormant filtered or projected base
///   has the empty active-at-clock identity set — non-distinguishing, shared by
///   every empty-active bucket — so recovering the addressed name is what keeps
///   the read from colliding with an earlier empty-active collection.
/// - [`Evaluator::eval_traverse`] binds a `::` base's traversed collection to its
///   field name (§6.4); recursing through `Select`/`Project` matches the checker's
///   `traverse_binds` scope model, which walks the same single-source spine.
///
/// [`Evaluator::eval_temporal`]: crate::eval::Evaluator::eval_temporal
/// [`Evaluator::eval_traverse`]: crate::eval::Evaluator::eval_traverse
pub(super) fn bind_name_of(expr: &TypedExpr) -> Option<String> {
    match expr.kind() {
        TypedKind::Field { name, .. } => Some(name.clone()),
        TypedKind::Traverse { member, .. } => Some(member.clone()),
        TypedKind::Select { base, .. } => bind_name_of(base),
        TypedKind::Project { source, .. } => bind_name_of(source),
        _ => None,
    }
}

/// Order rows through the view's [`SortOrder`] (§7.3, Annex B.5): successive sort
/// keys with each descending key reversed, occurrence identity as the final
/// tiebreak, and optional `none` last ascending / first descending because
/// `Value::None` is the Annex B.2 order maximum. The comparator is the shared
/// [`SortOrder::compare`], so a bounded window's §12.2 gap partition orders rows
/// identically. Each ordered row keeps its complete sort tuple (§7.3) so a bounded
/// window can retain it as the §12.2 immutable gap coordinate.
fn order_rows(mut ranked: Vec<(Row, Vec<Value>)>, sort: &[SortKey]) -> Vec<Row> {
    let order = SortOrder::from_keys(sort);
    ranked.sort_by(|(a_row, a_keys), (b_row, b_keys)| {
        order.compare(a_keys, a_row.id(), b_keys, b_row.id())
    });
    ranked.into_iter().map(|(row, keys)| row.with_sort(keys)).collect()
}

/// Apply `$skip`/`$limit` bounds after sorting (§7.3).
fn bound(rows: Vec<Row>, projection: &Projection) -> Vec<Row> {
    let skip = projection.skip.unwrap_or(0) as usize;
    let mut rows: Vec<Row> = rows.into_iter().skip(skip).collect();
    if let Some(limit) = projection.limit {
        rows.truncate(limit as usize);
    }
    rows
}

/// The identity value of a synthetic key (§7.2): the scalar itself, or the
/// positional [`Value::Composite`] tuple of the components in `$key` order — the
/// same carrier a collection's composite key identity uses, so B.4 orders it
/// positionally.
fn synthetic_key_value(components: &[Value]) -> Value {
    match components {
        [single] => single.clone(),
        many => Value::Composite(many.to_vec()),
    }
}

/// The stable identity of a synthetic-`$key` group (§7.2, §12.4): its group key
/// rendered to canonical key text (Annex D.2). The scalar or composite group key
/// flattens through the one shared D.2 codec; a group key value that D.2 gives no
/// key text (a non-key-eligible type the checker still admits, SPEC-ISSUES item
/// 16) falls back to its canonical wire JSON so identity stays total and pure.
fn group_row_id(identity: &Value) -> RowId {
    let text = liasse_ident::KeyText::from_key_values(std::slice::from_ref(identity))
        .map(|key| key.as_str().to_owned())
        .unwrap_or_else(|_| identity.to_canonical_json_string());
    RowId::keyed(text)
}
