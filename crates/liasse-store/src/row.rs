//! The stored form of one row.

use liasse_ident::RowIncarnation;
use liasse_value::Value;

/// One stored row: its immutable incarnation (D.1) and its typed payload value.
///
/// The store keeps the incarnation beside the value so that rekey can move a row
/// to a new address while preserving identity, and so replay can reproduce the
/// exact incarnation that a live insert allocated. The payload is an opaque
/// [`Value`] to the store — its shape is the runtime's concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    incarnation: RowIncarnation,
    value: Value,
}

impl StoredRow {
    /// Assemble a stored row from its incarnation and payload.
    #[must_use]
    pub fn new(incarnation: RowIncarnation, value: Value) -> Self {
        Self { incarnation, value }
    }

    /// The immutable incarnation — the row's durable identity (D.1).
    #[must_use]
    pub fn incarnation(&self) -> &RowIncarnation {
        &self.incarnation
    }

    /// The typed payload value.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}
