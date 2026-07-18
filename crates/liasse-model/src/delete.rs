//! Deletion policy: `$on_delete` and deferred delete decisions (SPEC.md §21.1).
//!
//! Two static rules over `$ref` `$on_delete` policies:
//!
//! * **Policy shape.** A declared `$on_delete` is exactly one of `restrict`,
//!   `cascade`, `none`, or a `"= patch"` expression; `none` is valid only for an
//!   optional ref, since it expands to a patch assigning `none` to the
//!   referencing field (§21.1, §5.6).
//! * **Deferred decision.** A ref MAY omit `$on_delete` only while its target
//!   cannot be deleted by any declaration in the owning module. When a mutation
//!   introduces a deleting capability over a target collection (`collection -
//!   keys`, `-row_source`, `collection = view`, `erase(row)`), every inbound ref
//!   to that collection MUST declare a policy, or the whole package is rejected
//!   (§21.1). The diagnostic names the deleting declaration and the ref.
//! * **Cross-module boundary.** A `$ref` whose target is an imported module
//!   interface (`#handle`) crosses a module boundary. §13.12 requires such a ref
//!   to declare `$on_delete` *immediately*, without the deferral above: the
//!   target package owns its own state and exposed membership and may evolve or
//!   uninstall independently, so no local reasoning can prove the target is
//!   undeletable. Omission is rejected here.
//!
//! CORE scope: the deleting-capability set is computed from directly authored
//! deleting operators. Transitive capability across mutation *calls* and across
//! cascade-induced row removal (also §21.1) is a documented runtime seam; the
//! same-module deferral this pass models catches the authored capabilities the
//! chapter's static cases pin. Which concrete peer instance a `#handle` binds is
//! a composition-runtime concern; the immediate `$on_delete` requirement itself
//! is decided here from the single package.

use std::collections::BTreeSet;

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, Expr, ExprKind, StmtKind};

use crate::build::RawMut;
use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::state::{Node, Reference, Shape};

/// Validate `$on_delete` policy shapes and the §21.1 deferred-decision rule.
pub(crate) fn check(reporter: &mut Reporter, sources: &mut SourceMap, root: &Shape, raw: &[RawMut]) {
    let deletable = deletable_targets(sources, raw);
    let mut refs = Vec::new();
    collect_refs(root, &mut String::new(), &mut refs);
    for reference in &refs {
        check_policy(reporter, reference.reference);
        // §13.12: a ref crossing a module boundary (`#handle` target) must declare
        // `$on_delete` at the ref site, immediately — the local deferral below,
        // which reasons about the owning module's own deleting capabilities, cannot
        // apply because the target instance is owned and evolved by another package.
        if reference.reference.target.trim_start().starts_with('#') {
            if reference.reference.on_delete.is_none() {
                reporter.reject_hint(
                    reference.reference.span,
                    code::DELETE,
                    format!(
                        "ref to imported module interface `{}` must declare `$on_delete`: a ref crossing a module boundary decides its deletion policy at the ref site (§13.12)",
                        reference.reference.target,
                    ),
                    "declare `$on_delete` as `restrict`, `cascade`, `none` (optional refs), or a `= patch`",
                );
            }
            continue;
        }
        if reference.reference.on_delete.is_none() && deletable.contains(&reference.target) {
            reporter.reject_hint(
                reference.reference.span,
                code::DELETE,
                format!(
                    "ref to `{}` leaves `$on_delete` undecided, but a mutation can delete from `{}`",
                    reference.reference.target, reference.reference.target,
                ),
                "declare `$on_delete` as `restrict`, `cascade`, `none` (optional refs), or a `= patch`",
            );
        }
    }
}

/// One located `$ref` and the normalized absolute path of its target.
struct LocatedRef<'a> {
    reference: &'a Reference,
    target: String,
}

fn collect_refs<'a>(shape: &'a Shape, prefix: &mut String, out: &mut Vec<LocatedRef<'a>>) {
    for member in &shape.members {
        let base = prefix.len();
        prefix.push('/');
        prefix.push_str(member.name.as_str());
        match &member.node {
            Node::Reference(reference) => out.push(LocatedRef {
                reference,
                target: normalize(&reference.target),
            }),
            // §5.5/§5.6: a `$set` of `$ref` holds a per-member reference to its
            // target relation. That member reference is a governed inbound ref, so
            // it MUST reach the §21.1 deferred-delete-decision gate exactly like a
            // scalar `$ref` field — an undecided policy over a deletable target
            // rejects the package, a decided one passes.
            Node::Set(set) => {
                if let Some(reference) = &set.element_ref {
                    out.push(LocatedRef {
                        reference,
                        target: normalize(&reference.target),
                    });
                }
            }
            Node::Struct(inner) => collect_refs(inner, prefix, out),
            Node::Collection(collection) => collect_refs(&collection.shape, prefix, out),
            _ => {}
        }
        prefix.truncate(base);
    }
}

/// §21.1 / §5.6: validate one declared `$on_delete` policy value.
fn check_policy(reporter: &mut Reporter, reference: &Reference) {
    let Some(policy) = &reference.on_delete else {
        return;
    };
    let text = policy.text.trim();
    match text {
        "restrict" | "cascade" => {}
        "none" => {
            if !reference.optional {
                reporter.reject_hint(
                    policy.span,
                    code::DELETE,
                    format!("`$on_delete: none` needs an optional ref, but `{}` is required", reference.target),
                    "make the ref optional (`\"$optional\": true`) or choose `restrict`/`cascade`/`= patch`",
                );
            }
        }
        _ if text.starts_with('=') => {}
        other => reporter.reject_hint(
            policy.span,
            code::DELETE,
            format!("`{other}` is not an `$on_delete` policy"),
            "use `restrict`, `cascade`, `none`, or a `= patch` expression",
        ),
    }
}

/// The set of collection paths a mutation can delete from (§21.1 capabilities).
fn deletable_targets(sources: &mut SourceMap, raw: &[RawMut]) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for entry in raw {
        for text in statement_texts(entry) {
            let sub = sources.add_label("delete-scan", text.clone());
            if let Ok(parsed) = parse_expression(sub, &text) {
                scan_statement(&parsed.statement.kind, &entry.path, &mut targets);
            }
        }
    }
    targets
}

fn statement_texts(entry: &RawMut) -> Vec<String> {
    if let Some(text) = entry.body.as_string() {
        vec![text.to_owned()]
    } else if let Some(items) = entry.body.as_array() {
        items.iter().filter_map(|v| v.as_string().map(str::to_owned)).collect()
    } else {
        Vec::new()
    }
}

fn scan_statement(kind: &StmtKind, receiver: &[String], targets: &mut BTreeSet<String>) {
    match kind {
        // `collection = view` replaces a whole collection.
        StmtKind::Assign { target, .. } => {
            if let Some(path) = collection_path(target, receiver) {
                targets.insert(path);
            }
        }
        StmtKind::Bare(expr) | StmtKind::Return(expr) | StmtKind::Clear(expr) => {
            scan_expr(expr, receiver, targets);
        }
    }
}

fn scan_expr(expr: &Expr, receiver: &[String], targets: &mut BTreeSet<String>) {
    match &expr.kind {
        // `collection - keys`
        ExprKind::Binary { op: liasse_syntax::BinaryOp::Sub, lhs, .. } => {
            if let Some(path) = collection_path(lhs, receiver) {
                targets.insert(path);
            }
        }
        // `-row_source`
        ExprKind::Unary { op: liasse_syntax::UnaryOp::Neg, operand } => {
            if let Some(path) = collection_path(operand, receiver) {
                targets.insert(path);
            }
        }
        // `erase(row)`
        ExprKind::Call { callee, args } => {
            if matches!(&callee.kind, ExprKind::Name(id) if id.text == "erase")
                && let Some(first) = args.first()
            {
                let inner = match first {
                    liasse_syntax::Arg::Positional(v) | liasse_syntax::Arg::Named { value: v, .. } => v,
                };
                if let Some(path) = collection_path(inner, receiver) {
                    targets.insert(path);
                }
            }
        }
        _ => {}
    }
}

/// The absolute collection path a target/row-source expression addresses, with
/// trailing selectors dropped (`/companies["a"].modules` folds to segments).
fn collection_path(expr: &Expr, receiver: &[String]) -> Option<String> {
    let mut segments = Vec::new();
    if !walk_path(expr, receiver, &mut segments) {
        return None;
    }
    Some(normalize(&segments.join("/")))
}

fn walk_path(expr: &Expr, receiver: &[String], segments: &mut Vec<String>) -> bool {
    match &expr.kind {
        ExprKind::Current => {
            segments.extend(receiver.iter().cloned());
            true
        }
        ExprKind::Root => true,
        ExprKind::Field { base, member } => {
            if !walk_path(base, receiver, segments) {
                return false;
            }
            segments.push(member.text.clone());
            true
        }
        ExprKind::Select { base, .. } => walk_path(base, receiver, segments),
        _ => false,
    }
}

/// Normalize a target path to the `/segment/...` index form used by refs.
fn normalize(target: &str) -> String {
    let trimmed = target.trim();
    let body = trimmed.trim_start_matches('/').trim_end_matches('/');
    format!("/{body}")
}
