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
use liasse_value::{RefTarget, Type};

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

    /// The top-level collection declared under `name`, if any. A member naming a
    /// keyed `$types` shape (`companies: "company"`, §5.8) resolves to that shape's
    /// collection, so a top-level named collection is a first-class collection.
    pub(crate) fn top_collection(&self, name: &str) -> Option<&'m Collection> {
        self.collection_member(self.model.root(), name)
    }

    /// Resolve a [`Node::Named`] (§5.8) to the concrete node it adopts, following a
    /// bounded chain of `$types` aliases. A bare-alias cycle (`a: "b", b: "a"`) or an
    /// unknown name ends the walk at the last node, so resolution is total and cannot
    /// loop: a recursive `$types` shape recurses through a keyed collection, never a
    /// bare alias, so the load gate admits no such cycle, but the bound keeps this
    /// safe regardless. A non-named node is returned as-is.
    pub(crate) fn resolved_node(&self, node: &'m Node) -> &'m Node {
        let types = self.model.types();
        let mut current = node;
        // The alias chain is at most one hop per table entry; a longer walk is a cycle.
        for _ in 0..=types.len() {
            match current {
                Node::Named(name) => match types.get(name) {
                    Some(next) => current = next,
                    None => return current,
                },
                _ => return current,
            }
        }
        current
    }

    /// The keyed collection a node adopts once its `$types`/`$like` names are
    /// resolved (§5.8), or `None` when it is not a collection.
    pub(crate) fn resolved_collection(&self, node: &'m Node) -> Option<&'m Collection> {
        match self.resolved_node(node) {
            Node::Collection(collection) => Some(collection),
            _ => None,
        }
    }

    /// The identity key type of top-level collection `name` (§5.4, A.9): a scalar
    /// type for a single-field key, a [`Type::Composite`] for a composite key.
    /// Used to resolve a `$set` of `$ref` element to its target's key type, which
    /// the model leaves unresolved (a documented seam).
    pub(crate) fn collection_key_type(&self, name: &str) -> Option<Type> {
        let collection = self.top_collection(name)?;
        self.key_type(collection, 0).as_scalar().cloned()
    }

    /// The model collection at a declaration-name path, descending nested
    /// collections (§5.4): `["companies"]` top-level, `["companies", "offices"]`
    /// nested.
    pub(crate) fn collection_at_path(&self, path: &[String]) -> Option<&'m Collection> {
        let (head, rest) = path.split_first()?;
        let mut current = self.top_collection(head)?;
        for segment in rest {
            // §5.8: a nested member naming a `$types`/`$like` collection shape
            // (`subcompanies: "company"`, `children: { $like: "^" }`) resolves to that
            // collection, so a self-referential nested collection is descendable.
            let member = current.shape.member(segment)?;
            current = self.resolved_collection(&member.node)?;
        }
        Some(current)
    }

    fn collection_member(&self, shape: &'m Shape, name: &str) -> Option<&'m Collection> {
        self.resolved_collection(&shape.member(name)?.node)
    }

    fn shape_row(&self, shape: &Shape, key: Option<ExprType>, depth: u32) -> RowType {
        let mut fields: Vec<(String, ExprType)> = shape
            .members
            .iter()
            .map(|member| (member.name.as_str().to_owned(), self.node_at(&member.node, depth + 1)))
            .collect();
        // §15.6: a row declaring a meter with `$limits` exposes that meter's
        // accessor (`.<meter>.balance`, `.<meter>.pools`), mirroring the model
        // resolver so a view/return reading remaining capacity type-checks. A
        // same-named application field wins.
        for meter in &shape.meters {
            if fields.iter().all(|(name, _)| name != meter) {
                fields.push((meter.clone(), ExprType::Row(meter_accessor_row())));
            }
        }
        RowType::new(fields, key)
    }

    fn collection_row(&self, collection: &Collection, depth: u32) -> RowType {
        let key = self.key_type(collection, depth);
        let mut row = self.shape_row(&collection.shape, Some(key), depth);
        // §15.3/§15.6: a `$consumes` collection's rows expose `funding`, the fixed
        // admission allocation recorded per spend (§15.3). A same-named application
        // field wins.
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
                ExprType::scalar(Type::Ref(RefTarget::for_key(&reference.key_type)))
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
            let ty = match collection.shape.member(field.as_str()).map(|member| &member.node) {
                // A.8: a struct `$key` field's declared key type is the struct
                // itself (field-name ordered), matching the `Value::Struct` the
                // store carries — not the `json` the old `as_scalar` fallback
                // produced. Mirrors the model resolver so the two agree, and
                // flows through `collection_key_type` for a `$set` of `$ref`.
                Some(Node::Struct(shape)) => shape.key_struct_type(),
                Some(node) => {
                    self.node_at(node, depth + 1).as_scalar().cloned().unwrap_or(Type::Json)
                }
                None => Type::Json,
            };
            components.push((field.as_str().to_owned(), ty));
        }
        match components.as_slice() {
            [(_, ty)] => ExprType::scalar(ty.clone()),
            _ => ExprType::scalar(Type::Composite(components)),
        }
    }
}

/// The row shape of a §15.6 meter accessor (`.<meter>`): `balance` is the
/// context-free current remaining capacity (a non-negative `decimal`), `pools`
/// the eligible pool view. Mirrors the model resolver so the two agree.
fn meter_accessor_row() -> RowType {
    RowType::keyless([
        ("balance".to_owned(), ExprType::scalar(Type::Decimal)),
        ("pools".to_owned(), ExprType::View(RowType::keyless(std::iter::empty()))),
    ])
}

/// The row shape of a spend's `funding` view (§15.3): source label, opaque pool
/// identity (`json`), and allocated `decimal` amount.
fn funding_row() -> RowType {
    RowType::keyless([
        ("source".to_owned(), ExprType::scalar(Type::Text)),
        ("pool".to_owned(), ExprType::scalar(Type::Json)),
        ("amount".to_owned(), ExprType::scalar(Type::Decimal)),
    ])
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
