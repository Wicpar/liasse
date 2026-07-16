//! Pure AST helpers for the mutation phase: statement/expression walking,
//! target resolution against the model tree, prototype-name parsing, and the
//! detection of state-changing operators. Kept separate from the stateful
//! [`super::MutPhase`] so the checking logic reads without these mechanics
//! inline.

use std::collections::BTreeMap;

use liasse_expr::ExprType;
use liasse_syntax::{BinaryOp, Expr, ExprKind, SpannedExpression, Stmt, StmtKind};

use crate::state::{Node, Shape};
use crate::walk::child_exprs;

/// Row bindings in scope while inferring a mutation's parameters: a filtered
/// selector `[:x | ...]` binds `x` to a row of the selected collection (§6.4),
/// which the inference walk resolves `x.field` against.
pub(super) type BindEnv = BTreeMap<String, ExprType>;

/// Whether an operator relates its two operands to a single scalar type, so a
/// bare `@p` operand inherits the sibling operand's type (§8.3): the comparison
/// and arithmetic operators. `&&`/`||`/`in`/`??` are excluded — they do not make
/// their operands share one scalar type.
pub(super) fn is_scalar_binop(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
    )
}

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

/// Collect every `@name` parameter reference that must infer a type *here*,
/// paired with the span of its use, for the §8.3 inferability check.
///
/// A parameter whose only occurrence is a call argument is deliberately not
/// collected: as a call argument it inherits its type from the callee's declared
/// signature (§8.3, "inherits ... type"), and the CORE model does not resolve
/// host namespaces (§16.4) or cross-program mutation contracts (§8.11). Such a
/// parameter's type is therefore *deferred* to that later resolution rather than
/// rejected here, so the walk descends into a call's callee but not its
/// arguments.
pub(super) fn collect_param_refs<'e>(expr: &'e Expr, out: &mut Vec<(&'e str, liasse_diag::ByteSpan)>) {
    if let ExprKind::Param(id) = &expr.kind {
        out.push((&id.text, id.span));
    }
    if let ExprKind::Call { callee, .. } = &expr.kind {
        collect_param_refs(callee, out);
        return;
    }
    for child in child_exprs(expr) {
        collect_param_refs(child, out);
    }
}

/// The pure value/view builtins the expression checker types by name (§6.5): the
/// generators, `size`/`has`/`assert`, the aggregates, and the `string.*`
/// namespace functions. Every *other* call in a mutation program is an in-program
/// mutation call (§8.11), a host-namespace call (§16.4), or a state operation
/// such as `erase`/`reinsert` (§21) — none of which the value checker types.
fn is_builtin_call(callee: &Expr) -> bool {
    match &callee.kind {
        ExprKind::Name(id) => matches!(
            id.text.as_str(),
            "size" | "has" | "assert" | "now" | "uuid" | "count" | "sum" | "avg" | "min" | "max"
                | "distinct"
        ),
        // `string.lower/upper/trim` are the only namespace builtins the checker
        // resolves; every other `ns.fn` is a host-namespace call (§16.4).
        ExprKind::Field { base, member } if !member.structural => {
            matches!(&base.kind, ExprKind::Name(ns) if ns.text == "string")
                && matches!(member.text.as_str(), "lower" | "upper" | "trim")
        }
        _ => false,
    }
}

/// Whether an expression is a program-level call the value checker cannot type:
/// an in-program mutation call (§8.11), a host-namespace call (§16.4), or a state
/// operation such as `erase`/`reinsert` (§21). The mutation phase accepts such a
/// call structurally — its target and arguments are resolved by the referenced
/// mutation, host contract, or operation, not by expression typing.
pub(super) fn is_program_call(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { callee, .. } if !is_builtin_call(callee))
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
