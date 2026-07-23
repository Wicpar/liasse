//! The type checker: turns a spanned AST ([`Expr`]) plus a [`Scope`] into a
//! [`TypedExpr`] or a bundle of [`Diagnostics`].
//!
//! Inference follows §6/§8.3 where the spec pins it and the documented
//! least-surprising choice elsewhere (arithmetic over an optional operand is a
//! static type error, SPEC-ISSUES item 3). The checker recurses structurally on
//! the AST, bounded by liasse-syntax's nesting cap (see [`crate::typed`]) —
//! except the projection-output dependency DFS in [`walk`], which recurses on
//! output-name edges and carries its own bound: the projection's output count.

mod blob;
mod keyring;
mod ops;
mod project;
mod temporal;
mod views;
mod walk;

use std::collections::BTreeMap;

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};
use liasse_syntax::{Expr, ExprKind, SpannedExpression, Stmt, StmtKind};
use liasse_value::{Decimal, Integer, Text, Type, Value};

use crate::scope::Scope;
use crate::ty::{ExprType, RowType};
use crate::typed::{TypedExpr, TypedKind};

/// The diagnostic code carried by a §16.3 effect-class / §16.5 namespace-origin
/// host-position violation, distinct from the generic `E-EXPR` type error. It lets
/// the load-time authenticator / role-view audit ([`audit_host_position`]) surface
/// exactly these position-policy diagnostics while discarding the type/name
/// diagnostics an auth/actor seam defers to admission.
pub(crate) const HOST_POSITION_CODE: &str = "E-HOST";

/// Type-check one value/view statement (`return e` or a bare expression),
/// yielding a [`TypedExpr`] or the diagnostics that reject it.
///
/// Mutation statement forms (`target = value`, `field -`) belong to the
/// mutation layer, not this crate; they are rejected here with a diagnostic.
pub fn check_statement(
    scope: &dyn Scope,
    source: SourceId,
    statement: &SpannedExpression,
) -> Result<TypedExpr, Diagnostics> {
    let stmt = statement.statement();
    match &stmt.kind {
        StmtKind::Bare(expr) | StmtKind::Return(expr) => check_expression(scope, source, expr),
        StmtKind::Assign { .. } | StmtKind::Clear(_) => {
            Err(single(reject(source, stmt, "a mutation statement is not a value or view expression")))
        }
    }
}

/// Type-check one value/view expression against `scope`.
pub fn check_expression(
    scope: &dyn Scope,
    source: SourceId,
    expr: &Expr,
) -> Result<TypedExpr, Diagnostics> {
    let mut checker = Checker::new(scope, source);
    match checker.check(expr) {
        Some(typed) if !checker.diags.has_errors() => {
            // §14.5: this is the TERMINAL enumeration guard. A projection/filter is
            // transparent to the unbounded-recurring marker (it propagates the flag
            // rather than rejecting eagerly, `check_block`/`check_select`), and a
            // bounded temporal selector clears it (`check_temporal_call`). If the
            // whole expression STILL denotes an unbounded recurring view here, no
            // bounding selector ever gated it, so reading it whole enumerates a
            // possibly-infinite series — the read §14.5 forbids. (A one-row `Row`
            // result names finite rows, not a whole-series read, so only a `View`
            // is rejected; an aggregate that consumes such a view into a scalar is
            // guarded at `check_aggregate`.)
            if matches!(typed.ty(), ExprType::View(row) if row.is_unbounded()) {
                checker.report(
                    expr,
                    "this view enumerates an unbounded recurring bucket; read it through a bounded temporal selector `.$at`/`.$between` (§14.5)",
                );
                return Err(checker.diags);
            }
            Ok(typed)
        }
        _ => Err(checker.diags),
    }
}

/// Audit an authenticator (§11.3 `$verify`/`$actor`/`$session`/`$check`) or a
/// role membership/`$view` (§10.3) expression for the §16.3 effect-class and
/// §16.5 namespace-origin host-position policy ONLY.
///
/// These positions are database-evaluated, so an app-registered, verifier, or
/// generated host call in them is a load-time error. Their *full* typing, though,
/// is a documented auth/actor seam: an authenticator reads `$proof`/`$credential`
/// and selects `$actor`/`$session` rows the request supplies, native keyring
/// verification (§17.7) is dispatched specially rather than as a value host op,
/// and a lenient load defers an app verifier namespace. So this runs the checker
/// but returns ONLY the host-position diagnostics it raises, discarding every
/// name/type diagnostic the seam legitimately owns. `Ok(())` when the expression
/// calls no app-registered / non-pure host function in this position — even when
/// it is not otherwise fully typeable at load. The `scope`'s [`host_position`] and
/// resolved [`namespace_op`]s decide the policy, exactly as full typing would.
///
/// [`host_position`]: Scope::host_position
/// [`namespace_op`]: Scope::namespace_op
pub fn audit_host_position(
    scope: &dyn Scope,
    source: SourceId,
    statement: &SpannedExpression,
) -> Result<(), Diagnostics> {
    let stmt = statement.statement();
    let expr = match &stmt.kind {
        StmtKind::Bare(expr) | StmtKind::Return(expr) => expr,
        // A mutation statement in a read position is a shape error the model
        // already rejects; this audit concerns value expressions only.
        StmtKind::Assign { .. } | StmtKind::Clear(_) => return Ok(()),
    };
    let mut checker = Checker::new(scope, source);
    let _ = checker.check(expr);
    let host_only: Diagnostics = checker
        .diags
        .iter()
        .filter(|diag| diag.code().is_some_and(|code| code.as_str() == HOST_POSITION_CODE))
        .cloned()
        .collect();
    if host_only.has_errors() {
        Err(host_only)
    } else {
        Ok(())
    }
}

/// Validate the object operand of a `collection - { object }` delete against the
/// target collection's composite key at load (§6.3/§8.5/A.9).
///
/// This is the direct-delete position of the ONE composite-key coercion the
/// `[{..}]` selector, `==`, and `in` already apply through
/// [`Checker::coerce_composite_key`]: the `collection` base is type-checked to
/// recover its key type, and only when that key is composite and the `operand` is
/// an authoring object is the object required to be a *key of the target* — naming
/// every `$key` component with its declared type and carrying no extra field
/// (A.9). A non-conforming object is rejected with the same `E-EXPR` type error
/// the peer positions emit, before the delete can activate.
///
/// A scalar-keyed target, a bare parameter key operand, a set operand, a
/// positional composite (another row's `$key`), or a base that is not a keyed
/// view is left untouched: this gate only rejects a non-conforming composite
/// object, matching the other positions exactly rather than over-rejecting the
/// forms whose key the runtime carrier owns.
pub fn check_composite_delete_operand(
    scope: &dyn Scope,
    source: SourceId,
    collection: &Expr,
    operand: &Expr,
) -> Result<(), Diagnostics> {
    // Recover the target collection's composite key type, if any. The collection
    // reference and the remainder of the delete belong to the mutation layer and
    // the runtime; a base that is not a composite-keyed view has no object-operand
    // form to gate here, so the delete is left untouched.
    let Some(key) = composite_key_of(scope, source, collection) else {
        return Ok(());
    };
    let mut checker = Checker::new(scope, source);
    let Some(operand) = checker.check(operand) else {
        // An operand that does not type on its own is a pre-existing structural
        // seam of the mutation phase, not this composite-key gate's concern.
        return Ok(());
    };
    // Route through the single validate-and-normalize point; the normalized
    // carrier is discarded (the runtime rebuilds it) — only a conformance
    // rejection is meaningful at load.
    checker.coerce_composite_key(operand, Some(&key));
    if checker.diags.has_errors() {
        Err(checker.diags)
    } else {
        Ok(())
    }
}

/// The composite `$key` type of the view `collection` names, if it is a keyed
/// view with a composite key. Diagnostics from resolving the base are discarded:
/// the base belongs to the mutation/runtime layers, and this helper only reports
/// whether a composite-key object operand needs gating.
fn composite_key_of(scope: &dyn Scope, source: SourceId, collection: &Expr) -> Option<ExprType> {
    let mut checker = Checker::new(scope, source);
    let key = match checker.check(collection)?.ty() {
        ExprType::View(row) => row.key().cloned(),
        _ => None,
    }?;
    matches!(key.as_scalar(), Some(Type::Composite(_))).then_some(key)
}

/// One lexical frame introduced *within* an expression (a filter row, a `::`
/// traversal, a projection source row, or an accumulating projection output).
pub(crate) struct Frame {
    pub(crate) current: ExprType,
    pub(crate) bindings: BTreeMap<String, ExprType>,
}

/// The checker: a base scope, the diagnostics under construction, and the stack
/// of expression-internal frames.
pub(crate) struct Checker<'a> {
    scope: &'a dyn Scope,
    source: SourceId,
    diags: Diagnostics,
    frames: Vec<Frame>,
}

impl<'a> Checker<'a> {
    fn new(scope: &'a dyn Scope, source: SourceId) -> Self {
        Self {
            scope,
            source,
            diags: Diagnostics::new(),
            frames: Vec::new(),
        }
    }

    /// Record a type error and yield `None`, so the caller aborts this subtree.
    pub(crate) fn error(&mut self, expr: &Expr, message: impl Into<String>) -> Option<TypedExpr> {
        self.report(expr, message);
        None
    }

    /// Record a type error without a typed result, for callers whose `None`
    /// carries a different type.
    pub(crate) fn report(&mut self, expr: &Expr, message: impl Into<String>) {
        self.report_span(expr.span, message);
    }

    /// Record a §16.3/§16.5 host-position policy violation under the dedicated
    /// [`HOST_POSITION_CODE`] and yield `None`. The distinct code lets the
    /// load-time authenticator / role-view audit ([`audit_host_position`]) pick out
    /// exactly these policy diagnostics from the type/name diagnostics an auth/actor
    /// seam defers to admission.
    pub(crate) fn host_position_error(
        &mut self,
        expr: &Expr,
        message: impl Into<String>,
    ) -> Option<TypedExpr> {
        self.diags.push(
            Diagnostic::error(message.into())
                .code(HOST_POSITION_CODE)
                .primary(Span::new(self.source, expr.span), "in this expression")
                .build(),
        );
        None
    }

    /// Record a type error at an explicit byte span — used where the offending
    /// operand is an already-typed sub-expression (carrying its own
    /// [`TypedExpr::span`](crate::typed::TypedExpr::span)) rather than a raw AST
    /// node.
    pub(crate) fn report_span(&mut self, span: ByteSpan, message: impl Into<String>) {
        self.diags.push(
            Diagnostic::error(message.into())
                .code("E-EXPR")
                .primary(Span::new(self.source, span), "in this expression")
                .build(),
        );
    }

    /// The current `.` type, walking local frames then the base scope.
    pub(crate) fn current_at(&self, depth: u32) -> Option<ExprType> {
        let frames = self.frames.len();
        let depth = depth as usize;
        if depth < frames {
            return frames
                .checked_sub(1 + depth)
                .and_then(|idx| self.frames.get(idx))
                .map(|frame| frame.current.clone());
        }
        match depth - frames {
            0 => self.scope.current(),
            up => self.scope.parent(up as u32),
        }
    }

    /// Resolve a bare name within the current scope chain (§6.2, §7.1).
    ///
    /// The innermost frame is the current projection/filter scope. Its own
    /// bindings win first — an earlier same-projection output (§7.1
    /// cross-reference) or a copied `[:name]`/`::` bind (§6.4). Then the current
    /// row's own field, so a *nested* projection reads its own row's field rather
    /// than an enclosing projection's like-named output (`... { id, xs: .ys { id }
    /// }` reads each `y`'s `id`, not the outer row's). Only after both fall through
    /// do enclosing frames and the base scope's lexical bindings apply; an
    /// enclosing projection's output is reached, if at all, through an explicit
    /// `^` parent step, never by a bare name shadowing a local field.
    fn resolve_name(&self, name: &str) -> Option<NameResolution> {
        if let Some(frame) = self.frames.last()
            && let Some(ty) = frame.bindings.get(name)
        {
            return Some(NameResolution::Frame(ty.clone()));
        }
        if let Some(ExprType::Row(row)) = self.current_at(0)
            && let Some(ty) = row.field(name)
        {
            return Some(NameResolution::Field(ty.clone()));
        }
        for frame in self.frames.iter().rev().skip(1) {
            if let Some(ty) = frame.bindings.get(name) {
                return Some(NameResolution::Frame(ty.clone()));
            }
        }
        if let Some(ty) = self.scope.binding(name) {
            return Some(NameResolution::Scope(ty));
        }
        None
    }

    pub(crate) fn push_frame(&mut self, current: ExprType) {
        self.frames.push(Frame {
            current,
            bindings: BTreeMap::new(),
        });
    }

    pub(crate) fn pop_frame(&mut self) {
        self.frames.pop();
    }

    pub(crate) fn bind(&mut self, name: String, ty: ExprType) {
        if let Some(frame) = self.frames.last_mut() {
            frame.bindings.insert(name, ty);
        }
    }

    /// Type-check one node.
    pub(crate) fn check(&mut self, expr: &Expr) -> Option<TypedExpr> {
        match &expr.kind {
            ExprKind::None => Some(self.literal(expr, none_type(), Value::None)),
            ExprKind::Bool(value) => {
                Some(self.literal(expr, ExprType::scalar(Type::Bool), Value::Bool(*value)))
            }
            ExprKind::Int(text) => self.int_literal(expr, text),
            ExprKind::Decimal(text) => self.decimal_literal(expr, text),
            ExprKind::Str(text) => Some(self.literal(
                expr,
                ExprType::scalar(Type::Text),
                Value::Text(Text::new(text.clone())),
            )),
            ExprKind::List(items) => self.check_list(expr, items),
            ExprKind::Object(members) => self.check_object(expr, members),
            ExprKind::Root => self.check_root(expr),
            ExprKind::Current => self.check_current(expr),
            ExprKind::Parent(depth) => self.check_parent(expr, *depth),
            ExprKind::Import(name) => self.check_import(expr, &name.text),
            ExprKind::Param(name) => self.check_param(expr, &name.text),
            ExprKind::Structural(name) => self.check_structural(expr, &name.text),
            ExprKind::Name(name) => self.check_name(expr, &name.text),
            ExprKind::Field { base, member } if member.structural => {
                self.check_structural_selector(expr, base, &member.text)
            }
            ExprKind::Field { base, member } => self.check_field(expr, base, &member.text),
            ExprKind::SameName { base, member } => self.check_traverse(expr, base, &member.text),
            ExprKind::Select { base, selector } => self.check_select(expr, base, selector),
            ExprKind::Call { callee, args } => self.check_call(expr, callee, args),
            ExprKind::Block { base, members } => self.check_block(expr, base, members),
            ExprKind::Unary { op, operand } => self.check_unary(expr, *op, operand),
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(expr, *op, lhs, rhs),
            ExprKind::Ternary { cond, then, otherwise } => {
                self.check_ternary(expr, cond, then, otherwise)
            }
            ExprKind::Combination { operands, operators } => {
                self.check_combination(expr, operands, operators)
            }
        }
    }

    fn literal(&self, expr: &Expr, ty: ExprType, value: Value) -> TypedExpr {
        TypedExpr::new(expr.span, ty, TypedKind::Literal(value))
    }

    fn int_literal(&mut self, expr: &Expr, text: &str) -> Option<TypedExpr> {
        match Integer::parse(text) {
            Ok(value) => Some(self.literal(expr, ExprType::scalar(Type::Int), Value::Int(value))),
            Err(err) => self.error(expr, err.to_string()),
        }
    }

    fn decimal_literal(&mut self, expr: &Expr, text: &str) -> Option<TypedExpr> {
        match Decimal::parse(text) {
            Ok(value) => {
                Some(self.literal(expr, ExprType::scalar(Type::Decimal), Value::Decimal(value)))
            }
            Err(err) => self.error(expr, err.to_string()),
        }
    }

    fn check_root(&mut self, expr: &Expr) -> Option<TypedExpr> {
        match self.scope.root() {
            Some(ty) => Some(TypedExpr::new(expr.span, ty, TypedKind::Root)),
            None => self.error(expr, "package root `/` is not available in this scope"),
        }
    }

    fn check_current(&mut self, expr: &Expr) -> Option<TypedExpr> {
        match self.current_at(0) {
            Some(ty) => Some(TypedExpr::new(expr.span, ty, TypedKind::Current)),
            None => self.error(expr, "`.` has no current value in this scope"),
        }
    }

    fn check_parent(&mut self, expr: &Expr, depth: u32) -> Option<TypedExpr> {
        match self.current_at(depth) {
            Some(ty) => Some(TypedExpr::new(expr.span, ty, TypedKind::Parent(depth))),
            None => self.error(expr, "no lexical parent scope at this depth"),
        }
    }

    fn check_param(&mut self, expr: &Expr, name: &str) -> Option<TypedExpr> {
        match self.scope.param(name) {
            Some(ty) => Some(TypedExpr::new(expr.span, ty, TypedKind::Param(name.to_owned()))),
            None => self.error(expr, format!("unknown parameter `@{name}`")),
        }
    }

    fn check_structural(&mut self, expr: &Expr, name: &str) -> Option<TypedExpr> {
        // A structural binding resolves from the base scope's feature context —
        // `$actor`/`$session`/…, a module package's `$config` struct (§13.1, a
        // package-level binding whose members `$config.member` then read as
        // ordinary fields of the returned row), and a bucket declaration's own
        // `$source`/`$from` — or, inside a projection over a bucketed view, from
        // the current row's structural bindings (`$index`/`$from`/`$until`/
        // `$source`, §14.4).
        match self.scope.structural(name).or_else(|| self.row_structural(name)) {
            Some(ty) => Some(TypedExpr::new(
                expr.span,
                ty,
                TypedKind::Structural(name.to_owned()),
            )),
            None => self.error(
                expr,
                format!("structural binding `${name}` is not available in this context"),
            ),
        }
    }

    /// A structural binding exposed by the current row's shape (§14.4): a
    /// projection over a bucketed view reads `$from`/`$until`/`$index`/`$source`
    /// off the row it maps. Innermost frame wins, matching lexical `.` scope.
    fn row_structural(&self, name: &str) -> Option<ExprType> {
        for frame in self.frames.iter().rev() {
            if let ExprType::Row(row) | ExprType::View(row) = &frame.current
                && let Some(ty) = row.structural(name)
            {
                return Some(ty.clone());
            }
        }
        None
    }

    fn check_import(&mut self, expr: &Expr, name: &str) -> Option<TypedExpr> {
        match self.scope.import(name) {
            Some(ty) => Some(TypedExpr::new(expr.span, ty, TypedKind::Import(name.to_owned()))),
            None => self.error(expr, format!("unknown import `#{name}`")),
        }
    }

    fn check_name(&mut self, expr: &Expr, name: &str) -> Option<TypedExpr> {
        match self.resolve_name(name) {
            Some(NameResolution::Frame(ty)) => Some(TypedExpr::new(
                expr.span,
                ty,
                TypedKind::LocalBinding(name.to_owned()),
            )),
            Some(NameResolution::Scope(ty)) => Some(TypedExpr::new(
                expr.span,
                ty,
                TypedKind::ScopeBinding(name.to_owned()),
            )),
            Some(NameResolution::Field(ty)) => {
                let base = TypedExpr::new(expr.span, self.current_at(0)?, TypedKind::Current);
                Some(TypedExpr::new(
                    expr.span,
                    ty,
                    TypedKind::Field {
                        base: Box::new(base),
                        name: name.to_owned(),
                    },
                ))
            }
            None => self.error(expr, format!("unknown name `{name}`")),
        }
    }

    fn check_field(&mut self, expr: &Expr, base: &Expr, member: &str) -> Option<TypedExpr> {
        let base = self.check(base)?;
        let row = match base.ty() {
            ExprType::Row(row) => row,
            // §6.4: `view.member` flattens the nested collection `member` across
            // the view's rows, exactly as `view::member` does — the dotted and
            // `::` spellings expand to the same traversal.
            ExprType::View(_) => return self.traverse_view(expr, base, member),
            // §5.8/§11.5: member access on a struct value — a host-namespace call
            // result (`identity.rp` on `webauthn.verify(@response)`), a static
            // struct. The runtime's `eval_field` already reads a `Value::Struct`
            // member; the checker types it against the struct's declared fields.
            ExprType::Scalar(Type::Struct(struct_ty)) => {
                let field_ty = struct_ty.field(member).cloned();
                return match field_ty {
                    Some(ty) => Some(TypedExpr::new(
                        expr.span,
                        ExprType::scalar(ty),
                        TypedKind::Field { base: Box::new(base), name: member.to_owned() },
                    )),
                    None => self.error(expr, format!("no member `{member}` on this struct")),
                };
            }
            other => {
                return self.error(
                    expr,
                    format!("cannot read field `{member}` of a {}", other.describe()),
                );
            }
        };
        match row.field(member) {
            Some(ty) => {
                let ty = ty.clone();
                Some(TypedExpr::new(
                    expr.span,
                    ty,
                    TypedKind::Field {
                        base: Box::new(base),
                        name: member.to_owned(),
                    },
                ))
            }
            None => self.error(expr, format!("no field `{member}` on this row")),
        }
    }

    fn check_list(&mut self, expr: &Expr, items: &[Expr]) -> Option<TypedExpr> {
        if items.is_empty() {
            // §7.4: the empty list is the empty-view combinator.
            let row = RowType::keyless(std::iter::empty::<(String, ExprType)>());
            return Some(TypedExpr::new(expr.span, ExprType::View(row), TypedKind::EmptyView));
        }
        let mut typed = Vec::with_capacity(items.len());
        let mut element: Option<Type> = None;
        for item in items {
            let checked = self.check(item)?;
            let ty = match checked.ty().as_scalar() {
                Some(ty) => ty.clone(),
                None => return self.error(item, "a list element must be a scalar value"),
            };
            match &element {
                Some(existing) if *existing != ty => {
                    return self.error(item, "list elements must share one type");
                }
                _ => element = Some(ty),
            }
            typed.push(checked);
        }
        let element = element?;
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Set(Box::new(element))),
            TypedKind::List(typed),
        ))
    }
}

/// How a bare name resolved.
enum NameResolution {
    /// A row binding introduced within the expression (resolved from a frame).
    Frame(ExprType),
    /// A lexical binding from the base scope.
    Scope(ExprType),
    /// A field of the current row (`name` reads as `.name`, §7.1).
    Field(ExprType),
}

/// The permissive static type of a bare `none` literal: `optional<json>`, the
/// widest optional (A.7). A more specific optional type flows from the field or
/// operand a `none` is compared or assigned against; this crate types the bare
/// literal at its widest and leaves narrowing to the model layer.
fn none_type() -> ExprType {
    ExprType::scalar(Type::Optional(Box::new(Type::Json)))
}

fn reject(source: SourceId, stmt: &Stmt, message: &str) -> Diagnostic {
    Diagnostic::error(message.to_owned())
        .code("E-EXPR")
        .primary(Span::new(source, stmt.span), "here")
        .build()
}

fn single(diag: Diagnostic) -> Diagnostics {
    let mut diags = Diagnostics::new();
    diags.push(diag);
    diags
}

