//! One captured row and its descendants — the unit the portable state section
//! (§19.5) and the §20.1 migration copy both carry.
//!
//! A row is not a flat field map: §5.4 makes a nested keyed collection *real row
//! state living under its parent*, so carrying a row forward means carrying the
//! whole subtree rooted at it. [`CapturedRow`] is that subtree — the row's own
//! writable members plus, per declared child collection, the captured rows living
//! under it — at any depth, uniformly.
//!
//! The wire form nests the same way: a child collection serializes as a JSON array
//! under its declaration name inside the parent row object. A member name is unique
//! within a shape (a field, static struct, view, and nested collection share one
//! namespace), so a child array never collides with a field, and a row with no
//! children serializes byte-identically to the flat form that preceded this — an
//! artifact written before nested carry-through still restores unchanged.

use std::collections::BTreeMap;

use liasse_ident::NameSegment;
use liasse_store::{CollectionPath, RowAddress, StoreError};
use liasse_value::Type;
use serde_json::Value as J;

use crate::compiled::CompiledCollection;
use crate::error::EngineError;
use crate::materialize::{self, FieldMap};
use crate::schema::Schema;

/// One committed row as a portable capture: its own writable members, and the
/// captured rows of every nested keyed collection (§5.4) beneath it, keyed by the
/// child collection's declaration name in Annex B order.
pub(crate) struct CapturedRow {
    fields: FieldMap,
    children: BTreeMap<String, Vec<CapturedRow>>,
}

impl CapturedRow {
    /// The row's own writable members (its scalar/ref/set fields and §5.3 static
    /// structs) — everything except its nested child collections.
    pub(crate) fn fields(&self) -> &FieldMap {
        &self.fields
    }

    /// The captured rows of the nested keyed collection declared as `name` under
    /// this row, empty when the row holds none.
    pub(crate) fn child(&self, name: &str) -> &[CapturedRow] {
        self.children.get(name).map_or(&[], Vec::as_slice)
    }

    /// Assemble the flat committed-row map a [`Prospective`](crate::state::Prospective)
    /// gathers into one captured tree per top-level collection, keyed by collection
    /// name. `reserved` is the §8.2 singleton's address, which is not a collection
    /// row and is carried separately.
    ///
    /// The map is in Annex B address order, in which a row precedes every row nested
    /// under it, so walking it in REVERSE reaches every child before its parent: each
    /// row is built with its already-assembled children attached, in one pass and
    /// without re-scanning. A row whose parent is absent from the map cannot be
    /// attached to anything; a well-formed store holds none (a subtree scan reaches a
    /// child only through its parent), so one is a store-integrity fault and is
    /// reported as such rather than dropped.
    pub(crate) fn forest(
        working: &BTreeMap<RowAddress, FieldMap>,
        reserved: &RowAddress,
    ) -> Result<BTreeMap<String, Vec<CapturedRow>>, StoreError> {
        let mut orphans: BTreeMap<RowAddress, BTreeMap<String, Vec<CapturedRow>>> = BTreeMap::new();
        let mut roots: BTreeMap<String, Vec<CapturedRow>> = BTreeMap::new();
        for (address, fields) in working.iter().rev() {
            if address == reserved {
                continue;
            }
            let Some(step) = address.steps().last() else { continue };
            let name = step.name().as_str().to_owned();
            let mut children = orphans.remove(address).unwrap_or_default();
            // Reverse order built each child list backwards; restore Annex B order.
            for rows in children.values_mut() {
                rows.reverse();
            }
            let row = Self { fields: fields.clone(), children };
            match address.parent() {
                Some(parent) => orphans.entry(parent).or_default().entry(name).or_default().push(row),
                None => roots.entry(name).or_default().push(row),
            }
        }
        if let Some(address) = orphans.keys().next() {
            return Err(StoreError::Corruption {
                detail: format!(
                    "committed rows are nested under `{}`, which holds no row of its own: the \
                     subtree cannot be captured because it has no parent to hang from (§5.4/§22.1)",
                    address.render()
                ),
            });
        }
        for rows in roots.values_mut() {
            rows.reverse();
        }
        Ok(roots)
    }

    /// This row's canonical wire form: its members as the shared value codec writes
    /// them, plus one JSON array per nested child collection under its declaration
    /// name (§19.5).
    pub(crate) fn to_wire(&self) -> J {
        let mut wire = materialize::struct_of(&self.fields).to_wire();
        if let J::Object(members) = &mut wire {
            for (name, rows) in &self.children {
                members.insert(name.clone(), J::Array(rows.iter().map(Self::to_wire).collect()));
            }
        }
        wire
    }

    /// Decode one row of `collection` from its wire form: the declared child
    /// collections are lifted out and decoded recursively against their own compiled
    /// shapes, then the remaining members decode through the row's optional-wrapped
    /// struct type. Lifting the children first is what keeps the row decode total —
    /// [`Type::Struct`] rejects a member it does not declare, and a child array is
    /// not a declared field.
    pub(crate) fn from_wire(collection: &CompiledCollection, wire: &J) -> Result<Self, EngineError> {
        let mut own = wire.clone();
        let mut children = BTreeMap::new();
        if let J::Object(members) = &mut own {
            for child in &collection.children {
                let Some(J::Array(rows)) = members.remove(&child.name) else { continue };
                let decoded: Result<Vec<Self>, EngineError> =
                    rows.iter().map(|row| Self::from_wire(child, row)).collect();
                children.insert(child.name.clone(), decoded?);
            }
        }
        let value = Self::row_type(collection).decode(&own).map_err(|error| {
            EngineError::Internal(format!("state row in `{}`: {error}", collection.name))
        })?;
        Ok(Self { fields: materialize::fields_of(&value), children })
    }

    /// Re-address this row and its whole subtree into `working`, ready to stage.
    /// `path` is the row's collection declaration path, `parent` the address of the
    /// row it hangs under (`None` at top level). A child collection the schema does
    /// not declare would have nowhere to go, so it is reported rather than dropped.
    pub(crate) fn place(
        &self,
        schema: Schema<'_>,
        path: &mut Vec<String>,
        parent: Option<&RowAddress>,
        working: &mut BTreeMap<RowAddress, FieldMap>,
    ) -> Result<(), EngineError> {
        let name = path.last().cloned().unwrap_or_default();
        let model = schema.collection_at_path(path).ok_or_else(|| {
            EngineError::Internal(format!("captured rows name no declared collection `{name}`"))
        })?;
        let key = materialize::row_key(model, &self.fields).ok_or_else(|| {
            EngineError::Internal(format!("captured row in `{name}` is missing a key field"))
        })?;
        let address = match parent {
            None => materialize::top_address(&name, key),
            Some(parent) => CollectionPath::nested(parent.steps().cloned(), NameSegment::new(name)).row(key),
        };
        for (child, rows) in &self.children {
            path.push(child.clone());
            for row in rows {
                row.place(schema, path, Some(&address), working)?;
            }
            path.pop();
        }
        working.insert(address, self.fields.clone());
        Ok(())
    }

    /// The optional-wrapped struct type used to decode one collection's rows: a
    /// stored non-optional field may hold `none`, so wrapping each declared member
    /// type in [`Type::Optional`] keeps the shared decoder total over captured rows.
    ///
    /// The row's declared members are its scalar/ref/set `fields` **and** its §5.3
    /// static struct members (`structs`) — a static struct compiles into
    /// `collection.structs`, not `fields`. [`Self::to_wire`] serializes every member
    /// of a row (`materialize::struct_of`), struct members included, so the decode
    /// type must carry them too or `Type::Struct::decode` rejects the serialized
    /// struct as an unexpected member and the artifact cannot restore (§19.5/§19.10).
    /// Both member kinds feed the one decode-type builder the §8.2 singleton path
    /// uses ([`crate::singleton::optional_decode_struct`]), which recursively
    /// optional-wraps a struct member's own members — so a keyed collection's static
    /// struct round-trips exactly as a singleton's does.
    fn row_type(collection: &CompiledCollection) -> Type {
        let fields = collection.fields.iter().map(|field| (field.name.clone(), field.ty.clone()));
        let structs = collection.structs.iter().map(|structure| (structure.name.clone(), structure.ty()));
        Type::Struct(crate::singleton::optional_decode_struct(fields.chain(structs)))
    }
}
