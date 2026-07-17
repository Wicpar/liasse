//! Seed-value materialization in a literal-or-expression position (§4.2, Annex
//! C.4).
//!
//! A `$data` value (and an expanded `$default`) is a *literal-or-expression*
//! position: a string is not always stored verbatim. Annex C.4 pins three forms
//! a string value takes before it decodes to the field type:
//!
//! ```text
//! "= expr"   the expression `expr`, evaluated against the seed state
//! "'= text"  the literal text beginning with `=` (one leading `'` removed)
//! "'text"    the literal text with one leading `'` removed
//! ```
//!
//! The leading-`'` escape removes **exactly one** quote and stores the remainder
//! verbatim — it never evaluates — so `"''x"` stores `"'x"`, `"'"` stores the
//! empty string, and `"'= 1 + 1"` stores the literal `"= 1 + 1"`. A string
//! beginning with `=` (and not escaped) is an expression evaluated against the
//! prospective seed state, so a `"= 1 + 1"` seed stores the computed `2`. Every
//! other string, and every non-string value, decodes verbatim as before.
//!
//! The same rule governs every `$data` value position: a keyed-collection field,
//! a static-struct member, and a singleton root member (§5.3, §8.2).

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, Cell, ExprType, Row, RowId, RowType};
use liasse_model::{Model, Node, Shape};
use liasse_syntax::{parse_expression, DocValue};
use liasse_value::{Struct, Text, Type, Value};

use crate::compiled::CompiledCollection;
use crate::doc;
use crate::error::{Rejection, RejectionReason};
use crate::eval::{row_cell, EvalCtx};
use crate::materialize::FieldMap;
use crate::scope::RuntimeScope;
use crate::state::Prospective;

/// Materialize a supplied seed value for field `name` of a keyed-collection row
/// (§4.2, C.4). `fields` are the row's values decoded so far, exposed as `.` to a
/// `= expr` value so an expression may read an earlier sibling field.
pub(crate) fn materialize(
    ty: &Type,
    name: &str,
    value: &DocValue,
    collection: &CompiledCollection,
    fields: &FieldMap,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, Rejection> {
    let row_ty = ExprType::Row(collection_row_type(collection));
    let scope = RuntimeScope::new(row_ty, ExprType::Row(ctx.schema.root_row_type()));
    let current = row_cell(collection, fields);
    materialize_typed(ty, name, value, &scope, &current, ctx, prospective)
}

/// Materialize a supplied seed value for a singleton root member (§8.2): a static
/// struct materializes each declared member the same way (§5.3); a scalar, set, or
/// reference member honors the escape/expression forms against the package root.
pub(crate) fn materialize_singleton(
    model: &Model,
    node: &Node,
    name: &str,
    value: &DocValue,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, Rejection> {
    match resolve(model, node) {
        Node::Struct(shape) => materialize_struct(model, shape, name, value, ctx, prospective),
        node => {
            let Some(ty) = crate::singleton::member_type(model, node) else {
                return Err(Rejection::new(RejectionReason::Malformed, format!("`{name}` is not seedable state")));
            };
            let root_ty = ExprType::Row(ctx.schema.root_row_type());
            let scope = RuntimeScope::new(root_ty.clone(), root_ty);
            materialize_typed(&ty, name, value, &scope, &empty_row(), ctx, prospective)
        }
    }
}

/// Materialize each declared member of a static-struct seed value (§5.3): an
/// omitted member stays absent (its default resolution is a documented seam), a
/// present one honors the escape/expression forms and recurses into a nested
/// struct.
fn materialize_struct(
    model: &Model,
    shape: &Shape,
    name: &str,
    value: &DocValue,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, Rejection> {
    let members = doc::object(value)
        .ok_or_else(|| Rejection::new(RejectionReason::Malformed, format!("struct `{name}` must be an object")))?;
    let root_ty = ExprType::Row(ctx.schema.root_row_type());
    let scope = RuntimeScope::new(root_ty.clone(), root_ty);
    let mut entries = Vec::new();
    for member in &shape.members {
        let Some(supplied) = members.iter().find(|m| m.name.text == member.name.as_str()) else { continue };
        let field_name = member.name.as_str();
        let field = match resolve(model, &member.node) {
            Node::Struct(inner) => materialize_struct(model, inner, field_name, &supplied.value, ctx, prospective)?,
            node => {
                let Some(ty) = crate::singleton::member_type(model, node) else { continue };
                materialize_typed(&ty, field_name, &supplied.value, &scope, &empty_row(), ctx, prospective)?
            }
        };
        entries.push((Text::new(field_name.to_owned()), field));
    }
    Ok(Value::Struct(Struct::new(entries)))
}

/// The core materialization of one `$data` value against `ty` (§4.2, C.4): honor
/// the leading-`'` literal escape and the leading-`=` expression form (evaluated
/// against `scope`/`current`), else decode verbatim.
fn materialize_typed(
    ty: &Type,
    name: &str,
    value: &DocValue,
    scope: &RuntimeScope,
    current: &Cell,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, Rejection> {
    if let Some(text) = doc::string(value) {
        if let Some(literal) = text.strip_prefix('\'') {
            // §4.2/C.4: one leading `'` removed; the remainder is a literal string
            // that is never evaluated (so `"'= 1 + 1"` stores `"= 1 + 1"`).
            return decode(ty, &serde_json::Value::String(literal.to_owned()), name);
        }
        if let Some(expr) = text.strip_prefix('=') {
            return evaluate(expr, ty, name, scope, current, ctx, prospective);
        }
    }
    decode(ty, &doc::to_json(value), name)
}

/// Evaluate a `= expr` seed value against the prospective seed state (§4.2), then
/// decode the scalar result against the field type so it is stored in the same
/// canonical form an ordinary seed value would be.
fn evaluate(
    expr: &str,
    ty: &Type,
    name: &str,
    scope: &RuntimeScope,
    current: &Cell,
    ctx: &EvalCtx<'_>,
    prospective: &Prospective,
) -> Result<Value, Rejection> {
    let expr = expr.trim();
    let mut sources = SourceMap::new();
    let src = sources.add_label("$data expression", expr.to_owned());
    let parsed = parse_expression(src, expr).map_err(|d| {
        Rejection::new(RejectionReason::Malformed, format!("seed field `{name}`: {}", d.render(&sources)))
    })?;
    let typed = check_statement(scope, src, &parsed).map_err(|d| {
        Rejection::new(RejectionReason::TypeError, format!("seed field `{name}`: {}", d.render(&sources)))
    })?;
    let value = match ctx.eval(prospective, &typed, current)? {
        Cell::Scalar(value) => value,
        // A ref-producing seed expression yields the target row's key (§5.6).
        Cell::Row(row) => row.key().clone(),
        Cell::Collection(_) => {
            return Err(Rejection::new(
                RejectionReason::TypeError,
                format!("seed field `{name}`: a `=` expression must evaluate to a scalar value"),
            ));
        }
    };
    // Round-trip through the field type so a `= 1 + 1` on an `int` field stores
    // the canonical `int`, and a mistyped result is rejected as a type error.
    decode(ty, &value.to_wire(), name)
}

/// The row's `.` type for a collection seed expression: each writable field as its
/// declared scalar type, keyless (a seed expression reads sibling fields, not
/// identity).
fn collection_row_type(collection: &CompiledCollection) -> RowType {
    RowType::keyless(
        collection
            .fields
            .iter()
            .map(|field| (field.name.clone(), ExprType::scalar(field.ty.clone()))),
    )
}

/// Unwrap a `$types` reference to the node it names, so struct/scalar dispatch sees
/// the resolved shape.
fn resolve<'a>(model: &'a Model, node: &'a Node) -> &'a Node {
    match node {
        Node::Named(name) => model.types().get(name).map_or(node, |inner| resolve(model, inner)),
        _ => node,
    }
}

/// An empty keyless `.` for a seed expression that reads no enclosing row — a
/// singleton/struct member expression reads `/…` through the environment root.
fn empty_row() -> Cell {
    Cell::Row(Box::new(Row::keyless(RowId::leaf(0), std::iter::empty())))
}

fn decode(ty: &Type, wire: &serde_json::Value, field: &str) -> Result<Value, Rejection> {
    ty.decode(wire)
        .map_err(|error| Rejection::new(RejectionReason::TypeError, format!("seed field `{field}`: {error}")))
}
