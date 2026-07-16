//! Evaluation of operators: exact integer/decimal/text arithmetic, comparison
//! through the Annex B order, short-circuit logic, membership, and the
//! conditional/fallback forms (§6.1, A.6, Annex B).

use std::cmp::Ordering;

use liasse_value::bigdecimal::{BigDecimal, RoundingMode, Zero};
use liasse_value::num_bigint::BigInt;
use liasse_value::{Decimal, Integer, RefKey, Text, Value};

use crate::env::Cell;
use crate::error::EvalError;
use crate::eval::Evaluator;
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
            NumClass::Decimal => decimal_arith(op, &left, &right)?,
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
        let value = self.eval_scalar(needle)?;
        let found = match self.eval(haystack)? {
            Cell::Scalar(Value::Set(members)) => members.contains(&value),
            Cell::Collection(rows) => rows.iter().any(|row| row.key() == &value),
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

fn decimal_arith(op: ArithOp, left: &Value, right: &Value) -> Result<Value, EvalError> {
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
            crate::eval::decimal::divide(&a, &b)?
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
/// toward zero so the remainder takes the dividend's sign (SPEC-ISSUES item 3).
fn decimal_remainder(a: &BigDecimal, b: &BigDecimal) -> BigDecimal {
    let quotient = (a / b).with_scale_round(0, RoundingMode::Down);
    (a - quotient * b).normalized()
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
    // current typed key. When exactly one side is a scalar-keyed ref, compare
    // its underlying key against the explicitly supplied key. Two refs keep
    // their own key-ordering comparison (`Ref` `Ord` already compares keys).
    match (ref_scalar_key(left), ref_scalar_key(right)) {
        (Some(key), None) => return compare(key, right),
        (None, Some(key)) => return compare(left, key),
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

/// The underlying scalar key of a scalar-keyed ref (§6.3, A.9): the value a ref
/// exposes when compared against an explicitly supplied key. A composite-keyed
/// ref is left intact — its comparison is handled by `Value`'s own ordering.
fn ref_scalar_key(value: &Value) -> Option<&Value> {
    match value {
        Value::Ref(reference) => match reference.key() {
            RefKey::Scalar(inner) => Some(inner),
            RefKey::Composite(_) => None,
        },
        _ => None,
    }
}
