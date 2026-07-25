//! Routing the §13.16 module-value operators through the §13.10 lifecycle handle.
//!
//! `pack`, `update_module` and `rollback_module` are host-privileged: they carry a
//! module instance through its lifecycle, so the interpreter recognises them
//! STRUCTURALLY (before typing, exactly as it recognises `#handle.mut` and
//! `module.<op>`) and routes them to the handle a host/root-scope transition lends.
//! With no handle lent they are refused LOUDLY — the privilege is enforced by
//! lending, not by a flag the caller could carry.
//!
//! This module only marshals: it turns the call's argument expressions into the
//! named `(member, value)` pairs the handle reads. What each operator *means*, and
//! which coordinates an instance can honour, belongs to
//! [`crate::modules::value`] and the module host.

use liasse_expr::Cell;
use liasse_model::{lifecycle_arg as arg, ModuleOperator};
use liasse_syntax::{Arg, BlockMemberKind, Expr, ExprKind};
use liasse_value::Value;

use crate::error::{Rejection, RejectionReason};
use crate::interp::Interp;
use liasse_diag::SourceId;

/// The §13.16 operator a call addresses, when `callee` is the bare name of one.
///
/// A package cannot declare a mutation or collection that reaches call position as
/// a bare `pack(…)`/`update_module(…)`/`rollback_module(…)` and also type-check
/// against §13.16's operand rules, so the classification is unambiguous — and it is
/// the SAME classification the expression checker makes, from the same source.
pub(crate) fn module_operator(callee: &Expr) -> Option<ModuleOperator> {
    let ExprKind::Name(name) = &callee.kind else { return None };
    ModuleOperator::classify(&name.text)
}

impl Interp<'_> {
    /// Run a §13.16 module-value operator when `expr` is one, yielding its result
    /// cell — the packed blob, the decoded package identity, or the selected point.
    /// `None` when `expr` is not one, so the caller falls through.
    pub(crate) fn module_operator_call(&self, expr: &Expr, source: SourceId) -> Option<Result<Cell, Rejection>> {
        let ExprKind::Call { callee, args } = &expr.kind else { return None };
        let operator = module_operator(callee)?;
        Some(self.run_module_operator(operator, args, source))
    }

    /// Marshal an operator's arguments and route them through the lent
    /// host-privileged handle (§13.10). With NO handle lent, refuse LOUDLY: only a
    /// host/root-scope transition may carry a module instance through its
    /// lifecycle, so a child module engine's call — or any single-engine admission
    /// — is refused rather than served.
    fn run_module_operator(
        &self,
        operator: ModuleOperator,
        args: &[Arg],
        source: SourceId,
    ) -> Result<Cell, Rejection> {
        let Some(lifecycle) = self.lifecycle else {
            return Err(Rejection::new(
                RejectionReason::Malformed,
                format!(
                    "`{}` carries a module instance through its lifecycle and is host-privileged \
                     (§13.16/§13.10): only the host/root-scope transition may perform it, and \
                     this caller is not lent that authority",
                    operator.name()
                ),
            ));
        };
        let current = self.current()?;
        let mut values = Vec::new();
        // The leading operands are positional and fixed by §13.16; the trailing axis
        // object (if any) contributes its named members.
        let leading: &[&str] = match operator {
            ModuleOperator::Pack => &[arg::MODULE],
            ModuleOperator::UpdateModule => &[arg::MODULE, arg::ONTO],
            ModuleOperator::Rollback => &[arg::MODULE, arg::POINT],
            // §13.16: `reinstall_module(m)` names what a `<-` does to `m` at ITS
            // destination. Standing alone it addresses no destination, so there is
            // no admission to re-run — refused by name rather than evaluated into
            // some value that looks like it moved an instance.
            ModuleOperator::Reinstall => {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!(
                        "`{}` is the source of a move into a module collection — write \
                         `.<collection>[<name>] <- {}(m)` (§13.16). On its own it names no \
                         destination, so there is no boundary to re-admit against.",
                        operator.name(),
                        operator.name(),
                    ),
                ));
            }
        };
        let mut rest = args;
        for name in leading {
            let Some((entry, tail)) = rest.split_first() else {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!("`{}` is missing its `{name}` operand (§13.16)", operator.name()),
                ));
            };
            let Arg::Positional(value) = entry else {
                return Err(Rejection::new(
                    RejectionReason::Malformed,
                    format!("`{}`'s `{name}` operand is positional (§13.16)", operator.name()),
                ));
            };
            values.push(((*name).to_owned(), self.operand_value(value, source, &current)?));
            rest = tail;
        }
        for entry in rest {
            match entry {
                Arg::Named { name, value } => {
                    values.push((name.text.clone(), self.scalar_value(value, source, &current)?));
                }
                Arg::Positional(Expr { kind: ExprKind::Object(members), .. }) => {
                    for member in members {
                        let BlockMemberKind::Named { name, value: Some(value) } = &member.kind else {
                            return Err(axis_shape(operator));
                        };
                        values.push((name.text.clone(), self.scalar_value(value, source, &current)?));
                    }
                }
                Arg::Positional(_) => return Err(axis_shape(operator)),
            }
        }
        lifecycle.perform(operator.op(), None, values)
    }

    /// Evaluate one operand. A nested §13.16 operator (`update_module(m, unpack(b))`
    /// is the §13.16 delegation example) is not a pure expression, so an operand
    /// that is itself a lifecycle operator routes through the handle first.
    pub(crate) fn operand_value(&self, expr: &Expr, source: SourceId, current: &Cell) -> Result<Value, Rejection> {
        if let Some(result) = self.module_operator_call(expr, source) {
            return match result? {
                Cell::Scalar(value) => Ok(value),
                _ => Err(Rejection::new(
                    RejectionReason::TypeError,
                    "a module operator yields a scalar value",
                )),
            };
        }
        self.scalar_value(expr, source, current)
    }
}

/// The loud refusal for a trailing argument that is not the `{ … }` axis object
/// §13.16 defines — an axis the runtime cannot name is one it would silently drop.
fn axis_shape(operator: ModuleOperator) -> Rejection {
    Rejection::new(
        RejectionReason::Malformed,
        format!("`{}`'s trailing argument is an axis object `{{ … }}` (§13.16)", operator.name()),
    )
}
