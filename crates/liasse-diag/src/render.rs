//! Rendering diagnostics to rustc-style annotated snippets.
//!
//! The backend is [`annotate-snippets`](https://docs.rs/annotate-snippets), the
//! renderer rustc itself extracted; it produces the caret-underlined,
//! `-->`-headed, `= help:`-footed layout users already know. It was chosen over
//! `ariadne` and `codespan-reporting` because it targets rustc's exact output
//! rather than a house style, and because it takes plain `&str` sources and byte
//! ranges directly, so no `Files`/`Cache` adapter trait is needed. The backend
//! is a private detail: callers see only the semantic types and a
//! render-to-`String`.

use crate::collection::Diagnostics;
use crate::diagnostic::{Code, Diagnostic, Label, Severity};
use crate::source::SourceMap;
use annotate_snippets::{AnnotationKind, Group, Level, Renderer, Snippet};

/// Whether rendered output carries ANSI color/style escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStyle {
    /// No escapes — deterministic text, suitable for logs, files, and tests.
    Plain,
    /// ANSI-styled, suitable for a color terminal.
    Ansi,
}

impl RenderStyle {
    fn renderer(self) -> Renderer {
        match self {
            Self::Plain => Renderer::plain(),
            Self::Ansi => Renderer::styled(),
        }
    }
}

impl Severity {
    fn level(self) -> Level<'static> {
        match self {
            Self::Error => Level::ERROR,
            Self::Warning => Level::WARNING,
            Self::Note => Level::NOTE,
        }
    }
}

/// One source's worth of annotations, gathered so all labels sharing a source
/// render inside a single snippet (as rustc groups them).
struct SnippetPlan<'a> {
    source: crate::source::SourceId,
    annotations: Vec<(AnnotationKind, &'a Label)>,
}

impl<'a> SnippetPlan<'a> {
    fn snippet(&self, sources: &'a SourceMap) -> Option<Snippet<'a, annotate_snippets::Annotation<'a>>> {
        let source = sources.get(self.source)?;
        let mut snippet = Snippet::source(source.text())
            .path(source.name().display())
            .line_start(1)
            .fold(true);
        for (kind, label) in &self.annotations {
            // Snap to char boundaries within the text so a stale, out-of-range,
            // or mid-char span can never make the backend slice off a boundary
            // and panic; a diagnostic must never panic.
            let range = source.char_boundary_range(label.span().bytes());
            snippet = snippet.annotation(kind.span(range).label(label.message()));
        }
        Some(snippet)
    }
}

impl Diagnostic {
    /// Renders this diagnostic to a plain (unstyled) rustc-style string.
    #[must_use]
    pub fn render(&self, sources: &SourceMap) -> String {
        self.render_with(sources, RenderStyle::Plain)
    }

    /// Renders this diagnostic in the requested [`RenderStyle`].
    #[must_use]
    pub fn render_with(&self, sources: &SourceMap, style: RenderStyle) -> String {
        style.renderer().render(&[self.group(sources)])
    }

    /// Assembles the backend group. Private: the backend type never escapes.
    fn group<'a>(&'a self, sources: &'a SourceMap) -> Group<'a> {
        let title = self.severity().level().primary_title(self.message());
        let title = match self.code() {
            Some(code) => title.id(Code::as_str(code)),
            None => title,
        };
        let mut group = Group::with_title(title);
        for plan in self.plans() {
            if let Some(snippet) = plan.snippet(sources) {
                group = group.element(snippet);
            }
        }
        for hint in self.helps() {
            group = group.element(Level::HELP.message(hint.as_str()));
        }
        group
    }

    /// Buckets labels by source, primary's source first, each source keeping the
    /// order labels were added — one bucket becomes one snippet.
    fn plans(&self) -> Vec<SnippetPlan<'_>> {
        let labeled = core::iter::once((AnnotationKind::Primary, self.primary()))
            .chain(
                self.secondaries()
                    .iter()
                    .map(|label| (AnnotationKind::Context, label)),
            );
        let mut plans: Vec<SnippetPlan<'_>> = Vec::new();
        for (kind, label) in labeled {
            let source = label.span().source();
            match plans.iter_mut().find(|plan| plan.source == source) {
                Some(plan) => plan.annotations.push((kind, label)),
                None => plans.push(SnippetPlan {
                    source,
                    annotations: vec![(kind, label)],
                }),
            }
        }
        plans
    }
}

impl Diagnostics {
    /// Renders every diagnostic, in order, to one plain rustc-style string.
    #[must_use]
    pub fn render(&self, sources: &SourceMap) -> String {
        self.render_with(sources, RenderStyle::Plain)
    }

    /// Renders every diagnostic, in order, in the requested [`RenderStyle`].
    #[must_use]
    pub fn render_with(&self, sources: &SourceMap, style: RenderStyle) -> String {
        let groups: Vec<Group<'_>> = self.iter().map(|diag| diag.group(sources)).collect();
        style.renderer().render(&groups)
    }
}
