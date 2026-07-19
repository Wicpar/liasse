//! The PostgreSQL backend driven through the reusable contract battery — the
//! identical suite that checks the in-memory reference (`liasse-store`). Each
//! contract guarantee is one isolated test so a failure names the violated
//! invariant, and each runs in its own throwaway schema (a unique namespace).
//!
//! The DSN and (if needed) a disposable cluster are resolved by [`support`];
//! that module documents the resolution order and fails with an actionable
//! message if no PostgreSQL can be reached — the suite never silently passes.
//! Each test drops its throwaway schema through a [`support::SchemaGuard`], so
//! cleanup happens even when an assertion panics.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::InstanceId;
use liasse_pg::PgStoreFactory;
use liasse_store::contract_tests as suite;
use liasse_store::{InstanceStore, StoreError, StoreFactory, Transition};

use support::SchemaGuard;

/// The instance identity every `contract_tests` function uses.
const SUITE_INSTANCE: &str = "instance-under-test";

/// Run one battery function against a fresh throwaway schema. A `SchemaGuard`
/// drops the schema whether the test returns, errors, or panics on a failed
/// assertion; the `PgHandle` keeps the shared cluster alive for the test's
/// duration and lets it be torn down once the last test in the process finishes.
fn run(test: fn(&mut PgStoreFactory) -> Result<(), StoreError>, what: &str) {
    let handle = support::acquire();
    let mut factory = handle.factory("conf");
    let _schema = SchemaGuard::new(&factory, InstanceId::new(SUITE_INSTANCE));
    test(&mut factory).expect(what);
}

#[test]
fn serial_positions_gapless_and_monotone() {
    run(suite::serial_positions_gapless_and_monotone, "gapless monotone positions");
}

#[test]
fn commit_is_all_or_nothing() {
    run(suite::commit_is_all_or_nothing, "atomic commit across staged writes");
}

#[test]
fn aborted_staging_leaves_no_trace() {
    run(suite::aborted_staging_leaves_no_trace, "abort leaves no trace");
}

#[test]
fn abort_then_commit_keeps_positions_gapless() {
    run(
        suite::abort_then_commit_keeps_positions_gapless,
        "abort then a different commit keeps positions gapless",
    );
}

#[test]
fn snapshot_at_frontier_ignores_later_commits() {
    run(suite::snapshot_at_frontier_ignores_later_commits, "frontier snapshot ignores later commits");
}

#[test]
fn scan_order_matches_annex_b() {
    run(suite::scan_order_matches_annex_b, "scan is in Annex B key order");
}

#[test]
fn rekey_preserves_incarnation() {
    run(suite::rekey_preserves_incarnation, "rekey preserves incarnation");
}

#[test]
fn replay_from_seq_reproduces() {
    run(suite::replay_from_seq_reproduces, "replay reproduces committed transitions");
}

#[test]
fn blob_round_trips_by_digest() {
    run(suite::blob_round_trips_by_digest, "blobs round-trip by digest");
}

#[test]
fn metadata_persists_through_commit() {
    run(suite::metadata_persists_through_commit, "definition and composition persist");
}

#[test]
fn history_points_map_to_positions() {
    run(suite::history_points_map_to_positions, "history points map to positions");
}

/// Durability past a process restart: state written through one connection is
/// rebuilt identically from the durable tables by a second, independent open.
/// This is the guarantee the in-memory reference cannot make and PostgreSQL can.
#[test]
fn reopen_rebuilds_state_from_durable_tables() {
    use liasse_store::{
        AddressStep, CollectionPath, DefinitionText, KeyValue, RowAddress, StoredRow,
    };
    use liasse_value::{Integer, Text, Value};

    let handle = support::acquire();
    let mut factory = handle.factory("reopen");
    let instance = InstanceId::new(SUITE_INSTANCE);
    let _schema = SchemaGuard::new(&factory, instance.clone());

    let address = |key: i64| {
        RowAddress::root(AddressStep::new(
            liasse_ident::NameSegment::new("items"),
            KeyValue::single(Value::Int(Integer::from(key))),
        ))
    };
    let items = CollectionPath::top(liasse_ident::NameSegment::new("items"));

    let (head, projected, digest) = {
        let mut store = factory.create(instance.clone()).expect("create");
        let mut txn = store.begin();
        txn.insert(address(1), Value::Text(Text::new("one"))).expect("insert 1");
        txn.insert(address(2), Value::Text(Text::new("two"))).expect("insert 2");
        txn.set_definition(DefinitionText::new("{ \"$liasse\": 1 }"));
        txn.commit().expect("commit");
        let digest = store.put_blob(b"durable bytes").expect("blob");

        let head = store.head().unwrap();
        let projected: Vec<(RowAddress, Value)> = store
            .scan(&items)
            .expect("scan")
            .into_iter()
            .map(|(a, r): (RowAddress, StoredRow)| (a, r.value().clone()))
            .collect();
        (head, projected, digest)
        // `store` drops here, closing its connection.
    };

    let reopened = factory.reopen(instance.clone()).expect("reopen");
    assert_eq!(reopened.head().unwrap(), head, "head survives a reopen");
    let after: Vec<(RowAddress, Value)> = reopened
        .scan(&items)
        .expect("scan")
        .into_iter()
        .map(|(a, r)| (a, r.value().clone()))
        .collect();
    assert_eq!(after, projected, "rows survive a reopen");
    assert_eq!(
        reopened.get_blob(&digest).expect("blob read").as_deref(),
        Some(b"durable bytes".as_slice()),
        "blob bytes survive a reopen"
    );
    assert!(reopened.definition().unwrap().is_some(), "definition survives a reopen");
}
