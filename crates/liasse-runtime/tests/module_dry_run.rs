#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! §20.4 prepared MODULE updates: a §13.14 single-instance update computed in full
//! but not applied.
//!
//! The two properties that make a dry run worth trusting, here on the module path:
//!
//! 1. a dry run of a module update that would be REJECTED reports exactly the
//!    rejection the effecting update reports — same variant, same rendered
//!    diagnostic — for the §20.1 admission suite *and* for the §13.14
//!    exposed-surface recheck that is the module path's own gate;
//! 2. a dry run of a VALID module update leaves the instance byte-identical — the
//!    packed instance boundary before and after is the same bytes, so nothing was
//!    applied.
//!
//! Plus the module staleness rule (§20.4): a plan whose MOUNT basis has moved — the
//! instance committed, or the address now mounts a different incarnation — is
//! refused by `apply_update`, and refusing it commits nothing.

mod support;

use liasse_runtime::{
    CallOutcome, CallRequest, Engine, InstallRequest, ModuleError, ModuleHost, Value,
};
use liasse_store::{CollectionPath, MemoryStore, MemoryStoreFactory, RowAddress};
use liasse_value::Text;
use support::{generator, TASKS};

/// The installed module: private `templates` state, one seeded row, and an exposed
/// interface projecting `id` and `label`.
const TPL_V1: &str = r#"{
  "$liasse": 1
  "$module": "example.moddryrun@1.0.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text" }
    "$mut": { "add": ".templates + { id: @id, label: @label }" }
  }
  "$data": { "templates": { "std": { "label": "Standard" } } }
  "$expose": { "templates": { "$view": ".templates { id, label }" } }
}"#;

/// A compatible minor release: a defaulted `rank` and one more §13.13 seed row, with
/// the exposed surface preserved. It commits cleanly.
const TPL_V1_1: &str = r#"{
  "$liasse": 1
  "$module": "example.moddryrun@1.1.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text", "rank": "int = 7" }
    "$mut": { "add": ".templates + { id: @id, label: @label }" }
  }
  "$data": { "templates": { "std": { "label": "Standard" }, "extra": { "label": "Extra" } } }
  "$expose": { "templates": { "$view": ".templates { id, label }" } }
}"#;

/// A major release whose `$check` the live `label` "Standard" cannot satisfy, so the
/// §20.1 final admission suite refuses the whole migration. The exposed surface is
/// preserved, so the §13.14 recheck passes it through to the §20 pipeline.
const TPL_V2_REJECTED: &str = r#"{
  "$liasse": 1
  "$module": "example.moddryrun@2.0.0"
  "$model": {
    "templates": {
      "$key": "id"
      "id": "text"
      "label": { "$type": "text", "$check": ["size(.) > 20", "label must be long"] }
    }
  }
  "$expose": { "templates": { "$view": ".templates { id, label }" } }
}"#;

/// A minor release that definitionally narrows the module's own exposed surface —
/// the projected `label` field is gone (§13.14, E.9), so the release is refused
/// before the §20 migration runs at all.
const TPL_V1_1_NARROWED: &str = r#"{
  "$liasse": 1
  "$module": "example.moddryrun@1.1.0"
  "$model": {
    "templates": { "$key": "id", "id": "text", "label": "text" }
  }
  "$expose": { "templates": { "$view": ".templates { id }" } }
}"#;

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn collection() -> CollectionPath {
    support::collection_at("/companies/acme/modules")
}

/// The address of the mounted instance the fixtures migrate.
fn at(name: &str) -> RowAddress {
    support::mount_at("/companies/acme/modules", name)
}

/// A host with one module installed at `kit`, running 1.0.0.
fn host() -> ModuleHost<MemoryStoreFactory> {
    let mut generator = generator();
    let root = Engine::load(MemoryStore::new(liasse_ident::InstanceId::new("root")), TASKS, &mut generator)
        .expect("root loads");
    let mut host = ModuleHost::new(MemoryStoreFactory::new(), root);
    host.install(&collection(), InstallRequest::new("kit", TPL_V1), &mut generator).expect("install");
    host
}

/// The mounted instance's whole exported boundary — active definition, committed
/// state, history index — as bytes.
fn packed(host: &ModuleHost<MemoryStoreFactory>) -> Vec<u8> {
    host.pack_instance(&at("kit")).expect("pack the mounted instance")
}

/// The `major.minor.patch` the instance is running, read through its packed
/// boundary's report path: the §13.15 `$from` an update would report next.
fn running_version(host: &ModuleHost<MemoryStoreFactory>) -> String {
    let mut generator = generator();
    // Preparing is effect-free (`&self`), so this reads the version without moving
    // the instance — a prepare of the release already in force reports it as `$from`.
    host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("plan computes").from().to_owned()
}

/// Property 2: a dry run of a VALID module update applies nothing.
///
/// The mounted instance's exported boundary is compared byte for byte across the
/// prepare. Anything the prepare had written — a migrated row, a §13.13 seed
/// insert, the new active definition — would move those bytes.
#[test]
fn dry_run_of_a_valid_module_update_leaves_the_instance_byte_identical() {
    // `host` is NOT `mut`: every call this test makes — the prepare included —
    // takes `&self`, so the compiler is the first witness that a dry run applies
    // nothing.
    let host = host();
    let mut generator = generator();

    let before = packed(&host);

    // `prepare_update` takes `&self`: preparing cannot mutate the mounted instance,
    // and the plan is dropped here, which is exactly what a dry run is.
    let plan = host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("plan computes");
    // The plan describes the update it did not do: 1.0.0 -> 1.1.0, the exposed
    // surface unchanged, and the §13.13 seed row the migration would insert.
    assert_eq!(plan.from(), "1.0.0");
    assert_eq!(plan.target().version.minor, 1, "the plan would adopt 1.1.0");
    assert_eq!(plan.exposed_unchanged(), vec!["templates".to_owned()]);
    assert!(plan.exposed_removed().is_empty(), "nothing is withdrawn");
    assert!(
        plan.instance_update().seeded().iter().any(|path| path.contains("extra")),
        "the plan reports the seed row it would insert: {:?}",
        plan.instance_update().seeded(),
    );
    drop(plan);

    assert_eq!(packed(&host), before, "a dry run leaves the instance byte-identical");
    // And the instance is still the one it was: 1.0.0 is in force.
    assert_eq!(running_version(&host), "1.0.0");
}

/// Property 1a: the dry run of a §20.1-rejected module update reports exactly the
/// rejection the effecting update reports.
///
/// Both diagnostics come from the same `prepare_update` call, which is the point —
/// `ModuleHost::update` IS `prepare_update` + `apply_update`, so a divergence is not
/// merely unlikely, it is unrepresentable.
#[test]
fn dry_run_reports_exactly_the_rejection_the_real_module_update_produces() {
    let mut host = host();
    let mut generator = generator();

    let dry = host.prepare_update(&at("kit"), TPL_V2_REJECTED, &mut generator).expect_err("rejected");
    let real = host.update(&at("kit"), TPL_V2_REJECTED, &mut generator).expect_err("rejected");

    assert_eq!(dry.to_string(), real.to_string(), "the dry run reports the update's own diagnostic");
    assert!(dry.to_string().contains("migration rejected"), "{dry}");
    // §20.3/E.9: the prior release stays in force after both.
    assert_eq!(running_version(&host), "1.0.0");
}

/// Property 1b: the same, for the §13.14 exposed-surface recheck — the gate the
/// MODULE path adds on top of §20. It is inside the prepare, not bolted onto the
/// apply, so a dry run catches a narrowing release too.
#[test]
fn dry_run_reports_the_1314_narrowing_refusal_the_module_update_produces() {
    let mut host = host();
    let mut generator = generator();

    let dry = host.prepare_update(&at("kit"), TPL_V1_1_NARROWED, &mut generator).expect_err("narrowing");
    let real = host.update(&at("kit"), TPL_V1_1_NARROWED, &mut generator).expect_err("narrowing");

    assert!(matches!(dry, ModuleError::ExposedNarrowed(_)), "{dry}");
    assert!(matches!(real, ModuleError::ExposedNarrowed(_)), "{real}");
    assert_eq!(dry.to_string(), real.to_string());
    assert_eq!(running_version(&host), "1.0.0");
}

/// A rejected dry run touches nothing at all — not even the bytes a successful one
/// leaves alone.
#[test]
fn a_rejected_module_dry_run_leaves_the_instance_untouched() {
    let host = host();
    let mut generator = generator();

    let before = packed(&host);
    host.prepare_update(&at("kit"), TPL_V2_REJECTED, &mut generator).expect_err("rejected");
    assert_eq!(packed(&host), before, "a rejected dry run applies nothing");
}

/// `update` IS prepare + apply: preparing then applying commits the same §13.15
/// report and leaves the same boundary as calling `update` directly.
#[test]
fn preparing_then_applying_is_the_module_update() {
    // Each host draws from its OWN fresh generator, so the two runs differ in
    // nothing but whether the update was taken in one call or two.
    let mut direct = host();
    let via_update = direct.update(&at("kit"), TPL_V1_1, &mut generator()).expect("update commits");

    let mut staged = host();
    let plan = staged.prepare_update(&at("kit"), TPL_V1_1, &mut generator()).expect("plan computes");
    let via_plan = staged.apply_update(plan).expect("apply commits");

    assert_eq!(via_plan, via_update, "the same §13.15 report either way");
    assert_eq!(packed(&staged), packed(&direct), "and the same committed boundary");
}

/// §20.4 staleness on the instance half of the mount basis: a commit against the
/// child between prepare and apply invalidates the plan, and refusing it commits
/// nothing.
#[test]
fn applying_a_module_plan_whose_instance_moved_is_refused_and_commits_nothing() {
    let mut host = host();
    let mut generator = generator();

    let plan = host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("plan computes");

    // A child mutation commits, so the instance is no longer at the position the
    // plan was computed against.
    let request = CallRequest::new("add").arg("id", text("t2")).arg("label", text("Second"));
    let outcome = host.child_call(&at("kit"), &request, &mut generator).expect("child call");
    assert!(matches!(outcome, CallOutcome::Committed { .. }));

    let before = packed(&host);
    let refused = host.apply_update(plan).expect_err("a stale plan must not commit");
    match &refused {
        ModuleError::Stale { prepared, current } => {
            assert_eq!(prepared.at(), current.at(), "the same mount, a moved position");
            assert_ne!(prepared.instance().head(), current.instance().head());
        }
        other => panic!("expected a §20.4 stale refusal, got {other}"),
    }
    assert_eq!(packed(&host), before, "refusing a stale module plan commits nothing");

    // Re-preparing against the current position applies.
    let fresh = host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("re-prepare");
    host.apply_update(fresh).expect("the re-prepared plan applies");
    assert_eq!(running_version(&host), "1.1.0");
}

/// §20.4 staleness on the MOUNT half — the module path's own rule. Uninstalling and
/// re-installing under the same name mounts a DIFFERENT incarnation (D.1), so the
/// plan no longer describes the instance living at that address and must not commit
/// into it.
#[test]
fn applying_a_module_plan_after_the_mount_was_reinstalled_is_refused() {
    let mut host = host();
    let mut generator = generator();

    let plan = host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("plan computes");

    host.uninstall(&at("kit")).expect("uninstall");
    host.install(&collection(), InstallRequest::new("kit", TPL_V1), &mut generator).expect("reinstall");

    let before = packed(&host);
    let refused = host.apply_update(plan).expect_err("another incarnation's plan must not apply");
    assert!(matches!(refused, ModuleError::Stale { .. }), "{refused}");
    assert_eq!(packed(&host), before, "the freshly mounted instance is untouched");
    assert_eq!(running_version(&host), "1.0.0");
}

/// A plan is pinned to the entry it was prepared against: renaming the instance
/// (§13.3 rekey) moves the mount, and the plan's address no longer names one.
#[test]
fn applying_a_module_plan_after_the_mount_was_renamed_is_refused() {
    let mut host = host();
    let mut generator = generator();

    let plan = host.prepare_update(&at("kit"), TPL_V1_1, &mut generator).expect("plan computes");
    host.rename(&at("kit"), "toolkit").expect("rename");

    let refused = host.apply_update(plan).expect_err("the plan's mount is gone");
    assert!(matches!(refused, ModuleError::Unknown(_)), "{refused}");
    // The renamed instance never moved off its release.
    let renamed = host.prepare_update(&at("toolkit"), TPL_V1_1, &mut generator).expect("plan computes");
    assert_eq!(renamed.from(), "1.0.0");
}
