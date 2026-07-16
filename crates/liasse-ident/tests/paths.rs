//! D.3 display paths: pinned display form and fallible string parse.

use liasse_ident::{CanonicalPath, IdentError, KeyComponent, KeyText, NameSegment, PathSegment};
use liasse_value::{Text, Value};

type Fallible = Result<(), Box<dyn std::error::Error>>;

fn name(value: &str) -> PathSegment {
    PathSegment::Name(NameSegment::new(value))
}

fn key(value: &str) -> Result<PathSegment, IdentError> {
    Ok(PathSegment::Key(KeyText::from_key_values(&[Value::Text(
        Text::new(value),
    )])?))
}

#[test]
fn nested_collection_path_matches_annex_example() -> Fallible {
    // D.3 example: /companies/acme/offices/paris/rooms/main.
    let path = CanonicalPath::new([
        name("companies"),
        key("acme")?,
        name("offices"),
        key("paris")?,
        name("rooms"),
        key("main")?,
    ]);
    assert_eq!(
        path.to_display_string(),
        "/companies/acme/offices/paris/rooms/main"
    );
    Ok(())
}

#[test]
fn key_slash_is_escaped_in_display_path() -> Fallible {
    // corpus: display-path-key-slash-escaped-in-correction — key "a/b" under
    // `notes`, addressing field `body`, renders as /notes/a%2Fb/body so the
    // in-key slash cannot be mistaken for a path separator.
    let path = CanonicalPath::new([name("notes"), key("a/b")?, name("body")]);
    assert_eq!(path.to_display_string(), "/notes/a%2Fb/body");
    Ok(())
}

#[test]
fn parse_splits_on_unescaped_separators_only() -> Fallible {
    // The escaped key segment stays a single segment through parse; resolving it
    // as key text and decoding recovers the original "a/b".
    let raw = CanonicalPath::parse("/notes/a%2Fb/body")?;
    let encoded: Vec<&str> = raw.iter().map(|s| s.as_encoded()).collect();
    assert_eq!(encoded, vec!["notes", "a%2Fb", "body"]);

    let note_name = raw.first().ok_or("segment 0")?.as_name()?;
    assert_eq!(note_name.as_str(), "notes");

    let key_text = raw.get(1).ok_or("segment 1")?.as_key()?;
    let components = key_text.components()?;
    assert_eq!(components.first().map(KeyComponent::as_str), Some("a/b"));
    Ok(())
}

#[test]
fn display_and_parse_agree_on_segment_texts() -> Fallible {
    let path = CanonicalPath::new([name("rates"), key("eu:std")?]);
    let rendered = path.to_display_string();
    let raw = CanonicalPath::parse(&rendered)?;
    let encoded: Vec<&str> = raw.iter().map(|s| s.as_encoded()).collect();
    // "eu:std" is one text key; its `:` becomes %3A, so it stays one segment.
    assert_eq!(encoded, vec!["rates", "eu%3Astd"]);
    Ok(())
}

#[test]
fn root_path_parses_to_no_segments() -> Fallible {
    assert!(CanonicalPath::parse("/")?.is_empty());
    Ok(())
}

#[test]
fn parse_rejects_malformed_paths() {
    assert!(matches!(
        CanonicalPath::parse("notes/a"),
        Err(IdentError::PathMissingRoot { .. })
    ));
    assert!(matches!(
        CanonicalPath::parse("/notes//body"),
        Err(IdentError::EmptyPathSegment { .. })
    ));
    assert!(matches!(
        CanonicalPath::parse("/notes/"),
        Err(IdentError::EmptyPathSegment { .. })
    ));
    assert!(matches!(
        CanonicalPath::parse("/notes/a%2Zb"),
        Err(IdentError::MalformedEscape { .. })
    ));
}
