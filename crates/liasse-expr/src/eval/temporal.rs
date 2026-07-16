//! Evaluation of the temporal selectors `.$at`, `.$between`, `.$all` (§14.1).
//!
//! The evaluator reduces the base view to rows and the selector's argument
//! expressions to instants, then hands both to the environment's temporal index
//! ([`Environment::temporal`](crate::Environment::temporal)). Activity
//! resolution — which rows are active in `[from, until)` — lives entirely in the
//! environment, so evaluation stays a pure function of it (§8.12). The one check
//! the evaluator owns is §14.1's non-empty-range rule for `.$between`.

use liasse_value::{Timestamp, Value};

use crate::env::{Cell, Row, TemporalQuery};
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::typed::{TypedExpr, TypedTemporal};

impl Evaluator<'_> {
    pub(crate) fn eval_temporal(
        &mut self,
        base: &TypedExpr,
        query: &TypedTemporal,
    ) -> Result<Cell, EvalError> {
        let rows: Vec<Row> = self.eval_view(base)?.into_iter().map(|scope| scope.row).collect();
        let query = self.reduce_query(query)?;
        Ok(Cell::Collection(self.env.temporal(&rows, &query)?))
    }

    /// Reduce a typed temporal selector to a value query, evaluating its instant
    /// operands. `.$between` rejects an empty or reversed range here (§14.1).
    fn reduce_query(&mut self, query: &TypedTemporal) -> Result<TemporalQuery, EvalError> {
        match query {
            TypedTemporal::All => Ok(TemporalQuery::All),
            TypedTemporal::At(instant) => Ok(TemporalQuery::At(self.instant(instant)?)),
            TypedTemporal::Between { start, end } => {
                let start = self.instant(start)?;
                let end = self.instant(end)?;
                if end <= start {
                    return Err(EvalError::EmptyTemporalRange);
                }
                Ok(TemporalQuery::Between(start, end))
            }
        }
    }

    fn instant(&mut self, expr: &TypedExpr) -> Result<Timestamp, EvalError> {
        match self.eval(expr)? {
            Cell::Scalar(Value::Timestamp(instant)) => Ok(instant),
            _ => Err(EvalError::ShapeMismatch { expected: "a timestamp instant" }),
        }
    }
}
