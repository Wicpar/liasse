//! Admission concurrency: what two overlapping writers of one instance may and may
//! not do.
//!
//! Admission used to open with `SELECT head FROM instance_meta WHERE id = 1 FOR
//! UPDATE` — a per-instance write mutex, so no two admissions to an instance could
//! overlap at all. It is gone: a commit transaction writes state and an unpositioned
//! residual, and serial positions are stamped afterwards over *settled* admissions
//! ([`liasse_pg`]'s `history`). These cases pin the three things that has to mean.
//!
//! 1. **No instance-wide meeting point** — an exclusive lock held on the instance's
//!    metadata does not stop a data mutation from being admitted. Under the old
//!    protocol the admission blocked on that row; it now never touches it.
//! 2. **Overlapping admissions both commit**, and history still gives them one
//!    monotone order.
//! 3. **The order is settled before it is published** — an admission that began
//!    earlier but commits later still takes the earlier position, and *nothing* is
//!    positioned while it is in flight. This is the property assigning positions
//!    after settlement exists to protect: a reader can never see a later commit and
//!    then watch an earlier one appear in its past.
//!
//! Plus the two silent-lost-update paths the old mutex was masking: an insert over a
//! row a concurrent admission created, and an update of a row a concurrent admission
//! removed. Both must be refused loudly, because a clobber that succeeds quietly is
//! strictly worse than the bottleneck this change removes.
// The standard integration-test preamble every sibling file in this directory
// carries: a test asserts by panicking (AGENTS.md), so the no-panic lints do not
// apply to it.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use liasse_ident::{InstanceId, NameSegment};
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_store::{
    AddressStep, CollectionPath, CommitOutcome, CommitSeq, InstanceStore, KeyValue, RowAddress,
    StoreError, Transition,
};
use liasse_value::{Integer, Text, Value};

/// Long enough that a genuinely unblocked admission always finishes inside it, and
/// short enough that a blocked one is reported as blocked rather than hanging the run.
const UNBLOCKED: Duration = Duration::from_secs(20);

/// How long a case waits before concluding that an admission which *should* be
/// waiting really is waiting. Only ever used to observe the absence of a result.
const STILL_WAITING: Duration = Duration::from_millis(500);

fn address(key: i64) -> RowAddress {
    RowAddress::root(AddressStep::new(
        NameSegment::new("items"),
        KeyValue::single(Value::Int(Integer::from(key))),
    ))
}

fn items() -> CollectionPath {
    CollectionPath::top(NameSegment::new("items"))
}

fn payload(text: &str) -> Value {
    Value::Text(Text::new(text))
}

/// Admit one single-row insert and report the position history gave it.
fn admit(store: &mut PgStore, key: i64, text: &str) -> Result<CommitOutcome, StoreError> {
    let mut txn = store.begin();
    txn.insert(address(key), payload(text))?;
    txn.commit()
}

/// A fresh instance plus two independent store handles over it — two writer
/// connections to one schema, which is what "two concurrent admissions" means for
/// this backend.
fn two_handles(seed: &str, handle: &support::PgHandle) -> (PgStoreFactory, InstanceId, PgStore, PgStore) {
    let factory = handle.factory(seed);
    let instance = InstanceId::new("concurrent-admission");
    let first = {
        let mut factory = factory.clone();
        <PgStoreFactory as liasse_store::StoreFactory>::create(&mut factory, instance.clone())
            .expect("create the instance")
    };
    let second = factory.reopen(instance.clone()).expect("second handle over the same instance");
    (factory, instance, first, second)
}

/// An exclusive lock on the instance's metadata row must not stop a data mutation
/// from being admitted.
///
/// This is the direct test of the removed mutex. `LOCK TABLE … IN EXCLUSIVE MODE`
/// conflicts with the `ROW SHARE` the old `SELECT … FOR UPDATE` took, so under the
/// previous protocol the admission below waited for the lock holder and this case
/// timed out. It also assigns no transaction id of its own, so it cannot hold the
/// history watermark down — what is being observed is the lock, and only the lock.
///
/// A data mutation now touches `instance_meta` nowhere: the head counter it used to
/// bump does not exist (the built history's tip is the head), the incarnation counter
/// moved to a sequence, and the definition/composition columns are written only by a
/// transition that actually changes one.
#[test]
fn an_instance_wide_lock_does_not_block_admission() {
    let handle = support::acquire();
    let (factory, instance, mut store, _second) = two_handles("lockfree", &handle);
    let _guard = support::SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance).quoted();

    // Hold an exclusive lock on the instance metadata for the whole of the admission.
    let mut blocker = factory.connect().expect("blocker connection");
    let mut held = blocker.transaction().expect("begin blocker");
    held.batch_execute(&format!("LOCK TABLE {schema}.instance_meta IN EXCLUSIVE MODE"))
        .expect("take the instance-wide lock");

    let (done, admitted) = mpsc::channel();
    let writer = thread::spawn(move || {
        let outcome = admit(&mut store, 1, "under an instance-wide lock");
        done.send(outcome).ok();
    });

    // Release the lock BEFORE asserting: a failing assertion here means the
    // admission is still blocked, and the schema teardown that runs on unwind would
    // then queue behind this very lock and wedge the run instead of reporting.
    let outcome = admitted.recv_timeout(UNBLOCKED);
    held.rollback().expect("release the blocker");

    let outcome = outcome
        .expect("admission must not wait on the instance metadata lock")
        .expect("admission must succeed");
    assert_eq!(outcome, CommitOutcome::Committed(CommitSeq::from_stored(1)));
    writer.join().expect("writer thread");
}

/// Two handles admitting to one instance both commit, and history gives them one
/// monotone order covering every admission exactly once.
#[test]
fn overlapping_admissions_all_commit_in_one_monotone_order() {
    const EACH: i64 = 8;

    let handle = support::acquire();
    let (factory, instance, mut first, mut second) = two_handles("overlap", &handle);
    let _guard = support::SchemaGuard::new(&factory, instance.clone());

    let left = thread::spawn(move || {
        (0..EACH).map(|i| admit(&mut first, i * 2, "left")).collect::<Vec<_>>()
    });
    let right = thread::spawn(move || {
        (0..EACH).map(|i| admit(&mut second, i * 2 + 1, "right")).collect::<Vec<_>>()
    });

    let mut positions: Vec<u64> = Vec::new();
    for side in [left.join().expect("left thread"), right.join().expect("right thread")] {
        // Each writer's own admissions are ordered among themselves: it waited for
        // one before issuing the next, so the declared coherence property applies.
        let mut own = Vec::new();
        for outcome in side {
            match outcome.expect("every admission must commit") {
                CommitOutcome::Committed(seq) => own.push(seq.get()),
                CommitOutcome::Unchanged => panic!("a single-row insert is never unchanged"),
            }
        }
        assert!(
            own.is_sorted_by(|earlier, later| earlier < later),
            "one writer's own admissions must be strictly increasing, got {own:?}"
        );
        positions.extend(own);
    }

    positions.sort_unstable();
    let expected: Vec<u64> = (1..=u64::try_from(EACH * 2).expect("small count")).collect();
    assert_eq!(positions, expected, "every admission takes exactly one distinct position");

    // Every row landed, and the head is the last position handed out.
    let reader = factory.reopen(instance).expect("reader handle");
    assert_eq!(reader.head().expect("head"), CommitSeq::from_stored(EACH as u64 * 2));
    assert_eq!(reader.scan(&items()).expect("scan").len(), (EACH * 2) as usize);
}

/// Nothing is positioned while an earlier admission is still in flight, and when it
/// settles it takes the earlier position — even though it committed last.
///
/// The straggler here is a real, in-flight `commit_log` residual written on a raw
/// connection and held uncommitted: an admission that has begun but not settled. A
/// second handle then admits and commits *first*. If positions were minted during
/// admission, that second admission would already hold a position and the straggler
/// would have to be squeezed in behind it — the anomaly. Instead: while the straggler
/// is open a history pass positions nothing at all (not even the admission that has
/// already committed), and once it settles the two are positioned in the order they
/// began.
#[test]
fn a_straggler_holds_history_back_and_then_takes_its_earlier_place() {
    let handle = support::acquire();
    let (factory, instance, mut builder, mut second) = two_handles("straggler", &handle);
    let _guard = support::SchemaGuard::new(&factory, instance.clone());
    let schema = factory.schema_for(&instance).quoted();

    // Both handles and the straggler's connection are opened up front: opening a
    // store reconciles the schema, and schema DDL waits behind a transaction that has
    // written to the tables it touches. Nothing here may wait on the straggler.
    let mut straggler = factory.connect().expect("straggler connection");

    // A settled baseline, so the straggler's `created` can be copied from a genuine
    // record rather than hand-built.
    assert_eq!(
        admit(&mut builder, 1, "baseline").expect("baseline"),
        CommitOutcome::Committed(CommitSeq::from_stored(1))
    );

    // The straggler: an admission that has written its residual and not committed.
    let mut in_flight = straggler.transaction().expect("begin straggler");
    in_flight
        .execute(
            &format!(
                "INSERT INTO {schema}.commit_log (transaction_id, ops, created) \
                 SELECT NULL, '[]'::jsonb, created FROM {schema}.commit_log ORDER BY seq LIMIT 1"
            ),
            &[],
        )
        .expect("write the straggler residual");

    // A second admission begins after it, and commits before it.
    let (done, admitted) = mpsc::channel();
    let writer = thread::spawn(move || {
        done.send(admit(&mut second, 2, "overtaker")).ok();
    });

    // Its state is committed, but history refuses to position it while an older
    // admission could still claim an earlier place.
    assert!(
        admitted.recv_timeout(STILL_WAITING).is_err(),
        "no position may be published while an earlier admission is in flight"
    );
    assert_eq!(builder.build_history().expect("pass"), 0, "a pass positions nothing yet");
    assert_eq!(
        builder.head().expect("head"),
        CommitSeq::from_stored(1),
        "the head stays at the last settled admission"
    );

    in_flight.commit().expect("settle the straggler");

    // Now both settle, in the order they began: the straggler took its identity
    // first, so it takes position 2 and the admission that overtook it takes 3.
    let outcome = admitted.recv_timeout(UNBLOCKED).expect("settled").expect("committed");
    assert_eq!(
        outcome,
        CommitOutcome::Committed(CommitSeq::from_stored(3)),
        "the later-starting admission is ordered after the straggler it overtook"
    );
    writer.join().expect("writer thread");
    assert_eq!(builder.head().expect("head"), CommitSeq::from_stored(3));
}

/// An insert onto an address a concurrent admission has just filled is refused, not
/// silently applied over the row that is there.
///
/// Staging checks occupancy against a read taken before the admission transaction
/// opens, so with two writers that check can be stale — the exact window the old
/// per-instance mutex hid. The durable write is what has to catch it, and it does:
/// the node placement revives a tombstone and refuses a live row.
#[test]
fn an_insert_over_a_concurrently_created_row_is_refused() {
    let handle = support::acquire();
    let (factory, instance, mut first, mut second) = two_handles("insert-race", &handle);
    let _guard = support::SchemaGuard::new(&factory, instance);

    // The first writer stages against an empty collection…
    let mut staged = first.begin();
    staged.insert(address(1), payload("first")).expect("stage the insert");

    // …while the second writer fills the address and commits.
    admit(&mut second, 1, "second").expect("the second writer commits").assert_committed();

    let refused = staged.commit();
    assert!(
        matches!(refused, Err(StoreError::Conflict { .. })),
        "an insert over a live row must be refused, got {refused:?}"
    );
    assert_eq!(
        second.row(&address(1)).expect("read").map(|row| row.value().clone()),
        Some(payload("second")),
        "the committed row must be untouched"
    );
}

/// An update of a row a concurrent admission has removed is refused, not resurrected.
///
/// The op carries the incarnation staging read; the durable write matches on it, so a
/// target that is no longer the row that was read takes the whole admission down.
#[test]
fn an_update_of_a_concurrently_deleted_row_is_refused() {
    let handle = support::acquire();
    let (factory, instance, mut first, mut second) = two_handles("update-race", &handle);
    let _guard = support::SchemaGuard::new(&factory, instance);

    admit(&mut first, 1, "original").expect("seed").assert_committed();

    // The first writer stages an update of the live row…
    let mut staged = first.begin();
    staged.update(&address(1), payload("edited")).expect("stage the update");

    // …while the second writer deletes it and commits.
    let mut deleting = second.begin();
    deleting.delete(&address(1)).expect("stage the delete");
    deleting.commit().expect("the delete commits").assert_committed();

    let refused = staged.commit();
    assert!(
        matches!(refused, Err(StoreError::Conflict { .. })),
        "an update of a removed row must be refused, got {refused:?}"
    );
    assert!(
        second.row(&address(1)).expect("read").is_none(),
        "the deleted row must stay deleted"
    );
}

/// Assertion helper: a committed outcome, or a named failure.
trait AssertCommitted {
    fn assert_committed(self);
}

impl AssertCommitted for CommitOutcome {
    fn assert_committed(self) {
        assert!(matches!(self, Self::Committed(_)), "expected a commit, got {self:?}");
    }
}
