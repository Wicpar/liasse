//! RED TEAM — a `$set`-of-`$ref` member drop (§5.6/§21.1 `DropMember`) and the
//! never-before-stored value shapes it rides on, driven through the real
//! PostgreSQL value column and checked for a pg-vs-`MemoryStore` divergence live
//! AND across a durable reopen — on the Annex-B `Ord` axis AND the sharper
//! canonical-text (A.7) axis.
//!
//! Why this is a fresh angle. The store-contract batteries and the two value
//! round-trip suites (`value_wire`, `redteam_value_codec_jsonb_divergence`) have
//! exhausted the scalar variants, composite/struct KEYS, and the tombstone /
//! auto-ancestor / reopen machinery. But the `$set`-of-`$ref` feature just landed,
//! and the surviving-row state it produces is a value shape neither round-trip
//! suite ever stored:
//!
//!   * `value_wire` stores a `Value::Set` of two *ints* and a `Value::Map` of one
//!     *text*→int entry, and compares them on the scale-INSENSITIVE `Value::Eq`
//!     axis only.
//!   * `redteam_value_codec_jsonb_divergence` adds the canonical-text axis but
//!     never stores a `Value::Set` or a `Value::Map` at all.
//!
//! So a **set of composite refs** and a **map keyed by structs** — the exact
//! payloads a `$set<$ref>` field and a struct-keyed lookup carry — have never
//! been round-tripped through pg on the canonical-text axis, and the update that
//! DROPS a set member (§21.1's surviving-row effect: the referencing row keeps its
//! identity, its `$set` loses one member, the store sees a plain `update`) has
//! never been reopened.
//!
//! What is asserted. The runtime is store-agnostic: a `cascade`/`clear` on a
//! `$set<$ref>` member resolves to `DeletePolicy::DropMember`, and the interpreter
//! re-places the surviving row with the member removed
//! (`interp::apply_deletion`). At the store contract that is exactly an `update`
//! of the row to a smaller set — the op stream this test replays verbatim on both
//! backends. Each stage's surviving value is built INDEPENDENTLY here (never read
//! back from the store), so a pass means the codec is a true inverse across the
//! reopen, not that the store agrees with itself.
//!
//! Cited: §5.6/§21.1 (`$set<$ref>` member drop is a surviving-row `update`, not a
//! row delete), A.7 (canonical wire/JSON; `1.0` ≠ `1.00` ≠ `1`), A.9/B.4
//! (`composite` ref key, positional order), B.1 (`decimal` scale in a set/map
//! member), §22.7/§19.2 (a reopen rebuilds an identical projection). Overarching
//! gate: pg must equal `MemoryStore` observably, and on the canonical-text axis.
//!
//! Like the rest of the suite it resolves the DSN through [`support`] and drops
//! its throwaway schema through a [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use std::collections::BTreeSet;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress,
    StoreFactory, StoredRow, Transition,
};
use liasse_value::{Decimal, Integer, Ref, Struct, Text, Value};

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}
fn dec(v: &str) -> Value {
    Value::Decimal(Decimal::parse(v).expect("decimal literal parses"))
}

/// A composite ref into a `stores` relation keyed by `(int, text)` — the A.9/B.4
/// positional key form a `$set<$ref<stores>>` member carries.
fn holder(n: i64, region: &str) -> Value {
    Value::Ref(Ref::composite(vec![int(n), text(region)]))
}

/// A struct value used as a MAP KEY — the never-round-tripped "map keyed by
/// struct" shape. Field order is text order (B.4); one field is scale-bearing.
fn struct_key(tag: &str, rank: &str) -> Value {
    Value::Struct(Struct::new([
        (Text::new("rank"), dec(rank)),
        (Text::new("tag"), text(tag)),
    ]))
}

/// The surviving row's payload at a given set membership: a struct bearing the
/// `$set<$ref>` field `holders`, a struct-keyed `map`, and a NUL-bearing note so
/// the value exercises the jsonb NUL escape alongside the fresh collection shapes.
/// Built purely from `members`, independent of any store answer.
fn payload(members: &[Value]) -> Value {
    let holders: BTreeSet<Value> = members.iter().cloned().collect();
    let meta = [
        (struct_key("north", "1.0"), dec("10.00")),
        (struct_key("south", "1.00"), dec("2.5")),
    ]
    .into_iter()
    .collect();
    Value::Struct(Struct::new([
        (Text::new("holders"), Value::Set(holders)),
        (Text::new("meta"), Value::Map(meta)),
        (Text::new("note"), text("a\u{0}b")),
    ]))
}

/// The one composite-keyed `docs` row this workload maintains.
fn doc_address() -> RowAddress {
    let key = KeyValue::composite(int(7), [text("doc")]);
    RowAddress::root(AddressStep::new(NameSegment::new("docs"), key))
}

fn docs_path() -> CollectionPath {
    CollectionPath::top(NameSegment::new("docs"))
}

/// The three `holders` set incarnations the workload walks through: the full set,
/// the set after dropping the middle member (the §21.1 `DropMember` effect), and
/// the empty set after dropping the last surviving member. `holder(2, "east")` is
/// the member a `cascade`/`clear` on the deleted target removes.
fn full() -> Vec<Value> {
    vec![holder(1, "west"), holder(2, "east"), holder(3, "north")]
}
fn after_drop() -> Vec<Value> {
    vec![holder(1, "west"), holder(3, "north")]
}
fn emptied() -> Vec<Value> {
    Vec::new()
}

/// Replay the exact op stream the runtime emits: insert the full-set row, then
/// two surviving-row `update`s that shrink the `$set<$ref>` (drop one member, then
/// the rest). Identical on both backends, so their incarnations line up.
fn apply_workload<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(doc_address(), payload(&full())).expect("insert full-set doc");
    txn.commit().expect("commit insert");

    let mut txn = store.begin();
    txn.update(&doc_address(), payload(&after_drop())).expect("drop one set member");
    txn.commit().expect("commit member drop");

    let mut txn = store.begin();
    txn.update(&doc_address(), payload(&emptied())).expect("drop the last set member");
    txn.commit().expect("commit emptied set");
}

/// The single `docs` row's stored value, or a panic if the row vanished.
fn doc_value<S: InstanceStore>(store: &S) -> Value {
    let rows: Vec<(RowAddress, StoredRow)> = store.scan(&docs_path()).expect("scan docs");
    assert_eq!(rows.len(), 1, "exactly one docs row must survive, got {rows:?}");
    rows[0].1.value().clone()
}

/// Assert two values agree on BOTH the Annex-B `Ord` axis and the A.7
/// canonical-text axis. The canonical text is what distinguishes `1.0`/`1.00`/`1`
/// that `Ord` folds together, so it is the axis a scale-losing reopen fails on.
fn assert_same(context: &str, expected: &Value, got: &Value) {
    assert_eq!(got, expected, "{context}: Annex-B Ord divergence");
    assert_eq!(
        got.to_canonical_json_string(),
        expected.to_canonical_json_string(),
        "{context}: canonical-text (A.7) divergence — expected `{}` got `{}`",
        expected.to_canonical_json_string(),
        got.to_canonical_json_string(),
    );
}

#[test]
fn set_of_ref_dropmember_reopens_zero_divergence() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("setofrefdrop");
    let instance = InstanceId::new("set-of-ref-dropmember-reopen");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    // The externally-known truth after the whole workload: the emptied `$set`.
    let expected = payload(&emptied());

    // Oracle and pg run the identical op stream, so incarnations line up.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    apply_workload(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    apply_workload(&mut pg);

    // The oracle holds exactly the independently built emptied-set payload.
    assert_same("memory oracle vs independent truth", &expected, &doc_value(&memory));

    // Live pg agrees with the oracle on both axes (the update path never lost a
    // set member's incarnation or the struct-keyed map's decimal scales).
    assert_same("pg live vs oracle", &expected, &doc_value(&pg));

    // The load-bearing assertion: reopen purely from the durable node tables — the
    // path a process restart takes — and the surviving `$set<$ref>` row must decode
    // back byte-identical. A codec that dropped a struct map key's `1.0`↔`1.00`
    // scale, reordered a composite-ref set member, or mishandled the emptied set on
    // the write→jsonb→read cycle diverges here while `Value::Eq` alone stayed blind.
    let reopened = pg_factory.reopen(instance.clone()).expect("reopen pg store");
    assert_same(
        "pg reopened (§22.7/§19.2) vs oracle",
        &expected,
        &doc_value(&reopened),
    );
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
