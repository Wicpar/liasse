//! Reusable contract-conformance suite.
//!
//! Every function here is generic over a [`StoreFactory`], so any backend — the
//! in-memory reference here, PostgreSQL next — runs the identical battery. Each
//! expectation is externally deducible (the suite knows what it wrote), never a
//! comparison of the store against its own answer. A backend's own `tests/`
//! invoke [`run_all`].
//!
//! The functions return `Result` for plumbing failures and use `assert!` for
//! invariant checks, so a violation fails the calling test.

use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_value::{Integer, Text, Value};

use crate::commit::{CommitOutcome, CommitSeq};
use crate::contract::{InstanceStore, StoreFactory, Transition};
use crate::error::StoreError;
use crate::key::{AddressStep, CollectionPath, KeyValue, RowAddress};
use crate::meta::{Composition, DefinitionText, Mount};
use crate::row::StoredRow;
use crate::snapshot::Snapshot;

fn instance() -> InstanceId {
    InstanceId::new("instance-under-test")
}

fn collection(name: &str) -> CollectionPath {
    CollectionPath::top(NameSegment::new(name))
}

fn address(name: &str, key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new(name),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

fn payload(text: &str) -> Value {
    Value::Text(Text::new(text))
}

/// The `(address, value)` projection of a scan — the externally-known shape,
/// with the store-allocated incarnation dropped.
fn projected(rows: &[(RowAddress, StoredRow)]) -> Vec<(RowAddress, Value)> {
    rows.iter()
        .map(|(address, row)| (address.clone(), row.value().clone()))
        .collect()
}

/// A non-empty commit must take the next gapless serial position; an empty
/// transition takes none (§22.2, §22.3).
pub fn serial_positions_gapless_and_monotone<F: StoreFactory>(
    factory: &mut F,
) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;
    assert_eq!(store.head(), CommitSeq::GENESIS);

    for expected in 1..=3i64 {
        let mut txn = store.begin();
        txn.insert(address("items", expected), payload("v"))?;
        let outcome = txn.commit()?;
        assert_eq!(outcome, CommitOutcome::Committed(store.head()));
        assert_eq!(i64::try_from(store.head().get()).ok(), Some(expected));
    }

    // An empty transition creates no commit and consumes no position.
    let txn = store.begin();
    assert!(txn.is_empty());
    assert_eq!(txn.commit()?, CommitOutcome::Unchanged);
    assert_eq!(store.head().get(), 3);
    Ok(())
}

/// Every write staged in one transition lands together at one position, or not
/// at all (§22.2).
pub fn commit_is_all_or_nothing<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    txn.insert(address("items", 1), payload("a"))?;
    txn.insert(address("items", 2), payload("b"))?;
    txn.insert(address("items", 3), payload("c"))?;
    txn.commit()?;

    // One position for the whole staged set, all three rows visible.
    assert_eq!(store.head().get(), 1);
    assert_eq!(store.scan(&collection("items"))?.len(), 3);

    // A conflicting op inside a transition errors; aborting keeps state intact.
    let mut txn = store.begin();
    txn.insert(address("items", 4), payload("d"))?;
    assert!(matches!(
        txn.insert(address("items", 1), payload("dup")),
        Err(StoreError::Conflict { .. })
    ));
    txn.abort();
    assert_eq!(store.head().get(), 1);
    assert!(store.row(&address("items", 4))?.is_none());
    Ok(())
}

/// A dropped transition leaves no committed trace (§22.2).
pub fn aborted_staging_leaves_no_trace<F: StoreFactory>(
    factory: &mut F,
) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;
    {
        let mut txn = store.begin();
        txn.insert(address("items", 1), payload("a"))?;
        txn.insert(address("items", 2), payload("b"))?;
        // Read-your-writes sees the staged rows before commit.
        assert!(txn.row(&address("items", 1))?.is_some());
        txn.abort();
    }
    assert_eq!(store.head(), CommitSeq::GENESIS);
    assert!(store.scan(&collection("items"))?.is_empty());
    assert!(store.log_from(CommitSeq::GENESIS)?.is_empty());
    Ok(())
}

/// A snapshot at an earlier frontier is blind to later commits (§22.7, §19.2).
pub fn snapshot_at_frontier_ignores_later_commits<F: StoreFactory>(
    factory: &mut F,
) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    txn.insert(address("items", 1), payload("first"))?;
    txn.commit()?;
    let first = store.head();

    let mut txn = store.begin();
    txn.insert(address("items", 2), payload("second"))?;
    txn.commit()?;

    let at_first = store.snapshot(first)?;
    assert!(at_first.row(&address("items", 1)).is_some());
    assert!(at_first.row(&address("items", 2)).is_none());

    let at_head = store.snapshot(store.head())?;
    assert!(at_head.row(&address("items", 2)).is_some());

    // A frontier past the head is a corruption error, never a silent read.
    assert!(matches!(
        store.snapshot(store.head().next()),
        Err(StoreError::Corruption { .. })
    ));
    Ok(())
}

/// A collection scan is in Annex B key-ascending order regardless of insertion
/// order (B.5).
pub fn scan_order_matches_annex_b<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    for key in [3, 1, 4, 1_000, 2, -5] {
        txn.insert(address("items", key), payload("v"))?;
    }
    txn.commit()?;

    let scanned: Vec<RowAddress> = store
        .scan(&collection("items"))?
        .into_iter()
        .map(|(addr, _)| addr)
        .collect();
    let expected: Vec<RowAddress> = [-5, 1, 2, 3, 4, 1_000].map(|k| address("items", k)).to_vec();
    assert_eq!(scanned, expected);
    Ok(())
}

/// A rekey moves a row while preserving its incarnation; a delete-then-insert
/// gets a fresh one (§5.4, D.1).
pub fn rekey_preserves_incarnation<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    let original = txn.insert(address("items", 1), payload("a"))?;
    txn.commit()?;

    let mut txn = store.begin();
    txn.rekey(&address("items", 1), address("items", 2), payload("a"))?;
    txn.commit()?;

    assert!(store.row(&address("items", 1))?.is_none());
    let moved = store
        .row(&address("items", 2))?
        .ok_or_else(|| StoreError::Corruption { detail: "rekeyed row vanished".to_owned() })?;
    assert_eq!(moved.incarnation(), &original);

    // Delete then re-insert at the same address allocates a new incarnation.
    let mut txn = store.begin();
    txn.delete(&address("items", 2))?;
    let reinserted = txn.insert(address("items", 2), payload("a"))?;
    txn.commit()?;
    assert_ne!(reinserted, original);
    Ok(())
}

/// Folding the log reproduces exactly the state the live commits produced, down
/// to incarnations (§19.2). Both the incremental current read and the cold
/// replay are checked against the suite's own oracle.
pub fn replay_from_seq_reproduces<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    txn.insert(address("items", 1), payload("one"))?;
    txn.insert(address("items", 2), payload("two"))?;
    txn.commit()?;

    let mut txn = store.begin();
    txn.update(&address("items", 1), payload("one-prime"))?;
    txn.insert(address("items", 3), payload("three"))?;
    txn.commit()?;

    let mut txn = store.begin();
    txn.delete(&address("items", 2))?;
    txn.rekey(&address("items", 3), address("items", 9), payload("three"))?;
    txn.commit()?;

    // Positions are the contiguous run 1..=3.
    let log = store.log_from(CommitSeq::GENESIS)?;
    let positions: Vec<u64> = log.iter().map(|t| t.seq().get()).collect();
    assert_eq!(positions, vec![1, 2, 3]);

    // Externally-known final rows.
    let expected = vec![
        (address("items", 1), payload("one-prime")),
        (address("items", 9), payload("three")),
    ];

    let live = store.scan(&collection("items"))?;
    assert_eq!(projected(&live), expected);

    let replayed = Snapshot::replay(&log, store.head())?;
    assert_eq!(projected(&replayed.scan(&collection("items"))), expected);

    // Cold replay reproduces the exact incarnations the live path assigned.
    for (addr, _) in &live {
        let live_row = store.row(addr)?;
        let replay_row = replayed.row(addr).cloned();
        assert_eq!(
            live_row.as_ref().map(StoredRow::incarnation),
            replay_row.as_ref().map(StoredRow::incarnation),
        );
    }
    Ok(())
}

/// Blob bytes round-trip by content digest, and storage is idempotent (§18).
pub fn blob_round_trips_by_digest<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let bytes = b"the quick brown fox".to_vec();
    let digest = store.put_blob(&bytes)?;
    assert!(store.has_blob(&digest));
    assert_eq!(store.get_blob(&digest)?.as_deref(), Some(bytes.as_slice()));

    // Same content, same digest, one stored copy.
    let again = store.put_blob(&bytes)?;
    assert_eq!(again, digest);

    // A different content's digest is not held.
    let other = store.put_blob(b"different")?;
    assert_ne!(other, digest);
    Ok(())
}

/// A staged definition and composition become durable at commit and are then
/// readable (§19.1, §19.5). A metadata-only transition still commits.
pub fn metadata_persists_through_commit<F: StoreFactory>(
    factory: &mut F,
) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;
    assert!(store.definition().is_none());
    assert!(store.composition().is_none());

    let definition = DefinitionText::new("{ \"$liasse\": 1 }");
    let composition = Composition::new().with(
        "child",
        Mount::new(
            InstanceId::new("child-incarnation"),
            HistoryPoint::new(LineageId::new("lineage-a"), PointId::new("point-1")),
        ),
    );

    let mut txn = store.begin();
    txn.set_definition(definition.clone());
    txn.set_composition(composition.clone());
    assert!(!txn.is_empty());
    assert_eq!(txn.commit()?, CommitOutcome::Committed(store.head()));

    assert_eq!(store.head().get(), 1);
    assert_eq!(store.definition(), Some(&definition));
    assert_eq!(store.composition(), Some(&composition));
    Ok(())
}

/// A recorded history point maps to the serial position it names, and a point
/// past the head is rejected (§19.3).
pub fn history_points_map_to_positions<F: StoreFactory>(
    factory: &mut F,
) -> Result<(), StoreError> {
    let mut store = factory.create(instance())?;

    let mut txn = store.begin();
    txn.insert(address("items", 1), payload("a"))?;
    txn.commit()?;
    let position = store.head();

    let point = HistoryPoint::new(LineageId::new("main"), PointId::new("p1"));
    assert!(store.point_position(&point).is_none());
    store.record_point(position, point.clone())?;
    assert_eq!(store.point_position(&point), Some(position));

    let future = HistoryPoint::new(LineageId::new("main"), PointId::new("p2"));
    assert!(matches!(
        store.record_point(store.head().next(), future),
        Err(StoreError::Corruption { .. })
    ));
    Ok(())
}

/// Run the whole battery against `factory`.
pub fn run_all<F: StoreFactory>(factory: &mut F) -> Result<(), StoreError> {
    serial_positions_gapless_and_monotone(factory)?;
    commit_is_all_or_nothing(factory)?;
    aborted_staging_leaves_no_trace(factory)?;
    snapshot_at_frontier_ignores_later_commits(factory)?;
    scan_order_matches_annex_b(factory)?;
    rekey_preserves_incarnation(factory)?;
    replay_from_seq_reproduces(factory)?;
    blob_round_trips_by_digest(factory)?;
    metadata_persists_through_commit(factory)?;
    history_points_map_to_positions(factory)?;
    Ok(())
}
