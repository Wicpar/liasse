//! Phase 5: seed data typing (SPEC.md §5, §9 static rules).
//!
//! `$data` supplies genesis values. This phase checks each seed value against
//! the type of the model declaration it names, through [`liasse_value`]'s
//! decoder — a successful decode is proof the seed conforms. `=`-prefixed seeds
//! are expressions (§4.2), not literals, and are left to expression evaluation.
//!
//! CORE scope: scalar, struct, set, and one level of keyed-collection row
//! fields are decoded; deeper nesting and cross-row default interactions are a
//! documented seam.

use serde_json::Value as J;

use liasse_syntax::{DocValue, DocValueKind};
use liasse_value::Type;

use crate::doc::DocValueExt;
use crate::report::{code, Reporter};
use crate::state::{Node, Shape};

/// Type-check the `$data` seed against the model tree.
pub(crate) fn check_seed(reporter: &mut Reporter, root: &Shape, data: &DocValue) {
    let Some(members) = data.as_object() else {
        reporter.reject(data.span, code::SEED, "`$data` must be an object of seed values");
        return;
    };
    for member in members {
        match root.member(&member.name.text) {
            Some(target) => check_node(reporter, &target.node, &member.value),
            None => reporter.reject_hint(
                member.span,
                code::SEED,
                format!("`$data` seeds `{}`, which is not a declared model member", member.name.text),
                "seed only declared fields and collections",
            ),
        }
    }
}

fn check_node(reporter: &mut Reporter, node: &Node, value: &DocValue) {
    if is_expression(value) {
        return; // `= expr` seeds are evaluated, not decoded.
    }
    match node {
        // §9.1: computed values and views mirror non-writable state and cannot
        // be seeded directly; naming one in `$data` is a definition-level misuse.
        Node::Scalar(field) if field.computed.is_some() => reporter.reject_hint(
            value.span,
            code::SEED,
            "`$data` supplies a computed value, which cannot be seeded directly (§9.1)",
            "a computed value is determined by its expression; remove it from `$data`",
        ),
        Node::View(_) => reporter.reject_hint(
            value.span,
            code::SEED,
            "`$data` supplies a view, which cannot be seeded directly (§9.1)",
            "a view is derived, not stored; remove it from `$data`",
        ),
        Node::Scalar(field) => decode_scalar(reporter, &field.ty, value),
        Node::Reference(reference) => decode_scalar(reporter, &reference.key_type, value),
        Node::Set(set) => check_set(reporter, &set.element, value),
        Node::Struct(shape) => check_struct(reporter, shape, value),
        Node::Collection(collection) => check_collection(reporter, &collection.shape, value),
        Node::Named(_) => {}
    }
}

fn decode_scalar(reporter: &mut Reporter, ty: &Type, value: &DocValue) {
    let wire = to_json(value);
    if let Err(error) = ty.decode(&wire) {
        reporter.reject_hint(
            value.span,
            code::SEED,
            format!("seed value does not conform to `{}`: {error}", ty.name()),
            "provide a value of the declared type",
        );
    }
}

fn check_set(reporter: &mut Reporter, element: &Type, value: &DocValue) {
    let Some(items) = value.as_array() else {
        reporter.reject(value.span, code::SEED, "a set seed must be an array of members");
        return;
    };
    for item in items {
        decode_scalar(reporter, element, item);
    }
}

fn check_struct(reporter: &mut Reporter, shape: &Shape, value: &DocValue) {
    let Some(members) = value.as_object() else {
        reporter.reject(value.span, code::SEED, "a struct seed must be an object");
        return;
    };
    for member in members {
        match shape.member(&member.name.text) {
            Some(target) => check_node(reporter, &target.node, &member.value),
            None => reporter.reject(
                member.span,
                code::SEED,
                format!("seed field `{}` is not declared on the struct", member.name.text),
            ),
        }
    }
}

fn check_collection(reporter: &mut Reporter, shape: &Shape, value: &DocValue) {
    let Some(rows) = value.as_object() else {
        reporter.reject_hint(
            value.span,
            code::SEED,
            "a collection seed is a map of key text to row values",
            "e.g. `\"acme\": { \"name\": \"Acme\" }`",
        );
        return;
    };
    for row in rows {
        check_struct(reporter, shape, &row.value);
    }
}

/// Whether a string seed value is a `= expr` expression (§4.2).
fn is_expression(value: &DocValue) -> bool {
    value
        .as_string()
        .is_some_and(|text| text.trim_start().starts_with('='))
}

/// Convert a spanned document value to a strict-JSON wire value for decoding.
fn to_json(value: &DocValue) -> J {
    match &value.kind {
        DocValueKind::Null => J::Null,
        DocValueKind::Bool(b) => J::Bool(*b),
        DocValueKind::Number(text) => {
            serde_json::from_str::<J>(text).unwrap_or(J::String(text.clone()))
        }
        DocValueKind::String(text) => J::String(text.clone()),
        DocValueKind::Array(items) => J::Array(items.iter().map(to_json).collect()),
        DocValueKind::Object(members) => J::Object(
            members
                .iter()
                .map(|m| (m.name.text.clone(), to_json(&m.value)))
                .collect(),
        ),
    }
}
