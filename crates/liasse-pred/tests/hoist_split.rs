//! RED TEAM (Phase 7a): the §7.3 hoist SPLIT is semantically faithful and its
//! §7.5 audit is exact, at the MIXED candidate/candidate-free boundary the shipped
//! parity gate under-exercises (it only hoists a whole `/allowed` read).
//!
//! `hoist` must replace every MAXIMAL candidate-free subtree that reaches outside
//! the candidate with a synthetic binding, leave everything candidate-dependent (and
//! every pure candidate-free constant) in the residual, and produce a residual that
//! evaluates — against the candidate plus the pre-evaluated hoisted env — to exactly
//! what the interpreter computes directly. `audit` must accept a clean residual and
//! REJECT one that still reaches a candidate-dependent external (a host call over the
//! candidate), so the caller falls back to the interpreter rather than shipping an
//! unservable residual (§7.5).
//!
//! Oracle: the shared interpreter's own direct evaluation of the ORIGINAL expression
//! (`TypedExpr::evaluate`) — externally deducible per AGENTS.md. A residual that
//! disagrees, a non-maximal/over-eager hoist, or an audit that mis-classifies a
//! mixed tree = HIGH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use liasse_diag::SourceMap;
use liasse_expr::hoist::{audit, hoist, CandidateRefs};
use liasse_expr::{
    CallSite, Cell, Environment, EvalError, ExprType, HostEffect, HostOp, HostPosition, Row, RowId,
    RowType, Scope, TypedExpr,
};
use liasse_syntax::parse_expression;
use liasse_value::{Integer, Timestamp, Type, Uuid, Value};

fn int(n: i64) -> Value {
    Value::Int(Integer::from(n))
}

/// Root `{ other, base, scale }` (all int), a candidate binding `c: int`, and one
/// pure host op `util.double(int) -> int` in a pure (view) position.
struct HoistScope;

fn root_type() -> ExprType {
    ExprType::Row(RowType::new(
        [
            ("other".to_owned(), ExprType::scalar(Type::Int)),
            ("base".to_owned(), ExprType::scalar(Type::Int)),
            ("scale".to_owned(), ExprType::scalar(Type::Int)),
        ],
        None,
    ))
}

impl Scope for HoistScope {
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
    fn binding(&self, name: &str) -> Option<ExprType> {
        (name == "c").then(|| ExprType::scalar(Type::Int))
    }
    fn namespace_op(&self, namespace: &str, function: &str) -> Option<HostOp> {
        ((namespace, function) == ("util", "double"))
            .then(|| HostOp::new([Type::Int], Type::Int, HostEffect::Pure))
    }
    fn host_position(&self) -> HostPosition {
        HostPosition::Pure
    }
}

/// An environment serving `binding(name)` from a map (the candidate `c` and every
/// hoisted synthetic), plus the fixed root row and a `util.double` host op.
struct BindEnv {
    root: Row,
    binds: BTreeMap<String, Cell>,
}

impl Environment for BindEnv {
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
    fn binding(&self, name: &str) -> Option<Cell> {
        self.binds.get(name).cloned()
    }
    fn now(&self) -> Timestamp {
        Timestamp::new(0, liasse_value::Precision::Micros)
    }
    fn uuid(&self, _site: CallSite) -> Uuid {
        Uuid::from_bytes([0; 16])
    }
    fn host_call(&self, namespace: &str, function: &str, args: &[Value]) -> Result<Value, EvalError> {
        match (namespace, function, args) {
            ("util", "double", [Value::Int(n)]) => Ok(Value::Int(Integer::from(n.as_bigint() * 2))),
            _ => Err(EvalError::HostCall { detail: "unexpected call".to_owned() }),
        }
    }
}

fn root_row() -> Row {
    Row::keyless(
        RowId::leaf(0),
        [
            ("other".to_owned(), Cell::scalar(int(100))),
            ("base".to_owned(), Cell::scalar(int(10))),
            ("scale".to_owned(), Cell::scalar(int(5))),
        ],
    )
}

fn check(source: &str) -> TypedExpr {
    let mut sources = SourceMap::new();
    let id = sources.add_label("hoist", source.to_owned());
    let parsed = parse_expression(id, source)
        .unwrap_or_else(|d| panic!("parse failed:\n{}", d.render(&sources)));
    liasse_expr::check_statement(&HoistScope, id, &parsed)
        .unwrap_or_else(|d| panic!("check failed:\n{}", d.render(&sources)))
}

/// The candidate binding: `c = int(3)`.
fn candidate_binds() -> BTreeMap<String, Cell> {
    let mut binds = BTreeMap::new();
    binds.insert("c".to_owned(), Cell::scalar(int(3)));
    binds
}

/// Hoist `source` (a flat filter's classification: `.` is the receiver, only `c`
/// is the candidate), assert the entry count and a clean audit, and prove the
/// residual + pre-evaluated env evaluates to the interpreter's direct result.
fn assert_faithful_split(source: &str, expected_entries: usize) {
    let expr = check(source);
    let refs = CandidateRefs::binds_only(["c".to_owned()]);
    let mut next = 0usize;
    let hoisted = hoist(&expr, &refs, &mut next);

    assert_eq!(
        hoisted.entries.len(),
        expected_entries,
        "`{source}` should hoist {expected_entries} maximal candidate-free subtree(s), got {}: {:?}",
        hoisted.entries.len(),
        hoisted.entries.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    );
    audit(&hoisted.residual).unwrap_or_else(|kind| panic!("`{source}` residual should audit, got: {kind}"));

    let root = root_row();
    let current = Cell::Row(Box::new(root.clone()));

    // Oracle: the interpreter's direct evaluation, candidate `c` served as a binding.
    let oracle_env = BindEnv { root: root.clone(), binds: candidate_binds() };
    let oracle = expr.evaluate(&oracle_env, &current).expect("oracle evaluates");

    // Pre-evaluate each hoisted subtree once (candidate-free), then evaluate the
    // residual against candidate + hoisted env — the pushdown reconstruction.
    let mut binds = candidate_binds();
    for (name, subtree) in &hoisted.entries {
        let value = subtree.evaluate(&oracle_env, &current).expect("hoisted subtree evaluates");
        binds.insert(name.clone(), value);
    }
    let residual_env = BindEnv { root, binds };
    let residual = hoisted.residual.evaluate(&residual_env, &current).expect("residual evaluates");

    assert_eq!(residual, oracle, "`{source}`: residual ≠ interpreter");
}

#[test]
fn candidate_free_read_is_hoisted() {
    // A single `/`-read as the free operand of a candidate comparison.
    assert_faithful_split("c == /other", 1);
}

#[test]
fn mixed_arith_hoists_only_the_free_operand() {
    // `c + /base`: `c` stays in the residual, `/base` is the one hoisted subtree.
    assert_faithful_split("c + /base", 1);
}

#[test]
fn two_free_operands_separated_by_a_candidate_node_hoist_separately() {
    // `(c + /base) * /scale`: `/base` and `/scale` are each maximal candidate-free
    // subtrees split apart by the candidate-dependent `c + /base`, so TWO hoists.
    assert_faithful_split("(c + /base) * /scale", 2);
}

#[test]
fn adjacent_free_operands_hoist_as_one_maximal_subtree() {
    // `c > /other + /base`: `/other + /base` is ONE maximal candidate-free subtree,
    // hoisted whole — not two separate reads.
    assert_faithful_split("c > /other + /base", 1);
}

#[test]
fn pure_constant_subtree_stays_in_the_residual() {
    // `c + 2`: `2` reaches nothing external, so it is NOT hoisted — the minimal env
    // evaluates it per-candidate exactly as the interpreter does.
    assert_faithful_split("c + 2", 0);
}

#[test]
fn candidate_free_host_call_is_hoisted_whole() {
    // A candidate-free host call reaches outside the candidate and is hoisted as one
    // subtree; the residual is a pure synthetic and audits clean.
    assert_faithful_split("c == util.double(/base)", 1);
}

#[test]
fn audit_rejects_a_candidate_dependent_host_call() {
    // `util.double(c)` reaches an EXTERNAL (a host call) whose argument is the
    // candidate, so it cannot be hoisted (it references the candidate) and cannot be
    // served by the minimal env. Nothing is hoisted, and the audit MUST reject it so
    // the caller falls back to the interpreter (§7.5).
    let expr = check("util.double(c) == 6");
    let refs = CandidateRefs::binds_only(["c".to_owned()]);
    let mut next = 0usize;
    let hoisted = hoist(&expr, &refs, &mut next);
    assert!(hoisted.entries.is_empty(), "a candidate-dependent host call is not hoistable");
    assert!(
        audit(&hoisted.residual).is_err(),
        "the residual still reaches a candidate-dependent host call and must fail audit",
    );
}

#[test]
fn audit_accepts_the_candidate_free_host_call_contrast() {
    // The mirror image: the SAME host call over a `/`-read is candidate-free, so it
    // hoists and the residual audits clean — proving the audit discriminates on
    // candidate-dependence, not on the external's mere presence.
    let expr = check("util.double(/base) == 6");
    let refs = CandidateRefs::binds_only(["c".to_owned()]);
    let mut next = 0usize;
    let hoisted = hoist(&expr, &refs, &mut next);
    assert_eq!(hoisted.entries.len(), 1);
    audit(&hoisted.residual).expect("a candidate-free host call hoists and audits clean");
}
