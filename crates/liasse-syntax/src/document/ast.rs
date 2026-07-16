//! The spanned document tree (SPEC.md §2.5, §4.2, Annex C.1).
//!
//! A [`SpannedDocument`] is the shared shape produced by both the Hjson
//! authoring form and the strict `liasse.json` built form. It is pure syntax:
//! member values are raw scalars, objects, and arrays with spans. No member is
//! interpreted — whether a string is an expression, a package name, or a type
//! is the model layer's job. A value of these types exists only as the output
//! of [`crate::parse_document`], so an unparsed document is unrepresentable.

use liasse_diag::ByteSpan;

/// A parsed document: one root value.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedDocument {
    /// The root value (an authoring definition is an object, but the parser
    /// accepts any JSON value at the root).
    pub root: DocValue,
}

impl SpannedDocument {
    /// The root value.
    #[must_use]
    pub fn root(&self) -> &DocValue {
        &self.root
    }
}

/// A spanned document value.
#[derive(Debug, Clone, PartialEq)]
pub struct DocValue {
    /// The bytes this value covers.
    pub span: ByteSpan,
    /// The value form.
    pub kind: DocValueKind,
}

/// The document value forms.
#[derive(Debug, Clone, PartialEq)]
pub enum DocValueKind {
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number, kept as its raw source text so decimal scale and integer
    /// spelling survive untouched for the value layer.
    Number(String),
    /// A string, with escapes decoded and `'''` blocks de-indented.
    String(String),
    /// An array, in element order.
    Array(Vec<DocValue>),
    /// An object, in member order (order carries no package semantics per
    /// Annex C.1, but is preserved for diagnostics and round-tripping).
    Object(Vec<DocMember>),
}

/// One object member: a name and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct DocMember {
    /// The bytes covering the whole `name: value` member.
    pub span: ByteSpan,
    /// The member name.
    pub name: DocName,
    /// The member value.
    pub value: DocValue,
}

/// A member name — quoted or unquoted, decoded to its logical text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocName {
    /// The bytes of the name token.
    pub span: ByteSpan,
    /// The decoded name text.
    pub text: String,
}
