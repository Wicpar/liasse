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
use crate::compiled::{Compiled, CompiledCollection, CompiledComputed};
use crate::env::RuntimeEnv;
use crate::error::Rejection;
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
}

impl EvalCtx<'_> {
    /// The request's fixed `now()` instant (A.5) — the context-free clock a §15.6
    /// meter accessor and a spend's default `$time` read.
    pub(crate) const fn now(&self) -> Timestamp {
        self.now
    }

    /// The evaluation environment over the current prospective state (§8.12): the
    /// package root with bucketed collections filtered to the rows active at
    /// [`Self::now`] (§14.1), plus the full extant set of each bucketed collection
    /// so a temporal selector can re-derive activity over inactive rows (§14.2).
    pub(crate) fn env(&self, prospective: &Prospective) -> RuntimeEnv {
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
    ) -> RuntimeEnv {
        RuntimeEnv::new(
            self.root(prospective),
            self.params.clone(),
            bindings,
            structurals,
            self.now,
            self.seed,
            self.temporal_index(prospective),
        )
    }

    /// The temporal-aware package-root row: bucketed collections expose only the
    /// rows active at [`Self::now`] (each carrying its `$from`/`$until` interval
    /// cells); every other collection is materialized in full (§8.2, §14.2). Named
    /// view members are then materialized as cells too, so an expression may read
    /// one view through another (`.other_view`, §7.1) — the public-surface `$view`
    /// forms lifted to `.view_name` resolve here.
    pub(crate) fn root(&self, prospective: &Prospective) -> Row {
        let base = self.base_root(prospective);
        let base = self.expose_computed(prospective, base);
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
        let env = RuntimeEnv::new(
            base.clone(),
            self.params.clone(),
            BTreeMap::new(),
            BTreeMap::new(),
            self.now,
            self.seed,
            temporal,
        );
        fold_computed(&env, &self.compiled.root_computed, base)
    }

    /// Fold each collection row's computed values (§5.2) into the row as cells,
    /// so a view, projection, or `return` reads a computed value like a stored
    /// field. Evaluated against the base root (writable state); a computed value
    /// reading `.field` sees the row, a computed value reading another computed
    /// value in the same row is resolved by the per-row fixed point. Views are
    /// exposed afterwards, so a view may read a computed value.
    fn expose_computed(&self, prospective: &Prospective, base: Row) -> Row {
        if self.compiled.collections.iter().all(|c| c.computed.is_empty()) {
            return base;
        }
        let temporal = self.temporal_index(prospective);
        let env = RuntimeEnv::new(
            base.clone(),
            self.params.clone(),
            BTreeMap::new(),
            BTreeMap::new(),
            self.now,
            self.seed,
            temporal,
        );
        let cells: Vec<(String, Cell)> = base
            .cells()
            .map(|(name, cell)| match (cell, self.compiled.collection(name)) {
                (Cell::Collection(rows), Some(collection)) if !collection.computed.is_empty() => {
                    let folded = rows
                        .iter()
                        .map(|row| fold_computed(&env, &collection.computed, row.clone()))
                        .collect();
                    (name.clone(), Cell::Collection(folded))
                }
                _ => (name.clone(), cell.clone()),
            })
            .collect();
        Row::new(base.id().clone(), base.key().clone(), cells)
    }

    fn base_root(&self, prospective: &Prospective) -> Row {
        let keep = |name: &str, fields: &FieldMap| self.active(name, fields);
        let interval = |name: &str, fields: &FieldMap| self.interval(name, fields);
        let temporal = Temporal { keep: &keep, interval: &interval };
        materialize::materialize_root_filtered(self.schema, prospective.working(), &temporal)
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
        let mut pending: Vec<&crate::compiled::CompiledView> = self.compiled.views.iter().collect();
        loop {
            let mut progressed = false;
            let mut still = Vec::new();
            for view in pending {
                let env = RuntimeEnv::new(
                    root.clone(),
                    self.params.clone(),
                    BTreeMap::new(),
                    BTreeMap::new(),
                    self.now,
                    self.seed,
                    temporal.clone(),
                );
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
    fn temporal_index(&self, prospective: &Prospective) -> Vec<Vec<Row>> {
        let keep = |name: &str, fields: &FieldMap| self.active(name, fields);
        let interval = |name: &str, fields: &FieldMap| self.interval(name, fields);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let mut index = Vec::new();
        for member in &self.schema.model().root().members {
            if let Node::Collection(collection) = &member.node {
                let name = member.name.as_str();
                if self.compiled.bucket(name).is_some() {
                    index.push(materialize::extant_bucketed_rows(
                        collection,
                        name,
                        prospective.working(),
                        &temporal,
                    ));
                }
            }
        }
        index
    }

    /// Whether collection `name`'s row `fields` is currently readable: always for
    /// a non-bucketed collection, else its bucket activity at [`Self::now`].
    fn active(&self, name: &str, fields: &FieldMap) -> bool {
        self.active_at(name, fields, self.now)
    }

    /// [`Self::active`] at an explicit instant — the meter pool resolution reads a
    /// bucketed source in the temporal context of the spend (§15.1).
    pub(crate) fn active_at(&self, name: &str, fields: &FieldMap, now: Timestamp) -> bool {
        match (self.compiled.bucket(name), self.compiled.collection(name)) {
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
        match (self.compiled.bucket(name), self.compiled.collection(name)) {
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
        let row = materialize::materialize_row(collection, address, prospective.working(), &temporal)?;
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
    pub(crate) fn env_at(&self, prospective: &Prospective, now: Timestamp) -> RuntimeEnv {
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
    ) -> RuntimeEnv {
        let keep = |name: &str, fields: &FieldMap| self.active_at(name, fields, now);
        let interval = |name: &str, fields: &FieldMap| self.interval_at(name, fields, now);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let root = materialize::materialize_root_filtered(self.schema, prospective.working(), &temporal);
        let index = self.temporal_index_at(prospective, now);
        RuntimeEnv::new(root, self.params.clone(), bindings, structurals, now, self.seed, index)
    }

    fn temporal_index_at(&self, prospective: &Prospective, now: Timestamp) -> Vec<Vec<Row>> {
        let keep = |name: &str, fields: &FieldMap| self.active_at(name, fields, now);
        let interval = |name: &str, fields: &FieldMap| self.interval_at(name, fields, now);
        let temporal = Temporal { keep: &keep, interval: &interval };
        let mut index = Vec::new();
        for member in &self.schema.model().root().members {
            if let Node::Collection(collection) = &member.node {
                let name = member.name.as_str();
                if self.compiled.bucket(name).is_some() {
                    index.push(materialize::extant_bucketed_rows(collection, name, prospective.working(), &temporal));
                }
            }
        }
        index
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
        let row = materialize::materialize_row(collection, address, prospective.working(), &temporal)?;
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
fn with_cell(row: Row, name: &str, cell: Cell) -> Row {
    let cells = row
        .cells()
        .filter(|(existing, _)| existing.as_str() != name)
        .map(|(existing, existing_cell)| (existing.clone(), existing_cell.clone()))
        .chain(std::iter::once((name.to_owned(), cell)));
    Row::new(row.id().clone(), row.key().clone(), cells)
}

/// Fold a row's computed values (§5.2) into it as cells, evaluated against
/// `env` with the row itself as `.`. A computed value that faults or yields a
/// non-scalar is left as an absent (`none`) cell — §5.2 makes a computed value
/// yielding `none` an absent optional. Iterated to a fixed point (bounded by the
/// number of computed values, since the model forbids cyclic dependencies) so
/// one computed value may read another regardless of declaration order.
fn fold_computed(env: &RuntimeEnv, computed: &[CompiledComputed], mut row: Row) -> Row {
    if computed.is_empty() {
        return row;
    }
    for _ in 0..computed.len() {
        let current = Cell::Row(Box::new(row.clone()));
        let mut cells: Vec<(String, Cell)> =
            row.cells().map(|(name, cell)| (name.clone(), cell.clone())).collect();
        let mut changed = false;
        for comp in computed {
            let value = match comp.expr.evaluate(env, &current) {
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
