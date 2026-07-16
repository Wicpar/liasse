//! A `SourceId` issued by one `SourceMap` must not resolve against a different
//! map, even when its dense index is in range there. The id carries its issuing
//! map's identity, so a foreign lookup is a clean `None` — never a silent quote
//! of an unrelated map's text under a diagnostic label.

use liasse_diag::{ByteSpan, Diagnostic, Source, SourceMap, Span};

#[test]
fn get_returns_none_for_id_not_issued_by_this_map() {
    let mut a = SourceMap::new();
    let id_a = a.add_file("a.liasse", "alpha\n");

    let mut b = SourceMap::new();
    let _id_b = b.add_file("b.liasse", "beta\n");

    // `id_a` (map A, index 0) has an in-range index in `b`, but was not issued
    // by `b`. Per the documented contract, `b.get(id_a)` is None.
    assert_eq!(
        b.get(id_a).map(Source::text),
        None,
        "SourceMap::get resolved a foreign id against the wrong map",
    );
    // The map that did issue it still resolves it.
    assert_eq!(a.get(id_a).map(Source::text), Some("alpha\n"));
}

#[test]
fn same_index_across_maps_is_a_distinct_id() {
    let mut a = SourceMap::new();
    let id_a = a.add_file("a.liasse", "alpha\n");

    let mut b = SourceMap::new();
    let id_b = b.add_file("b.liasse", "beta\n");

    // Both are the index-0 source of their map, yet the ids are not equal:
    // identity, not just position, distinguishes them.
    assert_eq!(id_a.index(), id_b.index());
    assert_ne!(id_a, id_b);
}

#[test]
fn cross_map_span_drops_its_label_instead_of_misrendering() {
    let mut a = SourceMap::new();
    let secret = a.add_file("secret.liasse", "password = hunter2\n");

    let mut b = SourceMap::new();
    let _public = b.add_file("public.liasse", "greeting = hello\n");

    // A diagnostic authored against map A's `secret.liasse`...
    let diag = Diagnostic::error("x")
        .primary(Span::new(secret, ByteSpan::at(0, 8)), "points into secret.liasse")
        .build();

    // ...rendered against the unrelated map B must not draw that label over
    // B's `public.liasse` text. With the foreign source unknown, the snippet is
    // dropped: no caret line, no quoted source line — and no panic.
    let rendered = diag.render(&b);
    assert!(!rendered.contains("public.liasse"), "foreign span quoted map B's source:\n{rendered}");
    assert!(!rendered.contains("greeting = hello"), "foreign span quoted map B's text:\n{rendered}");
    assert!(!rendered.contains("points into secret.liasse"), "foreign label was drawn:\n{rendered}");
    // The title still renders — the diagnostic degrades, it does not vanish.
    assert!(rendered.contains("x"), "diagnostic title missing:\n{rendered}");
}
