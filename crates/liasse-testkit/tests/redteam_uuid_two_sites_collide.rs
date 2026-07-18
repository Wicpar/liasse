//! RED-TEAM finding — SPEC-ISSUES #4 regression (§5.1/§8.12).
//!
//! # CONFIRMED BUG
//!
//! Two DISTINCT `uuid()` field-default call sites on the SAME row produce the
//! **identical** UUID. §5.1 (SPEC.md:387) and §8.12 (SPEC.md:1205) both pin:
//! "`uuid()` yields a fresh, distinct value on every evaluation". The generator
//! module even documents the intended guarantee — "the call-site span keeps two
//! distinct `uuid()` sites of one row apart" (crates/liasse-runtime/src/
//! generator.rs:11-12) — but it does not hold.
//!
//! ## Root cause
//!
//! `derive_uuid(seed, site, generation)` (crates/liasse-runtime/src/generator.rs:71)
//! disambiguates a `uuid()` evaluation by three inputs: the per-request `seed`,
//! the per-row `Generation`, and the call-site `ByteSpan`. Two field defaults on
//! ONE row share the request `seed` and the row's single `Generation` (one
//! `next_generation()` per row, crates/liasse-runtime/src/interp.rs:942), so the
//! ByteSpan is the ONLY discriminator left. But each field default is compiled
//! into its OWN sub-source — `compile_expr` calls `sources.add_label(label, text)`
//! per expression (crates/liasse-runtime/src/compiled.rs:528) — so its
//! `TypedExpr` spans are LOCAL offsets within that sub-source. Two defaults whose
//! body text is byte-identical (`"uuid()"`) therefore carry the identical span
//! `[0..6)`. `CallSite::new(expr.span())` (crates/liasse-runtime/src/eval/mod.rs:212)
//! passes only that `ByteSpan` to `derive_uuid` and drops the `SourceId` that
//! WOULD have distinguished them, so `derive_uuid` receives an identical
//! `(seed, span, generation)` triple for both fields and returns one UUID.
//!
//! ## Impact
//!
//! The ordinary idiom of a row carrying two independent `uuid()`-defaulted fields
//! — e.g. a public `id` plus a `secret`/`token`/`external_ref` — silently makes
//! the second field equal the first. A field intended to be an unguessable
//! secret becomes exactly the public identifier: a predictable-secret exposure,
//! not merely a cosmetic identity clash.
//!
//! Both assertions below are proved two independent ways: an engine-side row
//! `$check` (`.id == .secret` ADMITS, proving equality inside admission) and a
//! matcher read-back (`secret == $ref:id`). Neither depends on implementation
//! internals — the expected value (two independent `uuid()`s differ) is pinned by
//! SPEC.md text alone. Each test FAILS today and will pass once the engine
//! distinguishes the two call sites (e.g. by folding the field's `SourceId`, or a
//! per-call-site ordinal, into the derivation).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("uuid-two-sites"), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("22-runtime-semantics"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, context: &str) {
    for step in &result.steps {
        assert!(
            step.result.is_pass(),
            "{context}: step {} ({}) did not pass: {:?}",
            step.index,
            step.action,
            step.result
        );
    }
}

/// §5.1/§8.12: two independent `uuid()` field defaults on one row are two
/// evaluations and MUST differ, so the row `$check ".id != .secret"` admits the
/// insert. The engine instead makes them equal, so the check fails and the insert
/// is `rejected`. This test asserts the spec-required `ok` and FAILS today.
#[test]
fn two_uuid_defaults_on_one_row_are_distinct() {
    const APP: &str = r##"{
      format: 1
      name: uuid-two-sites-distinct
      suite: scenario
      spec: ["#state-model", "§5.1", "#mutations", "§8.12"]
      package: {
        $liasse: 1
        $app: "t.uuid2@1.0.0"
        $model: {
          accounts: {
            $key: "id"
            id: "uuid = uuid()"
            secret: "uuid = uuid()"
            name: "text"
            $check: ".id != .secret"
          }
          $mut: { add: [ ".accounts + { name: 'a' }", "return count(.accounts)" ] }
          $public: { accounts: { $view: ".accounts { id, secret, name }", $mut: { add: ".add" } } }
        }
      }
      steps: [
        { call: "public.accounts.add", args: {},
          expect: { outcome: ok, value: "1" } }
      ]
    }"##;
    let result = run(APP);
    assert_all_pass(&result, "two uuid() defaults must differ (§5.1/§8.12)");
}

/// Complementary engine-side proof: a row `$check ".id == .secret"` over two
/// independent `uuid()` defaults is UNSATISFIABLE under §5.1/§8.12 (two distinct
/// uuids can never be equal), so the insert MUST be `rejected`. Today the engine
/// ADMITS it (`ok`), directly proving `id == secret` — the collision. This
/// asserts the spec-required `rejected` and FAILS today.
#[test]
fn a_check_equating_two_uuid_defaults_is_unsatisfiable() {
    const PROOF: &str = r##"{
      format: 1
      name: uuid-secret-collision-proof
      suite: scenario
      spec: ["#state-model", "§5.1"]
      package: {
        $liasse: 1
        $app: "t.uuidproof@1.0.0"
        $model: {
          accounts: {
            $key: "id"
            id: "uuid = uuid()"
            secret: "uuid = uuid()"
            name: "text"
            $check: ".id == .secret"
          }
          $mut: { add: [ ".accounts + { name: 'a' }", "return count(.accounts)" ] }
          $public: { accounts: { $view: ".accounts { id }", $mut: { add: ".add" } } }
        }
      }
      steps: [
        { call: "public.accounts.add", args: {},
          expect: { outcome: rejected, violates: ["#state-model", "§5.1"] } }
      ]
    }"##;
    let proof = run(PROOF);
    assert_all_pass(&proof, "a `.id == .secret` check over two uuid() defaults must be unsatisfiable (§5.1)");
}

// ---------------------------------------------------------------------------
// PASSING CONTROLS — these isolate the bug to the same-row, two-call-site case.
// The engine gets the two dimensions the corpus already exercises RIGHT; only
// the third (two distinct sites on one row, discriminated solely by span) fails.
// ---------------------------------------------------------------------------

/// CONTROL (passes): one `uuid()` key-default call site across the three rows of
/// ONE bulk-insert statement yields three distinct keys — a distinct `Generation`
/// per row (interp.rs:942) discriminates them, so all three insert and
/// `count == 3`. This is the resolution's headline guarantee and it holds.
#[test]
fn control_bulk_insert_one_site_distinct_per_row() {
    const APP: &str = r##"{
      format: 1
      name: uuid-bulk-distinct
      suite: scenario
      spec: ["#state-model", "§5.1"]
      package: {
        $liasse: 1
        $app: "t.uuidbulk@1.0.0"
        $model: {
          staging: { $key: "name", name: "text" }
          rows: { $key: "id", id: "uuid = uuid()", name: "text" }
          $mut: { fill: [ ".rows + .staging { name: .name }", "return count(.rows)" ] }
          $public: { rows: { $view: ".rows { name, $sort: [name] }", $mut: { fill: ".fill" } } }
        }
        $data: { staging: { a: {}, b: {}, c: {} } }
      }
      steps: [ { call: "public.rows.fill", args: {}, expect: { outcome: ok, value: "3" } } ]
    }"##;
    assert_all_pass(&run(APP), "one uuid() site across bulk rows stays distinct (control)");
}

/// CONTROL (passes): a SECOND `uuid()` field carrying `$unique` across the three
/// bulk rows admits — the same single call site produces a distinct value per
/// row, so `$unique` is satisfied. (Contrast: two DIFFERENT sites on ONE row
/// collide.)
#[test]
fn control_second_uuid_site_unique_across_rows() {
    const APP: &str = r##"{
      format: 1
      name: uuid-unique-across-rows
      suite: scenario
      spec: ["#state-model", "§5.1"]
      package: {
        $liasse: 1
        $app: "t.uuiduniq@1.0.0"
        $model: {
          staging: { $key: "name", name: "text" }
          rows: { $key: "name", name: "text", tag: { $type: "uuid", $default: "= uuid()", $unique: true } }
          $mut: { fill: [ ".rows + .staging { name: .name }", "return count(.rows)" ] }
          $public: { rows: { $view: ".rows { name, tag, $sort: [name] }", $mut: { fill: ".fill" } } }
        }
        $data: { staging: { a: {}, b: {}, c: {} } }
      }
      steps: [ { call: "public.rows.fill", args: {}, expect: { outcome: ok, value: "3" } } ]
    }"##;
    assert_all_pass(&run(APP), "one uuid() site stays $unique across bulk rows (control)");
}

/// CONTROL (passes): `now()` IS shared across a request (A.5), so two `now()`
/// defaults on one row are equal and `$check ".t1 == .t2"` admits. This is the
/// intended behavior for `now()` and confirms the harness observes field-default
/// generative evaluation faithfully — the `uuid()` divergence above is a real
/// asymmetry, not a harness artifact.
#[test]
fn control_now_shared_within_row() {
    const APP: &str = r##"{
      format: 1
      name: now-shared
      suite: scenario
      spec: ["#annex-a", "§A.5"]
      package: {
        $liasse: 1
        $app: "t.nowshare@1.0.0"
        $model: {
          rows: { $key: "id", id: "uuid = uuid()", t1: "timestamp = now()", t2: "timestamp = now()", $check: ".t1 == .t2" }
          $mut: { add: [ ".rows + { }", "return count(.rows)" ] }
          $public: { rows: { $view: ".rows { id }", $mut: { add: ".add" } } }
        }
      }
      steps: [ { call: "public.rows.add", args: {}, expect: { outcome: ok, value: "1" } } ]
    }"##;
    assert_all_pass(&run(APP), "now() is shared within a request (control, A.5)");
}
