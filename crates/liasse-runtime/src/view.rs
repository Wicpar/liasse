//! View evaluation at a frontier and the ordered init/patch delta between two
//! frontiers (§7, §12.2) — the primitive a subscription layer turns into live
//! watches.
//!
//! A view is evaluated as a pure expression over the committed state at a
//! [`CommitSeq`]; row identity comes from `liasse-expr`'s [`RowId`], so an ordered
//! §12.2 patch ([`crate::patch::diff`]) is computed from two evaluations.

use std::collections::BTreeMap;

use liasse_expr::{Cell, RowId, SortOrder};
use liasse_value::Value;

use crate::patch::PatchOp;

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

    /// Whether `other` carries the same exposed value (output fields) as this row,
    /// ignoring occurrence identity and internal `$sort` position — the §12.2
    /// `update` test. A sort-position-only change (a non-projected `$sort` key
    /// moving) leaves the exposed value equal, so it needs a `move`, not an
    /// `update`.
    #[must_use]
    pub(crate) fn same_value(&self, other: &Self) -> bool {
        self.fields == other.fields
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
    /// A row-stream result: its rows in canonical order (zero or more), and the
    /// total order (§7.3, `$sort` directions) they are delivered in — the order a
    /// bounded window partitions rows through at its §12.2 gap coordinate.
    Rows { rows: Vec<ViewRow>, order: SortOrder },
    /// A scalar or aggregate result (§7.5): the single value it delivers, not
    /// wrapped in a row.
    Scalar(Value),
}

impl ViewResult {
    /// Build a result from an evaluated cell, in the total `order` the view's
    /// outermost `$sort` fixed (§7.3). A collection or single row becomes a row
    /// stream carrying that order; a scalar/aggregate cell becomes a
    /// [`ViewResult::Scalar`] carrying its value (§12.2), never a dropped empty
    /// stream.
    pub(crate) fn from_cell(cell: &Cell, order: SortOrder) -> Self {
        match cell {
            Cell::Collection(rows) => Self::Rows { rows: rows.iter().map(view_row).collect(), order },
            Cell::Row(row) => Self::Rows { rows: vec![view_row(row)], order },
            Cell::Scalar(value) => Self::Scalar(value.clone()),
        }
    }

    /// The rows in canonical order. A scalar result has no rows, so this is an
    /// empty slice; read [`Self::scalar`] to recover a scalar result's value.
    #[must_use]
    pub fn rows(&self) -> &[ViewRow] {
        match self {
            Self::Rows { rows, .. } => rows,
            Self::Scalar(_) => &[],
        }
    }

    /// The total order the rows are delivered in (§7.3): the `$sort` directions a
    /// bounded window partitions rows through at its §12.2 gap coordinate. A scalar
    /// result has no rows, hence no order.
    #[must_use]
    pub fn order(&self) -> Option<&SortOrder> {
        match self {
            Self::Rows { order, .. } => Some(order),
            Self::Scalar(_) => None,
        }
    }

    /// The scalar value, when this is a scalar/aggregate result (§12.2). A reader
    /// renders `Some(value)` as the JSON scalar; a row-stream result is `None`.
    #[must_use]
    pub fn scalar(&self) -> Option<&Value> {
        match self {
            Self::Scalar(value) => Some(value),
            Self::Rows { .. } => None,
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

/// The change between two view frontiers (§12.2): the full set at first
/// observation (the `init` payload), or the ordered §12.2 patch that carries the
/// prior frontier's client result to this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewDelta {
    /// The complete initial row set (no prior frontier) — the `init` payload.
    Init(Vec<ViewRow>),
    /// An ordered §12.2 patch: the operations that, applied in listed order to the
    /// prior frontier's result, yield this one EXACTLY — same occurrences, same
    /// exposed values, same order. The empty sequence is a frontier-only patch.
    Patch(Vec<PatchOp>),
}

impl ViewDelta {
    /// The delta from `prev` (the prior observation, or `None` for the first) to
    /// `next`. The first observation is an [`Init`]; a subsequent frontier is the
    /// ordered §12.2 patch [`crate::patch::diff`] computes — applying it in order
    /// to `prev`'s rows reproduces `next`'s rows including order (§12.2).
    ///
    /// [`Init`]: ViewDelta::Init
    #[must_use]
    pub fn between(prev: Option<&ViewResult>, next: &ViewResult) -> Self {
        match prev {
            None => Self::Init(next.rows().to_vec()),
            Some(prev) => Self::Patch(crate::patch::diff(prev.rows(), next.rows())),
        }
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
