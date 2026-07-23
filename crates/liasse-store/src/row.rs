//! The stored form of one row.

use liasse_ident::RowIncarnation;
use liasse_value::{Timestamp, Value};

/// One stored row: its immutable incarnation (D.1), its recorded admission
/// instant (§14.1 `$created`, §22.6), and its typed payload value.
///
/// The store keeps the incarnation beside the value so that rekey can move a row
/// to a new address while preserving identity, and so replay can reproduce the
/// exact incarnation that a live insert allocated. The `created` instant is the
/// row's recorded admission time (§22.1 recorded observation): fixed once at the
/// row's insert to the request's `now()` (§22.5), preserved across every later
/// update and rekey, and reproduced on replay — it is the `$created` lower bound a
/// lifecycle bucket defaults its `$from` to (§14.1). The payload is an opaque
/// [`Value`] to the store — its shape is the runtime's concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    incarnation: RowIncarnation,
    created: Timestamp,
    value: Value,
}

impl StoredRow {
    /// Assemble a stored row from its incarnation, recorded creation instant, and
    /// payload.
    #[must_use]
    pub fn new(incarnation: RowIncarnation, created: Timestamp, value: Value) -> Self {
        Self { incarnation, created, value }
    }

    /// The immutable incarnation — the row's durable identity (D.1).
    #[must_use]
    pub fn incarnation(&self) -> &RowIncarnation {
        &self.incarnation
    }

    /// The recorded admission instant (§14.1 `$created`, §22.6): the request `now()`
    /// fixed at the row's insert and preserved across updates and rekeys.
    #[must_use]
    pub fn created(&self) -> Timestamp {
        self.created
    }

    /// The typed payload value.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}
