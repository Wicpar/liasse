//! Reference resolution (SPEC.md §5.6, A.9).
//!
//! Every `$ref` names a target collection that MUST exist; the ref's visible
//! value type is that collection's key type. For a nested target this is the
//! target row's full identity (§5.4/§D.1/§A.9, SPEC-ISSUES item 26): every
//! ancestor collection `$key` followed by the local `$key`, in ancestor-then-local
//! order. This pass indexes the model's collections, then fills each
//! [`Reference`]'s key type or rejects an unresolvable target. Building the index
//! first and mutating references second keeps the tree free of aliasing.
//!
//! CORE scope: targets resolve against collections reachable by an absolute
//! `/segment/segment` path of collection names; a ref to a keyed *view* (§7.6)
//! is a documented seam.

use std::collections::BTreeMap;

use liasse_value::{RefTarget, Type};

use crate::report::{code, Reporter};
use crate::state::{Collection, Node, Reference, Shape};

/// The primary-key type of every collection, keyed by its absolute path.
struct Index {
    keys: BTreeMap<String, Type>,
}

impl Index {
    fn build(root: &Shape) -> Self {
        let mut keys = BTreeMap::new();
        Self::walk(root, &mut String::new(), &[], &mut keys);
        Self { keys }
    }

    fn walk(
        shape: &Shape,
        prefix: &mut String,
        ancestors: &[(String, Type)],
        keys: &mut BTreeMap<String, Type>,
    ) {
        for member in &shape.members {
            let base = prefix.len();
            prefix.push('/');
            prefix.push_str(member.name.as_str());
            match &member.node {
                Node::Collection(collection) => {
                    // §5.4/§D.1/§A.9 (SPEC-ISSUES item 26): a `$ref` to a nested
                    // collection carries the target row's FULL identity — every
                    // ancestor collection `$key` followed by this collection's local
                    // `$key`, in ancestor-then-local order. A root collection has no
                    // ancestors, so its ref key stays exactly its local `$key`
                    // (scalar or composite), unchanged.
                    let local = Self::key_components(collection, None);
                    let mut full = ancestors.to_vec();
                    full.extend(local.iter().cloned());
                    keys.insert(prefix.clone(), Self::compose(full));
                    // Descendants see this collection's key as an ancestor; qualify
                    // it by the declaration segment so an ancestor and a descendant
                    // sharing a key-field name (both `id`) stay distinct components.
                    let mut child_ancestors = ancestors.to_vec();
                    child_ancestors
                        .extend(Self::key_components(collection, Some(member.name.as_str())));
                    Self::walk(&collection.shape, prefix, &child_ancestors, keys);
                }
                Node::Struct(inner) => Self::walk(inner, prefix, ancestors, keys),
                _ => {}
            }
            prefix.truncate(base);
        }
    }

    /// This collection's local `$key` components as `(name, type)` pairs in `$key`
    /// order. `qualifier` prefixes each component name with the collection's
    /// declaration segment (`companies.id`) when the components are threaded down
    /// as a descendant's ancestor identity; `None` keeps the bare field name for a
    /// ref's own key type.
    fn key_components(collection: &Collection, qualifier: Option<&str>) -> Vec<(String, Type)> {
        collection
            .key
            .iter()
            .map(|field| {
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
                let name = match qualifier {
                    Some(segment) => format!("{segment}.{}", field.as_str()),
                    None => field.as_str().to_owned(),
                };
                (name, ty)
            })
            .collect()
    }

    /// One key component is a scalar key type; several are a composite (A.9).
    fn compose(components: Vec<(String, Type)>) -> Type {
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
                // §5.5/§5.6/A.9: a `$set` of `$ref` member is a governed inbound
                // ref exactly like a scalar `$ref`, so its target key type must be
                // resolved from the same collection index — not left at the `Json`
                // placeholder `ref_node` seeds. `ref_node` snapshots the element
                // type before this pass runs, so rebuild it from the now-resolved
                // key so the ref's visible value type (the target's key type, A.9)
                // is what the `$set` element carries into every RowType consumer.
                if let Some(reference) = &mut set.element_ref {
                    resolve_ref(reporter, reference, index);
                    set.element = Type::Ref(RefTarget::for_key(&reference.key_type));
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
