//! Pre-check inference of model-root computed-value types (§5.1, §5.2).
//!
//! A model-root computed value (`name: "= expr"`) is built with a placeholder
//! `json` type ([`ScalarField::ty`](crate::state::ScalarField::ty)) because the
//! result type is only known once its expression is typed. A reference to it
//! (`.name`) must resolve to that inferred type — a `bool` computed value used
//! as a `? :` condition, an `int` used in arithmetic, a `timestamp` used in an
//! ordering — not to the widest `json`, which would over-reject those uses.
//!
//! This pass types each root computed value against the model root and writes
//! the inferred scalar type back onto the field, so the later
//! [`check`](crate::check) tree walk and every expression that reads the value
//! observe the real type. It runs before the tree check purely to compute types;
//! diagnostics are the tree check's job and are discarded here. A computed value
//! that reads a sibling computed value is handled by iterating to a fixpoint in
//! dependency order (a cycle is left for `check`'s §5.1 acyclic-default rule to
//! report). Only a placeholder `json` computed field is ever refined, so the
//! pass can only make a reference *more* precisely typed, never override an
//! authored type.

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType};
use liasse_syntax::parse_expression;
use liasse_value::Type;

use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{Node, Shape};

/// Refine the type of every model-root computed value from its expression.
pub(crate) fn root_computed_types(sources: &mut SourceMap, resolver: &Resolver, root: &mut Shape) {
    // The names of root members that are still-placeholder computed values.
    let pending: Vec<String> = root
        .members
        .iter()
        .filter_map(|member| match &member.node {
            Node::Scalar(field) if field.computed.is_some() && field.ty == Type::Json => {
                Some(member.name.as_str().to_owned())
            }
            _ => None,
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    // A computed value may read a sibling computed value, so a single left-to-
    // right pass could type `.sibling` before the sibling is refined. Iterate to
    // a fixpoint: refinement is monotone toward a concrete type (a `json`
    // placeholder only ever becomes more precise), so at most one refinement can
    // land per pending field, bounding the loop at `pending.len()` passes.
    for _ in 0..pending.len() {
        let mut changed = false;
        for name in &pending {
            let Some(inferred) = infer_one(sources, resolver, root, name) else {
                continue;
            };
            if let Some(field) = root.members.iter_mut().find_map(|member| match &mut member.node {
                Node::Scalar(field) if member.name.as_str() == name && field.ty == Type::Json => Some(field),
                _ => None,
            }) {
                field.ty = inferred;
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Type the model-root computed value `name` against the current model root,
/// returning its inferred concrete scalar type (never `json`, which carries no
/// new information over the placeholder).
fn infer_one(sources: &mut SourceMap, resolver: &Resolver, root: &Shape, name: &str) -> Option<Type> {
    let field = root.members.iter().find_map(|member| match &member.node {
        Node::Scalar(field) if member.name.as_str() == name => Some(field),
        _ => None,
    })?;
    let source = field.computed.as_ref()?;
    // Building the scope reads the (partially refined) root immutably; the caller
    // applies any write afterwards, so the borrows never overlap.
    let root_ty = ExprType::Row(resolver.shape_row(root));
    let scope = ModelScope::nested(vec![root_ty.clone()], root_ty);
    let parsed = parse_expression(sources.add_label("infer", source.text.clone()), &source.text).ok()?;
    let typed = check_statement(&scope, sources.add_label("infer", source.text.clone()), &parsed).ok()?;
    match typed.ty().as_scalar() {
        Some(scalar) if *scalar != Type::Json => Some(scalar.clone()),
        _ => None,
    }
}
