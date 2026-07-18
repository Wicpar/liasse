//! Deterministic total order (Annex B). Every expected ordering is re-derived
//! from B's rules, then a deliberately scrambled input is sorted and checked.

use std::collections::BTreeSet;

use liasse_value::{
    Bytes, Duration, EnumType, Integer, Json, Precision, Ref, Struct, Text, Timestamp, Value,
    ValueError,
};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

/// Assert that `values` is already in ascending order and that sorting a
/// reversed copy reproduces it.
fn assert_ascending(values: Vec<Value>) {
    let mut scrambled: Vec<Value> = values.iter().rev().cloned().collect();
    scrambled.sort();
    assert_eq!(scrambled, values);
    for pair in values.windows(2) {
        if let [lower, higher] = pair {
            assert!(lower < higher, "expected {lower:?} < {higher:?}");
        }
    }
}

#[test]
fn bool_false_before_true() {
    assert_ascending(vec![Value::Bool(false), Value::Bool(true)]);
}

#[test]
fn int_mathematical_order_with_negatives() {
    assert_ascending(vec![int(-10), int(-2), int(0), int(5), int(42)]);
}

#[test]
fn decimal_numeric_equality_is_scale_insensitive() -> Result<(), ValueError> {
    use liasse_value::Decimal;
    let one_point_zero = Value::Decimal(Decimal::parse("1.0")?);
    let one_point_zero_zero = Value::Decimal(Decimal::parse("1.00")?);
    // B.1: numerically equal canonical values compare equal.
    assert_eq!(one_point_zero, one_point_zero_zero);
    // 1.05 < 1.5 even though "1.5" < "1.05" as text.
    assert!(Value::Decimal(Decimal::parse("1.05")?) < Value::Decimal(Decimal::parse("1.5")?));
    Ok(())
}

#[test]
fn timestamp_orders_after_precision_normalization() {
    // 1s == 1000ms; 1s < 1500ms; negatives sort before the epoch.
    let one_second = Value::Timestamp(Timestamp::new(1, Precision::Seconds));
    let thousand_millis = Value::Timestamp(Timestamp::new(1_000, Precision::Millis));
    assert_eq!(one_second, thousand_millis);
    assert_ascending(vec![
        Value::Timestamp(Timestamp::new(-1, Precision::Seconds)),
        Value::Timestamp(Timestamp::new(1, Precision::Seconds)),
        Value::Timestamp(Timestamp::new(1_500, Precision::Millis)),
    ]);
}

#[test]
fn text_uses_unicode_scalar_value_order_not_utf16() {
    // Codepoints: 'z' = U+007A, U+FFFD, U+10000. Scalar order is 7A < FFFD < 10000.
    // A UTF-16 comparison would misplace the astral character.
    assert_ascending(vec![text("z"), text("\u{FFFD}"), text("\u{10000}")]);
}

#[test]
fn bytes_use_unsigned_lexicographic_order() {
    let of = |bytes: &[u8]| Value::Bytes(Bytes::new(bytes.to_vec()));
    // 0x80 and 0xFF are above 0x00/0x7F under unsigned order; shorter after
    // a shared prefix.
    assert_ascending(vec![
        of(&[0x00]),
        of(&[0x41]),
        of(&[0x41, 0x00]),
        of(&[0x7F]),
        of(&[0x80]),
        of(&[0xFF]),
    ]);
}

#[test]
fn enum_uses_declaration_order_not_lexicographic() -> Result<(), ValueError> {
    // Declared red, green, blue: declaration order, though blue < green < red as text.
    let ty = EnumType::new(["red".into(), "green".into(), "blue".into()])?;
    assert_ascending(vec![
        Value::Enum(ty.parse("red")?),
        Value::Enum(ty.parse("green")?),
        Value::Enum(ty.parse("blue")?),
    ]);
    Ok(())
}

#[test]
fn enum_values_from_distinct_declarations_are_not_conflated() -> Result<(), ValueError> {
    // Both are the ordinal-0 label of their declaration, but the labels differ,
    // so they are different values — comparing ordinals alone would wrongly
    // report them equal (B.1 orders a single enum column, never mixes columns).
    let colors = EnumType::new(["red".into(), "green".into()])?;
    let sizes = EnumType::new(["small".into(), "large".into()])?;
    let red = Value::Enum(colors.parse("red")?);
    let small = Value::Enum(sizes.parse("small")?);
    assert_eq!(colors.parse("red")?.ordinal(), sizes.parse("small")?.ordinal());
    assert_ne!(red, small);

    // Within one declaration the label tiebreak leaves declaration order intact.
    assert!(Value::Enum(colors.parse("red")?) < Value::Enum(colors.parse("green")?));
    Ok(())
}

#[test]
fn duration_orders_by_exact_elapsed_not_text() -> Result<(), ValueError> {
    // Text sort would put "PT10M" < "PT1H" < "PT9M"; elapsed order is 9M<10M<1H.
    assert_ascending(vec![
        Value::Duration(Duration::parse("PT9M")?),
        Value::Duration(Duration::parse("PT10M")?),
        Value::Duration(Duration::parse("PT1H")?),
    ]);
    Ok(())
}

#[test]
fn optional_none_sorts_last_ascending() {
    // B.2: present values ascending, then none.
    assert_ascending(vec![int(1), int(2), Value::None]);
}

#[test]
fn json_type_rank_and_null_vs_none() {
    use liasse_value::bigdecimal::BigDecimal;
    // B.3: null < bool < number < string < array < object.
    assert_ascending(vec![
        Value::Json(Json::Null),
        Value::Json(Json::Bool(false)),
        Value::Json(Json::Number(BigDecimal::from(0))),
        Value::Json(Json::String(String::new())),
        Value::Json(Json::Array(Vec::new())),
        Value::Json(Json::Object(std::collections::BTreeMap::new())),
    ]);
    // JSON null is a present value; the Liasse none still sorts after it.
    assert!(Value::Json(Json::Null) < Value::None);
}

#[test]
fn json_object_key_dominates_value() {
    use liasse_value::bigdecimal::BigDecimal;
    let obj = |pairs: &[(&str, i64)]| {
        let map = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Json::Number(BigDecimal::from(*v))))
            .collect();
        Value::Json(Json::Object(map))
    };
    // {a:1} < {a:1,b:0} < {a:2} < {b:1}: key "a" beats key "b" even when b's
    // value is smaller (B.3 (key,value) pair order).
    assert_ascending(vec![
        obj(&[("a", 1)]),
        obj(&[("a", 1), ("b", 0)]),
        obj(&[("a", 2)]),
        obj(&[("b", 1)]),
    ]);
}

#[test]
fn set_compares_as_sorted_member_sequence() {
    let set = |members: &[&str]| {
        let inner: BTreeSet<Value> = members.iter().map(|m| text(m)).collect();
        Value::Set(inner)
    };
    // {c,a}=[a,c], {b,a}=[a,b], {c,b}=[b,c]; sequences: [a,b]<[a,c]<[b,c].
    assert_ascending(vec![set(&["b", "a"]), set(&["c", "a"]), set(&["c", "b"])]);
}

#[test]
fn struct_compares_fields_in_field_name_text_order() {
    // B.4: fields compared in canonical (text) field-name order: `a` before `b`.
    // Declared (b, a) to defeat a declaration-order comparator.
    let point = |a: i64, b: i64| {
        Value::Struct(Struct::new([
            (Text::new("b"), int(b)),
            (Text::new("a"), int(a)),
        ]))
    };
    // Compare by a first: (a=1,b=9) < (a=2,b=0) because 1 < 2 dominates b.
    assert!(point(1, 9) < point(2, 0));
    // Equal a, compare b: (a=1,b=0) < (a=1,b=5).
    assert!(point(1, 0) < point(1, 5));
}

#[test]
fn composite_ref_second_component_uses_int_order() {
    // B.4: components compared in $key order, each in its own type order, so
    // the int second component uses numeric order (2 < 10), not text.
    let key = |region: &str, code: i64| Value::Ref(Ref::composite(vec![text(region), int(code)]));
    assert_ascending(vec![
        key("eu", 2),
        key("eu", 10),
        key("us", 1),
    ]);
}

#[test]
fn composite_key_orders_positionally_in_key_order_not_field_name_order() {
    // B.4 composite key: lexicographic components in $key order — distinct from a
    // struct's field-name order. With $key:[region, code], eu:z vs us:a orders
    // eu:z < us:a by $key order (region: eu < us), the OPPOSITE of the field-name
    // order a struct {code, region} would use (code: a < z => us:a first). The
    // positional Value::Composite carrier realizes the composite-key rule.
    let composite = |region: &str, code: &str| Value::Composite(vec![text(region), text(code)]);
    assert_ascending(vec![composite("eu", "z"), composite("us", "a")]);

    // The very same components as a name-sorted struct compare in the OPPOSITE
    // order (by `code` first), proving the two B.4 rows are distinct rules — a
    // composite key is not ordered like a struct of the same fields.
    let as_struct = |region: &str, code: &str| {
        Value::Struct(Struct::new([
            (Text::new("region"), text(region)),
            (Text::new("code"), text(code)),
        ]))
    };
    assert!(as_struct("us", "a") < as_struct("eu", "z"));
    assert!(composite("eu", "z") < composite("us", "a"));
}

#[test]
fn period_fixed_sorts_before_calendar() -> Result<(), ValueError> {
    use liasse_value::{CalendarPeriodBuilder, Period};
    let fixed_small = Value::Period(Box::new(Period::Fixed(Duration::parse("P1D")?)));
    let fixed_large = Value::Period(Box::new(Period::Fixed(Duration::parse("P7D")?)));
    let builder = CalendarPeriodBuilder {
        months: 1,
        ..CalendarPeriodBuilder::default()
    };
    let calendar = Value::Period(Box::new(Period::Calendar(builder.build()?)));
    // Fixed by exact duration, then all fixed periods before any calendar one.
    assert_ascending(vec![fixed_small, fixed_large, calendar]);
    Ok(())
}

#[test]
fn blob_descriptor_absent_name_sorts_after_present_name() -> Result<(), ValueError> {
    // B.4 / SPEC-ISSUES item 30: descriptors equal on `$sha512`, `$bytes`,
    // `$media` order by the optional `$name` with `none` LAST — a named
    // descriptor sorts before an otherwise-equal unnamed one.
    use liasse_value::{BlobDescriptor, MediaType, Sha512};
    let hex = "ab".repeat(64);
    let named = Value::Blob(Box::new(BlobDescriptor::new(
        Sha512::parse(&hex)?,
        1,
        MediaType::new("application/pdf"),
        Some("a.pdf".to_owned()),
    )));
    let unnamed = Value::Blob(Box::new(BlobDescriptor::new(
        Sha512::parse(&hex)?,
        1,
        MediaType::new("application/pdf"),
        None,
    )));
    assert_ascending(vec![named, unnamed]);
    Ok(())
}

#[test]
fn calendar_period_absent_zone_sorts_after_present_zone() -> Result<(), ValueError> {
    // B.4 / SPEC-ISSUES item 30: calendar periods equal on the leading magnitude
    // and time members order by the optional `zone` with `none` LAST.
    use liasse_value::{CalendarPeriodBuilder, Period};
    let zoned = Value::Period(Box::new(Period::Calendar(
        CalendarPeriodBuilder { months: 1, zone: Some("Europe/Paris".to_owned()), ..CalendarPeriodBuilder::default() }
            .build()?,
    )));
    let zoneless = Value::Period(Box::new(Period::Calendar(
        CalendarPeriodBuilder { months: 1, zone: None, ..CalendarPeriodBuilder::default() }.build()?,
    )));
    assert_ascending(vec![zoned, zoneless]);
    Ok(())
}
