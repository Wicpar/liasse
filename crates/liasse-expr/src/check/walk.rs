//! Structural AST walks used by projection typing: reducing members to outputs,
//! ordering outputs by their acyclic cross-references (§7.1), and the grouped
//! aggregate/key-derived constraint (§7.2).

use std::collections::{BTreeMap, BTreeSet};

use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector};

use crate::ty::RowType;

/// A projection member reduced to an output name and its source expression.
pub(super) struct RawOutput {
    pub(super) name: String,
    pub(super) expr: Expr,
}

/// Dependency-order the outputs so each depends only on earlier ones; returns
/// `None` on a cycle (§7.1).
///
/// The DFS recurses on output-name dependency edges, not AST nesting, so its
/// bound is its own: the `Visit` marking visits each output at most once,
/// capping the recursion depth at the projection's output count — independent
/// of liasse-syntax's expression-nesting cap.
///
/// `loop_binds` names the in-scope §6.4 row bindings (a `[:name]`/`::` bind, and
/// the grouped `group` binding). §7.1/§6.4 (pinned): an output member never
/// shadows a same-named loop binding for a sibling member's expression, so a bare
/// reference to such a name reads the row binding, NOT the like-named output. Those
/// names therefore carry NO cross-reference dependency edge — they are excluded
/// from the output-name set the DFS follows.
pub(super) fn order_outputs(
    outputs: &[RawOutput],
    loop_binds: &BTreeSet<&str>,
) -> Option<Vec<usize>> {
    let names: BTreeSet<&str> = outputs
        .iter()
        .map(|o| o.name.as_str())
        .filter(|name| !loop_binds.contains(name))
        .collect();
    let mut ordered = Vec::with_capacity(outputs.len());
    let mut state = vec![Visit::New; outputs.len()];
    for start in 0..outputs.len() {
        if !visit(start, outputs, &names, &mut state, &mut ordered) {
            return None;
        }
    }
    Some(ordered)
}

/// DFS visit state for the output dependency ordering.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    Active,
    Done,
}

fn visit(
    index: usize,
    outputs: &[RawOutput],
    names: &BTreeSet<&str>,
    state: &mut [Visit],
    ordered: &mut Vec<usize>,
) -> bool {
    match state.get(index) {
        Some(Visit::Done) => return true,
        Some(Visit::Active) => return false,
        _ => {}
    }
    if let Some(slot) = state.get_mut(index) {
        *slot = Visit::Active;
    }
    if let Some(output) = outputs.get(index) {
        let mut refs = BTreeSet::new();
        collect_refs(&output.expr, names, &mut refs);
        for name in refs {
            if name == output.name {
                continue;
            }
            if let Some(dep) = outputs.iter().position(|o| o.name == name)
                && !visit(dep, outputs, names, state, ordered)
            {
                return false;
            }
        }
    }
    if let Some(slot) = state.get_mut(index) {
        *slot = Visit::Done;
    }
    ordered.push(index);
    true
}

/// Names referenced by `expr` that are projection output names.
fn collect_refs<'a>(expr: &'a Expr, names: &BTreeSet<&str>, out: &mut BTreeSet<&'a str>) {
    if let ExprKind::Name(name) = &expr.kind
        && names.contains(name.text.as_str())
    {
        out.insert(name.text.as_str());
    }
    for child in children(expr) {
        collect_refs(child, names, out);
    }
}

/// Whether a grouped output references a source field outside the key set,
/// outside any aggregate subtree (§7.2 aggregate/key-derived constraint).
///
/// A source value is reached two equivalent ways, both rejected: as a bare name
/// (`amount`, resolving to `.amount` on the current source row), or through a
/// row binding `b.field` that §6.4 introduced. `row_binds` maps each such
/// binding name to the row schema it actually denotes:
/// - the CURRENT source row for the projected collection and any `[:name]` filter
///   over it (`lines`, an `[:it]` filter) — `b.f` is then the bare `f` in
///   disguise and rejected identically;
/// - an OUTER `::` ancestor row for a `.entries::lines` chain (`entries`) — whose
///   schema is DIFFERENT from the current source. §7.2: a synthetic `$key`
///   replaces the inherited chain identity, so an ancestor read (`entries.id`,
///   `entries.opened`) is plain non-key source data and must be aggregated or
///   key-derived. Resolving `f` against the bind's OWN schema gives those two
///   reads one verdict; the old current-source-only test split them by an
///   irrelevant field-name coincidence.
///
/// The grouped `group` binding is NOT in the map: it names the group view, whose
/// fields are reached only through an aggregate (`max(group.f)`), which the
/// exemption below already clears.
pub(super) fn references_nonkey_field(
    expr: &Expr,
    source: &RowType,
    key_set: &BTreeSet<&str>,
    row_binds: &BTreeMap<&str, &RowType>,
) -> bool {
    if let ExprKind::Call { callee, .. } = &expr.kind
        && let ExprKind::Name(name) = &callee.kind
        && is_aggregate_name(&name.text)
    {
        return false; // aggregate over `group` is exempt.
    }
    if let ExprKind::Name(name) = &expr.kind {
        let text = name.text.as_str();
        if source.field(text).is_some() && !key_set.contains(text) {
            return true;
        }
    }
    // §7.2: a binding-qualified non-key access `b.f` is rejected against `b`'s OWN
    // row schema, not the current source — so an ancestor `::` bind (`entries.f`)
    // is tested against the ancestor's fields, and the current-source bind
    // (`it.f`/`lines.f`) against the source's, identically to the bare `f`.
    if let ExprKind::Field { base, member } = &expr.kind
        && let ExprKind::Name(base_name) = &base.kind
        && let Some(bind_schema) = row_binds.get(base_name.text.as_str())
    {
        let field = member.text.as_str();
        if bind_schema.field(field).is_some() && !key_set.contains(field) {
            return true;
        }
    }
    children(expr).any(|child| references_nonkey_field(child, source, key_set, row_binds))
}

fn is_aggregate_name(name: &str) -> bool {
    matches!(name, "count" | "sum" | "avg" | "min" | "max" | "distinct")
}

/// Whether `expr` reads the grouped `group` binding (§7.2) — as a bare name, a
/// qualified access `group.field`, or an aggregate `count(group)`.
///
/// §7.2: "Rows sharing the synthetic key form one group. `group` is the
/// source-row view for that output row." The group is DEFINED by the rows that
/// share the synthetic key, so the key is logically PRIOR to the group: the group
/// does not exist until the key has partitioned the rows. A `$key` component that
/// reads `group` is therefore a `key → group → key` cycle — statically ill-formed
/// under §7.1 ("They MAY refer to one another when their dependency graph is
/// acyclic") — and must be rejected at load rather than fault at read (the
/// evaluator's `group_key` frame binds no `group`, so such a key faults). Bare
/// `count(group)` in a NON-key output stays legal; this guard fires only on a key
/// component.
pub(super) fn references_group(expr: &Expr) -> bool {
    if let ExprKind::Name(name) = &expr.kind
        && name.text == "group"
    {
        return true;
    }
    children(expr).any(references_group)
}

pub(super) fn list_items(value: &Expr) -> Vec<&Expr> {
    match &value.kind {
        ExprKind::List(items) => items.iter().collect(),
        _ => Vec::new(),
    }
}

pub(super) fn shorthand_name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Name(name) | ExprKind::Param(name) => Some(name.text.clone()),
        ExprKind::Field { member, .. } => Some(member.text.clone()),
        _ => None,
    }
}

/// A `name:` self-binding projects the binding of that name.
pub(super) fn member_self(member: &BlockMember) -> Expr {
    if let BlockMemberKind::Named { name, .. } = &member.kind {
        return Expr {
            span: member.span,
            kind: ExprKind::Name(name.clone()),
        };
    }
    Expr { span: member.span, kind: ExprKind::Current }
}

/// The direct child expressions of a node, for structural walks.
fn children(expr: &Expr) -> Box<dyn Iterator<Item = &Expr> + '_> {
    match &expr.kind {
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => {
            Box::new(std::iter::once(base.as_ref()))
        }
        ExprKind::Select { base, selector } => {
            let sel: Box<dyn Iterator<Item = &Expr>> = match selector {
                Selector::Keys(keys) => Box::new(keys.iter()),
                Selector::Bind { condition, .. } => Box::new(condition.iter().map(|c| c.as_ref())),
            };
            Box::new(std::iter::once(base.as_ref()).chain(sel))
        }
        ExprKind::Call { callee, args } => {
            Box::new(std::iter::once(callee.as_ref()).chain(args.iter().map(arg_expr)))
        }
        ExprKind::Block { base, members } => {
            Box::new(std::iter::once(base.as_ref()).chain(members.iter().filter_map(member_expr)))
        }
        ExprKind::Unary { operand, .. } => Box::new(std::iter::once(operand.as_ref())),
        ExprKind::Binary { lhs, rhs, .. } => Box::new([lhs.as_ref(), rhs.as_ref()].into_iter()),
        ExprKind::Ternary { cond, then, otherwise } => {
            Box::new([cond.as_ref(), then.as_ref(), otherwise.as_ref()].into_iter())
        }
        ExprKind::List(items) => Box::new(items.iter()),
        ExprKind::Object(members) => Box::new(members.iter().filter_map(member_expr)),
        ExprKind::Combination { operands, .. } => Box::new(operands.iter()),
        _ => Box::new(std::iter::empty()),
    }
}

fn arg_expr(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) => value,
        Arg::Named { value, .. } => value,
    }
}

fn member_expr(member: &BlockMember) -> Option<&Expr> {
    match &member.kind {
        BlockMemberKind::Directive { value, .. }
        | BlockMemberKind::Named { value: Some(value), .. }
        | BlockMemberKind::Assign { value, .. }
        | BlockMemberKind::Shorthand(value) => Some(value),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => None,
    }
}
