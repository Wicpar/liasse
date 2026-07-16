//! Span offset math with externally-computed expected offsets, including
//! multi-byte UTF-8 anchored against real source text.

use liasse_diag::{ByteSpan, SourceMap, Span};

#[test]
fn reversed_bounds_are_rejected_forward_bounds_accepted() {
    // end < start is a caller bug, not an empty span.
    assert_eq!(ByteSpan::new(5, 3), None);
    // A well-formed range equals the same offsets built length-first.
    assert_eq!(ByteSpan::new(3, 5), Some(ByteSpan::at(3, 2)));
    // `cover` orders the offsets instead of rejecting.
    assert_eq!(ByteSpan::cover(5, 3), ByteSpan::at(3, 2));
}

#[test]
fn length_and_emptiness_follow_the_half_open_bounds() {
    let span = ByteSpan::at(10, 4);
    assert_eq!(span.start(), 10);
    assert_eq!(span.end(), 14);
    assert_eq!(span.len(), 4);
    assert!(!span.is_empty());

    let point = ByteSpan::point(7);
    assert_eq!(point.len(), 0);
    assert!(point.is_empty());
}

#[test]
fn merge_is_the_convex_hull_even_across_a_gap() {
    // Disjoint [2,4) and [8,10): hull is [2,10).
    assert_eq!(ByteSpan::at(2, 2).merge(ByteSpan::at(8, 2)), ByteSpan::at(2, 8));
    // Overlapping [2,6) and [4,9): hull is [2,9).
    assert_eq!(ByteSpan::at(2, 4).merge(ByteSpan::at(4, 5)), ByteSpan::at(2, 7));
    // Merge is commutative on these operands.
    assert_eq!(
        ByteSpan::at(8, 2).merge(ByteSpan::at(2, 2)),
        ByteSpan::at(2, 8),
    );
}

#[test]
fn containment_is_inclusive_of_boundaries() {
    let outer = ByteSpan::at(2, 8); // [2,10)
    assert!(outer.contains(ByteSpan::at(4, 2))); // [4,6) inside
    assert!(outer.contains(outer)); // reflexive
    assert!(outer.contains(ByteSpan::point(10))); // point at the right edge
    assert!(!outer.contains(ByteSpan::at(1, 2))); // [1,3) starts before
    assert!(!outer.contains(ByteSpan::at(9, 2))); // [9,11) ends after
}

#[test]
fn offset_containment_is_half_open() {
    let span = ByteSpan::at(3, 3); // [3,6)
    assert!(!span.contains_offset(2));
    assert!(span.contains_offset(3)); // start included
    assert!(span.contains_offset(5));
    assert!(!span.contains_offset(6)); // end excluded
}

#[test]
fn spans_slice_multibyte_utf8_at_the_right_byte_offsets() {
    // "café" is 5 bytes: c a f each 1 byte, é (U+00E9) is 2 bytes -> [3,5).
    let mut sources = SourceMap::new();
    let id = sources.add_label("repl", "café = 1");

    let e_acute = Span::new(id, ByteSpan::at(3, 2));
    assert_eq!(sources.span_text(e_acute), Some("é"));

    let word = Span::new(id, ByteSpan::at(0, 5));
    assert_eq!(sources.span_text(word), Some("café"));

    // A range that would split the é lands off a char boundary -> no text.
    let split = Span::new(id, ByteSpan::at(0, 4));
    assert_eq!(sources.span_text(split), None);

    // Past the end of the text -> no text, no panic.
    let past = Span::new(id, ByteSpan::at(6, 100));
    assert_eq!(sources.span_text(past), None);
}

#[test]
fn located_spans_only_merge_and_contain_within_one_source() {
    let mut sources = SourceMap::new();
    let a = sources.add_file("a.liasse", "0123456789");
    let b = sources.add_file("b.liasse", "0123456789");

    let a_lo = Span::new(a, ByteSpan::at(1, 2)); // [1,3) in a
    let a_hi = Span::new(a, ByteSpan::at(5, 2)); // [5,7) in a
    assert_eq!(a_lo.merge(a_hi), Some(Span::new(a, ByteSpan::at(1, 6))));
    assert!(a_lo.contains(Span::new(a, ByteSpan::at(1, 1))));

    let b_span = Span::new(b, ByteSpan::at(1, 2));
    // Different sources never merge, and never contain each other.
    assert_eq!(a_lo.merge(b_span), None);
    assert!(!a_lo.contains(b_span));
}
