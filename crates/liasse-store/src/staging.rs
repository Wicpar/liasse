//! The in-memory staged transition (§22.2 atomic admission).
//!
//! A [`MemoryTransition`] borrows its store exclusively and buffers writes as an
//! address-keyed overlay (for read-your-writes and occupancy checks) alongside
//! the ordered op list that becomes the durable record. Nothing reaches durable
//! state until [`MemoryTransition::commit`]; dropping discards the overlay, so
//! an aborted transition leaves no trace.

use std::collections::BTreeMap;

use liasse_ident::{RowIncarnation, TransactionId};
use liasse_value::Value;

use crate::commit::{CommitOutcome, CommittedRowOp};
use crate::contract::Transition;
use crate::error::StoreError;
use crate::key::{CollectionPath, RowAddress};
use crate::memory::MemoryStore;
use crate::meta::{Composition, DefinitionText};
use crate::row::StoredRow;

/// A staged transition over a [`MemoryStore`].
#[derive(Debug)]
pub struct MemoryTransition<'s> {
    store: &'s mut MemoryStore,
    /// Per-address staged change: `Some(row)` is a put, `None` is a delete.
    overlay: BTreeMap<RowAddress, Option<StoredRow>>,
    ops: Vec<CommittedRowOp>,
    definition: Option<DefinitionText>,
    composition: Option<Composition>,
    transaction: Option<TransactionId>,
}

impl<'s> MemoryTransition<'s> {
    pub(crate) fn new(store: &'s mut MemoryStore) -> Self {
        Self {
            store,
            overlay: BTreeMap::new(),
            ops: Vec::new(),
            definition: None,
            composition: None,
            transaction: None,
        }
    }

    /// The effective row at `address` under committed-plus-staged state.
    fn resolve(&self, address: &RowAddress) -> Option<StoredRow> {
        match self.overlay.get(address) {
            Some(staged) => staged.clone(),
            None => self.store.resolve_current(address).cloned(),
        }
    }
}

impl Transition for MemoryTransition<'_> {
    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError> {
        Ok(self.resolve(address))
    }

    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        let mut rows: BTreeMap<RowAddress, StoredRow> = self
            .store
            .current_rows()
            .iter()
            .filter(|(address, _)| collection.contains(address))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect();
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
        if self.resolve(&address).is_some() {
            return Err(StoreError::Conflict {
                address: address.render(),
                context: "insert",
            });
        }
        let incarnation = self.store.alloc_incarnation();
        self.overlay.insert(
            address.clone(),
            Some(StoredRow::new(incarnation.clone(), value.clone())),
        );
        self.ops.push(CommittedRowOp::Insert {
            address,
            incarnation: incarnation.clone(),
            value,
        });
        Ok(incarnation)
    }

    fn update(&mut self, address: &RowAddress, value: Value) -> Result<(), StoreError> {
        let incarnation = match self.resolve(address) {
            Some(row) => row.incarnation().clone(),
            None => {
                return Err(StoreError::NotFound {
                    address: address.render(),
                    context: "update",
                });
            }
        };
        self.overlay.insert(
            address.clone(),
            Some(StoredRow::new(incarnation.clone(), value.clone())),
        );
        self.ops.push(CommittedRowOp::Update {
            address: address.clone(),
            incarnation,
            value,
        });
        Ok(())
    }

    fn delete(&mut self, address: &RowAddress) -> Result<(), StoreError> {
        let incarnation = match self.resolve(address) {
            Some(row) => row.incarnation().clone(),
            None => {
                return Err(StoreError::NotFound {
                    address: address.render(),
                    context: "delete",
                });
            }
        };
        self.overlay.insert(address.clone(), None);
        self.ops.push(CommittedRowOp::Delete {
            address: address.clone(),
            incarnation,
        });
        Ok(())
    }

    fn rekey(
        &mut self,
        from: &RowAddress,
        to: RowAddress,
        value: Value,
    ) -> Result<(), StoreError> {
        let incarnation = match self.resolve(from) {
            Some(row) => row.incarnation().clone(),
            None => {
                return Err(StoreError::NotFound {
                    address: from.render(),
                    context: "rekey source",
                });
            }
        };
        if self.resolve(&to).is_some() {
            return Err(StoreError::Conflict {
                address: to.render(),
                context: "rekey target",
            });
        }
        self.overlay.insert(from.clone(), None);
        self.overlay.insert(
            to.clone(),
            Some(StoredRow::new(incarnation.clone(), value.clone())),
        );
        self.ops.push(CommittedRowOp::Rekey {
            from: from.clone(),
            to,
            incarnation,
            value,
        });
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
