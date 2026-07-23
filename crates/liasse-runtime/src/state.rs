//! Prospective state: the staged, read-your-writes working copy an admission
//! evaluates against (§8.1, §22.2).
//!
//! A [`Prospective`] is gathered from committed state at the head, mutated by a
//! program's statements, subjected to the rule pipeline, and finally diffed
//! against the committed base to produce the exact row operations the store
//! admits atomically. Nothing here touches the durable store; a rejected
//! admission simply drops the `Prospective`, leaving committed state intact.

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::RowId;
use liasse_ident::{NameSegment, RowIncarnation};
use liasse_model::Collection;
use liasse_store::{CollectionPath, InstanceStore, RowAddress, Snapshot, StoreError};
use liasse_value::Value;

use crate::generator::Generation;
use crate::materialize::{self, FieldMap};
use crate::schema::Schema;

/// Scans a collection path for its direct committed rows: the primitive a
/// [`Prospective`] gathers each top-level collection's rows from, backed by either
/// the live store or a frontier [`Snapshot`]. Each row carries its durable
/// [`RowIncarnation`] (D.1) so meter-pool funding can bind to a pool's incarnation,
/// not merely its reused application key (§15.2, Annex D.1).
type Scan<'a> =
    dyn Fn(&CollectionPath) -> Result<Vec<(RowAddress, RowIncarnation, Value)>, StoreError> + 'a;

/// Gathers the whole committed subtree under one row, reached through the given
/// declared nested-collection step names, in Annex B address order — the
/// shape-directed `scan_subtree` (§7.6) that replaces the former per-row,
/// per-nested-collection scan storm. Backed by either the live store or a frontier
/// [`Snapshot`], each of which serves it index-first / in one range.
type SubtreeScan<'a> =
    dyn Fn(&RowAddress, &[String]) -> Result<Vec<(RowAddress, RowIncarnation, Value)>, StoreError> + 'a;

/// One resolved change between the committed base and the admitted working
/// state, ready to stage into a store transition.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    /// A fresh row: the store allocates its incarnation on admission.
    Insert(RowAddress, Value),
    /// A new payload for an existing row, preserving its incarnation.
    Update(RowAddress, Value),
    /// Removal of a live row.
    Delete(RowAddress),
}

/// The staged working copy of state for one admission.
#[derive(Debug, Clone)]
pub(crate) struct Prospective {
    committed: BTreeMap<RowAddress, Value>,
    working: BTreeMap<RowAddress, FieldMap>,
    /// The durable [`RowIncarnation`] (D.1) of each COMMITTED row, keyed by address.
    /// A freshly-staged working row (not yet admitted) has no entry — the store
    /// allocates its incarnation at admission. Meter-pool funding reads this so a
    /// deleted-then-reinserted pool at a reused application key is a DISTINCT funding
    /// target from the deleted incarnation (§15.2, §5.6, Annex D.1), instead of
    /// conflating the two occurrences by their shared key.
    incarnations: BTreeMap<RowAddress, RowIncarnation>,
    /// A monotonic per-admission ordinal handed to each row's default resolution
    /// so a `uuid()` field default shared across the several rows of one request
    /// yields a distinct value per row (SPEC-ISSUES item 4, §5.1/§8.12). It is
    /// admission bookkeeping, not committed state — [`Self::diff`] ignores it and
    /// it is never staged. Every nested internal call (§8.11) threads the same
    /// `&mut Prospective`, so two rows anywhere in one request draw distinct
    /// ordinals even across a parent and its callee.
    next_generation: u64,
}

impl Prospective {
    /// Gather committed rows of every collection — top-level and nested (§5.4) —
    /// into a working copy.
    pub(crate) fn gather<S: InstanceStore>(
        store: &S,
        schema: Schema<'_>,
    ) -> Result<Self, StoreError> {
        Self::gather_from(
            &|path| {
                Ok(store
                    .scan(path)?
                    .into_iter()
                    .map(|(a, r)| (a, r.incarnation().clone(), r.value().clone()))
                    .collect())
            },
            &|root, steps| {
                Ok(store
                    .scan_subtree(root, steps)?
                    .into_iter()
                    .map(|(a, r)| (a, r.incarnation().clone(), r.value().clone()))
                    .collect())
            },
            schema,
        )
    }

    /// Gather the committed rows visible at a frontier snapshot into a
    /// read-only working copy (used for view evaluation at a frontier).
    pub(crate) fn from_snapshot(snapshot: &Snapshot, schema: Schema<'_>) -> Self {
        // A snapshot scan is infallible, so this cannot error.
        Self::gather_from(
            &|path| {
                Ok(snapshot
                    .scan(path)
                    .into_iter()
                    .map(|(a, r)| (a, r.incarnation().clone(), r.value().clone()))
                    .collect())
            },
            &|root, steps| {
                Ok(snapshot
                    .scan_subtree(root, steps)
                    .into_iter()
                    .map(|(a, r)| (a, r.incarnation().clone(), r.value().clone()))
                    .collect())
            },
            schema,
        )
        .unwrap_or_else(|_| Self::empty())
    }

    fn gather_from(
        scan: &Scan<'_>,
        subtree: &SubtreeScan<'_>,
        schema: Schema<'_>,
    ) -> Result<Self, StoreError> {
        let mut committed = BTreeMap::new();
        let mut working = BTreeMap::new();
        let mut incarnations = BTreeMap::new();
        for member in &schema.model().root().members {
            // §5.8: a top-level member naming a keyed shape (`companies: "company"`)
            // resolves to that collection, so its stored rows are gathered like any
            // collection's. `resolved_collection` is the identity for a real collection.
            if let Some(collection) = schema.resolved_collection(&member.node) {
                let path = CollectionPath::top(NameSegment::new(member.name.as_str()));
                gather_tree(
                    scan,
                    subtree,
                    schema,
                    collection,
                    &path,
                    &mut committed,
                    &mut working,
                    &mut incarnations,
                )?;
            }
        }
        // §8.2: the package root's singleton fields live in one reserved row.
        for (address, incarnation, value) in scan(&crate::singleton::path())? {
            working.insert(address.clone(), materialize::fields_of(&value));
            incarnations.insert(address.clone(), incarnation);
            committed.insert(address, value);
        }
        Ok(Self { committed, working, incarnations, next_generation: 0 })
    }

    /// An empty prospective state (genesis, before any seed).
    pub(crate) fn empty() -> Self {
        Self {
            committed: BTreeMap::new(),
            working: BTreeMap::new(),
            incarnations: BTreeMap::new(),
            next_generation: 0,
        }
    }

    /// Take the next generated-value generation for one row's default resolution
    /// (SPEC-ISSUES item 4, §5.1/§8.12), advancing the admission's monotonic
    /// ordinal. Each admitted row occurrence draws its own generation, so a
    /// `uuid()` field default is fresh per row while a state-derived default
    /// (`count(/coll) + 1`) still reads the same pre-statement state (§5.1).
    pub(crate) fn next_generation(&mut self) -> Generation {
        let generation = Generation::new(self.next_generation);
        self.next_generation += 1;
        generation
    }

    /// The current working rows, keyed by address, for temporal-aware root
    /// materialization through [`crate::eval::EvalCtx::root`].
    pub(crate) fn working(&self) -> &BTreeMap<RowAddress, FieldMap> {
        &self.working
    }

    /// The durable incarnation (D.1) of every committed row, indexed by the stable
    /// [`RowId`] a materialized row carries — the identity a resolved meter pool is
    /// keyed by (`materialize::row_id_of`, the same identity `accessor::expose`
    /// grafts by). Meter-pool funding binds to this so a deleted-then-reinserted
    /// pool at a reused key is a fresh funding target (§15.2, Annex D.1).
    ///
    /// A [`RowId`] shared by two committed addresses with different incarnations is
    /// dropped as ambiguous: the pool then falls back to key-only matching (the
    /// pre-incarnation behaviour), applied identically at freeze and read so the
    /// two stay consistent. In practice a well-formed model gives each live row a
    /// distinct `RowId` (the invariant `accessor::expose`'s graft already assumes).
    pub(crate) fn incarnation_index(&self) -> BTreeMap<RowId, RowIncarnation> {
        let mut index: BTreeMap<RowId, RowIncarnation> = BTreeMap::new();
        let mut ambiguous: BTreeSet<RowId> = BTreeSet::new();
        for (address, incarnation) in &self.incarnations {
            let Some(id) = materialize::row_id_of(address) else { continue };
            match index.get(&id) {
                Some(existing) if existing == incarnation => {}
                Some(_) => {
                    ambiguous.insert(id);
                }
                None => {
                    index.insert(id, incarnation.clone());
                }
            }
        }
        for id in ambiguous {
            index.remove(&id);
        }
        index
    }

    /// Whether a live row occupies `address`.
    pub(crate) fn contains(&self, address: &RowAddress) -> bool {
        self.working.contains_key(address)
    }

    /// The working fields of the row at `address`, if live.
    pub(crate) fn get(&self, address: &RowAddress) -> Option<&FieldMap> {
        self.working.get(address)
    }

    /// Stage a fresh row's fields at `address`.
    pub(crate) fn insert(&mut self, address: RowAddress, fields: FieldMap) {
        self.working.insert(address, fields);
    }

    /// Replace the fields of the row at `address` (must be live).
    pub(crate) fn replace(&mut self, address: &RowAddress, fields: FieldMap) {
        self.working.insert(address.clone(), fields);
    }

    /// Remove the row at `address`.
    pub(crate) fn remove(&mut self, address: &RowAddress) {
        self.working.remove(address);
    }

    /// The addresses of live rows in a collection, in Annex B order (B.5).
    pub(crate) fn addresses_in(&self, path: &CollectionPath) -> Vec<RowAddress> {
        self.working
            .keys()
            .filter(|address| path.contains(address))
            .cloned()
            .collect()
    }

    /// Diff the working state against the committed base into the ordered set of
    /// row changes to admit (§22.2). Deterministic: addresses are visited in
    /// Annex B order, so replay of the resulting log is faithful.
    pub(crate) fn diff(&self) -> Vec<Change> {
        let mut changes = Vec::new();
        for (address, fields) in &self.working {
            let value = materialize::struct_of(fields);
            match self.committed.get(address) {
                Some(prior) if *prior == value => {}
                Some(_) => changes.push(Change::Update(address.clone(), value)),
                None => changes.push(Change::Insert(address.clone(), value)),
            }
        }
        for address in self.committed.keys() {
            if !self.working.contains_key(address) {
                changes.push(Change::Delete(address.clone()));
            }
        }
        changes
    }
}

/// Gather every direct row of the collection at `path`, then the whole subtree of
/// committed rows under each in ONE shape-directed `scan_subtree` per row (§7.6),
/// so the working copy ends up identical to the former per-nested-collection scan
/// recursion while touching the store far fewer times. `steps` is the set of
/// nested keyed-collection names declared anywhere in this collection's shape —
/// exactly the collections that recursion descended — so every descendant row of a
/// well-formed store is reached (a stored child always sits under a declared nested
/// collection). A leaf shape has no nested collections, so `scan_subtree`
/// short-circuits without a store round trip.
#[allow(clippy::too_many_arguments)]
fn gather_tree<'m>(
    scan: &Scan<'_>,
    subtree: &SubtreeScan<'_>,
    schema: Schema<'m>,
    collection: &'m Collection,
    path: &CollectionPath,
    committed: &mut BTreeMap<RowAddress, Value>,
    working: &mut BTreeMap<RowAddress, FieldMap>,
    incarnations: &mut BTreeMap<RowAddress, RowIncarnation>,
) -> Result<(), StoreError> {
    let steps = nested_step_names(schema, collection);
    for (address, incarnation, value) in scan(path)? {
        working.insert(address.clone(), materialize::fields_of(&value));
        incarnations.insert(address.clone(), incarnation);
        if !steps.is_empty() {
            for (nested_address, nested_incarnation, nested_value) in subtree(&address, &steps)? {
                working.insert(nested_address.clone(), materialize::fields_of(&nested_value));
                incarnations.insert(nested_address.clone(), nested_incarnation);
                committed.insert(nested_address, nested_value);
            }
        }
        committed.insert(address, value);
    }
    Ok(())
}

/// The declared nested keyed-collection names reachable anywhere in `collection`'s
/// shape — the §7.6 step universe `scan_subtree` descends. It is the transitive
/// closure of `resolved_collection` over the shape graph, exactly the collections
/// the former [`gather_tree`] recursion visited level by level. A self-referential
/// shape (`subcompanies: "company"`) resolves its recursive member back to the same
/// shared collection node, so a visited-set over resolved collection identities
/// terminates the closure (the guard against a cyclic shape with shallow data).
fn nested_step_names<'m>(schema: Schema<'m>, collection: &'m Collection) -> Vec<String> {
    let mut steps: BTreeSet<String> = BTreeSet::new();
    let mut visited: Vec<*const Collection> = Vec::new();
    let mut stack: Vec<&'m Collection> = vec![collection];
    while let Some(current) = stack.pop() {
        let identity = std::ptr::from_ref::<Collection>(current);
        if visited.contains(&identity) {
            continue;
        }
        visited.push(identity);
        for member in &current.shape.members {
            // §5.4/§5.8: a nested keyed collection, whether declared directly or
            // adopted through a `$types`/`$like` name, contributes its step name and
            // its own shape's nested collections to the descent universe.
            if let Some(nested) = schema.resolved_collection(&member.node) {
                steps.insert(member.name.as_str().to_owned());
                stack.push(nested);
            }
        }
    }
    steps.into_iter().collect()
}
