//! Classifying a declared view's result shape (§7.1, §12.2).
//!
//! §12.2 delivers a single-row or scalar view result as one JSON object, and a
//! row-stream result as a JSON array. The distinction is the view expression's
//! static [`ExprType`]: a root/struct projection (`. { t }`, `.invoice { … }`)
//! or an aggregate (`= size(.docs)`) is singular, whereas a collection source or
//! selection (`.docs`, `.docs[:x | …]`) is a stream. The runtime materializes
//! every result to a flat row list, dropping that distinction, so the adapter
//! recovers it by type-checking each top-level view against the model root —
//! reproducing over the public [`Model`] surface the same projection the runtime
//! schema keeps private (`liasse-runtime`'s `schema.rs`, §5/§8.2).
//!
//! A view whose expression does not type-check under this reconstruction (a
//! request-scoped or import-bound read the root scope does not carry) is left
//! unclassified, so its result renders as an array exactly as before — no
//! passing collection view regresses.

use std::collections::BTreeSet;

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType, RowType, Scope};
use liasse_model::{Collection, Model, Node, Shape};
use liasse_syntax::{parse_expression, Expr, ExprKind, StmtKind};
use liasse_value::{RefTarget, Type};

/// The set of top-level view names whose result is singular (a single row or a
/// scalar), rendered as a JSON object rather than an array.
#[derive(Debug, Clone, Default)]
pub struct ViewShapes {
    singular: BTreeSet<String>,
}

impl ViewShapes {
    /// Classify every top-level declared view of `model`. A view expression that
    /// resolves to a single row or a scalar is recorded as singular; a stream or
    /// an unclassifiable expression is left out (rendered as an array).
    #[must_use]
    pub fn derive(model: &Model) -> Self {
        let schema = Schema { model };
        let scope = RootScope { root: ExprType::Row(schema.root_row()) };
        let mut singular = BTreeSet::new();
        for member in &model.root().members {
            if let Node::View(view) = &member.node
                && scope.classify_singular(&view.expr.text)
            {
                singular.insert(member.name.as_str().to_owned());
            }
        }
        Self { singular }
    }

    /// Whether the view named `name` renders as a single object (§12.2).
    #[must_use]
    pub fn is_singular(&self, name: &str) -> bool {
        self.singular.contains(name)
    }
}

/// A scope that resolves `.` and `/` to the package-root row type and every
/// other root/binding to `None` — the scope a top-level view is checked in.
struct RootScope {
    root: ExprType,
}

impl RootScope {
    /// Type-check `text` against this scope, returning `true` when its result is
    /// delivered as one JSON object (§12.2): a scalar aggregate, or a single-row
    /// *root or struct projection*.
    ///
    /// The static type alone is not enough: §6.3 types a single-key collection
    /// selection (`.coll[{k}] { … }`) as a one-row `Row`, yet the runtime
    /// materializes a *selection* as a collection cell delivered as an array (a
    /// corpus expectation). So a `Row` result also requires a projection spine
    /// that reaches the root or a struct through field access only — no
    /// collection selector — matching which cell the runtime produces.
    fn classify_singular(&self, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let mut sources = SourceMap::new();
        let source = sources.add_label("view-shape", text.to_owned());
        let Ok(parsed) = parse_expression(source, text) else {
            return false;
        };
        let (StmtKind::Bare(expr) | StmtKind::Return(expr)) = &parsed.statement().kind else {
            return false;
        };
        match check_statement(self, source, &parsed) {
            Ok(typed) => match typed.ty() {
                ExprType::Scalar(_) => true,
                ExprType::Row(_) => selectionless_spine(expr),
                ExprType::View(_) => false,
            },
            Err(_) => false,
        }
    }
}

impl Scope for RootScope {
    fn current(&self) -> Option<ExprType> {
        Some(self.root.clone())
    }

    fn parent(&self, _depth: u32) -> Option<ExprType> {
        None
    }

    fn root(&self) -> Option<ExprType> {
        Some(self.root.clone())
    }

    fn param(&self, _name: &str) -> Option<ExprType> {
        None
    }

    fn structural(&self, _name: &str) -> Option<ExprType> {
        None
    }

    fn import(&self, _name: &str) -> Option<ExprType> {
        None
    }

    fn binding(&self, _name: &str) -> Option<ExprType> {
        None
    }
}

/// Whether the projection *spine* of `expr` reaches the root or a struct through
/// field access alone — no collection selector. This is the base chain of the
/// outermost projection, following `Field`/`SameName`/`Block`/`Call` down to a
/// `Current`/`Root`/`Parent` atom; a `Select` in the spine is a collection
/// selection whose result the runtime delivers as an array, not an object. A
/// projected *field value* deeper in the block is not part of the spine, so a
/// root projection carrying a nested collection field (`. { rows: .coll }`) is
/// still a singular object.
fn selectionless_spine(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Current | ExprKind::Root | ExprKind::Parent(_) => true,
        ExprKind::Name(_) | ExprKind::Import(_) => true,
        ExprKind::Field { base, .. } | ExprKind::SameName { base, .. } => selectionless_spine(base),
        ExprKind::Block { base, .. } => selectionless_spine(base),
        ExprKind::Call { callee, .. } => selectionless_spine(callee),
        _ => false,
    }
}

/// Depth beyond which recursive `$types` expansion yields an opaque `json`,
/// matching the model/runtime resolver cap.
const MAX_DEPTH: u32 = 32;

/// A typed projection over the public [`Model`] surface: it folds each state-tree
/// node to its [`ExprType`] node-for-node with the runtime schema (§5, §8.2), so
/// a view expression can be type-checked against the same root shape the runtime
/// evaluates it in.
struct Schema<'m> {
    model: &'m Model,
}

impl Schema<'_> {
    /// The keyless row type of the package root `/` (§8.2).
    fn root_row(&self) -> RowType {
        self.shape_row(self.model.root(), None, 0)
    }

    /// Project a shape's members to a [`RowType`], folding each node to its
    /// [`ExprType`].
    fn shape_row(&self, shape: &Shape, key: Option<ExprType>, depth: u32) -> RowType {
        let fields = shape
            .members
            .iter()
            .map(|member| (member.name.as_str().to_owned(), self.node_at(&member.node, depth + 1)));
        RowType::new(fields, key)
    }

    /// The single-row type of a collection: its key identity plus its body shape.
    fn collection_row(&self, collection: &Collection, depth: u32) -> RowType {
        let key = self.key_type(collection, depth);
        self.shape_row(&collection.shape, Some(key), depth)
    }

    /// The [`ExprType`] of one state-tree node: scalars and sets stay scalar, a
    /// struct is a keyless row, a collection or view is a stream.
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

    /// The identity type of a collection's primary key (§5.4, A.9): the single
    /// key field's scalar type, or the composite key type over a composite key.
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
