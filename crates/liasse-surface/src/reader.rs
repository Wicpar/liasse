//! Read-only committed-state access for authentication and role admission.
//!
//! Authenticators and roles resolve application rows (sessions, accounts, member
//! sets) at the request's admission position (§11.3, §10.3 "re-evaluated at
//! admission"). They need only to *read* committed state and the virtual clock,
//! never to mutate — so they take a [`StateReader`] rather than the whole engine.
//! This keeps the store generic off their signatures and makes them trivially
//! testable against a fixed set of rows.

use liasse_runtime::{Engine, EngineError, Timestamp, ViewResult};
use liasse_store::InstanceStore;

/// Committed-state reads plus the current virtual instant — everything the
/// authentication and role layers need to resolve rows and judge expiry.
pub trait StateReader {
    /// Evaluate the named application view against current committed state
    /// (the head frontier). `None` means no view of that name is declared.
    ///
    /// # Errors
    /// Propagates a store/engine fault; a missing view is `Ok(None)`, not an
    /// error.
    fn view(&self, name: &str) -> Result<Option<ViewResult>, EngineError>;

    /// The current virtual instant (§11.7 expiry, §22 `now()`).
    fn now(&self) -> Timestamp;
}

/// A [`StateReader`] over a borrowed engine at a fixed instant. Holding the
/// instant by value (rather than re-sampling) keeps every resolution within one
/// request evaluated against the one admission-time clock reading (A.5).
pub struct EngineReader<'a, S> {
    engine: &'a Engine<S>,
    now: Timestamp,
}

impl<'a, S: InstanceStore> EngineReader<'a, S> {
    /// Read `engine`'s committed state as of the virtual instant `now`.
    #[must_use]
    pub fn new(engine: &'a Engine<S>, now: Timestamp) -> Self {
        Self { engine, now }
    }
}

impl<S: InstanceStore> StateReader for EngineReader<'_, S> {
    fn view(&self, name: &str) -> Result<Option<ViewResult>, EngineError> {
        self.engine.view_at_head(name)
    }

    fn now(&self) -> Timestamp {
        self.now
    }
}
