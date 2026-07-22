//! HARNESS un-vacuum guard (W4-F8): a `blob_get` step's asserted `holders`
//! (§18.8 fetch-plan order) and `bytes` (§18.9 served content) are actually
//! COMPARED against the fetch observation, not parsed and ignored.
//!
//! Before the fix, `report.rs::check_value` judged only `outcome`/`value`; a
//! `blob_get` expectation's `holders`/`bytes` (carried in `Expect::extra`) were
//! never read, so a case asserting them passed vacuously — a wrong served-content
//! or serve-order value would have been accepted. The observation now records the
//! served `bytes` and the `$serve`-ordered `holders`, and `check_expectation`
//! compares both.
//!
//! Externally deducible: `$in = $all[a, b]` with no `$serve` places a verified
//! copy in each and (§18.4) the flattened serve order is `[a, b]`; a fetch of the
//! uploaded content returns exactly those bytes. So the CORRECT assertion
//! `holders: [a, b], bytes: <content>` must pass, while a WRONG order `[b, a]`, a
//! WRONG content, or an absent holder MUST now fail — proving the comparison is
//! live. (If any wrong-assertion probe PASSED, the harness would still be
//! vacuous.)
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// $in = $all[a, b]: a verified copy in each store, flattened serve order [a, b].
const APP: &str = r##"{
  format: 1
  name: blob-get-holders-bytes-asserted
  suite: scenario
  spec: ["#blobs", "§18.8", "§18.9"]
  package: {
    $liasse: 1
    $app: "t.blobs.holdersguard@1.0.0"
    $model: {
      stores: { $key: "id", id: "text", connector: "text", enabled: "bool = true" }
      docs: {
        $key: "id"
        $blob_storage: { $in: { $all: ["/stores['a']", "/stores['b']"] } }
        id: "text"
        file: { $type: "blob", $max_bytes: "10485760", $media: ["text/plain"] }
      }
      $mut: { add: [ "doc = .docs + { id: @id, file: @file }", "return doc { id }" ] }
      one: { $view: ".docs[@id] { id, file }" }
      $public: { docs: { $view: ".one", $mut: { add: ".add" } } }
    }
    $data: { stores: { a: { connector: "fs-a" }, b: { connector: "fs-b" } } }
  }
  hosts: { connectors: {
    "fs-a": { capabilities: ["stream_upload", "stream_download", "checksum"] }
    "fs-b": { capabilities: ["stream_upload", "stream_download", "checksum"] }
  } }
  steps: STEPS
}"##;

const PUT: &str = r##"{ connect: "c1" }
{ blob_put: { call: "public.docs.add", param: "file", args: { id: "d1" },
    content: "guarded", media: "text/plain", on: "c1" },
  expect: { outcome: ok, value: { id: "d1" } } }"##;

fn run(get_expect: &str) -> CaseResult {
    let steps = format!(
        "[ {PUT}\n{{ blob_get: {{ surface: \"public.docs\", args: {{ id: \"d1\" }}, at: \".file\", on: \"c1\" }},\n  expect: {get_expect} }} ]"
    );
    let text = APP.replace("STEPS", &steps);
    let case = Case::from_hjson(&text, Path::new("<blob-get-holders-bytes-asserted>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("18-blobs"), SuiteKind::Red, &case)
}

/// The last step (the `blob_get`) is index 2 (connect=0, blob_put=1).
fn get_step_passes(result: &CaseResult) -> bool {
    result.steps.get(2).is_some_and(|step| step.result.is_pass())
}

/// CORRECT assertion: flattened serve order `[a, b]` and the exact served bytes.
/// Passes, establishing the fetch produces those holders/bytes.
#[test]
fn correct_holders_and_bytes_pass() {
    let result = run(r##"{ outcome: ok, holders: ["a", "b"], bytes: "guarded" }"##);
    for (index, step) in result.steps.iter().enumerate() {
        assert!(step.result.is_pass(), "step {index} did not pass: {:?}", step.result);
    }
}

/// WRONG serve ORDER `[b, a]`: if the harness compared `holders`, this fails.
/// A pass here would mean the assertion is still ignored (vacuous).
#[test]
fn wrong_holder_order_now_fails() {
    let result = run(r##"{ outcome: ok, holders: ["b", "a"], bytes: "guarded" }"##);
    assert!(
        !get_step_passes(&result),
        "a wrong `holders` order must fail now that the harness compares it (§18.8); \
         a pass means the assertion is still vacuous: {:?}",
        result.steps.get(2).map(|s| &s.result),
    );
}

/// WRONG served content: if the harness compared `bytes`, this fails.
#[test]
fn wrong_bytes_now_fails() {
    let result = run(r##"{ outcome: ok, holders: ["a", "b"], bytes: "tampered" }"##);
    assert!(
        !get_step_passes(&result),
        "a wrong served `bytes` must fail now that the harness compares it (§18.9): {:?}",
        result.steps.get(2).map(|s| &s.result),
    );
}
