//! Registry resolution: Annex E.8 major-compatibility acceptance, and the
//! typed missing/incompatible/ambiguous distinction the model relies on to
//! reject a package before activation (§16.2, §9.2 step 4).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use liasse_host::{ContractRef, Registry, ResolutionError, Version};

use common::{name, util_namespace};

/// A requirement `test.util@1` resolves to a registered `1.4.2` — E.8 permits
/// moving to a compatible minor/patch within the same major.
#[test]
fn compatible_minor_resolves_within_major() {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(util_namespace(Version::new(1, 4, 2), "ih-142")));

    let required = ContractRef::new(name("test.util"), 1);
    let resolved = registry.resolve_namespace(&required).expect("resolves");
    assert_eq!(resolved.descriptor().version(), Version::new(1, 4, 2));
}

/// A requirement for major 1 is not satisfied by a registered major 2, even
/// when the function surface looks identical — that is `Incompatible`, not
/// `Missing`.
#[test]
fn incompatible_major_rejects() {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(util_namespace(Version::new(2, 0, 0), "ih-2")));

    let required = ContractRef::new(name("test.util"), 1);
    match registry.resolve_namespace(&required) {
        Err(ResolutionError::Incompatible { found, .. }) => {
            assert_eq!(found, vec![Version::new(2, 0, 0)]);
        }
        _ => panic!("expected Incompatible"),
    }
}

/// A requirement for a contract with no registered descriptor at all is
/// `Missing`.
#[test]
fn absent_contract_is_missing() {
    let registry = Registry::new();
    let required = ContractRef::new(name("liasse.cbor"), 1);
    assert!(matches!(
        registry.resolve_namespace(&required),
        Err(ResolutionError::Missing { .. })
    ));
}

/// Two distinct descriptors with the same id and version but different
/// interface hashes cannot be resolved by any reading — `Ambiguous`.
#[test]
fn same_version_distinct_interface_is_ambiguous() {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(util_namespace(Version::new(1, 2, 0), "ih-a")));
    registry.register_namespace(Box::new(util_namespace(Version::new(1, 2, 0), "ih-b")));

    let required = ContractRef::new(name("test.util"), 1);
    match registry.resolve_namespace(&required) {
        Err(ResolutionError::Ambiguous { candidates, .. }) => {
            assert_eq!(candidates.len(), 2);
        }
        _ => panic!("expected Ambiguous"),
    }
}

/// Byte-identical duplicate registrations are not an ambiguity: one distinct
/// descriptor satisfies the requirement.
#[test]
fn identical_duplicate_registration_resolves() {
    let mut registry = Registry::new();
    registry.register_namespace(Box::new(util_namespace(Version::new(1, 2, 0), "ih-same")));
    registry.register_namespace(Box::new(util_namespace(Version::new(1, 2, 0), "ih-same")));

    let required = ContractRef::new(name("test.util"), 1);
    assert!(registry.resolve_namespace(&required).is_ok());
}

/// `name@major` parses; a bare name without `@major` is rejected — a
/// `$requires` value must pin a compatible major (§16.2).
#[test]
fn contract_ref_requires_major() {
    let parsed = ContractRef::parse("test.util@1").expect("parses");
    assert_eq!(parsed.major(), 1);
    assert_eq!(parsed.name().as_str(), "test.util");
    assert!(ContractRef::parse("test.util").is_err());
}
