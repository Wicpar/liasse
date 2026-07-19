//! Pre-check inference of computed-value types (§5.1, §5.2, §5.3).
//!
//! A computed value (`name: "= expr"`) is built with a placeholder `json` type
//! ([`ScalarField::ty`](crate::state::ScalarField::ty)) because its result type
//! is only known once its expression is typed. A reference to it (`.name`) must
//! resolve to that inferred type — a `bool` computed value used as a `? :`
//! condition, an `int` used in arithmetic or a comparison, a `timestamp` used in
//! an ordering — not to the widest `json`, which carries no typed operator and
//! would over-reject those uses (§5.2: a computed value "participates in views,
//! checks, sorting, and projections like any other value").
//!
//! This pass types every computed value in the tree — at the model root and
//! inside every nested struct and keyed collection — against the row it reads as
//! `.`, and writes the inferred scalar type back onto the field, so the later
//! [`check`](crate::check) tree walk and every expression that reads the value
//! (a `$check`, a default, a `$view`, another computed value) observe the real
//! type. It runs before the tree check purely to compute types; diagnostics are
//! the tree check's job and are discarded here.
//!
//! §5.1/§5.3: defaults and computed values form one dependency graph, and a
//! computed value may read a sibling (or, through `/`, a cross-shape) computed
//! value. The pass iterates the whole tree to a fixpoint in dependency order: a
//! placeholder `json` computed field is only ever refined *toward* a concrete
//! type, so the fixpoint is monotone and bounded by the count of pending fields.
//! A field on a genuine cycle never types and is left `json` for [`check`]'s §5.1
//! acyclic-default rule to report. Only a placeholder `json` computed field is
//! ever refined, so the pass can only make a reference *more* precisely typed,
//! never override an authored type.

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType};
use liasse_syntax::parse_expression;
use liasse_value::Type;

use crate::resolve::Resolver;
use crate::scope::ModelScope;
use crate::state::{ExprSource, Node, Shape};

/// Refine the type of every computed value in the tree from its expression.
///
/// `config` is a module package's `$config` struct row (§13.1), bound as the
/// `$config` structural so a computed value that reads it refines against its
/// members rather than failing to type; `None` outside a module.
pub(crate) fn computed_types(
    sources: &mut SourceMap,
    resolver: &Resolver,
    root: &mut Shape,
    config: Option<&ExprType>,
) {
    // Each pass refines at least one field or reaches the fixpoint; a monotone
    // `json`-to-concrete refinement over `bound` pending fields settles in at
    // most `bound` passes. The package root `/` is rebuilt each pass so a
    // computed value reading a refined field through a `/collection[...]`
    // selector observes the refinement (§5.3).
    let bound = pending_count(root);
    for _ in 0..bound {
        let root_row = ExprType::Row(resolver.shape_row(root));
        if !refine_shape(sources, resolver, &[], &root_row, root, config) {
            return;
        }
    }
}

/// One refinement pass over `shape` and its descendants. `ancestors` is the
/// lexical chain of enclosing rows (outermost first), excluding `shape` itself;
/// `root_row` is the package root `/`. Returns whether any computed field's type
/// was refined this pass.
fn refine_shape(
    sources: &mut SourceMap,
    resolver: &Resolver,
    ancestors: &[ExprType],
    root_row: &ExprType,
    shape: &mut Shape,
    config: Option<&ExprType>,
) -> bool {
    // The lexical chain a computed value in this shape reads: the ancestors plus
    // this shape's own row as `.`. Built from the shape's current (partially
    // refined) state, so a sibling refined earlier this pass is already visible.
    let own_row = ExprType::Row(resolver.shape_row(shape));
    let mut chain = ancestors.to_vec();
    chain.push(own_row);

    // The still-placeholder computed fields, with their expression text.
    let pending: Vec<(String, ExprSource)> = shape
        .members
        .iter()
        .filter_map(|member| match &member.node {
            Node::Scalar(field) if field.computed.is_some() && field.ty == Type::Json => field
                .computed
                .clone()
                .map(|src| (member.name.as_str().to_owned(), src)),
            _ => None,
        })
        .collect();
    let mut changed = false;
    for (name, src) in &pending {
        if let Some(inferred) = infer_expr(sources, &chain, root_row, src, config) {
            changed |= set_computed_type(shape, name, inferred);
        }
    }

    // Recurse into nested structs and collections, refreshing this shape's row so
    // a descendant reading `^.<field>` sees this pass's refinements.
    let refreshed = ExprType::Row(resolver.shape_row(shape));
    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(refreshed);
    for member in &mut shape.members {
        let inner = match &mut member.node {
            Node::Struct(inner) => inner,
            Node::Collection(collection) => &mut collection.shape,
            _ => continue,
        };
        changed |= refine_shape(sources, resolver, &child_ancestors, root_row, inner, config);
    }
    changed
}

/// Type the computed expression `src` against the lexical `chain` and package
/// root `root_row`, returning its inferred concrete scalar type (never `json`,
/// which carries no new information over the placeholder).
fn infer_expr(
    sources: &mut SourceMap,
    chain: &[ExprType],
    root_row: &ExprType,
    src: &ExprSource,
    config: Option<&ExprType>,
) -> Option<Type> {
    let scope = ModelScope::nested(chain.to_vec(), root_row.clone())
        .with_optional_structural("config", config);
    let parsed = parse_expression(sources.add_label("infer", src.text.clone()), &src.text).ok()?;
    let typed = check_statement(&scope, sources.add_label("infer", src.text.clone()), &parsed).ok()?;
    match typed.ty().as_scalar() {
        Some(scalar) if *scalar != Type::Json => Some(scalar.clone()),
        _ => None,
    }
}

/// Write `ty` onto the still-placeholder computed field `name` of `shape`,
/// returning whether a refinement was applied. A field already refined (or not a
/// placeholder computed value) is left untouched, keeping the pass monotone.
fn set_computed_type(shape: &mut Shape, name: &str, ty: Type) -> bool {
    let field = shape.members.iter_mut().find_map(|member| match &mut member.node {
        Node::Scalar(field)
            if member.name.as_str() == name && field.computed.is_some() && field.ty == Type::Json =>
        {
            Some(field)
        }
        _ => None,
    });
    match field {
        Some(field) => {
            field.ty = ty;
            true
        }
        None => false,
    }
}

/// The number of still-placeholder computed fields in `shape` and its
/// descendants — an upper bound on the number of refinement passes.
fn pending_count(shape: &Shape) -> usize {
    shape
        .members
        .iter()
        .map(|member| match &member.node {
            Node::Scalar(field) if field.computed.is_some() && field.ty == Type::Json => 1,
            Node::Struct(inner) => pending_count(inner),
            Node::Collection(collection) => pending_count(&collection.shape),
            _ => 0,
        })
        .sum()
}
