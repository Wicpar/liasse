//! Diagnostics: source spans, severities, labeled annotations, and hints,
//! rendered rustc-style. Every user-facing error in the workspace is a
//! diagnostic from this crate; a diagnostic must let a new user understand
//! what went wrong, why, and (when a hint applies) how to fix it.
//!
//! # Shape
//!
//! - Register source texts in a [`SourceMap`], which hands back cheap, stable
//!   [`SourceId`]s.
//! - Point at bytes with [`ByteSpan`] (pure offset math) located in a source as
//!   a [`Span`].
//! - Build a [`Diagnostic`] through [`Diagnostic::error`] /
//!   [`Diagnostic::warning`] / [`Diagnostic::note`]. The builder demands a
//!   [primary label](DiagnosticHead::primary) before it can be built, so an
//!   unlabeled diagnostic does not typecheck.
//! - Accumulate many into [`Diagnostics`] and ask [`Diagnostics::has_errors`].
//! - Render either to a `String` with [`Diagnostic::render`] /
//!   [`Diagnostics::render`]; the rendering backend is a private detail.
//!
//! ```
//! use liasse_diag::{ByteSpan, Diagnostic, SourceMap, Span};
//!
//! let mut sources = SourceMap::new();
//! let file = sources.add_file("greet.liasse", "let x = 1 + true\n");
//! let span = Span::new(file, ByteSpan::at(12, 4));
//!
//! let diag = Diagnostic::error("mismatched types")
//!     .code("E0308")
//!     .primary(span, "expected integer, found `bool`")
//!     .help("write an integer literal here")
//!     .build();
//!
//! assert!(diag.is_error());
//! assert!(diag.render(&sources).contains("expected integer, found `bool`"));
//! ```

mod collection;
mod diagnostic;
mod render;
mod source;
mod span;

pub use collection::Diagnostics;
pub use diagnostic::{Code, Diagnostic, DiagnosticBuilder, DiagnosticHead, Label, Severity};
pub use render::RenderStyle;
pub use source::{Source, SourceId, SourceMap, SourceName, Span};
pub use span::ByteSpan;
