//! Lifting inline surface expressions into evaluable top-level declarations.
//!
//! A surface `$view`/`$mut` may carry an expression directly on the surface
//! rather than a bare reference to a declared view or mutation (§10.1: "a value
//! containing a mutation expression or array defines an inline program for that
//! surface"; §10.2 likewise for a surface view). The model validates such an
//! inline member structurally but retains no runnable declaration for it, and
//! the surface router can only bind a *named* runtime view or mutation. So the
//! adapter reconstructs what a production host wires by hand: for each inline
//! surface member it synthesizes a top-level view or root mutation carrying that
//! expression — the same mechanism [`super::mod::inject_synthetic_views`] uses
//! for `$actor`/`$members` — and records the address→name map the router binds
//! through.
//!
//! A member that already names a declared runtime mutation (with or without a
//! receiver selector) is left to the router's reference path; only inline
//! members with no declared-mutation token are lifted here.

use std::collections::{BTreeMap, BTreeSet};

use liasse_diag::SourceMap;
use liasse_syntax::{
    parse_expression, Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector, StmtKind,
};
use serde_json::{Map, Value as J};

use super::router::identifier_tokens;

/// The synthetic top-level declarations reconstructed for a package's inline
/// surface members, plus the address→name maps the router binds through.
#[derive(Debug, Default, Clone)]
pub struct SurfaceLift {
    /// Synthetic top-level `$view` members: `(name, expression)`.
    views: Vec<(String, String)>,
    /// Synthetic top-level `$mut` members: `(name, body)`.
    muts: Vec<(String, J)>,
    /// Surface address (`<prefix>.<surface>`) → synthetic view name.
    view_of: BTreeMap<String, String>,
    /// Call address (`<prefix>.<surface>.<call>`) → synthetic mutation name.
    mut_of: BTreeMap<String, String>,
}

impl SurfaceLift {
    /// Analyze a package's `$public`/`$roles` and reconstruct the synthetic
    /// declarations for every inline surface member. A package with no `$model`
    /// object yields an empty lift.
    #[must_use]
    pub fn derive(package: &J) -> Self {
        let mut lift = Self::default();
        let Some(model) = package.get("$model") else { return lift };
        let declared = declared_mutations(model);
        let declared_views = declared_views(model);
        if let Some(public) = model.get("$public").and_then(J::as_object) {
            for (surface, definition) in public {
                lift.surface("public", surface, definition, &declared, &declared_views);
            }
        }
        if let Some(roles) = model.get("$roles").and_then(J::as_object) {
            for (role, definition) in roles {
                let Some(members) = definition.as_object() else { continue };
                for (surface, member) in members {
                    if surface.starts_with('$') {
                        continue;
                    }
                    lift.surface(role, surface, member, &declared, &declared_views);
                }
            }
        }
        lift
    }

    /// Reconstruct one surface's inline `$view` and `$mut` members under the
    /// address prefix (`public`, or a role name).
    fn surface(
        &mut self,
        prefix: &str,
        surface: &str,
        definition: &J,
        declared: &BTreeSet<String>,
        declared_views: &BTreeSet<String>,
    ) {
        let Some(members) = definition.as_object() else { return };
        if let Some(view) = members.get("$view").and_then(J::as_str)
            && liftable_view(view)
            && !references_declared_view(view, declared_views)
        {
            let name = format!("liasse_lift_view_{}_{prefix}_{surface}", self.views.len());
            self.views.push((name.clone(), view.to_owned()));
            self.view_of.insert(format!("{prefix}.{surface}"), name);
        }
        if let Some(calls) = members.get("$mut").and_then(J::as_object) {
            for (call, body) in calls {
                if !liftable_mut(body, declared) {
                    continue;
                }
                let name = format!("liasse_lift_mut_{}_{prefix}_{surface}_{call}", self.muts.len());
                self.muts.push((name.clone(), body.clone()));
                self.mut_of.insert(format!("{prefix}.{surface}.{call}"), name);
            }
        }
    }

    /// Inject every synthetic view and mutation into a package's `$model` object,
    /// so the engine compiles them alongside the declared ones.
    pub fn inject(&self, model: &mut Map<String, J>) {
        for (name, expr) in &self.views {
            model.insert(name.clone(), J::Object(one("$view", J::String(expr.clone()))));
        }
        if self.muts.is_empty() {
            return;
        }
        let entries = model
            .entry("$mut")
            .or_insert_with(|| J::Object(Map::new()))
            .as_object_mut();
        if let Some(entries) = entries {
            for (name, body) in &self.muts {
                entries.insert(name.clone(), body.clone());
            }
        }
    }

    /// A copy carrying only the view lifts. A lifted inline mutation whose
    /// parameters the model cannot yet infer would fail the whole load; dropping
    /// the mutation lifts lets a case that loaded before keep loading (its inline
    /// call then resolves `denied`, unchanged), while its view lifts still bind.
    #[must_use]
    pub fn views_only(&self) -> Self {
        Self {
            views: self.views.clone(),
            muts: Vec::new(),
            view_of: self.view_of.clone(),
            mut_of: BTreeMap::new(),
        }
    }

    /// Whether the lift carries any synthetic declaration.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.views.is_empty() && self.muts.is_empty()
    }

    /// The synthetic view bound to surface `address` (`<prefix>.<surface>`).
    #[must_use]
    pub fn view_name(&self, address: &str) -> Option<&str> {
        self.view_of.get(address).map(String::as_str)
    }

    /// The synthetic root mutation bound to call `address`
    /// (`<prefix>.<surface>.<call>`).
    #[must_use]
    pub fn mut_name(&self, address: &str) -> Option<&str> {
        self.mut_of.get(address).map(String::as_str)
    }
}

/// A single-member JSON object.
fn one(key: &str, value: J) -> Map<String, J> {
    let mut object = Map::new();
    object.insert(key.to_owned(), value);
    object
}

/// Whether a surface `$view` expression can be lifted to a top-level view.
///
/// A plain top-level named view has no binding for the *request scope*: a
/// surface parameter (`@param`, §10.1 `$params`) or a request-scoped variable
/// (`$actor`/`$members`/`$proof`/`$config`, §6.2), so a view reading one cannot
/// be lifted (SPEC-ISSUES item 10; the surface layer leaves scope-parameterized
/// evaluation to the runtime). A leading-`$` token is not by itself a request
/// variable, though: the projection combinators `$sort`/`$skip`/`$limit` and the
/// synthetic `$key` (§7.2/§7.3) are structural directives on the block, and the
/// temporal selectors `.$all`/`.$between`/`.$at` (§14.2) are structural *field
/// members* — both stay within a plain view's own evaluation and must lift. So
/// the guard parses the expression and refuses only a genuine `@param` or a
/// request-scoped `$name` used as an atom, not every `$`/`@` byte.
fn liftable_view(text: &str) -> bool {
    let mut sources = SourceMap::new();
    let source = sources.add_label("surface-view", text.to_owned());
    let Ok(parsed) = parse_expression(source, text) else {
        // Unparseable as a standalone view expression: leave it unlifted (the
        // surface stays unbound and its watch resolves `denied`), rather than
        // synthesize a declaration the engine will reject at load.
        return false;
    };
    let StmtKind::Bare(expr) = &parsed.statement().kind else { return false };
    !reads_request_scope(expr)
}

/// Whether an expression reads the request scope — a `@param` surface parameter
/// or a `$name` request-scoped variable used as an atom. Structural *field
/// members* (`.$all`) and projection *directives* (`$sort`, `$key`, …) are not
/// atoms and are deliberately not counted here.
fn reads_request_scope(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Param(_) | ExprKind::Structural(_) => true,
        ExprKind::None
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Decimal(_)
        | ExprKind::Str(_)
        | ExprKind::Root
        | ExprKind::Current
        | ExprKind::Parent(_)
        | ExprKind::Import(_)
        | ExprKind::Name(_) => false,
        ExprKind::List(items) => items.iter().any(reads_request_scope),
        ExprKind::Object(members) => members.iter().any(member_reads_request_scope),
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => reads_request_scope(base),
        ExprKind::Select { base, selector } => {
            reads_request_scope(base) || selector_reads_request_scope(selector)
        }
        ExprKind::Call { callee, args } => {
            reads_request_scope(callee) || args.iter().any(arg_reads_request_scope)
        }
        ExprKind::Block { base, members } => {
            reads_request_scope(base) || members.iter().any(member_reads_request_scope)
        }
        ExprKind::Unary { operand, .. } => reads_request_scope(operand),
        ExprKind::Binary { lhs, rhs, .. } => reads_request_scope(lhs) || reads_request_scope(rhs),
        ExprKind::Ternary { cond, then, otherwise } => {
            reads_request_scope(cond) || reads_request_scope(then) || reads_request_scope(otherwise)
        }
        ExprKind::Combination { operands, .. } => operands.iter().any(reads_request_scope),
    }
}

/// Whether a selector's key sources or filter predicate read the request scope.
fn selector_reads_request_scope(selector: &Selector) -> bool {
    match selector {
        Selector::Keys(keys) => keys.iter().any(reads_request_scope),
        Selector::Bind { condition, .. } => condition.as_deref().is_some_and(reads_request_scope),
    }
}

/// Whether a projection/patch block member's value reads the request scope. A
/// `$sort`/`$key`/… directive is itself structural, but its *value* is walked so
/// a `$sort: [ f(@param) ]` still counts.
fn member_reads_request_scope(member: &BlockMember) -> bool {
    match &member.kind {
        BlockMemberKind::Directive { value, .. } | BlockMemberKind::Assign { value, .. } => {
            reads_request_scope(value)
        }
        BlockMemberKind::Named { value, .. } => value.as_ref().is_some_and(reads_request_scope),
        BlockMemberKind::Shorthand(expr) => reads_request_scope(expr),
        BlockMemberKind::Clear(_) => false,
    }
}

/// Whether a call argument's value reads the request scope.
fn arg_reads_request_scope(arg: &Arg) -> bool {
    match arg {
        Arg::Positional(expr) | Arg::Named { value: expr, .. } => reads_request_scope(expr),
    }
}

/// Whether a surface `$view` expression is exactly a bare reference `.name` to
/// an already-declared top-level view. Such a reference must NOT be lifted into a
/// synthetic wrapper view: the wrapper's expression (`.name`) re-types as a
/// stream and loses the referenced view's singular delivery shape (§12.2). The
/// router's bare-reference path binds the declared view directly, preserving its
/// shape, so this member is left to that path.
fn references_declared_view(text: &str, declared_views: &BTreeSet<String>) -> bool {
    let Some(name) = text.strip_prefix('.') else { return false };
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && declared_views.contains(name)
}

/// Every declared top-level view name of the model: a member (outside
/// `$public`/`$roles`) whose value carries a `$view` key.
fn declared_views(model: &J) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(object) = model.as_object() else { return names };
    for (name, value) in object {
        if name.starts_with('$') {
            continue;
        }
        if value.as_object().is_some_and(|member| member.contains_key("$view")) {
            names.insert(name.clone());
        }
    }
    names
}

/// Whether a surface `$mut` member is an inline program to lift: an array
/// (an inline atomic program), or a string expression naming no declared
/// mutation. A string naming a declared mutation (with or without a receiver
/// selector) is left to the router's reference path.
fn liftable_mut(body: &J, declared: &BTreeSet<String>) -> bool {
    match body {
        J::Array(_) => true,
        J::String(text) => !identifier_tokens(text).into_iter().any(|token| declared.contains(token)),
        _ => false,
    }
}

/// Every declared mutation name in the model: the entry keys of every `$mut`
/// block outside `$public`/`$roles` (a surface `$mut` names external calls, not
/// runtime mutations).
fn declared_mutations(model: &J) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_mutations(model, &mut names);
    names
}

/// Recursively collect `$mut` entry keys, skipping surface subtrees.
fn collect_mutations(node: &J, names: &mut BTreeSet<String>) {
    let Some(object) = node.as_object() else { return };
    for (key, value) in object {
        match key.as_str() {
            "$public" | "$roles" => {}
            "$mut" => {
                if let Some(entries) = value.as_object() {
                    names.extend(entries.keys().cloned());
                }
            }
            _ => collect_mutations(value, names),
        }
    }
}
