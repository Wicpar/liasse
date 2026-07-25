//! Reading the §20.1 migration mappings out of a target definition's `$model`.
//!
//! The compiled form discards `$from`/`$as`/`$back`, so the plan is re-read from
//! the definition document. A mapping is declared per *collection* — at every
//! depth, because §5.4 makes a nested keyed collection a collection — and per §8.2
//! singleton root member, so the plan mirrors the model tree rather than flattening
//! it: a child collection's rename and its fields' transforms belong to the child,
//! which is exactly where the copy pass looks for them.

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_syntax::{parse_document, DocMember, DocValue};

use crate::doc;
use crate::error::EngineError;

/// The parsed migration mappings of a target definition (§20.1): per collection,
/// an optional collection rename and each field's `$from`/`$as`/`$back`, plus the
/// local mappings on §8.2 root singleton members.
pub(crate) struct MigrationPlan {
    pub(crate) collections: BTreeMap<String, CollectionMigration>,
    /// Local `$from`/`$as`/`$back` mappings on §8.2 root singleton members, keyed
    /// by the TARGET member name — the singleton analogue of a collection field's
    /// mapping, applied by the singleton carry loop in `build_migrated`.
    pub(crate) singleton_fields: BTreeMap<String, FieldMigration>,
}

/// One collection's migration: its optional source collection, its field
/// mappings, and the migrations of the nested keyed collections declared under it
/// (§5.4), keyed by the TARGET child name. A nested collection is a collection, so
/// it carries the same `$from` rename and the same per-field `$from`/`$as` as a
/// top-level one — the recursion is what keeps §20.1 depth-uniform.
#[derive(Default)]
pub(crate) struct CollectionMigration {
    pub(crate) from: Option<String>,
    pub(crate) fields: BTreeMap<String, FieldMigration>,
    pub(crate) children: BTreeMap<String, CollectionMigration>,
}

impl CollectionMigration {
    /// Whether this collection — or anything nested under it — declares a §20.1
    /// mapping worth carrying. A shape that declares none is not recorded, so the
    /// copy pass distinguishes "no mapping" from "an empty mapping".
    pub(crate) fn declares_a_mapping(&self) -> bool {
        self.from.is_some() || !self.fields.is_empty() || !self.children.is_empty()
    }
}

/// One field's local migration mapping (§20.1).
pub(crate) struct FieldMigration {
    pub(crate) from: String,
    pub(crate) transform: Option<String>,
    pub(crate) back: Option<String>,
}

impl MigrationPlan {
    /// Read the `$from`/`$as`/`$back` mappings out of a target definition's
    /// `$model`, which the compiled form discards.
    pub(crate) fn read(definition: &str) -> Result<Self, EngineError> {
        let mut sources = SourceMap::new();
        let src = sources.add_file("liasse.json", definition.to_owned());
        let document =
            parse_document(src, definition).map_err(|d| EngineError::Invalid(Box::new(d)))?;
        let mut collections = BTreeMap::new();
        let mut singleton_fields = BTreeMap::new();
        let Some(model) = doc::member(document.root(), "$model") else {
            return Ok(Self { collections, singleton_fields });
        };
        let Some(members) = doc::object(model) else {
            return Ok(Self { collections, singleton_fields });
        };
        for member in members {
            let Some(shape) = doc::object(&member.value) else { continue };
            // §5.4 vs §8.2: a top-level member declaring `$key` is a keyed
            // collection — its `$from` is a collection rename and its field members
            // carry their own mappings. A top-level member with no `$key` but a
            // `$from` is a §8.2 singleton member rename/transform. Routing the
            // singleton member here — rather than mis-filing its `{ $type, $from }`
            // object under `collections`, where the singleton carry never reads it —
            // is what lets a singleton `$from` copy/transform its value like a
            // collection field (§20.1).
            if Self::is_collection(shape) {
                let migration = Self::read_collection(shape);
                if migration.declares_a_mapping() {
                    collections.insert(member.name.text.clone(), migration);
                }
            } else if let Some(field) = Self::read_field(&member.value) {
                singleton_fields.insert(member.name.text.clone(), field);
            }
        }
        Ok(Self { collections, singleton_fields })
    }

    fn read_collection(shape: &[DocMember]) -> CollectionMigration {
        let mut migration = CollectionMigration::default();
        for member in shape {
            if member.name.text == "$from" {
                migration.from = doc::string(&member.value).map(str::to_owned);
                continue;
            }
            if member.name.text.starts_with('$') {
                continue;
            }
            // §5.4: a member declaring its own `$key` is a NESTED collection, not a
            // field — its `$from` renames the child collection and its members carry
            // their own mappings. Reading it as a field would file a child rename
            // under `fields`, where the copy pass never looks for it.
            match doc::object(&member.value) {
                Some(nested) if Self::is_collection(nested) => {
                    let child = Self::read_collection(nested);
                    if child.declares_a_mapping() {
                        migration.children.insert(member.name.text.clone(), child);
                    }
                }
                _ => {
                    if let Some(field) = Self::read_field(&member.value) {
                        migration.fields.insert(member.name.text.clone(), field);
                    }
                }
            }
        }
        migration
    }

    /// Whether a `$model` shape object declares a keyed collection (§5.4): it
    /// carries a `$key`. The one discriminator between a collection member and a
    /// field/struct member in the raw document.
    fn is_collection(shape: &[DocMember]) -> bool {
        shape.iter().any(|member| member.name.text == "$key")
    }

    fn read_field(value: &DocValue) -> Option<FieldMigration> {
        let members = doc::object(value)?;
        let from = members
            .iter()
            .find(|m| m.name.text == "$from")
            .and_then(|m| doc::string(&m.value))?
            .to_owned();
        let transform = members
            .iter()
            .find(|m| m.name.text == "$as")
            .and_then(|m| doc::string(&m.value))
            .map(str::to_owned);
        let back = members
            .iter()
            .find(|m| m.name.text == "$back")
            .and_then(|m| doc::string(&m.value))
            .map(str::to_owned);
        Some(FieldMigration { from, transform, back })
    }
}

/// Build the downgrade migration plan (§20.2): the older target's own declared
/// mappings, augmented with the exact inverses the *active* package's field
/// transforms provide. An active field declared `$from: X` with an exact inverse
/// `$back: B` reconstructs the older field `X` as `B(<active field>)`; the
/// target's own mapping for `X` (an explicit direct downgrade migration) wins over
/// the inferred inverse. A collection rename on downgrade is a documented seam, so
/// an inverse is attributed to the same-named target collection.
pub(crate) fn downgrade_plan(active_definition: &str, target: &str) -> Result<MigrationPlan, EngineError> {
    let mut plan = MigrationPlan::read(target)?;
    let active = MigrationPlan::read(active_definition)?;
    for (collection, migration) in active.collections {
        for (active_field, mapping) in migration.fields {
            let Some(back) = mapping.back else { continue };
            let target_collection = plan.collections.entry(collection.clone()).or_default();
            target_collection.fields.entry(mapping.from).or_insert(FieldMigration {
                from: active_field,
                transform: Some(back),
                back: None,
            });
        }
    }
    Ok(plan)
}
