//! Reference resolution (SPEC.md §5.6, A.9).
//!
//! Every `$ref` names a target collection that MUST exist; the ref's visible
//! value type is that collection's key type. This pass indexes the model's
//! collections, then fills each [`Reference`]'s key type or rejects an
//! unresolvable target. Building the index first and mutating references second
//! keeps the tree free of aliasing.
//!
//! CORE scope: targets resolve against collections reachable by an absolute
//! `/segment/segment` path of collection names; a ref to a keyed *view* (§7.6)
//! is a documented seam.

use std::collections::BTreeMap;

use liasse_value::Type;

use crate::report::{code, Reporter};
use crate::state::{Collection, Node, Reference, Shape};

/// The primary-key type of every collection, keyed by its absolute path.
struct Index {
    keys: BTreeMap<String, Type>,
}

impl Index {
    fn build(root: &Shape) -> Self {
        let mut keys = BTreeMap::new();
        Self::walk(root, &mut String::new(), &mut keys);
        Self { keys }
    }

    fn walk(shape: &Shape, prefix: &mut String, keys: &mut BTreeMap<String, Type>) {
        for member in &shape.members {
            let base = prefix.len();
            prefix.push('/');
            prefix.push_str(member.name.as_str());
            match &member.node {
                Node::Collection(collection) => {
                    keys.insert(prefix.clone(), Self::key_type(collection));
                    Self::walk(&collection.shape, prefix, keys);
                }
                Node::Struct(inner) => Self::walk(inner, prefix, keys),
                _ => {}
            }
            prefix.truncate(base);
        }
    }

    fn key_type(collection: &Collection) -> Type {
        let mut components: Vec<(String, Type)> = Vec::new();
        for field in &collection.key {
            let ty = collection
                .shape
                .member(field.as_str())
                .and_then(|member| match &member.node {
                    Node::Scalar(scalar) => Some(scalar.ty.clone()),
                    // A.8: a struct `$key` target carries a struct key type, so a
                    // `$ref` to a struct-keyed collection resolves to that struct
                    // (matching the stored key), not the `json` fallback.
                    Node::Struct(shape) => Some(shape.key_struct_type()),
                    _ => None,
                })
                .unwrap_or(Type::Json);
            components.push((field.as_str().to_owned(), ty));
        }
        match components.as_slice() {
            [(_, ty)] => ty.clone(),
            _ => Type::Composite(components),
        }
    }
}

/// Resolve every `$ref` target against the model, filling key types.
pub(crate) fn resolve(reporter: &mut Reporter, root: &mut Shape) {
    let index = Index::build(root);
    resolve_shape(reporter, root, &index);
}

fn resolve_shape(reporter: &mut Reporter, shape: &mut Shape, index: &Index) {
    for member in &mut shape.members {
        match &mut member.node {
            Node::Reference(reference) => resolve_ref(reporter, reference, index),
            Node::Struct(inner) => resolve_shape(reporter, inner, index),
            Node::Collection(collection) => resolve_shape(reporter, &mut collection.shape, index),
            Node::Set(set) => {
                if let Type::Ref(_) = &set.element {
                    // A set-of-refs element type is validated at declaration; a
                    // deeper target check is a documented seam.
                }
            }
            _ => {}
        }
    }
}

fn resolve_ref(reporter: &mut Reporter, reference: &mut Reference, index: &Index) {
    // §13.12: a `#handle` target names an imported module interface, not a local
    // collection. Its key type is the peer interface's key, knowable only once
    // the composition binds the handle (a runtime seam), so it is not resolved
    // against the local collection index and is not rejected as a missing
    // collection. The cross-boundary `$on_delete` requirement is enforced by the
    // deletion pass (`crate::delete`).
    if reference.target.trim_start().starts_with('#') {
        return;
    }
    let normalized = normalize_target(&reference.target);
    match index.keys.get(&normalized) {
        Some(key) => reference.key_type = key.clone(),
        None => reporter.reject_hint(
            reference.span,
            code::REF,
            format!("`$ref` target `{}` does not name a declared collection", reference.target),
            "reference an existing collection by its absolute path, e.g. `/accounts`",
        ),
    }
}

/// Normalize a target path to the `/segment/segment` index form.
fn normalize_target(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.starts_with('/') {
        trimmed.trim_end_matches('/').to_owned()
    } else {
        format!("/{}", trimmed.trim_end_matches('/'))
    }
}
