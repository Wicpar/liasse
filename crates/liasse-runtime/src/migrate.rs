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
//! The package-level `$migrations` program (§20.1) also runs here: the target's
//! program for the exact active source version executes over the prospective
//! target with `$old` bound to the read-only source state, so splits, merges, and
//! coordinated collection transforms commit atomically. The `$as`/`$back`/program
//! transforms resolve the built-in codec namespaces (§16.1) — `base64`,
//! `string.bytes`, and their inverses — seeded like the built-in cose contract.
//!
//! CORE scope: top-level keyed collections. `$old` is materialized as the source
//! model's stored collections (its computed values and views are a documented
//! seam); the Annex E contract-narrowing check (§20.3) uses the typed
//! effective-contract comparison in [`BoundaryContract`].

use std::collections::BTreeMap;

use liasse_artifact::{CompatibilityDecision, PackageIdentity, PackageName, UpdateRelation, Version};
use liasse_diag::SourceMap;
use liasse_expr::{check_statement, Cell, DbReadPosition, ExprType, HostPosition, Row, RowType, TypedExpr};
use liasse_model::{nondeterministic_call, Model, PackageId};
use liasse_store::{CommitSeq, InstanceStore, RowAddress};
use liasse_syntax::{parse_document, parse_expression};
use liasse_value::{Timestamp, Type, Value};

use crate::compiled::{Compiled, CompiledCollection, CompiledMutation, CompiledStmt};
use crate::contract::BoundaryContract;
use crate::doc;
use crate::engine::{compile_definition, Compilation, Engine};
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::host::{HostBinding, HostDispatch, HostSignatures};
use crate::interp::{rewrite_inbound_refs_across, Interp};
use crate::materialize::{self, FieldMap};
use crate::portable::{CaptureError, StateSection};
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
    /// The canonical display paths of rows a declared migration transform produced
    /// (§13.15 `$migrated`), in canonical path order. A §20.1 compatible
    /// same-identity copy is not a migrated row — only a `$from`/`$as` field mapping
    /// or a `$migrations` program write is — so a shape-compatible update reports an
    /// empty list.
    pub migrated: Vec<String>,
    /// The canonical display paths of seed rows the §13.13 apply-if-absent pass
    /// inserted at an address the instance did not already hold (§13.15 `$seeded`),
    /// in canonical path order.
    pub seeded: Vec<String>,
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
        let compilation = compile_definition(target, &crate::host::HostSignatures::default(), crate::imports::EMPTY.types())
            .map_err(UpdateError::Engine)?;
        let decision = compatibility(self.model(), &compilation.model)?;
        // §20.1/§20.3/Annex E.9: an in-place update is compatible only when a
        // CONNECTED §20.1 delta path exists between the active version and the
        // target — the single implicit structural-diff delta (no declared
        // `$migrations` key strictly between the two held-model endpoints) or a
        // delta declared for the exact active source version. A package off the
        // target's declared lineage (a declared key sits strictly between with no
        // delta from the active version) has NO such path even when it is
        // shape-compatible, so refuse it and leave the active package in force
        // (§9.4, Annex E.9) rather than run a single-hop compatible copy over an
        // undeclared route. The runtime never synthesizes an undeclared
        // intermediate version.
        if !self.connected_delta_path(target, &compilation.model) {
            let active = &self.model().header().identity.version;
            let want = &compilation.model.header().identity.version;
            return Err(UpdateError::Rejected(Rejection::new(
                RejectionReason::Compatibility,
                format!(
                    "no connected §20.1 delta path from the active `{}.{}.{}` to the target \
                     `{}.{}.{}`: the active version is off the target's declared migration \
                     lineage and the runtime never synthesizes an undeclared intermediate \
                     (§20.3, Annex E.9)",
                    active.major, active.minor, active.patch, want.major, want.minor, want.patch,
                ),
            )));
        }
        // §13.14/§20.3/Annex E: a same-major forward move (minor or patch) MUST
        // preserve or widen every exposed boundary contract, and Annex E.1 holds
        // a same-version republish to the identical gate. Reject a narrowing
        // release before activation (E.9) so the current package stays active.
        if decision.requires_non_narrowing()
            && let Some(reason) = self.boundary_narrowing(target, &compilation)
        {
            return Err(UpdateError::Rejected(Rejection::new(
                RejectionReason::Compatibility,
                format!("update narrows the boundary contract: {reason}"),
            )));
        }
        // §20.1/§22.1 fail-closed: a nested keyed collection (§5.4) holding committed
        // rows cannot be carried through this build's capture, so refuse the whole
        // migration rather than commit with those rows silently dropped. Every other
        // capture failure is a store fault.
        let old_state = StateSection::capture(self.schema(), self.store()).map_err(|error| match error {
            CaptureError::Store(error) => UpdateError::Engine(EngineError::Store(error)),
            CaptureError::NestedRows(message) => {
                UpdateError::Rejected(Rejection::new(RejectionReason::Unsupported, message))
            }
        })?;
        // §20.2: a downgrade loads the older package and applies an explicit direct
        // migration or the available exact inverses the *active* package declared
        // (`$from`/`$back`). Build that combined plan and reject the downgrade when a
        // populated live field the older shape cannot represent has no such transform.
        let plan = if decision.relation == UpdateRelation::Downgrade {
            let active = self.definition_source().map_err(UpdateError::Engine)?.ok_or_else(|| {
                UpdateError::Engine(EngineError::Internal("active definition unavailable for downgrade".to_owned()))
            })?;
            let plan = downgrade_plan(&active, target).map_err(UpdateError::Engine)?;
            downgrade_representable(self.compiled(), self.model(), &old_state, &compilation, &plan)
                .map_err(UpdateError::Rejected)?;
            plan
        } else {
            MigrationPlan::read(target).map_err(UpdateError::Engine)?
        };
        // §20.1: the migration selects the package-level program keyed to the
        // exact active source version. Capture it before staging so the target's
        // `$migrations` can read `$old` under the source model.
        let active_source = {
            let version = &self.model().header().identity.version;
            format!("{}.{}.{}", version.major, version.minor, version.patch)
        };
        // §20.1: `$old` is the complete read-only source state — it MAY read ANY
        // `$old` view, not only stored collections. Materialize the fully-folded
        // source root (computed values, root computed, nested/declared views) through
        // the live source engine now, at head, so `$old.items.doubled` resolves; the
        // raw stored-collection materialization inside `build_migrated` cannot.
        let old_root = self.source_root().map_err(UpdateError::Engine)?;
        let staged = build_migrated(
            self.compiled(),
            self.schema(),
            &old_state,
            old_root,
            &active_source,
            &compilation,
            &plan,
            generator,
            self.now(),
        )
        .map_err(UpdateError::Rejected)?;
        // §13.15: the per-item `$migrated`/`$seeded` paths are captured from the
        // build in canonical (`BTreeMap`/sorted) path order before the rows are
        // consumed by the commit.
        let migrated = staged.migrated.iter().map(RowAddress::render).collect();
        let seeded = staged.seeded.iter().map(RowAddress::render).collect();
        let commit = self
            .apply_migration(target, compilation, staged.rows)
            .map_err(UpdateError::Engine)?;
        Ok(UpdateReport { relation: decision.relation, commit, migrated, seeded })
    }

    /// The first boundary-contract narrowing the `target` release makes relative
    /// to the active one (Annex E.2), or `None` when it preserves or widens every
    /// exposed contract. The active contract is read from the currently active
    /// definition, so the comparison is against the release in force (E.9), and a
    /// two-hop widen-then-narrow is caught at the second hop. A definition whose
    /// `$model` cannot be re-parsed yields `None` (the migration then fails its
    /// ordinary pipeline instead).
    fn boundary_narrowing(&self, target: &str, candidate: &Compilation) -> Option<String> {
        // A store fault reading the active definition falls to `None` here, exactly
        // as an unparseable `$model` does — the migration then fails its ordinary
        // pipeline instead (this method is a pre-check, not the effecting path).
        let active_definition = self.definition_source().ok().flatten()?;
        let active_doc = model_document(&active_definition)?;
        let candidate_doc = model_document(target)?;
        let active = BoundaryContract::extract(self.compiled(), &active_doc);
        let candidate = BoundaryContract::extract(&candidate.compiled, &candidate_doc);
        active.narrowing(&candidate)
    }

    /// Whether a connected §20.1 delta path exists from the active version to the
    /// `target_model`'s version (§20.3, Annex E.9), reading declared `$migrations`
    /// keys from the `target` definition text.
    ///
    /// A path exists exactly when either:
    /// - the target declares a delta keyed to the EXACT active source version
    ///   (`$migrations["<active>"]`) — a declared one-hop delta from the active
    ///   version; or
    /// - the single implicit structural-diff delta between the active and target
    ///   versions is available, which requires that NO declared `$migrations` key
    ///   lies strictly between them (both endpoint models are held on this path).
    ///   This covers the adjacent compatible minor/patch, the shape-compatible
    ///   downgrade, and the same-version republish (an empty strictly-between
    ///   range).
    ///
    /// It does NOT exist when a declared key lies strictly between the two
    /// versions with no delta from the active version — the active version is off
    /// the target's declared lineage, so the implicit direct delta is blocked and
    /// no declared delta reaches it. Applying an undeclared intermediate is
    /// forbidden (§20.1), so the in-place update is refused.
    fn connected_delta_path(&self, target: &str, target_model: &Model) -> bool {
        let active = &self.model().header().identity.version;
        let active_source = format!("{}.{}.{}", active.major, active.minor, active.patch);
        // A declared one-hop delta from the exact active source version.
        if target_model.migrations().program(&active_source).is_some() {
            return true;
        }
        let active = (active.major, active.minor, active.patch);
        let want = &target_model.header().identity.version;
        let want = (want.major, want.minor, want.patch);
        let (lo, hi) = (active.min(want), active.max(want));
        // The implicit structural-diff delta is available only when no declared
        // `$migrations` key sits strictly between the two endpoints (§20.1).
        !declared_migration_sources(target)
            .into_iter()
            .any(|key| lo < key && key < hi)
    }
}

/// The declared `$migrations` source versions of `target` (§20.1) as
/// `(major, minor, patch)` triples — the versions from which the target declares
/// a delta. Empty when the target declares no `$migrations`. The keys parsed
/// clean during `compile_definition` (a malformed key is a load rejection before
/// this point), so an unparsable key here — which cannot occur on a compiled
/// target — is conservatively skipped.
fn declared_migration_sources(target: &str) -> Vec<(u64, u64, u64)> {
    let Some(model) = model_document(target) else { return Vec::new() };
    let Some(migrations) = doc::member(&model, "$migrations") else { return Vec::new() };
    let Some(members) = doc::object(migrations) else { return Vec::new() };
    members.iter().filter_map(|member| parse_version_triple(&member.name.text)).collect()
}

/// Parse a `major.minor.patch` version key into its `(major, minor, patch)`
/// triple, whose tuple ordering matches version precedence; `None` for anything
/// that is not exactly three `u64` components.
fn parse_version_triple(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
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

/// The staged migrated state plus the §13.15 per-item report facts: the rows to
/// commit, the rows produced by a declared migration transform (`$migrated`), and
/// the seed rows inserted where absent (`$seeded`), each address list in canonical
/// path order.
struct MigratedState {
    rows: BTreeMap<RowAddress, FieldMap>,
    migrated: Vec<RowAddress>,
    seeded: Vec<RowAddress>,
}

/// Build the prospective migrated state in the §20.1 order — compatible copy and
/// local `$from`/`$as` mappings, then the package-level `$migrations` program for
/// the active source version — verify reversible transforms, and return the
/// addressed rows to stage. `active_source` is the exact `major.minor.patch` of
/// the currently active package; the program keyed to it (if any) runs over `.`
/// (the prospective target) with `$old` bound to the read-only source state.
#[allow(clippy::too_many_arguments)]
fn build_migrated<G: crate::generator::Generators>(
    old_compiled: &Compiled,
    old_schema: Schema<'_>,
    old_state: &StateSection,
    old_root: Row,
    active_source: &str,
    target: &Compilation,
    plan: &MigrationPlan,
    generator: &mut G,
    now: Timestamp,
) -> Result<MigratedState, Rejection> {
    let schema = Schema::new(&target.model);
    // §16.1/§20.2: the migration transform scope resolves the built-in codec
    // namespaces (`base64`/`hex`/`string.bytes`), so `base64.encode(string.bytes(.))`
    // and its inverse type-check and evaluate. They are seeded like the built-in
    // cose contract, independent of the package's own `$requires`.
    let codec = HostBinding::codecs();
    let codec_sigs = codec.expr_signatures();
    // §20.1: `$old` is the complete read-only source state. `old_root` is the
    // fully-folded source root the caller materialized through the live source engine
    // ([`Engine::source_root`]) — computed values and views included — so a program
    // reading `$old.items.doubled` (a computed §5.2 value) resolves it, not just the
    // stored collections. `old_working` (the raw stored rows) still backs the §8.2
    // singleton carry below, which reads the reserved row directly.
    let old_working = old_state
        .working(old_schema)
        .map_err(|error| Rejection::new(RejectionReason::Malformed, format!("source state: {error}")))?;
    // §20.1: `$old` is bound as a plain row so a delta may dot into any source
    // member — including a declared `$view` (§7), which the delta "MAY read". The
    // model's `ViewDecl.row` is an empty placeholder for an ordinary view, so the
    // raw `root_row_type` would type `$old.<view>` with no fields and a projection
    // (`$old.<view> { id, … }`) fails to type-check as "unknown name"; overlay the
    // source's compiled view types (whose VALUES `source_root` already folds onto
    // `$old`) so the view members type as their real projected row.
    let old_root_ty = old_root_type(old_schema, old_compiled);
    // Migration builds the target's rows; neither a keyring selector nor a blob
    // placement member participates in a state transform (§20.1), so the
    // migrated-state context owns an empty keyring and placement index.
    let empty_placements = crate::env::BlobPlacements::default();
    let ctx = EvalCtx {
        schema,
        compiled: &target.compiled,
        params: BTreeMap::new(),
        now,
        seed: generator.next_seed(),
        keyrings: &[],
        placements: &empty_placements,
        // §20.1: `$old` is the read-only source state the program reads.
        context: BTreeMap::from([("old".to_owned(), Cell::Row(Box::new(old_root)))]),
        // §16.1/§20.2: resolve the built-in codec namespaces so a `$as`/`$back`
        // or program transform may call `base64.encode`/`string.bytes`.
        hosts: HostDispatch::new(&codec, &[], now),
        // A migration builds the target instance's own rows; no installed-module
        // aggregate participates in a state transform.
        modules: None,
        // §13.4: a migration transform imports no parent surface.
        imports: &crate::imports::EMPTY,
    };
    let root_ty = ExprType::Row(schema.root_row_type());
    let mut sources = SourceMap::new();
    let mut prospective = Prospective::empty();
    let mut touched = Vec::new();
    // §13.15 `$migrated`: the rows a declared migration transform produced — a
    // collection with a `$from`/`$as` field mapping, or a `$migrations` program
    // write. A §20.1 compatible same-identity copy is NOT a migrated row, so a
    // shape-compatible update accumulates nothing here.
    let mut migrated_addrs: Vec<RowAddress> = Vec::new();

    // §20.1 order (1/2): the compatible same-identity copy and the local `$from`
    // field mappings (with their `$as` transforms), in declaration order.
    for collection in &target.compiled.collections {
        let migration = plan.collections.get(&collection.name);
        let transformed = migration.is_some_and(|m| !m.fields.is_empty());
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
            let mut fields =
                map_row(collection, migration, old_row, old_collection, &ctx, &mut sources, &root_ty, &codec_sigs)?;
            // §5.1/§8.12: each migrated row draws its own generation, so a newly
            // added field defaulted from `uuid()` is fresh per row (SPEC-ISSUES
            // item 4).
            let generation = prospective.next_generation();
            // The migrated row is not yet staged (its address depends on the
            // possibly-defaulted key resolved just below), so a default resolves
            // against its own scalar/struct fields only (§5.1) — the same shape the
            // insert path uses.
            rules::apply_defaults(collection, &mut fields, &ctx, &prospective, generation, None)?;
            rules::normalize_all(collection, &mut fields, &ctx, &prospective)?;
            let address = key_address(schema, collection, &fields)?;
            if prospective.contains(&address) {
                return Err(Rejection::new(RejectionReason::DuplicateKey, "migration produced a duplicate key")
                    .at(address.render()));
            }
            // §13.15: a row of a collection carrying a declared `$from`/`$as` mapping
            // went through the migration transform, so it is a `$migrated` item; a
            // plain compatible copy is not.
            if transformed {
                migrated_addrs.push(address.clone());
            }
            prospective.insert(address.clone(), fields);
            touched.push(address);
        }
    }

    // §20.1/§8.2 carry of the root singleton reserved row. The singleton is one
    // reserved row of the package root's writable scalar/ref/set/static-struct
    // members (§8.2); each target member takes either its local `$from`/`$as`
    // mapping (the singleton analogue of `map_row`, applied through the same
    // [`apply_mapping`]) or — with no mapping — the compatible same-identity copy of
    // the same-named old value, and a member the target removed is dropped. Iterating
    // the TARGET's singleton-eligible members (the same `member_type` gate the
    // seed/materialize paths use) is what carries only declared state. Staging it
    // BEFORE the program means an explicit `$migrations` write of a singleton member
    // overwrites the carried value by read-your-writes (`interp::write_singleton_field`
    // stages onto the same reserved address), so a deliberate migration of the
    // singleton still wins while every member the program leaves alone keeps its
    // §20.1 compatible copy. Every carried value is re-validated against its target
    // type in `coerce_and_require` before commit.
    let singleton_address = crate::singleton::address();
    if let Some(old_singleton) = old_working.get(&singleton_address) {
        let mut migrated_singleton = FieldMap::new();
        for member in &target.model.root().members {
            let Some(target_ty) = crate::singleton::member_type(&target.model, &member.node) else { continue };
            let name = member.name.as_str();
            if let Some(mapping) = plan.singleton_fields.get(name) {
                // §20.1: a `$from` must name a durable singleton member of the SOURCE
                // model; a name that names none (a typo, a Unicode confusable) resolves
                // to no source and rejects, rather than silently leaving the target
                // member unpopulated — the singleton analogue of `map_row`'s check.
                let old_node = old_schema.model().root().member(&mapping.from).map(|m| &m.node);
                let Some(old_ty) = old_node.and_then(|node| crate::singleton::member_type(old_schema.model(), node))
                else {
                    return Err(Rejection::new(
                        RejectionReason::Malformed,
                        format!(
                            "migration `$from: \"{}\"` for singleton member `{name}` names no durable \
                             singleton member of the source package (§8.2/§20.1)",
                            mapping.from
                        ),
                    ));
                };
                let Some(source) = old_singleton.get(&mapping.from) else { continue };
                let value =
                    apply_mapping(mapping, source, old_ty, &target_ty, &ctx, &mut sources, &root_ty, &codec_sigs, name)?;
                migrated_singleton.insert(name.to_owned(), value);
            } else if let Some(value) = old_singleton.get(name) {
                migrated_singleton.insert(name.to_owned(), value.clone());
            }
        }
        if !migrated_singleton.is_empty() {
            prospective.insert(singleton_address, migrated_singleton);
        }
    }
    // §8.2/§5.1: an added singleton member takes its insertion default (a new root
    // field with a default), and every carried or defaulted member is normalized —
    // the singleton analogue of `apply_defaults`/`normalize_all` on a migrated
    // collection row, reusing the same seed-path machinery. Both are no-ops when the
    // target declares no singleton state.
    crate::seed::apply_singleton_defaults(&target.compiled, &ctx, &mut prospective)?;
    crate::seed::apply_singleton_normalizes(&target.compiled, &ctx, &mut prospective)?;

    // §20.1 order (3): the selected package-level `$migrations` program, in array
    // order, over the prospective target with `$old` bound. Only the program keyed
    // to the exact active source version runs (§20.1); a byte-identical replay of a
    // higher version finds no program and leaves the compatible copy in place.
    if let Some(statements) = target.model.migrations().program(active_source) {
        // §13.15: the program's own writes are migration transforms, so capture them
        // as `$migrated` items distinctly from the compatible copies already touched.
        let mut program_touched = Vec::new();
        run_program(&target.compiled, statements, &old_root_ty, &root_ty, &codec_sigs, &ctx, &mut prospective, &mut program_touched)?;
        migrated_addrs.extend(program_touched.iter().cloned());
        touched.append(&mut program_touched);
    }

    // §20.1 final check: the complete prospective target is checked under ordinary
    // keys, refs, uniqueness, and checks. Every migrated row is the state to admit,
    // so coerce ref-typed values to typed refs (a program's literal key becomes a
    // resolvable ref), reject any required field a migration left unpopulated, then
    // run the ordinary rule pipeline over the whole result.
    let addresses: Vec<RowAddress> = prospective.working().keys().cloned().collect();
    for address in &addresses {
        coerce_and_require(&target.compiled, &mut prospective, address)?;
    }
    // §5.9/§5.4/§22.1/B.5: coercion may have re-derived a KEY enum leaf to the
    // target's current declaration-order ordinal, so a row's canonical address no
    // longer matches the one fixed from its source-ordinal key. Re-key every moved
    // row (and rewrite inbound references to it) before the final admission check,
    // so committed state is in canonical key order and stays addressable by its own
    // current key.
    let mut addresses = rekey_coerced(schema, &target.compiled, &mut prospective, addresses)?;
    // §13.13/§4.1: the target's `$seed` (`$data` alias) applies APPLY-IF-ABSENT
    // over the migrated state — a seed row whose address is ABSENT after migration
    // is inserted, while an address already PRESENT (a carried, program-written, or
    // rekeyed row) keeps its migrated value unchanged and is never overwritten.
    // Seeded rows admit through the SAME rule pipeline the migrated rows do:
    // `seed::admit` resolves their defaults/normalization, and the `finalize` /
    // source-series / meter passes below check them alongside the rest. The §8.2
    // singleton is carried by the §20.1 copy above, so `ApplyIfAbsent` reconciles
    // only keyed-collection rows.
    let mut seeded_addrs: Vec<RowAddress> = Vec::new();
    if let Some(data) = &target.data {
        crate::seed::admit(&target.compiled, &ctx, &mut prospective, &mut seeded_addrs, data, crate::seed::SeedMode::ApplyIfAbsent)?;
        addresses.extend(seeded_addrs.iter().cloned());
    }
    rules::finalize(&target.compiled, &ctx, &prospective, &addresses)?;
    // §20.1: the migrated state runs the SAME eager admission suite an ordinary
    // transition does, not just keys/refs/uniqueness/checks. §14.5/§14.7: reject a
    // migrated source-backed series that is non-advancing, ill-bounded, or reaches
    // an `overflow: reject` boundary.
    ctx.validate_source_series(&prospective)?;
    // §15: re-fund every migrated spend against the migrated pools and reject the
    // migration when eligible capacity is insufficient (§15.2) or a migrated pool
    // projects a negative `$quantity` (§15.1) — the pool-`$quantity` check covers
    // top-level enforcing rows and root-derived pools (e.g. a `/credit_periods`
    // source). Spend re-funding over NESTED spend collections is inert here because
    // migration stages only top-level rows in CORE (the documented nested-collection
    // seam); a nested-spend re-fund under prospective migrated state, and the
    // module/interface aggregate enforcement (`EvalCtx.modules` is `None` on this
    // path), remain flagged follow-on holes rather than a subsystem-crossing change.
    crate::meter::admit::enforce(&ctx, &target.compiled.meters, &mut prospective, &addresses)?;
    let rows: BTreeMap<RowAddress, FieldMap> = addresses
        .into_iter()
        .filter_map(|address| prospective.get(&address).map(|fields| (address.clone(), fields.clone())))
        .collect();
    // §13.15: report only rows that survived the final admission, in canonical
    // (`BTreeMap`) path order — a coerced/rekeyed transform address that no longer
    // resolves is not a committed migrated row.
    let migrated = report_paths(&migrated_addrs, &rows);
    let seeded = report_paths(&seeded_addrs, &rows);
    Ok(MigratedState { rows, migrated, seeded })
}

/// The subset of `candidates` present in the committed `rows`, deduplicated and
/// returned in canonical (`BTreeMap`) path order for the §13.15 per-item lists.
fn report_paths(candidates: &[RowAddress], rows: &BTreeMap<RowAddress, FieldMap>) -> Vec<RowAddress> {
    rows.keys().filter(|address| candidates.contains(address)).cloned().collect()
}

/// The `$old` root type a §20.1 delta program reads (`$old`, bound as a plain
/// row so the program may dot into any source member).
///
/// [`Schema::root_row_type`] types every declared `$view` (§7) member with an
/// EMPTY row: the model's `ViewDecl.row` is a placeholder that is only populated
/// for the keyring / source-bucket / module views (by their deferred typing pass),
/// never for an ordinary `$view`. That empty row is invisible on the ordinary read
/// path — a view resolves through its own compiled program there — but a migration
/// binds `$old` as a plain row and lets the delta dot into a view member. Reading
/// the view whole (`$old.<view>`) then types as a fieldless view and PROJECTING it
/// (`$old.<view> { id, … }`) fails to type-check as "unknown name", even though
/// §20.1 says a delta "MAY read any `$old` view".
///
/// The source engine already typed each top-level `$view` into `Compiled::views`
/// (its `expr` carries the real projected row type), and [`Engine::source_root`]
/// folds those same views' VALUES onto `$old`. Overlay the compiled view types
/// onto the root row type so each `$old` view member types as its real projected
/// row, matching the materialized value. Non-view members (collections, root
/// computed scalars) and the already-populated keyring/bucket/module view members
/// keep their `root_row_type` type — none appear in `Compiled::views`.
fn old_root_type(old_schema: Schema<'_>, old_compiled: &Compiled) -> ExprType {
    let base = old_schema.root_row_type();
    let view_types: BTreeMap<&str, &ExprType> =
        old_compiled.views.iter().map(|view| (view.name.as_str(), view.expr.ty())).collect();
    let fields: Vec<(String, ExprType)> = base
        .fields()
        .map(|(name, ty)| {
            let ty = view_types.get(name.as_str()).map_or_else(|| ty.clone(), |view_ty| (*view_ty).clone());
            (name.clone(), ty)
        })
        .collect();
    ExprType::Row(RowType::new(fields, base.key().cloned()))
}

/// Run the package-level `$migrations` program (§20.1) as a root mutation over the
/// prospective target, with `$old` (the source state) and the built-in codec
/// namespaces in scope. Its writes accumulate into `prospective`/`touched`; a
/// failing statement rejects the whole atomic program (§20.1).
#[allow(clippy::too_many_arguments)]
fn run_program(
    target: &Compiled,
    statements: &[String],
    old_root_ty: &ExprType,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
    ctx: &EvalCtx<'_>,
    prospective: &mut Prospective,
    touched: &mut Vec<RowAddress>,
) -> Result<(), Rejection> {
    let mut sources = SourceMap::new();
    let mut program = Vec::with_capacity(statements.len());
    for text in statements {
        let src = sources.add_label("migration", text.clone());
        let parsed = parse_expression(src, text).map_err(|d| {
            Rejection::new(RejectionReason::Malformed, format!("migration statement: {}", d.render(&sources)))
        })?;
        program.push(CompiledStmt { stmt: parsed.statement, source: src });
    }
    // A root migration program: `.` and `/` are the target root; `$old` is the
    // read-only source root; the codec namespaces are resolvable (§16.1). §16.5/§20.1:
    // a delta program is a MUTATION program — it MAY use registered namespaces of every
    // effect class — so the scope is a `Mutation` host position, not a database-evaluated
    // `DbRead` one; a §16.1 core codec transform stays valid and a registered namespace
    // call is no longer misrejected by the origin rule. (App-namespace *dispatch* in a
    // migration remains served by the codec binding — the documented §20.1/§20.2 transform
    // path — so the position, not the resolution set, is what this corrects.)
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone())
        .with_structural("old", old_root_ty.clone())
        .with_host_ops(codec_sigs.clone())
        .with_host_position(HostPosition::Mutation);
    let mutation = CompiledMutation {
        name: "$migrations".to_owned(),
        path: Vec::new(),
        receiver_is_root: true,
        params: Vec::new(),
        scope,
        program,
        context_structurals: Vec::new(),
    };
    let mut interp = Interp {
        compiled: target,
        ctx,
        prospective,
        mutation: &mutation,
        receiver: None,
        touched: Vec::new(),
        ret: None,
        erase_result: None,
        erase_exports: Vec::new(),
        locals: BTreeMap::new(),
        depth: 0,
    };
    interp.run()?;
    touched.append(&mut interp.touched);
    Ok(())
}

/// Re-validate a migrated row against its declared TARGET shape for the §20.1
/// final check, and enforce population. Every migrated value — the compatible
/// same-identity copy, a `$from`/`$as` result, or a `$migrations` program write —
/// is re-decoded against its declared target type through the SAME portable codec
/// the §19 export/restore path enforces ([`Type::decode`] over the value's
/// canonical wire), so a representable value is coerced to the new type and an
/// unrepresentable one rejects, exactly as ordinary admission would. This closes
/// the class where a breaking scalar type change, a wrong-typed `$as` result, or a
/// narrowed enum committed type-invalid state that later failed export (§19.10):
///
/// - a scalar field (`decimal`→`int`, `text`→`int`, …): a canonical-representable
///   value is decoded to the target type; a non-representable one (`decimal 1.5`,
///   `"hello"` into `int`) rejects — a breaking type change needs an explicit `$as`;
/// - an enum leaf (top-level or nested): a label the target still declares
///   re-derives its declaration-order ordinal (§5.9/§5.4); a dropped label rejects;
/// - a ref field: a value produced as a plain scalar key (a program's literal
///   `team: "ghost"`) decodes to a typed ref so the §5.6 refs check resolves it;
/// - a required field the migration left unpopulated rejects (§5.1/§20.1).
///
/// The rule that a value "is compatible" iff it decodes under the target type is
/// pinned to §20.1 ("the *compatible* value is copied") and §22.1 (field/shape
/// types hold in EVERY committed state): the committable states are exactly those
/// the §19 codec can round-trip, which is the invariant a migration must preserve.
fn coerce_and_require(
    compiled: &Compiled,
    prospective: &mut Prospective,
    address: &RowAddress,
) -> Result<(), Rejection> {
    let decl: Vec<String> = address.steps().map(|s| s.name().as_str().to_owned()).collect();
    let Some(collection) = compiled.collection_at(&decl) else { return Ok(()) };
    let Some(mut fields) = prospective.get(address).cloned() else { return Ok(()) };
    let mut changed = false;
    for field in &collection.fields {
        if field.reference.is_some() {
            // A ref value produced as a plain scalar key is decoded to a typed
            // ref so the §5.6/§20.1 refs check resolves (or rejects) it. A
            // required-but-absent ref is left for the refs check to report.
            if let Some(value) = fields.get(&field.name)
                && !matches!(value, Value::None | Value::Ref(_))
            {
                let coerced = field.ty.decode(&value.to_wire()).map_err(|error| {
                    Rejection::new(RejectionReason::TypeError, format!("migrated ref `{}`: {error}", field.name))
                        .at(address.render())
                })?;
                fields.insert(field.name.clone(), coerced);
                changed = true;
            }
            continue;
        }
        // §5.5/§5.6: a `$set` of `$ref` is validated member-by-member by the refs
        // check (its members may be carried as bare scalar keys), not re-decoded
        // here — leave it untouched, exactly as before.
        if field.element_reference.is_some() {
            continue;
        }
        // §20.1/§22.1/§5.9: re-decode every other migrated value against its
        // declared target type. `decode(value.to_wire())` coerces a representable
        // value (a canonical `decimal`/`text` that parses as the target scalar, an
        // enum label still declared, a reordered enum re-derived to its ordinal) and
        // rejects an unrepresentable one — the same total, no-hand-rolled codec the
        // §19 portable path uses, descending into every container leaf.
        if let Some(value) = fields.get(&field.name)
            && !matches!(value, Value::None)
        {
            let coerced = field.ty.decode(&value.to_wire()).map_err(|error| {
                Rejection::new(
                    RejectionReason::TypeError,
                    format!("migrated field `{}` is not representable in its target type: {error}", field.name),
                )
                .at(address.render())
            })?;
            if &coerced != value {
                fields.insert(field.name.clone(), coerced);
                changed = true;
            }
        }
        if is_required(&field.ty) && matches!(fields.get(&field.name), None | Some(Value::None)) {
            return Err(Rejection::new(
                RejectionReason::Check,
                format!("migration left required field `{}` unpopulated", field.name),
            )
            .at(address.render()));
        }
    }
    // §5.3/§20.1/§22.1: a migrated static-struct member (§5.3) — including the §8.2
    // singleton's static structs, now compiled into `root_singleton.structs` —
    // carries its own scalar/enum leaves; re-decode the whole struct against its
    // reconstructed target struct type, so a struct-nested scalar type change or a
    // narrowed struct enum is coerced-or-rejected exactly like a top-level field. A
    // struct member compiles into `collection.structs`, not `fields`, so the field
    // loop above never reaches it.
    for struct_meta in &collection.structs {
        let Some(struct_ty) = collection.struct_type(&struct_meta.name) else { continue };
        if let Some(value) = fields.get(&struct_meta.name)
            && !matches!(value, Value::None)
        {
            let coerced = struct_ty.decode(&value.to_wire()).map_err(|error| {
                Rejection::new(
                    RejectionReason::TypeError,
                    format!("migrated struct `{}` is not representable in its target type: {error}", struct_meta.name),
                )
                .at(address.render())
            })?;
            if &coerced != value {
                fields.insert(struct_meta.name.clone(), coerced);
                changed = true;
            }
        }
    }
    if changed {
        prospective.replace(address, fields);
    }
    Ok(())
}

/// Whether a migrated field must carry a value: a non-optional, non-set scalar or
/// struct (§5.1). An optional field may stay `none`; a set defaults to empty.
fn is_required(ty: &Type) -> bool {
    !matches!(ty, Type::Optional(_) | Type::Set(_))
}

/// Re-address every migrated row whose COERCED key differs from the address
/// `build_migrated` fixed from its source-ordinal key, and return the reconciled
/// address list (§5.4/§5.9/§22.1/B.5).
///
/// The §20.1 coercion pass re-derives each migrated enum leaf to the target's
/// current declaration-order ordinal (§5.9); when that leaf is a KEY component —
/// a scalar enum key, a composite key carrying one, or a struct key whose member
/// is one — the row's canonical key changes, so it belongs at a different address.
/// Each such row is moved to the address its coerced key determines (a
/// migration-internal rekey), so committed state is in canonical key order (B.5)
/// and every row stays addressable by its own current key (§5.4/§8.5/§22.1). This
/// reuses the ordinary rekey's inbound-reference rewrite ([`rewrite_inbound_refs_across`]),
/// so a reference that keyed on a moved row follows it to the new key (§5.4) —
/// the compatible copy of a `Value::Ref` inbound reference, which the coercion
/// pass leaves at the source ordinal, would otherwise dangle.
///
/// A label reorder is a bijection, so re-derived keys never collide among the
/// reordered rows; a coerced key that lands on a DIFFERENT surviving row (a
/// program-produced overlap, never a pure reorder) is a genuine §20.1 uniqueness
/// violation and rejects rather than silently overwriting.
fn rekey_coerced(
    schema: Schema<'_>,
    compiled: &Compiled,
    prospective: &mut Prospective,
    addresses: Vec<RowAddress>,
) -> Result<Vec<RowAddress>, Rejection> {
    // The intended moves: a row whose coerced key addresses it elsewhere. Migration
    // stages only top-level rows (nested collections are a documented §20.1 seam),
    // so a moved row is always a top-level reference target.
    let mut relocations: BTreeMap<RowAddress, RowAddress> = BTreeMap::new();
    let singleton_address = crate::singleton::address();
    for address in &addresses {
        // §8.2: the singleton reserved row has no key and a fixed reserved address,
        // so it never rekeys. It resolves to the keyless `root_singleton` pseudo-
        // collection, which `key_address` (a keyed top-collection lookup) cannot
        // address — skip it here, exactly as the coercion/rekey passes leave it alone.
        if address == &singleton_address {
            continue;
        }
        let decl: Vec<String> = address.steps().map(|s| s.name().as_str().to_owned()).collect();
        let Some(collection) = compiled.collection_at(&decl) else { continue };
        let Some(fields) = prospective.get(address) else { continue };
        let coerced_address = key_address(schema, collection, fields)?;
        if &coerced_address != address {
            relocations.insert(address.clone(), coerced_address);
        }
    }
    if relocations.is_empty() {
        return Ok(addresses);
    }
    // Detach every moving row first, so a new address another move vacates does not
    // read as a collision; then re-place each, rejecting a collision with a
    // surviving row or an already-re-placed move.
    let mut detached: Vec<(RowAddress, RowAddress, FieldMap)> = Vec::with_capacity(relocations.len());
    for (old, new) in &relocations {
        let Some(fields) = prospective.get(old).cloned() else { continue };
        prospective.remove(old);
        detached.push((old.clone(), new.clone(), fields));
    }
    for (_old, new, fields) in &detached {
        if prospective.contains(new) {
            return Err(Rejection::new(
                RejectionReason::DuplicateKey,
                "migration rekeyed a row onto a key already held by another row",
            )
            .at(new.render()));
        }
        prospective.insert(new.clone(), fields.clone());
    }
    // Rewrite inbound references only AFTER every moving row is back in place. A
    // moving referrer whose source-ordinal key sorts after its referent's is still
    // detached while the referent is re-placed; running the rewrite per-row would
    // never revisit it, stranding its outbound ref at the source ordinal so a valid
    // §5.4 reorder self-reference dangles. Rewriting against the complete post-move
    // row set makes every referrer — scalar `$ref` and `$set`-of-`$ref` alike
    // ([`rewrite_inbound_refs_across`]) — follow its target regardless of sort order.
    for (old, new, _fields) in &detached {
        if let (Some(name), Some(old_step), Some(new_step)) =
            (new.steps().last().map(|s| s.name().as_str().to_owned()), old.steps().last(), new.steps().last())
        {
            rewrite_inbound_refs_across(compiled, prospective, &name, old_step.key(), new_step.key());
        }
    }
    Ok(addresses
        .into_iter()
        .map(|address| relocations.get(&address).cloned().unwrap_or(address))
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
    active_model: &Model,
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
        // §5.3/§20.2: a static struct member (§5.3) is a live value of the row, but
        // it compiles into `CompiledCollection::structs`, not `fields`, so the field
        // loop above never inspects it. A downgrade that drops a populated struct the
        // older shape cannot represent — no same-named target struct or field carries
        // it, no declared mapping reconstructs it — silently discards live data,
        // exactly the §20.2 loss the field loop rejects. Apply the identical gate to
        // struct members so the two travel on the same footing (a struct kept under
        // the same name is re-decoded against the target struct type by
        // `coerce_and_require`, which rejects an inner-shape mismatch there).
        for structure in &active_collection.structs {
            let populated =
                rows.iter().any(|row| row.get(&structure.name).is_some_and(|value| *value != Value::None));
            if !populated {
                continue;
            }
            let kept = target_collection.is_some_and(|collection| {
                collection.struct_type(&structure.name).is_some() || collection.field(&structure.name).is_some()
            });
            let reconstructed =
                migration.is_some_and(|migration| migration.fields.values().any(|f| f.from == structure.name));
            if !kept && !reconstructed {
                return Err(Rejection::new(
                    RejectionReason::Compatibility,
                    format!(
                        "downgrade drops populated struct `{}` of `{name}`: the older shape cannot represent \
                         it and no declared downgrade transform preserves it",
                        structure.name
                    ),
                ));
            }
        }
    }
    // §8.2/§20.2: the root singleton reserved row is live state too, but it is not a
    // keyed collection, so `active_state.collections()` never yields it and the loop
    // above never inspects it. Apply the identical §20.2 gate to each populated
    // singleton member (a writable root scalar/ref/set/static-struct, §8.2): a
    // downgrade that drops one for which the older shape declares no same-named
    // singleton member — and no declared mapping (`$from`) reconstructs — silently
    // discards live root state, exactly the loss the collection gates reject.
    if let Some(active_singleton) = active_state.singleton() {
        for member in &active_model.root().members {
            if crate::singleton::member_type(active_model, &member.node).is_none() {
                continue;
            }
            let name = member.name.as_str();
            let populated = active_singleton.get(name).is_some_and(|value| *value != Value::None);
            if !populated {
                continue;
            }
            let kept = target
                .model
                .root()
                .member(name)
                .and_then(|target_member| crate::singleton::member_type(&target.model, &target_member.node))
                .is_some();
            let reconstructed = plan.singleton_fields.values().any(|f| f.from == name);
            if !kept && !reconstructed {
                return Err(Rejection::new(
                    RejectionReason::Compatibility,
                    format!(
                        "downgrade drops populated root member `{name}`: the older shape cannot represent \
                         it and no declared downgrade transform preserves it"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Map one source row to the target row (§20.1): the compatible same-name copy,
/// then each declared local `$from` mapping with its optional `$as` transform. A
/// `$from` naming a field the source collection does not declare rejects — the
/// mapping names no source (§20.1), a confusable near-miss included.
#[allow(clippy::too_many_arguments)]
fn map_row(
    collection: &CompiledCollection,
    migration: Option<&CollectionMigration>,
    old_row: &FieldMap,
    old_collection: Option<&CompiledCollection>,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
) -> Result<FieldMap, Rejection> {
    let mut fields = FieldMap::new();
    for field in &collection.fields {
        let mapping = migration.and_then(|m| m.fields.get(&field.name));
        if let Some(mapping) = mapping {
            // §20.1: a `$from` must name an existing source field. A name that is
            // not a declared field of the source collection (a Unicode confusable,
            // a typo) resolves to no source and rejects — it does not silently
            // leave the target field unpopulated.
            if let Some(old_collection) = old_collection
                && old_collection.field(&mapping.from).is_none()
            {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "migration `$from: \"{}\"` for `{}` names no field of source collection `{}`",
                        mapping.from, field.name, old_collection.name
                    ),
                ));
            }
            let Some(source) = old_row.get(&mapping.from) else { continue };
            let old_ty = old_collection
                .and_then(|c| c.field(&mapping.from))
                .map_or(Type::Json, |f| f.ty.clone());
            let value = apply_mapping(mapping, source, old_ty, &field.ty, ctx, sources, root_ty, codec_sigs, &field.name)?;
            fields.insert(field.name.clone(), value);
        } else if let Some(value) = old_row.get(&field.name) {
            // §20.1 compatible same-identity copy.
            fields.insert(field.name.clone(), value.clone());
        }
    }
    // §20.1/§5.3 compatible same-identity copy of static struct members: a struct
    // member (§5.3) compiles into `collection.structs`, not `fields`, so the loop
    // above never touches it. Carry each forward verbatim from the source row, so a
    // struct-nested value is part of the EXPLICIT migrated state — where its enum
    // leaves are re-validated against the target's closed set in `coerce_and_require`
    // — rather than a stale value the store would otherwise silently retain.
    for struct_meta in &collection.structs {
        if let Some(value) = old_row.get(&struct_meta.name) {
            fields.insert(struct_meta.name.clone(), value.clone());
        }
    }
    Ok(fields)
}

/// Apply one field's local migration mapping to a source value (§20.1): a `$as`
/// transform (with an optional `$back` round-trip verification, §20.2) or, without
/// `$as`, the compatible same-identity copy. `old_ty` types the transform's `.`;
/// the result is re-validated against `target_ty` by [`coerce_and_require`], so a
/// wrong-typed `$as` result rejects rather than committing. Shared by the keyed-
/// collection [`map_row`] and the §8.2 singleton carry so a singleton `$from`
/// rename copies/transforms exactly like a collection field.
#[allow(clippy::too_many_arguments)]
fn apply_mapping(
    mapping: &FieldMigration,
    source: &Value,
    old_ty: Type,
    target_ty: &Type,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
    field: &str,
) -> Result<Value, Rejection> {
    match &mapping.transform {
        Some(text) => {
            let transformed = transform(text, source, old_ty, ctx, sources, root_ty, codec_sigs, field)?;
            if let Some(back) = &mapping.back {
                verify_reversible(back, source, &transformed, target_ty, ctx, sources, root_ty, codec_sigs, field)?;
            }
            Ok(transformed)
        }
        None => Ok(source.clone()),
    }
}

/// Evaluate a `$as`/`$from` transform expression with `.` bound to the old value.
#[allow(clippy::too_many_arguments)]
fn transform(
    text: &str,
    old_value: &Value,
    old_ty: Type,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
    field: &str,
) -> Result<Value, Rejection> {
    let typed = compile(text, old_ty, sources, root_ty, codec_sigs, field)?;
    let cell = ctx.eval(&Prospective::empty(), &typed, &Cell::Scalar(old_value.clone()))?;
    Ok(scalar(cell))
}

/// Verify a reversible transform round trip (§20.2): `$back($as(x)) == x`.
#[allow(clippy::too_many_arguments)]
fn verify_reversible(
    back: &str,
    original: &Value,
    transformed: &Value,
    target_ty: &Type,
    ctx: &EvalCtx<'_>,
    sources: &mut SourceMap,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
    field: &str,
) -> Result<(), Rejection> {
    let restored = transform(back, transformed, target_ty.clone(), ctx, sources, root_ty, codec_sigs, field)?;
    if restored == *original {
        Ok(())
    } else {
        Err(Rejection::new(
            RejectionReason::Check,
            "reversible transform round trip `$back($as(x)) == x` failed",
        ))
    }
}

/// Type-check a transform expression whose `.` is a scalar of `dot_ty`, resolving
/// the built-in codec namespaces (§16.1) so `base64.encode`/`string.bytes` type.
///
/// A `$from`/`$as`/`$back` transform is a database-evaluated position (§20.1/
/// §16.5): its `.`-rooted scope carries only the built-in (Core) codec namespaces
/// and is a [`HostPosition::DbRead`](liasse_expr::HostPosition) migration-transform
/// position, so a generated host op is refused by the effect-position check and an
/// app-registered namespace by the origin check — an app codec conversion is
/// written in the delta's `$up`/`$down` program instead. The one remaining
/// generated surface is the core `now()`/`uuid()`, which `check_statement` types
/// without a position gate; the §20.1 [`nondeterministic_call`] classifier — the
/// same one a `$migrations` program uses — bars it here so no fresh
/// non-source-derived value bakes into committed migrated state.
fn compile(
    text: &str,
    dot_ty: Type,
    sources: &mut SourceMap,
    root_ty: &ExprType,
    codec_sigs: &HostSignatures,
    field: &str,
) -> Result<TypedExpr, Rejection> {
    let src = sources.add_label("migration", text.to_owned());
    let parsed = parse_expression(src, text).map_err(|d| {
        Rejection::new(RejectionReason::Malformed, format!("migration transform: {}", d.render(sources)))
    })?;
    if let Some(func) = nondeterministic_call(parsed.statement()) {
        return Err(Rejection::new(
            RejectionReason::Malformed,
            format!(
                "migration transform for field `{field}` calls the non-deterministic `{func}()`: a \
                 `$from`/`$as` transform MUST use deterministic pure functions of its input `.` (§20.1)"
            ),
        ));
    }
    let scope = RuntimeScope::new(ExprType::scalar(dot_ty), root_ty.clone())
        .with_host_ops(codec_sigs.clone())
        .with_host_position(HostPosition::DbRead(DbReadPosition::MigrationTransform));
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
/// an optional collection rename and each field's `$from`/`$as`/`$back`, plus the
/// local mappings on §8.2 root singleton members.
struct MigrationPlan {
    collections: BTreeMap<String, CollectionMigration>,
    /// Local `$from`/`$as`/`$back` mappings on §8.2 root singleton members, keyed
    /// by the TARGET member name — the singleton analogue of a collection field's
    /// mapping, applied by the singleton carry loop in [`build_migrated`].
    singleton_fields: BTreeMap<String, FieldMigration>,
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
            if shape.iter().any(|m| m.name.text == "$key") {
                let migration = Self::read_collection(shape);
                if migration.from.is_some() || !migration.fields.is_empty() {
                    collections.insert(member.name.text.clone(), migration);
                }
            } else if let Some(field) = Self::read_field(&member.value) {
                singleton_fields.insert(member.name.text.clone(), field);
            }
        }
        Ok(Self { collections, singleton_fields })
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
