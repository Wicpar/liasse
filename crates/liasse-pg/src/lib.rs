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
//!   r2d2 read pool. Under the pure-PG re-architecture (`DESIGN-pure-pg.md`) the
//!   contract's `&self` reads are served by one indexed SQL statement each on a
//!   pooled connection ([`read`], [`store`]): the leaf reads (Phase 1) and now the
//!   `row`/`scan` node reads (Phase 2, §4.1/§4.2). A shrinking in-memory
//!   [`projection`] still backs `snapshot` (its replayable `log`) and the staging
//!   read base (its `current` map), both retired in Phase 3. A process restart
//!   rebuilds that projection — and answers every SQL read — from the durable
//!   tables (`PgStoreFactory::reopen`), which is what makes durability observable.
//! - Every mutating contract call maps to exactly one SQL transaction. The serial
//!   position comes from a per-instance counter row locked `FOR UPDATE`, so it is
//!   gapless and monotone — a plain PostgreSQL `SEQUENCE` gaps on rollback and is
//!   deliberately avoided (SPEC §22.3).
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

mod backend;
mod factory;
mod jsonb_text;
// The order-preserving `key_enc` BYTEA codec: the `nodes` write path
// ([`node_write`]) encodes each level key with it for the `key_enc` lookup/scan
// column.
mod key_enc;
mod key_enc_num;
mod node_load;
mod node_write;
mod projection;
mod read;
mod reconcile;
mod record_codec;
mod schema;
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
pub use schema::{IndexSpec, SCHEMA_VERSION, Schema, TableSpec};
pub use store::PgStore;
pub use transition::PgTransition;
