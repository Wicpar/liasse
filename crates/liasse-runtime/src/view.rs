//! View evaluation at a frontier and the minimal init/patch delta between two
//! frontiers (§7, §12.4–§12.6) — the primitive a subscription layer turns into
//! live watches.
//!
//! A view is evaluated as a pure expression over the committed state at a
//! [`CommitSeq`]; row identity comes from `liasse-expr`'s [`RowId`], so a delta
//! is a straightforward keyed comparison of two evaluations.

use std::collections::BTreeMap;

use liasse_expr::{Cell, RowId};
use liasse_value::Value;

/// One row of a view result: its stable identity and its scalar output fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRow {
    id: RowId,
    fields: BTreeMap<String, Value>,
}

impl ViewRow {
    /// The row's stable occurrence identity (B.5).
    #[must_use]
    pub fn id(&self) -> &RowId {
        &self.id
    }

    /// The value of an output field, if present.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.get(name)
    }

    /// The output fields in canonical name order.
    pub fn fields(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.fields.iter()
    }
}

/// A materialized view at one frontier: its rows in canonical order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewResult {
    rows: Vec<ViewRow>,
}

impl ViewResult {
    /// Build a result from an evaluated cell (a collection, a single row, or an
    /// empty scalar).
    pub(crate) fn from_cell(cell: &Cell) -> Self {
        let rows = match cell {
            Cell::Collection(rows) => rows.iter().map(view_row).collect(),
            Cell::Row(row) => vec![view_row(row)],
            Cell::Scalar(_) => Vec::new(),
        };
        Self { rows }
    }

    /// The rows in canonical order.
    #[must_use]
    pub fn rows(&self) -> &[ViewRow] {
        &self.rows
    }

    /// The number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the view holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The change between two view frontiers (§12.4): the full set at first
/// observation, or the added/removed/changed rows between successive frontiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewDelta {
    /// The complete initial row set (no prior frontier).
    Init(Vec<ViewRow>),
    /// Rows added, removed (by identity), and changed since the prior frontier.
    Patch { added: Vec<ViewRow>, removed: Vec<RowId>, changed: Vec<ViewRow> },
}

impl ViewDelta {
    /// The delta from `prev` (the prior observation, or `None` for the first) to
    /// `next`, keyed by row identity.
    #[must_use]
    pub fn between(prev: Option<&ViewResult>, next: &ViewResult) -> Self {
        let Some(prev) = prev else {
            return Self::Init(next.rows.clone());
        };
        let before: BTreeMap<&RowId, &ViewRow> = prev.rows.iter().map(|r| (&r.id, r)).collect();
        let after: BTreeMap<&RowId, &ViewRow> = next.rows.iter().map(|r| (&r.id, r)).collect();
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for row in &next.rows {
            match before.get(&row.id) {
                None => added.push(row.clone()),
                Some(prior) if prior.fields != row.fields => changed.push(row.clone()),
                Some(_) => {}
            }
        }
        let removed = prev
            .rows
            .iter()
            .filter(|row| !after.contains_key(&row.id))
            .map(|row| row.id.clone())
            .collect();
        Self::Patch { added, removed, changed }
    }
}

fn view_row(row: &liasse_expr::Row) -> ViewRow {
    let fields = row
        .cells()
        .filter_map(|(name, cell)| match cell {
            // §A.9 / Annex A wire table: a `none` optional field is an *absent*
            // optional value whose field-position wire form is an omitted member
            // (SPEC "omitted optional field"), distinct from a present JSON
            // `null` (`Value::Json(Json::Null)`). Drop it so a projected
            // optional that is `none` does not appear as a member.
            Cell::Scalar(Value::None) => None,
            Cell::Scalar(value) => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    ViewRow { id: row.id().clone(), fields }
}
