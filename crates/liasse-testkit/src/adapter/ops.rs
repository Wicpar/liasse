//! Driving the registry and chapter-local [`OpRequest`] steps over the engine.
//!
//! The core client verbs (`connect`/`call`/`watch`/…) route through the surface
//! host; the op steps here reach past it to the durable engine (§9, §20, §22).
//! Two need only the volatile-state handoff [`SurfaceHost::into_parts`] exposes:
//! a `restart` rebuilds a fresh host over the same engine (§22), and a
//! `host_load` re-loads a new package version through [`Engine::update`] (§9.2,
//! §20) and rebinds the router to the reloaded model. Every other op family —
//! module lifecycle, blobs, keyrings, artifacts, operator transitions — needs a
//! module host, blob engine, key provider, or artifact layer the surface host
//! does not assemble, so it stays a precise [`AdapterError::unsupported`] skip
//! (never a fabricated outcome) for a later phase to wire.

use liasse_store::InstanceStore;
use liasse_surface::{SurfaceHost, VirtualClock as SurfaceClock};

use crate::contract::Observation;
use crate::outcome::{Completion, Outcome};
use crate::request::OpRequest;
use crate::step_kind::StepKind;

use super::auth::AuthPlan;
use super::lift::SurfaceLift;
use super::{AdapterError, Loaded, State};

impl<S: InstanceStore> super::ScenarioAdapter<S> {
    /// Take the loaded stack out of `state`, leaving a sentinel failure in its
    /// place. The caller must restore a `Loaded` state before returning, or every
    /// later step skips with the sentinel — used to move the owned host through
    /// [`SurfaceHost::into_parts`].
    fn take_loaded(&mut self) -> Result<Loaded<S>, AdapterError> {
        let taken = std::mem::replace(&mut self.state, State::Failed("stack in transit".to_owned()));
        match taken {
            State::Loaded(loaded) => Ok(*loaded),
            State::Failed(message) => {
                self.state = State::Failed(message.clone());
                Err(AdapterError::LoadFailed(message))
            }
        }
    }

    /// Restore the loaded stack after a handoff.
    fn restore(&mut self, loaded: Loaded<S>) {
        self.state = State::Loaded(Box::new(loaded));
    }

    /// §22 restart/durability: tear the running host down and rebuild a fresh one
    /// over the same engine, router, and clock. Committed state survives; the
    /// volatile connections, subscriptions, and operation records are dropped, so
    /// the adapter forgets its open connections — a later step must reconnect.
    pub(super) fn drive_restart(&mut self) -> Result<Observation, AdapterError> {
        let loaded = self.take_loaded()?;
        let routing = loaded.routing;
        let (engine, router, clock) = loaded.host.into_parts();
        self.restore(Loaded { host: SurfaceHost::new(engine, router, clock), routing });
        self.open_connections.clear();
        Ok(Observation::ok(None))
    }

    /// Re-open every tracked connection on the current host at its head frontier —
    /// a §9.2 host lifecycle load rebuilds the host but does not drop clients, so
    /// a connection open before the load still resolves after it.
    fn reopen_connections(&mut self) {
        let ids: Vec<String> = self.open_connections.iter().cloned().collect();
        if let State::Loaded(loaded) = &mut self.state {
            for id in ids {
                loaded.host.connect(id);
            }
        }
    }

    /// Dispatch a non-core op step to its engine driver, or report a precise skip
    /// for a family the current layer does not wire.
    pub(super) fn drive_op(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        match request.kind {
            StepKind::HostLoad => self.drive_host_load(request),
            _ => Err(AdapterError::unsupported(unsupported_reason(&request.kind))),
        }
    }

    /// §9.2 host lifecycle `load(target)`: re-load the step's package into the
    /// running instance through [`Engine::update`], migrating committed state
    /// (§20.1) and rebinding the router to the reloaded model so subsequent
    /// watches read the new surfaces. A rejected migration leaves the instance
    /// unchanged and reports the refusal class.
    fn drive_host_load(&mut self, request: &OpRequest) -> Result<Observation, AdapterError> {
        let Some(package) = request.target.get("package").cloned() else {
            return Err(AdapterError::unsupported("`host_load` step carries no `package` to load"));
        };
        let loaded = self.take_loaded()?;
        let old_routing = loaded.routing.clone();
        let (mut engine, old_router, mut clock) = loaded.host.into_parts();

        let plan = AuthPlan::derive(&package);
        let outcome = Self::apply_host_load(&mut engine, &mut clock, &package, &plan);
        match outcome {
            Ok((completion, router, routing)) => {
                self.restore(Loaded { host: SurfaceHost::new(engine, router, clock), routing });
                self.reopen_connections();
                Ok(Observation { outcome: Outcome::Ok, value: None, completion: Some(completion), extra: Default::default() })
            }
            Err(observed) => {
                // The engine is unchanged (update is atomic); rebuild over the
                // prior router so later steps still resolve the active package.
                self.restore(Loaded {
                    host: SurfaceHost::new(engine, old_router, clock),
                    routing: old_routing,
                });
                self.reopen_connections();
                Ok(Observation::outcome(observed))
            }
        }
    }

    /// Run [`Engine::update`] for the reloaded `package`, injecting the same
    /// synthetic views/mutations a fresh load would and rebinding the router.
    /// Tries the richest surface lift first, falling back to fewer synthetic
    /// declarations (exactly as the initial load does) before giving up.
    fn apply_host_load(
        engine: &mut liasse_runtime::Engine<S>,
        clock: &mut SurfaceClock,
        package: &serde_json::Value,
        plan: &AuthPlan,
    ) -> Result<(Completion, liasse_surface::SurfaceRouter, super::router::Routing), Outcome> {
        let lift = SurfaceLift::derive(package);
        let mut attempts = vec![lift.clone()];
        if !lift.views_only().is_empty() {
            attempts.push(lift.views_only());
        }
        if !lift.is_empty() {
            attempts.push(SurfaceLift::default());
        }
        let mut last = Outcome::Error;
        for attempt in attempts {
            let Some(definition) = super::prepared_definition(package, plan, &attempt) else {
                continue;
            };
            let before = engine.head();
            match engine.update(&definition, clock) {
                Ok(_) => {
                    let completion =
                        if engine.head() == before { Completion::Unchanged } else { Completion::Committed };
                    match super::router::build(engine.model(), package, plan, &attempt) {
                        Ok((router, routing)) => return Ok((completion, router, routing)),
                        Err(_) => last = Outcome::Error,
                    }
                }
                Err(error) => last = update_outcome(&error),
            }
        }
        Err(last)
    }
}

/// Map an [`Engine::update`] failure to the harness outcome class (§9.4, §20):
/// a refused migration is a `rejected`, an off-line or statically invalid target
/// an `invalid`, a store fault an `error`.
fn update_outcome(error: &liasse_runtime::UpdateError) -> Outcome {
    use liasse_runtime::UpdateError as U;
    match error {
        U::Rejected(_) => Outcome::Rejected,
        U::Incompatible(_) => Outcome::Rejected,
        U::Engine(_) => Outcome::Invalid,
    }
}

/// The precise reason an op family is not driven yet — what host machinery it
/// needs beyond the surface host the adapter assembles.
fn unsupported_reason(kind: &StepKind) -> String {
    let need = match kind {
        StepKind::ModuleInstall
        | StepKind::ModuleUninstall
        | StepKind::ModuleDisable
        | StepKind::ModuleEnable
        | StepKind::ModuleUpdate
        | StepKind::ModuleRename => "a `ModuleHost` over a `StoreFactory` the adapter does not provision",
        StepKind::BlobPut | StepKind::BlobGet => "a `BlobEngine` with registered stores and connectors",
        StepKind::KeyringAdmin | StepKind::ProviderSet => "a `Keyring` over a `KeyProvider` the adapter does not wire",
        StepKind::Export
        | StepKind::Import
        | StepKind::Reconcile
        | StepKind::RunReconciler => "cross-step artifact byte passing the harness binding layer does not carry",
        StepKind::BuildArtifact
        | StepKind::LoadArtifact
        | StepKind::TamperArtifact
        | StepKind::RepackArtifact
        | StepKind::InspectArtifact
        | StepKind::ExtractArtifact
        | StepKind::TamperExtract => "the `liasse-artifact`/`liasse-host` archive layer the adapter does not link",
        StepKind::Operator => "a host-operator transition entry the surface host does not expose",
        StepKind::Erase
        | StepKind::Reinsert
        | StepKind::ScrubScopeOfCascadedRow
        | StepKind::ApplyCorrection => "the deletion/erasure host verbs the surface host does not expose",
        StepKind::ConnectorSet | StepKind::BudgetSet => "a host connector/budget control the adapter does not wire",
        _ => "engine wiring the current layer does not expose",
    };
    format!("`{}` needs {need}", kind.key())
}
