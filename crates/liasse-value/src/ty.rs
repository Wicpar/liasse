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
    /// A composite key: component `(name, type)` pairs in `$key` order. The wire
    /// value is the array of component wire values in that order; a named object
    /// selector `{ name: … }` is accepted as authoring syntax for the same tuple
    /// and normalized to `$key` order on decode (A.9).
    Composite(Vec<(String, Type)>),
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

/// A package reference refining a `module` type (SPEC §13.16): the package name
/// and the major version it must be compatible with. Compatibility is major-only
/// (§13.14/§20.3): a caret spelling such as `t.accounting@^1.2` refines to
/// `major = 1`, admitting any `1.x` instance of that package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePackageRef {
    name: String,
    major: u64,
}

impl ModulePackageRef {
    /// A reference to `name` at compatibility major `major`.
    #[must_use]
    pub fn new(name: impl Into<String>, major: u64) -> Self {
        Self {
            name: name.into(),
            major,
        }
    }

    /// The referenced package name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The compatibility major version.
    #[must_use]
    pub fn major(&self) -> u64 {
        self.major
    }
}

/// How tightly a `module` value type constrains the instance it admits
/// (SPEC §13.16). A module value's type is its definition; this is the refinement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleType {
    /// Bare `module` — admits any installed instance.
    Any,
    /// `module` refined by a package reference — admits an instance of that
    /// package at a compatible (major-only) version (§13.14/§20.3).
    Package(ModulePackageRef),
    /// `module` refined by a structural interface name (§13.8) — admits any
    /// instance whose definition exposes that interface.
    Interface(String),
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
    /// A composite key type (A.9, §5.4): component `(name, type)` pairs in `$key`
    /// order. Distinct from [`Type::Struct`], which is field-name-ordered: a
    /// composite key preserves its declared `$key` order so its value (a
    /// [`Value::Composite`](crate::Value::Composite)) orders and normalizes
    /// positionally. It is the type of a collection's composite primary key.
    Composite(Vec<(String, Type)>),
    /// A `module` value (SPEC §13.16): a move-only, unique owning handle to an
    /// installed module instance, typed by its definition (bare, package-refined,
    /// or interface-refined). It is the only move-only value type
    /// ([`is_copyable`](Type::is_copyable) `== false`); a struct, set, map,
    /// optional, or view carrying one classifies move-only by delegation.
    Module(ModuleType),
}

impl RefTarget {
    /// The ref target for a collection's key type (A.9): a composite key type
    /// becomes a composite target (its components positional, in `$key` order);
    /// any scalar key becomes a scalar target.
    #[must_use]
    pub fn for_key(key_type: &Type) -> Self {
        match key_type {
            Type::Composite(components) => Self::Composite(components.clone()),
            other => Self::Scalar(Box::new(other.clone())),
        }
    }
}

impl Type {
    /// A `timestamp` at the package-default precision (A.5).
    #[must_use]
    pub fn timestamp() -> Self {
        Self::Timestamp(Precision::DEFAULT)
    }

    /// The `(name, type)` components of a composite key type in `$key` order, or
    /// `None` when this is not a composite key.
    #[must_use]
    pub fn composite_components(&self) -> Option<&[(String, Type)]> {
        match self {
            Self::Composite(components) => Some(components),
            _ => None,
        }
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
            Self::Composite(_) => "composite key",
            Self::Module(_) => "module",
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
            Self::Composite(components) => components.iter().all(|(_, ty)| ty.is_key_eligible()),
            Self::Ref(_)
            | Self::Period
            | Self::Json
            | Self::Blob
            | Self::Optional(_)
            | Self::Set(_)
            | Self::Map(_, _)
            | Self::View(_)
            | Self::Module(_) => false,
        }
    }

    /// SPEC §8.5 copyability: whether `=` may COPY a value of this type. A copyable
    /// value duplicates freely; a **move-only** (affine) value never duplicates and
    /// is only transferred with the move operator `<-`/`->`.
    ///
    /// Every value type Liasse has today is copyable — scalars, immutable content
    /// and identity references (`blob`, `ref`), enums, and structs, sets, maps,
    /// optionals, and views built from copyable values. Compound types delegate to
    /// their components, so this method is the single classification hook a
    /// forthcoming move-only type opts into: a `module` value (a later task) owns
    /// live mutable state, so when its `Type` variant is added its arm here returns
    /// `false`, and any struct, set, map, optional, or view carrying one then
    /// reports move-only automatically. The scalar arms are enumerated (no
    /// wildcard) precisely so that adding that variant is a compile error until its
    /// copyability is decided here.
    #[must_use]
    pub fn is_copyable(&self) -> bool {
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
            | Self::Period
            | Self::Json
            | Self::Blob
            | Self::Enum(_)
            | Self::Ref(_) => true,
            Self::Optional(inner) | Self::Set(inner) | Self::View(inner) => inner.is_copyable(),
            Self::Map(key, value) => key.is_copyable() && value.is_copyable(),
            Self::Struct(fields) => fields.fields().all(|(_, ty)| ty.is_copyable()),
            Self::Composite(components) => components.iter().all(|(_, ty)| ty.is_copyable()),
            // A `module` value owns live mutable state with exactly one owner: it is
            // move-only (SPEC §8.5/§13.16), never copied by `=`. This is the sole
            // `false` arm; every container of a module classifies move-only above by
            // component delegation.
            Self::Module(_) => false,
        }
    }
}
