//! Pure AST helpers for the mutation phase: statement/expression walking,
//! target resolution against the model tree, prototype-name parsing, and the
//! detection of state-changing operators. Kept separate from the stateful
//! [`super::MutPhase`] so the checking logic reads without these mechanics
//! inline.

use std::collections::BTreeMap;

use liasse_expr::ExprType;
use liasse_syntax::{Expr, ExprKind, SpannedExpression, Stmt, StmtKind};

use crate::state::{Node, Shape};
use crate::walk::child_exprs;

/// The receiver body shape at `path` from the model root (§8.2).
pub(super) fn receiver_shape<'a>(root: &'a Shape, path: &[String]) -> &'a Shape {
    let mut shape = root;
    for segment in path {
        match shape.member(segment).map(|m| &m.node) {
            Some(Node::Collection(collection)) => shape = &collection.shape,
            Some(Node::Struct(inner)) => shape = inner,
            _ => break,
        }
    }
    shape
}

/// Record an inferred parameter type, or reject an incompatible re-inference.
pub(super) fn record(params: &mut BTreeMap<String, ExprType>, name: &str, ty: ExprType) {
    params.entry(name.to_owned()).or_insert(ty);
}

/// Parse a `$mut` member name into its base name and optional §8.3 prototype,
/// or explain why the prototype is malformed. The prototype object is parsed by
/// the shared A.2 type grammar ([`crate::types::parse_prototype`]).
pub(super) fn parse_name(raw: &str) -> Result<(String, Option<BTreeMap<String, ExprType>>), String> {
    let Some((base, rest)) = raw.split_once('(') else {
        return Ok((raw.trim().to_owned(), None));
    };
    let base = base.trim().to_owned();
    let Some(inner) = rest.trim_end().strip_suffix(')') else {
        return Err(format!(
            "the prototype in `{raw}` is missing its closing `)`; a prototype is written `name({{ param: type }})` (§8.3)"
        ));
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok((base, Some(BTreeMap::new())));
    }
    let params = crate::types::parse_prototype(inner)?;
    Ok((base, Some(params)))
}

/// Whether an expression uses a state-changing operator the value checker
/// cannot type (insert/replace/delete/patch/call).
pub(super) fn uses_mutation_operator(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Block { .. }
            | ExprKind::Call { .. }
            | ExprKind::Binary { op: liasse_syntax::BinaryOp::Add | liasse_syntax::BinaryOp::Sub, .. }
            | ExprKind::Unary { op: liasse_syntax::UnaryOp::Neg, .. }
    ) && contains_row_source(expr)
}

/// Whether the expression roots in a state row source (`.`, `/`, a collection).
fn contains_row_source(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Current | ExprKind::Root => true,
        ExprKind::Field { base, .. }
        | ExprKind::Select { base, .. }
        | ExprKind::Block { base, .. } => contains_row_source(base),
        ExprKind::Binary { lhs, .. } => contains_row_source(lhs),
        ExprKind::Unary { operand, .. } => contains_row_source(operand),
        _ => false,
    }
}

/// Resolve a target expression to the model node it addresses, if any.
pub(super) fn resolve_node<'t>(expr: &Expr, receiver: &'t Shape, root: &'t Shape) -> Option<&'t Node> {
    match &expr.kind {
        ExprKind::Field { base, member } => {
            let base_shape = target_shape(base, receiver, root)?;
            base_shape.member(&member.text).map(|m| &m.node)
        }
        _ => None,
    }
}

/// The shape a target-base expression addresses (a row body).
fn target_shape<'t>(expr: &Expr, receiver: &'t Shape, root: &'t Shape) -> Option<&'t Shape> {
    match &expr.kind {
        ExprKind::Current => Some(receiver),
        ExprKind::Root => Some(root),
        ExprKind::Select { base, .. } => match target_node(base, receiver, root)? {
            Node::Collection(collection) => Some(&collection.shape),
            _ => None,
        },
        ExprKind::Field { .. } => match resolve_node(expr, receiver, root)? {
            Node::Struct(inner) => Some(inner),
            Node::Collection(collection) => Some(&collection.shape),
            _ => None,
        },
        _ => None,
    }
}

fn target_node<'t>(expr: &Expr, receiver: &'t Shape, root: &'t Shape) -> Option<&'t Node> {
    resolve_node(expr, receiver, root)
}

/// Resolve an expression to a view/row type against the receiver row (for key
/// inference). Only the direct-field and current cases are modelled.
pub(super) fn resolve_target(expr: &Expr, receiver: &ExprType, root: &liasse_expr::RowType) -> Option<ExprType> {
    match &expr.kind {
        ExprKind::Current => Some(receiver.clone()),
        ExprKind::Root => Some(ExprType::Row(root.clone())),
        ExprKind::Field { base, member } => {
            let base_ty = resolve_target(base, receiver, root)?;
            base_ty.as_row().and_then(|r| r.field(&member.text)).cloned()
        }
        _ => None,
    }
}

/// The absolute `/segment/...` path a write target addresses, with selectors
/// dropped, resolved from the receiver `path`. `None` for a non-path target.
pub(super) fn write_path(expr: &Expr, receiver: &[String]) -> Option<String> {
    let mut segments = Vec::new();
    if !collect_segments(expr, receiver, &mut segments) {
        return None;
    }
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        out.push_str(&segment);
    }
    Some(out)
}

fn collect_segments(expr: &Expr, receiver: &[String], segments: &mut Vec<String>) -> bool {
    match &expr.kind {
        ExprKind::Current => {
            segments.extend(receiver.iter().cloned());
            true
        }
        ExprKind::Root => true,
        ExprKind::Field { base, member } => {
            collect_segments(base, receiver, segments) && {
                segments.push(member.text.clone());
                true
            }
        }
        ExprKind::Select { base, .. } => collect_segments(base, receiver, segments),
        _ => false,
    }
}

/// Collect every `@name` parameter reference reachable from `expr`, paired with
/// the span of its use, for the §8.3 inferability check.
pub(super) fn collect_param_refs<'e>(expr: &'e Expr, out: &mut Vec<(&'e str, liasse_diag::ByteSpan)>) {
    if let ExprKind::Param(id) = &expr.kind {
        out.push((&id.text, id.span));
    }
    for child in child_exprs(expr) {
        collect_param_refs(child, out);
    }
}

pub(super) fn stmt_exprs(stmt: &Stmt) -> Vec<&Expr> {
    match &stmt.kind {
        StmtKind::Return(expr) | StmtKind::Bare(expr) | StmtKind::Clear(expr) => vec![expr],
        StmtKind::Assign { target, value } => vec![target, value],
    }
}

pub(super) fn wrap(expr: Expr) -> SpannedExpression {
    SpannedExpression {
        statement: Stmt {
            span: expr.span,
            kind: StmtKind::Bare(expr),
        },
    }
}
