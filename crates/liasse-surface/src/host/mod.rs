//! The surface host (SPEC.md §10–§12): the owned state that drives external
//! requests over a runtime engine.
//!
//! A [`SurfaceHost`] owns the engine, the exposed [`SurfaceRouter`], the virtual
//! clock, the logical connections (each with its subscriptions and the frontier
//! completion barrier), and the retained operation records. It is plain owned
//! state with no interior mutability — the future executor drives it
//! single-threaded, one request at a time — and every external effect flows
//! through the engine's public admission and view API.

mod barrier;
mod call;
mod components;
mod correction;
mod erasure;
mod history;
mod operator;
mod update;

pub use components::{HostComponentError, KeyringErrorOr, VerifyErrorOr};
pub use correction::{
    ChooseMap, ChooseSide, ConflictCoordinate, CorrectionError, CorrectionOutcome,
};
pub use erasure::EraseOutcome;
pub use update::UpdateOutcome;

use std::collections::BTreeMap;

use liasse_host::sim::{SimConnector, SimKeyProvider};
use liasse_runtime::{Engine, EngineError, Timestamp, ViewResult, ViewRow};
use liasse_store::InstanceStore;

use crate::blobs::BlobHost;
use crate::cose::CoseKeyring;

use crate::authn::AuthContext;
use crate::clock::VirtualClock;
use crate::connection::Connection;
use crate::operation::{OperationKey, OperationLog, OperationStatus};
use crate::outcome::{Denial, DenialReason};
use crate::reader::EngineReader;
use crate::request::{Authenticate, AuthSelection};
use crate::role::Role;
use crate::router::SurfaceRouter;
use crate::window::WindowError;

/// A transport/host fault — never a spec outcome. A denied or rejected request
/// is a successful observation of that outcome, returned in the outcome type;
/// only a broken connection reference or a store fault is a [`SurfaceError`].
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    /// A request named a connection that is not open.
    #[error("no connection named `{0}` is open")]
    NoConnection(String),
    /// A store or engine fault surfaced from admission or a view.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// The result of authenticating a context on a connection: bound, or denied.
#[derive(Debug, Clone)]
pub enum AuthResult {
    /// The context verified and is now bound to the connection.
    Bound,
    /// Authentication was refused (§11).
    Denied(Denial),
}

/// The result of opening or resuming a subscription (§12.1 `view`, §12.2): the
/// initial result at its frontier, or a refusal.
#[derive(Debug, Clone)]
pub enum Subscription {
    /// The subscription opened; carries the complete initial result.
    Init(ViewResult),
    /// A bounded subscription opened; carries its initial windowed rows (§12.2).
    Window(Vec<ViewRow>),
    /// Opening the subscription was refused by authentication or roles (§10/§11).
    Denied(Denial),
    /// A bounded subscription could not open: its anchor identified no current
    /// occurrence (§12.2). Not an authorization failure — no admission is
    /// involved.
    Failed(WindowError),
}

/// The owned surface state over one engine.
pub struct SurfaceHost<S> {
    engine: Engine<S>,
    router: SurfaceRouter,
    clock: VirtualClock,
    connections: BTreeMap<String, Connection>,
    operations: OperationLog,
    /// The §17 keyrings composed into this host, by keyring name. A driver
    /// provisions one per case `$keyring` declaration over a host key provider;
    /// a `cose.sign`/`cose.verify` call resolves the ring by name here
    /// (§17.7/§17.8). Empty until a driver registers one.
    keyrings: BTreeMap<String, CoseKeyring<SimKeyProvider>>,
    /// The §18 blob hosts composed into this host, by blob-field name. A driver
    /// provisions one per accepted blob field over registered stores and
    /// connectors; a blob-parameter mutation resolves it by name at admission.
    /// Empty until a driver registers one.
    blobs: BTreeMap<String, BlobHost<SimConnector>>,
}

impl<S: InstanceStore> SurfaceHost<S> {
    /// Build a host over `engine`, exposing `router`, driven by `clock`.
    #[must_use]
    pub fn new(engine: Engine<S>, router: SurfaceRouter, clock: VirtualClock) -> Self {
        Self {
            engine,
            router,
            clock,
            connections: BTreeMap::new(),
            operations: OperationLog::new(),
            keyrings: BTreeMap::new(),
            blobs: BTreeMap::new(),
        }
    }

    /// The underlying engine, for reading committed state and views directly.
    #[must_use]
    pub fn engine(&self) -> &Engine<S> {
        &self.engine
    }

    /// Consume the host and hand back the engine, router, and clock it seals
    /// (§22 restart/durability). A restart is a *volatile-state* reset: the
    /// engine (and, with it, the durable store, its committed log, and the
    /// virtual clock) is retained, while the host's connections, live
    /// subscriptions, and retained operation records — none of which are durable
    /// — are dropped when the host is.
    ///
    /// A driver restarts by [`into_parts`](Self::into_parts)-ing the running
    /// host and immediately rebuilding a fresh one over the returned engine with
    /// [`SurfaceHost::new`]. Because the same engine is reused, no `$data` seed is
    /// re-applied and no generated value (a recorded `now()`, a `uuid()` key) is
    /// re-rolled: committed state is exactly what it was, which is precisely what
    /// the §22 durability cases assert survives a restart. The rebuilt host opens
    /// its connections afresh, each frontier starting at the retained head.
    #[must_use]
    pub fn into_parts(self) -> (Engine<S>, SurfaceRouter, VirtualClock) {
        (self.engine, self.router, self.clock)
    }

    /// The virtual clock, for advancing time and reading the instant (§11.7).
    pub fn clock_mut(&mut self) -> &mut VirtualClock {
        &mut self.clock
    }

    /// Advance the virtual clock to `now` and reflect the resulting temporal
    /// observations on every live view (§14.1, §22.6).
    ///
    /// Moving time forward is not a commit, yet a frontier "covers committed
    /// application changes *and* temporal bucket observations" (§12.2): when a row
    /// leaves its half-open active interval the runtime "MUST reflect the resulting
    /// current logical view and emit a new live frontier" (§22.6). So this moves
    /// both the session-expiry clock (§11.7) and the engine's bucket clock (§14),
    /// then sweeps every open subscription — re-evaluating its authorized view at
    /// the advanced instant and closing any whose authority a session expiry has
    /// removed. No application state changes and no commit is produced.
    ///
    /// # Errors
    /// [`SurfaceError::Engine`] from a store or view fault while sweeping.
    pub fn advance_time(&mut self, now: Timestamp) -> Result<(), SurfaceError> {
        self.clock.set(now.count());
        self.engine.set_time(now);
        let barrier = barrier::Barrier::new(&self.engine, &self.router, now);
        for connection in self.connections.values_mut() {
            let frontier = connection.frontier();
            barrier.sweep(connection, frontier)?;
        }
        Ok(())
    }

    /// Drag every open subscription through the engine's current head at the
    /// current instant, advancing each connection's frontier and sweeping its
    /// still-authorized subscriptions. Used after a host-driven state change that
    /// did not flow through a client `call` — an applied §19 `import` movement or a
    /// §23.5 operator transition — so live clients observe the new committed state
    /// coherently (§12.6, §22.6).
    ///
    /// # Errors
    /// [`SurfaceError::Engine`] from a store or view fault while sweeping.
    pub(super) fn sweep_all(&mut self) -> Result<(), SurfaceError> {
        let now = self.clock.instant();
        let head = self.engine.head();
        let barrier = barrier::Barrier::new(&self.engine, &self.router, now);
        for connection in self.connections.values_mut() {
            connection.advance_frontier(head);
            let frontier = connection.frontier();
            barrier.sweep(connection, frontier)?;
        }
        Ok(())
    }

    /// Open a logical connection named `id`, its frontier starting at the current
    /// head (§12).
    pub fn connect(&mut self, id: impl Into<String>) {
        let frontier = self.engine.head();
        self.connections.insert(id.into(), Connection::new(frontier));
    }

    /// Close connection `id`, dropping its subscriptions.
    pub fn disconnect(&mut self, id: &str) {
        self.connections.remove(id);
    }

    /// The current frontier of connection `id`, if open.
    #[must_use]
    pub fn frontier(&self, id: &str) -> Option<liasse_runtime::CommitSeq> {
        self.connections.get(id).map(Connection::frontier)
    }

    /// The current cached result of subscription `watch` on connection `id`
    /// (§12.2), or `None` if the connection/subscription is absent or closed.
    #[must_use]
    pub fn read_view(&self, id: &str, watch: &str) -> Option<&ViewResult> {
        self.connections.get(id)?.watch(watch)?.current()
    }

    /// The client-visible rows of a bounded subscription `watch` on connection
    /// `id` (§12.2), or `None` if the subscription is absent, closed, or not
    /// windowed.
    #[must_use]
    pub fn read_window(&self, id: &str, watch: &str) -> Option<&[ViewRow]> {
        self.connections.get(id)?.watch(watch)?.window_rows()
    }

    /// The close reason of subscription `watch`, if it has been closed (§12.2).
    #[must_use]
    pub fn close_reason(&self, id: &str, watch: &str) -> Option<&str> {
        self.connections.get(id)?.watch(watch)?.close_reason()
    }

    /// The retained status of the operation scoped by `key` (§12.3). The
    /// high-entropy identifier inside `key` is the capability to read it.
    #[must_use]
    pub fn operation_status(&self, key: &OperationKey) -> OperationStatus {
        self.operations.status(key)
    }

    /// Authenticate a context on connection `id` (§11.4, §11.8): verify the
    /// selection against committed state, and — on success — retain it for reuse
    /// on later requests and subscription frontiers.
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open.
    pub fn authenticate(
        &mut self,
        id: &str,
        request: &Authenticate,
    ) -> Result<AuthResult, SurfaceError> {
        if !self.connections.contains_key(id) {
            return Err(SurfaceError::NoConnection(id.to_owned()));
        }
        let now = self.clock.instant();
        let reader = EngineReader::new(&self.engine, now);
        let outcome = match self.router.role(request.role()) {
            Some(role) => self.verify_selection(role, request.selection(), &reader),
            None => Err(Denial::new(DenialReason::Unresolved, "the address names no exposed role")),
        };
        match outcome {
            Ok(_) => {
                if let Some(connection) = self.connections.get_mut(id) {
                    connection.set_context(request.context().to_owned(), request.selection().clone());
                }
                Ok(AuthResult::Bound)
            }
            Err(denial) => Ok(AuthResult::Denied(denial)),
        }
    }

    /// Verify one selection against a role (§11.4): the role must accept the
    /// named authenticator, and the authenticator must resolve an actor/session.
    /// Membership is *not* checked here — a resolved context may target several
    /// roles (§11.8).
    fn verify_selection(
        &self,
        role: &Role,
        selection: &AuthSelection,
        reader: &EngineReader<'_, S>,
    ) -> Result<AuthContext, Denial> {
        if !role.accepts(selection.auth()) {
            return Err(Denial::new(
                DenialReason::AuthenticatorNotAccepted,
                "the targeted role does not accept this authenticator",
            ));
        }
        let Some(authenticator) = self.router.authenticator(selection.auth()) else {
            return Err(Denial::new(
                DenialReason::AuthenticatorNotAccepted,
                "the named authenticator is not declared",
            ));
        };
        authenticator.resolve(selection.credential(), reader)
    }

    /// The surfaces granted to connection `id`'s context (§12.1 `manifest`):
    /// every public surface, plus every role surface whose role accepts the
    /// context's authenticator and holds its actor. Returns addresses sorted by
    /// their canonical dotted form.
    ///
    /// # Errors
    /// [`SurfaceError::NoConnection`] if `id` is not open; a store fault from
    /// re-evaluating membership.
    pub fn manifest(&self, id: &str, context: Option<&str>) -> Result<Vec<String>, SurfaceError> {
        let connection = self.connections.get(id).ok_or_else(|| SurfaceError::NoConnection(id.to_owned()))?;
        let mut surfaces: Vec<String> = self
            .router
            .public_surfaces()
            .map(|name| format!("public.{name}"))
            .collect();
        if let Some(selection) = connection.select_context(context) {
            self.append_role_surfaces(selection, &mut surfaces)?;
        }
        surfaces.sort();
        Ok(surfaces)
    }

    fn append_role_surfaces(
        &self,
        selection: &AuthSelection,
        surfaces: &mut Vec<String>,
    ) -> Result<(), SurfaceError> {
        let now = self.clock.instant();
        let reader = EngineReader::new(&self.engine, now);
        let Some(authenticator) = self.router.authenticator(selection.auth()) else {
            return Ok(());
        };
        let Ok(context) = authenticator.resolve(selection.credential(), &reader) else {
            return Ok(());
        };
        for role_name in self.router.role_names() {
            let Some(role) = self.router.role(role_name) else { continue };
            if !role.accepts(selection.auth()) {
                continue;
            }
            // §12.1: the manifest lists a scoped role's surfaces when the actor
            // holds it under ANY scope row (an empty scope asks the enumeration-safe
            // "member anywhere" question); admission still re-checks the exact scope
            // a request addresses (§10.3).
            if role.holds(context.actor().key(), &[], &reader)? {
                for surface in self.router.role_surfaces(role_name) {
                    surfaces.push(format!("{role_name}.{surface}"));
                }
            }
        }
        Ok(())
    }
}
