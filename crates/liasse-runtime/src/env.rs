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

use liasse_expr::{
    BlobPlacement, CallSite, Cell, Environment, EvalError, KeyringSelector, Row, RowId,
    TemporalQuery,
};
use liasse_value::{BlobDescriptor, Timestamp, Uuid, Value};

use crate::generator::derive_uuid;
use crate::host::HostDispatch;
use crate::keyring_view::{snapshot_for, KeyringSnapshot};
use crate::materialize::row_interval;
use crate::source_bucket::SourceBucketHorizon;

/// The engine's §18.5 logical placement ledger: the recorded placement facts of
/// each committed blob, keyed by its canonical `$sha512` digest.
///
/// §18.5 placement (`$stored`/`$satisfied`/`$surplus`) is engine-recorded state,
/// not something a pure expression can derive from the value tree: physical
/// placement and the current policy resolution live in the blob subsystem. The
/// engine records a blob's facts (through [`Engine::record_blob_placement`], which
/// the surface/driver feeds from its `blob_placement_state`, §18.5), and every
/// evaluation environment carries a snapshot so a `.blob.$satisfied`/`.$stored`/
/// `.$surplus` read resolves against them.
///
/// [`Engine::record_blob_placement`]: crate::Engine::record_blob_placement
#[derive(Debug, Clone, Default)]
pub(crate) struct BlobPlacements(BTreeMap<String, BlobPlacement>);

impl BlobPlacements {
    /// Record the §18.5 facts of the blob whose canonical digest is `digest`,
    /// replacing any prior facts (a re-record after a policy change updates them).
    pub(crate) fn record(&mut self, digest: impl Into<String>, facts: BlobPlacement) {
        self.0.insert(digest.into(), facts);
    }

    /// The recorded facts for `digest`, if any.
    pub(crate) fn get(&self, digest: &str) -> Option<&BlobPlacement> {
        self.0.get(digest)
    }
}

/// One bucketed collection's full extant row set (§14.2) tagged with the name of
/// the collection it belongs to.
///
/// A temporal selector addresses a collection by NAME (§7.1): `.periods.$at(t)`
/// ranges over `periods`. [`RuntimeEnv::working_set`] resolves a bare base against
/// the extant tagged with the collection the selector names, so a dormant (empty)
/// base still ranges over its own collection instead of colliding — by the shared
/// empty identity set — with an earlier empty-active bucket.
#[derive(Clone)]
pub(crate) struct NamedExtant {
    /// The declared collection name the extant belongs to (the `$key`-carrying
    /// bucketed collection member name; a source-backed bucket carries its
    /// collection name too).
    pub(crate) name: String,
    /// The collection's full extant rows (§14.2), each carrying its `$from`/`$until`
    /// interval cells; inactive rows are retained so `.$all` and a back-dated
    /// `.$at` observe rows that have already left their active interval.
    pub(crate) rows: Vec<Row>,
}

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
    /// The full extant rows of each STORED bucketed collection (§14.2), each
    /// tagged with its collection name ([`NamedExtant`]) and carrying its
    /// `$from`/`$until` interval cells. A temporal selector reads over the full set
    /// so `.$all` and a back-dated `.$at` observe rows that have already left their
    /// active interval. Source-backed buckets are NOT held here: their extant set
    /// is regenerated on demand from [`Self::source_horizon`] at a horizon the
    /// selector's own bound drives (§14.5), because an unbounded recurring series
    /// past the clock is not materialized until a bounded selector asks for it.
    temporal: Vec<NamedExtant>,
    /// The regenerable extant set of every source-backed bucket (§14.4–§14.6).
    /// `None` when the package declares none. A temporal selector regenerates it up
    /// to a horizon its bound supplies (§14.5); a bare/`.$all` read never reaches it
    /// on an unbounded recurring bucket, which the checker rejects.
    source_horizon: Option<SourceBucketHorizon<'a>>,
    /// The keyring version-view snapshots (§17.2) a keyring public selector
    /// resolves against. Each names the versions active (`.$current`) and
    /// accepted (`.$accepted`/`.$public`) at the read instant.
    keyrings: Vec<KeyringSnapshot>,
    /// The §18.5 logical placement facts a blob placement member resolves against
    /// (`blob.$satisfied`/`$stored`/`$surplus`), keyed by canonical `$sha512`
    /// digest. A snapshot of the engine's ledger; empty when no blob placement has
    /// been recorded, so a placement read then faults as a contract breach.
    placements: BlobPlacements,
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
        temporal: Vec<NamedExtant>,
        source_horizon: Option<SourceBucketHorizon<'a>>,
        keyrings: Vec<KeyringSnapshot>,
        placements: BlobPlacements,
        hosts: HostDispatch<'a>,
    ) -> Self {
        Self {
            root,
            params,
            bindings,
            structurals,
            now,
            seed,
            temporal,
            source_horizon,
            keyrings,
            placements,
            hosts,
        }
    }

    /// The generation horizon a temporal selector drives (§14.5): the EXCLUSIVE upper
    /// bound on generated interval starts. The generator yields every interval whose
    /// start lies below it and computes exactly the one boundary that closes the last
    /// such interval, never the following one — so a horizon coinciding with a valid
    /// boundary cannot force computing (and, under §14.7/A.4 `reject`, failing on) the
    /// next boundary, which would drop the already-generated in-window series.
    ///
    /// Each selector maps its own half-open reach onto that bound, kept at least one
    /// tick past the clock so the interval active at `now` is still generated for the
    /// working-set reconciliation ([`Self::working_set`]):
    /// - `.$at(t)` is inclusive on `t` (`from <= t`): the interval starting exactly at
    ///   `t` must be generated, so the exclusive-start bound is one tick past it.
    /// - `.$between(a, b)` is half-open on `b` (`from < b`): an interval starting at
    ///   `b` is already excluded, so `b` itself is the bound — no boundary at or past
    ///   it is computed; only the clock floor is nudged one tick past `now`.
    /// - `.$all` carries no explicit bound and stays at the clock; the checker forbids
    ///   it over an unbounded recurring bucket, so no unbounded enumeration escapes.
    fn horizon_for(&self, query: &TemporalQuery) -> Timestamp {
        match query {
            TemporalQuery::All => self.now.next_tick(),
            TemporalQuery::At(instant) => (*instant).max(self.now).next_tick(),
            TemporalQuery::Between(_, end) => (*end).max(self.now.next_tick()),
        }
    }

    /// The working set a temporal selector ranges over (§14.1). When `base` is a
    /// bare bucketed collection it re-derives activity from that collection's
    /// *full* extant set, so `.$all` and a back-dated `.$at` see inactive rows
    /// (§14.2); otherwise (a filtered or projected base) the query ranges over
    /// `base` as given, using each row's carried interval.
    ///
    /// `base_name` is the collection the selector ADDRESSES (§7.1) — `Some` for a
    /// bare bucketed base, `None` for a filtered/projected one. When present, the
    /// full extant is recovered by that name: an empty (dormant) base's identity
    /// set is shared by every empty-active bucket, so it is not a distinguishing
    /// key, whereas the addressed name is. When absent, the collection is
    /// recovered by identity-set equality (a filtered base whose active rows equal
    /// a collection's still re-derives that collection's inactive rows), and a
    /// base matching none ranges over itself.
    ///
    /// `source` is the freshly regenerated source-bucket extant set for this
    /// query's horizon (§14.5); it is searched after the stored buckets. Stored
    /// and source-backed buckets never share a collection name, so the two chains
    /// hold disjoint names and a name lookup is unambiguous.
    fn working_set<'w>(
        &'w self,
        base: &'w [Row],
        base_name: Option<&str>,
        source: &'w [NamedExtant],
    ) -> &'w [Row] {
        if let Some(name) = base_name
            && let Some(extant) =
                self.temporal.iter().chain(source.iter()).find(|extant| extant.name == name)
        {
            return &extant.rows;
        }
        let base_ids: BTreeSet<&RowId> = base.iter().map(Row::id).collect();
        for extant in self.temporal.iter().chain(source.iter()) {
            let active: BTreeSet<&RowId> =
                extant.rows.iter().filter(|row| active_at(row, self.now)).map(Row::id).collect();
            if active == base_ids {
                return &extant.rows;
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
    /// selected by the half-open activity rule for the query. `base_name` names
    /// the collection the selector addresses (§7.1) for a bare bucketed base.
    fn temporal(
        &self,
        base: &[Row],
        base_name: Option<&str>,
        query: &TemporalQuery,
    ) -> Result<Vec<Row>, EvalError> {
        // §14.5: regenerate the source-backed buckets' extant set up to the horizon
        // this selector's own bound drives, so a read past the clock still generates
        // the covering periods. Empty when the package has no source-backed bucket,
        // leaving stored-bucket resolution exactly as before.
        let source = match &self.source_horizon {
            Some(horizon) => horizon.extant_to(self.horizon_for(query)),
            None => Vec::new(),
        };
        Ok(self
            .working_set(base, base_name, &source)
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

    /// Resolve a §18.5 blob placement member off the engine's placement ledger,
    /// keyed by the descriptor's canonical `$sha512` digest. An unrecorded
    /// descriptor is a placement-index miss ([`EvalError::NoBlobPlacement`]): the
    /// blob's placement was never recorded, so the read cannot resolve — the
    /// contract-breach signal the mutation/view path turns into a dropped
    /// response, exactly as an unbound keyring or temporal read.
    fn blob_placement(&self, descriptor: &BlobDescriptor) -> Result<BlobPlacement, EvalError> {
        self.placements
            .get(&descriptor.sha512().to_canonical_text())
            .cloned()
            .ok_or(EvalError::NoBlobPlacement)
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
