//! Backend-failure mapping.
//!
//! Every driver or transport error collapses into [`StoreError::Backend`], whose
//! category survives independently of the underlying driver (SPEC §23.8:
//! "Runtime errors preserve their structured category independently of backend
//! details"). Structural failures — an occupied address, an absent row — are the
//! transition layer's business and never travel this path; only genuine
//! infrastructure faults do.

use liasse_store::StoreError;
use postgres::types::FromSql;
use postgres::{Error, Row};

/// Map a driver error into the [`StoreError::Backend`] category, preserving the
/// server's SQLSTATE and message where the failure came from PostgreSQL.
///
/// `postgres::Error`'s own `Display` collapses every server-side failure to the
/// opaque string `"db error"`, discarding the SQLSTATE and message that name the
/// actual cause — e.g. `22021` (invalid `U+0000` byte in a `text` value), `54000`
/// (index row exceeds the btree tuple maximum for an oversized key), or `23505`
/// (unique violation). SPEC §23.8 keeps the error *category* independent of the
/// backend; carrying the SQLSTATE and message keeps the *detail* actionable, which
/// AGENTS.md requires of a diagnostic. Transport faults with no `DbError` fall back
/// to the driver's own message.
pub fn backend(error: Error) -> StoreError {
    let detail = match error.as_db_error() {
        Some(db) => format!("{}: {}", db.code().code(), db.message()),
        None => error.to_string(),
    };
    StoreError::Backend { detail }
}

/// Read column `column` of `table` from a durable `row`, mapping a type mismatch
/// or an absent column to a [`StoreError::Corruption`] that names the offending
/// table and column.
///
/// [`postgres::Row::get`] is the panicking accessor — it unwraps a `try_get`
/// internally — so a durable value that no longer matches the expected Rust type
/// would abort the process. AGENTS.md forbids panics on the read path (the rule
/// covers a panicking method call, not just an explicit `unwrap`), so every read
/// of a stored cell goes through this non-panicking form instead.
pub fn cell<'a, T>(row: &'a Row, table: &str, column: &str) -> Result<T, StoreError>
where
    T: FromSql<'a>,
{
    row.try_get(column)
        .map_err(|error| StoreError::Corruption { detail: format!("{table}.{column}: {error}") })
}

/// Report an operational refusal (a schema stamped newer than this build) as a
/// backend failure with an actionable message.
pub fn refuse(detail: impl Into<String>) -> StoreError {
    StoreError::Backend { detail: detail.into() }
}

/// Report a durable-record inconsistency the store cannot reconcile.
pub fn corrupt(detail: impl Into<String>) -> StoreError {
    StoreError::Corruption { detail: detail.into() }
}
