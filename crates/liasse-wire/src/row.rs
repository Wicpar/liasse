//! One row of a live view as it travels on the wire.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::token::Occ;

/// A view row on the wire (§12.2): its opaque occurrence token and its exposed
/// value, and nothing else. The internal `RowId`, the `$sort` tuple that fixed its
/// order, and any unprojected field never appear here — the connect layer already
/// projected the row to the authorized value the engine's `to_wire` produced, with
/// absent optional fields omitted (Annex A).
///
/// The value is an opaque [`serde_json::Value`]: this crate carries it verbatim and
/// never inspects its shape, so the wire schema stays decoupled from the value
/// model. Two rows are the same occurrence exactly when their [`Occ`] is equal;
/// [`crate::apply`] keys every patch operation off that token, never off position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRow {
    /// The opaque occurrence token. On the wire it is the member `id` (§12.2 `$id`),
    /// the name shared with the patch vocabulary.
    #[serde(rename = "id")]
    occ: Occ,
    /// The row's exposed value, rendered by the engine and carried verbatim.
    value: Value,
}

impl WireRow {
    /// A row carrying `value` under occurrence `occ`.
    #[must_use]
    pub fn new(occ: Occ, value: Value) -> Self {
        Self { occ, value }
    }

    /// The occurrence token identifying this row.
    #[must_use]
    pub fn occ(&self) -> &Occ {
        &self.occ
    }

    /// The row's exposed value.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Replace the exposed value while preserving the occurrence identity — the
    /// effect of an `update` (§12.2) applied in place.
    pub(crate) fn set_value(&mut self, value: Value) {
        self.value = value;
    }
}
