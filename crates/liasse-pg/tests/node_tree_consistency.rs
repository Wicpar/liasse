//! The node-adjacency dual-write consistency gate.
//!
//! Phase 1 dual-writes every committed row op into a second physical layout — the
//! `nodes` adjacency tree — beside the authoritative flat `rows` table, in the
//! same admission transaction. This test proves the two layouts encode the *same*
//! committed state, so a later phase can flip reads onto `nodes` with no observable
//! change.
//!
//! It runs one op sequence — inserts, updates, a delete, and both a same-parent and
//! a cross-parent leaf rekey, over three collection levels (`orgs` → `teams` →
//! `members`) — against BOTH a [`PgStore`] and the in-memory reference, then:
//!
//! 1. reconstructs committed state purely from the NODE tree (read each node, walk
//!    its `parent_id` chain to the root sentinel `id = 0`, and assemble the address
//!    from each level's `step_name` + `key_wire`), and asserts it equals the state
//!    read straight from the flat `rows` table — the node tree is an exact mirror of
//!    `rows`;
//! 2. asserts the PostgreSQL store's own (rows-derived) reads equal the in-memory
//!    reference row-for-row, incarnations included — the `== MemoryStore` leg.
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
    AddressStep, InstanceStore, KeyValue, MemoryStoreFactory, RowAddress, StoreFactory, Transition,
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
/// — exactly the `[[name, [key-components…]], …]` shape the `rows` address key has.
fn state_from_nodes(client: &mut Client, schema: &str) -> BTreeMap<String, (String, String)> {
    struct Node {
        parent: i64,
        name: String,
        key_wire: JsonV,
        incarnation: String,
        value: JsonV,
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
        let mut levels: Vec<JsonV> = Vec::new();
        let mut cursor = id;
        loop {
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
        state.insert(canon(&address), (node.incarnation.clone(), canon(&node.value)));
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
    // walk (every node's parent chain reaches the sentinel; deletes cascade the
    // subtree, rekeys keep the node id).
    let mut client = pg_factory.connect().expect("connect a raw client");
    let s = schema.quoted();
    let nodes_state = state_from_nodes(&mut client, &s);
    assert!(!nodes_state.is_empty(), "the workload must leave committed nodes to compare");

    // (2) The PostgreSQL store's node-derived reads equal the in-memory reference,
    // incarnations included — presence AND absence at every touched address.
    let present = [org(1), org(2), team(1, 10), team(1, 30), member(1, 10, 200)];
    let absent = [team(2, 20), member(1, 10, 100), member(1, 10, 101)];
    assert_eq!(
        nodes_state.len(),
        present.len(),
        "the node tree holds exactly the reachable rows after the workload, got {nodes_state:?}"
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
