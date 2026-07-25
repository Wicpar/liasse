#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module lifecycle over the surface [`ModuleDeployment`]: a module installs
//! into a row-scoped module collection and its `$expose`d interface is readable through
//! the boundary; an interface aggregates across two installed instances; a
//! disabled instance leaves the aggregation but keeps its private state; enable
//! restores it; a duplicate name, empty name, and malformed binding are rejection
//! observations (not faults); and an update migrates a single instance.

use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest, InstallRequest};
use liasse_store::{CollectionPath, MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_surface::{
    Engine, ModuleDeployment, ModuleError, ModuleHost, ModuleObservation, ModuleUpdate,
    Precision, Value, VirtualClock,
};
use liasse_value::Text;

mod support;

const NOW: i128 = 1_700_000_000_000_000;

const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "example.root@1.0.0"
  "$model": { "flags": { "$key": "id", "id": "text" } }
}"#;

const TEMPLATES: &str = r#"{
  "$liasse": 1
  "$module": "example.templates@1.0.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text", "secret": "text" }
    "$mut": { "add": ".templates + { id: @id, label: @label, secret: @secret }" }
  }
  "$expose": { "templates": { "$view": ".templates { id, label }" } }
}"#;

/// A compatible successor adding a defaulted `pinned` field.
const TEMPLATES_V2: &str = r#"{
  "$liasse": 1
  "$module": "example.templates@1.1.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text", "secret": "text", "pinned": "bool = false" }
    "$mut": { "add": ".templates + { id: @id, label: @label, secret: @secret }" }
  }
  "$expose": { "templates": { "$view": ".templates { id, label }" } }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

/// The module collection the fixture mounts its instances in.
fn collection() -> CollectionPath {
    support::collection_at("/companies/acme/modules")
}

/// The address of the module-collection entry `name` — one mounted instance's
/// identity (§13.3), an ordinary row address.
fn at(name: &str) -> RowAddress {
    support::mount_at("/companies/acme/modules", name)
}

fn deployment() -> ModuleDeployment<MemoryStoreFactory> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let root = Engine::load(MemoryStore::new(InstanceId::new("root")), ROOT, &mut clock).expect("root loads");
    ModuleDeployment::new(ModuleHost::new(MemoryStoreFactory::new(), root), clock)
}

fn install(deployment: &mut ModuleDeployment<MemoryStoreFactory>, name: &str) {
    assert_eq!(
        deployment.install(&collection(), InstallRequest::new(name, TEMPLATES)).expect("install"),
        ModuleObservation::Applied,
    );
}

/// The `label`s the instance `name` exposes through its `templates` interface.
fn labels(deployment: &ModuleDeployment<MemoryStoreFactory>, name: &str) -> Vec<String> {
    let Ok(Some(result)) = deployment.interface_read(&at(name), "templates") else { return Vec::new() };
    result
        .rows()
        .iter()
        .filter_map(|row| match row.field("label") {
            Some(Value::Text(text)) => Some(text.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

fn add_template(deployment: &mut ModuleDeployment<MemoryStoreFactory>, at: &RowAddress, id: &str, label: &str) {
    let request = CallRequest::new("add").arg("id", text(id)).arg("label", text(label)).arg("secret", text("hush"));
    let outcome = deployment.child_call(at, &request).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

#[test]
fn install_exposes_a_readable_interface() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    add_template(&mut deployment, &at("sales"), "t1", "Invoice");

    let result = deployment.interface_read(&at("sales"), "templates").expect("read").expect("declared");
    assert_eq!(result.len(), 1);
    let row = &result.rows()[0];
    assert_eq!(row.field("label"), Some(&text("Invoice")));
    // §13.8 isolation: the private `secret` field does not cross the boundary.
    assert_eq!(row.field("secret"), None);
}

#[test]
fn disable_withdraws_the_boundary_and_enable_restores() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    install(&mut deployment, "support");
    add_template(&mut deployment, &at("sales"), "t1", "kept");
    add_template(&mut deployment, &at("support"), "u1", "other");
    assert_eq!(labels(&deployment, "sales"), vec!["kept".to_owned()]);

    assert_eq!(deployment.disable(&at("sales")).expect("disable"), ModuleObservation::Applied);
    assert!(!deployment.is_enabled(&at("sales")));
    assert_eq!(labels(&deployment, "support"), vec!["other".to_owned()], "a sibling is unaffected");
    match deployment.interface_read(&at("sales"), "templates") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no boundary read, got {other:?}"),
    }

    assert_eq!(deployment.enable(&at("sales")).expect("enable"), ModuleObservation::Applied);
    assert_eq!(labels(&deployment, "sales"), vec!["kept".to_owned()], "state survived disable/enable");
}

#[test]
fn duplicate_install_is_a_rejection_observation() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    assert_eq!(
        deployment.install(&collection(), InstallRequest::new("sales", TEMPLATES)).expect("observation, not a fault"),
        ModuleObservation::DuplicateName("sales".to_owned()),
    );
}

#[test]
fn empty_name_and_malformed_binding_are_rejection_observations() {
    let mut deployment = deployment();
    assert_eq!(
        deployment.install(&collection(), InstallRequest::new("", TEMPLATES)).expect("observation"),
        ModuleObservation::EmptyName,
    );
    let bad = InstallRequest::new("sales", TEMPLATES).use_handle("people", "acme.people/people");
    match deployment.install(&collection(), bad).expect("observation") {
        ModuleObservation::InvalidBinding(_) => {}
        other => panic!("a malformed binding is an observation, got {other:?}"),
    }
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    let incarnation = deployment.incarnation(&at("sales")).expect("installed").clone();
    add_template(&mut deployment, &at("sales"), "t1", "kept");

    assert_eq!(deployment.rename(&at("sales"), "revenue").expect("rename"), ModuleObservation::Applied);
    assert!(!deployment.is_installed(&at("sales")));
    assert_eq!(deployment.incarnation(&at("revenue")), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(labels(&deployment, "revenue").len(), 1, "the renamed entry holds the state");
}

#[test]
fn update_migrates_a_single_instance() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    add_template(&mut deployment, &at("sales"), "t1", "kept");

    match deployment.update(&at("sales"), TEMPLATES_V2).expect("update") {
        ModuleUpdate::Updated(_) => {}
        other => panic!("a compatible update migrates, got {other:?}"),
    }
    assert_eq!(labels(&deployment, "sales").len(), 1, "the template survived migration");
}

#[test]
fn uninstall_removes_instance() {
    let mut deployment = deployment();
    install(&mut deployment, "sales");
    assert_eq!(deployment.uninstall(&at("sales")).expect("uninstall"), ModuleObservation::Applied);
    assert!(!deployment.is_installed(&at("sales")));
    assert_eq!(
        deployment.uninstall(&at("sales")).expect("second uninstall observes unknown"),
        // The address, not the bare name: two collections may each hold a `sales`,
        // so naming only the instance would not say which one is absent.
        ModuleObservation::Unknown(at("sales").render()),
    );
}
