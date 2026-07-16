//! Rendered output: the pointed-at line, caret positioning, labels, hints, and
//! multi-span / multi-diagnostic layout. Column expectations are derived by
//! counting characters in the source line, not read back from the renderer.

use liasse_diag::{ByteSpan, Diagnostic, Diagnostics, RenderStyle, SourceMap, Span};

#[test]
fn single_primary_span_points_at_the_line_with_a_caret_and_hint() {
    let mut sources = SourceMap::new();
    // "let x = 1 + true": `true` starts at byte 12 (column 13, 1-based) and is
    // 4 bytes long.
    let src = "let x = 1 + true\n";
    let id = sources.add_file("greet.liasse", src);
    let diag = Diagnostic::error("mismatched types")
        .code("E0308")
        .primary(Span::new(id, ByteSpan::at(12, 4)), "expected integer, found `bool`")
        .help("write an integer literal here")
        .build();

    let out = diag.render(&sources);

    // Title with code, rustc-style.
    assert!(out.contains("error[E0308]: mismatched types"), "{out}");
    // Locator points at line 1, column 13.
    assert!(out.contains("--> greet.liasse:1:13"), "{out}");
    // The offending source line is quoted verbatim.
    assert!(out.contains("1 | let x = 1 + true"), "{out}");
    // Four carets, one per byte of `true`, carrying the label.
    assert!(out.contains("^^^^ expected integer, found `bool`"), "{out}");
    // The hint renders as a rustc `= help:` footer.
    assert!(out.contains("= help: write an integer literal here"), "{out}");
}

#[test]
fn secondary_span_in_same_source_underlines_with_context_marks() {
    let mut sources = SourceMap::new();
    let src = "let x = 1 + true\n";
    let id = sources.add_file("greet.liasse", src);
    let diag = Diagnostic::error("mismatched types")
        .primary(Span::new(id, ByteSpan::at(12, 4)), "expected integer, found `bool`")
        // The `1` operand is byte 8 (column 9), 1 byte long.
        .secondary(Span::new(id, ByteSpan::at(8, 1)), "left operand is an integer")
        .build();

    let out = diag.render(&sources);

    // Both the primary caret and the secondary underline share one snippet line.
    assert!(out.contains("-   ^^^^ expected integer, found `bool`"), "{out}");
    // The secondary label is drawn under its own mark.
    assert!(out.contains("left operand is an integer"), "{out}");
}

#[test]
fn secondary_span_in_another_source_gets_its_own_snippet_block() {
    let mut sources = SourceMap::new();
    let main = sources.add_file("main.liasse", "use other.thing\n");
    let other = sources.add_file("other.liasse", "thing = 1\n");
    let diag = Diagnostic::error("type mismatch across files")
        .primary(Span::new(main, ByteSpan::at(4, 11)), "used as text here")
        .secondary(Span::new(other, ByteSpan::at(0, 5)), "defined as integer here")
        .build();

    let out = diag.render(&sources);

    // Primary snippet header, then the cross-referenced `:::` secondary header.
    assert!(out.contains("--> main.liasse:1:5"), "{out}");
    assert!(out.contains("::: other.liasse:1:1"), "{out}");
    assert!(out.contains("used as text here"), "{out}");
    assert!(out.contains("defined as integer here"), "{out}");
}

#[test]
fn multibyte_source_places_the_caret_by_display_column() {
    let mut sources = SourceMap::new();
    // "café = 1": c a f are columns 1..3, é is display column 4 (bytes [3,5)).
    let id = sources.add_label("repl", "café = 1\n");
    let diag = Diagnostic::warning("accented identifier")
        .primary(Span::new(id, ByteSpan::at(3, 2)), "non-ASCII letter here")
        .build();

    let out = diag.render(&sources);

    // Synthetic origin is bracketed; the caret sits at display column 4.
    assert!(out.contains("--> <repl>:1:4"), "{out}");
    assert!(out.contains("café = 1"), "{out}");
    // Exactly one caret for the single-width é, not two for its two bytes.
    assert!(out.contains("^ non-ASCII letter here"), "{out}");
    assert!(!out.contains("^^ non-ASCII letter here"), "{out}");
}

#[test]
fn a_span_off_a_char_boundary_widens_instead_of_panicking() {
    let mut sources = SourceMap::new();
    // "café" holds é (U+00E9) at bytes [3,5). A span of [0,4) ends inside é.
    // The renderer must not slice the backend off that boundary and panic; it
    // widens to the whole char, so the annotation still covers "café".
    let id = sources.add_label("repl", "café = 1\n");
    let diag = Diagnostic::error("split char")
        .primary(Span::new(id, ByteSpan::at(0, 4)), "here")
        .build();

    let out = diag.render(&sources);
    assert!(out.contains("café = 1"), "{out}");
    // Widened end lands past é (byte 5), so all four display columns underline.
    assert!(out.contains("^^^^ here"), "{out}");

    // A span starting entirely past the end must also render without panic.
    let past = Diagnostic::error("past end")
        .primary(Span::new(id, ByteSpan::at(100, 4)), "gone")
        .build();
    assert!(!past.render(&sources).is_empty());
}

#[test]
fn note_without_code_renders_a_bare_note_title() {
    let mut sources = SourceMap::new();
    let id = sources.add_file("n.liasse", "value\n");
    let diag = Diagnostic::note("for your information")
        .primary(Span::new(id, ByteSpan::at(0, 5)), "this value")
        .build();

    let out = diag.render(&sources);
    assert!(out.contains("note: for your information"), "{out}");
    // No code means no `[...]` tag after the severity word.
    assert!(!out.contains("note["), "{out}");
}

#[test]
fn a_batch_renders_every_diagnostic_in_order() {
    let mut sources = SourceMap::new();
    let id = sources.add_file("b.liasse", "let x = 1 + true\n");
    let err = Diagnostic::error("mismatched types")
        .code("E0308")
        .primary(Span::new(id, ByteSpan::at(12, 4)), "found `bool`")
        .build();
    let warn = Diagnostic::warning("unused binding")
        .primary(Span::new(id, ByteSpan::at(4, 1)), "`x` is never read")
        .build();

    let mut diags = Diagnostics::new();
    diags.push(err);
    diags.push(warn);
    let out = diags.render(&sources);

    let error_at = out.find("error[E0308]: mismatched types");
    let warn_at = out.find("warning: unused binding");
    assert!(error_at.is_some(), "{out}");
    assert!(warn_at.is_some(), "{out}");
    // Insertion order is preserved in the rendered batch.
    assert!(error_at < warn_at, "{out}");
}

#[test]
fn ansi_style_emits_escapes_while_plain_does_not() {
    let mut sources = SourceMap::new();
    let id = sources.add_file("c.liasse", "value\n");
    let diag = Diagnostic::error("styled")
        .primary(Span::new(id, ByteSpan::at(0, 5)), "here")
        .build();

    let plain = diag.render_with(&sources, RenderStyle::Plain);
    let ansi = diag.render_with(&sources, RenderStyle::Ansi);
    assert!(!plain.contains('\u{1b}'), "plain must be escape-free: {plain:?}");
    assert!(ansi.contains('\u{1b}'), "ansi must carry escapes: {ansi:?}");
}
