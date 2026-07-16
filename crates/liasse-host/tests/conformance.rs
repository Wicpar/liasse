//! The checked-invocation guard: a well-behaved namespace passes; an
//! off-contract-type return and a drifting pure function are caught as typed
//! conformance violations; a verifier's rejection is passed through as a
//! contract-honouring failure, not a violation (SPEC-ISSUES items 15/16).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use liasse_host::sim::SimNamespace;
use liasse_host::{
    ConformanceGuard, ConformanceViolation, GuardError, HostNamespace, InvocationFailure,
};
use liasse_value::{Integer, Value};

use common::{
    drifting_namespace, off_type_namespace, token_namespace, util_namespace, verifier_namespace,
};

fn int(n: i64) -> Value {
    Value::Int(Integer::from(n))
}

/// A well-behaved pure function returns a conforming value and is stable across
/// equal-argument calls.
#[test]
fn well_behaved_pure_call_passes() {
    let ns = util_namespace(liasse_host::Version::new(1, 0, 0), "ih");
    let mut guard = ConformanceGuard::new();
    let first = guard.invoke(&ns, "double", &[int(21)]).expect("ok");
    assert_eq!(first, int(42));
    // A second identical call is stable — no drift.
    let second = guard.invoke(&ns, "double", &[int(21)]).expect("ok");
    assert_eq!(second, int(42));
}

/// A function whose declared result is `int` but returns a `text` is caught as
/// an off-contract-type violation (item 15).
#[test]
fn off_contract_type_is_caught() {
    let ns = off_type_namespace();
    let mut guard = ConformanceGuard::new();
    match guard.invoke(&ns, "double", &[int(1)]) {
        Err(GuardError::Violation(violation)) => {
            assert!(matches!(
                *violation,
                ConformanceViolation::OffContractType { .. }
            ));
        }
        other => panic!("expected OffContractType, got {other:?}"),
    }
}

/// A declared-`pure` function that returns a different value for equal
/// arguments across evaluations is caught as drift (items 15/16). The double's
/// `advance` stands in for a fresh evaluation/replay.
#[test]
fn pure_drift_is_caught() {
    let mut ns = drifting_namespace();
    let mut guard = ConformanceGuard::new();
    let first = guard.invoke(&ns, "drift", &[int(0)]).expect("first ok");
    ns.advance();
    match guard.invoke(&ns, "drift", &[int(0)]) {
        Err(GuardError::Violation(violation)) => match *violation {
            ConformanceViolation::PureDrift { first: recorded, .. } => {
                assert_eq!(recorded, first.to_canonical_json_string());
            }
            other => panic!("expected PureDrift, got {other:?}"),
        },
        other => panic!("expected drift violation, got {other:?}"),
    }
}

/// A verifier that rejects an unaccepted credential surfaces a contract-
/// honouring `Verification` failure, not a conformance violation.
#[test]
fn verifier_rejection_is_not_a_violation() {
    use liasse_value::Text;
    let ns = verifier_namespace("good", "proof-1");
    let mut guard = ConformanceGuard::new();

    let ok = guard
        .invoke(&ns, "accept", &[Value::Text(Text::new("good"))])
        .expect("accepted");
    assert_eq!(ok, Value::Text(Text::new("proof-1")));

    match guard.invoke(&ns, "accept", &[Value::Text(Text::new("bad"))]) {
        Err(GuardError::Invocation(InvocationFailure::Verification { .. })) => {}
        other => panic!("expected Verification failure, got {other:?}"),
    }
}

/// A generated function is not subject to drift detection: two evaluations may
/// (and do) differ, and neither is a violation.
#[test]
fn generated_function_may_vary() {
    let mut ns = token_namespace();
    let mut guard = ConformanceGuard::new();
    let first = guard.invoke(&ns, "token", &[]).expect("first token");
    ns.advance();
    let second = guard.invoke(&ns, "token", &[]).expect("second token");
    assert_ne!(first, second);
}

/// Invoking a function the descriptor does not declare is a typed violation
/// (there is no signature to check against).
#[test]
fn undeclared_function_is_a_violation() {
    let ns: SimNamespace = util_namespace(liasse_host::Version::new(1, 0, 0), "ih");
    let namespace: &dyn HostNamespace = &ns;
    let mut guard = ConformanceGuard::new();
    assert!(matches!(
        guard.invoke(namespace, "missing", &[int(1)]),
        Err(GuardError::Violation(_))
    ));
}
