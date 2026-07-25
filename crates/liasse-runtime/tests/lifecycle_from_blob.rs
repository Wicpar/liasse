#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.10 module lifecycle from a blob — install / update / remove folded into the
//! parent's own transition and committed atomically (memory store).
//!
//! A ROOT host-privileged mutation makes its OWN state change AND carries a module
//! instance through its lifecycle (`module.install`/`update`/`remove`), decoding the
//! package from a `.liasse` blob. The mounted / migrated / removed instance is one of
//! the engines the coordinator commits together, so the lifecycle change and the
//! parent's change land as ONE atomic transition — both persist, or the whole
//! transition rejects and every instance stays at its prior committed state.

mod support;

use liasse_artifact::ArtifactBuilder;
use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};
use liasse_runtime::{
    CallOutcome, CallRequest, Engine, ModuleHost, Value,
};
use liasse_store::{MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_value::{BlobDescriptor, Text};
use support::generator;

/// A root host whose `provision` mutation inserts its own `log` row and then installs
/// a module from a blob, and whose `revise`/`retire` mutations update / remove one —
/// each in the SAME transition as its own `log` change.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.host@1.0.0"
  "$model": {
    "log": { "$key": "id", "id": "text" }
    "log_view": { "$view": ".log { id }" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
    }
    "$mut": {
      "provision({ id: text, blob: blob })": [
        "e = .log + { id: @id }"
        "p = module.install({ blob: @blob, at: .companies['acme'].modules['sales'] })"
        "return e { id }"
      ]
      "revise({ id: text, blob: blob })": [
        "e = .log + { id: @id }"
        "module.update({ blob: @blob, module: .companies['acme'].modules['sales'].$value })"
        "return e { id }"
      ]
      "retire({ id: text })": [
        "e = .log + { id: @id }"
        "module.remove({ module: .companies['acme'].modules['sales'].$value })"
        "return e { id }"
      ]
    }
  }
  "$data": { "companies": { "acme": {} } }
}"#;

/// A `sales` module at version 1.0.0: one `qty` per item, seeded at 5, exposed.
const SALES_V1: &str = r#"{
  "$liasse": 1
  "$module": "t.blob.sales@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
    "items_view": { "$view": ".items { id, qty }" }
  }
  "$data": { "items": { "a": { "qty": "5" } } }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// `sales` version 1.1.0: a declared §20.1 delta from 1.0.0 that bumps every item's
/// `qty` by 100. The exposed interface is unchanged, so the update is non-narrowing.
const SALES_V2: &str = r#"{
  "$liasse": 1
  "$module": "t.blob.sales@1.1.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
    "items_view": { "$view": ".items { id, qty }" }
  }
  "$migrations": { "1.0.0": [ ".items = $old.items { id, qty: .qty + 100 }" ] }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// `sales` version 1.1.0 whose migration would drive `qty` past a `$check` bound, so
/// the migration is REJECTED and the instance must stay at its prior version.
const SALES_V2_BAD: &str = r#"{
  "$liasse": 1
  "$module": "t.blob.sales@1.1.0"
  "$model": {
    "items": {
      "$key": "id"
      "id": "text"
      "qty": { "$type": "int", "$check": ["(. < 100)", "qty too large"] }
    }
    "items_view": { "$view": ".items { id, qty }" }
  }
  "$migrations": { "1.0.0": [ ".items = $old.items { id, qty: .qty + 200 }" ] }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// A root whose `provision` mutation calls `module.install` with an UNSUPPORTED
/// `config` member alongside the supported `{ blob, space, name }`. The builtin
/// does not apply a `config`/`$data` overlay, so it must refuse rather than
/// silently ignore the member.
const ROOT_UNKNOWN_ARG: &str = r#"{
  "$liasse": 1
  "$app": "t.blob.host.badarg@1.0.0"
  "$model": {
    "log": { "$key": "id", "id": "text" }
    "log_view": { "$view": ".log { id }" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
    }
    "$mut": {
      "provision({ id: text, blob: blob })": [
        "e = .log + { id: @id }"
        "module.install({ blob: @blob, at: .companies['acme'].modules['sales'], config: 'x' })"
        "return e { id }"
      ]
    }
  }
  "$data": { "companies": { "acme": {} } }
}"#;

/// The address of the module-collection entry `name` — one mounted instance's
/// identity (§13.3), an ordinary row address.
fn at(name: &str) -> RowAddress {
    support::mount_at("/companies/acme/modules", name)
}

/// Serialize a module definition into a minimal `.liasse` blob (empty state/history
/// sections — opaque to the artifact layer, which only checksums them).
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

fn host() -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load("t.blob.host", ROOT);
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// Store `definition` as a package blob and return the descriptor to pass as `@blob`.
fn blob_of(host: &mut ModuleHost<MemoryStoreFactory>, definition: &str) -> BlobDescriptor {
    host.store_package_blob(&package_blob(definition), Some("sales.liasse".to_owned()))
        .expect("the package blob is stored")
}

fn root_log_ids(host: &ModuleHost<MemoryStoreFactory>) -> Vec<String> {
    let view = host.root().view_at_head("log_view").expect("view").expect("log_view exists");
    view.rows()
        .iter()
        .map(|row| match row.field("id").expect("id") {
            Value::Text(t) => t.as_str().to_owned(),
            other => panic!("id is not text: {other:?}"),
        })
        .collect()
}

/// The `qty` of the `sales` instance's item `a`, read through its exposed interface,
/// as its canonical decimal text.
fn sales_qty(host: &ModuleHost<MemoryStoreFactory>) -> Option<String> {
    let view = host.interface_read(&at("sales"), "items").expect("read")?;
    let row = view.rows().iter().find(|r| matches!(r.field("id"), Some(Value::Text(t)) if t.as_str() == "a"))?;
    match row.field("qty")? {
        Value::Int(v) => Some(v.to_canonical_text()),
        _ => None,
    }
}

#[test]
fn install_from_blob_commits_parent_change_and_new_instance_atomically() {
    let mut host = host();
    let blob = blob_of(&mut host, SALES_V1);

    assert!(root_log_ids(&host).is_empty(), "no log row before");
    assert!(!host.is_installed(&at("sales")), "sales not installed before");

    let request = CallRequest::new("provision").arg("id", text("first")).arg("blob", Value::Blob(Box::new(blob)));
    let outcome = host.call_root_lifecycle(&request, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the transition commits, got {outcome:?}");

    // BOTH persist: the parent's own log row AND the freshly-installed instance, as
    // one atomic transition — a fresh read sees both.
    assert_eq!(root_log_ids(&host), vec!["first".to_owned()], "the parent's own change committed");
    assert!(host.is_enabled(&at("sales")), "the module instance mounted");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "the installed instance carries its package seed");
}

#[test]
fn a_malformed_install_blob_rolls_back_the_whole_transition() {
    let mut host = host();
    // A blob that is not a readable `.liasse` package.
    let garbage = host.store_package_blob(b"not a package at all", None).expect("store");

    let request = CallRequest::new("provision").arg("id", text("first")).arg("blob", Value::Blob(Box::new(garbage)));
    let outcome = host.call_root_lifecycle(&request, &mut generator()).expect("no engine fault");

    assert!(matches!(outcome, CallOutcome::Rejected(_)), "a malformed package rejects, got {outcome:?}");
    // No half-mounted instance, and the parent's own change did NOT commit.
    assert!(!host.is_installed(&at("sales")), "no instance was half-mounted");
    assert!(root_log_ids(&host).is_empty(), "the parent's own change rolled back");
}

#[test]
fn update_from_blob_walks_the_migration_chain_atomically() {
    let mut host = host();
    let v1 = blob_of(&mut host, SALES_V1);
    let install = CallRequest::new("provision").arg("id", text("install")).arg("blob", Value::Blob(Box::new(v1)));
    host.call_root_lifecycle(&install, &mut generator()).expect("install");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "starts at the v1 seed");

    // A GOOD update: the declared 1.0.0 delta bumps qty by 100, atomically with the
    // parent's own log change.
    let v2 = blob_of(&mut host, SALES_V2);
    let revise = CallRequest::new("revise").arg("id", text("revise")).arg("blob", Value::Blob(Box::new(v2)));
    let outcome = host.call_root_lifecycle(&revise, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the update commits, got {outcome:?}");
    assert_eq!(sales_qty(&host), Some("105".to_owned()), "the migration chain walked and transformed the data");
    assert_eq!(
        root_log_ids(&host),
        vec!["install".to_owned(), "revise".to_owned()],
        "the parent's own `revise` change committed alongside the install's earlier row"
    );
}

#[test]
fn a_failed_migration_leaves_the_instance_at_its_prior_version() {
    let mut host = host();
    let v1 = blob_of(&mut host, SALES_V1);
    let install = CallRequest::new("provision").arg("id", text("install")).arg("blob", Value::Blob(Box::new(v1)));
    host.call_root_lifecycle(&install, &mut generator()).expect("install");

    // A bad update: the migration drives qty past its `$check`, so it is rejected.
    let bad = blob_of(&mut host, SALES_V2_BAD);
    let revise = CallRequest::new("revise").arg("id", text("revise")).arg("blob", Value::Blob(Box::new(bad)));
    let outcome = host.call_root_lifecycle(&revise, &mut generator()).expect("no engine fault");

    assert!(matches!(outcome, CallOutcome::Rejected(_)), "the failed migration rejects, got {outcome:?}");
    assert_eq!(sales_qty(&host), Some("5".to_owned()), "the instance stays at its prior version and data");
    assert_eq!(
        root_log_ids(&host),
        vec!["install".to_owned()],
        "the failed `revise` change rolled back; only the earlier install's row persists"
    );
}

#[test]
fn the_decoded_package_identity_is_recorded_and_reproduced() {
    use liasse_artifact::decode_package_from_blob;

    let mut host = host();
    let package_bytes = package_blob(SALES_V1);
    let expected_definition = *decode_package_from_blob(&package_bytes).expect("decode").definition_id();
    let blob = host.store_package_blob(&package_bytes, Some("sales.liasse".to_owned())).expect("store");
    let expected_content = *blob.sha512();

    let request = CallRequest::new("provision").arg("id", text("first")).arg("blob", Value::Blob(Box::new(blob)));
    host.call_root_lifecycle(&request, &mut generator()).expect("install");

    // The decoded package identity is a DURABLE fact of the commit: the parent's
    // composition pins the sales mount to the blob content id, the D.4 definition id,
    // and the version (§5.1) — read back from committed state, not re-derived.
    let composition = host.durable_composition().expect("read composition").expect("a composition was recorded");
    // §19.5: the mount key IS the entry's own row address, so the composition is
    // keyed by ordinary collection addressing rather than a separate mount path.
    let mount = composition.mount(&at("sales").render()).expect("the sales mount is recorded");
    let pin = mount.package().expect("the mount carries package provenance");
    assert_eq!(pin.content(), &expected_content, "the mount pins the decoded blob content id");
    assert_eq!(pin.definition(), &expected_definition, "the mount pins the D.4 definition id");
    assert_eq!(pin.version(), [1, 0, 0], "the mount pins the package version");

    // Reproduced on replay / in audit: re-reading the committed fact yields the
    // identical pin — it is stored and reused verbatim, never re-generated (§5.1).
    let again = host.durable_composition().expect("re-read").expect("still recorded");
    assert_eq!(
        again.mount(&at("sales").render()).and_then(liasse_store::Mount::package),
        Some(pin),
        "the recorded provenance is reproduced verbatim on a fresh read"
    );

    // The in-memory audit accessor agrees with the durable fact.
    let mounted = host.mounted_package(&at("sales")).expect("mounted package");
    assert_eq!(mounted.definition, expected_definition);
    assert_eq!(mounted.content, expected_content);
}

#[test]
fn an_unsupported_lifecycle_argument_is_refused_loudly() {
    let root: Engine<MemoryStore> = support::load("t.blob.host.badarg", ROOT_UNKNOWN_ARG);
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    let blob = blob_of(&mut host, SALES_V1);

    let request = CallRequest::new("provision").arg("id", text("first")).arg("blob", Value::Blob(Box::new(blob)));
    let outcome = host.call_root_lifecycle(&request, &mut generator()).expect("no engine fault");

    match outcome {
        CallOutcome::Rejected(rejection) => assert!(
            rejection.message().contains("config") && rejection.message().contains("does not support"),
            "the refusal names the unsupported `config` member: {}",
            rejection.message()
        ),
        other => panic!("expected a loud refusal of the unsupported `config` argument, got {other:?}"),
    }

    // The unsupported argument aborted the whole transition: nothing half-applied.
    assert!(!host.is_installed(&at("sales")), "no instance mounted when the arg is refused");
    assert!(root_log_ids(&host).is_empty(), "the parent's own change rolled back");
}

#[test]
fn remove_within_a_transition_commits_atomically() {
    let mut host = host();
    let v1 = blob_of(&mut host, SALES_V1);
    let install = CallRequest::new("provision").arg("id", text("install")).arg("blob", Value::Blob(Box::new(v1)));
    host.call_root_lifecycle(&install, &mut generator()).expect("install");
    assert!(host.is_installed(&at("sales")), "installed before removal");

    let retire = CallRequest::new("retire").arg("id", text("retire"));
    let outcome = host.call_root_lifecycle(&retire, &mut generator()).expect("no engine fault");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the removal commits, got {outcome:?}");

    assert!(!host.is_installed(&at("sales")), "the instance was removed");
    assert_eq!(root_log_ids(&host), vec!["install".to_owned(), "retire".to_owned()], "the parent's own change committed");
}
