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
use liasse_value::{Ref, Struct, Text, Type, Value};

use crate::cascade::{self, PlannedDeletion};
use crate::compiled::{Compiled, CompiledCollection, CompiledMutation, CompiledStruct};
use crate::deletion::{Erasure, Extract, Occurrence, RowRef};
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize::{self, FieldMap};
use crate::refid::{identity_of, ref_identity};
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

/// The internal-call nesting bound (§8.11): a program calling another mutation
/// recurses this interpreter, so a cyclic mutation graph is capped rather than
/// overflowing the stack. Real packages nest only a handful of levels.
const MAX_CALL_DEPTH: usize = 64;

/// The mutation-program interpreter over one admission.
pub(crate) struct Interp<'a> {
    pub(crate) compiled: &'a Compiled,
    pub(crate) ctx: &'a EvalCtx<'a>,
    pub(crate) prospective: &'a mut Prospective,
    pub(crate) mutation: &'a CompiledMutation,
    pub(crate) receiver: Option<RowTarget>,
    pub(crate) touched: Vec<RowAddress>,
    pub(crate) ret: Option<(Expr, SourceId)>,
    /// The durable extract a `return erase(row)` produced (§21.2 step 6): the
    /// erasure's response value, delivered in place of an ordinary evaluated
    /// `return` because the erase mutated state and cannot be re-evaluated
    /// post-commit as a pure expression.
    pub(crate) erase_result: Option<Value>,
    /// The reintegration bundles every `erase(row)` in this program produced
    /// (§21.2), in execution order — the export sink. Each erase pushes its extract
    /// here whether it was a `return erase(row)` or a bare `erase(row)` statement,
    /// so the produced bundle is always captured (relocation, not destruction) and
    /// a bare erase never silently drops it. The engine reads this back after the
    /// program so no committed erasure leaves an uncaptured bundle.
    pub(crate) erase_exports: Vec<Value>,
    /// Lexical locals bound by `name = …` statements (§8.1), in declaration
    /// order, visible to later statements and to the `return`.
    pub(crate) locals: BTreeMap<String, LocalBind>,
    /// The internal-call nesting depth (§8.11): `0` for the request's own
    /// program, incremented for each `.mut()` call it makes, so recursion is
    /// bounded by [`MAX_CALL_DEPTH`].
    pub(crate) depth: usize,
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
                // §21.2: `return erase(row)` is not a pure post-commit expression —
                // the erase mutates state during the program and its response is the
                // durable extract. Execute it now and record the extract as the
                // response, rather than deferring to `eval_return`.
                if let ExprKind::Call { callee, args } = &expr.kind
                    && is_erase(callee)
                {
                    self.erase_result = Some(self.exec_erase(args, source)?);
                    return Ok(());
                }
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

    // ---- host-namespace calls (§16.4, §17.7) ------------------------------

    /// If `expr` is a resolved host-namespace call `ns.fn(args)` — where `ns` is a
    /// `$requires` key the load-time registry resolution bound — its namespace
    /// local key, the function name, and the argument list (§16.4). A core-language
    /// call (`string.lower`, `now`, an aggregate) is not a `$requires` namespace,
    /// so it falls through to the ordinary expression checker.
    fn host_call<'e>(&self, expr: &'e Expr) -> Option<(&'e str, &'e str, &'e [Arg])> {
        let ExprKind::Call { callee, args } = &expr.kind else { return None };
        let ExprKind::Field { base, member } = &callee.kind else { return None };
        if member.structural {
            return None;
        }
        let ExprKind::Name(namespace) = &base.kind else { return None };
        self.ctx
            .hosts
            .is_namespace(&namespace.text)
            .then_some((namespace.text.as_str(), member.text.as_str(), args.as_slice()))
    }

    /// Evaluate a host-namespace call to its result cell and pinned type (§16.2).
    /// A cose call is routed to the managed keyring; every other namespace call
    /// evaluates its arguments as values and invokes the component through the
    /// conformance guard, so a nonconforming return or a verifier rejection is a
    /// typed rejection that commits no effect (§16.3).
    fn eval_host_call(
        &self,
        namespace: &str,
        function: &str,
        args: &[Arg],
        source: SourceId,
    ) -> Result<(Cell, ExprType), Rejection> {
        if self.ctx.hosts.is_cose(namespace) {
            return self.eval_cose_call(function, args, source);
        }
        let current = self.current()?;
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.scalar_value(arg_expr(arg), source, &current)?);
        }
        let result = self.ctx.hosts.invoke(namespace, function, &values)?;
        let ty = self
            .ctx
            .hosts
            .result_type(namespace, function)
            .unwrap_or_else(|| ExprType::scalar(Type::Json));
        Ok((Cell::Scalar(result), ty))
    }

    /// Evaluate a `cose.sign(/ring, claims)` call (§17.7/§17.8): the first argument
    /// names the keyring by path, the second evaluates to the claim object; signing
    /// goes through the ring's active version, so a §17.9 provider outage rejects
    /// the mutation before any token is minted. The result is the token value.
    fn eval_cose_call(
        &self,
        function: &str,
        args: &[Arg],
        source: SourceId,
    ) -> Result<(Cell, ExprType), Rejection> {
        match function {
            "sign" => {
                let [ring_arg, claims_arg] = args else {
                    return Err(Rejection::new(
                        RejectionReason::Malformed,
                        "`cose.sign` takes a keyring path and a claims object",
                    ));
                };
                let ring = keyring_ref(arg_expr(ring_arg)).ok_or_else(|| {
                    Rejection::new(
                        RejectionReason::Malformed,
                        "`cose.sign` first argument must be a keyring path `/ring`",
                    )
                })?;
                let current = self.current()?;
                let claims = self.scalar_value(arg_expr(claims_arg), source, &current)?;
                let token = self.ctx.hosts.cose_sign(ring, &claims)?;
                Ok((Cell::Scalar(token), ExprType::scalar(Type::Json)))
            }
            // §17.7: `cose.verify` runs during authentication (`$verify`), where the
            // surface layer drives it against the ring's accepted set — not inside a
            // mutation program.
            "verify" => Err(Rejection::new(
                RejectionReason::Malformed,
                "`cose.verify` runs during authentication (`$verify`), not in a mutation program",
            )),
            other => Err(Rejection::new(
                RejectionReason::Malformed,
                format!("unknown cose function `{other}`"),
            )),
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
        // §8.2: `.field = value` at the package root (no row receiver) writes a
        // durable singleton root member. A singleton lives in its own reserved row,
        // not a keyed collection, so it takes a dedicated write path rather than the
        // collection-field one.
        if self.receiver.is_none()
            && let Some((field, ty)) = self.root_singleton_target(target)
        {
            return self.write_singleton_field(&field, &ty, value, source);
        }
        // §8.7: `.coll = view` replaces a whole collection — the target names a
        // collection rather than a row field. Diff the replacement view against the
        // current collection, applying inserts/updates and deleting dropped keys
        // through ordinary §21.1 planning.
        if let Some(loc) = self.collection_ref(target, source)? {
            return self.replace_collection(&loc, value, source);
        }
        let Some((row, field)) = self.field_target(target, source)? else {
            // A target that is neither a row field nor a collection stages nothing
            // (a documented seam).
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

    /// When `target` is a bare root member (`.field`) that names a durable
    /// singleton root member (§8.2), its name and decoded type. `None` for a
    /// selector, a nested path, or a name that is not a writable root singleton
    /// (a collection, computed view, or read-only computed scalar).
    fn root_singleton_target(&self, target: &Expr) -> Option<(String, liasse_value::Type)> {
        let ExprKind::Field { base, member } = &target.kind else { return None };
        if !matches!(base.kind, ExprKind::Current) {
            return None;
        }
        let model = self.ctx.schema.model();
        let node = &model.root().member(&member.text)?.node;
        let ty = crate::singleton::member_type(model, node)?;
        Some((member.text.clone(), ty))
    }

    /// Write a singleton root member into its reserved row (§8.2): evaluate the
    /// value, coerce an enum label / check assignability against the declared type
    /// (§5.9), and stage it onto the singleton row, marking it touched.
    fn write_singleton_field(
        &mut self,
        field: &str,
        ty: &liasse_value::Type,
        value: &Expr,
        source: SourceId,
    ) -> Result<(), Rejection> {
        let current = self.current()?;
        let typed = check_expression(&self.scope(), source, value)
            .map_err(|_| Rejection::new(RejectionReason::Malformed, "the assigned value did not type-check"))?;
        let scalar = match self.ctx.eval_with(self.prospective, &typed, &current, self.binding_cells())? {
            Cell::Scalar(value) => value,
            _ => return Err(Rejection::new(RejectionReason::TypeError, "a field is assigned a scalar value")),
        };
        let address = crate::singleton::address();
        let where_path = format!("{}/{}", address.render(), field);
        let scalar = if crate::rules::is_enum_field(ty) {
            crate::rules::coerce_value(ty, &scalar, field, &where_path)?
        } else {
            if let Some(from) = typed.ty().as_scalar()
                && !crate::schema::assignable(from, ty)
            {
                return Err(Rejection::new(
                    RejectionReason::TypeError,
                    format!("value of type `{}` is not assignable to `{}`", from.name(), ty.name()),
                )
                .at(where_path));
            }
            scalar
        };
        let mut fields = self.prospective.get(&address).cloned().unwrap_or_else(FieldMap::new);
        fields.insert(field.to_owned(), scalar);
        // §8.2/§8.3: the assigned target applies its own normalization, so a
        // written singleton member is normalized exactly as a collection field is.
        rules::normalize_singleton_field(self.compiled, field, &mut fields, self.ctx, self.prospective)?;
        self.prospective.insert(address.clone(), fields);
        self.mark(address);
        Ok(())
    }

    /// Bind a lexical local `name` to `value` (§8.1). An insert expression
    /// (`.coll + { … }`) performs the insert and binds the constructed row, so
    /// `name = .coll + { … }` then `return name { … }` returns the committed row
    /// (§8.4, §8.10); any other right-hand side binds its evaluated value.
    fn bind_local(&mut self, name: String, value: &Expr, source: SourceId) -> Result<(), Rejection> {
        // §16.4/§17.7: `name = ns.fn(args)` binds the result of a resolved
        // host-namespace call — a pure/verifier/generated function, or a
        // `cose.sign(/ring, claims)` token minted through the managed keyring.
        if let Some((namespace, function, args)) = self.host_call(value) {
            let (cell, ty) = self.eval_host_call(namespace, function, args, source)?;
            self.locals.insert(name, LocalBind::Value(cell, ty));
            return Ok(());
        }
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
            let keys = self.delete_key_values(rhs, &loc.decl, source)?;
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
    ///
    /// §8.5/§6.3/A.9: a composite-key operand is an authoring object
    /// (`{ region, code }`) that `scalar_value` yields as a bare `Value::Struct`;
    /// normalize each to the row's positional `Value::Composite` key — the same
    /// identity the `[{..}]` selector form and `==` reconcile to — before it flows to
    /// `bind_deleted`/`delete_rows`, EXACTLY as the sibling `exec_delete` statement
    /// path does. Without this the un-normalized `Value::Struct` reaches
    /// `key_value_of` (which only decomposes a `Value::Composite`) and the §21.1
    /// cascade planner (keyed by `key_identity`), so the capture comes back empty and
    /// the removal silently no-ops. A single-field key passes through unchanged
    /// (`normalize_key_operand` is a no-op for a lone `$key`); the same missing/extra
    /// component rejection guards a malformed object operand (fallible, propagated).
    fn delete_key_values(&self, keys: &Expr, decl: &[String], source: SourceId) -> Result<Vec<Value>, Rejection> {
        let current = self.current()?;
        let collection = self.collection_at(decl)?;
        let key_fields = collection.key.clone();
        let name = decl.last().cloned().unwrap_or_default();
        // §5.9/§5.4/§8.5: normalize the operand to the positional key identity, then
        // coerce an enum key component from its authoring `text` label to the
        // positioned `Value::Enum` the row is keyed under (as `exec_delete` does),
        // so a `return del { … }` addresses and captures the live row.
        let normalize = |value: Value| {
            materialize::normalize_key_operand(&key_fields, value)
                .map(|key| rules::coerce_key_operand(collection, key, &name))
        };
        Ok(match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().map(normalize).collect::<Result<_, _>>()?,
            scalar => vec![normalize(scalar)?],
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
                // §5.4/§8.4: `key` is the captured row's application-visible identity,
                // which for a composite key is the positional `Value::Composite`
                // tuple. Decompose it into the N-component `KeyValue` the row was
                // stored under (`key_value_of`); `KeyValue::single` would wrap the
                // whole tuple as one component, the lookup would miss, and the
                // delete-and-return view would come back empty. Mirrors 3fdb601.
                let address = materialize::top_address(&collection, materialize::key_value_of(key));
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
        // §5.4: an atomic rekey rewrites every inbound reference to the new key in
        // the same transition, then leaves each rewritten referencing row in the
        // touched set so its checks and reference integrity are re-validated — a
        // rewritten ref that violates a target row's check or resolves nowhere
        // rejects the complete transition. Refs target top-level collections, so a
        // top-level key change is the only one that moves an inbound target.
        if address.depth() == 1
            && let (Some(name), Some(old), Some(new)) =
                (path.first().cloned(), address.steps().last(), new_address.steps().last())
        {
            let old_key = old.key().clone();
            let new_key = new.key().clone();
            self.rewrite_inbound_refs(&name, &old_key, &new_key);
        }
        self.mark(new_address);
        Ok(())
    }

    /// Rewrite every inbound reference into top-level collection `target` whose
    /// key matches `old`, to point at `new` (§5.4), marking each rewritten row
    /// touched so the final rule pass re-validates it. The rewrite itself is the
    /// shared [`rewrite_inbound_refs_across`] — the same one a migration-internal
    /// rekey reuses (`migrate::build_migrated`), so an ordinary rekey and a
    /// migration rekey rewrite inbound references identically.
    fn rewrite_inbound_refs(&mut self, target: &str, old: &KeyValue, new: &KeyValue) {
        for address in rewrite_inbound_refs_across(self.compiled, self.prospective, target, old, new) {
            self.mark(address);
        }
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
            // §21.2: a bare `erase(row)` statement plans and applies the same live
            // removal as ordinary deletion and scrubs the retained payload; its
            // extract is discarded (only a `return erase(row)` delivers it).
            ExprKind::Call { callee, args } if is_erase(callee) => {
                self.exec_erase(args, source)?;
                Ok(())
            }
            // §8.11: a statement invoking a declared mutation (`.rename(…)`, or the
            // bare shorthand `rename({ … })`) runs it inside the same atomic
            // program. A callee that resolves no declared mutation (a bare
            // host-namespace call, an unknown call) stays a documented seam.
            ExprKind::Call { callee, args } => match self.internal_call_target(callee) {
                Some(mutation) => self.exec_internal_call(mutation, args, source),
                None => Ok(()),
            },
            ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } => self.exec_insert(lhs, rhs, source),
            ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } => self.exec_delete(lhs, rhs, source),
            // `-selection` — a prefix-minus delete of the rows a selector picks
            // (§8): `-.coll[:x | pred]` removes every matching row through the
            // same §21.1 cascade planner as a keyed delete.
            ExprKind::Unary { op: UnaryOp::Neg, operand } => self.exec_delete_selection(operand, source),
            ExprKind::Block { base, members } => self.exec_patch(base, members, source),
            // Other statement forms are CORE seams.
            _ => Ok(()),
        }
    }

    /// The declared mutation an internal-call callee names (§8.11), if any: a
    /// leading-dot `.name(…)` on the current receiver/root, or a bare `name(…)`
    /// shorthand. A `namespace.fn(…)` host call (base is a named namespace, not
    /// `.`) or an `assert` resolves no mutation and is left to its own path.
    fn internal_call_target(&self, callee: &Expr) -> Option<&'a CompiledMutation> {
        let name = match &callee.kind {
            ExprKind::Field { base, member } if matches!(base.kind, ExprKind::Current) && !member.structural => {
                &member.text
            }
            ExprKind::Name(id) => &id.text,
            _ => return None,
        };
        self.compiled.mutation(name)
    }

    /// Run a declared mutation as an internal call (§8.11): its program executes
    /// against the same prospective state, preserving the external request's
    /// `$actor`/`$session` bindings, so its writes and any rejection are the
    /// caller's — a failure inside the call rejects the caller's earlier writes
    /// too (§8.8/§22.2). Its `return` is ignored: only the outer program's
    /// trailing `return` is the call's response (§8.10).
    fn exec_internal_call(
        &mut self,
        mutation: &'a CompiledMutation,
        args: &[Arg],
        source: SourceId,
    ) -> Result<(), Rejection> {
        if self.depth >= MAX_CALL_DEPTH {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                "internal mutation calls are nested too deeply",
            ));
        }
        let receiver = self.internal_receiver(mutation)?;
        let params = self.internal_args(mutation, args, source)?;
        // §8.11: the call runs in the same atomic program. It carries the request's
        // `$actor`/`$session` (in `ctx.context`) and its own bound parameters, but
        // evaluates against the shared prospective state, so effects accumulate and
        // a rejection unwinds the whole program.
        let child_ctx = EvalCtx {
            schema: self.ctx.schema,
            compiled: self.ctx.compiled,
            params,
            now: self.ctx.now,
            seed: self.ctx.seed,
            keyrings: self.ctx.keyrings,
            placements: self.ctx.placements,
            context: self.ctx.context.clone(),
            hosts: self.ctx.hosts,
            modules: self.ctx.modules,
        };
        let mut child = Interp {
            compiled: self.compiled,
            ctx: &child_ctx,
            prospective: &mut *self.prospective,
            mutation,
            receiver,
            touched: Vec::new(),
            ret: None,
            erase_result: None,
            erase_exports: Vec::new(),
            locals: BTreeMap::new(),
            depth: self.depth + 1,
        };
        child.run()?;
        // The call's writes are the caller's: carry its touched rows so the final
        // rule pass validates them in the same transition (§22.2).
        let touched = std::mem::take(&mut child.touched);
        for address in touched {
            self.mark(address);
        }
        Ok(())
    }

    /// The receiver row of an internal mutation call (§8.11): `None` for a root
    /// mutation. A row-mutation call reuses the caller's receiver when it targets
    /// the same collection path; a key-addressed row call (`#coll.mut(key…)`) is a
    /// documented CORE seam that rejects rather than mis-targeting.
    fn internal_receiver(&self, mutation: &CompiledMutation) -> Result<Option<RowTarget>, Rejection> {
        if mutation.receiver_is_root || mutation.path.is_empty() {
            return Ok(None);
        }
        match &self.receiver {
            Some(receiver) if receiver.path == mutation.path => Ok(Some(receiver.clone())),
            _ => Err(Rejection::new(
                RejectionReason::Malformed,
                format!("internal call to row mutation `{}` cannot resolve its receiver here", mutation.name),
            )),
        }
    }

    /// Bind an internal call's parameters from its argument object (§8.11). The
    /// argument is a single object mapping parameter names to values (the shorthand
    /// `{ @id }` expands to `id: @id`); `mut()` / `mut({})` supplies none. Each
    /// value is evaluated in the caller's scope. A declared parameter takes its
    /// supplied value, else `none` for an optional one (§8.3), else a missing
    /// required argument rejects.
    fn internal_args(
        &self,
        mutation: &CompiledMutation,
        args: &[Arg],
        source: SourceId,
    ) -> Result<BTreeMap<String, Cell>, Rejection> {
        let current = self.current()?;
        let mut supplied: BTreeMap<String, Value> = BTreeMap::new();
        for arg in args {
            match arg {
                Arg::Positional(Expr { kind: ExprKind::Object(members), .. }) => {
                    for member in members {
                        if let Some((name, value)) = self.object_member(member, &current, source)? {
                            supplied.insert(name, value);
                        }
                    }
                }
                Arg::Named { name, value } => {
                    supplied.insert(name.text.clone(), self.scalar_value(value, source, &current)?);
                }
                Arg::Positional(_) => {
                    return Err(Rejection::new(
                        RejectionReason::Malformed,
                        "an internal call takes an argument object mapping parameter names to values",
                    ));
                }
            }
        }
        let mut params = BTreeMap::new();
        for (name, ty) in &mutation.params {
            let cell = match supplied.remove(name) {
                Some(value) => Cell::Scalar(value),
                None if matches!(ty.as_scalar(), Some(Type::Optional(_))) => Cell::Scalar(Value::None),
                None => {
                    return Err(Rejection::new(
                        RejectionReason::Malformed,
                        format!("internal call is missing argument `@{name}`"),
                    ));
                }
            };
            params.insert(name.clone(), cell);
        }
        Ok(params)
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
            // §8.7: `collection + { … }` inserts one row; `collection + view { … }`
            // inserts every row of a source view (a bulk insertion). A literal row
            // object is an `Object`; anything else is a source view to iterate.
            if matches!(object.kind, ExprKind::Object(_)) {
                self.insert_row(collection, object, source)?;
            } else {
                self.insert_from_view(collection, object, source)?;
            }
            return Ok(());
        }
        // §8.5: `.set_field + values` is set union — adding an existing member
        // leaves the set unchanged (a no-op that produces no state change).
        if let Some((row, field)) = self.set_field_target(collection, source)? {
            return self.set_mutate(&row, &field, object, source, true);
        }
        Ok(())
    }

    /// Insert every row of a source view into a collection (§8.7 "insert from a
    /// view"). §5.1 fixes the batch semantics: the statement builds its complete
    /// prospective row set before any row of it becomes selectable, so every
    /// inserted row's defaults observe the *pre-statement* state — two rows of one
    /// bulk insert see the same `count(/coll)`, never each other. Defaults are
    /// therefore resolved for all rows against the unchanged prospective (phase
    /// one) before any is staged (phase two).
    fn insert_from_view(&mut self, collection: &Expr, view: &Expr, source: SourceId) -> Result<(), Rejection> {
        let Some(loc) = self.collection_ref(collection, source)? else {
            return Err(Rejection::new(RejectionReason::Malformed, "insert targets a collection"));
        };
        let compiled = self.collection_at(&loc.decl)?;
        let current = self.current()?;
        let rows = match self.eval_value(view, source, &current)? {
            Cell::Collection(rows) => rows,
            Cell::Row(row) => vec![*row],
            // A scalar source is not a row set; a bulk insert takes a view.
            Cell::Scalar(_) => {
                return Err(Rejection::new(
                    RejectionReason::TypeError,
                    "a bulk insert takes a source view of rows",
                ));
            }
        };
        // Phase one: resolve each row's complete fields against the pre-statement
        // state, so no row observes a sibling of the same statement (§5.1).
        let mut staged: Vec<(RowAddress, FieldMap)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut fields = FieldMap::new();
            for (name, cell) in row.cells() {
                if let Cell::Scalar(value) = cell {
                    fields.insert(name.clone(), value.clone());
                }
            }
            // §5.1/§8.12: each row of the batch draws its own generation, so a
            // `uuid()` default is fresh per row while a state-derived default reads
            // the same pre-statement state (SPEC-ISSUES item 4).
            let generation = self.prospective.next_generation();
            rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective, generation)?;
            rules::normalize_all(compiled, &mut fields, self.ctx, self.prospective)?;
            rules::coerce_fields(compiled, &mut fields, &loc.decl.join("."))?;
            let address = self.key_address(&loc.store_path, &loc.decl, &fields)?;
            staged.push((address, fields));
        }
        // Phase two: establish every identity together, rejecting a key that
        // collides with committed state or with another row of the same batch.
        let mut seen: BTreeSet<RowAddress> = BTreeSet::new();
        for (address, _) in &staged {
            if self.prospective.contains(address) || !seen.insert(address.clone()) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "a row with this key already exists")
                    .at(address.render()));
            }
        }
        for (address, fields) in staged {
            self.prospective.insert(address.clone(), fields);
            self.mark(address);
        }
        Ok(())
    }

    /// Replace a whole collection from a source view (§8.7 `collection = view`).
    /// The replacement matches existing rows by key: a matching key keeps its
    /// incarnation and receives the normalized replacement values (an update); a
    /// new key inserts a new incarnation; an existing key absent from the
    /// replacement is deleted through ordinary §21.1 `$on_delete` planning — so a
    /// dropped `restrict`-ref target rejects the whole transition. §5.1/§8.7 fix
    /// the batch semantics: the statement builds its complete prospective row set
    /// against the pre-statement state (phase one) before any of it is staged
    /// (phase two), and the engine validates the complete resulting collection
    /// before admission.
    fn replace_collection(&mut self, loc: &CollectionLoc, view: &Expr, source: SourceId) -> Result<(), Rejection> {
        let compiled = self.collection_at(&loc.decl)?;
        let current = self.current()?;
        let rows = match self.eval_value(view, source, &current)? {
            Cell::Collection(rows) => rows,
            Cell::Row(row) => vec![*row],
            // A scalar source is not a row set; a replacement takes a view.
            Cell::Scalar(_) => {
                return Err(Rejection::new(
                    RejectionReason::TypeError,
                    "a collection replacement takes a source view of rows",
                ));
            }
        };
        // Phase one: resolve every replacement row's complete fields against the
        // pre-statement state, so no row observes a sibling of the same statement
        // (§5.1) and matching is by the fully-defaulted key (§8.7).
        let mut staged: Vec<(RowAddress, FieldMap)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut fields = FieldMap::new();
            for (name, cell) in row.cells() {
                if let Cell::Scalar(value) = cell {
                    fields.insert(name.clone(), value.clone());
                }
            }
            // §5.1/§8.12: each replacement row draws its own generation, so a
            // `uuid()` default is fresh per row (SPEC-ISSUES item 4).
            let generation = self.prospective.next_generation();
            rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective, generation)?;
            rules::normalize_all(compiled, &mut fields, self.ctx, self.prospective)?;
            rules::coerce_fields(compiled, &mut fields, &loc.decl.join("."))?;
            let address = self.key_address(&loc.store_path, &loc.decl, &fields)?;
            staged.push((address, fields));
        }
        // A replacement view supplying two rows of one key cannot form one
        // prospective row set (§8.7): reject rather than silently collapse them.
        let mut replacement: BTreeSet<RowAddress> = BTreeSet::new();
        for (address, _) in &staged {
            if !replacement.insert(address.clone()) {
                return Err(Rejection::new(
                    RejectionReason::DuplicateKey,
                    "the replacement view supplies two rows with the same key",
                )
                .at(address.render()));
            }
        }
        // The collection's existing rows before replacement (§8.7 matches by key).
        let existing = self.prospective.addresses_in(&loc.store_path);
        // Phase two: stage every replacement row — a matching key updates in place
        // (the diff keeps its incarnation), a new key inserts (§8.7). The final
        // rule pass validates the complete resulting collection (§8.7).
        for (address, fields) in staged {
            self.prospective.insert(address.clone(), fields);
            self.mark(address);
        }
        // §8.7: every existing key absent from the replacement is dropped.
        let dropped: Vec<RowAddress> =
            existing.into_iter().filter(|address| !replacement.contains(address)).collect();
        if dropped.is_empty() {
            return Ok(());
        }
        // §5.4/§21.1: the cascade planner operates over the top-level graph; a
        // dropped nested-collection row (no inbound refs in CORE scope) is removed
        // directly with its subtree, a dropped top-level row through the planner so
        // its inbound `$on_delete` policies (restrict/cascade/clear/patch) apply.
        if loc.decl.len() > 1 {
            for address in dropped {
                self.remove_subtree(&address);
            }
            return Ok(());
        }
        let name = loc.decl.last().cloned().unwrap_or_default();
        let model = self.ctx.schema.top_collection(&name);
        let initial: Vec<RowRef> = dropped
            .iter()
            .filter_map(|address| {
                let step = address.steps().last()?;
                let key = match model {
                    Some(model) => materialize::key_identity(model, step.key()),
                    None => step.key().components().next()?.clone(),
                };
                Some(RowRef::new(name.clone(), key))
            })
            .collect();
        self.delete_rows(initial)
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
    ///
    /// Each operand member is decoded to the set's ELEMENT type before the
    /// union/difference (§5.5/§5.6/A.9): a `$set` of `$ref` stores every member as
    /// the `Value::Ref` carrier `$data` seeding produces and the §21.1 cascade
    /// planner walks, while a mutation operand is the member's application-visible
    /// target key (`Value::Text`, or a composite tuple). Because `Value`'s total
    /// order discriminates variants (B.1), a raw-text operand would never equal a
    /// stored `Ref` — a remove-by-key would silently no-op, an add would store a
    /// planner-invisible pseudo-member (a dangling ref past deletion), and re-adding
    /// an existing member would duplicate its identity. Scalar-element sets already
    /// match the stored form, so they pass through unchanged.
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
        let element_target = self
            .compiled
            .collection_at(&row.path)
            .and_then(|c| c.field(field))
            .and_then(|f| f.element_reference.as_ref())
            .map(|info| info.target.clone());
        let mut members: BTreeSet<Value> = match self.prospective.get(&row.address).and_then(|f| f.get(field)) {
            Some(Value::Set(existing)) => existing.clone(),
            _ => BTreeSet::new(),
        };
        for member in incoming {
            // §5.5 / A.1: `none` is absence, never a set member. Adding `none` is a
            // no-op that yields the same set, and `none` is never present to remove —
            // so a `none` operand is a no-op in BOTH directions. Skipping it leaves the
            // set byte-for-byte unchanged, which `prospective.diff()` reports as
            // `Unchanged` when it was the only change.
            if matches!(member, Value::None) {
                continue;
            }
            let member = match &element_target {
                Some(target) => self.ref_member(target, member)?,
                None => member,
            };
            if add {
                members.insert(member);
            } else {
                members.remove(&member);
            }
        }
        self.write_field(row, field, Value::Set(members))
    }

    /// Decode a `$set`-of-`$ref` mutation operand into the `Value::Ref` carrier its
    /// members are stored and compared as (§5.5/§5.6/A.9). A member already carried
    /// as a `Ref` (the set-valued operand of a set-to-set mutation, or a `none`)
    /// passes through; a bare application-visible key is normalized to its
    /// `$key`-order identity and wrapped as the collection's uniform ref shape
    /// through the same `refid::ref_of` construction `$data` decode and inbound-ref
    /// rewrite use — never a second, divergent hand-rolled carrier.
    fn ref_member(&self, target: &str, operand: Value) -> Result<Value, Rejection> {
        if matches!(operand, Value::Ref(_) | Value::None) {
            return Ok(operand);
        }
        let Some(names) = self.compiled.collection(target).map(|c| c.key.clone()) else {
            return Ok(operand);
        };
        let normalized = materialize::normalize_key_operand(&names, operand)?;
        let components: Vec<Value> =
            materialize::key_value_of(&normalized).components().cloned().collect();
        Ok(Value::Ref(crate::refid::ref_of(&names, &components)))
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
        // §5.1/§8.12: this inserted row draws its own generation, so a `uuid()`
        // default here differs from the one another `+` statement of the same
        // request produces (SPEC-ISSUES item 4).
        let generation = self.prospective.next_generation();
        rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective, generation)?;
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
            // §5.1/§8.12: each nested-initializer row draws its own generation, so
            // a `uuid()` default is fresh per row (SPEC-ISSUES item 4).
            let generation = self.prospective.next_generation();
            rules::apply_defaults(compiled, &mut fields, self.ctx, self.prospective, generation)?;
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
        // §5.5/§22.1: complete the struct's declared shape after its explicit member
        // defaults — an omitted non-optional `set`/`map` member starts as the empty
        // container (the "row OR struct" default, the same absent-container fill
        // `apply_defaults` runs for a row's collection fields), recursing into nested
        // static structs. An omitted optional member stays absent (A.1). Runs before
        // the struct `$check` so the check sees the completed shape (§5.10).
        rules::complete_struct_containers(&mut fields, &struct_meta.fields);
        // §5.3/§5.10/§8.8: the struct `$check` is NOT enforced here — a struct check
        // reading the containing row via `^` (§6.2) needs the parent scope frame,
        // which the row is not yet in when its members are being built. It is
        // enforced over the completed prospective row in the final rule pass
        // (`rules::check_structs`), where `.` is the struct and `^` the containing
        // row, matching the read/view path fdc7639 established.
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
        // §8.5/§6.3/A.9: a composite-key delete operand is an authoring object
        // (`{ region, code }`) that evaluates to a bare `Value::Struct`; normalize
        // it to the row's positional `Value::Composite` key — the same identity the
        // `[{..}]` selector form and `==` reconcile to — before addressing rows, or
        // the cascade planner (keyed by `key_identity`) matches nothing and the
        // delete silently no-ops.
        let key_fields = self.collection_at(&loc.decl)?.key.clone();
        let normalize = |value: Value| materialize::normalize_key_operand(&key_fields, value);
        let targets: Vec<Value> = match self.scalar_value(keys, source, &current)? {
            Value::Set(members) => members.into_iter().map(normalize).collect::<Result<_, _>>()?,
            scalar => vec![normalize(scalar)?],
        };
        // §5.9/§5.4/§8.5: coerce an enum key component from its authoring `text`
        // label to the positioned `Value::Enum` the row is keyed under, so the
        // delete addresses the live row rather than no-opping on a `text`↔`enum`
        // mismatch. A non-enum key passes through unchanged.
        let collection = self.collection_at(&loc.decl)?;
        let targets: Vec<Value> =
            targets.into_iter().map(|key| rules::coerce_key_operand(collection, key, &name)).collect();
        // §5.4/§21.1: the cascade planner operates over the top-level graph; a
        // nested collection's row (a meter spend/pool, §15) has no inbound refs in
        // CORE scope, so it is removed directly with its descendant subtree.
        if loc.decl.len() > 1 {
            for key in targets {
                // §5.4/B.4: `key` is the application-visible identity, which for a
                // composite key is the positional `Value::Composite` tuple. Route it
                // through `key_value_of` so it decomposes into the N-component
                // `KeyValue` the row was stored under (`materialize::row_key`);
                // `KeyValue::single` would wrap the whole tuple as one component and
                // address a non-existent one-component row, so `remove_subtree`'s
                // `contains` guard would miss and the delete would silently no-op.
                // A single-field key passes through as a lone component unchanged.
                // Mirrors the 3fdb601 fix on the sibling `exec_erase` path.
                self.remove_subtree(&loc.store_path.row(materialize::key_value_of(&key)));
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

    /// Execute the `erase(row)` builtin (§21.2): plan and apply the *same* live
    /// removal an ordinary deletion would (step 1, `$on_delete` cascades/patches
    /// included), capturing each targeted row's retained payload before it is
    /// removed, then scrub each captured payload to a digest stub and return the
    /// durable extract (steps 2–6). Because the removal flows through ordinary
    /// admission, the erased row is then unobservable in live views and absent from
    /// a fresh export; the returned extract carries only the content hash, never the
    /// scrubbed bytes, so an erase response never re-leaks what it scrubbed.
    ///
    /// Erasure is scoped to top-level collection rows in CORE, like keyed deletion
    /// (§21.1); a nested-collection erasure is a documented seam.
    fn exec_erase(&mut self, args: &[Arg], source: SourceId) -> Result<Value, Rejection> {
        let Some(arg) = args.first() else {
            return Err(Rejection::new(RejectionReason::Malformed, "`erase` requires a row selector"));
        };
        let selector = arg_expr(arg);
        let base = match &selector.kind {
            ExprKind::Select { base, .. } => base.as_ref(),
            _ => selector,
        };
        let Some(loc) = self.collection_ref(base, source)? else {
            return Err(Rejection::new(RejectionReason::Malformed, "`erase` targets a top-level collection row"));
        };
        let name = loc.decl.last().cloned().unwrap_or_default();
        let current = self.current()?;
        let keys: Vec<Value> = match self.eval_value(selector, source, &current)? {
            Cell::Collection(rows) => rows.iter().map(|row| row.key().clone()).collect(),
            Cell::Row(row) => vec![row.key().clone()],
            Cell::Scalar(scalar) => vec![scalar],
        };
        // §21.2 step 1: plan the SAME live removal an ordinary deletion would — the
        // §21.1 delete-closure (the direct targets plus every `cascade` row expanded
        // to a fixed point). Planned before any effect so the closure is computed
        // from the pre-erase state.
        let initial: Vec<RowRef> = keys.into_iter().map(|key| RowRef::new(name.clone(), key)).collect();
        let planned = cascade::plan(self.compiled, self.ctx, self.prospective, &initial)?;
        // §21.2 step 2: capture the retained payload of EVERY row in the delete-closure
        // (not just the direct targets) into the reintegration bundle, under a stable
        // occurrence identity. Erasure differs from deletion only in history: a
        // cascade-deleted row is scrubbed on the same footing as the direct target,
        // so its payload MUST be exported too, before the removal is applied. A
        // surviving-but-patched row is NOT in the closure and keeps its history.
        let mut history = Erasure::new();
        let mut occurrences = Vec::new();
        for row_ref in planned.plan.deletes() {
            let Some(address) = planned.addresses.get(row_ref) else { continue };
            let Some(fields) = self.prospective.get(address) else { continue };
            let payload = materialize::struct_of(fields);
            let occurrence = Occurrence::new(occurrence_id(row_ref, &payload));
            history.record(occurrence.clone(), payload);
            occurrences.push(occurrence);
        }
        // §21.2 step 1 (apply): the planned §21.1 live removal, atomically.
        self.apply_deletion(&planned)?;
        // §21.2 steps 3–6: scrub every captured closure payload to a digest stub and
        // produce the durable extract — the portable reintegration bundle. Capturing
        // it is a commit precondition and fail-closed: a payload that cannot be
        // captured rejects the whole erasure (a scrubbed byte is never left
        // unrecoverable, §21.2). The bundle is retained on the interpreter's export
        // sink so a bare `erase(row)` statement captures it too, not only a
        // `return erase(row)`.
        let extract = history
            .erase(&occurrences)
            .map_err(|error| Rejection::new(RejectionReason::Evaluation, error.to_string()))?;
        let response = extract_response(&extract);
        self.erase_exports.push(response.clone());
        Ok(response)
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
        // §5.6/§21.1: drop each removed set member from its surviving referencing
        // row so a `cascade` on a `$set`-of-`$ref` member leaves no dangling
        // membership at the deleted target (§22.1). The row keeps its identity; the
        // set stays canonical after the removal, so re-placing it (which marks it
        // touched for the finalize integrity pass) needs no re-normalization.
        for (row, field_removals) in planned.plan.member_removals() {
            let Some(address) = planned.addresses.get(row) else { continue };
            let Some(mut fields) = self.prospective.get(address).cloned() else { continue };
            for (field, members) in field_removals {
                if let Some(Value::Set(set)) = fields.get_mut(field) {
                    for member in members {
                        set.remove(member);
                    }
                }
            }
            self.place(address, std::slice::from_ref(&row.collection), fields)?;
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
                    // §5.4/§8.9: `key` is a row's application-visible identity, a
                    // positional `Value::Composite` for a composite key; route it
                    // through `key_value_of` so a bulk patch over a composite-keyed
                    // collection addresses the stored N-component row rather than a
                    // never-matching one-component `KeyValue::single`.
                    address: loc.store_path.row(materialize::key_value_of(&key)),
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
            // A composite-keyed collection selected by a non-struct value: when that
            // value is the positional `Value::Composite` tuple (another row's `$key`
            // identity), decompose it into the stored N-component key rather than
            // wrapping the whole tuple as one component (§5.4). A plain scalar (a
            // malformed composite selector) stays single and fails closed at lookup.
            (_, scalar) => Ok(materialize::key_value_of(&scalar)),
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

/// Whether a call names the `erase(row)` builtin (§21.2): a bare `erase(...)`.
fn is_erase(callee: &Expr) -> bool {
    matches!(&callee.kind, ExprKind::Name(id) if id.text == "erase")
}

/// A stable occurrence identity for one scrubbed closure row (§21.2 step 2): its
/// collection and key identity, plus its payload's canonical wire form. Keying on
/// the row identity — not the payload alone — keeps two distinct closure rows that
/// happen to project equal payloads separate occurrences, so the reintegration
/// bundle covers the whole delete-closure without collision.
fn occurrence_id(row: &RowRef, payload: &Value) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        row.collection,
        row.key.to_canonical_json_string(),
        payload.to_canonical_json_string()
    )
}

/// The response value an `erase(row)` returns (§21.2 step 6): the extract's
/// durable content hash (§21.3). Only the hash crosses the response boundary —
/// never the scrubbed payloads — so identifying an extract for a later
/// reinsertion cannot re-leak the bytes the erasure removed.
fn extract_response(extract: &Extract) -> Value {
    Value::Text(Text::new(extract.hash().to_owned()))
}

/// The value expression of a call argument, positional or named (§16.4 host-call
/// arguments carry no keyword semantics in CORE — the name is decorative).
fn arg_expr(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) | Arg::Named { value, .. } => value,
    }
}

/// The keyring name a `cose.sign` path argument `/ring` addresses (§17.7): a root
/// field access `Field { base: Root, member }`. Any other shape is not a keyring
/// path.
fn keyring_ref(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Field { base, member }
            if matches!(base.kind, ExprKind::Root) && !member.structural =>
        {
            Some(&member.text)
        }
        _ => None,
    }
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

/// Rewrite every inbound reference into top-level collection `target` whose key
/// matches `old`, to point at `new` (§5.4), returning the addresses of the rows
/// it rewrote so a caller can re-validate them — an ordinary rekey marks each
/// touched, a migration lets the final §20.1 pass cover them.
///
/// Matching is by *application identity* (`refid::ref_identity`), not raw carrier
/// equality: a composite ref exposes its `$key`-order tuple as a
/// [`Value::Composite`], the same value the target row's identity produces, so the
/// two compare equal. The rewritten value keeps its stored carrier — a `Ref`
/// becomes the collection's uniform ref shape at the new key (`refid::ref_of`: a
/// scalar-keyed ref for a single-field key, a positional composite-keyed ref for a
/// composite key), a bare stored key (§6.3 ref/key equality) becomes the bare new
/// identity.
pub(crate) fn rewrite_inbound_refs_across(
    compiled: &Compiled,
    prospective: &mut Prospective,
    target: &str,
    old: &KeyValue,
    new: &KeyValue,
) -> Vec<RowAddress> {
    let Some(names) = compiled.collection(target).map(|c| c.key.clone()) else { return Vec::new() };
    let old_id = identity_of(&names, &old.components().cloned().collect::<Vec<_>>());
    let new_components: Vec<Value> = new.components().cloned().collect();
    let new_id = identity_of(&names, &new_components);
    let new_ref = crate::refid::ref_of(&names, &new_components);
    let candidates: Vec<RowAddress> = prospective.working().keys().cloned().collect();
    let mut rewritten = Vec::new();
    for address in candidates {
        let decl: Vec<String> = address.steps().map(|s| s.name().as_str().to_owned()).collect();
        let Some(collection) = compiled.collection_at(&decl) else { continue };
        let Some(existing) = prospective.get(&address) else { continue };
        let mut fields = existing.clone();
        let mut changed = false;
        for field in &collection.fields {
            // A scalar `$ref` field: rewrite its single value to the new key.
            if let Some(info) = &field.reference
                && info.target == target
                && let Some(rewrite) =
                    rewrite_ref_value(fields.get(&field.name), &names, &old_id, &new_id, &new_ref)
            {
                fields.insert(field.name.clone(), rewrite);
                changed = true;
            }
            // §5.5/§5.4: a `$set` of `$ref` holds many inbound references — rewrite
            // every member that targeted the rekeyed row, preserving the rest of the
            // membership.
            if let Some(info) = &field.element_reference
                && info.target == target
                && let Some(Value::Set(members)) = fields.get(&field.name)
            {
                let mut rebuilt = BTreeSet::new();
                let mut member_changed = false;
                for member in members {
                    match rewrite_ref_value(Some(member), &names, &old_id, &new_id, &new_ref) {
                        Some(rewrite) => {
                            rebuilt.insert(rewrite);
                            member_changed = true;
                        }
                        None => {
                            rebuilt.insert(member.clone());
                        }
                    }
                }
                if member_changed {
                    fields.insert(field.name.clone(), Value::Set(rebuilt));
                    changed = true;
                }
            }
        }
        if changed {
            prospective.replace(&address, fields);
            rewritten.push(address);
        }
    }
    rewritten
}

/// If `value` is an inbound reference to the row whose application identity is
/// `old`, the value it becomes when that target is rekeyed to identity `new`
/// (§5.4) — preserving the stored carrier: a `Ref` becomes `new_ref`, a bare
/// stored key (§6.3 ref/key equality) becomes the bare `new` identity. Matching
/// is by application identity (`refid::ref_identity`) so a composite ref carried
/// as `Ref::scalar(struct)` is recognized. `None` when `value` does not
/// reference `old`.
fn rewrite_ref_value(
    value: Option<&Value>,
    names: &[String],
    old: &Value,
    new: &Value,
    new_ref: &Ref,
) -> Option<Value> {
    match value {
        Some(Value::Ref(reference)) if &ref_identity(names, reference.key()) == old => {
            Some(Value::Ref(new_ref.clone()))
        }
        Some(bare) if bare == old => Some(new.clone()),
        _ => None,
    }
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
