//! Canonical wire encoding/decoding round-trips and rejections (Annex A).
//!
//! Expected strings are the byte-exact canonical forms named in Annex A and
//! the annex-a-types-wire corpus, re-derived from the encoding rules — never
//! echoed from the implementation.

use liasse_value::{
    BlobDescriptor, Bytes, Date, Decimal, Duration, EnumType, Integer, MediaType, Precision, Ref,
    Sha512, Timestamp, Type, Value, ValueError,
};

// ---- scalar canonical wire strings ------------------------------------------

#[test]
fn int_wire_is_json_string_not_number() -> Result<(), ValueError> {
    // A.1: `int` canonical form is a JSON string of base-10 digits.
    let value = Value::Int(Integer::parse("43")?);
    assert_eq!(value.to_canonical_json_string(), "\"43\"");
    Ok(())
}

#[test]
fn int_decodes_from_json_number_and_normalizes() -> Result<(), ValueError> {
    // SPEC-ISSUES item 2 decoder stance: accept a bare number, emit the string.
    let value = Type::Int.decode(&serde_json::json!(43))?;
    assert_eq!(value.to_canonical_json_string(), "\"43\"");
    Ok(())
}

#[test]
fn decimal_product_has_no_exponent_and_preserves_scale() -> Result<(), ValueError> {
    // A.6: 0.0001 * 0.0001 is exact; A.1: plain notation, no exponent.
    let a = Decimal::parse("0.0001")?;
    let product = Decimal::from_big_decimal(a.as_big_decimal() * a.as_big_decimal());
    assert_eq!(product.to_canonical_text(), "0.00000001");
    Ok(())
}

#[test]
fn decimal_large_magnitude_stays_plain() -> Result<(), ValueError> {
    let a = Decimal::parse("10000000000")?;
    let product = Decimal::from_big_decimal(a.as_big_decimal() * a.as_big_decimal());
    assert_eq!(product.to_canonical_text(), "100000000000000000000");
    Ok(())
}

#[test]
fn decimal_preserves_trailing_zero_scale() -> Result<(), ValueError> {
    // SPEC-ISSUES item 1 choice: preserve the operation scale verbatim.
    assert_eq!(Decimal::parse("1.00")?.to_canonical_text(), "1.00");
    assert_eq!(Decimal::parse("-0")?.to_canonical_text(), "0");
    Ok(())
}

#[test]
fn bytes_wire_is_dollar_bytes_padded_base64() {
    // "Hello" -> "SGVsbG8=" (A.1 / D.2 padded base64).
    let value = Value::Bytes(Bytes::new(b"Hello".to_vec()));
    assert_eq!(value.to_canonical_json_string(), "{\"$bytes\":\"SGVsbG8=\"}");
}

#[test]
fn empty_bytes_wire_is_empty_base64() {
    let value = Value::Bytes(Bytes::new(Vec::new()));
    assert_eq!(value.to_canonical_json_string(), "{\"$bytes\":\"\"}");
}

#[test]
fn uuid_uppercase_input_normalizes_to_lowercase() -> Result<(), ValueError> {
    let value = Type::Uuid.decode(&serde_json::json!("00112233-4455-6677-8899-AABBCCDDEEFF"))?;
    assert_eq!(
        value.to_canonical_json_string(),
        "\"00112233-4455-6677-8899-aabbccddeeff\""
    );
    Ok(())
}

#[test]
fn timestamp_wire_is_signed_base10_string() {
    let positive = Value::Timestamp(Timestamp::new(1_000_000, Precision::Micros));
    assert_eq!(positive.to_canonical_json_string(), "\"1000000\"");
    let negative = Value::Timestamp(Timestamp::new(-1_000_000, Precision::Micros));
    assert_eq!(negative.to_canonical_json_string(), "\"-1000000\"");
}

#[test]
fn date_wire_is_yyyy_mm_dd() -> Result<(), ValueError> {
    let value = Value::Date(Date::parse("2024-01-31")?);
    assert_eq!(value.to_canonical_json_string(), "\"2024-01-31\"");
    Ok(())
}

#[test]
fn duration_canonical_iso8601_forms() -> Result<(), ValueError> {
    for text in ["PT1H", "P7D", "PT15M", "PT0S"] {
        assert_eq!(Duration::parse(text)?.to_canonical_text(), text);
    }
    // Composite: 1 day 2 hours 3 minutes 4.5 seconds.
    let composite = Duration::parse("P1DT2H3M4.5S")?;
    assert_eq!(composite.to_canonical_text(), "P1DT2H3M4.5S");
    Ok(())
}

#[test]
fn enum_wire_is_label_not_ordinal() -> Result<(), ValueError> {
    let ty = EnumType::new(["red".into(), "green".into(), "blue".into()])?;
    let value = Value::Enum(ty.parse("green")?);
    assert_eq!(value.to_canonical_json_string(), "\"green\"");
    Ok(())
}

// ---- composite / generic slots ---------------------------------------------

#[test]
fn ref_composite_wire_is_key_order_array() {
    let value = Value::Ref(Ref::composite(vec![
        Value::Text(liasse_value::Text::new("eu")),
        Value::Text(liasse_value::Text::new("x")),
    ]));
    assert_eq!(value.to_canonical_json_string(), "[\"eu\",\"x\"]");
}

#[test]
fn none_generic_slot_wire_is_dollar_none() {
    assert_eq!(Value::None.to_canonical_json_string(), "{\"$none\":true}");
}

#[test]
fn json_null_is_preserved_and_distinct_from_none() -> Result<(), ValueError> {
    let value = Type::Json.decode(&serde_json::json!(null))?;
    assert_eq!(value.to_canonical_json_string(), "null");
    assert_ne!(value, Value::None);
    Ok(())
}

#[test]
fn json_object_keys_are_canonically_sorted() -> Result<(), ValueError> {
    // A.7: object keys sorted by text order regardless of input order.
    let value = Type::Json.decode(&serde_json::json!({"b": 0, "a": 1}))?;
    assert_eq!(value.to_canonical_json_string(), "{\"a\":1,\"b\":0}");
    Ok(())
}

#[test]
fn json_array_order_is_preserved() -> Result<(), ValueError> {
    let value = Type::Json.decode(&serde_json::json!([3, 1, 2]))?;
    assert_eq!(value.to_canonical_json_string(), "[3,1,2]");
    Ok(())
}

#[test]
fn blob_descriptor_wire_has_string_byte_count_and_canonically_sorted_keys(
) -> Result<(), ValueError> {
    // §18: `$sha512` is 128 lowercase-hex chars, `$bytes` is the byte count as
    // a string ("184320"), `$media` a media type, `$name` optional. A.7 sorts
    // object keys by text order: "$bytes" < "$media" < "$name" < "$sha512".
    let hex = "ab".repeat(64);
    let descriptor = BlobDescriptor::new(
        Sha512::parse(&hex)?,
        184_320,
        MediaType::new("application/pdf"),
        Some("report.pdf".to_owned()),
    );
    let value = Value::Blob(Box::new(descriptor));
    let expected = format!(
        "{{\"$bytes\":\"184320\",\"$media\":\"application/pdf\",\"$name\":\"report.pdf\",\"$sha512\":\"{hex}\"}}"
    );
    assert_eq!(value.to_canonical_json_string(), expected);

    // Round-trip: decoding the emitted wire reproduces the same value.
    assert_eq!(Type::Blob.decode(&value.to_wire())?, value);
    Ok(())
}

// ---- decode rejections Annex A calls out -----------------------------------

#[test]
fn fixed_period_rejects_calendar_month() {
    // A.4: "P1M" is a calendar quantity, illegal in the fixed-period string form.
    let outcome = Type::Period.decode(&serde_json::json!("P1M"));
    assert!(matches!(outcome, Err(ValueError::CalendarInFixedPeriod(_))));
}

#[test]
fn calendar_period_requires_nonzero_magnitude() {
    let all_zero = serde_json::json!({
        "years": 0, "months": 0, "weeks": 0, "days": 0, "time": "PT0S"
    });
    assert!(matches!(
        Type::Period.decode(&all_zero),
        Err(ValueError::EmptyCalendarPeriod)
    ));
}

#[test]
fn int_rejects_fractional_text() {
    assert!(matches!(
        Type::Int.decode(&serde_json::json!("4.5")),
        Err(ValueError::MalformedInt(_))
    ));
}

#[test]
fn bytes_rejects_non_base64() {
    assert!(matches!(
        Type::Bytes.decode(&serde_json::json!({"$bytes": "!!!!"})),
        Err(ValueError::MalformedBase64(_))
    ));
}

#[test]
fn uuid_rejects_malformed() {
    assert!(matches!(
        Type::Uuid.decode(&serde_json::json!("not-a-uuid")),
        Err(ValueError::MalformedUuid(_))
    ));
}

#[test]
fn date_rejects_impossible_calendar_date() {
    assert!(matches!(
        Type::Date.decode(&serde_json::json!("2024-13-40")),
        Err(ValueError::MalformedDate(_))
    ));
}

#[test]
fn duration_rejects_year_component() {
    assert!(matches!(
        Type::Duration.decode(&serde_json::json!("P1Y")),
        Err(ValueError::CalendarInFixedPeriod(_))
    ));
}

#[test]
fn int_rejects_wrong_json_shape() {
    assert!(matches!(
        Type::Int.decode(&serde_json::json!(true)),
        Err(ValueError::TypeMismatch { ty: "int", .. })
    ));
}

#[test]
fn decimal_rejects_extreme_scale_exponent() {
    // DoS guard: `BigDecimal` parses `1E-2000000000` (scale two billion), whose
    // canonical plain form is a multi-gigabyte string. Decode must reject the
    // extreme scale at the wire boundary (A.6 bounds the result scale) so it can
    // never reach the canonical-text encoder.
    assert!(matches!(
        Type::Decimal.decode(&serde_json::json!("1E-2000000000")),
        Err(ValueError::DecimalScaleOutOfRange { .. })
    ));
    // The mirror image (a huge negative scale, i.e. billions of trailing zeros)
    // is rejected the same way.
    assert!(matches!(
        Type::Decimal.decode(&serde_json::json!("1E+2000000000")),
        Err(ValueError::DecimalScaleOutOfRange { .. })
    ));
}
