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
use liasse_model::Collection;
use liasse_store::{AddressStep, CollectionPath, KeyValue, RowAddress};
use liasse_value::{Struct, Text, Timestamp, Value};

use crate::error::{Rejection, RejectionReason};
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
/// single-field key, or the positional [`Value::Composite`] tuple in `$key` order
/// for a composite key, matching how a selector compares `row.key()`.
pub(crate) fn key_identity(collection: &Collection, key: &KeyValue) -> Value {
    let mut components = key.components();
    match collection.key.as_slice() {
        [_] => components.next().cloned().unwrap_or(Value::None),
        _ => Value::Composite(components.cloned().collect()),
    }
}

/// The typed storage [`KeyValue`] that addresses a row carrying application-visible
/// key identity `key` — the inverse of [`key_identity`] (§5.4, Annex B.4).
///
/// A composite key identity is the positional [`Value::Composite`] tuple in `$key`
/// order, so it decomposes into the N-component [`KeyValue`] the row was stored
/// under (`{ first } :: rest`); any other value is a single-field key. Wrapping the
/// whole composite tuple as one component instead would build a one-component key
/// that never equals the stored composite [`RowAddress`], so a lookup by an erase
/// or delete target's `row.key()` would miss the row.
pub(crate) fn key_value_of(key: &Value) -> KeyValue {
    match key {
        Value::Composite(components) => match components.as_slice() {
            [first, rest @ ..] => KeyValue::composite(first.clone(), rest.iter().cloned()),
            [] => KeyValue::single(Value::Composite(Vec::new())),
        },
        other => KeyValue::single(other.clone()),
    }
}

/// Normalize an authoring key operand to the application-visible key identity a
/// row carries (§6.3, A.9), given the collection's ordered `$key` field names.
///
/// A single-field key keeps its lone scalar identity. A composite key supplied as
/// an authoring object (`{ region, code }` — a [`Value::Struct`]) becomes the
/// positional [`Value::Composite`] tuple in `$key` order, the identical value
/// [`key_identity`] derives for a stored composite row, so a `collection - keys`
/// delete operand matches the row's key exactly as the `[{..}]` selector form
/// does. A value that already carries the positional composite (another row's
/// `$key`) or is a plain scalar passes through unchanged.
///
/// Defense in depth for the load gate (`liasse_expr::check_composite_delete_operand`,
/// §6.3/§8.5/A.9): an object operand must name *exactly* the `$key` components.
/// A missing component is refused rather than silently filled with `Value::None`
/// (which would match no row and no-op the delete), and an extra non-component
/// field is refused rather than silently dropped (which would delete on a
/// malformed key). The load check rejects such operands first; this guarantees the
/// runtime never acts on a non-key operand even if one reaches it.
pub(crate) fn normalize_key_operand(
    key_fields: &[String],
    value: Value,
) -> Result<Value, Rejection> {
    match key_fields {
        [_] => Ok(value),
        _ => match value {
            Value::Struct(fields) => {
                let mut components = Vec::with_capacity(key_fields.len());
                for name in key_fields {
                    match fields.get(name) {
                        Some(component) => components.push(component.clone()),
                        None => {
                            return Err(Rejection::new(
                                RejectionReason::Malformed,
                                format!(
                                    "composite delete operand is missing `$key` component `{name}` (§6.3, A.9)"
                                ),
                            ));
                        }
                    }
                }
                if fields.fields().count() != key_fields.len() {
                    return Err(Rejection::new(
                        RejectionReason::Malformed,
                        "composite delete operand carries a field that is not a `$key` component (§6.3, A.9)",
                    ));
                }
                Ok(Value::Composite(components))
            }
            other => Ok(other),
        },
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
pub(crate) fn materialize_root_filtered<'m>(
    schema: Schema<'m>,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
) -> Row {
    let mut cells: Vec<(String, Cell)> = Vec::new();
    for member in &schema.model().root().members {
        // §5.8: a top-level member naming a keyed shape (`companies: "company"`)
        // resolves to that collection and materializes as a keyed cell, exactly like
        // a directly-declared collection. `resolved_collection` is the identity for a
        // real `Node::Collection`, so the two cases share this arm.
        if let Some(collection) = schema.resolved_collection(&member.node) {
            let name = member.name.as_str();
            let rows = collection_rows(schema, name, collection, working, temporal, true);
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

/// Materialize the single row at `address` (top-level or nested) with its
/// nested collections and struct cells (§5.4), for a row receiver / local-row
/// binding read (§8.1, §8.10). `None` when no row lives there.
pub(crate) fn materialize_row<'m>(
    schema: Schema<'m>,
    collection: &'m Collection,
    address: &RowAddress,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
) -> Option<Row> {
    let fields = working.get(address)?;
    let step = address.steps().last()?;
    let key = key_identity(collection, step.key());
    let id = RowId::keyed(row_id_text(step.key()));
    // §14.1: a single-row bare read (a `return`/receiver read, a meter spend row)
    // descends its nested keyed collections active-filtered exactly like the
    // top-level bare read — the caller's `temporal.keep` decides activity, so a
    // nested bucketed collection exposes only its rows active at the read instant.
    Some(build_row(schema, collection, fields, key, id, address, working, temporal, true))
}

/// The full extant row set of one bucketed collection (§14.2), ignoring current
/// activity — the working set a temporal selector re-derives from. Each row
/// carries its `$from`/`$until` interval cells.
pub(crate) fn extant_bucketed_rows<'m>(
    schema: Schema<'m>,
    collection: &'m Collection,
    name: &str,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
) -> Vec<Row> {
    collection_rows(schema, name, collection, working, temporal, false)
}

/// The rows of one top-level collection in key-ascending order (B.5). When
/// `filter_active` holds, a bucketed collection's inactive rows are dropped (a
/// bare read, §14.1); otherwise every extant row is kept (`.$all`, §14.2). Each
/// bucketed row carries its evaluated `$from`/`$until` interval cells.
fn collection_rows<'m>(
    schema: Schema<'m>,
    name: &str,
    collection: &'m Collection,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
    filter_active: bool,
) -> Vec<Row> {
    let path = CollectionPath::top(NameSegment::new(name));
    rows_at(schema, &path, collection, working, temporal, filter_active, None)
}

/// The rows of one collection at `path` in key-ascending order (B.5), each built
/// with its nested collections materialized (§5.4). `parent_id` is `None` for a
/// top-level collection (a key-derived leaf identity) and the containing row's
/// identity for a nested collection, so a nested row extends its ancestor's
/// identity path (§7.2, Annex D.1). `filter_active` drops a bucketed collection's
/// inactive rows; nested collections are read in full.
fn rows_at<'m>(
    schema: Schema<'m>,
    path: &CollectionPath,
    collection: &'m Collection,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
    filter_active: bool,
    parent_id: Option<&RowId>,
) -> Vec<Row> {
    let name = path.name().as_str();
    working
        .iter()
        .filter(|(address, _)| path.contains(address))
        .filter(|(_, fields)| !filter_active || (temporal.keep)(name, fields))
        .filter_map(|(address, fields)| {
            let step = address.steps().last()?;
            let key = key_identity(collection, step.key());
            // §12.4 / Annex D.1: a view row's identity derives from its key, not
            // its materialized position, so it survives sibling deletions.
            let key_text = row_id_text(step.key());
            let id = match parent_id {
                None => RowId::keyed(key_text),
                Some(parent) => parent.child_keyed(key_text),
            };
            let mut row = build_row(schema, collection, fields, key, id, address, working, temporal, filter_active);
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

/// The stable [`RowId`] a materialized row at `address` carries (§7.2, D.1): the
/// key-derived identity of each address step, top to bottom — the same chain
/// [`rows_at`] builds. Lets a later pass (meter accessors, §15.6) key extra cells
/// by row identity and match them onto the materialized tree.
#[must_use]
pub(crate) fn row_id_of(address: &RowAddress) -> Option<RowId> {
    let mut steps = address.steps();
    let first = steps.next()?;
    let mut id = RowId::keyed(row_id_text(first.key()));
    for step in steps {
        id = id.child_keyed(row_id_text(step.key()));
    }
    Some(id)
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
/// field (§5.4). A nested keyed collection member is materialized from the rows
/// living under this row's `address` (§5.4), extending its identity; every other
/// field reads its stored value (absent reads as `none`).
#[allow(clippy::too_many_arguments)]
fn build_row<'m>(
    schema: Schema<'m>,
    collection: &'m Collection,
    fields: &FieldMap,
    key: Value,
    id: RowId,
    address: &RowAddress,
    working: &BTreeMap<RowAddress, FieldMap>,
    temporal: &Temporal<'_>,
    filter_active: bool,
) -> Row {
    let cells: Vec<(String, Cell)> = collection
        .shape
        .members
        .iter()
        .map(|member| {
            let name = member.name.as_str();
            // §5.4/§5.8: a nested keyed collection — declared directly OR adopted by a
            // `$types`/`$like` name (`subcompanies: "company"`, `children: { $like:
            // "^" }`) — is a traversable keyed-row source. Materialize the rows stored
            // under this row's address, extending its identity (§7.2, D.1). The
            // recursion is bounded by the DATA, not the type: `rows_at` only reaches the
            // rows that exist under `address`, so a type-level-infinite self-referential
            // shape with finite data terminates when a level has no stored rows.
            //
            // §14.1/§14.2: `filter_active` propagates from the enclosing read, so a
            // nested BUCKETED collection exposes only its rows active at the clock on a
            // bare read (matching a top-level bucket) while the `.$all` extant walk
            // (`filter_active == false`) keeps its expired rows.
            let cell = match schema.resolved_collection(&member.node) {
                Some(nested) => {
                    let nested_path =
                        CollectionPath::nested(address.steps().cloned(), NameSegment::new(name));
                    Cell::Collection(rows_at(schema, &nested_path, nested, working, temporal, filter_active, Some(&id)))
                }
                None => Cell::Scalar(fields.get(name).cloned().unwrap_or(Value::None)),
            };
            (name.to_owned(), cell)
        })
        .collect();
    Row::new(id, key, cells)
}
