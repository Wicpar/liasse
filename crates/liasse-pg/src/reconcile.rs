//! The self-reconciling schema lifecycle.
//!
//! On every open ([`crate::PgStoreFactory::reopen`]/`create`) the backend brings
//! an instance's physical schema into *exact* correspondence with what the current
//! model declares: it creates every fixed table and derived index that is missing,
//! and it drops every orphan — a structure a prior backend version or a superseded
//! model left behind. Migrations never pollute the database; only used structures
//! persist. Three orphan classes are eliminated:
//!
//! - **Orphan indexes.** When [`Schema::indexes`] shrinks across a
//!   [`SCHEMA_VERSION`] change, older databases retain the removed secondary
//!   indexes forever. Reconciliation diffs the live secondary indexes against the
//!   declared set and drops the difference.
//! - **Orphan tables.** A base table present in the instance schema but absent from
//!   the fixed set ([`Schema::tables`]) is a leftover from an earlier layout and is
//!   dropped `CASCADE`. The six fixed tables are never orphans, so the retained
//!   history and blob stores (§21: `commit_log`/`history_points`/`blobs`) are safe.
//! - **Orphan rows.** Handled by the write path, not here: a collection lives as a
//!   key prefix in the single `rows` table, so a §20 migration removing it issues a
//!   `Delete` per row and each leaves the table with no residue (proven by the
//!   crate's no-orphan-rows gate).
//!
//! Create and clean are driven from the *same* enumerable [`Schema::tables`] /
//! [`Schema::indexes`] data, so the desired set has one source of truth. The whole
//! diff applies in one transaction — atomic, or not at all.
//!
//! # Never dropping an intrinsic index
//!
//! Primary-key and unique constraints materialize indexes intrinsic to their
//! table: they drop with the table and must never be reconciled away. The live-set
//! query excludes any index that backs a `pg_constraint` row (its `conindid`), so
//! only bare `CREATE INDEX` secondary indexes — the ones this backend manages — are
//! ever candidates for dropping.
//!
//! # Version gate
//!
//! Stamping runs before the drop side: a schema stamped newer than this build is
//! refused *before* any prune, so an older backend can never delete a structure a
//! newer one legitimately added. Refusing rolls the transaction back untouched.

use liasse_store::StoreError;
use postgres::{Client, Transaction};

use crate::backend::{backend, cell, refuse};
use crate::schema::{SCHEMA_VERSION, Schema, TableSpec};

/// Bring `schema`'s physical objects into exact correspondence with the model:
/// create every missing fixed table and derived index, and drop every orphan
/// secondary index and table. Atomic; refuses (and rolls back) a schema newer than
/// this build before pruning anything.
pub(crate) fn reconcile(client: &mut Client, schema: &Schema) -> Result<(), StoreError> {
    let s = schema.quoted();
    let mut txn = client.transaction().map_err(backend)?;

    // CREATE side — idempotent DDL for the fixed tables and every derived index.
    txn.batch_execute(&schema.create_ddl()).map_err(backend)?;

    // Version gate — stamp forward, then refuse a newer schema before any prune.
    txn.execute(
        &format!(
            "INSERT INTO {s}.schema_version (id, version) VALUES (1, $1) \
             ON CONFLICT (id) DO UPDATE \
             SET version = GREATEST(schema_version.version, EXCLUDED.version)"
        ),
        &[&SCHEMA_VERSION],
    )
    .map_err(backend)?;
    let stamped: i32 = cell(
        &txn.query_one(&format!("SELECT version FROM {s}.schema_version WHERE id = 1"), &[])
            .map_err(backend)?,
        "schema_version",
        "version",
    )?;
    if stamped > SCHEMA_VERSION {
        return Err(refuse(format!(
            "schema `{}` is version {stamped}, newer than this build ({SCHEMA_VERSION}); \
             refusing to open",
            schema.name()
        )));
    }

    // DROP side — prune orphan secondary indexes, then orphan tables.
    let declared_indexes: Vec<String> =
        schema.indexes().iter().map(|index| index.name().to_owned()).collect();
    for live in live_secondary_indexes(&mut txn, schema)? {
        if !declared_indexes.iter().any(|declared| declared == &live) {
            txn.batch_execute(&schema.drop_index_sql(&live)).map_err(backend)?;
        }
    }
    let fixed_tables = schema.tables();
    for live in live_tables(&mut txn, schema)? {
        if !fixed_tables.iter().any(|fixed: &TableSpec| fixed.name() == live) {
            txn.batch_execute(&schema.drop_table_sql(&live)).map_err(backend)?;
        }
    }

    txn.commit().map_err(backend)
}

/// The live secondary indexes in `schema`: every index that does **not** back a
/// primary-key or unique constraint. Constraint-backed indexes are intrinsic to
/// their table (they drop with it) and are excluded here so the reconciler can
/// never drop one; a bare `CREATE INDEX` is the only kind this backend manages.
fn live_secondary_indexes(
    txn: &mut Transaction<'_>,
    schema: &Schema,
) -> Result<Vec<String>, StoreError> {
    let rows = txn
        .query(
            "SELECT c.relname \
             FROM pg_class c \
             JOIN pg_index x ON x.indexrelid = c.oid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind = 'i' \
               AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = c.oid)",
            &[&schema.name()],
        )
        .map_err(backend)?;
    rows.iter().map(|row| cell::<String>(row, "pg_class", "relname")).collect()
}

/// The live base tables in `schema` — the candidate set the fixed [`Schema::tables`]
/// is diffed against to find orphan tables.
fn live_tables(txn: &mut Transaction<'_>, schema: &Schema) -> Result<Vec<String>, StoreError> {
    let rows = txn
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = $1 AND table_type = 'BASE TABLE'",
            &[&schema.name()],
        )
        .map_err(backend)?;
    rows.iter().map(|row| cell::<String>(row, "information_schema.tables", "table_name")).collect()
}
