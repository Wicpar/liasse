//! The per-request evaluation context: everything an expression needs beyond
//! the prospective state.
//!
//! Building a [`RuntimeEnv`] materializes the whole package root, so an
//! [`EvalCtx`] rebuilds it on demand from the current prospective state — later
//! statements and defaults observe earlier effects (§8.1). The cost is
//! proportional to live rows; incremental materialization is a documented
//! optimization seam the [`Environment`](liasse_expr::Environment) boundary keeps
//! open.

use std::collections::BTreeMap;

use liasse_expr::{Cell, Row, RowId, TypedExpr};
use liasse_value::{Timestamp, Value};

use crate::bucket;
use crate::compiled::{Compiled, CompiledCollection};
use crate::env::RuntimeEnv;
use crate::error::Rejection;
use crate::materialize::{self, FieldMap};
use crate::schema::Schema;
use crate::state::Prospective;

/// The out-of-band inputs of one admitted request.
pub(crate) struct EvalCtx<'a> {
    pub(crate) schema: Schema<'a>,
    pub(crate) compiled: &'a Compiled,
    pub(crate) params: BTreeMap<String, Cell>,
    pub(crate) now: Timestamp,
    pub(crate) seed: u64,
}

impl EvalCtx<'_> {
    /// The evaluation environment over the current prospective state, with
    /// bucketed collections filtered to the rows active at [`Self::now`] (§14).
    pub(crate) fn env(&self, prospective: &Prospective) -> RuntimeEnv {
        RuntimeEnv::new(self.root(prospective), self.params.clone(), self.now, self.seed)
    }

    /// The temporal-aware package-root row: bucketed collections expose only the
    /// rows active at [`Self::now`]; every other collection is materialized in
    /// full (§8.2, §14.2).
    pub(crate) fn root(&self, prospective: &Prospective) -> Row {
        let keep = |name: &str, fields: &FieldMap| self.active(name, fields);
        materialize::materialize_root_filtered(self.schema, prospective.working(), &keep)
    }

    /// Whether collection `name`'s row `fields` is currently readable: always for
    /// a non-bucketed collection, else its bucket activity at [`Self::now`].
    fn active(&self, name: &str, fields: &FieldMap) -> bool {
        match (self.compiled.bucket(name), self.compiled.collection(name)) {
            (Some(bucket), Some(collection)) => bucket::is_active(bucket, collection, fields, self.now),
            _ => true,
        }
    }

    /// Evaluate a typed expression with `current` as `.`, against the current
    /// prospective state.
    pub(crate) fn eval(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        current: &Cell,
    ) -> Result<Cell, Rejection> {
        let env = self.env(prospective);
        typed.evaluate(&env, current).map_err(Rejection::from)
    }
}

/// A logical row cell over a field map, for evaluating a default, normalizer, or
/// row check whose `.` is the provisional row (§5.1, §5.10). Every declared field
/// is present (absent values read as `none`), so a check can reference a sibling.
pub(crate) fn row_cell(collection: &CompiledCollection, fields: &FieldMap) -> Cell {
    let cells = collection.fields.iter().map(|field| {
        let value = fields.get(&field.name).cloned().unwrap_or(Value::None);
        (field.name.clone(), Cell::Scalar(value))
    });
    let key = collection
        .key
        .iter()
        .map(|name| fields.get(name).cloned().unwrap_or(Value::None))
        .next()
        .unwrap_or(Value::None);
    Cell::Row(Box::new(Row::new(RowId::leaf(0), key, cells)))
}
