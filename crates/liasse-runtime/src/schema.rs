//! Projecting the validated [`Model`] onto the expression type model.
//!
//! The runtime must re-check every `$mut` value expression it evaluates (the
//! model validates programs but keeps only their parameter contract, not a
//! typed tree), which needs a [`Scope`](liasse_expr::Scope). A scope resolves
//! roots to their [`ExprType`], so the runtime needs the same state-tree →
//! [`RowType`] projection the model used internally. That projection is private
//! to the model, so [`Schema`] reproduces it over the public [`Model`] surface,
//! matching `liasse-model`'s resolver node-for-node (§5, §8.2).

use liasse_expr::{ExprType, RowType};
use liasse_model::{Collection, Model, Node, Shape};
use liasse_value::{RefTarget, StructType, Type};

/// Depth beyond which recursive `$types` expansion yields an opaque `json`,
/// matching the model resolver's cap (a documented CORE simplification).
const MAX_DEPTH: u32 = 32;

/// A typed view over a validated [`Model`]: projections plus the collection and
/// field metadata the admission pipeline consults.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Schema<'m> {
    model: &'m Model,
}

impl<'m> Schema<'m> {
    /// Wrap a validated model.
    pub(crate) fn new(model: &'m Model) -> Self {
        Self { model }
    }

    /// The underlying model.
    pub(crate) fn model(&self) -> &'m Model {
        self.model
    }

    /// The keyless row type of the package root `/` (§8.2).
    pub(crate) fn root_row_type(&self) -> RowType {
        self.shape_row(self.model.root(), None, 0)
    }

    /// The receiver `.` type at `path` from the root, folding each collection or
    /// view field to its single-row type (§8.2). An empty path is the root.
    pub(crate) fn receiver_row_type(&self, path: &[String]) -> Option<ExprType> {
        let mut current = ExprType::Row(self.root_row_type());
        for segment in path {
            let row = current.as_row()?;
            current = match row.field(segment)? {
                ExprType::View(row) | ExprType::Row(row) => ExprType::Row(row.clone()),
                _ => return None,
            };
        }
        Some(current)
    }

    /// The top-level collection declared under `name`, if any.
    pub(crate) fn top_collection(&self, name: &str) -> Option<&'m Collection> {
        Self::collection_member(self.model.root(), name)
    }

    fn collection_member<'a>(shape: &'a Shape, name: &str) -> Option<&'a Collection> {
        match shape.member(name).map(|m| &m.node) {
            Some(Node::Collection(collection)) => Some(collection),
            _ => None,
        }
    }

    fn shape_row(&self, shape: &Shape, key: Option<ExprType>, depth: u32) -> RowType {
        let fields = shape
            .members
            .iter()
            .map(|member| (member.name.as_str().to_owned(), self.node_at(&member.node, depth + 1)));
        RowType::new(fields, key)
    }

    fn collection_row(&self, collection: &Collection, depth: u32) -> RowType {
        let key = self.key_type(collection, depth);
        self.shape_row(&collection.shape, Some(key), depth)
    }

    fn node_at(&self, node: &Node, depth: u32) -> ExprType {
        if depth > MAX_DEPTH {
            return ExprType::scalar(Type::Json);
        }
        match node {
            Node::Scalar(field) => ExprType::scalar(field.ty.clone()),
            Node::Struct(shape) => ExprType::Row(self.shape_row(shape, None, depth)),
            Node::Collection(collection) => ExprType::View(self.collection_row(collection, depth)),
            Node::Set(set) => ExprType::scalar(Type::Set(Box::new(set.element.clone()))),
            Node::View(view) => ExprType::View(view.row.clone()),
            Node::Reference(reference) => {
                ExprType::scalar(Type::Ref(RefTarget::Scalar(Box::new(reference.key_type.clone()))))
            }
            Node::Named(name) => match self.model.types().get(name) {
                Some(target) => self.node_at(target, depth + 1),
                None => ExprType::scalar(Type::Json),
            },
        }
    }

    /// The identity type of a collection's primary key (§5.4, A.9).
    fn key_type(&self, collection: &Collection, depth: u32) -> ExprType {
        let mut components: Vec<(String, Type)> = Vec::new();
        for field in &collection.key {
            let ty = collection
                .shape
                .member(field.as_str())
                .map(|member| self.node_at(&member.node, depth + 1))
                .and_then(|et| et.as_scalar().cloned())
                .unwrap_or(Type::Json);
            components.push((field.as_str().to_owned(), ty));
        }
        match components.as_slice() {
            [(_, ty)] => ExprType::scalar(ty.clone()),
            _ => ExprType::scalar(Type::Struct(StructType::new(components))),
        }
    }
}

/// Whether a value of type `from` may be assigned to a field of type `to`
/// (§8.5): the declared type, an optional widening, or `none` into an optional.
#[must_use]
pub(crate) fn assignable(from: &Type, to: &Type) -> bool {
    if from == to {
        return true;
    }
    match to {
        Type::Optional(inner) => is_none(from) || assignable(from, inner),
        // `json` accepts any json-shaped scalar the checker already narrowed.
        Type::Json => matches!(from, Type::Json),
        _ => false,
    }
}

/// Whether `ty` is the (widest) optional a bare `none` literal carries, so a
/// `none` value is recognised as assignable into any optional target.
fn is_none(ty: &Type) -> bool {
    matches!(ty, Type::Optional(inner) if matches!(**inner, Type::Json))
}
