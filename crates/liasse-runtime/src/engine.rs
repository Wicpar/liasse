//! The engine: it loads a validated package into a store, seeds genesis, admits
//! mutation calls as atomic commits, evaluates views at a frontier, and replays
//! deterministically (§8, §9, §22).
//!
//! The store provides the durability, gapless ordering, and replayable log; the
//! engine provides the semantics on top. Determinism is a consequence: every
//! generated and sampled value an admission needs is written into the committed
//! ops, so rebuilding an engine over the same store — or replaying the same
//! request sequence under the same [`Generators`] — reproduces state exactly.

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_expr::{check_expression, Cell, SortOrder};
use liasse_ident::NameSegment;
use liasse_model::Model;
use liasse_store::{
    AddressStep, CommitOutcome, CommitSeq, DefinitionText, InstanceStore, KeyValue, RowAddress, Transition,
};
use liasse_syntax::parse_document;
use liasse_value::Timestamp;

use liasse_host::sim::SimKeyProvider;
use liasse_host::Registry;

use crate::blobs::PlacementState;
use crate::compiled::{Compiled, CompiledMutation};
use crate::doc;
use crate::host::{HostBinding, HostDispatch, HostSignatures};
use crate::keyring::Keyring;
use crate::keyring_view::KeyringSnapshot;
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::generator::Generators;
use crate::interp::{Interp, RowTarget};
use crate::outcome::CallOutcome;
use crate::request::{CallRequest, ViewQuery};
use crate::response::ResponseValue;
use crate::schema::Schema;
use crate::state::{Change, Prospective};
use crate::view::ViewResult;

/// The parsed, validated, compiled artefacts of one definition text — the
/// reusable output of the load-time front end that genesis, restore, and update
/// all consume (§9.2).
pub(crate) struct Compilation {
    pub(crate) sources: SourceMap,
    pub(crate) model: Model,
    pub(crate) compiled: Compiled,
    pub(crate) data: Option<liasse_syntax::DocValue>,
    /// The package's `$requires` declarations (§16.2), as `(local namespace,
    /// "name@major")` pairs in declaration order — resolved against the host
    /// registry at load, before activation.
    pub(crate) requires: Vec<(String, String)>,
}

/// Parse a definition text and compile its model, statements, views, and buckets
/// (§9.2 steps 1–6), returning the reusable [`Compilation`] without admitting any
/// genesis. A static failure is [`EngineError::Invalid`].
///
/// `hosts` supplies the resolved `$requires` namespaces' pinned signatures
/// (§16.2), so a host-namespace call in a view/default/computed value type-checks
/// against its declared contract. A caller managing no host components passes
/// [`HostSignatures::default`] (empty), leaving a host-call expression to fault as
/// an unknown function — the deferred-requirement behaviour of [`Engine::load`].
pub(crate) fn compile_definition(
    definition: &str,
    hosts: &HostSignatures,
) -> Result<Compilation, EngineError> {
    let mut sources = SourceMap::new();
    let src = sources.add_file("liasse.json", definition.to_owned());
    let document = parse_document(src, definition).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    // §16.2: type-check the package's `$view`/`$default`/computed host-namespace
    // calls against the same resolved signatures the compiled layer uses, so a
    // host call in a read/write position no longer faults as an unknown function
    // at `Model::build` before activation. An empty `hosts` (the default load,
    // restore, migration) reproduces the plain `Model::build` behaviour.
    let descriptors = hosts.descriptors();
    let model = Model::build_with_hosts(&mut sources, src, &document, &descriptors)
        .map_err(|d| EngineError::Invalid(Box::new(d)))?;
    let model_doc = doc::member(document.root(), "$model")
        .cloned()
        .ok_or_else(|| EngineError::Internal("definition has no `$model`".to_owned()))?;
    // §4.4: the package's declared `timestamp_precision` governs how a bare
    // `timestamp` field decodes its wire count; default microseconds when unset.
    let precision = doc::member(document.root(), "$semantics")
        .and_then(|semantics| doc::member(semantics, "timestamp_precision"))
        .and_then(doc::string)
        .and_then(liasse_value::Precision::parse)
        .unwrap_or(liasse_value::Precision::DEFAULT);
    let mut compiled = Compiled::build(&mut sources, &model, &model_doc, precision, hosts)?;
    // §17.1 / §9.2 step 5: infer or enforce each keyring's `$usage` against the
    // protected operations its call sites perform, rejecting a declared `$usage`
    // that excludes a required operation (`$usage: []` on a signed ring).
    crate::compiled::enforce_keyring_usage(
        &compiled.mutations,
        &mut compiled.keyrings,
        &model_doc,
        src,
    )?;
    // §4.1: `$data` is an alias of `$seed` (both-declared is rejected by the
    // model layer, so first-present is the single seed source here).
    let data = doc::member(document.root(), "$data")
        .or_else(|| doc::member(document.root(), "$seed"))
        .cloned();
    let requires = read_requires(document.root());
    Ok(Compilation { sources, model, compiled, data, requires })
}

/// Parse a definition's `$requires` declarations (§16.2) without building its
/// model — the cheap front step a host-managing load performs before compiling,
/// so the resolved namespace signatures are available to type-check the package's
/// host-call views and defaults.
fn requires_of(definition: &str) -> Result<Vec<(String, String)>, EngineError> {
    let mut sources = SourceMap::new();
    let src = sources.add_file("liasse.json", definition.to_owned());
    let document = parse_document(src, definition).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    Ok(read_requires(document.root()))
}

/// The package's `$requires` declarations as `(local namespace, "name@major")`
/// pairs (§16.2). The model has already validated the block's shape (a decl-name
/// key mapping to a string); resolving each against the host registry is the
/// runtime's load-time job.
fn read_requires(root: &liasse_syntax::DocValue) -> Vec<(String, String)> {
    let Some(requires) = doc::member(root, "$requires").and_then(doc::object) else {
        return Vec::new();
    };
    requires
        .iter()
        .filter_map(|member| doc::string(&member.value).map(|spec| (member.name.text.clone(), spec.to_owned())))
        .collect()
}

/// A loaded, activated package instance over a store `S`.
pub struct Engine<S> {
    store: S,
    model: Model,
    compiled: Compiled,
    /// The engine-owned virtual clock (§14, A.5): the instant `now()` samples and
    /// against which bucket activity is evaluated. It is fixed at load from the
    /// initial [`Generators::now`] sample and advances only by an explicit
    /// [`Engine::advance`]/[`Engine::set_time`], so temporal reads are
    /// deterministic and independent of a wall clock.
    clock: Timestamp,
    /// This instance's logical position in its own history (§19.2/§19.3): the
    /// active lineage and the selected point within it, plus the lineage ancestry
    /// a rollback branches. Decoupled from the volatile store commit seat so an
    /// export names a stable `(lineage, point)` that survives a restore and an
    /// import classifies an incoming artifact by its lineage relationship (§19.8).
    cursor: crate::lineage::HistoryCursor,
    sources: SourceMap,
    /// The live keyrings this package declares (§17): the version lifecycle over
    /// the in-process key provider, bootstrapped at load and advanced by due
    /// rotations as the virtual clock moves. A keyring public selector reads a
    /// snapshot of these ([`Self::keyring_snapshots`]).
    keyrings: Vec<Keyring<SimKeyProvider>>,
    /// The resolved host components this package binds (§16.2): the registered
    /// [`Registry`](liasse_host::Registry) and the resolved `$requires` map a
    /// mutation program's host-namespace call dispatches against. Built at load,
    /// when a missing/incompatible/ambiguous requirement fails before activation.
    host: HostBinding,
    /// The immutable installation `$config` this module instance was installed
    /// with (§13.1), as the `$config` structural cell a child's expressions read
    /// through `$config`/`$config.member`. `None` for an application, a module with
    /// no `$config`, and an instance not yet bound (a bare load, before install).
    /// Bound once at install by [`Engine::bind_config`] and carried into every
    /// evaluation context this engine builds.
    config: Option<Cell>,
    /// The §18.5 logical placement ledger: the recorded `$stored`/`$satisfied`/
    /// `$surplus` facts of each committed blob, keyed by canonical `$sha512`
    /// digest (§18.5). Populated by [`Engine::record_blob_placement`] — the
    /// surface/driver feeds it from the blob subsystem's `blob_placement_state`,
    /// since physical placement lives outside application state — and carried into
    /// every evaluation context so a mutation `return` or a `$view` reading a
    /// placement member resolves the fact.
    blob_placements: crate::env::BlobPlacements,
}

/// Bootstrap a live keyring per declaration (§17.3), over the in-process key
/// provider, at the load clock. A ring whose provider fails the capability check
/// is dropped rather than failing the whole load, leaving its selector to fault
/// exactly as an unmaterialized member would.
fn build_keyrings(compiled: &Compiled, clock: Timestamp) -> Vec<Keyring<SimKeyProvider>> {
    let mut rings = Vec::new();
    for decl in &compiled.keyrings {
        let provider = crate::keyring_view::built_in_provider(&decl.policy);
        if let Ok(mut ring) = Keyring::load(decl.name.clone(), provider, decl.policy.clone()) {
            let _ = ring.bootstrap(clock);
            rings.push(ring);
        }
    }
    rings
}

impl<S: InstanceStore> Engine<S> {
    /// Load `definition` into `store`, validating it statically and admitting
    /// genesis (`$data` seeds through the full rule pipeline) as one commit
    /// (§9.1–§9.3). A static failure returns [`EngineError::Invalid`]; a rejected
    /// seed returns [`EngineError::Seed`].
    ///
    /// This form manages no host components beyond the runtime's built-in
    /// `liasse.cose` namespace, so a `$requires` entry naming any other namespace
    /// is *deferred* rather than failing the load: the package activates, and a
    /// mutation that actually calls the unbound namespace fails as an unknown
    /// function. Use [`Engine::load_with_hosts`] to supply registered host
    /// namespaces, key providers, and connectors and have `$requires` resolved
    /// strictly (§16.2: a missing/incompatible/ambiguous requirement fails load).
    pub fn load<G: Generators>(
        store: S,
        definition: &str,
        generator: &mut G,
    ) -> Result<Self, EngineError> {
        // The default load manages no host components (only the built-in cose
        // namespace), so a `$requires` entry is resolved leniently and its
        // signatures are empty — a host-call view/default in a package loaded this
        // way faults as an unknown function until the host wiring lands.
        let Compilation { sources, model, compiled, data, requires } =
            compile_definition(definition, &HostSignatures::default())?;
        let host = HostBinding::resolve(Registry::new(), &requires, false)?;
        let clock = generator.now();
        let cursor = crate::lineage::HistoryCursor::genesis(store.instance());
        let keyrings = build_keyrings(&compiled, clock);
        let mut engine = Self { store, model, compiled, clock, cursor, sources, keyrings, host, config: None, blob_placements: crate::env::BlobPlacements::default() };
        engine.genesis(definition, data.as_ref(), generator)?;
        Ok(engine)
    }

    /// Load `definition` into `store` against a host [`Registry`], resolving the
    /// package's `$requires` host-namespace declarations before activation (§16.2,
    /// §9.2 step 4). A missing, incompatible, or ambiguous requirement returns
    /// [`EngineError::Requirement`]; the package does not activate.
    ///
    /// Build the registry with
    /// [`register_namespace`](liasse_host::Registry::register_namespace),
    /// [`register_provider`](liasse_host::Registry::register_provider), and
    /// [`register_connector`](liasse_host::Registry::register_connector) to supply
    /// the host components a case's `hosts` block provisions (a `sim` namespace,
    /// a key provider, a connector). The runtime's built-in `liasse.cose`
    /// namespace is added automatically when the registry carries none, so a
    /// keyring package's `cose.sign`/`cose.verify` resolves without external
    /// wiring; a host may register its own cose descriptor instead.
    pub fn load_with_hosts<G: Generators>(
        store: S,
        definition: &str,
        generator: &mut G,
        registry: Registry,
    ) -> Result<Self, EngineError> {
        // §16.2: resolve the package's requirements against the registry *before*
        // compiling, so a host-namespace call in a view/default type-checks against
        // the pinned descriptor. A missing/incompatible/ambiguous requirement fails
        // here, before activation; only a resolved namespace's signatures are
        // supplied to the checker.
        let requires = requires_of(definition)?;
        let host = HostBinding::resolve(registry, &requires, true)?;
        let Compilation { sources, model, compiled, data, .. } =
            compile_definition(definition, &host.expr_signatures())?;
        let clock = generator.now();
        let cursor = crate::lineage::HistoryCursor::genesis(store.instance());
        let keyrings = build_keyrings(&compiled, clock);
        let mut engine = Self { store, model, compiled, clock, cursor, sources, keyrings, host, config: None, blob_placements: crate::env::BlobPlacements::default() };
        engine.genesis(definition, data.as_ref(), generator)?;
        Ok(engine)
    }

    /// Rebuild an activated instance over `store` from a definition and a
    /// portable state capture (§19.10 restore): compile the definition, then
    /// admit the captured rows verbatim as one genesis-position commit. Unlike
    /// [`Engine::load`] this applies no `$data` seed and no defaults — the capture
    /// is already the authoritative committed state — so a restore reproduces the
    /// exported state exactly.
    ///
    /// `cursor` is the logical position the artifact selected (§19.2): the restore
    /// adopts it rather than restarting at the genesis point, so a re-export names
    /// the *same* `(lineage, point)` and a continuation advances past it. The
    /// genesis-position store commit that stages the captured rows is not a new
    /// history point — the restored state *is* the selected point — so the cursor
    /// is adopted verbatim and not advanced.
    pub(crate) fn from_state<G: Generators>(
        store: S,
        definition: &str,
        state: &crate::portable::StateSection,
        cursor: crate::lineage::HistoryCursor,
        generator: &mut G,
    ) -> Result<Self, EngineError> {
        // A restore manages no host components (§19.10 rebuilds over a bare
        // registry), so requirements are deferred and host-call expressions carry
        // empty signatures — the same as the default load.
        let Compilation { sources, model, compiled, requires, .. } =
            compile_definition(definition, &HostSignatures::default())?;
        let host = HostBinding::resolve(Registry::new(), &requires, false)?;
        let clock = generator.now();
        let keyrings = build_keyrings(&compiled, clock);
        let mut engine = Self { store, model, compiled, clock, cursor, sources, keyrings, host, config: None, blob_placements: crate::env::BlobPlacements::default() };
        engine.install_state(definition, state)?;
        Ok(engine)
    }

    /// This instance's incarnation (D.1).
    #[must_use]
    pub fn instance(&self) -> &liasse_ident::InstanceId {
        self.store.instance()
    }

    /// This instance's logical history cursor (§19.2/§19.3): the selected point,
    /// its lineage ancestry, and the classification of an incoming point against
    /// it. The §19 history operations read it to name an exported point and to
    /// classify an import; an applied movement mutates it through
    /// [`Self::cursor_mut`].
    #[must_use]
    pub(crate) fn cursor(&self) -> &crate::lineage::HistoryCursor {
        &self.cursor
    }

    /// Mutable access to the history cursor, so an applied import (fast-forward or
    /// rollback, §19.8) moves the selected point to the incoming one.
    pub(crate) fn cursor_mut(&mut self) -> &mut crate::lineage::HistoryCursor {
        &mut self.cursor
    }

    /// The active definition text (D.4).
    pub(crate) fn definition_source(&self) -> Option<String> {
        self.store.definition().map(|d| d.source().to_owned())
    }

    pub(crate) fn compiled(&self) -> &Compiled {
        &self.compiled
    }

    pub(crate) fn schema(&self) -> Schema<'_> {
        Schema::new(&self.model)
    }

    /// Stage every captured row as an insert against the current empty base and
    /// commit it as the definition-load genesis (§19.10). The captured rows are
    /// re-addressed through [`StateSection::working`], which places each top-level
    /// collection row at its key position and the §8.2 singleton reserved row at
    /// its reserved address — so a restore reproduces the exported root singleton
    /// state, not only its collections.
    fn install_state(
        &mut self,
        definition: &str,
        state: &crate::portable::StateSection,
    ) -> Result<(), EngineError> {
        let mut prospective = Prospective::empty();
        for (address, fields) in state.working(Schema::new(&self.model))? {
            prospective.insert(address, fields);
        }
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        txn.commit()?;
        Ok(())
    }

    /// Restore the definition AND state of an imported point as one atomic
    /// movement — the fast-forward or rollback an applied import performs (§19.8).
    /// Returns the new head, or the current head when nothing changed.
    ///
    /// A movement restores *the selected point*, which the artifact carries with
    /// the definition active at that point (§19.5). When the point pre- or
    /// post-dates a migration, its state was captured under a different shape than
    /// the currently active one, so the definition must be adopted before the
    /// captured rows are read — otherwise the point's bytes are reinterpreted under
    /// a foreign schema and its values are lost (§20.2). This mirrors
    /// [`Engine::from_state`] (the fresh-instance restore, §19.10) and the §20
    /// migration commit: compile the point's definition, rebind its `$requires`
    /// against the live registry, reinstall the captured rows under the point's own
    /// schema, record the definition on the same commit, then adopt it as active.
    pub(crate) fn reinstall_point(
        &mut self,
        definition: &str,
        state: &crate::portable::StateSection,
    ) -> Result<CommitSeq, EngineError> {
        // §16.2/§20: re-resolve the point definition's `$requires` against the live
        // registry before activation, strictly, exactly as a migration does.
        let Compilation { sources, model, compiled, requires, .. } =
            compile_definition(definition, &HostSignatures::default())?;
        self.host.rebind(&requires)?;
        // §19.5: read the captured rows under the POINT's own schema, not the
        // engine's current one, so a movement across a migration reinstalls the
        // point's shape rather than reinterpreting its bytes under a foreign model.
        let target = state.working(Schema::new(&model))?;
        let mut prospective = Prospective::gather(&self.store, self.schema())?;
        // Drop every live address absent from the target, then overwrite the rest.
        let live: Vec<_> = prospective.working().keys().cloned().collect();
        for address in live {
            if !target.contains_key(&address) {
                prospective.remove(&address);
            }
        }
        for (address, fields) in target {
            prospective.insert(address, fields);
        }
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        // §19.8/§19.10: record the point's definition on the same commit so a
        // restart reproduces it, then adopt it as the active definition below.
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        let seq = match txn.commit()? {
            CommitOutcome::Committed(seq) => seq,
            CommitOutcome::Unchanged => self.store.head(),
        };
        self.model = model;
        self.compiled = compiled;
        self.sources = sources;
        self.keyrings = build_keyrings(&self.compiled, self.clock);
        Ok(seq)
    }

    /// §19.9 activation: install a computed merged/corrected logical state as one
    /// atomic transition into a *new lineage*, the engine primitive a host
    /// reconciliation correction commits through.
    ///
    /// `merged` is the accepted combined row set — the [`MergeOutcome::merged`] of
    /// a clean automatic merge, or the composition a host correction resolved over
    /// a conflicted plan. Its rows replace live state exactly as an applied import
    /// does (§19.8): every address absent from `merged` is removed, the rest
    /// overwritten, staged as one commit. The engine's lineage then advances to a
    /// freshly derived lineage so a subsequent [`export`](Self::export) names the
    /// reconciled point on its own lineage, recording that a reconciliation
    /// happened. Retaining *both* source histories as alternate lineages (§19.9
    /// "preserving both source histories") is a documented artifact-container seam;
    /// CORE records the accepted result on the new lineage over the prior history.
    ///
    /// [`MergeOutcome::merged`]: crate::MergeOutcome
    pub fn activate_merge(
        &mut self,
        merged: &BTreeMap<RowAddress, crate::materialize::FieldMap>,
    ) -> Result<CommitSeq, EngineError> {
        let schema = Schema::new(&self.model);
        let mut prospective = Prospective::gather(&self.store, schema)?;
        let live: Vec<RowAddress> = prospective.working().keys().cloned().collect();
        for address in live {
            if !merged.contains_key(&address) {
                prospective.remove(&address);
            }
        }
        for (address, fields) in merged {
            prospective.insert(address.clone(), fields.clone());
        }
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        let seq = match txn.commit()? {
            CommitOutcome::Committed(seq) => seq,
            CommitOutcome::Unchanged => self.store.head(),
        };
        // §19.9: record the accepted result as a fresh point on a new lineage
        // branched from the prior head, so a subsequent export names the
        // reconciled point on its own lineage over the prior history.
        self.cursor.begin_reconciled();
        Ok(seq)
    }

    /// Commit a migration (§20): replace live state with the migrated rows under
    /// the new definition in one atomic commit, then swap in the target's model
    /// and compiled artefacts. The migrated rows were already checked against the
    /// target's rule pipeline by the caller, so this only stages the diff.
    pub(crate) fn apply_migration(
        &mut self,
        definition: &str,
        target: Compilation,
        migrated: BTreeMap<RowAddress, crate::materialize::FieldMap>,
    ) -> Result<CommitSeq, EngineError> {
        // §16.2/§20: the target keeps the context's registered components but
        // declares its own `$requires`; re-resolve them before staging, so an
        // unmet requirement fails the migration before any effect.
        self.host.rebind(&target.requires)?;
        let schema = Schema::new(&self.model);
        let mut prospective = Prospective::gather(&self.store, schema)?;
        let live: Vec<RowAddress> = prospective.working().keys().cloned().collect();
        for address in live {
            prospective.remove(&address);
        }
        for (address, fields) in migrated {
            prospective.insert(address, fields);
        }
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        let seq = match txn.commit()? {
            // §20/§19.2: a migration is a linear continuation, so it takes a fresh
            // point on the active lineage.
            CommitOutcome::Committed(seq) => {
                self.cursor.advance();
                seq
            }
            CommitOutcome::Unchanged => self.store.head(),
        };
        self.model = target.model;
        self.compiled = target.compiled;
        self.sources = target.sources;
        self.keyrings = build_keyrings(&self.compiled, self.clock);
        Ok(seq)
    }

    /// The current head serial position.
    #[must_use]
    pub fn head(&self) -> CommitSeq {
        self.store.head()
    }

    /// The current virtual-clock instant (§14, A.5). Every `now()` an admission
    /// or view samples reads this value; it advances only explicitly.
    #[must_use]
    pub fn now(&self) -> Timestamp {
        self.clock
    }

    /// Move the virtual clock to `now` (§14). Bucket activity is re-evaluated
    /// against it on the next read, so a row can enter or leave its active
    /// interval without any commit. Time is expected to be non-decreasing.
    pub fn set_time(&mut self, now: Timestamp) {
        self.clock = now;
        self.rotate_due();
    }

    /// Advance the virtual clock by `ticks` of its current precision (§14) — the
    /// `advance_time` step. Saturates rather than overflowing.
    pub fn advance(&mut self, ticks: i128) {
        let count = self.clock.count().saturating_add(ticks);
        self.clock = Timestamp::new(count, self.clock.precision());
        self.rotate_due();
    }

    /// Record the §18.5 logical placement facts of a committed blob, keyed by its
    /// canonical `$sha512` `digest`: the verified stores (`$stored`), whether the
    /// placement policy is satisfied over them (`$satisfied`), and the verified
    /// copies outside the currently required policy (`$surplus`).
    ///
    /// Physical placement lives in the blob subsystem, not application state, so
    /// the engine cannot derive these itself: the surface/driver reads them from a
    /// blob host's [`blob_placement_state`] and records them here (§18.5). Every
    /// subsequent evaluation that reads `.blob.$satisfied`/`.$stored`/`.$surplus`
    /// — a mutation `return`, a `$view` — resolves against the recorded facts.
    /// Re-recording a digest replaces its facts, so a policy change that shifts
    /// `$surplus`/`$satisfied` without moving bytes is reflected on the next read
    /// (§18.5).
    ///
    /// [`blob_placement_state`]: crate::PlacementState
    pub fn record_blob_placement(&mut self, digest: impl Into<String>, state: &PlacementState) {
        self.blob_placements.record(digest, state.facts());
    }

    /// Perform any due keyring rotation before the next operation (§17.4): moving
    /// the virtual clock past a cadence retires the prior active version and
    /// activates a new one, and reaching the `$overlap` lead exposes a pending
    /// version. A provider failure keeps the current version active (§17.9).
    fn rotate_due(&mut self) {
        let clock = self.clock;
        for ring in &mut self.keyrings {
            ring.ensure_current(clock);
        }
    }

    /// A read-time snapshot of every live keyring's version view at the current
    /// clock (§17.2), the keyring index an evaluation environment answers a
    /// keyring public selector against.
    fn keyring_snapshots(&self) -> Vec<KeyringSnapshot> {
        self.keyrings.iter().map(|ring| KeyringSnapshot::of(ring, self.clock)).collect()
    }

    /// The validated package model.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Type-check and bind an installation's `$config` values onto this instance
    /// (§13.1, §13.3). Consumes the model's declared `$config` struct schema: it
    /// rejects a supplied member the struct does not declare or a value that does
    /// not decode to the declared type, fills each omitted member from its default
    /// (rejecting a required member that was omitted), and records the resolved
    /// struct as the `$config` value the child's expressions read.
    ///
    /// Called once at install, after genesis (§13.3 "loading validates ... the
    /// configuration ... before the instance becomes active"). A package with no
    /// `$config` accepts no installation values.
    pub(crate) fn bind_config<G: Generators>(
        &mut self,
        supplied: &BTreeMap<String, liasse_value::Value>,
        generator: &mut G,
    ) -> Result<(), crate::config::ConfigBindError> {
        let resolved = self.resolve_config(supplied, generator)?;
        self.config = resolved.as_ref().map(crate::config::cell);
        Ok(())
    }

    /// Resolve the installation `$config` against the declared struct (§13.3),
    /// returning the resolved values, or `None` for a package with no `$config`.
    fn resolve_config<G: Generators>(
        &self,
        supplied: &BTreeMap<String, liasse_value::Value>,
        generator: &mut G,
    ) -> Result<Option<BTreeMap<String, liasse_value::Value>>, crate::config::ConfigBindError> {
        let Some(schema) = self.model.config_schema() else {
            // A package with no `$config` declares no installation values, so any
            // supplied member is unknown (§13.1).
            if let Some(name) = supplied.keys().next() {
                return Err(crate::config::ConfigBindError::Mismatch(
                    crate::config::ConfigError::UnknownMember(name.clone()),
                ));
            }
            return Ok(None);
        };
        let engine_schema = Schema::new(&self.model);
        let snapshots = self.keyring_snapshots();
        let ctx = EvalCtx {
            schema: engine_schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: generator.next_seed(),
            keyrings: &snapshots,
            placements: &self.blob_placements,
            context: BTreeMap::new(),
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            modules: None,
        };
        let prospective = Prospective::gather(&self.store, engine_schema)
            .map_err(|error| crate::config::ConfigBindError::Engine(EngineError::Store(error)))?;
        let resolved = crate::config::resolve(schema, supplied, &ctx, &prospective)
            .map_err(crate::config::ConfigBindError::Mismatch)?;
        Ok(Some(resolved))
    }

    /// The structural bindings every evaluation context this engine builds carries
    /// beyond the request's own (§11.1 `$actor`/`$session`): the immutable
    /// installation `$config` (§13.1), so a child's default, computed value,
    /// `$view`, exposed interface, or mutation resolves `$config`/`$config.member`
    /// to the values it was installed with. Empty for an application or an
    /// unbound instance.
    fn base_context(&self) -> BTreeMap<String, Cell> {
        let mut context = BTreeMap::new();
        if let Some(config) = &self.config {
            context.insert("config".to_owned(), config.clone());
        }
        context
    }

    /// The backing store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The internally-provisioned keyring named `ring` (§17), for reading its
    /// version metadata (`.$current`/`.$accepted`/`.$versions`) and lifecycle
    /// state. The engine bootstraps and rotates it on the virtual clock; a host
    /// driver reads it to assert acceptance and rotation. `None` when the package
    /// declares no keyring of that name.
    #[must_use]
    pub fn keyring(&self, ring: &str) -> Option<&Keyring<SimKeyProvider>> {
        self.keyrings.iter().find(|r| r.name() == ring)
    }

    /// Mutable access to keyring `ring`'s backing key provider, for the §17.9
    /// `provider_set` fault-injection vocabulary a driver uses to make a
    /// `cose.sign` mutation fail (unavailability, per-operation failure, an
    /// invalid public key). `None` when no keyring of that name is declared.
    pub fn keyring_provider_mut(&mut self, ring: &str) -> Option<&mut SimKeyProvider> {
        self.keyrings.iter_mut().find(|r| r.name() == ring).map(Keyring::provider_mut)
    }

    /// Mutable lifecycle access to the internally-provisioned keyring `ring`
    /// (§17.3/§17.4), so a host operator can drive the version lifecycle the
    /// engine does not schedule automatically: `bind_activate` a manual policy's
    /// externally created version (bind the provider's
    /// [`MANUAL_EXTERNAL_KEY`](crate::MANUAL_EXTERNAL_KEY) handle), `revoke` a
    /// version, or `destroy` its provider material. The engine reads these rings
    /// for every subsequent snapshot, so the `/ring.$current`/`.$accepted`/
    /// `.$versions` views reflect the transition on the next read. `None` when the
    /// package declares no keyring of that name.
    pub fn keyring_admin(&mut self, ring: &str) -> Option<&mut Keyring<SimKeyProvider>> {
        self.keyrings.iter_mut().find(|r| r.name() == ring)
    }

    /// The declared keyring names (§17.1), in declaration order — so a driver maps
    /// a `$provider` fault target to the rings it backs.
    pub fn keyring_names(&self) -> impl Iterator<Item = &str> {
        self.keyrings.iter().map(Keyring::name)
    }

    /// Verify a `cose.sign` token against keyring `ring`'s accepted versions at
    /// the current instant (§17.7), returning the verified claims. Acceptance-
    /// based: no provider operation is involved, so an existing token keeps
    /// verifying through a provider outage while a revoked / retired-past-`$retain`
    /// / foreign-ring / tampered token is denied. This is the runtime capability
    /// the surface/testkit auth path (`$verify: "cose.verify(/ring, $credential)"`)
    /// drives.
    ///
    /// # Errors
    /// [`CoseVerifyError`](crate::CoseVerifyError) for a malformed token, an unknown ring, a foreign-ring
    /// token, a tampered claim set, or a no-longer-accepted version.
    pub fn cose_verify(
        &self,
        ring: &str,
        token: &liasse_value::Value,
    ) -> Result<liasse_value::Value, crate::host::CoseVerifyError> {
        crate::host::cose_verify(&self.keyrings, ring, token, self.clock)
    }

    fn genesis<G: Generators>(
        &mut self,
        definition: &str,
        data: Option<&liasse_syntax::DocValue>,
        generator: &mut G,
    ) -> Result<(), EngineError> {
        let schema = Schema::new(&self.model);
        let snapshots = self.keyring_snapshots();
        let ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: generator.next_seed(),
            keyrings: &snapshots,
            placements: &self.blob_placements,
            // §11.1: genesis seeding executes with no actor.
            context: BTreeMap::new(),
            // §16.3/§8.8: a `$data` seed's field default may call a resolved host
            // namespace (a pure or generated function), so genesis carries the live
            // dispatch the same way a mutation admission does.
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            // Genesis seeds the instance's own state; a `.modules::iface` read is a
            // parent-engine concern, so no installed-module aggregate crosses here.
            modules: None,
        };
        let mut prospective = Prospective::empty();
        let mut touched = Vec::new();
        if let Some(data) = data {
            crate::seed::admit(&self.compiled, &ctx, &mut prospective, &mut touched, data, crate::seed::SeedMode::Genesis)
                .map_err(EngineError::Seed)?;
        } else {
            // §8.2: even with no `$data`, a writable singleton root field declared
            // `= default` takes its default at genesis, then normalizes it (§8.8).
            crate::seed::apply_singleton_defaults(&self.compiled, &ctx, &mut prospective)
                .map_err(EngineError::Seed)?;
            crate::seed::apply_singleton_normalizes(&self.compiled, &ctx, &mut prospective)
                .map_err(EngineError::Seed)?;
        }
        crate::rules::finalize(&self.compiled, &ctx, &prospective, &touched).map_err(EngineError::Seed)?;
        // §14.5: reject seed data whose source-backed recurring bucket would generate
        // a non-advancing or ill-bounded series.
        ctx.validate_source_series(&prospective).map_err(EngineError::Seed)?;
        // §15.2: a seeded spend is funded through the same allocation as a mutation.
        crate::meter::admit::enforce(&ctx, &self.compiled.meters, &mut prospective, &touched)
            .map_err(EngineError::Seed)?;

        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        // §9.3: a definition load creates a commit even when state is unchanged.
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        txn.commit()?;
        Ok(())
    }

    /// Overlay an installation `$data` object (§13.3) onto this instance's already
    /// seeded genesis: `data_text` is the JSON text of the `$data` object, admitted
    /// through the ordinary insertion pipeline (defaults, normalizers, checks, meter
    /// funding) as one commit over current state — so every resulting value passes
    /// load validation. This inserts the installation rows the module composition
    /// seeds a child with. Merging supplied scalar/struct fields into an existing
    /// package-`$data` row and unioning sets (the full §13.3/§13.13 overlay) is a
    /// documented seam: a row colliding with a package-`$data` key is rejected as a
    /// duplicate rather than field-merged.
    pub(crate) fn overlay_install_data<G: Generators>(
        &mut self,
        data_text: &str,
        generator: &mut G,
    ) -> Result<(), EngineError> {
        // Wrap the `$data` object as a one-member document so the existing document
        // parser yields its spanned `DocValue`.
        let wrapper = format!("{{\"$data\":{data_text}}}");
        let mut sources = SourceMap::new();
        let src = sources.add_file("install-data", wrapper.clone());
        let document = parse_document(src, &wrapper).map_err(|d| EngineError::Invalid(Box::new(d)))?;
        let Some(data) = doc::member(document.root(), "$data") else {
            return Ok(());
        };
        let schema = Schema::new(&self.model);
        let snapshots = self.keyring_snapshots();
        let ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: generator.next_seed(),
            keyrings: &snapshots,
            placements: &self.blob_placements,
            // §13.1: the installation `$config` is bound before this overlay, so an
            // overlaid `$data` value reading `$config` resolves it.
            context: self.base_context(),
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            modules: None,
        };
        let mut prospective = Prospective::gather(&self.store, schema)?;
        let mut touched = Vec::new();
        crate::seed::admit(&self.compiled, &ctx, &mut prospective, &mut touched, data, crate::seed::SeedMode::Genesis)
            .map_err(EngineError::Seed)?;
        crate::rules::finalize(&self.compiled, &ctx, &prospective, &touched).map_err(EngineError::Seed)?;
        ctx.validate_source_series(&prospective).map_err(EngineError::Seed)?;
        crate::meter::admit::enforce(&ctx, &self.compiled.meters, &mut prospective, &touched)
            .map_err(EngineError::Seed)?;
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        // §13.3/§19.2: an installation `$data` overlay that changes state takes a
        // fresh point on the active lineage.
        if let CommitOutcome::Committed(_) = txn.commit()? {
            self.cursor.advance();
        }
        Ok(())
    }

    /// Admit a mutation call as an atomic commit (§8, §22.2). A rule failure is
    /// an application [`CallOutcome::Rejected`], not an [`EngineError`]; only a
    /// store or engine fault errors.
    pub fn call<G: Generators>(
        &mut self,
        request: &CallRequest,
        generator: &mut G,
    ) -> Result<CallOutcome, EngineError> {
        let Some(mutation) = self.compiled.mutation(request.mutation()) else {
            return Ok(rejected(RejectionReason::Malformed, format!("unknown mutation `{}`", request.mutation())));
        };
        let params = match collect_params(mutation, request) {
            Ok(params) => params,
            Err(rejection) => return Ok(CallOutcome::Rejected(rejection)),
        };
        let schema = Schema::new(&self.model);
        let snapshots = self.keyring_snapshots();
        let mut ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params,
            now: self.clock,
            seed: generator.next_seed(),
            keyrings: &snapshots,
            placements: &self.blob_placements,
            // §13.1: a module instance's mutation reads its installed `$config`;
            // `$actor`/`$session` are merged in below once resolved.
            context: self.base_context(),
            // §16.4/§17.7: a mutation program may call a resolved host namespace
            // (`util.double(...)`) or sign a session token (`cose.sign(/ring, …)`);
            // the dispatch resolves the call and routes cose to the live keyring.
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            // A mutation admits against this instance's own state; interface-addressed
            // cross-module dispatch is routed by the parent host (§13.10), not folded
            // into this engine's evaluation.
            modules: None,
        };

        let receiver = match receiver_target(&self.compiled, mutation, request) {
            Ok(receiver) => receiver,
            Err(rejection) => return Ok(CallOutcome::Rejected(rejection)),
        };

        let mut prospective = Prospective::gather(&self.store, schema)?;
        // §11.1: an authenticated admission binds `$actor` (and `$session`, when
        // the authenticator declared one) to the row the request resolved, so the
        // program reads them. The row is re-materialized from committed state by
        // key at this admission position (§10.3, §11.3), not carried from the
        // authenticator, so a state change since resolution is observed.
        // §11.1/§13.1: merge the resolved `$actor`/`$session` into the `$config`
        // the context already carries, so a mutation reads all three.
        ctx.context.extend(auth_context(&self.compiled, &ctx, &prospective, request));
        let mut interp = Interp {
            compiled: &self.compiled,
            ctx: &ctx,
            prospective: &mut prospective,
            mutation,
            receiver,
            touched: Vec::new(),
            ret: None,
            erase_result: None,
            erase_exports: Vec::new(),
            locals: BTreeMap::new(),
            depth: 0,
        };
        if let Err(rejection) = interp.run() {
            return Ok(CallOutcome::Rejected(rejection));
        }
        let touched = std::mem::take(&mut interp.touched);
        let ret = interp.ret.take();
        let erase_result = interp.erase_result.take();
        // §21.2: every reintegration bundle the program's erases produced, captured
        // whether returned or from a bare `erase(row)` statement, so no committed
        // erasure leaves its export undelivered (relocation, not destruction).
        let erase_exports = std::mem::take(&mut interp.erase_exports);
        let locals = std::mem::take(&mut interp.locals);
        let receiver = interp.receiver.take();

        if let Err(rejection) = crate::rules::finalize(&self.compiled, &ctx, &prospective, &touched) {
            return Ok(CallOutcome::Rejected(rejection));
        }

        // §14.5: reject a transition (a source insert/edit, or a change to referenced
        // period data) that would make a source-backed recurring bucket non-advancing
        // or ill-bounded.
        if let Err(rejection) = ctx.validate_source_series(&prospective) {
            return Ok(CallOutcome::Rejected(rejection));
        }

        // §15.2: fund every new or changed spend from the reachable pools, freezing
        // the allocation as an admission fact and rejecting the whole transition on
        // insufficient eligible capacity.
        if let Err(rejection) =
            crate::meter::admit::enforce(&ctx, &self.compiled.meters, &mut prospective, &touched)
        {
            return Ok(CallOutcome::Rejected(rejection));
        }

        // §8.6/§8.10: the `return` is evaluated from the resulting state — the
        // prospective state that will be committed. It is part of the admitted
        // operation, so a genuine evaluation fault (e.g. `.$between`'s empty-range
        // rejection, §14.1) rejects the whole transition and commits nothing,
        // rather than silently yielding a valueless success.
        //
        // §6.3: a program that changes state and returns a keyed selection of an
        // affected row returns that row as a single row (a `row-mutation receiver`
        // is a one-row context). A program that changes nothing is a query, so a
        // keyed-selection `return` is delivered as the row view it denotes — an
        // array — exactly as a `$view` would (§12.2). `state_changed` distinguishes
        // the two; the shape only differs for a scalar/composite-key selection,
        // which types as a single `Row`.
        let changes = prospective.diff();
        let state_changed = !changes.is_empty();
        // §21.2: a `return erase(row)` delivers the durable extract the erase
        // produced during the program, in place of a post-commit `return`.
        let response = if let Some(value) = erase_result {
            Some(ResponseValue::new(Cell::Scalar(value)))
        } else {
            match eval_return(
                &ctx,
                &prospective,
                &receiver,
                &locals,
                mutation,
                ret.as_ref(),
                state_changed,
            ) {
                Ok(response) => response,
                Err(rejection) => return Ok(CallOutcome::Rejected(rejection)),
            }
        };
        // §21.2: a bare `erase(row)` statement with no explicit `return` still
        // delivers its captured reintegration bundle, so a committed erasure never
        // silently drops the export. A program with its own `return` keeps that
        // response; the bundle stays captured on the sink for the deferred
        // reintegration load-action.
        let response = response
            .or_else(|| erase_exports.last().cloned().map(|value| ResponseValue::new(Cell::Scalar(value))));

        if !state_changed {
            // §8.9: no state change → `unchanged`; the frontier does not advance.
            return Ok(CallOutcome::Unchanged { response });
        }

        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        let seq = match txn.commit()? {
            // §19.2: a state-changing commit takes a fresh point on the active
            // lineage — the identity a later export names and an import classifies.
            CommitOutcome::Committed(seq) => {
                self.cursor.advance();
                seq
            }
            CommitOutcome::Unchanged => self.store.head(),
        };
        Ok(CallOutcome::Committed { seq, response })
    }

    /// Evaluate a named view against committed state at `frontier` (§7, §12.4)
    /// with no parameters bound and no actor identity — an unauthenticated,
    /// argument-free read. A `$view` reading `@param` takes its declared default
    /// and one reading `$actor`/`$session` faults unbound. Use [`Engine::view_with`]
    /// to bind parameters and an actor/session identity. Returns `None` when no
    /// view of that name is declared.
    pub fn view(&self, name: &str, frontier: CommitSeq) -> Result<Option<ViewResult>, EngineError> {
        self.view_with(name, frontier, &ViewQuery::new())
    }

    /// Evaluate a named view against committed state at `frontier` with the
    /// parameter bindings and actor/session identity `query` supplies (§10.1,
    /// §11.1) — the param- and actor-aware read the surface layer calls to serve a
    /// `$view` with `$params` or a role `$view` filtering on `$actor`.
    ///
    /// Each declared `$params` entry the view reads as `@name` resolves from the
    /// query's parameters, taking its declared default when unbound (§8.3); a view
    /// reading `$actor`/`$session` resolves the row the query's actor/session key
    /// names, re-materialized from committed state at `frontier` (§10.3, §11.3), so
    /// a role view observes state as of the read. The view is evaluated at the
    /// virtual clock, so bucketed collections expose only the rows active at that
    /// instant (§14). Returns `None` when no view of that name is declared.
    pub fn view_with(
        &self,
        name: &str,
        frontier: CommitSeq,
        query: &ViewQuery,
    ) -> Result<Option<ViewResult>, EngineError> {
        self.view_with_impl(name, frontier, query, None)
    }

    /// [`Engine::view_with`] with the installed module instances (§13.9) folded into
    /// the read, so a root package's `.modules::iface` aggregation and a
    /// `/collection[k].catalog` nested view resolve against the children the
    /// [`ModuleHost`](crate::ModuleHost) installed. The `ModuleHost` — which owns
    /// this root engine and its children — builds the aggregate by reading each
    /// enabled child's exposed interface `$view` through the boundary, so only the
    /// projected fields cross and a private child field stays unreachable (§13.8).
    pub(crate) fn view_with_modules(
        &self,
        name: &str,
        frontier: CommitSeq,
        query: &ViewQuery,
        modules: &crate::modules::ModuleAggregate,
    ) -> Result<Option<ViewResult>, EngineError> {
        self.view_with_impl(name, frontier, query, Some(modules))
    }

    fn view_with_impl(
        &self,
        name: &str,
        frontier: CommitSeq,
        query: &ViewQuery,
        modules: Option<&crate::modules::ModuleAggregate>,
    ) -> Result<Option<ViewResult>, EngineError> {
        // A plain top-level view (§7) takes no parameters; a `$public`/role surface
        // view (§10.1) reads `$params`/`$actor`. Resolve the plain view first, then
        // the surface view addressed by `name`.
        let surface = self.compiled.surface_view(name);
        let (expr, params) = match self.compiled.view(name) {
            Some(view) => (&view.expr, None),
            None => match surface {
                Some(surface) => (&surface.expr, Some(surface.params.as_slice())),
                None => return Ok(None),
            },
        };
        let snapshot = self.store.snapshot(frontier)?;
        let schema = Schema::new(&self.model);
        let prospective = Prospective::from_snapshot(&snapshot, schema);
        let keyrings = self.keyring_snapshots();
        let mut ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: 0,
            keyrings: &keyrings,
            placements: &self.blob_placements,
            // §13.1: a module instance's `$view` reads its installed `$config`;
            // `$actor`/`$session` are merged in below when the query supplies them.
            context: self.base_context(),
            // §16.3: a `$view` may call a resolved *pure* host namespace (the
            // checker admits only pure functions in a read position), so a view
            // read carries the live dispatch to evaluate it. Signing (cose) and
            // effectful namespaces never reach a view — they are rejected at load.
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            // §13.9: the installed children a `.modules::iface` read aggregates,
            // supplied only on the parent-host module-aware read path.
            modules,
        };
        // §10.1: bind each supplied argument, then fill an omitted declared
        // parameter with its declared default (§8.3), so a `$view` reading `@name`
        // resolves whether or not the caller supplied it.
        ctx.params = self.bind_params(&ctx, &prospective, query, params)?;
        // §11.1/§11.3: a role `$view` reads `$actor`/`$session`, bound to the row
        // each key resolves at this frontier — the same re-materialization an
        // authenticated admission performs, so the view sees state as of the read.
        ctx.context
            .extend(bind_context(&self.compiled, &ctx, &prospective, query.actor_key(), query.session_key()));
        // §10.3/§10.5: a scoped-role surface view reads `.` as the role-holding row
        // keyed by the request scope, and — under `$recursive` — nests the same
        // projection through the checked descendant relation as a keyed tree. This
        // is materialized directly over the covered row (already carrying its
        // self-referential nested collections in full, §5.4/§5.8), not through the
        // root-rooted evaluation a `$public`/package-level view takes.
        if let Some(scope) = surface.and_then(|surface| surface.scope.as_ref()) {
            return scope.materialize(&ctx, &prospective, expr, query.scope_key());
        }
        let current = Cell::Row(Box::new(ctx.root(&prospective)));
        let env = ctx.env(&prospective);
        // §12.2: a `$view` delivers a row stream. Evaluate in view context so a
        // scalar/composite-key selection the view wraps (`.people['a'] { … }`)
        // yields its 0/1-row view — one row when present, none when absent — rather
        // than a coerced single row or the one-row cardinality rejection an
        // ordinary evaluation raises (§6.3). A scalar/aggregate view stays a value.
        let cell = expr
            .evaluate_view(&env, &current)
            .map_err(|error| EngineError::Internal(error.message()))?;
        // §7.3/§12.2: carry the view's total `$sort` order alongside the rows so a
        // bounded window partitions at its gap coordinate through the same order the
        // evaluator sorted by.
        Ok(Some(ViewResult::from_cell(&cell, self.compiled.view_order_of(expr))))
    }

    /// §10.3/§10.5: resolve the receiver a scoped-role addressed call mutates — the
    /// role-holding row keyed by `scope_key` (the empty `descendant` path), or a
    /// covered descendant addressed by `descendant` (its key path down through
    /// `$field`/`$through`) — at `frontier`.
    ///
    /// The addressing walk re-evaluates the recursive coverage relation at every
    /// step, so a descendant that is not a strict, `$where`-included, non-`$except`
    /// step, or a scope that names no live row, is [`ScopedResolution::Denied`] —
    /// indistinguishable by class from a nonexistent address (§10.4), so a bad key
    /// path is no oracle. `actor_key` binds `$actor` for a `$where`/`$except`
    /// predicate that reads it (§11.1), re-materialized from committed state at
    /// `frontier`. An `address` that is not a scoped-role surface is
    /// [`ScopedResolution::Unscoped`]: its receiver comes from the call's own
    /// arguments, exactly as an ordinary public or package-level-role call.
    pub fn scoped_receiver(
        &self,
        address: &str,
        frontier: CommitSeq,
        actor_key: Option<&liasse_value::Value>,
        scope_key: &[liasse_value::Value],
        descendant: &[liasse_value::Value],
    ) -> Result<crate::recursion::ScopedResolution, EngineError> {
        use crate::recursion::ScopedResolution;
        let Some(scope) =
            self.compiled.surface_view(address).and_then(|surface| surface.scope.as_ref())
        else {
            return Ok(ScopedResolution::Unscoped);
        };
        let snapshot = self.store.snapshot(frontier)?;
        let schema = Schema::new(&self.model);
        let prospective = Prospective::from_snapshot(&snapshot, schema);
        let keyrings = self.keyring_snapshots();
        let mut ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: 0,
            keyrings: &keyrings,
            placements: &self.blob_placements,
            context: self.base_context(),
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            modules: None,
        };
        // §11.1/§10.3: bind `$actor` so a `$where`/`$except` predicate reading it
        // resolves the actor row at this frontier, exactly as the coverage read does.
        ctx.context.extend(bind_context(&self.compiled, &ctx, &prospective, actor_key, None));
        match scope.resolve_receiver(&ctx, &prospective, scope_key, descendant)? {
            Some(receiver) => Ok(ScopedResolution::Receiver(receiver)),
            None => Ok(ScopedResolution::Denied),
        }
    }

    /// The parameter cells a surface `$view` read runs against (§10.1): each
    /// supplied argument, then each omitted declared parameter bound to its
    /// declared default (or `none` when it declares none, §8.3). A plain view
    /// declares no parameters, so it just carries whatever the query supplied.
    fn bind_params(
        &self,
        ctx: &EvalCtx<'_>,
        prospective: &Prospective,
        query: &ViewQuery,
        declared: Option<&[crate::compiled::CompiledParam]>,
    ) -> Result<BTreeMap<String, Cell>, EngineError> {
        let mut params: BTreeMap<String, Cell> = query
            .params()
            .iter()
            .map(|(name, value)| (name.clone(), Cell::Scalar(value.clone())))
            .collect();
        for param in declared.unwrap_or(&[]) {
            if params.contains_key(&param.name) {
                continue;
            }
            let cell = match &param.default {
                Some(default) => {
                    let root = Cell::Row(Box::new(ctx.root(prospective)));
                    ctx.eval(prospective, default, &root)
                        .map_err(|rejection| EngineError::Internal(rejection.message().to_owned()))?
                }
                None => Cell::Scalar(liasse_value::Value::None),
            };
            params.insert(param.name.clone(), cell);
        }
        Ok(params)
    }

    /// Evaluate a named view against current committed state (the head frontier)
    /// with no parameters or actor identity ([`Engine::view`]).
    pub fn view_at_head(&self, name: &str) -> Result<Option<ViewResult>, EngineError> {
        self.view(name, self.store.head())
    }

    /// Evaluate an `$expose`d interface's `$view` against this instance at head
    /// (§13.8/§13.9) — the cross-boundary read a parent or peer performs through
    /// the module interface handle (`::templates`). The boundary grants access only
    /// to the fields the exposed projection selects, so a private field the
    /// projection omits never appears in the result and a private child path is
    /// unreachable through the interface (§13.8 isolation). No parameters and no
    /// actor identity cross the boundary — the projection reads only the child's
    /// own committed state. Returns `None` when no interface of that name exposes a
    /// readable `$view` (an absent or mutation-only interface).
    pub fn interface_read(&self, interface: &str) -> Result<Option<ViewResult>, EngineError> {
        let Some(cell) = self.interface_cell(interface)? else { return Ok(None) };
        let order = self
            .compiled
            .exposed_view(interface)
            .map_or_else(SortOrder::unordered, |expr| self.compiled.view_order_of(expr));
        Ok(Some(ViewResult::from_cell(&cell, order)))
    }

    /// The rows an `$expose`d interface `$view` projects through the boundary
    /// (§13.8), keeping each row's identity key so a §13.9 aggregation reads
    /// `iface.$key` and `iface.field` faithfully. `None` when no interface of that
    /// name exposes a readable `$view`. This is the key-preserving form the parent
    /// [`ModuleHost`](crate::ModuleHost) folds into a `.modules::iface` read; the
    /// public [`Engine::interface_read`] drops row identity to scalar output fields.
    pub(crate) fn interface_rows(&self, interface: &str) -> Result<Option<Vec<liasse_expr::Row>>, EngineError> {
        Ok(self.interface_cell(interface)?.map(|cell| match cell {
            Cell::Collection(rows) => rows,
            Cell::Row(row) => vec![*row],
            Cell::Scalar(_) => Vec::new(),
        }))
    }

    /// Evaluate the `$expose`d interface `$view` for `interface` against this
    /// instance at head (§13.8/§13.9), or `None` when no interface of that name
    /// exposes a readable `$view`. The boundary grants access only to the fields the
    /// exposed projection selects, so a private field never appears in the result
    /// (§13.8 isolation); no parameters and no actor cross the boundary.
    fn interface_cell(&self, interface: &str) -> Result<Option<Cell>, EngineError> {
        let Some(expr) = self.compiled.exposed_view(interface) else {
            return Ok(None);
        };
        let snapshot = self.store.snapshot(self.store.head())?;
        let schema = Schema::new(&self.model);
        let prospective = Prospective::from_snapshot(&snapshot, schema);
        let keyrings = self.keyring_snapshots();
        let ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: 0,
            keyrings: &keyrings,
            placements: &self.blob_placements,
            // §13.1: the exposed interface `$view` reads the child's installed
            // `$config`, so `.templates { …, currency: $config.currency }` resolves.
            context: self.base_context(),
            hosts: HostDispatch::new(&self.host, &self.keyrings, self.clock),
            // A child interface read resolves against the child's own state; it
            // exposes no further module spaces of its own here.
            modules: None,
        };
        let current = Cell::Row(Box::new(ctx.root(&prospective)));
        let env = ctx.env(&prospective);
        let cell = expr
            .evaluate_view(&env, &current)
            .map_err(|error| EngineError::Internal(error.message()))?;
        Ok(Some(cell))
    }

    /// The `$expose`d interface names that carry a readable `$view` (§13.8), in
    /// declaration order — the interfaces [`Engine::interface_read`] serves and a
    /// parent aggregates over (§13.9).
    pub fn exposed_interface_names(&self) -> impl Iterator<Item = &str> {
        self.compiled.exposed_views.iter().map(|e| e.interface.as_str())
    }

    /// The `(field, type)` pairs the `$expose`d interface `$view` for `interface`
    /// projects across the boundary (§13.8), or `None` when no such interface is
    /// declared. The output field types a parent's `$interfaces` `$view` contract is
    /// checked against for structural satisfaction at install.
    pub(crate) fn exposed_view_fields(&self, interface: &str) -> Option<Vec<(String, liasse_value::Type)>> {
        let expr = self.compiled.exposed_view(interface)?;
        let row = expr.ty().as_view().or_else(|| expr.ty().as_row())?;
        Some(
            row.fields()
                .filter_map(|(name, ty)| ty.as_scalar().map(|ty| (name.clone(), ty.clone())))
                .collect(),
        )
    }

    /// The `$interfaces` boundary contracts of the `$modules` space at declaration
    /// path `path` (§13.8), if this package declares one there — the contract a
    /// child's `$expose` must structurally satisfy at install.
    pub(crate) fn module_space_interfaces(
        &self,
        path: &[String],
    ) -> Option<&[crate::compiled::CompiledInterfaceContract]> {
        self.compiled.module_space_interfaces(path)
    }

    /// Resolve an `$expose`d interface mutation to the private root mutation it
    /// binds (§13.8): the interface handle `interface` and the contract name
    /// `mutation` map to a bound reference like `.create_template`, whose child
    /// mutation name this returns (`create_template`). `None` when the interface
    /// binds no such mutation, or the binding is a row-scoped or inline program the
    /// CORE dispatch does not yet route (a documented seam).
    pub(crate) fn exposed_mutation(&self, interface: &str, mutation: &str) -> Option<String> {
        let iface = self.model.exposed_interfaces().iter().find(|i| i.name.as_str() == interface)?;
        let bound = iface.muts.iter().find(|m| m.name.as_str() == mutation)?;
        exposed_mutation_name(&bound.binding.text)
    }

    /// The dotted addresses of every compiled `$public`/role surface `$view`
    /// (`public.<name>`, `<role>.<name>`, §10.1) — the names [`Engine::view_with`]
    /// serves. Lets the surface layer discover which of its declared surfaces the
    /// runtime compiled a param- and actor-aware view for.
    pub fn surface_view_addresses(&self) -> impl Iterator<Item = &str> {
        self.compiled.surface_views.iter().map(|v| v.address.as_str())
    }

    /// The declared `$params` of the surface `$view` at `address` as
    /// `(name, scalar type)` pairs (§10.1) — the contract a client's `view`
    /// arguments decode against before [`Engine::view_with`] binds them. Empty when
    /// no surface view of that name is declared or it takes no parameters.
    pub fn surface_view_params(&self, address: &str) -> Vec<(String, liasse_value::Type)> {
        self.compiled
            .surface_view(address)
            .into_iter()
            .flat_map(|view| view.params.iter())
            .filter_map(|param| param.ty.as_scalar().map(|ty| (param.name.clone(), ty.clone())))
            .collect()
    }
}

fn rejected(reason: RejectionReason, message: impl Into<String>) -> CallOutcome {
    CallOutcome::Rejected(Rejection::new(reason, message))
}

/// The child root-mutation name a simple `$expose` `$mut` binding names (§13.8):
/// `.create_template` → `create_template`. Only a bare root-mutation reference is
/// routed by the CORE interface-mutation dispatch; a row-scoped receiver
/// (`.templates[@t].disable`) or an inline program is a documented seam, so this
/// returns `None` for anything but a leading-dot bare identifier.
fn exposed_mutation_name(binding: &str) -> Option<String> {
    let text = binding.trim();
    let text = text.strip_prefix('=').map_or(text, str::trim);
    let rest = text.strip_prefix('.')?;
    let rest = rest.strip_suffix("()").unwrap_or(rest);
    if rest.is_empty() || !rest.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(rest.to_owned())
}

/// The request-scoped `$actor`/`$session` structural cells an authenticated
/// admission binds (§11.1). Each is the row the request's resolved key names,
/// re-materialized from committed state at the admission position (§10.3) as the
/// same read-facing row cell a receiver observes. A key that resolves no live row
/// (a race with a concurrent delete) binds nothing, so a program that reads the
/// binding faults exactly as an unbound structural — fail closed (§6.3).
fn auth_context(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    request: &CallRequest,
) -> BTreeMap<String, Cell> {
    bind_context(compiled, ctx, prospective, request.actor_key(), request.session_key())
}

/// Bind the `$actor`/`$session` structural cells from the `actor_key`/`session_key`
/// a request or view read supplies (§11.1). Each is the row that key names in the
/// declared actor/session collection, re-materialized from committed state at the
/// read position (§10.3, §11.3) as the same read-facing row cell a receiver
/// observes. An absent collection or key, or a key that resolves no live row,
/// binds nothing, so a program or view reading the binding faults exactly as an
/// unbound structural — fail closed (§6.3).
fn bind_context(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    actor_key: Option<&liasse_value::Value>,
    session_key: Option<&liasse_value::Value>,
) -> BTreeMap<String, Cell> {
    let mut context = BTreeMap::new();
    let mut bind = |name: &str, path: &Option<Vec<String>>, key: Option<&liasse_value::Value>| {
        let (Some(path), Some(key)) = (path, key) else { return };
        // §5.4/§11.1: the actor/session row's key is its application-visible
        // identity — a positional `Value::Composite` when the actor/session
        // collection is composite-keyed. Route it through `key_value_of` so the
        // binding addresses the stored N-component row rather than a one-component
        // `KeyValue::single` that would never match (fail-closed, unbound §6.3).
        let address = crate::materialize::top_address(
            &path.join("/"),
            crate::materialize::key_value_of(key),
        );
        if let Some(cell) = ctx.materialize_row_cell(prospective, path, &address) {
            context.insert(name.to_owned(), cell);
        }
    };
    bind("actor", &compiled.actor_collection, actor_key);
    bind("session", &compiled.session_collection, session_key);
    context
}

/// Bind each declared parameter to its supplied argument (§8.3).
fn collect_params(
    mutation: &CompiledMutation,
    request: &CallRequest,
) -> Result<BTreeMap<String, Cell>, Rejection> {
    let mut params = BTreeMap::new();
    for (name, ty) in &mutation.params {
        match request.arg_value(name) {
            Some(value) => {
                params.insert(name.clone(), Cell::Scalar(value.clone()));
            }
            // §8.3/§A.1: an omitted argument for an optional parameter binds the
            // absent value `none` (assigning it clears an optional field, §8.5),
            // rather than rejecting; a required parameter must be supplied.
            None if is_optional(ty) => {
                params.insert(name.clone(), Cell::Scalar(liasse_value::Value::None));
            }
            None => {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!("missing argument `@{name}`"),
                ));
            }
        }
    }
    Ok(params)
}

/// Whether a parameter's type is optional (§8.3): a missing argument for it
/// binds `none` rather than rejecting.
fn is_optional(ty: &liasse_expr::ExprType) -> bool {
    matches!(ty.as_scalar(), Some(liasse_value::Type::Optional(_)))
}

/// The receiver row of a row mutation from the request key (§8.2), or `None`
/// for a root/struct mutation. A nested-collection mutation (§5.4) addresses its
/// receiver through the whole ancestor chain: the receiver key supplies each
/// level's key components in declaration order, so the address descends
/// `companies/co/accounts/a1` rather than a spurious top-level `accounts/co:a1`.
fn receiver_target(
    compiled: &Compiled,
    mutation: &CompiledMutation,
    request: &CallRequest,
) -> Result<Option<RowTarget>, Rejection> {
    if mutation.receiver_is_root || mutation.path.is_empty() {
        return Ok(None);
    }
    // §10.5: a scoped-role covered-descendant call addresses a row below the
    // mutation's declared collection (`companies[root].subcompanies[a]` through a
    // `companies` mutation), so the receiver descends the request's own path
    // override — validated as an included descendant chain by the surface's §10.5
    // admission — rather than the mutation's declared path. An ordinary call carries
    // none, addressing the receiver at the mutation's own location.
    let path = request.receiver_path_override().unwrap_or(&mutation.path);
    let mut remaining = request.receiver_key();
    let mut address: Option<RowAddress> = None;
    let mut prefix: Vec<String> = Vec::with_capacity(path.len());
    for name in path {
        prefix.push(name.clone());
        let arity = compiled.collection_at(&prefix).map_or(1, |c| c.key.len().max(1));
        if remaining.len() < arity {
            return Err(Rejection::new(RejectionReason::Malformed, "a row mutation requires a receiver key"));
        }
        let (components, rest) = remaining.split_at(arity);
        remaining = rest;
        let Some((first, tail)) = components.split_first() else {
            return Err(Rejection::new(RejectionReason::Malformed, "a row mutation requires a receiver key"));
        };
        let key = KeyValue::composite(first.clone(), tail.iter().cloned());
        let step = AddressStep::new(NameSegment::new(name.clone()), key);
        address = Some(match address {
            None => RowAddress::root(step),
            Some(parent) => parent.child(step),
        });
    }
    let address = address.ok_or_else(|| {
        Rejection::new(RejectionReason::Malformed, "a row mutation requires a receiver key")
    })?;
    Ok(Some(RowTarget { address, path: path.to_vec() }))
}

fn stage<T: Transition>(txn: &mut T, changes: Vec<Change>) -> Result<(), EngineError> {
    for change in changes {
        match change {
            Change::Insert(address, value) => {
                txn.insert(address, value)?;
            }
            Change::Update(address, value) => txn.update(&address, value)?,
            Change::Delete(address) => txn.delete(&address)?,
        }
    }
    Ok(())
}

/// Evaluate a mutation's `return` from the admitted state (§8.6, §8.10).
///
/// `Ok(None)` means there is no `return` (or no receiver row to evaluate it
/// over); `Ok(Some(_))` is the evaluated response; `Err` is a genuine evaluation
/// fault, which is part of the admitted operation and rejects the whole
/// transition (§14.1's `.$between` empty-range rejection is the canonical case).
fn eval_return(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    receiver: &Option<RowTarget>,
    locals: &BTreeMap<String, crate::interp::LocalBind>,
    mutation: &CompiledMutation,
    ret: Option<&(liasse_syntax::Expr, liasse_diag::SourceId)>,
    state_changed: bool,
) -> Result<Option<ResponseValue>, Rejection> {
    let Some((expr, source)) = ret else { return Ok(None) };
    let Some(current) = current_cell(ctx, prospective, receiver) else { return Ok(None) };
    // The `return` may name a `name = …` local (§8.1); resolve every binding's
    // type and cell against the committed state it is evaluated over (§8.10).
    let (types, cells) = crate::interp::local_bindings(locals, ctx, prospective);
    let mut scope = mutation.scope.clone();
    for (name, ty) in types {
        scope = scope.with_binding(name, ty);
    }
    let Ok(typed) = check_expression(&scope, *source, expr) else { return Ok(None) };
    // §6.3: when the program changed nothing, a keyed-selection `return` is a query
    // over unaffected state, so it is delivered as the row view it denotes (a
    // collection). When the program changed state, the same selection names an
    // affected row (a `row-mutation receiver`, a one-row context), delivered as
    // that single row. Only a scalar/composite-key selection projection — one that
    // statically types as a `Row` yet reaches a collection selector — differs
    // between the two; every other shape delivers identically under both paths.
    let deliver_as_view = !state_changed && returns_row_selection(expr);
    let cell = if deliver_as_view {
        ctx.eval_view_with(prospective, &typed, &current, cells)?
    } else {
        ctx.eval_with(prospective, &typed, &current, cells)?
    };
    Ok(Some(ResponseValue::new(cell)))
}

/// Whether `expr` is a projection whose base spine reaches a collection selector
/// (`.coll[k] { … }`), the one `return` shape whose delivered cardinality depends
/// on whether the program changed state (§6.3). A root/struct projection
/// (`. { … }`), a bare field, or an aggregate has no such selector and delivers
/// identically either way, so it is left to the ordinary evaluation path.
fn returns_row_selection(expr: &liasse_syntax::Expr) -> bool {
    use liasse_syntax::ExprKind;
    matches!(&expr.kind, ExprKind::Block { base, .. } if spine_reaches_selector(base))
}

/// Whether the projection spine of `expr` bottoms out at a collection selector,
/// following field access, nested projection, and `::` traversal down the base
/// chain (mirrors the adapter's `selectionless_spine`, inverted).
fn spine_reaches_selector(expr: &liasse_syntax::Expr) -> bool {
    use liasse_syntax::ExprKind;
    match &expr.kind {
        ExprKind::Select { .. } => true,
        ExprKind::Field { base, .. }
        | ExprKind::SameName { base, .. }
        | ExprKind::Block { base, .. } => spine_reaches_selector(base),
        ExprKind::Call { callee, .. } => spine_reaches_selector(callee),
        _ => false,
    }
}

fn current_cell(
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    receiver: &Option<RowTarget>,
) -> Option<Cell> {
    match receiver {
        None => Some(Cell::Row(Box::new(ctx.root(prospective)))),
        Some(receiver) => ctx.materialize_row_cell(prospective, &receiver.path, &receiver.address),
    }
}
