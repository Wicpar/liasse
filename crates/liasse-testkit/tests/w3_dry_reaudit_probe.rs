//! WAVE-3 DRY RE-AUDIT — three harder edges over areas prior waves declared
//! convergent. Every expectation is hand-derived from SPEC.md and cited inline;
//! nothing here reads an expected value off the program.
//!
//! VERDICTS after running:
//!   * Probe A (§5.7 composite unique ∘ §9.1 normalization): HELD (DRY). Passes.
//!   * Probe B (Annex B.4 nested composite with a `none` member): **FINDING** —
//!     `probe_b_struct_sort_present_member_precedes_absent` FAILS against the
//!     current implementation; the paired `control_b_*` PASSES. See its block.
//!   * Probe C (§22.2 transient write-then-revert net-effect): HELD (DRY). Passes.
//!
//! Probe A (§5.7 + §9.1 normalization order) — HELD: a COMPOSITE candidate key
//!   whose two components each differ in raw spelling but NORMALIZE equal still
//!   collides. Existing corpus covers single-field normalization collision
//!   (05.../red/normalization-defeats-unique-evasion) and
//!   composite-without-normalization (05.../common/
//!   unique-none-component-does-not-conflict) SEPARATELY; their interaction
//!   (composite ∘ normalization) is the harder, previously-uncovered edge. It
//!   holds because §9.1's admission order normalizes each field BEFORE the
//!   candidate tuple is built (rules.rs::finalize -> normalize_all then
//!   check_uniqueness), and `tuple_of` reads the already-normalized field values.
//!
//! Probe B (Annex B.4 nested composite with a `none` member) — FINDING: sorting a
//!   view by a STRUCT-valued key where one row's optional struct member is `none`.
//!   B.4: "an absent optional member sorts last among values equal on every
//!   preceding member — a present value precedes an absent one". The existing
//!   struct-sort case (annex-b.../red/
//!   struct-fields-compared-in-field-name-text-order) uses all-present members, so
//!   the B.4 present-before-absent clause was untested for a struct sort key.
//!
//! Probe C (§22.2 net-effect diff) — HELD: a mutation that writes a field to a
//!   NEW value then back to its ORIGINAL value within one program — a transient
//!   change with a net-zero effect — returns `unchanged`. §22.2: "A program
//!   producing no state change returns `unchanged` and creates no commit." The
//!   existing empty-delta case (22.../red/empty-delta-boundary-noop-vs-net-change)
//!   only writes fields to their CURRENT value (no transient divergence); a
//!   write-then-revert is the harder edge. It holds because state.rs::diff()
//!   compares the FINAL materialized row value against the committed baseline,
//!   so a value that reverts within the program leaves an empty delta.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<probe>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("probe"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|s| s.result.is_pass());
    if !ok {
        for step in &result.steps {
            println!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation");
}

// ── PROBE A — §5.7 composite candidate key ∘ §9.1 normalization ───────────────
// §9.1 admission order is "defaults, normalization, checks, key, ref,
// uniqueness"; normalization (§5.1/§8.8) fixes the field value BEFORE the
// candidate-key comparison. §5.7: a composite candidate key "groups the fields
// of one composite candidate key". Two rows whose (country, tax_id) differ only
// in case/padding normalize to the SAME tuple ("fr","123") and MUST collide
// through the composite candidate key.
#[test]
fn probe_a_composite_unique_collides_after_normalization() {
    let text = r##"{
      format: 1
      name: probe-a-composite-unique-normalized
      suite: scenario
      spec: ["#state-model", "§5.7", "§5.1", "§9.1", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.w3.compnorm@1.0.0"
        $model: {
          taxpayers: {
            $key: "id"
            $unique: [["country", "tax_id"]]
            id: "text"
            country: { $type: "text", $normalize: "string.lower(string.trim(.))" }
            tax_id: { $type: "text", $normalize: "string.trim(.)" }
          }
          $mut: { add: ".taxpayers + { id: @id, country: @country, tax_id: @tax_id }" }
          $public: {
            taxpayers: {
              $view: ".taxpayers { id, country, tax_id }"
              $mut: { add: ".add" }
            }
          }
        }
      }
      steps: [
        { call: "public.taxpayers.add", args: { id: "t1", country: "FR", tax_id: "123" },
          expect: { outcome: ok } }
        // both components differ in raw spelling but normalize to ("fr","123")
        { call: "public.taxpayers.add", args: { id: "t2", country: "  fr ", tax_id: " 123 " },
          expect: { outcome: rejected, violates: ["§5.7"] } }
        { watch: "public.taxpayers", id: "w1",
          expect_init: { value: [ { id: "t1", country: "fr", tax_id: "123" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "probe-a-composite-unique-normalized");
}

// CONTROL A — the same model admits two rows that normalize to DISTINCT tuples,
// and admits a component-swap (order matters in a composite key: ("fr","123") ≠
// ("123","fr")). Isolates that the composite constraint is not over-eager.
#[test]
fn control_a_distinct_normalized_tuples_admit() {
    let text = r##"{
      format: 1
      name: control-a-distinct-normalized
      suite: scenario
      spec: ["#state-model", "§5.7", "§B.4"]
      package: {
        $liasse: 1
        $app: "t.w3.compnormctl@1.0.0"
        $model: {
          taxpayers: {
            $key: "id"
            $unique: [["country", "tax_id"]]
            id: "text"
            country: { $type: "text", $normalize: "string.lower(string.trim(.))" }
            tax_id: { $type: "text", $normalize: "string.trim(.)" }
          }
          $mut: { add: ".taxpayers + { id: @id, country: @country, tax_id: @tax_id }" }
          $public: {
            taxpayers: {
              $view: ".taxpayers { id, country, tax_id }"
              $mut: { add: ".add" }
            }
          }
        }
      }
      steps: [
        { call: "public.taxpayers.add", args: { id: "t1", country: "FR", tax_id: "123" },
          expect: { outcome: ok } }
        // normalizes to ("de","123") — distinct first component -> admits
        { call: "public.taxpayers.add", args: { id: "t2", country: "DE", tax_id: "123" },
          expect: { outcome: ok } }
        // normalizes to ("fr","999") — distinct second component -> admits
        { call: "public.taxpayers.add", args: { id: "t3", country: "fr", tax_id: "999" },
          expect: { outcome: ok } }
        // component swap ("123","fr") ≠ ("fr","123") -> admits (ordered composite)
        { call: "public.taxpayers.add", args: { id: "t4", country: "123", tax_id: "fr" },
          expect: { outcome: ok } }
        { watch: "public.taxpayers", id: "w1", expect_init: { value: { $unordered: [
          { id: "t1", country: "fr", tax_id: "123" }
          { id: "t2", country: "de", tax_id: "123" }
          { id: "t3", country: "fr", tax_id: "999" }
          { id: "t4", country: "123", tax_id: "fr" }
        ] } } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-a-distinct-normalized");
}

// ── PROBE B — Annex B.4 nested composite with a `none` member — FINDING ────────
// §B.4 struct: "lexicographic fields in canonical field-name order" with the
// clause "an absent optional member sorts last among values equal on every
// preceding member — a present value precedes an absent one ... a struct whose
// optional member is none sorts after one whose corresponding member is present".
// Field names compare in text order: "city" (U+0063) < "zip" (U+007A), so `city`
// is the leading component. Rows:
//   r1: pt = { city: "paris", zip: "75001" }
//   r2: pt = { city: "paris" }            (zip omitted -> none, §9.1/A.1)
//   r3: pt = { city: "lyon",  zip: "69001" }
// city ascending: "lyon" < "paris" => r3 first. Within city="paris": zip present
// (r1) precedes zip none (r2). SPEC-correct key-ascending order: r3, r1, r2.
// Only `id` is projected, so no struct wire spelling is asserted — just row order.
//
// OBSERVED (divergence): [r3, r2, r1] — r2 (zip=none) sorts BEFORE r1 (zip present),
// the exact opposite of B.4's "a present value precedes an absent one". The step
// reports `$[1].id: literal value mismatch` (expected r1 at index 1, got r2).
//
// ROOT CAUSE: a seed-provided struct with an omitted optional member OMITS that
// member from the stored value instead of storing it as `Value::None`:
//   * crates/liasse-runtime/src/seed.rs::decode_struct (~L253-268) pushes only
//     SUPPLIED members; an omitted optional member is left absent from the map.
//     The mutation-insert path shares this: crates/liasse-runtime/src/interp.rs::
//     struct_value (~L1315-1334) also leaves "an omitted optional member ...
//     absent (A.1)", so the defect is not seed-specific — a mutation-constructed
//     struct with a `none` member mis-sorts identically. (Contrast
//     crates/liasse-value/src/decode.rs::decode_struct ~L502-510, the wire path,
//     which fills an absent optional member with `Value::None`.)
//   * crates/liasse-value/src/value.rs (~L76-77) `struct Struct(BTreeMap<Text,
//     Value>)` DERIVES `Ord`. Comparing {city} against {city, zip} is a
//     shorter-prefix comparison, so the map missing `zip` sorts FIRST — whereas
//     if `zip` were stored as `Value::None` (rank u8::MAX at value.rs ~L167) the
//     present value would correctly precede it. `control_b_*` (all members
//     present, both maps carry `zip`) sorts correctly, isolating the defect to
//     the omitted-member representation, not to struct sorting in general.
#[test]
fn probe_b_struct_sort_present_member_precedes_absent() {
    let text = r##"{
      format: 1
      name: probe-b-struct-none-member-sorts-last
      suite: scenario
      spec: ["#annex-b", "§B.4", "§B.2", "§9.1", "§A.1"]
      package: {
        $liasse: 1
        $app: "t.w3.structnone@1.0.0"
        $model: {
          rows: {
            $key: "id"
            id: "text"
            pt: {
              city: "text"
              zip: "text?"
            }
          }
          $public: {
            by_pt: { $view: ".rows { id, $sort: [pt] }" }
          }
        }
        $data: {
          rows: {
            r1: { pt: { city: "paris", zip: "75001" } }
            r2: { pt: { city: "paris" } }
            r3: { pt: { city: "lyon", zip: "69001" } }
          }
        }
      }
      steps: [
        { watch: "public.by_pt", id: "w1",
          expect_init: { value: [
            { id: "r3" }
            { id: "r1" }
            { id: "r2" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "probe-b-struct-none-member-sorts-last");
}

// CONTROL B — the identical model with EVERY row's optional member PRESENT sorts
// correctly by the second struct field (§B.4 field-name order, all present). This
// isolates the defect (if any) to the present-vs-absent clause, not to struct
// sorting in general.
//   r1: { city: "paris", zip: "75001" }
//   r2: { city: "paris", zip: "75002" }
//   r3: { city: "lyon",  zip: "69001" }
// Order: r3 (lyon) < r1 (paris,75001) < r2 (paris,75002).
#[test]
fn control_b_struct_sort_all_present_orders_by_second_field() {
    let text = r##"{
      format: 1
      name: control-b-struct-all-present
      suite: scenario
      spec: ["#annex-b", "§B.4"]
      package: {
        $liasse: 1
        $app: "t.w3.structpresent@1.0.0"
        $model: {
          rows: {
            $key: "id"
            id: "text"
            pt: {
              city: "text"
              zip: "text?"
            }
          }
          $public: {
            by_pt: { $view: ".rows { id, $sort: [pt] }" }
          }
        }
        $data: {
          rows: {
            r1: { pt: { city: "paris", zip: "75001" } }
            r2: { pt: { city: "paris", zip: "75002" } }
            r3: { pt: { city: "lyon", zip: "69001" } }
          }
        }
      }
      steps: [
        { watch: "public.by_pt", id: "w1",
          expect_init: { value: [
            { id: "r3" }
            { id: "r1" }
            { id: "r2" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-b-struct-all-present");
}

// ── PROBE C — §22.2 net-effect diff of a transient write-then-revert ───────────
// §22.2: "A program producing no state change returns `unchanged` and creates no
// commit." The trigger is the NET effect, not whether statements ran. A program
// that writes x to a NEW value then back to its ORIGINAL value has a net-zero
// delta and MUST report `unchanged` (§8.9): the client frontier does not advance.
#[test]
fn probe_c_transient_write_then_revert_is_unchanged() {
    let text = r##"{
      format: 1
      name: probe-c-transient-revert-unchanged
      suite: scenario
      spec: ["#runtime", "§22.2", "§8.9"]
      package: {
        $liasse: 1
        $app: "t.w3.revert@1.0.0"
        $model: {
          rows: {
            $key: "id"
            id: "text"
            x: "int"
            $mut: {
              churn: [
                ".x = @tmp"
                ".x = @orig"
              ]
            }
          }
          $public: {
            rows: {
              $view: ".rows { id, x }"
              $mut: { churn: ".rows[@id].churn" }
            }
          }
        }
        $data: {
          rows: { "r1": { x: "1" } }
        }
      }
      steps: [
        { connect: "c1" }
        // x: 1 -> 5 -> 1. Net delta empty -> unchanged, no commit.
        { call: "public.rows.churn", args: { id: "r1", tmp: "5", orig: "1" }, on: "c1",
          expect: { outcome: ok, completion: unchanged } }
        { watch: "public.rows", on: "c1", id: "w1",
          expect_init: { value: [ { id: "r1", x: "1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "probe-c-transient-revert-unchanged");
}

// CONTROL C — the same program whose final value DIFFERS from the original is a
// real net change and commits (§22.2). x: 1 -> 5 -> 9, net 1->9, committed.
#[test]
fn control_c_transient_then_new_value_commits() {
    let text = r##"{
      format: 1
      name: control-c-transient-then-commit
      suite: scenario
      spec: ["#runtime", "§22.2", "§8.9"]
      package: {
        $liasse: 1
        $app: "t.w3.revertctl@1.0.0"
        $model: {
          rows: {
            $key: "id"
            id: "text"
            x: "int"
            $mut: {
              churn: [
                ".x = @tmp"
                ".x = @orig"
              ]
            }
          }
          $public: {
            rows: {
              $view: ".rows { id, x }"
              $mut: { churn: ".rows[@id].churn" }
            }
          }
        }
        $data: {
          rows: { "r1": { x: "1" } }
        }
      }
      steps: [
        { connect: "c1" }
        // x: 1 -> 5 -> 9. Net delta 1->9 -> committed.
        { call: "public.rows.churn", args: { id: "r1", tmp: "5", orig: "9" }, on: "c1",
          expect: { outcome: ok, completion: committed } }
        { watch: "public.rows", on: "c1", id: "w1",
          expect_init: { value: [ { id: "r1", x: "9" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-c-transient-then-commit");
}
