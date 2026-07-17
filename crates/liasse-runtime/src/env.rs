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

use liasse_expr::{CallSite, Cell, Environment, EvalError, KeyringSelector, Row, RowId, TemporalQuery};
use liasse_value::{Timestamp, Uuid, Value};

use crate::generator::derive_uuid;
use crate::host::HostDispatch;
use crate::keyring_view::{snapshot_for, KeyringSnapshot};
use crate::materialize::row_interval;

/// A read-only, deterministic evaluation context.
pub(crate) struct RuntimeEnv<'a> {
    root: Row,
    params: BTreeMap<String, Cell>,
    /// Lexical local bindings `name` a mutation program introduced with a
    /// `name = …` statement (§8.1), resolved by [`Environment::binding`].
    bindings: BTreeMap<String, Cell>,
    /// Structural bindings `$name` bound in the current context (e.g. the
    /// `$target` of an `$on_delete` patch, §21.1), resolved by
    /// [`Environment::structural`].
    structurals: BTreeMap<String, Cell>,
    now: Timestamp,
    seed: u64,
    /// The full extant rows of each bucketed collection (§14.2), each carrying
    /// its `$from`/`$until` interval cells. A temporal selector reads over the
    /// full set so `.$all` and a back-dated `.$at` observe rows that have already
    /// left their active interval.
    temporal: Vec<Vec<Row>>,
    /// The keyring version-view snapshots (§17.2) a keyring public selector
    /// resolves against. Each names the versions active (`.$current`) and
    /// accepted (`.$accepted`/`.$public`) at the read instant.
    keyrings: Vec<KeyringSnapshot>,
    /// The host-namespace dispatch a resolved `namespace.function(...)` call in a
    /// view/default/computed value runs through (§16.2/§16.3). A [`HostDispatch::none`]
    /// answers no namespace, so a host call in a position with no live binding
    /// faults as a contract breach; only a mutation admission, a genesis seed, and
    /// a view read carry a live dispatch.
    hosts: HostDispatch<'a>,
}

impl<'a> RuntimeEnv<'a> {
    /// Build the context from a materialized `root`, the request `params`, the
    /// fixed generative samples, the bucketed-collection temporal and keyring
    /// version indices, and the host-call dispatch. The inputs are each a
    /// distinct, unrelated environment slot rather than a bundle to abstract away,
    /// so the constructor takes them positionally.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        root: Row,
        params: BTreeMap<String, Cell>,
        bindings: BTreeMap<String, Cell>,
        structurals: BTreeMap<String, Cell>,
        now: Timestamp,
        seed: u64,
        temporal: Vec<Vec<Row>>,
        keyrings: Vec<KeyringSnapshot>,
        hosts: HostDispatch<'a>,
    ) -> Self {
        Self { root, params, bindings, structurals, now, seed, temporal, keyrings, hosts }
    }

    /// The working set a temporal selector ranges over (§14.1). When `base` is
    /// exactly a bare bucketed collection — its identity set equals that
    /// collection's rows active at [`Self::now`] — the query re-derives activity
    /// from that collection's *full* extant set, so `.$all` and a back-dated
    /// `.$at` see inactive rows (§14.2). Otherwise (a filtered or projected base)
    /// the query ranges over `base` as given, using each row's carried interval.
    fn working_set<'w>(&'w self, base: &'w [Row]) -> &'w [Row] {
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

impl Environment for RuntimeEnv<'_> {
    fn root(&self) -> &Row {
        &self.root
    }

    fn param(&self, name: &str) -> Option<Cell> {
        self.params.get(name).cloned()
    }

    fn binding(&self, name: &str) -> Option<Cell> {
        self.bindings.get(name).cloned()
    }

    fn structural(&self, name: &str) -> Option<Cell> {
        self.structurals.get(name).cloned()
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

    /// Resolve a keyring public selector over the ring's version view (§17.2).
    /// `base` is the ring's full version collection; the owning snapshot answers
    /// the lifecycle subset: the active version for `.$current`, the accepted set
    /// for `.$accepted`/`.$public`, every version for `.$versions`. An
    /// environment that owns no matching keyring rejects (§17.2 contract breach).
    fn keyring(&self, base: &[Row], selector: KeyringSelector) -> Result<Vec<Row>, EvalError> {
        let snapshot = snapshot_for(&self.keyrings, base).ok_or(EvalError::NoKeyringIndex)?;
        Ok(match selector {
            KeyringSelector::Current => snapshot.current(),
            KeyringSelector::Accepted | KeyringSelector::Public => snapshot.accepted_rows(),
            KeyringSelector::Versions => snapshot.rows.clone(),
        })
    }

    /// Invoke a resolved host-namespace function in a view/default/computed value
    /// (§16.2/§16.3): dispatch to the bound host component through the conformance
    /// guard, so a nonconforming return or a verifier rejection is a typed
    /// evaluation failure that commits no effect. The call is pure-recomputable —
    /// the checker admitted it only where the effect class permits (a pure
    /// function in a read/replay position), so re-evaluation is deterministic.
    fn host_call(&self, namespace: &str, function: &str, args: &[Value]) -> Result<Value, EvalError> {
        self.hosts.eval_call(namespace, function, args)
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
