//! The spanned type-expression AST (SPEC.md Annex A.2).
//!
//! [`parse_type_expression`](super::parse_type_expression) produces a
//! [`SpannedType`]: the A.2 shape of a declared type with every node spanned, but
//! no commitment to meaning. Whether `text` is a primitive, `company` a `$types`
//! reference, or `ref<...>` a deferred seam is decided by the model layer, which
//! maps this tree to a `liasse_value::Type`.

use liasse_diag::ByteSpan;

/// One parsed A.2 type expression, spanned at its outermost extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpannedType {
    /// The bytes this type expression covers.
    pub span: ByteSpan,
    /// The A.2 form.
    pub kind: TypeExprKind,
}

/// The A.2 type-expression forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExprKind {
    /// A bare, possibly dotted name: a primitive keyword (`text`, `json`, …) or a
    /// `$types` reference (`company`, `accounting.money`). The model resolves it.
    Name(String),
    /// A postfix `T?` optional (A.2 shorthand for `optional<T>`). Kept distinct
    /// from [`Optional`](TypeExprKind::Optional) so the model can reject a
    /// redundant `optional<T>?`.
    OptionalSuffix(Box<SpannedType>),
    /// The generic `optional<T>` form.
    Optional(Box<SpannedType>),
    /// `set<T>`.
    Set(Box<SpannedType>),
    /// `view<T>`.
    View(Box<SpannedType>),
    /// `map<K, V>`.
    Map(Box<SpannedType>, Box<SpannedType>),
    /// `ref<target>`, carrying the raw target-path text (the model defers the
    /// string `ref` form to the object `{ "$ref": ... }` form).
    Ref { target: String },
    /// An A.2 key-path reference — `collection.$key`, `/absolute.col.$key`, or
    /// `#surface.$key` — carrying the raw path text.
    KeyPath(String),
    /// A static-struct type `{ field: T, optional_field?: U }`.
    Struct(Vec<TypeField>),
}

/// One field of a struct-type literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeField {
    /// The field name.
    pub name: String,
    /// The span of the field name.
    pub name_span: ByteSpan,
    /// Whether the field was declared optional with a `?` suffix.
    pub optional: bool,
    /// The field's declared type.
    pub ty: SpannedType,
    /// The bytes the whole field declaration covers.
    pub span: ByteSpan,
}
