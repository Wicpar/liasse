//! Typing of selectors, `::` traversal, calls (aggregates, built-ins, `now`,
//! `uuid`), and object literals (§6.3, §6.4, §6.5, §7.5).

use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector};
use liasse_value::{StructType, Type};

use crate::check::Checker;
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
                self.push_frame(ExprType::Row(row.clone()));
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
        let base_row = match base.ty() {
            ExprType::View(row) => row,
            other => {
                return self.error(expr, format!("cannot traverse `::` a {}", other.describe()));
            }
        };
        let inner = match base_row.field(member) {
            Some(ExprType::View(row)) => row.clone(),
            Some(other) => {
                return self.error(
                    expr,
                    format!("`::{member}` is a {}, not a nested collection", other.describe()),
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

    /// Collect the row bindings a `::` chain contributes (§6.4): each traversed
    /// collection binds to its own field name.
    pub(crate) fn traverse_binds(typed: &TypedExpr, out: &mut Vec<(String, ExprType)>) {
        match typed.kind() {
            TypedKind::Traverse { base, member } => {
                Self::traverse_binds(base, out);
                if let ExprType::View(row) = typed.ty() {
                    out.push((member.clone(), ExprType::Row(row.clone())));
                }
            }
            TypedKind::Field { name, .. } => {
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
        let func = match (namespace, function) {
            ("string", "lower") => BuiltinFn::StringLower,
            ("string", "upper") => BuiltinFn::StringUpper,
            ("string", "trim") => BuiltinFn::StringTrim,
            _ => {
                return self.error(
                    expr,
                    format!("unknown function `{namespace}.{function}`"),
                );
            }
        };
        self.check_builtin(expr, func, args, ExprType::scalar(Type::Text))
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
