//! Phase 3: mutation programs (SPEC.md §8).
//!
//! Each `$mut` entry is a sequential atomic program. This phase resolves the
//! receiver `.`, infers parameters from their uses (§8.3) merged with an
//! explicit `name({ proto })` prototype, and checks the statements against the
//! rules a load must catch: assignment to a read-only computed value (§5.2,
//! §8.5), a `return` that is not the final statement (§8.10), a non-`bool`
//! `assert` condition (§8.8), and the well-formedness of every value
//! sub-expression through [`liasse_expr`].
//!
//! The inference walk itself lives in [`params`], because §10.1 defines a
//! surface `$view` (and `$recursive` predicate) parameter as inferred "exactly as
//! a mutation parameter is (§8.3)" — one walk serves both, so the read and write
//! contexts cannot drift apart.
//!
//! CORE scope: parameter inference covers the `field = @p`, `collection[@p]`,
//! and `{ field: @p }` contexts §3.2/§8.3 use; deeper cross-call inference and
//! full insert/replace result typing are documented seams. A statement whose
//! form the phase does not model is accepted structurally rather than
//! mis-rejected.

mod helpers;
pub(crate) mod params;

use liasse_diag::{ByteSpan, SourceId, SourceMap};
use liasse_expr::{check_statement, ExprType};
use liasse_syntax::{parse_expression, Arg, BinaryOp, Expr, ExprKind, Stmt, StmtKind};
use liasse_value::Type;

use crate::build::RawMut;
use crate::doc::DocValueExt;
use crate::host::HostDescriptors;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{Node, Shape};

use helpers::{
    collect_param_refs, is_program_call, local_binding_name, receiver_shape, references_deferred,
    resolve_node, uses_mutation_operator, wrap, write_path, BindEnv, Params,
};
// Re-exported for the surface phase's inline-program check (§10.1), which walks a
// statement's expressions to reject a public `$actor`/`$session` reference.
pub(crate) use helpers::stmt_exprs;
// Re-exported for the module phase (§13.8): validating a `$modules` interface
// `$mut` contract name against the same `name({ proto })` prototype grammar.
pub(crate) use helpers::parse_name;

/// A validated mutation: where it is declared, its external name, and its
/// inferred/declared parameter contract.
#[derive(Debug, Clone)]
pub struct Mutation {
    /// The receiver location from the model root (empty = root mutation).
    pub path: Vec<String>,
    /// The external mutation name.
    pub name: DeclName,
    /// The parameter contract (name → type), in name order.
    pub params: Vec<(String, ExprType)>,
}

/// Check every raw mutation, returning the validated set.
// A phase entry point threading the resolved model context (root, resolver,
// buckets, `$config`, and the §16.2 host signatures) into one walk; each input is
// a distinct resolved artifact, not a bundle with its own meaning.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_mutations(
    reporter: &mut Reporter,
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &Shape,
    raw: &[RawMut],
    source_buckets: &[String],
    config: Option<&ExprType>,
    hosts: &HostDescriptors,
) -> Vec<Mutation> {
    let root_row = ExprType::Row(resolver.shape_row(root));
    raw.iter()
        .filter_map(|entry| {
            let mut phase = MutPhase {
                reporter,
                sources,
                root,
                root_row: root_row.clone(),
                source_buckets,
                config,
                hosts,
            };
            phase.check(entry)
        })
        .collect()
}

/// §5.2/§8.5: whether `target`, resolved from the receiver body at `path`, names
/// a read-only computed value — an assignment a load must reject. A surface
/// inline `$mut` program (§10.1) is a mutation program bound by the same rule, so
/// its assignments are judged through this one predicate rather than a divergent
/// copy of the resolution logic.
pub(crate) fn assigns_read_only_computed(root: &Shape, path: &[String], target: &Expr) -> bool {
    matches!(
        resolve_node(target, receiver_shape(root, path), root),
        Some(Node::Scalar(field)) if !field.is_writable()
    )
}

struct MutPhase<'a, 'b> {
    reporter: &'a mut Reporter<'b>,
    sources: &'a mut SourceMap,
    root: &'a Shape,
    root_row: ExprType,
    /// Absolute paths of source-backed bucket collections (read-only, §14.4).
    source_buckets: &'a [String],
    /// A module package's `$config` struct row (§13.1), bound as the `$config`
    /// structural so a module mutation body reads it; `None` outside a module.
    config: Option<&'a ExprType>,
    /// The resolved `$requires` host-namespace signatures (§16.2), so a `@param`
    /// used only as a host-namespace call argument (`ns.fn(@p)`) is inferred into
    /// the contract from the host function's declared argument type (§8.3/§16.4).
    hosts: &'a HostDescriptors,
}

impl MutPhase<'_, '_> {
    fn check(&mut self, entry: &RawMut) -> Option<Mutation> {
        let (base, prototype) = match parse_name(&entry.name) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.reporter.reject_hint(
                    entry.span,
                    code::MUTATION,
                    reason,
                    "declare the prototype as `name({ param: type })` (§8.3)",
                );
                return None;
            }
        };
        let name = match DeclName::parse(&base) {
            Ok(name) => name,
            Err(reason) => {
                self.reporter.reject(entry.span, code::MUTATION, reason);
                return None;
            }
        };
        let receiver = self.receiver_type(&entry.path)?;
        let statements = self.parse_program(entry)?;

        let mut params = Params::from_prototype(prototype);
        self.infer_params(&statements, &receiver, &mut params);
        self.check_param_inference(&statements, &params);

        let scope = self.build_scope(&receiver, &params);
        self.check_statements(entry, &statements, &scope);

        Some(Mutation {
            path: entry.path.clone(),
            name,
            params: params.into_pairs(),
        })
    }

    /// The `.` type of the receiver at `path` (§8.2).
    fn receiver_type(&self, path: &[String]) -> Option<ExprType> {
        let mut current = self.root_row.clone();
        for segment in path {
            let row = current.as_row()?;
            let field = row.field(segment)?;
            current = match field {
                ExprType::View(row) | ExprType::Row(row) => ExprType::Row(row.clone()),
                _ => return None,
            };
        }
        Some(current)
    }

    /// Each parsed statement paired with the sub-source its spans index, so a
    /// self-built diagnostic points at the right bytes.
    fn parse_program(&mut self, entry: &RawMut) -> Option<Vec<(Stmt, SourceId)>> {
        let bodies: Vec<&str> = if let Some(text) = entry.body.as_string() {
            vec![text]
        } else if let Some(items) = entry.body.as_array() {
            items.iter().filter_map(DocValueExt::as_string).collect()
        } else {
            self.reporter.reject_hint(
                entry.body.span,
                code::MUTATION,
                "a mutation is a statement string or an array of statement strings",
                "e.g. `\".done = true\"` or `[\".done = true\", \"return .\"]`",
            );
            return None;
        };
        if bodies.is_empty() {
            self.reporter.reject(entry.span, code::MUTATION, "a mutation program has no statements");
            return None;
        }
        let mut statements = Vec::new();
        for text in bodies {
            statements.push(self.parse_stmt(text)?);
        }
        Some(statements)
    }

    fn parse_stmt(&mut self, text: &str) -> Option<(Stmt, SourceId)> {
        let sub = self.sources.add_label("mut", text.to_owned());
        match parse_expression(sub, text) {
            Ok(parsed) => Some((parsed.statement, sub)),
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }

    fn build_scope(&self, receiver: &ExprType, params: &Params) -> ModelScope {
        let mut scope = ModelScope::nested(vec![receiver.clone()], self.root_row.clone())
            .with_optional_structural("config", self.config);
        for (name, ty) in params.iter() {
            scope = scope.with_param(name.clone(), ty.clone());
        }
        scope
    }

    /// §8.3: infer each `@name` from its use context, through the shared
    /// inference walk [`params::Inference`] a surface `$view` and `$recursive`
    /// predicate reuse for the same rule in a read position (§10.1).
    fn infer_params(
        &self,
        statements: &[(Stmt, SourceId)],
        receiver: &ExprType,
        params: &mut Params,
    ) {
        let program: Vec<&Stmt> = statements.iter().map(|(stmt, _)| stmt).collect();
        params::Inference::program(self.root_row.clone(), self.hosts).infer(
            &program,
            receiver,
            &BindEnv::new(),
            params,
        );
    }

    /// §8.3: every referenced `@name` must resolve to one contract type, whether
    /// inferred from a use context or fixed by an explicit prototype. A parameter
    /// used only in a position that constrains no type (e.g. `return @value`)
    /// leaves more than one valid shape, so the package cannot load.
    fn check_param_inference(&mut self, statements: &[(Stmt, SourceId)], params: &Params) {
        let mut reported: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (stmt, source) in statements {
            let mut refs = Vec::new();
            for expr in stmt_exprs(stmt) {
                collect_param_refs(expr, &mut refs);
            }
            for (name, span) in refs {
                if !reported.insert(name.to_owned()) {
                    continue;
                }
                if !params.contains(name) {
                    self.reject_at(
                        *source,
                        span,
                        &format!("parameter `@{name}` cannot be inferred to a single type (§8.3)"),
                        "give it a type with a prototype, e.g. `name({ value: text })`",
                    );
                } else if params.conflicts(name) {
                    self.reject_at(
                        *source,
                        span,
                        &format!("parameter `@{name}` is used with two incompatible types (§8.3)"),
                        "use the parameter consistently, or fix a prototype so all uses agree",
                    );
                }
            }
        }
    }

    fn check_statements(
        &mut self,
        entry: &RawMut,
        statements: &[(Stmt, SourceId)],
        scope: &ModelScope,
    ) {
        let receiver_shape = receiver_shape(self.root, &entry.path);
        let last = statements.len().saturating_sub(1);
        // Local bindings introduced by earlier `local = ...` statements are visible
        // to later ones (§8, Annex C.9), so the scope grows as the program is walked.
        let mut scope = scope.clone();
        // Locals bound to a value the CORE phase cannot type — a mutation-operator
        // result (insert/replace/delete/patch) or a host/program-call result
        // (§8.11/§16.4) — are left UNBOUND. A later value expression built over such
        // a local is itself untypeable here, so the §16.2 deferral is TRANSITIVE:
        // track the deferred names and accept a reference to one structurally rather
        // than reject it with a spurious "unknown name" (full typing runs under a
        // host-resolved load).
        let mut deferred: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (index, (stmt, source)) in statements.iter().enumerate() {
            self.check_readonly(stmt, &entry.path, *source);
            match &stmt.kind {
                StmtKind::Return(_) if index != last => self.reject_at(
                    *source,
                    stmt.span,
                    "`return` may appear only as the final statement (§8.10)",
                    "move `return` to the end of the program",
                ),
                StmtKind::Assign { target, value } => {
                    if let Some(local) = local_binding_name(target) {
                        // `local = value_or_mutation_result` (Annex C.9): check the
                        // value, then bind the local for later statements. An
                        // insert/replace or host/program-call result the CORE phase
                        // cannot type — or a value built over an already-deferred
                        // local — stays a documented seam: left unbound and tracked
                        // as deferred, so a later reference to it defers too.
                        if uses_mutation_operator(value)
                            || is_program_call(value)
                            || references_deferred(value, &deferred)
                        {
                            deferred.insert(local.to_owned());
                        } else if let Some(typed) = self.type_value(value, &scope, *source) {
                            let ty = typed.ty().clone();
                            scope = scope.with_binding(local.to_owned(), ty);
                        }
                    } else {
                        self.check_assign(target, value, receiver_shape, &scope, *source);
                    }
                }
                StmtKind::Bare(expr) => self.check_bare(expr, &scope, *source),
                StmtKind::Clear(target) => self.check_clear(target, receiver_shape, *source),
                StmtKind::Return(_) => {}
            }
        }
    }

    /// §8.5: the clear operator `field -` removes an *optional* field's value.
    /// Applied to a required field it has no defined meaning (it would leave a
    /// row missing a required value), so the program is rejected at load.
    fn check_clear(&mut self, target: &Expr, receiver_shape: &Shape, source: SourceId) {
        let optional = match resolve_node(target, receiver_shape, self.root) {
            Some(Node::Scalar(field)) => matches!(field.ty, Type::Optional(_)),
            Some(Node::Reference(reference)) => reference.optional,
            // A non-scalar target (or one this phase cannot resolve) is accepted
            // structurally rather than mis-rejected.
            _ => return,
        };
        if !optional {
            self.reject_at(
                source,
                target.span,
                "the clear operator `-` applies only to an optional field (§8.5)",
                "mark the field `$optional`, or assign a value instead of clearing it",
            );
        }
    }

    /// §14.4: a source-backed bucket collection's rows are read-only, so any
    /// insert/replace/delete/patch targeting one rejects.
    fn check_readonly(&mut self, stmt: &Stmt, receiver: &[String], source: SourceId) {
        let target = match &stmt.kind {
            StmtKind::Assign { target, .. } => Some(target),
            StmtKind::Bare(expr) => match &expr.kind {
                ExprKind::Binary { op: liasse_syntax::BinaryOp::Add | liasse_syntax::BinaryOp::Sub, lhs, .. } => Some(lhs.as_ref()),
                ExprKind::Unary { op: liasse_syntax::UnaryOp::Neg, operand } => Some(operand.as_ref()),
                ExprKind::Block { base, .. } => Some(base.as_ref()),
                _ => None,
            },
            _ => None,
        };
        let Some(target) = target else { return };
        let Some(path) = write_path(target, receiver) else { return };
        if self.source_buckets.contains(&path) {
            self.reject_at(
                source,
                target.span,
                "a source-backed bucket collection is read-only (§14.4)",
                "change the bucket's source rows or the tables they reference instead",
            );
        }
    }

    /// Emit a mutation rejection whose span indexes the statement sub-source.
    fn reject_at(&mut self, source: SourceId, span: ByteSpan, message: &str, hint: &str) {
        self.reporter.emit(
            liasse_diag::Diagnostic::error(message.to_owned())
                .code(code::MUTATION)
                .primary(liasse_diag::Span::new(source, span), "here")
                .help(hint.to_owned())
                .build(),
        );
    }

    fn check_assign(
        &mut self,
        target: &Expr,
        value: &Expr,
        receiver_shape: &Shape,
        scope: &ModelScope,
        source: SourceId,
    ) {
        // Resolve the target field's type up front so the `self.root` borrow is
        // released before the `&mut self` type-check below.
        let target_ty = match resolve_node(target, receiver_shape, self.root) {
            Some(Node::Scalar(field)) if !field.is_writable() => {
                self.reject_at(
                    source,
                    target.span,
                    "assignment targets a read-only computed value (§5.2)",
                    "a computed value is determined by its expression; remove the assignment",
                );
                return;
            }
            Some(Node::Scalar(field)) => Some(field.ty.clone()),
            _ => None,
        };
        // Best-effort typing of the assigned value; mutation-operator RHS forms
        // are accepted structurally. When both the target field type and the
        // value type are known, the value must be assignable to the field (§8.5,
        // the §8.3 contract type of a parameter used as the value).
        if let Some(typed) = self.type_value(value, scope, source)
            && let Some(field_ty) = &target_ty
            && !crate::check::value_assignable(&typed, field_ty)
        {
            self.reject_at(
                source,
                value.span,
                &format!(
                    "this value has type `{}` but the field expects `{}` (§8.5)",
                    typed.ty().describe(),
                    field_ty.name()
                ),
                "assign a value of the field's declared type",
            );
        }
    }

    fn check_bare(&mut self, expr: &Expr, scope: &ModelScope, source: SourceId) {
        if let ExprKind::Call { callee, args } = &expr.kind
            && matches!(&callee.kind, ExprKind::Name(id) if id.text == "assert")
        {
            self.check_assert(expr, args, scope, source);
            return;
        }
        // §8.5/§6.3/A.9: a direct `collection - { object }` delete names its removed
        // rows by key. The object operand is authoring syntax for the target's
        // composite `$key` tuple and must be a *key of that target*, so it is
        // validated at load through the SAME coercion the `[{..}]` selector, `==`,
        // and `in` apply — a wrong-typed, wrong-arity, or extra-field operand is
        // rejected here rather than silently no-ooping (or over-deleting) at
        // runtime. `type_value` below accepts the delete's mutation operator
        // structurally, so this is the operand's only load-time gate.
        if let ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } = &expr.kind
            && let Err(diags) = liasse_expr::check_composite_delete_operand(scope, source, lhs, rhs)
        {
            self.reporter.emit_all(diags);
        }
        self.type_value(expr, scope, source);
    }

    fn check_assert(&mut self, expr: &Expr, args: &[Arg], scope: &ModelScope, source: SourceId) {
        let Some(Arg::Positional(cond)) = args.first() else {
            self.reject_at(source, expr.span, "`assert` takes a condition and a message", "e.g. `assert(.balance >= @amount, 'Insufficient funds')`");
            return;
        };
        if let Some(typed) = self.type_value(cond, scope, source)
            && typed.ty().as_scalar() != Some(&Type::Bool)
        {
            self.reject_at(source, cond.span, "an `assert` condition must be `bool`", "compare or test a value to produce a boolean");
        }
    }

    /// Type-check a pure value/view sub-expression against `source` (where its
    /// spans are valid), skipping (and accepting) mutation-operator forms the
    /// value checker cannot type.
    fn type_value(
        &mut self,
        expr: &Expr,
        scope: &ModelScope,
        source: SourceId,
    ) -> Option<liasse_expr::TypedExpr> {
        // Mutation-operator forms (insert/replace/delete/patch) and program-level
        // calls (in-program mutation calls §8.11, host-namespace calls §16.4,
        // and `erase`/`reinsert` operations §21) are not typed as value
        // expressions; the phase accepts them structurally.
        if uses_mutation_operator(expr) || is_program_call(expr) {
            return None;
        }
        let spanned = wrap(expr.clone());
        match check_statement(scope, source, &spanned) {
            Ok(typed) => Some(typed),
            Err(diags) => {
                self.reporter.emit_all(diags);
                None
            }
        }
    }
}
