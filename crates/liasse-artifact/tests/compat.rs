//! The Annex E version-relationship matrix, re-derived from the E.1 rule text
//! (SPEC.md Annex E).

use liasse_artifact::{
    CompatibilityDecision, ContractRule, PackageIdentity, UpdateRelation,
};

type Fallible = Result<(), Box<dyn std::error::Error>>;

fn classify(from: &str, to: &str) -> Result<CompatibilityDecision, Box<dyn std::error::Error>> {
    let active = PackageIdentity::parse(from)?;
    let candidate = PackageIdentity::parse(to)?;
    Ok(CompatibilityDecision::classify(&active, &candidate))
}

#[test]
fn same_version_is_same_line() -> Fallible {
    let d = classify("v.app@1.2.3", "v.app@1.2.3")?;
    assert_eq!(d.relation, UpdateRelation::SameVersion);
    assert_eq!(d.rule, ContractRule::SameLine);
    assert!(!d.is_line_forward());
    Ok(())
}

#[test]
fn patch_bump_must_preserve_or_widen() -> Fallible {
    // E.1: patch preserves the same boundary contracts.
    let d = classify("v.app@1.2.3", "v.app@1.2.4")?;
    assert_eq!(d.relation, UpdateRelation::Patch);
    assert_eq!(d.rule, ContractRule::MustPreserveOrWiden);
    assert!(d.is_line_forward());
    Ok(())
}

#[test]
fn minor_bump_must_preserve_or_widen() -> Fallible {
    // E.1: minor may add or widen compatible boundary contracts.
    let d = classify("v.app@1.2.3", "v.app@1.3.0")?;
    assert_eq!(d.relation, UpdateRelation::Minor);
    assert_eq!(d.rule, ContractRule::MustPreserveOrWiden);
    assert!(d.is_line_forward());
    Ok(())
}

#[test]
fn major_bump_may_break() -> Fallible {
    // E.1: major may change or remove boundary contracts.
    let d = classify("v.app@1.9.9", "v.app@2.0.0")?;
    assert_eq!(d.relation, UpdateRelation::Major);
    assert_eq!(d.rule, ContractRule::MayBreak);
    assert!(!d.is_line_forward());
    Ok(())
}

#[test]
fn lower_version_is_downgrade() -> Fallible {
    // §20.2: a downgrade needs an explicit transform or exact inverses.
    let d = classify("v.app@2.0.0", "v.app@1.9.9")?;
    assert_eq!(d.relation, UpdateRelation::Downgrade);
    assert_eq!(d.rule, ContractRule::RequiresDowngradeTransform);
    Ok(())
}

#[test]
fn patch_downgrade_is_downgrade() -> Fallible {
    let d = classify("v.app@1.2.4", "v.app@1.2.3")?;
    assert_eq!(d.relation, UpdateRelation::Downgrade);
    Ok(())
}

#[test]
fn different_name_is_unrelated() -> Fallible {
    // A different compatibility line: versions are not comparable (§19.8).
    let d = classify("v.app@1.0.0", "v.other@1.0.0")?;
    assert_eq!(d.relation, UpdateRelation::Unrelated);
    assert_eq!(d.rule, ContractRule::Unrelated);
    Ok(())
}

#[test]
fn prerelease_version_is_rejected() {
    // SPEC-ISSUES item 26: prerelease/build metadata is unspecified; the strict
    // three-component grammar rejects it.
    assert!(PackageIdentity::parse("v.app@1.0.0-rc1").is_err());
}

#[test]
fn identity_round_trips_through_canonical_text() -> Fallible {
    let id = PackageIdentity::parse("vendor.app@3.4.5")?;
    assert_eq!(id.to_canonical_text(), "vendor.app@3.4.5");
    Ok(())
}
