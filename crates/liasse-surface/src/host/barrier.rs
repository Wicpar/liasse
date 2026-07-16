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

use crate::authn::AuthContext;
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

    /// Re-evaluate only *authority* for every open subscription on `connection` at
    /// the barrier instant, closing any whose authority the current state has
    /// removed, without advancing its row frontier (§12.2). This is the sweep a
    /// commit applies to *peer* connections: the commit is an outgoing frontier
    /// that can revoke a subscription's authority (so it must close), but §12.3's
    /// completion barrier advances a subscription's rows only on its own
    /// connection, so a peer's rows are left at their prior frontier.
    pub(crate) fn close_lost_authority(&self, connection: &mut Connection) -> Result<(), EngineError> {
        for id in connection.watch_ids() {
            let Some(watch) = connection.watch(&id) else { continue };
            if watch.close_reason().is_some() {
                continue;
            }
            let authz = watch.authz().clone();
            if self.authorized(&authz, connection)?.is_none()
                && let Some(watch) = connection.watch_mut(&id)
            {
                watch.close("authority removed at frontier");
            }
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
        let args = watch.args().clone();

        // §12.2: re-evaluate authorization and, for a role subscription, recover
        // the actor identity so the recomputed `$view` sees the same `$actor` as
        // the initial read. `None` means authority was removed.
        let context = match self.authorized(&authz, connection)? {
            Some(context) => context,
            None => {
                if let Some(watch) = connection.watch_mut(id) {
                    watch.close("authority removed at frontier");
                }
                return Ok(());
            }
        };
        // §10.1: re-supply the subscription's `$params` arguments and resolved
        // identity, so a parameterized or actor-scoped view recomputes correctly
        // after a commit or a time advance rather than faulting to an empty result.
        let query = super::call::view_query(args, context.as_ref());
        let Some(result) = self.engine.view_with(&view, frontier, &query)? else {
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
    /// at an outgoing frontier. `Ok(None)` means authority was removed; `Ok(Some(_))`
    /// means still authorized, carrying the re-resolved [`AuthContext`] for a role
    /// subscription (or `None` for a public one, which is always authorized). A
    /// role subscription re-runs its authenticator (catching expiry and revocation)
    /// and re-checks membership (catching a removed grant).
    pub(crate) fn authorized(
        &self,
        authz: &WatchAuthz,
        connection: &Connection,
    ) -> Result<Option<Option<AuthContext>>, EngineError> {
        let Some(role_name) = authz.role_name() else {
            return Ok(Some(None));
        };
        // §11.4/§12.2: re-authorize from the subscription's own retained `auth`
        // selection when it opened with one, otherwise from the connection's stored
        // context. Either way the credential is re-verified against committed state
        // below, so revocation and expiry still close the subscription.
        let selection = match authz.selection() {
            Some(selection) => selection,
            None => match authz.context().and_then(|name| connection.context(name)) {
                Some(selection) => selection,
                None => return Ok(None),
            },
        };
        let Some(role) = self.router.role(role_name) else {
            return Ok(None);
        };
        if !role.accepts(selection.auth()) {
            return Ok(None);
        }
        let Some(authenticator) = self.router.authenticator(selection.auth()) else {
            return Ok(None);
        };
        let reader = EngineReader::new(self.engine, self.now);
        let context = match authenticator.resolve(selection.credential(), &reader) {
            Ok(context) => context,
            Err(_) => return Ok(None),
        };
        if role.holds(context.actor().key(), &reader)? {
            Ok(Some(Some(context)))
        } else {
            Ok(None)
        }
    }
}
