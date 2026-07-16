//! The mutation-program interpreter (§8): it executes statements in order
//! against the prospective state, applying insertion defaults and normalization
//! as it writes, and records the return expression for post-commit evaluation.
//!
//! CORE scope covers the operator forms the tasks app (§3.2) and the §5/§8 rule
//! cases exercise: field assignment, keyed single-row insert, keyed delete,
//! keyed single-row patch, optional-field clear, `assert`, and `return`. Local
//! bindings, view-sourced insert/replace, internal calls, and multi-row patch
//! sources are documented seams.

use liasse_diag::SourceId;
use liasse_expr::{check_expression, Cell, ExprType};
use liasse_ident::NameSegment;
use liasse_syntax::{Arg, BinaryOp, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, Stmt, StmtKind};
use liasse_store::{CollectionPath, KeyValue, RowAddress};
use liasse_value::Value;

use crate::compiled::{Compiled, CompiledCollection, CompiledMutation};
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

/// The mutation-program interpreter over one admission.
pub(crate) struct Interp<'a> {
    pub(crate) compiled: &'a Compiled,
    pub(crate) ctx: &'a EvalCtx<'a>,
    pub(crate) prospective: &'a mut Prospective,
    pub(crate) mutation: &'a CompiledMutation,
    pub(crate) receiver: Option<RowTarget>,
    pub(crate) touched: Vec<RowAddress>,
    pub(crate) ret: Option<(Expr, SourceId)>,
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
                Ok(row_cell(collection, fields))
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

    fn eval_value(&self, expr: &Expr, source: SourceId, current: &Cell) -> Result<Cell, Rejection> {
        let typed = check_expression(&self.mutation.scope, source, expr)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the request expression did not type-check"))?;
        self.ctx.eval(self.prospective, &typed, current)
    }

    fn scalar_value(&self, expr: &Expr, source: SourceId, current: &Cell) -> Result<Value, Rejection> {
        match self.eval_value(expr, source, current)? {
            Cell::Scalar(value) => Ok(value),
            _ => Err(Rejection::new(RejectionReason::TypeError, "expected a scalar value here")),
        }
    }

    // ---- assignment -------------------------------------------------------

    fn exec_assign(&mut self, target: &Expr, value: &Expr, source: SourceId) -> Result<(), Rejection> {
        let Some((row, field)) = self.field_target(target, source)? else {
            // A local binding (`name = ...`) or collection replacement is a
            // documented CORE seam: it stages no change.
            return Ok(());
        };
        let current = self.current()?;
        let typed = check_expression(&self.mutation.scope, source, value)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the assigned value did not type-check"))?;
        let scalar = match self.ctx.eval(self.prospective, &typed, &current)? {
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
        let Some((_, name)) = self.collection_ref(collection) else {
            // A set-member addition (`set + values`) is a documented CORE seam.
            return Ok(());
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
        self.mark(address);
        Ok(())
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
        let Some((path, _)) = self.collection_ref(collection) else {
            return Ok(());
        };
        let current = self.current()?;
        let targets: Vec<Value> = match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().collect(),
            scalar => vec![scalar],
        };
        for key in targets {
            // §8.9 / SPEC-ISSUES item 7: `collection - key` of an absent key is
            // unassigned; the least-surprising choice is a no-op success.
            self.prospective.remove(&path.row(KeyValue::single(key)));
        }
        Ok(())
    }

    fn exec_patch(&mut self, base: &Expr, members: &[BlockMember], source: SourceId) -> Result<(), Rejection> {
        let row = self
            .row_target(base, source)?
            .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "a patch needs a row source"))?;
        // §8.9: a keyed row patch targets one existing row; a missing target rejects.
        let original = self.prospective.get(&row.address).cloned().ok_or_else(|| {
            Rejection::new(RejectionReason::MissingTarget, "the patched row does not exist")
                .at(row.address.render())
        })?;
        let start = row_cell(self.collection(&row.collection)?, &original);
        let scope = self.patch_scope(&row.collection);
        // §8.6: every right-hand expression reads the row at the patch start.
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
            match self.ctx.eval(self.prospective, &typed, start)? {
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
