//! Store: the typed storage contract the runtime executes against — state,
//! history, blobs, and serial-ordering primitives. Implementations provide the
//! guarantees; the runtime provides the semantics. `liasse-pg` is the
//! PostgreSQL implementation.
//!
//! # What this crate owns
//!
//! - The [`InstanceStore`] / [`Transition`] / [`StoreFactory`] contract
//!   ([`contract`]): atomic commit admission at one gapless serial position
//!   ([`CommitSeq`]), frontier [`Snapshot`] reads, a replayable commit log
//!   ([`CommittedTransition`]), history-point recording (§19), content-addressed
//!   blob hooks, and durable per-instance metadata ([`DefinitionText`],
//!   [`Composition`]).
//! - Typed keys and values at the boundary ([`key`]): a row is addressed by a
//!   [`RowAddress`] of typed [`KeyValue`]s, never a bare string, and ordered by
//!   the Annex B [`liasse_value::Value`] order.
//! - [`MemoryStore`] — the `BTreeMap`-backed reference implementation, proving
//!   the contract is implementable and serving as the runtime's test double.
//! - [`contract_tests`] — the reusable conformance battery, generic over any
//!   [`StoreFactory`], so memory and PostgreSQL run the identical checks.
//!
//! # Semantics-free by design
//!
//! The store stores, orders, and retrieves. It enforces structural facts (one
//! row per address, gapless positions, faithful replay) but never type, ref,
//! check, or authorization rules — those live in the runtime above (§23).
//!
//! # Documented spec-gap choices
//!
//! Delete/rekey of an absent address is reported as a structural
//! [`StoreError::NotFound`]; whether that is an application error is a semantic
//! question left to the runtime (SPEC-ISSUES item 7). History lineage/branch
//! structure (§19.3) is the runtime's concern; the store records the point-to-
//! position mapping only.

mod commit;
mod contract;
mod error;
mod key;
mod memory;
mod meta;
mod row;
mod snapshot;
mod staging;
mod view_program;

pub mod contract_tests;

pub use commit::{CommitOutcome, CommitSeq, CommittedRowOp, CommittedTransition};
pub use contract::{InstanceStore, StoreFactory, Transition};
pub use error::StoreError;
pub use key::{key_from_components, AddressStep, CollectionPath, KeyValue, RowAddress};
pub use memory::{MemoryStore, MemoryStoreFactory};
pub use meta::{Composition, DefinitionText, Mount};
pub use row::StoredRow;
pub use snapshot::Snapshot;
pub use staging::MemoryTransition;
pub use view_program::{
    CandidateSubtree, EvalFault, EvaluatedRow, SortDirection, ViewProgram, ViewSource,
    MAX_SUBTREE_DEPTH,
};
