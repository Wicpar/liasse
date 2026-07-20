//! View evaluation at a frontier and the ordered init/patch delta between two
//! frontiers (§7, §12.2) — the primitive a subscription layer turns into live
//! watches.
//!
//! A view is evaluated as a pure expression over the committed state at a
//! [`CommitSeq`]; row identity comes from `liasse-expr`'s [`RowId`], so an ordered
//! §12.2 patch ([`crate::patch::diff`]) is computed from two evaluations.

use std::collections::BTreeMap;

use liasse_expr::{Cell, Row, RowId, SortOrder};
use liasse_value::{Json, Struct, Text, Value};
use serde_json::Value as J;

use crate::error::EngineError;
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
    /// stream. Fallible because a projected nested sub-collection member is
    /// materialized through the canonical json codec ([`collection_value`]).
    pub(crate) fn from_cell(cell: &Cell, order: SortOrder) -> Result<Self, EngineError> {
        Ok(match cell {
            Cell::Collection(rows) => {
                Self::Rows { rows: rows.iter().map(view_row).collect::<Result<_, _>>()?, order }
            }
            Cell::Row(row) => Self::Rows { rows: vec![view_row(row)?], order },
            Cell::Scalar(value) => Self::Scalar(value.clone()),
        })
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

/// The change between two view frontiers (§12.2). A `$view` delivers one of two
/// result shapes (§7, §7.5), and each carries its own delta form on the same
/// `init`/`patch` semantics:
///
/// - a ROW stream ([`ViewResult::Rows`]): [`Init`] is the complete row set at
///   first observation, then [`Patch`] is the ordered op sequence advancing it;
/// - a SCALAR/aggregate value ([`ViewResult::Scalar`]): [`Scalar`] conveys the
///   value at first observation or when it changed, and is the frontier-only no-op
///   when it did not.
///
/// A view's result shape is fixed for its lifetime, so successive deltas keep one
/// form; [`ViewDelta::between`] falls back to a full re-init if the shape ever
/// changes rather than panicking.
///
/// [`Init`]: ViewDelta::Init
/// [`Patch`]: ViewDelta::Patch
/// [`Scalar`]: ViewDelta::Scalar
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewDelta {
    /// The complete initial row set (no prior frontier, or a shape change) — the
    /// `init` payload of a row-stream view.
    Init(Vec<ViewRow>),
    /// An ordered §12.2 patch over a row-stream view: the operations that, applied
    /// in listed order to the prior frontier's result, yield this one EXACTLY —
    /// same occurrences, same exposed values, same order. The empty sequence is a
    /// frontier-only patch.
    Patch(Vec<PatchOp>),
    /// A scalar/aggregate view's value (§7.5, §12.2). `Some(value)` conveys the
    /// value — at first observation or when it changed — and the client sets its
    /// result to `value`; `None` is the frontier-only no-op for an unchanged
    /// scalar, mirroring an empty [`Patch`]. The carried [`Value`] MAY itself be
    /// [`Value::None`] (an absent optional, or `avg`/`min`/`max` over empty input,
    /// §7.5), a present scalar reading distinct from the `None` no-op.
    Scalar(Option<Value>),
}

impl ViewDelta {
    /// The delta from `prev` (the prior observation, or `None` for the first) to
    /// `next` (§12.2). Like shapes compare like-to-like:
    ///
    /// - two row streams yield the ordered §12.2 [`crate::patch::diff`] as a
    ///   [`Patch`] (or an [`Init`] on the first observation) — applying it in order
    ///   to `prev`'s rows reproduces `next`'s rows including order;
    /// - two scalars yield [`Scalar`] carrying the new value when it changed, or
    ///   the frontier-only [`Scalar`]`(None)` when it did not (mirroring the
    ///   empty-patch case), and the scalar value at the first observation.
    ///
    /// A view's result shape is fixed for its lifetime; a shape mismatch (or the
    /// first observation) takes the guarded fallback of a full re-init of `next` in
    /// its own shape, never a panic.
    ///
    /// [`Init`]: ViewDelta::Init
    /// [`Patch`]: ViewDelta::Patch
    /// [`Scalar`]: ViewDelta::Scalar
    #[must_use]
    pub fn between(prev: Option<&ViewResult>, next: &ViewResult) -> Self {
        match (prev, next) {
            // Same-shape row stream: the ordered §12.2 patch from prior to next.
            (Some(ViewResult::Rows { rows: prev_rows, .. }), ViewResult::Rows { rows, .. }) => {
                Self::Patch(crate::patch::diff(prev_rows, rows))
            }
            // Same-shape scalar: the new value when it changed, else the
            // frontier-only no-op (§7.5, §12.2), mirroring the empty-patch case.
            (Some(ViewResult::Scalar(prev_value)), ViewResult::Scalar(value)) => {
                if prev_value == value {
                    Self::Scalar(None)
                } else {
                    Self::Scalar(Some(value.clone()))
                }
            }
            // First observation, or a guarded shape change: a full re-init of
            // `next` in its own shape — never a panic.
            (_, ViewResult::Rows { rows, .. }) => Self::Init(rows.clone()),
            (_, ViewResult::Scalar(value)) => Self::Scalar(Some(value.clone())),
        }
    }

    /// The §12.2 delta between two ordered ROW SLICES — the primitive a bounded
    /// window (§12.2) uses to carry its prior client-visible window to the newly
    /// refreshed one.
    ///
    /// A window's client result is a `Vec<ViewRow>`, the bounded slice the client
    /// tracks, not a full [`ViewResult`]; §12.2 fixes the same init/patch contract
    /// on it, so every position is relative to the window and a row the window's
    /// shift pushed past its bound renders as a `remove`. `prev` is `None` at the
    /// first observation (the window's [`Init`], shipping its rows); otherwise the
    /// ordered [`crate::patch::diff`] carries `prev` to `next` EXACTLY. This mirrors
    /// [`ViewDelta::between`] but over the client-visible slice rather than the full
    /// authorized view — a window is always a row stream, so there is no scalar
    /// case — keeping a windowed subscription §12.2-coherent against its own window
    /// instead of the whole view.
    ///
    /// [`Init`]: ViewDelta::Init
    #[must_use]
    pub fn between_rows(prev: Option<&[ViewRow]>, next: &[ViewRow]) -> Self {
        match prev {
            Some(prev) => Self::Patch(crate::patch::diff(prev, next)),
            None => Self::Init(next.to_vec()),
        }
    }
}

fn view_row(row: &Row) -> Result<ViewRow, EngineError> {
    let fields = row
        .cells()
        .filter_map(|(name, cell)| {
            cell_field_value(cell).map(|opt| opt.map(|value| (name.clone(), value))).transpose()
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(ViewRow { id: row.id().clone(), fields, sort_tuple: row.sort().to_vec() })
}

/// The exposed value of one projected output cell, or `None` when it is omitted.
///
/// A `none` optional is an *absent* optional whose field-position wire form is an
/// omitted member (§A.9 / Annex A wire table, "omitted optional field"), distinct
/// from a present JSON `null` (`Value::Json(Json::Null)`).
///
/// A nested cell is a REQUIRED output of the row (§7.1: "projection members are
/// unordered named outputs"; §8: a response value MAY be a nested collection), so
/// it is carried inline in the row's INITIAL materialized value, recursing to any
/// depth (Annex C.7):
///
/// - a nested single row — a §5.3 keyless static struct, or a keyed single
///   sub-view row (§7.1/§8) — as a nested object [`Value::Struct`];
/// - a nested sub-collection view member (`kids: .children { … }`) as the ordered
///   array of its projected row objects, in canonical B.5 order ([`Value::Json`]).
///
/// The live §12.2 patch stream over such a sub-view is a SEPARATE concern
/// ([`crate::patch::diff`]); it does not license dropping the member from the
/// one-shot materialized value a `view`/`view_at_head` read returns.
fn cell_field_value(cell: &Cell) -> Result<Option<Value>, EngineError> {
    Ok(match cell {
        Cell::Scalar(Value::None) => None,
        Cell::Scalar(value) => Some(value.clone()),
        Cell::Row(row) => Some(struct_value(row)?),
        Cell::Collection(rows) => Some(collection_value(rows)?),
    })
}

/// A projected row as a canonical [`Value::Struct`]: its member cells in
/// declaration order, each recursively converted, with an absent (`none`) member
/// omitted — the nested-object wire form (§5.3 for a keyless static struct; a
/// keyed single sub-view row, §7.1/§8, shares the same object shape).
fn struct_value(row: &Row) -> Result<Value, EngineError> {
    let members = row
        .cells()
        .filter_map(|(name, cell)| {
            cell_field_value(cell).map(|opt| opt.map(|value| (Text::new(name.clone()), value))).transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Struct(Struct::new(members)))
}

/// A projected sub-collection view member as an inline [`Value::Json`] array of
/// its row objects (§7.1/§8), in the collection's canonical B.5 row order — each
/// row recursively shaped by its own projection through [`struct_value`]. Routed
/// through the canonical json codec exactly as [`crate::recursion`] materializes a
/// nested tree; the codec's json-number bound is total on already-canonical values
/// and any residual malformation surfaces as an engine invariant break, never a
/// dropped member or a panic.
fn collection_value(rows: &[Row]) -> Result<Value, EngineError> {
    let objects = rows
        .iter()
        .map(|row| struct_value(row).map(|value| value.to_wire()))
        .collect::<Result<Vec<_>, _>>()?;
    let json = Json::from_wire(&J::Array(objects)).map_err(|error| EngineError::Internal(error.to_string()))?;
    Ok(Value::Json(json))
}
