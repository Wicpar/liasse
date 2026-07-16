//! The completion barrier: advancing a connection's subscriptions through a
//! commit (SPEC.md §12.3, §12.6).
//!
//! Before a call returns `committed`, the runtime advances every still-authorized
//! active subscription on the same logical connection through the commit (§12.3
//! final paragraph). This type performs that sweep: for each open subscription it
//! re-evaluates authorization at the new frontier (§12.2) and either recomputes
//! the authorized view and emits a coherent patch, or emits `close` when the
//! state has removed that subscription's authority.

use liasse_runtime::{CommitSeq, EngineError, Engine, Timestamp};
use liasse_store::InstanceStore;

use crate::connection::Connection;
use crate::reader::EngineReader;
use crate::router::SurfaceRouter;
use crate::watch::WatchAuthz;

/// Borrows the engine and router at a fixed instant to sweep one connection.
/// Constructed from disjoint [`SurfaceHost`] fields so it can advance
/// subscriptions while the host mutably owns the connection map.
///
/// [`SurfaceHost`]: crate::SurfaceHost
pub(crate) struct Barrier<'a, S> {
    engine: &'a Engine<S>,
    router: &'a SurfaceRouter,
    now: Timestamp,
}

impl<'a, S: InstanceStore> Barrier<'a, S> {
    pub(crate) fn new(engine: &'a Engine<S>, router: &'a SurfaceRouter, now: Timestamp) -> Self {
        Self { engine, router, now }
    }

    /// Advance every open subscription on `connection` through `frontier` (§12.3).
    /// A subscription that has lost authority or whose view is gone is closed;
    /// the rest are recomputed at `frontier` and patched coherently.
    pub(crate) fn sweep(
        &self,
        connection: &mut Connection,
        frontier: CommitSeq,
    ) -> Result<(), EngineError> {
        for id in connection.watch_ids() {
            self.advance_one(connection, &id, frontier)?;
        }
        Ok(())
    }

    fn advance_one(
        &self,
        connection: &mut Connection,
        id: &str,
        frontier: CommitSeq,
    ) -> Result<(), EngineError> {
        let Some(watch) = connection.watch(id) else { return Ok(()) };
        if watch.close_reason().is_some() {
            return Ok(());
        }
        let view = watch.view().to_owned();
        let authz = watch.authz().clone();

        if !self.authorized(&authz, connection)? {
            if let Some(watch) = connection.watch_mut(id) {
                watch.close("authority removed at frontier");
            }
            return Ok(());
        }
        let Some(result) = self.engine.view(&view, frontier)? else {
            if let Some(watch) = connection.watch_mut(id) {
                watch.close("surface removed at frontier");
            }
            return Ok(());
        };
        if let Some(watch) = connection.watch_mut(id) {
            watch.advance(result, frontier);
        }
        Ok(())
    }

    /// §12.2: re-evaluate authentication, session validity, and role membership
    /// at an outgoing frontier. A public subscription is always authorized; a
    /// role subscription re-runs its authenticator (catching expiry and
    /// revocation) and re-checks membership (catching a removed grant).
    pub(crate) fn authorized(
        &self,
        authz: &WatchAuthz,
        connection: &Connection,
    ) -> Result<bool, EngineError> {
        let (Some(role_name), Some(context_name)) = (authz.role_name(), authz.context()) else {
            return Ok(true);
        };
        let Some(selection) = connection.context(context_name) else {
            return Ok(false);
        };
        let Some(role) = self.router.role(role_name) else {
            return Ok(false);
        };
        if !role.accepts(selection.auth()) {
            return Ok(false);
        }
        let Some(authenticator) = self.router.authenticator(selection.auth()) else {
            return Ok(false);
        };
        let reader = EngineReader::new(self.engine, self.now);
        let context = match authenticator.resolve(selection.credential(), &reader) {
            Ok(context) => context,
            Err(_) => return Ok(false),
        };
        role.holds(context.actor().key(), &reader)
    }
}
