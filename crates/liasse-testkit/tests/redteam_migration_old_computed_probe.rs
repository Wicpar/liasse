//! RED-TEAM probe: a §20.1 migration program cannot read a computed value from
//! `$old`, though §20.1 says it "MAY read any `$old` view".
//!
//! §20.1: "`$old` is the complete read-only state under the delta's source model.
//! … it MAY read any `$old` view and MUST use deterministic pure functions."
//!
//! A computed value (§5.2) is part of the readable source state — the 1.0.0 model
//! declares `doubled = qty * 2`. A 2.0.0 migration program
//! `.mirror = $old.items { id, d: .doubled }` reads that source view and MUST
//! build `mirror` rows with `d = qty*2`.
//!
//! Root cause: `build_migrated` materializes `$old` from the source model's STORED
//! collections only (crates/liasse-runtime/src/migrate.rs `old_working` /
//! `materialize_all`), not folding the source model's computed values (§5.2) or
//! views (§7). The `$old` row TYPE still carries `doubled`, so the program type-
//! checks, but at runtime `$old.items.doubled` evaluates to `none`. Two failure
//! modes result, neither spec-conformant:
//!
//!   * REQUIRED target (`mirror.d: int`): `none` leaves the required field
//!     unpopulated, so the whole update is REJECTED — a valid §20.1 program fails.
//!   * OPTIONAL target (`mirror.d: int?`): the update ADMITS with `d` silently
//!     `none` instead of `10` — SILENT DATA LOSS of the computed value.
//!
//! The CONTROL migration that reads the STORED `.qty` admits and yields `d = 5`,
//! isolating the defect to reading a computed `$old` view.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// The 1.0.0 base package (loaded as the root). `READ` and the host_load target are
// substituted per-case so the 2.0.0 target is inline (the adapter's host_load does
// not resolve a `packages` label).
const APP: &str = r##"{
  format: 1
  name: migration-old-computed-probe
  suite: scenario
  spec: ["#evolution", "§20.1"]
  package: {
    $liasse: 1
    $app: "t.mig.oldcomputed@1.0.0"
    $model: {
      items: {
        $key: "id"
        id: "text"
        qty: "int"
        doubled: "= qty * 2"
      }
      $public: { items: { $view: ".items { id, qty, doubled }" } }
    }
    $data: { items: { i1: { qty: "5" } } }
  }
  steps: STEPS
}"##;

// The inline 2.0.0 target whose migration reads `READ` (`.qty` or `.doubled`) into
// a `DTYPE`-typed `mirror.d`.
const V2: &str = r##"{
  $liasse: 1
  $app: "t.mig.oldcomputed@2.0.0"
  $model: {
    mirror: { $key: "id", id: "text", d: "DTYPE" }
    $migrations: { "1.0.0": [ ".mirror = $old.items { id, d: READ }" ] }
    $public: { mirror: { $view: ".mirror { id, d, $sort: [id] }" } }
  }
}"##;

fn run_typed(read: &str, dtype: &str, expect_d: &str) -> CaseResult {
    let steps = format!(
        r##"[
          {{ host_load: {{ package: {v2} }}, expect: {{ outcome: ok }} }}
          {{ watch: "public.mirror", id: "w1",
            expect_init: {{ value: [ {{ id: "i1", d: "{d}" }} ] }} }}
        ]"##,
        v2 = V2.replace("READ", read).replace("DTYPE", dtype),
        d = expect_d,
    );
    let text = APP.replace("STEPS", &steps);
    let case = Case::from_hjson(&text, Path::new("<migration-old-computed-probe>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("20-evolution-migrations"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

/// CONTROL: reading the STORED `.qty` from `$old` migrates cleanly (`d = 5`),
/// proving the migration scaffolding, `$old` binding, and `mirror` build are sound.
#[test]
fn control_migration_reads_old_stored_field() {
    assert_all_pass(&run_typed(".qty", "int", "5"));
}

/// THE FINDING (required target): reading the COMPUTED `.doubled` from `$old` must
/// migrate cleanly (`d = 10`, §20.1 "MAY read any `$old` view"). Because `$old`
/// carries no computed values, `.doubled` evaluates to `none`, the required
/// `mirror.d` is left unpopulated, and the update is REJECTED.
#[test]
fn finding_migration_reads_old_computed_value_required() {
    assert_all_pass(&run_typed(".doubled", "int", "10"));
}

/// THE FINDING (optional target, worse): with `mirror.d` OPTIONAL, the missing
/// `$old` computed value does not even reject — the migration ADMITS with `d`
/// silently `none` instead of the mandated `10`. §20.1 requires the value to be
/// readable; instead the migrated state silently loses it. This asserts the
/// spec-correct `d = 10`; the observed migrated `mirror` row carries no `d`.
#[test]
fn finding_migration_reads_old_computed_value_optional_silent_loss() {
    assert_all_pass(&run_typed(".doubled", "int?", "10"));
}
