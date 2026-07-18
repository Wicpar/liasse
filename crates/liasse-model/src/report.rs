//! Diagnostic emission for the model builder.
//!
//! Validation is accumulate-then-report (AGENTS.md parsing-language rule,
//! SPEC.md multi-error requirement): a [`Reporter`] threads one
//! [`Diagnostics`] and the [`SourceId`] of the definition text through every
//! phase so a build gathers *all* static rejections before failing, rather
//! than stopping at the first. Each rejection names what is wrong, points at
//! the offending span, and carries a fix hint when one exists.
//!
//! Diagnostics are built with the [`liasse_diag`] builder directly at the call
//! site (it already makes an unlabeled diagnostic unrepresentable); the
//! reporter only supplies span location and the shared sink.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, SourceId, Span};

/// Stable machine-facing codes for model-layer rejections. Grouped by the spec
/// chapter whose rule is violated so a reader can trace a code to its clause.
pub mod code {
    /// A declaration name breaks the §2.5 name grammar.
    pub const NAME_GRAMMAR: &str = "M-NAME";
    /// A reserved `$`-prefixed member is not a known Liasse declaration (§2.5).
    pub const RESERVED_MEMBER: &str = "M-RESERVED";
    /// An object bears two mutually-exclusive node-kind shape markers (Annex C.2,
    /// §5.3-§5.9): its node kind is not uniquely determined (SPEC-ISSUES 25).
    pub const SHAPE: &str = "M-SHAPE";
    /// An unknown member in a closed declaration object (§2.5, Annex C).
    pub const UNKNOWN_MEMBER: &str = "M-UNKNOWN";
    /// A required member is missing (§4.1, Annex C.1).
    pub const MISSING_MEMBER: &str = "M-MISSING";
    /// The package header shape is wrong (§4, Annex C.1, Annex E).
    pub const HEADER: &str = "M-HEADER";
    /// A `$liasse` language generation the runtime does not support (§4.1).
    pub const LANGUAGE: &str = "M-LIASSE";
    /// A malformed type expression (Annex A.2).
    pub const TYPE: &str = "M-TYPE";
    /// A key / uniqueness declaration is invalid (§5.4, §5.7, A.8).
    pub const KEY: &str = "M-KEY";
    /// A ref target does not resolve (§5.6).
    pub const REF: &str = "M-REF";
    /// An enum declaration is invalid (§5.9).
    pub const ENUM: &str = "M-ENUM";
    /// A default / computed dependency graph is cyclic (§5.1).
    pub const CYCLE: &str = "M-CYCLE";
    /// An expression fails to type-check in its declared position (§6, §8).
    pub const EXPR: &str = "M-EXPR";
    /// A mutation program is statically invalid (§8).
    pub const MUTATION: &str = "M-MUT";
    /// A surface / role exposes something that does not exist (§10).
    pub const SURFACE: &str = "M-SURFACE";
    /// A `$data` seed value does not conform to the model type (§5, §9).
    pub const SEED: &str = "M-SEED";
    /// An `$auth` authenticator declaration is statically invalid (§11).
    pub const AUTH: &str = "M-AUTH";
    /// A `$bucket` lifecycle declaration is statically invalid (§14).
    pub const BUCKET: &str = "M-BUCKET";
    /// A `$limits`/`$consumes` meter declaration is statically invalid (§15).
    pub const METER: &str = "M-METER";
    /// A `$keyring` declaration is statically invalid (§17).
    pub const KEYRING: &str = "M-KEYRING";
    /// A blob accepted-type or `$blob_storage` placement is invalid (§18).
    pub const BLOB: &str = "M-BLOB";
    /// A `$modules`/`$use`/`$deps`/`$expose`/`$config` module composition
    /// declaration is statically invalid (§13).
    pub const MODULE: &str = "M-MODULE";
    /// A `$history` policy declaration is malformed (§19).
    pub const HISTORY: &str = "M-HISTORY";
    /// A `$migrations`/`$from`/`$as`/`$back` declaration is invalid (§20).
    pub const MIGRATION: &str = "M-MIGRATE";
    /// A `$ref` `$on_delete` policy is missing or invalid (§21).
    pub const DELETE: &str = "M-DELETE";
    /// A `$sort` declaration is malformed (§7.3).
    pub const SORT: &str = "M-SORT";
}

/// Accumulates every static rejection against one definition source.
pub(crate) struct Reporter<'a> {
    source: SourceId,
    diags: &'a mut Diagnostics,
}

impl<'a> Reporter<'a> {
    /// A reporter writing into `diags`, locating spans in `source`.
    pub(crate) fn new(source: SourceId, diags: &'a mut Diagnostics) -> Self {
        Self { source, diags }
    }

    /// Locate a byte span in the definition source.
    pub(crate) fn locate(&self, span: ByteSpan) -> Span {
        Span::new(self.source, span)
    }

    /// Push an already-built diagnostic.
    pub(crate) fn emit(&mut self, diag: Diagnostic) {
        self.diags.push(diag);
    }

    /// Push several diagnostics (e.g. a nested expression check's bundle).
    pub(crate) fn emit_all(&mut self, diags: Diagnostics) {
        self.diags.extend(diags);
    }

    /// Reject with a code, a headline message, and the span it is about.
    pub(crate) fn reject(&mut self, span: ByteSpan, code: &str, message: impl Into<String>) {
        let located = self.locate(span);
        self.emit(
            Diagnostic::error(message.into())
                .code(code)
                .primary(located, "here")
                .build(),
        );
    }

    /// Reject with a code, message, the span, and a fix hint.
    pub(crate) fn reject_hint(
        &mut self,
        span: ByteSpan,
        code: &str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) {
        let located = self.locate(span);
        self.emit(
            Diagnostic::error(message.into())
                .code(code)
                .primary(located, "here")
                .help(hint.into())
                .build(),
        );
    }
}
