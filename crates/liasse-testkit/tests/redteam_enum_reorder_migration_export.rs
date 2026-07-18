//! RED-TEAM cross-cutting probe (Target B round 4): §5.9 enum ordering threaded
//! through a §20.1 schema migration that REORDERS an enum's labels, and then
//! through a §19.5/§19.10 artifact export round-trip.
//!
//! §5.9: "The `$enum` array lists distinct accepted labels and establishes their
//! declaration order ... their default total order follows that order." §B.1: an
//! enum column orders by declaration order. §E.5 makes "changing explicit sort
//! semantics" a breaking change — so an enum reorder is a MAJOR bump (v1 -> v2),
//! which the update path admits (only same-major narrowing is refused, §E.1).
//!
//! After the reorder migration commits, an existing row's enum value keeps the
//! SAME label (§20.1 compatible same-identity copy; the domain is unchanged) but
//! its position in the total order MUST follow the NEW declaration order (§5.9).
//! The internal enum representation is `(ordinal, label)` and
//! `EnumValue::from_parts` (crates/liasse-value/src/enumeration.rs:76) reconstructs
//! a stored value from that pair WITHOUT re-parsing the label against the current
//! `EnumType`. If the migration copies the stored value verbatim, the row carries
//! its v1 ordinal into a v2 world where that ordinal names a different declared
//! position — so a `$sort` by the enum field would order by the STALE ordinal,
//! violating §5.9/§B.1/§22.1.
//!
//! Every expectation below is deducible from SPEC.md text alone; none encodes
//! implementation behavior. Passing CONTROLS isolate the reorder from ordinary
//! migration and from the artifact layer.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// The chapter-local step keys these probes use (§19 sandbox/restore verbs);
/// documented in `tests/19-history-artifacts/NOTES.md` and mirrored here so an
/// inline case parses exactly as the corpus loader would accept it.
fn allowed_keys() -> BTreeSet<String> {
    ["in_sandbox", "restore", "inspect_artifact"].into_iter().map(str::to_owned).collect()
}

fn run(case_text: &str, name: &str) -> CaseResult {
    let case = Case::from_hjson(case_text, Path::new(name), &allowed_keys()).expect("case parses");
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

/// CONTROL — v1 baseline: an enum sorts by its declaration order (§5.9/§B.1).
/// Declared `a < b < c`; rows seeded out of order read sorted by that order.
#[test]
fn control_v1_enum_sorts_by_declaration_order() {
    let case = r##"{
      format: 1
      name: enum-v1-sort-control
      suite: scenario
      spec: ["#state-model", "§5.9", "§B.1"]
      package: {
        $liasse: 1
        $app: "t.enumreord@1.0.0"
        $model: {
          items: { $key: "id", id: "text", status: { $enum: ["a", "b", "c"] } }
          $public: { items: { $view: ".items { id, status, $sort: [status] }" } }
        }
        $data: { items: { i1: { status: "c" }, i2: { status: "a" }, i3: { status: "b" } } }
      }
      steps: [
        { watch: "public.items", id: "w1", expect_init: { value: [
          { id: "i2", status: "a" }
          { id: "i3", status: "b" }
          { id: "i1", status: "c" }
        ] } }
      ]
    }"##;
    let result = run(case, "enum-v1-sort-control");
    assert_steps_pass(&result, 1, "v1 enum declaration-order sort control");
}

/// THE PROBE — §5.9 × §20.1 × §B.1: after a MAJOR-bump migration that reorders the
/// enum labels from `[a,b,c]` to `[c,b,a]`, an existing row's status keeps its
/// LABEL but its total-order position must follow the NEW declaration order. The
/// same public surface `.items { id, status, $sort: [status] }` is declared in both
/// versions, so it resolves against the migrated model; only the enum's declared
/// order differs. Expected sort after v2: `c < b < a`, i.e. [i1(c), i3(b), i2(a)].
/// A stale-ordinal read would keep the v1 order [i2(a), i3(b), i1(c)].
#[test]
fn enum_reorder_migration_resorts_by_new_declaration_order() {
    let case = r##"{
      format: 1
      name: enum-reorder-migration-resort
      suite: scenario
      spec: ["#state-model", "§5.9", "§B.1", "#evolution", "§20.1", "#runtime", "§22.1"]
      package: {
        $liasse: 1
        $app: "t.enumreord@1.0.0"
        $model: {
          items: { $key: "id", id: "text", status: { $enum: ["a", "b", "c"] } }
          $public: { items: { $view: ".items { id, status, $sort: [status] }" } }
        }
        $data: { items: { i1: { status: "c" }, i2: { status: "a" }, i3: { status: "b" } } }
      }
      steps: [
        // CONTROL: v1 order a<b<c before the migration.
        { watch: "public.items", id: "w0", expect_init: { value: [
          { id: "i2", status: "a" }
          { id: "i3", status: "b" }
          { id: "i1", status: "c" }
        ] } }
        // MAJOR bump reorders the enum to c<b<a; the surface is byte-identical.
        { host_load: { package: {
            $liasse: 1
            $app: "t.enumreord@2.0.0"
            $model: {
              items: { $key: "id", id: "text", status: { $enum: ["c", "b", "a"] } }
              $public: { items: { $view: ".items { id, status, $sort: [status] }" } }
            }
          } }, expect: { outcome: ok, result: committed } }
        // §5.9: the new declaration order c<b<a governs the sort; labels unchanged.
        { watch: "public.items", id: "w1", expect_init: { value: [
          { id: "i1", status: "c" }
          { id: "i3", status: "b" }
          { id: "i2", status: "a" }
        ] } }
      ]
    }"##;
    let result = run(case, "enum-reorder-migration-resort");
    assert_steps_pass(&result, 3, "enum reorder migration re-sorts by new order");
}

/// THE PROBE (export leg) — §19.10: restoring an artifact reproduces the same
/// owned logical state. Export AFTER the reorder migration, then restore in a
/// sandbox. The restore path rebuilds the surface router from the BASE (v1)
/// package, so its view `.items { id, status, $sort: [status] }` is byte-identical
/// to v2's; the `$sort` executes against the RESTORED model's enum type. The
/// restored order must therefore match the post-migration order c<b<a — the
/// export must have captured the migrated state, not a stale-ordinal snapshot.
#[test]
fn enum_reorder_survives_export_roundtrip() {
    let case = r##"{
      format: 1
      name: enum-reorder-export-roundtrip
      suite: scenario
      spec: ["#state-model", "§5.9", "§B.1", "#evolution", "§20.1", "#history", "§19.10", "§19.5"]
      package: {
        $liasse: 1
        $app: "t.enumreord@1.0.0"
        $model: {
          items: { $key: "id", id: "text", status: { $enum: ["a", "b", "c"] } }
          $public: { items: { $view: ".items { id, status, $sort: [status] }" } }
        }
        $data: { items: { i1: { status: "c" }, i2: { status: "a" }, i3: { status: "b" } } }
      }
      steps: [
        { host_load: { package: {
            $liasse: 1
            $app: "t.enumreord@2.0.0"
            $model: {
              items: { $key: "id", id: "text", status: { $enum: ["c", "b", "a"] } }
              $public: { items: { $view: ".items { id, status, $sort: [status] }" } }
            }
          } }, expect: { outcome: ok, result: committed } }
        { export: { as: "a2" }, expect: { outcome: ok } }
        { in_sandbox: "s1", steps: [
          { restore: { from: "a2" }, expect: { outcome: ok } }
          { watch: "public.items", id: "w1", expect_init: { value: [
            { id: "i1", status: "c" }
            { id: "i3", status: "b" }
            { id: "i2", status: "a" }
          ] } }
        ] }
      ]
    }"##;
    let result = run(case, "enum-reorder-export-roundtrip");
    assert_steps_pass(&result, 4, "enum reorder survives export round-trip");
}
