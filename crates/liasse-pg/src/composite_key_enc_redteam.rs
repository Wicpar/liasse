//! RED TEAM — explicit all-pairs `key_enc` order/canonicality + `value_codec`
//! round-trip battery for the bare `Value::Composite` arm.
//!
//! The companion [`key_enc_proptest`](crate::key_enc_proptest) samples composites
//! only at random arity 2..4, and the explicit
//! [`key_enc_boundary_test`](crate::key_enc_boundary_test) enumerates `Ref::composite`
//! and multi-component `KeyValue`s but never the bare `Value::Composite` variant —
//! so the corners a random sweep is least likely to hit (empty/arity-1 tuples,
//! shared-prefix arity ladders, none/scale/precision-variant components, nested
//! composite-of-composite, and the `map(16) < composite(17) < none(0xFF)`
//! cross-rank neighbours) had no deterministic witness. This battery closes that
//! gap over the full Cartesian product:
//!
//! ```text
//! sign(memcmp(encode a, encode b)) == sign(a.cmp(b))       (Annex B order)
//! (encode a == encode b)           == (a == b)             (canonicality)
//! decode(encode v)                 == v                    (value_codec round-trip)
//! ```
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use core::cmp::Ordering;

use liasse_store::KeyValue;
use liasse_value::{Decimal, Integer, Precision, Ref, Struct, Text, Timestamp, Value};

use crate::key_enc;
use crate::value_codec;

fn enc(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    key_enc::encode_value(value, &mut out);
    out
}

fn sign(o: Ordering) -> i8 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn dec(t: &str) -> Value {
    Value::Decimal(Decimal::parse(t).expect("dec"))
}
fn text(t: &str) -> Value {
    Value::Text(Text::new(t))
}
fn comp(v: Vec<Value>) -> Value {
    Value::Composite(v)
}
fn ts(count: i128, p: Precision) -> Value {
    Value::Timestamp(Timestamp::new(count, p))
}

fn battery() -> Vec<(&'static str, Value)> {
    vec![
        // arity ladder + shared-prefix (the corner random draws never hit)
        ("[]", comp(vec![])),
        ("[0]", comp(vec![int(0)])),
        ("[0,0]", comp(vec![int(0), int(0)])),
        ("[0,1]", comp(vec![int(0), int(1)])),
        ("[0,1,2]", comp(vec![int(0), int(1), int(2)])),
        ("[1]", comp(vec![int(1)])),
        ("[a]", comp(vec![text("a")])),
        ("[a,b]", comp(vec![text("a"), text("b")])),
        ("['',0]", comp(vec![text(""), int(0)])),
        ("[a\\0,0]", comp(vec![text("a\u{0}"), int(0)])),
        // none components
        ("[none]", comp(vec![Value::None])),
        ("[none,0]", comp(vec![Value::None, int(0)])),
        ("[0,none]", comp(vec![int(0), Value::None])),
        // scale/precision-variant components: MUST collapse to equal
        ("[dec1.0]", comp(vec![dec("1.0")])),
        ("[dec1.00]", comp(vec![dec("1.00")])),
        ("[dec1]", comp(vec![dec("1")])),
        ("[ts1000ms]", comp(vec![ts(1000, Precision::Millis)])),
        ("[ts1s]", comp(vec![ts(1, Precision::Seconds)])),
        ("[dec1.0,0]", comp(vec![dec("1.0"), int(0)])),
        ("[dec1.00,0]", comp(vec![dec("1.00"), int(0)])),
        // nested composite of composite
        ("[[0]]", comp(vec![comp(vec![int(0)])])),
        ("[[0],1]", comp(vec![comp(vec![int(0)]), int(1)])),
        ("[[0,1]]", comp(vec![comp(vec![int(0), int(1)])])),
        // composite containing struct / ref
        (
            "[struct{a:0,b:1}]",
            comp(vec![Value::Struct(Struct::new([
                (Text::new("a"), int(0)),
                (Text::new("b"), int(1)),
            ]))]),
        ),
        ("[ref-scalar 0]", comp(vec![Value::Ref(Ref::scalar(int(0)))])),
        ("[ref-comp[0,1]]", comp(vec![Value::Ref(Ref::composite(vec![int(0), int(1)]))])),
        // cross-rank neighbours: Map(16) < Composite(17) < None(255); Struct(14); Ref(11)
        ("map{}", Value::Map(std::collections::BTreeMap::new())),
        ("set{}", Value::Set(std::collections::BTreeSet::new())),
        ("struct{}", Value::Struct(Struct::new([]))),
        ("ref 0", Value::Ref(Ref::scalar(int(0)))),
        ("none", Value::None),
        ("int 0", int(0)),
    ]
}

#[test]
fn composite_key_enc_order_and_canonicality_all_pairs() {
    let items = battery();
    let mut failures = Vec::new();
    for (na, a) in &items {
        for (nb, b) in &items {
            let (ea, eb) = (enc(a), enc(b));
            // 1. byte order must match Annex B Value::Ord
            if sign(ea.cmp(&eb)) != sign(a.cmp(b)) {
                failures.push(format!(
                    "ORDER {na} vs {nb}: bytes={:?} value={:?}\n  ea={:02x?}\n  eb={:02x?}",
                    sign(ea.cmp(&eb)),
                    sign(a.cmp(b)),
                    ea,
                    eb
                ));
            }
            // 2. canonicality: byte-equal iff value-equal
            if (ea == eb) != (a == b) {
                failures.push(format!(
                    "CANON {na} vs {nb}: bytes-eq={} value-eq={}",
                    ea == eb,
                    a == b
                ));
            }
        }
    }
    assert!(failures.is_empty(), "key_enc divergences:\n{}", failures.join("\n"));
}

#[test]
fn composite_value_codec_round_trips() {
    let mut failures = Vec::new();
    for (name, v) in battery() {
        let wire = value_codec::encode(&v);
        match value_codec::decode(&wire) {
            Ok(back) => {
                if back != v {
                    failures.push(format!("VALUE {name}: {v:?} -> {back:?} (wire {wire})"));
                }
            }
            Err(e) => failures.push(format!("VALUE {name}: decode error {e:?} (wire {wire})")),
        }
        // key column round-trip (canonical): decode(encode_key(v)) == v under Annex B
        let kwire = value_codec::encode_key(&v);
        match value_codec::decode(&kwire) {
            Ok(back) => {
                if back != v {
                    failures.push(format!("KEY {name}: {v:?} -> {back:?} (wire {kwire})"));
                }
            }
            Err(e) => failures.push(format!("KEY {name}: decode error {e:?} (wire {kwire})")),
        }
    }
    assert!(failures.is_empty(), "value_codec divergences:\n{}", failures.join("\n"));
}

#[test]
fn composite_as_keyvalue_component_all_pairs() {
    // A KeyValue whose components are themselves Value::Composite / mixed, to exercise
    // the plain-concat framing (encode_key_value) with composite units.
    let kv = |first: Value, rest: Vec<Value>| KeyValue::composite(first, rest);
    let items: Vec<(&str, KeyValue)> = vec![
        ("[[0]]", kv(comp(vec![int(0)]), vec![])),
        ("[[0],[1]]", kv(comp(vec![int(0)]), vec![comp(vec![int(1)])])),
        ("[[0,1]]", kv(comp(vec![int(0), int(1)]), vec![])),
        ("[[0],int1]", kv(comp(vec![int(0)]), vec![int(1)])),
        ("[int0,[1]]", kv(int(0), vec![comp(vec![int(1)])])),
        ("[[dec1.0]]", kv(comp(vec![dec("1.0")]), vec![])),
        ("[[dec1.00]]", kv(comp(vec![dec("1.00")]), vec![])),
        ("[[none]]", kv(comp(vec![Value::None]), vec![])),
    ];
    let mut failures = Vec::new();
    for (na, a) in &items {
        for (nb, b) in &items {
            let ea = key_enc::encode_key_value(a);
            let eb = key_enc::encode_key_value(b);
            if sign(ea.cmp(&eb)) != sign(a.cmp(b)) {
                failures.push(format!("KVORDER {na} vs {nb}"));
            }
            if (ea == eb) != (a == b) {
                failures.push(format!("KVCANON {na} vs {nb}: bytes-eq={} value-eq={}", ea == eb, a == b));
            }
        }
    }
    assert!(failures.is_empty(), "keyvalue composite divergences:\n{}", failures.join("\n"));
}
