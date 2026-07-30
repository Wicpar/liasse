//! §8.3 parameter inference, shared by mutation programs (§8, [`super`]) and
//! surface read positions (§10.1: a `$view` and a `$recursive` `$where`/`$except`
//! predicate).
//!
//! §10.1 states the read-position rule in terms of the mutation one: a surface
//! `$view` parameter "is inferred exactly as a mutation parameter is (§8.3):
//! every typed use of `@name` — a comparison against a typed field, a
//! key-selector position, a typed function argument — contributes a constraint,
//! and all uses MUST agree on one type." Keeping ONE walk means the two contexts
//! cannot drift: an anchor the mutation phase learns to read is immediately an
//! anchor a view reads, and the "one compatible type" conflict rule is the same
//! [`record`] in both.
//!
//! The two contexts differ only in which *positions* anchor a type, which
//! [`ParamPosition`] selects:
//!
//! - [`ParamPosition::Program`] — a mutation body. `{ field: @p }` is an insert
//!   or patch member, so it anchors `@p` to the target field (§8.3/§8.6), and a
//!   `$requires` host-namespace call argument anchors `@p` to the host
//!   signature (§16.4).
//! - [`ParamPosition::Read`] — a surface `$view` or `$recursive` predicate.
//!   `{ ... }` there is a PROJECTION: `{ label: @p }` names an output member and
//!   binds it to the parameter's value, which constrains nothing, so a parameter
//!   projected straight through is *uninferable* and §10.1 requires an explicit
//!   `$params` declaration. A read position is database-evaluated (§16.5) and
//!   therefore has no host-namespace call at all — only built-ins, whose pinned
//!   signatures anchor their arguments in both positions.

use std::collections::BTreeSet;

use liasse_diag::ByteSpan;
use liasse_expr::{ExprType, RowType};
use liasse_syntax::{BinaryOp, Expr, ExprKind, Selector, Stmt};
use liasse_value::Type;

use crate::host::HostDescriptors;
use crate::walk::child_exprs;

use super::helpers::{
    arg_expr, host_call_target, is_scalar_binop, local_binding_name, record, stmt_exprs, BindEnv,
    Params,
};

/// Which positions of an expression anchor a `@name`'s type (see the module
/// docs): a mutation body's write positions, or a read position's comparisons
/// and selectors only.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamPosition {
    /// A mutation program (§8.3): assignments, inserts, patches, key selectors,
    /// and `$requires` host-call arguments all anchor a parameter.
    Program,
    /// A surface `$view` or `$recursive` predicate (§10.1): comparisons, key
    /// selectors, and the fixed-type temporal arguments anchor a parameter; a
    /// `{ ... }` projection member and (absent) host calls do not.
    Read,
}

/// The §8.3 inference walk over one program or read expression.
///
/// `root_row` is the package root `/`, against which a `/collection[@p]` anchor
/// resolves. `hosts` carries the resolved `$requires` signatures for a
/// [`ParamPosition::Program`] walk and is `None` for a read position, which
/// §16.5 forbids a registered-namespace call in.
pub(super) struct Inference<'a> {
    root_row: ExprType,
    position: ParamPosition,
    hosts: Option<&'a HostDescriptors>,
}

impl<'a> Inference<'a> {
    /// Inference over a mutation program (§8.3), with the §16.2 host signatures
    /// a bare `ns.fn(@p)` argument takes its type from (§16.4).
    pub(super) fn program(root_row: ExprType, hosts: &'a HostDescriptors) -> Self {
        Self { root_row, position: ParamPosition::Program, hosts: Some(hosts) }
    }

    /// Inference over a surface read expression (§10.1): a `$view` or a
    /// `$recursive` `$where`/`$except` predicate.
    fn read(root_row: ExprType) -> Self {
        Self { root_row, position: ParamPosition::Read, hosts: None }
    }

    /// §8.3: infer each `@name` from its use context. `binds` seeds the row
    /// bindings already in scope — a `$recursive` `$bind` candidate row (§10.5)
    /// for a read position, empty for a program, whose own `local = value`
    /// statements extend it as the walk proceeds.
    pub(super) fn infer(
        &self,
        statements: &[&Stmt],
        receiver: &ExprType,
        binds: &BindEnv,
        params: &mut Params,
    ) {
        let mut binds = binds.clone();
        for stmt in statements {
            if let liasse_syntax::StmtKind::Assign { target, value } = &stmt.kind {
                if let Some(local) = local_binding_name(target) {
                    // §8/Annex C.9: a local binding `local = value` is visible to
                    // later statements, so track its type here — a subsequent
                    // `local.field = @p` (below) then resolves the local's row and
                    // infers `@p` from the field. A value the CORE phase cannot type
                    // (a mutation-operator or host/program-call result) resolves to
                    // `None` and stays unbound, exactly as the full check phase defers
                    // it; the param uses on such a value are inferred by the `infer_in`
                    // walk instead.
                    if let Some(ty) = self.resolve(value, receiver, &binds) {
                        binds.insert(local.to_owned(), ty);
                    }
                } else if let ExprKind::Param(id) = &value.kind
                    && let Some(ty) = self.resolve(target, receiver, &binds)
                    && ty.as_scalar().is_some()
                {
                    // A scalar assignment `field = @p` constrains `@p` to the target
                    // field's type (§8.3) — including a field of a local binding
                    // (`t.label = @p`), resolved through `binds` above. The general
                    // expression walk below does not relate the assignment's two
                    // sides, so it is inferred here.
                    record(params, &id.text, ty);
                }
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
        // a load conflict here. A read position (§16.5) admits no such call, so the
        // pass does not run there.
        if let Some(hosts) = self.hosts {
            for stmt in statements {
                for expr in stmt_exprs(stmt) {
                    Self::infer_host_args(hosts, expr, params);
                }
            }
        }
    }

    /// §8.3/§16.4: fill a host-namespace call argument's parameter type when no
    /// prior use pinned it. A bare `@param` positional argument of a host call
    /// `ns.fn(…, @p, …)` takes the host function's declared argument type at that
    /// position when the resolved `$requires` descriptor is available (§16.2);
    /// otherwise it takes the permissive top type `json`, whose real validation is
    /// the runtime host-call boundary (§16). Either way the parameter BINDS.
    fn infer_host_args(hosts: &HostDescriptors, expr: &Expr, params: &mut Params) {
        if let ExprKind::Call { callee, args } = &expr.kind
            && let Some((namespace, function)) = host_call_target(callee)
        {
            let signature = hosts.op(namespace, function);
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
            Self::infer_host_args(hosts, child, params);
        }
    }

    fn infer_in(&self, expr: &Expr, receiver: &ExprType, binds: &BindEnv, params: &mut Params) {
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
                                Self::infer_composite_key(members, &key_ty, params);
                            }
                            _ => {}
                        }
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // `collection + { field: @p }` insert — @p inherits the target
                // collection's field type, not the receiver's (§8.3). A write
                // operator only exists in a mutation program.
                if self.position == ParamPosition::Program
                    && *op == BinaryOp::Add
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
                if self.position == ParamPosition::Program
                    && *op == BinaryOp::Sub
                    && let Some(key_ty) = self.select_key_type(lhs, receiver, binds)
                {
                    match &rhs.kind {
                        ExprKind::Param(id) => record(params, &id.text, key_ty),
                        ExprKind::Object(members) => {
                            Self::infer_composite_key(members, &key_ty, params);
                        }
                        _ => {}
                    }
                }
                // A scalar comparison or arithmetic relates its two operands to
                // one type, so a bare `@p` operand inherits the sibling's scalar
                // type: `assert(.balance >= @amount)`, `.balance - @amount`, and
                // ref-key comparisons like `x.account == @account` (§8.3). This is
                // §10.1's "comparison against a typed field" in a read position.
                if is_scalar_binop(*op) {
                    self.infer_scalar_operand(lhs, rhs, receiver, binds, params);
                    self.infer_scalar_operand(rhs, lhs, receiver, binds, params);
                }
            }
            // `row_source { field = @p }` / `{ field: @p }` patch — @p inherits
            // the patched row's field type, in both the projection (`field:`)
            // and assignment (`field =`) member forms (§8.6). In a READ position
            // the same syntax is a §7 projection whose member NAMES an output and
            // constrains nothing, so it anchors no parameter there (§10.1).
            ExprKind::Block { base, members } => {
                if self.position == ParamPosition::Program
                    && let Some(row) = self.target_row(base, receiver, binds)
                {
                    self.infer_object(members, &ExprType::Row(row), params);
                }
            }
            // `{ field: @p }` against the receiver row (a program-only anchor, as
            // for [`ExprKind::Block`] above).
            ExprKind::Object(members) => {
                if self.position == ParamPosition::Program {
                    self.infer_object(members, receiver, params);
                }
            }
            // A temporal window selector `.base.$at(t)` / `.base.$between(a, b)`
            // takes `timestamp` instants (§14.1); a bare `@param` argument inherits
            // `timestamp`. This is §10.1's "typed function argument" — the general
            // checker otherwise ignores call arguments, so a parameter used *only*
            // here would stay uninferred (§8.3).
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
                // §10.1's third anchor, "a typed function argument": a CORE
                // built-in's signature is pinned (§6.5/§16.1), so a bare `@p`
                // argument inherits the type that position declares. This is the
                // ONLY callable kind a database-evaluated position admits (§16.5),
                // so a view's `string.lower(@q)` anchors `@q` to `text` with no
                // host resolution needed. A generic slot (`size`/`has`, an
                // aggregate's view argument, `assert`'s message) pins no single
                // type, so `core_builtin_param` yields `None` and the use
                // contributes no constraint — leaving the §10.1
                // explicit-declaration error rather than a guess.
                if let Some((namespace, function)) = builtin_call_target(callee) {
                    for (index, arg) in args.iter().enumerate() {
                        if let ExprKind::Param(id) = &arg_expr(arg).kind
                            && let Some(ty) = liasse_expr::core_builtin_param(namespace, function, index)
                        {
                            record(params, &id.text, ExprType::scalar(ty));
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
        Self::infer_context_object(members, params);
    }

    /// An object key selector `[{ comp: @p, ... }]` (§6.3): each member names a
    /// key component, so its parameter inherits that component's type — matched by
    /// component name, not member position (Annex A.9). Both multi-component key
    /// forms spell a component by name: a composite key by its `$key`-ordered
    /// components, and a struct `$key` (A.8) by its field-name-ordered members;
    /// each addressed the same way here.
    fn infer_composite_key(
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
    fn infer_context_object(members: &[liasse_syntax::BlockMember], params: &mut Params) {
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
}

/// The §10.1 parameter contract of one surface read expression — a `$view` or a
/// `$recursive` `$where`/`$except` predicate — after §8.3 inference over its
/// declared `$params`.
#[derive(Debug, Clone)]
pub struct ViewParams {
    params: Vec<(String, ExprType)>,
    unconstrained: Vec<(String, ByteSpan)>,
    conflicting: Vec<(String, ByteSpan)>,
}

impl ViewParams {
    /// The settled contract, declared entries merged with inferred ones, in name
    /// order. §10.1: "the resulting parameter shape, inferred or declared, is part
    /// of the external surface contract".
    pub fn params(&self) -> &[(String, ExprType)] {
        &self.params
    }

    /// Every `@name` the expression does not constrain to a unique type, with the
    /// span of its first use. §10.1 makes each a static load error whose
    /// diagnostic requests an explicit `$params` declaration.
    pub fn unconstrained(&self) -> &[(String, ByteSpan)] {
        &self.unconstrained
    }

    /// Every `@name` whose uses (or whose declared `$params` type and inferred
    /// use) disagree — §8.3's "all uses of the same parameter MUST infer one
    /// compatible type", applied to a read position by §10.1.
    pub fn conflicting(&self) -> &[(String, ByteSpan)] {
        &self.conflicting
    }
}

/// §10.1: infer a surface `$view` or `$recursive` predicate's parameters from
/// `statement`, over the surface's `declared` `$params` (authoritative where
/// present) and the row `bindings` already in scope (a `$recursive` `$bind`
/// candidate, §10.5).
///
/// `receiver` is the expression's `.` — the package root for a `$public` surface,
/// the role-holding row for a scoped role (§10.3) — and `root_row` is the package
/// root `/`.
///
/// This is the ONE entry point both the static model ([`crate::surface`], which
/// reports the diagnostics) and the runtime's surface compilation use, so the
/// contract a client is served is exactly the contract the load validated.
pub fn infer_view_params(
    root_row: &ExprType,
    receiver: &ExprType,
    bindings: &[(String, ExprType)],
    declared: &[(String, ExprType)],
    statement: &Stmt,
) -> ViewParams {
    let mut params = Params::from_declared(declared.iter().cloned());
    let binds: BindEnv = bindings.iter().cloned().collect();
    Inference::read(root_row.clone()).infer(&[statement], receiver, &binds, &mut params);

    let mut refs = Vec::new();
    for expr in stmt_exprs(statement) {
        collect_read_param_refs(expr, &mut refs);
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut unconstrained = Vec::new();
    let mut conflicting = Vec::new();
    for (name, span) in refs {
        if !seen.insert(name) {
            continue;
        }
        if !params.contains(name) {
            unconstrained.push((name.to_owned(), span));
        } else if params.conflicts(name) {
            conflicting.push((name.to_owned(), span));
        }
    }
    ViewParams { params: params.into_pairs(), unconstrained, conflicting }
}

/// The `(namespace, function)` a call's callee names, for the core-built-in
/// signature lookup: `None` namespace for a bare-name callee (`assert(...)`),
/// `Some(ns)` for a namespaced one (`string.lower(...)`). A structural member
/// (`.$at`), a selector, or any other callee shape is not a named function call
/// and yields `None`. Whether the pair names a CORE built-in — as opposed to an
/// app-registered `$requires` namespace, whose arguments are inferred by
/// [`Inference::infer_host_args`] instead — is decided by
/// [`liasse_expr::core_builtin_param`], which pins only the built-in table.
fn builtin_call_target(callee: &Expr) -> Option<(Option<&str>, &str)> {
    match &callee.kind {
        ExprKind::Name(id) => Some((None, id.text.as_str())),
        ExprKind::Field { base, member } if !member.structural => match &base.kind {
            ExprKind::Name(namespace) => Some((Some(namespace.text.as_str()), member.text.as_str())),
            _ => None,
        },
        _ => None,
    }
}

/// Collect EVERY `@name` reference of a read expression, paired with the span of
/// its use.
///
/// A mutation program's walk ([`super::helpers::collect_param_refs`]) deliberately
/// skips a non-host call argument, whose type is deferred to the callee mutation's
/// own contract (§8.11) — a documented cross-program seam. A read position has no
/// such seam: a `$view` is database-evaluated (§16.5), so it calls only the
/// built-in pure functions, whose signatures are pinned and therefore anchor a
/// bare `@p` argument (§10.1's "typed function argument", see [`Inference::infer_in`]).
/// Every `@name` occurrence there must be constrained by the expression itself,
/// and one that is not — a projection member, a parameter-to-parameter
/// comparison, or a built-in slot that pins no single type — is the §10.1
/// explicit-declaration error.
fn collect_read_param_refs<'e>(expr: &'e Expr, out: &mut Vec<(&'e str, ByteSpan)>) {
    if let ExprKind::Param(id) = &expr.kind {
        out.push((&id.text, id.span));
    }
    for child in child_exprs(expr) {
        collect_read_param_refs(child, out);
    }
}
