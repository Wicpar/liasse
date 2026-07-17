#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module lifecycle over the surface [`ModuleDeployment`]: a module installs
//! into a row-scoped module space and its `$expose`d interface is readable through
//! the boundary; an interface aggregates across two installed instances; a
//! disabled instance leaves the aggregation but keeps its private state; enable
//! restores it; a duplicate name, empty name, and malformed binding are rejection
//! observations (not faults); and an update migrates a single instance.

use liasse_ident::InstanceId;
use liasse_runtime::{CallOutcome, CallRequest, InstallRequest};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_surface::{
    Engine, ModuleDeployment, ModuleError, ModuleHost, ModuleObservation, ModuleSpace, ModuleUpdate,
    Precision, Value, VirtualClock,
};
use liasse_value::Text;

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

fn space() -> ModuleSpace {
    ModuleSpace::new("/companies/acme/modules").expect("mount path")
}

fn deployment() -> ModuleDeployment<MemoryStoreFactory> {
    let mut clock = VirtualClock::new(NOW, Precision::Micros);
    let root = Engine::load(MemoryStore::new(InstanceId::new("root")), ROOT, &mut clock).expect("root loads");
    ModuleDeployment::new(ModuleHost::new(MemoryStoreFactory::new(), root), clock)
}

fn install(deployment: &mut ModuleDeployment<MemoryStoreFactory>, space: &ModuleSpace, name: &str) {
    assert_eq!(
        deployment.install(space, InstallRequest::new(name, TEMPLATES)).expect("install"),
        ModuleObservation::Applied,
    );
}

fn add_template(deployment: &mut ModuleDeployment<MemoryStoreFactory>, space: &ModuleSpace, name: &str, id: &str, label: &str) {
    let request = CallRequest::new("add").arg("id", text(id)).arg("label", text(label)).arg("secret", text("hush"));
    let outcome = deployment.child_call(space, name, &request).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

#[test]
fn install_exposes_a_readable_interface() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    add_template(&mut deployment, &space, "sales", "t1", "Invoice");

    let result = deployment.interface_read(&space, "sales", "templates").expect("read").expect("declared");
    assert_eq!(result.len(), 1);
    let row = &result.rows()[0];
    assert_eq!(row.field("label"), Some(&text("Invoice")));
    // §13.8 isolation: the private `secret` field does not cross the boundary.
    assert_eq!(row.field("secret"), None);
}

#[test]
fn disable_leaves_aggregation_and_enable_restores() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    install(&mut deployment, &space, "support");
    add_template(&mut deployment, &space, "sales", "t1", "kept");
    add_template(&mut deployment, &space, "support", "u1", "other");
    assert_eq!(deployment.aggregate(&space, "templates").expect("agg").len(), 2);

    assert_eq!(deployment.disable(&space, "sales").expect("disable"), ModuleObservation::Applied);
    assert!(!deployment.is_enabled(&space, "sales"));
    assert_eq!(deployment.aggregate(&space, "templates").expect("agg").len(), 1, "disabled leaves the aggregation");
    match deployment.interface_read(&space, "sales", "templates") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no boundary read, got {other:?}"),
    }

    assert_eq!(deployment.enable(&space, "sales").expect("enable"), ModuleObservation::Applied);
    assert_eq!(deployment.aggregate(&space, "templates").expect("agg").len(), 2, "state survived disable/enable");
}

#[test]
fn duplicate_install_is_a_rejection_observation() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    assert_eq!(
        deployment.install(&space, InstallRequest::new("sales", TEMPLATES)).expect("observation, not a fault"),
        ModuleObservation::DuplicateName("sales".to_owned()),
    );
}

#[test]
fn empty_name_and_malformed_binding_are_rejection_observations() {
    let (mut deployment, space) = (deployment(), space());
    assert_eq!(
        deployment.install(&space, InstallRequest::new("", TEMPLATES)).expect("observation"),
        ModuleObservation::EmptyName,
    );
    let bad = InstallRequest::new("sales", TEMPLATES).use_handle("people", "not-a-spec");
    match deployment.install(&space, bad).expect("observation") {
        ModuleObservation::InvalidBinding(_) => {}
        other => panic!("a malformed binding is an observation, got {other:?}"),
    }
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    let incarnation = deployment.incarnation(&space, "sales").expect("installed").clone();
    add_template(&mut deployment, &space, "sales", "t1", "kept");

    assert_eq!(deployment.rename(&space, "sales", "revenue").expect("rename"), ModuleObservation::Applied);
    assert!(!deployment.is_installed(&space, "sales"));
    assert_eq!(deployment.incarnation(&space, "revenue"), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(deployment.aggregate(&space, "templates").expect("agg")[0].instance(), "revenue");
}

#[test]
fn update_migrates_a_single_instance() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    add_template(&mut deployment, &space, "sales", "t1", "kept");

    match deployment.update(&space, "sales", TEMPLATES_V2).expect("update") {
        ModuleUpdate::Updated(_) => {}
        other => panic!("a compatible update migrates, got {other:?}"),
    }
    assert_eq!(deployment.aggregate(&space, "templates").expect("agg").len(), 1, "the template survived migration");
}

#[test]
fn uninstall_removes_instance() {
    let (mut deployment, space) = (deployment(), space());
    install(&mut deployment, &space, "sales");
    assert_eq!(deployment.uninstall(&space, "sales").expect("uninstall"), ModuleObservation::Applied);
    assert!(!deployment.is_installed(&space, "sales"));
    assert_eq!(
        deployment.uninstall(&space, "sales").expect("second uninstall observes unknown"),
        ModuleObservation::Unknown("sales".to_owned()),
    );
}
