//! The storage contract the runtime executes against.
//!
//! The store is semantics-free: it stores, orders, and retrieves. Every
//! guarantee here is structural or temporal — atomic admission, one gapless
//! serial order, frontier snapshots, replayable logs, content-addressed blobs —
//! and none of it validates types, refs, checks, or authorization, which the
//! runtime layers above (§23). The traits are synchronous and `&mut`-based:
//! concurrency is the runtime's concern, with one writer per instance, so a
//! staged [`Transition`] takes exclusive access for its lifetime.

use liasse_ident::{HistoryPoint, InstanceId, RowIncarnation, TransactionId};
use liasse_value::{Sha512, Value};

use crate::commit::{CommitOutcome, CommitSeq, CommittedTransition};
use crate::error::StoreError;
use crate::key::{CollectionPath, RowAddress};
use crate::meta::{Composition, DefinitionText};
use crate::row::StoredRow;
use crate::snapshot::Snapshot;

/// A store for one package instance's durable state, history, and blobs.
pub trait InstanceStore {
    /// The staged-transition handle this store hands out. It borrows the store
    /// exclusively (one writer per instance) for read-your-writes staging.
    type Transition<'s>: Transition
    where
        Self: 's;

    /// The instance incarnation this store owns (D.1).
    fn instance(&self) -> &InstanceId;

    /// The current head position: the highest committed serial position, or
    /// [`CommitSeq::GENESIS`] before any commit.
    fn head(&self) -> CommitSeq;

    /// Read one row of current committed state by its canonical address.
    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError>;

    /// Scan a collection's direct rows in Annex B key-ascending order (B.5).
    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError>;

    /// A materialized snapshot of committed state at `frontier` — the live-view
    /// and replay read primitive. A `frontier` past [`InstanceStore::head`] is a
    /// corruption error; equal to the head yields current state.
    fn snapshot(&self, frontier: CommitSeq) -> Result<Snapshot, StoreError>;

    /// The committed transitions at positions `>= from`, in ascending order —
    /// the replay stream (§19.2).
    fn log_from(&self, from: CommitSeq) -> Result<Vec<CommittedTransition>, StoreError>;

    /// Open a staged transition: buffer reads and writes against a prospective
    /// state, then [`Transition::commit`] atomically or [`Transition::abort`].
    fn begin(&mut self) -> Self::Transition<'_>;

    /// Record that `point` names the retained state at `at` (§19.3). History
    /// materialization is independent of the write path (§19.2), so this is a
    /// separate durable hook rather than part of a transition.
    fn record_point(&mut self, at: CommitSeq, point: HistoryPoint) -> Result<(), StoreError>;

    /// The serial position a recorded history point names, if any.
    fn point_position(&self, point: &HistoryPoint) -> Option<CommitSeq>;

    /// Store blob bytes content-addressed by their SHA-512 (§18), returning the
    /// computed digest. Idempotent: storing the same bytes twice is one blob.
    /// Placement policy (which holders keep it) lives above the store.
    fn put_blob(&mut self, bytes: &[u8]) -> Result<Sha512, StoreError>;

    /// Fetch blob bytes by digest, if held.
    fn get_blob(&self, digest: &Sha512) -> Result<Option<Vec<u8>>, StoreError>;

    /// Whether the store holds a blob for `digest`.
    fn has_blob(&self, digest: &Sha512) -> bool;

    /// The active definition text (D.4), if one has been recorded.
    fn definition(&self) -> Option<&DefinitionText>;

    /// The current composition of mounted children (§19.5), if recorded.
    fn composition(&self) -> Option<&Composition>;
}

/// A staged state transition: the unit of atomic admission (§22.2).
///
/// Reads see committed state overlaid with this transition's own staged writes
/// (read-your-writes). Staging never touches durable state; only
/// [`Transition::commit`] does, and it does so all-or-nothing, taking the next
/// serial position. Dropping without committing discards every staged write, so
/// an aborted transition leaves no trace.
pub trait Transition {
    /// Read a row through the transition: staged writes shadow committed state.
    fn row(&self, address: &RowAddress) -> Result<Option<StoredRow>, StoreError>;

    /// Scan a collection through the transition, in Annex B key order (B.5),
    /// reflecting staged inserts, updates, deletes, and rekeys.
    fn scan(&self, collection: &CollectionPath) -> Result<Vec<(RowAddress, StoredRow)>, StoreError>;

    /// Stage a fresh row at `address`, allocating and returning its incarnation
    /// (D.1). Errors [`StoreError::Conflict`] if the address already holds a row.
    fn insert(&mut self, address: RowAddress, value: Value) -> Result<RowIncarnation, StoreError>;

    /// Stage a new payload for an existing row, preserving its incarnation.
    /// Errors [`StoreError::NotFound`] if no row lives at `address`.
    fn update(&mut self, address: &RowAddress, value: Value) -> Result<(), StoreError>;

    /// Stage removal of a live row. Errors [`StoreError::NotFound`] if absent.
    fn delete(&mut self, address: &RowAddress) -> Result<(), StoreError>;

    /// Stage an atomic rekey (§5.4): move the row at `from` to `to` with payload
    /// `value`, preserving its incarnation and history continuity. Errors
    /// [`StoreError::NotFound`] if `from` is absent, or [`StoreError::Conflict`]
    /// if `to` is already occupied.
    fn rekey(
        &mut self,
        from: &RowAddress,
        to: RowAddress,
        value: Value,
    ) -> Result<(), StoreError>;

    /// Stage a new active definition for this instance (a `load` commit).
    fn set_definition(&mut self, definition: DefinitionText);

    /// Stage a new composition for this instance.
    fn set_composition(&mut self, composition: Composition);

    /// Tag this transition with a shared cross-instance transaction identity so
    /// each affected instance records the same atomic grouping (§19.1).
    fn set_transaction(&mut self, transaction: TransactionId);

    /// Whether nothing has been staged — a commit of an empty transition is
    /// [`CommitOutcome::Unchanged`] (§22.2).
    fn is_empty(&self) -> bool;

    /// Atomically admit every staged write, taking the next serial position.
    /// All-or-nothing: on any error the prior committed state is intact. An
    /// empty transition returns [`CommitOutcome::Unchanged`] without a commit.
    fn commit(self) -> Result<CommitOutcome, StoreError>;

    /// Discard every staged write, leaving committed state untouched.
    fn abort(self);
}

/// Constructs instance stores. The conformance suite is generic over this trait
/// so the identical battery runs against any backend (the in-memory reference
/// here, PostgreSQL next).
pub trait StoreFactory {
    /// The store type produced.
    type Store: InstanceStore;

    /// Create a fresh, empty instance store at genesis for `instance`.
    fn create(&mut self, instance: InstanceId) -> Result<Self::Store, StoreError>;
}
