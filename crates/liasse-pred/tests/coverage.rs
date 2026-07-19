//! The §10.5 coverage `scan_view` path: the covered root projected unconditionally,
//! then a hereditary depth-first descent through the nested keyed collection,
//! admitting each descendant by the composed `$where && !$except` and recursing only
//! into admitted candidates (a failing candidate prunes its whole subtree).
//!
//! A concrete company tree is walked with `$where: plan != 'closed'` and
//! `$except: id == 'hr'`. The expected admitted set — the root `acme`, then `eng`
//! and its child `team1`, with `hr` (excepted, subtree pruned) and `labs` (closed,
//! subtree pruned including `team2`) dropped — is hand-derived from §10.5 and §7.2,
//! externally deducible per AGENTS.md. The result rows carry their relative key path
//! and the scalar `row_object` projection.

#![allow(clippy::unwrap_used, clippy::panic)]

use liasse_diag::SourceMap;
use liasse_expr::{check_statement, ExprType, RowType, Scope, TypedExpr};
use liasse_ident::{InstanceId, NameSegment};
use liasse_pred::{CandidateDescriptor, Member, ProjectShape, RowPrograms, RowProgramsParts};
use liasse_store::{
    AddressStep, InstanceStore, KeyValue, RowAddress, Transition, ViewSource,
};
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

/// A scope where `child` (and `.`) is a company row `{ id: text, plan: text }`.
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
    let src = sources.add_label("coverage", text.to_owned());
    let parsed = parse_expression(src, text).unwrap();
    check_statement(&CompanyScope, src, &parsed).unwrap()
}

#[test]
fn hereditary_pruning_over_coverage() {
    // The company tree under `acme`, nested through `subcompanies`.
    let acme = RowAddress::root(AddressStep::new(NameSegment::new("companies"), subkey("acme")));
    let eng = acme.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey("eng")));
    let hr = acme.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey("hr")));
    let labs = acme.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey("labs")));
    let team1 = eng.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey("team1")));
    let team2 = labs.clone().child(AddressStep::new(NameSegment::new("subcompanies"), subkey("team2")));

    let mut store = liasse_store::MemoryStore::new(InstanceId::new("coverage"));
    let mut txn = store.begin();
    txn.insert(acme.clone(), company("acme", "active")).unwrap();
    txn.insert(eng, company("eng", "active")).unwrap();
    txn.insert(hr, company("hr", "active")).unwrap();
    txn.insert(labs, company("labs", "closed")).unwrap();
    txn.insert(team1, company("team1", "active")).unwrap();
    txn.insert(team2, company("team2", "active")).unwrap();
    txn.commit().unwrap();

    // The composed admit `$where && !$except` and the scalar `row_object` projection.
    let admit = check("child.plan != 'closed' && !(child.id == 'hr')");
    let id_out = check("child.id");
    let plan_out = check("child.plan");
    let descriptor =
        CandidateDescriptor::new(true, vec![Member::scalar("id"), Member::scalar("plan")]);
    let program = RowPrograms::new(RowProgramsParts {
        admit: Some(admit),
        outputs: vec![("id".to_owned(), id_out), ("plan".to_owned(), plan_out)],
        sort: Vec::new(),
        env: Vec::new(),
        bind: Some("child".to_owned()),
        descriptor,
        subtree_steps: Vec::new(),
        shape: ProjectShape::Coverage,
    })
    .unwrap();

    let rows = store
        .scan_view(ViewSource::Coverage { root: &acme, field: "subcompanies" }, &program, None, None)
        .unwrap();

    // Expected: the root `acme` (unconditional, empty rel path), then `eng` and its
    // child `team1` in depth-first key order; `hr` (excepted) and `labs` (closed)
    // are dropped with their whole subtrees (`team2` never reached).
    let actual: Vec<(Vec<String>, Value)> = rows
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
        .collect();

    let expected = vec![
        (Vec::new(), company("acme", "active")),
        (vec!["eng".to_owned()], company("eng", "active")),
        (vec!["eng".to_owned(), "team1".to_owned()], company("team1", "active")),
    ];
    assert_eq!(actual, expected);
}
