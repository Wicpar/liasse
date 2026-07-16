//! Syntax: pest grammars and parsers for the Liasse authoring form, the
//! strict-JSON built definition, and the Liasse expression/path grammar
//! (SPEC.md Annex C). Produces spanned syntax trees only — no semantics.
//!
//! # Two entry points
//!
//! - [`parse_document`] parses the authoring/built *document* form into a
//!   [`SpannedDocument`]: a tree of raw scalars, arrays, and objects, each node
//!   spanned. It interprets no member — whether a string is an expression, a
//!   package name, or a type name is the model layer's concern.
//! - [`parse_expression`] parses one Liasse *expression* or `$mut` statement
//!   into a [`SpannedExpression`]: a spanned AST covering roots, field access,
//!   selectors, projections, view combinators, CEL operators, and mutation
//!   statement forms. It resolves no name and checks no type.
//!
//! Both take the [`SourceId`](liasse_diag::SourceId) under which the caller has
//! registered the text in a [`SourceMap`](liasse_diag::SourceMap), and return
//! [`Diagnostics`](liasse_diag::Diagnostics) on failure — every parse error is
//! a rustc-style diagnostic pointing at the offending span, never a raw pest
//! error string.
//!
//! # Authoring-form scope
//!
//! The document parser accepts the Hjson conveniences the spec's own examples
//! use: `//` / `#` / `/* */` comments, unquoted member names, optional and
//! trailing commas, and `'''...'''` multiline strings. It intentionally does
//! **not** accept Hjson's quoteless *string values* (a bare word as a value):
//! the spec never uses them and they make the value grammar ambiguous, so a
//! string value must be quoted or a `'''` block. Strict `liasse.json` is a
//! subset of what this parser accepts, so the same parser reads both forms.
//!
//! # Documented ambiguity choices
//!
//! - **View-combinator precedence (SPEC-ISSUES item 25).** The spec pins no
//!   relative precedence, associativity, or grouping for the `|` and `&`
//!   combinators. Rather than silently nest them, the parser records a mixed
//!   chain flat in [`ExprKind::Combination`](expr::ast::ExprKind::Combination),
//!   leaving the decision to a later phase.
//! - **Bare `=` / empty expression (SPEC-ISSUES item 25).** An empty
//!   expression source is rejected with a diagnostic rather than parsed into a
//!   silent empty node; the caller (model layer) decides what a `$data` value
//!   of `"="` means.

mod document;
mod error;
mod expr;
mod scan;
mod text;

/// Saturating conversion of a `pest` byte offset into the `u32` span coordinate
/// `liasse_diag` uses. This is the crate's single offset-narrowing point: every
/// parser and the structural scanner route through it. Offsets past `u32::MAX`
/// mean a >4 GiB source, so clamping to the maximum (rather than wrapping)
/// keeps the resulting span in-bounds without a panic.
pub(crate) fn clamp(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

pub use document::ast::{
    DocMember, DocName, DocValue, DocValueKind, SpannedDocument,
};
pub use document::parse_document;
pub use expr::ast::{
    Arg, BinaryOp, BlockMember, BlockMemberKind, CombinatorOp, Expr, ExprKind, Ident, Selector,
    SpannedExpression, Stmt, StmtKind, UnaryOp,
};
pub use expr::parse_expression;
