//! Evaluation of the keyring public version selectors `.$current`, `.$accepted`,
//! `.$public`, `.$versions` (§17.2).
//!
//! The evaluator reduces the base to the keyring's version rows and hands them,
//! with the selector, to the environment's keyring index
//! ([`Environment::keyring`](crate::Environment::keyring)). Version-lifecycle
//! resolution — which versions are active, accepted, or retained — lives in the
//! environment, so evaluation stays a pure function of it (§8.12). The one shape
//! rule the evaluator owns is `.$current`'s single active version (§17.3): the
//! selector types as a row, so exactly one version must come back.

use crate::env::{Cell, KeyringSelector, Row};
use crate::error::EvalError;
use crate::eval::Evaluator;
use crate::ty::ExprType;
use crate::typed::TypedExpr;

impl Evaluator<'_> {
    pub(crate) fn eval_keyring(
        &mut self,
        expr: &TypedExpr,
        base: &TypedExpr,
        selector: KeyringSelector,
    ) -> Result<Cell, EvalError> {
        let rows: Vec<Row> = self.eval_view(base)?.into_iter().map(|scope| scope.row).collect();
        let versions = self.env.keyring(&rows, selector)?;
        if matches!(expr.ty(), ExprType::Row(_)) {
            // §17.3: at most one version is active, so `.$current` is one row.
            return match versions.len() {
                1 => versions
                    .into_iter()
                    .next()
                    .map(|row| Cell::Row(Box::new(row)))
                    .ok_or(EvalError::ShapeMismatch { expected: "one keyring version" }),
                found => Err(EvalError::Cardinality {
                    context: "the active keyring version",
                    found,
                }),
            };
        }
        Ok(Cell::Collection(versions))
    }
}
