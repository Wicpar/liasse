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
use liasse_expr::{check_statement, Cell, ExprType, Row, TypedExpr};
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
use crate::materialize::{self, FieldMap, Temporal};
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
        // §20.1: the migration selects the package-level program keyed to the
        // exact active source version. Capture it before staging so the target's
        // `$migrations` can read `$old` under the source model.
        let active_source = {
            let version = &self.model().header().identity.version;
            format!("{}.{}.{}", version.major, version.minor, version.patch)
        };
        let migrated = build_migrated(
            self.compiled(),
            self.schema(),
            &old_state,
            &active_source,
            &compilation,
            &plan,
            generator,
            self.now(),
        )
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
    active_source: &str,
    target: &Compilation,
    plan: &MigrationPlan,
    generator: &mut G,
    now: Timestamp,
) -> Result<BTreeMap<RowAddress, FieldMap>, Rejection> {
    let schema = Schema::new(&target.model);
    // §16.1/§20.2: the migration transform scope resolves the built-in codec
    // namespaces (`base64`/`hex`/`string.bytes`), so `base64.encode(string.bytes(.))`
    // and its inverse type-check and evaluate. They are seeded like the built-in
    // cose contract, independent of the package's own `$requires`.
    let codec = HostBinding::codecs();
    let codec_sigs = codec.expr_signatures();
    // §20.1: `$old` is the complete read-only source state, materialized under the
    // source (old) model and bound as the `$old` structural for the program.
    let old_working = old_state
        .working(old_schema)
        .map_err(|error| Rejection::new(RejectionReason::Malformed, format!("source state: {error}")))?;
    let old_root = materialize_all(old_schema, &old_working);
    let old_root_ty = ExprType::Row(old_schema.root_row_type());
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
    };
    let root_ty = ExprType::Row(schema.root_row_type());
    let mut sources = SourceMap::new();
    let mut prospective = Prospective::empty();
    let mut touched = Vec::new();

    // §20.1 order (1/2): the compatible same-identity copy and the local `$from`
    // field mappings (with their `$as` transforms), in declaration order.
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
            let mut fields =
                map_row(collection, migration, old_row, old_collection, &ctx, &mut sources, &root_ty, &codec_sigs)?;
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

    // §20.1/§8.2 compatible same-identity copy of the root singleton reserved row.
    // The singleton is one reserved row of the package root's writable scalar/ref/
    // set/static-struct members (§8.2); a member both models declare unchanged is a
    // compatible same-identity member exactly like a keyed-collection field, so it
    // MUST be carried into migrated live state. Iterating the TARGET's singleton-
    // eligible members (the same `member_type` gate the seed/materialize paths use)
    // carries each old value forward and drops a member the target removed — the
    // singleton analogue of the collection compatible copy in `map_row`. Staging it
    // BEFORE the program means an explicit `$migrations` write of a singleton member
    // overwrites the carried value by read-your-writes (`interp::write_singleton_field`
    // stages onto the same reserved address), so a deliberate migration of the
    // singleton still wins while every member the program leaves alone keeps its
    // §20.1 compatible copy.
    let singleton_address = crate::singleton::address();
    if let Some(old_singleton) = old_working.get(&singleton_address) {
        let mut migrated_singleton = FieldMap::new();
        for member in &target.model.root().members {
            if crate::singleton::member_type(&target.model, &member.node).is_some()
                && let Some(value) = old_singleton.get(member.name.as_str())
            {
                migrated_singleton.insert(member.name.as_str().to_owned(), value.clone());
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
        run_program(&target.compiled, statements, &old_root_ty, &root_ty, &codec_sigs, &ctx, &mut prospective, &mut touched)?;
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
    let addresses = rekey_coerced(schema, &target.compiled, &mut prospective, addresses)?;
    rules::finalize(&target.compiled, &ctx, &prospective, &addresses)?;
    Ok(addresses
        .into_iter()
        .filter_map(|address| prospective.get(&address).map(|fields| (address.clone(), fields.clone())))
        .collect())
}

/// Materialize the whole source root (§20.1 `$old`): every top-level collection
/// with its stored rows, non-temporal (a migration source is read whole).
fn materialize_all(schema: Schema<'_>, working: &BTreeMap<RowAddress, FieldMap>) -> Row {
    let keep = |_: &str, _: &FieldMap| true;
    let interval = |_: &str, _: &FieldMap| None;
    let temporal = Temporal { keep: &keep, interval: &interval };
    materialize::materialize_root_filtered(schema, working, &temporal)
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
    // read-only source root; the codec namespaces are resolvable (§16.1).
    let scope = RuntimeScope::new(root_ty.clone(), root_ty.clone())
        .with_structural("old", old_root_ty.clone())
        .with_host_ops(codec_sigs.clone());
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
        locals: BTreeMap::new(),
        depth: 0,
    };
    interp.run()?;
    touched.append(&mut interp.touched);
    Ok(())
}

/// Coerce a migrated row for the §20.1 final check and enforce population: a
/// ref-typed field carrying a scalar key (a program's literal `team: "ghost"`) is
/// decoded to a typed ref so the refs check resolves it; a migrated enum value is
/// re-validated against the target's closed label set so a narrowed set rejects
/// (§5.9/§22.1); a required field the migration left unpopulated is a §5.1/§20.1
/// state-population gap and rejects.
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
        // §5.9/§20.1/§22.1: a migrated enum value — the compatible same-identity
        // copy of a value that parsed under the SOURCE enum, or a program/`$as`
        // result — is re-validated against the TARGET's closed label set. A
        // narrowing release that drops its label leaves it out of the target's
        // domain, so it rejects here rather than stranding an undeclared label in
        // committed state; a retained label is re-resolved to its current ordinal.
        // The re-validation DESCENDS into containers (`rules::coerce_value`), so an
        // enum a struct/set/map layer down — not only a top-level enum field — is
        // re-checked too, gated on `contains_enum` rather than `is_enum_field`.
        if rules::contains_enum(&field.ty)
            && let Some(value) = fields.get(&field.name)
        {
            let coerced = rules::coerce_value(&field.ty, value, &field.name, &address.render())?;
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
    // §5.9/§20.1/§22.1: a migrated static-struct member (§5.3) carries its own enum
    // leaves; re-validate each against the TARGET's closed label set by descending
    // into the reconstructed struct type (`rules::coerce_value`). A struct member
    // compiles into `collection.structs`, not `fields`, so the field loop above
    // skips it — the path a narrowing release used to strand an out-of-domain label
    // one struct layer down (the top-level fix of 80fac2c reached only `fields`).
    for struct_meta in &collection.structs {
        let Some(struct_ty) = collection.struct_type(&struct_meta.name) else { continue };
        if rules::contains_enum(&struct_ty)
            && let Some(value) = fields.get(&struct_meta.name)
        {
            let coerced = rules::coerce_value(&struct_ty, value, &struct_meta.name, &address.render())?;
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
            let value = match &mapping.transform {
                Some(text) => {
                    let old_ty = old_collection
                        .and_then(|c| c.field(&mapping.from))
                        .map_or(Type::Json, |f| f.ty.clone());
                    let transformed =
                        transform(text, source, old_ty.clone(), ctx, sources, root_ty, codec_sigs, &field.name)?;
                    if let Some(back) = &mapping.back {
                        verify_reversible(
                            back, source, &transformed, &field.ty, ctx, sources, root_ty, codec_sigs, &field.name,
                        )?;
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
/// A `$from`/`$as`/`$back` transform is a *pure* position (§20.1): its `.`-rooted
/// scope carries only the pure codec namespaces and defaults to
/// [`HostPosition::Pure`](liasse_expr::HostPosition), so a generated host op is
/// already refused by the effect-position check. The one remaining generated
/// surface is the core `now()`/`uuid()`, which `check_statement` types without a
/// position gate; the §20.1 [`nondeterministic_call`] classifier — the same one a
/// `$migrations` program uses — bars it here so no fresh non-source-derived value
/// bakes into committed migrated state.
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
    let scope = RuntimeScope::new(ExprType::scalar(dot_ty), root_ty.clone()).with_host_ops(codec_sigs.clone());
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
