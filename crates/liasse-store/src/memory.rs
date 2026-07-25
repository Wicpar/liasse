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

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_value::{Sha512, Timestamp};

use crate::commit::{CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition};
use crate::contract::{InstanceStore, PendingCommit, StoreFactory};
use crate::error::StoreError;
use crate::key::{CollectionPath, RowAddress};
use crate::meta::{Composition, DefinitionText};
use crate::row::StoredRow;
use crate::snapshot::Snapshot;
use crate::staging::MemoryTransition;

/// One admitted transition that has not been given a serial position yet — the
/// *residual* an admission records and history later builds from (§22.1).
#[derive(Debug)]
struct Residual {
    ops: Vec<CommittedRowOp>,
    created: Timestamp,
    transaction: Option<TransactionId>,
}

/// A `BTreeMap`-backed store for one package instance.
#[derive(Debug)]
pub struct MemoryStore {
    instance: InstanceId,
    current: BTreeMap<RowAddress, StoredRow>,
    /// Admitted-but-unpositioned transitions, in admission order — the queue
    /// [`MemoryStore::build_history`] drains.
    residuals: Vec<Residual>,
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
            current: BTreeMap::new(),
            residuals: Vec::new(),
            log: Vec::new(),
            next_incarnation: 0,
            points: BTreeMap::new(),
            blobs: BTreeMap::new(),
            definition: None,
            composition: None,
        }
    }

    /// The built history's tip — the highest position handed out, or
    /// [`CommitSeq::GENESIS`] before the first. There is no separate head counter:
    /// the head IS history's tip, exactly as in the durable backend.
    fn tip(&self) -> CommitSeq {
        self.log.last().map_or(CommitSeq::GENESIS, CommittedTransition::seq)
    }

    /// Build history over every settled admission: position each residual, in
    /// admission order, after the current tip (§22.1).
    ///
    /// The durable backend must wait for admissions that are still in flight before
    /// it may position anything; here an admission *is* settled the moment it
    /// returns, because the store is in-process and takes `&mut self` for the whole
    /// of one, so a pass always drains the queue completely. Same rule, no waiting
    /// to do.
    fn build_history(&mut self) {
        let mut seq = self.tip();
        for residual in std::mem::take(&mut self.residuals) {
            seq = seq.next();
            let Residual { ops, created, transaction } = residual;
            self.log.push(CommittedTransition::new(seq, ops, created, transaction));
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

    /// Allocate the next opaque row incarnation (D.1). Tokens are opaque, so gaps
    /// from aborted transitions are harmless — which is exactly why the durable
    /// backend draws them from a `SEQUENCE`.
    pub(crate) fn alloc_incarnation(&mut self) -> RowIncarnation {
        let token = format!("row-{}", self.next_incarnation);
        self.next_incarnation += 1;
        RowIncarnation::new(token)
    }

    /// Atomically admit a staged transition, then build the history it belongs to.
    /// Empty transitions admit nothing (§22.2); otherwise the ops are applied to
    /// current state and recorded as a residual, and the history pass gives that
    /// residual its serial position.
    ///
    /// Admission itself assigns no position — the same split the durable backend
    /// makes (§22.1) — so the two stores agree on *when* a position exists, not just
    /// on what it is.
    pub(crate) fn commit_transition(
        &mut self,
        ops: Vec<CommittedRowOp>,
        created: Timestamp,
        transaction: Option<TransactionId>,
        definition: Option<DefinitionText>,
        composition: Option<Composition>,
    ) -> Result<CommitOutcome, StoreError> {
        if ops.is_empty() && definition.is_none() && composition.is_none() {
            return Ok(CommitOutcome::Unchanged);
        }
        for op in &ops {
            self.apply_current(op, created);
        }
        if let Some(definition) = definition {
            self.definition = Some(definition);
        }
        if let Some(composition) = composition {
            self.composition = Some(composition);
        }
        self.residuals.push(Residual { ops, created, transaction });
        self.build_history();
        Ok(CommitOutcome::Committed(self.tip()))
    }

    /// Apply one already-validated op to the current map. Staging established
    /// occupancy, so this never needs the replay corruption checks. `created` is the
    /// commit's fixed instant (§22.5): a fresh insert records it as the row's
    /// `$created`, while an update or rekey PRESERVES the row's existing `$created`
    /// (§22.6) — the same fold the log replay ([`Snapshot::apply`]) performs, so the
    /// current map and a frontier replay agree row-for-row.
    fn apply_current(&mut self, op: &CommittedRowOp, created: Timestamp) {
        match op {
            CommittedRowOp::Insert { address, incarnation, value } => {
                self.current
                    .insert(address.clone(), StoredRow::new(incarnation.clone(), created, value.clone()));
            }
            CommittedRowOp::Update { address, incarnation, value } => {
                let preserved = self.current.get(address).map_or(created, StoredRow::created);
                self.current
                    .insert(address.clone(), StoredRow::new(incarnation.clone(), preserved, value.clone()));
            }
            CommittedRowOp::Delete { address, .. } => {
                self.current.remove(address);
            }
            CommittedRowOp::Rekey { from, to, incarnation, value } => {
                let preserved = self.current.get(from).map_or(created, StoredRow::created);
                self.current.remove(from);
                self.current
                    .insert(to.clone(), StoredRow::new(incarnation.clone(), preserved, value.clone()));
            }
        }
    }
}

impl InstanceStore for MemoryStore {
    type Transition<'s> = MemoryTransition<'s>;

    fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// The in-memory reference commits an all-or-none multi-instance transition by
    /// staging and validating every participant first, then committing each in turn
    /// (§13.10): in-process and single-writer, so a validated commit does not fail
    /// and no other writer interleaves — indivisible in practice.
    fn multi_instance_atomic_commit(&self) -> bool {
        true
    }

    /// Commit an already-staged payload directly (§13.10): the resolved ops are
    /// admitted as one transition without re-staging or re-allocating incarnations.
    fn commit_pending(&mut self, pending: PendingCommit) -> Result<CommitOutcome, StoreError> {
        let PendingCommit { ops, created, transaction, definition, composition } = pending;
        self.commit_transition(ops, created, transaction, definition, composition)
    }

    fn head(&self) -> Result<CommitSeq, StoreError> {
        Ok(self.tip())
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

    fn scan_subtree(
        &self,
        root: &RowAddress,
        steps: &[String],
    ) -> Result<Vec<(RowAddress, StoredRow)>, StoreError> {
        // The oracle: an ordered prefix range over the live address map restricted
        // to descents through `steps`. `current` holds only live rows, so a
        // tombstoned intermediate is simply absent while its live orphan
        // descendants — whose addresses still extend `root` through `steps` — are
        // kept, exactly as `scan_subtree` promises. The `BTreeMap` iterates in
        // `RowAddress` (Annex B) order, so no sort is needed.
        Ok(self
            .current
            .iter()
            .filter(|(address, _)| address.descends_from(root, steps))
            .map(|(address, row)| (address.clone(), row.clone()))
            .collect())
    }

    fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError> {
        let head = self.tip();
        if frontier > head {
            return Err(StoreError::Corruption {
                detail: format!(
                    "snapshot frontier {} is past head {}",
                    frontier.get(),
                    head.get()
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
        let head = self.tip();
        if at > head {
            return Err(StoreError::Corruption {
                detail: format!("history point at {} is past head {}", at.get(), head.get()),
            });
        }
        self.points.insert(point, at);
        Ok(())
    }

    fn point_position(&self, point: &HistoryPoint) -> Result<Option<CommitSeq>, StoreError> {
        Ok(self.points.get(point).copied())
    }

    fn put_blob(&mut self, bytes: &[u8]) -> Result<Sha512, StoreError> {
        // §18.1: one shared content hasher on `Sha512` (see `liasse-pg`'s
        // `put_blob`), so the reference and the durable backend cannot drift.
        let digest = Sha512::of(bytes);
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
