//! The candidate descriptor and the descriptor-driven candidate build.
//!
//! A face reconstructs, from the stored payload, its typed key, and the prefetched
//! subtree, exactly the [`Row`] the runtime's `materialize::build_row` produces:
//! every declared non-collection member a scalar cell (absent ⇒ `none`), every
//! declared nested keyed collection a [`Cell::Collection`] materialized from the
//! subtree by relative path, and the key-derived [`RowId`] identity (§7.2, D.1).
//! This is the one seam that is not literally shared code (§7.3); the layer-1
//! lowering-parity gate checks its equivalence against the interpreter over the
//! corpus.

use liasse_expr::{Cell, Row, RowId};
use liasse_store::{CandidateSubtree, KeyValue};
use liasse_value::{Struct, Text, Value};

/// A declared candidate shape: how to rebuild one collection's row from its stored
/// `Value::Struct`, its typed key, and its live subtree.
#[derive(Debug, Clone)]
pub struct CandidateDescriptor {
    /// Whether the collection's key is a single field (its identity is the lone
    /// component) rather than a composite (a positional [`Value::Composite`], §5.4).
    single_field_key: bool,
    /// The declared shape members in declaration order — a scalar field, or a
    /// nested keyed collection with its own descriptor.
    members: Vec<Member>,
}

/// One declared shape member: a scalar/struct field, or a nested keyed collection.
#[derive(Debug, Clone)]
pub struct Member {
    name: String,
    nested: Option<CandidateDescriptor>,
}

/// One direct child of a nested-collection level while rebuilding it from the flat
/// subtree: its full relative path, stored value, and key.
struct ChildRow<'a> {
    path: &'a [(String, KeyValue)],
    value: &'a Value,
    key: &'a KeyValue,
}

impl Member {
    /// A scalar/ref/struct field member.
    #[must_use]
    pub fn scalar(name: impl Into<String>) -> Self {
        Self { name: name.into(), nested: None }
    }

    /// A nested keyed-collection member with its own descriptor.
    #[must_use]
    pub fn nested(name: impl Into<String>, descriptor: CandidateDescriptor) -> Self {
        Self { name: name.into(), nested: Some(descriptor) }
    }

    /// The nested collection's step name, when this member is a nested collection.
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.nested.as_ref().map(|_| self.name.as_str())
    }
}

impl CandidateDescriptor {
    /// A descriptor for a collection with the given key arity and shape members.
    #[must_use]
    pub fn new(single_field_key: bool, members: Vec<Member>) -> Self {
        Self { single_field_key, members }
    }

    /// The nested-collection step names this descriptor reads through (one level),
    /// for the program's subtree read-set.
    #[must_use]
    pub fn nested_steps(&self) -> Vec<String> {
        self.members.iter().filter_map(|m| m.step().map(str::to_owned)).collect()
    }

    /// Build the candidate [`Row`] — the value tree a face evaluates against.
    #[must_use]
    pub fn build_row(&self, value: &Value, key: &KeyValue, subtree: &CandidateSubtree) -> Row {
        let id = RowId::keyed(row_id_text(key));
        self.build_at(value, key, &id, &[], subtree)
    }

    /// Build the row at relative path `rel` under identity `id`.
    fn build_at(
        &self,
        value: &Value,
        key: &KeyValue,
        id: &RowId,
        rel: &[(String, KeyValue)],
        subtree: &CandidateSubtree,
    ) -> Row {
        let fields = fields_of(value);
        let key_identity = self.key_identity(key);
        let cells = self.members.iter().map(|member| {
            let cell = match &member.nested {
                Some(nested) => Cell::Collection(nested.build_nested(&member.name, id, rel, subtree)),
                None => Cell::scalar(fields.get(&member.name).cloned().unwrap_or(Value::None)),
            };
            (member.name.clone(), cell)
        });
        Row::new(id.clone(), key_identity, cells)
    }

    /// The direct child rows under `step` at relative path `rel`, in Annex-B key
    /// order, each built recursively — reproducing `materialize::rows_at`.
    fn build_nested(
        &self,
        step: &str,
        parent_id: &RowId,
        rel: &[(String, KeyValue)],
        subtree: &CandidateSubtree,
    ) -> Vec<Row> {
        let mut children: Vec<ChildRow<'_>> = Vec::new();
        for (path, value) in &subtree.0 {
            if path.len() != rel.len() + 1 || !path.starts_with(rel) {
                continue;
            }
            match path.last() {
                Some((last_step, key)) if last_step == step => {
                    children.push(ChildRow { path: path.as_slice(), value, key });
                }
                _ => {}
            }
        }
        // A well-formed subtree already arrives in key order per (parent, step); a
        // defensive sort keeps the Annex-B guarantee even if the source did not.
        children.sort_by(|a, b| a.key.cmp(b.key));
        children
            .into_iter()
            .map(|child| {
                let child_id = parent_id.child_keyed(row_id_text(child.key));
                self.build_at(child.value, child.key, &child_id, child.path, subtree)
            })
            .collect()
    }

    /// The application-visible key identity of a row (§5.4): the lone component for
    /// a single-field key, else the positional [`Value::Composite`] tuple.
    fn key_identity(&self, key: &KeyValue) -> Value {
        if self.single_field_key {
            key.components().next().cloned().unwrap_or(Value::None)
        } else {
            Value::Composite(key.components().cloned().collect())
        }
    }
}

/// Decompose a stored row struct into its field map.
fn fields_of(value: &Value) -> std::collections::BTreeMap<String, Value> {
    match value {
        Value::Struct(fields) => {
            fields.fields().map(|(name, value)| (name.as_str().to_owned(), value.clone())).collect()
        }
        _ => std::collections::BTreeMap::new(),
    }
}

/// The canonical D.2 key text of a row's key — its stable identity component
/// (Annex D.1), exactly as `materialize::row_id_text` computes it.
fn row_id_text(key: &KeyValue) -> String {
    let components: Vec<Value> = key.components().cloned().collect();
    match liasse_ident::KeyText::from_key_values(&components) {
        Ok(text) => text.as_str().to_owned(),
        Err(_) => {
            components.iter().map(Value::to_canonical_json_string).collect::<Vec<_>>().join(":")
        }
    }
}

/// A keyless static-struct value (§5.3) reconstructed from a projected keyless row,
/// used by the flat projection's field-value extraction.
#[must_use]
pub(crate) fn struct_value(row: &Row) -> Value {
    Value::Struct(Struct::new(
        row.cells().filter_map(|(name, cell)| Some((Text::new(name.clone()), field_value(cell)?))),
    ))
}

/// The exposed value of one projected cell for a FLAT view (§7.2, `cell_field_value`):
/// a `none` optional is omitted, a keyless nested row is carried inline as a
/// `Value::Struct`, and a keyed sub-view / collection is dropped.
#[must_use]
pub(crate) fn field_value(cell: &Cell) -> Option<Value> {
    match cell {
        Cell::Scalar(Value::None) => None,
        Cell::Scalar(value) => Some(value.clone()),
        Cell::Row(row) if row.key() == &Value::None => Some(struct_value(row)),
        Cell::Row(_) | Cell::Collection(_) => None,
    }
}

/// The exposed value of one projected cell for a §10.5 COVERAGE view (`row_object`):
/// only scalar cells survive (a `none` omitted); any nested row/collection is
/// dropped — the nested keyed view is re-added by the descent, not the projection.
#[must_use]
pub(crate) fn coverage_field_value(cell: &Cell) -> Option<Value> {
    match cell {
        Cell::Scalar(Value::None) => None,
        Cell::Scalar(value) => Some(value.clone()),
        Cell::Row(_) | Cell::Collection(_) => None,
    }
}
