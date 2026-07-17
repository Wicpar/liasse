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
use crate::contract::BoundaryContract;
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
        // §16.2/§20: the target keeps the context's registered components but
        // declares its own `$requires`, re-resolved by [`apply_migration`]. The
        // target compilation itself does not re-type its host-call views/defaults
        // against the live registry here — a target whose views call an unregistered
        // namespace fails to compile as an unknown function, which is the correct
        // load rejection (a resolvable host-call view under migration is a seam).
        let compilation =
            compile_definition(target, &crate::host::HostSignatures::default()).map_err(UpdateError::Engine)?;
        let decision = compatibility(self.model(), &compilation.model)?;
        // §13.14/§20.3/Annex E: a same-major forward move (minor or patch) MUST
        // preserve or widen every exposed boundary contract. Reject a narrowing
        // release before activation (E.9) so the current package stays active.
        if decision.is_line_forward()
            && let Some(reason) = self.boundary_narrowing(target, &compilation)
        {
            return Err(UpdateError::Rejected(Rejection::new(
                RejectionReason::Compatibility,
                format!("update narrows the boundary contract: {reason}"),
            )));
        }
        let old_state =
            StateSection::capture(self.schema(), self.store()).map_err(|e| UpdateError::Engine(EngineError::Store(e)))?;
        // §20.2: a downgrade loads the older package and applies an explicit direct
        // migration or the available exact inverses the *active* package declared
        // (`$from`/`$back`). Build that combined plan and reject the downgrade when a
        // populated live field the older shape cannot represent has no such transform.
        let plan = if decision.relation == UpdateRelation::Downgrade {
            let active = self.definition_source().ok_or_else(|| {
                UpdateError::Engine(EngineError::Internal("active definition unavailable for downgrade".to_owned()))
            })?;
            let plan = downgrade_plan(&active, target).map_err(UpdateError::Engine)?;
            downgrade_representable(self.compiled(), &old_state, &compilation, &plan)
                .map_err(UpdateError::Rejected)?;
            plan
        } else {
            MigrationPlan::read(target).map_err(UpdateError::Engine)?
        };
        let migrated = build_migrated(self.compiled(), &old_state, &compilation, &plan, generator, self.now())
            .map_err(UpdateError::Rejected)?;
        let commit = self
            .apply_migration(target, compilation, migrated)
            .map_err(UpdateError::Engine)?;
        Ok(UpdateReport { relation: decision.relation, commit })
    }

    /// The first boundary-contract narrowing the `target` release makes relative
    /// to the active one (Annex E.2), or `None` when it preserves or widens every
    /// exposed contract. The active contract is read from the currently active
    /// definition, so the comparison is against the release in force (E.9), and a
    /// two-hop widen-then-narrow is caught at the second hop. A definition whose
    /// `$model` cannot be re-parsed yields `None` (the migration then fails its
    /// ordinary pipeline instead).
    fn boundary_narrowing(&self, target: &str, candidate: &Compilation) -> Option<String> {
        let active_definition = self.definition_source()?;
        let active_doc = model_document(&active_definition)?;
        let candidate_doc = model_document(target)?;
        let active = BoundaryContract::extract(self.compiled(), &active_doc);
        let candidate = BoundaryContract::extract(&candidate.compiled, &candidate_doc);
        active.narrowing(&candidate)
    }
}

/// Re-parse a definition text and return its `$model` document, or `None` when it
/// does not parse or declares no `$model`.
fn model_document(definition: &str) -> Option<liasse_syntax::DocValue> {
    let mut sources = SourceMap::new();
    let src = sources.add_file("liasse.json", definition.to_owned());
    let document = parse_document(src, definition).ok()?;
    doc::member(document.root(), "$model").cloned()
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
        // Migration builds the target's rows; a keyring selector does not
        // participate in state transforms, so the migrated-state context owns no
        // keyring index (§20.1).
        keyrings: &[],
        // A migration transform runs with no actor (§11.1).
        context: BTreeMap::new(),
        // A migration transform is a pure expression; it resolves no host call.
        hosts: crate::host::HostDispatch::none(now),
        // A migration builds the target instance's own rows; no installed-module
        // aggregate participates in a state transform.
        modules: None,
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

/// Build the downgrade migration plan (§20.2): the older target's own declared
/// mappings, augmented with the exact inverses the *active* package's field
/// transforms provide. An active field declared `$from: X` with an exact inverse
/// `$back: B` reconstructs the older field `X` as `B(<active field>)`; the
/// target's own mapping for `X` (an explicit direct downgrade migration) wins over
/// the inferred inverse. A collection rename on downgrade is a documented seam, so
/// an inverse is attributed to the same-named target collection.
fn downgrade_plan(active_definition: &str, target: &str) -> Result<MigrationPlan, EngineError> {
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

/// §20.2 downgrade representability: reject the downgrade when the older shape
/// cannot represent a populated live field and no declared transform preserves it.
///
/// A field of the active shape carrying a value in some live row is representable
/// only when the older shape keeps a field of the same name (the compatible copy),
/// or some target-field mapping reads it (an explicit direct migration, or an exact
/// inverse folded into [`downgrade_plan`]). A populated field with neither would be
/// silently discarded — the §20.2 asymmetry with a forward migration, which MAY
/// drop a source field because it survives in history — so the downgrade is
/// rejected and the current package stays active (E.9).
fn downgrade_representable(
    active_compiled: &Compiled,
    active_state: &StateSection,
    target: &Compilation,
    plan: &MigrationPlan,
) -> Result<(), Rejection> {
    for (name, rows) in active_state.collections() {
        let Some(active_collection) = active_compiled.collection(name) else { continue };
        let target_collection = target.compiled.collection(name);
        let migration = plan.collections.get(name);
        for field in &active_collection.fields {
            let populated = rows.iter().any(|row| row.get(&field.name).is_some_and(|value| *value != Value::None));
            if !populated {
                continue;
            }
            let kept = target_collection.is_some_and(|collection| collection.field(&field.name).is_some());
            let reconstructed =
                migration.is_some_and(|migration| migration.fields.values().any(|f| f.from == field.name));
            if !kept && !reconstructed {
                return Err(Rejection::new(
                    RejectionReason::Compatibility,
                    format!(
                        "downgrade drops populated field `{}` of `{name}`: the older shape cannot represent \
                         it and no declared downgrade transform preserves it",
                        field.name
                    ),
                ));
            }
        }
    }
    Ok(())
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
