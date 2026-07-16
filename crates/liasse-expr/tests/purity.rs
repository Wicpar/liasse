#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! Purity and the scope/generativity seams (§8.12, §6.2, §6.4).

mod common;

use common::{
    as_scalar, collection, eval, ids, keyless_row, row, row_type, scalar, scell, view, vtext,
    FixedEnv, FixedScope,
};
use liasse_expr::{Cell, ExprType, Row};
use liasse_value::{Precision, Timestamp, Type, Uuid, Value};

#[test]
fn evaluation_is_deterministic_for_the_same_environment() {
    // §8.12: `now()`/`uuid()` are environment-owned samples, so two evaluations
    // against the same environment yield identical results.
    let scope = FixedScope::new(ExprType::scalar(Type::Int));
    let mut env = FixedEnv::new(keyless_row(0, vec![]));
    env.now = Timestamp::new(42, Precision::Micros);
    env.uuid = Uuid::from_bytes([9; 16]);
    let dot = Cell::Scalar(common::vint(0));

    let first = eval(&scope, &env, &dot, "now()");
    let second = eval(&scope, &env, &dot, "now()");
    assert_eq!(first, second);
    assert_eq!(as_scalar(&first), Value::Timestamp(Timestamp::new(42, Precision::Micros)));

    let uuid_a = eval(&scope, &env, &dot, "uuid()");
    let uuid_b = eval(&scope, &env, &dot, "uuid()");
    assert_eq!(uuid_a, uuid_b);
    assert_eq!(as_scalar(&uuid_a), Value::Uuid(Uuid::from_bytes([9; 16])));
}

#[test]
fn caret_reads_the_lexical_parent_scope() {
    // §6.2: `^` reads the enclosing struct. `.` = badge, `^` = profile.
    let profile_ty = row_type(vec![("name", scalar(Type::Text))], None);
    let badge_ty = row_type(vec![], None);
    let scope = FixedScope::with_contexts(
        vec![ExprType::Row(profile_ty), ExprType::Row(badge_ty)],
        ExprType::Row(row_type(vec![], None)),
    );

    let profile = keyless_row(1, vec![("name", scell(vtext("Ann")))]);
    let badge = keyless_row(2, vec![]);
    let env = FixedEnv::new(keyless_row(0, vec![]));

    let typed = common::check(&scope, "^.name");
    let result = typed
        .evaluate_scoped(&env, &[Cell::Row(Box::new(profile)), Cell::Row(Box::new(badge))])
        .expect("eval");
    assert_eq!(as_scalar(&result), vtext("Ann"));
}

#[test]
fn double_colon_flattens_and_binds_traversed_collections() {
    // §6.4: `.projects::tasks` flattens tasks across projects and binds each
    // level to its field name, so the projection reads `projects.id`.
    let task_ty = row_type(vec![("id", scalar(Type::Text))], Some(scalar(Type::Text)));
    let project_ty = row_type(
        vec![("id", scalar(Type::Text)), ("tasks", view(task_ty.clone()))],
        Some(scalar(Type::Text)),
    );
    let root_ty = row_type(vec![("projects", view(project_ty))], None);
    let scope = FixedScope::new(ExprType::Row(root_ty));

    let task = |seed: u64, id: &str| -> Row {
        row(seed, vtext(id), vec![("id", scell(vtext(id)))])
    };
    let project = |seed: u64, id: &str, tasks: Vec<Row>| -> Row {
        row(
            seed,
            vtext(id),
            vec![("id", scell(vtext(id))), ("tasks", collection(tasks))],
        )
    };
    let root = keyless_row(
        0,
        vec![(
            "projects",
            collection(vec![
                project(1, "p1", vec![task(11, "t1"), task(12, "t2")]),
                project(2, "p2", vec![task(21, "t3")]),
            ]),
        )],
    );
    let dot = Cell::Row(Box::new(root.clone()));
    let env = FixedEnv::new(root);

    let result = eval(&scope, &env, &dot, ".projects::tasks { id, project: projects.id }");
    assert_eq!(ids(&result, "id"), vec![vtext("t1"), vtext("t2"), vtext("t3")]);
    assert_eq!(
        ids(&result, "project"),
        vec![vtext("p1"), vtext("p1"), vtext("p2")]
    );
}
