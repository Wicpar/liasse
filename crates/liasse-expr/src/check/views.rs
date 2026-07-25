//! Typing of selectors, `::` traversal, calls (aggregates, built-ins, `now`,
//! `uuid`), and object literals (§6.3, §6.4, §6.5, §7.5).

use liasse_diag::{Diagnostic, Span};
use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector};
use liasse_value::{RefTarget, StructType, Type};

use crate::check::Checker;
use crate::env::CallSite;
use crate::host::{HostEffect, HostOp, HostOrigin, HostPosition};
use crate::ty::ExprType;
use crate::typed::{AggFunc, BuiltinFn, TypedExpr, TypedKind, TypedSelector};

impl Checker<'_> {
    /// Validate and normalize a composite key operand to `$key` order (§6.3, A.9).
    ///
    /// This is the single validate-and-normalize point for an authoring object
    /// operand naming a composite key (`{ region, code }`) in every position it can
    /// appear — a `collection[{..}]` selector, `==`, and `in`. When `expected` is a
    /// composite key type and `operand` is the authoring object (a struct-typed
    /// operand), the object MUST be a *key of that target's type*: it names every
    /// `$key` component, each with the declared component type, and carries no extra
    /// field (A.9). A conforming object is wrapped so it evaluates to the positional
    /// [`Value::Composite`](liasse_value::Value::Composite) a composite row's key
    /// carries; a non-conforming one is rejected at load with the same E-EXPR type
    /// error the scalar path emits (§6 intro: static types are checked at load).
    ///
    /// An operand that is already a composite key, a ref, or any other non-struct
    /// shape is left untouched for its own comparison path.
    pub(crate) fn coerce_composite_key(
        &mut self,
        operand: TypedExpr,
        expected: Option<&ExprType>,
    ) -> Option<TypedExpr> {
        let Some(Type::Composite(components)) = expected.and_then(ExprType::as_scalar) else {
            return Some(operand);
        };
        let Some(Type::Struct(struct_ty)) = operand.ty().as_scalar() else {
            return Some(operand);
        };
        if let Err(reason) = composite_key_conforms(struct_ty, components) {
            self.report_span(operand.span(), reason);
            return None;
        }
        let order: Vec<String> = components.iter().map(|(name, _)| name.clone()).collect();
        let span = operand.span();
        Some(TypedExpr::new(
            span,
            ExprType::scalar(Type::Composite(components.clone())),
            TypedKind::Composite { order, source: Box::new(operand) },
        ))
    }

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
                let key_type = row.key().cloned();
                let mut typed = Vec::with_capacity(keys.len());
                let mut single_scalar = keys.len() == 1;
                for key in keys {
                    let checked = self.check(key)?;
                    if matches!(checked.ty().as_scalar(), Some(Type::Set(_))) {
                        single_scalar = false;
                    }
                    // A.9: an object key selector on a composite-keyed collection
                    // is authoring syntax for the `$key`-order tuple — validate it
                    // against the target key and normalize it.
                    typed.push(self.coerce_composite_key(checked, key_type.as_ref())?);
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
    ///
    /// The base is a view (the ordinary `collection::member` aggregation, and the
    /// §13.9 `.modules::iface` aggregation over every instance) or a single row.
    /// A single-row base arises when `::` follows a key selection that narrows to
    /// one instance — §13.9 "the parent MAY select one instance … using ordinary
    /// selectors" (`.modules[key]::iface`, and the §13.4/§13.10/W4 single-instance
    /// interface addresses `/companies[k].modules[k]::iface`). Its `[:base]` level
    /// degenerates to that one row, so `member` is read off it exactly as it is
    /// off each row of a view. The evaluator already treats a single-row base as a
    /// one-row view ([`Evaluator::eval_view`]), so only the type side needed the
    /// row base admitted.
    ///
    /// [`Evaluator::eval_view`]: crate::eval::Evaluator::eval_view
    pub(crate) fn check_traverse(
        &mut self,
        expr: &Expr,
        base: &Expr,
        member: &str,
    ) -> Option<TypedExpr> {
        let base = self.check(base)?;
        match base.ty() {
            ExprType::View(_) | ExprType::Row(_) => self.traverse_view(expr, base, member),
            other => self.error(expr, format!("cannot traverse `::` a {}", other.describe())),
        }
    }

    /// Flatten the nested collection `member` across the rows of an already-typed
    /// view or single row `base` (§6.4). Shared by the `::` traversal and by
    /// ordinary `view.member` field access — both expand to the same per-row
    /// flatten and bind the traversed level to its field name.
    pub(crate) fn traverse_view(
        &mut self,
        expr: &Expr,
        base: TypedExpr,
        member: &str,
    ) -> Option<TypedExpr> {
        let base_row = match base.ty() {
            // A view aggregates `member` across its rows; a single row (a key
            // selection narrowed to one instance, §13.9) reads `member` off itself.
            ExprType::View(row) | ExprType::Row(row) => row,
            // The caller guarantees a view or row base.
            _ => return self.error(expr, "expected a view or row to traverse"),
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
                // §13.8/§13.10: `#handle.mutation(args)` dispatches to an interface
                // `$mut` on an imported module instance — a well-typed call whose
                // result is the contract's declared `$return`.
                ExprKind::Import(handle) => {
                    self.check_interface_call(expr, &handle.text, &member.text, args)
                }
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
            "uuid" => Some(TypedExpr::new(
                expr.span,
                ExprType::scalar(Type::Uuid),
                // §5.1/§8.12 (SPEC-ISSUES item 4): pin the call site to its OWN
                // sub-source here. `expr.span` is a local byte offset within this
                // default's sub-source, so two byte-identical `uuid()` defaults on
                // one row share it; pairing it with `self.source` makes the site
                // globally unique, so the runtime derives a distinct UUID for each.
                TypedKind::Uuid(CallSite::new(Span::new(self.source, expr.span))),
            )),
            "size" => self.check_size(expr, args),
            "has" => self.check_builtin(expr, BuiltinFn::Has, args, ExprType::scalar(Type::Bool)),
            "assert" => {
                self.check_builtin(expr, BuiltinFn::Assert, args, ExprType::scalar(Type::Bool))
            }
            // §13.16 blob boundary + lifecycle operators. `unpack` needs no host
            // authority; the other three carry an instance through its §13.10
            // lifecycle and are refused by the pure evaluator.
            "unpack" => self.check_unpack(expr, args),
            _ => match crate::lifecycle::ModuleOperator::classify(name) {
                Some(operator) => self.check_module_operator(expr, operator, args),
                None => self.error(expr, format!("unknown function `{name}`")),
            },
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
        // §6.5: their arity is part of the signature loading validates, so a call
        // supplying another count is rejected here rather than at evaluation.
        if let Some(builtin) = CoreStringFn::resolve(namespace, function) {
            if args.len() != builtin.arity {
                return self.arity_error(expr, namespace, function, builtin.arity, args.len());
            }
            return self.check_builtin(expr, builtin.func, args, ExprType::scalar(builtin.result));
        }
        // §16.1: `time.duration(text)` parses an ISO-8601 duration literal to a
        // `duration` value (the §11.5 `now() + time.duration('P30D')` session TTL).
        if (namespace, function) == ("time", "duration") {
            return self.check_builtin(expr, BuiltinFn::TimeDuration, args, ExprType::scalar(Type::Duration));
        }
        // §16.1/§16.5: the core codec namespaces (`base64`/`hex` byte↔text codecs
        // and the `string` byte codecs) are engine-linked built-ins, resolved here
        // — before any `$requires` namespace — with a `Core` origin and `Pure`
        // effect, so they stay legal in EVERY database-evaluated position (a view,
        // a `$check`, a computed value, a field default, a §20 `$as`/`$back`
        // transform), not only a mutation body. They are checked through the shared
        // host-call path so the pinned `[param] -> result` signature and the
        // position policy apply uniformly (§16.2).
        if let Some(op) = core_codec_op(namespace, function) {
            return self.check_host_call(expr, namespace, function, args, &op);
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

    /// Type-check a resolved host-namespace call against its pinned signature, the
    /// current position's effect policy (§16.2/§16.3, §8.8), and the §16.5
    /// execution-context rule (a `$requires`-registered namespace is legal only in
    /// a mutation program).
    fn check_host_call(
        &mut self,
        expr: &Expr,
        namespace: &str,
        function: &str,
        args: &[Arg],
        op: &HostOp,
    ) -> Option<TypedExpr> {
        let position = self.scope.host_position();
        // §16.3/§8.8: the effect-class check runs FIRST — a generated or verifier
        // function in a database-evaluated position is the stronger, position-wide
        // violation, and its diagnostic is corpus-pinned. Only an otherwise
        // admissible *pure* app call then reaches the §16.5 origin check below.
        if !position.permits_effect(op.effect()) {
            return self.host_position_error(
                expr,
                format!(
                    "`{namespace}.{function}` is a {} host function, which cannot run in {} (§16.3)",
                    op.effect().describe(),
                    position.describe(),
                ),
            );
        }
        // §16.5: a call to a `$requires`-registered namespace is legal only inside
        // a mutation program body; every other expression position is
        // database-evaluated and restricted to the built-in namespaces (§6.5).
        if !position.permits_origin(op.origin()) {
            self.report_host_origin(expr, namespace, position);
            return None;
        }
        // §16.2: the argument count and each argument's type must match the
        // pinned signature — a mismatch is a static type error, not a runtime one.
        if args.len() != op.params().len() {
            return self.arity_error(expr, namespace, function, op.params().len(), args.len());
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

    /// Emit the §16.5 rejection for an app-registered namespace call in a
    /// database-evaluated position (a view/filter/projection/sort, a coverage
    /// predicate, a computed value, a `$check`/`$normalize`, an auth `$verify`, a
    /// field default, a bucket/meter/placement/migration expression). The
    /// diagnostic names the namespace, cites §16.5, lists the built-ins the
    /// position does admit, and points the author at the mutation body.
    fn report_host_origin(&mut self, expr: &Expr, namespace: &str, position: HostPosition) {
        self.diags.push(
            Diagnostic::error(format!(
                "app-registered namespace `{namespace}` cannot be called in {} — only a \
                 mutation program may call a `$requires` namespace (§16.5)",
                position.describe(),
            ))
            .code(crate::check::HOST_POSITION_CODE)
            .primary(Span::new(self.source, expr.span), "app-registered namespace call")
            .help(
                "every position outside a mutation body is database-evaluated and admits only \
                 the built-in namespaces (§6.5: string, time, convert, hex, base64, sha)",
            )
            .help(
                "run an application procedure inside a mutation: compute the value in a mutation \
                 statement and store it, then read the stored field here; for credential \
                 verification, use an auth mutation that mints a native token (§11.5)",
            )
            .build(),
        );
    }

    /// Type-check `size` (§7). `size` counts the elements of a `text`, a `set`, a
    /// `map`, or a view; only the view form ranges over collection rows.
    ///
    /// §14.5: counting every row of an unbounded recurring bucket enumerates a
    /// possibly-infinite series — the exact whole-series read the `count` twin
    /// performs and `check_aggregate` rejects — so a `size` over such a view must be
    /// gated the same way: the bucket has to be read through a bounded temporal
    /// selector (`.$at`/`.$between`) first. Every other `size` (text/set/map, or a
    /// bounded view) is unaffected and flows through the shared builtin path.
    fn check_size(&mut self, expr: &Expr, args: &[Arg]) -> Option<TypedExpr> {
        let [Arg::Positional(sole)] = args else {
            return self.check_builtin(expr, BuiltinFn::Size, args, ExprType::scalar(Type::Int));
        };
        let checked = self.check(sole)?;
        if checked.ty().as_view().is_some_and(|row| row.is_unbounded()) {
            return self.error(
                sole,
                "`size` over an unbounded recurring bucket enumerates the whole series; read it through a bounded temporal selector `.$at`/`.$between` first (§14.5)",
            );
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Int),
            TypedKind::Builtin { func: BuiltinFn::Size, args: vec![checked] },
        ))
    }

    /// The shared "wrong number of arguments" rejection for a call whose callee
    /// carries a pinned arity — a core `string` utility (§6.5) or a host-namespace
    /// signature (§16.2).
    fn arity_error(
        &mut self,
        expr: &Expr,
        namespace: &str,
        function: &str,
        expected: usize,
        supplied: usize,
    ) -> Option<TypedExpr> {
        self.error(
            expr,
            format!(
                "`{namespace}.{function}` takes {expected} argument(s), but {supplied} were supplied"
            ),
        )
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
            let Some(row) = source.ty().as_view() else {
                return self.error(arg, "`count` takes a view");
            };
            // §14.5: `count` over an unbounded recurring bucket enumerates the whole
            // (possibly-infinite) series, and its scalar result cannot carry the
            // unbounded marker onward (unlike a projection/filter, which propagates
            // it) — so the source must be gated by a bounded temporal selector.
            if row.is_unbounded() {
                return self.error(
                    arg,
                    "this aggregate enumerates an unbounded recurring bucket; read it through a bounded temporal selector `.$at`/`.$between` before aggregating (§14.5)",
                );
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
        // §14.5: aggregating over an unbounded recurring bucket enumerates the whole
        // series; the scalar/set result cannot carry the unbounded marker onward, so
        // a bounded temporal selector must gate the source first.
        if row.is_unbounded() {
            return self.error(
                base,
                "this aggregate enumerates an unbounded recurring bucket; read it through a bounded temporal selector `.$at`/`.$between` before aggregating (§14.5)",
            );
        }
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
            let (name, value) = self.named_member(expr, member)?;
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

    /// Parse one object member into its `(field name, checked value)` pair (§6.3):
    /// `name: value`, the value-elided `name` (a field read of `.name`), and the
    /// `@name`/`name` shorthand. Shared by the object-literal check and the
    /// interface-mutation argument object (§8.11).
    fn named_member(&mut self, expr: &Expr, member: &BlockMember) -> Option<(String, TypedExpr)> {
        match &member.kind {
            BlockMemberKind::Named { name, value: Some(value) } => {
                Some((name.text.clone(), self.check(value)?))
            }
            BlockMemberKind::Named { name, value: None } => {
                let synthetic = Expr { span: member.span, kind: ExprKind::Name(name.clone()) };
                Some((name.text.clone(), self.check(&synthetic)?))
            }
            BlockMemberKind::Shorthand(inner) => match &inner.kind {
                ExprKind::Param(name) | ExprKind::Name(name) => {
                    Some((name.text.clone(), self.check(inner)?))
                }
                _ => {
                    self.error(inner, "an object shorthand must name a field");
                    None
                }
            },
            _ => {
                self.error(expr, "an object literal member must be `name: value`");
                None
            }
        }
    }

    /// Type a `#handle.mutation(args)` dispatch to an interface `$mut` on an
    /// imported module instance (§13.8/§13.10): resolve the interface contract, type
    /// each supplied argument against the declared parameter prototype, require every
    /// non-optional declared parameter, and type the whole call as the contract's
    /// declared `$return`. The result is a [`TypedKind::InterfaceCall`] the runtime
    /// admits within the transition (never a pure value).
    fn check_interface_call(
        &mut self,
        expr: &Expr,
        handle: &str,
        mutation: &str,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        let Some(contract) = self.scope.interface_mut(handle, mutation) else {
            return self.error(
                expr,
                format!("`#{handle}` exposes no interface mutation `{mutation}` (§13.8)"),
            );
        };
        let supplied = self.interface_call_args(expr, args)?;
        let mut typed_args = Vec::with_capacity(supplied.len());
        for (name, value) in supplied {
            match contract.params.iter().find(|(param, _)| param == &name) {
                Some((_, declared)) => {
                    if let (Some(actual), Some(declared)) =
                        (value.ty().as_scalar(), declared.as_scalar())
                        && !arg_conforms(actual, declared, &value)
                    {
                        return self.error(
                            expr,
                            format!(
                                "interface mutation `#{handle}.{mutation}` expects `{}` for `{name}`, but a `{}` was supplied (§13.8)",
                                declared.name(),
                                actual.name(),
                            ),
                        );
                    }
                }
                // A contract that declares no explicit prototype (`params` empty)
                // checks no parameter names; otherwise an undeclared name is refused.
                None if contract.params.is_empty() => {}
                None => {
                    return self.error(
                        expr,
                        format!("interface mutation `#{handle}.{mutation}` declares no parameter `{name}` (§13.8)"),
                    );
                }
            }
            typed_args.push((name, value));
        }
        for (name, ty) in &contract.params {
            let optional = matches!(ty.as_scalar(), Some(Type::Optional(_)));
            if !optional && !typed_args.iter().any(|(supplied, _)| supplied == name) {
                return self.error(
                    expr,
                    format!("interface mutation `#{handle}.{mutation}` is missing argument `{name}` (§13.8)"),
                );
            }
        }
        Some(TypedExpr::new(
            expr.span,
            contract.ret.clone(),
            TypedKind::InterfaceCall {
                handle: handle.to_owned(),
                mutation: mutation.to_owned(),
                args: typed_args,
            },
        ))
    }

    /// The `(parameter name, checked value)` pairs an interface-mutation argument
    /// object supplies (§8.11): a single positional object mapping parameter names to
    /// values (with `@name`/`name` shorthand), or explicit named arguments.
    fn interface_call_args(&mut self, expr: &Expr, args: &[Arg]) -> Option<Vec<(String, TypedExpr)>> {
        let mut out = Vec::new();
        for arg in args {
            match arg {
                Arg::Positional(Expr { kind: ExprKind::Object(members), .. }) => {
                    for member in members {
                        out.push(self.named_member(expr, member)?);
                    }
                }
                Arg::Named { name, value } => out.push((name.text.clone(), self.check(value)?)),
                Arg::Positional(_) => {
                    self.error(
                        expr,
                        "an interface mutation call takes an argument object mapping parameter names to values (§8.11)",
                    );
                    return None;
                }
            }
        }
        Some(out)
    }
}

/// Whether a supplied component value type satisfies a declared composite-key
/// component. Beyond exact equality, a `ref` component (§D.1/A.9) is satisfied by a
/// value of the row-key type it targets: a `{ $ref: /logins }` component whose target has
/// a composite key accepts that composite key (`login.$key`), and a scalar-key ref
/// accepts its scalar key — the referenced row's identity IS that key.
fn component_matches(supplied: &Type, declared: &Type) -> bool {
    if supplied == declared {
        return true;
    }
    match declared {
        Type::Ref(RefTarget::Composite(components)) => {
            *supplied == Type::Composite(components.clone())
        }
        Type::Ref(RefTarget::Scalar(inner)) => supplied == inner.as_ref(),
        _ => false,
    }
}

/// Whether an authoring object operand (`struct_ty`) is a key of a composite-keyed
/// target whose `$key` components are `components` (§6.3, A.9): it names every
/// component with the declared component type and carries no extra field. `Err`
/// carries the load-time diagnostic explaining the first mismatch.
fn composite_key_conforms(
    struct_ty: &StructType,
    components: &[(String, Type)],
) -> Result<(), String> {
    for (name, ty) in components {
        match struct_ty.field(name) {
            Some(supplied) if component_matches(supplied, ty) => {}
            Some(supplied) => {
                return Err(format!(
                    "composite key component `{name}` is `{}`, but the target key declares `{}` \
                     (§6.3, A.9)",
                    supplied.name(),
                    ty.name(),
                ));
            }
            None => {
                return Err(format!(
                    "composite key is missing component `{name}`; an object operand must name \
                     every `$key` component (§6.3, A.9)"
                ));
            }
        }
    }
    // Every component matched; a differing field count means the object carries an
    // extra, non-component field — not a key of the target's type (A.9 arity).
    if struct_ty.fields().count() != components.len() {
        let extra: Vec<&str> = struct_ty
            .fields()
            .map(|(name, _)| name.as_str())
            .filter(|name| !components.iter().any(|(comp, _)| comp == name))
            .collect();
        return Err(format!(
            "object operand carries field(s) `{}` that are not `$key` components of the target \
             (§6.3, A.9)",
            extra.join("`, `"),
        ));
    }
    Ok(())
}

/// One core `string` utility (§6.5/§16.1): the built-in a `namespace.function`
/// resolves to, together with the signature package loading validates — the
/// argument count and the result type.
///
/// The roster is the single place the core `string` names are listed: the checker
/// resolves calls through it, and [`is_core_string_call`](super::is_core_string_call)
/// lets the model layer classify a mutation-program call by the same list, so the
/// two never drift.
pub(crate) struct CoreStringFn {
    func: BuiltinFn,
    arity: usize,
    result: Type,
}

impl CoreStringFn {
    /// The core `string` utility `namespace.function` names, if any.
    pub(crate) fn resolve(namespace: &str, function: &str) -> Option<Self> {
        let (func, arity, result) = match (namespace, function) {
            ("string", "lower") => (BuiltinFn::StringLower, 1, Type::Text),
            ("string", "upper") => (BuiltinFn::StringUpper, 1, Type::Text),
            ("string", "casefold") => (BuiltinFn::StringCasefold, 1, Type::Text),
            ("string", "trim") => (BuiltinFn::StringTrim, 1, Type::Text),
            // §6.5: the search predicates take `(subject, needle)` and answer
            // `bool` over Unicode scalar values.
            ("string", "starts_with") => (BuiltinFn::StringStartsWith, 2, Type::Bool),
            ("string", "ends_with") => (BuiltinFn::StringEndsWith, 2, Type::Bool),
            ("string", "contains") => (BuiltinFn::StringContains, 2, Type::Bool),
            _ => return None,
        };
        Some(Self { func, arity, result })
    }
}

/// The pinned op of a core codec built-in (§16.1) a `namespace.function` names, if
/// any: the `base64`/`hex` byte↔text codecs (`encode(bytes) -> text`,
/// `decode(text) -> bytes`) and the `string` byte codecs (`bytes(text) -> bytes`,
/// `from_bytes(bytes) -> text`). Every one is engine-linked, so its origin is
/// [`HostOrigin::Core`] and its effect [`HostEffect::Pure`] — legal in every
/// database-evaluated position (§16.5), unlike an app-registered `$requires`
/// namespace. The `string.bytes`/`from_bytes` entries coexist with the language
/// `string` built-ins of [`CoreStringFn`] (resolved before this), which never
/// collide by function name.
fn core_codec_op(namespace: &str, function: &str) -> Option<HostOp> {
    let (param, result) = match (namespace, function) {
        ("base64" | "hex", "encode") => (Type::Bytes, Type::Text),
        ("base64" | "hex", "decode") => (Type::Text, Type::Bytes),
        ("string", "bytes") => (Type::Text, Type::Bytes),
        ("string", "from_bytes") => (Type::Bytes, Type::Text),
        _ => return None,
    };
    Some(HostOp::new([param], result, HostEffect::Pure, HostOrigin::Core))
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
/// already allows: the bare `none` literal fills any `T?`, and a present
/// value fills an `T?` whose inner type it matches (A.1).
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
