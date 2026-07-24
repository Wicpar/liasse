//! §13.10 multi-engine atomic transition — durable PostgreSQL store.
//!
//! The in-memory reference proves the coordinator commits all-or-none in process;
//! this suite proves the SAME coordinator, over [`liasse_pg::PgStore`], commits every
//! touched instance DURABLY as ONE PostgreSQL transaction. Every deployment's
//! instances share one database (one schema each), so a folded transition commits in
//! a single SQL transaction spanning every touched schema, under ordered per-instance
//! head locks:
//!
//! - a cross-engine dispatch commits the parent order AND the child meter spend as
//!   one transition — both persist and a FRESH reopen sees both, tagged with the same
//!   cross-instance transaction identity (§19.1);
//! - a parent that rejects AFTER a successful child dispatch leaves the child's
//!   DURABLE state exactly as it was;
//! - a child dispatch that over-spends rolls back the parent's DURABLE writes;
//! - concurrent folded commits touching overlapping instances are serialized by the
//!   ordered `FOR UPDATE` head locks — no lost update, no partial commit, no deadlock.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use std::thread;

use liasse_ident::{InstanceId, NameSegment};
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, FixedGenerators, InstallRequest, ModuleHost, ModuleSpace,
    Precision,
};
use liasse_store::{
    AddressStep, CollectionPath, CommitSeq, GroupMember, InstanceStore, KeyValue, RowAddress,
    StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};

/// A fixed micro-precision instant used as the deterministic `now()` sample.
const NOW_MICROS: i128 = 1_700_000_000_000_000;

fn generator() -> FixedGenerators {
    FixedGenerators::new(NOW_MICROS, Precision::Micros)
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn space() -> ModuleSpace {
    ModuleSpace::new("/companies/acme/modules").expect("well-formed mount path")
}

/// A root application whose `buy` inserts its own order and then dispatches the
/// `bank` child's exposed `consume` in the same transition (§13.10). `buy_then_fail`
/// rejects with a false assertion AFTER a successful child dispatch.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.pg.host@1.0.0"
  "$model": {
    "orders": { "$key": "id", "id": "text", "cost": "int" }
    "companies": { "$key": "id", "id": "text", "modules": { "$modules": {} } }
    "orders_view": { "$view": ".orders { id, cost }" }
    "$mut": {
      "buy": [
        "o = .orders + { id: @id, cost: @cost }"
        "r = #bank.consume({ amount: @cost })"
        "return o { id }"
      ]
      "buy_then_fail": [
        "o = .orders + { id: @id, cost: @cost }"
        "r = #bank.consume({ amount: @cost })"
        "assert(false, 'the parent rejects after a successful child dispatch')"
        "return o { id }"
      ]
    }
  }
  "$data": { "companies": { "acme": {} } }
}"#;

/// A metered `bank` module: `consume` asserts the pool covers the amount and debits
/// it, exposing that private mutation through its `credits` interface.
const BANK: &str = r#"{
  "$liasse": 1
  "$module": "t.pg.bank@1.0.0"
  "$model": {
    "pools": { "$key": "id", "id": "text", "balance": "int" }
    "credits_view": { "$view": ".pools { id, balance }" }
    "$mut": {
      "consume": [
        "assert(.pools['main'].balance >= @amount, 'insufficient credits')"
        ".pools['main'].balance = .pools['main'].balance - @amount"
        "return { remaining: .pools['main'].balance }"
      ]
    }
  }
  "$data": { "pools": { "main": { "balance": "10" } } }
  "$expose": {
    "credits": { "$view": ".pools { id, balance }", "$mut": { "consume": ".consume" } }
  }
}"#;

/// A host with the `bank` child installed in `acme`'s space, its root store created
/// under `root_id` so it can be reopened for a durable read after the host drops.
struct Fixture {
    host: ModuleHost<PgStoreFactory>,
    factory: PgStoreFactory,
    root_id: InstanceId,
    bank_id: InstanceId,
}

fn build(handle: &support::PgHandle, seed: &str) -> Fixture {
    let mut factory = handle.factory(seed);
    let root_id = InstanceId::new(format!("{seed}-root"));
    let root_store = factory.create(root_id.clone()).expect("create root schema");
    let root = Engine::load(root_store, ROOT, &mut generator()).expect("root loads");
    let mut host = ModuleHost::new(factory.clone(), root);
    host.install(&space(), InstallRequest::new("bank", BANK), &mut generator())
        .expect("the bank child installs");
    let bank_id = host.incarnation(&space(), "bank").expect("bank installed").clone();
    Fixture { host, factory, root_id, bank_id }
}

/// The bank pool balance as read through its exposed `credits` interface on the LIVE
/// host — a durable SQL read (the PostgreSQL store holds no projection).
fn live_bank_balance(host: &ModuleHost<PgStoreFactory>) -> Value {
    let view =
        host.interface_read(&space(), "bank", "credits").expect("read").expect("credits exposed");
    view.rows()[0].field("balance").expect("balance projected").clone()
}

/// The root order ids as read through the live host's `orders_view`.
fn live_order_ids(host: &ModuleHost<PgStoreFactory>) -> Vec<String> {
    let view = host.root().view_at_head("orders_view").expect("view").expect("orders_view exists");
    view.rows()
        .iter()
        .map(|row| match row.field("id").expect("id projected") {
            Value::Text(t) => t.as_str().to_owned(),
            other => panic!("id is not text: {other:?}"),
        })
        .collect()
}

/// The `balance` field of `/pools/main`, read from a store that is NOT the live host —
/// a fresh reopen proving the value is durable, not a live-session artefact.
fn durable_balance(store: &PgStore) -> Value {
    let pools = CollectionPath::top(NameSegment::new("pools"));
    store
        .scan(&pools)
        .expect("scan pools")
        .into_iter()
        .find_map(|(_address, row)| match row.value() {
            Value::Struct(fields) => fields.get("balance").cloned(),
            _ => None,
        })
        .expect("no /pools/main row in the reopened bank store")
}

/// The order ids durably present in a reopened root store.
fn durable_order_ids(store: &PgStore) -> Vec<String> {
    let orders = CollectionPath::top(NameSegment::new("orders"));
    store
        .scan(&orders)
        .expect("scan orders")
        .into_iter()
        .filter_map(|(_address, row)| match row.value() {
            Value::Struct(fields) => match fields.get("id") {
                Some(Value::Text(id)) => Some(id.as_str().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The transaction identity of a store's most recent committed transition, if any.
fn last_transaction(store: &PgStore) -> Option<String> {
    let log = store.log_from(CommitSeq::GENESIS).expect("log");
    log.last().and_then(|t| t.transaction().map(|id| id.as_str().to_owned()))
}

/// §13.10: a cross-engine `#bank.consume` dispatch commits the parent order AND the
/// child meter spend DURABLY as ONE PostgreSQL transaction — both persist, a fresh
/// reopen sees both, and both carry the same cross-instance transaction identity.
#[test]
fn cross_engine_dispatch_commits_parent_and_child_as_one_pg_transaction() {
    let handle = support::acquire();
    let Fixture { mut host, factory, root_id, bank_id } = build(&handle, "xeng");

    assert_eq!(live_bank_balance(&host), int(10), "the bank starts with 10 credits");
    assert!(live_order_ids(&host).is_empty(), "no order before the transition");

    let request = CallRequest::new("buy").arg("id", text("o1")).arg("cost", int(4));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the folded transition commits: {outcome:?}");

    // Live durable reads (PostgreSQL, no projection) — both sides landed.
    assert_eq!(live_order_ids(&host), vec!["o1".to_owned()], "the parent order committed");
    assert_eq!(live_bank_balance(&host), int(6), "the child meter spend committed (10 - 4)");

    // Drop the host (closing every writer), then reopen from scratch: a FRESH read of
    // both durable schemas sees both writes, tagged with ONE shared transaction id.
    drop(host);
    let root = factory.reopen(root_id).expect("reopen root");
    let bank = factory.reopen(bank_id).expect("reopen bank");
    assert_eq!(durable_order_ids(&root), vec!["o1".to_owned()], "the order is durable across a reopen");
    assert_eq!(durable_balance(&bank), int(6), "the spend is durable across a reopen");
    let root_tx = last_transaction(&root);
    let bank_tx = last_transaction(&bank);
    assert!(root_tx.is_some(), "the root commit carries a shared transaction id");
    assert_eq!(root_tx, bank_tx, "parent and child committed under ONE cross-instance transaction (§19.1)");
    assert!(root.head().unwrap().get() > 0 && bank.head().unwrap().get() > 0, "both heads advanced");
}

/// §13.10: a parent that rejects AFTER a successful child dispatch leaves the child's
/// DURABLE state exactly as it was — the child's staged spend never reaches the disk.
#[test]
fn parent_reject_after_child_leaves_child_durable_state_unchanged() {
    let handle = support::acquire();
    let Fixture { mut host, factory, root_id, bank_id } = build(&handle, "prej");

    let bank_head_before = factory.reopen(bank_id.clone()).expect("reopen bank").head().unwrap();

    let request = CallRequest::new("buy_then_fail").arg("id", text("o1")).arg("cost", int(4));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "the parent rejects the whole transition: {outcome:?}");

    assert_eq!(live_bank_balance(&host), int(10), "the child spend rolled back on the live host");
    assert!(live_order_ids(&host).is_empty(), "the parent's own order did not commit");

    drop(host);
    let root = factory.reopen(root_id).expect("reopen root");
    let bank = factory.reopen(bank_id).expect("reopen bank");
    assert_eq!(durable_balance(&bank), int(10), "the child's DURABLE balance is unchanged (10)");
    assert_eq!(bank.head().unwrap(), bank_head_before, "no new bank commit reached the disk");
    assert!(durable_order_ids(&root).is_empty(), "no parent order is durable");
}

/// §13.10: a child dispatch that over-spends its meter rejects the WHOLE transition —
/// the parent's own durable order insert never lands.
#[test]
fn child_reject_rolls_back_parent_durable_writes() {
    let handle = support::acquire();
    let Fixture { mut host, factory, root_id, bank_id } = build(&handle, "crej");

    let root_head_before = factory.reopen(root_id.clone()).expect("reopen root").head().unwrap();

    // Cost 20 over-spends the bank's balance of 10: `consume` asserts 10 >= 20 → false.
    let request = CallRequest::new("buy").arg("id", text("o1")).arg("cost", int(20));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "the child over-spend rejects the transition: {outcome:?}");

    assert!(live_order_ids(&host).is_empty(), "the parent order rolled back on the live host");
    assert_eq!(live_bank_balance(&host), int(10), "the bank balance is untouched");

    drop(host);
    let root = factory.reopen(root_id).expect("reopen root");
    let bank = factory.reopen(bank_id).expect("reopen bank");
    assert!(durable_order_ids(&root).is_empty(), "the parent's DURABLE order write rolled back");
    assert_eq!(root.head().unwrap(), root_head_before, "no new root commit reached the disk");
    assert_eq!(durable_balance(&bank), int(10), "the bank's DURABLE balance is unchanged (10)");
}

/// §13.10: concurrent folded commits touching the SAME two instances are serialized
/// by the ordered `FOR UPDATE` head locks. Every thread commits a row into BOTH
/// instances as one group; the test completing proves no deadlock, and both instances
/// ending with exactly one row per thread (heads advanced by the thread count) proves
/// no lost update and no partial commit.
#[test]
fn concurrent_group_commits_serialize_on_ordered_head_locks() {
    const THREADS: usize = 8;

    let handle = support::acquire();
    let mut factory = handle.factory("conc");
    let a = InstanceId::new("conc-a");
    let b = InstanceId::new("conc-b");
    factory.create(a.clone()).expect("create a");
    factory.create(b.clone()).expect("create b");

    let address = |thread: usize| {
        RowAddress::root(AddressStep::new(
            NameSegment::new("items"),
            KeyValue::single(int(i64::try_from(thread).unwrap())),
        ))
    };

    // Reopen every thread's pair of connections up front (sequentially, so no two
    // reconciles race), then hand each thread its own two stores.
    let mut workers = Vec::with_capacity(THREADS);
    for t in 0..THREADS {
        let sa = factory.reopen(a.clone()).expect("reopen a");
        let sb = factory.reopen(b.clone()).expect("reopen b");
        workers.push(thread::spawn(move || {
            let mut sa = sa;
            let mut sb = sb;
            let pa = {
                let mut txn = sa.begin();
                txn.insert(address(t), text(&format!("a{t}"))).expect("stage a");
                txn.into_pending()
            };
            let pb = {
                let mut txn = sb.begin();
                txn.insert(address(t), text(&format!("b{t}"))).expect("stage b");
                txn.into_pending()
            };
            let members = vec![
                GroupMember { store: &mut sa, pending: pa },
                GroupMember { store: &mut sb, pending: pb },
            ];
            <PgStore as InstanceStore>::commit_pending_group(members).expect("group commits");
        }));
    }
    for worker in workers {
        // A join that returns is itself the no-deadlock proof.
        worker.join().expect("no thread panicked or deadlocked");
    }

    // No lost update: every one of the THREADS commits landed on BOTH instances, so
    // each head advanced by exactly the thread count and each holds one row per thread.
    let items = CollectionPath::top(NameSegment::new("items"));
    let ra = factory.reopen(a.clone()).expect("reopen a");
    let rb = factory.reopen(b.clone()).expect("reopen b");
    assert_eq!(ra.head().unwrap().get(), THREADS as u64, "instance A advanced once per thread — no lost update");
    assert_eq!(rb.head().unwrap().get(), THREADS as u64, "instance B advanced once per thread — no lost update");
    assert_eq!(ra.scan(&items).unwrap().len(), THREADS, "A holds one row per thread — no partial commit");
    assert_eq!(rb.scan(&items).unwrap().len(), THREADS, "B holds one row per thread — no partial commit");
}
