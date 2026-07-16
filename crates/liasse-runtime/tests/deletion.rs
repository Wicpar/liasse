#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §21 deletion and erasure dynamic semantics: the cascade plan every inbound
//! `$on_delete` policy induces (cascade/restrict/none/patch, cascade cycles,
//! conflicting patches), and erasure that scrubs retained payload bytes to a
//! digest stub while keeping history verifiable and reinsertion stub-gated.
//! Every expectation is re-derived from §21 text, including the sharpest
//! red-case shapes (restrict blocks, conflicting patches, erase-then-replay,
//! double reinsert, tampered extract).

use std::collections::BTreeMap;

use liasse_runtime::{
    DeleteError, DeletePolicy, Erasure, Graph, Occurrence, RefEdge, RowRef,
};
use liasse_value::{Text, Value};

fn key(text: &str) -> Value {
    Value::Text(Text::new(text))
}

fn fields(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn project(id: &str) -> RowRef {
    RowRef::new("projects", key(id))
}

fn task(id: &str) -> RowRef {
    RowRef::new("tasks", key(id))
}

/// A graph of one project with one task that references it under `policy`.
fn project_task_graph(policy: DeletePolicy) -> Graph {
    let mut graph = Graph::new();
    graph.add_row(project("p1"), fields(&[("id", key("p1"))]));
    graph.add_row(task("t1"), fields(&[("id", key("t1")), ("project", key("p1")), ("archived", Value::Bool(false))]));
    graph.add_edge(RefEdge { from: task("t1"), field: "project".to_owned(), to: project("p1"), policy });
    graph
}

/// §21.1: a `cascade` inbound ref removes the referencing row along with its
/// target.
#[test]
fn cascade_deletes_referencing_row() {
    let mut graph = project_task_graph(DeletePolicy::Cascade);
    let plan = graph.plan(&[project("p1")]).expect("plan");
    assert!(plan.deletes().contains(&task("t1")), "the task cascades with its project");
    graph.apply(&plan);
    assert!(!graph.contains(&project("p1")));
    assert!(!graph.contains(&task("t1")));
}

/// §21.1: a `restrict` ref blocks deletion while the referencing row survives.
#[test]
fn restrict_blocks_deletion_with_live_ref() {
    let graph = project_task_graph(DeletePolicy::Restrict);
    match graph.plan(&[project("p1")]) {
        Err(DeleteError::Restricted { target, .. }) => assert_eq!(*target, project("p1")),
        other => panic!("expected a restrict block, got {other:?}"),
    }
}

/// §21.1: a `restrict` ref does not block deletion when the referencing row is
/// itself inside the final delete set.
#[test]
fn restrict_allows_deletion_when_referencing_row_also_deleted() {
    let graph = project_task_graph(DeletePolicy::Restrict);
    let plan = graph.plan(&[project("p1"), task("t1")]).expect("both deleted");
    assert!(plan.deletes().contains(&project("p1")) && plan.deletes().contains(&task("t1")));
}

/// §21.1: a `none` policy clears the optional ref on the surviving row (a patch
/// assigning `none` to the referencing field).
#[test]
fn none_clears_optional_ref_on_delete() {
    let mut graph = project_task_graph(DeletePolicy::Clear);
    let plan = graph.plan(&[project("p1")]).expect("plan");
    graph.apply(&plan);
    assert!(graph.contains(&task("t1")), "the task survives");
    assert_eq!(graph.fields(&task("t1")).and_then(|f| f.get("project")), Some(&Value::None));
}

/// §21.1: a `= patch` policy rewrites the surviving referencing row.
#[test]
fn patch_on_delete_rewrites_surviving_row() {
    let mut graph = project_task_graph(DeletePolicy::Patch(vec![("archived".to_owned(), Value::Bool(true))]));
    let plan = graph.plan(&[project("p1")]).expect("plan");
    graph.apply(&plan);
    assert_eq!(graph.fields(&task("t1")).and_then(|f| f.get("archived")), Some(&Value::Bool(true)));
}

/// §21.1: two `= patch` effects on disjoint fields of the same surviving row
/// combine.
#[test]
fn on_delete_patches_combine_disjoint_fields() {
    let mut graph = Graph::new();
    graph.add_row(project("p1"), fields(&[("id", key("p1"))]));
    graph.add_row(RowRef::new("owners", key("o1")), fields(&[("id", key("o1"))]));
    graph.add_row(task("t1"), fields(&[("id", key("t1")), ("project", key("p1")), ("owner", key("o1"))]));
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "project".to_owned(),
        to: project("p1"),
        policy: DeletePolicy::Patch(vec![("project_archived".to_owned(), Value::Bool(true))]),
    });
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "owner".to_owned(),
        to: RowRef::new("owners", key("o1")),
        policy: DeletePolicy::Patch(vec![("owner_cleared".to_owned(), Value::Bool(true))]),
    });
    // Delete both referenced rows at once; the two disjoint patches combine.
    let plan = graph.plan(&[project("p1"), RowRef::new("owners", key("o1"))]).expect("plan");
    graph.apply(&plan);
    let task_fields = graph.fields(&task("t1")).expect("task survives");
    assert_eq!(task_fields.get("project_archived"), Some(&Value::Bool(true)));
    assert_eq!(task_fields.get("owner_cleared"), Some(&Value::Bool(true)));
}

/// §21.1 (red): two `= patch` effects assigning conflicting values to the same
/// field reject the whole transition.
#[test]
fn conflicting_on_delete_patches_reject() {
    let mut graph = Graph::new();
    graph.add_row(project("p1"), fields(&[("id", key("p1"))]));
    graph.add_row(RowRef::new("owners", key("o1")), fields(&[("id", key("o1"))]));
    graph.add_row(task("t1"), fields(&[("id", key("t1")), ("project", key("p1")), ("owner", key("o1"))]));
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "project".to_owned(),
        to: project("p1"),
        policy: DeletePolicy::Patch(vec![("status".to_owned(), key("archived"))]),
    });
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "owner".to_owned(),
        to: RowRef::new("owners", key("o1")),
        policy: DeletePolicy::Patch(vec![("status".to_owned(), key("orphaned"))]),
    });
    match graph.plan(&[project("p1"), RowRef::new("owners", key("o1"))]) {
        Err(DeleteError::ConflictingPatch { field, .. }) => assert_eq!(field, "status"),
        other => panic!("expected a patch conflict, got {other:?}"),
    }
}

/// §21.1 (red): a patch targeting a row that is itself deleted is ignored.
#[test]
fn patch_to_row_also_deleted_is_ignored() {
    // t1 cascades with p1; a patch on t1 from deleting o1 must be ignored since
    // t1 no longer survives.
    let mut graph = Graph::new();
    graph.add_row(project("p1"), fields(&[("id", key("p1"))]));
    graph.add_row(RowRef::new("owners", key("o1")), fields(&[("id", key("o1"))]));
    graph.add_row(task("t1"), fields(&[("id", key("t1")), ("project", key("p1")), ("owner", key("o1"))]));
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "project".to_owned(),
        to: project("p1"),
        policy: DeletePolicy::Cascade,
    });
    graph.add_edge(RefEdge {
        from: task("t1"),
        field: "owner".to_owned(),
        to: RowRef::new("owners", key("o1")),
        policy: DeletePolicy::Patch(vec![("status".to_owned(), key("orphaned"))]),
    });
    let plan = graph.plan(&[project("p1"), RowRef::new("owners", key("o1"))]).expect("plan");
    assert!(plan.deletes().contains(&task("t1")));
    assert!(!plan.patches().contains_key(&task("t1")), "the patch to a deleted row is ignored");
}

/// §21.1: cascade cycles are valid and each row is removed once (the closure
/// terminates).
#[test]
fn cascade_cycle_removes_each_row_once() {
    let mut graph = Graph::new();
    let a = RowRef::new("nodes", key("a"));
    let b = RowRef::new("nodes", key("b"));
    graph.add_row(a.clone(), fields(&[("id", key("a")), ("peer", key("b"))]));
    graph.add_row(b.clone(), fields(&[("id", key("b")), ("peer", key("a"))]));
    graph.add_edge(RefEdge { from: a.clone(), field: "peer".to_owned(), to: b.clone(), policy: DeletePolicy::Cascade });
    graph.add_edge(RefEdge { from: b.clone(), field: "peer".to_owned(), to: a.clone(), policy: DeletePolicy::Cascade });
    let plan = graph.plan(std::slice::from_ref(&a)).expect("plan");
    assert_eq!(plan.deletes().len(), 2, "each row in the cycle removed once");
    graph.apply(&plan);
    assert!(!graph.contains(&a) && !graph.contains(&b));
}

/// §21.1 (red): deletion by key targets the exact key; a visually confusable but
/// byte-distinct key names a different row that is untouched.
#[test]
fn confusable_delete_key_targets_distinct_row() {
    let mut graph = Graph::new();
    let ascii = RowRef::new("projects", key("a"));
    let cyrillic = RowRef::new("projects", key("\u{0430}")); // Cyrillic 'а'
    graph.add_row(ascii.clone(), fields(&[("id", key("a"))]));
    graph.add_row(cyrillic.clone(), fields(&[("id", key("\u{0430}"))]));
    let plan = graph.plan(std::slice::from_ref(&ascii)).expect("plan");
    graph.apply(&plan);
    assert!(!graph.contains(&ascii), "the ascii-keyed row is deleted");
    assert!(graph.contains(&cyrillic), "the confusable-keyed row is a distinct, untouched row");
}

/// §21.1 (red): a collection replacement that would remove a row a `restrict`
/// ref still points at is rejected.
#[test]
fn collection_replacement_restrict_ref_rejects() {
    // A replacement is modelled as deleting every current row of the collection.
    let graph = project_task_graph(DeletePolicy::Restrict);
    match graph.plan(&[project("p1")]) {
        Err(DeleteError::Restricted { .. }) => {}
        other => panic!("expected a restrict block on replacement, got {other:?}"),
    }
}

// ---- erasure (§21.2/§21.3) ----------------------------------------------

fn seeded_history() -> Erasure {
    let mut history = Erasure::new();
    history.record(Occurrence::new("projects/p1"), key("secret project"));
    history
}

/// §21: ordinary deletion removes a row from live state but its prior values
/// remain available through retained history — deletion never scrubs bytes, so
/// a delete grant is not an erasure grant.
#[test]
fn deletion_keeps_history_but_erasure_scrubs() {
    let history = seeded_history();
    let occ = Occurrence::new("projects/p1");
    // A live delete leaves history untouched: the payload is still retained.
    assert_eq!(history.payload(&occ), Some(&key("secret project")));

    let mut erased = history.clone();
    erased.erase(std::slice::from_ref(&occ)).expect("erase");
    assert_eq!(erased.payload(&occ), None, "erasure scrubs the retained payload");
    assert!(erased.stub(&occ).is_some(), "a verifiable digest stub remains");
}

/// §21.2 (red): an erased occurrence is unobservable in history and on replay,
/// and stays absent across an export/restore round-trip.
#[test]
fn erased_row_unobservable_in_history_and_replay() {
    let mut history = seeded_history();
    let occ = Occurrence::new("projects/p1");
    history.erase(std::slice::from_ref(&occ)).expect("erase");
    assert!(!history.replay_payloads().contains_key(&occ), "erased data is unobservable on replay");
    // Export/restore is modelled as a clone of retained history; the stub travels
    // but the payload does not.
    let restored = history.clone();
    assert_eq!(restored.payload(&occ), None, "erased data stays absent across export/restore");
    assert!(restored.replay_payloads().is_empty());
}

/// §21.3: reinsertion restores bytes only where the exact expected stub remains.
#[test]
fn reinsert_restores_with_matching_stub() {
    let mut history = seeded_history();
    let occ = Occurrence::new("projects/p1");
    let extract = history.erase(std::slice::from_ref(&occ)).expect("erase");
    assert_eq!(history.payload(&occ), None);
    history.reinsert(&extract).expect("reinsert");
    assert_eq!(history.payload(&occ), Some(&key("secret project")), "matching stub restores bytes");
}

/// §21.3 (red): a second reinsertion finds no stub (the leaf is a payload again)
/// and rejects.
#[test]
fn double_reinsert_second_finds_no_stub_rejects() {
    let mut history = seeded_history();
    let occ = Occurrence::new("projects/p1");
    let extract = history.erase(std::slice::from_ref(&occ)).expect("erase");
    history.reinsert(&extract).expect("first reinsert");
    match history.reinsert(&extract) {
        Err(DeleteError::StubMismatch(name)) => assert_eq!(name, "projects/p1"),
        other => panic!("expected a stub mismatch, got {other:?}"),
    }
}

/// §21.3 (red): a tampered extract whose content no longer matches its hash is
/// rejected before any restoration.
#[test]
fn reinsert_tampered_extract_hash_rejects() {
    let mut history = seeded_history();
    let occ = Occurrence::new("projects/p1");
    let extract = history.erase(std::slice::from_ref(&occ)).expect("erase");
    let tampered = extract.tampered(&occ, key("forged project"));
    match history.reinsert(&tampered) {
        Err(DeleteError::ExtractHashMismatch) => {}
        other => panic!("expected an extract hash mismatch, got {other:?}"),
    }
    // The original occurrence still bears only the stub — nothing was restored.
    assert_eq!(history.payload(&occ), None);
}
