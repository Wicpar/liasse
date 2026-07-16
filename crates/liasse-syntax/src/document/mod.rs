//! The Liasse authoring document form and strict-JSON built form
//! (SPEC.md §2.5, §4.2, Annex C.1).

pub mod ast;
mod parse;

pub use parse::parse_document;
