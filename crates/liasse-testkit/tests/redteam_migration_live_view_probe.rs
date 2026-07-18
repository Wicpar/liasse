//! RED-TEAM cross-cutting probe (round 2): a §20 migration committed WHILE a live
//! view is watching (§12.2). Two seams:
//!
//! - A value-transforming migration on a SURVIVING surface: §9.3 makes the
//!   definition update a commit, so §12.2 must re-evaluate the live view at that
//!   outgoing frontier and reflect the migrated value.
//! - A migration that RENAMES the watched collection away (its surface disappears):
//!   §12.2 (line 1709) — "When the current state removes that subscription's
//!   authority or surface, the runtime emits `close`." The watcher on the vanished
//!   surface MUST receive `close(frontier, reason)`.
//!
//! Every §20-migration corpus case opens its watch AFTER `host_load`; none holds a
//! live subscription ACROSS the definition change, so the §20 × §12.2 coherence is
//! untested.
//!
//! Expectations are deducible from SPEC.md text alone (§9.3, §12.2, §20.1).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(case_text: &str, name: &str) -> CaseResult {
    let case = Case::from_hjson(case_text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

fn assert_steps_pass(result: &CaseResult, upto: usize, context: &str) {
    for index in 0..upto {
        let step = result
            .steps
            .get(index)
            .unwrap_or_else(|| panic!("{context}: no step {index}: {:?}", result.steps));
        assert!(
            step.result.is_pass(),
            "{context}: step {index} ({}) did not pass: {:?}",
            step.action,
            step.result
        );
    }
}

/// §9.3 × §12.2 × §20.1: a live watch on `public.people` held across a value-
/// transforming migration (`name` -> `display_name` via `string.upper`). The
/// definition update is a commit (§9.3); the live view MUST re-evaluate at that
/// frontier. A fresh read of the surviving surface after the migration shows the
/// migrated shape and value.
#[test]
fn live_view_reflects_value_transforming_migration() {
    let case = r##"{
      format: 1
      name: migration-live-transform
      suite: scenario
      spec: ["#loading", "§9.3", "#clients", "§12.2", "#evolution", "§20.1"]
      package: {
        $liasse: 1
        $app: "t.mlv@1.0.0"
        $model: {
          people: { $key: "id", id: "text", name: "text" }
          $public: { people: { $view: ".people { id, name, $sort: [id] }" } }
        }
        $data: { people: { p1: { name: "bob" } } }
      }
      steps: [
        { watch: "public.people", id: "w1",
          expect_init: { value: [ { id: "p1", name: "bob" } ] } }
        { host_load: { package: {
            $liasse: 1
            $app: "t.mlv@2.0.0"
            $model: {
              people: { $key: "id", id: "text",
                display_name: { $type: "text", $from: "name", $as: "string.upper(.)" } }
              $public: { people: { $view: ".people { id, display_name, $sort: [id] }" } }
            }
          } }, expect: { outcome: ok, result: committed } }
        { watch: "public.people", id: "w2",
          expect_init: { value: [ { id: "p1", display_name: "BOB" } ] } }
      ]
    }"##;
    let result = run(case, "migration-live-transform");
    assert_steps_pass(&result, 3, "live view reflects value-transforming migration");
}

/// §12.2 (line 1709): a migration that RENAMES the watched collection away removes
/// the subscription's surface, so the runtime MUST emit `close`. The watcher on the
/// now-absent `public.customers` must receive `close(frontier, reason)`.
#[test]
fn watcher_on_renamed_away_surface_receives_close() {
    let case = r##"{
      format: 1
      name: migration-renames-watched-surface
      suite: scenario
      spec: ["#clients", "§12.2", "#evolution", "§20.1", "#loading", "§9.3"]
      package: {
        $liasse: 1
        $app: "t.mrn@1.0.0"
        $model: {
          customers: { $key: "id", id: "text", name: "text" }
          $public: { customers: { $view: ".customers { id, name, $sort: [id] }" } }
        }
        $data: { customers: { c1: { name: "Acme" } } }
      }
      steps: [
        { watch: "public.customers", id: "w1",
          expect_init: { value: [ { id: "c1", name: "Acme" } ] } }
        { host_load: { package: {
            $liasse: 1
            $app: "t.mrn@2.0.0"
            $model: {
              clients: { $from: "customers", $key: "id", id: "text", name: "text" }
              $public: { clients: { $view: ".clients { id, name, $sort: [id] }" } }
            }
          } }, expect: { outcome: ok, result: committed } }
        // §12.2: the watched surface public.customers no longer exists -> close.
        { expect_close: { watch: "w1", reason: "$any" } }
      ]
    }"##;
    let result = run(case, "migration-renames-watched-surface");
    assert_steps_pass(&result, 3, "watcher on renamed-away surface receives close");
}
