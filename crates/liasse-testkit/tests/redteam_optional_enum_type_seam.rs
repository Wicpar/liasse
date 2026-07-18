//! RED-TEAM probe: the expanded-field `$type` value that carries an inline shape
//! (`{ $type: { $enum: [...] }, ... }`) silently loses its declared type.
//!
//! `Builder::expanded_field` (crates/liasse-model/src/build/fields.rs:117-124)
//! handles the `$type` member ONLY when its value is a string
//! (`member.value.as_string()`); an object-valued `$type` such as
//! `{ $enum: [...] }` never matches, so `base_ty` keeps its `Type::Json` default
//! (fields.rs:113) and the field compiles to `json` (or `optional<json>` when
//! `$optional` is set). §5.9 ("An enum is a closed set of checked labels ...
//! Enum values are checked labels") is therefore never enforced on the field: an
//! undeclared label is admitted and stored as an ordinary `json` string.
//!
//! The set-of-enum path (`Builder::shape_or_type`, build/shapes.rs:241-267) DOES
//! accept an inline `{ $enum: [...] }`, so `$set: { $enum: [...] }` type-checks
//! (proven by `05/set-of-enum-reads-in-declaration-order`). The seam is that the
//! expanded `$type` position, the analogous inline-shape slot, does not.
//!
//! # Isolation
//!
//! Four field spellings of the SAME `status` enum, one syntactic axis apart:
//!
//! | spelling                                       | dispatch            | compiled type   |
//! |------------------------------------------------|---------------------|-----------------|
//! | `{ $enum: [...] }`                             | object_node/$enum   | `enum`  (OK)    |
//! | `"Status"` / `{ $type: "Status" }` (named)     | string `$type` path | `enum`  (OK)    |
//! | `{ $type: { $enum: [...] } }`                  | expanded_field      | `json`  (BUG)   |
//! | `{ $type: { $enum: [...] }, $optional: true }` | expanded_field      | `optional<json>`|
//!
//! The two control spellings enforce §5.9 (an out-of-set label rejects); the two
//! inline-`$type` spellings admit it. The only difference between control and bug
//! is inline-object vs string `$type` — exactly fields.rs:118.
//!
//! Every expectation is deducible from SPEC.md text alone (§5.9, plus §8.3
//! parameter inference for the `add` mutation); none encodes implementation
//! behaviour.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

/// One keyed `things` collection with a single `status` enum field, spelled by
/// the injected `FIELD` fragment (and any injected `$types` via `TYPES`). The
/// `add` surface mutation infers `@status` from the field type (§8.3), so an
/// enum field checks the label against its closed set (§5.9) and a `json` field
/// does not. Mirrors the passing corpus case
/// `05/enum-unknown-label-rejected`, changing only the field spelling.
const APP: &str = r##"{
  format: 1
  name: optional-enum-type-seam
  suite: scenario
  spec: ["#state-model", "§5.9"]
  package: {
    $liasse: 1
    $app: "t.optenum@1.0.0"
    TYPES
    $model: {
      things: {
        $key: "id"
        id: "text"
        status: FIELD
      }
      $public: {
        things: {
          $view: ".things { id, status }"
          $mut: {
            add: ".things + { id: @id, status: @status }"
          }
        }
      }
    }
  }
  steps: STEPS
}"##;

/// §5.9: an undeclared label (`"archived"`) must reject the transition; a
/// declared one (`"draft"`) commits and reads back through the surface view.
const REJECT_STEPS: &str = r##"[
  { call: "public.things.add", args: { id: "t1", status: "archived" },
    expect: { outcome: rejected, violates: ["§5.9"] } }
  { call: "public.things.add", args: { id: "t1", status: "draft" },
    expect: { outcome: ok } }
  { watch: "public.things", id: "w1",
    expect_init: { value: [ { id: "t1", status: "draft" } ] } }
]"##;

/// Build the app with `TYPES`/`FIELD`/`STEPS` substituted, then run it in-process
/// against the memory store, exactly as `corpus_scenarios` does.
fn run_field(name: &str, types: &str, field: &str, steps: &str) -> CaseResult {
    let text = APP.replace("TYPES", types).replace("FIELD", field).replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new(name), &BTreeSet::new()).expect("case parses");
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

/// CONTROL — the direct enum field form `{ $enum: [...] }` (§5.9, A.3). Proves
/// the closed-set machinery works end to end; this is the baseline the inline
/// `$type` spelling must match. Must PASS.
#[test]
fn control_direct_enum_rejects_out_of_set_label() {
    let result =
        run_field("direct-enum", "", r##"{ $enum: ["draft", "active", "closed"] }"##, REJECT_STEPS);
    assert_steps_pass(&result, 3, "direct enum enforces its closed set");
}

/// CONTROL — a named enum reached through the STRING `$type` value
/// `{ $type: "Status", $optional: true }` (A.2 named type + A.3 `$type`/`$optional`,
/// §5.8, §5.9). This takes the SAME `expanded_field` path as the bug, but its
/// `$type` value is a string, so `as_string()` matches and the enum survives.
/// Must PASS — isolating the defect to the inline-OBJECT `$type` value.
#[test]
fn control_named_enum_via_string_type_rejects_out_of_set_label() {
    let result = run_field(
        "named-enum-string-type",
        r##"$types: { Status: { $enum: ["draft", "active", "closed"] } }"##,
        r##"{ $type: "Status", $optional: true }"##,
        REJECT_STEPS,
    );
    assert_steps_pass(&result, 3, "string `$type` naming an enum enforces its closed set");
}

/// BUG — the inline-object `$type` value `{ $type: { $enum: [...] } }` (no
/// `$optional`). `expanded_field` skips the object-valued `$type`, so the field
/// compiles to `json` and §5.9 is never enforced: `"archived"` is admitted.
/// Minimal diff from `control_direct_enum_*` (only `$enum` wrapped in `$type`).
/// CURRENTLY FAILS at step 0 (actual outcome `ok`, expected `rejected` §5.9).
#[test]
fn inline_type_enum_required_rejects_out_of_set_label() {
    let result = run_field(
        "inline-type-enum-required",
        "",
        r##"{ $type: { $enum: ["draft", "active", "closed"] } }"##,
        REJECT_STEPS,
    );
    assert_steps_pass(&result, 1, "inline `$type: { $enum }` must enforce §5.9's closed set");
}

/// BUG — the flagged form `{ $type: { $enum: [...] }, $optional: true }`. The
/// object-valued `$type` is dropped and the field compiles to `optional<json>`,
/// so an optional enum silently loses its closed-set validation (and its §5.9 /
/// §B.1 declaration ordering). CURRENTLY FAILS at step 0 (actual `ok`).
#[test]
fn inline_type_enum_optional_rejects_out_of_set_label() {
    let result = run_field(
        "inline-type-enum-optional",
        "",
        r##"{ $type: { $enum: ["draft", "active", "closed"] }, $optional: true }"##,
        REJECT_STEPS,
    );
    assert_steps_pass(&result, 1, "inline optional `$type: { $enum }` must enforce §5.9's closed set");
}
