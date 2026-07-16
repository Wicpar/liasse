//! D.2 canonical key text: escaping, composite join, no normalization.
//! Expected strings are taken directly from Annex D.2 and the
//! `tests/annex-d-identity` corpus cases.

use liasse_ident::{KeyComponent, KeyText};
use liasse_value::{Integer, Struct, Text, Uuid, Value};

type Fallible = Result<(), Box<dyn std::error::Error>>;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

#[test]
fn text_key_escapes_slash_and_colon() -> Fallible {
    // corpus: seed-key-text-escapes-slash-and-colon — typed key "a/b:c" is the
    // encoded member name "a%2Fb%3Ac".
    let key = KeyText::from_key_values(&[text("a/b:c")])?;
    assert_eq!(key.as_str(), "a%2Fb%3Ac");
    Ok(())
}

#[test]
fn composite_key_joins_in_key_order() -> Fallible {
    // corpus: seed-composite-key-joined-in-key-order — ["eu","std"] -> "eu:std".
    let key = KeyText::from_key_values(&[text("eu"), text("std")])?;
    assert_eq!(key.as_str(), "eu:std");
    // Order is significant: the reverse produces a different member name.
    let swapped = KeyText::from_key_values(&[text("std"), text("eu")])?;
    assert_eq!(swapped.as_str(), "std:eu");
    assert_ne!(key, swapped);
    Ok(())
}

#[test]
fn uuid_key_is_lowercase_hyphenated() -> Fallible {
    // corpus: canonical-uuid-key-lowercase-hyphenated. Parse an upper-case input
    // to prove the canonical text is the lowercase hyphenated form (D.2).
    let value = Value::Uuid(Uuid::parse("550E8400-E29B-41D4-A716-446655440000")?);
    let key = KeyText::from_key_values(&[value])?;
    assert_eq!(key.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    Ok(())
}

#[test]
fn percent_escape_is_not_double_encoded() -> Fallible {
    // corpus: percent-escape-not-double-encoded.
    //   typed key "/"    -> "%2F"   (original slash encoded once)
    //   typed key "%2F"  -> "%252F" (original percent encoded to %25)
    let slash = KeyText::from_key_values(&[text("/")])?;
    let literal = KeyText::from_key_values(&[text("%2F")])?;
    assert_eq!(slash.as_str(), "%2F");
    assert_eq!(literal.as_str(), "%252F");
    assert_ne!(slash, literal);
    Ok(())
}

#[test]
fn unicode_equivalent_keys_stay_distinct() -> Fallible {
    // corpus: nfc-equivalent-keys-are-distinct — D.2 applies no NFC/NFD folding.
    let composed = KeyText::from_key_values(&[text("caf\u{00e9}")])?;
    let decomposed = KeyText::from_key_values(&[text("cafe\u{0301}")])?;
    assert_ne!(composed, decomposed);
    assert_eq!(composed.as_str(), "caf\u{00e9}");
    assert_eq!(decomposed.as_str(), "cafe\u{0301}");
    Ok(())
}

#[test]
fn bool_and_int_use_canonical_scalar_text() -> Fallible {
    // D.2 table: bool is true|false, int is canonical decimal digits.
    let yes = KeyComponent::from_scalar(&Value::Bool(true))?;
    let no = KeyComponent::from_scalar(&Value::Bool(false))?;
    let count = KeyComponent::from_scalar(&Value::Int(Integer::from(20_i64)))?;
    assert_eq!(yes.as_str(), "true");
    assert_eq!(no.as_str(), "false");
    assert_eq!(count.as_str(), "20");
    Ok(())
}

#[test]
fn struct_key_flattens_in_canonical_field_name_order() -> Fallible {
    // D.2: a struct key contributes "its components in canonical field-name
    // order". Field names are supplied out of order ("region" after "code"); the
    // canonical order is "code" then "region", so the joined text is "std:eu"
    // regardless of construction order — externally fixed by the field names.
    let out_of_order = Struct::new([
        (Text::new("region"), text("eu")),
        (Text::new("code"), text("std")),
    ]);
    let key = KeyText::from_key_values(&[Value::Struct(out_of_order)])?;
    assert_eq!(key.as_str(), "std:eu");
    Ok(())
}

#[test]
fn non_key_eligible_value_is_rejected() {
    // D.2 gives no scalar key text for `none`; it cannot be a key component.
    assert!(KeyComponent::from_scalar(&Value::None).is_err());
    // An empty key is likewise not representable.
    assert!(KeyText::from_key_values(&[]).is_err());
}

#[test]
fn escaped_key_text_round_trips_through_components() -> Fallible {
    // Parsing the escaped member name and decoding its components recovers the
    // exact original typed key text, join order preserved.
    let key = KeyText::from_key_values(&[text("a/b:c"), text("plain")])?;
    let reparsed = KeyText::parse(key.as_str().to_owned())?;
    assert_eq!(reparsed, key);
    let components = reparsed.components()?;
    let decoded: Vec<&str> = components.iter().map(KeyComponent::as_str).collect();
    assert_eq!(decoded, vec!["a/b:c", "plain"]);
    Ok(())
}
