//! The deterministic evaluation [`Environment`] an admission or view read runs
//! against (§8.12).
//!
//! It owns the materialized package-root [`Row`], the request's parameter cells,
//! the two generative samples fixed once per request — `now()` (A.5) and the
//! seed behind `uuid()` (SPEC-ISSUES item 4, per-call-site) — and the temporal
//! index the `.$at`/`.$between`/`.$all` selectors resolve against (§14.1). Every
//! method is a pure lookup, so "same environment ⇒ same result" holds by
//! construction.

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::{CallSite, Cell, Environment, EvalError, Row, RowId, TemporalQuery};
use liasse_value::{Timestamp, Uuid};

use crate::generator::derive_uuid;
use crate::materialize::row_interval;

/// A read-only, deterministic evaluation context.
pub(crate) struct RuntimeEnv {
    root: Row,
    params: BTreeMap<String, Cell>,
    now: Timestamp,
    seed: u64,
    /// The full extant rows of each bucketed collection (§14.2), each carrying
    /// its `$from`/`$until` interval cells. A temporal selector reads over the
    /// full set so `.$all` and a back-dated `.$at` observe rows that have already
    /// left their active interval.
    temporal: Vec<Vec<Row>>,
}

impl RuntimeEnv {
    /// Build the context from a materialized `root`, the request `params`, the
    /// fixed generative samples, and the bucketed-collection temporal index.
    pub(crate) fn new(
        root: Row,
        params: BTreeMap<String, Cell>,
        now: Timestamp,
        seed: u64,
        temporal: Vec<Vec<Row>>,
    ) -> Self {
        Self { root, params, now, seed, temporal }
    }

    /// The working set a temporal selector ranges over (§14.1). When `base` is
    /// exactly a bare bucketed collection — its identity set equals that
    /// collection's rows active at [`Self::now`] — the query re-derives activity
    /// from that collection's *full* extant set, so `.$all` and a back-dated
    /// `.$at` see inactive rows (§14.2). Otherwise (a filtered or projected base)
    /// the query ranges over `base` as given, using each row's carried interval.
    fn working_set<'a>(&'a self, base: &'a [Row]) -> &'a [Row] {
        let base_ids: BTreeSet<&RowId> = base.iter().map(Row::id).collect();
        for extant in &self.temporal {
            let active: BTreeSet<&RowId> =
                extant.iter().filter(|row| active_at(row, self.now)).map(Row::id).collect();
            if active == base_ids {
                return extant;
            }
        }
        base
    }
}

impl Environment for RuntimeEnv {
    fn root(&self) -> &Row {
        &self.root
    }

    fn param(&self, name: &str) -> Option<Cell> {
        self.params.get(name).cloned()
    }

    fn structural(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn import(&self, _name: &str) -> Option<Cell> {
        None
    }

    fn now(&self) -> Timestamp {
        self.now
    }

    fn uuid(&self, site: CallSite) -> Uuid {
        derive_uuid(self.seed, site.span())
    }

    /// Resolve a temporal selector over a bucketed base view (§14.1). Each row's
    /// `[from, until)` comes from its `$from`/`$until` interval cells; a row is
    /// selected by the half-open activity rule for the query.
    fn temporal(&self, base: &[Row], query: &TemporalQuery) -> Result<Vec<Row>, EvalError> {
        Ok(self
            .working_set(base)
            .iter()
            .filter(|row| selects(row, query))
            .cloned()
            .collect())
    }
}

/// Whether `row`'s half-open interval `[from, until)` selects it for `query`
/// (§14.1): `.$at(t)` is `from <= t < until`; `.$between(a, b)` is a non-empty
/// intersection with `[a, b)`; `.$all` selects every extant row (§14.2). An
/// absent bound is unbounded on that side.
fn selects(row: &Row, query: &TemporalQuery) -> bool {
    let (from, until) = row_interval(row);
    match query {
        TemporalQuery::All => true,
        TemporalQuery::At(at) => from.is_none_or(|f| *at >= f) && until.is_none_or(|u| *at < u),
        TemporalQuery::Between(a, b) => from.is_none_or(|f| f < *b) && until.is_none_or(|u| u > *a),
    }
}

/// Whether `row` is active at instant `now` — the bare-read predicate (§14.1),
/// used to recover which collection a temporal base names.
fn active_at(row: &Row, now: Timestamp) -> bool {
    selects(row, &TemporalQuery::At(now))
}
