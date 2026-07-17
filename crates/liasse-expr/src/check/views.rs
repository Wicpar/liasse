//! Typing of selectors, `::` traversal, calls (aggregates, built-ins, `now`,
//! `uuid`), and object literals (§6.3, §6.4, §6.5, §7.5).

use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector};
use liasse_value::{StructType, Type};

use crate::check::Checker;
use crate::host::HostOp;
use crate::ty::ExprType;
use crate::typed::{AggFunc, BuiltinFn, TypedExpr, TypedKind, TypedSelector};

impl Checker<'_> {
    pub(crate) fn check_select(
        &mut self,
        expr: &Expr,
        base: &Expr,
        selector: &Selector,
    ) -> Option<TypedExpr> {
        let base = self.check(base)?;
        let row = match base.ty() {
            ExprType::View(row) => row.clone(),
            other => {
                return self.error(
                    expr,
                    format!("cannot select rows from a {}", other.describe()),
                );
            }
        };
        match selector {
            Selector::Keys(keys) => {
                let mut typed = Vec::with_capacity(keys.len());
                let mut single_scalar = keys.len() == 1;
                for key in keys {
                    let checked = self.check(key)?;
                    if matches!(checked.ty().as_scalar(), Some(Type::Set(_))) {
                        single_scalar = false;
                    }
                    typed.push(checked);
                }
                // §6.3: a lone scalar/composite key is a one-or-zero row context
                // (usable where exactly one row is required); anything else is a
                // multi-row view.
                let ty = if single_scalar {
                    ExprType::Row(row)
                } else {
                    ExprType::View(row)
                };
                Some(TypedExpr::new(
                    expr.span,
                    ty,
                    TypedKind::Select {
                        base: Box::new(base),
                        selector: TypedSelector::Keys(typed),
                    },
                ))
            }
            Selector::Bind { name, condition } => {
                // §6.4: `[:name | condition]` names the row under test `name`; `.`
                // inside the filter stays the enclosing receiver (so a meter source
                // `/pools[:p | p.owner == .]` compares against the enforcing row,
                // §15.3). Keep the outer `.` and bind only the new name.
                let outer = self.current_at(0).unwrap_or_else(|| ExprType::Row(row.clone()));
                self.push_frame(outer);
                self.bind(name.text.clone(), ExprType::Row(row.clone()));
                let condition = match condition {
                    Some(cond) => {
                        let checked = self.check(cond)?;
                        if checked.ty().as_scalar() != Some(&Type::Bool) {
                            self.pop_frame();
                            return self.error(cond, "a `[:name | …]` filter must be `bool`");
                        }
                        Some(Box::new(checked))
                    }
                    None => None,
                };
                self.pop_frame();
                Some(TypedExpr::new(
                    expr.span,
                    ExprType::View(row),
                    TypedKind::Select {
                        base: Box::new(base),
                        selector: TypedSelector::Bind {
                            name: name.text.clone(),
                            condition,
                        },
                    },
                ))
            }
        }
    }

    /// `base::member` (§6.4): flatten `member` across the rows of `base`.
    pub(crate) fn check_traverse(
        &mut self,
        expr: &Expr,
        base: &Expr,
        member: &str,
    ) -> Option<TypedExpr> {
        let base = self.check(base)?;
        if base.ty().as_view().is_none() {
            return self.error(
                expr,
                format!("cannot traverse `::` a {}", base.ty().describe()),
            );
        }
        self.traverse_view(expr, base, member)
    }

    /// Flatten the nested collection `member` across the rows of an already-typed
    /// view `base` (§6.4). Shared by the `::` traversal and by ordinary
    /// `view.member` field access — both expand to the same per-row flatten and
    /// bind the traversed level to its field name.
    pub(crate) fn traverse_view(
        &mut self,
        expr: &Expr,
        base: TypedExpr,
        member: &str,
    ) -> Option<TypedExpr> {
        let base_row = match base.ty() {
            ExprType::View(row) => row,
            // The caller guarantees a view base.
            _ => return self.error(expr, "expected a view to traverse"),
        };
        let inner = match base_row.field(member) {
            Some(ExprType::View(row)) => row.clone(),
            Some(other) => {
                return self.error(
                    expr,
                    format!("`{member}` is a {}, not a nested collection", other.describe()),
                );
            }
            None => return self.error(expr, format!("no nested collection `{member}` to traverse")),
        };
        Some(TypedExpr::new(
            expr.span,
            ExprType::View(inner),
            TypedKind::Traverse {
                base: Box::new(base),
                member: member.to_owned(),
            },
        ))
    }

    /// Collect the row bindings a selection/traversal chain contributes to a
    /// projection body (§6.4): each traversed collection binds to its own field
    /// name, and every `[:name]` binding along the chain stays visible. Walking
    /// the whole left spine keeps outer bindings (`.companies[:c].offices[:o]`
    /// exposes both `c` and `o`) in scope where the outputs are typed.
    pub(crate) fn traverse_binds(typed: &TypedExpr, out: &mut Vec<(String, ExprType)>) {
        match typed.kind() {
            TypedKind::Traverse { base, member } => {
                Self::traverse_binds(base, out);
                if let ExprType::View(row) = typed.ty() {
                    out.push((member.clone(), ExprType::Row(row.clone())));
                }
            }
            TypedKind::Select { base, selector } => {
                Self::traverse_binds(base, out);
                if let TypedSelector::Bind { name, .. } = selector
                    && let ExprType::View(row) | ExprType::Row(row) = typed.ty()
                {
                    out.push((name.clone(), ExprType::Row(row.clone())));
                }
            }
            TypedKind::Field { base, name } => {
                Self::traverse_binds(base, out);
                if let ExprType::View(row) = typed.ty() {
                    out.push((name.clone(), ExprType::Row(row.clone())));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn check_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        match &callee.kind {
            ExprKind::Name(name) => self.check_named_call(expr, &name.text, args),
            // `.base.$at(t)` / `.base.$between(a, b)` — a temporal selector (§14.1)
            // is a structural member applied to a view, not a namespace call.
            ExprKind::Field { base, member } if member.structural => {
                self.check_temporal_call(expr, base, &member.text, args)
            }
            ExprKind::Field { base, member } => match &base.kind {
                ExprKind::Name(ns) => self.check_namespace_call(expr, &ns.text, &member.text, args),
                _ => self.error(expr, "unsupported call target"),
            },
            _ => self.error(expr, "unsupported call target"),
        }
    }

    fn check_named_call(&mut self, expr: &Expr, name: &str, args: &[Arg]) -> Option<TypedExpr> {
        if let Some(func) = aggregate_of(name) {
            return self.check_aggregate(expr, func, args);
        }
        match name {
            "now" => Some(TypedExpr::new(
                expr.span,
                ExprType::scalar(Type::timestamp()),
                TypedKind::Now,
            )),
            "uuid" => Some(TypedExpr::new(expr.span, ExprType::scalar(Type::Uuid), TypedKind::Uuid)),
            "size" => self.check_builtin(expr, BuiltinFn::Size, args, ExprType::scalar(Type::Int)),
            "has" => self.check_builtin(expr, BuiltinFn::Has, args, ExprType::scalar(Type::Bool)),
            "assert" => {
                self.check_builtin(expr, BuiltinFn::Assert, args, ExprType::scalar(Type::Bool))
            }
            _ => self.error(expr, format!("unknown function `{name}`")),
        }
    }

    fn check_namespace_call(
        &mut self,
        expr: &Expr,
        namespace: &str,
        function: &str,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        // §16.1: the core `string` utilities resolve before any host namespace.
        if let Some(func) = core_string_fn(namespace, function) {
            return self.check_builtin(expr, func, args, ExprType::scalar(Type::Text));
        }
        // §16.2: a declared `$requires` host namespace supplies a pinned signature
        // the call site is type-checked against. An undeclared namespace resolves
        // nothing, so the function name fails validation (a host call must name an
        // explicit requirement — availability in the context does not substitute).
        match self.scope.namespace_op(namespace, function) {
            Some(op) => self.check_host_call(expr, namespace, function, args, &op),
            None => self.error(expr, format!("unknown function `{namespace}.{function}`")),
        }
    }

    /// Type-check a resolved host-namespace call against its pinned signature and
    /// the current position's effect policy (§16.2/§16.3, §8.8).
    fn check_host_call(
        &mut self,
        expr: &Expr,
        namespace: &str,
        function: &str,
        args: &[Arg],
        op: &HostOp,
    ) -> Option<TypedExpr> {
        // §16.3/§8.8: only an effect class the position admits may run here — a
        // generated or verifier function in a view/check is rejected at load.
        let position = self.scope.host_position();
        if !position.permits(op.effect()) {
            return self.error(
                expr,
                format!(
                    "`{namespace}.{function}` is a {} host function, which cannot run in {} (§16.3)",
                    op.effect().describe(),
                    position.describe(),
                ),
            );
        }
        // §16.2: the argument count and each argument's type must match the
        // pinned signature — a mismatch is a static type error, not a runtime one.
        if args.len() != op.params().len() {
            return self.error(
                expr,
                format!(
                    "`{namespace}.{function}` takes {} argument(s), but {} were supplied",
                    op.params().len(),
                    args.len(),
                ),
            );
        }
        let mut typed = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(op.params()) {
            let value = arg_value(arg);
            let checked = self.check(value)?;
            let actual = match checked.ty().as_scalar() {
                Some(ty) => ty,
                None => {
                    return self.error(
                        value,
                        format!("`{namespace}.{function}` takes scalar arguments"),
                    );
                }
            };
            if !arg_conforms(actual, param, &checked) {
                return self.error(
                    value,
                    format!(
                        "`{namespace}.{function}` expects `{}` here, but a `{}` was supplied \
                         (pinned signature, §16.2)",
                        param.name(),
                        actual.name(),
                    ),
                );
            }
            typed.push(checked);
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(op.result().clone()),
            TypedKind::HostCall {
                namespace: namespace.to_owned(),
                function: function.to_owned(),
                args: typed,
            },
        ))
    }

    fn check_builtin(
        &mut self,
        expr: &Expr,
        func: BuiltinFn,
        args: &[Arg],
        result: ExprType,
    ) -> Option<TypedExpr> {
        let mut typed = Vec::with_capacity(args.len());
        for arg in args {
            let value = match arg {
                Arg::Positional(value) => value,
                Arg::Named { value, .. } => value,
            };
            typed.push(self.check(value)?);
        }
        Some(TypedExpr::new(
            expr.span,
            result,
            TypedKind::Builtin { func, args: typed },
        ))
    }

    fn check_aggregate(
        &mut self,
        expr: &Expr,
        func: AggFunc,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        let arg = match args {
            [Arg::Positional(arg)] => arg,
            _ => return self.error(expr, "an aggregate takes one view argument"),
        };
        if func == AggFunc::Count {
            let source = self.check(arg)?;
            if source.ty().as_view().is_none() {
                return self.error(arg, "`count` takes a view");
            }
            return Some(TypedExpr::new(
                expr.span,
                ExprType::scalar(Type::Int),
                TypedKind::Aggregate {
                    func,
                    source: Box::new(source),
                    field: None,
                },
            ));
        }
        // sum/avg/min/max/distinct take `view.field`.
        let (base, member) = match &arg.kind {
            ExprKind::Field { base, member } => (base.as_ref(), member.text.clone()),
            _ => return self.error(arg, "this aggregate takes a `view.field`"),
        };
        let source = self.check(base)?;
        let row = match source.ty().as_view() {
            Some(row) => row,
            None => return self.error(base, "this aggregate takes a `view.field`"),
        };
        let element = match row.field(&member).and_then(ExprType::as_scalar) {
            Some(ty) => ty.clone(),
            None => return self.error(arg, format!("no scalar field `{member}` to aggregate")),
        };
        let base_numeric = strip_optional(&element);
        // §7.5: `sum` returns the field's numeric type and `avg` converts every
        // input to `decimal`; both require a numeric field. `min`/`max`/`distinct`
        // range over any field type (Annex B order), so they are unrestricted.
        let numeric = matches!(base_numeric, Type::Int | Type::Decimal);
        let result = match func {
            AggFunc::Sum if numeric => base_numeric.clone(),
            AggFunc::Avg if numeric => Type::Optional(Box::new(Type::Decimal)),
            AggFunc::Sum | AggFunc::Avg => {
                return self.error(
                    arg,
                    format!(
                        "`sum`/`avg` require a numeric (`int`/`decimal`) field, but `{member}` is `{}`",
                        base_numeric.name()
                    ),
                );
            }
            AggFunc::Min | AggFunc::Max => Type::Optional(Box::new(base_numeric.clone())),
            AggFunc::Distinct => Type::Set(Box::new(base_numeric.clone())),
            AggFunc::Count => Type::Int,
        };
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(result),
            TypedKind::Aggregate {
                func,
                source: Box::new(source),
                field: Some(member),
            },
        ))
    }

    /// An object literal in value position: a struct value / composite-key
    /// operand (§6.3). Every field must be scalar.
    pub(crate) fn check_object(
        &mut self,
        expr: &Expr,
        members: &[BlockMember],
    ) -> Option<TypedExpr> {
        let mut fields = Vec::with_capacity(members.len());
        let mut types = Vec::with_capacity(members.len());
        for member in members {
            let (name, value) = match &member.kind {
                BlockMemberKind::Named { name, value: Some(value) } => {
                    (name.text.clone(), self.check(value)?)
                }
                BlockMemberKind::Named { name, value: None } => {
                    let synthetic = Expr {
                        span: member.span,
                        kind: ExprKind::Name(name.clone()),
                    };
                    (name.text.clone(), self.check(&synthetic)?)
                }
                BlockMemberKind::Shorthand(inner) => match &inner.kind {
                    ExprKind::Param(name) | ExprKind::Name(name) => {
                        (name.text.clone(), self.check(inner)?)
                    }
                    _ => {
                        return self.error(inner, "an object shorthand must name a field");
                    }
                },
                _ => return self.error(expr, "an object literal member must be `name: value`"),
            };
            let ty = match value.ty().as_scalar() {
                Some(ty) => ty.clone(),
                None => return self.error(expr, "an object field must be a scalar value"),
            };
            types.push((name.clone(), ty));
            fields.push((name, value));
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Struct(StructType::new(types))),
            TypedKind::Struct(fields),
        ))
    }
}

/// The core `string` utility (§16.1) a `namespace.function` names, if any.
fn core_string_fn(namespace: &str, function: &str) -> Option<BuiltinFn> {
    match (namespace, function) {
        ("string", "lower") => Some(BuiltinFn::StringLower),
        ("string", "upper") => Some(BuiltinFn::StringUpper),
        ("string", "trim") => Some(BuiltinFn::StringTrim),
        _ => None,
    }
}

/// The value expression of a call argument (a host call's arguments carry no
/// keyword semantics; the name is decorative, §16.4).
fn arg_value(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) | Arg::Named { value, .. } => value,
    }
}

/// Whether an argument of type `actual` satisfies a pinned parameter type
/// `declared` (§16.2). Exact type identity, plus the two widenings assignment
/// already allows: the bare `none` literal fills any `optional<T>`, and a present
/// value fills an `optional<T>` whose inner type it matches (A.1).
fn arg_conforms(actual: &Type, declared: &Type, checked: &TypedExpr) -> bool {
    if actual == declared {
        return true;
    }
    match declared {
        Type::Optional(inner) => checked.is_none_literal() || arg_conforms(actual, inner, checked),
        _ => false,
    }
}

fn aggregate_of(name: &str) -> Option<AggFunc> {
    Some(match name {
        "count" => AggFunc::Count,
        "sum" => AggFunc::Sum,
        "avg" => AggFunc::Avg,
        "min" => AggFunc::Min,
        "max" => AggFunc::Max,
        "distinct" => AggFunc::Distinct,
        _ => return None,
    })
}

fn strip_optional(ty: &Type) -> Type {
    match ty {
        Type::Optional(inner) => (**inner).clone(),
        other => other.clone(),
    }
}
