//! The single typed error a store operation can return.
//!
//! The store is semantics-free: it never reports type, ref, check, or
//! authorization failures — those live above it. Its failures are structural
//! (an address already occupied, an address absent), integrity (the durable
//! record disagrees with itself), or backend transport. Past a successful call
//! the returned value is proof the operation held.

use thiserror::Error;

/// Every way a store operation can fail. Categories are stable and independent
/// of any backend detail (§23.8: "Runtime errors preserve their structured
/// category independently of backend details").
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// A structural uniqueness violation: an insert or rekey targets an address
    /// that already holds a row. The address identity within its collection is
    /// unique (§5.4), so two rows can never share one address.
    #[error("row address `{address}` is already occupied ({context})")]
    Conflict { address: String, context: &'static str },

    /// An update, delete, or rekey named an address with no live row. Whether
    /// an absent-key mutation is an application error is a semantic question
    /// (SPEC-ISSUES item 7); the store only reports the structural fact.
    #[error("no live row at address `{address}` ({context})")]
    NotFound { address: String, context: &'static str },

    /// The durable record is internally inconsistent: a committed transition
    /// references an incarnation or address that replay cannot reconcile, or a
    /// requested commit position lies outside the recorded log. This is never a
    /// well-formed input's fault.
    #[error("store corruption: {detail}")]
    Corruption { detail: String },

    /// A backend transport or infrastructure failure (a connection dropped, a
    /// disk write failed). Carried opaquely so the category survives regardless
    /// of the underlying driver.
    #[error("store backend failure: {detail}")]
    Backend { detail: String },

    /// A [`ViewProgram`](crate::ViewProgram) face faulted while evaluating one
    /// candidate during [`scan_view`](crate::InstanceStore::scan_view) — a
    /// division by zero on the candidate's values, a shape mismatch, an unbound
    /// reference. Distinct from [`StoreError::Corruption`]: the durable record is
    /// well-formed, the *evaluation* faulted. The runtime answers this with the
    /// interpreter fallback so the surfaced behaviour is interpreter-exact.
    #[error("view-program evaluation fault: {detail}")]
    Eval { detail: String },
}
