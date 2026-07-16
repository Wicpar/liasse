//! Prospective state: the staged, read-your-writes working copy an admission
//! evaluates against (§8.1, §22.2).
//!
//! A [`Prospective`] is gathered from committed state at the head, mutated by a
//! program's statements, subjected to the rule pipeline, and finally diffed
//! against the committed base to produce the exact row operations the store
//! admits atomically. Nothing here touches the durable store; a rejected
//! admission simply drops the `Prospective`, leaving committed state intact.

use std::collections::BTreeMap;

use liasse_ident::NameSegment;
use liasse_model::Node;
use liasse_store::{CollectionPath, InstanceStore, RowAddress, StoreError};
use liasse_value::Value;

use crate::materialize::{self, FieldMap};
use crate::schema::Schema;

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
}

impl Prospective {
    /// Gather committed rows of every top-level collection into a working copy.
    pub(crate) fn gather<S: InstanceStore>(
        store: &S,
        schema: Schema<'_>,
    ) -> Result<Self, StoreError> {
        let mut committed = BTreeMap::new();
        let mut working = BTreeMap::new();
        for member in &schema.model().root().members {
            if let Node::Collection(_) = &member.node {
                let path = CollectionPath::top(NameSegment::new(member.name.as_str()));
                for (address, row) in store.scan(&path)? {
                    working.insert(address.clone(), materialize::fields_of(row.value()));
                    committed.insert(address, row.value().clone());
                }
            }
        }
        Ok(Self { committed, working })
    }

    /// Gather the committed rows visible at a frontier snapshot into a
    /// read-only working copy (used for view evaluation at a frontier).
    pub(crate) fn from_snapshot(snapshot: &liasse_store::Snapshot, schema: Schema<'_>) -> Self {
        let mut committed = BTreeMap::new();
        let mut working = BTreeMap::new();
        for member in &schema.model().root().members {
            if let Node::Collection(_) = &member.node {
                let path = CollectionPath::top(NameSegment::new(member.name.as_str()));
                for (address, row) in snapshot.scan(&path) {
                    working.insert(address.clone(), materialize::fields_of(row.value()));
                    committed.insert(address, row.value().clone());
                }
            }
        }
        Self { committed, working }
    }

    /// An empty prospective state (genesis, before any seed).
    pub(crate) fn empty() -> Self {
        Self { committed: BTreeMap::new(), working: BTreeMap::new() }
    }

    /// The current working rows, keyed by address, for temporal-aware root
    /// materialization through [`crate::eval::EvalCtx::root`].
    pub(crate) fn working(&self) -> &BTreeMap<RowAddress, FieldMap> {
        &self.working
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
