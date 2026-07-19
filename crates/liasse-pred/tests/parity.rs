//! Layer-1 lowering parity (§9 of `liasse-pg/DESIGN-pure-pg.md`): the interpreter's
//! direct evaluation of a flat `$view` EQUALS `scan_view` over the lowered
//! [`RowPrograms`] on `MemoryStore`.
//!
//! Both sides start from the SAME rows and the SAME checked view expression, but
//! reach the result by independent paths: the interpreter materializes the
//! collection into a root and runs `TypedExpr::evaluate_view` (filter → project →
//! sort → bound); the pushdown path lowers the expression (hoisting candidate-free
//! subexpressions into an env), rebuilds each candidate from the descriptor, and
//! runs the three faces through `scan_view`. Agreement — same rows, same exposed
//! values, same order, same `$sort` tuples — proves the reimplemented seam (the
//! descriptor candidate build, the faces, the hoist, the `scan_view` ordering)
//! against the interpreter oracle. The item rows are built INDEPENDENTLY on the
//! interpreter side (not via the descriptor), so a candidate-build divergence
//! surfaces. Adversarial shapes covered: a bound filter, a hoisted `/`-collection
//! read, a mixed-direction two-key sort, an absent-optional (`none`-placement) sort
//! key, and `$skip`/`$limit` over ties.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};

use liasse_expr::hoist::{audit, hoist, CandidateRefs};
use liasse_expr::lower::lower_flat_view;
use liasse_expr::{
    check_statement, Cell, Environment, ExprType, Row, RowId, RowType, Scope, TypedExpr,
};
use liasse_ident::{InstanceId, KeyText, NameSegment};
use liasse_pred::{CandidateDescriptor, Member, ProjectShape, RowPrograms, RowProgramsParts};
use liasse_store::{
    AddressStep, CollectionPath, InstanceStore, KeyValue, RowAddress, SortDirection, Transition,
    ViewSource,
};
use liasse_syntax::parse_expression;
use liasse_value::{Integer, Struct, Text, Timestamp, Type, Value};

/// One item row: its int key, an `active` flag, a `label`, and an optional `rank`.
#[derive(Clone)]
struct Item {
    id: i64,
    active: bool,
    label: &'static str,
    rank: Option<i64>,
}

fn int(n: i64) -> Value {
    Value::Int(Integer::from(n))
}

fn key_text(id: i64) -> String {
    KeyText::from_key_values(&[int(id)]).unwrap().as_str().to_owned()
}

/// The stored payload of an item — an absent `rank` is simply omitted (§A.1).
fn item_value(item: &Item) -> Value {
    let mut fields = vec![
        (Text::new("active"), Value::Bool(item.active)),
        (Text::new("id"), int(item.id)),
        (Text::new("label"), Value::Text(Text::new(item.label))),
    ];
    if let Some(rank) = item.rank {
        fields.push((Text::new("rank"), int(rank)));
    }
    Value::Struct(Struct::new(fields))
}

/// The interpreter's materialized item row, built INDEPENDENTLY of the descriptor:
/// its key-derived identity, its key, and a cell per declared field (absent ⇒
/// `none`), exactly as `materialize::build_row` would.
fn item_row(item: &Item) -> Row {
    let rank = item.rank.map_or(Value::None, int);
    Row::new(
        RowId::keyed(key_text(item.id)),
        int(item.id),
        [
            ("active".to_owned(), Cell::scalar(Value::Bool(item.active))),
            ("id".to_owned(), Cell::scalar(int(item.id))),
            ("label".to_owned(), Cell::scalar(Value::Text(Text::new(item.label)))),
            ("rank".to_owned(), Cell::scalar(rank)),
        ],
    )
}

/// The package root the interpreter evaluates against: `items` as a keyed
/// collection and `allowed` as a root `set<int>` singleton (for a hoisted read).
fn root_row(items: &[Item], allowed: &BTreeSet<i64>) -> Row {
    // Materialization delivers a collection in Annex-B key order (B.5), so present
    // the rows key-ascending — an unsorted view then delivers that source order.
    let mut ordered: Vec<Item> = items.to_vec();
    ordered.sort_by_key(|item| item.id);
    let rows: Vec<Row> = ordered.iter().map(item_row).collect();
    let allowed_set: BTreeSet<Value> = allowed.iter().map(|n| int(*n)).collect();
    Row::keyless(
        RowId::leaf(0),
        [
            ("items".to_owned(), Cell::Collection(rows)),
            ("allowed".to_owned(), Cell::scalar(Value::Set(allowed_set))),
        ],
    )
}

/// The scope: `.`/`/` is the root row `{ items: view<item>, allowed: set<int> }`.
struct RootScope;

fn item_row_type() -> RowType {
    RowType::new(
        [
            ("active".to_owned(), ExprType::scalar(Type::Bool)),
            ("id".to_owned(), ExprType::scalar(Type::Int)),
            ("label".to_owned(), ExprType::scalar(Type::Text)),
            ("rank".to_owned(), ExprType::scalar(Type::Optional(Box::new(Type::Int)))),
        ],
        Some(ExprType::scalar(Type::Int)),
    )
}

fn root_type() -> ExprType {
    ExprType::Row(RowType::new(
        [
            ("items".to_owned(), ExprType::View(item_row_type())),
            ("allowed".to_owned(), ExprType::scalar(Type::Set(Box::new(Type::Int)))),
        ],
        None,
    ))
}

impl Scope for RootScope {
    fn current(&self) -> Option<ExprType> {
        Some(root_type())
    }
    fn parent(&self, _depth: u32) -> Option<ExprType> {
        None
    }
    fn root(&self) -> Option<ExprType> {
        Some(root_type())
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
    fn binding(&self, _name: &str) -> Option<ExprType> {
        None
    }
}

/// A minimal environment whose only live channel is the package root.
struct RootEnv {
    root: Row,
}

impl Environment for RootEnv {
    fn root(&self) -> &Row {
        &self.root
    }
    fn param(&self, _name: &str) -> Option<Cell> {
        None
    }
    fn structural(&self, _name: &str) -> Option<Cell> {
        None
    }
    fn import(&self, _name: &str) -> Option<Cell> {
        None
    }
    fn now(&self) -> Timestamp {
        Timestamp::new(0, liasse_value::Precision::DEFAULT)
    }
    fn uuid(&self, _site: liasse_expr::CallSite) -> liasse_value::Uuid {
        liasse_value::Uuid::from_bytes([0; 16])
    }
}

/// A comparable view row: its identity, its exposed fields, its `$sort` tuple.
type Extracted = (RowId, BTreeMap<String, Value>, Vec<Value>);

/// The exposed value of one interpreter cell (`cell_field_value`, §7.2).
fn field_value(cell: &Cell) -> Option<Value> {
    match cell {
        Cell::Scalar(Value::None) => None,
        Cell::Scalar(value) => Some(value.clone()),
        Cell::Row(row) if row.key() == &Value::None => Some(struct_of(row)),
        Cell::Row(_) | Cell::Collection(_) => None,
    }
}

fn struct_of(row: &Row) -> Value {
    Value::Struct(Struct::new(
        row.cells().filter_map(|(name, cell)| Some((Text::new(name.clone()), field_value(cell)?))),
    ))
}

/// Run the interpreter path: materialize the root, evaluate the view, extract rows.
fn interpreter_rows(view: &TypedExpr, root: &Row) -> Vec<Extracted> {
    let env = RootEnv { root: root.clone() };
    let current = Cell::Row(Box::new(root.clone()));
    let cell = view.evaluate_view(&env, &current).unwrap();
    let rows = match cell {
        Cell::Collection(rows) => rows,
        other => panic!("a flat view must deliver a collection, got {other:?}"),
    };
    rows.iter()
        .map(|row| {
            let fields = row
                .cells()
                .filter_map(|(name, cell)| Some((name.clone(), field_value(cell)?)))
                .collect();
            (row.id().clone(), fields, row.sort().to_vec())
        })
        .collect()
}

/// Run the pushdown path: build a store, lower the view, `scan_view`, reassemble.
fn pushdown_rows(view: &TypedExpr, items: &[Item], root: &Row) -> Vec<Extracted> {
    let mut store = liasse_store::MemoryStore::new(InstanceId::new("parity"));
    let mut txn = store.begin();
    for item in items {
        let address = RowAddress::root(AddressStep::new(
            NameSegment::new("items"),
            KeyValue::single(int(item.id)),
        ));
        txn.insert(address, item_value(item)).unwrap();
    }
    txn.commit().unwrap();

    let program = lower_program(view, root);
    let collection = CollectionPath::top(NameSegment::new("items"));
    let flat = lower_flat_view(view).unwrap();
    let evaluated = store
        .scan_view(ViewSource::Collection(&collection), &program, flat.skip, flat.limit)
        .unwrap();
    evaluated
        .iter()
        .map(|row| {
            let key = row.key_path.last().unwrap().components().next().unwrap();
            let id =
                RowId::keyed(KeyText::from_key_values(std::slice::from_ref(key)).unwrap().as_str().to_owned());
            let fields = match &row.projected {
                Value::Struct(fields) => {
                    fields.fields().map(|(n, v)| (n.as_str().to_owned(), v.clone())).collect()
                }
                other => panic!("projection must be a struct, got {other:?}"),
            };
            (id, fields, row.sort.clone())
        })
        .collect()
}

/// Lower a flat view into a [`RowPrograms`], hoisting candidate-free subexpressions
/// and pre-evaluating them against the interpreter environment (as the runtime
/// would at head frontier).
fn lower_program(view: &TypedExpr, root: &Row) -> RowPrograms {
    let flat = lower_flat_view(view).expect("view lowers");
    let env_source = RootEnv { root: root.clone() };
    let root_cell = Cell::Row(Box::new(root.clone()));
    let mut next = 0usize;
    let mut entries: Vec<(String, TypedExpr)> = Vec::new();

    let bind = flat.bind.clone();
    let admit = match &flat.filter {
        Some(condition) => {
            let refs = CandidateRefs::binds_only(bind.clone());
            let hoisted = hoist(condition, &refs, &mut next);
            audit(&hoisted.residual).expect("filter residual audits");
            entries.extend(hoisted.entries);
            Some(hoisted.residual)
        }
        None => None,
    };

    let output_names: Vec<String> = flat.outputs.iter().map(|(name, _)| name.clone()).collect();
    let mut outputs = Vec::new();
    for (name, expr) in &flat.outputs {
        let refs = CandidateRefs::current(bind.clone());
        let hoisted = hoist(expr, &refs, &mut next);
        audit(&hoisted.residual).expect("output residual audits");
        entries.extend(hoisted.entries);
        outputs.push((name.clone(), hoisted.residual));
    }

    let mut sort = Vec::new();
    for (expr, descending) in &flat.sort {
        let mut binds: Vec<String> = bind.clone().into_iter().collect();
        binds.extend(output_names.clone());
        let refs = CandidateRefs::current(binds);
        let hoisted = hoist(expr, &refs, &mut next);
        audit(&hoisted.residual).expect("sort residual audits");
        entries.extend(hoisted.entries);
        let direction = if *descending { SortDirection::Descending } else { SortDirection::Ascending };
        sort.push((hoisted.residual, direction));
    }

    let env: Vec<(String, Cell)> = entries
        .into_iter()
        .map(|(name, subtree)| (name, subtree.evaluate(&env_source, &root_cell).unwrap()))
        .collect();

    let descriptor = CandidateDescriptor::new(
        true,
        vec![
            Member::scalar("active"),
            Member::scalar("id"),
            Member::scalar("label"),
            Member::scalar("rank"),
        ],
    );
    RowPrograms::new(RowProgramsParts {
        admit,
        outputs,
        sort,
        env,
        bind,
        descriptor,
        subtree_steps: Vec::new(),
        shape: ProjectShape::Flat,
    })
    .unwrap()
}

fn check(text: &str) -> TypedExpr {
    let mut sources = liasse_diag::SourceMap::new();
    let src = sources.add_label("parity", text.to_owned());
    let parsed = parse_expression(src, text).unwrap();
    check_statement(&RootScope, src, &parsed).unwrap()
}

fn assert_parity(text: &str, items: &[Item], allowed: &BTreeSet<i64>) {
    let view = check(text);
    let root = root_row(items, allowed);
    let interpreter = interpreter_rows(&view, &root);
    let pushdown = pushdown_rows(&view, items, &root);
    assert_eq!(
        interpreter, pushdown,
        "\nview: {text}\ninterpreter: {interpreter:#?}\npushdown:    {pushdown:#?}"
    );
}

fn items() -> Vec<Item> {
    vec![
        Item { id: 3, active: true, label: "gamma", rank: Some(2) },
        Item { id: 1, active: true, label: "alpha", rank: None },
        Item { id: 4, active: false, label: "delta", rank: Some(2) },
        Item { id: 2, active: true, label: "beta", rank: Some(1) },
        Item { id: 5, active: true, label: "epsilon", rank: None },
    ]
}

#[test]
fn filter_multi_key_mixed_sort_none_placement() {
    // A bound filter, a mixed-direction two-key sort whose leading key is an
    // absent-optional (`none` placement), over ties on `rank`.
    assert_parity(
        ".items[:c | c.active] { id, label, rank, $sort: [rank, -id] }",
        &items(),
        &BTreeSet::new(),
    );
}

#[test]
fn descending_none_first() {
    assert_parity(".items { id, rank, $sort: [-rank] }", &items(), &BTreeSet::new());
}

#[test]
fn skip_and_limit_over_ties() {
    assert_parity(
        ".items[:c | c.active] { id, rank, $sort: [rank, id], $skip: 1, $limit: 2 }",
        &items(),
        &BTreeSet::new(),
    );
}

#[test]
fn hoisted_root_collection_read() {
    // `/allowed` is candidate-free and hoisted into the program env; the filter
    // membership test evaluates the candidate's `id` against the hoisted set.
    let allowed: BTreeSet<i64> = [1, 2, 5].into_iter().collect();
    assert_parity(".items[:c | c.id in /allowed] { id, $sort: [id] }", &items(), &allowed);
}

#[test]
fn unfiltered_unsorted_source_order() {
    assert_parity(".items { id, label }", &items(), &BTreeSet::new());
}
