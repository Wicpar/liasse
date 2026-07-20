//! The static type of an expression result.
//!
//! Scalar and structured results reuse [`liasse_value::Type`] verbatim (Annex
//! A). Rows and row streams are not one `Type` — a view carries per-field types
//! *and* an identity — so [`ExprType`] adds [`ExprType::Row`] and
//! [`ExprType::View`] over a [`RowType`]. The type checker produces an
//! `ExprType` for every node; a well-typed [`TypedExpr`](crate::TypedExpr) is
//! proof one exists.

use std::collections::BTreeMap;

use liasse_value::Type;

/// The static type of an expression's result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub enum ExprType {
    /// A scalar or structured value of a canonical [`Type`] (includes
    /// `optional<T>`, `set<T>`, `map<K,V>`, `ref<T>`, and static structs).
    Scalar(#[cfg_attr(feature = "eval-wire", serde(with = "crate::wire::type_serde"))] Type),
    /// Exactly one row of a collection or view.
    Row(RowType),
    /// A stream of rows (a view / selection result).
    View(RowType),
}

impl ExprType {
    /// A scalar result.
    #[must_use]
    pub fn scalar(ty: Type) -> Self {
        Self::Scalar(ty)
    }

    /// The scalar type, if this is a scalar result.
    #[must_use]
    pub fn as_scalar(&self) -> Option<&Type> {
        match self {
            Self::Scalar(ty) => Some(ty),
            _ => None,
        }
    }

    /// The row type, if this is a single-row result.
    #[must_use]
    pub fn as_row(&self) -> Option<&RowType> {
        match self {
            Self::Row(row) => Some(row),
            _ => None,
        }
    }

    /// The row type of a view, if this is a stream result.
    #[must_use]
    pub fn as_view(&self) -> Option<&RowType> {
        match self {
            Self::View(row) => Some(row),
            _ => None,
        }
    }

    /// A short human-readable form for diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Scalar(ty) => ty.name().to_owned(),
            Self::Row(_) => "row".to_owned(),
            Self::View(_) => "view".to_owned(),
        }
    }
}

/// The shape of a row: its visible fields (name → type) and, when the row is
/// keyed, the type of its identity key.
///
/// A bucketed (temporal) row additionally carries *structural bindings*
/// (`$source`/`$from`/`$until`/`$index`, §14.4) — names a projection over the row
/// may read that are not ordinary fields — and a temporal `unbounded` marker
/// (§14.5): a recurring source-backed bucket whose series may run forever must be
/// read through a bounded temporal selector, never enumerated whole. Neither the
/// structural bindings nor the marker participates in row *identity*: two rows
/// with the same visible fields and key denote the same shape (§12.4/Annex E view
/// identity), so [`PartialEq`] compares only `fields` and `key`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "eval-wire", derive(serde::Serialize, serde::Deserialize))]
pub struct RowType {
    fields: BTreeMap<String, ExprType>,
    key: Option<Box<ExprType>>,
    /// Structural bindings a projection over this row may read (§14.4). Empty for
    /// an ordinary (non-bucketed) row.
    structural: BTreeMap<String, ExprType>,
    /// Whether reading this row stream whole would enumerate an unbounded series
    /// (§14.5). Only a recurring source-backed bucket with a possibly-unbounded
    /// upper bound sets this; a bounded temporal selector clears it.
    unbounded: bool,
    /// The target relation these rows are natively identified by (§6.3/§7.4): the
    /// absolute path of the backing keyed collection (`/tasks`). A `$view`, a
    /// synthetic-`$key` projection, a struct row, or any re-identified stream
    /// leaves this `None` — it does not name one relation. Two view operands of a
    /// `|`/`&` combinator that name DIFFERENT relations do not share an identity
    /// domain (§6.3 "values belonging to different target relations are statically
    /// incomparable"). Like `structural`/`unbounded`, it never participates in row
    /// identity, so [`PartialEq`] ignores it.
    relation: Option<String>,
}

impl PartialEq for RowType {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields && self.key == other.key
    }
}

impl Eq for RowType {}

impl RowType {
    /// A row with the given fields and optional key identity type.
    #[must_use]
    pub fn new(
        fields: impl IntoIterator<Item = (String, ExprType)>,
        key: Option<ExprType>,
    ) -> Self {
        Self {
            fields: fields.into_iter().collect(),
            key: key.map(Box::new),
            structural: BTreeMap::new(),
            unbounded: false,
            relation: None,
        }
    }

    /// A keyless row (a static struct / projected group).
    #[must_use]
    pub fn keyless(fields: impl IntoIterator<Item = (String, ExprType)>) -> Self {
        Self::new(fields, None)
    }

    /// Attach the structural bindings a projection over this bucketed row may read
    /// (`$source`/`$from`/`$until`/`$index`, §14.4).
    #[must_use]
    pub fn with_structural(
        mut self,
        structural: impl IntoIterator<Item = (String, ExprType)>,
    ) -> Self {
        self.structural = structural.into_iter().collect();
        self
    }

    /// Mark this row stream as an unbounded recurring bucket (§14.5): reading it
    /// whole enumerates a possibly-infinite series and is rejected; a bounded
    /// temporal selector (`.$at`/`.$between`) must gate it.
    #[must_use]
    pub fn unbounded(mut self, unbounded: bool) -> Self {
        self.unbounded = unbounded;
        self
    }

    /// Tag these rows with the target relation that natively identifies them
    /// (§6.3/§7.4): the absolute path of the backing keyed collection. A view or a
    /// re-identified stream passes `None`.
    #[must_use]
    pub fn with_relation(mut self, relation: Option<String>) -> Self {
        self.relation = relation;
        self
    }

    /// The type of a visible field.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&ExprType> {
        self.fields.get(name)
    }

    /// The visible fields in canonical field-name order.
    pub fn fields(&self) -> impl Iterator<Item = (&String, &ExprType)> {
        self.fields.iter()
    }

    /// The type of a structural binding `$name` this row exposes (§14.4).
    #[must_use]
    pub fn structural(&self, name: &str) -> Option<&ExprType> {
        self.structural.get(name)
    }

    /// Whether reading this row stream whole would enumerate an unbounded series
    /// (§14.5).
    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        self.unbounded
    }

    /// The identity key type, if the row is keyed.
    #[must_use]
    pub fn key(&self) -> Option<&ExprType> {
        self.key.as_deref()
    }

    /// The target relation these rows are natively identified by (§6.3/§7.4), if
    /// they name one — the absolute path of the backing keyed collection. `None`
    /// for a view, a synthetic-`$key` projection, or any re-identified stream.
    #[must_use]
    pub fn relation(&self) -> Option<&str> {
        self.relation.as_deref()
    }
}
