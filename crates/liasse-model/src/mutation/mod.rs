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
//! CORE scope: parameter inference covers the `field = @p`, `collection[@p]`,
//! and `{ field: @p }` contexts §3.2/§8.3 use; deeper cross-call inference and
//! full insert/replace result typing are documented seams. A statement whose
//! form the phase does not model is accepted structurally rather than
//! mis-rejected.

mod helpers;

use liasse_diag::{ByteSpan, SourceId, SourceMap};
use liasse_expr::{check_statement, ExprType, RowType};
use liasse_syntax::{parse_expression, Arg, BinaryOp, Expr, ExprKind, Selector, Stmt, StmtKind};
use liasse_value::Type;

use crate::build::RawMut;
use crate::doc::DocValueExt;
use crate::host::HostDescriptors;
use crate::names::DeclName;
use crate::report::{code, Reporter};
use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{Node, Shape};
use crate::walk::child_exprs;

use helpers::{
    arg_expr, collect_param_refs, host_call_target, is_program_call, is_scalar_binop,
    local_binding_name, receiver_shape, record, resolve_node, uses_mutation_operator, wrap,
    write_path, BindEnv, Params,
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

    /// §8.3: infer each `@name` from its use context.
    fn infer_params(
        &self,
        statements: &[(Stmt, SourceId)],
        receiver: &ExprType,
        params: &mut Params,
    ) {
        let binds = BindEnv::new();
        for (stmt, _) in statements {
            // A scalar assignment `field = @p` constrains `@p` to the target
            // field's type (§8.3); the general expression walk below does not
            // relate the assignment's two sides, so it is inferred here.
            if let StmtKind::Assign { target, value } = &stmt.kind
                && let ExprKind::Param(id) = &value.kind
                && let Some(ty) = self.resolve(target, receiver, &binds)
                && ty.as_scalar().is_some()
            {
                record(params, &id.text, ty);
            }
            for expr in stmt_exprs(stmt) {
                self.infer_in(expr, receiver, &binds, params);
            }
        }
        // §8.3/§16.4/§11.5: a *second* pass fills a host-namespace call argument's
        // parameter — the login shape `identity = webauthn.verify(@response)` uses
        // `@response` nowhere else, so it must become a real contract parameter that
        // the caller passes explicitly in the §12.1 closed argument object. Running
        // it after every prototype/state-anchored use above makes it order
        // independent and strictly gap-filling: a parameter already pinned by a
        // prototype or a state use keeps that stronger type, and a mismatch against
        // the host signature is enforced at the call boundary (§16.2/§16.5), not as
        // a load conflict here.
        for (stmt, _) in statements {
            for expr in stmt_exprs(stmt) {
                self.infer_host_args(expr, params);
            }
        }
    }

    /// §8.3/§16.4: fill a host-namespace call argument's parameter type when no
    /// prior use pinned it. A bare `@param` positional argument of a host call
    /// `ns.fn(…, @p, …)` takes the host function's declared argument type at that
    /// position when the resolved `$requires` descriptor is available (§16.2);
    /// otherwise it takes the permissive top type `json`, whose real validation is
    /// the runtime host-call boundary (§16). Either way the parameter BINDS.
    fn infer_host_args(&self, expr: &Expr, params: &mut Params) {
        if let ExprKind::Call { callee, args } = &expr.kind
            && let Some((namespace, function)) = host_call_target(callee)
        {
            let signature = self.hosts.op(namespace, function);
            for (index, arg) in args.iter().enumerate() {
                if let ExprKind::Param(id) = &arg_expr(arg).kind
                    && !params.contains(&id.text)
                {
                    let ty = signature
                        .and_then(|op| op.params().get(index))
                        .map_or_else(|| ExprType::scalar(Type::Json), |arg_ty| ExprType::scalar(arg_ty.clone()));
                    record(params, &id.text, ty);
                }
            }
        }
        for child in child_exprs(expr) {
            self.infer_host_args(child, params);
        }
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

    fn infer_in(
        &self,
        expr: &Expr,
        receiver: &ExprType,
        binds: &BindEnv,
        params: &mut Params,
    ) {
        match &expr.kind {
            // `collection[@p]` — @p inherits the collection key type. A composite
            // key is addressed by an object selector `[{ comp: @p, ... }]` (§6.3),
            // whose members name each key component; each `@p` then inherits that
            // component's type from the composite key struct, by name and not by
            // position (Annex A.9).
            ExprKind::Select { base, selector: Selector::Keys(keys) } => {
                if let Some(key_ty) = self.select_key_type(base, receiver, binds) {
                    for key in keys {
                        match &key.kind {
                            ExprKind::Param(id) => record(params, &id.text, key_ty.clone()),
                            ExprKind::Object(members) => {
                                self.infer_composite_key(members, &key_ty, params);
                            }
                            _ => {}
                        }
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // `collection + { field: @p }` insert — @p inherits the target
                // collection's field type, not the receiver's (§8.3).
                if *op == BinaryOp::Add
                    && let (Some(row), ExprKind::Object(members)) =
                        (self.target_row(lhs, receiver, binds), &rhs.kind)
                {
                    self.infer_object(members, &ExprType::Row(row), params);
                }
                // `collection - key` delete — the operand is the removed row's key,
                // so a bare `@p` inherits the collection's key type (§8.5). A
                // composite key is addressed by an object operand `{ comp: @p, ... }`
                // (§6.3, A.9), mirroring the `[{..}]` selector: each `@p` inherits
                // its named component's type from the composite key struct.
                if *op == BinaryOp::Sub
                    && let Some(key_ty) = self.select_key_type(lhs, receiver, binds)
                {
                    match &rhs.kind {
                        ExprKind::Param(id) => record(params, &id.text, key_ty),
                        ExprKind::Object(members) => {
                            self.infer_composite_key(members, &key_ty, params);
                        }
                        _ => {}
                    }
                }
                // A scalar comparison or arithmetic relates its two operands to
                // one type, so a bare `@p` operand inherits the sibling's scalar
                // type: `assert(.balance >= @amount)`, `.balance - @amount`, and
                // ref-key comparisons like `x.account == @account` (§8.3).
                if is_scalar_binop(*op) {
                    self.infer_scalar_operand(lhs, rhs, receiver, binds, params);
                    self.infer_scalar_operand(rhs, lhs, receiver, binds, params);
                }
            }
            // `row_source { field = @p }` / `{ field: @p }` patch — @p inherits
            // the patched row's field type, in both the projection (`field:`)
            // and assignment (`field =`) member forms (§8.6).
            ExprKind::Block { base, members } => {
                if let Some(row) = self.target_row(base, receiver, binds) {
                    self.infer_object(members, &ExprType::Row(row), params);
                }
            }
            // `{ field: @p }` against the receiver row.
            ExprKind::Object(members) => {
                self.infer_object(members, receiver, params);
            }
            // A temporal window selector `.base.$at(t)` / `.base.$between(a, b)`
            // takes `timestamp` instants (§14.1); a bare `@param` argument inherits
            // `timestamp`. The general checker otherwise ignores call arguments, so
            // a parameter used *only* here would stay uninferred (§8.3).
            ExprKind::Call { callee, args } => {
                if let ExprKind::Field { member, .. } = &callee.kind
                    && member.structural
                    && matches!(member.text.as_str(), "at" | "between")
                {
                    for arg in args {
                        if let ExprKind::Param(id) = &arg_expr(arg).kind {
                            record(params, &id.text, ExprType::scalar(Type::timestamp()));
                        }
                    }
                }
                // A host-namespace call argument (`ns.fn(@p)`, §16.4) is inferred in
                // a separate pass ([`Self::infer_host_args`]) that runs after every
                // state-anchored use, so a prototype- or state-typed parameter keeps
                // its stronger type and the host signature is enforced at the call
                // boundary (§16.2/§16.5) rather than becoming a load conflict here.
            }
            _ => {}
        }
        // Recurse into children, threading a row binding introduced by a
        // filtered selector `[:x | ...]` so that `x.field == @p` inside the
        // condition resolves `x` to a row of the selected collection (§6.4).
        if let ExprKind::Select { base, selector: Selector::Bind { name, condition } } = &expr.kind {
            self.infer_in(base, receiver, binds, params);
            if let Some(cond) = condition {
                let mut inner = binds.clone();
                if let Some(row) = self.target_row(base, receiver, binds) {
                    inner.insert(name.text.clone(), ExprType::Row(row));
                }
                self.infer_in(cond, receiver, &inner, params);
            }
        } else {
            for child in child_exprs(expr) {
                self.infer_in(child, receiver, binds, params);
            }
        }
    }

    /// `@p` (`param_side`) inherits `other_side`'s type when the sibling
    /// operand resolves to a scalar (§8.3).
    fn infer_scalar_operand(
        &self,
        param_side: &Expr,
        other_side: &Expr,
        receiver: &ExprType,
        binds: &BindEnv,
        params: &mut Params,
    ) {
        if let ExprKind::Param(id) = &param_side.kind
            && let Some(ty) = self.resolve(other_side, receiver, binds)
            && ty.as_scalar().is_some()
        {
            record(params, &id.text, ty);
        }
    }

    /// The row type a collection/row source expression addresses, for insert and
    /// patch parameter inference.
    fn target_row(&self, expr: &Expr, receiver: &ExprType, binds: &BindEnv) -> Option<RowType> {
        match self.resolve(expr, receiver, binds)? {
            ExprType::View(row) | ExprType::Row(row) => Some(row),
            _ => None,
        }
    }

    fn infer_object(
        &self,
        members: &[liasse_syntax::BlockMember],
        receiver: &ExprType,
        params: &mut Params,
    ) {
        use liasse_syntax::BlockMemberKind;
        let row = receiver.as_row();
        for member in members {
            // A member binds a field in the projection (`field: value`),
            // assignment (`field = value`), or `@name` shorthand form. The
            // `@name` shorthand means `name = @name` (§8.6): the field is the
            // parameter's own name, so the parameter inherits that field's type.
            let (field, value): (&str, &Expr) = match &member.kind {
                BlockMemberKind::Named { name, value: Some(value) } => (&name.text, value),
                BlockMemberKind::Assign { target, value } => (&target.text, value),
                BlockMemberKind::Shorthand(value) => {
                    if let ExprKind::Param(param) = &value.kind
                        && let Some(field_ty) = row.and_then(|r| r.field(&param.text))
                    {
                        record(params, &param.text, field_ty.clone());
                    }
                    continue;
                }
                _ => continue,
            };
            match &value.kind {
                // `field: @p` / `field = @p` — @p inherits the field's type.
                ExprKind::Param(param) => {
                    if let Some(field_ty) = row.and_then(|r| r.field(field)) {
                        record(params, &param.text, field_ty.clone());
                    }
                }
                // `field: { ... }` — a nested struct-literal value (§5.3): its
                // members share the containing row's insertion but infer against
                // the field's *own* row shape, recursively.
                ExprKind::Object(inner) => {
                    if let Some(ExprType::Row(nested) | ExprType::View(nested)) =
                        row.and_then(|r| r.field(field))
                    {
                        self.infer_object(inner, &ExprType::Row(nested.clone()), params);
                    }
                }
                _ => {}
            }
        }
        // §15.4/§15.6: a hypothetical meter-accessor or spend context supplies
        // the reserved structural members `$time` (timestamp) and `$amount`
        // (numeric); a parameter in either position inherits that fixed type even
        // though the surrounding accessor call is an opaque runtime seam.
        self.infer_context_object(members, params);
    }

    /// An object key selector `[{ comp: @p, ... }]` (§6.3): each member names a
    /// key component, so its parameter inherits that component's type — matched by
    /// component name, not member position (Annex A.9). Both multi-component key
    /// forms spell a component by name: a composite key by its `$key`-ordered
    /// components, and a struct `$key` (A.8) by its field-name-ordered members;
    /// each addressed the same way here.
    fn infer_composite_key(
        &self,
        members: &[liasse_syntax::BlockMember],
        key_ty: &ExprType,
        params: &mut Params,
    ) {
        use liasse_syntax::BlockMemberKind;
        let Some(key) = key_ty.as_scalar() else { return };
        for member in members {
            let (comp, value) = match &member.kind {
                BlockMemberKind::Named { name, value: Some(value) } => (&name.text, value),
                BlockMemberKind::Assign { target, value } => (&target.text, value),
                _ => continue,
            };
            let component = match key {
                Type::Composite(components) => {
                    components.iter().find(|(name, _)| name == comp).map(|(_, ty)| ty)
                }
                Type::Struct(fields) => fields.field(comp),
                _ => None,
            };
            if let ExprKind::Param(param) = &value.kind
                && let Some(ty) = component
            {
                record(params, &param.text, ExprType::scalar(ty.clone()));
            }
        }
    }

    /// §15 spend/accessor context: infer a parameter used as the reserved
    /// structural `$time` (timestamp) or `$amount` (numeric decimal) member of a
    /// context object (Annex §15 grammar: `$time?: timestamp-expression`,
    /// `$amount?: numeric-expression`).
    fn infer_context_object(
        &self,
        members: &[liasse_syntax::BlockMember],
        params: &mut Params,
    ) {
        use liasse_syntax::BlockMemberKind;
        for member in members {
            // A structural context member `$time`/`$amount` parses as a directive
            // (`$name: expr`).
            let BlockMemberKind::Directive { name, value } = &member.kind else {
                continue;
            };
            let ty = match name.text.as_str() {
                "time" => Type::timestamp(),
                "amount" => Type::Decimal,
                _ => continue,
            };
            if let ExprKind::Param(param) = &value.kind {
                record(params, &param.text, ExprType::scalar(ty));
            }
        }
    }

    fn select_key_type(&self, base: &Expr, receiver: &ExprType, binds: &BindEnv) -> Option<ExprType> {
        match self.resolve(base, receiver, binds)? {
            ExprType::View(row) => row.key().cloned(),
            _ => None,
        }
    }

    /// Resolve a value/row-source expression to its [`ExprType`] against the
    /// receiver row, the package root, and any in-scope row bindings — enough of
    /// the expression grammar (`.`, `/`, a bound name, field access, and key or
    /// filtered selection) to drive §8.3 parameter inference.
    fn resolve(&self, expr: &Expr, receiver: &ExprType, binds: &BindEnv) -> Option<ExprType> {
        match &expr.kind {
            ExprKind::Current => Some(receiver.clone()),
            ExprKind::Root => Some(self.root_row.clone()),
            ExprKind::Name(id) => binds.get(&id.text).cloned(),
            // `.base.$all` (§14.2) is a temporal selector that preserves the
            // bucketed base view's row shape, so a filtered bind or key selection
            // over it resolves the same rows the base does.
            ExprKind::Field { base, member } if member.structural && member.text == "all" => {
                let base_ty = self.resolve(base, receiver, binds)?;
                base_ty.as_view().map(|row| ExprType::View(row.clone()))
            }
            ExprKind::Field { base, member } => {
                let base_ty = self.resolve(base, receiver, binds)?;
                base_ty.as_row().and_then(|r| r.field(&member.text)).cloned()
            }
            ExprKind::Select { base, selector } => {
                let row = self.resolve(base, receiver, binds)?.as_view()?.clone();
                match selector {
                    Selector::Keys(_) => Some(ExprType::Row(row)),
                    Selector::Bind { .. } => Some(ExprType::View(row)),
                }
            }
            _ => None,
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
                        // cannot type stays a documented seam — left unbound rather
                        // than mis-typed — so a later reference to it is accepted
                        // structurally exactly as before.
                        if let Some(typed) = self.type_value(value, &scope, *source) {
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
