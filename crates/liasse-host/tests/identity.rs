//! Contract identity, the Annex E.8 acceptance predicate, and the typed
//! op-signature shape the model compares against a call site (§16.2).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use liasse_host::{ContractError, ContractName, ContractRef, EffectClass, Version};
use liasse_value::Type;

use common::util_namespace;

/// The package-name grammar accepts dotted lowercase identifiers and rejects a
/// component that does not begin with a letter.
#[test]
fn contract_name_grammar() {
    assert!(ContractName::parse("liasse.cbor").is_ok());
    assert!(ContractName::parse("test.util_2").is_ok());
    assert!(matches!(
        ContractName::parse("1bad"),
        Err(ContractError::BadFirstChar { .. })
    ));
    assert!(matches!(ContractName::parse(""), Err(ContractError::EmptyName)));
}

/// Versions parse and order by component (E.1), so a newest-within-major choice
/// is well-defined.
#[test]
fn version_parse_and_order() {
    let a = Version::parse("1.2.0").expect("parses");
    let b = Version::parse("1.3.0").expect("parses");
    assert!(a < b);
    assert!(Version::parse("1.2").is_err());
    assert!(Version::parse("1.2.x").is_err());
}

/// E.8 acceptance: same contract and same major is a compatible substitution;
/// a different major is not.
#[test]
fn acceptance_is_by_major() {
    let required = ContractRef::new(ContractName::parse("test.util").expect("name"), 1);
    let util = ContractName::parse("test.util").expect("name");
    assert!(required.accepts(&util, Version::new(1, 9, 9)));
    assert!(!required.accepts(&util, Version::new(2, 0, 0)));
    let other = ContractName::parse("test.other").expect("name");
    assert!(!required.accepts(&other, Version::new(1, 0, 0)));
}

/// A namespace's declared op signature is a typed value the model can compare
/// against a call site: parameter and result types are `liasse_value::Type`s.
#[test]
fn op_signature_is_typed_for_shape_checking() {
    let ns = util_namespace(Version::new(1, 2, 0), "ih-util-1");
    let descriptor = liasse_host::HostNamespace::descriptor(&ns);
    let double = descriptor.function("double").expect("declares double");

    assert_eq!(double.effect(), EffectClass::Pure);
    assert_eq!(double.signature().params(), &[Type::Int]);
    assert_eq!(double.signature().result(), &Type::Int);
    // A call passing a `text` where an `int` is pinned is a shape mismatch the
    // model detects by comparing these types — not something the namespace has
    // to be invoked to discover.
    assert_ne!(double.signature().params(), &[Type::Text]);
}

/// A pure function's effect class permits it to run in a view; a generated one
/// does not (§16.3).
#[test]
fn effect_class_gates_view_evaluation() {
    assert!(EffectClass::Pure.runs_in_view());
    assert!(!EffectClass::Generated.runs_in_view());
    assert!(!EffectClass::Verifier.runs_in_view());
}
