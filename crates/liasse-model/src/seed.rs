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

/// §4.1: an address supplied by BOTH `$seed` (or its `$data` alias) and `$bundle`
/// is a static load error — the two carry distinct ownership, so a value must be
/// owned by exactly one. This checks the observable top-level addresses: a shared
/// singleton field name, or a shared row key within a shared collection. Deeper
/// nested-collection and set-member overlap is a documented seam no address in CORE
/// reaches (§13.13), so it is not walked here.
pub(crate) fn check_seed_bundle_disjoint(reporter: &mut Reporter, seed: Option<&DocValue>, bundle: &DocValue) {
    let (Some(seed_members), Some(bundle_members)) =
        (seed.and_then(DocValueExt::as_object), bundle.as_object())
    else {
        return;
    };
    for bundle_member in bundle_members {
        let Some(seed_member) = seed_members.iter().find(|m| m.name.text == bundle_member.name.text) else {
            continue;
        };
        // A shared top-level member: for a collection or struct the overlap is at
        // the row key / struct field (an inner-member name present in both); a shared
        // scalar member is a whole-address overlap. Comparing inner object members
        // covers both a collection's keyed rows and a struct's fields.
        match (seed_member.value.as_object(), bundle_member.value.as_object()) {
            (Some(seed_inner), Some(bundle_inner)) => {
                for inner in bundle_inner {
                    if seed_inner.iter().any(|s| s.name.text == inner.name.text) {
                        reject_shared(reporter, inner.span, &format!("{}.{}", bundle_member.name.text, inner.name.text));
                    }
                }
            }
            _ => reject_shared(reporter, bundle_member.span, &bundle_member.name.text),
        }
    }
}

fn reject_shared(reporter: &mut Reporter, span: liasse_diag::ByteSpan, address: &str) {
    reporter.reject_hint(
        span,
        code::SEED,
        format!("`{address}` is supplied by both `$seed`/`$data` and `$bundle` (§4.1)"),
        "an address is owned by exactly one of apply-if-absent `$seed` and package-authoritative `$bundle`",
    );
}

fn check_node(reporter: &mut Reporter, node: &Node, value: &DocValue) {
    if let Some(body) = expression_body(value) {
        // §4.2/C.4 (SPEC-ISSUES 25): a `= expr` seed is evaluated, not decoded —
        // but the bare `=` marker with an EMPTY body is a malformed expression, a
        // static load error rather than a silent empty/literal value. A literal
        // `=` is written `'=` (§4.2). The non-empty body is left to evaluation.
        if body.trim().is_empty() {
            reporter.reject_hint(
                value.span,
                code::SEED,
                "a `$data` value of `=` alone is the expression marker with an empty body; the \
                 expression after `=` must be non-empty (§4.2, Annex C.4)",
                "write `= <expression>`, or `'=` to store the literal text `=`",
            );
        }
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

/// The expression body of a `= expr` seed value (§4.2), or `None` for a literal
/// seed. `Some("")` (equivalently, an all-whitespace body) is the degenerate
/// bare-`=` marker with no expression after it (SPEC-ISSUES 25).
fn expression_body(value: &DocValue) -> Option<&str> {
    value.as_string().and_then(|text| text.trim_start().strip_prefix('='))
}

/// Convert a spanned document value to a strict-JSON wire value for decoding.
/// Shared with the field-default builder, whose literal `$default` (§4.2/§C.4) is
/// decoded against the field type through the same wire form as a `$data` seed.
pub(crate) fn to_json(value: &DocValue) -> J {
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
