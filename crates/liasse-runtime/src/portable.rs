//! Portable state capture: the opaque `state/current.cbor.zst` section of a
//! `.liasse` artifact (§19.5, §19.6, Annex D), owned by the runtime.
//!
//! The artifact layer treats the state section as opaque bytes verified by
//! checksum; deciding its encoding is the runtime's job. A [`StateSection`]
//! captures every collection's committed writable rows in Annex B key order —
//! top-level collections and, nested inside each row, the §5.4 keyed collections
//! living under it — and (de)serializes them through the same canonical
//! strict-JSON value codec the rest of the runtime uses (`Value::to_wire` /
//! `Type::decode`), so a capture round-trips a value back to itself given the
//! definition's field types.
//!
//! Each field is decoded through an *optional* wrapper of its declared type: a
//! stored row may hold `none` in a non-optional field (admission fills every
//! declared field, §5.1). A `none` is written as *absence* — an omitted member,
//! not a `{ "$none": true }` sentinel (SPEC-ISSUES item 29; the sentinel is gone)
//! — so the optional wrapper is exactly what lets the shared decoder read that
//! omitted member back as `none` without a schema-fragility special case.
//!
//! A capture carries the COMPLETE committed row tree: every top-level keyed
//! collection (including one adopted through a §5.8 `$types`/`$like` name), every
//! nested keyed collection under it at any depth ([`CapturedRow`]), and the §8.2
//! package-root singleton reserved row with its own scalar/ref/set/static-struct
//! members. Nothing committed is left behind, so a migration copies and an export
//! emits the whole instance (§20.1 "the compatible value is copied", §22.1
//! committed-state integrity).

use std::collections::BTreeMap;

use liasse_model::Model;
use liasse_store::{InstanceStore, RowAddress, StoreError};
use serde_json::Value as J;

use crate::captured::CapturedRow;
use crate::compiled::Compiled;
use crate::error::EngineError;
use crate::materialize::{self, FieldMap};
use crate::schema::Schema;
use crate::state::Prospective;

/// A portable capture of one instance's committed writable state: every
/// collection's row tree in Annex B order, and the §8.2 package-root singleton
/// reserved row (absent when the package declares no singleton state).
pub(crate) struct StateSection {
    collections: Vec<(String, Vec<CapturedRow>)>,
    /// The §8.2 singleton reserved row — the package root's writable scalar/ref/
    /// set/static-struct members folded into one struct — as gathered under
    /// [`crate::singleton::path`]. `None` when the instance holds no singleton row
    /// (a package with no writable root member), so nothing is emitted or staged.
    singleton: Option<FieldMap>,
}

impl StateSection {
    /// Capture the committed row tree of every collection and the §8.2 singleton
    /// reserved row from `store`.
    pub(crate) fn capture<S: InstanceStore>(
        schema: Schema<'_>,
        store: &S,
    ) -> Result<Self, StoreError> {
        let prospective = Prospective::gather(store, schema)?;
        let reserved = crate::singleton::address();
        let mut forest = CapturedRow::forest(prospective.working(), prospective.created(), &reserved)?;
        // §5.8: a top-level member naming a keyed shape (`companies: "company"`) IS a
        // collection, so it is captured like a directly-declared one — through the
        // same `resolved_collection` identity the gather and compile paths select by.
        // Selecting on the declared node form alone would omit an adopted
        // collection's whole shape from every capture.
        let collections: Vec<(String, Vec<CapturedRow>)> = schema
            .model()
            .root()
            .members
            .iter()
            .filter(|member| schema.resolved_collection(&member.node).is_some())
            .map(|member| {
                let name = member.name.as_str().to_owned();
                let rows = forest.remove(&name).unwrap_or_default();
                (name, rows)
            })
            .collect();
        // Every gathered tree must have been claimed by a declared collection above:
        // the gather scans only paths the model declares, so a leftover means the
        // store and the model disagree about what exists. Emitting the capture anyway
        // would drop those rows without a word — exactly the failure this whole path
        // exists to prevent — so it is reported instead (§22.1).
        if let Some(name) = forest.keys().next() {
            return Err(StoreError::Corruption {
                detail: format!(
                    "committed rows live in `{name}`, which the active model declares no collection \
                     for: capturing the instance would drop them silently (§22.1)"
                ),
            });
        }
        // §8.2: `Prospective::gather` scans the singleton reserved row under
        // `singleton::path()` into its working copy at `singleton::address()`;
        // capture it through the same address so the artifact carries the durable
        // root state the store persist/restart path already keeps.
        let singleton = prospective.get(&reserved).cloned();
        Ok(Self { collections, singleton })
    }

    /// The captured collections, name and row trees.
    pub(crate) fn collections(&self) -> &[(String, Vec<CapturedRow>)] {
        &self.collections
    }

    /// The captured row trees of the top-level collection named `name`, if the
    /// capture carries it — the source rows a §20.1 migration copies forward.
    pub(crate) fn collection(&self, name: &str) -> Option<&[CapturedRow]> {
        self.collections
            .iter()
            .find(|(captured, _)| captured == name)
            .map(|(_, rows)| rows.as_slice())
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
            object.insert(name.clone(), J::Array(rows.iter().map(CapturedRow::to_wire).collect()));
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
            let decoded: Result<Vec<CapturedRow>, EngineError> =
                rows.iter().map(|row| CapturedRow::from_wire(collection, row)).collect();
            collections.push((collection.name.clone(), decoded?));
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

    /// The captured rows re-addressed to their key positions, ready to stage —
    /// every nested row under the address of the parent it was captured beneath, so
    /// a restore reproduces the whole committed tree, not only its top level.
    pub(crate) fn working(
        &self,
        schema: Schema<'_>,
    ) -> Result<BTreeMap<RowAddress, FieldMap>, EngineError> {
        let mut working = BTreeMap::new();
        for (name, rows) in &self.collections {
            if schema.top_collection(name).is_none() {
                continue;
            }
            let mut path = vec![name.clone()];
            for row in rows {
                row.place(schema, &mut path, None, &mut working)?;
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
}
