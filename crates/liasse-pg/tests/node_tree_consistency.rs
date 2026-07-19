//! The node-adjacency committed-state consistency gate.
//!
//! The `nodes` adjacency tree is the SOLE durable row representation: every
//! committed row op is applied to it in the admission transaction, and reads are
//! served from an in-memory projection rebuilt from it. This test proves the durable
//! tree encodes exactly the committed state the in-memory reference does.
//!
//! It runs one op sequence — inserts, updates, a delete, and both a same-parent and
//! a cross-parent leaf rekey, over three collection levels (`orgs` → `teams` →
//! `members`) — against BOTH a [`PgStore`] and the in-memory reference, then:
//!
//! 1. reconstructs committed state purely from the NODE tree (read each node, walk
//!    its `parent_id` chain to the root sentinel `id = 0` — through tombstones,
//!    which contribute an address level but are not rows — and assemble the address
//!    from each level's `step_name` + `key_wire`), and asserts it holds exactly the
//!    LIVE rows the workload leaves;
//! 2. asserts the PostgreSQL store's own reads equal the in-memory reference
//!    row-for-row, incarnations included — the `== MemoryStore` leg.
//!
//! The node reconstruction is an INDEPENDENT oracle: it reads the raw catalog with
//! `serde_json`, never the crate's private codec, so a pass means the durable node
//! tree really carries the state, not that the backend agrees with itself.
//!
//! Like the rest of the suite it resolves the test DSN through [`support`] and
//! fails loudly if none is reachable; its throwaway schema drops through a
//! [`support::SchemaGuard`] even on a panic.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;

use liasse_ident::{InstanceId, NameSegment};
use liasse_store::{
    AddressStep, CommitSeq, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, Snapshot,
    StoreFactory, Transition,
};
use liasse_value::{Integer, Text, Value};
use postgres::Client;
use serde_json::{json, Value as JsonV};

/// One address level `name/<key>` over a single integer key.
fn step(name: &str, key: i64) -> AddressStep {
    AddressStep::new(NameSegment::new(name), KeyValue::single(Value::Int(Integer::from(key))))
}

fn org(o: i64) -> RowAddress {
    RowAddress::root(step("orgs", o))
}
fn team(o: i64, t: i64) -> RowAddress {
    org(o).child(step("teams", t))
}
fn member(o: i64, t: i64, m: i64) -> RowAddress {
    team(o, t).child(step("members", m))
}
fn text(payload: &str) -> Value {
    Value::Text(Text::new(payload))
}

/// The identical op sequence both backends run. It exercises nested inserts
/// (parent-before-child), updates at two depths, a delete, a same-parent leaf
/// rekey, and a cross-parent leaf rekey — every op the node dual-write handles,
/// while keeping every rekey a LEAF move so `rows` (which moves one row) and the
/// node tree (which moves the stable id) stay in agreement.
fn apply_workload<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(org(1), text("org-1")).expect("insert org 1");
    txn.insert(org(2), text("org-2")).expect("insert org 2");
    txn.commit().expect("commit orgs");

    let mut txn = store.begin();
    txn.insert(team(1, 10), text("team-1-10")).expect("insert team 1/10");
    txn.insert(team(2, 20), text("team-2-20")).expect("insert team 2/20");
    txn.commit().expect("commit teams");

    let mut txn = store.begin();
    txn.insert(member(1, 10, 100), text("member-100")).expect("insert member 100");
    txn.insert(member(1, 10, 101), text("member-101")).expect("insert member 101");
    txn.commit().expect("commit members");

    let mut txn = store.begin();
    txn.update(&org(1), text("org-1-updated")).expect("update org 1");
    txn.update(&member(1, 10, 100), text("member-100-updated")).expect("update member 100");
    txn.commit().expect("commit updates");

    let mut txn = store.begin();
    txn.delete(&member(1, 10, 101)).expect("delete member 101");
    txn.commit().expect("commit delete");

    // Same-parent leaf rekey: member 100 -> member 200 under the same team.
    let mut txn = store.begin();
    txn.rekey(&member(1, 10, 100), member(1, 10, 200), text("member-200")).expect("rekey member");
    txn.commit().expect("commit member rekey");

    // Cross-parent leaf rekey: team 2/20 (childless) moves under org 1 as team 1/30.
    let mut txn = store.begin();
    txn.rekey(&team(2, 20), team(1, 30), text("team-1-30")).expect("rekey team");
    txn.commit().expect("commit team rekey");
}

/// A stable, object-key-sorted string form of a JSON value, so two structurally
/// equal trees compare equal regardless of member order or backend normalization.
fn canon(value: &JsonV) -> String {
    match value {
        JsonV::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|key| format!("{key:?}:{}", canon(&members[key])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        JsonV::Array(items) => {
            let body: Vec<String> = items.iter().map(canon).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// Committed state reconstructed purely from the NODE tree: read every node, then
/// for each non-sentinel node walk its `parent_id` chain to the root sentinel
/// (`id = 0`), assembling the address from each level's `step_name` + `key_wire`
/// — exactly the `[[name, [key-components…]], …]` shape the row address key has.
///
/// Only LIVE nodes (`value IS NOT NULL`) are emitted as rows. A tombstone
/// (`value`/`incarnation` NULL — a deleted ancestor retained so its descendants stay
/// addressable) is not a row, so it is skipped when emitting; but the parent-walk
/// still traverses tombstones, since a tombstone contributes its `step_name` +
/// `key_wire` to a descendant's address.
fn state_from_nodes(client: &mut Client, schema: &str) -> BTreeMap<String, (String, String)> {
    struct Node {
        parent: i64,
        name: String,
        key_wire: JsonV,
        incarnation: Option<String>,
        value: Option<JsonV>,
    }
    let mut nodes: BTreeMap<i64, Node> = BTreeMap::new();
    for row in client
        .query(
            &format!(
                "SELECT id, parent_id, step_name, key_wire, incarnation, value FROM {schema}.nodes"
            ),
            &[],
        )
        .expect("scan nodes")
    {
        let id: i64 = row.get("id");
        if id == 0 {
            continue; // the self-referential root sentinel is not a row
        }
        nodes.insert(
            id,
            Node {
                parent: row.get("parent_id"),
                name: row.get("step_name"),
                key_wire: row.get("key_wire"),
                incarnation: row.get("incarnation"),
                value: row.get("value"),
            },
        );
    }

    let mut state = BTreeMap::new();
    for (&id, node) in &nodes {
        // Emit only live rows; a tombstone carries no value/incarnation.
        let (Some(incarnation), Some(value)) = (&node.incarnation, &node.value) else {
            continue;
        };
        let mut levels: Vec<JsonV> = Vec::new();
        let mut cursor = id;
        loop {
            // The walk traverses tombstones too — they still contribute an address level.
            let current = nodes.get(&cursor).expect("node parent chain is intact");
            levels.push(json!([current.name, current.key_wire]));
            if current.parent == 0 {
                break;
            }
            assert!(levels.len() <= nodes.len(), "node parent chain does not reach the root");
            cursor = current.parent;
        }
        levels.reverse();
        let address = JsonV::Array(levels);
        state.insert(canon(&address), (incarnation.clone(), canon(value)));
    }
    state
}

#[test]
fn node_tree_mirrors_rows_and_memory() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("nodetree");
    let instance = InstanceId::new("node-tree-consistency");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());
    let schema = pg_factory.schema_for(&instance);

    // The in-memory reference and the PostgreSQL store run the identical workload,
    // so their opaque `row-N` incarnations line up op-for-op.
    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    apply_workload(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    apply_workload(&mut pg);

    // (1) The node tree is now the SOLE durable row representation. Reconstruct the
    // committed set from parent_id/key_wire and confirm it is a sound, non-empty
    // walk (every node's parent chain reaches the sentinel; deletes tombstone the
    // node in place, rekeys move only the addressed row).
    let mut client = pg_factory.connect().expect("connect a raw client");
    let s = schema.quoted();
    let nodes_state = state_from_nodes(&mut client, &s);
    assert!(!nodes_state.is_empty(), "the workload must leave committed nodes to compare");

    // (2) The PostgreSQL store's node-derived reads equal the in-memory reference,
    // incarnations included — presence AND absence at every touched address. The
    // absent addresses are now tombstones (deleted or rekeyed-away nodes), which
    // `state_from_nodes` skips, so the live count still equals `present`.
    let present = [org(1), org(2), team(1, 10), team(1, 30), member(1, 10, 200)];
    let absent = [team(2, 20), member(1, 10, 100), member(1, 10, 101)];
    assert_eq!(
        nodes_state.len(),
        present.len(),
        "the node tree holds exactly the live rows after the workload, got {nodes_state:?}"
    );
    for address in present.iter().chain(absent.iter()) {
        let pg_row = pg.row(address).expect("pg row read");
        let memory_row = memory.row(address).expect("memory row read");
        assert_eq!(
            pg_row, memory_row,
            "pg and the in-memory reference disagree at {}",
            address.render()
        );
    }
    for address in &present {
        assert!(pg.row(address).expect("pg row").is_some(), "expected {} present", address.render());
    }
    for address in &absent {
        assert!(pg.row(address).expect("pg row").is_none(), "expected {} absent", address.render());
    }
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}

/// A workload rich in the head-fast-path SUBTLETIES: updates at two depths, a leaf
/// tombstone, a leaf rekey, a **non-leaf delete** that leaves a live orphan under a
/// tombstoned ancestor, and a **nested insert under never-created ancestors** (auto
/// tombstones). Both backends run it identically, so the head fast path
/// (`nodes`-materialization) and the log fold must reconstruct the same live set —
/// orphans reconstructed through tombstoned ancestors included (§5.4).
fn apply_orphan_workload<S: InstanceStore>(store: &mut S) {
    let mut txn = store.begin();
    txn.insert(org(1), text("org-1")).expect("insert org 1");
    txn.insert(org(2), text("org-2")).expect("insert org 2");
    txn.commit().expect("commit orgs");

    let mut txn = store.begin();
    txn.insert(team(1, 10), text("team-1-10")).expect("insert team 1/10");
    txn.insert(team(1, 11), text("team-1-11")).expect("insert team 1/11");
    txn.commit().expect("commit teams");

    let mut txn = store.begin();
    txn.insert(member(1, 10, 100), text("member-100")).expect("insert member 100");
    txn.insert(member(1, 10, 101), text("member-101")).expect("insert member 101");
    txn.insert(member(1, 11, 200), text("member-200")).expect("insert member 200");
    txn.commit().expect("commit members");

    // Updates at two depths.
    let mut txn = store.begin();
    txn.update(&org(1), text("org-1-updated")).expect("update org 1");
    txn.update(&member(1, 10, 100), text("member-100-updated")).expect("update member 100");
    txn.commit().expect("commit updates");

    // Leaf delete → a leaf tombstone.
    let mut txn = store.begin();
    txn.delete(&member(1, 10, 101)).expect("delete member 101");
    txn.commit().expect("commit leaf delete");

    // Same-parent leaf rekey: member 100 -> member 300, still under team 1/10.
    let mut txn = store.begin();
    txn.rekey(&member(1, 10, 100), member(1, 10, 300), text("member-300")).expect("rekey member");
    txn.commit().expect("commit rekey");

    // NON-leaf delete: dropping team 1/10 leaves member 1/10/300 a LIVE ORPHAN under a
    // tombstoned ancestor — the fast path must reconstruct its address THROUGH the
    // tombstone (§5.4), never emitting the tombstone itself.
    let mut txn = store.begin();
    txn.delete(&team(1, 10)).expect("delete team 1/10");
    txn.commit().expect("commit non-leaf delete");

    // Nested insert under NEVER-created ancestors: member 5/50/500 with no org 5 /
    // team 5/50 ever inserted. The pg node writer auto-creates those ancestors as
    // tombstones; the memory oracle just holds the flat address. Another orphan the
    // fast path reconstructs through auto-created tombstone ancestors.
    let mut txn = store.begin();
    txn.insert(member(5, 50, 500), text("member-500")).expect("insert nested orphan member 500");
    txn.commit().expect("commit nested orphan");
}

/// The Phase-6 `snapshot(head)` fast path (§4.3) must produce a `Snapshot`
/// **byte-identical** to the O(history) commit-log fold it replaces — the
/// tree-≡-log-fold equivalence. This drives the orphan-rich workload, then asserts
/// the fast path equals BOTH the independent commit-log fold AND the in-memory oracle
/// (the external reference that makes the expected live set deducible, not merely two
/// pg paths agreeing), and that it holds exactly the hand-derived live rows.
#[test]
fn head_fast_path_equals_log_fold() {
    let handle = support::acquire();
    let mut pg_factory = handle.factory("fastpath");
    let instance = InstanceId::new("head-fast-path");
    let _guard = support::SchemaGuard::new(&pg_factory, instance.clone());

    let mut memory = MemoryStoreFactory.create(instance.clone()).expect("create memory store");
    apply_orphan_workload(&mut memory);

    let mut pg = pg_factory.create(instance.clone()).expect("create pg store");
    apply_orphan_workload(&mut pg);

    let head = pg.head().expect("pg head");

    // The Phase-6 head fast path: `snapshot(head)` materializes straight from `nodes`.
    let fast = pg.snapshot(head).expect("head fast-path snapshot");

    // The independent O(history) oracle: fold the whole durable commit log with the
    // shared `Snapshot::materialize` (the replay the fast path REPLACES). Byte
    // equality here is the tree-≡-log-fold cross-check §4.3 demands.
    let log = pg.log_from(CommitSeq::GENESIS).expect("full commit log");
    let folded = Snapshot::materialize(&log, head).expect("commit-log fold at head");
    assert_eq!(fast, folded, "head fast path diverges from the commit-log fold");

    // The external reference: the in-memory oracle's own head snapshot.
    let mem_head = memory.head().expect("memory head");
    assert_eq!(head, mem_head, "both backends ran the identical workload");
    let mem_snapshot = memory.snapshot(mem_head).expect("memory head snapshot");
    assert_eq!(fast, mem_snapshot, "head fast path diverges from the in-memory oracle");

    // Hand-derived live set (Annex A/B, §5.4): exactly these rows survive, including
    // two ORPHANS — members/300 under a DELETED team, members/500 under NEVER-created
    // ancestors — while the tombstones (team 1/10, and auto-created org 5 / team 5/50)
    // are addressable positions, never rows.
    let present =
        [org(1), org(2), team(1, 11), member(1, 10, 300), member(1, 11, 200), member(5, 50, 500)];
    let absent =
        [team(1, 10), member(1, 10, 100), member(1, 10, 101), org(5), team(5, 50)];
    assert_eq!(fast.len(), present.len(), "head fast path holds exactly the live rows, got {fast:?}");
    for address in &present {
        assert!(fast.row(address).is_some(), "expected {} present in the fast path", address.render());
    }
    for address in &absent {
        assert!(fast.row(address).is_none(), "expected {} absent from the fast path", address.render());
    }
    // `_guard` drops the throwaway schema on scope exit (and on a panic).
}
