//! The diagnostic builder and the accumulate-then-report collection.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, Severity, SourceMap, Span};

fn span(sources: &mut SourceMap) -> Span {
    let id = sources.add_file("m.liasse", "0123456789");
    Span::new(id, ByteSpan::at(2, 3))
}

#[test]
fn builder_carries_every_part_through_to_the_diagnostic() {
    let mut sources = SourceMap::new();
    let s = span(&mut sources);
    let extra = Span::new(s.source(), ByteSpan::at(6, 2));

    let diag = Diagnostic::error("bad thing")
        .code("L0001")
        .primary(s, "here")
        .secondary(extra, "and here")
        .help("do it differently")
        .help("or this way")
        .build();

    assert_eq!(diag.severity(), Severity::Error);
    assert!(diag.is_error());
    assert_eq!(diag.message(), "bad thing");
    assert_eq!(diag.code().map(|c| c.as_str()), Some("L0001"));
    assert_eq!(diag.primary().span(), s);
    assert_eq!(diag.primary().message(), "here");
    assert_eq!(diag.secondaries().len(), 1);
    assert_eq!(diag.secondaries().first().map(|l| l.message()), Some("and here"));
    assert_eq!(diag.helps(), &["do it differently".to_owned(), "or this way".to_owned()]);
}

#[test]
fn severity_constructors_set_the_right_severity_and_error_flag() {
    let mut sources = SourceMap::new();
    let s = span(&mut sources);

    let warn = Diagnostic::warning("careful").primary(s, "x").build();
    assert_eq!(warn.severity(), Severity::Warning);
    assert!(!warn.is_error());

    let note = Diagnostic::note("aside").primary(s, "x").build();
    assert_eq!(note.severity(), Severity::Note);
    assert!(!note.is_error());
}

#[test]
fn an_empty_code_slug_is_dropped_rather_than_rendered_blank() {
    let mut sources = SourceMap::new();
    let s = span(&mut sources);
    let diag = Diagnostic::error("no code").code("").primary(s, "x").build();
    assert!(diag.code().is_none());
}

#[test]
fn collection_reports_errors_only_when_an_error_was_pushed() {
    let mut sources = SourceMap::new();
    let s = span(&mut sources);

    let mut diags = Diagnostics::new();
    assert!(diags.is_empty());
    assert!(!diags.has_errors());

    diags.push(Diagnostic::warning("w").primary(s, "x").build());
    assert!(!diags.has_errors());
    assert_eq!(diags.error_count(), 0);

    diags.push(Diagnostic::error("e1").primary(s, "x").build());
    diags.push(Diagnostic::error("e2").primary(s, "x").build());
    assert!(diags.has_errors());
    assert_eq!(diags.error_count(), 2);
    assert_eq!(diags.len(), 3);
}

#[test]
fn collection_preserves_insertion_order() {
    let mut sources = SourceMap::new();
    let s = span(&mut sources);
    let diags: Diagnostics = [
        Diagnostic::error("first").primary(s, "x").build(),
        Diagnostic::warning("second").primary(s, "x").build(),
    ]
    .into_iter()
    .collect();

    let messages: Vec<&str> = diags.iter().map(Diagnostic::message).collect();
    assert_eq!(messages, ["first", "second"]);
}
