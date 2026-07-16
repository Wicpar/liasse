//! D.1/D.5 identity: incarnation stability across rekey and rebind, and the
//! distinctness of unrelated lineages and aliased point-ids.

use liasse_ident::{
    CanonicalPath, HistoryPoint, IdentError, InstanceId, InstanceIdentity, KeyText, LineageId,
    NameSegment, PathSegment, PointId, RowIdentity, RowIncarnation,
};
use liasse_value::{Text, Value};

type Fallible = Result<(), Box<dyn std::error::Error>>;

fn address(name: &str, k: &str) -> Result<CanonicalPath, IdentError> {
    Ok(CanonicalPath::new([
        PathSegment::Name(NameSegment::new(name)),
        PathSegment::Key(KeyText::from_key_values(&[Value::Text(Text::new(k))])?),
    ]))
}

#[test]
fn rekey_preserves_row_identity() -> Fallible {
    // D.1/§5.4: an atomic rekey keeps the incarnation, so the row's durable
    // identity survives the address change.
    let original = RowIdentity::new(address("users", "alice")?, RowIncarnation::new("row-1"));
    let before = original.clone();
    let rekeyed = original.rekey(address("users", "alice2")?);

    assert_eq!(rekeyed, before, "rekey preserves durable identity");
    assert_eq!(rekeyed.incarnation(), before.incarnation());
    assert_ne!(
        rekeyed.address().to_display_string(),
        before.address().to_display_string(),
        "the visible address did change"
    );
    Ok(())
}

#[test]
fn distinct_incarnations_are_distinct_rows() -> Fallible {
    // D.1: delete-then-insert allocates a new incarnation, so a row reusing the
    // same key is a different durable row.
    let first = RowIdentity::new(address("users", "alice")?, RowIncarnation::new("row-1"));
    let reinserted = RowIdentity::new(address("users", "alice")?, RowIncarnation::new("row-2"));
    assert_ne!(first, reinserted);
    Ok(())
}

#[test]
fn rebind_preserves_instance_identity() {
    // D.1: renaming/rebinding an instance preserves its incarnation.
    let instance = InstanceIdentity::new("billing", InstanceId::new("inst-1"));
    let before = instance.clone();
    let rebound = instance.rebind("invoicing");
    assert_eq!(rebound, before);
    assert_eq!(rebound.label(), "invoicing");
    assert_eq!(before.label(), "billing");
}

#[test]
fn point_id_aliases_across_lineages_stay_distinct() {
    // SPEC-ISSUES item 21 / D.5: the same point token in two unrelated lineages
    // is not one point.
    let shared_point = PointId::new("p-7");
    let here = HistoryPoint::new(LineageId::new("lineage-a"), shared_point.clone());
    let elsewhere = HistoryPoint::new(LineageId::new("lineage-b"), shared_point);
    assert_ne!(here, elsewhere);
    assert_eq!(here.point(), elsewhere.point());
}

#[test]
fn opaque_id_canonical_text_is_the_token() {
    let id = LineageId::new("lineage-42");
    assert_eq!(id.to_canonical_text(), "lineage-42");
    assert_eq!(id.as_str(), "lineage-42");
}
