//! The single "walk every reference-bearing field of a row, including those nested
//! inside static structs" descent the runtime's reference passes share (§5.3).
//!
//! A `$ref` is a legal member of a static struct (§5.3: "Structs MAY contain
//! fields, structs, sets, views, and nested keyed collections."), and it compiles
//! into `CompiledStruct::fields` with `reference: Some(..)` exactly like a
//! top-level field (`compiled::compile_struct`). But every runtime pass that
//! reasons about references historically iterated only the collection's top-level
//! `fields`, so a struct-nested ref escaped reference validity (§5.6/§22.1),
//! `$on_delete` planning (§21.1), and atomic-rekey rewrite (§5.4) — while the CORE
//! model's own §21.1 gate (`liasse-model::delete::collect_refs`) DID descend into
//! `Node::Struct`, a model/runtime asymmetry that let a package declare a policy
//! the runtime then ignored.
//!
//! [`ref_sites`] closes that gap once, in one place: it yields every
//! reference-bearing field of a row's static-struct tree (recursively, at any
//! depth), so the three passes — [`crate::rules`] validity, [`crate::cascade`]
//! `$on_delete` edges, and [`crate::interp`] rekey rewrite — descend identically,
//! mirroring the model's `collect_refs` shape.

use liasse_value::Value;

use crate::compiled::{CompiledCollection, CompiledField, CompiledStruct};
use crate::materialize::FieldMap;

/// One reference-bearing field located within a row's static-struct tree (§5.3):
/// the static-struct name path from the row down to the field's container (empty
/// for a top-level field) and the field descriptor. A struct member may be a
/// scalar `$ref` (`field.reference`) or a `$set`-of-`$ref` (`field.element_reference`);
/// the caller inspects whichever applies. Struct descent is recursive, so a ref at
/// any depth is reached.
pub(crate) struct RefSite<'a> {
    pub(crate) container: Vec<&'a str>,
    pub(crate) field: &'a CompiledField,
}

impl RefSite<'_> {
    /// The stored value of this reference field within a row's `fields`, descending
    /// the static-struct container path. `None` when an intermediate struct value
    /// or the field itself is absent (or an intermediate member is not a struct).
    pub(crate) fn value<'v>(&self, fields: &'v FieldMap) -> Option<&'v Value> {
        let Some((head, rest)) = self.container.split_first() else {
            return fields.get(&self.field.name);
        };
        let mut current: &Value = fields.get(*head)?;
        for name in rest {
            let Value::Struct(inner) = current else { return None };
            current = inner.get(name)?;
        }
        let Value::Struct(inner) = current else { return None };
        inner.get(&self.field.name)
    }

    /// A dotted diagnostic name for this reference field: `owner` at the top level,
    /// `meta.owner` one struct deep — so a §21.1 deletion diagnostic names the ref
    /// site even when it is struct-nested.
    pub(crate) fn display_name(&self) -> String {
        if self.container.is_empty() {
            return self.field.name.clone();
        }
        let mut name = self.container.join(".");
        name.push('.');
        name.push_str(&self.field.name);
        name
    }
}

/// Every reference-bearing field of `collection`'s row — top-level and
/// struct-nested at any depth (§5.3) — in a stable declaration order. The result
/// is data-independent (derived from the compiled shape alone), so a caller
/// iterating many rows computes it once and reuses it per row.
pub(crate) fn ref_sites(collection: &CompiledCollection) -> Vec<RefSite<'_>> {
    let mut out = Vec::new();
    let mut container = Vec::new();
    collect(&collection.fields, &collection.structs, &mut container, &mut out);
    out
}

/// Accumulate the reference-bearing fields of one struct level and recurse into its
/// nested static structs, tracking the container name path.
fn collect<'a>(
    fields: &'a [CompiledField],
    structs: &'a [CompiledStruct],
    container: &mut Vec<&'a str>,
    out: &mut Vec<RefSite<'a>>,
) {
    for field in fields {
        if field.reference.is_some() || field.element_reference.is_some() {
            out.push(RefSite { container: container.clone(), field });
        }
    }
    for structure in structs {
        container.push(&structure.name);
        collect(&structure.fields, &structure.structs, container, out);
        container.pop();
    }
}
