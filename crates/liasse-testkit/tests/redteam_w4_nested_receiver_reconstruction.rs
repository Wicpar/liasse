//! RED-TEAM (WAVE 4) — TARGET 7: a surface `$mut` naming a DEPTH-≥2 nested
//! receiver reconstructs a receiver key that DROPS every ancestor selector, so a
//! call the runtime would admit is rejected `Malformed` by the harness.
//!
//! SPEC §10.1: a declared row-mutation reference "MUST select exactly one
//! receiver before naming the mutation" and "the surface parameters are the
//! selector parameters combined with the referenced mutation's parameters". For
//! a mutation declared on a NESTED collection, the receiver row's canonical key
//! is its full ancestor path (§8.2): `.companies[@company].accounts[@account]`
//! selects the account row by the 2-component key `[company, account]`.
//!
//! The harness reconstructs that receiver in `adapter/router.rs::receiver_args`.
//! The bug: its `Select { Keys(keys), base }` arm returned only THIS selector's
//! `key_params` and never recursed into `base`, so
//! `.companies[@company].accounts[@account]` reconstructed the receiver `[account]`
//! (dropping `@company`) — a 1-component key for a 2-level path — and the runtime
//! rejected it `Malformed` before any mutation logic ran.
//!
//! Externally deducible: seed holds `companies[co].accounts[a1]`; §10.1/§8.2 make
//! `add_note` a legal receiver-bound mutation on that account, so a call carrying
//! `{ company: co, account: a1, text: … }` MUST admit and return the new note.
//! There is NO meter here — admission is purely the receiver-key reconstruction.
//! A CONTROL exercises a depth-1 (root-collection) receiver to show single-level
//! reconstruction was never broken; the PROBE exercises the depth-2 receiver.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// A company holding accounts, and each account holding a `notes` sub-collection.
// `add_note` is a receiver-bound mutation on the (nested) `accounts` collection;
// `log` is the depth-1 control, a receiver-bound mutation on the (root)
// `companies` collection that appends to a company-level `logs` sub-collection.
// Both use the identical known-good append-then-return body, so the ONLY variable
// between control and probe is the receiver depth. NO meters: the only thing
// under test is receiver-key reconstruction.
const APP: &str = r##"{
  format: 1
  name: w4-nested-receiver-reconstruction
  suite: scenario
  spec: ["#interfaces", "§10.1", "#state", "§8.2"]
  package: {
    $liasse: 1
    $app: "t.rt.nestrecv@1.0.0"
    $model: {
      companies: {
        $key: "id"
        id: "text"
        logs: {
          $key: "id"
          id: "uuid = uuid()"
          text: "text"
        }
        accounts: {
          $key: "id"
          id: "text"
          notes: {
            $key: "id"
            id: "uuid = uuid()"
            text: "text"
          }
          $mut: {
            add_note: [
              "note = .notes + { text: @text }"
              "return note { id }"
            ]
          }
        }
        $mut: {
          log: [
            "entry = .logs + { text: @text }"
            "return entry { id }"
          ]
        }
      }
      $public: {
        desk: {
          $view: ".companies { id, accounts: .accounts { id } }"
          $mut: {
            log: ".companies[@company].log"
            add_note: ".companies[@company].accounts[@account].add_note"
          }
        }
      }
    }
    $data: { companies: { co: { accounts: { a1: {} } } } }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case =
        Case::from_hjson(&text, Path::new("<w4-nested-receiver-reconstruction>"), &BTreeSet::new())
            .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("10-interfaces-roles"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, label: &str) {
    for (index, step) in result.steps.iter().enumerate() {
        assert!(
            step.result.is_pass(),
            "[{label}] step {index} did not pass: observed={:?} result={:?}",
            step.observed,
            step.result
        );
    }
}

/// PASSING CONTROL: a DEPTH-1 receiver `.companies[@company].log` addresses the
/// root-collection row by its single-component key `[company]`. Single-level
/// reconstruction was never broken, so this establishes the surface/call path is
/// sound and the probe's only variable is the ancestor selector.
#[test]
fn depth_one_receiver_admits() {
    let result = run(
        r##"[
          { call: "public.desk.log", args: { company: "co", text: "opened" },
            expect: { outcome: ok, value: { id: "$any:uuid" } } }
        ]"##,
    );
    assert_all_pass(&result, "depth-1-control");
}

/// THE PROBE (§10.1/§8.2): a DEPTH-2 receiver
/// `.companies[@company].accounts[@account].add_note` MUST admit — the account row
/// is addressed by the full key `[co, a1]`. On the buggy harness `receiver_args`
/// dropped `@company`, reconstructing `[a1]` (a 1-component key for the 2-level
/// path), and the runtime rejected it `Malformed`; the fix collects the ancestor
/// selector params in order, so the call admits and returns the new note.
#[test]
fn depth_two_nested_receiver_admits() {
    let result = run(
        r##"[
          { call: "public.desk.add_note", args: { company: "co", account: "a1", text: "hi" },
            expect: { outcome: ok, value: { id: "$any:uuid" } } }
        ]"##,
    );
    assert_all_pass(&result, "depth-2-probe");
}
