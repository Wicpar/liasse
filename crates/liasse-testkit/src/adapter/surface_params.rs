//! Reconstructing the `$params` a parameterized surface `$view` needs (§10.1).
//!
//! A surface `$view` may read a surface parameter `@name` (§10.1), but the model
//! infers surface-view parameter types only where a declared `$params` names them
//! — inference proper is defined for mutations (§8.3), not surface views
//! (SPEC-ISSUES item 10). Without a `$params` block the runtime cannot type the
//! `@name` occurrence, so [`compile_surface_views`] drops the whole surface view
//! and the surface is left unserved (a role `$view` then resolves to `denied`
//! even for an authorized member).
//!
//! A production host declares `$params` by hand. This reconstructs that: for each
//! `$public`/`$roles` surface whose `$view` reads a `@name` but declares no
//! `$params`, it injects a `$params` block, typing each parameter from the
//! collection key it selects (`.docs[@id]` ⇒ the `docs` key type) and defaulting
//! to `text` for any other occurrence (a filter comparand, §6.4). The runtime
//! then compiles and serves the parameterized surface view.
//!
//! [`compile_surface_views`]: (runtime-internal)

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_syntax::{parse_expression, Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, StmtKind};
use serde_json::{Map, Value as J};

/// Reconstruct the surface-view parameter wiring a production host declares by
/// hand (§10.1). For every `$public`/`$roles` surface: a bare `$view` reference
/// to a parameterized top-level view (`.meta` ⇒ `.docs[@id] { … }`) is inlined
/// onto the surface (that top-level view cannot compile as a scope-free
/// declaration, so it is then removed), and a `$params` block is injected for the
/// now-inline `@param` view. A package with no `$model` object is left untouched.
pub(super) fn inject(package: &mut J) {
    let (key_types, param_views) = match package.get("$model").and_then(J::as_object) {
        Some(model) => (collection_key_types(model), param_views(model)),
        None => return,
    };
    let Some(model) = package.get_mut("$model").and_then(J::as_object_mut) else {
        return;
    };
    if let Some(public) = model.get_mut("$public").and_then(J::as_object_mut) {
        for surface in public.values_mut() {
            inject_surface(surface, &param_views, &key_types);
        }
    }
    if let Some(roles) = model.get_mut("$roles").and_then(J::as_object_mut) {
        for role in roles.values_mut() {
            let Some(members) = role.as_object_mut() else { continue };
            for (name, surface) in members.iter_mut() {
                if name.starts_with('$') {
                    continue;
                }
                inject_surface(surface, &param_views, &key_types);
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

/// Inline a bare surface `$view` reference to a parameterized top-level view, then
/// inject the `$params` block the (now inline) `@param` view needs — when the
/// surface declares none.
fn inject_surface(surface: &mut J, param_views: &BTreeMap<String, String>, key_types: &BTreeMap<String, Vec<String>>) {
    let Some(members) = surface.as_object_mut() else { return };
    if members.contains_key("$params") {
        return;
    }
    // Inline a `.name` reference to a parameterized top-level view onto the surface.
    if let Some(view) = members.get("$view").and_then(J::as_str)
        && let Some(target) = bare_reference(view)
        && let Some(expr) = param_views.get(target)
    {
        let expr = expr.clone();
        members.insert("$view".to_owned(), J::String(expr));
    }
    let Some(view) = members.get("$view").and_then(J::as_str) else { return };
    if !view.contains('@') {
        return;
    }
    let params = infer_params(view, key_types);
    if params.is_empty() {
        return;
    }
    let block = params.into_iter().map(|(name, ty)| (name, J::String(ty))).collect::<Map<_, _>>();
    members.insert("$params".to_owned(), J::Object(block));
}

/// The parameterized top-level views of `$model`: each non-`$` member carrying a
/// `$view` that reads a `@param`, mapped to its `$view` expression. These cannot
/// compile as scope-free top-level views (SPEC-ISSUES item 10), so a surface that
/// references one takes the expression inline instead.
fn param_views(model: &Map<String, J>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, decl) in model {
        if name.starts_with('$') {
            continue;
        }
        let Some(view) = decl.as_object().and_then(|object| object.get("$view")).and_then(J::as_str) else {
            continue;
        };
        if view.contains('@') && !infer_params(view, &BTreeMap::new()).is_empty() {
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

/// The declared type of every `@param` a surface `$view` reads: a key-selector
/// parameter (`.coll[@p]`) takes the collection key's type; every other
/// occurrence defaults to `text`. An unparseable view yields no parameters (the
/// surface is left as-is, unserved, exactly as before).
fn infer_params(view: &str, key_types: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, String> {
    let mut sources = SourceMap::new();
    let source = sources.add_label("surface-view-params", view.to_owned());
    let Ok(parsed) = parse_expression(source, view) else { return BTreeMap::new() };
    let StmtKind::Bare(expr) = &parsed.statement().kind else { return BTreeMap::new() };
    let mut out = BTreeMap::new();
    collect(expr, key_types, &mut out);
    out
}

/// Walk `expr`, recording each `@param` name against its inferred type. A
/// key-selector parameter is typed from the base collection's key (an override,
/// so it wins wherever the same name also appears bare); every other `@param`
/// defaults to `text`.
fn collect(expr: &Expr, key_types: &BTreeMap<String, Vec<String>>, out: &mut BTreeMap<String, String>) {
    match &expr.kind {
        ExprKind::Param(id) => {
            out.entry(id.text.clone()).or_insert_with(|| "text".to_owned());
        }
        ExprKind::Select { base, selector } => {
            if let Selector::Keys(keys) = selector
                && let Some(collection) = collection_name(base)
                && let Some(types) = key_types.get(collection)
            {
                for (position, key) in keys.iter().enumerate() {
                    if let ExprKind::Param(id) = &key.kind
                        && let Some(ty) = types.get(position)
                    {
                        out.insert(id.text.clone(), ty.clone());
                    }
                }
            }
            collect(base, key_types, out);
            match selector {
                Selector::Keys(keys) => keys.iter().for_each(|key| collect(key, key_types, out)),
                Selector::Bind { condition: Some(condition), .. } => collect(condition, key_types, out),
                Selector::Bind { .. } => {}
            }
        }
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => collect(base, key_types, out),
        ExprKind::List(items) => items.iter().for_each(|item| collect(item, key_types, out)),
        ExprKind::Object(members) => members.iter().for_each(|member| collect_member(member, key_types, out)),
        ExprKind::Block { base, members } => {
            collect(base, key_types, out);
            members.iter().for_each(|member| collect_member(member, key_types, out));
        }
        ExprKind::Call { callee, args } => {
            collect(callee, key_types, out);
            args.iter().for_each(|arg| collect_arg(arg, key_types, out));
        }
        ExprKind::Unary { operand, .. } => collect(operand, key_types, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect(lhs, key_types, out);
            collect(rhs, key_types, out);
        }
        ExprKind::Ternary { cond, then, otherwise } => {
            collect(cond, key_types, out);
            collect(then, key_types, out);
            collect(otherwise, key_types, out);
        }
        ExprKind::Combination { operands, .. } => operands.iter().for_each(|operand| collect(operand, key_types, out)),
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Decimal(_)
        | ExprKind::Str(_)
        | ExprKind::Root
        | ExprKind::Current
        | ExprKind::Parent(_)
        | ExprKind::Import(_)
        | ExprKind::Structural(_)
        | ExprKind::Name(_) => {}
    }
}

/// Collect the `@param`s a projection/patch block member reads.
fn collect_member(member: &BlockMember, key_types: &BTreeMap<String, Vec<String>>, out: &mut BTreeMap<String, String>) {
    match &member.kind {
        BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
            collect(value, key_types, out);
        }
        BlockMemberKind::Named { value: Some(value), .. } => collect(value, key_types, out),
        BlockMemberKind::Shorthand(expr) => collect(expr, key_types, out),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => {}
    }
}

/// Collect the `@param`s a call argument reads.
fn collect_arg(arg: &Arg, key_types: &BTreeMap<String, Vec<String>>, out: &mut BTreeMap<String, String>) {
    match arg {
        Arg::Positional(expr) | Arg::Named { value: expr, .. } => collect(expr, key_types, out),
    }
}

/// The collection name a selector base names — the immediate `.name`/`name`
/// segment a key selector indexes into. `None` for a deeper or computed base.
fn collection_name(base: &Expr) -> Option<&str> {
    match &base.kind {
        ExprKind::Field { member, .. } => Some(member.text.as_str()),
        ExprKind::Name(id) => Some(id.text.as_str()),
        _ => None,
    }
}

/// The key-field type strings of every top-level collection in `$model`, in
/// `$key` order — the type a `@param` selecting that key takes.
fn collection_key_types(model: &Map<String, J>) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for (name, collection) in model {
        if name.starts_with('$') {
            continue;
        }
        let Some(object) = collection.as_object() else { continue };
        let Some(key) = object.get("$key") else { continue };
        let fields: Vec<&str> = match key {
            J::String(field) => vec![field.as_str()],
            J::Array(fields) => fields.iter().filter_map(J::as_str).collect(),
            _ => continue,
        };
        let types = fields
            .iter()
            .map(|field| field_type(object.get(*field)).unwrap_or_else(|| "text".to_owned()))
            .collect();
        out.insert(name.clone(), types);
    }
    out
}

/// The leading type token of a field declaration string (`"int = 0"` ⇒ `int`,
/// `"text?"` ⇒ `text`). A non-string (expanded or `$ref`) declaration yields
/// `None`, defaulting the key param to `text`.
fn field_type(field: Option<&J>) -> Option<String> {
    let text = field?.as_str()?;
    let ty = text.split(['=', ' ']).next()?.trim().trim_end_matches('?');
    (!ty.is_empty()).then(|| ty.to_owned())
}
