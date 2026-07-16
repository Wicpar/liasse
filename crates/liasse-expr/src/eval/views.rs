//! Evaluation of views: row streams with binding context, projection (plain and
//! grouped), sorting and bounds, aggregates, and set combinators
//! (§7.1–§7.5, Annex B.2/B.5).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use liasse_value::Value;

use crate::env::{Cell, Row, RowId};
use crate::error::EvalError;
use crate::eval::{Evaluator, RowScope};
use crate::ty::ExprType;
use crate::typed::{CombineOp, Projection, SortKey, TypedExpr, TypedKind, TypedSelector};

impl Evaluator<'_> {
    /// A view expression as an ordered stream of rows with their binding context.
    pub(crate) fn eval_view(&mut self, expr: &TypedExpr) -> Result<Vec<RowScope>, EvalError> {
        match expr.kind() {
            TypedKind::Traverse { base, member } => self.eval_traverse(base, member),
            TypedKind::Select { base, selector: TypedSelector::Bind { name, condition } } => {
                let rows = self.eval(base)?;
                let rows = match rows {
                    Cell::Collection(rows) => rows,
                    _ => return Err(EvalError::ShapeMismatch { expected: "a collection" }),
                };
                let kept = self.select_bind_scopes(rows, name, condition)?;
                Ok(kept)
            }
            _ => match self.eval(expr)? {
                Cell::Collection(rows) => Ok(rows.into_iter().map(bare_scope).collect()),
                Cell::Row(row) => Ok(vec![bare_scope(*row)]),
                _ => Err(EvalError::ShapeMismatch { expected: "a view" }),
            },
        }
    }

    /// Collect a view expression into a plain [`Cell::Collection`].
    pub(crate) fn collect_view(&mut self, expr: &TypedExpr) -> Result<Cell, EvalError> {
        let scopes = self.eval_view(expr)?;
        Ok(Cell::Collection(scopes.into_iter().map(|s| s.row).collect()))
    }

    fn select_bind_scopes(
        &mut self,
        rows: Vec<Row>,
        name: &str,
        condition: &Option<Box<TypedExpr>>,
    ) -> Result<Vec<RowScope>, EvalError> {
        let mut kept = Vec::new();
        for row in rows {
            if let Some(cond) = condition {
                self.push(Cell::Row(Box::new(row.clone())));
                self.bind(name.to_owned(), Cell::Row(Box::new(row.clone())));
                let verdict = self.eval(cond);
                self.pop();
                if !matches!(verdict?, Cell::Scalar(Value::Bool(true))) {
                    continue;
                }
            }
            kept.push(RowScope { row: row.clone(), binds: vec![(name.to_owned(), Cell::Row(Box::new(row)))] });
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
                out.push(RowScope { row, binds });
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
            let projected = self.project_row(&RowScope { row: row.clone(), binds: Vec::new() }, projection, None)?;
            return Ok(Cell::Row(Box::new(projected)));
        }
        let scopes = self.eval_view(source)?;
        let rows = if projection.key.is_empty() {
            self.project_plain(scopes, projection)?
        } else {
            self.project_grouped(scopes, projection)?
        };
        Ok(Cell::Collection(rows))
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
        for (name, cell) in &scope.binds {
            self.bind(name.clone(), cell.clone());
        }
        if let Some((group_cell, _)) = &group {
            self.bind("group".to_owned(), group_cell.clone());
        }
        let mut cells: BTreeMap<String, Cell> = BTreeMap::new();
        for output in &projection.outputs {
            match self.eval(&output.expr) {
                Ok(cell) => {
                    self.bind(output.name.clone(), cell.clone());
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
            None => (scope.row.id().clone(), scope.row.key().clone()),
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
        for (name, cell) in &scope.binds {
            self.bind(name.clone(), cell.clone());
        }
        for (name, cell) in projected.cells() {
            self.bind(name.clone(), cell.clone());
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
        let left: Vec<Row> = self.eval_view(lhs)?.into_iter().map(|s| s.row).collect();
        let right: Vec<Row> = self.eval_view(rhs)?.into_iter().map(|s| s.row).collect();
        let right_keys: BTreeSet<&Value> = right.iter().map(Row::key).collect();
        let result = match op {
            // §7.4: union keeps left order, then right identities not already present.
            CombineOp::Union => {
                let mut left_keys: BTreeSet<Value> = left.iter().map(|r| r.key().clone()).collect();
                let mut rows = left.clone();
                for row in right {
                    if left_keys.insert(row.key().clone()) {
                        rows.push(row);
                    }
                }
                rows
            }
            CombineOp::Intersect => left
                .into_iter()
                .filter(|row| right_keys.contains(row.key()))
                .collect(),
            CombineOp::Difference => left
                .into_iter()
                .filter(|row| !right_keys.contains(row.key()))
                .collect(),
        };
        Ok(Cell::Collection(result))
    }
}

fn bare_scope(row: Row) -> RowScope {
    RowScope { row, binds: Vec::new() }
}

/// The binding name a `::` base contributes: a field's name, or a nested
/// traversal's innermost member.
fn bind_name_of(expr: &TypedExpr) -> Option<String> {
    match expr.kind() {
        TypedKind::Field { name, .. } => Some(name.clone()),
        TypedKind::Traverse { member, .. } => Some(member.clone()),
        _ => None,
    }
}

/// Order rows by successive sort keys (descending flips each), with occurrence
/// identity as the final tiebreaker (Annex B.5). Optional `none` sorts last
/// ascending / first descending because `Value::None` is the order maximum
/// (Annex B.2).
fn order_rows(mut ranked: Vec<(Row, Vec<Value>)>, sort: &[SortKey]) -> Vec<Row> {
    ranked.sort_by(|(a_row, a_keys), (b_row, b_keys)| {
        for (index, key) in sort.iter().enumerate() {
            let a = a_keys.get(index);
            let b = b_keys.get(index);
            let ordering = match (a, b) {
                (Some(a), Some(b)) => a.cmp(b),
                _ => Ordering::Equal,
            };
            let ordering = if key.descending { ordering.reverse() } else { ordering };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        a_row.id().cmp(b_row.id())
    });
    ranked.into_iter().map(|(row, _)| row).collect()
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

/// The identity value of a synthetic key: the scalar itself, or a struct of the
/// components in `$key` order (§7.2).
fn synthetic_key_value(components: &[Value]) -> Value {
    match components {
        [single] => single.clone(),
        many => {
            let fields = many
                .iter()
                .enumerate()
                .map(|(index, value)| (liasse_value::Text::new(index.to_string()), value.clone()));
            Value::Struct(liasse_value::Struct::new(fields))
        }
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
