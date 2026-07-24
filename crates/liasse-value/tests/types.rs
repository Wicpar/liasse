//! Key eligibility (Annex A.8). The enumerated eligible set is re-derived from
//! SPEC.md lines 4468–4475, never echoed from the implementation.

use liasse_value::{EnumType, RefTarget, StructType, Type, ValueError};

#[test]
fn enumerated_scalar_types_are_key_eligible() -> Result<(), ValueError> {
    // A.8: text, bool, int, decimal, bytes, uuid, date, timestamp, duration, enum.
    let eligible = [
        Type::Text,
        Type::Bool,
        Type::Int,
        Type::Decimal,
        Type::Bytes,
        Type::Uuid,
        Type::Date,
        Type::timestamp(),
        Type::Duration,
        Type::Enum(EnumType::new(["a".into(), "b".into()])?),
    ];
    for ty in eligible {
        assert!(ty.is_key_eligible(), "{} must be key-eligible", ty.name());
    }
    Ok(())
}

#[test]
fn ref_is_not_key_eligible_even_with_an_eligible_target() {
    // A.8 (lines 4468–4473) enumerates the eligible set and omits `ref`; §5.6
    // gives a ref a target key type but never lists `ref` among key-eligible
    // base types. So a ref field is not itself a key component — regardless of
    // how eligible its target key is.
    let scalar_ref = Type::Ref(RefTarget::Scalar(Box::new(Type::Uuid)));
    assert!(!scalar_ref.is_key_eligible());

    let composite_ref = Type::Ref(RefTarget::Composite(vec![
        ("region".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]));
    assert!(!composite_ref.is_key_eligible());
}

#[test]
fn excluded_types_are_not_key_eligible() {
    // Line 4475 excludes optionals, JSON, blobs, sets, maps, and views; `period`
    // is likewise absent from the enumerated set.
    let excluded = [
        Type::Json,
        Type::Blob,
        Type::Period,
        Type::Optional(Box::new(Type::Text)),
        Type::Set(Box::new(Type::Text)),
        Type::Map(Box::new(Type::Text), Box::new(Type::Int)),
        Type::View(Box::new(Type::Text)),
    ];
    for ty in excluded {
        assert!(!ty.is_key_eligible(), "{} must not be key-eligible", ty.name());
    }
}

#[test]
fn struct_key_eligibility_follows_its_fields() {
    // A struct is eligible only when every field is a required key-eligible type.
    let all_eligible = Type::Struct(StructType::new([
        ("country".to_owned(), Type::Text),
        ("code".to_owned(), Type::Int),
    ]));
    assert!(all_eligible.is_key_eligible());

    // A ref field is not key-eligible, so the enclosing struct is not either.
    let has_ref_field = Type::Struct(StructType::new([
        ("owner".to_owned(), Type::Ref(RefTarget::Scalar(Box::new(Type::Uuid)))),
    ]));
    assert!(!has_ref_field.is_key_eligible());

    // An optional field disqualifies the struct as well.
    let has_optional_field = Type::Struct(StructType::new([
        ("nickname".to_owned(), Type::Optional(Box::new(Type::Text))),
    ]));
    assert!(!has_optional_field.is_key_eligible());
}

#[test]
fn every_value_type_today_is_copyable() {
    // SPEC §8.5: no move-only type exists yet, so every constructible value type is
    // copyable — `=` may duplicate it. This pins the baseline the forthcoming
    // move-only `module` type will break by returning `false` from `is_copyable`.
    let scalars = [
        Type::Text,
        Type::Bool,
        Type::Int,
        Type::Decimal,
        Type::Bytes,
        Type::Uuid,
        Type::Date,
        Type::timestamp(),
        Type::Duration,
        Type::Period,
        Type::Json,
        Type::Blob,
        Type::Ref(RefTarget::Scalar(Box::new(Type::Uuid))),
    ];
    for ty in scalars {
        assert!(ty.is_copyable(), "{} must be copyable today", ty.name());
    }
}

#[test]
fn compound_copyability_delegates_to_components() {
    // A struct, set, map, optional, and view are copyable exactly when their
    // components are — the delegation that will make any container of a future
    // move-only value move-only automatically.
    let struct_of_copyables = Type::Struct(StructType::new([
        ("name".to_owned(), Type::Text),
        ("count".to_owned(), Type::Int),
    ]));
    assert!(struct_of_copyables.is_copyable());
    assert!(Type::Set(Box::new(Type::Uuid)).is_copyable());
    assert!(Type::Map(Box::new(Type::Text), Box::new(Type::Int)).is_copyable());
    assert!(Type::Optional(Box::new(Type::Decimal)).is_copyable());
    assert!(Type::View(Box::new(Type::Text)).is_copyable());
}
