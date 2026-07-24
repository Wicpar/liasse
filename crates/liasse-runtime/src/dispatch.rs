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
use liasse_value::Value;

use crate::error::Rejection;

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
