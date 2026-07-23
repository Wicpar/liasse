//! Evaluation of operators: exact integer/decimal/text arithmetic, comparison
//! through the Annex B order, short-circuit logic, membership, and the
//! conditional/fallback forms (§6.1, A.6, Annex B).

use std::borrow::Cow;
use std::cmp::Ordering;

use liasse_value::bigdecimal::{BigDecimal, Zero};
use liasse_value::num_bigint::BigInt;
use liasse_value::{Decimal, Integer, RefKey, Text, Value};

use crate::env::Cell;
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::semantics::DivisionRounding;
use crate::typed::{ArithOp, CmpOp, LogicOp, NumClass, TypedExpr};

impl Evaluator<'_> {
    pub(crate) fn eval_scalar(&mut self, expr: &TypedExpr) -> Result<Value, EvalError> {
        match self.eval(expr)? {
            Cell::Scalar(value) => Ok(value),
            _ => Err(EvalError::ShapeMismatch { expected: "a scalar value" }),
        }
    }

    pub(crate) fn eval_arith(
        &mut self,
        op: ArithOp,
        class: NumClass,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let left = self.eval_scalar(lhs)?;
        let right = self.eval_scalar(rhs)?;
        let value = match class {
            NumClass::TextConcat => Value::Text(concat(&left, &right)?),
            NumClass::Int => int_arith(op, &left, &right)?,
            // §4.4/A.6: decimal `/` rounds its quotient under the package's
            // declared division rounding mode, which the environment resolves.
            NumClass::Decimal => decimal_arith(op, &left, &right, self.env.decimal_division())?,
        };
        Ok(Cell::Scalar(value))
    }

    pub(crate) fn eval_neg(
        &mut self,
        class: NumClass,
        operand: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let value = self.eval_scalar(operand)?;
        let negated = match (class, value) {
            (NumClass::Int, Value::Int(int)) => Value::Int(Integer::from(-int.as_bigint().clone())),
            (NumClass::Decimal, Value::Decimal(dec)) => {
                Value::Decimal(Decimal::from_big_decimal(-dec.as_big_decimal().clone()))
            }
            _ => return Err(EvalError::ShapeMismatch { expected: "a numeric operand" }),
        };
        Ok(Cell::Scalar(negated))
    }

    pub(crate) fn eval_compare(
        &mut self,
        op: CmpOp,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let left = self.eval_scalar(lhs)?;
        let right = self.eval_scalar(rhs)?;
        let ordering = compare(&left, &right);
        let verdict = match op {
            CmpOp::Eq => ordering == Ordering::Equal,
            CmpOp::Ne => ordering != Ordering::Equal,
            CmpOp::Lt => ordering == Ordering::Less,
            CmpOp::Le => ordering != Ordering::Greater,
            CmpOp::Gt => ordering == Ordering::Greater,
            CmpOp::Ge => ordering != Ordering::Less,
        };
        Ok(Cell::Scalar(Value::Bool(verdict)))
    }

    pub(crate) fn eval_logic(
        &mut self,
        op: LogicOp,
        lhs: &TypedExpr,
        rhs: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let left = matches!(self.eval_scalar(lhs)?, Value::Bool(true));
        let verdict = match op {
            LogicOp::And if !left => false,
            LogicOp::Or if left => true,
            _ => matches!(self.eval_scalar(rhs)?, Value::Bool(true)),
        };
        Ok(Cell::Scalar(Value::Bool(verdict)))
    }

    pub(crate) fn eval_not(&mut self, operand: &TypedExpr) -> Result<Cell, EvalError> {
        let value = matches!(self.eval_scalar(operand)?, Value::Bool(true));
        Ok(Cell::Scalar(Value::Bool(!value)))
    }

    pub(crate) fn eval_in(
        &mut self,
        needle: &TypedExpr,
        haystack: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        // §6.3: a keyed-row needle denotes its identity key, so membership by a
        // row (`/stores['s3'] in .file.$stored`, §18.11) tests that key against
        // the haystack's row keys — the same identity a set/view is compared by.
        let value = match self.eval(needle)? {
            Cell::Scalar(value) => value,
            Cell::Row(row) => row.key().clone(),
            _ => return Err(EvalError::ShapeMismatch { expected: "a scalar or keyed row" }),
        };
        let found = match self.eval(haystack)? {
            // §6.3: a set's members carry the needle's exact element type (the
            // checker pins it), so a `ref` needle compares against `ref` members
            // directly — no key unwrap.
            Cell::Scalar(Value::Set(members)) => members.contains(&value),
            // §6.3/§7.6/A.9: a view's identity is its rows' target keys, and "a ref
            // value is a target key", so a `ref` needle denotes its target key here.
            // Unwrap a scalar-keyed ref to that key before matching — exactly as
            // `select_by_keys` does — so `task.owner in .people` is the §6.3 identity
            // comparison rather than a never-equal `ref`-vs-key mismatch.
            Cell::Collection(rows) => {
                let key = super::ref_key_value(&value);
                rows.iter().any(|row| row.key() == key.as_ref())
            }
            _ => return Err(EvalError::ShapeMismatch { expected: "a set or view" }),
        };
        Ok(Cell::Scalar(Value::Bool(found)))
    }

    pub(crate) fn eval_ternary(
        &mut self,
        cond: &TypedExpr,
        then: &TypedExpr,
        otherwise: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        if matches!(self.eval_scalar(cond)?, Value::Bool(true)) {
            self.eval(then)
        } else {
            self.eval(otherwise)
        }
    }

    pub(crate) fn eval_fallback(
        &mut self,
        expr: &TypedExpr,
        primary: &TypedExpr,
        other: &TypedExpr,
    ) -> Result<Cell, EvalError> {
        let value = self.eval(primary)?;
        let empty = match &value {
            Cell::Collection(rows) => rows.is_empty(),
            Cell::Scalar(Value::None) => true,
            _ => false,
        };
        if empty {
            self.eval(other)
        } else {
            let _ = expr;
            Ok(value)
        }
    }
}

fn concat(left: &Value, right: &Value) -> Result<Text, EvalError> {
    match (left, right) {
        (Value::Text(a), Value::Text(b)) => {
            Ok(Text::new(format!("{}{}", a.as_str(), b.as_str())))
        }
        _ => Err(EvalError::ShapeMismatch { expected: "two text operands" }),
    }
}

fn int_arith(op: ArithOp, left: &Value, right: &Value) -> Result<Value, EvalError> {
    let (Value::Int(a), Value::Int(b)) = (left, right) else {
        return Err(EvalError::ShapeMismatch { expected: "two int operands" });
    };
    let (a, b) = (a.as_bigint(), b.as_bigint());
    let result = match op {
        ArithOp::Add => a + b,
        ArithOp::Sub => a - b,
        ArithOp::Mul => a * b,
        // A.6: integer division truncates toward zero (num-bigint `/`/`%`).
        ArithOp::Div => {
            if is_zero_int(b) {
                return Err(EvalError::DivisionByZero);
            }
            a / b
        }
        // SPEC-ISSUES item 3: remainder takes the dividend's sign (truncated),
        // consistent with A.6 integer division; documented, not spec-pinned.
        ArithOp::Rem => {
            if is_zero_int(b) {
                return Err(EvalError::DivisionByZero);
            }
            a % b
        }
    };
    Ok(Value::Int(Integer::from(result)))
}

fn decimal_arith(
    op: ArithOp,
    left: &Value,
    right: &Value,
    rounding: DivisionRounding,
) -> Result<Value, EvalError> {
    let a = to_big_decimal(left)?;
    let b = to_big_decimal(right)?;
    let result = match op {
        ArithOp::Add => a + b,
        ArithOp::Sub => a - b,
        ArithOp::Mul => a * b,
        ArithOp::Div => {
            if b.is_zero() {
                return Err(EvalError::DivisionByZero);
            }
            crate::eval::decimal::divide(&a, &b, rounding)?
        }
        // SPEC-ISSUES item 3: remainder carries the dividend's sign (truncated
        // toward zero), consistent with the integer choice and A.6 division —
        // not the quotient. Exact, then normalized (item 1).
        ArithOp::Rem => {
            if b.is_zero() {
                return Err(EvalError::DivisionByZero);
            }
            decimal_remainder(&a, &b)
        }
    };
    Ok(Value::Decimal(Decimal::from_big_decimal(result)))
}

/// Exact decimal remainder `a - trunc(a / b) * b`, with the quotient truncated
/// toward zero so the remainder takes the dividend's sign (A.6 / SPEC-ISSUES
/// item 3).
///
/// This is `BigDecimal`'s `%`: it brings both mantissas to the common scale
/// `max(a.scale, b.scale)`, takes the integer remainder (`BigInt` `%` truncates
/// toward zero, so the sign follows the dividend), and rescales. That is exact
/// for every magnitude. It must NOT be computed by truncating `a / b`: the `/`
/// operator rounds the quotient to a bounded significant-digit precision, so a
/// quotient one ulp below an integer (a long run of trailing 9s, e.g.
/// `(3·10^100 − 1) / 10^100` = `2.999…9`) rounds UP to the next integer, and
/// truncating that rounded value yields the wrong quotient and a remainder
/// outside `[0, |b|)` — `(3·10^100 − 1) % 10^100` would be `−1` instead of
/// `10^100 − 1`. `normalized()` renders the exact result minimal-scale (A.1).
fn decimal_remainder(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    (a % b).normalized()
}

fn to_big_decimal(value: &Value) -> Result<BigDecimal, EvalError> {
    match value {
        Value::Int(int) => Ok(BigDecimal::from(int.as_bigint().clone())),
        Value::Decimal(dec) => Ok(dec.as_big_decimal().clone()),
        _ => Err(EvalError::ShapeMismatch { expected: "a numeric operand" }),
    }
}

fn is_zero_int(value: &BigInt) -> bool {
    value.sign() == liasse_value::num_bigint::Sign::NoSign
}

/// Compare two scalars through the Annex B order, promoting a mixed
/// `int`/`decimal` pair to decimal first (they are numerically comparable per
/// §6.1 even though the cross-type value rank would separate them).
fn compare(left: &Value, right: &Value) -> Ordering {
    // §6.3: a ref compared with a key of its declared target compares the
    // current typed key. When exactly one side is a ref, unwrap it to its target
    // key identity (a scalar, or the positional composite tuple) and compare
    // against the explicitly supplied key. Two refs keep their own key-ordering
    // comparison (`Ref` `Ord` already compares keys).
    match (ref_target_key(left), ref_target_key(right)) {
        (Some(key), None) => return compare(key.as_ref(), right),
        (None, Some(key)) => return compare(left, key.as_ref()),
        _ => {}
    }
    match (left, right) {
        (Value::Int(_), Value::Decimal(_)) | (Value::Decimal(_), Value::Int(_)) => {
            match (to_big_decimal(left), to_big_decimal(right)) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => left.cmp(right),
            }
        }
        _ => left.cmp(right),
    }
}

/// The target-key identity a ref exposes when compared against an explicitly
/// supplied key (§6.3, A.9): a scalar-keyed ref its inner scalar, a
/// composite-keyed ref the positional [`Value::Composite`] tuple of its
/// components — the same value a composite key selector normalizes to. `None`
/// for a non-ref value.
fn ref_target_key(value: &Value) -> Option<Cow<'_, Value>> {
    match value {
        Value::Ref(reference) => match reference.key() {
            RefKey::Scalar(inner) => Some(Cow::Borrowed(inner)),
            RefKey::Composite(components) => Some(Cow::Owned(Value::Composite(components.clone()))),
        },
        _ => None,
    }
}
