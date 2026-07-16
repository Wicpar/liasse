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
use liasse_value::{Struct, Text, Timestamp, Value};

use crate::schema::Schema;

/// The half-open `[from, until)` interval of one bucketed row, evaluated once at
/// the read instant. Each side is `None` when unconstrained (§14.1). A read of a
/// non-bucketed collection yields `None` (no interval, always active, §8.2).
pub(crate) type Interval = (Option<Timestamp>, Option<Timestamp>);

/// The temporal role of a collection during materialization (§14): whether a row
/// is currently readable ([`keep`](Temporal::keep)) and its `[from, until)`
/// interval ([`interval`](Temporal::interval), `None` for a non-bucketed
/// collection). The two are supplied together so a bucketed row is materialized
/// once, carrying its interval as `$from`/`$until` cells for temporal selectors.
pub(crate) struct Temporal<'a> {
    /// Whether collection `name`'s row is active at the clock (bare-read filter).
    pub(crate) keep: &'a dyn Fn(&str, &FieldMap) -> bool,
    /// Collection `name`'s row interval, or `None` when it is not bucketed.
    pub(crate) interval: &'a dyn Fn(&str, &FieldMap) -> Option<Interval>,
}

/// The structural cell name of a bucketed row's interval start (§14.4).
const FROM_CELL: &str = "$from";
/// The structural cell name of a bucketed row's interval end (§14.4).
const UNTIL_CELL: &str = "$until";

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
/// the rows [`Temporal::keep`] admits. Each top-level collection becomes a keyed
/// [`Cell::Collection`] in Annex B order. A bucketed collection's inactive rows
/// are excluded from ordinary reads while remaining extant in the store (§14);
/// each surviving bucketed row carries its `$from`/`$until` interval cells so a
/// temporal selector can re-derive activity. A non-bucketed collection keeps
/// every row and no interval, reproducing §8.2 exactly.
pub(crate) fn materialize_root_filtered(
    schema: Schema<'_>,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
) -> Row {
    let mut cells: Vec<(String, Cell)> = Vec::new();
    for member in &schema.model().root().members {
        if let Node::Collection(collection) = &member.node {
            let name = member.name.as_str();
            let rows = collection_rows(name, collection, working, temporal, true);
            cells.push((name.to_owned(), Cell::Collection(rows)));
        }
    }
    // §8.2: the package root's singleton fields (scalars, structs, sets, refs)
    // fold onto the root row as cells beside its collections.
    let empty = FieldMap::new();
    let singleton = working.get(&crate::singleton::address()).unwrap_or(&empty);
    cells.extend(crate::singleton::cells(schema.model(), schema.model().root(), singleton));
    Row::keyless(RowId::leaf(0), cells)
}

/// The full extant row set of one bucketed collection (§14.2), ignoring current
/// activity — the working set a temporal selector re-derives from. Each row
/// carries its `$from`/`$until` interval cells.
pub(crate) fn extant_bucketed_rows(
    collection: &Collection,
    name: &str,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
) -> Vec<Row> {
    collection_rows(name, collection, working, temporal, false)
}

/// The rows of one top-level collection in key-ascending order (B.5). When
/// `filter_active` holds, a bucketed collection's inactive rows are dropped (a
/// bare read, §14.1); otherwise every extant row is kept (`.$all`, §14.2). Each
/// bucketed row carries its evaluated `$from`/`$until` interval cells.
fn collection_rows(
    name: &str,
    collection: &Collection,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
    filter_active: bool,
) -> Vec<Row> {
    let path = CollectionPath::top(NameSegment::new(name));
    working
        .iter()
        .filter(|(address, _)| path.contains(address))
        .filter(|(_, fields)| !filter_active || (temporal.keep)(name, fields))
        .filter_map(|(address, fields)| {
            let step = address.steps().last()?;
            let key = key_identity(collection, step.key());
            // §12.4 / Annex D.1: a view row's identity derives from its key, not
            // its materialized position, so it survives sibling deletions.
            let id = RowId::keyed(row_id_text(step.key()));
            let mut row = build_row(collection, fields, key, id);
            if let Some(interval) = (temporal.interval)(name, fields) {
                row = with_interval_cells(row, interval);
            }
            Some(row)
        })
        .collect()
}

/// Add the `$from`/`$until` structural cells to a bucketed row (§14.4): a present
/// bound is a `timestamp` cell, an absent bound a `none` cell.
fn with_interval_cells(row: Row, (from, until): Interval) -> Row {
    let cells = row
        .cells()
        .map(|(name, cell)| (name.clone(), cell.clone()))
        .chain([
            (FROM_CELL.to_owned(), interval_cell(from)),
            (UNTIL_CELL.to_owned(), interval_cell(until)),
        ]);
    Row::new(row.id().clone(), row.key().clone(), cells)
}

/// A bound cell: its `timestamp` value, or `none` when unbounded (§14.1).
fn interval_cell(bound: Option<Timestamp>) -> Cell {
    Cell::Scalar(bound.map_or(Value::None, Value::Timestamp))
}

/// A bucketed row's `$from`/`$until` interval as read back from its structural
/// cells — the temporal index a [`TemporalQuery`] filters on (§14.1).
///
/// [`TemporalQuery`]: liasse_expr::TemporalQuery
#[must_use]
pub(crate) fn row_interval(row: &Row) -> Interval {
    (bound_cell(row, FROM_CELL), bound_cell(row, UNTIL_CELL))
}

fn bound_cell(row: &Row, name: &str) -> Option<Timestamp> {
    match row.cell(name)?.as_scalar()? {
        Value::Timestamp(instant) => Some(*instant),
        _ => None,
    }
}

/// The canonical D.2 key text of a row's key — its stable identity component
/// (Annex D.1). Key fields are validated key-eligible at build, so the D.2
/// rendering succeeds; the join fallback keeps this total for the impossible
/// non-key-eligible case rather than panicking.
fn row_id_text(key: &KeyValue) -> String {
    let components: Vec<Value> = key.components().cloned().collect();
    match liasse_ident::KeyText::from_key_values(&components) {
        Ok(text) => text.as_str().to_owned(),
        Err(_) => components
            .iter()
            .map(Value::to_canonical_json_string)
            .collect::<Vec<_>>()
            .join(":"),
    }
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
