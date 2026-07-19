//! Pred: the ONE row-program evaluator (§7.3 of `liasse-pg/DESIGN-pure-pg.md`).
//!
//! [`RowPrograms`] is the sole implementor of [`liasse_store::ViewProgram`]: it
//! carries a lowered view read's admit filter, projection, and sort keys as
//! **residual** `liasse-expr` [`TypedExpr`](liasse_expr::TypedExpr)s (candidate-free
//! subexpressions already hoisted into a shared env), and evaluates each face by
//! rebuilding the candidate [`Row`](liasse_expr::Row) from a [`CandidateDescriptor`]
//! and running the SAME linked interpreter the runtime and the pushdown extension
//! run (`TypedExpr::evaluate_bound`). Parity across executors is therefore by
//! construction — same faces, same interpreter — and the only reimplemented seam,
//! the descriptor-driven candidate build, is gated by the layer-1 lowering-parity
//! corpus (§9).
//!
//! The interpreter lives here ONCE: the in-memory store calls the faces directly
//! (the oracle), and the pushdown extension deserializes the version-locked
//! [`wire`](liasse_expr::wire) faces and runs the identical evaluator inside
//! PostgreSQL. [`EVAL_ABI`] pins the wire's version lock.

mod descriptor;
mod penv;
mod program;

pub use descriptor::{CandidateDescriptor, Member};
pub use program::{ProjectShape, RowPrograms, RowProgramsParts, EVAL_ABI};
