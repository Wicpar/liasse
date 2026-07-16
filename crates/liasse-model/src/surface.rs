//! Phase 4: surfaces and roles (SPEC.md §10).
//!
//! Validates the static shape of `$public` and `$roles`: a surface's optional
//! `$params`, an optional `$view` that must evaluate to a row stream, and a
//! `$mut` map whose every reference names a mutation the model actually
//! declares (§10.1). A role additionally carries `$auth` and `$members`.
//!
//! CORE scope: a role `$view`/`$members` that reads `$actor` is validated
//! syntactically rather than fully typed (the `$actor` row type is a later
//! pass); nested roles on rows and `$recursive` coverage are documented seams.

use liasse_diag::SourceMap;
use liasse_expr::ExprType;
use liasse_syntax::{parse_expression, Expr, ExprKind, StmtKind};
use liasse_value::Type;

use crate::build::RawSurface;
use crate::doc::DocValueExt;
use crate::mutation::Mutation;
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
) -> Vec<Surface> {
    let root_row = ExprType::Row(resolver.shape_row(root));
    let mut phase = SurfacePhase {
        reporter,
        sources,
        root_row: root_row.clone(),
        receiver_row: root_row,
        path: Vec::new(),
        mutations,
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
    /// The model-root row (`/`).
    root_row: ExprType,
    /// The `.` receiver row for the block being checked (the row at `path`).
    receiver_row: ExprType,
    /// The receiver location of the block being checked (§10.3 role scope).
    path: Vec<String>,
    mutations: &'a [Mutation],
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
                "$view" => {
                    if public {
                        self.check_view(&member.value, &params);
                    }
                }
                "$mut" => self.surface_muts(&member.value, &mut calls),
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
        let scope = ModelScope::nested(vec![self.receiver_row.clone()], self.root_row.clone());
        let sub = self.sources.add_label("recursive-through", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return None;
            }
        };
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

    fn check_view(&mut self, value: &liasse_syntax::DocValue, params: &[(String, ExprType)]) {
        let Some(text) = value.as_string() else {
            self.reporter.reject(value.span, code::SURFACE, "`$view` must be an expression string");
            return;
        };
        let mut scope = ModelScope::nested(vec![self.receiver_row.clone()], self.root_row.clone());
        for (name, ty) in params {
            scope = scope.with_param(name.clone(), ty.clone());
        }
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
        // §7.1/§10.1/§12.2: a surface `$view` result may be a row stream, a
        // single row (a root or struct projection like `. { exact_sum }`), or a
        // scalar (an aggregate/computed value); §12.2 delivers a single-row or
        // scalar result as one object. All three are valid external read
        // results, so only the expression's well-formedness is enforced here.
        if let Err(diags) = liasse_expr::check_statement(&scope, sub, &parsed) {
            self.reporter.emit_all(diags);
        }
    }

    fn surface_muts(&mut self, value: &liasse_syntax::DocValue, calls: &mut Vec<DeclName>) {
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
            self.check_mut_reference(&member.value);
        }
    }

    /// §10.1: a string surface-mutation value that is a declared-mutation
    /// reference must name a mutation the model declares; an inline program (an
    /// array, or a state-changing expression) is accepted structurally.
    fn check_mut_reference(&mut self, value: &liasse_syntax::DocValue) {
        let Some(text) = value.as_string() else {
            // An array is an inline atomic program.
            return;
        };
        let sub = self.sources.add_label("surface-mut", text.to_owned());
        let parsed = match parse_expression(sub, text) {
            Ok(parsed) => parsed,
            Err(diags) => {
                self.reporter.emit_all(diags);
                return;
            }
        };
        let StmtKind::Bare(expr) = &parsed.statement().kind else {
            return;
        };
        let reference = match &expr.kind {
            ExprKind::Call { callee, .. } => callee.as_ref(),
            ExprKind::Field { .. } => expr,
            _ => return, // an inline program (insert/replace/patch/…).
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

