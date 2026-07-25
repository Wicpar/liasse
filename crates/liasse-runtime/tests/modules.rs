#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13 module composition over the row-scoped [`ModuleHost`]: a module installs
//! into a module collection and its `$expose`d interface is readable through the
//! boundary; an interface read resolves against the child's own state; a private
//! child field is unreachable across the boundary (isolation); an interface
//! aggregates across two installed instances with inherited identity; a disabled
//! instance retains its private state and is removed from the aggregation; enable
//! restores it; the §13.13 seed three-way merge follows its rule; and an install
//! into a **declared** module collection is admitted only when its containing row is live
//! in root state (§13.2/§13.3) — a live company row's space accepts, a ghost row is
//! refused, and a top-level space (contained by the always-live root) accepts.

mod support;

use std::collections::BTreeMap;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost,
    SeedMerge, Value,
};
use liasse_store::{CollectionPath, MemoryStore, MemoryStoreFactory, RowAddress};
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

/// A root that **declares** module collections, so §13.2/§13.3 requires a live
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
      "modules": { "$key": "text", "$value": "module" }
    }
    "hub": { "$key": "text", "$value": "module" }
  }
  "$data": { "companies": { "acme": { "name": "Acme" } } }
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

fn host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("root")), TASKS, &mut generator)
        .expect("root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn install(host: &mut ModuleHost<MemoryStoreFactory>, collection: &CollectionPath, name: &str) {
    let mut generator = generator();
    host.install(collection, InstallRequest::new(name, TEMPLATES), &mut generator).expect("install");
}

/// The `label`s the instance `name` exposes through its `templates` interface, in
/// read order. §13.9's "the parent reads every instance exposing an interface" is
/// now ordinary collection traversal over the module collection, so the per-instance
/// read is what the host itself offers; a test that wants the whole collection reads
/// it through a root view (`module_visibility`).
fn labels(host: &ModuleHost<MemoryStoreFactory>, name: &str) -> Vec<String> {
    let Ok(Some(result)) = host.interface_read(&at(name), "templates") else { return Vec::new() };
    result
        .rows()
        .iter()
        .filter_map(|row| match row.field("label") {
            Some(Value::Text(text)) => Some(text.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

fn add_template(host: &mut ModuleHost<MemoryStoreFactory>, at: &RowAddress, id: &str, label: &str, secret: &str) {
    let mut generator = generator();
    let request = CallRequest::new("add").arg("id", text(id)).arg("label", text(label)).arg("secret", text(secret));
    let outcome = host.child_call(at, &request, &mut generator).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "add commits");
}

#[test]
fn installed_module_exposes_a_readable_interface() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    add_template(&mut host, &at("sales"), "t1", "Invoice", "hush");

    // §13.8/§13.9: the exposed interface is readable through the boundary.
    let result = host
        .interface_read(&at("sales"), "templates")
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
    let mut host = host();
    install(&mut host, &collection(), "sales");
    add_template(&mut host, &at("sales"), "t1", "Invoice", "top-secret");

    // §13.8 isolation: the boundary grants access only to bound members; `secret`
    // is a private field the `$expose` `$view` does not project, so it never
    // crosses the boundary.
    let result = host.interface_read(&at("sales"), "templates").expect("read").expect("declared");
    let row = &result.rows()[0];
    assert_eq!(row.field("secret"), None, "a private child field is unreachable through the interface");
    // The child still holds it privately: an unknown interface exposes nothing.
    assert_eq!(host.interface_read(&at("sales"), "private").expect("read"), None);
}

#[test]
fn each_installed_instance_exposes_its_own_interface_rows() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    install(&mut host, &collection(), "support");
    add_template(&mut host, &at("sales"), "s1", "Sales note", "x");
    add_template(&mut host, &at("support"), "u1", "Support note", "y");

    // §13.8: each entry of the module collection reads its OWN instance's exposed
    // rows through the boundary — the entries are ordinary rows, so reading the
    // whole collection is the §6.4 traversal a root view performs.
    assert_eq!(labels(&host, "sales"), vec!["Sales note".to_owned()]);
    assert_eq!(labels(&host, "support"), vec!["Support note".to_owned()]);
}

#[test]
fn install_is_isolated_per_instance_and_per_containing_row() {
    let mut host = host();
    let globex = support::collection_at("/companies/globex/modules");
    install(&mut host, &collection(), "sales");
    install(&mut host, &globex, "sales");
    add_template(&mut host, &at("sales"), "a1", "Acme only", "x");

    // §13.2: the same package installed under two containing rows is two independent
    // instances — the collections are different collections, by ordinary nesting.
    let acme_rows = host.interface_read(&at("sales"), "templates").expect("acme").expect("declared");
    assert_eq!(acme_rows.len(), 1);
    let globex_entry = globex.row(liasse_store::KeyValue::single(text("sales")));
    let globex_rows = host.interface_read(&globex_entry, "templates").expect("globex").expect("declared");
    assert_eq!(globex_rows.len(), 0, "the sibling row's collection is independent");
    assert_ne!(host.incarnation(&at("sales")), host.incarnation(&globex_entry));
}

fn declared_host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("spaces")), DECLARED_ROOT, &mut generator)
        .expect("declared-space root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn try_install(
    host: &mut ModuleHost<MemoryStoreFactory>,
    collection: &CollectionPath,
    name: &str,
) -> Result<(), ModuleError> {
    let mut generator = generator();
    host.install(collection, InstallRequest::new(name, TEMPLATES), &mut generator).map(|_| ())
}

#[test]
fn install_into_declared_space_with_live_containing_row_is_admitted() {
    // §13.2/§13.3: `acme` is a live company row, so its module collection exists and the
    // install is admitted — the check must not over-reject a real containing row.
    let mut host = declared_host();
    let acme = support::collection_at("/companies/acme/modules");
    try_install(&mut host, &acme, "kit").expect("install into a live containing row's space");
    assert!(host.is_installed(&acme.row(liasse_store::KeyValue::single(text("kit")))));
}

#[test]
fn install_into_ghost_containing_row_is_rejected() {
    // §13.2/§13.3: no `ghost` company row exists, so there is no module collection
    // under it; the install has nothing to target and is refused, never admitted
    // into a ghost row. The refusal names the ENTRY it resolved, which pins the
    // containment it was addressed at, not merely that something failed.
    let mut host = declared_host();
    let ghost = support::collection_at("/companies/ghost/modules");
    match try_install(&mut host, &ghost, "kit") {
        Err(ModuleError::MissingContainingRow(address)) => {
            assert_eq!(address, support::mount_at("/companies/ghost/modules", "kit").render());
        }
        other => panic!("expected a missing-containing-row refusal, got {other:?}"),
    }
    assert!(
        !host.is_installed(&ghost.row(liasse_store::KeyValue::single(text("kit")))),
        "no instance is recorded under a ghost containing row"
    );
}

#[test]
fn install_into_top_level_declared_space_is_admitted() {
    // §13.2: a top-level module collection is contained by the package root, which is
    // always live, so an install is admitted with no containing row to resolve.
    let mut host = declared_host();
    let hub = support::collection_at("/hub");
    try_install(&mut host, &hub, "kit").expect("install into a top-level space");
    assert!(host.is_installed(&hub.row(liasse_store::KeyValue::single(text("kit")))));
}

#[test]
fn disable_retains_state_and_removes_boundary_occurrences() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    install(&mut host, &collection(), "support");
    add_template(&mut host, &at("sales"), "t1", "kept", "x");
    add_template(&mut host, &at("support"), "u1", "other", "y");
    assert_eq!(labels(&host, "sales"), vec!["kept".to_owned()]);

    // §13.3/§13.12: disabling removes the active boundary occurrences while
    // retaining the private stored state.
    host.disable(&at("sales")).expect("disable");
    assert!(!host.is_enabled(&at("sales")));
    assert_eq!(labels(&host, "support"), vec!["other".to_owned()], "a sibling is unaffected");
    match host.interface_read(&at("sales"), "templates") {
        Err(ModuleError::Disabled(_)) => {}
        other => panic!("a disabled instance exposes no boundary read, got {other:?}"),
    }

    // §13.3: enabling revalidates and restores the boundary over the exact
    // preserved private state.
    host.enable(&at("sales")).expect("enable");
    assert_eq!(labels(&host, "sales"), vec!["kept".to_owned()], "the private state survived disable/enable");
}

#[test]
fn filtered_exposed_view_only_projects_matching_rows() {
    // §13.9 canonical shape: an exposed `$view` may filter (`[:t | t.enabled]`), so
    // only the matching rows cross the boundary.
    let mut host = host();
    host.install(&collection(), InstallRequest::new("sales", FILTERED), &mut generator()).expect("install");

    let mut add = |id: &str, label: &str, enabled: bool| {
        let request = CallRequest::new("add")
            .arg("id", text(id))
            .arg("label", text(label))
            .arg("enabled", Value::Bool(enabled));
        host.child_call(&at("sales"), &request, &mut generator()).expect("add");
    };
    add("a", "shown", true);
    add("b", "hidden", false);

    assert_eq!(labels(&host, "sales"), vec!["shown".to_owned()], "the disabled template is filtered out");
}

#[test]
fn duplicate_name_in_a_space_is_rejected() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    let mut generator = generator();
    match host.install(&collection(), InstallRequest::new("sales", TEMPLATES), &mut generator) {
        Err(ModuleError::DuplicateName(_)) => {}
        other => panic!("a duplicate instance name must be rejected, got {other:?}"),
    }
}

#[test]
fn empty_instance_name_is_rejected() {
    let mut host = host();
    let mut generator = generator();
    match host.install(&collection(), InstallRequest::new("", TEMPLATES), &mut generator) {
        Err(ModuleError::EmptyName) => {}
        other => panic!("an empty instance name must be rejected, got {other:?}"),
    }
}

#[test]
fn install_records_boundary_bindings() {
    // §13.3: the admitted instance records `$config`/`$use`/`$deps`. `currency` is
    // a declared `$config` member of `CONFIGURED`, so its supplied value is both
    // accepted (§13.1 type-check) and recorded on the instance.
    let mut host = host();
    let request = InstallRequest::new("sales", CONFIGURED)
        .config("currency", text("EUR"))
        .use_handle("people", "people")
        .optional_use("billing", "acme.billing/customers@1")
        .dep("tax", "acme.tax@2");
    let mut generator = generator();
    host.install(&collection(), request, &mut generator).expect("install");

    let bindings = host.bindings(&at("sales")).expect("installed");
    assert_eq!(bindings.config.get("currency"), Some(&text("EUR")));
    assert_eq!(bindings.uses.len(), 2, "one required and one optional handle");
    assert!(bindings.uses.iter().any(|(h, _, opt)| h == "billing" && *opt), "billing is optional");
    assert_eq!(bindings.deps.len(), 1, "one private dep");
}

#[test]
fn malformed_use_binding_is_rejected() {
    let mut host = host();
    let mut generator = generator();
    // A peer spec must be `line/interface@major`; a bare word is malformed.
    let request = InstallRequest::new("sales", TEMPLATES).use_handle("people", "acme.people/people");
    match host.install(&collection(), request, &mut generator) {
        Err(ModuleError::InvalidBinding(_)) => {}
        other => panic!("a malformed binding spec must be rejected, got {other:?}"),
    }
}

#[test]
fn rename_preserves_incarnation_and_state() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    let incarnation = host.incarnation(&at("sales")).expect("installed").clone();
    add_template(&mut host, &at("sales"), "t1", "kept", "x");

    host.rename(&at("sales"), "revenue").expect("rename");
    assert!(!host.is_installed(&at("sales")), "the old name no longer addresses the instance");
    assert_eq!(host.incarnation(&at("revenue")), Some(&incarnation), "rename preserves the incarnation");
    assert_eq!(labels(&host, "revenue"), vec!["kept".to_owned()], "rename preserves state");
}

#[test]
fn uninstall_removes_the_instance() {
    let mut host = host();
    install(&mut host, &collection(), "sales");
    host.uninstall(&at("sales")).expect("uninstall");
    assert!(!host.is_installed(&at("sales")));
    match host.interface_read(&at("sales"), "templates") {
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
    let mut host = host();
    let mut generator = generator();
    // §13.3: an explicit `$config` value is bound; an omitted one takes the default.
    host.install(&collection(), InstallRequest::new("kit_eur", CONFIGURED).config("currency", text("EUR")), &mut generator)
        .expect("install with explicit config");
    host.install(&collection(), InstallRequest::new("kit_def", CONFIGURED), &mut generator)
        .expect("install with default config");

    // §13.1: the child's exposed `$view` reads `$config.currency`; the installed
    // value crosses the boundary.
    let eur = host.interface_read(&at("kit_eur"), "templates").expect("read").expect("declared");
    assert_eq!(eur.rows()[0].field("currency"), Some(&text("EUR")), "the installed config value is read");
    // §13.3: the omitted member resolves to the declared `text = 'USD'` default.
    let def = host.interface_read(&at("kit_def"), "templates").expect("read").expect("declared");
    assert_eq!(def.rows()[0].field("currency"), Some(&text("USD")), "an omitted config member takes its default");
}

#[test]
fn config_value_type_mismatch_rejects_install() {
    let mut host = host();
    let mut generator = generator();
    // §13.1/§13.3: `currency` is declared `text`; a boolean does not decode to it,
    // so the install is rejected before the instance activates.
    let request = InstallRequest::new("kit", CONFIGURED).config("currency", Value::Bool(true));
    match host.install(&collection(), request, &mut generator) {
        Err(ModuleError::ConfigMismatch(_)) => {}
        other => panic!("a type-mismatched `$config` value must reject the install, got {other:?}"),
    }
}

#[test]
fn config_unknown_member_rejects_install() {
    let mut host = host();
    let mut generator = generator();
    // §13.1/§2.5: `tax_id` is not a declared `$config` member, so supplying it is
    // rejected (the declared `currency` still resolves to its default).
    let request = InstallRequest::new("kit", CONFIGURED).config("tax_id", text("X1"));
    match host.install(&collection(), request, &mut generator) {
        Err(ModuleError::ConfigMismatch(_)) => {}
        other => panic!("an undeclared `$config` member must reject the install, got {other:?}"),
    }
}
