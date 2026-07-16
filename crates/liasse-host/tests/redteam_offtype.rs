//! Red-team regression: the conformance guard must catch an off-contract
//! *variant* even when the offending value's canonical wire spelling coincides
//! with the declared type's wire form. A `Value::Text("42")` returned where an
//! `int` is declared shares its JSON-string wire shape with an `int`, so a
//! wire round-trip check waves it through; a structural variant check does not
//! (guard contract: `checked.rs` — a non-conforming return is `OffContractType`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;

use liasse_host::{
    ConformanceGuard, ConformanceViolation, ContractName, EffectClass, FunctionDescriptor,
    GuardError, HostNamespace, InterfaceHash, InvocationFailure, NamespaceDescriptor, OpSignature,
    Version,
};
use liasse_value::{Integer, Text, Type, Value};

/// A `probe : (int) -> R` namespace that returns a fixed `Value` regardless of
/// the arguments, so a test can declare a result type `R` and hand back a value
/// that may or may not conform to it structurally.
struct ProbeNamespace {
    descriptor: NamespaceDescriptor,
    returns: Value,
}

impl ProbeNamespace {
    fn new(result_type: Type, returns: Value) -> Self {
        let descriptor = NamespaceDescriptor::new(
            ContractName::parse("test.util").expect("name"),
            Version::new(1, 0, 0),
            InterfaceHash::new("ih-probe"),
            std::iter::empty(),
            std::iter::once((
                "probe".to_owned(),
                FunctionDescriptor::new(
                    OpSignature::new([Type::Int], result_type),
                    EffectClass::Pure,
                ),
            )),
        );
        Self {
            descriptor,
            returns,
        }
    }
}

impl HostNamespace for ProbeNamespace {
    fn descriptor(&self) -> &NamespaceDescriptor {
        &self.descriptor
    }

    fn invoke(&self, function: &str, _args: &[Value]) -> Result<Value, InvocationFailure> {
        if function == "probe" {
            Ok(self.returns.clone())
        } else {
            Err(InvocationFailure::UnknownFunction(function.to_owned()))
        }
    }
}

fn probe(result_type: Type, returns: Value) -> Result<Value, GuardError> {
    let ns = ProbeNamespace::new(result_type, returns);
    ConformanceGuard::new().invoke(&ns, "probe", &[Value::Int(Integer::from(1))])
}

/// The reported finding: `double : (int) -> int` returns a `Value::Text("42")`,
/// whose JSON-string wire form is indistinguishable from an int's. The guard
/// must catch the wrong *variant* even though `"42"` decodes as a valid int.
#[test]
fn numeric_text_returned_where_int_declared_is_caught() {
    match probe(Type::Int, Value::Text(Text::new("42"))) {
        Err(GuardError::Violation(violation)) => assert!(matches!(
            *violation,
            ConformanceViolation::OffContractType { .. }
        )),
        Ok(value) => panic!(
            "guard returned Ok({value:?}) — an off-contract Text passed as int. \
             The runtime now holds a Value::Text where the pinned signature \
             promises a Value::Int."
        ),
        Err(other) => panic!("expected an OffContractType violation, got {other:?}"),
    }
}

/// A genuinely conforming `int` return still passes — the fix rejects a wrong
/// variant, not the declared one.
#[test]
fn conforming_int_return_passes() {
    let value = probe(Type::Int, Value::Int(Integer::from(42))).expect("conforming int passes");
    assert_eq!(value, Value::Int(Integer::from(42)));
}

/// The wire spellings of `int`, `uuid`, `date`, `decimal`, and an `enum` label
/// all canonicalise to a JSON string, so a numeric-looking `text` returned
/// where any of them is declared would slip a wire round-trip. Each must be
/// caught as an off-contract variant.
#[test]
fn numeric_text_is_caught_across_string_wire_types() {
    for declared in [Type::Uuid, Type::Date, Type::Decimal, Type::Bytes] {
        match probe(declared.clone(), Value::Text(Text::new("42"))) {
            Err(GuardError::Violation(violation)) => assert!(
                matches!(*violation, ConformanceViolation::OffContractType { .. }),
                "declared {}: expected OffContractType",
                declared.name()
            ),
            other => panic!("declared {}: expected a violation, got {other:?}", declared.name()),
        }
    }
}

/// A wrong variant hidden inside a composite (a `Value::Text` element of a
/// declared `set<int>`) is caught by recursing into the composite, not just the
/// top-level variant.
#[test]
fn wrong_variant_inside_a_set_is_caught() {
    let mut members = BTreeSet::new();
    members.insert(Value::Text(Text::new("7")));
    let declared = Type::Set(Box::new(Type::Int));

    match probe(declared, Value::Set(members)) {
        Err(GuardError::Violation(violation)) => assert!(matches!(
            *violation,
            ConformanceViolation::OffContractType { .. }
        )),
        other => panic!("expected a set-element violation, got {other:?}"),
    }
}
