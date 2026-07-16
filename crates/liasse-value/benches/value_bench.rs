//! Benchmarks for the two hot abstractions of `liasse-value`: the total-order
//! comparison the runtime sorts with constantly (Annex B), and the canonical
//! wire encode/decode of a composite row (Annex A).

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

use liasse_value::bigdecimal::BigDecimal;
use liasse_value::num_bigint::BigInt;
use liasse_value::{
    Bytes, Decimal, Duration, Integer, Json, Precision, StructType, Text, Timestamp, Type, Value,
};

/// A representative mix of value types to exercise cross-type rank comparison
/// and within-type comparison together.
fn mixed_values() -> Vec<Value> {
    let mut values = Vec::new();
    for n in (-200i64..200).rev() {
        values.push(Value::Int(Integer::from(n)));
        values.push(Value::Decimal(Decimal::from(BigInt::from(n))));
        values.push(Value::Text(Text::new(format!("row-{n:04}"))));
        values.push(Value::Timestamp(Timestamp::new(
            i128::from(n) * 7,
            Precision::Micros,
        )));
        values.push(Value::Duration(Duration::from_nanos(i128::from(n))));
        values.push(Value::Json(Json::Number(BigDecimal::from(n))));
    }
    values.push(Value::None);
    values
}

/// A composite row: the shape a projection encodes and a mutation decodes.
fn row_type() -> Type {
    Type::Struct(StructType::new([
        ("id".to_owned(), Type::Uuid),
        ("name".to_owned(), Type::Text),
        ("amount".to_owned(), Type::Decimal),
        ("at".to_owned(), Type::timestamp()),
        ("tags".to_owned(), Type::Set(Box::new(Type::Text))),
        ("weight".to_owned(), Type::Optional(Box::new(Type::Int))),
    ]))
}

fn row_wire() -> serde_json::Value {
    serde_json::json!({
        "id": "00112233-4455-6677-8899-aabbccddeeff",
        "name": "Sample Row",
        "amount": "1234.5600",
        "at": "1717171717000000",
        "tags": ["gamma", "alpha", "beta"],
    })
}

fn bench_total_order(c: &mut Criterion) {
    let values = mixed_values();
    c.bench_function("sort_mixed_values", |b| {
        b.iter_batched(
            || values.clone(),
            |mut v| {
                v.sort();
                black_box(v);
            },
            BatchSize::LargeInput,
        );
    });

    let bytes: Vec<Value> = (0u32..500)
        .map(|n| Value::Bytes(Bytes::new(n.to_be_bytes().to_vec())))
        .collect();
    c.bench_function("sort_bytes", |b| {
        b.iter_batched(
            || bytes.clone(),
            |mut v| {
                v.sort();
                black_box(v);
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_wire(c: &mut Criterion) {
    let ty = row_type();
    let wire = row_wire();

    c.bench_function("decode_composite_row", |b| {
        b.iter(|| {
            if let Ok(value) = ty.decode(black_box(&wire)) {
                black_box(value);
            }
        });
    });

    if let Ok(value) = ty.decode(&wire) {
        c.bench_function("encode_composite_row", |b| {
            b.iter(|| black_box(value.to_canonical_json_string()));
        });
    }
}

criterion_group!(benches, bench_total_order, bench_wire);
criterion_main!(benches);
