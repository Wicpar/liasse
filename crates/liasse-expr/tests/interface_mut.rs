#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! §13.8/§13.10 interface-mutation call typing at the expression layer.
//!
//! `#handle.mutation(args)` addressed on an imported module instance is a
//! well-typed call whose result is the interface contract's declared `$return`
//! (not a view-shape read that faults). The checker resolves the contract through
//! [`Scope::interface_mut`], types each argument against the declared prototype,
//! and yields a `TypedKind::InterfaceCall` the runtime admits within the parent
//! transition — the pure evaluator refuses it loudly ([`EvalError::InterfaceDispatch`]),
//! since a cross-engine dispatch is a transition effect, never a pure value.

use liasse_expr::{
    CallSite, Cell, Environment, EvalError, ExprType, InterfaceMut, Row, RowId, Scope, TypedExpr,
};
use liasse_syntax::parse_expression;
use liasse_value::{StructType, Timestamp, Type, Uuid, Value};

/// A scope binding `#credits` (a peer interface handle) with one `$mut` contract
/// `consume({ amount: decimal }) -> { remaining: decimal }`, and a `@cost`
/// parameter typed `decimal`, so a well-formed call type-checks.
struct IfaceScope;

impl IfaceScope {
    fn ret_ty() -> ExprType {
        ExprType::scalar(Type::Struct(StructType::new(vec![(
            "remaining".to_owned(),
            Type::Decimal,
        )])))
    }
}

impl Scope for IfaceScope {
    fn current(&self) -> Option<ExprType> {
        Some(ExprType::scalar(Type::Int))
    }
    fn parent(&self, _depth: u32) -> Option<ExprType> {
        None
    }
    fn root(&self) -> Option<ExprType> {
        Some(ExprType::scalar(Type::Int))
    }
    fn param(&self, name: &str) -> Option<ExprType> {
        (name == "cost").then(|| ExprType::scalar(Type::Decimal))
    }
    fn structural(&self, _name: &str) -> Option<ExprType> {
        None
    }
    fn import(&self, name: &str) -> Option<ExprType> {
        // The peer handle reads as its interface `$view` shape; the `$mut` contract
        // is resolved separately through `interface_mut`.
        (name == "credits").then(|| ExprType::scalar(Type::Int))
    }
    fn binding(&self, _name: &str) -> Option<ExprType> {
        None
    }
    fn interface_mut(&self, handle: &str, mutation: &str) -> Option<InterfaceMut> {
        if handle == "credits" && mutation == "consume" {
            Some(InterfaceMut {
                params: vec![("amount".to_owned(), ExprType::scalar(Type::Decimal))],
                ret: Self::ret_ty(),
            })
        } else {
            None
        }
    }
}

struct NullEnv {
    root: Row,
}

impl Environment for NullEnv {
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
}

fn check(source: &str) -> Result<TypedExpr, String> {
    let mut sources = liasse_diag::SourceMap::new();
    let id = sources.add_label("test", source);
    let parsed = parse_expression(id, source).map_err(|d| d.render(&sources))?;
    liasse_expr::check_statement(&IfaceScope, id, &parsed).map_err(|d| d.render(&sources))
}

/// §13.8/§13.10: `#credits.consume({ amount: @cost })` type-checks as a callable
/// whose result is the contract's declared `$return` — a `{ remaining: decimal }`
/// struct — carrying the resolved handle, contract, and typed argument.
#[test]
fn interface_call_types_as_its_return() {
    let typed = check("#credits.consume({ amount: @cost })")
        .expect("a bound interface mutation type-checks");
    assert_eq!(
        typed.ty(),
        &IfaceScope::ret_ty(),
        "result type is the declared `$return`"
    );
    let call = typed
        .as_interface_call()
        .expect("the node is an interface-mutation dispatch");
    assert_eq!(call.handle, "credits");
    assert_eq!(call.mutation, "consume");
    assert_eq!(call.args.len(), 1);
    let (name, value) = call.args.first().expect("one argument");
    assert_eq!(name, "amount");
    assert_eq!(value.ty().as_scalar(), Some(&Type::Decimal));
}

/// §13.10: the pure value evaluator cannot dispatch a cross-engine mutation, so it
/// refuses the node loudly rather than faking a value — the runtime intercepts it
/// before evaluation.
#[test]
fn interface_call_refuses_pure_evaluation() {
    let typed = check("#credits.consume({ amount: @cost })").expect("type-checks");
    let env = NullEnv {
        root: Row::keyless(RowId::leaf(0), Vec::new()),
    };
    let error = typed
        .evaluate(&env, &Cell::Scalar(Value::None))
        .expect_err("a dispatch is not a pure value");
    assert_eq!(error, EvalError::InterfaceDispatch);
}

/// A `#handle.mutation` the interface does not declare is refused at check time.
#[test]
fn unknown_interface_mutation_is_rejected() {
    let error = check("#credits.refund({ amount: @cost })").expect_err("no such contract");
    assert!(
        error.contains("no interface mutation `refund`"),
        "diagnostic names the missing contract: {error}"
    );
}

/// An argument typed against a declared parameter's type must conform.
#[test]
fn interface_call_arg_type_mismatch_is_rejected() {
    let error = check("#credits.consume({ amount: 'x' })").expect_err("text is not a decimal");
    assert!(
        error.contains("expects `decimal`"),
        "diagnostic names the expected type: {error}"
    );
}

/// Every declared non-optional parameter must be supplied.
#[test]
fn interface_call_missing_argument_is_rejected() {
    let error = check("#credits.consume({ })").expect_err("`amount` is required");
    assert!(
        error.contains("missing argument `amount`"),
        "diagnostic names the missing argument: {error}"
    );
}

/// An argument not named by the declared prototype is refused.
#[test]
fn interface_call_undeclared_argument_is_rejected() {
    let error =
        check("#credits.consume({ amount: @cost, tip: @cost })").expect_err("`tip` is undeclared");
    assert!(
        error.contains("declares no parameter `tip`"),
        "diagnostic names the undeclared parameter: {error}"
    );
}
