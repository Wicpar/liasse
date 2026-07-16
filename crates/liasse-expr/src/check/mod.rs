//! The type checker: turns a spanned AST ([`Expr`]) plus a [`Scope`] into a
//! [`TypedExpr`] or a bundle of [`Diagnostics`].
//!
//! Inference follows §6/§8.3 where the spec pins it and the documented
//! least-surprising choice elsewhere (arithmetic over an optional operand is a
//! static type error, SPEC-ISSUES item 3). The checker recurses structurally on
//! the AST, bounded by liasse-syntax's 512 nesting cap (see [`crate::typed`]) —
//! except the projection-output dependency DFS in [`walk`], which recurses on
//! output-name edges and carries its own bound: the projection's output count.

mod ops;
mod project;
mod views;
mod walk;

use std::collections::BTreeMap;

use liasse_diag::{Diagnostic, Diagnostics, SourceId, Span};
use liasse_syntax::{Expr, ExprKind, SpannedExpression, Stmt, StmtKind};
use liasse_value::{Decimal, Integer, Text, Type, Value};

use crate::scope::Scope;
use crate::ty::{ExprType, RowType};
use crate::typed::{TypedExpr, TypedKind};

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
        Some(typed) if !checker.diags.has_errors() => Ok(typed),
        _ => Err(checker.diags),
    }
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

    pub(crate) fn span(&self, expr: &Expr) -> Span {
        Span::new(self.source, expr.span)
    }

    /// Record a type error and yield `None`, so the caller aborts this subtree.
    pub(crate) fn error(&mut self, expr: &Expr, message: impl Into<String>) -> Option<TypedExpr> {
        self.report(expr, message);
        None
    }

    /// Record a type error without a typed result, for callers whose `None`
    /// carries a different type.
    pub(crate) fn report(&mut self, expr: &Expr, message: impl Into<String>) {
        let span = self.span(expr);
        self.diags.push(
            Diagnostic::error(message.into())
                .code("E-EXPR")
                .primary(span, "in this expression")
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

    /// Resolve a bare name to a binding type: local frames first, then the base
    /// scope's lexical bindings, then a field of the current row.
    fn resolve_name(&self, name: &str) -> Option<NameResolution> {
        for frame in self.frames.iter().rev() {
            if let Some(ty) = frame.bindings.get(name) {
                return Some(NameResolution::Frame(ty.clone()));
            }
        }
        if let Some(ty) = self.scope.binding(name) {
            return Some(NameResolution::Scope(ty));
        }
        if let Some(ExprType::Row(row)) = self.current_at(0)
            && let Some(ty) = row.field(name)
        {
            return Some(NameResolution::Field(ty.clone()));
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
        match self.scope.structural(name) {
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

