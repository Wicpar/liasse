//! The in-memory reference implementation run through the reusable contract
//! battery. Each contract guarantee is one isolated test so a failure names the
//! violated invariant. The identical `contract_tests` module drives the
//! PostgreSQL backend next.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use liasse_store::MemoryStoreFactory;
use liasse_store::contract_tests as suite;

#[test]
fn serial_positions_gapless_and_monotone() {
    suite::serial_positions_gapless_and_monotone(&mut MemoryStoreFactory::new())
        .expect("gapless monotone positions");
}

#[test]
fn commit_is_all_or_nothing() {
    suite::commit_is_all_or_nothing(&mut MemoryStoreFactory::new())
        .expect("atomic commit across staged writes");
}

#[test]
fn aborted_staging_leaves_no_trace() {
    suite::aborted_staging_leaves_no_trace(&mut MemoryStoreFactory::new())
        .expect("abort leaves no trace");
}

#[test]
fn abort_then_commit_keeps_positions_gapless() {
    suite::abort_then_commit_keeps_positions_gapless(&mut MemoryStoreFactory::new())
        .expect("abort then a different commit keeps positions gapless");
}

#[test]
fn snapshot_at_frontier_ignores_later_commits() {
    suite::snapshot_at_frontier_ignores_later_commits(&mut MemoryStoreFactory::new())
        .expect("frontier snapshot ignores later commits");
}

#[test]
fn scan_order_matches_annex_b() {
    suite::scan_order_matches_annex_b(&mut MemoryStoreFactory::new())
        .expect("scan is in Annex B key order");
}

#[test]
fn scan_subtree_reaches_nested_orphans() {
    suite::scan_subtree_reaches_nested_orphans(&mut MemoryStoreFactory::new())
        .expect("scan_subtree reaches nested rows and orphans in Annex B order");
}

#[test]
fn rekey_preserves_incarnation() {
    suite::rekey_preserves_incarnation(&mut MemoryStoreFactory::new())
        .expect("rekey preserves incarnation");
}

#[test]
fn replay_from_seq_reproduces() {
    suite::replay_from_seq_reproduces(&mut MemoryStoreFactory::new())
        .expect("replay reproduces committed transitions");
}

#[test]
fn blob_round_trips_by_digest() {
    suite::blob_round_trips_by_digest(&mut MemoryStoreFactory::new())
        .expect("blobs round-trip by digest");
}

#[test]
fn metadata_persists_through_commit() {
    suite::metadata_persists_through_commit(&mut MemoryStoreFactory::new())
        .expect("definition and composition persist");
}

#[test]
fn history_points_map_to_positions() {
    suite::history_points_map_to_positions(&mut MemoryStoreFactory::new())
        .expect("history points map to positions");
}
