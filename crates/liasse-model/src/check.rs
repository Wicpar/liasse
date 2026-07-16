//! Phase 2: typing the state-tree expressions (SPEC.md §5.1, §5.2, §5.10, §7).
//!
//! Defaults, computed values, `$normalize`, `$check`, and `$view` expressions
//! are parsed and type-checked in their declaration scope through
//! [`liasse_expr`], with each expression registered as its own diagnostic
//! sub-source so a rejection points at the offending bytes *within* the
//! expression. This phase also enforces the §5.1 acyclic-default rule over each
//! shape's sibling fields.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType, Scope};
use liasse_syntax::{parse_expression, Expr, ExprKind, SpannedExpression};
use liasse_value::Type;

use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{Check, ExprSource, Node, ScalarField, Shape};
use crate::walk::child_exprs;

/// Type-check every expression in the state tree.
pub(crate) fn check_tree(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &Shape,
) {
    let root_row = ExprType::Row(resolver.shape_row(root));
    let mut checker = TreeChecker {
        reporter,
        sources,
        resolver,
        root: root_row.clone(),
    };
    checker.shape(root, vec![root_row]);
}

/// Carries the shared borrows through the recursive tree walk.
struct TreeChecker<'a, 'b> {
    reporter: &'a mut Reporter<'b>,
    sources: &'a mut SourceMap,
    resolver: &'a Resolver<'a>,
    root: ExprType,
}

impl TreeChecker<'_, '_> {
    /// Check one shape whose row is `contexts.last()`.
    fn shape(&mut self, shape: &Shape, contexts: Vec<ExprType>) {
        self.detect_cycles(shape);
        for check in &shape.checks {
            let scope = ModelScope::nested(contexts.clone(), self.root.clone());
            self.check_bool(&scope, check, "a row/struct `$check` must be a `bool` condition");
        }
        for member in &shape.members {
            match &member.node {
                Node::Scalar(field) => self.scalar(field, &contexts),
                Node::Struct(inner) => {
                    let mut nested = contexts.clone();
                    nested.push(ExprType::Row(self.resolver.shape_row(inner)));
                    self.shape(inner, nested);
                }
                Node::Collection(collection) => {
                    let mut nested = contexts.clone();
                    nested.push(ExprType::Row(self.resolver.collection_row(collection)));
                    self.shape(&collection.shape, nested);
                }
                Node::View(view) => self.view(&view.expr, &contexts),
                Node::Set(_) | Node::Reference(_) | Node::Named(_) => {}
            }
        }
    }

    /// Check a scalar field's default, computed, normalize, and check exprs.
    fn scalar(&mut self, field: &ScalarField, contexts: &[ExprType]) {
        // A default and a computed value read the containing row as `.`.
        let row_scope = ModelScope::nested(contexts.to_vec(), self.root.clone());
        // `$normalize`/`$check` read the field's own value as `.` (§8.8).
        let mut value_chain = contexts.to_vec();
        value_chain.push(ExprType::scalar(field.ty.clone()));
        let value_scope = ModelScope::nested(value_chain, self.root.clone());

        if let Some(default) = &field.default
            && let Some(typed) = self.check_value(&row_scope, default)
        {
            self.expect_assignable(&typed, &field.ty, default);
        }
        if let Some(computed) = &field.computed {
            self.check_pure_value(&row_scope, computed);
        }
        if let Some(normalize) = &field.normalize
            && let Some(typed) = self.check_pure_value(&value_scope, normalize)
        {
            self.expect_assignable(&typed, &field.ty, normalize);
        }
        for check in &field.checks {
            self.check_bool(&value_scope, check, "a field `$check` must be a `bool` condition");
        }
    }

    fn view(&mut self, expr: &ExprSource, contexts: &[ExprType]) {
        let scope = ModelScope::nested(contexts.to_vec(), self.root.clone());
        if let Some(typed) = self.check_pure_value(&scope, expr)
            && typed.ty().as_view().is_none()
        {
            self.reporter.reject_hint(
                expr.span,
                code::EXPR,
                "a `$view` must evaluate to a row stream",
                "select a collection, e.g. `.tasks`, optionally with a projection",
            );
        }
    }

    fn check_bool(&mut self, scope: &dyn Scope, check: &Check, message: &str) {
        // §8.8: a `$check` is a pure position — no generated functions.
        if let Some(typed) = self.check_pure_value(scope, &check.condition)
            && typed.ty().as_scalar() != Some(&Type::Bool)
        {
            self.reporter.reject(check.condition.span, code::EXPR, message.to_owned());
        }
    }

    /// Parse and type-check one expression against `scope`, returning the typed
    /// node or emitting the rejection. Generated functions are permitted (used
    /// for `$default`, which §8.8 exempts).
    fn check_value(
        &mut self,
        scope: &dyn Scope,
        source: &ExprSource,
    ) -> Option<liasse_expr::TypedExpr> {
        self.check_expr(scope, source, false)
    }

    /// Like [`check_value`](Self::check_value) but for a pure position (computed
    /// value, `$normalize`, `$check`, `$view`): a generated function such as
    /// `now()` or `uuid()` is rejected (§8.8).
    fn check_pure_value(
        &mut self,
        scope: &dyn Scope,
        source: &ExprSource,
    ) -> Option<liasse_expr::TypedExpr> {
        self.check_expr(scope, source, true)
    }

    fn check_expr(
        &mut self,
        scope: &dyn Scope,
        source: &ExprSource,
        pure: bool,
    ) -> Option<liasse_expr::TypedExpr> {
        let parsed = self.parse(source)?;
        if pure && let Some(func) = generated_call(statement_expr(&parsed)) {
            self.reporter.reject_hint(
                source.span,
                code::EXPR,
                format!(
                    "the generated function `{func}()` may not appear in a pure position — a computed value, `$normalize`, `$check`, or `$view` (§8.8)"
                ),
                "generated functions like `now()`/`uuid()` are allowed only in `$default` and mutation bodies",
            );
        }
        let sub = self.sources.add_label("expr", source.text.clone());
        match check_statement(scope, sub, &parsed) {
            Ok(typed) => Some(typed),
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }

    fn parse(&mut self, source: &ExprSource) -> Option<SpannedExpression> {
        if source.text.trim().is_empty() {
            self.reporter.reject(source.span, code::EXPR, "an expression must not be empty");
            return None;
        }
        let sub = self.sources.add_label("expr", source.text.clone());
        match parse_expression(sub, &source.text) {
            Ok(parsed) => Some(parsed),
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }

    fn expect_assignable(
        &mut self,
        value: &liasse_expr::TypedExpr,
        target: &Type,
        source: &ExprSource,
    ) {
        if !value_assignable(value, target) {
            self.reporter.reject_hint(
                source.span,
                code::EXPR,
                format!(
                    "this expression has type `{}` but the field expects `{}`",
                    value.ty().describe(),
                    target.name()
                ),
                "make the expression's result match the declared field type",
            );
        }
    }

    /// §5.1: the default/computed dependency graph of a shape's fields must be
    /// acyclic.
    fn detect_cycles(&mut self, shape: &Shape) {
        let names: BTreeSet<&str> = shape.members.iter().map(|m| m.name.as_str()).collect();
        let mut graph: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for member in &shape.members {
            if let Node::Scalar(field) = &member.node {
                let mut deps = BTreeSet::new();
                for src in field.default.iter().chain(field.computed.iter()) {
                    if let Ok(parsed) = parse_expression(self.sources.add_label("dep", src.text.clone()), &src.text) {
                        collect_field_refs(statement_expr(&parsed), &names, &mut deps);
                    }
                }
                deps.remove(member.name.as_str());
                graph.insert(member.name.as_str(), deps);
            }
        }
        if let Some(cycle) = find_cycle(&graph) {
            let span = shape
                .member(&cycle)
                .map_or_else(|| liasse_diag::ByteSpan::point(0), |m| m.span);
            self.reporter.reject_hint(
                span,
                code::CYCLE,
                format!("field `{cycle}` participates in a cyclic default/computed dependency (§5.1)"),
                "break the cycle so insertion values can be evaluated in a topological order",
            );
        }
    }
}

/// Whether a *typed* expression result is assignable to a declared field type.
///
/// This adds the one value-sensitive rule on top of [`assignable`]: a literal
/// `none` is the absent value of *every* `optional<T>` (A.1), so it is accepted
/// against any optional target even though its static type is the widest
/// optional (`optional<json>`, A.7 — liasse-expr types the bare literal at its
/// widest and leaves this narrowing to the model layer).
pub(crate) fn value_assignable(value: &liasse_expr::TypedExpr, target: &Type) -> bool {
    if value.is_none_literal() && matches!(target, Type::Optional(_)) {
        return true;
    }
    assignable(value.ty(), target)
}

/// Whether an expression result is assignable to a declared field type.
///
/// Assignment typing follows §5.3/§8: optionality is meaningful (a `none` is
/// absence, A.1), so it widens but never silently narrows. A definite `T`
/// assigns to `optional<T>`, but an `optional<T>` does **not** assign to a
/// required `T` — that would let a `none` flow into a field the state model
/// requires to be present (§8.3: a parameter "inherits … optionality"). Wrapped
/// types are assignable only when their inner types are.
pub(crate) fn assignable(value: &ExprType, target: &Type) -> bool {
    let Some(scalar) = value.as_scalar() else {
        return matches!(target, Type::View(_));
    };
    scalar_assignable(scalar, target)
}

/// Sound assignability between two scalar/structured types (see [`assignable`]).
fn scalar_assignable(value: &Type, target: &Type) -> bool {
    if value == target {
        return true;
    }
    match (value, target) {
        // An `optional<T>` never narrows to a non-optional target: the value may
        // be `none`, which a required field cannot hold (§8.3, A.1).
        (Type::Optional(_), other) if !matches!(other, Type::Optional(_)) => false,
        // Optional widens: a definite `T` (or an `optional<T>`) is assignable to
        // `optional<U>` exactly when its value type is assignable to `U`. This
        // also governs `optional<T> -> optional<U>`, whose inners must match.
        (_, Type::Optional(target_inner)) => {
            let value_inner = match value {
                Type::Optional(inner) => inner.as_ref(),
                other => other,
            };
            scalar_assignable(value_inner, target_inner)
        }
        // `json` carries any *definite* value (A.1: `none` is not a `json`
        // value, so an optional is excluded by the arm above).
        (_, Type::Json) => true,
        // Numeric widening: an `int` is assignable where a `decimal` is expected.
        (Type::Int, Type::Decimal) => true,
        _ => false,
    }
}

/// The name of the first generated (impure) function call reachable from `expr`,
/// if any (§8.12: `now()` and `uuid()` are the generated language functions).
fn generated_call(expr: &Expr) -> Option<&'static str> {
    if let ExprKind::Call { callee, .. } = &expr.kind
        && let ExprKind::Name(id) = &callee.kind
    {
        match id.text.as_str() {
            "now" => return Some("now"),
            "uuid" => return Some("uuid"),
            _ => {}
        }
    }
    for child in child_exprs(expr) {
        if let Some(found) = generated_call(child) {
            return Some(found);
        }
    }
    None
}

fn statement_expr(parsed: &SpannedExpression) -> &Expr {
    use liasse_syntax::StmtKind;
    match &parsed.statement().kind {
        StmtKind::Bare(expr) | StmtKind::Return(expr) | StmtKind::Clear(expr) => expr,
        StmtKind::Assign { value, .. } => value,
    }
}

/// References `expr` makes to sibling declaration names (`.name`, bare `name`).
fn collect_field_refs(expr: &Expr, siblings: &BTreeSet<&str>, out: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Name(id) if siblings.contains(id.text.as_str()) => {
            out.insert(id.text.clone());
        }
        ExprKind::Field { base, member } => {
            if matches!(base.kind, ExprKind::Current) && siblings.contains(member.text.as_str()) {
                out.insert(member.text.clone());
            }
        }
        _ => {}
    }
    for child in child_exprs(expr) {
        collect_field_refs(child, siblings, out);
    }
}

/// Find any name on a cycle in the dependency graph, or `None` if acyclic.
fn find_cycle(graph: &BTreeMap<&str, BTreeSet<String>>) -> Option<String> {
    let mut state: BTreeMap<&str, Visit> = BTreeMap::new();
    for node in graph.keys() {
        if let Some(name) = visit(node, graph, &mut state) {
            return Some(name);
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    Active,
    Done,
}

fn visit<'g>(
    node: &'g str,
    graph: &BTreeMap<&'g str, BTreeSet<String>>,
    state: &mut BTreeMap<&'g str, Visit>,
) -> Option<String> {
    match state.get(node) {
        Some(Visit::Done) => return None,
        Some(Visit::Active) => return Some(node.to_owned()),
        None => {}
    }
    state.insert(node, Visit::Active);
    if let Some(deps) = graph.get(node) {
        for dep in deps {
            if let Some((key, _)) = graph.get_key_value(dep.as_str())
                && let Some(found) = visit(key, graph, state)
            {
                return Some(found);
            }
        }
    }
    state.insert(node, Visit::Done);
    None
}
