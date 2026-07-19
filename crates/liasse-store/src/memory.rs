//! In-memory reference implementation of the storage contract.
//!
//! This proves the contract is implementable and serves as the runtime's test
//! double. State is a `BTreeMap` keyed by [`RowAddress`], so rows are held in
//! Annex B order for free (B.5). It keeps the durable record honest by holding
//! both an incrementally maintained current map (fast reads the runtime hammers)
//! and the append-only commit log (the replay stream); a frontier snapshot folds
//! the log, so an arbitrary-frontier read and a current read are answered by
//! independent paths that the conformance suite cross-checks against a known
//! oracle.
//!
//! No interior mutability and no reference counting: the store owns its state
//! outright and a [`MemoryTransition`] borrows it exclusively (AGENTS.md).

use std::collections::BTreeMap;

use data_encoding::HEXLOWER;
use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_value::Sha512;
use sha2::{Digest as _, Sha512 as Sha512Hasher};

use crate::commit::{CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition};
use crate::contract::{InstanceStore, StoreFactory};
use crate::error::StoreError;
use crate::key::{CollectionPath, RowAddress};
use crate::meta::{Composition, DefinitionText};
use crate::row::StoredRow;
use crate::snapshot::Snapshot;
use crate::staging::MemoryTransition;

/// A `BTreeMap`-backed store for one package instance.
#[derive(Debug)]
pub struct MemoryStore {
    instance: InstanceId,
    head: CommitSeq,
    current: BTreeMap<RowAddress, StoredRow>,
    log: Vec<CommittedTransition>,
    next_incarnation: u64,
    points: BTreeMap<HistoryPoint, CommitSeq>,
    blobs: BTreeMap<Sha512, Vec<u8>>,
    definition: Option<DefinitionText>,
    composition: Option<Composition>,
}

impl MemoryStore {
    /// A fresh, empty instance store at genesis.
    #[must_use]
    pub fn new(instance: InstanceId) -> Self {
        Self {
            instance,
            head: CommitSeq::GENESIS,
            current: BTreeMap::new(),
            log: Vec::new(),
            next_incarnation: 0,
            points: BTreeMap::new(),
            blobs: BTreeMap::new(),
            definition: None,
            composition: None,
        }
    }

    /// The live current row at `address`, if any (staging overlays on top).
    pub(crate) fn resolve_current(&self, address: &RowAddress) -> Option<&StoredRow> {
        self.current.get(address)
    }

    /// The whole current map — the base a staged scan filters and overlays.
    pub(crate) fn current_rows(&self) -> &BTreeMap<RowAddress, StoredRow> {
        &self.current
    }

    /// Allocate the next opaque row incarnation (D.1). Tokens are opaque, so
    /// gaps from aborted transitions are harmless; only serial positions must be
    /// gapless.
    pub(crate) fn alloc_incarnation(&mut self) -> RowIncarnation {
        let token = format!("row-{}", self.next_incarnation);
        self.next_incarnation += 1;
        RowIncarnation::new(token)
    }

    /// Atomically admit a staged transition. Empty transitions consume no
    /// position (§22.2); otherwise the next position is taken, the ops are
    /// applied to current state, and the transition is appended to the log.
    pub(crate) fn commit_transition(
        &mut self,
        ops: Vec<CommittedRowOp>,
        transaction: Option<TransactionId>,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
    ) -> Result<CommitOutcome, StoreError> {
        if ops.is_empty() && definition.is_none() && composition.is_none() {
            return Ok(CommitOutcome::Unchanged);
        }
        let seq = self.head.next();
        for op in &ops {
            self.apply_current(op);
        }
        if let Some(definition) = definition {
            self.definition = Some(definition);
        }
        if let Some(composition) = composition {
            self.composition = Some(composition);
        }
        self.log.push(CommittedTransition::new(seq, ops, transaction));
        self.head = seq;
        Ok(CommitOutcome::Committed(seq))
    }

    /// Apply one already-validated op to the current map. Staging established
    /// occupancy, so this never needs the replay corruption checks.
    fn apply_current(&mut self, op: &CommittedRowOp) {
        match op {
            CommittedRowOp::Insert { address, incarnation, value }
            | CommittedRowOp::Update { address, incarnation, value } => {
                self.current
                    .insert(address.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
            CommittedRowOp::Delete { address, .. } => {
                self.current.remove(address);
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                self.current.remove(from);
                self.current
                    .insert(to.clone(), StoredRow::new(incarnation.clone(), value.clone()));
            }
        }
    }
}

impl InstanceStore for MemoryStore {
    type Transition<'s> = MemoryTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    fn head(&self) -> Result<CommitSeq, StoreError> {
        Ok(self.head)
    }

    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError> {
        Ok(self.current.get(address).cloned())
    }

    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        Ok(self
            .current
            .iter()
            .filter(|(address, _)| collection.contains(address))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect())
    }

    fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        if frontier > self.head {
            return Err(StoreError::Corruption {
                detail: format!(
                    "snapshot frontier {} is past head {}",
                    frontier.get(),
                    self.head.get()
                ),
            });
        }
        Snapshot::replay(&self.log, frontier)
    }

    fn log_from(&self, from: CommitSeq) -> Result<Vec<CommittedTransition>, StoreError> {
        Ok(self
            .log
            .iter()
            .filter(|transition| transition.seq() >= from)
            .cloned()
            .collect())
    }

    fn begin(&mut self) -> Self::Transition<'_> {
        MemoryTransition::new(self)
    }

    fn record_point(&mut self, at: CommitSeq, point: HistoryPoint) -> Result<(), StoreError> {
        if at > self.head {
            return Err(StoreError::Corruption {
                detail: format!("history point at {} is past head {}", at.get(), self.head.get()),
            });
        }
        self.points.insert(point, at);
        Ok(())
    }

    fn point_position(&self, point: &HistoryPoint) -> Result<Option<CommitSeq>, StoreError> {
        Ok(self.points.get(point).copied())
    }

    fn put_blob(&mut self, bytes: &[u8]) -> Result<Sha512, StoreError> {
        let mut hasher = Sha512Hasher::new();
        hasher.update(bytes);
        let hex = HEXLOWER.encode(&hasher.finalize());
        let digest = Sha512::parse(&hex).map_err(|error| StoreError::Corruption {
            detail: format!("computed SHA-512 did not round-trip: {error}"),
        })?;
        self.blobs.entry(digest).or_insert_with(|| bytes.to_vec());
        Ok(digest)
    }

    fn get_blob(&self, digest: &Sha512) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.blobs.get(digest).cloned())
    }

    fn has_blob(&self, digest: &Sha512) -> Result<bool, StoreError> {
        Ok(self.blobs.contains_key(digest))
    }

    fn definition(&self) -> Result<Option<DefinitionText>, StoreError> {
        Ok(self.definition.clone())
    }

    fn composition(&self) -> Result<Option<Composition>, StoreError> {
        Ok(self.composition.clone())
    }
}

/// A factory producing fresh [`MemoryStore`]s. Used by the conformance suite so
/// the identical battery runs against every backend.
#[derive(Debug, Default)]
pub struct MemoryStoreFactory;

impl MemoryStoreFactory {
    /// A new factory.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl StoreFactory for MemoryStoreFactory {
    type Store = MemoryStore;

    fn create(&mut self, instance: InstanceId) -> Result<Self::Store, StoreError> {
        Ok(MemoryStore::new(instance))
    }
}
