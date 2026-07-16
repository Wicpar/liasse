//! Structural traversal over the spanned expression AST, shared by the model's
//! phases.
//!
//! `liasse-syntax` produces the tree but owns no semantics, so each model phase
//! walks it for its own question — sibling-dependency collection and generated
//! function detection ([`crate::check`]), parameter-reference collection
//! ([`crate::mutation`]). They all need the same answer to "what are a node's
//! direct child expressions?", and keeping that answer in one place keeps every
//! walk in lockstep with the AST when a variant is added.

use liasse_syntax::{Arg, BlockMember, BlockMemberKind, Expr, ExprKind, Selector};

/// The direct child expressions of a node, in source order.
pub(crate) fn child_exprs(expr: &Expr) -> Vec<&Expr> {
    let mut out: Vec<&Expr> = Vec::new();
    match &expr.kind {
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => out.push(base),
        ExprKind::Select { base, selector } => {
            out.push(base);
            match selector {
                Selector::Keys(keys) => out.extend(keys.iter()),
                Selector::Bind { condition, .. } => {
                    out.extend(condition.iter().map(|c| c.as_ref()));
                }
            }
        }
        ExprKind::Call { callee, args } => {
            out.push(callee);
            out.extend(args.iter().map(arg_expr));
        }
        ExprKind::Block { base, members } => {
            out.push(base);
            out.extend(members.iter().filter_map(block_member_expr));
        }
        ExprKind::Unary { operand, .. } => out.push(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        ExprKind::Ternary { cond, then, otherwise } => {
            out.push(cond);
            out.push(then);
            out.push(otherwise);
        }
        ExprKind::List(items) => out.extend(items.iter()),
        ExprKind::Object(members) => out.extend(members.iter().filter_map(block_member_expr)),
        ExprKind::Combination { operands, .. } => out.extend(operands.iter()),
        _ => {}
    }
    out
}

/// The value expression a positional or named argument carries.
fn arg_expr(arg: &Arg) -> &Expr {
    match arg {
        Arg::Positional(value) | Arg::Named { value, .. } => value,
    }
}

/// The value expression a block member carries, if it carries one.
fn block_member_expr(member: &BlockMember) -> Option<&Expr> {
    match &member.kind {
        BlockMemberKind::Directive { value, .. }
        | BlockMemberKind::Named { value: Some(value), .. }
        | BlockMemberKind::Assign { value, .. }
        | BlockMemberKind::Shorthand(value) => Some(value),
        BlockMemberKind::Named { value: None, .. } | BlockMemberKind::Clear(_) => None,
    }
}
