//! The validated state-model tree (SPEC.md §5).
//!
//! Every node here is proof of its own static validity: a [`Collection`] cannot
//! exist without a key that names declared, key-eligible fields; a
//! [`ScalarField`] carries a canonical [`Type`]; a [`Reference`] names a target
//! that resolves. The [`crate::build`] module is the only producer, turning a
//! spanned `$model` object into this tree while accumulating every rejection.
//!
//! The tree also answers "what is the type of this node" for expression
//! checking: [`Node::expr_type`] and [`Shape::row_type`] project the tree onto
//! the [`liasse_expr`] type model, resolving named shapes through a
//! [`Resolver`](crate::resolve::Resolver) with a recursion guard.

use liasse_diag::ByteSpan;
use liasse_value::Type;

use crate::names::DeclName;

/// The source text and span of an authored expression, kept so it can be parsed
/// and type-checked in its declaration position.
#[derive(Debug, Clone)]
pub struct ExprSource {
    /// The bare expression text (no leading `=`).
    pub text: String,
    /// The bytes the expression string covers in the definition source.
    pub span: ByteSpan,
}

/// A reusable diagnostic message coupled to a condition expression (§8.8).
#[derive(Debug, Clone)]
pub struct Check {
    /// The boolean condition expression.
    pub condition: ExprSource,
    /// The author-supplied message, if any (else a generated diagnostic).
    pub message: Option<String>,
}

/// A scalar field or computed value (§5.1, §5.2).
#[derive(Debug, Clone)]
pub struct ScalarField {
    /// The field type. For a computed value this is the checked result type of
    /// its expression.
    pub ty: Type,
    /// The computing expression, when this is a read-only computed value.
    pub computed: Option<ExprSource>,
    /// The insertion default, when declared.
    pub default: Option<ExprSource>,
    /// The `$normalize` expression, when declared.
    pub normalize: Option<ExprSource>,
    /// Field-level `$check`s.
    pub checks: Vec<Check>,
    /// Whether `$unique: true` added a single-field candidate key.
    pub unique: bool,
    /// The bytes of the member value.
    pub span: ByteSpan,
}

impl ScalarField {
    /// Whether the field is writable (has no computing expression).
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.computed.is_none()
    }
}

/// A `$set` member (§5.5): unique payload-free membership of an element type.
#[derive(Debug, Clone)]
pub struct SetField {
    /// The element type.
    pub element: Type,
    /// The bytes of the declaration.
    pub span: ByteSpan,
}

/// A `$ref` field (§5.6): a typed target key that must resolve.
#[derive(Debug, Clone)]
pub struct Reference {
    /// The absolute target collection path (`/accounts`, `/companies`).
    pub target: String,
    /// The resolved target key type.
    pub key_type: Type,
    /// Whether the ref is optional.
    pub optional: bool,
    /// The raw `$on_delete` policy source, if declared (kept for a later pass).
    pub on_delete: Option<ExprSource>,
    /// The bytes of the declaration.
    pub span: ByteSpan,
}

/// A `$view` declaration (§7.1).
#[derive(Debug, Clone)]
pub struct ViewDecl {
    /// The view/source expression source.
    pub expr: ExprSource,
    /// The checked row shape of the view result.
    pub row: liasse_expr::RowType,
}

/// One shape member: its name, its declaration span, and its node.
#[derive(Debug, Clone)]
pub struct Member {
    /// The declared member name.
    pub name: DeclName,
    /// The bytes of the whole member.
    pub span: ByteSpan,
    /// The member's node.
    pub node: Node,
}

/// A state-tree node.
#[derive(Debug, Clone)]
pub enum Node {
    /// A scalar field or computed value.
    Scalar(ScalarField),
    /// A static struct (§5.3).
    Struct(Shape),
    /// A keyed collection (§5.4).
    Collection(Box<Collection>),
    /// A set (§5.5).
    Set(SetField),
    /// A computed view (§7).
    View(ViewDecl),
    /// A checked reference (§5.6).
    Reference(Reference),
    /// A reference to a reusable `$types` shape or a `$like` positional shape
    /// (§5.8), resolved lazily to avoid infinite expansion of recursive shapes.
    Named(String),
}

/// A struct or collection body: named members plus shape-level checks.
///
/// Mutations (§8) and surfaces (§10) are *not* held here: they are collected
/// flat on the [`Model`](crate::Model) with their receiver path, which keeps the
/// data tree free of behaviour and avoids re-borrowing a node while its
/// mutations are validated.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    /// The members in declaration order.
    pub members: Vec<Member>,
    /// Struct/row-level `$check`s (§5.10).
    pub checks: Vec<Check>,
}

impl Shape {
    /// The member named `name`, if present.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name.as_str() == name)
    }
}

/// A keyed collection (§5.4): a shape plus a primary key and candidate keys.
#[derive(Debug, Clone)]
pub struct Collection {
    /// The primary-key field names, in `$key` order.
    pub key: Vec<DeclName>,
    /// The bytes of the `$key` declaration.
    pub key_span: ByteSpan,
    /// Additional candidate keys (§5.7); each is one composite key's field list.
    pub unique: Vec<Vec<DeclName>>,
    /// Whether the collection declares `$consumes` (§15.1): a spending
    /// collection whose rows expose a `funding` accessor recording each admitted
    /// spend's allocation (§15.3, §15.6).
    pub consumes: bool,
    /// The collection body.
    pub shape: Shape,
}
