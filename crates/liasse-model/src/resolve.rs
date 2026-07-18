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
            Node::Reference(reference) => {
                ExprType::scalar(Type::Ref(RefTarget::for_key(&reference.key_type)))
            }
            Node::Named(name) => match self.types.get(name) {
                Some(target) => self.node_at(target, depth + 1),
                None => ExprType::scalar(Type::Json),
            },
        }
    }

    fn shape_at(&self, shape: &Shape, key: Option<ExprType>, depth: u32) -> RowType {
        let mut fields: Vec<(String, ExprType)> = shape
            .members
            .iter()
            .map(|member| (member.name.as_str().to_owned(), self.node_at(&member.node, depth + 1)))
            .collect();
        // §15.6: a row that declares a meter with `$limits` exposes that meter's
        // accessor (`.<meter>.balance`, `.<meter>.pools`) so a view or computed
        // value reading remaining capacity type-checks. A same-named application
        // field wins (it is already in `fields`).
        for meter in &shape.meters {
            if fields.iter().all(|(name, _)| name != meter) {
                fields.push((meter.clone(), ExprType::Row(meter_accessor_row())));
            }
        }
        RowType::new(fields, key)
    }

    fn collection_at(&self, collection: &Collection, depth: u32) -> RowType {
        let key = self.key_type(collection, depth);
        let mut row = self.shape_at(&collection.shape, Some(key), depth);
        // §15.3/§15.6: a spending collection's rows expose `funding`, the fixed
        // admission allocation recorded for the spend. The runtime materializes
        // it; the model gives the accessor a type so `spend.funding` and
        // `spend { funding }` check. A user field of the same name wins.
        if collection.consumes && row.field("funding").is_none() {
            let fields = row
                .fields()
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .chain(std::iter::once(("funding".to_owned(), ExprType::View(funding_row()))))
                .collect::<Vec<_>>();
            row = RowType::new(fields, row.key().cloned());
        }
        row
    }

    /// The identity type of a collection's primary key (§5.4, A.9): the field
    /// type for a single key, or the composite key type (`(name, type)` in `$key`
    /// order) for a composite key.
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
            _ => ExprType::scalar(Type::Composite(components)),
        }
    }
}

/// The row shape of a §15.6 meter accessor (`.<meter>`): the members an
/// expression reads off it. `balance` is the context-free current remaining
/// capacity (a non-negative `decimal`, §15.1/§15.2); `pools` is the eligible
/// pool view. The parameterized `balance({...})`/`pools({...})` call forms
/// (§15.6) are a documented runtime seam, not modelled here.
fn meter_accessor_row() -> RowType {
    RowType::keyless([
        ("balance".to_owned(), ExprType::scalar(Type::Decimal)),
        ("pools".to_owned(), ExprType::View(RowType::keyless(std::iter::empty()))),
    ])
}

/// The row shape of a spend's `funding` view (§15.3): the members the returned
/// funding view pins — the source label, the pool identity, and the allocated
/// amount. The `pool` identity is an opaque composite (`[subscription, start]`),
/// typed `json`. Whether source-projected metadata also appears is unspecified
/// (SPEC-ISSUES item 14), so only the deducible members are exposed here.
fn funding_row() -> RowType {
    RowType::keyless([
        ("source".to_owned(), ExprType::scalar(Type::Text)),
        ("pool".to_owned(), ExprType::scalar(Type::Json)),
        ("amount".to_owned(), ExprType::scalar(Type::Decimal)),
    ])
}
