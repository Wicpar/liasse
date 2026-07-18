//! Deep nested rows whose *flattened* address key would blow the btree limit still
//! commit — the node-adjacency layout's headline win.
//!
//! The earlier backend keyed every row by one flat `addr_key`: the whole address,
//! all its levels concatenated, as a single `TEXT` primary key. A btree index tuple
//! caps at ~2704 bytes, so a deeply nested row whose levels *summed* past that limit
//! failed to insert (SQLSTATE `54000`) even though no single level was large.
//!
//! The node tree stores each address level as its own row keyed by that level's key
//! alone, so a ten-level chain of ~300-byte keys — whose flattened key would be
//! ~3 KiB, past the limit — commits: every per-level `key_enc` stays small, well
//! under the btree cap. This test builds exactly such a chain (each ancestor
//! inserted parent-first in one transaction), asserts it COMMITS on `nodes`, that it
//! matches the in-memory reference row-for-row, and that it survives a reopen (the
//! projection reconstructing the deep chain from the node tree).
//!
//! Out of scope, and documented as a Postgres-fundamental limit: a *single* key
//! COMPONENT larger than ~2704 bytes. That would overflow the `node_key_lookup`
//! index on one level's `key_enc` regardless of nesting; the escape hatch (hashing
//! oversized `key_enc` values) is a separate, deferred concern.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreFactory, Transition,
};
use liasse_value::{Text, Value};

/// Nesting depth of the chain. Ten ~300-byte levels flatten to ~3 KiB.
const DEPTH: usize = 10;

/// Bytes in each level's text key — well under the ~2704-byte btree limit on its own,
/// but ten of them flattened exceed it.
const KEY_BYTES: usize = 300;

/// A ~`KEY_BYTES`-byte text key for `level`, distinct per level (a level tag prefix)
/// so the chain is a genuine nested path rather than a repeated key.
fn level_key(level: usize) -> KeyValue {
    let payload = format!("L{level:02}-{}", "x".repeat(KEY_BYTES));
    KeyValue::single(Value::Text(Text::new(payload)))
}

/// The nested chain of addresses, shallowest first: a top-level row, then a row
/// nested one level deeper under each preceding one, for `DEPTH` levels. Each level
/// is its own collection (`c00`, `c01`, …).
fn chain() -> Vec<RowAddress> {
    let mut addresses = Vec::with_capacity(DEPTH);
    let mut address = RowAddress::root(AddressStep::new(NameSegment::new("c00"), level_key(0)));
    addresses.push(address.clone());
    for level in 1..DEPTH {
        let step = AddressStep::new(NameSegment::new(format!("c{level:02}")), level_key(level));
        address = address.child(step);
        addresses.push(address.clone());
    }
    addresses
}

/// Insert the whole chain in one transaction, parent-first, so each nested insert
/// resolves its just-staged parent node. The flattened key of the deepest rows would
/// exceed the btree limit — on the flat `rows` layout this `commit` failed.
fn apply<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    for (level, address) in chain().into_iter().enumerate() {
        txn.insert(address, Value::Text(Text::new(format!("value-{level}"))))
            .expect("insert a chain level");
    }
    txn.commit().expect("commit the deep chain");
}

#[test]
fn deep_flattened_key_commits_on_nodes() {
    let handle = support::acquire();
    let mut factory = handle.factory("hugekey");
    let instance = InstanceId::new("huge-key-flattened");
    let _guard = support::SchemaGuard::new(&factory, instance.clone());

    // The in-memory reference and the PostgreSQL store run the identical workload,
    // so their opaque `row-N` incarnations line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    apply(&mut memory);

    // The headline assertion: this `apply` COMMITS. On the flat `rows` layout the
    // deepest inserts overflowed the ~2704-byte btree key limit and errored.
    let mut pg = factory.create(instance.clone()).expect("create pg store");
    apply(&mut pg);

    // Every level matches the reference, incarnations included — presence and value.
    for address in chain() {
        assert_eq!(
            pg.row(&address).expect("pg row read"),
            memory.row(&address).expect("memory row read"),
            "pg and the in-memory reference disagree at depth {}",
            address.depth()
        );
    }
    drop(pg);

    // Durability: a projection reopened from the node tree reconstructs the whole
    // deep chain, still matching the reference at every level.
    let reopened = factory.reopen(instance).expect("reopen");
    for address in chain() {
        assert_eq!(
            reopened.row(&address).expect("reopened row read"),
            memory.row(&address).expect("memory row read"),
            "reopened pg and the reference disagree at depth {}",
            address.depth()
        );
    }
    let deepest = chain().into_iter().next_back().expect("chain is non-empty");
    assert_eq!(deepest.depth(), DEPTH, "the deepest address spans every level");
    assert!(
        reopened.row(&deepest).expect("deepest row read").is_some(),
        "the deepest row — whose flattened key would exceed the btree limit — survived reopen"
    );
}
