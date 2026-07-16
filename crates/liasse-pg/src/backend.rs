//! Backend-failure mapping.
//!
//! Every driver or transport error collapses into [`StoreError::Backend`], whose
//! category survives independently of the underlying driver (SPEC §23.8:
//! "Runtime errors preserve their structured category independently of backend
//! details"). Structural failures — an occupied address, an absent row — are the
//! transition layer's business and never travel this path; only genuine
//! infrastructure faults do.

use liasse_store::StoreError;

/// Map a driver error into the opaque [`StoreError::Backend`] category.
pub fn backend<E: core::fmt::Display>(error: E) -> StoreError {
    StoreError::Backend { detail: error.to_string() }
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
