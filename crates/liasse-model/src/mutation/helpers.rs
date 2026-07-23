//! Pure AST helpers for the mutation phase: statement/expression walking,
//! target resolution against the model tree, prototype-name parsing, and the
//! detection of state-changing operators. Kept separate from the stateful
//! [`super::MutPhase`] so the checking logic reads without these mechanics
//! inline.

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::ExprType;
use liasse_syntax::{Arg, BinaryOp, Expr, ExprKind, SpannedExpression, Stmt, StmtKind};
use liasse_value::{RefTarget, Type};

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

/// The parameter contract inferred for one mutation (§8.3): each name's settled
/// type plus the names whose uses inferred two incompatible types.
///
/// §8.3 requires that "all uses of the same parameter MUST infer one compatible
/// type". A key selector over a `text`-keyed and an `int`-keyed collection, for
/// instance, infers two incompatible types for one `@p`; keeping only the first
/// (as a plain map insert would) hides that conflict, so it is recorded here and
/// surfaced as a load rejection.
#[derive(Default)]
pub(super) struct Params {
    types: BTreeMap<String, ExprType>,
    conflicts: BTreeSet<String>,
}

impl Params {
    /// Seed the contract from an explicit `name({ proto })` prototype (§8.3),
    /// whose declared types later uses must stay compatible with.
    pub(super) fn from_prototype(prototype: Option<BTreeMap<String, ExprType>>) -> Self {
        Self {
            types: prototype.unwrap_or_default(),
            conflicts: BTreeSet::new(),
        }
    }

    /// Whether a settled type exists for `name`.
    pub(super) fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }

    /// Whether `name`'s uses inferred two incompatible types.
    pub(super) fn conflicts(&self, name: &str) -> bool {
        self.conflicts.contains(name)
    }

    /// The settled `name → type` pairs in name order.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &ExprType)> {
        self.types.iter()
    }

    /// Consume into the ordered pair list the [`super::Mutation`] contract holds.
    pub(super) fn into_pairs(self) -> Vec<(String, ExprType)> {
        self.types.into_iter().collect()
    }
}

/// Record an inferred parameter type, flagging an incompatible re-inference as a
/// §8.3 conflict rather than silently keeping the first.
pub(super) fn record(params: &mut Params, name: &str, ty: ExprType) {
    match params.types.get(name) {
        None => {
            params.types.insert(name.to_owned(), ty);
        }
        Some(existing) if !compatible(existing, &ty) => {
            params.conflicts.insert(name.to_owned());
        }
        Some(_) => {}
    }
}

/// Whether two inferred types for one parameter are §8.3-compatible. Two scalar
/// types are compatible when equal, related by the `int`/`decimal` numeric
/// widening, or when one is a `ref` and the other is that ref target's key type
/// (§5.6/§6.3, below); a non-scalar (row/view) re-inference is left permissive,
/// as the CORE model does not compare row identities here.
fn compatible(a: &ExprType, b: &ExprType) -> bool {
    match (a.as_scalar(), b.as_scalar()) {
        (Some(x), Some(y)) => scalar_compatible(x, y),
        _ => true,
    }
}

/// Whether two scalar types unify for one parameter (§8.3 "one compatible type").
fn scalar_compatible(x: &Type, y: &Type) -> bool {
    if x == y {
        return true;
    }
    match (x, y) {
        (Type::Int, Type::Decimal) | (Type::Decimal, Type::Int) => true,
        // §5.6 (line 515) "A ref exposes the target's key type" / §6.3 (line 721)
        // "a key of the same declared target compares the current typed key": a
        // `@param` used as a `/coll[@p]` selector infers the target key type, and
        // the same `@param` used as a `$ref:/coll` field value infers a `ref` to
        // that target. Both denote one value domain — the target's key — so they
        // are one compatible type, not a `text`-vs-`ref` conflict. A genuine
        // mismatch (a `ref<accounts>` against an unrelated `int` key) still fails
        // here because the ref's own key type differs.
        (Type::Ref(target), other) | (other, Type::Ref(target)) => ref_key_compatible(target, other),
        _ => false,
    }
}

/// Whether a scalar type is the key type a `ref` target exposes (§5.6): a
/// scalar-keyed target against its scalar key, or a composite-keyed target
/// against the positional composite key type.
fn ref_key_compatible(target: &RefTarget, key: &Type) -> bool {
    match target {
        RefTarget::Scalar(inner) => inner.as_ref() == key,
        RefTarget::Composite(components) => {
            matches!(key, Type::Composite(supplied) if supplied == components)
        }
    }
}

/// Parse a `$mut` member name into its base name and optional §8.3 prototype,
/// or explain why the prototype is malformed. The prototype object is parsed by
/// the shared A.2 type grammar ([`crate::types::parse_prototype`]). Shared with
/// the module phase, which validates a `$modules` interface `$mut` contract name
/// against the same prototype grammar (§13.8).
pub(crate) fn parse_name(raw: &str) -> Result<(String, Option<BTreeMap<String, ExprType>>), String> {
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

/// The name a local-binding statement introduces (§8, Annex C.9:
/// `local = value_or_mutation_result`). A local binding's target is a bare name;
/// a field assignment's target is a `.field` path (an [`ExprKind::Field`]) and a
/// set/collection operation is a [`StmtKind::Bare`] statement, so a plain
/// [`ExprKind::Name`] target is unambiguously the local-binding form.
pub(super) fn local_binding_name(target: &Expr) -> Option<&str> {
    match &target.kind {
        ExprKind::Name(id) => Some(&id.text),
        _ => None,
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
/// A parameter used as a host-namespace call argument (`ns.fn(…, @p, …)`, §16.4)
/// *is* collected: it is a real contract parameter, inferred from the host
/// function's declared argument signature (see
/// [`super::MutPhase::infer_host_args`]), so the caller passes it explicitly in the
/// §12.1 closed argument object. Collecting it here keeps this inferability check
/// consistent with that inference — the parameter is inferred *and* found, so a
/// host-only parameter (`identity = ns.verify(@response)`) is never falsely
/// rejected as "cannot be inferred".
///
/// A parameter whose only occurrence is a *non-host* call argument stays
/// deliberately uncollected: an in-program mutation-call argument (§8.11) inherits
/// its type from the callee's contract, a documented cross-program seam the CORE
/// model does not resolve, so its type is *deferred* rather than rejected here. The
/// walk therefore descends into a call's callee, into a host call's bare-parameter
/// arguments, but not into a non-host call's arguments.
pub(super) fn collect_param_refs<'e>(expr: &'e Expr, out: &mut Vec<(&'e str, liasse_diag::ByteSpan)>) {
    if let ExprKind::Param(id) = &expr.kind {
        out.push((&id.text, id.span));
    }
    if let ExprKind::Call { callee, args } = &expr.kind {
        collect_param_refs(callee, out);
        if host_call_target(callee).is_some() {
            for arg in args {
                if let ExprKind::Param(id) = &arg_expr(arg).kind {
                    out.push((&id.text, id.span));
                }
            }
        }
        return;
    }
    for child in child_exprs(expr) {
        collect_param_refs(child, out);
    }
}

/// The value expression a positional or named argument carries.
pub(super) fn arg_expr(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) | Arg::Named { value, .. } => value,
    }
}

/// The `(namespace, function)` a host-namespace-shaped call names (§16.4): a
/// `Field { base: Name(ns), member }` callee with a non-structural member that is
/// not a `string.*` builtin. This is the structural shape of an application
/// host-namespace call — a bare-identifier namespace applied to a positional
/// argument list — distinct from an in-program mutation call (whose callee bases
/// on an `#import`, a selector, `.`, or a local row binding, never a bare
/// `namespace.fn(positional)`) and from a value/view builtin (`size`, `string.*`).
///
/// Whether `ns` resolves to a declared `$requires` namespace is confirmed by the
/// caller against the pinned descriptors; a bare positional `@param` argument is
/// never a valid mutation-call form (a mutation takes a closed argument object or
/// nothing, §8.5), so this shape uniquely identifies a host-call argument whose
/// parameter the contract must carry.
pub(super) fn host_call_target(callee: &Expr) -> Option<(&str, &str)> {
    if is_builtin_call(callee) {
        return None;
    }
    match &callee.kind {
        ExprKind::Field { base, member } if !member.structural => match &base.kind {
            ExprKind::Name(ns) => Some((ns.text.as_str(), member.text.as_str())),
            _ => None,
        },
        _ => None,
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

/// Whether `expr` reads any local name in `deferred` — a local the CORE phase
/// bound to a value it could not type (a mutation-operator result, §8, or a
/// host/program-call result, §8.11/§16.4), left UNBOUND rather than mis-typed.
///
/// A value expression built over such a local is itself untypeable at this phase
/// (its type flows from the deferred result), so the §16.2 deferral is transitive:
/// the phase accepts it structurally rather than rejecting a well-formed reference
/// with a spurious "unknown name". Its full typing runs under a host-resolved load.
pub(super) fn references_deferred(expr: &Expr, deferred: &BTreeSet<String>) -> bool {
    if let ExprKind::Name(id) = &expr.kind
        && deferred.contains(&id.text)
    {
        return true;
    }
    child_exprs(expr).into_iter().any(|child| references_deferred(child, deferred))
}

pub(crate) fn stmt_exprs(stmt: &Stmt) -> Vec<&Expr> {
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
