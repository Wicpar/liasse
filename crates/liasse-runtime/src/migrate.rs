//! Package evolution and migrations (§20).
//!
//! [`Engine::update`] loads a target definition over the active instance and
//! commits the migrated state atomically (§20.1, §20.3). The migration order the
//! spec pins is honoured: the compatible same-identity copy first, then each
//! field's local `$from` mapping (with an optional `$as` transform), then
//! insertion defaults fill any added field (§5.1). A declared inverse `$back` is
//! verified per migrated value (`$back($as(x)) == x`, §20.2); a failed round trip
//! rejects the whole migration. The complete prospective target is then checked
//! under the ordinary key/ref/uniqueness/check pipeline before the update commits.
//!
//! CORE scope: field-level `$from`/`$as`/`$back` mappings and collection renames
//! via `$from`, over top-level keyed collections. The package-level `$migrations`
//! program (splits/merges reading `$old`) needs the two-model program runtime and
//! is a documented seam; the Annex E contract-narrowing check (§20.3) likewise
//! needs the typed effective-contract comparison and is left to the model layer.

use std::collections::BTreeMap;

use liasse_artifact::{CompatibilityDecision, PackageIdentity, PackageName, UpdateRelation, Version};
use liasse_diag::SourceMap;
use liasse_expr::{check_statement, Cell, ExprType, TypedExpr};
use liasse_model::{Model, PackageId};
use liasse_store::{CommitSeq, InstanceStore, RowAddress};
use liasse_syntax::{parse_document, parse_expression};
use liasse_value::Value;

use crate::compiled::{Compiled, CompiledCollection};
use crate::doc;
use crate::engine::{compile_definition, Compilation, Engine};
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::materialize::{self, FieldMap};
use crate::portable::StateSection;
use crate::rules;
use crate::schema::Schema;
use crate::scope::RuntimeScope;
use crate::state::Prospective;

/// A failure to update a package instance (§20.3, §9.4).
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The target definition is statically invalid or the store faulted.
    #[error(transparent)]
    Engine(EngineError),
    /// The prospective migrated state was refused by the admission pipeline —
    /// a failed check, dangling ref, uniqueness collision, or a failed
    /// reversible round trip (§20.1 final check, §20.2).
    #[error("migration rejected: {}", .0.message())]
    Rejected(Rejection),
    /// The target is on a different compatibility line, so no update relation
    /// exists (§19.8 unrelated; an update is not an install).
    #[error("incompatible update: {0}")]
    Incompatible(String),
}

/// The observable result of a successful update (§20.3, §13.15 shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    /// The Annex E version relationship of the target to the active package.
    pub relation: UpdateRelation,
    /// The commit the update took.
    pub commit: CommitSeq,
}

impl<S: InstanceStore> Engine<S> {
    /// Update this instance to a target definition (§20). Builds the migrated
    /// state through the §20.1 order, verifies reversible transforms, admits the
    /// result through the ordinary rule pipeline, and commits it atomically as the
    /// new active definition. A rejected migration leaves the instance unchanged.
    pub fn update<G: crate::generator::Generators>(
        &mut self,
        target: &str,
        generator: &mut G,
    ) -> Result<UpdateReport, UpdateError> {
        let compilation = compile_definition(target).map_err(UpdateError::Engine)?;
        let decision = compatibility(self.model(), &compilation.model)?;
        let plan = MigrationPlan::read(target).map_err(UpdateError::Engine)?;
        let old_state =
            StateSection::capture(self.schema(), self.store()).map_err(|e| UpdateError::Engine(EngineError::Store(e)))?;
        let migrated = build_migrated(self.compiled(), &old_state, &compilation, &plan, generator, self.now())
            .map_err(UpdateError::Rejected)?;
        let commit = self
            .apply_migration(target, compilation, migrated)
            .map_err(UpdateError::Engine)?;
        Ok(UpdateReport { relation: decision.relation, commit })
    }
}

/// Classify the update from the active package to the target (Annex E, §20.3).
fn compatibility(active: &Model, target: &Model) -> Result<CompatibilityDecision, UpdateError> {
    let active = identity(&active.header().identity).map_err(UpdateError::Engine)?;
    let target = identity(&target.header().identity).map_err(UpdateError::Engine)?;
    let decision = CompatibilityDecision::classify(&active, &target);
    if matches!(decision.relation, UpdateRelation::Unrelated) {
        return Err(UpdateError::Incompatible(format!(
            "`{}` is a different compatibility line than the active package",
            target.name.as_str()
        )));
    }
    Ok(decision)
}

/// Convert a model package identity to the artifact-layer identity the Annex E
/// decision runs over.
fn identity(id: &PackageId) -> Result<PackageIdentity, EngineError> {
    let name = PackageName::parse(id.name.as_str())
        .map_err(|error| EngineError::Internal(format!("package name: {error}")))?;
    let version = Version::new(id.version.major, id.version.minor, id.version.patch);
    Ok(PackageIdentity::new(name, version))
}

/// Build the prospective migrated state (§20.1 order) and verify reversible
/// transforms, returning the addressed rows to stage.
fn build_migrated<G: crate::generator::Generators>(
    old_compiled: &Compiled,
    old_state: &StateSection,
    target: &Compilation,
    plan: &MigrationPlan,
    generator: &mut G,
    now: liasse_value::Timestamp,
) -> Result<BTreeMap<RowAddress, FieldMap>, Rejection> {
    let schema = Schema::new(&target.model);
    let ctx = EvalCtx {
        schema,
        compiled: &target.compiled,
        params: BTreeMap::new(),
        now,
        seed: generator.next_seed(),
    };
    let root_ty = ExprType::Row(schema.root_row_type());
    let mut sources = SourceMap::new();
    let mut prospective = Prospective::empty();
    let mut touched = Vec::new();

    for collection in &target.compiled.collections {
        let migration = plan.collections.get(&collection.name);
        let source_name = migration
            .and_then(|m| m.from.clone())
            .unwrap_or_else(|| collection.name.clone());
        let Some(old_rows) = old_state
            .collections()
            .iter()
            .find(|(name, _)| *name == source_name)
            .map(|(_, rows)| rows)
        else {
            continue;
        };
        let old_collection = old_compiled.collection(&source_name);
        for old_row in old_rows {
            let mut fields = map_row(collection, migration, old_row, old_collection, &ctx, &mut sources, &root_ty)?;
            rules::apply_defaults(collection, &mut fields, &ctx, &prospective)?;
            rules::normalize_all(collection, &mut fields, &ctx, &prospective)?;
            let address = key_address(schema, collection, &fields)?;
            if prospective.contains(&address) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "migration produced a duplicate key")
                    .at(address.render()));
            }
            prospective.insert(address.clone(), fields);
            touched.push(address);
        }
    }

    rules::finalize(&target.compiled, &ctx, &prospective, &touched)?;
    Ok(touched
        .into_iter()
        .filter_map(|address| prospective.get(&address).map(|fields| (address.clone(), fields.clone())))
        .collect())
}

/// Map one source row to the target row (§20.1): the compatible same-name copy,
/// then each declared local `$from` mapping with its optional `$as` transform.
fn map_row(
    collection: &CompiledCollection,
    migration: Option<&CollectionMigration>,
    old_row: &FieldMap,
    old_collection: Option<&CompiledCollection>,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
) -> Result<FieldMap, Rejection> {
    let mut fields = FieldMap::new();
    for field in &collection.fields {
        let mapping = migration.and_then(|m| m.fields.get(&field.name));
        if let Some(mapping) = mapping {
            let Some(source) = old_row.get(&mapping.from) else { continue };
            let value = match &mapping.transform {
                Some(text) => {
                    let old_ty = old_collection
                        .and_then(|c| c.field(&mapping.from))
                        .map_or(liasse_value::Type::Json, |f| f.ty.clone());
                    let transformed = transform(text, source, old_ty.clone(), ctx, sources, root_ty)?;
                    if let Some(back) = &mapping.back {
                        verify_reversible(back, source, &transformed, &field.ty, ctx, sources, root_ty)?;
                    }
                    transformed
                }
                None => source.clone(),
            };
            fields.insert(field.name.clone(), value);
        } else if let Some(value) = old_row.get(&field.name) {
            // §20.1 compatible same-identity copy.
            fields.insert(field.name.clone(), value.clone());
        }
    }
    Ok(fields)
}

/// Evaluate a `$as`/`$from` transform expression with `.` bound to the old value.
fn transform(
    text: &str,
    old_value: &Value,
    old_ty: liasse_value::Type,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
) -> Result<Value, Rejection> {
    let typed = compile(text, old_ty, ctx, sources, root_ty)?;
    let cell = ctx.eval(&Prospective::empty(), &typed, &Cell::Scalar(old_value.clone()))?;
    Ok(scalar(cell))
}

/// Verify a reversible transform round trip (§20.2): `$back($as(x)) == x`.
fn verify_reversible(
    back: &str,
    original: &Value,
    transformed: &Value,
    target_ty: &liasse_value::Type,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
) -> Result<(), Rejection> {
    let restored = transform(back, transformed, target_ty.clone(), ctx, sources, root_ty)?;
    if restored == *original {
        Ok(())
    } else {
        Err(Rejection::new(
            RejectionReason::Check,
            "reversible transform round trip `$back($as(x)) == x` failed",
        ))
    }
}

/// Type-check a transform expression whose `.` is a scalar of `dot_ty`.
fn compile(
    text: &str,
    dot_ty: liasse_value::Type,
    _ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
) -> Result<TypedExpr, Rejection> {
    let src = sources.add_label("migration", text.to_owned());
    let parsed = parse_expression(src, text).map_err(|d| {
        Rejection::new(RejectionReason::Malformed, format!("migration transform: {}", d.render(sources)))
    })?;
    let scope = RuntimeScope::new(ExprType::scalar(dot_ty), root_ty.clone());
    check_statement(&scope, src, &parsed).map_err(|d| {
        Rejection::new(RejectionReason::TypeError, format!("migration transform: {}", d.render(sources)))
    })
}

fn key_address(
    schema: Schema<'_>,
    collection: &CompiledCollection,
    fields: &FieldMap,
) -> Result<RowAddress, Rejection> {
    let model = schema
        .top_collection(&collection.name)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "unknown target collection"))?;
    let key = materialize::row_key(model, fields)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, "migrated row is missing a key field"))?;
    Ok(materialize::top_address(&collection.name, key))
}

fn scalar(cell: Cell) -> Value {
    match cell {
        Cell::Scalar(value) => value,
        _ => Value::None,
    }
}

/// The parsed migration mappings of a target definition (§20.1): per collection,
/// an optional collection rename and each field's `$from`/`$as`/`$back`.
struct MigrationPlan {
    collections: BTreeMap<String, CollectionMigration>,
}

/// One collection's migration: its optional source collection and field mappings.
#[derive(Default)]
struct CollectionMigration {
    from: Option<String>,
    fields: BTreeMap<String, FieldMigration>,
}

/// One field's local migration mapping (§20.1).
struct FieldMigration {
    from: String,
    transform: Option<String>,
    back: Option<String>,
}

impl MigrationPlan {
    /// Read the `$from`/`$as`/`$back` mappings out of a target definition's
    /// `$model`, which the compiled form discards.
    fn read(definition: &str) -> Result<Self, EngineError> {
        let mut sources = SourceMap::new();
        let src = sources.add_file("liasse.json", definition.to_owned());
        let document =
            parse_document(src, definition).map_err(|d| EngineError::Invalid(Box::new(d)))?;
        let mut collections = BTreeMap::new();
        let Some(model) = doc::member(document.root(), "$model") else {
            return Ok(Self { collections });
        };
        let Some(members) = doc::object(model) else {
            return Ok(Self { collections });
        };
        for member in members {
            let Some(shape) = doc::object(&member.value) else { continue };
            let migration = Self::read_collection(shape);
            if migration.from.is_some() || !migration.fields.is_empty() {
                collections.insert(member.name.text.clone(), migration);
            }
        }
        Ok(Self { collections })
    }

    fn read_collection(shape: &[liasse_syntax::DocMember]) -> CollectionMigration {
        let mut migration = CollectionMigration::default();
        for member in shape {
            if member.name.text == "$from" {
                migration.from = doc::string(&member.value).map(str::to_owned);
                continue;
            }
            if member.name.text.starts_with('$') {
                continue;
            }
            if let Some(field) = Self::read_field(&member.value) {
                migration.fields.insert(member.name.text.clone(), field);
            }
        }
        migration
    }

    fn read_field(value: &liasse_syntax::DocValue) -> Option<FieldMigration> {
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
