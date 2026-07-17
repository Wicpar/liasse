#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.8–§13.10 module composition made visible to the **root** engine: a root
//! package's `.modules::iface` view aggregates the installed children through the
//! boundary; a root-level and a company-nested module space both resolve; a
//! private child field never crosses the interface (isolation); and an
//! interface-addressed mutation routes to a child's exposed mutation and commits.
//!
//! Unlike `modules.rs` — which drives the [`ModuleHost`] boundary API directly —
//! these exercise the seam this increment closes: evaluating a root-package view
//! whose expression addresses `.modules::iface`, folding the installed instances
//! into the root engine's own evaluation.

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleHost, ModuleSpace, Value, ViewQuery,
    ViewResult,
};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_value::Text;
use support::generator;

/// A root package with a **root-level** module space `modules` declaring a
/// `templates` interface, plus a `catalog` view that aggregates it across every
/// installed instance (§13.9), and a public surface over the catalog.
const ROOT_LEVEL: &str = r#"{
  "$liasse": 1
  "$app": "t.mod.host@1.0.0"
  "$model": {
    "modules": {
      "$modules": {
        "$interfaces": {
          "templates": { "$view": { "$key": "id", "id": "text", "label": "text" } }
        }
      }
    }
    "catalog": {
      "$view": ".modules::templates { module: modules.$key, id, label, $sort: [module, id] }"
    }
  }
}"#;

/// A root package with a **company-nested** module space (`companies.*.modules`)
/// and a nested `catalog` view read through a public surface — the canonical §13.9
/// corpus shape.
const NESTED: &str = r#"{
  "$liasse": 1
  "$app": "t.mod.host@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "modules": {
        "$modules": {
          "$interfaces": {
            "templates": { "$view": { "$key": "id", "id": "text", "label": "text" } }
          }
        }
      }
      "catalog": {
        "$view": ".modules::templates { module: modules.$key, id, label, $sort: [module, id] }"
      }
    }
    "$public": {
      "catalog": {
        "$params": { "company": "text" }
        "$view": "/companies[@company].catalog"
      }
    }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
}"#;

/// A child module: private `templates` with a `secret` field the `$expose` view
/// omits, and a `create_template` mutation bound to the `create` interface
/// contract (§13.8/§13.10).
const CHILD: &str = r#"{
  "$liasse": 1
  "$module": "t.tpl@1.0.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text", "secret": "text = ''" }
    "$mut": {
      "create_template": [
        "t = .templates + { id: @id, label: @label, secret: @secret }"
        "return t { id, label }"
      ]
    }
  }
  "$expose": {
    "templates": {
      "$view": ".templates { id, label }"
      "$mut": { "create": ".create_template" }
    }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn host(definition: &str) -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("root")), definition, &mut generator)
        .expect("root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn install(host: &mut ModuleHost<MemoryStoreFactory>, space: &ModuleSpace, name: &str) {
    host.install(space, InstallRequest::new(name, CHILD), &mut generator()).expect("install");
}

fn create(host: &mut ModuleHost<MemoryStoreFactory>, space: &ModuleSpace, name: &str, id: &str, label: &str, secret: &str) {
    let request = CallRequest::new("create_template")
        .arg("id", text(id))
        .arg("label", text(label))
        .arg("secret", text(secret));
    match host.interface_call(space, name, "templates", "create", &request, &mut generator()) {
        Ok(CallOutcome::Committed { .. }) => {}
        other => panic!("interface_call must commit, got {other:?}"),
    }
}

fn rows(result: &ViewResult) -> Vec<(String, String, String)> {
    result
        .rows()
        .iter()
        .map(|row| {
            let get = |name: &str| match row.field(name) {
                Some(Value::Text(t)) => t.as_str().to_owned(),
                other => panic!("expected text field `{name}`, got {other:?}"),
            };
            (get("module"), get("id"), get("label"))
        })
        .collect()
}

#[test]
fn root_view_aggregates_two_installed_children() {
    // §13.9: a root view over `.modules::templates` reads every enabled instance
    // exposing the interface; inherited identity is the instance name plus the
    // exposed row, so the projection carries `modules.$key` and the child fields.
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    install(&mut host, &space, "kit_a");
    install(&mut host, &space, "kit_b");
    create(&mut host, &space, "kit_a", "a2", "Zeta", "x");
    create(&mut host, &space, "kit_a", "z1", "Alpha", "x");
    create(&mut host, &space, "kit_b", "a1", "Mid", "x");

    let result = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(
        rows(&result),
        vec![
            ("kit_a".to_owned(), "a2".to_owned(), "Zeta".to_owned()),
            ("kit_a".to_owned(), "z1".to_owned(), "Alpha".to_owned()),
            ("kit_b".to_owned(), "a1".to_owned(), "Mid".to_owned()),
        ],
        "the aggregation orders by [module, id] across both instances"
    );
}

#[test]
fn root_view_over_company_nested_module_space_resolves() {
    // The canonical §13.9 corpus shape: `.modules::templates` nested under a
    // company row, read through a `$params`-bound public surface.
    let space = ModuleSpace::new("/companies/acme/modules").expect("mount");
    let mut host = host(NESTED);
    install(&mut host, &space, "kit_a");
    install(&mut host, &space, "kit_b");
    create(&mut host, &space, "kit_a", "a2", "Zeta", "x");
    create(&mut host, &space, "kit_b", "a1", "Mid", "x");

    let query = ViewQuery::new().param("company", text("acme"));
    let result = host.root_view("public.catalog", &query).expect("view").expect("declared");
    assert_eq!(
        rows(&result),
        vec![
            ("kit_a".to_owned(), "a2".to_owned(), "Zeta".to_owned()),
            ("kit_b".to_owned(), "a1".to_owned(), "Mid".to_owned()),
        ],
        "the company-scoped module space aggregates its own installed children"
    );
}

#[test]
fn installation_data_overlay_seeds_child_rows_visible_in_the_aggregation() {
    // §13.3: the installation `$data` overlays onto the child genesis; the seeded
    // rows then appear in the parent's `.modules::templates` aggregation. This is
    // the shape the §13.9 corpus cases seed their children with (no child mutation
    // required).
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    host.install(
        &space,
        InstallRequest::new("kit_a", CHILD)
            .data(r#"{"templates":{"a2":{"label":"Zeta"},"z1":{"label":"Alpha"}}}"#),
        &mut generator(),
    )
    .expect("install with data");
    host.install(&space, InstallRequest::new("kit_b", CHILD).data(r#"{"templates":{"a1":{"label":"Mid"}}}"#), &mut generator())
        .expect("install with data");

    let result = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(
        rows(&result),
        vec![
            ("kit_a".to_owned(), "a2".to_owned(), "Zeta".to_owned()),
            ("kit_a".to_owned(), "z1".to_owned(), "Alpha".to_owned()),
            ("kit_b".to_owned(), "a1".to_owned(), "Mid".to_owned()),
        ],
        "installation `$data` rows are seeded into each child and aggregated"
    );
}

#[test]
fn private_child_field_is_unreachable_through_a_root_view() {
    // §13.8 isolation: the boundary grants access only to the exposed `$view`
    // fields, so a root read of `.modules::templates` never sees the child's
    // private `secret` — the interface row carries only `id`/`label`.
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    install(&mut host, &space, "kit");
    create(&mut host, &space, "kit", "t1", "Invoice", "top-secret");

    let result = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(result.len(), 1);
    let row = &result.rows()[0];
    assert_eq!(row.field("label"), Some(&text("Invoice")));
    assert_eq!(row.field("secret"), None, "a private child field never crosses the interface boundary");
}

#[test]
fn interface_mutation_routes_to_the_childs_exposed_mutation() {
    // §13.10: a parent routes `templates.create` to the child's bound
    // `create_template`; the child admits it atomically, and the new row is then
    // visible in the parent's `.modules::templates` aggregation.
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    install(&mut host, &space, "kit");

    let request = CallRequest::new("create_template")
        .arg("id", text("t1"))
        .arg("label", text("Alpha"))
        .arg("secret", text("hush"));
    let outcome = host
        .interface_call(&space, "kit", "templates", "create", &request, &mut generator())
        .expect("interface call");
    // §13.8: the bound mutation returns exactly the declared `$return` shape.
    match outcome {
        CallOutcome::Committed { response, .. } => {
            let wire = response.expect("create returns a value").to_wire();
            assert_eq!(wire, serde_json::json!({ "id": "t1", "label": "Alpha" }));
        }
        other => panic!("interface_call must commit, got {other:?}"),
    }

    // The parent now observes the child's new row through the aggregation.
    let result = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(rows(&result), vec![("kit".to_owned(), "t1".to_owned(), "Alpha".to_owned())]);
}

/// A child whose exposed `$view` omits `label`, narrower than the `templates`
/// interface contract `{ id, label }` (§13.8 structural satisfaction).
const THIN_CHILD: &str = r#"{
  "$liasse": 1
  "$module": "t.tplthin@1.0.0"
  "$model": { "templates": { "$key": "id", "id": "text", "label": "text" } }
  "$expose": { "templates": { "$view": ".templates { id }" } }
}"#;

#[test]
fn install_rejects_a_child_that_does_not_satisfy_the_interface_contract() {
    // §13.8: view satisfaction is structural — the module space's `templates`
    // interface requires `{ id, label }`, but the child's exposed view projects
    // `{ id }` only, so the install is refused before the instance activates.
    use liasse_runtime::ModuleError;
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    match host.install(&space, InstallRequest::new("kit", THIN_CHILD), &mut generator()) {
        Err(ModuleError::InterfaceContract(interface, _)) => assert_eq!(interface, "templates"),
        other => panic!("a contract-violating `$expose` must be rejected, got {other:?}"),
    }
    assert!(!host.is_installed(&space, "kit"), "the rejected instance never activated");
}

#[test]
fn unknown_interface_mutation_is_rejected() {
    // §13.8: the boundary routes only bound contracts; an unbound name is refused.
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host(ROOT_LEVEL);
    install(&mut host, &space, "kit");
    let request = CallRequest::new("x").arg("id", text("t1"));
    assert!(
        host.interface_call(&space, "kit", "templates", "nope", &request, &mut generator()).is_err(),
        "an unbound interface mutation is not routable"
    );
}
