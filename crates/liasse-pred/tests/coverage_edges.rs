//! RED TEAM (Phase 7a): §10.5 coverage `scan_view` descent EDGES the shipped
//! `coverage.rs` gate omits — a tombstoned intermediate blocking its live branch,
//! an absent root, a childless single node, and the "root projected UNCONDITIONALLY
//! even when it would fail the predicate" invariant (the shipped test's root passes
//! the predicate, so that arm is never exercised).
//!
//! The oracle for each is hand-derived from `ViewSource::Coverage`'s contract in
//! `crates/liasse-store/src/view_program.rs` (lines 148-155, 226-285): the covered
//! root is admitted by scope membership and projected unconditionally, its
//! DESCENDANTS are admitted hereditarily, a tombstone blocks its branch (live rows
//! only), and an absent/tombstoned root yields nothing — externally deducible from
//! §10.5/§7.2 per AGENTS.md.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType, RowType, Scope, TypedExpr};
use liasse_ident::{InstanceId, NameSegment};
use liasse_pred::{CandidateDescriptor, Member, ProjectShape, RowPrograms, RowProgramsParts};
use liasse_store::{AddressStep, InstanceStore, KeyValue, MemoryStore, RowAddress, Transition, ViewSource};
use liasse_syntax::parse_expression;
use liasse_value::{Struct, Text, Type, Value};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}
fn company(id: &str, plan: &str) -> Value {
    Value::Struct(Struct::new([(Text::new("id"), text(id)), (Text::new("plan"), text(plan))]))
}
fn subkey(id: &str) -> KeyValue {
    KeyValue::single(text(id))
}
fn sub(parent: &RowAddress, id: &str) -> RowAddress {
    parent.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey(id)))
}

struct CompanyScope;
fn company_type() -> ExprType {
    ExprType::Row(RowType::new(
        [
            ("id".to_owned(), ExprType::scalar(Type::Text)),
            ("plan".to_owned(), ExprType::scalar(Type::Text)),
        ],
        Some(ExprType::scalar(Type::Text)),
    ))
}
impl Scope for CompanyScope {
    fn current(&self) -> Option<ExprType> {
        Some(company_type())
    }
    fn parent(&self, _depth: u32) -> Option<ExprType> {
        None
    }
    fn root(&self) -> Option<ExprType> {
        None
    }
    fn param(&self, _name: &str) -> Option<ExprType> {
        None
    }
    fn structural(&self, _name: &str) -> Option<ExprType> {
        None
    }
    fn import(&self, _name: &str) -> Option<ExprType> {
        None
    }
    fn binding(&self, name: &str) -> Option<ExprType> {
        (name == "child").then(company_type)
    }
}

fn check(text: &str) -> TypedExpr {
    let mut sources = SourceMap::new();
    let src = sources.add_label("coverage-edges", text.to_owned());
    let parsed = parse_expression(src, text).unwrap();
    check_statement(&CompanyScope, src, &parsed).unwrap()
}

/// A coverage program: `$where plan != 'closed'`, projecting `{ id, plan }`.
fn program() -> RowPrograms {
    RowPrograms::new(RowProgramsParts {
        admit: Some(check("child.plan != 'closed'")),
        outputs: vec![("id".to_owned(), check("child.id")), ("plan".to_owned(), check("child.plan"))],
        sort: Vec::new(),
        env: Vec::new(),
        bind: Some("child".to_owned()),
        descriptor: CandidateDescriptor::new(true, vec![Member::scalar("id"), Member::scalar("plan")]),
        subtree_steps: Vec::new(),
        shape: ProjectShape::Coverage,
    })
    .unwrap()
}

/// The `(relative key-path ids, projected value)` sequence a coverage scan delivers.
fn run(store: &MemoryStore, root: &RowAddress) -> Vec<(Vec<String>, Value)> {
    store
        .scan_view(ViewSource::Coverage { root, field: "subcompanies" }, &program(), None, None)
        .unwrap()
        .iter()
        .map(|row| {
            let path = row
                .key_path
                .iter()
                .map(|key| match key.components().next() {
                    Some(Value::Text(t)) => t.as_str().to_owned(),
                    other => panic!("unexpected key {other:?}"),
                })
                .collect();
            (path, row.projected.clone())
        })
        .collect()
}

#[test]
fn tombstoned_intermediate_blocks_its_live_branch() {
    // acme → eng → team1, all live and admitted; then eng is DELETED. The descent
    // must stop at acme: a tombstoned intermediate is never scanned, so its still-
    // stored grandchild team1 is unreachable.
    let acme = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("acme")));
    let eng = sub(&acme, "eng");
    let team1 = sub(&eng, "team1");

    let mut store = MemoryStore::new(InstanceId::new("cov-tomb"));
    let mut txn = store.begin();
    txn.insert(acme.clone(), company("acme", "active")).unwrap();
    txn.insert(eng.clone(), company("eng", "active")).unwrap();
    txn.insert(team1.clone(), company("team1", "active")).unwrap();
    txn.commit().unwrap();

    // Sanity: with eng live, the full chain is delivered.
    assert_eq!(
        run(&store, &acme),
        vec![
            (Vec::new(), company("acme", "active")),
            (vec!["eng".to_owned()], company("eng", "active")),
            (vec!["eng".to_owned(), "team1".to_owned()], company("team1", "active")),
        ],
    );

    // Tombstone the intermediate `eng`.
    let mut txn = store.begin();
    txn.delete(&eng).unwrap();
    txn.commit().unwrap();

    // The branch is blocked: only the root survives.
    assert_eq!(run(&store, &acme), vec![(Vec::new(), company("acme", "active"))]);
}

#[test]
fn absent_root_yields_nothing() {
    // A coverage scan rooted at a never-inserted address yields no rows at all —
    // not even a phantom root projection.
    let store = MemoryStore::new(InstanceId::new("cov-absent"));
    let ghost = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("ghost")));
    assert_eq!(run(&store, &ghost), Vec::new());
}

#[test]
fn tombstoned_root_yields_nothing() {
    let acme = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("acme")));
    let mut store = MemoryStore::new(InstanceId::new("cov-tomb-root"));
    let mut txn = store.begin();
    txn.insert(acme.clone(), company("acme", "active")).unwrap();
    txn.commit().unwrap();
    let mut txn = store.begin();
    txn.delete(&acme).unwrap();
    txn.commit().unwrap();
    assert_eq!(run(&store, &acme), Vec::new());
}

#[test]
fn childless_root_projects_only_itself() {
    let acme = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("acme")));
    let mut store = MemoryStore::new(InstanceId::new("cov-single"));
    let mut txn = store.begin();
    txn.insert(acme.clone(), company("acme", "active")).unwrap();
    txn.commit().unwrap();
    assert_eq!(run(&store, &acme), vec![(Vec::new(), company("acme", "active"))]);
}

#[test]
fn root_projected_unconditionally_though_it_fails_the_predicate() {
    // The root itself is `closed` — it would be REJECTED by `$where plan != 'closed'`
    // if it were filtered. §10.5: the covered root is admitted by scope membership
    // (already resolved) and projected UNCONDITIONALLY; only its DESCENDANTS are
    // predicate-admitted. A live admitted child must still be descended into.
    let acme = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("acme")));
    let eng = sub(&acme, "eng");
    let closed_child = sub(&acme, "shut");

    let mut store = MemoryStore::new(InstanceId::new("cov-root-fails"));
    let mut txn = store.begin();
    txn.insert(acme.clone(), company("acme", "closed")).unwrap(); // root fails the predicate
    txn.insert(eng, company("eng", "active")).unwrap(); // admitted descendant
    txn.insert(closed_child, company("shut", "closed")).unwrap(); // pruned descendant
    txn.commit().unwrap();

    assert_eq!(
        run(&store, &acme),
        vec![
            // Root present despite being `closed` (projected unconditionally).
            (Vec::new(), company("acme", "closed")),
            // `eng` (active) admitted; `shut` (closed, sorts after eng) pruned.
            (vec!["eng".to_owned()], company("eng", "active")),
        ],
    );
}
