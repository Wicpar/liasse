//! Portable state capture: the opaque `state/current.cbor.zst` section of a
//! `.liasse` artifact (§19.5, §19.6, Annex D), owned by the runtime.
//!
//! The artifact layer treats the state section as opaque bytes verified by
//! checksum; deciding its encoding is the runtime's job. A [`StateSection`]
//! captures every top-level collection's committed writable rows in Annex B key
//! order and (de)serializes them through the same canonical strict-JSON value
//! codec the rest of the runtime uses (`Value::to_wire` / `Type::decode`), so a
//! capture round-trips a value back to itself given the definition's field types.
//!
//! Each field is decoded through an *optional* wrapper of its declared type: a
//! stored row may hold `none` in a non-optional field (admission fills every
//! declared field, §5.1). A `none` is written as *absence* — an omitted member,
//! not a `{ "$none": true }` sentinel (SPEC-ISSUES item 29; the sentinel is gone)
//! — so the optional wrapper is exactly what lets the shared decoder read that
//! omitted member back as `none` without a schema-fragility special case.
//!
//! CORE scope mirrors the rest of the engine: top-level keyed collections with
//! scalar/ref/set fields, plus the §8.2 package-root singleton reserved row (its
//! own scalar/ref/set/static-struct members). Nested collections (§5.4) are a
//! documented seam this build does not carry through a capture — but the seam is
//! **fail-closed**, not fail-open: [`StateSection::capture`] refuses (a
//! [`CaptureError::NestedRows`]) when an instance actually holds nested rows,
//! rather than emitting a capture that silently drops them. So a migration
//! rejects and an export errors instead of committing or exporting with the
//! nested data gone (§20.1 "the compatible value is copied", §22.1
//! committed-state integrity). Faithful nested-collection carry-through is a
//! tracked feature, not a bug.

use std::collections::BTreeMap;

use liasse_ident::NameSegment;
use liasse_model::{Model, Node};
use liasse_store::{CollectionPath, InstanceStore, RowAddress, StoreError};
use liasse_value::Type;
use serde_json::Value as J;

use crate::compiled::{Compiled, CompiledCollection};
use crate::error::EngineError;
use crate::materialize::{self, FieldMap};
use crate::schema::Schema;
use crate::state::Prospective;

/// A portable capture of one instance's committed writable state: every
/// top-level collection's rows in Annex B order, and the §8.2 package-root
/// singleton reserved row (absent when the package declares no singleton state).
pub(crate) struct StateSection {
    collections: Vec<(String, Vec<FieldMap>)>,
    /// The §8.2 singleton reserved row — the package root's writable scalar/ref/
    /// set/static-struct members folded into one struct — as gathered under
    /// [`crate::singleton::path`]. `None` when the instance holds no singleton row
    /// (a package with no writable root member), so nothing is emitted or staged.
    singleton: Option<FieldMap>,
}

/// Why a portable state capture could not be produced.
///
/// A capture that would silently omit committed rows is refused rather than
/// returned incomplete. The CORE portable path carries top-level keyed
/// collections and the §8.2 singleton, so an instance holding **nested** keyed-
/// collection rows (§5.4) cannot be captured faithfully in this build. Returning
/// [`Self::NestedRows`] instead of a lossy capture keeps every caller fail-closed
/// — a migration rejects and an export errors — rather than committing or
/// emitting an artifact that has dropped live data (§20.1/§22.1).
pub(crate) enum CaptureError {
    /// The store faulted while scanning committed rows.
    Store(StoreError),
    /// The instance holds committed rows in a nested keyed collection (§5.4) this
    /// build does not carry through a capture; the message names the first such
    /// row's address.
    NestedRows(String),
}

impl From<CaptureError> for EngineError {
    /// An export or merge surfaces a capture refusal as an [`EngineError`]: a
    /// store fault stays a store error, a nested-row refusal becomes the
    /// fail-closed [`EngineError::Unsupported`] (§19.5/§20.1/§22.1).
    fn from(error: CaptureError) -> Self {
        match error {
            CaptureError::Store(error) => Self::Store(error),
            CaptureError::NestedRows(detail) => Self::Unsupported(detail),
        }
    }
}

impl StateSection {
    /// Capture the committed rows of every top-level collection and the §8.2
    /// singleton reserved row from `store`.
    ///
    /// Refuses ([`CaptureError::NestedRows`]) when the instance holds committed
    /// rows in a nested keyed collection (§5.4) the CORE portable path does not
    /// carry, so a migration or export fails closed rather than silently dropping
    /// that live data (§20.1/§22.1). See the module docs.
    pub(crate) fn capture<S: InstanceStore>(
        schema: Schema<'_>,
        store: &S,
    ) -> Result<Self, CaptureError> {
        let prospective = Prospective::gather(store, schema).map_err(CaptureError::Store)?;
        // Fail-closed on the nested-collection seam. `Prospective::gather` descends
        // into nested keyed collections (§5.4), so a nested row is present in the
        // working copy even though the loop below selects only top-level collections
        // and the §8.2 singleton. A nested row is addressed below the top level
        // (`depth() > 1`; every top-level row and the singleton row are depth 1), so
        // its presence is exactly the condition under which emitting this capture
        // would drop committed data — refuse instead of losing it silently.
        if let Some(nested) = prospective.working().keys().find(|address| address.depth() > 1) {
            return Err(CaptureError::NestedRows(format!(
                "instance holds committed rows in a nested keyed collection at `{}`; migration and \
                 export do not carry nested keyed collections (§5.4) through in this build, so the \
                 operation is refused to avoid silent data loss (§20.1/§22.1)",
                nested.render()
            )));
        }
        let mut collections = Vec::new();
        for member in &schema.model().root().members {
            if !matches!(&member.node, Node::Collection(_)) {
                continue;
            }
            let name = member.name.as_str();
            let path = CollectionPath::top(NameSegment::new(name));
            let rows = prospective
                .addresses_in(&path)
                .into_iter()
                .filter_map(|address| prospective.get(&address).cloned())
                .collect();
            collections.push((name.to_owned(), rows));
        }
        // §8.2: `Prospective::gather` scans the singleton reserved row under
        // `singleton::path()` into its working copy at `singleton::address()`;
        // capture it through the same address so the artifact carries the durable
        // root state the store persist/restart path already keeps.
        let singleton = prospective.get(&crate::singleton::address()).cloned();
        Ok(Self { collections, singleton })
    }

    /// The captured collections, name and rows.
    pub(crate) fn collections(&self) -> &[(String, Vec<FieldMap>)] {
        &self.collections
    }

    /// The captured §8.2 root singleton reserved row, or `None` when the instance
    /// holds no singleton state. The singleton is not a keyed collection, so it is
    /// absent from [`Self::collections`]; a caller reasoning over ALL captured live
    /// state (e.g. the §20.2 downgrade representability gate) reads it here.
    pub(crate) fn singleton(&self) -> Option<&FieldMap> {
        self.singleton.as_ref()
    }

    /// Serialize to canonical strict-JSON bytes for the artifact state section.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut object = serde_json::Map::new();
        for (name, rows) in &self.collections {
            let wire = rows
                .iter()
                .map(|fields| materialize::struct_of(fields).to_wire())
                .collect();
            object.insert(name.clone(), J::Array(wire));
        }
        // §8.2: the singleton is one struct row, not a collection, so it serializes
        // as a single object under the reserved `$root` name. That name is
        // `$`-prefixed, which no application collection member can carry, so it never
        // collides with a collection entry above.
        if let Some(fields) = &self.singleton {
            object.insert(
                crate::singleton::ROOT_NAME.to_owned(),
                materialize::struct_of(fields).to_wire(),
            );
        }
        serde_json::to_vec(&J::Object(object)).unwrap_or_default()
    }

    /// Decode a state section against a definition's compiled field types and
    /// model (the model resolves the §8.2 singleton row's decode type).
    pub(crate) fn from_bytes(
        bytes: &[u8],
        compiled: &Compiled,
        model: &Model,
    ) -> Result<Self, EngineError> {
        let root: J = serde_json::from_slice(bytes)
            .map_err(|error| EngineError::Internal(format!("state section is not JSON: {error}")))?;
        let object = root
            .as_object()
            .ok_or_else(|| EngineError::Internal("state section must be a JSON object".to_owned()))?;
        let mut collections = Vec::new();
        for collection in &compiled.collections {
            let Some(J::Array(rows)) = object.get(&collection.name) else {
                continue;
            };
            let ty = Self::row_type(collection);
            let mut decoded = Vec::with_capacity(rows.len());
            for row in rows {
                let value = ty.decode(row).map_err(|error| {
                    EngineError::Internal(format!("state row in `{}`: {error}", collection.name))
                })?;
                decoded.push(materialize::fields_of(&value));
            }
            collections.push((collection.name.clone(), decoded));
        }
        // §8.2: decode the singleton reserved row, if the section carries one,
        // through its optional-wrapped struct type — the same `Type::decode`
        // discipline as a collection row, so a stored `none` (dropped from the wire
        // by absence) round-trips back to `none`.
        let singleton = match object.get(crate::singleton::ROOT_NAME) {
            Some(row) => {
                let value = crate::singleton::row_type(model).decode(row).map_err(|error| {
                    EngineError::Internal(format!("state singleton row: {error}"))
                })?;
                Some(materialize::fields_of(&value))
            }
            None => None,
        };
        Ok(Self { collections, singleton })
    }

    /// The captured rows re-addressed to their key positions, ready to stage.
    pub(crate) fn working(&self, schema: Schema<'_>) -> Result<BTreeMap<RowAddress, FieldMap>, EngineError> {
        let mut working = BTreeMap::new();
        for (name, rows) in &self.collections {
            let Some(model) = schema.top_collection(name) else { continue };
            for fields in rows {
                let key = materialize::row_key(model, fields).ok_or_else(|| {
                    EngineError::Internal(format!("captured row in `{name}` is missing a key field"))
                })?;
                working.insert(materialize::top_address(name, key), fields.clone());
            }
        }
        // §8.2: the singleton reserved row is keyed by its own reserved address, not
        // a model key field, so it re-addresses directly — the same address the
        // store persist path stages it under (`singleton::address`).
        if let Some(fields) = &self.singleton {
            working.insert(crate::singleton::address(), fields.clone());
        }
        Ok(working)
    }

    /// The optional-wrapped struct type used to decode one collection's rows: a
    /// stored non-optional field may hold `none`, so wrapping each declared member
    /// type in [`Type::Optional`] keeps the shared decoder total over captured rows.
    ///
    /// The row's declared members are its scalar/ref/set `fields` **and** its §5.3
    /// static struct members (`structs`) — a static struct compiles into
    /// `collection.structs`, not `fields`. [`StateSection::to_bytes`] serializes
    /// every member of a row (`materialize::struct_of`), struct members included, so
    /// the decode type must carry them too or `Type::Struct::decode` rejects the
    /// serialized struct as an unexpected member and the artifact cannot restore
    /// (§19.5/§19.10). Both member kinds feed the one decode-type builder the §8.2
    /// singleton path uses ([`crate::singleton::optional_decode_struct`]), which
    /// recursively optional-wraps a struct member's own members — so a keyed
    /// collection's static struct round-trips exactly as a singleton's does.
    fn row_type(collection: &CompiledCollection) -> Type {
        let fields = collection.fields.iter().map(|field| (field.name.clone(), field.ty.clone()));
        let structs = collection.structs.iter().map(|structure| (structure.name.clone(), structure.ty()));
        Type::Struct(crate::singleton::optional_decode_struct(fields.chain(structs)))
    }
}
