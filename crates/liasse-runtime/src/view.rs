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

/// One row of a view result: its stable identity, its scalar output fields, and
/// the `$sort` tuple that fixed its ordered position (§7.3, empty when unsorted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRow {
    id: RowId,
    fields: BTreeMap<String, Value>,
    sort_tuple: Vec<Value>,
}

impl ViewRow {
    /// The row's stable occurrence identity (B.5).
    #[must_use]
    pub fn id(&self) -> &RowId {
        &self.id
    }

    /// The row's `$sort` tuple: the ordered coordinate that fixed its position in
    /// a sorted view (§7.3), or empty when unsorted. A bounded window retains this
    /// as its immutable ordered gap coordinate once the anchor leaves (§12.2).
    #[must_use]
    pub fn sort_tuple(&self) -> &[Value] {
        &self.sort_tuple
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

/// A materialized view at one frontier (§12.2). A `$view` delivers one of two
/// shapes: a row stream (a collection, single-row, or keyed-selection view) or a
/// single scalar value (a bare scalar field like `.n`, or an aggregate like
/// `= size(.docs)`, §7.5). The two are distinct kinds, not a row list that
/// happens to be empty — a scalar result carries its value directly so a reader
/// can render it as the JSON scalar §12.2 pins, rather than an empty stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewResult {
    /// A row-stream result: its rows in canonical order (zero or more).
    Rows(Vec<ViewRow>),
    /// A scalar or aggregate result (§7.5): the single value it delivers, not
    /// wrapped in a row.
    Scalar(Value),
}

impl ViewResult {
    /// Build a result from an evaluated cell. A collection or single row becomes a
    /// row stream; a scalar/aggregate cell becomes a [`ViewResult::Scalar`]
    /// carrying its value (§12.2), never a dropped empty stream.
    pub(crate) fn from_cell(cell: &Cell) -> Self {
        match cell {
            Cell::Collection(rows) => Self::Rows(rows.iter().map(view_row).collect()),
            Cell::Row(row) => Self::Rows(vec![view_row(row)]),
            Cell::Scalar(value) => Self::Scalar(value.clone()),
        }
    }

    /// The rows in canonical order. A scalar result has no rows, so this is an
    /// empty slice; read [`Self::scalar`] to recover a scalar result's value.
    #[must_use]
    pub fn rows(&self) -> &[ViewRow] {
        match self {
            Self::Rows(rows) => rows,
            Self::Scalar(_) => &[],
        }
    }

    /// The scalar value, when this is a scalar/aggregate result (§12.2). A reader
    /// renders `Some(value)` as the JSON scalar; a row-stream result is `None`.
    #[must_use]
    pub fn scalar(&self) -> Option<&Value> {
        match self {
            Self::Scalar(value) => Some(value),
            Self::Rows(_) => None,
        }
    }

    /// The number of rows (a scalar result reports zero — it has no rows).
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows().len()
    }

    /// Whether the view holds no rows (always true for a scalar result).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows().is_empty()
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
            return Self::Init(next.rows().to_vec());
        };
        let before: BTreeMap<&RowId, &ViewRow> = prev.rows().iter().map(|r| (&r.id, r)).collect();
        let after: BTreeMap<&RowId, &ViewRow> = next.rows().iter().map(|r| (&r.id, r)).collect();
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for row in next.rows() {
            match before.get(&row.id) {
                None => added.push(row.clone()),
                Some(prior) if prior.fields != row.fields => changed.push(row.clone()),
                Some(_) => {}
            }
        }
        let removed = prev
            .rows()
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
    ViewRow { id: row.id().clone(), fields, sort_tuple: row.sort().to_vec() }
}
