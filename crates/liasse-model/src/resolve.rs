//! Projecting the state tree onto the [`liasse_expr`] type model.
//!
//! Expression checking asks "what is the type of `.tasks`, of `.` here, of the
//! package root". [`Resolver`] answers by walking [`Node`]s and [`Shape`]s into
//! [`ExprType`]/[`RowType`], resolving reusable `$types` shapes (§5.8) on the
//! way. Recursive shapes (`subcompanies: "company"`) would expand forever, so a
//! depth cap terminates the projection; access nested past the cap simply stops
//! type-checking, which no ordinary static expression reaches. That cap is a
//! documented CORE simplification of full lazy shape typing.

use std::collections::BTreeMap;

use liasse_expr::{ExprType, RowType};
use liasse_value::{RefTarget, Type};

use crate::state::{Collection, Node, Shape};

/// Depth beyond which recursive `$types` expansion yields an opaque type.
const MAX_DEPTH: u32 = 32;

/// The row type at a receiver `path` from a root row (§8.2), shared by the
/// mutation, bucket, and meter phases: walk each segment through a row/view
/// field, folding a collection or view field to its single-row type.
pub(crate) fn row_at(root: &ExprType, path: &[String]) -> Option<ExprType> {
    let mut current = root.clone();
    for segment in path {
        let row = current.as_row()?;
        current = match row.field(segment)? {
            ExprType::View(row) | ExprType::Row(row) => ExprType::Row(row.clone()),
            _ => return None,
        };
    }
    Some(current)
}

/// Projects state-tree nodes onto expression types against a `$types` table.
pub(crate) struct Resolver<'a> {
    types: &'a BTreeMap<String, Node>,
}

impl<'a> Resolver<'a> {
    /// A resolver over the package's reusable `$types` shapes.
    pub(crate) fn new(types: &'a BTreeMap<String, Node>) -> Self {
        Self { types }
    }

    /// The keyless row type of a shape (a static struct or the model root).
    pub(crate) fn shape_row(&self, shape: &Shape) -> RowType {
        self.shape_at(shape, None, 0)
    }

    /// The keyed row type of a collection.
    pub(crate) fn collection_row(&self, collection: &Collection) -> RowType {
        self.collection_at(collection, 0)
    }

    fn node_at(&self, node: &Node, depth: u32) -> ExprType {
        if depth > MAX_DEPTH {
            return ExprType::scalar(Type::Json);
        }
        match node {
            Node::Scalar(field) => ExprType::scalar(field.ty.clone()),
            Node::Struct(shape) => ExprType::Row(self.shape_at(shape, None, depth)),
            Node::Collection(collection) => {
                ExprType::View(self.collection_at(collection, depth))
            }
            Node::Set(set) => ExprType::scalar(Type::Set(Box::new(set.element.clone()))),
            Node::View(view) => ExprType::View(view.row.clone()),
            Node::Reference(reference) => ExprType::scalar(Type::Ref(RefTarget::Scalar(
                Box::new(reference.key_type.clone()),
            ))),
            Node::Named(name) => match self.types.get(name) {
                Some(target) => self.node_at(target, depth + 1),
                None => ExprType::scalar(Type::Json),
            },
        }
    }

    fn shape_at(&self, shape: &Shape, key: Option<ExprType>, depth: u32) -> RowType {
        let fields = shape
            .members
            .iter()
            .map(|member| (member.name.as_str().to_owned(), self.node_at(&member.node, depth + 1)));
        RowType::new(fields, key)
    }

    fn collection_at(&self, collection: &Collection, depth: u32) -> RowType {
        let key = self.key_type(collection, depth);
        self.shape_at(&collection.shape, Some(key), depth)
    }

    /// The identity type of a collection's primary key (§5.4, A.9): the field
    /// type for a single key, or a struct of the key fields for a composite key.
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
            _ => {
                let struct_ty = liasse_value::StructType::new(components);
                ExprType::scalar(Type::Struct(struct_ty))
            }
        }
    }
}
