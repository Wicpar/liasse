//! Building the logical value tree an expression reads (`liasse_expr::Row`) from
//! the prospective row map.
//!
//! Evaluation is a pure function of an [`Environment`](liasse_expr::Environment),
//! whose root is a package-root [`Row`]. The store keeps each row as an opaque
//! [`Value::Struct`] of its writable fields; this module folds those stored
//! structs back into the keyed [`Row`] tree the evaluator walks, in Annex B key
//! order (B.5).
//!
//! CORE scope: top-level keyed collections and their scalar/ref/set/struct
//! fields are materialized. Nested collections, sibling views, and row-local
//! computed values are documented seams — a mutation body or view over the tasks
//! app (§3.2) and the §5/§8 rule cases reads only the modelled shapes.

use std::collections::BTreeMap;

use liasse_expr::{Cell, Row, RowId};
use liasse_ident::NameSegment;
use liasse_model::{Collection, Node};
use liasse_store::{AddressStep, CollectionPath, KeyValue, RowAddress};
use liasse_value::{Struct, Text, Value};

use crate::schema::Schema;

/// A row's writable fields, keyed by field name (the mutable working form of a
/// stored [`Value::Struct`]).
pub(crate) type FieldMap = BTreeMap<String, Value>;

/// Decompose a stored row struct into its field map.
pub(crate) fn fields_of(value: &Value) -> FieldMap {
    match value {
        Value::Struct(fields) => fields
            .fields()
            .map(|(name, value)| (name.as_str().to_owned(), value.clone()))
            .collect(),
        _ => FieldMap::new(),
    }
}

/// Reassemble a field map into a canonical row struct value.
pub(crate) fn struct_of(fields: &FieldMap) -> Value {
    Value::Struct(Struct::new(
        fields.iter().map(|(name, value)| (Text::new(name.clone()), value.clone())),
    ))
}

/// The typed key of a row from its field values (§5.4): a single scalar for a
/// one-field key, or the key-field components in `$key` order for a composite.
pub(crate) fn row_key(collection: &Collection, fields: &FieldMap) -> Option<KeyValue> {
    let mut components = collection
        .key
        .iter()
        .map(|field| fields.get(field.as_str()).cloned());
    let first = components.next().flatten()?;
    let mut rest = Vec::new();
    for component in components {
        rest.push(component?);
    }
    Some(KeyValue::composite(first, rest))
}

/// The application-visible key value of a row (§5.4): the lone component for a
/// single-field key, or a struct of the named components for a composite key,
/// matching how a selector compares `row.key()`.
pub(crate) fn key_identity(collection: &Collection, key: &KeyValue) -> Value {
    let fields: Vec<&str> = collection.key.iter().map(|f| f.as_str()).collect();
    let mut components = key.components();
    match fields.as_slice() {
        [_] => components.next().cloned().unwrap_or(Value::None),
        _ => Value::Struct(Struct::new(
            fields.iter().zip(components).map(|(name, value)| (Text::new((*name).to_owned()), value.clone())),
        )),
    }
}

/// The address of a row in a top-level collection.
pub(crate) fn top_address(name: &str, key: KeyValue) -> RowAddress {
    RowAddress::root(AddressStep::new(NameSegment::new(name), key))
}

/// Materialize the package-root row from the working state (§8.2), keeping only
/// the rows `keep` admits. Each top-level collection becomes a keyed
/// [`Cell::Collection`] in Annex B order. `keep` is the temporal-activity
/// predicate (§14): a bucketed collection's inactive rows are excluded from
/// ordinary reads while remaining extant in the store; a non-bucketed
/// collection's predicate is a constant `true`, reproducing §8.2 exactly.
pub(crate) fn materialize_root_filtered(
    schema: Schema<'_>,
    working: &BTreeMap<RowAddress, FieldMap>,
    keep: &dyn Fn(&str, &FieldMap) -> bool,
) -> Row {
    let mut cells: Vec<(String, Cell)> = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Collection(collection) = &member.node {
            let name = member.name.as_str();
            let rows = collection_rows(name, collection, working, keep);
            cells.push((name.to_owned(), Cell::Collection(rows)));
        }
    }
    Row::keyless(RowId::leaf(0), cells)
}

/// The rows of one top-level collection admitted by `keep`, in key-ascending
/// order (B.5).
fn collection_rows(
    name: &str,
    collection: &Collection,
    working: &BTreeMap<RowAddress, FieldMap>,
    keep: &dyn Fn(&str, &FieldMap) -> bool,
) -> Vec<Row> {
    let path = CollectionPath::top(NameSegment::new(name));
    working
        .iter()
        .filter(|(address, _)| path.contains(address))
        .filter(|(_, fields)| keep(name, fields))
        .enumerate()
        .filter_map(|(index, (address, fields))| {
            let step = address.steps().last()?;
            let key = key_identity(collection, step.key());
            Some(build_row(collection, fields, key, RowId::leaf(index as u64)))
        })
        .collect()
}

/// One collection row as a logical [`Row`]: its key, and a cell per declared
/// field (§5.4). Fields absent from storage read as `none`.
fn build_row(collection: &Collection, fields: &FieldMap, key: Value, id: RowId) -> Row {
    let cells = collection.shape.members.iter().map(|member| {
        let name = member.name.as_str();
        let cell = match &member.node {
            Node::Collection(_) => Cell::Collection(Vec::new()),
            _ => Cell::Scalar(fields.get(name).cloned().unwrap_or(Value::None)),
        };
        (name.to_owned(), cell)
    });
    Row::new(id, key, cells)
}
