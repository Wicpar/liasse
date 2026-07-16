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
use liasse_expr::{check_expression, Cell, ExprType, Row, RowId};
use liasse_ident::NameSegment;
use liasse_syntax::{Arg, BinaryOp, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, Stmt, StmtKind, UnaryOp};
use liasse_store::{CollectionPath, KeyValue, RowAddress};
use liasse_value::{Struct, Text, Value};

use crate::cascade::{self, PlannedDeletion};
use crate::compiled::{Compiled, CompiledCollection, CompiledMutation, CompiledStruct};
use crate::deletion::RowRef;
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize::{self, FieldMap};
use crate::rules;
use crate::scope::RuntimeScope;
use crate::state::Prospective;

/// A row-source location: the selected row's address and the declaration-name
/// path of its collection (`["companies"]` top-level, `["companies", "offices"]`
/// nested, §5.4). The path resolves the compiled collection and the receiver
/// row type; the address locates the row (ancestor identity included).
#[derive(Clone)]
pub(crate) struct RowTarget {
    pub(crate) address: RowAddress,
    pub(crate) path: Vec<String>,
}

/// A resolved collection location: where its rows live in the store
/// ([`CollectionPath`], ancestor identity included) and the declaration-name
/// path that resolves its compiled shape (§5.4).
struct CollectionLoc {
    store_path: CollectionPath,
    decl: Vec<String>,
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
                let Some(ty) = ctx.schema.receiver_row_type(&target.path) else {
                    continue;
                };
                let Some(cell) = ctx.materialize_row_cell(prospective, &target.path, &target.address) else {
                    continue;
                };
                types.insert(name.clone(), ty);
                cells.insert(name.clone(), cell);
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
                self.ctx.materialize_row_cell(self.prospective, &receiver.path, &receiver.address).ok_or_else(
                    || {
                        Rejection::new(RejectionReason::MissingTarget, "the selected row no longer exists")
                            .at(receiver.address.render())
                    },
                )
            }
        }
    }

    /// The compiled collection at a declaration-name path (top-level or nested).
    fn collection_at(&self, path: &[String]) -> Result<&'a CompiledCollection, Rejection> {
        self.compiled.collection_at(path).ok_or_else(|| {
            Rejection::new(RejectionReason::Malformed, format!("unknown collection `{}`", path.join("/")))
        })
    }

    /// The key of `fields` in `collection`, in `$key` order (§5.4).
    fn key_of(collection: &CompiledCollection, fields: &FieldMap) -> Option<KeyValue> {
        let mut components = collection.key.iter().map(|field| fields.get(field.as_str()).cloned());
        let first = components.next().flatten()?;
        let mut rest = Vec::new();
        for component in components {
            rest.push(component?);
        }
        Some(KeyValue::composite(first, rest))
    }

    /// The address `fields` occupy in the collection at declaration path `path`,
    /// rooted under `store_path` so a nested row keeps its ancestor identity.
    fn key_address(&self, store_path: &CollectionPath, path: &[String], fields: &FieldMap) -> Result<RowAddress, Rejection> {
        let collection = self.collection_at(path)?;
        let key = Self::key_of(collection, fields)
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "the row is missing a key field"))?;
        Ok(store_path.row(key))
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
            // §6.3/§5.6: a single row used where a scalar is required — a ref value,
            // a key selector — is its typed key. This is what makes `author: $actor`
            // store the actor's account key (§11.3), and a `.coll[$actor]` selector
            // resolve by the actor's identity.
            Cell::Row(row) => Ok(row.key().clone()),
            Cell::Collection(_) => {
                Err(Rejection::new(RejectionReason::TypeError, "expected a scalar value here"))
            }
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
        let collection = self.collection_at(&row.path)?;
        // An enum field takes a declared label (§5.9): coerce a `text` value to a
        // positioned enum value, rejecting an undeclared label. Non-enum fields
        // keep the ordinary static assignability check.
        let scalar = if let Some(field_meta) = collection.field(&field) {
            let where_path = format!("{}/{}", row.address.render(), field);
            if crate::rules::is_enum_field(&field_meta.ty) {
                crate::rules::coerce_value(&field_meta.ty, &scalar, &field, &where_path)?
            } else {
                if let Some(from) = typed.ty().as_scalar()
                    && !crate::schema::assignable(from, &field_meta.ty)
                {
                    return Err(Rejection::new(
                        RejectionReason::TypeError,
                        format!("value of type `{}` is not assignable to `{}`", from.name(), field_meta.ty.name()),
                    )
                    .at(where_path));
                }
                scalar
            }
        } else {
            scalar
        };
        self.write_field(&row, &field, scalar)
    }

    /// Bind a lexical local `name` to `value` (§8.1). An insert expression
    /// (`.coll + { … }`) performs the insert and binds the constructed row, so
    /// `name = .coll + { … }` then `return name { … }` returns the committed row
    /// (§8.4, §8.10); any other right-hand side binds its evaluated value.
    fn bind_local(&mut self, name: String, value: &Expr, source: SourceId) -> Result<(), Rejection> {
        if let ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } = &value.kind
            && self.collection_ref(lhs, source)?.is_some()
        {
            let target = self.insert_row(lhs, rhs, source)?;
            self.locals.insert(name, LocalBind::Row(target));
            return Ok(());
        }
        // `name = .coll - keys` / `name = -.coll[…]` (§8.4): the deleted rows, as
        // they existed immediately before removal, are captured in selector order
        // and bound as a collection value, so `return name { … }` projects them.
        if let ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } = &value.kind
            && let Some(loc) = self.collection_ref(lhs, source)?
        {
            let keys = self.delete_key_values(rhs, source)?;
            return self.bind_deleted(name, loc.decl, keys);
        }
        if let ExprKind::Unary { op: UnaryOp::Neg, operand } = &value.kind
            && let ExprKind::Select { base, .. } = &operand.kind
            && let Some(loc) = self.collection_ref(base, source)?
        {
            let keys = self.selection_key_values(operand, source)?;
            return self.bind_deleted(name, loc.decl, keys);
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
    fn bind_deleted(&mut self, name: String, decl: Vec<String>, keys: Vec<Value>) -> Result<(), Rejection> {
        // Cascade deletion planning is scoped to top-level collections (§21.1);
        // nested-collection deletion is a documented seam.
        let collection = decl.last().cloned().unwrap_or_default();
        let mut seen = BTreeSet::new();
        let mut ordered = Vec::new();
        for key in keys {
            if seen.insert(key.clone()) {
                ordered.push(key);
            }
        }
        let compiled = self.collection_at(&decl)?;
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
        let collection = self.collection_at(&row.path)?;
        rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
        self.place(&row.address, &row.path, fields)
    }

    /// Place `fields` for the row currently at `address` (whose collection is at
    /// declaration path `path`), moving it — and its whole descendant subtree — to
    /// the new address when a key field changed (an atomic rekey, §5.4). A nested
    /// row keeps its ancestor identity; an ancestor rekey re-roots every descendant
    /// under the new ancestor key so no ghost subtree survives at the old address.
    fn place(&mut self, address: &RowAddress, path: &[String], fields: FieldMap) -> Result<(), Rejection> {
        let new_address = self.key_address(&address.collection(), path, &fields)?;
        if &new_address == address {
            self.prospective.replace(address, fields);
            self.mark(address.clone());
            return Ok(());
        }
        if self.prospective.contains(&new_address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "rekey target already exists")
                .at(new_address.render()));
        }
        // Re-root every descendant subtree row under the new ancestor address, then
        // move the row itself. Collision on any moved descendant rejects (§5.4).
        let old_depth = address.depth();
        let descendants: Vec<RowAddress> = self
            .prospective
            .working()
            .keys()
            .filter(|other| other.depth() > old_depth && is_prefix(address, other))
            .cloned()
            .collect();
        for descendant in descendants {
            let moved = reroot(&new_address, &descendant, old_depth);
            if self.prospective.contains(&moved) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "rekey descendant target already exists")
                    .at(moved.render()));
            }
            let Some(sub) = self.prospective.get(&descendant).cloned() else { continue };
            self.prospective.remove(&descendant);
            self.prospective.insert(moved.clone(), sub);
            self.mark(moved);
        }
        self.prospective.remove(address);
        self.prospective.insert(new_address.clone(), fields);
        if let Some(receiver) = &mut self.receiver
            && &receiver.address == address
        {
            receiver.address = new_address.clone();
        }
        self.mark(new_address);
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
        if self.collection_ref(collection, source)?.is_some() {
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
            .collection_at(&row.path)
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
    /// insertion defaults, normalization, static-struct defaults (§5.3), and any
    /// supplied nested-collection initializers (§5.5), returning its address so a
    /// local binding can name the inserted row. A nested initializer's rows are
    /// staged under the parent address and validated atomically (§5.5): a failure
    /// there rejects the whole insertion, parent included.
    fn insert_row(
        &mut self,
        collection: &Expr,
        object: &Expr,
        source: SourceId,
    ) -> Result<RowTarget, Rejection> {
        let Some(loc) = self.collection_ref(collection, source)? else {
            return Err(Rejection::new(RejectionReason::Malformed, "insert targets a collection"));
        };
        let ExprKind::Object(members) = &object.kind else {
            return Err(Rejection::new(RejectionReason::Malformed, "insert takes a `{ field: value }` row"));
        };
        let compiled = self.collection_at(&loc.decl)?;
        let current = self.current()?;
        // Partition the object members: scalar/ref/set fields and static structs
        // stage onto this row; a member naming a child collection is a nested
        // initializer staged after the parent address is known.
        let mut fields = FieldMap::new();
        let mut initializers: Vec<(String, &Expr)> = Vec::new();
        for member in members {
            if let Some((field, value_expr)) = named_member(member) {
                if compiled.child(&field).is_some() {
                    initializers.push((field, value_expr));
                    continue;
                }
                if let Some(struct_meta) = compiled.structs.iter().find(|s| s.name == field) {
                    let value = self.struct_value(struct_meta, value_expr, source, &current)?;
                    fields.insert(field, value);
                    continue;
                }
            }
            if let Some((field, value)) = self.object_member(member, &current, source)? {
                fields.insert(field, value);
            }
        }
        rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective)?;
        rules::normalize_all(compiled, &mut fields, self.ctx, self.prospective)?;
        rules::coerce_fields(compiled, &mut fields, &loc.decl.join("."))?;
        let address = self.key_address(&loc.store_path, &loc.decl, &fields)?;
        if self.prospective.contains(&address) {
            return Err(Rejection::new(RejectionReason::DuplicateKey, "a row with this key already exists")
                .at(address.render()));
        }
        self.prospective.insert(address.clone(), fields);
        self.mark(address.clone());
        for (child, value_expr) in initializers {
            self.stage_initializer(&address, &loc.decl, &child, value_expr, source)?;
        }
        Ok(RowTarget { address, path: loc.decl })
    }

    /// Stage the rows of a supplied nested-collection initializer (§5.5) under
    /// the parent `address`: the initializer is a keyed row view, each row of
    /// which is inserted into the child collection through the ordinary defaults,
    /// normalization, and duplicate-key rules. Rows are validated atomically with
    /// the parent by the shared final rule pass over the touched set.
    fn stage_initializer(
        &mut self,
        parent: &RowAddress,
        parent_decl: &[String],
        child: &str,
        value_expr: &Expr,
        source: SourceId,
    ) -> Result<(), Rejection> {
        let mut decl = parent_decl.to_vec();
        decl.push(child.to_owned());
        let store_path =
            CollectionPath::nested(parent.steps().cloned(), NameSegment::new(child.to_owned()));
        let current = self.current()?;
        let rows = match self.eval_value(value_expr, source, &current)? {
            Cell::Collection(rows) => rows,
            Cell::Row(row) => vec![*row],
            Cell::Scalar(_) => {
                return Err(Rejection::new(
                    RejectionReason::TypeError,
                    format!("child collection `{child}` initializer must be a keyed row view"),
                ));
            }
        };
        let compiled = self.collection_at(&decl)?;
        for row in rows {
            let mut fields = FieldMap::new();
            for (name, cell) in row.cells() {
                if let Cell::Scalar(value) = cell {
                    fields.insert(name.clone(), value.clone());
                }
            }
            rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective)?;
            rules::normalize_all(compiled, &mut fields, self.ctx, self.prospective)?;
            rules::coerce_fields(compiled, &mut fields, &decl.join("."))?;
            let address = self.key_address(&store_path, &decl, &fields)?;
            if self.prospective.contains(&address) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "a row with this key already exists")
                    .at(address.render()));
            }
            self.prospective.insert(address.clone(), fields);
            self.mark(address);
        }
        Ok(())
    }

    /// Build a static-struct field value from its supplied initializer object
    /// (§5.3): every supplied member decodes onto the struct, then the struct's
    /// own field defaults resolve (§5.1) and its normalizers run. An omitted
    /// optional struct member stays absent. Returns the struct as a value that
    /// shares the containing row's lifecycle.
    fn struct_value(
        &self,
        struct_meta: &CompiledStruct,
        value_expr: &Expr,
        source: SourceId,
        current: &Cell,
    ) -> Result<Value, Rejection> {
        let ExprKind::Object(members) = &value_expr.kind else {
            // A non-object struct initializer (a view/ref) is a documented seam;
            // evaluate it verbatim as a scalar value.
            return self.scalar_value(value_expr, source, current);
        };
        let mut fields = FieldMap::new();
        for member in members {
            if let Some((field, value)) = self.object_member(member, current, source)? {
                fields.insert(field, value);
            }
        }
        for field in &struct_meta.fields {
            if fields.contains_key(&field.name) {
                continue;
            }
            if let Some((typed, _)) = &field.default {
                let struct_cell = struct_row_cell(struct_meta, &fields);
                let value = match self.ctx.eval(self.prospective, typed, &struct_cell)? {
                    Cell::Scalar(value) => value,
                    _ => Value::None,
                };
                fields.insert(field.name.clone(), value);
            }
        }
        // §5.10: a struct `$check` constrains the complete struct after defaults,
        // with `.` the prospective struct; a failure rejects the containing insert.
        let struct_cell = struct_row_cell(struct_meta, &fields);
        for check in &struct_meta.row_checks {
            if !matches!(self.ctx.eval(self.prospective, &check.condition, &struct_cell)?, Cell::Scalar(Value::Bool(true))) {
                return Err(Rejection::new(RejectionReason::Check, check.message.clone()));
            }
        }
        Ok(Value::Struct(Struct::new(
            fields.into_iter().map(|(name, value)| (Text::new(name), value)),
        )))
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
        let Some(loc) = self.collection_ref(collection, source)? else {
            // §8.5: `.set_field - values` is set difference — removing an absent
            // member leaves the set unchanged.
            if let Some((row, field)) = self.set_field_target(collection, source)? {
                return self.set_mutate(&row, &field, keys, source, false);
            }
            return Ok(());
        };
        let name = loc.decl.last().cloned().unwrap_or_default();
        let current = self.current()?;
        let targets: Vec<Value> = match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().collect(),
            scalar => vec![scalar],
        };
        // §5.4/§21.1: the cascade planner operates over the top-level graph; a
        // nested collection's row (a meter spend/pool, §15) has no inbound refs in
        // CORE scope, so it is removed directly with its descendant subtree.
        if loc.decl.len() > 1 {
            for key in targets {
                self.remove_subtree(&loc.store_path.row(KeyValue::single(key)));
            }
            return Ok(());
        }
        let initial: Vec<RowRef> = targets.into_iter().map(|key| RowRef::new(name.clone(), key)).collect();
        self.delete_rows(initial)
    }

    /// Remove the row at `address` and every descendant row beneath it (§5.4), a
    /// direct nested-collection deletion.
    fn remove_subtree(&mut self, address: &RowAddress) {
        if !self.prospective.contains(address) {
            return;
        }
        let depth = address.depth();
        let descendants: Vec<RowAddress> = self
            .prospective
            .working()
            .keys()
            .filter(|other| other.depth() > depth && is_prefix(address, other))
            .cloned()
            .collect();
        for descendant in descendants {
            self.prospective.remove(&descendant);
        }
        self.prospective.remove(address);
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
        let Some(loc) = self.collection_ref(base, source)? else {
            return Ok(());
        };
        let name = loc.decl.last().cloned().unwrap_or_default();
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
            let decl = std::slice::from_ref(&row.collection);
            let collection = self.collection_at(decl)?;
            for field in patch.keys() {
                rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
            }
            self.place(address, decl, fields)?;
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
            let Some(loc) = self.collection_ref(inner, source)? else {
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
                    address: loc.store_path.row(KeyValue::single(key)),
                    path: loc.decl.clone(),
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
        let start = row_cell(self.collection_at(&row.path)?, &original);
        let scope = self.patch_scope(&row.path);
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
            let collection = self.collection_at(&row.path)?;
            rules::normalize_field(collection, field, &mut fields, self.ctx, self.prospective)?;
        }
        self.place(&row.address, &row.path, fields)
    }

    fn patch_scope(&self, path: &[String]) -> RuntimeScope {
        let root = ExprType::Row(self.ctx.schema.root_row_type());
        let current = self.ctx.schema.receiver_row_type(path).unwrap_or_else(|| root.clone());
        let mut scope = RuntimeScope::new(current, root);
        for (name, ty) in &self.mutation.params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        // §6.2/§11.1: `$actor`/`$session` stay in scope for a patch value expression.
        for (name, ty) in &self.mutation.context_structurals {
            scope = scope.with_structural(name.clone(), ty.clone());
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

    /// Resolve an expression denoting one row to its address and collection path.
    /// A keyed selector over a collection (top-level or nested) locates one row;
    /// `.` is the receiver. The row need not exist — a stale ancestor address is
    /// still resolvable but reads/patches against it reject (§6.3).
    fn row_target(&self, expr: &Expr, source: SourceId) -> Result<Option<RowTarget>, Rejection> {
        match &expr.kind {
            ExprKind::Current => Ok(self.receiver.clone()),
            ExprKind::Select { base, selector: Selector::Keys(keys) } => {
                let Some(loc) = self.collection_ref(base, source)? else {
                    return Ok(None);
                };
                let Some(key_expr) = keys.first() else { return Ok(None) };
                let current = self.current()?;
                let collection = self.collection_at(&loc.decl)?;
                let key = self.key_from_expr(collection, key_expr, source, &current)?;
                Ok(Some(RowTarget { address: loc.store_path.row(key), path: loc.decl }))
            }
            _ => Ok(None),
        }
    }

    /// A keyed selector's key value as a [`KeyValue`] (§5.4): a lone scalar for a
    /// single-field key, or the composite components in `$key` order read from a
    /// struct selector value.
    fn key_from_expr(
        &self,
        collection: &CompiledCollection,
        key_expr: &Expr,
        source: SourceId,
        current: &Cell,
    ) -> Result<KeyValue, Rejection> {
        let value = self.scalar_value(key_expr, source, current)?;
        match (collection.key.as_slice(), value) {
            ([_], scalar) => Ok(KeyValue::single(scalar)),
            (_, Value::Struct(fields)) => {
                let mut components = collection.key.iter().map(|field| {
                    fields
                        .fields()
                        .find(|(name, _)| name.as_str() == field.as_str())
                        .map(|(_, v)| v.clone())
                });
                let first = components.next().flatten().ok_or_else(|| {
                    Rejection::new(RejectionReason::Malformed, "composite key selector is missing a component")
                })?;
                let mut rest = Vec::new();
                for component in components {
                    rest.push(component.ok_or_else(|| {
                        Rejection::new(RejectionReason::Malformed, "composite key selector is missing a component")
                    })?);
                }
                Ok(KeyValue::composite(first, rest))
            }
            (_, scalar) => Ok(KeyValue::single(scalar)),
        }
    }

    /// Resolve a collection expression to its store path and declaration path.
    /// A bare/top-level name (`.companies`) resolves to a top-level collection; a
    /// member of a resolved parent row (`.companies[@c].offices`) resolves to that
    /// nested collection scoped under the parent's address (§5.4). `None` when the
    /// expression is not a known collection.
    fn collection_ref(&self, expr: &Expr, source: SourceId) -> Result<Option<CollectionLoc>, Rejection> {
        match &expr.kind {
            ExprKind::Name(id) => Ok(self.top_loc(&id.text)),
            ExprKind::Field { base, member } => {
                if let Some(loc) = self.top_loc(&member.text) {
                    return Ok(Some(loc));
                }
                // Nested: the base resolves to a parent row whose compiled shape
                // declares `member` as a child collection.
                let Some(parent) = self.row_target(base, source)? else {
                    return Ok(None);
                };
                let Some(parent_collection) = self.compiled.collection_at(&parent.path) else {
                    return Ok(None);
                };
                if parent_collection.child(&member.text).is_none() {
                    return Ok(None);
                }
                let mut decl = parent.path.clone();
                decl.push(member.text.clone());
                let store_path = CollectionPath::nested(
                    parent.address.steps().cloned(),
                    NameSegment::new(member.text.clone()),
                );
                Ok(Some(CollectionLoc { store_path, decl }))
            }
            _ => Ok(None),
        }
    }

    /// A top-level collection location, if `name` names one.
    fn top_loc(&self, name: &str) -> Option<CollectionLoc> {
        self.compiled.collection(name).map(|_| CollectionLoc {
            store_path: CollectionPath::top(NameSegment::new(name)),
            decl: vec![name.to_owned()],
        })
    }
}

fn is_assert(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Name(id) if id.text == "assert")
}

/// The `name: value` pair of an insert-object member when it is an explicit
/// named or assignment member, so the interpreter can route a nested-collection
/// or static-struct member before decoding it as a scalar field.
fn named_member(member: &BlockMember) -> Option<(String, &Expr)> {
    match &member.kind {
        BlockMemberKind::Named { name, value: Some(value) } => Some((name.text.clone(), value)),
        BlockMemberKind::Assign { target, value } => Some((target.text.clone(), value)),
        _ => None,
    }
}

/// A logical row cell over a static struct's provisional fields, for evaluating
/// a struct field's default (`.` = the struct, §5.1). Every declared struct
/// field is present (absent reads as `none`).
fn struct_row_cell(struct_meta: &CompiledStruct, fields: &FieldMap) -> Cell {
    let cells = struct_meta.fields.iter().map(|field| {
        (field.name.clone(), Cell::Scalar(fields.get(&field.name).cloned().unwrap_or(Value::None)))
    });
    Cell::Row(Box::new(Row::new(RowId::leaf(0), Value::None, cells)))
}

/// Whether `prefix`'s steps are the leading steps of `address` (an ancestor
/// address prefix). Combined with a strict-depth check by the caller, this
/// identifies the descendant subtree of a rekeyed ancestor (§5.4).
fn is_prefix(prefix: &RowAddress, address: &RowAddress) -> bool {
    let mut steps = address.steps();
    prefix.steps().all(|step| steps.next() == Some(step))
}

/// Re-root a descendant address under `new_ancestor`: keep the new ancestor's
/// first `old_depth` levels and append the descendant's tail below that depth
/// (the descendant retains its own key, its ancestor identity is rewritten).
fn reroot(new_ancestor: &RowAddress, descendant: &RowAddress, old_depth: usize) -> RowAddress {
    let mut ancestor = new_ancestor.steps().cloned();
    let Some(first) = ancestor.next() else { return descendant.clone() };
    let mut address = RowAddress::root(first);
    for step in ancestor {
        address = address.child(step);
    }
    for step in descendant.steps().skip(old_depth).cloned() {
        address = address.child(step);
    }
    address
}
