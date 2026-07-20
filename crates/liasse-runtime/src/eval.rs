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

use liasse_model::Node;

use crate::bucket;
use crate::compiled::{Compiled, CompiledCollection, CompiledComputed, CompiledStruct};
use crate::env::{NamedExtant, RuntimeEnv};
use crate::error::Rejection;
use crate::generator::Generation;
use crate::materialize::{self, FieldMap, Interval, Temporal};
use crate::schema::Schema;
use crate::state::Prospective;

/// The out-of-band inputs of one admitted request.
pub(crate) struct EvalCtx<'a> {
    pub(crate) schema: Schema<'a>,
    pub(crate) compiled: &'a Compiled,
    pub(crate) params: BTreeMap<String, Cell>,
    pub(crate) now: Timestamp,
    pub(crate) seed: u64,
    /// The keyring version-view snapshots (§17.2) an environment answers a
    /// keyring public selector against, materialized under each ring name in the
    /// package root. Empty when the package declares no keyring.
    pub(crate) keyrings: &'a [crate::keyring_view::KeyringSnapshot],
    /// The engine's §18.5 blob placement ledger (`blob.$satisfied`/`$stored`/
    /// `$surplus`), keyed by canonical `$sha512` digest. A snapshot is carried into
    /// every environment this context builds, so a mutation `return` and a `$view`
    /// reading a placement member resolve it. Empty when the package has recorded
    /// no blob placement.
    pub(crate) placements: &'a crate::env::BlobPlacements,
    /// Request-scoped structural bindings introduced by the admitting context —
    /// `$actor` and, when the authenticator declared one, `$session` (§11.1).
    /// Every environment this context builds carries them, so a mutation program,
    /// its patches, and its `return` all resolve `$actor`/`$session`. Empty for a
    /// public or internal call, and for genesis and view reads (§11.1).
    pub(crate) context: BTreeMap<String, Cell>,
    /// The host-namespace dispatch a mutation program's `ns.fn(args)` call runs
    /// against (§16.4, §17.7): the resolved `$requires` namespaces and the live
    /// keyrings a `cose.sign` reaches. [`HostDispatch::none`] for genesis, views,
    /// and migration, where a host call flows through the pure expression checker.
    pub(crate) hosts: crate::host::HostDispatch<'a>,
    /// The installed module instances visible to a `.modules::iface` read (§13.9),
    /// folded into the package-root row before views. `None` for every evaluation
    /// that is not a root-engine module-aware read (genesis, mutation admission,
    /// a plain view, a child interface read); the [`ModuleHost`](crate::ModuleHost)
    /// supplies it only when reading a root view over its installed children.
    pub(crate) modules: Option<&'a crate::modules::ModuleAggregate>,
}

impl<'a> EvalCtx<'a> {
    /// The request's fixed `now()` instant (A.5) — the context-free clock a §15.6
    /// meter accessor and a spend's default `$time` read.
    pub(crate) const fn now(&self) -> Timestamp {
        self.now
    }

    /// The evaluation environment over the current prospective state (§8.12): the
    /// package root with bucketed collections filtered to the rows active at
    /// [`Self::now`] (§14.1), plus the full extant set of each bucketed collection
    /// so a temporal selector can re-derive activity over inactive rows (§14.2).
    pub(crate) fn env(&self, prospective: &Prospective) -> RuntimeEnv<'a> {
        self.env_with(prospective, BTreeMap::new(), BTreeMap::new())
    }

    /// The evaluation environment carrying `bindings` (the lexical locals a
    /// `name = …` statement bound, §8.1) and `structurals` (context bindings such
    /// as the `$target` of an `$on_delete` patch, §21.1).
    pub(crate) fn env_with(
        &self,
        prospective: &Prospective,
        bindings: BTreeMap<String, Cell>,
        structurals: BTreeMap<String, Cell>,
    ) -> RuntimeEnv<'a> {
        RuntimeEnv::new(
            self.root(prospective),
            self.params.clone(),
            bindings,
            self.with_context(structurals),
            self.now,
            self.seed,
            // A view/mutation-statement/patch read produces at most one occurrence
            // per generated call site, so it evaluates at the root generation; a
            // per-row default resolution uses [`Self::env_generative`] instead.
            Generation::ROOT,
            self.temporal_index(prospective),
            self.source_horizon(prospective, self.now),
            self.keyrings.to_vec(),
            self.placements.clone(),
            self.hosts,
        )
    }

    /// The evaluation environment for one admitted row's default resolution
    /// (SPEC-ISSUES item 4, §5.1/§8.12), tagged with `generation` so a `uuid()`
    /// default is fresh per row (two rows of one request never share a generated
    /// key), while a state-derived default still reads the same pre-statement
    /// state. It carries the request context (`$config`/`$actor`/`$session`) and
    /// no lexical locals, exactly as [`Self::env`].
    fn env_generative(&self, prospective: &Prospective, generation: Generation) -> RuntimeEnv<'a> {
        RuntimeEnv::new(
            self.root(prospective),
            self.params.clone(),
            BTreeMap::new(),
            self.with_context(BTreeMap::new()),
            self.now,
            self.seed,
            generation,
            self.temporal_index(prospective),
            self.source_horizon(prospective, self.now),
            self.keyrings.to_vec(),
            self.placements.clone(),
            self.hosts,
        )
    }

    /// The evaluation environment that folds each computed value, nested view, or
    /// declared view over `base` as the root (§5.2, §7.1). It carries the request
    /// context so a folded expression resolves `$config` (§13.1) — and, harmlessly,
    /// `$actor`/`$session`, which no foldable expression can reference — while
    /// introducing no lexical local bindings.
    fn fold_env(
        &self,
        base: Row,
        temporal: Vec<NamedExtant>,
        source_horizon: Option<crate::source_bucket::SourceBucketHorizon<'a>>,
    ) -> RuntimeEnv<'a> {
        RuntimeEnv::new(
            base,
            self.params.clone(),
            BTreeMap::new(),
            self.context.clone(),
            self.now,
            self.seed,
            // A folded computed value / view produces at most one occurrence per
            // generated call site per row, so it reads at the root generation.
            Generation::ROOT,
            temporal,
            source_horizon,
            self.keyrings.to_vec(),
            self.placements.clone(),
            self.hosts,
        )
    }

    /// Merge the request-scoped context bindings (`$actor`/`$session`, §11.1) with
    /// the caller's own structurals. A caller-supplied name wins, so a feature that
    /// rebinds a structural (none does in CORE) keeps precedence.
    fn with_context(&self, structurals: BTreeMap<String, Cell>) -> BTreeMap<String, Cell> {
        if self.context.is_empty() {
            return structurals;
        }
        let mut merged = self.context.clone();
        merged.extend(structurals);
        merged
    }

    /// The temporal-aware package-root row: bucketed collections expose only the
    /// rows active at [`Self::now`] (each carrying its `$from`/`$until` interval
    /// cells); every other collection is materialized in full (§8.2, §14.2). Named
    /// view members are then materialized as cells too, so an expression may read
    /// one view through another (`.other_view`, §7.1) — the public-surface `$view`
    /// forms lifted to `.view_name` resolve here.
    pub(crate) fn root(&self, prospective: &Prospective) -> Row {
        let base = self.base_root(prospective);
        // §17.2: materialize each declared keyring's version metadata as a keyed
        // collection under the ring name, before computed values and views, so a
        // `/ring.$current` selector (and a view reading it) resolves its base.
        let base = self.expose_keyrings(base);
        // §14.4–§14.6: materialize each source-backed / recurring bucket collection
        // from its `$source` view, before meters and views, so a pool source or a
        // `.collection.$at`/`.$between` read resolves against the derived rows active
        // at the clock.
        let base = self.expose_source_buckets(base, self.now);
        // §13.9: fold the installed module instances into the containing rows so a
        // `.modules::iface` aggregation resolves before computed values and views
        // read it. Only present for a root-engine module-aware read; every other
        // evaluation leaves the module spaces empty.
        let base = self.expose_modules(base);
        let base = self.expose_computed(prospective, base);
        // §5.2/§5.3: fold each root static-struct member's own computed values onto
        // its materialized struct-row (recursing into nested structs), before views
        // so a view or projection reads a struct-nested computed value like a field.
        let base = self.expose_struct_computed(prospective, base);
        let base = self.expose_nested_views(prospective, base);
        let base = self.expose_root_computed(prospective, base);
        // §15.6: fold the `.<meter>.balance`/`.pools` and `funding` accessor cells
        // onto the row tree before views, so a `$view` may read remaining capacity.
        let base = crate::meter::accessor::expose(self, prospective, base);
        self.expose_views(prospective, base)
    }

    /// Fold each root-level computed value (§5.2) into the package-root row as a
    /// cell, evaluated with the root itself as `.` so `count(.items)` reads a
    /// sibling collection. Iterated to a fixed point (bounded by their count) so
    /// one root computed may read another regardless of declaration order. Exposed
    /// before views, so a view may project a root computed value.
    fn expose_root_computed(&self, prospective: &Prospective, base: Row) -> Row {
        if self.compiled.root_computed.is_empty() {
            return base;
        }
        let temporal = self.temporal_index(prospective);
        let env = self.fold_env(base.clone(), temporal, self.source_horizon(prospective, self.now));
        fold_computed(&env, &self.compiled.root_computed, base)
    }

    /// Fold each collection row's computed values (§5.2) into the row as cells, so
    /// a view, projection, or `return` reads a computed value like a stored field.
    /// Evaluated against the base root (writable state); a computed value reading
    /// `.field` sees the row, a computed value reading another computed value in the
    /// same row is resolved by the per-row fixed point. Views are exposed afterwards,
    /// so a view may read a computed value.
    ///
    /// §5.2 makes a computed value participate "like any other value", so a
    /// collection computed value may read ANOTHER collection's computed value through
    /// a `/`-selector (`/config["main"].doubled`). Folding a single pass against the
    /// unfolded base leaves that target absent, so this iterates to a cross-collection
    /// fixed point: each pass rebuilds the fold env from the progressively-folded root
    /// and re-folds every collection, so the reader observes the target's FOLDED
    /// value. The computed dependency graph is acyclic (§5.1), so it converges; the
    /// number of collections carrying computed values bounds the longest
    /// cross-collection chain (one pass advances the resolved frontier by one hop).
    fn expose_computed(&self, prospective: &Prospective, base: Row) -> Row {
        let bound = self.compiled.collections.iter().filter(|c| !c.computed.is_empty()).count();
        if bound == 0 {
            return base;
        }
        let temporal = self.temporal_index(prospective);
        let mut base = base;
        for _ in 0..bound {
            let env = self.fold_env(base.clone(), temporal.clone(), self.source_horizon(prospective, self.now));
            let next = self.fold_collection_computed(&env, base.clone());
            if next == base {
                break;
            }
            base = next;
        }
        base
    }

    /// One pass of the §5.2 collection-computed fold: fold each collection row's
    /// computed values against `env`, whose root is the current (partially folded)
    /// package root. Factored out of [`Self::expose_computed`] so its cross-collection
    /// fixed point can rebuild `env` and re-fold between passes.
    fn fold_collection_computed(&self, env: &RuntimeEnv<'_>, base: Row) -> Row {
        let cells: Vec<(String, Cell)> = base
            .cells()
            .map(|(name, cell)| match (cell, self.compiled.collection(name)) {
                (Cell::Collection(rows), Some(collection)) if !collection.computed.is_empty() => {
                    let folded =
                        rows.iter().map(|row| fold_computed(env, &collection.computed, row.clone())).collect();
                    (name.clone(), Cell::Collection(folded))
                }
                _ => (name.clone(), cell.clone()),
            })
            .collect();
        Row::new(base.id().clone(), base.key().clone(), cells)
    }

    /// Fold each root static-struct member's read-only computed values (§5.2/§5.3)
    /// onto its materialized struct-row, so a view or projection reads a
    /// struct-nested computed value like a stored field. A computed value's `.` is
    /// the struct row and its `^` the containing row (§6.2); the fold recurses into
    /// nested static structs to any depth (Annex C.7). Only the root singleton
    /// structs are folded here — they materialize as a keyless `Cell::Row`; a
    /// collection-nested struct materializes as a `Value::Struct` scalar and remains
    /// a documented seam.
    fn expose_struct_computed(&self, prospective: &Prospective, base: Row) -> Row {
        let structs = &self.compiled.root_singleton.structs;
        if structs.iter().all(|meta| !meta.has_computed()) {
            return base;
        }
        let temporal = self.temporal_index(prospective);
        let env = self.fold_env(base.clone(), temporal, self.source_horizon(prospective, self.now));
        let parents = [Cell::Row(Box::new(base.clone()))];
        let cells: Vec<(String, Cell)> = base
            .cells()
            .map(|(name, cell)| match (cell, structs.iter().find(|meta| &meta.name == name)) {
                (Cell::Row(row), Some(meta)) => {
                    let folded = fold_struct_computed(&env, meta, &parents, (**row).clone());
                    (name.clone(), Cell::Row(Box::new(folded)))
                }
                _ => (name.clone(), cell.clone()),
            })
            .collect();
        Row::new(base.id().clone(), base.key().clone(), cells)
    }

    /// Fold the installed module instances (§13.9) into the containing rows: a
    /// root-level `$modules` space becomes a keyed instance collection on the root,
    /// a row-scoped one becomes a keyed instance collection on each row of its
    /// containing collection (§13.2). Only runs for a root-engine module-aware read;
    /// every other evaluation carries no aggregate, so the module spaces stay empty
    /// and a `.modules::iface` read there is an empty stream.
    fn expose_modules(&self, base: Row) -> Row {
        let Some(modules) = self.modules else { return base };
        if self.compiled.module_spaces.is_empty() {
            return base;
        }
        modules.fold_into(base, self.compiled.module_spaces.iter().map(|space| space.path.as_slice()))
    }

    /// Fold each collection's nested `$view` members (§7.1) into its rows as cells,
    /// evaluated with the row as `.` — the row-scoped analogue of [`expose_views`],
    /// so a `/companies[@c].catalog` read of a `catalog: ".modules::iface { … }"`
    /// nested view resolves against the row (which already carries its injected
    /// module spaces). A nested view that faults over a given row is left absent, so
    /// a reader of it faults exactly as an unmaterialized member would.
    fn expose_nested_views(&self, prospective: &Prospective, base: Row) -> Row {
        if self.compiled.collections.iter().all(|c| c.views.is_empty()) {
            return base;
        }
        let temporal = self.temporal_index(prospective);
        let env = self.fold_env(base.clone(), temporal, self.source_horizon(prospective, self.now));
        let cells: Vec<(String, Cell)> = base
            .cells()
            .map(|(name, cell)| match (cell, self.compiled.collection(name)) {
                (Cell::Collection(rows), Some(collection)) if !collection.views.is_empty() => {
                    let folded = rows.iter().map(|row| fold_views(&env, &collection.views, row.clone())).collect();
                    (name.clone(), Cell::Collection(folded))
                }
                _ => (name.clone(), cell.clone()),
            })
            .collect();
        Row::new(base.id().clone(), base.key().clone(), cells)
    }

    /// Fold each declared keyring's version-metadata rows (§17.2) into the
    /// package-root row as a keyed collection under the ring name, so a keyring
    /// public selector's base (`/ring`) resolves to the same version rows the
    /// environment's keyring index classifies. A package with no keyring is
    /// unchanged.
    fn expose_keyrings(&self, base: Row) -> Row {
        let mut root = base;
        for snapshot in self.keyrings {
            root = with_cell(root, &snapshot.name, Cell::Collection(snapshot.rows.clone()));
        }
        root
    }

    fn base_root(&self, prospective: &Prospective) -> Row {
        let keep = |name: &str, fields: &FieldMap| self.active(name, fields);
        let interval = |name: &str, fields: &FieldMap| self.interval(name, fields);
        let temporal = Temporal { keep: &keep, interval: &interval };
        materialize::materialize_root_filtered(self.schema, prospective.working(), &temporal)
    }

    /// Fold each source-backed / recurring bucket collection (§14.4–§14.6) into
    /// `base` as a keyed collection cell, materialized from its `$source` view over
    /// `base` and filtered to the rows active at `now` (§14.1, §15.1). A package
    /// with no source-backed bucket is unchanged. `base` must not itself carry the
    /// derived cells (a source view reads stored collections, not derived buckets),
    /// so this runs on the base root before computed values, meters, and views.
    fn expose_source_buckets(&self, base: Row, now: Timestamp) -> Row {
        if self.compiled.source_buckets.is_empty() {
            return base;
        }
        let inputs = self.bucket_inputs(&base, now);
        let mut root = base.clone();
        for bucket in &self.compiled.source_buckets {
            // The horizon is the exclusive upper bound on generated interval starts
            // (§14.5); one tick past `now` keeps the interval whose start coincides
            // with the clock (a period boundary landing exactly on `now`) in the pool.
            let rows = bucket.materialize(&inputs, now.next_tick(), true);
            root = with_cell(root, &bucket.name, Cell::Collection(rows));
        }
        root
    }

    /// The materialization inputs for the source-backed buckets: the base root
    /// (stored collections, no derived cells) plus the request context.
    fn bucket_inputs<'b>(&'b self, base_root: &'b Row, now: Timestamp) -> crate::source_bucket::BucketInputs<'b> {
        crate::source_bucket::BucketInputs {
            base_root,
            params: &self.params,
            context: &self.context,
            now,
            seed: self.seed,
            keyrings: self.keyrings,
        }
    }

    /// Validate every source-backed bucket's series for admission (§14.5): reject a
    /// transition that would produce a non-advancing recurrence or a series bound at
    /// or before its start, for any source row of any derived bucket. Evaluated over
    /// the prospective state, so an insert, a source edit, or a change to referenced
    /// period data (a plan's `period`) is caught before commit.
    pub(crate) fn validate_source_series(&self, prospective: &Prospective) -> Result<(), Rejection> {
        if self.compiled.source_buckets.is_empty() {
            return Ok(());
        }
        let base = self.expose_keyrings(self.base_root(prospective));
        let inputs = self.bucket_inputs(&base, self.now);
        for bucket in &self.compiled.source_buckets {
            bucket.validate(&inputs)?;
        }
        Ok(())
    }

    /// The full extant derived rows of every source-backed bucket at `now` (§14.2):
    /// the working set a temporal selector re-derives activity over. The generation
    /// horizon is the exclusive upper bound on interval starts (§14.5); one tick past
    /// `now` keeps a period whose boundary coincides with the clock in the set.
    fn source_bucket_extant(&self, base_root: &Row, now: Timestamp) -> Vec<Vec<Row>> {
        if self.compiled.source_buckets.is_empty() {
            return Vec::new();
        }
        let inputs = self.bucket_inputs(base_root, now);
        self.compiled
            .source_buckets
            .iter()
            .map(|bucket| bucket.materialize(&inputs, now.next_tick(), false))
            .collect()
    }

    /// The `[from, until)` interval of every materialized source-bucket row at
    /// `now`, keyed by row identity — the index a meter reads to filter and order
    /// bucketed pools drawn from a source-backed collection (§15.1, §15.2). Each
    /// derived row carries its `$from`/`$until` cells, so the same identity a
    /// projected pool row keeps resolves its interval.
    pub(crate) fn source_bucket_interval_index(
        &self,
        prospective: &Prospective,
        now: Timestamp,
    ) -> BTreeMap<RowId, Interval> {
        let mut index = BTreeMap::new();
        if self.compiled.source_buckets.is_empty() {
            return index;
        }
        let base = self.expose_keyrings(self.base_root(prospective));
        for rows in self.source_bucket_extant(&base, now) {
            for row in rows {
                index.insert(row.id().clone(), materialize::row_interval(&row));
            }
        }
        index
    }

    /// Evaluate each declared view against the root and fold its result into the
    /// root row as a same-named cell (§7.1). Views are resolved to a fixed point
    /// so one view may reference another regardless of declaration order; a view
    /// that never resolves (its source is not yet materialized) is simply left
    /// out, so an expression that reads it faults exactly as before.
    fn expose_views(&self, prospective: &Prospective, mut root: Row) -> Row {
        if self.compiled.views.is_empty() {
            return root;
        }
        let temporal = self.temporal_index(prospective);
        let source_horizon = self.source_horizon(prospective, self.now);
        let mut pending: Vec<&crate::compiled::CompiledView> = self.compiled.views.iter().collect();
        loop {
            let mut progressed = false;
            let mut still = Vec::new();
            for view in pending {
                let env = self.fold_env(root.clone(), temporal.clone(), source_horizon.clone());
                let current = Cell::Row(Box::new(root.clone()));
                match view.expr.evaluate(&env, &current) {
                    Ok(cell) => {
                        root = with_cell(root, &view.name, cell);
                        progressed = true;
                    }
                    Err(_) => still.push(view),
                }
            }
            pending = still;
            if !progressed || pending.is_empty() {
                break;
            }
        }
        root
    }

    /// The full extant rows of every bucketed collection (§14.2), the working set
    /// [`RuntimeEnv`] substitutes when a temporal selector reads a bare bucketed
    /// collection, so `.$all` and a back-dated `.$at` observe inactive rows.
    fn temporal_index(&self, prospective: &Prospective) -> Vec<NamedExtant> {
        self.temporal_index_at(prospective, self.now)
    }

    /// Whether collection `name`'s row `fields` is currently readable: always for
    /// a non-bucketed collection, else its bucket activity at [`Self::now`].
    fn active(&self, name: &str, fields: &FieldMap) -> bool {
        self.active_at(name, fields, self.now)
    }

    /// [`Self::active`] at an explicit instant — the meter pool resolution reads a
    /// bucketed source in the temporal context of the spend (§15.1).
    pub(crate) fn active_at(&self, name: &str, fields: &FieldMap, now: Timestamp) -> bool {
        match (self.compiled.bucket(name), self.compiled.find_collection(name)) {
            (Some(bucket), Some(collection)) => bucket::is_active(bucket, collection, fields, now),
            _ => true,
        }
    }

    /// Collection `name`'s row interval `[from, until)` at [`Self::now`], or
    /// `None` when it is not bucketed (§14.1).
    fn interval(&self, name: &str, fields: &FieldMap) -> Option<Interval> {
        self.interval_at(name, fields, self.now)
    }

    /// [`Self::interval`] at an explicit instant (§15.1 spend-time pool context).
    pub(crate) fn interval_at(&self, name: &str, fields: &FieldMap, now: Timestamp) -> Option<Interval> {
        match (self.compiled.bucket(name), self.compiled.find_collection(name)) {
            (Some(bucket), Some(collection)) => Some(bucket::interval_bounds(bucket, collection, fields, now)),
            _ => None,
        }
    }

    /// Materialize the single row at `address` (with its nested collections)
    /// evaluated at instant `now` — the enforcing/spend row a meter resolves its
    /// pools and metadata over in the temporal context of the spend (§15.1). Each
    /// bucketed nested collection carries its `$from`/`$until` interval cells at
    /// `now`, and computed values are folded (§5.2).
    pub(crate) fn materialize_row_at(
        &self,
        prospective: &Prospective,
        decl_path: &[String],
        address: &liasse_store::RowAddress,
        now: Timestamp,
    ) -> Option<Row> {
        let collection = self.schema.collection_at_path(decl_path)?;
        let keep = |name: &str, fields: &FieldMap| self.active_at(name, fields, now);
        let interval = |name: &str, fields: &FieldMap| self.interval_at(name, fields, now);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let row = materialize::materialize_row(self.schema, collection, address, prospective.working(), &temporal)?;
        match self.compiled.collection_at(decl_path) {
            Some(compiled) if !compiled.computed.is_empty() => {
                let env = self.env_at(prospective, now);
                Some(fold_computed(&env, &compiled.computed, row))
            }
            _ => Some(row),
        }
    }

    /// An evaluation environment over the prospective state whose virtual clock is
    /// `now` (a spend-time meter evaluation).
    pub(crate) fn env_at(&self, prospective: &Prospective, now: Timestamp) -> RuntimeEnv<'a> {
        self.env_at_full(prospective, now, BTreeMap::new(), BTreeMap::new())
    }

    /// [`Self::env_at`] carrying local `bindings` (`pool`/`spend`, §15.2) and
    /// `structurals` (`$until`/`$from`/`$quantity` for a pool `$order` key).
    pub(crate) fn env_at_full(
        &self,
        prospective: &Prospective,
        now: Timestamp,
        bindings: BTreeMap<String, Cell>,
        structurals: BTreeMap<String, Cell>,
    ) -> RuntimeEnv<'a> {
        let keep = |name: &str, fields: &FieldMap| self.active_at(name, fields, now);
        let interval = |name: &str, fields: &FieldMap| self.interval_at(name, fields, now);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let root = materialize::materialize_root_filtered(self.schema, prospective.working(), &temporal);
        // §15.1: a meter source reads a bucketed pool in the temporal context of the
        // spend, so the derived source-bucket collections are materialized (and
        // active-filtered) at this evaluation instant `now`, not the request clock.
        let root = self.expose_source_buckets(root, now);
        let index = self.temporal_index_at(prospective, now);
        RuntimeEnv::new(
            root,
            self.params.clone(),
            bindings,
            self.with_context(structurals),
            now,
            self.seed,
            // A spend-time meter accessor evaluation produces no generated key.
            Generation::ROOT,
            index,
            self.source_horizon(prospective, now),
            self.keyrings.to_vec(),
            self.placements.clone(),
            self.hosts,
        )
    }

    /// The full extant rows of every STORED bucketed collection (§14.2) at `now`.
    /// Source-backed buckets are excluded: their extant set is regenerated on demand
    /// from a [`SourceBucketHorizon`] at a horizon the temporal selector's own bound
    /// drives (§14.5), so a future `.$at`/`.$between` still generates the periods
    /// covering it rather than stopping at `now`.
    fn temporal_index_at(&self, prospective: &Prospective, now: Timestamp) -> Vec<NamedExtant> {
        let keep = |name: &str, fields: &FieldMap| self.active_at(name, fields, now);
        let interval = |name: &str, fields: &FieldMap| self.interval_at(name, fields, now);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let mut index = Vec::new();
        for member in &self.schema.model().root().members {
            if let Node::Collection(collection) = &member.node {
                let name = member.name.as_str();
                if self.compiled.bucket(name).is_some() {
                    // §7.1: tag the extant with its collection name so a temporal
                    // selector addressing this bucket resolves against it by name.
                    index.push(NamedExtant {
                        name: name.to_owned(),
                        rows: materialize::extant_bucketed_rows(self.schema, collection, name, prospective.working(), &temporal),
                    });
                }
            }
        }
        index
    }

    /// The regenerable extant set of every source-backed bucket (§14.4–§14.6) at
    /// clock `now`, which a temporal selector re-materializes at a horizon its own
    /// bound drives (§14.5). `None` when the package declares no source-backed
    /// bucket, so an ordinary package carries no horizon and pays nothing.
    fn source_horizon(
        &self,
        prospective: &Prospective,
        now: Timestamp,
    ) -> Option<crate::source_bucket::SourceBucketHorizon<'a>> {
        if self.compiled.source_buckets.is_empty() {
            return None;
        }
        let base = self.expose_keyrings(self.base_root(prospective));
        let inputs = self.bucket_inputs(&base, now);
        crate::source_bucket::SourceBucketHorizon::capture(&self.compiled.source_buckets, &inputs)
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

    /// Evaluate `typed` with an explicit lexical current chain (§6.2): `currents`
    /// runs outermost-first, so its last element is `.` and earlier elements are
    /// reachable as `^`, `^^`, …. This is the admission-time counterpart of the
    /// read path's [`TypedExpr::evaluate_scoped`], used to enforce a static-struct
    /// `$check` whose `^` reads the containing row (§5.3/§8.8).
    pub(crate) fn eval_scoped(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        currents: &[Cell],
    ) -> Result<Cell, Rejection> {
        let env = self.env(prospective);
        typed.evaluate_scoped(&env, currents).map_err(Rejection::from)
    }

    /// Evaluate one admitted row's insertion default at `generation` (SPEC-ISSUES
    /// item 4, §5.1/§8.12): a `uuid()` call site yields a distinct value per row
    /// because each row draws a fresh generation, while a state-derived default
    /// still reads the same pre-statement state. The default-resolution analogue
    /// of [`Self::eval`], reached from [`crate::rules::apply_defaults`].
    pub(crate) fn eval_generative(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        current: &Cell,
        generation: Generation,
    ) -> Result<Cell, Rejection> {
        let env = self.env_generative(prospective, generation);
        typed.evaluate(&env, current).map_err(Rejection::from)
    }

    /// Evaluate `typed` with `current` as `.` and `bindings` in scope — the
    /// lexical locals a mutation program's `name = …` statements introduced.
    pub(crate) fn eval_with(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        current: &Cell,
        bindings: BTreeMap<String, Cell>,
    ) -> Result<Cell, Rejection> {
        let env = self.env_with(prospective, bindings, BTreeMap::new());
        typed.evaluate(&env, current).map_err(Rejection::from)
    }

    /// [`Self::eval_with`] in view context (§12.2): a keyed selection is delivered
    /// as the 0/1-row view it denotes rather than coerced to a single row, so a
    /// no-state-change `return .coll[@k] { … }` query yields a collection (§6.3).
    pub(crate) fn eval_view_with(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        current: &Cell,
        bindings: BTreeMap<String, Cell>,
    ) -> Result<Cell, Rejection> {
        let env = self.env_with(prospective, bindings, BTreeMap::new());
        typed.evaluate_view(&env, current).map_err(Rejection::from)
    }

    /// A fully materialized row cell for the row at `address` in the collection
    /// at declaration path `decl_path` — the read-facing form a `return`, a local
    /// `name = …` row binding, or a row-receiver `.` observes (§8.1, §8.10). Unlike
    /// [`Self::row_cell_of`] this includes nested collections and static structs
    /// (§5.3, §5.4) and folds computed values. `None` when no row lives there.
    pub(crate) fn materialize_row_cell(
        &self,
        prospective: &Prospective,
        decl_path: &[String],
        address: &liasse_store::RowAddress,
    ) -> Option<Cell> {
        let collection = self.schema.collection_at_path(decl_path)?;
        let keep = |name: &str, fields: &FieldMap| self.active(name, fields);
        let interval = |name: &str, fields: &FieldMap| self.interval(name, fields);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let row = materialize::materialize_row(self.schema, collection, address, prospective.working(), &temporal)?;
        let compiled = self.compiled.collection_at(decl_path);
        let row = match compiled {
            Some(compiled) if !compiled.computed.is_empty() => {
                let env = self.env(prospective);
                fold_computed(&env, &compiled.computed, row)
            }
            _ => row,
        };
        // §15.6: a `$consumes` spend row exposes `funding`; a meter-declaring row
        // exposes its `.<meter>` accessor — folded here for a `return`/receiver
        // read of that single row.
        let row = crate::meter::accessor::augment_row(self, prospective, decl_path, address, row);
        Some(Cell::Row(Box::new(row)))
    }

    /// A logical row cell for one collection row, with its computed values (§5.2)
    /// folded in — the read-facing form a `return`, a local `name = …` row
    /// binding, or a row-receiver `.` observes. A collection with no computed
    /// values reuses the bare [`row_cell`].
    pub(crate) fn row_cell_of(
        &self,
        prospective: &Prospective,
        collection: &CompiledCollection,
        fields: &FieldMap,
    ) -> Cell {
        let base = row_cell(collection, fields);
        if collection.computed.is_empty() {
            return base;
        }
        let Cell::Row(row) = base else { return base };
        let env = self.env(prospective);
        Cell::Row(Box::new(fold_computed(&env, &collection.computed, *row)))
    }

    /// Evaluate `typed` with `current` as `.`, `bindings` as locals, and
    /// `structurals` as context bindings (`$target`, …) — the `$on_delete` patch
    /// evaluation path (§21.1).
    pub(crate) fn eval_full(
        &self,
        prospective: &Prospective,
        typed: &TypedExpr,
        current: &Cell,
        bindings: BTreeMap<String, Cell>,
        structurals: BTreeMap<String, Cell>,
    ) -> Result<Cell, Rejection> {
        let env = self.env_with(prospective, bindings, structurals);
        typed.evaluate(&env, current).map_err(Rejection::from)
    }
}

/// Rebuild `row` with an extra (or replaced) `name` cell carrying `cell` — the
/// step that folds an evaluated view into the package-root row.
pub(crate) fn with_cell(row: Row, name: &str, cell: Cell) -> Row {
    let cells = row
        .cells()
        .filter(|(existing, _)| existing.as_str() != name)
        .map(|(existing, existing_cell)| (existing.clone(), existing_cell.clone()))
        .chain(std::iter::once((name.to_owned(), cell)));
    Row::new(row.id().clone(), row.key().clone(), cells)
}

/// Fold a row's computed values (§5.2) into it as cells, evaluated against
/// `env` with the row itself as `.`. See [`fold_computed_scoped`]; a bare row has
/// no lexical parent, so `^` is out of reach.
fn fold_computed(env: &RuntimeEnv<'_>, computed: &[CompiledComputed], row: Row) -> Row {
    fold_computed_scoped(env, computed, &[], row)
}

/// Fold a row's computed values (§5.2) into it as cells, evaluated against `env`
/// with the row itself as `.` and `parents` (outermost first) reachable as `^`,
/// `^^`, … (§6.2) — the lexical chain a struct-nested computed value reads its
/// containing row through. A computed value that faults or yields a non-scalar is
/// left as an absent (`none`) cell — §5.2 makes a computed value yielding `none`
/// an absent optional. Iterated to a fixed point (bounded by the number of
/// computed values, since the model forbids cyclic dependencies) so one computed
/// value may read another regardless of declaration order.
fn fold_computed_scoped(
    env: &RuntimeEnv<'_>,
    computed: &[CompiledComputed],
    parents: &[Cell],
    mut row: Row,
) -> Row {
    if computed.is_empty() {
        return row;
    }
    for _ in 0..computed.len() {
        let mut currents = parents.to_vec();
        currents.push(Cell::Row(Box::new(row.clone())));
        let mut cells: Vec<(String, Cell)> =
            row.cells().map(|(name, cell)| (name.clone(), cell.clone())).collect();
        let mut changed = false;
        for comp in computed {
            let value = match comp.expr.evaluate_scoped(env, &currents) {
                Ok(Cell::Scalar(value)) => value,
                // A non-scalar computed result (a row/collection) is a documented
                // CORE seam; leave it absent rather than guess a projection.
                Ok(_) => continue,
                Err(_) => Value::None,
            };
            let next = Cell::Scalar(value);
            match cells.iter_mut().find(|(name, _)| name == &comp.name) {
                Some(slot) if slot.1 == next => {}
                Some(slot) => {
                    slot.1 = next;
                    changed = true;
                }
                None => {
                    cells.push((comp.name.clone(), next));
                    changed = true;
                }
            }
        }
        row = Row::new(row.id().clone(), row.key().clone(), cells);
        if !changed {
            break;
        }
    }
    row
}

/// Fold a static struct's computed values (§5.2) onto its materialized
/// struct-row, then recurse into each nested static struct (§5.3), passing the
/// folded row as the child's lexical parent so a deeper computed value reaches an
/// ancestor field through `^`, `^^`, … (§6.2). `parents` is the chain (outermost
/// first) already above this struct; the whole walk is bounded by the compiled
/// struct tree, which is finite, so it cannot recurse forever.
fn fold_struct_computed(
    env: &RuntimeEnv<'_>,
    meta: &CompiledStruct,
    parents: &[Cell],
    row: Row,
) -> Row {
    let row = fold_computed_scoped(env, &meta.computed, parents, row);
    if meta.structs.is_empty() {
        return row;
    }
    let mut child_parents = parents.to_vec();
    child_parents.push(Cell::Row(Box::new(row.clone())));
    let cells: Vec<(String, Cell)> = row
        .cells()
        .map(|(name, cell)| match (cell, meta.structs.iter().find(|nested| &nested.name == name)) {
            (Cell::Row(child), Some(nested)) => {
                let folded = fold_struct_computed(env, nested, &child_parents, (**child).clone());
                (name.clone(), Cell::Row(Box::new(folded)))
            }
            _ => (name.clone(), cell.clone()),
        })
        .collect();
    Row::new(row.id().clone(), row.key().clone(), cells)
}

/// Fold a collection row's nested `$view` members (§7.1) into it as cells,
/// evaluated against `env` with the row itself as `.`. A nested view that faults
/// (its source is not materialized for this row) is left out, so a reader faults
/// exactly as before. Views are folded in declaration order; a nested view reading
/// another nested view is a documented seam (declaration order suffices for the
/// CORE `.modules::iface` aggregation, which reads only the row's module spaces).
fn fold_views(env: &RuntimeEnv<'_>, views: &[crate::compiled::CompiledView], mut row: Row) -> Row {
    for view in views {
        let current = Cell::Row(Box::new(row.clone()));
        if let Ok(cell) = view.expr.evaluate(env, &current) {
            row = with_cell(row, &view.name, cell);
        }
    }
    row
}

/// A logical row cell over a field map, for evaluating a default, normalizer, or
/// row check whose `.` is the provisional row (§5.1, §5.10). Every declared field
/// is present (absent values read as `none`), so a check can reference a sibling.
///
/// §5.3: a static-struct member is stored as a `Value::Struct` scalar (it compiles
/// into `collection.structs`, not `collection.fields`); expose it as a cell too, so
/// a row `$check` reading `.meta.tag` descends the struct the same way the
/// materialized read path does (`build_row`, `materialize::build_row`) rather than
/// faulting on a missing field.
pub(crate) fn row_cell(collection: &CompiledCollection, fields: &FieldMap) -> Cell {
    let field_cells = collection.fields.iter().map(|field| {
        let value = fields.get(&field.name).cloned().unwrap_or(Value::None);
        (field.name.clone(), Cell::Scalar(value))
    });
    let struct_cells = collection.structs.iter().map(|structure| {
        let value = fields.get(&structure.name).cloned().unwrap_or(Value::None);
        (structure.name.clone(), Cell::Scalar(value))
    });
    let cells = field_cells.chain(struct_cells);
    let key = collection
        .key
        .iter()
        .map(|name| fields.get(name).cloned().unwrap_or(Value::None))
        .next()
        .unwrap_or(Value::None);
    Cell::Row(Box::new(Row::new(RowId::leaf(0), key, cells)))
}
