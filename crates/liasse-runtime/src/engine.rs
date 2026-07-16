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
use liasse_expr::{check_expression, Cell};
use liasse_ident::LineageId;
use liasse_model::Model;
use liasse_store::{CommitOutcome, CommitSeq, DefinitionText, InstanceStore, KeyValue, RowAddress, Transition};
use liasse_syntax::parse_document;
use liasse_value::Timestamp;

use crate::compiled::{Compiled, CompiledMutation};
use crate::doc;
use crate::error::{EngineError, Rejection, RejectionReason};
use crate::eval::EvalCtx;
use crate::generator::Generators;
use crate::interp::{Interp, RowTarget};
use crate::outcome::CallOutcome;
use crate::request::CallRequest;
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
}

/// Parse a definition text and compile its model, statements, views, and buckets
/// (§9.2 steps 1–6), returning the reusable [`Compilation`] without admitting any
/// genesis. A static failure is [`EngineError::Invalid`].
pub(crate) fn compile_definition(definition: &str) -> Result<Compilation, EngineError> {
    let mut sources = SourceMap::new();
    let src = sources.add_file("liasse.json", definition.to_owned());
    let document = parse_document(src, definition).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    let model = Model::build(&mut sources, src, &document).map_err(|d| EngineError::Invalid(Box::new(d)))?;
    let model_doc = doc::member(document.root(), "$model")
        .cloned()
        .ok_or_else(|| EngineError::Internal("definition has no `$model`".to_owned()))?;
    let compiled = Compiled::build(&mut sources, &model, &model_doc)?;
    let data = doc::member(document.root(), "$data").cloned();
    Ok(Compilation { sources, model, compiled, data })
}

/// The genesis lineage identifier of an instance (§19.3, D.5): its first
/// lineage, deterministically derived from the instance incarnation so a
/// restore of the same instance reconstructs the same lineage identity.
fn genesis_lineage(instance: &liasse_ident::InstanceId) -> LineageId {
    LineageId::new(format!("{}#L0", instance.as_str()))
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
    /// This instance's genesis lineage (§19.3): the lineage its committed points
    /// belong to, so an export names a stable `(lineage, point)` history point
    /// and an import can classify an incoming artifact against local history.
    lineage: LineageId,
    sources: SourceMap,
}

impl<S: InstanceStore> Engine<S> {
    /// Load `definition` into `store`, validating it statically and admitting
    /// genesis (`$data` seeds through the full rule pipeline) as one commit
    /// (§9.1–§9.3). A static failure returns [`EngineError::Invalid`]; a rejected
    /// seed returns [`EngineError::Seed`].
    ///
    /// Host requirement resolution (§9.2 step 4) is a documented seam: the public
    /// [`Model`] does not expose its `$requires` descriptors, so a registry pass
    /// belongs to the features layer that adds host components.
    pub fn load<G: Generators>(
        store: S,
        definition: &str,
        generator: &mut G,
    ) -> Result<Self, EngineError> {
        let Compilation { sources, model, compiled, data } = compile_definition(definition)?;
        let clock = generator.now();
        let lineage = genesis_lineage(store.instance());
        let mut engine = Self { store, model, compiled, clock, lineage, sources };
        engine.genesis(definition, data.as_ref(), generator)?;
        Ok(engine)
    }

    /// Rebuild an activated instance over `store` from a definition and a
    /// portable state capture (§19.10 restore): compile the definition, then
    /// admit the captured rows verbatim as one genesis-position commit. Unlike
    /// [`Engine::load`] this applies no `$data` seed and no defaults — the capture
    /// is already the authoritative committed state — so a restore reproduces the
    /// exported state exactly.
    pub(crate) fn from_state<G: Generators>(
        store: S,
        definition: &str,
        state: &crate::portable::StateSection,
        generator: &mut G,
    ) -> Result<Self, EngineError> {
        let Compilation { sources, model, compiled, .. } = compile_definition(definition)?;
        let clock = generator.now();
        let lineage = genesis_lineage(store.instance());
        let mut engine = Self { store, model, compiled, clock, lineage, sources };
        engine.install_state(definition, state)?;
        Ok(engine)
    }

    /// This instance's incarnation (D.1).
    #[must_use]
    pub fn instance(&self) -> &liasse_ident::InstanceId {
        self.store.instance()
    }

    /// This instance's genesis lineage (§19.3).
    #[must_use]
    pub(crate) fn lineage(&self) -> &LineageId {
        &self.lineage
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
    /// commit it as the definition-load genesis (§19.10).
    fn install_state(
        &mut self,
        definition: &str,
        state: &crate::portable::StateSection,
    ) -> Result<(), EngineError> {
        let schema = Schema::new(&self.model);
        let mut prospective = Prospective::empty();
        for (name, rows) in state.collections() {
            let Some(model) = schema.top_collection(name) else { continue };
            for fields in rows {
                let Some(key) = crate::materialize::row_key(model, fields) else {
                    return Err(EngineError::Internal(format!(
                        "captured row in `{name}` is missing a key field"
                    )));
                };
                let address = crate::materialize::top_address(name, key);
                prospective.insert(address, fields.clone());
            }
        }
        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        txn.commit()?;
        Ok(())
    }

    /// Replace live state with `state`, staging the exact diff as one commit —
    /// the movement an applied import (fast-forward or rollback) performs
    /// (§19.8). Returns the new head, or the current head when nothing changed.
    pub(crate) fn reinstall_state(
        &mut self,
        state: &crate::portable::StateSection,
    ) -> Result<CommitSeq, EngineError> {
        let schema = Schema::new(&self.model);
        let mut prospective = Prospective::gather(&self.store, schema)?;
        let target = state.working(schema)?;
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
        Ok(match txn.commit()? {
            CommitOutcome::Committed(seq) => seq,
            CommitOutcome::Unchanged => self.store.head(),
        })
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
            CommitOutcome::Committed(seq) => seq,
            CommitOutcome::Unchanged => self.store.head(),
        };
        self.model = target.model;
        self.compiled = target.compiled;
        self.sources = target.sources;
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
    }

    /// Advance the virtual clock by `ticks` of its current precision (§14) — the
    /// `advance_time` step. Saturates rather than overflowing.
    pub fn advance(&mut self, ticks: i128) {
        let count = self.clock.count().saturating_add(ticks);
        self.clock = Timestamp::new(count, self.clock.precision());
    }

    /// The validated package model.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The backing store.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    fn genesis<G: Generators>(
        &mut self,
        definition: &str,
        data: Option<&liasse_syntax::DocValue>,
        generator: &mut G,
    ) -> Result<(), EngineError> {
        let schema = Schema::new(&self.model);
        let ctx = EvalCtx {
            schema,
            compiled: &self.compiled,
            params: BTreeMap::new(),
            now: self.clock,
            seed: generator.next_seed(),
        };
        let mut prospective = Prospective::empty();
        let mut touched = Vec::new();
        if let Some(data) = data {
            crate::seed::admit(&self.compiled, &ctx, &mut prospective, &mut touched, data)
                .map_err(EngineError::Seed)?;
        }
        crate::rules::finalize(&self.compiled, &ctx, &prospective, &touched).map_err(EngineError::Seed)?;

        let changes = prospective.diff();
        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        // §9.3: a definition load creates a commit even when state is unchanged.
        txn.set_definition(DefinitionText::new(definition.to_owned()));
        txn.commit()?;
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
        let ctx =
            EvalCtx { schema, compiled: &self.compiled, params, now: self.clock, seed: generator.next_seed() };

        let receiver = match receiver_target(mutation, request) {
            Ok(receiver) => receiver,
            Err(rejection) => return Ok(CallOutcome::Rejected(rejection)),
        };

        let mut prospective = Prospective::gather(&self.store, schema)?;
        let mut interp = Interp {
            compiled: &self.compiled,
            ctx: &ctx,
            prospective: &mut prospective,
            mutation,
            receiver,
            touched: Vec::new(),
            ret: None,
            locals: BTreeMap::new(),
        };
        if let Err(rejection) = interp.run() {
            return Ok(CallOutcome::Rejected(rejection));
        }
        let touched = std::mem::take(&mut interp.touched);
        let ret = interp.ret.take();
        let locals = std::mem::take(&mut interp.locals);
        let receiver = interp.receiver.take();

        if let Err(rejection) = crate::rules::finalize(&self.compiled, &ctx, &prospective, &touched) {
            return Ok(CallOutcome::Rejected(rejection));
        }

        let changes = prospective.diff();
        if changes.is_empty() {
            // §8.9: no state change → `unchanged`; the response is evaluated from
            // the unchanged state and the frontier does not advance.
            let response =
                eval_return(&self.compiled, &ctx, &prospective, &receiver, &locals, mutation, ret.as_ref());
            return Ok(CallOutcome::Unchanged { response });
        }

        let mut txn = self.store.begin();
        stage(&mut txn, changes)?;
        let seq = match txn.commit()? {
            CommitOutcome::Committed(seq) => seq,
            CommitOutcome::Unchanged => self.store.head(),
        };
        // §8.6: the response is evaluated from the committed resulting state.
        let post = Prospective::gather(&self.store, schema)?;
        let response = eval_return(&self.compiled, &ctx, &post, &receiver, &locals, mutation, ret.as_ref());
        Ok(CallOutcome::Committed { seq, response })
    }

    /// Evaluate a named view against committed state at `frontier` (§7, §12.4).
    /// Returns `None` when no view of that name is declared.
    pub fn view(&self, name: &str, frontier: CommitSeq) -> Result<Option<ViewResult>, EngineError> {
        let Some(view) = self.compiled.view(name) else { return Ok(None) };
        let snapshot = self.store.snapshot(frontier)?;
        let schema = Schema::new(&self.model);
        let prospective = Prospective::from_snapshot(&snapshot, schema);
        // §14: the view is evaluated at the virtual clock, so bucketed
        // collections expose only the rows active at that instant.
        let ctx =
            EvalCtx { schema, compiled: &self.compiled, params: BTreeMap::new(), now: self.clock, seed: 0 };
        let current = Cell::Row(Box::new(ctx.root(&prospective)));
        let env = ctx.env(&prospective);
        let cell = view
            .expr
            .evaluate(&env, &current)
            .map_err(|error| EngineError::Internal(error.message()))?;
        Ok(Some(ViewResult::from_cell(&cell)))
    }

    /// Evaluate a named view against current committed state (the head frontier).
    pub fn view_at_head(&self, name: &str) -> Result<Option<ViewResult>, EngineError> {
        self.view(name, self.store.head())
    }
}

fn rejected(reason: RejectionReason, message: impl Into<String>) -> CallOutcome {
    CallOutcome::Rejected(Rejection::new(reason, message))
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
/// for a root/struct mutation.
fn receiver_target(
    mutation: &CompiledMutation,
    request: &CallRequest,
) -> Result<Option<RowTarget>, Rejection> {
    if mutation.receiver_is_root {
        return Ok(None);
    }
    let Some(collection) = mutation.path.last().cloned() else {
        return Ok(None);
    };
    let key = key_of(request.receiver_key())?;
    let address = crate::materialize::top_address(&collection, key);
    Ok(Some(RowTarget { address, collection }))
}

fn key_of(components: &[liasse_value::Value]) -> Result<KeyValue, Rejection> {
    match components.split_first() {
        Some((first, rest)) => Ok(KeyValue::composite(first.clone(), rest.iter().cloned())),
        None => Err(Rejection::new(
            RejectionReason::Malformed,
            "a row mutation requires a receiver key",
        )),
    }
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
fn eval_return(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    receiver: &Option<RowTarget>,
    locals: &BTreeMap<String, crate::interp::LocalBind>,
    mutation: &CompiledMutation,
    ret: Option<&(liasse_syntax::Expr, liasse_diag::SourceId)>,
) -> Option<ResponseValue> {
    let (expr, source) = ret?;
    let current = current_cell(compiled, ctx, prospective, receiver)?;
    // The `return` may name a `name = …` local (§8.1); resolve every binding's
    // type and cell against the committed state it is evaluated over (§8.10).
    let (types, cells) = crate::interp::local_bindings(locals, ctx, prospective);
    let mut scope = mutation.scope.clone();
    for (name, ty) in types {
        scope = scope.with_binding(name, ty);
    }
    let typed = check_expression(&scope, *source, expr).ok()?;
    let cell = ctx.eval_with(prospective, &typed, &current, cells).ok()?;
    Some(ResponseValue::new(cell))
}

fn current_cell(
    compiled: &Compiled,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
    receiver: &Option<RowTarget>,
) -> Option<Cell> {
    match receiver {
        None => Some(Cell::Row(Box::new(ctx.root(prospective)))),
        Some(receiver) => {
            let collection = compiled.collection(&receiver.collection)?;
            let fields = prospective.get(&receiver.address)?;
            Some(ctx.row_cell_of(prospective, collection, fields))
        }
    }
}
