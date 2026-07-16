//! Admission throughput of a small mutation on a large collection — the runtime
//! hot path (§22.2). Each call gathers the prospective state, executes the
//! program, runs the rule pipeline (defaults/normalization/checks/uniqueness),
//! diffs, and commits; the cost is dominated by materializing and scanning the
//! collection, so the bench measures it against a 10k-row collection.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use liasse_ident::{InstanceId, NameSegment};
use liasse_runtime::{CallRequest, Engine, FixedGenerators, Precision, Value};
use liasse_store::{AddressStep, InstanceStore, KeyValue, MemoryStore, RowAddress, Transition};
use liasse_value::{Integer, Struct, Text};

const ROWS: i64 = 10_000;

const BANK: &str = r#"{
  "$liasse": 1
  "$app": "bench.bank@1.0.0"
  "$model": {
    "accounts": {
      "$key": "id"
      "$unique": ["email"]
      "id": "text"
      "email": "text"
      "balance": "int = 0"
      "$check": [".balance >= 0", "No overdraft"]
    }
    "$mut": { "set_balance({ id: text, amount: int })": ".accounts[@id].balance = @amount" }
  }
}"#;

fn text(value: impl Into<String>) -> Value {
    Value::Text(Text::new(value.into()))
}

fn account_row(index: i64) -> Value {
    Value::Struct(Struct::new([
        (Text::new("id"), text(format!("acct-{index:05}"))),
        (Text::new("email"), text(format!("u{index}@x.com"))),
        (Text::new("balance"), Value::Int(Integer::from(0))),
    ]))
}

/// A store pre-loaded with `ROWS` account rows, so `Engine::load`'s genesis sees
/// a fully populated collection to admit mutations against.
fn populated_store() -> MemoryStore {
    let mut store = MemoryStore::new(InstanceId::new("bench"));
    let mut txn = store.begin();
    for index in 0..ROWS {
        let address = RowAddress::root(AddressStep::new(
            NameSegment::new("accounts"),
            KeyValue::single(text(format!("acct-{index:05}"))),
        ));
        txn.insert(address, account_row(index)).expect("insert");
    }
    let _ = txn.commit();
    store
}

fn admission_throughput(c: &mut Criterion) {
    let mut generator = FixedGenerators::new(0, Precision::Micros);
    let mut engine = Engine::load(populated_store(), BANK, &mut generator).expect("load");
    let mut toggle: i64 = 0;
    c.bench_function("set_balance_on_10k_rows", |b| {
        b.iter(|| {
            toggle ^= 1;
            let request = CallRequest::new("set_balance")
                .arg("id", text("acct-00000"))
                .arg("amount", Value::Int(Integer::from(100 + toggle)));
            let outcome = engine.call(&request, &mut generator).expect("call");
            black_box(outcome);
        });
    });
}

criterion_group!(benches, admission_throughput);
criterion_main!(benches);
