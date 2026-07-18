//! Root singleton state (§8.2): the package root's non-collection writable
//! members — scalar fields, static structs, sets, and references declared
//! directly under `$model` rather than inside a keyed collection.
//!
//! A keyed collection stores one row per key; a singleton root field stores one
//! value for the whole instance. Both are durable writable state, so the
//! singleton fields are kept as a single reserved row (a struct of every
//! singleton member's value) that gathers, diffs, and stages exactly like any
//! collection row. At materialization they fold back onto the package-root
//! [`Row`] as cells, so a view or projection reads `.field` (or a nested
//! `.struct.member`) the same way it reads a collection.

use liasse_expr::{Cell, Row, RowId};
use liasse_ident::NameSegment;
use liasse_model::{Model, Node, Shape};
use liasse_store::{AddressStep, CollectionPath, KeyValue, RowAddress};
use liasse_value::{RefTarget, StructType, Text, Type, Value};

use crate::materialize::FieldMap;

/// Reserved collection name of the singleton root row. A model member can never
/// carry this name — `$`-prefixed keys are directives, not declared members —
/// so it never collides with an application collection.
pub(crate) const ROOT_NAME: &str = "$root";

/// The reserved address of the singleton root row.
#[must_use]
pub(crate) fn address() -> RowAddress {
    RowAddress::root(AddressStep::new(NameSegment::new(ROOT_NAME), KeyValue::single(Value::Text(Text::new("")))))
}

/// The reserved collection path the singleton root row is scanned under.
#[must_use]
pub(crate) fn path() -> CollectionPath {
    CollectionPath::top(NameSegment::new(ROOT_NAME))
}

/// The decodable [`Type`] of a singleton-eligible root (or struct) member, or
/// `None` for a member that is not durable singleton state: a keyed collection,
/// a computed view, or a read-only computed scalar (§5.2).
pub(crate) fn member_type(model: &Model, node: &Node) -> Option<Type> {
    match node {
        Node::Scalar(field) if field.is_writable() => Some(field.ty.clone()),
        Node::Scalar(_) | Node::Collection(_) | Node::View(_) => None,
        Node::Set(set) => Some(Type::Set(Box::new(set.element.clone()))),
        Node::Reference(reference) => {
            Some(Type::Ref(RefTarget::for_key(&reference.key_type)))
        }
        Node::Struct(shape) => Some(Type::Struct(struct_type(model, shape))),
        Node::Named(name) => model.types().get(name).and_then(|node| member_type(model, node)),
    }
}

/// The [`StructType`] of a static struct shape, over its singleton-eligible
/// members (a nested collection or view inside a struct is not durable state).
fn struct_type(model: &Model, shape: &Shape) -> StructType {
    StructType::new(
        shape
            .members
            .iter()
            .filter_map(|member| Some((member.name.as_str().to_owned(), member_type(model, &member.node)?))),
    )
}

/// Fold every singleton root member of `shape` into read-facing cells over the
/// stored singleton `fields`. An absent member reads as `none` (a struct member
/// as an all-`none` sub-row), matching how a collection materializes an
/// unwritten field.
pub(crate) fn cells(model: &Model, shape: &Shape, fields: &FieldMap) -> Vec<(String, Cell)> {
    shape
        .members
        .iter()
        .filter_map(|member| {
            member_type(model, &member.node)?;
            let value = fields.get(member.name.as_str()).cloned().unwrap_or(Value::None);
            Some((member.name.as_str().to_owned(), node_cell(model, &member.node, value)))
        })
        .collect()
}

/// The read-facing cell of one singleton member: a static struct becomes a
/// keyless [`Row`] whose members recurse (so `.struct.member` resolves), every
/// other member its scalar value.
fn node_cell(model: &Model, node: &Node, value: Value) -> Cell {
    match node {
        Node::Struct(shape) => Cell::Row(Box::new(struct_row(model, shape, value))),
        Node::Named(name) => match model.types().get(name) {
            Some(inner) => node_cell(model, inner, value),
            None => Cell::Scalar(value),
        },
        _ => Cell::Scalar(value),
    }
}

/// A static struct value as a keyless row of its members' cells.
fn struct_row(model: &Model, shape: &Shape, value: Value) -> Row {
    let members = match value {
        Value::Struct(members) => members,
        _ => liasse_value::Struct::new(Vec::<(Text, Value)>::new()),
    };
    let cells = shape.members.iter().filter_map(|member| {
        member_type(model, &member.node)?;
        let field = members.get(member.name.as_str()).cloned().unwrap_or(Value::None);
        Some((member.name.as_str().to_owned(), node_cell(model, &member.node, field)))
    });
    Row::new(RowId::leaf(0), Value::None, cells)
}
