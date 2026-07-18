//! The ephemeral response value a mutation `return` produces (§8.10), plus the
//! wire projection tests and clients read it through.

use liasse_expr::{Cell, Row};

/// A mutation response: the [`Cell`] the `return` expression evaluated to from
/// the admitted state (§8.6). It is call output, not stored state.
#[derive(Debug, Clone)]
pub struct ResponseValue {
    cell: Cell,
}

impl ResponseValue {
    /// Wrap an evaluated response cell.
    #[must_use]
    pub(crate) fn new(cell: Cell) -> Self {
        Self { cell }
    }

    /// The underlying cell (a scalar value, a row, or a row collection).
    #[must_use]
    pub fn cell(&self) -> &Cell {
        &self.cell
    }

    /// The canonical strict-JSON projection of the response (Annex A): a scalar
    /// as its wire value, a row as an object of its cells, a collection as an
    /// array of such objects.
    #[must_use]
    pub fn to_wire(&self) -> serde_json::Value {
        cell_to_wire(&self.cell)
    }
}

fn cell_to_wire(cell: &Cell) -> serde_json::Value {
    match cell {
        Cell::Scalar(value) => value.to_wire(),
        Cell::Row(row) => row_to_wire(row),
        Cell::Collection(rows) => serde_json::Value::Array(rows.iter().map(row_to_wire).collect()),
    }
}

fn row_to_wire(row: &Row) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (name, cell) in row.cells() {
        // A `none` optional field is an absent optional value: its field-position
        // wire form is an omitted member (SPEC Annex A "omitted optional field" /
        // SPEC-ISSUES item 29 — `none` is absence, no `{ "$none": true }` sentinel),
        // so it is dropped rather than serialized.
        if matches!(cell, Cell::Scalar(liasse_value::Value::None)) {
            continue;
        }
        map.insert(name.clone(), cell_to_wire(cell));
    }
    serde_json::Value::Object(map)
}
