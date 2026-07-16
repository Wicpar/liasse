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
            StepKind::Operator => self.active().operator(&request.target),
            _ => Err(AdapterError::unsupported(unsupported_reason(&request.kind))),
        }
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
        let policy = movement_policy(&request.target);
        self.active().reconcile(&base, &incoming, &policy)
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
        let (router, routing) = router::build(engine.model(), &ctx.package, &plan, &ctx.lift)
            .map_err(|err| AdapterError::Host(format!("sandbox router rebuild failed: {err}")))?;
        Ok(Some(Loaded { host: SurfaceHost::new(engine, router, clock), routing }))
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
        let engine = Engine::load(store, &definition, &mut clock).map_err(|err| err.to_string())?;
        let (router, routing) =
            router::build(engine.model(), &ctx.package, &plan, &ctx.lift).map_err(|err| err.to_string())?;
        Ok(Loaded { host: SurfaceHost::new(engine, router, clock), routing })
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
        StepKind::ModuleInstall
        | StepKind::ModuleUninstall
        | StepKind::ModuleDisable
        | StepKind::ModuleEnable
        | StepKind::ModuleUpdate
        | StepKind::ModuleRename => {
            "the corpus's row-scoped module spaces (`/co/acme/modules`), \
             `.modules[..]::interface` addressing, and `$config`/`$use`/`$deps` peer bindings, \
             which the surface `ModuleDeployment`'s flat name-keyed single-space model does not carry"
        }
        StepKind::BlobPut | StepKind::BlobGet => {
            "a blob-parameter mutation admission that binds a verified §18 descriptor into a \
             surface call backed by a `BlobEngine` and `hosts.connectors` — the surface `call` \
             path admits no blob parameter, and the standalone `BlobHost` façade is field- not \
             mutation-addressed"
        }
        StepKind::KeyringAdmin | StepKind::ProviderSet => {
            "a managed `KeyringAdmin` over a host `KeyProvider` with the §17.9 fault-injection \
             vocabulary the adapter does not provision from the case's `hosts` block"
        }
        StepKind::RunReconciler => {
            "activation of a computed §19.9 merge into a new lineage, which the surface `reconcile` \
             computes but never applies, plus the `apply_correction` conflict-resolution the host \
             correction API the surface does not expose"
        }
        StepKind::BuildArtifact
        | StepKind::LoadArtifact
        | StepKind::TamperArtifact
        | StepKind::RepackArtifact
        | StepKind::InspectArtifact
        | StepKind::ExtractArtifact
        | StepKind::TamperExtract => {
            "full `.liasse` archive assembly from a package plus resource files, with the \
             deterministic entry/digest/manifest tamper ops and recursive Annex D.5 verification, \
             via a `liasse-artifact` archive layer the adapter does not link"
        }
        StepKind::Erase
        | StepKind::Reinsert
        | StepKind::ScrubScopeOfCascadedRow
        | StepKind::ApplyCorrection => {
            "the deletion/erasure/correction host verbs the surface host does not expose"
        }
        StepKind::ConnectorSet | StepKind::BudgetSet => {
            "a host `BlobConnector`/component budget control provisioned from the case's `hosts` \
             block, which the adapter does not wire"
        }
        _ => "engine wiring the current layer does not expose",
    };
    format!("`{}` needs {need}", kind.key())
}
