//! Evaluation of the aggregate functions (§7.5): `count`, `sum`, `avg`, `min`,
//! `max`, `distinct`, each skipping absent inputs and giving the spec's empty
//! identities.

use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{Decimal, Integer, Type, Value};

use crate::env::Cell;
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::ty::ExprType;
use crate::typed::{AggFunc, TypedExpr};

impl Evaluator<'_> {
    pub(crate) fn eval_aggregate(
        &mut self,
        expr: &TypedExpr,
        func: AggFunc,
        source: &TypedExpr,
        field: Option<&str>,
    ) -> Result<Cell, EvalError> {
        let scopes = self.eval_view(source)?;
        if func == AggFunc::Count {
            return Ok(Cell::Scalar(Value::Int(Integer::from(scopes.len() as i64))));
        }
        let field = field.ok_or(EvalError::ShapeMismatch { expected: "an aggregated field" })?;
        let mut values = Vec::new();
        for scope in &scopes {
            match scope.row.cell(field) {
                Some(Cell::Scalar(Value::None)) | None => {} // §7.5: skip absent inputs.
                Some(Cell::Scalar(value)) => values.push(value.clone()),
                Some(_) => return Err(EvalError::ShapeMismatch { expected: "a scalar field" }),
            }
        }
        Ok(Cell::Scalar(combine(func, values, expr.ty())?))
    }
}

/// Combine collected field values under an aggregate (§7.5).
fn combine(func: AggFunc, values: Vec<Value>, result_ty: &ExprType) -> Result<Value, EvalError> {
    match func {
        AggFunc::Count => Ok(Value::Int(Integer::from(values.len() as i64))),
        AggFunc::Sum => Ok(sum(values, result_ty)),
        AggFunc::Avg => average(values),
        AggFunc::Min => Ok(values.into_iter().min_by(|a, b| a.cmp(b)).unwrap_or(Value::None)),
        AggFunc::Max => Ok(values.into_iter().max_by(|a, b| a.cmp(b)).unwrap_or(Value::None)),
        AggFunc::Distinct => Ok(Value::Set(values.into_iter().collect())),
    }
}

fn sum(values: Vec<Value>, result_ty: &ExprType) -> Value {
    let any_decimal = values.iter().any(|v| matches!(v, Value::Decimal(_)));
    if values.is_empty() {
        // §7.5: empty sum is numeric zero of the field type.
        return match result_ty.as_scalar() {
            Some(Type::Decimal) => Value::Decimal(Decimal::from(BigInt::from(0))),
            _ => Value::Int(Integer::from(0)),
        };
    }
    if any_decimal {
        let total: BigDecimal = values
            .iter()
            .map(to_decimal)
            .fold(BigDecimal::from(0), |acc, next| acc + next);
        Value::Decimal(Decimal::from_big_decimal(total))
    } else {
        let total: BigInt = values
            .iter()
            .filter_map(|v| match v {
                Value::Int(int) => Some(int.as_bigint().clone()),
                _ => None,
            })
            .fold(BigInt::from(0), |acc, next| acc + next);
        Value::Int(Integer::from(total))
    }
}

/// §7.5: `avg` converts every input exactly to decimal and divides under the
/// package decimal semantics (normalized per SPEC-ISSUES item 1); empty input
/// yields `none`.
fn average(values: Vec<Value>) -> Result<Value, EvalError> {
    if values.is_empty() {
        return Ok(Value::None);
    }
    let count = values.len();
    let total: BigDecimal = values
        .iter()
        .map(to_decimal)
        .fold(BigDecimal::from(0), |acc, next| acc + next);
    let divisor = BigDecimal::from(BigInt::from(count as i64));
    let mean = crate::eval::decimal::divide(&total, &divisor)?;
    Ok(Value::Decimal(Decimal::from_big_decimal(mean)))
}

fn to_decimal(value: &Value) -> BigDecimal {
    match value {
        Value::Int(int) => BigDecimal::from(int.as_bigint().clone()),
        Value::Decimal(dec) => dec.as_big_decimal().clone(),
        _ => BigDecimal::from(0),
    }
}
