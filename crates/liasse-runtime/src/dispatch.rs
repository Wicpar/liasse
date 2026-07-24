//! The cross-instance dispatch handle a multi-engine atomic transition lends into
//! the running interpreter (§13.10).
//!
//! A mutation program MAY reach across module boundaries with a
//! `#handle.mutation(args)` call. When the transition is a multi-engine one, the
//! coordinator that owns the reached engines lends a [`Dispatch`] into the parent
//! [`Interp`](crate::interp): the call resolves to the exposed mutation on the
//! addressed engine, stages the child transition into the same coordinator, and
//! yields the child mutation's declared `$return` to the caller. Every touched
//! engine stages together and commits together — all-or-none — so a rejected child
//! dispatch is a [`Rejection`] that unwinds the whole parent transition, and the
//! pure value evaluator never dispatches (a cross-engine effect is not a value).

use liasse_expr::Cell;
use liasse_model::LifecycleOp;
use liasse_value::Value;

use crate::error::Rejection;

/// The handles a transition lends into one engine's staging (§13.10): the
/// cross-instance [`Dispatch`] (a `#handle.mut(...)` reaches another engine) and the
/// host-privileged [`Lifecycle`] (a `module.<op>(...)` carries an instance through
/// its lifecycle). Both default to `None` — an ordinary single-engine admission
/// lends neither, so a cross-engine or lifecycle call is refused LOUDLY.
#[derive(Clone, Copy, Default)]
pub(crate) struct Handles<'a> {
    /// The cross-instance dispatch handle, when the coordinator owns reached engines.
    pub(crate) dispatch: Option<&'a dyn Dispatch>,
    /// The host/root-scope lifecycle handle, when this is the primary of a
    /// lifecycle transition.
    pub(crate) lifecycle: Option<&'a dyn Lifecycle>,
}

/// A cross-instance dispatch target the interpreter reaches while running a parent
/// transition (§13.10). The parent's interpreter holds one so a
/// `#handle.mutation(args)` call resolves to the exposed mutation on the addressed
/// child or peer engine, stages that child transition into the shared coordinator,
/// and returns its `$return`. The staged child change joins the same atomic commit:
/// parent and every reached engine advance together, or the whole transition is
/// rejected and every engine keeps its prior committed state.
pub(crate) trait Dispatch {
    /// Resolve `#handle.mutation(args)` to the exposed mutation on the lent engine,
    /// stage it into the coordinator, and return its `$return` value (§13.8/§13.10).
    /// `args` are the already-evaluated `(parameter, value)` pairs the caller
    /// supplied. A rejected child transition is `Err(Rejection)` — it unwinds the
    /// whole parent transition, so nothing commits.
    fn dispatch(
        &self,
        handle: &str,
        mutation: &str,
        args: Vec<(String, Value)>,
    ) -> Result<Cell, Rejection>;
}

/// The host-privileged module-lifecycle handle a transition lends into the running
/// interpreter (§13.10). Present ONLY when the primary engine is the host/root
/// scope: a `module.install`/`module.update`/`module.remove(args)` call resolves to
/// this handle, which decodes the package definition from the blob argument
/// (failing LOUDLY on a malformed/incompatible package), records the lifecycle
/// intent, and returns the decoded package identity to the caller. The mounted,
/// migrated, or removed instance is one of the engines the coordinator commits
/// together, so the lifecycle change commits atomically with the parent's own
/// change — all touched engines or none.
///
/// A non-host/non-root caller (a child module engine) is lent NO lifecycle handle,
/// so its `module.*` call is refused LOUDLY by the interpreter — the privilege is
/// enforced by lending, exactly as the cross-engine [`Dispatch`] is.
pub(crate) trait Lifecycle {
    /// Perform lifecycle operation `op` with the evaluated `(member, value)` pairs
    /// of the call's argument object (`{ blob: @pkg, space: "…", name: "…" }`).
    /// Decodes the blob (install/update), staging the mount/migration/removal into
    /// the transition, and returns the decoded package identity as a value the
    /// caller may bind or return. A refusal (malformed blob, unknown instance, …)
    /// is an `Err(Rejection)` that unwinds the WHOLE transition — nothing commits.
    fn perform(&self, op: LifecycleOp, args: Vec<(String, Value)>) -> Result<Cell, Rejection>;
}
