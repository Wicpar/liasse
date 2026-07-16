//! Minimal navigation over the spanned definition document.
//!
//! The validated [`Model`](liasse_model::Model) keeps a mutation's parameter
//! contract but not its statement program, and keeps no `$data` seed at all, so
//! the runtime reads both straight from the document it already parsed. The
//! model's own document extension trait is private, so this is the small subset
//! the runtime needs: member lookup and scalar/array/object shape access.

use liasse_syntax::{DocMember, DocValue, DocValueKind};

/// A member of `value` named `name`, if `value` is an object holding one.
pub(crate) fn member<'a>(value: &'a DocValue, name: &str) -> Option<&'a DocValue> {
    object(value)?.iter().find(|m| m.name.text == name).map(|m| &m.value)
}

/// The members of `value`, if it is an object.
pub(crate) fn object(value: &DocValue) -> Option<&[DocMember]> {
    match &value.kind {
        DocValueKind::Object(members) => Some(members),
        _ => None,
    }
}

/// The string text of `value`, if it is a string scalar.
pub(crate) fn string(value: &DocValue) -> Option<&str> {
    match &value.kind {
        DocValueKind::String(text) => Some(text),
        _ => None,
    }
}

/// The boolean of `value`, if it is a boolean scalar.
pub(crate) fn bool_value(value: &DocValue) -> Option<bool> {
    match &value.kind {
        DocValueKind::Bool(b) => Some(*b),
        _ => None,
    }
}

/// The elements of `value`, if it is an array.
pub(crate) fn array(value: &DocValue) -> Option<&[DocValue]> {
    match &value.kind {
        DocValueKind::Array(items) => Some(items),
        _ => None,
    }
}

/// The shape object at a declared receiver `path` from `$model` (each segment a
/// collection or struct member), if it resolves to an object.
pub(crate) fn shape_at<'a>(model: &'a DocValue, path: &[String]) -> Option<&'a DocValue> {
    let mut current = model;
    for segment in path {
        current = member(current, segment)?;
    }
    object(current).map(|_| current)
}

/// Convert a spanned document value into a strict-JSON wire value, so a seed row
/// can be decoded against its declared type via `liasse_value::Type::decode`.
/// Numbers keep their exact source text through `serde_json`'s arbitrary
/// precision, matching how the value layer decodes `int`/`decimal`.
pub(crate) fn to_json(value: &DocValue) -> serde_json::Value {
    use serde_json::Value as J;
    match &value.kind {
        DocValueKind::Null => J::Null,
        DocValueKind::Bool(b) => J::Bool(*b),
        DocValueKind::Number(text) => {
            serde_json::from_str::<serde_json::Number>(text).map_or(J::Null, J::Number)
        }
        DocValueKind::String(text) => J::String(text.clone()),
        DocValueKind::Array(items) => J::Array(items.iter().map(to_json).collect()),
        DocValueKind::Object(members) => {
            J::Object(members.iter().map(|m| (m.name.text.clone(), to_json(&m.value))).collect())
        }
    }
}
