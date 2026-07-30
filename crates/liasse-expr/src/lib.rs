//! Expressions: the typed expression layer of SPEC.md §6–§7.
//!
//! This crate is the seam between the syntax the parser produces
//! ([`liasse_syntax`]) and the state a runtime holds. It does two things:
//!
//! 1. **Static typing** ([`check_expression`]): an AST plus a [`Scope`] becomes
//!    a [`TypedExpr`] — proof the expression is well-typed — or a bundle of
//!    [`Diagnostics`](liasse_diag::Diagnostics). Inference follows §6/§8.3.
//! 2. **Pure evaluation** ([`TypedExpr::evaluate`]): a `TypedExpr` plus an
//!    [`Environment`] becomes a [`Cell`] ([`liasse_value::Value`]s and row
//!    streams). Evaluation reads a `TypedExpr`, never raw AST, so no operator or
//!    function is ever re-typed at run time.
//!
//! # Purity and generativity
//!
//! Evaluation is a pure function of the environment (§8.12): the environment
//! owns `now()` (fixed per operation, A.5) and `uuid()` (per SPEC-ISSUES item 4
//! the environment, not this crate, decides per-call-site identity), so the same
//! environment always yields the same result.
//!
//! # Recursion
//!
//! Typing and evaluation recurse structurally on the AST; liasse-syntax caps
//! expression nesting (`scan::MAX_NESTING_DEPTH`) before this crate sees a tree,
//! and that cap is calibrated to clear the checker's and evaluator's stack budget
//! (which overflows well below `pest`'s). The one non-structural recursion — the
//! checker's projection-output dependency ordering — is separately bounded by the
//! projection's output count (see [`typed`]).
//!
//! # Documented spec-gap choices (SPEC-ISSUES)
//!
//! - **Item 1 (decimal spelling).** Division and `avg` results are normalized
//!   (trailing zeros trimmed) as the least-surprising canonical form.
//! - **Item 3 (arithmetic edges).** Division by zero is an
//!   [`EvalError::DivisionByZero`]; `%` remainder takes the sign of the dividend
//!   (truncated toward zero, consistent with A.6 integer division); an optional
//!   operand in arithmetic is a static type error.
//! - **Item 4 (`uuid()` identity).** Delegated to [`Environment::uuid`] via a
//!   [`CallSite`].

mod affine;
mod check;
mod env;
mod error;
mod eval;
#[cfg(feature = "eval-wire")]
pub mod hoist;
mod host;
mod lifecycle;
#[cfg(feature = "eval-wire")]
pub mod lower;
mod order;
mod scope;
mod semantics;
mod ty;
mod typed;
#[cfg(feature = "eval-wire")]
pub mod wire;

pub use affine::{Affinity, MoveTracker, assignment_rejects_move_only, place_key, read_places};
pub use check::{
    audit_host_position, check_composite_delete_operand, check_expression, check_statement,
    core_builtin_param, is_core_string_call,
};
pub use env::{
    BlobPlacement, CallSite, Cell, Environment, KeyringSelector, Row, RowId, RowIdPart,
    TemporalQuery,
};
pub use error::EvalError;
pub use host::{DbReadPosition, HostEffect, HostOp, HostOrigin, HostPosition};
pub use lifecycle::{
    arg as lifecycle_arg, LifecycleOp, MigrateAxis, ModuleOperator, LIFECYCLE_NAMESPACE, MOVE_OPERATOR,
};
pub use order::{SortOrder, ViewOrders};
pub use scope::{InterfaceMut, Scope};
pub use semantics::DivisionRounding;
pub use ty::{ExprType, RowType};
pub use typed::{InterfaceCall, TypedExpr};

/// The identity member of a MAP row (§5.4). A map is the degenerate keyed
/// collection, so its entries are ordinary rows whose two members are named by
/// the markers that declared them. The name lives here, beside the row model
/// that reads it, so the model builder, the checker, and the evaluator all agree
/// on one spelling.
pub const MAP_KEY: &str = "$key";

/// The value member of a MAP row (§5.4). See [`MAP_KEY`].
pub const MAP_VALUE: &str = "$value";
