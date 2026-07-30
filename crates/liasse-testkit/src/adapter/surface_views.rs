//! Inlining a parameterized top-level `$view` onto the surfaces that reference
//! it (§10.1/§7.1).
//!
//! §10.1 infers a surface `$view` parameter from its typed uses, exactly as §8.3
//! infers a mutation parameter, so a surface that reads `@name` needs no
//! hand-written `$params` block for the runtime to type and serve it. Nothing is
//! injected here.
//!
//! What remains is a *shape* fix a production host would not need. A case may
//! factor a parameterized read into a top-level `$view` declaration and expose it
//! by reference (`"$view": ".meta"`). A top-level `$view` is not a surface: it has
//! no parameter scope of its own (§7.1), so `@name` in it resolves to nothing and
//! the whole package fails to load. This inlines the referenced expression onto
//! each surface that names it — where `@name` *does* have a parameter scope, and
//! §10.1 inference then types it — and drops the now-unreferenced top-level
//! declaration.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, StmtKind};
use serde_json::{Map, Value as J};

/// Inline every bare surface `$view` reference to a parameterized top-level view
/// (`.meta` ⇒ `.docs[@id] { … }`) onto the referencing surface, then drop the
/// top-level declaration (which cannot compile scope-free). A package with no
/// `$model` object is left untouched.
pub(super) fn inline_param_views(package: &mut J) {
    let param_views = match package.get("$model").and_then(J::as_object) {
        Some(model) => param_views(model),
        None => return,
    };
    if param_views.is_empty() {
        return;
    }
    let Some(model) = package.get_mut("$model").and_then(J::as_object_mut) else {
        return;
    };
    if let Some(public) = model.get_mut("$public").and_then(J::as_object_mut) {
        for surface in public.values_mut() {
            inline_surface(surface, &param_views);
        }
    }
    if let Some(roles) = model.get_mut("$roles").and_then(J::as_object_mut) {
        for role in roles.values_mut() {
            let Some(members) = role.as_object_mut() else { continue };
            for (name, surface) in members.iter_mut() {
                if name.starts_with('$') {
                    continue;
                }
                inline_surface(surface, &param_views);
            }
        }
    }
    // A parameterized top-level view reads a surface parameter and so cannot
    // compile as a scope-free top-level declaration (it would fail the whole
    // load). Now that its expression is inlined onto every surface referencing
    // it, drop it so the load succeeds.
    for name in param_views.keys() {
        model.remove(name);
    }
}

/// Inline a bare surface `$view` reference to a parameterized top-level view.
fn inline_surface(surface: &mut J, param_views: &BTreeMap<String, String>) {
    let Some(members) = surface.as_object_mut() else { return };
    if let Some(view) = members.get("$view").and_then(J::as_str)
        && let Some(target) = bare_reference(view)
        && let Some(expr) = param_views.get(target).cloned()
    {
        members.insert("$view".to_owned(), J::String(expr));
    }
}

/// The parameterized top-level views of `$model`: each non-`$` member carrying a
/// `$view` that reads a `@param`, mapped to its `$view` expression. A top-level
/// `$view` has no parameter scope (§7.1), so it cannot compile as written; a
/// surface that references one takes the expression inline instead, where §10.1
/// inference types the parameter.
fn param_views(model: &Map<String, J>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, decl) in model {
        if name.starts_with('$') {
            continue;
        }
        let Some(view) = decl.as_object().and_then(|object| object.get("$view")).and_then(J::as_str) else {
            continue;
        };
        if view.contains('@') && !param_names(view).is_empty() {
            out.insert(name.clone(), view.to_owned());
        }
    }
    out
}

/// The identifier a bare `.name` reference names (alphanumeric/underscore only),
/// or `None` for any other `$view` form.
fn bare_reference(text: &str) -> Option<&str> {
    let name = text.strip_prefix('.')?;
    (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')).then_some(name)
}

/// Every `@param` name a view expression reads. An unparseable view yields none
/// (the declaration is left as-is).
fn param_names(view: &str) -> BTreeSet<String> {
    let mut sources = SourceMap::new();
    let source = sources.add_label("surface-view-params", view.to_owned());
    let Ok(parsed) = parse_expression(source, view) else { return BTreeSet::new() };
    let StmtKind::Bare(expr) = &parsed.statement().kind else { return BTreeSet::new() };
    let mut out = BTreeSet::new();
    collect(expr, &mut out);
    out
}

/// Walk `expr`, recording every `@param` name it reads.
fn collect(expr: &Expr, out: &mut BTreeSet<String>) {
    if let ExprKind::Param(id) = &expr.kind {
        out.insert(id.text.clone());
    }
    match &expr.kind {
        ExprKind::Select { base, selector } => {
            collect(base, out);
            match selector {
                Selector::Keys(keys) => keys.iter().for_each(|key| collect(key, out)),
                Selector::Bind { condition: Some(condition), .. } => collect(condition, out),
                Selector::Bind { .. } => {}
            }
        }
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => collect(base, out),
        ExprKind::List(items) => items.iter().for_each(|item| collect(item, out)),
        ExprKind::Object(members) => members.iter().for_each(|member| collect_member(member, out)),
        ExprKind::Block { base, members } => {
            collect(base, out);
            members.iter().for_each(|member| collect_member(member, out));
        }
        ExprKind::Call { callee, args } => {
            collect(callee, out);
            args.iter().for_each(|arg| collect_arg(arg, out));
        }
        ExprKind::Unary { operand, .. } => collect(operand, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect(lhs, out);
            collect(rhs, out);
        }
        ExprKind::Ternary { cond, then, otherwise } => {
            collect(cond, out);
            collect(then, out);
            collect(otherwise, out);
        }
        ExprKind::Combination { operands, .. } => operands.iter().for_each(|operand| collect(operand, out)),
        _ => {}
    }
}

/// Collect the `@param`s a projection/patch block member reads.
fn collect_member(member: &BlockMember, out: &mut BTreeSet<String>) {
    match &member.kind {
        BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
            collect(value, out);
        }
        BlockMemberKind::Named { value: Some(value), .. } => collect(value, out),
        BlockMemberKind::Shorthand(expr) => collect(expr, out),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
    }
}

/// Collect the `@param`s a call argument reads.
fn collect_arg(arg: &Arg, out: &mut BTreeSet<String>) {
    match arg {
        Arg::Positional(expr) | Arg::Named { value: expr, .. } => collect(expr, out),
    }
}
