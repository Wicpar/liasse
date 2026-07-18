//! RED TEAM — the self-describing value codec's SUBTLE arms driven through the
//! real PostgreSQL `jsonb` value column, checked for a pg-vs-`MemoryStore`
//! divergence live AND across a durable reopen.
//!
//! The shared `value_wire` round-trip already stores one *tame* value of most
//! variants and compares them with `Value::Eq`. Two blind spots remain, and this
//! case attacks both:
//!
//! 1. **`Value::Eq` is Annex-B `Ord`, which is scale/precision-INSENSITIVE.** A
//!    `timestamp` that lost its declared precision on the pg round-trip would
//!    still compare EQUAL to the reference — the divergence is invisible to an
//!    `Ord` parity check. So every row here is additionally compared on its
//!    **canonical JSON text** (`Value::to_canonical_json_string`, A.7), the
//!    byte-exact serialization that distinguishes precision-variant timestamps
//!    (`1000`ms vs `1`s) and structure. Since SPEC-ISSUES #1 a `decimal`'s
//!    canonical text is minimal scale, so `1`, `1.0`, and `1.00` share one
//!    spelling (`1`); the text axis then verifies both backends canonicalize a
//!    scale-variant decimal to that same spelling, catching a backend that would
//!    emit a non-minimal decimal string. The reference is the externally-known
//!    input, never the store's own answer.
//! 2. **Untested variants and raw-number-in-`jsonb` paths.** `value_wire` never
//!    stores a `Value::Blob` (the B.4 four-tuple descriptor), a `Value::Date`, or
//!    a `Value::Composite` as a row value, and never exercises the codec arms that
//!    emit an actual `jsonb` NUMBER — the `enum` ordinal and the `period` calendar
//!    magnitudes — which PostgreSQL's `jsonb` numeric normalization and
//!    `serde_json`'s `arbitrary_precision` reader both touch. This battery drives
//!    all of them at their edges: `u64::MAX` blob byte-counts, a `U+0000` in a
//!    blob name/media, `i64::MIN`/`i64::MAX` calendar magnitudes, a `u32::MAX`
//!    enum ordinal, `i128::MIN`/`MAX` durations, mixed-scale decimals, and one
//!    instant at two precisions.
//!
//! Cited: A.7 (canonical wire/JSON), A.1/A.4 (`decimal` scale, `period` fields),
//! A.9/B.4 (`composite`, `blob` four-tuple ordering), B.1/B.3 (`timestamp`
//! precision, `json` ladder), §22.7/§19.2 (a reopen rebuilds an identical
//! projection; snapshots fold the durable log). Overarching gate: pg must equal
//! `MemoryStore` observably, and on the sharper canonical-text axis too.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{
    BlobDescriptor, Bytes, CalendarPeriodBuilder, Date, Decimal, Duration, EnumValue, Integer, Json,
    MediaType, Period, Precision, Ref, Sha512, Struct, Text, Timestamp, Uuid, Value,
};

fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn dec(v: &str) -> Value {
    Value::Decimal(Decimal::parse(v).expect("decimal parses"))
}

/// A 128-hex SHA-512 from a repeated byte, so distinct contents get distinct
/// descriptors without a real hash.
fn sha(byte: u8) -> Sha512 {
    Sha512::parse(&format!("{byte:02x}").repeat(64)).expect("64-byte digest is valid hex")
}

fn blob(byte: u8, count: u64, media: &str, name: Option<&str>) -> Value {
    Value::Blob(Box::new(BlobDescriptor::new(
        sha(byte),
        count,
        MediaType::new(media),
        name.map(str::to_owned),
    )))
}

/// A calendar period exercising every policy variant, a zone carrying a `U+0000`,
/// and `i64` extreme magnitudes — the raw-number `jsonb` arm at its edge.
fn calendar_period() -> Value {
    let mut builder = CalendarPeriodBuilder {
        years: i64::MIN,
        months: i64::MAX,
        weeks: -7,
        days: 0,
        time: Duration::from_nanos(123_456_789),
        zone: Some("Zone\u{0}Region".to_owned()),
        ..CalendarPeriodBuilder::default()
    };
    builder.set_overflow("reject").expect("overflow keyword");
    builder.set_ambiguous("later").expect("ambiguous keyword");
    builder.set_missing("backward").expect("missing keyword");
    Value::Period(Box::new(Period::Calendar(builder.build().expect("magnitude present"))))
}

/// A `json` value spanning the full B.3 ladder: `null < bool < number < string <
/// array < object`, with a scale-bearing number, a `U+0000` string, an empty
/// object, and nesting.
fn json_ladder() -> Value {
    let wire = serde_json::json!({
        "": [serde_json::Value::Null, false, true],
        "nums": ["not-a-number-key-tag"],
        "z\u{0}k": { "inner": [1, 2, 3] },
        "empty_obj": {},
        "empty_arr": []
    });
    let mut value = Json::from_wire(&wire).expect("json from wire");
    // Splice a scale-bearing number in so the number arm carries a fraction the
    // A.7 canonicalization must normalize identically on both backends.
    if let Json::Object(map) = &mut value {
        map.insert(
            "scaled".to_owned(),
            Json::from_wire(&"1.500".parse::<serde_json::Value>().expect("num")).expect("json num"),
        );
    }
    Value::Json(value)
}

/// A `map` whose keys are themselves `composite` tuples, inserted OUT of B.4
/// order, so the codec's map arm must land them in Annex-B order on both sides.
fn composite_keyed_map() -> Value {
    let entries = [
        (Value::Composite(vec![int(2), text("a")]), text("second-region")),
        (Value::Composite(vec![int(1), text("b")]), text("first-region-b")),
        (Value::Composite(vec![int(1), text("a")]), text("first-region-a")),
    ];
    Value::Map(entries.into_iter().collect())
}

/// The adversarial battery: `(int key, value)`. Keys are ascending so the scan
/// order equals this order and is deterministic on both backends.
fn battery() -> Vec<(i64, Value)> {
    vec![
        // --- blob four-tuple (B.4) at its edges ---
        (0, blob(0x00, 12_345, "application/pdf", None)),
        (1, blob(0xff, 0, "text/plain", Some(""))),
        (2, blob(0xa5, u64::MAX, "application/x-thing", Some("a\u{0}b invoice.pdf"))),
        (3, blob(0x5a, 1, "text/pl\u{0}ain", Some("named"))),
        // --- date (never round-tripped as a value before) ---
        (4, Value::Date(Date::parse("2026-07-18").expect("date"))),
        (5, Value::Date(Date::parse("0001-01-01").expect("min date"))),
        // --- period, fixed and calendar ---
        (6, Value::Period(Box::new(Period::Fixed(Duration::from_nanos(i128::MAX))))),
        (7, calendar_period()),
        // --- json B.3 ladder ---
        (8, json_ladder()),
        (9, Value::Json(Json::Null)),
        // --- map with composite keys, adversarial insert order ---
        (10, composite_keyed_map()),
        // --- set: cross-type members + a scale-bearing decimal ---
        (11, Value::Set([dec("1.500"), int(2), text("s"), Value::Bool(true)].into_iter().collect())),
        // --- mixed-scale decimals: Ord-EQUAL, and (SPEC-ISSUES #1) canonical-text
        // EQUAL too — every scale variant renders minimal-scale "1"/"0" ---
        (12, dec("1.00")),
        (13, dec("1.0")),
        (14, dec("1")),
        (15, dec("-0")),
        // --- one instant at two precisions (Ord-equal, precision preserved) ---
        (16, Value::Timestamp(Timestamp::new(1000, Precision::Millis))),
        (17, Value::Timestamp(Timestamp::new(1, Precision::Seconds))),
        // --- enum: raw jsonb ordinal at u32::MAX, and a NUL label ---
        (18, Value::Enum(EnumValue::from_parts(u32::MAX, "z"))),
        (19, Value::Enum(EnumValue::from_parts(0, "\u{0}empty\u{0}"))),
        // --- duration i128 extremes ---
        (20, Value::Duration(Duration::from_nanos(i128::MIN))),
        (21, Value::Duration(Duration::from_nanos(i128::MAX))),
        // --- nested composite carrying a scale-bearing decimal ---
        (22, Value::Composite(vec![text("eu"), int(1), dec("1.50")])),
        // --- refs, scalar and composite, carrying scale / NUL ---
        (23, Value::Ref(Ref::scalar(dec("2.00")))),
        (24, Value::Ref(Ref::composite(vec![int(1), text("k\u{0}")]))),
        // --- struct with an omitted (none) field alongside present fields ---
        (
            25,
            Value::Struct(Struct::new([
                (Text::new("absent"), Value::None),
                (Text::new("flag"), Value::Bool(false)),
                (Text::new("n"), int(9)),
            ])),
        ),
        // --- text/bytes/uuid/none edges ---
        (26, text("a\u{0}b\\c\u{0}")),
        (27, Value::Bytes(Bytes::new(vec![0u8, 255, 0, 1]))),
        (28, Value::Uuid(Uuid::parse("00112233-4455-6677-8899-aabbccddeeff").expect("uuid"))),
        (29, Value::None),
    ]
}

fn collection() -> CollectionPath {
    CollectionPath::top(NameSegment::new("v"))
}
fn address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(NameSegment::new("v"), KeyValue::single(int(key))))
}

/// The identical op stream both backends run: insert the battery in two commits,
/// update three rows to fresh adversarial values, then delete two — so multiple
/// frontiers exist to fold and the update/delete op codecs carry subtle values.
fn apply_workload<S: InstanceStore>(store: &mut S) {
    let items = battery();
    let (first, second) = items.split_at(items.len() / 2);

    let mut txn = store.begin();
    for (key, value) in first {
        txn.insert(address(*key), value.clone()).unwrap();
    }
    txn.commit().unwrap();

    let mut txn = store.begin();
    for (key, value) in second {
        txn.insert(address(*key), value.clone()).unwrap();
    }
    txn.commit().unwrap();

    // Update three rows to DIFFERENT subtle values (exercise the update codec).
    let mut txn = store.begin();
    txn.update(&address(12), dec("3.000")).unwrap(); // scale 3, was "1.00"
    txn.update(&address(0), blob(0x11, 7, "image/png", Some("re\u{0}named"))).unwrap();
    txn.update(&address(8), Value::Json(Json::Bool(true))).unwrap();
    txn.commit().unwrap();

    // Delete two rows.
    let mut txn = store.begin();
    txn.delete(&address(29)).unwrap();
    txn.delete(&address(4)).unwrap();
    txn.commit().unwrap();
}

/// The externally-known final live state after the workload: the battery with the
/// three updates applied and the two deletes removed. This is the oracle every
/// assertion checks against — never a backend's own answer.
fn expected_final() -> Vec<(i64, Value)> {
    let mut map: std::collections::BTreeMap<i64, Value> = battery().into_iter().collect();
    map.insert(12, dec("3.000"));
    map.insert(0, blob(0x11, 7, "image/png", Some("re\u{0}named")));
    map.insert(8, Value::Json(Json::Bool(true)));
    map.remove(&29);
    map.remove(&4);
    map.into_iter().collect()
}

/// Every key the workload touches (present or deleted), so both presence and
/// absence are compared at each.
fn touched() -> Vec<i64> {
    battery().into_iter().map(|(k, _)| k).collect()
}

fn update(a: &Value) -> String {
    a.to_canonical_json_string()
}

/// A store's live projection must equal the externally-known `expected_final`,
/// on BOTH the `Ord` axis (`StoredRow`/`Value::Eq`) and the sharper A.7
/// canonical-JSON-text axis — the latter is what catches a lost decimal scale or
/// timestamp precision that `Ord` equality would hide.
fn assert_matches_oracle<S: InstanceStore>(store: &S, label: &str) {
    // Presence/absence + value equality at every touched address.
    let expected: std::collections::BTreeMap<i64, Value> = expected_final().into_iter().collect();
    for key in touched() {
        let got = store.row(&address(key)).expect("row read");
        match expected.get(&key) {
            None => assert!(got.is_none(), "{label}: key {key} must be absent, got {got:?}"),
            Some(want) => {
                let row = got.unwrap_or_else(|| panic!("{label}: key {key} must be present"));
                // Ord-axis equality.
                assert_eq!(
                    row.value(),
                    want,
                    "{label}: Ord-value mismatch at key {key}"
                );
                // Sharper canonical-text axis: distinguishes precision-variant
                // timestamps and structure that Ord equality collapses, and
                // confirms scale-variant decimals canonicalize to one spelling (#1).
                assert_eq!(
                    update(row.value()),
                    update(want),
                    "{label}: CANONICAL-TEXT mismatch at key {key} — a value that is Annex-B \
                     equal but canonically distinct (scale/precision/structure) survived on the \
                     reference but not here",
                );
            }
        }
    }

    // Scan order + payloads equal the oracle, order included.
    let live: Vec<(i64, Value)> = store
        .scan(&collection())
        .expect("scan")
        .into_iter()
        .map(|(addr, row)| (scan_key(&addr), row.value().clone()))
        .collect();
    let want: Vec<(i64, Value)> = expected_final();
    assert_eq!(live.len(), want.len(), "{label}: scan cardinality");
    for ((gk, gv), (wk, wv)) in live.iter().zip(want.iter()) {
        assert_eq!(gk, wk, "{label}: scan key order");
        assert_eq!(gv, wv, "{label}: scan Ord-value at key {wk}");
        assert_eq!(update(gv), update(wv), "{label}: scan CANONICAL-TEXT at key {wk}");
    }
}

/// Recover the int key of an address for readable scan comparisons.
fn scan_key(address: &RowAddress) -> i64 {
    let step = address.steps().last().expect("address has a step");
    let component = step.key().components().next().expect("single-int key");
    match component {
        Value::Int(i) => i.to_canonical_text().parse().expect("int key"),
        other => panic!("unexpected key component {other:?}"),
    }
}

/// pg and memory must agree at every frontier snapshot (folding the durable log)
/// and on the whole log — the §22.7/§19.2 durability gate, on both value axes.
fn assert_stores_agree<A: InstanceStore, B: InstanceStore>(a: &A, b: &B, label: &str) {
    assert_eq!(a.head(), b.head(), "{label}: head");

    let head = a.head().get();
    for f in 0..=head {
        let frontier = CommitSeq::from_stored(f);
        let sa = a.snapshot(frontier).expect("snapshot a");
        let sb = b.snapshot(frontier).expect("snapshot b");
        assert_eq!(sa.frontier(), sb.frontier(), "{label}: frontier {f}");
        assert_eq!(sa.len(), sb.len(), "{label}: snapshot len at {f}");
        let mut ra: Vec<(RowAddress, StoredRow)> = sa.scan(&collection());
        let mut rb: Vec<(RowAddress, StoredRow)> = sb.scan(&collection());
        ra.sort_by(|x, y| x.0.cmp(&y.0));
        rb.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(ra, rb, "{label}: snapshot scan at frontier {f}");
        // Sharper axis inside the fold, too.
        for ((_, x), (_, y)) in ra.iter().zip(rb.iter()) {
            assert_eq!(
                update(x.value()),
                update(y.value()),
                "{label}: snapshot canonical-text at frontier {f}"
            );
        }
    }

    let la = a.log_from(CommitSeq::GENESIS).expect("log a");
    let lb = b.log_from(CommitSeq::GENESIS).expect("log b");
    assert_eq!(la.len(), lb.len(), "{label}: log length");
}

#[test]
fn subtle_value_codec_is_zero_divergence_through_pg_and_reopen() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("valuecodecjsonb");
    let instance = InstanceId::new("value-codec-jsonb");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // Reference oracle: MemoryStore holds the exact input values verbatim.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory");
    apply_workload(&mut memory);
    // The oracle must itself be faithful (proves the expectations, not the store).
    assert_matches_oracle(&memory, "memory reference");

    // Backend under test.
    let mut pg = pg_factory.create(instance.clone()).expect("create pg");
    apply_workload(&mut pg);

    // Live: pg equals the oracle on both axes, and equals memory frontier-by-frontier.
    assert_matches_oracle(&pg, "live pg");
    assert_stores_agree(&pg, &memory, "live pg vs memory");

    // Reopen: the whole projection is rebuilt from the durable `jsonb` columns —
    // where a codec arm that decodes a raw `jsonb` number wrong, or loses a scale,
    // would first surface.
    let reopened = pg_factory.reopen(instance).expect("reopen pg");
    assert_matches_oracle(&reopened, "reopened pg");
    assert_stores_agree(&reopened, &memory, "reopened pg vs memory");
}
