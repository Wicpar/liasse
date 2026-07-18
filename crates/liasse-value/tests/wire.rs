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
    // A bare JSON number carries a single value, so both boundaries accept it and
    // emit the canonical string (SPEC-ISSUES item 2 restricts the canonicality
    // gate to string *spellings*, which a number does not have).
    let value = Type::Int.decode(&serde_json::json!(43))?;
    assert_eq!(value.to_canonical_json_string(), "\"43\"");
    Ok(())
}

#[test]
fn noncanonical_int_string_rejected_at_wire_accepted_when_authored() -> Result<(), ValueError> {
    // SPEC-ISSUES item 2: a leading-zero / signed / `-0` int *string* is a
    // non-canonical spelling. The wire boundary rejects it; authoring normalizes.
    for spelling in ["007", "+7", "-0"] {
        assert!(
            matches!(
                Type::Int.decode_wire(&serde_json::json!(spelling)),
                Err(ValueError::NonCanonicalScalar { ty: "int", .. })
            ),
            "wire must reject non-canonical int `{spelling}`"
        );
    }
    assert_eq!(Type::Int.decode(&serde_json::json!("007"))?.to_canonical_json_string(), "\"7\"");
    assert_eq!(Type::Int.decode(&serde_json::json!("-0"))?.to_canonical_json_string(), "\"0\"");
    Ok(())
}

#[test]
fn noncanonical_base64_padding_is_malformed_at_both_boundaries() {
    // "Hello" is canonical `SGVsbG8=`. Dropping the required `=` padding is not a
    // decodable-but-non-canonical spelling — the canonical padded-base64 decoder
    // rejects it as malformed (A.1), so both boundaries reject it the same way
    // (SPEC-ISSUES item 2 needs no separate `bytes` gate).
    let unpadded = serde_json::json!({ "$bytes": "SGVsbG8" });
    assert!(matches!(Type::Bytes.decode_wire(&unpadded), Err(ValueError::MalformedBase64(_))));
    assert!(matches!(Type::Bytes.decode(&unpadded), Err(ValueError::MalformedBase64(_))));
}

#[test]
fn canonical_scalars_pass_the_wire_boundary_unchanged() -> Result<(), ValueError> {
    // The gate is round-trip equality: a scalar already in canonical form decodes
    // identically at both boundaries (no false rejection).
    for (ty, wire) in [
        (Type::Int, serde_json::json!("7")),
        (Type::Uuid, serde_json::json!("00112233-4455-6677-8899-aabbccddeeff")),
        (Type::Bytes, serde_json::json!({ "$bytes": "SGVsbG8=" })),
        (Type::Duration, serde_json::json!("PT1H")),
        (Type::Date, serde_json::json!("2024-01-31")),
    ] {
        assert_eq!(ty.decode_wire(&wire)?, ty.decode(&wire)?);
    }
    Ok(())
}

#[test]
fn decimal_product_has_no_exponent() -> Result<(), ValueError> {
    // A.6: 0.0001 * 0.0001 is exact; A.1: plain notation, no exponent. All eight
    // fractional digits are significant, so minimal scale keeps them.
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
fn decimal_canonical_text_is_minimal_scale() -> Result<(), ValueError> {
    // SPEC-ISSUES item 1 (resolved): A.1's canonical form is minimal scale — one
    // spelling per mathematical value. Every trailing fractional zero is stripped
    // and the point dropped when no fractional digit remains; integer-part zeros
    // (magnitude, not scale) survive; `-0` is `0`.
    for (input, canonical) in [
        ("1.00", "1"),
        ("1.50", "1.5"),
        ("2.5000000000000000", "2.5"),
        ("0.500", "0.5"),
        ("10.0", "10"),
        ("100", "100"),
        ("0.00", "0"),
        ("-0", "0"),
        ("-1.230", "-1.23"),
    ] {
        assert_eq!(
            Decimal::parse(input)?.to_canonical_text(),
            canonical,
            "canonical text of `{input}`"
        );
    }
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
fn uuid_uppercase_input_rejected_at_wire_boundary() {
    // SPEC-ISSUES item 2: the machine wire boundary requires the canonical
    // lowercase-hyphenated form (A.1). An uppercase spelling is rejected as
    // malformed at admission, never normalized.
    let outcome = Type::Uuid.decode_wire(&serde_json::json!("00112233-4455-6677-8899-AABBCCDDEEFF"));
    assert!(matches!(outcome, Err(ValueError::NonCanonicalScalar { ty: "uuid", .. })));
}

#[test]
fn uuid_uppercase_input_normalizes_at_authoring_boundary() -> Result<(), ValueError> {
    // SPEC-ISSUES item 2: the human-authoring boundary stays lenient and
    // canonicalizes on decode, so a value read back always renders canonically.
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
fn none_wire_is_json_null_not_a_sentinel_object() {
    // SPEC-ISSUES item 29: `none` is absence and carries no wire sentinel. The
    // `{ "$none": true }` object is gone; a positional `none` is JSON `null`.
    assert_eq!(Value::None.to_canonical_json_string(), "null");
}

#[test]
fn none_sentinel_object_is_now_an_ordinary_json_value() -> Result<(), ValueError> {
    // SPEC-ISSUES item 29: with the sentinel removed, a `json` object that happens
    // to be `{ "$none": true }` is an ordinary present value and round-trips as
    // itself (the round-trip bug the item describes is fixed).
    let object = serde_json::json!({ "$none": true });
    let value = Type::Json.decode(&object)?;
    assert_ne!(value, Value::None);
    assert_eq!(value.to_canonical_json_string(), "{\"$none\":true}");
    // Under `optional<json>`, a present `{ "$none": true }` object decodes to that
    // object, not to `none`.
    let opt = Type::Optional(Box::new(Type::Json)).decode(&object)?;
    assert_eq!(opt, value);
    Ok(())
}

#[test]
fn json_null_is_preserved_and_distinct_from_none() -> Result<(), ValueError> {
    let value = Type::Json.decode(&serde_json::json!(null))?;
    assert_eq!(value.to_canonical_json_string(), "null");
    assert_ne!(value, Value::None);
    Ok(())
}

#[test]
fn optional_json_present_null_is_json_null_not_none() -> Result<(), ValueError> {
    // A.7 / item 29: under `optional<json>`, a present JSON `null` is the JSON
    // value `null` (a present value), never `none`. `none` there is absence, and
    // absence is an omitted member (exercised by the corpus), never a wire value.
    let value = Type::Optional(Box::new(Type::Json)).decode(&serde_json::json!(null))?;
    assert_eq!(value, Value::Json(liasse_value::Json::Null));
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
fn json_number_scale_is_bounded_like_decimal() -> Result<(), serde_json::Error> {
    // SPEC-ISSUES item 28 / A.7: a number inside a `json` value is bounded by the
    // same scale limit as an A.6 `decimal`. An extreme exponent is rejected at the
    // decode boundary with the same diagnostic class as an over-scale decimal, so
    // A.7 canonicalization can never materialize a multi-gigabyte digit string.
    // (`arbitrary_precision` keeps the exact exponent, so it reaches the bound.)
    for source in [
        "1E-2000000000",                    // billions of fractional digits
        "1E+2000000000",                    // billions of trailing zeros
        r#"{"a":[1,{"b":1E-2000000000}]}"#, // applies recursively through arrays/objects
    ] {
        let wire = serde_json::from_str::<serde_json::Value>(source)?;
        assert!(
            matches!(Type::Json.decode(&wire), Err(ValueError::DecimalScaleOutOfRange { .. })),
            "over-scale json number `{source}` must be rejected at decode"
        );
    }
    // A number within the bound (a large but ordinary scale) still decodes.
    let ok = serde_json::from_str::<serde_json::Value>("0.00000001")?;
    assert!(Type::Json.decode(&ok).is_ok());
    Ok(())
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
