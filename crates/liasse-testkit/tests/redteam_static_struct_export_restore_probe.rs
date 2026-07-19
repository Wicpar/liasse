//! RED-TEAM probe of the §19.5 portable-state capture at the EXACT boundary the
//! convergence coordinator flagged: a depth-1 top-level collection whose row
//! carries a nested §5.3 STATIC STRUCT must still export and restore, even though
//! the `capture` path fails closed on genuine depth>1 nested KEYED collections
//! (`portable.rs`, `StateSection::capture` guard `address.depth() > 1`).
//!
//! A static struct is part of the row VALUE (a `Node::Struct`, never a
//! `Node::Collection`), so `Prospective::gather_tree` does NOT address it as a
//! separate row — every such row stays at `depth() == 1`. The guard must not fire,
//! and `StateSection::row_type` must chain `collection.structs` into the decode
//! type so the struct member (and its OMITTED OPTIONAL member, dropped from the
//! wire by absence, A.1) round-trips exactly. This is the acknowledged
//! fail-closed boundary's PASSING side — a regression here (guard over-fires, or
//! the struct member is dropped/mis-decoded) is a real §19.10 restore bug, not the
//! acknowledged nested-collection refusal.
//!
//! Every expectation is externally deducible from SPEC.md: §5.3 (a static struct
//! shares the row's identity/lifecycle), §5.1 (defaults resolve during the insert),
//! §19.10/§19.2 (restore reproduces the same owned logical state), A.1 (`none` is
//! absence on the wire; a `decimal`'s canonical text is minimal scale). The seed
//! is the externally-known input; the restored view must reproduce it.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// A top-level collection `orders` whose row carries a nested static struct
// `address` (itself holding an OMITTED optional `line2` and a defaulted
// `country`) plus a scale-bearing `decimal` inside the struct (`tax`). A second
// collection `notes` makes the app non-trivial and forces the codec to key more
// than one collection through the same portable object. Nothing here is a nested
// KEYED collection, so every row is depth-1 and the capture guard must not fire.
const APP: &str = r##"{
  format: 1
  name: static-struct-export-restore-probe
  suite: scenario
  spec: ["#history", "§19.10", "§19.2", "§5.3", "§5.1"]
  package: {
    $liasse: 1
    $app: "t.hist.sstruct@1.0.0"
    $model: {
      orders: {
        $key: "id"
        id: "text"
        address: {
          line1: "text"
          line2: "text?"
          city: "text"
          country: "text = 'FR'"
          tax: "decimal"
        }
      }
      notes: {
        $key: "id"
        id: "text"
        body: "text"
      }
      $public: {
        orders: { $view: ".orders { id, address, $sort: [id] }" }
        notes: { $view: ".notes { id, body, $sort: [id] }" }
      }
    }
    $data: {
      // Every declared struct member supplied explicitly (seed does not resolve
      // omitted static-struct-member defaults — a documented seam, seed.rs). o1
      // supplies the optional `line2`; o2 OMITS it, so the optional-none
      // round-trip through the portable codec's optional wrapper is exercised.
      orders: {
        o1: { address: { line1: "1 Main St", line2: "Apt 4", city: "Paris", country: "FR", tax: "1.50" } }
        o2: { address: { line1: "9 Rue X", city: "Lyon", country: "US", tax: "2.00" } }
      }
      notes: {
        n1: { body: "hello" }
      }
    }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    // `export` / `in_sandbox` / `restore` are the §19 chapter-local step keys
    // documented in tests/19-history-artifacts/NOTES.md.
    let allowed: BTreeSet<String> =
        ["export", "in_sandbox", "restore"].into_iter().map(String::from).collect();
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<static-struct-export-restore-probe>"), &allowed)
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("19-history-artifacts"), SuiteKind::Red, &case)
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

/// PASSING CONTROL: the live view (no export) reproduces the seeded static-struct
/// rows, including the omitted optional `line2` read as absent and the defaulted
/// `country`. Establishes the externally-known baseline the restore must match.
#[test]
fn live_view_reproduces_static_struct_rows() {
    let result = run(
        r##"[
          { watch: "public.orders", id: "w1", expect_init: { value: [
            { id: "o1", address: { line1: "1 Main St", line2: "Apt 4", city: "Paris", country: "FR", tax: "1.5" } }
            { id: "o2", address: { line1: "9 Rue X", city: "Lyon", country: "US", tax: "2" } }
          ] } }
        ]"##,
    );
    assert_all_pass(&result);
}

/// THE PROBE: export the whole app, restore it into a fresh sandbox, and assert
/// the static-struct rows reproduce EXACTLY — the omitted optional stays absent,
/// the defaulted `country` is carried, the scale-bearing `decimal` returns at its
/// minimal-scale canonical spelling. A capture-guard over-fire (refusing a
/// depth-1 static-struct row as if it were a nested keyed collection) would make
/// `export` error; a decode-type that omits `collection.structs` would drop or
/// fault the `address` member on `restore`.
#[test]
fn static_struct_rows_survive_export_restore() {
    let result = run(
        r##"[
          { export: { as: "a1" }, expect: { outcome: ok } }
          { in_sandbox: "s1", steps: [
            { restore: { from: "a1" }, expect: { outcome: ok } }
            { watch: "public.orders", id: "wo", expect_init: { value: [
              { id: "o1", address: { line1: "1 Main St", line2: "Apt 4", city: "Paris", country: "FR", tax: "1.5" } }
              { id: "o2", address: { line1: "9 Rue X", city: "Lyon", country: "US", tax: "2" } }
            ] } }
            { watch: "public.notes", id: "wn", expect_init: { value: [
              { id: "n1", body: "hello" }
            ] } }
          ] }
        ]"##,
    );
    assert_all_pass(&result);
}
