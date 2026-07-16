//! Ergonomic, span-preserving access over the [`liasse_syntax`] document tree.
//!
//! The builder reads a [`DocValue`] repeatedly — "is this an object", "find the
//! `$key` member", "read this as a string". Rather than scatter `match`
//! ladders, the [`DocValueExt`] trait exposes those questions as methods that
//! keep the spanned member around, so every rejection can still point at the
//! exact offending bytes.

use liasse_syntax::{DocMember, DocValue, DocValueKind};

/// Span-preserving accessors over a spanned document value.
pub(crate) trait DocValueExt {
    /// The members if this value is an object, else `None`.
    fn as_object(&self) -> Option<&[DocMember]>;
    /// The decoded text if this value is a string, else `None`.
    fn as_string(&self) -> Option<&str>;
    /// The elements if this value is an array, else `None`.
    fn as_array(&self) -> Option<&[DocValue]>;
    /// The boolean if this value is a bool, else `None`.
    fn as_bool(&self) -> Option<bool>;
    /// The raw number text if this value is a number, else `None`.
    fn as_number(&self) -> Option<&str>;
    /// The first member named `name`, if this value is an object.
    fn member(&self, name: &str) -> Option<&DocMember>;
    /// A human-readable name of this value's form, for diagnostics.
    fn kind_name(&self) -> &'static str;
}

impl DocValueExt for DocValue {
    fn as_object(&self) -> Option<&[DocMember]> {
        match &self.kind {
            DocValueKind::Object(members) => Some(members),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<&str> {
        match &self.kind {
            DocValueKind::String(text) => Some(text),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[DocValue]> {
        match &self.kind {
            DocValueKind::Array(items) => Some(items),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match &self.kind {
            DocValueKind::Bool(value) => Some(*value),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<&str> {
        match &self.kind {
            DocValueKind::Number(text) => Some(text),
            _ => None,
        }
    }

    fn member(&self, name: &str) -> Option<&DocMember> {
        self.as_object()?.iter().find(|m| m.name.text == name)
    }

    fn kind_name(&self) -> &'static str {
        match &self.kind {
            DocValueKind::Null => "null",
            DocValueKind::Bool(_) => "a boolean",
            DocValueKind::Number(_) => "a number",
            DocValueKind::String(_) => "a string",
            DocValueKind::Array(_) => "an array",
            DocValueKind::Object(_) => "an object",
        }
    }
}
