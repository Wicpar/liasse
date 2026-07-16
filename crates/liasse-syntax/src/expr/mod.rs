//! The Liasse expression / path / selector / projection / mutation-statement
//! layer (SPEC.md §6, §7, Annex C.3-C.9).

pub mod ast;
mod lower;
mod parse;

pub use parse::parse_expression;
