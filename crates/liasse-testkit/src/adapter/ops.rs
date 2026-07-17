//! Dispatching the registry and chapter-local [`OpRequest`] steps.
//!
//! The core client verbs (`connect`/`call`/`watch`/…) route through the active
//! instance's surface host ([`Instance`]); the op steps here are the ones that
//! either reach a §19 host operation or a family the current layer does not wire.
//! `export`/`import`/`reconcile` move `.liasse` bytes through the adapter's shared
//! [`artifacts`](super::ScenarioAdapter::artifacts) table; `restore` activates an
//! isolated sandbox instance from an artifact (§19.10); `host_load` and `operator`
//! drive on the active instance. Every remaining family ([`unsupported_reason`]
//! names each precisely) stays a precise [`AdapterError::unsupported`] skip.

use liasse_runtime::{Engine, ImportRelation, Precision};
use liasse_store::{InstanceStore, MemoryStore};
use liasse_surface::{SurfaceHost, VirtualClock as SurfaceClock};

use crate::contract::Observation;
use crate::outcome::Outcome;
use crate::request::OpRequest;
use crate::step_kind::StepKind;

use super::{auth::AuthPlan, router, AdapterError, Loaded, EPOCH_MICROS};

impl<S: InstanceStore> super::ScenarioAdapter<S> {
    /// Dispatch a non-core op step to its driver, or report a precise skip for a
    /// family the current layer does not wire.
    pub(super) fn drive_op(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        match request.kind {
            StepKind::Export => self.drive_export(request),
            StepKind::Import => self.drive_import(request),
            StepKind::Reconcile => self.drive_reconcile(request),
            StepKind::Restore => self.drive_restore(request),
            StepKind::HostLoad => self.drive_host_load(request),
            StepKind::ModuleInstall => self.module_state()?.install(&request.target),
            StepKind::ModuleDisable => self.module_state()?.disable(&request.target),
            StepKind::ModuleEnable => self.module_state()?.enable(&request.target),
            StepKind::ModuleUninstall => self.module_state()?.uninstall(&request.target),
            StepKind::ModuleRename => self.module_state()?.rename(&request.target),
            StepKind::ModuleUpdate => self.module_state()?.update(&request.target),
            StepKind::BuildArtifact => self.drive_build_artifact(request),
            StepKind::RepackArtifact => self.drive_repack_artifact(request),
            StepKind::LoadArtifact => self.drive_load_artifact(request),
            StepKind::TamperArtifact => self.drive_tamper_artifact(request),
            StepKind::InspectArtifact => self.drive_inspect_artifact(request),
            StepKind::ExtractArtifact => self.drive_extract_artifact(request),
            StepKind::ApplyCorrection => self.drive_apply_correction(request),
            StepKind::Operator => self.active().operator(&request.target),
            StepKind::OperationStatus => self.drive_operation_status(request),
            StepKind::Manifest => self.drive_manifest(request),
            StepKind::Resume => self.drive_resume(request),
            StepKind::Authenticate => self.drive_authenticate(request),
            StepKind::ExpectClose => self.drive_expect_close(request),
            StepKind::BlobPut => self.drive_blob_put(request),
            StepKind::BlobGet => self.drive_blob_get(request),
            StepKind::ConnectorSet => self.drive_connector_set(request),
            StepKind::ProviderSet => self.drive_provider_set(request),
            StepKind::KeyringAdmin => self.drive_keyring_admin(request),
            _ => Err(AdapterError::unsupported(unsupported_reason(&request.kind))),
        }
    }

    /// §18.7 `blob_put`: stage a §18 descriptor and admit its mutation through the
    /// composed blob host, converting a lying/oversize/unaccepted descriptor into a
    /// pre-admission rejection (§18.1/§18.2).
    fn drive_blob_put(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(call) = target.get("call").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`blob_put` step carries no `call` mutation address"));
        };
        let Some(param) = target.get("param").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`blob_put` step carries no blob `param` name"));
        };
        let content = target.get("content").and_then(serde_json::Value::as_str).unwrap_or_default();
        let media = target.get("media").and_then(serde_json::Value::as_str).unwrap_or_default();
        let connection = target
            .get("on")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| super::connection_name(request.on.as_ref()));
        let spec = super::blobs::BlobPutSpec {
            call: call.to_owned(),
            param: param.to_owned(),
            args: target.get("args").cloned().unwrap_or(serde_json::Value::Null),
            content: content.as_bytes().to_vec(),
            media: media.to_owned(),
            name: target.get("name").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
            claim: target.get("claim").cloned(),
            operation_id: target.get("operation_id").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
            connection,
        };
        self.active().blob_put(&spec)
    }

    /// §17.9 `provider_set`: reconfigure the engine keyring's backing provider from
    /// this step onward, so a later `cose.sign` mutation or due rotation fails per
    /// the injected fault.
    fn drive_provider_set(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(spec) = super::keyrings::ProviderSetSpec::parse(&request.target) else {
            return Err(AdapterError::unsupported("`provider_set` step carries no provider configuration"));
        };
        self.active().provider_set(&spec)
    }

    /// §17.3/§17.4 `keyring_admin`: a keyring lifecycle transition against the
    /// engine's self-provisioned ring.
    fn drive_keyring_admin(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(spec) = super::keyrings::KeyringAdminSpec::parse(&request.target) else {
            return Err(AdapterError::unsupported("`keyring_admin` step carries no `ring`/`op`"));
        };
        self.active().keyring_admin(&spec)
    }

    /// §18.12 `connector_set`: reconfigure a simulated connector from this step on.
    fn drive_connector_set(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let fail = target
            .get("fail")
            .and_then(serde_json::Value::as_array)
            .map(|list| list.iter().filter_map(|op| connector_op(op.as_str()?)).collect())
            .unwrap_or_default();
        let spec = super::blobs::ConnectorSetSpec {
            connector: target.get("connector").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
            available: target.get("available").and_then(serde_json::Value::as_bool),
            fail,
            corrupt: target.get("corrupt").and_then(serde_json::Value::as_str).map(ToOwned::to_owned),
        };
        self.active().connector_set(&spec)
    }

    /// §18.8/§18.9 `blob_get`: a precise seam. Fetch visibility is the §18.8
    /// authorization over the caller's surface projection, and the corpus's fetch
    /// cases resolve it through role-scoped/filtered surface views (or a blob-value
    /// view the model layer does not yet compile) — a projection evaluation the
    /// composed blob host, keyed only by digest, does not perform.
    fn drive_blob_get(&mut self, _request: &OpRequest) -> Result<Observation, AdapterError> {
        Err(AdapterError::unsupported(
            "`blob_get` needs the §18.8 fetch-visibility decision over the caller's surface \
             projection (role-scoped/filtered views, or a blob-value view the model does not \
             compile), which the digest-keyed composed blob host does not evaluate",
        ))
    }

    /// §12.3 `operation_status`: query the retained status of the operation whose
    /// high-entropy identifier the step names — the identifier is the capability, so
    /// an unrecorded one reports `unknown`.
    fn drive_operation_status(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(id) = request.target.get("id").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`operation_status` step carries no operation `id`"));
        };
        let id = id.to_owned();
        self.active().operation_status(&id)
    }

    /// §12.1 `manifest`: the surfaces granted to the step's connection context.
    fn drive_manifest(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let connection = super::connection_name(request.on.as_ref());
        let context = request.member("context").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
        self.active().manifest(&connection, context.as_deref())
    }

    /// §12.2 `resume`: reopen a subscription over the named surface from a retained
    /// frontier. The retained `from` is a hint the surface reconstructs a fresh
    /// `init` over, so it is parsed leniently (an absent/opaque token resumes from
    /// genesis, which the surface's current-frontier reconstruction still covers).
    fn drive_resume(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let connection = super::connection_name(request.on.as_ref());
        let Some(surface) = request.target.get("surface").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`resume` step carries no `surface` address"));
        };
        let Some(watch_id) = request.target.get("id").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`resume` step carries no subscription `id`"));
        };
        let from = request
            .target
            .get("from")
            .and_then(serde_json::Value::as_u64)
            .map_or(liasse_runtime::CommitSeq::GENESIS, liasse_runtime::CommitSeq::from_stored);
        let (surface, watch_id) = (surface.to_owned(), watch_id.to_owned());
        self.active().resume(&connection, &surface, &watch_id, from)
    }

    /// §12.2 `expect_close`: report subscription `watch`'s close reason, so the
    /// step can assert the runtime closed the live view when state removed its
    /// authority. A still-live subscription yields a value-less `ok`, which the
    /// executor judges as "expected close, none observed".
    fn drive_expect_close(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(watch_id) = request.target.get("watch").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`expect_close` step carries no subscription `watch`"));
        };
        let watch_id = watch_id.to_owned();
        self.active().close_reason(&watch_id)
    }

    /// §11.4/§11.8 `authenticate`: bind (or refuse) an authentication context on the
    /// step's connection, optionally naming a multiplexed context (`as`).
    fn drive_authenticate(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let connection = super::connection_name(request.on.as_ref());
        let context = request.member("as").and_then(serde_json::Value::as_str).map(ToOwned::to_owned);
        let payload = request.target.clone();
        self.active().authenticate_op(&connection, &payload, context.as_deref())
    }

    /// §19.5 `export`: capture the active instance's committed boundary as verified
    /// `.liasse` bytes and hold them under the step's `as` label, so a later
    /// `import`/`reconcile` consumes them. When the export runs inside a sandbox,
    /// its §19.9 merge base is the artifact the sandbox was restored from — the
    /// shared ancestor the exported point diverged from — recorded here so a later
    /// `reconcile` can name the base the corpus step leaves implicit.
    fn drive_export(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(label) = request.target.get("as").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`export` step carries no `as` label"));
        };
        let label = label.to_owned();
        let bytes = self.active().export()?;
        if let Some(Some(origin)) = self.sandbox_origins.last() {
            self.artifact_origin.insert(label.clone(), origin.clone());
        }
        self.artifacts.insert(label, bytes);
        Ok(Observation::ok(None))
    }

    /// §19.8 `import`: classify the named artifact against the active instance's
    /// history and, when the movement policy permits, activate it.
    fn drive_import(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let bytes = self.artifact_bytes(&request.target, "import")?;
        let policy = movement_policy(&request.target);
        self.active().import(&bytes, &policy)
    }

    /// §19.9 `reconcile`: compute the three-way merge of the named incoming
    /// artifact against the active instance's live state. The merge base is the
    /// shared ancestor the incoming diverged from — the artifact the incoming's
    /// producing sandbox was restored from — which the corpus step leaves implicit.
    fn drive_reconcile(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let incoming = self.artifact_bytes(&request.target, "reconcile")?;
        let Some(from) = request.target.get("from").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`reconcile` step carries no `from` artifact label"));
        };
        let Some(base) = self.artifact_origin.get(from).and_then(|origin| self.artifacts.get(origin)).cloned()
        else {
            return Err(AdapterError::unsupported(
                "`reconcile` base artifact (the shared ancestor the incoming diverged from) is not in \
                 scope: the incoming was not produced by a restored sandbox",
            ));
        };
        // §19.9: a `bind_plan` retains this plan's base/incoming bytes so a later
        // `apply_correction` recovers and resolves it (local state is unchanged
        // between the two steps).
        if let Some(label) = request.target.get("bind_plan").and_then(serde_json::Value::as_str) {
            self.plans.insert(
                label.to_owned(),
                super::correction::ReconcilePlan { base: base.clone(), incoming: incoming.clone() },
            );
        }
        let policy = movement_policy(&request.target);
        self.active().reconcile(&base, &incoming, &policy)
    }

    /// §19.9 `apply_correction`: recover the reconciliation plan the step's `plan`
    /// label bound, and resolve its conflicts under the step's display-path-keyed
    /// `choose` map against the active instance, activating the corrected
    /// composition (adapter/correction.rs).
    fn drive_apply_correction(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let target = &request.target;
        let Some(label) = target.get("plan").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported("`apply_correction` step carries no `plan` label"));
        };
        let Some(choose) = target.get("choose").cloned() else {
            return Err(AdapterError::unsupported("`apply_correction` step carries no `choose` map"));
        };
        let Some(plan) = self.plans.get(label) else {
            return Err(AdapterError::unsupported(format!(
                "`apply_correction` names no bound reconciliation plan `{label}`"
            )));
        };
        let (base, incoming) = (plan.base.clone(), plan.incoming.clone());
        self.active().apply_correction(&base, &incoming, &choose)
    }

    /// §19.10 `restore`: activate the current sandbox instance over a throwaway
    /// in-memory store from a verified artifact. Verification (§19.8) runs first,
    /// so a tampered artifact observes `invalid` and no instance is activated. The
    /// restored instance takes the base incarnation and its genesis lineage, so an
    /// artifact it later exports classifies against the base's history.
    fn drive_restore(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        if self.sandboxes.is_empty() {
            return Err(AdapterError::unsupported(
                "`restore` activates an isolated instance and is only driven inside an `in_sandbox` group",
            ));
        }
        let bytes = self.artifact_bytes(&request.target, "restore")?;
        let Some(from) = request.target.get("from").and_then(serde_json::Value::as_str).map(ToOwned::to_owned)
        else {
            return Err(AdapterError::unsupported("`restore` step carries no `from` artifact label"));
        };
        let loaded = match Self::restore_stack(&self.load_ctx, &bytes) {
            Ok(Some(loaded)) => loaded,
            Ok(None) => return Ok(Observation::outcome(Outcome::Invalid)),
            Err(error) => return Err(error),
        };
        let Some(top) = self.sandboxes.last_mut() else {
            return Err(AdapterError::unsupported("`restore` requires an open `in_sandbox` group"));
        };
        top.install(loaded);
        if let Some(origin) = self.sandbox_origins.last_mut() {
            *origin = Some(from);
        }
        Ok(Observation::ok(None))
    }

    /// Build a sandbox stack by restoring a verified artifact over a fresh
    /// in-memory store at the base incarnation, replaying the base wiring. `Ok(None)`
    /// is a failed §19.8 verification (an `invalid` observation); `Err` is a harness
    /// fault (the base package's router could not be rebuilt).
    fn restore_stack(
        ctx: &super::LoadContext,
        bytes: &[u8],
    ) -> Result<Option<Loaded<MemoryStore>>, AdapterError> {
        let store = MemoryStore::new(ctx.instance.clone());
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        let engine = match Engine::restore(store, bytes, &mut clock) {
            Ok(engine) => engine,
            Err(_) => return Ok(None),
        };
        let plan = AuthPlan::derive(&ctx.package, ctx.hosts.as_ref());
        let (router, mut routing) = router::build(engine.model(), &ctx.package, &plan, &ctx.lift)
            .map_err(|err| AdapterError::Host(format!("sandbox router rebuild failed: {err}")))?;
        routing.load_view_param_types(&engine);
        let mut host = SurfaceHost::new(engine, router, clock);
        let blobs = super::blobs::provision(&mut host, &ctx.package, ctx.hosts.as_ref());
        Ok(Some(Loaded { host, routing, blobs }))
    }

    /// Load an independent installation of the case package into a fresh in-memory
    /// instance (`in_sandbox` with `fresh: true`): its own genesis and incarnation,
    /// so an artifact it exports shares no history point with the base (§19.8).
    pub(super) fn fresh_stack(
        ctx: &super::LoadContext,
        instance: liasse_ident::InstanceId,
    ) -> Result<Loaded<MemoryStore>, String> {
        let plan = AuthPlan::derive(&ctx.package, ctx.hosts.as_ref());
        let definition = super::prepared_definition(&ctx.package, &plan, &ctx.lift)
            .ok_or_else(|| "prepared definition did not serialize".to_owned())?;
        let store = MemoryStore::new(instance);
        let mut clock = SurfaceClock::new(EPOCH_MICROS, Precision::Micros);
        let engine = super::load_engine(store, &definition, &mut clock, &ctx.package, ctx.hosts.as_ref())
            .map_err(|err| err.to_string())?;
        let (router, mut routing) =
            router::build(engine.model(), &ctx.package, &plan, &ctx.lift).map_err(|err| err.to_string())?;
        routing.load_view_param_types(&engine);
        let mut host = SurfaceHost::new(engine, router, clock);
        let blobs = super::blobs::provision(&mut host, &ctx.package, ctx.hosts.as_ref());
        Ok(Loaded { host, routing, blobs })
    }

    /// §9.2 host lifecycle `load(target)`: re-load the step's package into the
    /// active instance through [`Engine::update`], migrating committed state (§20.1)
    /// and rebinding the router. A rejected migration leaves the instance unchanged
    /// and reports the refusal class.
    fn drive_host_load(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(package) = request.target.get("package").cloned() else {
            return Err(AdapterError::unsupported("`host_load` step carries no `package` to load"));
        };
        self.active().host_load(&package)
    }

    /// The `.liasse` bytes the step's `from` label names, or a precise skip when no
    /// such artifact is in scope.
    fn artifact_bytes(&self, target: &serde_json::Value, action: &str) -> Result<Vec<u8>, AdapterError> {
        let Some(label) = target.get("from").and_then(serde_json::Value::as_str) else {
            return Err(AdapterError::unsupported(format!("`{action}` step carries no `from` artifact label")));
        };
        self.artifacts
            .get(label)
            .cloned()
            .ok_or_else(|| AdapterError::unsupported(format!("`{action}` names no artifact `{label}` in scope")))
    }
}

/// The §19.8 movement policy a step permits, read from its `policy` array of
/// canonical relation tokens. An unknown token is ignored.
fn movement_policy(target: &serde_json::Value) -> Vec<ImportRelation> {
    target
        .get("policy")
        .and_then(serde_json::Value::as_array)
        .map(|tokens| tokens.iter().filter_map(|token| relation_from_token(token.as_str()?)).collect())
        .unwrap_or_default()
}

/// Parse one `connector_set { fail }` operation token (§18.12).
fn connector_op(token: &str) -> Option<liasse_host::sim::ConnectorOp> {
    use liasse_host::sim::ConnectorOp;
    Some(match token {
        "upload" => ConnectorOp::Upload,
        "download" => ConnectorOp::Download,
        "copy" => ConnectorOp::Copy,
        "delete" => ConnectorOp::Delete,
        _ => return None,
    })
}

/// Parse one canonical movement-relation token (§19.8).
fn relation_from_token(token: &str) -> Option<ImportRelation> {
    Some(match token {
        "same_point" => ImportRelation::SamePoint,
        "fast_forward" => ImportRelation::FastForward,
        "rollback" => ImportRelation::Rollback,
        "merge" => ImportRelation::Merge,
        "unrelated" => ImportRelation::Unrelated,
        _ => return None,
    })
}

/// The precise reason an op family is not driven yet — naming the genuine
/// adapter/surface seam each family hits, not merely "unsupported".
fn unsupported_reason(kind: &StepKind) -> String {
    let need = match kind {
        StepKind::RunReconciler => {
            "a background reconciler loop over retained lineages, which the single-step \
             `reconcile`/`apply_correction` verbs do not model"
        }
        StepKind::TamperExtract => {
            "extract-then-tamper over a child-module `.liasse` (§19 embedded artifacts), which needs \
             the runtime's module-artifact embedding the export path does not yet emit"
        }
        StepKind::Erase | StepKind::Reinsert | StepKind::ScrubScopeOfCascadedRow => {
            "the deletion/erasure host verbs the surface host does not expose"
        }
        StepKind::BudgetSet => {
            "a host component budget control provisioned from the case's `hosts` block, which the \
             adapter does not wire"
        }
        _ => "engine wiring the current layer does not expose",
    };
    format!("`{}` needs {need}", kind.key())
}
