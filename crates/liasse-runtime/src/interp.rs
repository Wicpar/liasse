//! The mutation-program interpreter (§8): it executes statements in order
//! against the prospective state, applying insertion defaults and normalization
//! as it writes, and records the return expression for post-commit evaluation.
//!
//! CORE scope covers the operator forms the tasks app (§3.2) and the §5/§8 rule
//! cases exercise: field assignment, keyed single-row insert, keyed delete,
//! keyed single-row patch, optional-field clear, `assert`, and `return`. Local
//! bindings, view-sourced insert/replace, internal calls, and multi-row patch
//! sources are documented seams.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceId;
use liasse_expr::{check_expression, Cell, ExprType};
use liasse_ident::NameSegment;
use liasse_syntax::{Arg, BinaryOp, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, Stmt, StmtKind, UnaryOp};
use liasse_store::{CollectionPath, KeyValue, RowAddress};
use liasse_value::Value;

use crate::cascade::{self, PlannedDeletion};
use crate::compiled::{Compiled, CompiledCollection, CompiledMutation};
use crate::deletion::RowRef;
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize::{self, FieldMap};
use crate::rules;
use crate::scope::RuntimeScope;
use crate::state::Prospective;

/// A row-source location: the selected row's address and its collection name.
#[derive(Clone)]
pub(crate) struct RowTarget {
    pub(crate) address: RowAddress,
    pub(crate) collection: String,
}

/// The row(s) a patch base resolves to (§8.9): exactly one (a keyed selector or
/// the receiver) or a whole filtered set (a bound `[:x | pred]` selection).
enum PatchPlan {
    Single(RowTarget),
    Many(Vec<RowTarget>),
}

/// A lexical local a `name = …` statement bound (§8.1): either a row the
/// statement inserted or selected (tracked by address so a later read observes
/// the current committed fields, §8.10) or an evaluated scalar/value.
#[derive(Clone)]
pub(crate) enum LocalBind {
    Row(RowTarget),
    Value(Cell, ExprType),
}

/// The type and cell of every local binding, resolved against `prospective` so a
/// row binding reads its current fields (§8.1 read-your-writes; §8.10 committed
/// state for the response). Shared by the interpreter (mid-program) and the
/// engine's `return` evaluation (post-commit).
pub(crate) fn local_bindings(
    locals: &BTreeMap<String, LocalBind>,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> (BTreeMap<String, ExprType>, BTreeMap<String, Cell>) {
    let mut types = BTreeMap::new();
    let mut cells = BTreeMap::new();
    for (name, bind) in locals {
        match bind {
            LocalBind::Row(target) => {
                let Some(ty) = ctx.schema.receiver_row_type(std::slice::from_ref(&target.collection))
                else {
                    continue;
                };
                let (Some(collection), Some(fields)) =
                    (ctx.compiled.collection(&target.collection), prospective.get(&target.address))
                else {
                    continue;
                };
                types.insert(name.clone(), ty);
                cells.insert(name.clone(), ctx.row_cell_of(prospective, collection, fields));
            }
            LocalBind::Value(cell, ty) => {
                types.insert(name.clone(), ty.clone());
                cells.insert(name.clone(), cell.clone());
            }
        }
    }
    (types, cells)
}

/// The mutation-program interpreter over one admission.
pub(crate) struct Interp<'a> {
    pub(crate) compiled: &'a Compiled,
    pub(crate) ctx: &'a EvalCtx<'a>,
    pub(crate) prospective: &'a mut Prospective,
    pub(crate) mutation: &'a CompiledMutation,
    pub(crate) receiver: Option<RowTarget>,
    pub(crate) touched: Vec<RowAddress>,
    pub(crate) ret: Option<(Expr, SourceId)>,
    /// Lexical locals bound by `name = …` statements (§8.1), in declaration
    /// order, visible to later statements and to the `return`.
    pub(crate) locals: BTreeMap<String, LocalBind>,
}

impl<'a> Interp<'a> {
    /// Run every statement in order (§8.1).
    pub(crate) fn run(&mut self) -> Result<(), Rejection> {
        let count = self.mutation.program.len();
        for index in 0..count {
            let Some(compiled) = self.mutation.program.get(index) else { break };
            let source = compiled.source;
            let stmt = compiled.stmt.clone();
            self.exec(&stmt, source)?;
        }
        Ok(())
    }

    fn exec(&mut self, stmt: &Stmt, source: SourceId) -> Result<(), Rejection> {
        match &stmt.kind {
            StmtKind::Return(expr) => {
                self.ret = Some((expr.clone(), source));
                Ok(())
            }
            StmtKind::Assign { target, value } => self.exec_assign(target, value, source),
            StmtKind::Clear(target) => self.exec_clear(target, source),
            StmtKind::Bare(expr) => self.exec_bare(expr, source),
        }
    }

    /// The receiver `.` cell over the current prospective state.
    fn current(&self) -> Result<Cell, Rejection> {
        match &self.receiver {
            None => Ok(Cell::Row(Box::new(self.ctx.root(self.prospective)))),
            Some(receiver) => {
                let collection = self.collection(&receiver.collection)?;
                let fields = self.prospective.get(&receiver.address).ok_or_else(|| {
                    Rejection::new(RejectionReason::MissingTarget, "the selected row no longer exists")
                        .at(receiver.address.render())
                })?;
                Ok(self.ctx.row_cell_of(self.prospective, collection, fields))
            }
        }
    }

    fn collection(&self, name: &str) -> Result<&'a CompiledCollection, Rejection> {
        self.compiled
            .collection(name)
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, format!("unknown collection `{name}`")))
    }

    /// The address of the row `fields` occupies in collection `name`, by key.
    fn key_address(&self, name: &str, fields: &FieldMap) -> Result<RowAddress, Rejection> {
        let model = self
            .ctx
            .schema
            .top_collection(name)
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, format!("unknown collection `{name}`")))?;
        let key = materialize::row_key(model, fields)
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "the row is missing a key field"))?;
        Ok(materialize::top_address(name, key))
    }

    /// The mutation's scope extended with the current local bindings' types, so a
    /// statement (or a value inside one) may reference a `name = …` binding.
    fn scope(&self) -> RuntimeScope {
        let (types, _) = local_bindings(&self.locals, self.ctx, self.prospective);
        let mut scope = self.mutation.scope.clone();
        for (name, ty) in types {
            scope = scope.with_binding(name, ty);
        }
        scope
    }

    /// The current local bindings as evaluation cells.
    fn binding_cells(&self) -> BTreeMap<String, Cell> {
        local_bindings(&self.locals, self.ctx, self.prospective).1
    }

    fn eval_value(&self, expr: &Expr, source: SourceId, current: &Cell) -> Result<Cell, Rejection> {
        let typed = check_expression(&self.scope(), source, expr)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the request expression did not type-check"))?;
        self.ctx.eval_with(self.prospective, &typed, current, self.binding_cells())
    }

    fn scalar_value(&self, expr: &Expr, source: SourceId, current: &Cell) -> Result<Value, Rejection> {
        match self.eval_value(expr, source, current)? {
            Cell::Scalar(value) => Ok(value),
            _ => Err(Rejection::new(RejectionReason::TypeError, "expected a scalar value here")),
        }
    }

    // ---- assignment -------------------------------------------------------

    fn exec_assign(&mut self, target: &Expr, value: &Expr, source: SourceId) -> Result<(), Rejection> {
        // `name = <expr>` binds a lexical local (§8.1): the constructed/selected
        // row or evaluated value becomes visible to later statements and the
        // `return`, rather than staging a field write.
        if let ExprKind::Name(id) = &target.kind {
            return self.bind_local(id.text.clone(), value, source);
        }
        let Some((row, field)) = self.field_target(target, source)? else {
            // A collection replacement is a documented CORE seam: it stages nothing.
            return Ok(());
        };
        let current = self.current()?;
        let typed = check_expression(&self.scope(), source, value)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the assigned value did not type-check"))?;
        let scalar = match self.ctx.eval_with(self.prospective, &typed, &current, self.binding_cells())? {
            Cell::Scalar(value) => value,
            _ => return Err(Rejection::new(RejectionReason::TypeError, "a field is assigned a scalar value")),
        };
        let collection = self.collection(&row.collection)?;
        if let Some(field_meta) = collection.field(&field)
            && let Some(from) = typed.ty().as_scalar()
            && !crate::schema::assignable(from, &field_meta.ty)
        {
            return Err(Rejection::new(
                RejectionReason::TypeError,
                format!("value of type `{}` is not assignable to `{}`", from.name(), field_meta.ty.name()),
            )
            .at(format!("{}/{}", row.address.render(), field)));
        }
        self.write_field(&row, &field, scalar)
    }

    /// Bind a lexical local `name` to `value` (§8.1). An insert expression
    /// (`.coll + { … }`) performs the insert and binds the constructed row, so
    /// `name = .coll + { … }` then `return name { … }` returns the committed row
    /// (§8.4, §8.10); any other right-hand side binds its evaluated value.
    fn bind_local(&mut self, name: String, value: &Expr, source: SourceId) -> Result<(), Rejection> {
        if let ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } = &value.kind
            && self.collection_ref(lhs).is_some()
        {
            let target = self.insert_row(lhs, rhs, source)?;
            self.locals.insert(name, LocalBind::Row(target));
            return Ok(());
        }
        // `name = .coll - keys` / `name = -.coll[…]` (§8.4): the deleted rows, as
        // they existed immediately before removal, are captured in selector order
        // and bound as a collection value, so `return name { … }` projects them.
        if let ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } = &value.kind
            && let Some((_path, collection)) = self.collection_ref(lhs)
        {
            let keys = self.delete_key_values(rhs, source)?;
            return self.bind_deleted(name, collection, keys);
        }
        if let ExprKind::Unary { op: UnaryOp::Neg, operand } = &value.kind
            && let ExprKind::Select { base, .. } = &operand.kind
            && let Some((_path, collection)) = self.collection_ref(base)
        {
            let keys = self.selection_key_values(operand, source)?;
            return self.bind_deleted(name, collection, keys);
        }
        let current = self.current()?;
        let typed = check_expression(&self.scope(), source, value)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the bound value did not type-check"))?;
        let ty = typed.ty().clone();
        let cell = self.ctx.eval_with(self.prospective, &typed, &current, self.binding_cells())?;
        self.locals.insert(name, LocalBind::Value(cell, ty));
        Ok(())
    }

    /// The key values a `.coll - keys` delete targets, in order: a lone scalar,
    /// or every member of a set operand (§8.4 selector order).
    fn delete_key_values(&self, keys: &Expr, source: SourceId) -> Result<Vec<Value>, Rejection> {
        let current = self.current()?;
        Ok(match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().collect(),
            scalar => vec![scalar],
        })
    }

    /// The key values a `-.coll[…]` selection targets, in the order the selection
    /// yields them (§8.4): a filtered set in canonical order, a key list in the
    /// order written (with duplicates naturally collapsed by the capture).
    fn selection_key_values(&self, selection: &Expr, source: SourceId) -> Result<Vec<Value>, Rejection> {
        let current = self.current()?;
        Ok(match self.eval_value(selection, source, &current)? {
            Cell::Collection(rows) => rows.iter().map(|row| row.key().clone()).collect(),
            Cell::Row(row) => vec![row.key().clone()],
            Cell::Scalar(_) => Vec::new(),
        })
    }

    /// Capture the pre-delete rows for `keys` (deduplicated to first occurrence,
    /// §8.4), delete them through the §21.1 cascade planner, and bind the captured
    /// collection to `name` so a `return name { … }` projects the removed rows.
    fn bind_deleted(&mut self, name: String, collection: String, keys: Vec<Value>) -> Result<(), Rejection> {
        let mut seen = BTreeSet::new();
        let mut ordered = Vec::new();
        for key in keys {
            if seen.insert(key.clone()) {
                ordered.push(key);
            }
        }
        let compiled = self.collection(&collection)?;
        let rows: Vec<liasse_expr::Row> = ordered
            .iter()
            .filter_map(|key| {
                let address = materialize::top_address(&collection, KeyValue::single(key.clone()));
                let fields = self.prospective.get(&address)?;
                match self.ctx.row_cell_of(self.prospective, compiled, fields) {
                    Cell::Row(row) => Some(*row),
                    _ => None,
                }
            })
            .collect();
        let ty = self
            .ctx
            .schema
            .root_row_type()
            .field(&collection)
            .cloned()
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, format!("unknown collection `{collection}`")))?;
        let initial: Vec<RowRef> =
            ordered.into_iter().map(|key| RowRef::new(collection.clone(), key)).collect();
        self.delete_rows(initial)?;
        self.locals.insert(name, LocalBind::Value(Cell::Collection(rows), ty));
        Ok(())
    }

    /// Write `value` into `field` of the row at `row`, normalizing it, and
    /// rekeying the row if a key field changed (§5.4).
    fn write_field(&mut self, row: &RowTarget, field: &str, value: Value) -> Result<(), Rejection> {
        let mut fields = self.prospective.get(&row.address).cloned().ok_or_else(|| {
            Rejection::new(RejectionReason::MissingTarget, "the target row does not exist")
                .at(row.address.render())
        })?;
        fields.insert(field.to_owned(), value);
        let collection = self.collection(&row.collection)?;
        rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
        self.place(&row.address, &row.collection, fields)
    }

    /// Place `fields` for the row currently at `address`, moving it to a new
    /// address when its key changed (an atomic rekey, §5.4).
    fn place(&mut self, address: &RowAddress, collection: &str, fields: FieldMap) -> Result<(), Rejection> {
        let new_address = self.key_address(collection, &fields)?;
        if &new_address != address {
            if self.prospective.contains(&new_address) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "rekey target already exists")
                    .at(new_address.render()));
            }
            self.prospective.remove(address);
            self.prospective.insert(new_address.clone(), fields);
            if let Some(receiver) = &mut self.receiver
                && &receiver.address == address
            {
                receiver.address = new_address.clone();
            }
            self.mark(new_address);
        } else {
            self.prospective.replace(address, fields);
            self.mark(address.clone());
        }
        Ok(())
    }

    fn mark(&mut self, address: RowAddress) {
        if !self.touched.contains(&address) {
            self.touched.push(address);
        }
    }

    // ---- clear ------------------------------------------------------------

    fn exec_clear(&mut self, target: &Expr, source: SourceId) -> Result<(), Rejection> {
        let Some((row, field)) = self.field_target(target, source)? else {
            return Ok(());
        };
        self.write_field(&row, &field, Value::None)
    }

    // ---- bare statements --------------------------------------------------

    fn exec_bare(&mut self, expr: &Expr, source: SourceId) -> Result<(), Rejection> {
        match &expr.kind {
            ExprKind::Call { callee, args } if is_assert(callee) => self.exec_assert(args, source),
            ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } => self.exec_insert(lhs, rhs, source),
            ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } => self.exec_delete(lhs, rhs, source),
            // `-selection` — a prefix-minus delete of the rows a selector picks
            // (§8): `-.coll[:x | pred]` removes every matching row through the
            // same §21.1 cascade planner as a keyed delete.
            ExprKind::Unary { op: UnaryOp::Neg, operand } => self.exec_delete_selection(operand, source),
            ExprKind::Block { base, members } => self.exec_patch(base, members, source),
            // Internal mutation calls and other statement forms are CORE seams.
            _ => Ok(()),
        }
    }

    fn exec_assert(&mut self, args: &[Arg], source: SourceId) -> Result<(), Rejection> {
        let current = self.current()?;
        let Some(Arg::Positional(cond)) = args.first() else {
            return Err(Rejection::new(RejectionReason::Malformed, "`assert` requires a condition"));
        };
        if matches!(self.eval_value(cond, source, &current)?, Cell::Scalar(Value::Bool(true))) {
            return Ok(());
        }
        let message = match args.get(1) {
            Some(Arg::Positional(Expr { kind: ExprKind::Str(text), .. })) => text.clone(),
            _ => "assertion failed".to_owned(),
        };
        Err(Rejection::new(RejectionReason::Assertion, message))
    }

    fn exec_insert(&mut self, collection: &Expr, object: &Expr, source: SourceId) -> Result<(), Rejection> {
        if self.collection_ref(collection).is_some() {
            self.insert_row(collection, object, source)?;
            return Ok(());
        }
        // §8.5: `.set_field + values` is set union — adding an existing member
        // leaves the set unchanged (a no-op that produces no state change).
        if let Some((row, field)) = self.set_field_target(collection, source)? {
            return self.set_mutate(&row, &field, object, source, true);
        }
        Ok(())
    }

    /// Resolve `expr` to `(row, field)` when it addresses a set-typed field of a
    /// row — the target of a `+`/`-` set mutation (§8.5). Any other target is
    /// `None`, leaving the statement a documented no-op.
    fn set_field_target(&self, expr: &Expr, source: SourceId) -> Result<Option<(RowTarget, String)>, Rejection> {
        let Some((row, field)) = self.field_target(expr, source)? else { return Ok(None) };
        let is_set = self
            .compiled
            .collection(&row.collection)
            .and_then(|c| c.field(&field))
            .is_some_and(|f| matches!(f.ty, liasse_value::Type::Set(_)));
        Ok(is_set.then_some((row, field)))
    }

    /// Apply a set `+`/`-` mutation to `field` of `row` (§8.5): union in (or
    /// difference out) every incoming member. The incoming operand is a single
    /// scalar or a set value; an unchanged result stages nothing (§8.9).
    fn set_mutate(
        &mut self,
        row: &RowTarget,
        field: &str,
        value: &Expr,
        source: SourceId,
        add: bool,
    ) -> Result<(), Rejection> {
        let current = self.current()?;
        let incoming: Vec<Value> = match self.eval_value(value, source, &current)? {
            Cell::Scalar(Value::Set(members)) => members.into_iter().collect(),
            Cell::Scalar(scalar) => vec![scalar],
            _ => return Err(Rejection::new(RejectionReason::TypeError, "a set mutation takes a member or a set")),
        };
        let mut members: BTreeSet<Value> = match self.prospective.get(&row.address).and_then(|f| f.get(field)) {
            Some(Value::Set(existing)) => existing.clone(),
            _ => BTreeSet::new(),
        };
        for member in incoming {
            if add {
                members.insert(member);
            } else {
                members.remove(&member);
            }
        }
        self.write_field(row, field, Value::Set(members))
    }

    /// Construct and stage one row from `collection + { … }` (§8.4), applying
    /// insertion defaults and normalization, and return its address so a local
    /// binding can name the inserted row.
    fn insert_row(
        &mut self,
        collection: &Expr,
        object: &Expr,
        source: SourceId,
    ) -> Result<RowTarget, Rejection> {
        let Some((_, name)) = self.collection_ref(collection) else {
            return Err(Rejection::new(RejectionReason::Malformed, "insert targets a top-level collection"));
        };
        let ExprKind::Object(members) = &object.kind else {
            return Err(Rejection::new(RejectionReason::Malformed, "insert takes a `{ field: value }` row"));
        };
        let current = self.current()?;
        let mut fields = FieldMap::new();
        for member in members {
            if let Some((field, value)) = self.object_member(member, &current, source)? {
                fields.insert(field, value);
            }
        }
        let collection = self.collection(&name)?;
        rules::apply_defaults(collection, &mut fields, self.ctx, self.prospective)?;
        rules::normalize_all(collection, &mut fields, self.ctx, self.prospective)?;
        let address = self.key_address(&name, &fields)?;
        if self.prospective.contains(&address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "a row with this key already exists")
                .at(address.render()));
        }
        self.prospective.insert(address.clone(), fields);
        self.mark(address.clone());
        Ok(RowTarget { address, collection: name })
    }

    fn object_member(
        &self,
        member: &BlockMember,
        current: &Cell,
        source: SourceId,
    ) -> Result<Option<(String, Value)>, Rejection> {
        match &member.kind {
            BlockMemberKind::Named { name, value: Some(value) } => {
                Ok(Some((name.text.clone(), self.scalar_value(value, source, current)?)))
            }
            BlockMemberKind::Assign { target, value } => {
                Ok(Some((target.text.clone(), self.scalar_value(value, source, current)?)))
            }
            BlockMemberKind::Shorthand(expr) => match &expr.kind {
                ExprKind::Param(id) => Ok(Some((id.text.clone(), self.scalar_value(expr, source, current)?))),
                ExprKind::Field { member: field, .. } | ExprKind::Name(field) => {
                    Ok(Some((field.text.clone(), self.scalar_value(expr, source, current)?)))
                }
                _ => Ok(None),
            },
            BlockMemberKind::Named { value: None, .. }
            | BlockMemberKind::Clear(_)
            | BlockMemberKind::Directive { .. } => Ok(None),
        }
    }

    fn exec_delete(&mut self, collection: &Expr, keys: &Expr, source: SourceId) -> Result<(), Rejection> {
        let Some((_path, name)) = self.collection_ref(collection) else {
            // §8.5: `.set_field - values` is set difference — removing an absent
            // member leaves the set unchanged.
            if let Some((row, field)) = self.set_field_target(collection, source)? {
                return self.set_mutate(&row, &field, keys, source, false);
            }
            return Ok(());
        };
        let current = self.current()?;
        let targets: Vec<Value> = match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().collect(),
            scalar => vec![scalar],
        };
        let initial: Vec<RowRef> = targets.into_iter().map(|key| RowRef::new(name.clone(), key)).collect();
        self.delete_rows(initial)
    }

    /// `-selection` (§8): delete every row a selector picks. The operand is a
    /// collection selection (`.coll[:x | pred]`, `.coll[keys]`); it is evaluated
    /// to its row set and each selected row is deleted by key through the §21.1
    /// planner. A non-collection operand (a scalar negation) stages nothing.
    fn exec_delete_selection(&mut self, operand: &Expr, source: SourceId) -> Result<(), Rejection> {
        let base = match &operand.kind {
            ExprKind::Select { base, .. } => base.as_ref(),
            _ => operand,
        };
        let Some((_path, name)) = self.collection_ref(base) else {
            return Ok(());
        };
        let current = self.current()?;
        let keys: Vec<Value> = match self.eval_value(operand, source, &current)? {
            Cell::Collection(rows) => rows.iter().map(|row| row.key().clone()).collect(),
            Cell::Row(row) => vec![row.key().clone()],
            Cell::Scalar(_) => return Ok(()),
        };
        let initial: Vec<RowRef> = keys.into_iter().map(|key| RowRef::new(name.clone(), key)).collect();
        self.delete_rows(initial)
    }

    /// Plan and apply the deletion of `initial` (§21.1): a delete is a graph
    /// operation, so the cascade closure and the surviving-row patches every
    /// inbound `$on_delete` policy induces are planned from the pre-delete state
    /// (an absent key stages nothing; a `restrict` block or conflicting patch
    /// rejects) and then applied atomically.
    fn delete_rows(&mut self, initial: Vec<RowRef>) -> Result<(), Rejection> {
        let planned = cascade::plan(self.compiled, self.ctx, self.prospective, &initial)?;
        self.apply_deletion(&planned)
    }

    /// Apply a planned deletion (§21.1): remove every closured row, then patch
    /// each surviving referencing row (normalizing the written fields, §5.4).
    fn apply_deletion(&mut self, planned: &PlannedDeletion) -> Result<(), Rejection> {
        for row in planned.plan.deletes() {
            if let Some(address) = planned.addresses.get(row) {
                self.prospective.remove(address);
            }
        }
        for (row, patch) in planned.plan.patches() {
            let Some(address) = planned.addresses.get(row) else { continue };
            let Some(mut fields) = self.prospective.get(address).cloned() else { continue };
            for (field, value) in patch {
                fields.insert(field.clone(), value.clone());
            }
            let collection = self.collection(&row.collection)?;
            for field in patch.keys() {
                rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
            }
            self.place(address, &row.collection, fields)?;
        }
        Ok(())
    }

    fn exec_patch(&mut self, base: &Expr, members: &[BlockMember], source: SourceId) -> Result<(), Rejection> {
        match self.patch_plan(base, source)? {
            // §8.9: a keyed row patch targets one existing row; a missing target rejects.
            PatchPlan::Single(row) => {
                let original = self.prospective.get(&row.address).cloned().ok_or_else(|| {
                    Rejection::new(RejectionReason::MissingTarget, "the patched row does not exist")
                        .at(row.address.render())
                })?;
                self.apply_patch(&row, original, members, source)
            }
            // §8.9: a filtered bulk patch patches each matched row; a zero-match
            // selection stages nothing and the program completes unchanged.
            PatchPlan::Many(rows) => {
                for row in rows {
                    let Some(original) = self.prospective.get(&row.address).cloned() else { continue };
                    self.apply_patch(&row, original, members, source)?;
                }
                Ok(())
            }
        }
    }

    /// Resolve a patch base to the row(s) it targets: the receiver or a keyed
    /// selector names exactly one row (§8.9, a missing one rejects); a bound
    /// filtered selector `.coll[:x | pred]` names its whole matching set (§8.9
    /// bulk patch), possibly empty.
    fn patch_plan(&self, base: &Expr, source: SourceId) -> Result<PatchPlan, Rejection> {
        if let ExprKind::Select { base: inner, selector: Selector::Bind { .. } } = &base.kind {
            let Some((_path, name)) = self.collection_ref(inner) else {
                return Err(Rejection::new(RejectionReason::Malformed, "a patch needs a row source"));
            };
            let current = self.current()?;
            let keys: Vec<Value> = match self.eval_value(base, source, &current)? {
                Cell::Collection(rows) => rows.iter().map(|row| row.key().clone()).collect(),
                Cell::Row(row) => vec![row.key().clone()],
                Cell::Scalar(_) => Vec::new(),
            };
            let targets = keys
                .into_iter()
                .map(|key| RowTarget {
                    address: materialize::top_address(&name, KeyValue::single(key)),
                    collection: name.clone(),
                })
                .collect();
            return Ok(PatchPlan::Many(targets));
        }
        let row = self
            .row_target(base, source)?
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "a patch needs a row source"))?;
        Ok(PatchPlan::Single(row))
    }

    /// Apply one patch to `row` whose current fields are `original` (§8.6): every
    /// right-hand expression reads the row at the patch start, then the collected
    /// updates are written, normalized, and the row re-placed (rekeying if a key
    /// field changed, §5.4).
    fn apply_patch(
        &mut self,
        row: &RowTarget,
        original: FieldMap,
        members: &[BlockMember],
        source: SourceId,
    ) -> Result<(), Rejection> {
        let start = row_cell(self.collection(&row.collection)?, &original);
        let scope = self.patch_scope(&row.collection);
        let mut updates: Vec<(String, Value)> = Vec::new();
        for member in members {
            if let Some(update) = self.patch_member(member, &scope, &start, source)? {
                updates.push(update);
            }
        }
        let mut fields = original;
        for (field, value) in &updates {
            fields.insert(field.clone(), value.clone());
        }
        for (field, _) in &updates {
            let collection = self.collection(&row.collection)?;
            rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
        }
        self.place(&row.address, &row.collection, fields)
    }

    fn patch_scope(&self, collection: &str) -> RuntimeScope {
        let root = ExprType::Row(self.ctx.schema.root_row_type());
        let current = self
            .ctx
            .schema
            .receiver_row_type(std::slice::from_ref(&collection.to_owned()))
            .unwrap_or_else(|| root.clone());
        let mut scope = RuntimeScope::new(current, root);
        for (name, ty) in &self.mutation.params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        let (types, _) = local_bindings(&self.locals, self.ctx, self.prospective);
        for (name, ty) in types {
            scope = scope.with_binding(name, ty);
        }
        scope
    }

    fn patch_member(
        &self,
        member: &BlockMember,
        scope: &RuntimeScope,
        start: &Cell,
        source: SourceId,
    ) -> Result<Option<(String, Value)>, Rejection> {
        let value_of = |expr: &Expr| -> Result<Value, Rejection> {
            let typed = check_expression(scope, source, expr)
                .map_err(|_| Rejection::new(RejectionReason::Malformed, "a patch value did not type-check"))?;
            match self.ctx.eval_with(self.prospective, &typed, start, self.binding_cells())? {
                Cell::Scalar(value) => Ok(value),
                _ => Err(Rejection::new(RejectionReason::TypeError, "a patch assigns a scalar value")),
            }
        };
        match &member.kind {
            BlockMemberKind::Assign { target, value } => Ok(Some((target.text.clone(), value_of(value)?))),
            BlockMemberKind::Named { name, value: Some(value) } => Ok(Some((name.text.clone(), value_of(value)?))),
            BlockMemberKind::Clear(field) => Ok(Some((field.text.clone(), Value::None))),
            BlockMemberKind::Shorthand(expr) => match &expr.kind {
                ExprKind::Param(id) => Ok(Some((id.text.clone(), value_of(expr)?))),
                ExprKind::Field { member: field, .. } | ExprKind::Name(field) => {
                    Ok(Some((field.text.clone(), value_of(expr)?)))
                }
                _ => Ok(None),
            },
            BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Directive { .. } => Ok(None),
        }
    }

    // ---- target resolution ------------------------------------------------

    /// Resolve an assignment/clear target to `(row, field)`, or `None` for a
    /// local binding or unsupported target.
    fn field_target(&self, target: &Expr, source: SourceId) -> Result<Option<(RowTarget, String)>, Rejection> {
        let ExprKind::Field { base, member } = &target.kind else {
            return Ok(None);
        };
        Ok(self.row_target(base, source)?.map(|row| (row, member.text.clone())))
    }

    /// Resolve an expression denoting one row to its address and collection.
    fn row_target(&self, expr: &Expr, source: SourceId) -> Result<Option<RowTarget>, Rejection> {
        match &expr.kind {
            ExprKind::Current => Ok(self.receiver.clone()),
            ExprKind::Select { base, selector: Selector::Keys(keys) } => {
                let Some((path, name)) = self.collection_ref(base) else {
                    return Ok(None);
                };
                let Some(key_expr) = keys.first() else { return Ok(None) };
                let current = self.current()?;
                let key = self.scalar_value(key_expr, source, &current)?;
                Ok(Some(RowTarget { address: path.row(KeyValue::single(key)), collection: name }))
            }
            _ => Ok(None),
        }
    }

    /// Resolve a collection expression (`.name`, `/name`, bare `name`) to its
    /// path and name, or `None` when it is not a known top-level collection.
    fn collection_ref(&self, expr: &Expr) -> Option<(CollectionPath, String)> {
        let name = match &expr.kind {
            ExprKind::Field { member, .. } => member.text.clone(),
            ExprKind::Name(id) => id.text.clone(),
            _ => return None,
        };
        self.compiled
            .collection(&name)
            .map(|_| (CollectionPath::top(NameSegment::new(name.clone())), name))
    }
}

fn is_assert(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Name(id) if id.text == "assert")
}
