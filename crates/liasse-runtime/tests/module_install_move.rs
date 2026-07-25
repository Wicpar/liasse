#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §13.16 `<-` install: moving a module value into a slot of a module collection.
//!
//! The whole risk of this lowering is the **display path**. `.modules[@id]` names
//! a slot relative to a row; the host addresses instances by the space's mount path
//! (`/companies/acme/modules`), which interleaves the containing-row keys. Get that
//! wrong and the module installs into the WRONG space while every call reports
//! success — a wrong module state that ships looking correct.
//!
//! Every test here is therefore written as a *discrimination*: the installed
//! instance must be reachable at exactly one mount and absent at every neighbouring
//! mount the same program could plausibly have selected. The neighbours are chosen
//! adversarially — a sibling row of the same collection, a nested space under the
//! same row, a top-level space of the same declaration name, and a key whose text
//! contains the path separator itself.

mod support;

use liasse_artifact::ArtifactBuilder;
use liasse_ident::{HistoryPoint, InstanceId, LineageId, PointId};
use liasse_runtime::{CallOutcome, CallRequest, Engine, ModuleHost, Value};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_value::{BlobDescriptor, ModuleHandle, Text};
use support::generator;

/// A root host whose lifecycle mutations install a module by MOVING an unpacked
/// module value into a slot (§13.16 `<-`), rather than by naming the mount in a
/// `module.install({ space, name })` call.
///
/// It declares three module collections whose display paths a mistake could confuse:
/// a row-scoped `companies.modules`, a deeper `companies.divisions.modules` under a
/// nested collection, and a top-level `modules` of the SAME declaration name.
const ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.install.host@1.0.0"
  "$model": {
    "log": { "$key": "id", "id": "text" }
    "log_view": { "$view": ".log { id }" }
    "modules": { "$key": "text", "$value": "module" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
      "divisions": {
        "$key": "id"
        "id": "text"
        "modules": { "$key": "text", "$value": "module" }
      }
    }
    "$mut": {
      "provision({ id: text, company: text, name: text, blob: blob })": [
        "e = .log + { id: @id }"
        ".companies[@company].modules[@name] <- unpack(@blob)"
        "return e { id }"
      ]
      "provision_division({ id: text, company: text, division: text, name: text, blob: blob })": [
        "e = .log + { id: @id }"
        ".companies[@company].divisions[@division].modules[@name] <- unpack(@blob)"
        "return e { id }"
      ]
      "provision_root({ id: text, name: text, blob: blob })": [
        "e = .log + { id: @id }"
        ".modules[@name] <- unpack(@blob)"
        "return e { id }"
      ]
      "provision_two_step({ id: text, company: text, name: text, blob: blob })": [
        "e = .log + { id: @id }"
        "m <- unpack(@blob)"
        ".companies[@company].modules[@name] <- m"
        "return e { id }"
      ]
      "provision_into_a_plain_collection({ id: text, name: text, blob: blob })": [
        "e = .log + { id: @id }"
        ".log[@name] <- unpack(@blob)"
        "return e { id }"
      ]
      // §13.16 "Move": the host lends a handle to an instance it owns and the
      // program relocates it into another slot (§13.16 "Delegation" is how a
      // `module` parameter is reached at all).
      "relocate({ id: text, company: text, to: text, at: module })": [
        "e = .log + { id: @id }"
        ".companies[@company].modules[@to] <- @at"
        "return e { id }"
      ]
      "relocate_to_division({ id: text, company: text, division: text, to: text, at: module })": [
        "e = .log + { id: @id }"
        ".companies[@company].divisions[@division].modules[@to] <- @at"
        "return e { id }"
      ]
      // §13.16 "Re-admission" outside a move: the operator names what a `<-` does
      // at ITS destination, so with no destination there is nothing to re-admit.
      "stray_readmit({ id: text, at: module })": [
        "e = .log + { id: @id }"
        "m <- reinstall_module(@at)"
        "return e { id }"
      ]
      // §13.16 "Re-admission": the explicit form of the same move, which puts the
      // instance through the DESTINATION collection's admission.
      "reinstall_into_division({ id: text, company: text, division: text, to: text, at: module })": [
        "e = .log + { id: @id }"
        ".companies[@company].divisions[@division].modules[@to] <- reinstall_module(@at)"
        "return e { id }"
      ]
    }
  }
  "$data": {
    "companies": {
      "acme": { "divisions": { "eu": {} } }
      "globex": {}
      "acme/x": {}
    }
  }
}"#;

/// A root whose module collection is declared ONLY on the company row, reached from
/// a ROW mutation of `companies` as the bare `.modules[@name]` §13.16 spells.
const ROW_ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.install.rowhost@1.0.0"
  "$model": {
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
      "$mut": {
        "provision({ name: text, blob: blob })": [
          ".modules[@name] <- unpack(@blob)"
        ]
      }
    }
  }
  "$data": { "companies": { "acme": {}, "globex": {} } }
}"#;

/// A root that declares a top-level `modules` collection AND a row-scoped
/// `companies.modules` of the same declaration name — the shape a confusion between
/// two collections would show up in. They are told apart by the written
/// containment, exactly as any two same-named collections are.
const AMBIGUOUS_ROOT: &str = r#"{
  "$liasse": 1
  "$app": "t.install.ambiguous@1.0.0"
  "$model": {
    "modules": { "$key": "text", "$value": "module" }
    "companies": {
      "$key": "id"
      "id": "text"
      "modules": { "$key": "text", "$value": "module" }
    }
    "$mut": {
      "provision_root({ name: text, blob: blob })": [
        "/modules[@name] <- unpack(@blob)"
      ]
      "provision_row({ company: text, name: text, blob: blob })": [
        ".companies[@company].modules[@name] <- unpack(@blob)"
      ]
    }
  }
  "$data": { "companies": { "acme": {} } }
}"#;

/// A `sales` module seeded at 5, exposing its items so an install is observable
/// through the space it landed in.
const SALES_V1: &str = r#"{
  "$liasse": 1
  "$module": "t.install.sales@1.0.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
  }
  "$data": { "items": { "a": { "qty": "5" } } }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// A second package, seeded at 9, so an override is distinguishable from a no-op.
const SALES_V2: &str = r#"{
  "$liasse": 1
  "$module": "t.install.sales@1.1.0"
  "$model": {
    "items": { "$key": "id", "id": "text", "qty": "int" }
  }
  "$data": { "items": { "a": { "qty": "9" } } }
  "$expose": { "items": { "$view": ".items { id, qty }" } }
}"#;

/// The module-collection entry `name` occupies under the collection the display
/// path `mount` names — one mounted instance's identity (§13.3).
fn at(mount: &str, name: &str) -> liasse_store::RowAddress {
    support::mount_at(mount, name)
}

/// Serialize a module definition into a minimal `.liasse` package blob.
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

fn host_of(definition: &str, instance: &str) -> ModuleHost<MemoryStoreFactory> {
    let root: Engine<MemoryStore> = support::load(instance, definition);
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

fn host() -> ModuleHost<MemoryStoreFactory> {
    host_of(ROOT, "t.install.host")
}

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn blob_of(host: &mut ModuleHost<MemoryStoreFactory>, definition: &str) -> BlobDescriptor {
    host.store_package_blob(&package_blob(definition), Some("sales.liasse".to_owned()))
        .expect("the package blob is stored")
}

fn blob_arg(host: &mut ModuleHost<MemoryStoreFactory>, definition: &str) -> Value {
    Value::Blob(Box::new(blob_of(host, definition)))
}

/// The `qty` of item `a` read through the instance's exposed interface AT the given
/// mount — the read-back that proves an install landed in that space and no other.
fn qty_at(host: &ModuleHost<MemoryStoreFactory>, mount: &str, name: &str) -> Option<String> {
    let collection = neighbours().into_iter().find(|(label, _)| *label == mount).map(|(_, c)| c)?;
    let view = host.interface_read(&collection.row(liasse_store::KeyValue::single(text(name))), "items").ok()??;
    let row = view.rows().iter().find(|r| matches!(r.field("id"), Some(Value::Text(t)) if t.as_str() == "a"))?;
    match row.field("qty")? {
        Value::Int(v) => Some(v.to_canonical_text()),
        _ => None,
    }
}

/// Every module collection the fixture declares, plus the ones a plausible mistake
/// would produce. Each is built STRUCTURALLY from its containing rows' typed keys,
/// so the `acme/x` company is one key holding a separator — never two path levels.
fn neighbours() -> Vec<(&'static str, liasse_store::CollectionPath)> {
    vec![
        ("/companies/acme/modules", support::collection_at("/companies/acme/modules")),
        ("/companies/globex/modules", support::collection_at("/companies/globex/modules")),
        (
            "/companies/acme/divisions/eu/modules",
            support::collection_at("/companies/acme/divisions/eu/modules"),
        ),
        (
            "/companies/{acme/x}/modules",
            liasse_store::CollectionPath::nested(
                [liasse_store::AddressStep::new(
                    liasse_ident::NameSegment::new("companies"),
                    liasse_store::KeyValue::single(text("acme/x")),
                )],
                liasse_ident::NameSegment::new("modules"),
            ),
        ),
        ("/modules", support::collection_at("/modules")),
    ]
}

/// The mounts at which `name` is installed, over every neighbouring collection. A
/// correct install occupies EXACTLY ONE of them.
fn mounts_holding(host: &ModuleHost<MemoryStoreFactory>, name: &str) -> Vec<&'static str> {
    neighbours()
        .into_iter()
        .filter(|(_, collection)| {
            host.is_installed(&collection.row(liasse_store::KeyValue::single(text(name))))
        })
        .map(|(label, _)| label)
        .collect()
}

fn call(host: &mut ModuleHost<MemoryStoreFactory>, request: &CallRequest) -> CallOutcome {
    host.call_root_lifecycle(request, &mut generator()).expect("no engine fault")
}

// ---- the display path ------------------------------------------------------

#[test]
fn a_move_into_a_row_scoped_space_installs_under_that_row_and_no_other() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    let request = CallRequest::new("provision")
        .arg("id", text("first"))
        .arg("company", text("acme"))
        .arg("name", text("sales"))
        .arg("blob", blob);

    let outcome = call(&mut host, &request);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");

    // The interleaved containing-row key selects Acme's space — not Globex's, not
    // the nested division space under the same row, not the top-level space.
    assert_eq!(
        mounts_holding(&host, "sales"),
        vec!["/companies/acme/modules"],
        "the instance is installed at exactly the mount the written path names"
    );
    assert_eq!(
        qty_at(&host, "/companies/acme/modules", "sales"),
        Some("5".to_owned()),
        "and it reads back through that space carrying its package seed"
    );
}

#[test]
fn the_containing_row_key_selects_the_space_not_the_declaration_alone() {
    let mut host = host();
    let acme = blob_arg(&mut host, SALES_V1);
    let globex = blob_arg(&mut host, SALES_V2);

    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("a"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", acme),
    );
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("g"))
            .arg("company", text("globex"))
            .arg("name", text("sales"))
            .arg("blob", globex),
    );

    // §13.2: "the same package installed in each space creates two independent
    // instances". Two installs of the SAME declaration path under different rows
    // must be two instances, each carrying its own package's seed.
    assert_eq!(qty_at(&host, "/companies/acme/modules", "sales"), Some("5".to_owned()));
    assert_eq!(qty_at(&host, "/companies/globex/modules", "sales"), Some("9".to_owned()));
}

#[test]
fn a_nested_containing_row_interleaves_every_ancestor_key() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    let request = CallRequest::new("provision_division")
        .arg("id", text("d"))
        .arg("company", text("acme"))
        .arg("division", text("eu"))
        .arg("name", text("sales"))
        .arg("blob", blob);

    let outcome = call(&mut host, &request);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");

    // Two levels of containment: dropping either key would land the instance in
    // `/companies/acme/modules` or `/divisions/eu/modules`.
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/divisions/eu/modules"]);
    assert_eq!(qty_at(&host, "/companies/acme/divisions/eu/modules", "sales"), Some("5".to_owned()));
}

#[test]
fn a_top_level_space_has_no_containing_row_key_to_interleave() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    let outcome = call(
        &mut host,
        &CallRequest::new("provision_root").arg("id", text("r")).arg("name", text("sales")).arg("blob", blob),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");
    assert_eq!(mounts_holding(&host, "sales"), vec!["/modules"]);
}

#[test]
fn a_key_containing_the_path_separator_cannot_forge_a_deeper_containment() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    // The company row is keyed `acme/x` — a key holding the path separator. Under a
    // display-path coordinate a naive join would read `/companies/acme/x/modules`,
    // whose declaration path is `["companies", "x"]`, a collection this package never
    // declares. There is no display path here: the address carries the key as ONE
    // typed component, so the separator is data and can never become a level.
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("e"))
            .arg("company", text("acme/x"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/{acme/x}/modules"]);
    // The key is ONE typed component holding a separator, so it never becomes a path
    // level: the address a naive concatenation would have produced names the
    // collection `companies.x.modules`, which this package does not declare at all.
    let forged = support::collection_at("/companies/acme/x/modules");
    assert!(
        !host.is_installed(&forged.row(liasse_store::KeyValue::single(text("sales")))),
        "the naive concatenation addresses a different collection, so the install could \
         not have landed there by accident"
    );
}

#[test]
fn a_row_mutation_installs_into_its_own_receivers_space() {
    let mut host = host_of(ROW_ROOT, "t.install.rowhost");
    let blob = blob_arg(&mut host, SALES_V1);
    let request = CallRequest::new("provision").receiver(text("acme")).arg("name", text("sales")).arg("blob", blob);

    let outcome = call(&mut host, &request);
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");
    assert!(host.is_installed(&at("/companies/acme/modules", "sales")), "installed under the receiver row");
    assert!(!host.is_installed(&at("/companies/globex/modules", "sales")), "and under no sibling row");
}

// ---- refusals: never a guess, never a silent no-op --------------------------

#[test]
fn two_collections_of_the_same_name_are_told_apart_by_the_written_containment() {
    // A module collection is an ORDINARY collection, so a package that declares a
    // top-level `modules` AND a row-scoped `companies.modules` is told apart by the
    // language's own collection references, not by a module-specific coordinate.
    // The shape is adversarial on purpose: a resolution that preferred one over the
    // other would install into a real, different collection while reporting success.
    let mut host = host_of(AMBIGUOUS_ROOT, "t.install.ambiguous");
    let root_blob = blob_arg(&mut host, SALES_V1);
    let row_blob = blob_arg(&mut host, SALES_V2);

    let outcome = call(&mut host, &CallRequest::new("provision_root").arg("name", text("sales")).arg("blob", root_blob));
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the rooted install commits, got {outcome:?}");
    assert!(host.is_installed(&at("/modules", "sales")), "the package-root collection holds it");
    assert!(!host.is_installed(&at("/companies/acme/modules", "sales")), "and the row-scoped one does not");

    let outcome = call(
        &mut host,
        &CallRequest::new("provision_row").arg("company", text("acme")).arg("name", text("sales")).arg("blob", row_blob),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the row-scoped install commits, got {outcome:?}");
    // The two carry DIFFERENT package seeds, so a collision could not hide behind
    // equal values: each collection must read back its own.
    assert_eq!(qty_at(&host, "/modules", "sales"), Some("5".to_owned()));
    assert_eq!(qty_at(&host, "/companies/acme/modules", "sales"), Some("9".to_owned()));
}

#[test]
fn a_move_into_a_collection_that_is_not_a_module_collection_is_refused() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    let outcome = call(
        &mut host,
        &CallRequest::new("provision_into_a_plain_collection")
            .arg("id", text("x"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );
    let CallOutcome::Rejected(rejection) = &outcome else {
        panic!("a module moved into a non-module collection must be refused, got {outcome:?}");
    };
    assert!(
        rejection.message().contains("module collection"),
        "the refusal says the destination is not a module collection: {}",
        rejection.message()
    );
    assert!(mounts_holding(&host, "sales").is_empty(), "nothing was installed anywhere");
}

#[test]
fn a_ghost_containing_row_has_no_collection_to_install_into() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("x"))
            .arg("company", text("ghost"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );

    // §13.2/§13.3: the addressed entry is checked against LIVE root state. This is
    // BAD INPUT — the caller named a row that is not there — so it is a REJECTION,
    // not an engine error, and the refusal quotes the address it resolved, which
    // pins the containment the write addressed rather than only that it failed.
    let CallOutcome::Rejected(rejection) = &outcome else {
        panic!("an entry whose containing row is not live must be rejected, got {outcome:?}");
    };
    assert!(
        rejection.message().contains("ghost"),
        "the refusal names the address that resolved to no row, got {rejection:?}"
    );
    assert!(mounts_holding(&host, "sales").is_empty(), "nothing was half-installed");
    assert!(
        host.root().view_at_head("log_view").expect("view").expect("declared").rows().is_empty(),
        "and the program's own change rolled back with it"
    );
}

// ---- the move's own semantics ----------------------------------------------

#[test]
fn a_module_binds_to_a_local_and_installs_from_it() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    // §8.5: `=` copies, and a module is move-only, so `m <- unpack(@blob)` is the
    // only spelling that binds one. The bound handle then installs.
    let outcome = call(
        &mut host,
        &CallRequest::new("provision_two_step")
            .arg("id", text("t"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the install commits, got {outcome:?}");
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/modules"]);
}

#[test]
fn moving_into_an_occupied_slot_replaces_its_occupant() {
    let mut host = host();
    let v1 = blob_arg(&mut host, SALES_V1);
    let v2 = blob_arg(&mut host, SALES_V2);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", v1),
    );
    assert_eq!(qty_at(&host, "/companies/acme/modules", "sales"), Some("5".to_owned()));

    // §13.16: "install; an occupant is dropped and uninstalled".
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("two"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", v2),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the override commits, got {outcome:?}");
    assert_eq!(
        qty_at(&host, "/companies/acme/modules", "sales"),
        Some("9".to_owned()),
        "the slot now holds the moved-in instance, not the replaced one"
    );
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/modules"], "and exactly one instance remains");
}

#[test]
fn a_malformed_package_rolls_back_the_whole_transition() {
    let mut host = host();
    let garbage = host.store_package_blob(b"not a package at all", None).expect("store");
    let outcome = call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("bad"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", Value::Blob(Box::new(garbage))),
    );
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "a malformed package rejects, got {outcome:?}");
    assert!(mounts_holding(&host, "sales").is_empty(), "no instance was half-mounted");
    assert!(
        host.root().view_at_head("log_view").expect("view").expect("declared").rows().is_empty(),
        "the program's own change rolled back with it"
    );
}

// ---- §13.16 "Move": relocating an installed instance ------------------------

/// The `module` value denoting an installed instance. Only the runtime can mint one
/// — `Value::Module` is refused by the wire decoder — so this stands for the trusted
/// host handing a program a handle to an instance it owns (§13.16 delegation).
fn handle(mount: &str, name: &str) -> Value {
    Value::Module(ModuleHandle::Mounted(at(mount, name).render()))
}

#[test]
fn moving_a_mounted_handle_within_its_space_rekeys_the_instance() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );
    let before = host.incarnation(&at("/companies/acme/modules", "sales")).cloned().expect("installed");

    // §13.16 "Move": "Moving a handle between slots relocates the instance,
    // emptying the source."
    let outcome = call(
        &mut host,
        &CallRequest::new("relocate")
            .arg("id", text("two"))
            .arg("company", text("acme"))
            .arg("to", text("billing"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the relocation commits, got {outcome:?}");

    assert!(mounts_holding(&host, "sales").is_empty(), "the source slot is empty");
    assert_eq!(mounts_holding(&host, "billing"), vec!["/companies/acme/modules"], "and the destination holds it");
    assert_eq!(
        qty_at(&host, "/companies/acme/modules", "billing"),
        Some("5".to_owned()),
        "carrying the very same state"
    );
    // §13.3: a rekey preserves the incarnation, so the durable identity (D.1) is
    // the instance's own — this is a relocation, not a reinstall.
    assert_eq!(
        host.incarnation(&at("/companies/acme/modules", "billing")),
        Some(&before),
        "the relocated instance keeps its incarnation"
    );
}

#[test]
fn moving_a_mounted_handle_across_collections_is_refused_by_name() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );

    // The destination collection declares its own §13.4 parent surfaces, §13.5 peer
    // set and §13.8 interface contracts; the instance was admitted against the
    // source's and against none of the destination's.
    let outcome = call(
        &mut host,
        &CallRequest::new("relocate_to_division")
            .arg("id", text("two"))
            .arg("company", text("acme"))
            .arg("division", text("eu"))
            .arg("to", text("sales"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    let CallOutcome::Rejected(rejection) = &outcome else {
        panic!("a cross-collection relocation must be refused, got {outcome:?}");
    };
    let detail = rejection.message();
    assert!(detail.contains("divisions"), "the refusal names the destination it would have written: {detail}");
    assert!(
        detail.contains("reinstall_module"),
        "and names the operator that performs the move honestly: {detail}"
    );

    // The instance stayed exactly where it was, and the program's own change with it.
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/modules"]);
    assert_eq!(
        host.root().view_at_head("log_view").expect("view").expect("declared").rows().len(),
        1,
        "only the install's own log row survives; the refused relocation's rolled back"
    );
}

#[test]
fn reinstall_module_moves_across_collections_by_re_admitting_the_instance() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );
    let before = host.incarnation(&at("/companies/acme/modules", "sales")).cloned().expect("installed");

    // §13.16 "Re-admission": the SAME move the plain `<-` refuses, written as the
    // explicit request to re-run the destination's admission.
    let outcome = call(
        &mut host,
        &CallRequest::new("reinstall_into_division")
            .arg("id", text("two"))
            .arg("company", text("acme"))
            .arg("division", text("eu"))
            .arg("to", text("sales"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the re-admission commits, got {outcome:?}");

    // It lands in the destination and NOWHERE else — the source is empty, and the
    // three neighbouring collections a mistake could reach are untouched.
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/divisions/eu/modules"]);
    // The instance kept its own state and history: a re-admission re-checks the
    // boundary, it does not re-create the instance.
    assert_eq!(qty_at(&host, "/companies/acme/divisions/eu/modules", "sales"), Some("5".to_owned()));
    assert_eq!(
        host.incarnation(&at("/companies/acme/divisions/eu/modules", "sales")),
        Some(&before),
        "the private store, and therefore the D.1 incarnation, is the instance's own"
    );
}

#[test]
fn reinstall_module_outside_a_move_is_refused_by_name() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );

    // The dangerous reading is that `reinstall_module(m)` is an ordinary
    // value-yielding call: it types as `module`, so binding it would look like it
    // had moved an instance while moving nothing at all. With no destination there
    // is no boundary to re-admit against, so it is refused BY NAME.
    let outcome = call(
        &mut host,
        &CallRequest::new("stray_readmit")
            .arg("id", text("two"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    let CallOutcome::Rejected(rejection) = &outcome else {
        panic!("a destination-less re-admission must be refused, got {outcome:?}");
    };
    assert!(
        rejection.message().contains("reinstall_module"),
        "the refusal names the operator: {}",
        rejection.message()
    );
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/modules"], "nothing moved");
}

#[test]
fn reinstall_module_into_a_collection_with_no_live_containing_row_is_rejected() {
    let mut host = host();
    let blob = blob_arg(&mut host, SALES_V1);
    call(
        &mut host,
        &CallRequest::new("provision")
            .arg("id", text("one"))
            .arg("company", text("acme"))
            .arg("name", text("sales"))
            .arg("blob", blob),
    );

    // §13.2 is part of the destination's admission, so `reinstall_module` runs it:
    // no `ghost` division row exists, so there is no collection to re-admit into.
    // The whole transition rejects and the instance stays where it was — the
    // re-admission is not a rekey that happens first and checks afterwards.
    let outcome = call(
        &mut host,
        &CallRequest::new("reinstall_into_division")
            .arg("id", text("two"))
            .arg("company", text("acme"))
            .arg("division", text("ghost"))
            .arg("to", text("sales"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    assert!(matches!(outcome, CallOutcome::Rejected(_)), "a dead destination must reject, got {outcome:?}");
    assert_eq!(mounts_holding(&host, "sales"), vec!["/companies/acme/modules"], "the instance did not move");
    assert_eq!(
        host.root().view_at_head("log_view").expect("view").expect("declared").rows().len(),
        1,
        "and the program's own change rolled back with it"
    );
}

#[test]
fn relocating_onto_an_occupied_slot_replaces_its_occupant() {
    let mut host = host();
    let v1 = blob_arg(&mut host, SALES_V1);
    let v2 = blob_arg(&mut host, SALES_V2);
    for (id, name, blob) in [("one", "sales", v1), ("two", "billing", v2)] {
        call(
            &mut host,
            &CallRequest::new("provision")
                .arg("id", text(id))
                .arg("company", text("acme"))
                .arg("name", text(name))
                .arg("blob", blob),
        );
    }
    assert_eq!(qty_at(&host, "/companies/acme/modules", "billing"), Some("9".to_owned()));

    // §13.16: a move into a slot "replaces any instance already there" — the same
    // rule an install-over follows.
    let outcome = call(
        &mut host,
        &CallRequest::new("relocate")
            .arg("id", text("three"))
            .arg("company", text("acme"))
            .arg("to", text("billing"))
            .arg("at", handle("/companies/acme/modules", "sales")),
    );
    assert!(matches!(outcome, CallOutcome::Committed { .. }), "the relocation commits, got {outcome:?}");
    assert!(mounts_holding(&host, "sales").is_empty(), "the source slot is empty");
    assert_eq!(
        qty_at(&host, "/companies/acme/modules", "billing"),
        Some("5".to_owned()),
        "the destination holds the moved instance, not the occupant it replaced"
    );
}
