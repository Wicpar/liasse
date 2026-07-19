//! The three faces evaluate a candidate through the shared interpreter (§7.2).
//!
//! A hand-built program — admit `c.active`, project `{ id: c.id }`, sort by `c.id`
//! descending — is evaluated over generated candidate rows, and each face's verdict
//! is checked against the value the candidate carries (externally deducible, per
//! AGENTS.md): `admits` iff the row's `active` field is `true`, `project` is the
//! row's `id` re-exposed, `sort_tuple` is that `id`. This gates the
//! descriptor-driven candidate build and the `evaluate_bound` face wiring end to
//! end, independently of the runtime lowering.

#![allow(clippy::unwrap_used, clippy::panic)]

use liasse_diag::{SourceId, SourceMap};
use liasse_expr::{check_statement, ExprType, RowType, Scope, TypedExpr};
use liasse_pred::{CandidateDescriptor, Member, ProjectShape, RowPrograms, RowProgramsParts};
use liasse_store::{CandidateSubtree, KeyValue, SortDirection, ViewProgram};
use liasse_syntax::parse_expression;
use liasse_value::{Integer, Struct, Text, Type, Value};
use proptest::prelude::*;

/// A scope where the bound name `c` is a row with a `bool active`, an `int id`, and
/// an `int` key — enough to check `c.active`, `c.id`.
struct CandidateScope;

impl Scope for CandidateScope {
    fn current(&self) -> Option<ExprType> {
        None
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
        (name == "c").then(|| {
            ExprType::Row(RowType::new(
                [
                    ("active".to_owned(), ExprType::scalar(Type::Bool)),
                    ("id".to_owned(), ExprType::scalar(Type::Int)),
                ],
                Some(ExprType::scalar(Type::Int)),
            ))
        })
    }
}

fn check(sources: &mut SourceMap, text: &str) -> (TypedExpr, SourceId) {
    let src = sources.add_label("faces-test", text.to_owned());
    let parsed = parse_expression(src, text).unwrap();
    let typed = check_statement(&CandidateScope, src, &parsed).unwrap();
    (typed, src)
}

fn program() -> RowPrograms {
    let mut sources = SourceMap::new();
    let (admit, _) = check(&mut sources, "c.active");
    let (id_out, _) = check(&mut sources, "c.id");
    let (sort_key, _) = check(&mut sources, "c.id");
    let descriptor =
        CandidateDescriptor::new(true, vec![Member::scalar("active"), Member::scalar("id")]);
    RowPrograms::new(RowProgramsParts {
        admit: Some(admit),
        outputs: vec![("id".to_owned(), id_out)],
        sort: vec![(sort_key, SortDirection::Descending)],
        env: Vec::new(),
        bind: Some("c".to_owned()),
        descriptor,
        subtree_steps: Vec::new(),
        shape: ProjectShape::Flat,
    })
    .unwrap()
}

fn candidate(active: bool, id: i64) -> (Value, KeyValue) {
    let value = Value::Struct(Struct::new([
        (Text::new("active"), Value::Bool(active)),
        (Text::new("id"), Value::Int(Integer::from(id))),
    ]));
    (value, KeyValue::single(Value::Int(Integer::from(id))))
}

proptest! {
    #[test]
    fn faces_expose_the_candidate(active: bool, id in -1_000_000i64..1_000_000) {
        let program = program();
        let (value, key) = candidate(active, id);
        let subtree = CandidateSubtree::default();

        prop_assert_eq!(program.admits(&value, &key, &subtree).unwrap(), active);

        let projected = program.project(&value, &key, &subtree).unwrap();
        let expected = Value::Struct(Struct::new([(Text::new("id"), Value::Int(Integer::from(id)))]));
        prop_assert_eq!(projected, expected);

        let sort = program.sort_tuple(&value, &key, &subtree).unwrap();
        prop_assert_eq!(sort, vec![Value::Int(Integer::from(id))]);
    }
}
