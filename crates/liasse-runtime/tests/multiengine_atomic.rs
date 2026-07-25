#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
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
//! These prove the in-memory coordinator's atomicity directly, INCLUDING the
//! canonical §13.10 form — an in-program `$use` peer-alias dispatch (`#credits`,
//! resolved through the caller's §13.5 peer bindings, not a raw instance name) —
//! which commits atomically with the caller on memory. The corpus
//! `cross-module-atomic-transition` case stays debt-gated (the shared memory+PG
//! gate): its peer alias now resolves, but the scenario adapter forwards its
//! child-mutation argument untyped and the durable PostgreSQL multi-instance commit
//! is the deferred Part-B follow-up.

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleHost, Value,
};
use liasse_store::{CollectionPath, InstanceStore, MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_value::{Integer, Text};
use support::generator;

/// A root application whose `buy` mutation inserts its own order and then calls the
/// `bank` child's exposed `consume` mutation in the same transition (§13.10), plus a
/// `buy_then_fail` variant that rejects with a false assertion AFTER the child
/// dispatch. `companies/acme` is seeded live so the `bank` instance mounts in its
/// module collection.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.multi.host@1.0.0"
  "$model": {
    "orders": { "$key": "id", "id": "text", "cost": "int" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
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
      "buy_thrice({ id: text, c1: int, c2: int, c3: int })": [
        "o = .orders + { id: @id, cost: @c1 }"
        "r1 = #bank.consume({ amount: @c1 })"
        "r2 = #bank.consume({ amount: @c2 })"
        "r3 = #bank.consume({ amount: @c3 })"
        "return o { id }"
      ]
      "buy_interleaved({ id: text, a: int, b: int, c: int })": [
        "o = .orders + { id: @id, cost: @a }"
        "r1 = #bank.consume({ amount: @a })"
        "r2 = #vault.consume({ amount: @b })"
        "r3 = #bank.consume({ amount: @c })"
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
/// reaches the bank through the CANONICAL §13.5 `$use` peer alias `credits` — a
/// handle name DISTINCT from the installed instance name (`bank`), so the dispatch
/// exercises peer-alias resolution, not a raw instance-name match. Dispatching it
/// through `interface_call` exercises the child-transition FOLD: the shop's
/// transition and the bank's meter spend commit together or none do (§13.10).
const SHOP: &str = r#"{
  "$liasse": 1
  "$module": "t.multi.shop@1.0.0"
  "$use": { "credits": "t.multi.bank/credits@1" }
  "$model": {
    "orders": { "$key": "id", "id": "text", "cost": "int" }
    "orders_view": { "$view": ".orders { id, cost }" }
    "$mut": {
      "place": [
        "o = .orders + { id: @id, cost: @cost }"
        "r = #credits.consume({ amount: @cost })"
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

/// A `rogue` module whose EXPOSED `place` mutation hard-codes `#bank.consume` even
/// though it declares NO `$use` import for it — an over-reach at a non-imported
/// sibling by raw instance name. §13.10 import scope must refuse the dispatch loudly
/// rather than let it reach the sibling.
const ROGUE: &str = r#"{
  "$liasse": 1
  "$module": "t.multi.rogue@1.0.0"
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

/// A `bank` variant that declares an `$auth` actor collection and, in `consume`,
/// RECORDS the admitting `$actor` into a receipt it exposes — so a dispatched
/// transition reveals whether the child observed the parent's `$actor` (§13.11). The
/// `consume` faults on an unbound `$actor`, so without propagation the transition
/// rejects; with it, the receipt carries the parent's actor key.
const BANK_AUTH: &str = r#"{
  "$liasse": 1
  "$module": "t.multi.bank@1.0.0"
  "$model": {
    "pools": { "$key": "id", "id": "text", "balance": "int" }
    "accounts": { "$key": "id", "id": "text", "name": "text" }
    "receipts": { "$key": "id", "id": "text", "who": { "$ref": "/accounts" } }
    "credits_view": { "$view": ".pools { id, balance }" }
    "$mut": {
      "consume": [
        "assert(.pools['main'].balance >= @amount, 'insufficient credits')"
        ".pools['main'].balance = .pools['main'].balance - @amount"
        "rec = .receipts + { id: $actor.id, who: $actor }"
        "return { remaining: .pools['main'].balance }"
      ]
    }
    "$auth": {
      "session": {
        "$credential": "text"
        "$verify": "$credential"
        "$actor": "/accounts[$proof.account]"
      }
    }
  }
  "$data": {
    "pools": { "main": { "balance": "10" } }
    "accounts": { "alice": { "name": "Alice" } }
  }
  "$expose": {
    "credits": {
      "$view": ".pools { id, balance }"
      "$mut": { "consume": ".consume" }
    }
    "receipts": { "$view": ".receipts { id, who }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn int(value: i64) -> Value {
    Value::Int(Integer::from(value))
}

/// The module collection the fixture mounts its instances in.
fn collection() -> CollectionPath {
    support::collection_at("/companies/acme/modules")
}

/// The address of the module-collection entry `name` — one mounted instance's
/// identity (§13.3), an ordinary row address.
fn at(name: &str) -> RowAddress {
    support::mount_at("/companies/acme/modules", name)
}

/// A host with the `bank` child installed in `acme`'s module collection.
fn host_with_bank() -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(
        &collection(),
        InstallRequest::new("bank", BANK),
        &mut generator(),
    )
    .expect("the bank child installs");
    host
}

/// The `bank` pool balance as read through its exposed `credits` interface.
fn bank_balance(host: &ModuleHost<MemoryStoreFactory>) -> Value {
    let view = host
        .interface_read(&at("bank"), "credits")
        .expect("read")
        .expect("credits is exposed");
    let row = &view.rows()[0];
    row.field("balance").expect("balance is projected").clone()
}

/// A host with `bank` and `shop` children installed in `acme`'s module collection. The
/// shop declares its `credits` peer against the bank (§13.5), so `#credits` resolves
/// to the bank instance at dispatch.
fn host_with_shop_and_bank() -> ModuleHost<MemoryStoreFactory> {
    let mut host = host_with_bank();
    host.install(
        &collection(),
        InstallRequest::new("shop", SHOP).use_handle("credits", "t.multi.bank/credits@1"),
        &mut generator(),
    )
    .expect("the shop child installs");
    host
}

/// A host with `bank` and a `rogue` child that hard-codes `#bank` without declaring
/// it under `$use` (§13.10 import-scope over-reach).
fn host_with_rogue_and_bank() -> ModuleHost<MemoryStoreFactory> {
    let mut host = host_with_bank();
    host.install(
        &collection(),
        InstallRequest::new("rogue", ROGUE),
        &mut generator(),
    )
    .expect("the rogue child installs (its over-reach is caught at dispatch, not install)");
    host
}

/// The order ids committed in a named child instance's exposed `orders` interface.
fn child_order_ids(host: &ModuleHost<MemoryStoreFactory>, name: &str) -> Vec<String> {
    let view = host
        .interface_read(&at(name), "orders")
        .expect("read")
        .expect("orders is exposed");
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
    let view = host
        .root()
        .view_at_head("orders_view")
        .expect("view")
        .expect("orders_view exists");
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
    assert_eq!(
        bank_balance(&host),
        int(10),
        "the bank starts with 10 credits"
    );
    assert!(
        root_order_ids(&host).is_empty(),
        "no order before the transition"
    );
    let root_head_before = host.root().store().head().expect("head");

    let request = CallRequest::new("buy")
        .arg("id", text("o1"))
        .arg("cost", int(4));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the cross-engine transition commits: {outcome:?}"
    );
    // The parent order committed AND the child meter spend committed — one transition.
    assert_eq!(
        root_order_ids(&host),
        vec!["o1".to_owned()],
        "the parent order committed"
    );
    assert_eq!(
        bank_balance(&host),
        int(6),
        "the child meter spend committed (10 - 4)"
    );
    let root_head_after = host.root().store().head().expect("head");
    assert_ne!(
        root_head_before, root_head_after,
        "the root head advanced with the child"
    );
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
    assert_eq!(
        bank_balance(&host),
        int(10),
        "the bank starts with 10 credits"
    );

    // Two spends of 6 against balance 10: the composed second spend over-draws.
    let request = CallRequest::new("buy_twice")
        .arg("id", text("o1"))
        .arg("cost", int(6));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the second composed spend over-draws and rejects the whole transition: {outcome:?}"
    );
    assert_eq!(
        bank_balance(&host),
        int(10),
        "no spend committed — the composed over-draw rolled back (not 4)"
    );
    assert!(
        root_order_ids(&host).is_empty(),
        "the parent's own order did not commit either"
    );
}

/// §13.10 same-engine re-entrancy: two spends that TOGETHER fit the budget both
/// commit as ONE composed change on the reached engine (10 - 3 - 3 = 4), proving the
/// composition applies both writes rather than only the last.
#[test]
fn same_engine_reentrancy_composes_two_fitting_spends() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    // Two spends of 3 against balance 10: both fit, composed to 10 - 3 - 3 = 4.
    let request = CallRequest::new("buy_twice")
        .arg("id", text("o1"))
        .arg("cost", int(3));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "both composed spends commit: {outcome:?}"
    );
    assert_eq!(
        bank_balance(&host),
        int(4),
        "both spends applied (10 - 3 - 3), not just the last (7)"
    );
    assert_eq!(
        root_order_ids(&host),
        vec!["o1".to_owned()],
        "the parent order committed with them"
    );
}

/// §13.10: a parent mutation that rejects AFTER a successful child dispatch leaves
/// the child exactly as before — the child's staged spend never commits.
#[test]
fn parent_reject_after_child_dispatch_leaves_child_unchanged() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("buy_then_fail")
        .arg("id", text("o1"))
        .arg("cost", int(4));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the parent's false assertion rejects the whole transition: {outcome:?}"
    );
    assert_eq!(
        bank_balance(&host),
        int(10),
        "the child meter spend rolled back with the parent"
    );
    assert!(
        root_order_ids(&host).is_empty(),
        "the parent's own order did not commit either"
    );
}

/// §13.10: the SINGLE-engine admission path has no coordinator, so a `#bank.consume`
/// cross-engine dispatch is refused loudly rather than faked — the parent's own
/// order never commits without the child. This is the pre-coordinator behaviour the
/// multi-engine path replaces (the corpus case rejects here for the same reason).
#[test]
fn single_engine_call_refuses_cross_engine_dispatch() {
    let mut host = host_with_bank();

    let request = CallRequest::new("buy")
        .arg("id", text("o1"))
        .arg("cost", int(4));
    let outcome = host
        .root_mut()
        .call(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "a cross-engine dispatch outside a multi-engine transition is refused: {outcome:?}"
    );
    assert!(
        root_order_ids(&host).is_empty(),
        "nothing committed on the single-engine path"
    );
    assert_eq!(bank_balance(&host), int(10), "the child was never reached");
}

/// §13.10: a child dispatch that over-spends its meter rejects the WHOLE parent
/// transition — the parent's own order insert never commits.
#[test]
fn child_reject_rejects_the_whole_parent_transition() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    // cost 20 > balance 10: the bank's `consume` assertion fails.
    let request = CallRequest::new("buy")
        .arg("id", text("o1"))
        .arg("cost", int(20));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the child's meter over-spend rejects the whole transition: {outcome:?}"
    );
    assert_eq!(bank_balance(&host), int(10), "the child meter is unchanged");
    assert!(
        root_order_ids(&host).is_empty(),
        "the parent's order insert did not commit"
    );
}

/// §13.10 fold: an `interface_call` on a child whose exposed mutation reaches a peer
/// folds the two transitions into ONE atomic commit — the shop order and the bank
/// meter spend commit together.
#[test]
fn interface_call_folds_a_peer_reaching_child_transition() {
    let mut host = host_with_shop_and_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("place")
        .arg("id", text("s1"))
        .arg("cost", int(3));
    let outcome = host
        .interface_call(
            &at("shop"),
            "orders",
            "place",
            &request,
            &mut generator(),
        )
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the folded shop+bank transition commits: {outcome:?}"
    );
    assert_eq!(
        child_order_ids(&host, "shop"),
        vec!["s1".to_owned()],
        "the shop order committed"
    );
    assert_eq!(
        bank_balance(&host),
        int(7),
        "the bank meter spend committed (10 - 3)"
    );
}

/// §13.10 fold: when the reached peer rejects, the child's own transition rejects
/// with it — the shop order does not commit either.
#[test]
fn interface_call_fold_rejects_when_the_peer_rejects() {
    let mut host = host_with_shop_and_bank();
    assert_eq!(bank_balance(&host), int(10));

    // cost 20 > balance 10: the bank peer's `consume` assertion fails.
    let request = CallRequest::new("place")
        .arg("id", text("s1"))
        .arg("cost", int(20));
    let outcome = host
        .interface_call(
            &at("shop"),
            "orders",
            "place",
            &request,
            &mut generator(),
        )
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the peer's over-spend rejects the whole folded transition: {outcome:?}"
    );
    assert!(
        child_order_ids(&host, "shop").is_empty(),
        "the shop order did not commit"
    );
    assert_eq!(bank_balance(&host), int(10), "the bank meter is unchanged");
}

/// §13.10 canonical form: an in-program `$use` PEER-ALIAS dispatch (`#credits`, a
/// handle name DISTINCT from the installed instance name `bank`) resolves through
/// the caller's §13.5 peer bindings to the bank instance and commits atomically with
/// the caller — the exact form §13.10's own example uses, proven on the in-memory
/// store. Before the peer-alias fix, `#credits` matched no installed child NAME and
/// the dispatch rejected as "no reachable module instance".
#[test]
fn peer_alias_dispatch_resolves_to_the_peer_and_commits_atomically() {
    let mut host = host_with_shop_and_bank();
    assert_eq!(
        bank_balance(&host),
        int(10),
        "the bank starts with 10 credits"
    );
    assert!(
        child_order_ids(&host, "shop").is_empty(),
        "no shop order before the transition"
    );

    let request = CallRequest::new("place")
        .arg("id", text("s1"))
        .arg("cost", int(4));
    let outcome = host
        .interface_call(
            &at("shop"),
            "orders",
            "place",
            &request,
            &mut generator(),
        )
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the canonical `#credits` peer-alias transition commits atomically on memory: {outcome:?}"
    );
    // The alias `credits` resolved to the `bank` instance: its meter spend committed
    // together with the caller's own order — one atomic cross-module transition.
    assert_eq!(
        child_order_ids(&host, "shop"),
        vec!["s1".to_owned()],
        "the caller's order committed"
    );
    assert_eq!(
        bank_balance(&host),
        int(6),
        "the aliased peer's meter spend committed (10 - 4)"
    );
}

/// §13.10 import scope: a module that hard-codes `#bank` on a sibling it never
/// declared under `$use` is refused LOUDLY at dispatch — it cannot over-reach a
/// non-imported sibling by raw instance name, so neither its own order nor the
/// bank's meter changes.
#[test]
fn dispatch_to_a_non_imported_sibling_is_refused() {
    let mut host = host_with_rogue_and_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("place")
        .arg("id", text("r1"))
        .arg("cost", int(3));
    let outcome = host
        .interface_call(
            &at("rogue"),
            "orders",
            "place",
            &request,
            &mut generator(),
        )
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "reaching a non-imported sibling is refused (import scope): {outcome:?}"
    );
    assert!(
        child_order_ids(&host, "rogue").is_empty(),
        "the over-reaching order did not commit"
    );
    assert_eq!(
        bank_balance(&host),
        int(10),
        "the non-imported sibling was never reached"
    );
}

/// §13.11 actor propagation: the folded/dispatched child admits under the external
/// request's `$actor`, so a child mutation reading `$actor` resolves the caller's
/// identity (against the child's own actor collection) rather than an unbound actor.
/// Here the root `buy` carries `$actor = alice` and dispatches `#bank.consume`; the
/// bank records a receipt whose `who` is the propagated actor.
#[test]
fn dispatched_child_observes_the_parents_actor() {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(
        &collection(),
        InstallRequest::new("bank", BANK_AUTH),
        &mut generator(),
    )
    .expect("the bank child installs");

    let request = CallRequest::new("buy")
        .arg("id", text("o1"))
        .arg("cost", int(4))
        .actor(text("alice"));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the child bound the propagated `$actor` and committed: {outcome:?}"
    );
    // The dispatched child recorded the PARENT's actor key in its receipt.
    let receipts = host
        .interface_read(&at("bank"), "receipts")
        .expect("read")
        .expect("receipts exposed");
    let who = receipts.rows()[0].field("who").expect("who is projected");
    assert_eq!(
        who,
        &text("alice"),
        "the child observed the parent's `$actor` (alice)"
    );
}

/// §13.11 fail-closed contrast: the SAME dispatch with NO actor bound leaves the
/// child's `$actor` unbound, so its `$actor`-reading mutation faults and the whole
/// transition rejects — proving the child genuinely reads the propagated identity
/// (the commit above was not vacuous).
#[test]
fn dispatched_child_without_actor_faults_closed() {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(
        &collection(),
        InstallRequest::new("bank", BANK_AUTH),
        &mut generator(),
    )
    .expect("the bank child installs");

    let request = CallRequest::new("buy")
        .arg("id", text("o1"))
        .arg("cost", int(4));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "an unbound `$actor` in the reached child fails closed and rejects the transition: {outcome:?}"
    );
    assert!(root_order_ids(&host).is_empty(), "nothing committed");
}

/// A module whose exposed `ping` mutation dispatches its OPTIONAL same-line peer's
/// `ping` — the building block of a cyclic dispatch graph (§13.10). Two instances
/// bound to each other make `ping` recurse across the boundary without bound.
const LOOPER: &str = r#"{
  "$liasse": 1
  "$module": "t.multi.loop@1.0.0"
  "$use": { "peer": "t.multi.loop/hop@1" }
  "$model": {
    "marks": { "$key": "id", "id": "text" }
    "marks_view": { "$view": ".marks { id }" }
    "$mut": {
      "ping": [
        "m = .marks + { id: @id }"
        "r = #peer.ping({ id: @id })"
        "return m { id }"
      ]
    }
  }
  "$expose": {
    "hop": {
      "$view": ".marks { id }"
      "$mut": { "ping": ".ping" }
    }
  }
}"#;

/// A metered `acct` module for the NESTED same-engine re-entry probe (§13.10). Its
/// exposed `a`→`outer` spends its own pool and then dispatches its `mid` peer's
/// `bounce`; its exposed `b`→`inner` spends the SAME pool with NO dispatch. Paired
/// with `MID` over a finite `acct ⇄ mid` cycle, `outer` re-enters `acct`'s own
/// `inner` WHILE `outer` is still mid-staging — the in-flight re-entry the fix must
/// refuse rather than double-commit.
const ACCT: &str = r#"{
  "$liasse": 1
  "$module": "t.acct@1.0.0"
  "$use": { "mid": "t.mid/hop@1" }
  "$model": {
    "pools": { "$key": "id", "id": "text", "balance": "int" }
    "credits_view": { "$view": ".pools { id, balance }" }
    "$mut": {
      "outer": [
        ".pools['main'].balance = .pools['main'].balance - @amount"
        "r = #mid.bounce({ amount: @amount })"
        "return { remaining: .pools['main'].balance }"
      ]
      "inner": [
        ".pools['main'].balance = .pools['main'].balance - @amount"
        "return { remaining: .pools['main'].balance }"
      ]
    }
  }
  "$data": { "pools": { "main": { "balance": "10" } } }
  "$expose": {
    "a": {
      "$view": ".pools { id, balance }"
      "$mut": { "outer": ".outer" }
    }
    "b": {
      "$view": ".pools { id, balance }"
      "$mut": { "inner": ".inner" }
    }
  }
}"#;

/// The `mid` hop that closes the `acct ⇄ mid` cycle: its exposed `hop`→`bounce`
/// dispatches its `back` peer's `inner` (bound to `acct`'s `b` interface) and writes
/// nothing of its own — so the only staged change is `acct`'s, whose re-entrant
/// second spend the coordinator must refuse rather than silently double-commit.
const MID: &str = r#"{
  "$liasse": 1
  "$module": "t.mid@1.0.0"
  "$use": { "back": "t.acct/b@1" }
  "$model": {
    "marks": { "$key": "id", "id": "text" }
    "marks_view": { "$view": ".marks { id }" }
    "$mut": {
      "bounce({ amount: int })": [
        "r = #back.inner({ amount: @amount })"
        "return { ok: @amount }"
      ]
    }
  }
  "$expose": {
    "hop": {
      "$view": ".marks { id }"
      "$mut": { "bounce": ".bounce" }
    }
  }
}"#;

/// A host with a second metered `bank` instance named `vault`, so a root program can
/// interleave dispatches to two DISTINCT engines (`#bank`, `#vault`, `#bank`) and
/// prove each composes on its OWN prospective, keyed by its own index (§13.10).
fn host_with_bank_and_vault() -> ModuleHost<MemoryStoreFactory> {
    let mut host = host_with_bank();
    host.install(
        &collection(),
        InstallRequest::new("vault", BANK),
        &mut generator(),
    )
    .expect("the vault child installs");
    host
}

/// A host closing the finite `acct ⇄ mid` cycle via the optional-peer reinstall
/// trick (as the cyclic depth-cap test does): install `mid` (optional `back`,
/// resolves absent), install `acct` (optional `mid` → binds `mid`), uninstall `mid`,
/// reinstall `mid` (optional `back` → binds `acct`). Now `acct.outer` reaches
/// `mid.bounce`, which reaches back into `acct.inner`.
fn host_with_acct_mid_cycle() -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(
        &collection(),
        InstallRequest::new("mid", MID).optional_use("back", "t.acct/b@1"),
        &mut generator(),
    )
    .expect("mid installs (optional back resolves absent)");
    host.install(
        &collection(),
        InstallRequest::new("acct", ACCT).optional_use("mid", "t.mid/hop@1"),
        &mut generator(),
    )
    .expect("acct installs, binding mid");
    host.uninstall(&at("mid")).expect("mid uninstalls");
    host.install(
        &collection(),
        InstallRequest::new("mid", MID).optional_use("back", "t.acct/b@1"),
        &mut generator(),
    )
    .expect("mid re-installs, now bound to acct");
    host
}

/// The `acct` pool balance as read through its exposed `a` interface.
fn acct_balance(host: &ModuleHost<MemoryStoreFactory>) -> Value {
    let view = host
        .interface_read(&at("acct"), "a")
        .expect("read")
        .expect("interface a is exposed");
    view.rows()[0]
        .field("balance")
        .expect("balance is projected")
        .clone()
}

/// The named metered instance's pool balance, read through its `credits` interface.
fn instance_balance(host: &ModuleHost<MemoryStoreFactory>, name: &str) -> Value {
    let view = host
        .interface_read(&at(name), "credits")
        .expect("read")
        .expect("credits is exposed");
    view.rows()[0]
        .field("balance")
        .expect("balance is projected")
        .clone()
}

/// §13.10 sequential re-entrancy: THREE same-engine dispatches in one transition
/// (`#bank.consume` at 2, 3, 4) each read all prior writes and compose to a single
/// commit — 10 − 2 − 3 − 4 = 1 — never a lost update or a double-apply. Each dispatch
/// is a COMPLETED sibling (off the active stack, recorded in `scratch`) by the time
/// the next is reached, so the overlay composition holds.
#[test]
fn sequential_same_engine_composes_three_spends() {
    let mut host = host_with_bank();
    assert_eq!(bank_balance(&host), int(10));

    let request = CallRequest::new("buy_thrice")
        .arg("id", text("o1"))
        .arg("c1", int(2))
        .arg("c2", int(3))
        .arg("c3", int(4));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the three composed spends commit as one transition: {outcome:?}"
    );
    assert_eq!(
        bank_balance(&host),
        int(1),
        "all three spends applied (10 - 2 - 3 - 4), not just the last"
    );
    assert_eq!(
        root_order_ids(&host),
        vec!["o1".to_owned()],
        "the parent order committed"
    );
}

/// §13.10 interleaved re-entrancy: `#bank`(3), `#vault`(5), `#bank`(4) in ONE
/// transition — the two `#bank` dispatches accumulate on bank's single prospective
/// (10 − 3 − 4 = 3) while `#vault` composes independently on its own (10 − 5 = 5),
/// keyed by each engine's own index. A completed same-engine sibling composes; the
/// distinct engine stays disjoint.
#[test]
fn interleaved_same_engine_composes_per_engine() {
    let mut host = host_with_bank_and_vault();
    assert_eq!(instance_balance(&host, "bank"), int(10));
    assert_eq!(instance_balance(&host, "vault"), int(10));

    let request = CallRequest::new("buy_interleaved")
        .arg("id", text("o1"))
        .arg("a", int(3))
        .arg("b", int(5))
        .arg("c", int(4));
    let outcome = host
        .call_multi(&request, &mut generator())
        .expect("no engine fault");

    assert!(
        matches!(outcome, CallOutcome::Committed { .. }),
        "the interleaved transition commits: {outcome:?}"
    );
    assert_eq!(
        instance_balance(&host, "bank"),
        int(3),
        "both bank dispatches accumulate on one prospective (10 - 3 - 4)"
    );
    assert_eq!(
        instance_balance(&host, "vault"),
        int(5),
        "vault composed independently on its own index (10 - 5)"
    );
}

/// §13.10 nested re-entry (the residual lost-update the fix closes): an
/// `interface_call(acct.a.outer)` over a finite `acct ⇄ mid` cycle re-enters `acct`'s
/// own `inner` WHILE `outer` is still mid-staging on the active dispatch stack. That
/// in-flight change lives OUTSIDE `scratch`, so it cannot be composed by the overlay
/// — the coordinator must REFUSE the re-entry LOUDLY rather than stage `inner` against
/// committed state and double-commit `acct` (which committed balance 7, one spend
/// silently lost, before the fix). Nothing commits: `acct` keeps its prior balance.
#[test]
fn nested_reentry_to_an_in_flight_engine_is_refused() {
    let mut host = host_with_acct_mid_cycle();
    assert_eq!(acct_balance(&host), int(10), "acct starts with 10");

    // outer spends 3, then dispatches mid.bounce, which dispatches back into acct.inner
    // WHILE outer is still staging — a re-entry to an in-flight engine.
    let request = CallRequest::new("outer").arg("amount", int(3));
    let outcome = host
        .interface_call(&at("acct"), "a", "outer", &request, &mut generator())
        .expect("no engine fault — the nested re-entry is a rejection, not a crash");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the nested re-entry to an in-flight engine is refused loudly: {outcome:?}"
    );
    // The prohibited outcome (double-commit) would have left balance 7 (10 committed
    // twice: outer's 10-3 then inner's 10-3 clobbering it). The transition rejects, so
    // acct keeps its prior committed balance — NOT the doubled 7.
    let balance = acct_balance(&host);
    assert_ne!(
        balance,
        int(7),
        "the double-commit (silent lost update) must NOT stand"
    );
    assert_eq!(
        balance,
        int(10),
        "acct is left at its prior committed state — nothing committed"
    );
}

/// §13.10 depth cap: a cyclic cross-engine dispatch graph (`a` ⇄ `b`, each `ping`
/// dispatching the other's `ping`) is refused LOUDLY after a bounded number of
/// boundary hops — the test COMPLETING is itself the proof it did not overflow the
/// stack. The mutual binding is built by installing `a` (optional peer, absent),
/// then `b` (binds to `a`), then re-installing `a` so it binds to `b` — closing the
/// cycle both ways.
#[test]
fn cyclic_cross_engine_dispatch_is_refused_without_overflow() {
    let root: Engine<MemoryStore> = support::load("t.multi.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    let peer = "t.multi.loop/hop@1";
    // a: optional peer resolves absent (no sibling yet).
    host.install(
        &collection(),
        InstallRequest::new("a", LOOPER).optional_use("peer", peer),
        &mut generator(),
    )
    .expect("a installs");
    // b: its optional peer auto-binds to the only candidate, a.
    host.install(
        &collection(),
        InstallRequest::new("b", LOOPER).optional_use("peer", peer),
        &mut generator(),
    )
    .expect("b installs");
    // Re-install a so its optional peer now binds to b — closing the a ⇄ b cycle.
    host.uninstall(&at("a")).expect("a uninstalls");
    host.install(
        &collection(),
        InstallRequest::new("a", LOOPER).optional_use("peer", peer),
        &mut generator(),
    )
    .expect("a re-installs, now bound to b");

    let request = CallRequest::new("ping").arg("id", text("x"));
    let outcome = host
        .interface_call(&at("a"), "hop", "ping", &request, &mut generator())
        .expect("no engine fault — the cycle is a rejection, not a crash");

    assert!(
        matches!(outcome, CallOutcome::Rejected(_)),
        "the cyclic dispatch is refused loudly (no stack overflow): {outcome:?}"
    );
    // Nothing committed: an aborted transition leaves every engine at its prior state.
    let marks = host
        .interface_read(&at("b"), "hop")
        .expect("read")
        .expect("hop exposed");
    assert!(
        marks.rows().is_empty(),
        "the aborted cyclic transition committed nothing"
    );
}
