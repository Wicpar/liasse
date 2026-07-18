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
use crate::eval::{Evaluator, RowScope};
use crate::typed::{TypedExpr, TypedKind, TypedSelector, TypedTemporal};

impl Evaluator<'_> {
    pub(crate) fn eval_temporal(
        &mut self,
        base: &TypedExpr,
        query: &TypedTemporal,
    ) -> Result<Cell, EvalError> {
        // §7.1: a bare or a filtered/projected bucketed base NAMES the ONE
        // collection the selector addresses (`bind_name_of` recovers that name
        // through the `Select`/`Project` spine). Resolve the read against that
        // collection's extant *by name* — it distinguishes a dormant base from any
        // other empty-active bucket, where the empty identity set could not.
        let base_name = super::views::bind_name_of(base);
        let query = self.reduce_query(query)?;
        if let Some(name) = &base_name
            && let Some(active) = self.env.temporal_by_name(name, &query)
        {
            // The environment returned the collection's rows active for the query
            // over its FULL extant (§14.2), *without* the base's own
            // filter/projection. Re-apply that transform now (§7.1): a filtered
            // base narrows those rows, a projected base reshapes them — so the read
            // is (rows the base view keeps) ∩ (active for the query), never the
            // collection's whole extant. A bare base contributes the identity
            // transform, leaving the active rows unchanged.
            return Ok(Cell::Collection(self.rebase_view(base, active)?));
        }
        // A multi-source/anonymous base (`bind_name_of` gave `None`) or a base whose
        // name addresses no bucketed collection: range over the base as evaluated,
        // letting the environment's identity-set fallback recover its extant.
        let rows: Vec<Row> = self.eval_view(base)?.into_iter().map(|scope| scope.row).collect();
        Ok(Cell::Collection(self.env.temporal(&rows, base_name.as_deref(), &query)?))
    }

    /// Re-apply a single-source temporal base's own filter/projection to the rows
    /// its named collection contributes active for the query (§7.1/§14.1), yielding
    /// the base view's rows. The leaf `Field`/`Traverse` names the collection and
    /// contributes `rows` unchanged; each enclosing `Select`/`Project` layer narrows
    /// or reshapes them exactly as it would over the live collection, so the temporal
    /// read honors the predicate/projection instead of exposing the full extant.
    fn rebase_view(&mut self, base: &TypedExpr, rows: Vec<Row>) -> Result<Vec<Row>, EvalError> {
        Ok(self.rebase_scopes(base, rows)?.into_iter().map(|scope| scope.row).collect())
    }

    /// Evaluate the base view's [`Select`](TypedKind::Select)/[`Project`](TypedKind::Project)
    /// spine over `rows` — the collection's active extant — instead of the live
    /// collection. Mirrors [`Evaluator::eval_view`]'s single-source arms, but seeds
    /// the leaf with the recovered rows rather than re-reading the root, so the same
    /// narrowing/reshaping helpers apply the base's transform to the extant.
    fn rebase_scopes(
        &mut self,
        base: &TypedExpr,
        rows: Vec<Row>,
    ) -> Result<Vec<RowScope>, EvalError> {
        match base.kind() {
            // The leaf naming the collection: the recovered rows ARE its rows, so a
            // bare base ranges over them whole (the identity transform).
            TypedKind::Field { .. } | TypedKind::Traverse { .. } => {
                Ok(rows.into_iter().map(RowScope::bare).collect())
            }
            TypedKind::Select { base: inner, selector: TypedSelector::Bind { name, condition } } => {
                let base_scopes = self.rebase_scopes(inner, rows)?;
                self.select_bind_scopes(base_scopes, name, condition)
            }
            TypedKind::Select { base: inner, selector: TypedSelector::Keys(keys) } => {
                let inner: Vec<Row> =
                    self.rebase_scopes(inner, rows)?.into_iter().map(|scope| scope.row).collect();
                let selected = self.select_by_keys(&inner, keys)?;
                Ok(selected.into_iter().map(RowScope::bare).collect())
            }
            TypedKind::Project { source, projection } => {
                let scopes = self.rebase_scopes(source, rows)?;
                let projected = self.project_scopes(scopes, projection)?;
                Ok(projected.into_iter().map(RowScope::bare).collect())
            }
            // `bind_name_of` returns `Some` only for the single-source spine above,
            // so a rebased base is always one of these arms; any other shape names no
            // single collection and never reaches here.
            _ => Ok(rows.into_iter().map(RowScope::bare).collect()),
        }
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
