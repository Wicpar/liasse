//! Postgres: the PostgreSQL-backed implementation of the `liasse-store`
//! contract. The only crate in the workspace that speaks to PostgreSQL.
//!
//! # Architecture
//!
//! - [`PgStoreFactory`] opens connections and owns per-instance schema lifecycle:
//!   one PostgreSQL schema per instance (derived from its identity), created on
//!   open and droppable as a unit ([`schema`]). Every open runs the self-reconciling
//!   lifecycle ([`reconcile`]): the physical schema is brought into exact
//!   correspondence with the model — missing tables and indexes are created, and
//!   orphan indexes/tables a superseded model or an older backend left behind are
//!   dropped, so migrations never pollute the database. It refuses to open a schema
//!   stamped newer than the embedded, versioned DDL knows.
//! - [`PgStore`] holds one writer connection (one writer per instance) plus an
//!   r2d2 read pool, and **no in-memory read model of durable state**. Under the
//!   pure-PG re-architecture (`DESIGN-pure-pg.md`) every contract `&self` read is
//!   served by one indexed SQL statement (or, for `snapshot`, one log read plus a
//!   Rust fold) on a pooled connection ([`read`], [`store`]): the leaf reads
//!   (Phase 1), the `row`/`scan` node reads (Phase 2, §4.1/§4.2), and now
//!   `snapshot`'s §4.3 log fold. The in-memory projection was deleted in Phase 3,
//!   satisfying the "no in-memory projection" mandate: a process restart is a
//!   no-op — a reopened [`PgStore`] answers reads straight from the durable tables
//!   with nothing to rebuild (`PgStoreFactory::reopen`), which is what makes
//!   durability observable.
//! - Every mutating contract call maps to exactly one SQL transaction, which writes
//!   **state only** — it takes no serial position and no instance-wide lock, so two
//!   admissions to one instance overlap. Serial positions (SPEC §22.3) are stamped
//!   afterwards by [`history`], over admissions that have *settled*, which is what
//!   makes them monotone without any writer waiting on another (SPEC §22.1: history
//!   construction follows committed transitions independently of write admission).
//!
//! # Sync driver choice
//!
//! The contract is synchronous and `&mut`-based (concurrency is the runtime's
//! concern, one writer per instance). The maintained `postgres` crate — the
//! blocking facade over `tokio-postgres` — matches that shape directly, with no
//! async runtime and no async colouring bleeding into the contract. TLS is not
//! required for local integration testing and is left off (`NoTls`).
//!
//! # Schema-free persistence
//!
//! The store never holds a [`liasse_value::Type`], so it cannot decode a value's
//! type-directed canonical wire form. Values and addresses persist through a
//! lossless, self-describing tagged codec ([`value_codec`], [`record_codec`])
//! built solely from `liasse-value`/`liasse-ident` public surface, so a decoded
//! value is as well-formed as one the runtime parsed and a malformed durable
//! record is a [`liasse_store::StoreError::Corruption`].

// The body of one admission transaction — the crate's only durable write on
// behalf of a transition.
mod admit;
mod backend;
mod factory;
// Post-settlement serial-position assignment: the half of admission that §22.1
// separates from writing.
mod history;
mod jsonb_text;
// The order-preserving `key_enc` BYTEA codec: the `nodes` write path
// ([`node_write`]) encodes each level key with it for the `key_enc` lookup/scan
// column.
mod key_enc;
mod key_enc_num;
mod node_load;
mod node_write;
mod read;
mod reconcile;
mod record_codec;
mod schema;
// The order-preserving `sort_enc` BYTEA codec (§7.4): the `ORDER BY` key the
// pushdown `liasse.eval_sort` face emits, built on the shared [`key_enc`] machinery.
mod sort_enc;
mod store;
mod transition;
mod value_codec;

#[cfg(test)]
mod composite_key_enc_redteam;
#[cfg(test)]
mod key_enc_boundary_test;
#[cfg(test)]
mod key_enc_proptest;

pub use factory::PgStoreFactory;
pub use schema::{IndexSpec, MIN_COMPATIBLE_VERSION, SCHEMA_VERSION, Schema, SequenceSpec, TableSpec};
pub use sort_enc::encode_sort_tuple;
pub use store::PgStore;
pub use transition::PgTransition;
