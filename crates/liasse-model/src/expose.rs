//! Phase: module `$expose` interface capture and typing (SPEC.md §13.8).
//!
//! A module package's top-level `$expose` binds each child-visible interface
//! name to a private `$view` (the projection a parent or peer observes) and an
//! optional `$mut` map of bound mutation contracts. The header phase
//! ([`crate::module::check_expose`]) validates the block's *grammar* before the
//! tree exists; this pass runs after the state tree is built, so it can type each
//! exposed `$view` against the module's own root scope and check that every bound
//! `$mut` reference names a mutation the module declares.
//!
//! The captured [`ExposedInterface`]s are retained on the [`Model`](crate::Model)
//! so the runtime can evaluate the exposed view against a child instance and
//! resolve an interface-addressed mutation to its private receiver (§13.8/§13.9).
//! Structural satisfaction of a *parent's* `$interfaces` contract by these
//! exposures is a cross-package check the composition runtime performs at install
//! (§13.3 "Loading validates ... interfaces ... before the instance becomes
//! active"); it is a documented seam here.

use liasse_diag::SourceMap;
use liasse_expr::ExprType;
use liasse_syntax::{parse_expression, DocValue, ExprKind, StmtKind};

use crate::doc::DocValueExt;
use crate::mutation::Mutation;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{ExprSource, Shape};

/// One exposed module interface (§13.8): the child-visible handle name, the
/// private `$view` projection a consumer observes through it, and the private
/// mutations bound to its callable contracts.
#[derive(Debug, Clone)]
pub struct ExposedInterface {
    /// The interface handle name a parent or peer addresses (`::templates`).
    pub name: DeclName,
    /// The bound `$view` projection over the module's own state, when declared.
    /// Its projected fields are the only state that crosses the boundary — a
    /// private field the view does not project is unreachable (§13.8 isolation).
    pub view: Option<ExprSource>,
    /// The callable contracts this interface binds to private mutations (§13.8).
    pub muts: Vec<ExposedMut>,
}

/// One bound interface mutation (§13.8): the contract name a consumer calls and
/// the private mutation reference or inline program it resolves to.
#[derive(Debug, Clone)]
pub struct ExposedMut {
    /// The interface mutation contract name a consumer calls (`::templates.create`).
    pub name: DeclName,
    /// The bound private mutation reference (`.create_template`) or inline program.
    pub binding: ExprSource,
}

/// Type each `$expose` interface's `$view` against the module root and validate
/// its `$mut` bindings, returning the captured interfaces (§13.8). An absent or
/// malformed block yields no interfaces; grammar errors are already reported by
/// the header phase, so this pass only adds the typing diagnostics.
pub(crate) fn check_and_capture(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &Shape,
    mutations: &[Mutation],
    expose: Option<&DocValue>,
    config: Option<&ExprType>,
) -> Vec<ExposedInterface> {
    let Some(interfaces) = expose.and_then(DocValueExt::as_object) else {
        return Vec::new();
    };
    let root_row = ExprType::Row(resolver.shape_row(root));
    let mut phase = ExposePhase { reporter, sources, root_row, mutations, config: config.cloned() };
    let mut out = Vec::new();
    for interface in interfaces {
        if let Some(exposed) = phase.interface(interface) {
            out.push(exposed);
        }
    }
    out
}

struct ExposePhase<'a, 'b> {
    reporter: &'a mut Reporter<'b>,
    sources: &'a mut SourceMap,
    root_row: ExprType,
    mutations: &'a [Mutation],
    /// The module package's `$config` struct row (§13.1), bound as the `$config`
    /// structural so an exposed `$view` reads it (`currency: $config.currency`);
    /// `None` when the module declares no `$config`.
    config: Option<ExprType>,
}

impl ExposePhase<'_, '_> {
    fn interface(&mut self, interface: &liasse_syntax::DocMember) -> Option<ExposedInterface> {
        let name = DeclName::parse(&interface.name.text).ok()?;
        let members = interface.value.as_object()?;
        let mut view = None;
        let mut muts = Vec::new();
        for member in members {
            match member.name.text.as_str() {
                "$view" => view = self.view(&member.value),
                "$mut" => self.muts(&member.value, &mut muts),
                _ => {} // grammar already reported by the header phase.
            }
        }
        Some(ExposedInterface { name, view, muts })
    }

    /// Type an exposed `$view` against the module root and capture its source. A
    /// `$view` that does not type-check is reported; the exposure is still
    /// captured (as an [`ExprSource`]) so a later phase reports against it once.
    fn view(&mut self, value: &DocValue) -> Option<ExprSource> {
        let text = value.as_string()?;
        let text = text.trim_start().strip_prefix('=').map_or(text, str::trim).to_owned();
        let scope = ModelScope::nested(vec![self.root_row.clone()], self.root_row.clone())
            .with_optional_structural("config", self.config.as_ref());
        let sub = self.sources.add_label("expose-view", text.clone());
        match parse_expression(sub, &text) {
            Ok(parsed) => {
                // §13.4: an `$expose` `$view` MAY read a parent-provided surface
                // (`company: #company.name`) the standalone model cannot type — the
                // projection is resolved only when the module is installed. Defer
                // such a view's type-check to the composition runtime
                // (`compile_exposed_views`), which binds the resolved import types;
                // an import-free view is still typed here so its errors surface early.
                if !parsed.references_import()
                    && let Err(diags) = liasse_expr::check_statement(&scope, sub, &parsed)
                {
                    self.reporter.emit_all(diags);
                }
            }
            Err(diags) => self.reporter.emit_all(diags),
        }
        Some(ExprSource { text, span: value.span })
    }

    /// Validate an interface `$mut` map: each binding names a declared mutation or
    /// is an inline single-statement program (§13.8). Captures each as an
    /// [`ExposedMut`] the runtime resolves to a private receiver at call time.
    fn muts(&mut self, value: &DocValue, out: &mut Vec<ExposedMut>) {
        let Some(members) = value.as_object() else {
            self.reporter.reject(value.span, code::MODULE, "an `$expose` `$mut` maps contract names to bindings");
            return;
        };
        for member in members {
            let Ok(name) = DeclName::parse(&member.name.text) else {
                self.reporter.reject(member.span, code::MODULE, format!("`{}` is not a valid interface mutation name", member.name.text));
                continue;
            };
            let text = member.value.as_string().unwrap_or_default().to_owned();
            self.check_binding(&member.value, &text);
            out.push(ExposedMut { name, binding: ExprSource { text, span: member.value.span } });
        }
    }

    /// A bound `$mut` value is either a declared-mutation reference (`.create`,
    /// `.templates[@t].disable`) — which must name a mutation the module declares —
    /// or an inline program (an array, or a bare state-changing expression), which
    /// is accepted structurally (§13.8, corpus NOTES). Mirrors the surface-mutation
    /// reference rule.
    fn check_binding(&mut self, value: &DocValue, text: &str) {
        let Some(text) = Some(text).filter(|t| !t.is_empty()) else {
            // An array is an inline atomic program (no reference to resolve).
            return;
        };
        let sub = self.sources.add_label("expose-mut", text.to_owned());
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
            _ => return, // an inline insert/replace/patch program.
        };
        if let ExprKind::Field { base, member } = &reference.kind
            && let Some(path) = resolve_path(base)
        {
            let exists = self
                .mutations
                .iter()
                .any(|m| m.path == path && m.name.as_str() == member.text);
            if !exists {
                self.reporter.reject_hint(
                    value.span,
                    code::MODULE,
                    format!("`$expose` binds `{}`, which is not a declared mutation", member.text),
                    "name a mutation the module declares under `$mut`, or write an inline program",
                );
            }
        }
    }
}

/// The absolute receiver path a bound `$mut` reference base addresses, selectors
/// dropped. A `.`-rooted reference is the module root; a `/`-rooted reference is
/// absolute from the root (both resolve to the same module tree here).
fn resolve_path(expr: &liasse_syntax::Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ExprKind::Current | ExprKind::Root => Some(Vec::new()),
        ExprKind::Field { base, member } => {
            let mut path = resolve_path(base)?;
            path.push(member.text.clone());
            Some(path)
        }
        ExprKind::Select { base, .. } => resolve_path(base),
        _ => None,
    }
}
