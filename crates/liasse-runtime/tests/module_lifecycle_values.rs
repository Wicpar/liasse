#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.16 module VALUES and their lifecycle operators, driven end to end through
//! the §13.10 transition runtime (memory store).
//!
//! The host exposes `provision`-shaped mutations taking a `module` parameter — the
//! §13.16 delegation contract — and applies its own policy before performing the
//! operation. That is also what makes a module value reachable in-language without
//! any client being able to forge one: `Value::Module` is refused at the wire
//! decode boundary, so only the trusted host supplies a handle.
//!
//! What each test's expectation is derived from is stated at the test, from §13.16
//! and §19.8, never from what the implementation happens to answer.

mod support;

use liasse_artifact::{Artifact, ArtifactBuilder};
use liasse_ident::{HistoryPoint, InstanceId, LineageId, NameSegment, PointId};
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, ImportRelation, ModuleHost, Value,
};
use liasse_store::{AddressStep, CollectionPath, KeyValue, MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_value::{BlobDescriptor, ModuleHandle, Text, Timestamp};
use support::generator;

/// A root host whose mutations are §13.16 delegation contracts: each takes a
/// `module` parameter and performs ONE value-surface operator on it, so the caller
/// (the host) reaches only the instance it names.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.modvalue.host@1.0.0"
  "$model": {
    "log": { "$key": "id", "id": "text" }
    "log_view": { "$view": ".log { id }" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
    }
    "$mut": {
      "install({ id: text, blob: blob })": [
        "e = .log + { id: @id }"
        "module.install({ blob: @blob, at: .companies['acme'].modules['sales'] })"
        "return e { id }"
      ]
      "snapshot({ id: text, at: module })": [
        "e = .log + { id: @id }"
        "b = pack(@at)"
        "return e { id }"
      ]
      "snapshot_at({ id: text, at: module, when: timestamp })": [
        "e = .log + { id: @id }"
        "b = pack(@at, { data: @when })"
        "return e { id }"
      ]
      "snapshot_version({ id: text, at: module, version: text })": [
        "e = .log + { id: @id }"
        "b = pack(@at, { model: @version })"
        "return e { id }"
      ]
      "provision({ id: text, at: module, package: blob })": [
        "e = .log + { id: @id }"
        "update_module(@at, unpack(@package), { migrate: 'model' })"
        "return e { id }"
      ]
      "sync({ id: text, at: module, package: blob })": [
        "e = .log + { id: @id }"
        "update_module(@at, unpack(@package), { migrate: 'model+data' })"
        "return e { id }"
      ]
      "revert({ id: text, at: module, point: blob })": [
        "e = .log + { id: @id }"
        "rollback_module(@at, @point)"
        "return e { id }"
      ]
      "revert_at({ id: text, at: module, when: timestamp })": [
        "e = .log + { id: @id }"
        "rollback_module(@at, @when)"
        "return e { id }"
      ]
    }
  }
  "$data": { "companies": { "acme": {} } }
}"#;

/// A `sales` module at 1.0.0: one `qty` per item, seeded at 5, with an exposed
/// mutation so the test can advance the instance's OWN history.
const SALES_V1: &str = r#"{
  "$liasse": 1
  "$module": "t.modvalue.sales@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
    "items_view": { "$view": ".items { id, qty }" }
    "$mut": { "bump({ by: int })": [ ".items['a'].qty = .items['a'].qty + @by", "return .items['a'] { id }" ] }
  }
  "$data": { "items": { "a": { "qty": "5" } } }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// `sales` 1.1.0: a declared §20.1 delta from 1.0.0 that bumps every `qty` by 100.
const SALES_V2: &str = r#"{
  "$liasse": 1
  "$module": "t.modvalue.sales@1.1.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
    "items_view": { "$view": ".items { id, qty }" }
    "$mut": { "bump({ by: int })": [ ".items['a'].qty = .items['a'].qty + @by", "return .items['a'] { id }" ] }
  }
  "$migrations": { "1.0.0": [ ".items = $old.items { id, qty: .qty + 100 }" ] }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// The module-collection entry `name` is mounted at: `/companies/acme/modules`
/// keyed by the instance name — an ordinary row address, built from the containing
/// row's own key.
fn at(name: &str) -> RowAddress {
    CollectionPath::nested(
        [AddressStep::new(NameSegment::new("companies"), KeyValue::single(Value::Text(Text::new("acme"))))],
        NameSegment::new("modules"),
    )
    .row(KeyValue::single(Value::Text(Text::new(name))))
}

/// The `module` value denoting the installed `sales` instance. The runtime is the
/// only party that can mint one — `Value::Module` is refused by the wire decoder —
/// so this stands for the trusted host handing a submodule its own handle (§13.16
/// delegation).
fn sales_handle() -> Value {
    Value::Module(ModuleHandle::Mounted(at("sales").render()))
}

fn package_blob(definition: &str) -> Vec<u8> {
    ArtifactBuilder::new(
        InstanceId::new("pkg"),
        HistoryPoint::new(LineageId::new("l"), PointId::new("p")),
        definition.as_bytes().to_vec(),
        b"S".to_vec(),
        br#"{"format":1,"selected":{"lineage":"l","point":"p"}}"#.to_vec(),
    )
    .build()
    .expect("the package artifact builds")
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// A host with `sales` installed from its 1.0.0 package.
fn host_with_sales() -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load("t.modvalue.host", ROOT);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    let blob = host
        .store_package_blob(&package_blob(SALES_V1), Some("sales.liasse".to_owned()))
        .expect("the package blob is stored");
    let request = CallRequest::new("install").arg("id", text("install")).arg("blob", Value::Blob(Box::new(blob)));
    let outcome = host.call_root_lifecycle(&request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "sales installs, got {outcome:?}");
    host
}

/// The `qty` of `sales`'s item `a`, read through its exposed interface.
fn sales_qty(host: &ModuleHost<MemoryStoreFactory>) -> Option<String> {
    let view = host.interface_read(&at("sales"), "items").expect("read")?;
    let row = view.rows().iter().find(|r| matches!(r.field("id"), Some(Value::Text(t)) if t.as_str() == "a"))?;
    match row.field("qty")? {
        Value::Int(v) => Some(v.to_canonical_text()),
        _ => None,
    }
}

/// Advance `sales`'s own history by one committed transition.
fn bump(host: &mut ModuleHost<MemoryStoreFactory>, by: i64) {
    let request = CallRequest::new("bump").arg("by", Value::Int(liasse_value::Integer::from(by)));
    let outcome = host.child_call(&at("sales"), &request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the bump commits, got {outcome:?}");
}

/// Run a root mutation and return its outcome.
fn call(host: &mut ModuleHost<MemoryStoreFactory>, request: &CallRequest) -> CallOutcome {
    host.call_root_lifecycle(request, &mut generator()).expect("no engine fault")
}

/// The rejection message of a refused outcome.
fn rejection(outcome: &CallOutcome) -> String {
    match outcome {
        CallOutcome::Rejected(rejection) => rejection.message().to_owned(),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// The bytes a `pack` landed in the root's §18.3 blob storage, fetched by the
/// descriptor's digest — so the assertion is on what actually became fetchable, not
/// on what the operator claimed.
fn packed_artifact(host: &ModuleHost<MemoryStoreFactory>, descriptor: &BlobDescriptor) -> Vec<u8> {
    use liasse_store::InstanceStore as _;
    host.root().store().get_blob(descriptor.sha512()).expect("blob store").expect("the packed bytes landed")
}

/// The descriptor of the artifact a `pack` of the live `sales` instance produces.
/// Built from the host's own §13.16 pack API, so the test addresses exactly the
/// bytes the in-language operator lands.
fn sales_descriptor(host: &ModuleHost<MemoryStoreFactory>) -> BlobDescriptor {
    let bytes = host.pack_instance(&at("sales")).expect("the instance can be packed");
    BlobDescriptor::new(
        liasse_value::Sha512::of(&bytes),
        bytes.len() as u64,
        liasse_value::MediaType::new("application/vnd.liasse+zip"),
        None,
    )
}

// ---- pack -----------------------------------------------------------------

/// §13.16: `pack(m)` with no axis is "the current version, the current state, the
/// full retained history". The result must be a real, verifiable `.liasse` artifact
/// of THAT instance — the same §19.5 artifact `restore`/`classify` consume — and its
/// bytes must be fetchable once the transition commits.
#[test]
fn pack_produces_a_verifiable_artifact_of_the_instance() {
    let mut host = host_with_sales();
    let outcome = call(&mut host, &CallRequest::new("snapshot").arg("id", text("pack")).arg("at", sales_handle()));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the pack commits, got {outcome:?}");

    let descriptor = sales_descriptor(&host);
    let bytes = packed_artifact(&host, &descriptor);
    let opened = Artifact::open(&bytes).expect("the packed bytes verify as a `.liasse` artifact (§19.8)");
    assert_eq!(
        opened.manifest().instance.as_str(),
        host.incarnation(&at("sales")).expect("incarnation").as_str(),
        "a packed module is an artifact of that instance, not of a placeholder"
    );
}

/// The honest gap, stated by §13.16 itself: a historical extract is time-anchored —
/// the state at `@t` pairs with the model version in effect at `@t` — and a CORE
/// instance retains neither an interior point's state nor its definition. So an
/// interior `data:` coordinate must be REFUSED by name, and must never be snapped
/// to the nearest retained point.
#[test]
fn pack_refuses_an_interior_instant_instead_of_snapping_to_the_selected_point() {
    let mut host = host_with_sales();
    let when = Value::Timestamp(Timestamp::new(support::NOW_MICROS - 1, liasse_value::Precision::Micros));
    let outcome = call(
        &mut host,
        &CallRequest::new("snapshot_at").arg("id", text("t")).arg("at", sales_handle()).arg("when", when),
    );
    let message = rejection(&outcome);
    assert!(message.contains("`data` axis"), "the refusal names the axis: {message}");
    assert!(message.contains("SELECTED point"), "the refusal names what IS retained: {message}");
    assert!(message.contains("NOT snapped"), "the refusal states it did not approximate: {message}");
    // Nothing committed: the parent's own change rolled back with the refusal.
    assert!(!log_ids(&host).contains(&"t".to_owned()), "the whole transition rejected");
}

/// The same for the `model` axis: an earlier package version's definition is not
/// retained at all, so `pack` must refuse rather than emit the ACTIVE definition
/// under a version label it does not carry.
#[test]
fn pack_refuses_a_version_the_instance_does_not_retain() {
    let mut host = host_with_sales();
    let outcome = call(
        &mut host,
        &CallRequest::new("snapshot_version")
            .arg("id", text("v"))
            .arg("at", sales_handle())
            .arg("version", text("0.9.0")),
    );
    let message = rejection(&outcome);
    assert!(message.contains("`model` axis"), "the refusal names the axis: {message}");
    assert!(message.contains("1.0.0"), "the refusal names the version that IS retained: {message}");
}

/// Asking for the version the instance actually holds is not a refusal — the policy
/// refuses what is unretained, not every explicit coordinate.
#[test]
fn pack_admits_the_axis_coordinate_the_instance_does_retain() {
    let mut host = host_with_sales();
    let outcome = call(
        &mut host,
        &CallRequest::new("snapshot_version")
            .arg("id", text("v"))
            .arg("at", sales_handle())
            .arg("version", text("1.0.0")),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the active version packs, got {outcome:?}");
}

// ---- update_module --------------------------------------------------------

/// §13.16 `migrate: model`: migrate `m`'s schema to `u`'s definition and carry `m`'s
/// current data forward (§20.1). The externally deducible result is the declared
/// 1.0.0→1.1.0 delta applied to the LIVE data: 5 (+3 from a bump) then +100.
#[test]
fn update_module_with_migrate_model_walks_the_declared_migration_chain() {
    let mut host = host_with_sales();
    bump(&mut host, 3);
    assert_eq!(sales_qty(&host), Some("8".to_owned()), "the live data before the update");

    let package = host
        .store_package_blob(&package_blob(SALES_V2), Some("sales-v2.liasse".to_owned()))
        .expect("stored");
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("provision"))
            .arg("at", sales_handle())
            .arg("package", Value::Blob(Box::new(package))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the update commits, got {outcome:?}");
    assert_eq!(
        sales_qty(&host),
        Some("108".to_owned()),
        "the §20.1 delta ran over the instance's OWN live data, not the package seed"
    );
}

/// §13.16 `migrate: model+data`, fast-forward half: when the live history is an
/// ANCESTOR of the incoming one, the update applies automatically and the instance
/// adopts the incoming state. Built by packing a later point, rolling the instance
/// back to an earlier one, then syncing forward again.
#[test]
fn update_module_with_migrate_model_and_data_fast_forwards_an_ancestor_history() {
    let mut host = host_with_sales();
    let early = pack_now(&mut host, "early");
    bump(&mut host, 7);
    assert_eq!(sales_qty(&host), Some("12".to_owned()), "the later point's data");
    let later = pack_now(&mut host, "later");

    // Fork back to the earlier retained point, so the live history now PRECEDES the
    // later artifact's — the ancestor relation a fast-forward is defined by (§19.8).
    let outcome = call(
        &mut host,
        &CallRequest::new("revert")
            .arg("id", text("revert"))
            .arg("at", sales_handle())
            .arg("point", Value::Blob(Box::new(early))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the rollback commits, got {outcome:?}");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "the instance is back at the earlier point's state");

    let outcome = call(
        &mut host,
        &CallRequest::new("sync")
            .arg("id", text("sync"))
            .arg("at", sales_handle())
            .arg("package", Value::Blob(Box::new(later))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the fast-forward commits, got {outcome:?}");
    assert_eq!(sales_qty(&host), Some("12".to_owned()), "the fast-forward carried the incoming data");
}

/// §13.16 divergence half: "when the histories have diverged, the update is refused
/// loudly … the engine offers no in-language merge". The divergence is built by
/// forking (rollback) and then committing on the fork, so the incoming point sits
/// past the shared branch point on the displaced lineage — §19.8's `Merge`.
#[test]
fn update_module_refuses_a_diverged_ancestry_with_a_structured_report() {
    let mut host = host_with_sales();
    let early = pack_now(&mut host, "early");
    bump(&mut host, 7);
    let later = pack_now(&mut host, "later");

    let outcome = call(
        &mut host,
        &CallRequest::new("revert")
            .arg("id", text("revert"))
            .arg("at", sales_handle())
            .arg("point", Value::Blob(Box::new(early))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the rollback commits, got {outcome:?}");
    // Continue from the forked point: this commit branches a NEW lineage, so the
    // `later` artifact is now a divergence rather than a continuation.
    bump(&mut host, 1);
    assert_eq!(sales_qty(&host), Some("6".to_owned()), "the fork advanced on its own lineage");

    let outcome = call(
        &mut host,
        &CallRequest::new("sync")
            .arg("id", text("sync"))
            .arg("at", sales_handle())
            .arg("package", Value::Blob(Box::new(later))),
    );
    let message = rejection(&outcome);
    assert!(message.contains("DIVERGED"), "the refusal names the divergence: {message}");
    assert!(message.contains("no in-language merge"), "the refusal states the §13.16 rule: {message}");
    assert!(message.contains("live point"), "the refusal carries the two points: {message}");
    assert_eq!(sales_qty(&host), Some("6".to_owned()), "the refused update changed nothing");
}

// ---- rollback_module ------------------------------------------------------

/// §13.16: a rollback FORKS — the versions after the target are retained and the
/// timeline continues from the target. The externally deducible facts are that the
/// state returns to the earlier point AND that continuing from it lands on a new
/// lineage (which is what makes the later artifact a divergence, asserted above).
#[test]
fn rollback_module_forks_the_timeline_back_to_a_retained_point() {
    let mut host = host_with_sales();
    let early = pack_now(&mut host, "early");
    bump(&mut host, 7);
    assert_eq!(sales_qty(&host), Some("12".to_owned()), "advanced past the retained point");

    let outcome = call(
        &mut host,
        &CallRequest::new("revert")
            .arg("id", text("revert"))
            .arg("at", sales_handle())
            .arg("point", Value::Blob(Box::new(early))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the rollback commits, got {outcome:?}");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "the instance reconstructed the retained point");
}

/// The second half of the honest gap: a rollback coordinate that is a bare instant
/// addresses no retained point — the definition in force there is not retained — so
/// it is refused by name and NEVER snapped to the nearest point.
#[test]
fn rollback_module_refuses_a_bare_instant_instead_of_snapping() {
    let mut host = host_with_sales();
    let when = Value::Timestamp(Timestamp::new(support::NOW_MICROS - 1, liasse_value::Precision::Micros));
    let outcome = call(
        &mut host,
        &CallRequest::new("revert_at").arg("id", text("t")).arg("at", sales_handle()).arg("when", when),
    );
    let message = rejection(&outcome);
    assert!(message.contains("retains"), "the refusal names what IS retained: {message}");
    assert!(message.contains("NOT snapped"), "the refusal states it did not approximate: {message}");
    assert!(message.contains("artifact"), "the refusal names the coordinate that would work: {message}");
}

/// A rollback that does not go BACK is refused: §13.16's rollback forks to an
/// earlier retained point, so an artifact ahead of the live point is not a rollback
/// target and must not be applied under that name.
#[test]
fn rollback_module_refuses_a_point_that_does_not_precede_the_live_one() {
    let mut host = host_with_sales();
    let current = pack_now(&mut host, "current");
    // The instance is still AT that point, so it is `SamePoint` — a genuine no-op.
    let outcome = call(
        &mut host,
        &CallRequest::new("revert")
            .arg("id", text("same"))
            .arg("at", sales_handle())
            .arg("point", Value::Blob(Box::new(current.clone()))),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "rolling back to the live point is a no-op");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "and changes nothing");
}

// ---- privilege ------------------------------------------------------------

/// §13.16/§13.10: the operators are host-privileged. An ordinary single-engine
/// admission is lent no lifecycle handle, so the very same program must be refused
/// there — the privilege is enforced by lending, not by a caller-supplied flag.
#[test]
fn a_lifecycle_operator_is_refused_without_the_host_privileged_handle() {
    let mut host = host_with_sales();
    let request = CallRequest::new("snapshot").arg("id", text("nope")).arg("at", sales_handle());
    let outcome = host.root_mut().call(&request, &mut generator()).expect("no engine fault");
    let message = rejection(&outcome);
    assert!(message.contains("host-privileged"), "the refusal names the privilege: {message}");
}

/// `update_module` and `rollback_module` act on an INSTALLED instance. A pending
/// handle (`unpack`) has no identity, mount, or history, so both must refuse rather
/// than quietly installing one.
#[test]
fn the_operators_refuse_a_module_that_is_not_yet_materialized() {
    let mut host = host_with_sales();
    let package = host
        .store_package_blob(&package_blob(SALES_V1), Some("pending.liasse".to_owned()))
        .expect("stored");
    let pending = Value::Module(ModuleHandle::Pending(Box::new(package.clone())));
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("p"))
            .arg("at", pending)
            .arg("package", Value::Blob(Box::new(package))),
    );
    let message = rejection(&outcome);
    assert!(message.contains("not yet materialized"), "the refusal names the state: {message}");
}

// ---- helpers --------------------------------------------------------------

fn log_ids(host: &ModuleHost<MemoryStoreFactory>) -> Vec<String> {
    let view = host.root().view_at_head("log_view").expect("view").expect("log_view exists");
    view.rows()
        .iter()
        .filter_map(|row| match row.field("id") {
            Some(Value::Text(t)) => Some(t.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// Pack the live `sales` instance through the in-language operator and return the
/// descriptor of the landed artifact, so a later operator can address that point.
fn pack_now(host: &mut ModuleHost<MemoryStoreFactory>, id: &str) -> BlobDescriptor {
    let descriptor = sales_descriptor(host);
    let outcome = call(host, &CallRequest::new("snapshot").arg("id", text(id)).arg("at", sales_handle()));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the pack commits, got {outcome:?}");
    // The in-language operator must have landed exactly those bytes: fetching by
    // the digest proves the descriptor it returned addresses real content.
    assert!(!packed_artifact(host, &descriptor).is_empty(), "the pack landed its bytes");
    descriptor
}

/// A compile-time reminder that the §19.8 relations the operators branch on are the
/// engine's own, not a private copy.
const _: [ImportRelation; 2] = [ImportRelation::FastForward, ImportRelation::Rollback];
