//! Typing of the temporal selectors `.$at(t)`, `.$between(a, b)`, `.$all`
//! (§14.1, §14.2).
//!
//! Each selects a temporal region of a bucketed base view and yields a view of
//! the same row shape (the selector changes *which* rows are visible, never the
//! fields). The base must be a view; a `.$at`/`.$between` instant must be a
//! `timestamp`. Whether the base is genuinely bucketed is a model/runtime
//! concern (the environment supplies the temporal index), so this layer types
//! the selector over any view and leaves bucket-declaration checks upstream.

use liasse_syntax::{Arg, Expr};
use liasse_value::Type;

use crate::check::Checker;
use crate::ty::{ExprType, RowType};
use crate::typed::{TypedExpr, TypedKind, TypedTemporal};

impl Checker<'_> {
    /// `.base.$all` (§14.2): every extant row of the bucketed base view.
    pub(crate) fn check_temporal_all(
        &mut self,
        expr: &Expr,
        base: &Expr,
        selector: &str,
    ) -> Option<TypedExpr> {
        if selector != "all" {
            return self.error(
                expr,
                format!("`.${selector}` is not a temporal selector (expected `.$all`, §14.2)"),
            );
        }
        let (base, row) = self.temporal_base(base)?;
        Some(self.temporal_node(expr, row, base, TypedTemporal::All))
    }

    /// `.base.$at(t)` and `.base.$between(a, b)` (§14.1).
    pub(crate) fn check_temporal_call(
        &mut self,
        expr: &Expr,
        base: &Expr,
        selector: &str,
        args: &[Arg],
    ) -> Option<TypedExpr> {
        let (base, row) = self.temporal_base(base)?;
        let query = match selector {
            "at" => {
                let mut instants = self.instants(expr, args, 1)?;
                TypedTemporal::At(Box::new(instants.remove(0)))
            }
            "between" => {
                let mut instants = self.instants(expr, args, 2)?;
                let start = instants.remove(0);
                let end = instants.remove(0);
                TypedTemporal::Between {
                    start: Box::new(start),
                    end: Box::new(end),
                }
            }
            other => {
                return self.error(
                    expr,
                    format!("`.${other}(…)` is not a temporal selector (expected `.$at`/`.$between`, §14.1)"),
                );
            }
        };
        Some(self.temporal_node(expr, row, base, query))
    }

    /// Check a selector's base and require it to be a view (§14.1 applies to a
    /// bucketed collection).
    fn temporal_base(&mut self, base_expr: &Expr) -> Option<(TypedExpr, RowType)> {
        let base = self.check(base_expr)?;
        match base.ty() {
            ExprType::View(row) => {
                let row = row.clone();
                Some((base, row))
            }
            other => {
                let message =
                    format!("a temporal selector applies to a view, not a {}", other.describe());
                self.report(base_expr, message);
                None
            }
        }
    }

    /// Type-check exactly `count` `timestamp` instant arguments (§14.1).
    fn instants(&mut self, expr: &Expr, args: &[Arg], count: usize) -> Option<Vec<TypedExpr>> {
        if args.len() != count {
            self.report(
                expr,
                format!("this temporal selector takes {count} `timestamp` argument(s), found {}", args.len()),
            );
            return None;
        }
        let mut typed = Vec::with_capacity(count);
        for arg in args {
            let value = match arg {
                Arg::Positional(value) => value,
                Arg::Named { value, .. } => value,
            };
            let checked = self.check(value)?;
            if !matches!(checked.ty().as_scalar(), Some(Type::Timestamp(_))) {
                self.report(value, "a temporal selector instant must be a `timestamp`");
                return None;
            }
            typed.push(checked);
        }
        Some(typed)
    }

    fn temporal_node(
        &self,
        expr: &Expr,
        row: RowType,
        base: TypedExpr,
        query: TypedTemporal,
    ) -> TypedExpr {
        TypedExpr::new(
            expr.span,
            ExprType::View(row),
            TypedKind::Temporal {
                base: Box::new(base),
                query,
            },
        )
    }
}
