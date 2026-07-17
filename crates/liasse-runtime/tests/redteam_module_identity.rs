#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM PROBE — §13.9 aggregation inherited identity.
//!
//! SPEC.md §13.9 ("Aggregating module data"): when a parent reads every instance
//! exposing an interface through `.modules::iface`,
//!
//!     Inherited identity is:
//!         module instance identity + exposed row identity
//!
//! So two DIFFERENT instances (`kit_a`, `kit_b`) that each expose a row with the
//! SAME exposed key (`dup`) MUST have DISTINCT inherited identities — the module
//! instance component keeps them apart. The expected result is therefore
//! externally deducible from §13.9 alone, independent of the implementation.
//!
//! This is the one seam the landed §13.9 aggregation path
//! (`ModuleHost::root_view`, exercised passing in `module_visibility.rs`) leaves
//! untested: every existing aggregation case uses DISTINCT keys across instances,
//! so an identity collision never surfaces.

mod support;

use liasse_runtime::{
    Engine, InstallRequest, ModuleHost, ModuleSpace, PatchOp, Value, ViewDelta, ViewQuery,
};
use liasse_store::{MemoryStore, MemoryStoreFactory};
use liasse_value::Text;
use support::generator;

/// Root package: a root-level `$modules` space `modules` declaring a `templates`
/// interface, and a `catalog` view that aggregates it across every installed
/// instance (§13.9) — the canonical corpus shape `.modules::templates { module:
/// modules.$key, id, label, $sort: [module, id] }`.
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

/// A child module exposing its `templates` collection through the `templates`
/// interface. Installation `$data` seeds rows onto a fresh child (the passing
/// `installation_data_overlay_seeds_child_rows_visible_in_the_aggregation` shape).
const CHILD: &str = r#"{
  "$liasse": 1
  "$module": "t.tpl@1.0.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text" }
  }
  "$expose": {
    "templates": { "$view": ".templates { id, label }" }
  }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn host() -> ModuleHost<MemoryStoreFactory> {
    let root = Engine::load(
        MemoryStore::new(liasse_ident::InstanceId::new("root")),
        ROOT_LEVEL,
        &mut generator(),
    )
    .expect("root loads");
    ModuleHost::new(MemoryStoreFactory::new(), root)
}

/// Install two instances into the same space, each exposing a template keyed
/// `dup` (same exposed row key, distinct instance and distinct label).
fn host_with_two_dup_instances() -> (ModuleHost<MemoryStoreFactory>, ModuleSpace) {
    let space = ModuleSpace::new("/modules").expect("mount");
    let mut host = host();
    host.install(
        &space,
        InstallRequest::new("kit_a", CHILD).data(r#"{"templates":{"dup":{"label":"A"}}}"#),
        &mut generator(),
    )
    .expect("install kit_a");
    host.install(
        &space,
        InstallRequest::new("kit_b", CHILD).data(r#"{"templates":{"dup":{"label":"B"}}}"#),
        &mut generator(),
    )
    .expect("install kit_b");
    (host, space)
}

#[test]
fn aggregation_same_exposed_key_across_instances_has_distinct_identity() {
    let (host, _space) = host_with_two_dup_instances();

    let result = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");

    // Both instances' rows appear in a one-shot read (the projection keeps both).
    assert_eq!(result.len(), 2, "both instances' exposed rows appear in the aggregation");

    // §13.9: inherited identity = module instance identity + exposed row identity.
    // kit_a and kit_b are DIFFERENT instances, so their two `dup` rows MUST carry
    // DISTINCT identities. If the runtime keys the aggregated rows by the exposed
    // row key alone (dropping the instance component), the identities collide.
    let id_a = result.rows()[0].id().clone();
    let id_b = result.rows()[1].id().clone();
    assert_ne!(
        id_a, id_b,
        "§13.9 violated: rows from distinct instances (kit_a, kit_b) exposing the same key `dup` \
         collapsed to a single identity {id_a:?} — the module instance component is missing from \
         the inherited identity"
    );
}

#[test]
fn live_view_delta_distinguishes_rows_of_distinct_instances() {
    // The observable §12.2 consequence of the §13.9 identity collision: a live
    // watch computes its patch as the ordered op sequence between two frontiers
    // (`ViewDelta::between`), keyed on identity. Disabling kit_b removes ITS `dup`
    // row from the aggregation (§13.12); the correct patch is exactly one `remove`
    // of an identity distinct from kit_a's, and NOTHING touching kit_a (no insert,
    // move, or update).
    let (mut host, space) = host_with_two_dup_instances();

    let before = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(before.len(), 2);

    host.disable(&space, "kit_b").expect("disable kit_b");
    let after = host.root_view("catalog", &ViewQuery::new()).expect("view").expect("declared");
    assert_eq!(after.len(), 1, "only kit_a's row remains after disabling kit_b");
    assert_eq!(after.rows()[0].field("module"), Some(&text("kit_a")));
    let kit_a_id = after.rows()[0].id().clone();

    let delta = ViewDelta::between(Some(&before), &after);
    match delta {
        ViewDelta::Patch(ops) => {
            // kit_b's `dup` row left the aggregation -> exactly one operation, a
            // `remove`. More than one op, or any op targeting kit_a's identity,
            // would mean the two `dup` rows shared one identity (§13.9 collision):
            // kit_a's row would be diffed against kit_b's `dup=B` and spuriously
            // updated/moved instead of kit_b simply removed.
            assert_eq!(ops.len(), 1, "§12.2/§13.9: exactly one operation, got {ops:?}");
            match &ops[0] {
                PatchOp::Remove { id } => assert_ne!(
                    *id, kit_a_id,
                    "§13.9: the removal must target kit_b's `dup`, not kit_a's — a shared \
                     identity would hide kit_b's removal behind kit_a's surviving row"
                ),
                other => panic!(
                    "§12.2/§13.9: disabling kit_b must be a single `remove` of its `dup` row, \
                     got {other:?}"
                ),
            }
        }
        other => panic!("expected a patch delta between the two frontiers, got {other:?}"),
    }
}
