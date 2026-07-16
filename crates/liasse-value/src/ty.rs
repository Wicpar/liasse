//! The Liasse type model (Annex A) used to parse wire values into [`Value`]s.
//!
//! [`Type::decode`] (in `decode.rs`) turns raw strict-JSON into a [`Value`]
//! proven to conform to the type, so downstream code never re-validates.

use std::collections::BTreeMap;

use crate::enumeration::EnumType;
use crate::temporal::Precision;

/// The declared key type a `ref<T>` points at (A.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    /// A scalar key: a single wire value.
    Scalar(Box<Type>),
    /// A composite key: an array of component wire values in `$key` order.
    Composite(Vec<Type>),
}

/// A static struct type: named fields (A.3). A field declared optional carries
/// a [`Type::Optional`] type; on decode an absent optional field becomes
/// `Value::None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructType {
    fields: BTreeMap<String, Type>,
}

impl StructType {
    /// Build from field declarations.
    #[must_use]
    pub fn new(fields: impl IntoIterator<Item = (String, Type)>) -> Self {
        Self {
            fields: fields.into_iter().collect(),
        }
    }

    /// The declared fields in field-name text order.
    pub fn fields(&self) -> impl Iterator<Item = (&String, &Type)> {
        self.fields.iter()
    }

    /// Look up a declared field type.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&Type> {
        self.fields.get(name)
    }
}

/// A Liasse type (Annex A / A.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Text,
    Bool,
    Int,
    Decimal,
    Bytes,
    Uuid,
    Date,
    Timestamp(Precision),
    Duration,
    Period,
    Json,
    Blob,
    Enum(EnumType),
    Optional(Box<Type>),
    Set(Box<Type>),
    Map(Box<Type>, Box<Type>),
    View(Box<Type>),
    Ref(RefTarget),
    Struct(StructType),
}

impl Type {
    /// A `timestamp` at the package-default precision (A.5).
    #[must_use]
    pub fn timestamp() -> Self {
        Self::Timestamp(Precision::DEFAULT)
    }

    /// The type name used in diagnostics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Decimal => "decimal",
            Self::Bytes => "bytes",
            Self::Uuid => "uuid",
            Self::Date => "date",
            Self::Timestamp(_) => "timestamp",
            Self::Duration => "duration",
            Self::Period => "period",
            Self::Json => "json",
            Self::Blob => "blob",
            Self::Enum(_) => "enum",
            Self::Optional(_) => "optional",
            Self::Set(_) => "set",
            Self::Map(_, _) => "map",
            Self::View(_) => "view",
            Self::Ref(_) => "ref",
            Self::Struct(_) => "struct",
        }
    }

    /// Whether this type may serve as a collection key component (A.8).
    ///
    /// A.8 (SPEC.md lines 4468–4473) enumerates the key-eligible types
    /// exhaustively — `text, bool, int, decimal, bytes, uuid, date, timestamp,
    /// duration, enum`, and structs composed solely of key-eligible required
    /// fields — and line 4475 excludes optionals, JSON, blobs, sets, maps, and
    /// views. `ref` and `period` appear in neither list; the enumerated set is a
    /// closed "MAY use" list, so a `ref` field is **not** itself a key
    /// component. §5.6 gives a ref a *target* key type but never adds `ref` to
    /// the eligible base types, so we follow the strict enumeration.
    #[must_use]
    pub fn is_key_eligible(&self) -> bool {
        match self {
            Self::Text
            | Self::Bool
            | Self::Int
            | Self::Decimal
            | Self::Bytes
            | Self::Uuid
            | Self::Date
            | Self::Timestamp(_)
            | Self::Duration
            | Self::Enum(_) => true,
            Self::Struct(fields) => fields.fields().all(|(_, ty)| ty.is_key_eligible()),
            Self::Ref(_)
            | Self::Period
            | Self::Json
            | Self::Blob
            | Self::Optional(_)
            | Self::Set(_)
            | Self::Map(_, _)
            | Self::View(_) => false,
        }
    }
}
