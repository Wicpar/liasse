//! The diagnostic model: a severity, an optional code, exactly one primary
//! labeled span, any number of secondary labeled spans, and help lines.
//!
//! A [`Diagnostic`] is built through two builder types so that an unlabeled
//! diagnostic cannot be constructed. [`DiagnosticHead`] carries only what is
//! known before a span is attached and exposes no `build`; the primary label is
//! the sole path from a head to a buildable [`DiagnosticBuilder`].

use crate::source::Span;

/// The seriousness of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// Blocks the operation; the surrounding request fails.
    Error,
    /// The operation proceeds, but the author should look.
    Warning,
    /// Supplementary context attached to its own line.
    Note,
}

impl Severity {
    /// Whether this severity blocks the operation.
    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(self, Self::Error)
    }
}

/// A short machine-facing slug identifying a diagnostic class, rendered like
/// rustc's `error[E0308]`.
///
/// A code never carries user-facing prose; that is the diagnostic message. The
/// slug is validated to be non-empty so an empty `[]` tag can never render.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Code(String);

impl Code {
    /// Builds a code, or `None` for an empty slug.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Option<Self> {
        let slug = slug.into();
        if slug.is_empty() {
            None
        } else {
            Some(Self(slug))
        }
    }

    /// The slug text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A span paired with the message that explains what is at that span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    span: Span,
    message: String,
}

impl Label {
    /// The located span this label points at.
    #[must_use]
    pub fn span(&self) -> Span {
        self.span
    }

    /// The label text drawn beside the span's underline.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A fully-formed, immutable diagnostic. Every diagnostic has exactly one
/// primary label; there is no way to construct one without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    severity: Severity,
    code: Option<Code>,
    message: String,
    primary: Label,
    secondary: Vec<Label>,
    helps: Vec<String>,
}

impl Diagnostic {
    /// Begins an error diagnostic with its headline message.
    #[must_use]
    pub fn error(message: impl Into<String>) -> DiagnosticHead {
        DiagnosticHead::new(Severity::Error, message.into())
    }

    /// Begins a warning diagnostic with its headline message.
    #[must_use]
    pub fn warning(message: impl Into<String>) -> DiagnosticHead {
        DiagnosticHead::new(Severity::Warning, message.into())
    }

    /// Begins a note diagnostic with its headline message.
    #[must_use]
    pub fn note(message: impl Into<String>) -> DiagnosticHead {
        DiagnosticHead::new(Severity::Note, message.into())
    }

    /// The diagnostic's severity.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// The diagnostic's code, if any.
    #[must_use]
    pub fn code(&self) -> Option<&Code> {
        self.code.as_ref()
    }

    /// The headline message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The primary label — the one carat-underlined span the message is about.
    #[must_use]
    pub fn primary(&self) -> &Label {
        &self.primary
    }

    /// The secondary labels, in the order they were added.
    #[must_use]
    pub fn secondaries(&self) -> &[Label] {
        &self.secondary
    }

    /// The help lines, in the order they were added.
    #[must_use]
    pub fn helps(&self) -> &[String] {
        &self.helps
    }

    /// Whether this diagnostic blocks the operation.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity.is_error()
    }
}

/// A diagnostic under construction that has a severity, message, and maybe a
/// code, but not yet a primary label. It cannot be turned into a
/// [`Diagnostic`]; only [`DiagnosticHead::primary`] can, which is what makes an
/// unlabeled diagnostic unrepresentable.
#[derive(Debug, Clone)]
pub struct DiagnosticHead {
    severity: Severity,
    code: Option<Code>,
    message: String,
}

impl DiagnosticHead {
    fn new(severity: Severity, message: String) -> Self {
        Self {
            severity,
            code: None,
            message,
        }
    }

    /// Attaches a code slug. A later call replaces an earlier one; an empty
    /// slug is dropped ([`Code::new`] rejects it).
    #[must_use]
    pub fn code(mut self, slug: impl Into<String>) -> Self {
        self.code = Code::new(slug);
        self
    }

    /// Attaches the primary label, unlocking the full builder.
    #[must_use]
    pub fn primary(self, span: Span, label: impl Into<String>) -> DiagnosticBuilder {
        DiagnosticBuilder {
            severity: self.severity,
            code: self.code,
            message: self.message,
            primary: Label {
                span,
                message: label.into(),
            },
            secondary: Vec::new(),
            helps: Vec::new(),
        }
    }
}

/// A diagnostic under construction that already has its primary label, so it
/// can accrue secondary labels and help lines and be built.
#[derive(Debug, Clone)]
pub struct DiagnosticBuilder {
    severity: Severity,
    code: Option<Code>,
    message: String,
    primary: Label,
    secondary: Vec<Label>,
    helps: Vec<String>,
}

impl DiagnosticBuilder {
    /// Attaches or replaces the code slug.
    #[must_use]
    pub fn code(mut self, slug: impl Into<String>) -> Self {
        self.code = Code::new(slug);
        self
    }

    /// Adds a secondary label pointing at supporting context.
    #[must_use]
    pub fn secondary(mut self, span: Span, label: impl Into<String>) -> Self {
        self.secondary.push(Label {
            span,
            message: label.into(),
        });
        self
    }

    /// Adds a help/hint line telling the user how to fix the problem.
    #[must_use]
    pub fn help(mut self, hint: impl Into<String>) -> Self {
        self.helps.push(hint.into());
        self
    }

    /// Finishes construction, yielding an immutable [`Diagnostic`].
    #[must_use]
    pub fn build(self) -> Diagnostic {
        Diagnostic {
            severity: self.severity,
            code: self.code,
            message: self.message,
            primary: self.primary,
            secondary: self.secondary,
            helps: self.helps,
        }
    }
}
