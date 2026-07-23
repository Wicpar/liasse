//! Typing of operators: arithmetic, comparison, logic, membership, negation,
//! the conditional/fallback forms, and view combinators (§6.1, §7.4).

use liasse_syntax::{BinaryOp, CombinatorOp, Expr, UnaryOp};
use liasse_value::{RefTarget, Type};

use crate::check::Checker;
use crate::ty::ExprType;
use crate::typed::{ArithOp, CmpOp, CombineOp, LogicOp, NumClass, TypedExpr, TypedKind};

/// The target relation a `|`/`&` combinator operand's rows are natively
/// identified by (§6.3/§7.4), or `None` when the operand names no single relation
/// — it is re-identified by a synthetic `$key` projection (§7.2/§7.4 "projection
/// into a common synthetic key can adapt heterogeneous sources"), a `$view`, a
/// `::` traversal, or a multi-source node, and so combines with any operand.
///
/// A plain projection or a filter/selection keeps the source relation's identity;
/// a synthetic-`$key` projection drops it; a combinator takes its identity from
/// the left operand (§7.4). At the leaf, a keyed collection reference carries its
/// backing relation on the row ([`RowType::relation`]); everything else carries
/// none.
fn combinator_relation(expr: &TypedExpr) -> Option<&str> {
    match expr.kind() {
        TypedKind::Project { source, projection } if projection.key.is_empty() => {
            combinator_relation(source)
        }
        TypedKind::Project { .. } => None,
        TypedKind::Select { base, .. } => combinator_relation(base),
        TypedKind::Combine { lhs, .. } => combinator_relation(lhs),
        _ => match expr.ty() {
            ExprType::View(row) | ExprType::Row(row) => row.relation(),
            _ => None,
        },
    }
}

impl Checker<'_> {
    pub(crate) fn check_unary(
        &mut self,
        expr: &Expr,
        op: UnaryOp,
        operand: &Expr,
    ) -> Option<TypedExpr> {
        let operand = self.check(operand)?;
        match op {
            UnaryOp::Not => {
                if operand.ty().as_scalar() != Some(&Type::Bool) {
                    return self.error(expr, "`!` requires a `bool` operand");
                }
                Some(TypedExpr::new(
                    expr.span,
                    ExprType::scalar(Type::Bool),
                    TypedKind::Not(Box::new(operand)),
                ))
            }
            UnaryOp::Neg => {
                let class = match operand.ty().as_scalar() {
                    Some(Type::Int) => NumClass::Int,
                    Some(Type::Decimal) => NumClass::Decimal,
                    _ => return self.error(expr, "`-` requires an `int` or `decimal` operand"),
                };
                let ty = operand.ty().clone();
                Some(TypedExpr::new(
                    expr.span,
                    ty,
                    TypedKind::Neg {
                        class,
                        operand: Box::new(operand),
                    },
                ))
            }
        }
    }

    pub(crate) fn check_binary(
        &mut self,
        expr: &Expr,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Option<TypedExpr> {
        match op {
            BinaryOp::Add => self.check_arith(expr, ArithOp::Add, lhs, rhs),
            BinaryOp::Sub => self.check_sub(expr, lhs, rhs),
            BinaryOp::Mul => self.check_arith(expr, ArithOp::Mul, lhs, rhs),
            BinaryOp::Div => self.check_arith(expr, ArithOp::Div, lhs, rhs),
            BinaryOp::Rem => self.check_arith(expr, ArithOp::Rem, lhs, rhs),
            BinaryOp::Eq => self.check_compare(expr, CmpOp::Eq, lhs, rhs),
            BinaryOp::Ne => self.check_compare(expr, CmpOp::Ne, lhs, rhs),
            BinaryOp::Lt => self.check_compare(expr, CmpOp::Lt, lhs, rhs),
            BinaryOp::Le => self.check_compare(expr, CmpOp::Le, lhs, rhs),
            BinaryOp::Gt => self.check_compare(expr, CmpOp::Gt, lhs, rhs),
            BinaryOp::Ge => self.check_compare(expr, CmpOp::Ge, lhs, rhs),
            BinaryOp::And => self.check_logic(expr, LogicOp::And, lhs, rhs),
            BinaryOp::Or => self.check_logic(expr, LogicOp::Or, lhs, rhs),
            BinaryOp::In => self.check_in(expr, lhs, rhs),
            BinaryOp::Fallback => self.check_fallback(expr, lhs, rhs),
        }
    }

    /// `-` is arithmetic subtraction on numbers and view difference on views
    /// (§7.4). The operand kinds decide which.
    fn check_sub(&mut self, expr: &Expr, lhs: &Expr, rhs: &Expr) -> Option<TypedExpr> {
        let left = self.check(lhs)?;
        let right = self.check(rhs)?;
        if let (ExprType::View(row), ExprType::View(other)) = (left.ty(), right.ty()) {
            // §14.5/§7.4: computing `a - b` enumerates every row of `b` to build the
            // removal set, so the difference is unbounded if EITHER operand is — an
            // unbounded recurring bucket on either side forces an infinite-series
            // read. A bounding selector over the whole difference still clears it.
            let unbounded = row.is_unbounded() || other.is_unbounded();
            let row = row.clone().unbounded(unbounded);
            return Some(TypedExpr::new(
                expr.span,
                ExprType::View(row),
                TypedKind::Combine {
                    op: CombineOp::Difference,
                    lhs: Box::new(left),
                    rhs: Box::new(right),
                },
            ));
        }
        self.finish_arith(expr, ArithOp::Sub, left, right)
    }

    fn check_arith(
        &mut self,
        expr: &Expr,
        op: ArithOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Option<TypedExpr> {
        let left = self.check(lhs)?;
        let right = self.check(rhs)?;
        self.finish_arith(expr, op, left, right)
    }

    fn finish_arith(
        &mut self,
        expr: &Expr,
        op: ArithOp,
        left: TypedExpr,
        right: TypedExpr,
    ) -> Option<TypedExpr> {
        let (lt, rt) = (left.ty().as_scalar(), right.ty().as_scalar());
        // SPEC-ISSUES item 3: arithmetic over an optional operand is a static
        // type error in this implementation (CEL static typing, §6.1) — the
        // author coalesces with `?? …` first. Neither the spec nor A.6 pins
        // none-propagation, so we reject rather than silently invent it.
        if matches!(lt, Some(Type::Optional(_))) || matches!(rt, Some(Type::Optional(_))) {
            return self.error(
                expr,
                "arithmetic operand is optional; coalesce it (`x ?? 0`) before the operator",
            );
        }
        let class = match (op, lt, rt) {
            (ArithOp::Add, Some(Type::Text), Some(Type::Text)) => NumClass::TextConcat,
            // §11.5/A.5: a timestamp shifted by a duration (either operand order for
            // `+`, timestamp-first for `-`) yields a timestamp.
            (ArithOp::Add | ArithOp::Sub, Some(Type::Timestamp(_)), Some(Type::Duration))
            | (ArithOp::Add, Some(Type::Duration), Some(Type::Timestamp(_))) => NumClass::TimeShift,
            (_, Some(Type::Int), Some(Type::Int)) => NumClass::Int,
            (_, Some(Type::Decimal), Some(Type::Decimal))
            | (_, Some(Type::Int), Some(Type::Decimal))
            | (_, Some(Type::Decimal), Some(Type::Int)) => NumClass::Decimal,
            _ => {
                return self.error(
                    expr,
                    format!(
                        "operator has no type for operands `{}` and `{}`",
                        left.ty().describe(),
                        right.ty().describe()
                    ),
                );
            }
        };
        let result = match class {
            NumClass::Int => Type::Int,
            NumClass::Decimal => Type::Decimal,
            NumClass::TextConcat => Type::Text,
            // The shift preserves the timestamp operand's declared precision (A.5).
            NumClass::TimeShift => match (lt, rt) {
                (Some(Type::Timestamp(p)), _) | (_, Some(Type::Timestamp(p))) => Type::Timestamp(*p),
                _ => Type::timestamp(),
            },
        };
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(result),
            TypedKind::Arith {
                op,
                class,
                lhs: Box::new(left),
                rhs: Box::new(right),
            },
        ))
    }

    fn check_compare(
        &mut self,
        expr: &Expr,
        op: CmpOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Option<TypedExpr> {
        let left = self.check(lhs)?;
        let right = self.check(rhs)?;
        // §6.3: comparing a reference to a keyed row compares the ref to the row's
        // identity key (a subscription's `account` ref against the enforcing account
        // row, §15.3). Coerce the row side to its key value so the comparison is
        // between the ref and the row's scalar key.
        let (left, right) = coerce_ref_row_key(left, right);
        // A.9/§6.3: an object literal compared against a composite-keyed ref is
        // authoring syntax for the `$key`-order tuple — validate and normalize it
        // so the two compare as equal-valued composite keys (a mismatched or
        // wrong-arity object is a load-time type error, like the scalar path).
        let (left, right) = self.coerce_composite_ref_key(left, right)?;
        let (lt, rt) = match (left.ty().as_scalar(), right.ty().as_scalar()) {
            (Some(lt), Some(rt)) => (lt, rt),
            _ => return self.error(expr, "only scalar values are comparable"),
        };
        if !comparable(lt, rt) {
            // §6.3: refs into different target relations are statically
            // incomparable — a key-text collision must not leak across them.
            if let (Type::Ref(_), Type::Ref(_)) = (lt, rt) {
                return self.error(
                    expr,
                    "equality between refs targeting different relations is statically incomparable",
                );
            }
            return self.error(
                expr,
                format!(
                    "cannot compare `{}` with `{}`",
                    left.ty().describe(),
                    right.ty().describe()
                ),
            );
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Bool),
            TypedKind::Compare {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
            },
        ))
    }

    /// Validate and normalize an object literal compared against a composite-keyed
    /// ref to the ref target's `$key`-order tuple (§6.3, A.9), so the comparison is
    /// between two equal-valued composite keys rather than a struct against a ref.
    /// A mismatched-type or wrong-arity object is rejected at load (`None`), exactly
    /// as the scalar `==` type check rejects a mismatched scalar key.
    fn coerce_composite_ref_key(
        &mut self,
        left: TypedExpr,
        right: TypedExpr,
    ) -> Option<(TypedExpr, TypedExpr)> {
        fn composite_key_of(expr: &TypedExpr) -> Option<ExprType> {
            match expr.ty().as_scalar() {
                Some(Type::Ref(RefTarget::Composite(components))) => {
                    Some(ExprType::scalar(Type::Composite(components.clone())))
                }
                _ => None,
            }
        }
        if let Some(key) = composite_key_of(&left) {
            let right = self.coerce_composite_key(right, Some(&key))?;
            return Some((left, right));
        }
        if let Some(key) = composite_key_of(&right) {
            let left = self.coerce_composite_key(left, Some(&key))?;
            return Some((left, right));
        }
        Some((left, right))
    }

    fn check_logic(
        &mut self,
        expr: &Expr,
        op: LogicOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Option<TypedExpr> {
        let left = self.check(lhs)?;
        let right = self.check(rhs)?;
        if left.ty().as_scalar() != Some(&Type::Bool)
            || right.ty().as_scalar() != Some(&Type::Bool)
        {
            return self.error(expr, "`&&`/`||` require `bool` operands");
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Bool),
            TypedKind::Logic {
                op,
                lhs: Box::new(left),
                rhs: Box::new(right),
            },
        ))
    }

    fn check_in(&mut self, expr: &Expr, lhs: &Expr, rhs: &Expr) -> Option<TypedExpr> {
        let needle = self.check(lhs)?;
        let haystack = self.check(rhs)?;
        // §6.3/A.9: membership in a composite-keyed view takes an authoring object
        // naming each key component — validate and normalize it to the row's
        // `$key`-order tuple so it matches the row keys, exactly as `==` and the
        // `[{..}]` selector do (a mismatched/incomplete key is rejected at load).
        let needle = if let ExprType::View(row) = haystack.ty() {
            let key_type = row.key().cloned();
            self.coerce_composite_key(needle, key_type.as_ref())?
        } else {
            needle
        };
        let ok = match haystack.ty() {
            ExprType::Scalar(Type::Set(elem)) => Some(elem.as_ref()) == needle.ty().as_scalar(),
            ExprType::View(_) => true,
            _ => false,
        };
        if !ok {
            return self.error(expr, "`in` requires a set or view on the right");
        }
        Some(TypedExpr::new(
            expr.span,
            ExprType::scalar(Type::Bool),
            TypedKind::In {
                needle: Box::new(needle),
                haystack: Box::new(haystack),
            },
        ))
    }

    /// `a ?? b` — view fallback when `a` is empty, or scalar coalesce when `a`
    /// is optional (§7.4). One resolved [`TypedKind::Fallback`] serves both.
    fn check_fallback(&mut self, expr: &Expr, lhs: &Expr, rhs: &Expr) -> Option<TypedExpr> {
        let primary = self.check(lhs)?;
        let other = self.check(rhs)?;
        let ty = match (primary.ty(), other.ty()) {
            // §14.5: a static checker cannot prove the fallback branch is never taken,
            // so the result is unbounded if EITHER branch is — `[] ?? b` with an
            // unbounded `b` can deliver `b` whole. Copying only the primary's row
            // dropped the fallback's marker; OR it in. A bounding selector clears it.
            (ExprType::View(row), ExprType::View(other_row)) => {
                let unbounded = row.is_unbounded() || other_row.is_unbounded();
                ExprType::View(row.clone().unbounded(unbounded))
            }
            (ExprType::Scalar(Type::Optional(inner)), _) => ExprType::scalar((**inner).clone()),
            (ExprType::Scalar(_), _) => primary.ty().clone(),
            _ => return self.error(expr, "`??` operands must be two views or an optional value"),
        };
        Some(TypedExpr::new(
            expr.span,
            ty,
            TypedKind::Fallback {
                primary: Box::new(primary),
                other: Box::new(other),
            },
        ))
    }

    pub(crate) fn check_ternary(
        &mut self,
        expr: &Expr,
        cond: &Expr,
        then: &Expr,
        otherwise: &Expr,
    ) -> Option<TypedExpr> {
        let cond = self.check(cond)?;
        if cond.ty().as_scalar() != Some(&Type::Bool) {
            return self.error(expr, "a `? :` condition must be `bool`");
        }
        let then = self.check(then)?;
        let otherwise = self.check(otherwise)?;
        let ty = match (then.ty(), otherwise.ty()) {
            // §14.5: a static checker cannot prove which branch is taken, so the
            // result is unbounded if EITHER branch is — `@flag ? [] : b` with an
            // unbounded `b` can deliver `b` whole. Copying only one branch's row
            // dropped the other's marker; OR it in. A bounding selector clears it.
            (ExprType::View(row), ExprType::View(other)) => {
                let unbounded = row.is_unbounded() || other.is_unbounded();
                ExprType::View(row.clone().unbounded(unbounded))
            }
            (a, b) if a == b => a.clone(),
            // §7.4: `cond ? view : []` — an empty view branch adopts the other.
            (ExprType::View(row), _) | (_, ExprType::View(row)) => ExprType::View(row.clone()),
            _ => return self.error(expr, "both `? :` branches must share a type"),
        };
        Some(TypedExpr::new(
            expr.span,
            ty,
            TypedKind::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
        ))
    }

    /// A flat `|` / `&` chain (§7.4). Per SPEC-ISSUES item 25's resolution, `|`
    /// (union) and `&` (intersection) share one precedence level: a chain
    /// repeating a single combinator (`a | b | c`) is well-formed and folds
    /// strictly left-to-right, but a chain MIXING the two (`a | b & c`) is
    /// ambiguous — the two groupings differ observably in row order, projection,
    /// and identity (§7.4) — and is a static error the author disambiguates with
    /// `( )` grouping (CEL syntax, §6.1): `(a | b) & c` or `a | (b & c)`.
    /// Difference (`-`), `??`, and the `? :` conditional bind at their own
    /// grammar levels, so they never appear in this flat operator list. Every
    /// operand must be a view sharing the left's identity domain (key type).
    pub(crate) fn check_combination(
        &mut self,
        expr: &Expr,
        operands: &[Expr],
        operators: &[CombinatorOp],
    ) -> Option<TypedExpr> {
        // §7.4 / SPEC-ISSUES 25: reject an un-parenthesized chain mixing `|` and
        // `&` before typing it, so the ambiguous grouping never silently resolves
        // to the left-fold reading. A homogeneous chain (all-`|` or all-`&`) is
        // left-associative and passes.
        let unions = operators.iter().filter(|op| **op == CombinatorOp::Union).count();
        if unions != 0 && unions != operators.len() {
            return self.error(
                expr,
                "an un-parenthesized view combination that mixes `|` (union) and `&` \
                 (intersection) is ambiguous: the two groupings give different rows, \
                 projection, and identity (§7.4). Group it explicitly with `( )` — \
                 e.g. `(a | b) & c` or `a | (b & c)`",
            );
        }
        let mut iter = operands.iter();
        // The grammar always parses at least one operand; an empty chain can
        // only reach here through a hand-built AST. Still a diagnostic, never a
        // silent None: every rejection explains itself.
        let Some(first) = iter.next() else {
            return self.error(expr, "a `|`/`&` view combination has no operands");
        };
        let mut acc = self.check(first)?;
        let mut acc_row = match acc.ty() {
            ExprType::View(row) => row.clone(),
            _ => return self.error(first, "a `|`/`&` combinator operand must be a view"),
        };
        // §7.4/§6.3: the identity domain is the RELATION, not merely the key TYPE.
        // Two distinct target relations that only share a scalar key type (`.tasks
        // | .users`, both `text`-keyed) do NOT share an identity domain — §6.3
        // "values belonging to different target relations are statically
        // incomparable". A left-fold takes identity from the left operand (§7.4),
        // so the whole chain's relation is the first operand's.
        let left_relation = combinator_relation(&acc).map(str::to_owned);
        for (operand, op) in iter.zip(operators.iter()) {
            let right = self.check(operand)?;
            if let (Some(left), Some(right)) = (&left_relation, combinator_relation(&right))
                && left.as_str() != right
            {
                return self.error(
                    operand,
                    "combined views address different target relations, which do not share an \
                     identity domain (§7.4/§6.3); project both into a common synthetic `$key` \
                     first if you mean to combine heterogeneous sources",
                );
            }
            let right_unbounded = match right.ty() {
                ExprType::View(row) if row.key() == acc_row.key() => row.is_unbounded(),
                ExprType::View(_) => {
                    return self.error(
                        operand,
                        "combined views must share one identity domain (§7.4)",
                    );
                }
                _ => return self.error(operand, "a `|`/`&` combinator operand must be a view"),
            };
            let combine = match op {
                CombinatorOp::Union => CombineOp::Union,
                CombinatorOp::Intersect => CombineOp::Intersect,
            };
            // §14.5/§7.4: computing `a | b` / `a & b` enumerates every row of BOTH
            // operands (a union reads the right for its new identities, §7.4; an
            // intersection reads both), so the combined view is unbounded if EITHER
            // operand is — an unbounded recurring bucket on either side forces an
            // infinite-series read the terminal guard must reject. Copying only the
            // left's row dropped the right's marker; OR it in. A bounding selector
            // over the WHOLE combined view still clears it (`check_temporal_call`).
            let unbounded = acc_row.is_unbounded() || right_unbounded;
            acc_row = acc_row.unbounded(unbounded);
            acc = TypedExpr::new(
                expr.span,
                ExprType::View(acc_row.clone()),
                TypedKind::Combine {
                    op: combine,
                    lhs: Box::new(acc),
                    rhs: Box::new(right),
                },
            );
        }
        Some(acc)
    }
}

/// Whether two scalar types can be compared through the Annex B order.
///
/// Equal types compare; `int` and `decimal` compare after promotion; an
/// `optional<T>` compares only against a present value of its own base type `T`
/// (`optional<int>` vs `int`) or against `json`, so a bare `none`
/// (`optional<json>`) compares against a `json` value or an identical
/// `optional<json>` — never against an arbitrary optional. Comparing a typed
/// `optional<T>` to `none` with `==` is therefore rejected; `has()` is the
/// sanctioned absence idiom. Two refs compare
/// only when they name the same target relation (§6.3); and a ref compares
/// against a key of its own declared target — §6.3 "Equality between a row or
/// ref and a key of the same declared target compares the current typed key",
/// which is exactly the case where a key is supplied explicitly (a scalar key
/// against a scalar-keyed ref, or the composite key tuple against a
/// composite-keyed ref).
/// Coerce a ref-vs-keyed-row comparison to a ref-vs-key comparison (§6.3): when
/// exactly one side is a scalar `ref` and the other a keyed row, replace the row
/// with its identity key value (`.$key`), so the two compare as a ref against the
/// target's key type. Any other pairing is returned unchanged.
fn coerce_ref_row_key(left: TypedExpr, right: TypedExpr) -> (TypedExpr, TypedExpr) {
    fn key_type(expr: &TypedExpr) -> Option<ExprType> {
        match expr.ty() {
            ExprType::Row(row) => row.key().cloned(),
            _ => None,
        }
    }
    fn to_key(expr: TypedExpr, key: ExprType) -> TypedExpr {
        let span = expr.span();
        TypedExpr::new(span, key, TypedKind::Key(Box::new(expr)))
    }
    let left_ref = matches!(left.ty().as_scalar(), Some(Type::Ref(_)));
    let right_ref = matches!(right.ty().as_scalar(), Some(Type::Ref(_)));
    match (left_ref, right_ref) {
        (true, false) => match key_type(&right) {
            Some(key) => (left, to_key(right, key)),
            None => (left, right),
        },
        (false, true) => match key_type(&left) {
            Some(key) => (to_key(left, key), right),
            None => (left, right),
        },
        _ => (left, right),
    }
}

fn comparable(a: &Type, b: &Type) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Type::Int | Type::Decimal, Type::Int | Type::Decimal) => true,
        (Type::Optional(inner), other) | (other, Type::Optional(inner)) => {
            inner.as_ref() == other || matches!(other, Type::Json)
        }
        (Type::Ref(x), Type::Ref(y)) => x == y,
        (Type::Ref(target), other) | (other, Type::Ref(target))
            if !matches!(other, Type::Ref(_)) =>
        {
            ref_key_matches(target, other)
        }
        _ => false,
    }
}

/// Whether an explicitly supplied key value type is the declared key type of a
/// ref's target (§6.3). A scalar-keyed target compares against its scalar key
/// type; a composite-keyed target compares against the object/tuple of its
/// component key types.
fn ref_key_matches(target: &RefTarget, key: &Type) -> bool {
    match target {
        RefTarget::Scalar(inner) => inner.as_ref() == key,
        RefTarget::Composite(components) => match key {
            // The composite key value type, compared positionally in `$key` order.
            Type::Composite(supplied) => {
                supplied.len() == components.len()
                    && supplied.iter().zip(components).all(|((_, a), (_, b))| a == b)
            }
            // A.9: a named object selector is authoring syntax for the same tuple —
            // its fields (name-keyed) match the composite key components by name.
            Type::Struct(struct_ty) => {
                struct_ty.fields().count() == components.len()
                    && components.iter().all(|(name, ty)| {
                        struct_ty.field(name).is_some_and(|supplied| supplied == ty)
                    })
            }
            _ => false,
        },
    }
}
