#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! §16.2/§16.3 host-namespace call typing and evaluation at the expression layer.
//!
//! The checker resolves a `namespace.function(args)` call against a pinned
//! signature the [`Scope`] supplies ([`Scope::namespace_op`]) and admits it only
//! where the position's effect policy permits ([`Scope::host_position`]); the
//! evaluator defers the call to [`Environment::host_call`]. Both are exercised
//! here against a hand-built scope/environment pair whose signatures and results
//! are stated in the test, so every expectation is externally deducible.

mod common;

use std::collections::BTreeMap;

use common::{scell, vint};
use liasse_expr::{
    CallSite, Cell, Environment, EvalError, ExprType, HostEffect, HostOp, HostPosition, Row, RowId,
    Scope, TypedExpr,
};
use liasse_syntax::parse_expression;
use liasse_value::{Integer, Text, Timestamp, Type, Uuid, Value};

/// A scope carrying one host namespace `util` with a fixed op table, at a chosen
/// effect position. `.` and `/` are an `int` so a literal-argument host call
/// type-checks without further state.
struct HostScope {
    ops: BTreeMap<(String, String), HostOp>,
    position: HostPosition,
}

impl HostScope {
    fn new(position: HostPosition) -> Self {
        let mut ops = BTreeMap::new();
        ops.insert(
            ("util".to_owned(), "double".to_owned()),
            HostOp::new([Type::Int], Type::Int, HostEffect::Pure),
        );
        ops.insert(
            ("util".to_owned(), "token".to_owned()),
            HostOp::new([], Type::Text, HostEffect::Generated),
        );
        ops.insert(
            ("util".to_owned(), "check".to_owned()),
            HostOp::new([Type::Text], Type::Text, HostEffect::Verifier),
        );
        Self { ops, position }
    }
}

impl Scope for HostScope {
    fn current(&self) -> Option<ExprType> {
        Some(ExprType::scalar(Type::Int))
    }
    fn parent(&self, _depth: u32) -> Option<ExprType> {
        None
    }
    fn root(&self) -> Option<ExprType> {
        Some(ExprType::scalar(Type::Int))
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
    fn namespace_op(&self, namespace: &str, function: &str) -> Option<HostOp> {
        self.ops.get(&(namespace.to_owned(), function.to_owned())).cloned()
    }
    fn host_position(&self) -> HostPosition {
        self.position
    }
}

/// An environment whose `host_call` performs the sim behaviour directly (no
/// conformance guard — that is the runtime's layer): `double` doubles its integer
/// argument, `token` returns a fixed text, and `explode` fails.
struct HostEnv {
    root: Row,
}

impl HostEnv {
    fn new() -> Self {
        Self { root: Row::keyless(RowId::leaf(0), Vec::new()) }
    }
}

impl Environment for HostEnv {
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
        Timestamp::new(0, liasse_value::Precision::Micros)
    }
    fn uuid(&self, _site: CallSite) -> Uuid {
        Uuid::from_bytes([0; 16])
    }
    fn host_call(&self, namespace: &str, function: &str, args: &[Value]) -> Result<Value, EvalError> {
        match (namespace, function, args) {
            ("util", "double", [Value::Int(n)]) => {
                Ok(Value::Int(Integer::from(n.as_bigint() * 2)))
            }
            ("util", "token", []) => Ok(Value::Text(Text::new("tok"))),
            ("util", "explode", _) => Err(EvalError::HostCall { detail: "boom".to_owned() }),
            _ => Err(EvalError::HostCall { detail: "unexpected call".to_owned() }),
        }
    }
}

fn check(scope: &dyn Scope, source: &str) -> Result<TypedExpr, String> {
    let mut sources = liasse_diag::SourceMap::new();
    let id = sources.add_label("test", source);
    let parsed = parse_expression(id, source).map_err(|d| d.render(&sources))?;
    liasse_expr::check_statement(scope, id, &parsed).map_err(|d| d.render(&sources))
}

fn eval(scope: &dyn Scope, env: &dyn Environment, source: &str) -> Result<Cell, EvalError> {
    let typed = check(scope, source).expect("type-checks");
    typed.evaluate(env, &scell(vint(0)))
}

/// §16.3: a pure host function type-checks in a pure (view/check) position, and
/// its result type is the pinned signature's result.
#[test]
fn pure_call_typechecks_in_a_pure_position() {
    let scope = HostScope::new(HostPosition::Pure);
    let typed = check(&scope, "util.double(3)").expect("a pure call is admissible in a view");
    assert_eq!(typed.ty().as_scalar(), Some(&Type::Int), "result type is the pinned `int`");
}

/// §16.3/§8.8: a generated host function may not run in a pure position.
#[test]
fn generated_call_in_a_pure_position_is_rejected() {
    let scope = HostScope::new(HostPosition::Pure);
    let error = check(&scope, "util.token()").expect_err("generated is not admissible in a view");
    assert!(error.contains("generated"), "the diagnostic names the effect class: {error}");
}

/// §16.3: a verifier may not run in a pure position.
#[test]
fn verifier_call_in_a_pure_position_is_rejected() {
    let scope = HostScope::new(HostPosition::Pure);
    let error = check(&scope, "util.check('x')").expect_err("verifier is not admissible in a view");
    assert!(error.contains("verifier"), "the diagnostic names the effect class: {error}");
}

/// §16.3/§8.8: a write position (a field default, a mutation value) admits a
/// generated function.
#[test]
fn generated_call_in_a_write_position_is_admissible() {
    let scope = HostScope::new(HostPosition::Write);
    let typed = check(&scope, "util.token()").expect("generated is admissible in a write position");
    assert_eq!(typed.ty().as_scalar(), Some(&Type::Text));
}

/// §16.3: an admission (`$verify`) position admits a verifier.
#[test]
fn verifier_call_in_an_admission_position_is_admissible() {
    let scope = HostScope::new(HostPosition::Admission);
    check(&scope, "util.check('x')").expect("verifier is admissible at admission");
}

/// §16.2: an argument whose type does not match the pinned signature is a static
/// type error — `double` expects `int`, a `text` is supplied.
#[test]
fn argument_type_mismatch_is_rejected() {
    let scope = HostScope::new(HostPosition::Pure);
    let error = check(&scope, "util.double('x')").expect_err("text is not an int argument");
    assert!(error.contains("int"), "the diagnostic names the expected type: {error}");
}

/// §16.2: the argument count must match the pinned signature.
#[test]
fn arity_mismatch_is_rejected() {
    let scope = HostScope::new(HostPosition::Pure);
    check(&scope, "util.double(1, 2)").expect_err("double takes one argument");
}

/// §16.2: a namespace the scope does not declare is an unknown function — the
/// checker never invents a host call.
#[test]
fn undeclared_namespace_is_an_unknown_function() {
    let scope = HostScope::new(HostPosition::Pure);
    let error = check(&scope, "cbor.encode(1)").expect_err("cbor is not a declared namespace");
    assert!(error.contains("unknown function"), "an undeclared call is unknown: {error}");
}

/// §16.1: a core `string` utility still resolves as a built-in, unaffected by the
/// host-namespace resolution path.
#[test]
fn core_string_namespace_still_resolves() {
    let scope = HostScope::new(HostPosition::Pure);
    let typed = check(&scope, "string.upper('ab')").expect("string.upper is a core built-in");
    assert_eq!(typed.ty().as_scalar(), Some(&Type::Text));
}

/// §16.2/§16.3: evaluation defers a resolved host call to the environment's
/// host-call hook; `double(3) = 6` is produced by the environment, not the
/// evaluator.
#[test]
fn evaluation_dispatches_to_the_host_call_hook() {
    let scope = HostScope::new(HostPosition::Pure);
    let env = HostEnv::new();
    let result = eval(&scope, &env, "util.double(3)").expect("host call evaluates");
    assert_eq!(result.as_scalar(), Some(&vint(6)));
}

/// A host call composes with ordinary arithmetic: the hook's result flows into the
/// surrounding expression (`double(3) + 1 = 7`).
#[test]
fn host_call_result_composes_with_arithmetic() {
    let scope = HostScope::new(HostPosition::Pure);
    let env = HostEnv::new();
    let result = eval(&scope, &env, "util.double(3) + 1").expect("evaluates");
    assert_eq!(result.as_scalar(), Some(&vint(7)));
}

/// A host-call failure from the environment surfaces as a typed [`EvalError`], not
/// a value — the runtime maps this to a host rejection.
#[test]
fn host_call_failure_propagates_as_a_typed_error() {
    let mut scope = HostScope::new(HostPosition::Pure);
    // Declare a pure `explode : (int) -> int` the environment fails.
    scope
        .ops
        .insert(("util".to_owned(), "explode".to_owned()), HostOp::new([Type::Int], Type::Int, HostEffect::Pure));
    let env = HostEnv::new();
    let error = eval(&scope, &env, "util.explode(1)").expect_err("the host call fails");
    assert!(matches!(error, EvalError::HostCall { .. }), "a host-call refusal is typed: {error:?}");
}

/// The default [`Environment::host_call`] owns no dispatch: a resolved host call
/// against a bare environment is a contract breach, not a silent value.
#[test]
fn default_environment_has_no_host_dispatch() {
    struct Bare;
    impl Environment for Bare {
        fn root(&self) -> &Row {
            // A leaked static keeps the borrow simple for this negative test.
            static ROOT: std::sync::OnceLock<Row> = std::sync::OnceLock::new();
            ROOT.get_or_init(|| Row::keyless(RowId::leaf(0), Vec::new()))
        }
        fn param(&self, _n: &str) -> Option<Cell> {
            None
        }
        fn structural(&self, _n: &str) -> Option<Cell> {
            None
        }
        fn import(&self, _n: &str) -> Option<Cell> {
            None
        }
        fn now(&self) -> Timestamp {
            Timestamp::new(0, liasse_value::Precision::Micros)
        }
        fn uuid(&self, _s: CallSite) -> Uuid {
            Uuid::from_bytes([0; 16])
        }
    }
    let scope = HostScope::new(HostPosition::Pure);
    let typed = check(&scope, "util.double(3)").expect("type-checks");
    let error = typed.evaluate(&Bare, &scell(vint(0))).expect_err("no host dispatch");
    assert!(matches!(error, EvalError::NoHostDispatch), "a bare environment rejects: {error:?}");
}
