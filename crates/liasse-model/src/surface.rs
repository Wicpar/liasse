//! Phase 4: surfaces and roles (SPEC.md §10).
//!
//! Validates the static shape of `$public` and `$roles`: a surface's optional
//! `$params`, an optional `$view` that must evaluate to a row stream, and a
//! `$mut` map whose every reference names a mutation the model actually
//! declares (§10.1). A role additionally carries `$auth` and `$members`.
//!
//! CORE scope: a role `$view`/`$members` that reads `$actor` is validated
//! syntactically rather than fully typed (the `$actor` row type is a later
//! pass); nested roles on rows are a documented seam. `$recursive` coverage
//! (§10.5) is validated here: `$field`/`$through`/`$bind` presence, the covered
//! `$field`, the descendant row-stream shape and identity of `$through`, its
//! strict-descendant (acyclic) navigation, and `bool` `$where`/`$except`
//! predicate types. The strict-descendant check is structural — it verifies the
//! `$through` spine is rooted at `.` and descends into a contained collection
//! ([`descends_from_current`]) — so a relation that could revisit a row through
//! a *ref* field (which needs per-step ref-vs-containment type analysis) is a
//! documented residual, not covered by this decidable subset.

use liasse_diag::SourceMap;
use liasse_expr::ExprType;
use liasse_syntax::{parse_expression, Expr, ExprKind, SpannedExpression, Stmt, StmtKind};
use liasse_value::Type;

use crate::build::RawSurface;
use crate::doc::DocValueExt;
use crate::mutation::{stmt_exprs, Mutation};
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::Shape;
use crate::types::{NamedTypes, TypeParser};

/// A validated surface: its exposed name and whether it is public.
#[derive(Debug, Clone)]
pub struct Surface {
    /// The external surface name.
    pub name: DeclName,
    /// Whether the surface is public (unauthenticated).
    pub public: bool,
    /// The external call names it exposes.
    pub calls: Vec<DeclName>,
}

/// Validate every collected `$public`/`$roles` block.
pub(crate) fn check_surfaces(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &Shape,
    mutations: &[Mutation],
    raw: &[RawSurface],
    config: Option<&ExprType>,
) -> Vec<Surface> {
    let root_row = ExprType::Row(resolver.shape_row(root));
    let mut phase = SurfacePhase {
        reporter,
        sources,
        root,
        root_row: root_row.clone(),
        receiver_row: root_row,
        path: Vec::new(),
        mutations,
        config: config.cloned(),
    };
    let mut surfaces = Vec::new();
    for block in raw {
        // A nested `$roles` scopes `.` and its mutation references to the row at
        // its declaration path (§10.3); a root `$public` uses the model root.
        phase.receiver_row = crate::resolve::row_at(&phase.root_row, &block.path)
            .unwrap_or_else(|| phase.root_row.clone());
        phase.path = block.path.clone();
        if block.public {
            phase.public_block(block, &mut surfaces);
        } else {
            phase.roles_block(block, &mut surfaces);
        }
    }
    surfaces
}

struct SurfacePhase<'a, 'b> {
    reporter: &'a mut Reporter<'b>,
    sources: &'a mut SourceMap,
    /// The model-root shape (`/`), for resolving an inline program's write target.
    root: &'a Shape,
    /// The model-root row (`/`).
    root_row: ExprType,
    /// The `.` receiver row for the block being checked (the row at `path`).
    receiver_row: ExprType,
    /// The receiver location of the block being checked (§10.3 role scope).
    path: Vec<String>,
    mutations: &'a [Mutation],
    /// A module package's `$config` struct row (§13.1), bound as the `$config`
    /// structural so a module surface's `$view`/predicate reads it; `None`
    /// outside a module.
    config: Option<ExprType>,
}

impl SurfacePhase<'_, '_> {
    fn public_block(&mut self, block: &RawSurface, out: &mut Vec<Surface>) {
        let Some(members) = block.value.as_object() else {
            self.reporter.reject(block.value.span, code::SURFACE, "`$public` must be an object of surfaces");
            return;
        };
        for member in members {
            if let Some(surface) = self.surface(&member.name.text, &member.value, true) {
                out.push(surface);
            }
        }
    }

    fn roles_block(&mut self, block: &RawSurface, out: &mut Vec<Surface>) {
        let Some(members) = block.value.as_object() else {
            self.reporter.reject(block.value.span, code::SURFACE, "`$roles` must be an object of roles");
            return;
        };
        for role in members {
            let Some(role_members) = role.value.as_object() else {
                self.reporter.reject(role.value.span, code::SURFACE, "a role must be an object");
                continue;
            };
            if !role_members.iter().any(|m| m.name.text == "$members") {
                self.reporter.reject_hint(
                    role.value.span,
                    code::SURFACE,
                    format!("role `{}` is missing `$members`", role.name.text),
                    "a role needs `$members` to decide who holds it",
                );
            }
            for member in role_members {
                match member.name.text.as_str() {
                    "$auth" | "$members" | "$recursive" => {}
                    other if other.starts_with('$') => self.reporter.reject(
                        member.span,
                        code::SURFACE,
                        format!("`{other}` is not a role member"),
                    ),
                    _ => {
                        // A role's non-`$` members are its granted surfaces.
                        if let Some(surface) = self.surface(&member.name.text, &member.value, false) {
                            out.push(surface);
                        }
                    }
                }
            }
        }
    }

    fn surface(&mut self, name: &str, value: &liasse_syntax::DocValue, public: bool) -> Option<Surface> {
        let name = match DeclName::parse(name) {
            Ok(name) => name,
            Err(reason) => {
                self.reporter.reject(value.span, code::SURFACE, reason);
                return None;
            }
        };
        let Some(members) = value.as_object() else {
            self.reporter.reject(value.span, code::SURFACE, "a surface must be an object");
            return None;
        };
        let params = self.surface_params(value);
        let mut calls = Vec::new();
        for member in members {
            match member.name.text.as_str() {
                "$params" => {}
                // §8.8: a surface `$view` is a pure read position whether granted
                // publicly or through a role, so the generated-call gate runs in
                // both cases; only the public path is additionally fully typed (a
                // role `$view`'s `$actor` typing stays a documented seam).
                "$view" => self.check_view(&member.value, &params, public),
                "$mut" => self.surface_muts(&member.value, public, &mut calls),
                // §10.5: a scoped surface MAY propagate through a checked
                // descendant relation. Its shape and predicate types are
                // validated here; the runtime performs the actual traversal.
                "$recursive" => self.check_recursive(&member.value, &params),
                other if other.starts_with('$') => self.reporter.reject(
                    member.span,
                    code::SURFACE,
                    format!("`{other}` is not a surface member"),
                ),
                other => self.reporter.reject(
                    member.span,
                    code::SURFACE,
                    format!("`{other}` is not a surface member; call names live under `$mut`"),
                ),
            }
        }
        Some(Surface { name, public, calls })
    }

    /// Build the `$params` types of a surface (§10.1) for its `$view` scope.
    fn surface_params(&mut self, value: &liasse_syntax::DocValue) -> Vec<(String, ExprType)> {
        let Some(params_member) = value.member("$params") else {
            return Vec::new();
        };
        let Some(members) = params_member.value.as_object() else {
            self.reporter.reject(params_member.value.span, code::SURFACE, "`$params` must be an object");
            return Vec::new();
        };
        let mut params = Vec::new();
        for member in members {
            if let Some(ty) = self.param_type(member) {
                params.push((member.name.text.clone(), ty));
            }
        }
        params
    }

    /// The declared type of one `$params` entry (§10.1). A field declaration is
    /// either a bare type string (`"text"`, `"text?"`, `"text = 'x'"`) or the
    /// expanded object form (A.3) carrying `$type` alongside the request-scoped
    /// `$normalize`/`$check`/`$default`/`$optional` behaviors §12.1 applies.
    fn param_type(&mut self, member: &liasse_syntax::DocMember) -> Option<ExprType> {
        if let Some(text) = member.value.as_string() {
            let type_str = text.split_once('=').map_or(text, |(lhs, _)| lhs).trim();
            return match TypeParser::parse(type_str, &NamedTypes::new()) {
                Ok(ty) => Some(ExprType::scalar(ty)),
                Err(reason) => {
                    self.reporter.reject(member.value.span, code::SURFACE, reason);
                    None
                }
            };
        }
        let Some(type_member) = member.value.member("$type") else {
            self.reporter.reject_hint(
                member.value.span,
                code::SURFACE,
                "a `$params` field declaration needs a type",
                "e.g. `\"title\": \"text\"` or `{ \"$type\": \"text\", \"$normalize\": \"string.trim(.)\" }`",
            );
            return None;
        };
        let optional = member
            .value
            .member("$optional")
            .and_then(|m| m.value.as_bool())
            .unwrap_or(false);
        let text = type_member.value.as_string().unwrap_or_default();
        match TypeParser::parse(text.trim(), &NamedTypes::new()) {
            Ok(ty) => {
                let ty = if optional { Type::Optional(Box::new(ty)) } else { ty };
                Some(ExprType::scalar(ty))
            }
            Err(reason) => {
                self.reporter.reject(type_member.value.span, code::SURFACE, reason);
                None
            }
        }
    }

    /// §10.5: validate a `$recursive` descendant-coverage block. `$field`,
    /// `$through`, and `$bind` are required; `$where`/`$except` are optional bool
    /// predicates that read the candidate through `$bind`. The checker verifies
    /// the descendant relation resolves to a keyed row stream (descendant shape
    /// and identity) and that each predicate is `bool`.
    fn check_recursive(&mut self, value: &liasse_syntax::DocValue, params: &[(String, ExprType)]) {
        let Some(members) = value.as_object() else {
            self.reporter.reject(value.span, code::SURFACE, "`$recursive` must be an object");
            return;
        };
        for member in members {
            match member.name.text.as_str() {
                "$field" | "$through" | "$bind" | "$where" | "$except" => {}
                other => self.reporter.reject(
                    member.span,
                    code::SURFACE,
                    format!("`{other}` is not a `$recursive` member (§10.5)"),
                ),
            }
        }
        let field = self.recursive_string(value, "$field");
        let through = self.recursive_string(value, "$through");
        let bind = self.recursive_string(value, "$bind");
        let (Some(field), Some(through), Some(bind)) = (field, through, bind) else {
            self.reporter.reject_hint(
                value.span,
                code::SURFACE,
                "`$recursive` requires `$field`, `$through`, and `$bind` (§10.5)",
                "e.g. `{ \"$field\": \"children\", \"$through\": \".children\", \"$bind\": \"child\" }`",
            );
            return;
        };
        // `$field` names where the nested descendant view appears; it is a field
        // of the covered row.
        if self.receiver_row.as_row().and_then(|r| r.field(&field)).is_none() {
            self.reporter.reject_hint(
                value.span,
                code::SURFACE,
                format!("`$recursive` `$field` `{field}` is not a field of the covered row (§10.5)"),
                "name the descendant collection field the coverage nests under",
            );
        }
        // `$through` yields strict descendants: it must resolve to a keyed row
        // stream (descendant shape + identity).
        let Some(descendant) = self.recursive_view(&through) else {
            return;
        };
        if descendant.as_view().and_then(liasse_expr::RowType::key).is_none() {
            self.reporter.reject_hint(
                value.span,
                code::SURFACE,
                "`$recursive` `$through` must yield keyed descendants (§10.5)",
                "traverse to a keyed collection so each descendant has identity",
            );
            return;
        }
        // `$where`/`$except` are bool predicates over one bound candidate.
        let candidate = descendant
            .as_view()
            .map_or_else(|| descendant.clone(), |row| ExprType::Row(row.clone()));
        for directive in ["$where", "$except"] {
            if let Some(text) = self.recursive_string(value, directive) {
                self.check_recursive_predicate(&bind, &candidate, params, &text);
            }
        }
    }

    /// A required string member of a `$recursive` block, stripped of surrounding
    /// whitespace, or `None` (with no diagnostic — the caller reports the missing
    /// required set together).
    fn recursive_string(&self, value: &liasse_syntax::DocValue, name: &str) -> Option<String> {
        value
            .member(name)
            .and_then(|m| m.value.as_string())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// Type-check a `$recursive` `$through` expression against the covered row,
    /// returning its result type. A non-stream result is rejected.
    fn recursive_view(&mut self, text: &str) -> Option<ExprType> {
        let scope = ModelScope::nested(vec![self.receiver_row.clone()], self.root_row.clone())
            .with_optional_structural("config", self.config.as_ref());
        let sub = self.sources.add_label("recursive-through", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return None;
            }
        };
        self.reject_generated(&parsed, sub);
        // §10.5: `$through` "yields strict descendants of the current row" and the
        // checker "verifies ... acyclicity". A strict-descendant relation is rooted
        // at `.` (the covered row) and descends into a contained collection; a
        // traversal rooted at the package root (`/`), a lexical parent (`^`), an
        // import (`#`), a name/structural binding, or a bare `.` reaches the current
        // row, a sibling, or an ancestor, so the coverage relation is not strict and
        // may be cyclic (a row covering itself). Reject it before the shape check.
        if !strict_descendant_through(parsed.statement()) {
            self.reporter.reject_hint(
                parsed.statement().span,
                code::SURFACE,
                "`$recursive` `$through` must yield strict descendants of the covered row so the \
                 coverage relation is acyclic (§10.5)",
                "root the traversal at `.` and descend into a nested collection, e.g. `.subcompanies`",
            );
            return None;
        }
        match liasse_expr::check_statement(&scope, sub, &parsed) {
            Ok(typed) if typed.ty().as_view().is_some() => Some(typed.ty().clone()),
            Ok(_) => {
                self.reporter.reject_hint(
                    parsed.statement().span,
                    code::SURFACE,
                    "`$recursive` `$through` must resolve to a descendant row stream (§10.5)",
                    "traverse from `.` to a nested collection, e.g. `.subcompanies`",
                );
                None
            }
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }

    /// Type-check a `$recursive` `$where`/`$except` predicate: the candidate row
    /// is bound to `$bind`, and the predicate must be `bool` (§10.5).
    fn check_recursive_predicate(
        &mut self,
        bind: &str,
        candidate: &ExprType,
        params: &[(String, ExprType)],
        text: &str,
    ) {
        let mut scope = ModelScope::nested(vec![self.receiver_row.clone()], self.root_row.clone())
            .with_optional_structural("config", self.config.as_ref())
            .with_binding(bind.to_owned(), candidate.clone());
        for (name, ty) in params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        let sub = self.sources.add_label("recursive-predicate", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return;
            }
        };
        self.reject_generated(&parsed, sub);
        match liasse_expr::check_statement(&scope, sub, &parsed) {
            Ok(typed) if typed.ty().as_scalar() == Some(&Type::Bool) => {}
            Ok(typed) => self.reporter.reject_hint(
                parsed.statement().span,
                code::SURFACE,
                format!(
                    "a `$recursive` predicate must be `bool`, not `{}` (§10.5)",
                    typed.ty().describe()
                ),
                "compare or test a value to produce a boolean",
            ),
            Err(diags) => self.reporter.emit_all(diags),
        }
    }

    /// §8.8/§16.3: a surface `$view` and every `$recursive` relation/predicate is
    /// a pure read position — a materialized, incrementally maintained view (§7.1,
    /// §10.1) — so a generated function (`now()`/`uuid()`) is unreproducible there.
    /// Reject it with the same `M-EXPR` effect-class error the model-root pure
    /// positions emit, reusing [`crate::check::generated_call`]. The span indexes
    /// the expression sub-source `sub`.
    fn reject_generated(&mut self, parsed: &SpannedExpression, sub: liasse_diag::SourceId) {
        if let Some(func) = crate::check::generated_call(crate::check::statement_expr(parsed)) {
            self.reporter.emit(
                liasse_diag::Diagnostic::error(format!(
                    "the generated function `{func}()` may not appear in a pure read position — a \
                     surface `$view` or `$recursive` relation (§8.8)"
                ))
                .code(code::EXPR)
                .primary(liasse_diag::Span::new(sub, parsed.statement().span), "here")
                .help(
                    "generated functions like `now()`/`uuid()` are allowed only in `$default` and \
                     mutation bodies",
                )
                .build(),
            );
        }
    }

    /// Validate a surface `$view`. The §8.8 pure-position gate (an AST walk that
    /// needs no type information) runs for every `$view` — public or role-granted —
    /// so a generated `now()`/`uuid()` is rejected in this materialized read
    /// position regardless of how the surface is exposed. Full type-checking runs
    /// only for a `public` surface: a role `$view` may read `$actor`, whose row
    /// type is resolved by a later pass (the documented seam, see the module docs),
    /// so it is purity-gated but not yet fully typed here.
    fn check_view(&mut self, value: &liasse_syntax::DocValue, params: &[(String, ExprType)], public: bool) {
        let Some(text) = value.as_string() else {
            self.reporter.reject(value.span, code::SURFACE, "`$view` must be an expression string");
            return;
        };
        // The parsed AST's spans index `sub`; the type checker must render its
        // diagnostics against that same source, so `sub` is reused rather than
        // re-registered against unrelated text.
        let sub = self.sources.add_label("view", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return;
            }
        };
        self.reject_generated(&parsed, sub);
        if !public {
            return;
        }
        let mut scope = ModelScope::nested(vec![self.receiver_row.clone()], self.root_row.clone())
            .with_optional_structural("config", self.config.as_ref());
        for (name, ty) in params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        // §7.1/§10.1/§12.2: a surface `$view` result may be a row stream, a
        // single row (a root or struct projection like `. { exact_sum }`), or a
        // scalar (an aggregate/computed value); §12.2 delivers a single-row or
        // scalar result as one object. All three are valid external read
        // results, so only the expression's well-formedness is enforced here.
        if let Err(diags) = liasse_expr::check_statement(&scope, sub, &parsed) {
            self.reporter.emit_all(diags);
        }
    }

    fn surface_muts(&mut self, value: &liasse_syntax::DocValue, public: bool, calls: &mut Vec<DeclName>) {
        let Some(members) = value.as_object() else {
            self.reporter.reject(value.span, code::SURFACE, "a surface `$mut` is a map of external names");
            return;
        };
        for member in members {
            match DeclName::parse(&member.name.text) {
                Ok(name) => calls.push(name),
                Err(reason) => {
                    self.reporter.reject(member.span, code::SURFACE, reason);
                    continue;
                }
            }
            self.check_mut_value(&member.value, public);
        }
    }

    /// §10.1: a surface-mutation value is either a declared-mutation reference (a
    /// single `.name`/`x.y()` string, checked against the model's mutations) or an
    /// inline atomic program (a state-changing expression string, or an array of
    /// statement strings). The reference form is resolved here; the inline form is
    /// held to the §8 statement rules a load must catch (see [`check_inline`]).
    fn check_mut_value(&mut self, value: &liasse_syntax::DocValue, public: bool) {
        if let Some(text) = value.as_string() {
            let sub = self.sources.add_label("surface-mut", text.to_owned());
            let parsed = match parse_expression(sub, text) {
                Ok(parsed) => parsed,
                Err(diags) => {
                    self.reporter.emit_all(diags);
                    return;
                }
            };
            if self.is_reference(parsed.statement()) {
                self.check_mut_reference(parsed.statement(), value);
            } else {
                self.check_inline(&[(parsed.statement, sub)], public);
            }
            return;
        }
        let Some(items) = value.as_array() else {
            self.reporter.reject_hint(
                value.span,
                code::SURFACE,
                "a surface mutation is a declared-mutation reference or an inline program",
                "e.g. `\".add\"`, `\".tasks + { id: @id }\"`, or `[\".done = true\", \"return .\"]`",
            );
            return;
        };
        let mut statements = Vec::new();
        for item in items {
            let Some(text) = item.as_string() else {
                self.reporter.reject(item.span, code::SURFACE, "an inline program statement must be a string");
                return;
            };
            let sub = self.sources.add_label("surface-mut", text.to_owned());
            match parse_expression(sub, text) {
                Ok(parsed) => statements.push((parsed.statement, sub)),
                Err(diags) => {
                    self.reporter.emit_all(diags);
                    return;
                }
            }
        }
        self.check_inline(&statements, public);
    }

    /// Whether a surface-mutation statement is a declared-mutation reference — a
    /// bare `x.y` path or `x.y()` call — rather than an inline program statement.
    fn is_reference(&self, stmt: &liasse_syntax::Stmt) -> bool {
        let StmtKind::Bare(expr) = &stmt.kind else {
            return false;
        };
        matches!(
            &expr.kind,
            ExprKind::Field { .. } | ExprKind::Call { .. }
        )
    }

    /// §5.2/§8.5 and §10.2: hold an inline surface `$mut` program to the statement
    /// rules a load must catch — a write to a read-only computed value, and, in a
    /// public surface, a reference to `$actor`/`$session` an unauthenticated
    /// operation can never bind. Full value/parameter typing of an inline program
    /// stays a documented seam (the runtime resolves it), so a program is not
    /// otherwise re-typed here.
    fn check_inline(&mut self, statements: &[(liasse_syntax::Stmt, liasse_diag::SourceId)], public: bool) {
        for (stmt, source) in statements {
            if public {
                for expr in stmt_exprs(stmt) {
                    self.reject_public_actor(expr, *source);
                }
            }
            if let StmtKind::Assign { target, .. } = &stmt.kind
                && crate::mutation::assigns_read_only_computed(self.root, &self.path, target)
            {
                self.reject_at(
                    *source,
                    target.span,
                    "assignment targets a read-only computed value (§5.2)",
                    "a computed value is determined by its expression; remove the assignment",
                );
            }
        }
    }

    /// §10.2: a public operation has no `$actor`/`$session`, so a public inline
    /// program referencing either is statically invalid — no context can ever bind
    /// it. Reports the first such reference reachable in `expr`.
    fn reject_public_actor(&mut self, expr: &Expr, source: liasse_diag::SourceId) {
        if let ExprKind::Structural(name) = &expr.kind
            && matches!(name.text.as_str(), "actor" | "session")
        {
            self.reject_at(
                source,
                name.span,
                format!("`${}` is not available in a public surface (§10.2)", name.text),
                "a public operation is unauthenticated; expose this through a role with an `$auth`",
            );
            return;
        }
        for child in crate::walk::child_exprs(expr) {
            self.reject_public_actor(child, source);
        }
    }

    /// Emit a surface rejection whose span indexes a `$mut` statement sub-source.
    fn reject_at(
        &mut self,
        source: liasse_diag::SourceId,
        span: liasse_diag::ByteSpan,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) {
        self.reporter.emit(
            liasse_diag::Diagnostic::error(message.into())
                .code(code::SURFACE)
                .primary(liasse_diag::Span::new(source, span), "here")
                .help(hint.into())
                .build(),
        );
    }

    /// §10.1: a declared-mutation reference must name a mutation the model
    /// declares; a bare-collection receiver (a row stream) is not a valid row
    /// mutation receiver (Annex C.10).
    fn check_mut_reference(&mut self, stmt: &liasse_syntax::Stmt, value: &liasse_syntax::DocValue) {
        let StmtKind::Bare(expr) = &stmt.kind else {
            return;
        };
        let reference = match &expr.kind {
            ExprKind::Call { callee, .. } => callee.as_ref(),
            ExprKind::Field { .. } => expr,
            _ => return,
        };
        if let ExprKind::Field { base, member } = &reference.kind
            && let Some(path) = self.resolve_path(base)
        {
            let exists = self
                .mutations
                .iter()
                .any(|m| m.path == path && m.name.as_str() == member.text);
            if !exists {
                self.reporter.reject_hint(
                    value.span,
                    code::SURFACE,
                    format!("surface exposes `{}`, which is not a declared mutation", member.text),
                    "name a mutation declared under a matching `$mut`",
                );
                return;
            }
            // Annex C.10 / §8.2: a row mutation needs a receiver resolving to
            // exactly one row. When the receiver base is a bare collection (a
            // row stream with no key selection) the reference is invalid.
            if self
                .base_type(base)
                .is_some_and(|ty| ty.as_view().is_some())
            {
                self.reporter.reject_hint(
                    value.span,
                    code::SURFACE,
                    format!(
                        "`{}` targets a collection with no row selection; a row mutation needs a selected row (§C.10)",
                        member.text
                    ),
                    "select a row before the mutation name, e.g. `.collection[@id].mutation`",
                );
            }
        }
    }

    /// The type a surface-mutation receiver base addresses, so a bare-collection
    /// receiver (a row stream) can be distinguished from a single selected row.
    /// `.` is the block's scoped receiver row (§10.3); `/` is the model root.
    fn base_type(&self, expr: &Expr) -> Option<ExprType> {
        match &expr.kind {
            ExprKind::Current => Some(self.receiver_row.clone()),
            ExprKind::Root => Some(self.root_row.clone()),
            ExprKind::Field { base, member } => {
                let base_ty = self.base_type(base)?;
                base_ty.as_row().and_then(|row| row.field(&member.text)).cloned()
            }
            // A key selection on a collection stream resolves to one row.
            ExprKind::Select { base, .. } => {
                let base_ty = self.base_type(base)?;
                base_ty.as_view().map(|row| ExprType::Row(row.clone()))
            }
            _ => None,
        }
    }

    /// The absolute receiver path a mutation reference base addresses (selectors
    /// dropped). A `.`-rooted reference is relative to the block's scope path
    /// (§10.3); a `/`-rooted reference is absolute from the model root.
    fn resolve_path(&self, expr: &Expr) -> Option<Vec<String>> {
        match &expr.kind {
            ExprKind::Current => Some(self.path.clone()),
            ExprKind::Root => Some(Vec::new()),
            ExprKind::Field { base, member } => {
                let mut path = self.resolve_path(base)?;
                path.push(member.text.clone());
                Some(path)
            }
            ExprKind::Select { base, .. } => self.resolve_path(base),
            _ => None,
        }
    }
}

/// Whether a `$recursive` `$through` statement navigates strictly downward from
/// the covered row (§10.5). Only a bare view expression can (a `return`/assign is
/// not a traversal); its downward navigation is decided by [`descends_from_current`].
fn strict_descendant_through(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Bare(expr) => descends_from_current(expr),
        StmtKind::Return(_) | StmtKind::Assign { .. } | StmtKind::Clear(_) => false,
    }
}

/// Whether an expression's base spine is a strict-descendant traversal of the
/// covered row (§10.5). It is iff the spine is rooted at `.` (the covered row)
/// and takes at least one downward navigation step — a field access, a row
/// selector, or a same-name traversal — into a contained collection. A spine
/// rooted at the package root (`/`), a lexical parent (`^`), an import (`#`), a
/// name/structural binding, or a bare `.` (zero steps) can reach the current
/// row, a sibling, or an ancestor, so it is not a strict descendant. Filter
/// predicates inside a selector are not part of the spine and may read any scope.
///
/// This is a structural check: a `.`-rooted step through a *ref* field is
/// accepted here, so a relation that revisits a row via refs (needing per-step
/// ref-vs-containment type analysis) is a documented residual beyond this
/// decidable subset. The reported whole-collection / ancestor / self cases —
/// every non-`.`-rooted spine — are rejected.
fn descends_from_current(expr: &Expr) -> bool {
    let mut node = expr;
    let mut steps = 0u32;
    loop {
        match &node.kind {
            ExprKind::Field { base, .. }
            | ExprKind::SameName { base, .. }
            | ExprKind::Select { base, .. } => {
                steps += 1;
                node = base;
            }
            ExprKind::Current => return steps > 0,
            _ => return false,
        }
    }
}

