#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §19.9 host correction over a reconciliation plan, addressed by D.3 display path.
//!
//! The escaped-key case (§D.3, annex-d `display-path-key-slash-escaped-in-correction`)
//! is the attack: a row keyed with the text `a/b` must be addressed as
//! `/notes/a%2Fb/body`, so the `/` inside the key cannot be confused with a path
//! separator. A correction that matched the raw `/notes/a/b/body` would resolve
//! the wrong (or no) coordinate.

mod support;

use liasse_surface::{ChooseMap, ChooseSide, ConflictCoordinate, CorrectionError};
use support::{host, text};

#[test]
fn escaped_display_path_addresses_the_slashed_key() {
    let host = host();
    // A field conflict on the body of the row whose key literally contains `/`.
    let conflict = ConflictCoordinate::field("notes", text("a/b"), "body");
    assert_eq!(
        conflict.display_path().expect("key is D.2-eligible"),
        "/notes/a%2Fb/body",
        "the key segment escapes `/` as %2F (§D.3)"
    );

    let choose = ChooseMap::new().with("/notes/a%2Fb/body", ChooseSide::Incoming);
    let outcome = host.apply_correction(&[conflict], &choose).expect("correction resolves");

    assert!(outcome.is_complete(), "every conflict was chosen");
    assert_eq!(
        outcome.chosen("/notes/a%2Fb/body"),
        Some(ChooseSide::Incoming),
        "the escaped path selected the incoming side for exactly this row's body"
    );
}

#[test]
fn raw_unescaped_path_does_not_match_the_slashed_key() {
    let host = host();
    let conflict = ConflictCoordinate::field("notes", text("a/b"), "body");
    // The §D.3 attack: addressing with the raw, unescaped path must not resolve the
    // conflict — the real coordinate is left unresolved.
    let choose = ChooseMap::new().with("/notes/a/b/body", ChooseSide::Incoming);
    let error = host.apply_correction(&[conflict], &choose).unwrap_err();
    assert!(
        matches!(&error, CorrectionError::Unresolved(path) if path == "/notes/a%2Fb/body"),
        "the escaped conflict is unresolved, not silently matched: {error:?}"
    );
}

#[test]
fn a_choose_key_addressing_no_conflict_is_rejected() {
    let host = host();
    let conflict = ConflictCoordinate::field("notes", text("n1"), "body");
    let choose = ChooseMap::new()
        .with("/notes/n1/body", ChooseSide::Incoming)
        .with("/notes/n2/body", ChooseSide::Local);
    let error = host.apply_correction(&[conflict], &choose).unwrap_err();
    assert!(
        matches!(&error, CorrectionError::Unmatched(path) if path == "/notes/n2/body"),
        "a stray choose key is a correction error: {error:?}"
    );
}

#[test]
fn a_whole_row_conflict_addresses_without_a_field_segment() {
    let host = host();
    // A delete-vs-modify / competing-insert conflict is a whole-row coordinate.
    let conflict = ConflictCoordinate::row("notes", text("a/b"));
    assert_eq!(conflict.display_path().expect("D.2-eligible"), "/notes/a%2Fb");
    let choose = ChooseMap::new().with("/notes/a%2Fb", ChooseSide::Local);
    let outcome = host.apply_correction(&[conflict], &choose).expect("resolves");
    assert_eq!(outcome.chosen("/notes/a%2Fb"), Some(ChooseSide::Local));
}
