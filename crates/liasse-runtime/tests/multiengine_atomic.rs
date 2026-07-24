#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.10 multi-engine atomic transition (in-memory store).
//!
//! A ROOT mutation reaches across a module boundary with `#handle.mutation(args)`.
//! The [`ModuleHost`] coordinator lends every reached child engine into the parent
//! transition, so the parent and every child it dispatches into stage together and
//! commit together — all-or-none — under one shared transaction:
//!
//! - a successful cross-engine dispatch commits the parent order AND the child
//!   meter spend as ONE transition (both engines advance, not two separate seqs);
//! - a parent that rejects AFTER a successful child dispatch leaves the child
//!   exactly as it was (the child's staged spend never commits);
//! - a child dispatch that over-spends its meter rejects the WHOLE parent
//!   transition (the parent's own order insert never commits).
//!
//! These prove the in-memory coordinator's atomicity directly; the corpus
//! `cross-module-atomic-transition` case stays debt-gated because the store gate is
//! shared with PostgreSQL, whose multi-instance commit is the deferred follow-up.

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleHost, ModuleSpace, Value,
};
use liasse_store::{InstanceStore, MemoryStore, MemoryStoreFactory};
use liasse_value::{Integer, Text};
use support::generator;

/// A root application whose `buy` mutation inserts its own order and then calls the
/// `bank` child's exposed `consume` mutation in the same transition (§13.10), plus a
/// `buy_then_fail` variant that rejects with a false assertion AFTER the child
/// dispatch. `companies/acme` is seeded live so the `bank` instance mounts in its
/// module space.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.multi.host@1.0.0"
  "$model": {
    "orders": { "$key": "id", "id": "text", "cost": "int" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$modules": {} }
    }
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
      "buy_twice": [
        "o = .orders + { id: @id, cost: @cost }"
        "r1 = #bank.consume({ amount: @cost })"
        "r2 = #bank.consume({ amount: @cost })"
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
  "$module": "t.multi.bank@1.0.0"
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
    "credits": {
      "$view": ".pools { id, balance }"
      "$mut": { "consume": ".consume" }
    }
  }
}"#;

/// A `shop` module whose EXPOSED `place` mutation inserts its own order and then
/// reaches the `bank` peer's `consume` in the same transition (§13.10). Dispatching
/// it through `interface_call` exercises the child-transition FOLD: the shop's
/// transition and the bank's meter spend commit together or none do.
const SHOP: &str = r#"{
  "$liasse": 1
  "$module": "t.multi.shop@1.0.0"
  "$model": {
    "orders": { "$key": "id", "id": "text", "cost": "int" }
    "orders_view": { "$view": ".orders { id, cost }" }
    "$mut": {
      "place": [
        "o = .orders + { id: @id, cost: @cost }"
        "r = #bank.consume({ amount: @cost })"
        "return o { id }"
      ]
    }
  }
  "$expose": {
    "orders": {
      "$view": ".orders { id, cost }"
      "$mut": { "place": ".place" }
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

fn space() -> ModuleSpace {
    ModuleSpace::new("/companies/acme/modules").expect("well-formed mount path")
}

/// A host with the `bank` child installed in `acme`'s module space.
fn host_with_bank() -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(&space(), InstallRequest::new("bank", BANK), &mut generator())
        .expect("the bank child installs");
    host
}

/// The `bank` pool balance as read through its exposed `credits` interface.
fn bank_balance(host: &ModuleHost<MemoryStoreFactory>) -> Value {
    let view = host
        .interface_read(&space(), "bank", "credits")
        .expect("read")
        .expect("credits is exposed");
    let row = &view.rows()[0];
    row.field("balance").expect("balance is projected").clone()
}

/// A host with `bank` and `shop` children installed in `acme`'s module space.
fn host_with_shop_and_bank() -> ModuleHost<MemoryStoreFactory> {
    let mut host = host_with_bank();
    host.install(&space(), InstallRequest::new("shop", SHOP), &mut generator())
        .expect("the shop child installs");
    host
}

/// The order ids committed in a named child instance's exposed `orders` interface.
fn child_order_ids(host: &ModuleHost<MemoryStoreFactory>, name: &str) -> Vec<String> {
    let view = host.interface_read(&space(), name, "orders").expect("read").expect("orders is exposed");
    view.rows()
        .iter()
        .map(|row| match row.field("id").expect("id is projected") {
            Value::Text(t) => t.as_str().to_owned(),
            other => panic!("id is not text: {other:?}"),
        })
        .collect()
}

/// The root order ids currently committed.
fn root_order_ids(host: &ModuleHost<MemoryStoreFactory>) -> Vec<String> {
    let view = host.root().view_at_head("orders_view").expect("view").expect("orders_view exists");
    view.rows()
        .iter()
        .map(|row| match row.field("id").expect("id is projected") {
            Value::Text(t) => t.as_str().to_owned(),
            other => panic!("id is not text: {other:?}"),
        })
        .collect()
}

/// §13.10: a cross-engine `#bank.consume` dispatch commits the parent order AND the
/// child meter spend as ONE transition — both engines advance together.
#[test]
fn cross_engine_dispatch_commits_parent_and_child_together() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10), "the bank starts with 10 credits");
    assert!(root_order_ids(&host).is_empty(), "no order before the transition");
    let root_head_before = host.root().store().head().expect("head");

    let request = CallRequest::new("buy").arg("id", text("o1")).arg("cost", int(4));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the cross-engine transition commits: {outcome:?}"
    );
    // The parent order committed AND the child meter spend committed — one transition.
    assert_eq!(root_order_ids(&host), vec!["o1".to_owned()], "the parent order committed");
    assert_eq!(bank_balance(&host), int(6), "the child meter spend committed (10 - 4)");
    let root_head_after = host.root().store().head().expect("head");
    assert_ne!(root_head_before, root_head_after, "the root head advanced with the child");
}

/// §13.10 same-engine re-entrancy: two `#bank.consume(6)` in ONE transition against
/// a balance of 10 compose — the second spend reads the first's write (balance 4),
/// asserts 4 >= 6, and REJECTS, rolling the whole transition back. Before the
/// composition fix each dispatch re-read committed state (10), both passed, and the
/// second's absolute write silently clobbered the first (final balance 4 — a 12-unit
/// spend committed against a 10 budget, one spend lost).
#[test]
fn same_engine_reentrancy_composes_and_rejects_the_overspend() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10), "the bank starts with 10 credits");

    // Two spends of 6 against balance 10: the composed second spend over-draws.
    let request = CallRequest::new("buy_twice").arg("id", text("o1")).arg("cost", int(6));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the second composed spend over-draws and rejects the whole transition: {outcome:?}"
    );
    assert_eq!(bank_balance(&host), int(10), "no spend committed — the composed over-draw rolled back (not 4)");
    assert!(root_order_ids(&host).is_empty(), "the parent's own order did not commit either");
}

/// §13.10 same-engine re-entrancy: two spends that TOGETHER fit the budget both
/// commit as ONE composed change on the reached engine (10 - 3 - 3 = 4), proving the
/// composition applies both writes rather than only the last.
#[test]
fn same_engine_reentrancy_composes_two_fitting_spends() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    // Two spends of 3 against balance 10: both fit, composed to 10 - 3 - 3 = 4.
    let request = CallRequest::new("buy_twice").arg("id", text("o1")).arg("cost", int(3));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");

    assert!(matches!(outcome, CallOutcome::Committed { .. }), "both composed spends commit: {outcome:?}");
    assert_eq!(bank_balance(&host), int(4), "both spends applied (10 - 3 - 3), not just the last (7)");
    assert_eq!(root_order_ids(&host), vec!["o1".to_owned()], "the parent order committed with them");
}

/// §13.10: a parent mutation that rejects AFTER a successful child dispatch leaves
/// the child exactly as before — the child's staged spend never commits.
#[test]
fn parent_reject_after_child_dispatch_leaves_child_unchanged() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("buy_then_fail").arg("id", text("o1")).arg("cost", int(4));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the parent's false assertion rejects the whole transition: {outcome:?}"
    );
    assert_eq!(bank_balance(&host), int(10), "the child meter spend rolled back with the parent");
    assert!(root_order_ids(&host).is_empty(), "the parent's own order did not commit either");
}

/// §13.10: the SINGLE-engine admission path has no coordinator, so a `#bank.consume`
/// cross-engine dispatch is refused loudly rather than faked — the parent's own
/// order never commits without the child. This is the pre-coordinator behaviour the
/// multi-engine path replaces (the corpus case rejects here for the same reason).
#[test]
fn single_engine_call_refuses_cross_engine_dispatch() {
    let mut host = host_with_bank();

    let request = CallRequest::new("buy").arg("id", text("o1")).arg("cost", int(4));
    let outcome = host.root_mut().call(&request, &mut generator()).expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "a cross-engine dispatch outside a multi-engine transition is refused: {outcome:?}"
    );
    assert!(root_order_ids(&host).is_empty(), "nothing committed on the single-engine path");
    assert_eq!(bank_balance(&host), int(10), "the child was never reached");
}

/// §13.10: a child dispatch that over-spends its meter rejects the WHOLE parent
/// transition — the parent's own order insert never commits.
#[test]
fn child_reject_rejects_the_whole_parent_transition() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    // cost 20 > balance 10: the bank's `consume` assertion fails.
    let request = CallRequest::new("buy").arg("id", text("o1")).arg("cost", int(20));
    let outcome = host.call_multi(&request, &mut generator()).expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the child's meter over-spend rejects the whole transition: {outcome:?}"
    );
    assert_eq!(bank_balance(&host), int(10), "the child meter is unchanged");
    assert!(root_order_ids(&host).is_empty(), "the parent's order insert did not commit");
}

/// §13.10 fold: an `interface_call` on a child whose exposed mutation reaches a peer
/// folds the two transitions into ONE atomic commit — the shop order and the bank
/// meter spend commit together.
#[test]
fn interface_call_folds_a_peer_reaching_child_transition() {
    let mut host = host_with_shop_and_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("place").arg("id", text("s1")).arg("cost", int(3));
    let outcome = host
        .interface_call(&space(), "shop", "orders", "place", &request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the folded shop+bank transition commits: {outcome:?}"
    );
    assert_eq!(child_order_ids(&host, "shop"), vec!["s1".to_owned()], "the shop order committed");
    assert_eq!(bank_balance(&host), int(7), "the bank meter spend committed (10 - 3)");
}

/// §13.10 fold: when the reached peer rejects, the child's own transition rejects
/// with it — the shop order does not commit either.
#[test]
fn interface_call_fold_rejects_when_the_peer_rejects() {
    let mut host = host_with_shop_and_bank();
    assert_eq!(bank_balance(&host), int(10));

    // cost 20 > balance 10: the bank peer's `consume` assertion fails.
    let request = CallRequest::new("place").arg("id", text("s1")).arg("cost", int(20));
    let outcome = host
        .interface_call(&space(), "shop", "orders", "place", &request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the peer's over-spend rejects the whole folded transition: {outcome:?}"
    );
    assert!(child_order_ids(&host, "shop").is_empty(), "the shop order did not commit");
    assert_eq!(bank_balance(&host), int(10), "the bank meter is unchanged");
}
