//! The source registry: stable ids, name display, and out-of-map lookups.

use liasse_diag::{Source, SourceId, SourceMap, SourceName};

#[test]
fn ids_are_stable_and_resolve_to_their_source() {
    let mut sources = SourceMap::new();
    let a = sources.add_file("a.liasse", "alpha");
    let b = sources.add_label("scratch", "beta");

    // Distinct sources get distinct ids assigned in insertion order.
    assert_ne!(a, b);
    assert_eq!(a.index(), 0);
    assert_eq!(b.index(), 1);
    assert_eq!(sources.len(), 2);

    assert_eq!(sources.get(a).map(Source::text), Some("alpha"));
    assert_eq!(
        sources.get(a).map(Source::name),
        Some(&SourceName::File("a.liasse".to_owned())),
    );
    assert_eq!(
        sources.get(b).map(Source::name),
        Some(&SourceName::Label("scratch".to_owned())),
    );
}

#[test]
fn name_display_brackets_synthetic_labels_only() {
    assert_eq!(SourceName::File("dir/x.liasse".to_owned()).display(), "dir/x.liasse");
    assert_eq!(SourceName::Label("repl".to_owned()).display(), "<repl>");
}

#[test]
fn an_id_from_another_map_does_not_resolve() {
    let mut one = SourceMap::new();
    let _ = one.add_file("only.liasse", "text");
    let two = SourceMap::new();

    // `one`'s single id addresses index 0; the empty map has nothing there.
    let borrowed_id: SourceId = one.add_file("second.liasse", "more");
    assert!(two.get(borrowed_id).is_none());
    assert!(two.is_empty());
}
