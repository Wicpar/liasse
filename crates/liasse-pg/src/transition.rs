//! [`PgTransition`]: a staged transition over a [`PgStore`].
//!
//! Staging is identical in shape to the in-memory reference: an address-keyed
//! overlay gives read-your-writes and the occupancy checks the contract mandates,
//! while an ordered op list becomes the durable record. Nothing touches
//! PostgreSQL until [`PgTransition::commit`], which hands the whole staged set to
//! [`PgStore`] to admit in one SQL transaction; dropping discards the overlay, so
//! an aborted transition leaves no trace (§22.2).

use std::collections::BTreeMap;

use liasse_ident::{RowIncarnation, TransactionId};
use liasse_store::{
    CollectionPath, CommitOutcome, CommittedRowOp, Composition, DefinitionText, InstanceStore,
    RowAddress, StoreError, StoredRow, Transition,
};
use liasse_value::Value;

use crate::store::PgStore;

/// A staged transition borrowing its store exclusively.
#[derive(Debug)]
pub struct PgTransition<'s> {
    store: &'s mut PgStore,
    /// Per-address staged change: `Some(row)` is a put, `None` is a delete.
    overlay: BTreeMap<RowAddress, Option<StoredRow>>,
    ops: Vec<CommittedRowOp>,
    definition: Option<DefinitionText>,
    composition: Option<Composition>,
    transaction: Option<TransactionId>,
}

impl<'s> PgTransition<'s> {
    pub(crate) fn new(store: &'s mut PgStore) -> Self {
        Self {
            store,
            overlay: BTreeMap::new(),
            ops: Vec::new(),
            definition: None,
            composition: None,
            transaction: None,
        }
    }

    /// The effective row at `address` under committed-plus-staged state: a staged
    /// entry shadows the committed base; otherwise the committed base is read live
    /// from SQL via [`PgStore::row`] (the pooled §4.1 point lookup). Staging writes
    /// nothing to PostgreSQL until commit, so that base-read observes exactly the
    /// committed pre-transition state — identical to what the deleted `current` map
    /// held (Phase 3, §4.3).
    fn resolve(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError> {
        match self.overlay.get(address) {
            Some(staged) => Ok(staged.clone()),
            None => self.store.row(address),
        }
    }
}

impl Transition for PgTransition<'_> {
    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError> {
        self.resolve(address)
    }

    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        // The committed base is the pooled §4.2 ordered scan; the staged overlay then
        // shadows it (a staged put appears/overrides, a staged delete hides a base
        // row) — nothing is written to PostgreSQL until commit, so the base is the
        // committed pre-transition state.
        let mut rows: BTreeMap<RowAddress, StoredRow> =
            self.store.scan(collection)?.into_iter().collect();
        for (address, staged) in &self.overlay {
            if !collection.contains(address) {
                continue;
            }
            match staged {
                Some(row) => {
                    rows.insert(address.clone(), row.clone());
                }
                None => {
                    rows.remove(address);
                }
            }
        }
        Ok(rows.into_iter().collect())
    }

    fn insert(&mut self, address: RowAddress, value: Value) -> Result<RowIncarnation, StoreError> {
        if self.resolve(&address)?.is_some() {
            return Err(StoreError::Conflict { address: address.render(), context: "insert" });
        }
        let incarnation = self.store.alloc_incarnation()?;
        self.overlay
            .insert(address.clone(), Some(StoredRow::new(incarnation.clone(), value.clone())));
        self.ops.push(CommittedRowOp::Insert { address, incarnation: incarnation.clone(), value });
        Ok(incarnation)
    }

    fn update(&mut self, address: &RowAddress, value: Value) -> Result<(), StoreError> {
        let incarnation = match self.resolve(address)? {
            Some(row) => row.incarnation().clone(),
            None => return Err(StoreError::NotFound { address: address.render(), context: "update" }),
        };
        self.overlay
            .insert(address.clone(), Some(StoredRow::new(incarnation.clone(), value.clone())));
        self.ops.push(CommittedRowOp::Update { address: address.clone(), incarnation, value });
        Ok(())
    }

    fn delete(&mut self, address: &RowAddress) -> Result<(), StoreError> {
        let incarnation = match self.resolve(address)? {
            Some(row) => row.incarnation().clone(),
            None => return Err(StoreError::NotFound { address: address.render(), context: "delete" }),
        };
        self.overlay.insert(address.clone(), None);
        self.ops.push(CommittedRowOp::Delete { address: address.clone(), incarnation });
        Ok(())
    }

    fn rekey(&mut self, from: &RowAddress, to: RowAddress, value: Value) -> Result<(), StoreError> {
        let incarnation = match self.resolve(from)? {
            Some(row) => row.incarnation().clone(),
            None => {
                return Err(StoreError::NotFound { address: from.render(), context: "rekey source" });
            }
        };
        if self.resolve(&to)?.is_some() {
            return Err(StoreError::Conflict { address: to.render(), context: "rekey target" });
        }
        self.overlay.insert(from.clone(), None);
        self.overlay
            .insert(to.clone(), Some(StoredRow::new(incarnation.clone(), value.clone())));
        self.ops.push(CommittedRowOp::Rekey { from: from.clone(), to, incarnation, value });
        Ok(())
    }

    fn set_definition(&mut self, definition: DefinitionText) {
        self.definition = Some(definition);
    }

    fn set_composition(&mut self, composition: Composition) {
        self.composition = Some(composition);
    }

    fn set_transaction(&mut self, transaction: TransactionId) {
        self.transaction = Some(transaction);
    }

    fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.definition.is_none() && self.composition.is_none()
    }

    fn commit(self) -> Result<CommitOutcome, StoreError> {
        let Self { store, ops, definition, composition, transaction, overlay: _ } = self;
        store.commit_transition(ops, transaction, definition, composition)
    }

    fn abort(self) {
        // Dropping discards the overlay and ops; committed state is untouched.
    }
}
