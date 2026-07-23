#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module composition over the row-scoped [`ModuleHost`]: a module installs
//! into a module space and its `$expose`d interface is readable through the
//! boundary; an interface read resolves against the child's own state; a private
//! child field is unreachable across the boundary (isolation); an interface
//! aggregates across two installed instances with inherited identity; a disabled
//! instance retains its private state and is removed from the aggregation; enable
//! restores it; the §13.13 seed three-way merge follows its rule; and an install
//! into a **declared** module space is admitted only when its containing row is live
//! in root state (§13.2/§13.3) — a live company row's space accepts, a ghost row is
//! refused, and a top-level space (contained by the always-live root) accepts.

mod support;

use std::collections::BTreeMap;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost, ModuleSpace,
    SeedMerge, Value,
};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_value::Text;
use support::{generator, TASKS};

/// A module package with private `templates` state — of which only `id` and
/// `label` are exposed; `secret` is a private field the `$expose` `$view` omits —
/// and an `add` mutation.
const TEMPLATES: &str = r#"{
  "$liasse": 1
  "$module": "acme.sales_templates@1.0.0"
  "$model": {
    "templates": {
      "$key": "id"
      "id": "text"
      "label": "text"
      "secret": "text"
    }
    "$mut": { "add": ".templates + { id: @id, label: @label, secret: @secret }" }
  }
  "$expose": {
    "templates": { "$view": ".templates { id, label }" }
  }
}"#;

/// A module whose exposed `$view` filters to `enabled` templates only — the
/// spec's canonical §13.9 aggregation shape (`.templates[:t | t.enabled] { … }`).
const FILTERED: &str = r#"{
  "$liasse": 1
  "$module": "acme.filtered@1.0.0"
  "$model": {
    "templates": {
      "$key": "id"
      "id": "text"
      "label": "text"
      "enabled": "bool = true"
    }
    "$mut": {
      "add": ".templates + { id: @id, label: @label, enabled: @enabled }"
    }
  }
  "$expose": {
    "templates": { "$view": ".templates[:t | t.enabled] { id, label }" }
  }
}"#;

/// A root that **declares** `$modules` spaces, so §13.2/§13.3 requires a live
/// containing row before an install is admitted — unlike [`TASKS`], which declares
/// no space and leaves the undeclared-space seam untouched. `companies` seeds one
/// live row (`acme`); the top-level `hub` space is contained by the always-live
/// package root.
const DECLARED_ROOT: &str = r#"{
  "$liasse": 1
  "$app": "example.spaces@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "name": "text"
      "modules": { "$modules": {} }
    }
    "hub": { "$modules": {} }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn space() -> ModuleSpace {
    ModuleSpace::new("/companies/acme/modules").expect("well-formed mount path")
}

fn host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("root")), TASKS, &mut generator)
        .expect("root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn install(host: &mut ModuleHost<MemoryStoreFactory>, space: &ModuleSpace, name: &str) {
    let mut generator = generator();
    host.install(space, InstallRequest::new(name, TEMPLATES), &mut generator).expect("install");
}

fn add_template(host: &mut ModuleHost<MemoryStoreFactory>, space: &ModuleSpace, name: &str, id: &str, label: &str, secret: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("label", text(label)).arg("secret", text(secret));
    let outcome = host.child_call(space, name, &request, &mut generator).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

#[test]
fn installed_module_exposes_a_readable_interface() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    add_template(&mut host, &space, "sales", "t1", "Invoice", "hush");

    // §13.8/§13.9: the exposed interface is readable through the boundary.
    let result = host
        .interface_read(&space, "sales", "templates")
        .expect("interface read")
        .expect("the child declares a `templates` interface");
    // §13.8: the interface read resolves against the child's own state.
    assert_eq!(result.len(), 1, "the one added template is exposed");
    let row = &result.rows()[0];
    assert_eq!(row.field("id"), Some(&text("t1")));
    assert_eq!(row.field("label"), Some(&text("Invoice")), "the exposed row carries the child's value");
}

#[test]
fn private_child_field_is_unreachable_across_the_boundary() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    add_template(&mut host, &space, "sales", "t1", "Invoice", "top-secret");

    // §13.8 isolation: the boundary grants access only to bound members; `secret`
    // is a private field the `$expose` `$view` does not project, so it never
    // crosses the boundary.
    let result = host.interface_read(&space, "sales", "templates").expect("read").expect("declared");
    let row = &result.rows()[0];
    assert_eq!(row.field("secret"), None, "a private child field is unreachable through the interface");
    // The child still holds it privately: an unknown interface exposes nothing.
    assert_eq!(host.interface_read(&space, "sales", "private").expect("read"), None);
}

#[test]
fn interface_aggregates_across_two_installed_instances() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    install(&mut host, &space, "support");
    add_template(&mut host, &space, "sales", "s1", "Sales note", "x");
    add_template(&mut host, &space, "support", "u1", "Support note", "y");

    // §13.9: the parent reads every instance exposing an interface; each row's
    // inherited identity is the instance identity plus the exposed row.
    let rows = host.aggregate(&space, "templates").expect("aggregate");
    assert_eq!(rows.len(), 2, "one exposed row per instance");
    let by_instance: BTreeMap<&str, &Value> = rows
        .iter()
        .map(|r| (r.instance(), r.row().field("label").expect("label projected")))
        .collect();
    assert_eq!(by_instance.get("sales"), Some(&&text("Sales note")));
    assert_eq!(by_instance.get("support"), Some(&&text("Support note")));
}

#[test]
fn install_is_isolated_per_instance_and_per_space() {
    let (mut host, acme) = (host(), space());
    let globex = ModuleSpace::new("/companies/globex/modules").expect("mount path");
    install(&mut host, &acme, "sales");
    install(&mut host, &globex, "sales");
    add_template(&mut host, &acme, "sales", "a1", "Acme only", "x");

    // The same package installed in two spaces is two independent instances (§13.2).
    assert_eq!(host.aggregate(&acme, "templates").expect("acme").len(), 1);
    assert_eq!(host.aggregate(&globex, "templates").expect("globex").len(), 0, "sibling space is independent");
    assert_ne!(host.incarnation(&acme, "sales"), host.incarnation(&globex, "sales"));
}

fn declared_host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("spaces")), DECLARED_ROOT, &mut generator)
        .expect("declared-space root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn try_install(host: &mut ModuleHost<MemoryStoreFactory>, space: &ModuleSpace, name: &str) -> Result<(), ModuleError> {
    let mut generator = generator();
    host.install(space, InstallRequest::new(name, TEMPLATES), &mut generator).map(|_| ())
}

#[test]
fn install_into_declared_space_with_live_containing_row_is_admitted() {
    // §13.2/§13.3: `acme` is a live company row, so its module space exists and the
    // install is admitted — the check must not over-reject a real containing row.
    let mut host = declared_host();
    let acme = ModuleSpace::new("/companies/acme/modules").expect("mount path");
    try_install(&mut host, &acme, "kit").expect("install into a live containing row's space");
    assert!(host.is_installed(&acme, "kit"));
}

#[test]
fn install_into_ghost_containing_row_is_rejected() {
    // §13.2/§13.3: no `ghost` company row exists, so `/companies/ghost/modules` names
    // no space; the install has nothing to target and is refused, never admitted into
    // a ghost row.
    let mut host = declared_host();
    let ghost = ModuleSpace::new("/companies/ghost/modules").expect("mount path");
    match try_install(&mut host, &ghost, "kit") {
        Err(ModuleError::MissingContainingRow(path)) => assert_eq!(path, "/companies/ghost/modules"),
        other => panic!("expected a missing-containing-row refusal, got {other:?}"),
    }
    assert!(!host.is_installed(&ghost, "kit"), "no instance is recorded for a ghost-row space");
}

#[test]
fn install_into_top_level_declared_space_is_admitted() {
    // §13.2: a top-level `$modules` space is contained by the package root, which is
    // always live, so an install is admitted with no containing row to resolve.
    let mut host = declared_host();
    let hub = ModuleSpace::new("/hub").expect("mount path");
    try_install(&mut host, &hub, "kit").expect("install into a top-level space");
    assert!(host.is_installed(&hub, "kit"));
}

#[test]
fn disable_retains_state_and_removes_boundary_occurrences() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    install(&mut host, &space, "support");
    add_template(&mut host, &space, "sales", "t1", "kept", "x");
    add_template(&mut host, &space, "support", "u1", "other", "y");
    assert_eq!(host.aggregate(&space, "templates").expect("agg").len(), 2);

    // §13.3/§13.12: disabling removes the active boundary occurrences (so the
    // aggregation drops it) while retaining the private stored state.
    host.disable(&space, "sales").expect("disable");
    assert!(!host.is_enabled(&space, "sales"));
    assert_eq!(host.aggregate(&space, "templates").expect("agg").len(), 1, "the disabled instance leaves the aggregation");
    match host.interface_read(&space, "sales", "templates") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no boundary read, got {other:?}"),
    }

    // §13.3: enabling revalidates and restores the boundary over the exact
    // preserved private state.
    host.enable(&space, "sales").expect("enable");
    let rows = host.aggregate(&space, "templates").expect("agg");
    assert_eq!(rows.len(), 2, "enable restores the boundary occurrence");
    assert!(rows.iter().any(|r| r.instance() == "sales" && r.row().field("label") == Some(&text("kept"))),
        "the private state survived disable/enable");
}

#[test]
fn filtered_exposed_view_only_projects_matching_rows() {
    // §13.9 canonical shape: an exposed `$view` may filter (`[:t | t.enabled]`), so
    // only the matching rows cross the boundary.
    let (mut host, space) = (host(), space());
    host.install(&space, InstallRequest::new("sales", FILTERED), &mut generator()).expect("install");

    let mut add = |id: &str, label: &str, enabled: bool| {
        let request = CallRequest::new("add")
            .arg("id", text(id))
            .arg("label", text(label))
            .arg("enabled", Value::Bool(enabled));
        host.child_call(&space, "sales", &request, &mut generator()).expect("add");
    };
    add("a", "shown", true);
    add("b", "hidden", false);

    let rows = host.aggregate(&space, "templates").expect("aggregate");
    assert_eq!(rows.len(), 1, "the disabled template is filtered out of the exposed view");
    assert_eq!(rows[0].row().field("label"), Some(&text("shown")));
}

#[test]
fn duplicate_name_in_a_space_is_rejected() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    let mut generator = generator();
    match host.install(&space, InstallRequest::new("sales", TEMPLATES), &mut generator) {
        Err(ModuleError::DuplicateName(_)) => {}
        other => panic!("a duplicate instance name must be rejected, got {other:?}"),
    }
}

#[test]
fn empty_instance_name_is_rejected() {
    let (mut host, space) = (host(), space());
    let mut generator = generator();
    match host.install(&space, InstallRequest::new("", TEMPLATES), &mut generator) {
        Err(ModuleError::EmptyName) => {}
        other => panic!("an empty instance name must be rejected, got {other:?}"),
    }
}

#[test]
fn install_records_boundary_bindings() {
    // §13.3: the admitted instance records `$config`/`$use`/`$deps`. `currency` is
    // a declared `$config` member of `CONFIGURED`, so its supplied value is both
    // accepted (§13.1 type-check) and recorded on the instance.
    let (mut host, space) = (host(), space());
    let request = InstallRequest::new("sales", CONFIGURED)
        .config("currency", text("EUR"))
        .use_handle("people", "/companies/acme/modules/people")
        .optional_use("billing", "acme.billing/customers@1")
        .dep("tax", "acme.tax@2");
    let mut generator = generator();
    host.install(&space, request, &mut generator).expect("install");

    let bindings = host.bindings(&space, "sales").expect("installed");
    assert_eq!(bindings.config.get("currency"), Some(&text("EUR")));
    assert_eq!(bindings.uses.len(), 2, "one required and one optional handle");
    assert!(bindings.uses.iter().any(|(h, _, opt)| h == "billing" && *opt), "billing is optional");
    assert_eq!(bindings.deps.len(), 1, "one private dep");
}

#[test]
fn malformed_use_binding_is_rejected() {
    let (mut host, space) = (host(), space());
    let mut generator = generator();
    // A peer spec must be `line/interface@major`; a bare word is malformed.
    let request = InstallRequest::new("sales", TEMPLATES).use_handle("people", "not-a-spec");
    match host.install(&space, request, &mut generator) {
        Err(ModuleError::InvalidBinding(_)) => {}
        other => panic!("a malformed binding spec must be rejected, got {other:?}"),
    }
}

#[test]
fn nonabsolute_space_is_rejected() {
    match ModuleSpace::new("companies/acme/modules") {
        Err(ModuleError::InvalidSpace(_)) => {}
        other => panic!("a relative mount path is not a module space, got {other:?}"),
    }
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    let incarnation = host.incarnation(&space, "sales").expect("installed").clone();
    add_template(&mut host, &space, "sales", "t1", "kept", "x");

    host.rename(&space, "sales", "revenue").expect("rename");
    assert!(!host.is_installed(&space, "sales"), "the old name no longer addresses the instance");
    assert_eq!(host.incarnation(&space, "revenue"), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(host.aggregate(&space, "templates").expect("agg")[0].instance(), "revenue", "rename preserves state");
}

#[test]
fn uninstall_removes_the_instance() {
    let (mut host, space) = (host(), space());
    install(&mut host, &space, "sales");
    host.uninstall(&space, "sales").expect("uninstall");
    assert!(!host.is_installed(&space, "sales"));
    match host.interface_read(&space, "sales", "templates") {
        Err(ModuleError::Unknown(_)) => {}
        other => panic!("an uninstalled instance is unknown, got {other:?}"),
    }
}

#[test]
fn seed_three_way_merge_retains_local_edits() {
    // §13.13: the new seed replaces a field only when the current value still
    // equals the old seed value; a locally edited field is retained.
    let mut old_seed = BTreeMap::new();
    old_seed.insert("title".to_owned(), text("Welcome"));
    old_seed.insert("body".to_owned(), text("v1 body"));

    let mut new_seed = BTreeMap::new();
    new_seed.insert("title".to_owned(), text("Welcome v2"));
    new_seed.insert("body".to_owned(), text("v2 body"));

    let mut current = BTreeMap::new();
    // `title` is still the old seed value; `body` was edited locally.
    current.insert("title".to_owned(), text("Welcome"));
    current.insert("body".to_owned(), text("edited by user"));

    let merged = SeedMerge { old_seed: &old_seed, new_seed: &new_seed, current: &current }.merge();
    assert_eq!(merged.get("title"), Some(&text("Welcome v2")), "unchanged field takes the new seed");
    assert_eq!(merged.get("body"), Some(&text("edited by user")), "locally edited field is retained");
}

/// A module package with a declared `$config` struct (§13.1): `currency` is a
/// text installation value defaulting to `USD`. Its exposed interface projects
/// `$config.currency`, so a boundary read observes the value the instance was
/// installed with.
const CONFIGURED: &str = r#"{
  "$liasse": 1
  "$module": "acme.configured@1.0.0"
  "$config": { "currency": "text = 'USD'" }
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text" }
  }
  "$data": { "templates": { "std": { "label": "Standard" } } }
  "$expose": {
    "templates": { "$view": ".templates { id, label, currency: $config.currency }" }
  }
}"#;

#[test]
fn child_reads_installed_config_value_through_the_binding() {
    let (mut host, space) = (host(), space());
    let mut generator = generator();
    // §13.3: an explicit `$config` value is bound; an omitted one takes the default.
    host.install(&space, InstallRequest::new("kit_eur", CONFIGURED).config("currency", text("EUR")), &mut generator)
        .expect("install with explicit config");
    host.install(&space, InstallRequest::new("kit_def", CONFIGURED), &mut generator)
        .expect("install with default config");

    // §13.1: the child's exposed `$view` reads `$config.currency`; the installed
    // value crosses the boundary.
    let eur = host.interface_read(&space, "kit_eur", "templates").expect("read").expect("declared");
    assert_eq!(eur.rows()[0].field("currency"), Some(&text("EUR")), "the installed config value is read");
    // §13.3: the omitted member resolves to the declared `text = 'USD'` default.
    let def = host.interface_read(&space, "kit_def", "templates").expect("read").expect("declared");
    assert_eq!(def.rows()[0].field("currency"), Some(&text("USD")), "an omitted config member takes its default");
}

#[test]
fn config_value_type_mismatch_rejects_install() {
    let (mut host, space) = (host(), space());
    let mut generator = generator();
    // §13.1/§13.3: `currency` is declared `text`; a boolean does not decode to it,
    // so the install is rejected before the instance activates.
    let request = InstallRequest::new("kit", CONFIGURED).config("currency", Value::Bool(true));
    match host.install(&space, request, &mut generator) {
        Err(ModuleError::ConfigMismatch(_)) => {}
        other => panic!("a type-mismatched `$config` value must reject the install, got {other:?}"),
    }
}

#[test]
fn config_unknown_member_rejects_install() {
    let (mut host, space) = (host(), space());
    let mut generator = generator();
    // §13.1/§2.5: `tax_id` is not a declared `$config` member, so supplying it is
    // rejected (the declared `currency` still resolves to its default).
    let request = InstallRequest::new("kit", CONFIGURED).config("tax_id", text("X1"));
    match host.install(&space, request, &mut generator) {
        Err(ModuleError::ConfigMismatch(_)) => {}
        other => panic!("an undeclared `$config` member must reject the install, got {other:?}"),
    }
}
