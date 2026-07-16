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
pub enum ExprType {
    /// A scalar or structured value of a canonical [`Type`] (includes
    /// `optional<T>`, `set<T>`, `map<K,V>`, `ref<T>`, and static structs).
    Scalar(Type),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowType {
    fields: BTreeMap<String, ExprType>,
    key: Option<Box<ExprType>>,
}

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
        }
    }

    /// A keyless row (a static struct / projected group).
    #[must_use]
    pub fn keyless(fields: impl IntoIterator<Item = (String, ExprType)>) -> Self {
        Self::new(fields, None)
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

    /// The identity key type, if the row is keyed.
    #[must_use]
    pub fn key(&self) -> Option<&ExprType> {
        self.key.as_deref()
    }
}
