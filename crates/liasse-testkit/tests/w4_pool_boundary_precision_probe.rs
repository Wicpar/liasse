#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe — §14.1 half-open pool-window admission across a bucket boundary,
//! and its root cause: §A.1/§A.5 timestamp-argument precision.
//!
//! ── FINDING ──────────────────────────────────────────────────────────────────
//! A `timestamp` mutation argument was decoded at the microsecond DEFAULT precision
//! instead of the package's declared `$semantics.timestamp_precision`. The wire form
//! (§A.1) is the base-10 count only — "precision is a property of the declared type"
//! — and an inferred `@param` used as a `timestamp` field infers that field's type,
//! whose effective precision is the package precision (§4.4/§A.5). Decoding `@at =
//! "1767830400"` at microseconds and then rescaling to a seconds field (§22.5/§A.5)
//! collapsed it to `1768 s`, so a spend the case timed EXACTLY at a pool's `$until`
//! boundary instead landed near the epoch — always inside every pool — and was
//! ADMITTED where §14.1's half-open `[from, until)` requires it EXCLUDED at `$until`.
//! (The engine's boundary comparison, `meter/resolve.rs::active_at`, was already
//! correct: `time >= from && time < until`. The defect was purely the arg precision.)
//!
//! Root cause / fix: the wire-decode boundary resolves an inferred `timestamp`
//! parameter's precision to the package precision before decoding its wire count
//! (`liasse-testkit/src/adapter/router.rs`), matching the seed/field-write path.
//!
//! The FINDING regression is the corpus case
//! `tests/15-meters/red/spend-at-pool-until-boundary-excluded.hjson` (formerly on the
//! scenario debt ledger with an INVERTED reason). These probes self-red-team the
//! neighbouring edges the single corpus case does not pin: the `$from` (lower,
//! inclusive) boundary, spends just-before/just-after BOTH edges, and a non-seconds
//! package precision (proving the resolution is general, not seconds-hardcoded).

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

fn run_case_text(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<probe>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = liasse_testkit::ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("probe"), SuiteKind::Red, &case)
}

fn assert_all_pass(result: &CaseResult, name: &str) {
    let ok = result.steps.iter().all(|step| step.result.is_pass());
    if !ok {
        for step in &result.steps {
            eprintln!(
                "  {name} step {} [{}] -> {:?} observed={:?}",
                step.index, step.action, step.result, step.observed
            );
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// ── SELF-RED-TEAM 1 — both edges at seconds precision ────────────────────────
// Explicit `[from, until)` window with from=1767830000, until=1767830400 (seconds).
// §14.1: the lower bound is inclusive, the upper bound exclusive. A spend is funded
// iff its `$time` lies in `[from, until)`; the meter rejects otherwise (§15.2 step 6).
#[test]
fn half_open_pool_window_both_edges_seconds() {
    let text = r##"{
      format: 1
      name: half-open-pool-window-both-edges-seconds
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.1", "§14.2"]
      package: {
        $liasse: 1
        $app: "t.edges@1.0.0"
        $semantics: { timestamp_precision: "s" }
        $model: {
          users: {
            $key: "id", id: "text"
            topups: {
              $key: "id"
              $bucket: { $from: ".starts_at", $until: ".expires_at" }
              id: "text"
              amount: "decimal"
              starts_at: "timestamp"
              expires_at: "timestamp? = none"
            }
            spends: {
              $key: "id"
              $consumes: "credits"
              id: "uuid = uuid()"
              amount: "decimal"
              occurred_at: "timestamp = now()"
            }
            $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" }, $order: ["$until"] } }
            $mut: {
              consume_at: [
                "spend = .spends + { amount: @amount, occurred_at: @at }"
                "return spend { id }"
              ]
            }
          }
          $public: { wallet: { $mut: { consume_at: ".users[@user].consume_at" } } }
        }
        $data: {
          users: { u1: { topups: { t1: { amount: "100", starts_at: "1767830000", expires_at: "1767830400" } } } }
        }
      }
      steps: [
        // just BELOW $from -> inactive -> rejected.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767829999" },
          expect: { outcome: rejected, violates: ["#buckets", "§14.1", "#meters", "§15.2"] } }
        // exactly AT $from -> active (inclusive lower bound) -> ok.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830000" },
          expect: { outcome: ok, value: { id: "$any:uuid" } } }
        // just ABOVE $from -> active -> ok.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830001" },
          expect: { outcome: ok, value: { id: "$any:uuid" } } }
        // just BELOW $until -> active -> ok.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830399" },
          expect: { outcome: ok, value: { id: "$any:uuid" } } }
        // exactly AT $until -> inactive (exclusive upper bound) -> rejected.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830400" },
          expect: { outcome: rejected, violates: ["#buckets", "§14.1", "#meters", "§15.2"] } }
        // just ABOVE $until -> inactive -> rejected.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830401" },
          expect: { outcome: rejected, violates: ["#buckets", "§14.1", "#meters", "§15.2"] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "half-open-pool-window-both-edges-seconds");
}

// ── SELF-RED-TEAM 2 — precision resolution is general (milliseconds) ──────────
// The same `$until` edge at a MILLISECOND package precision. If the arg precision
// were hardcoded to seconds (or left at the microsecond default) the boundary would
// land in the wrong place; resolving to the DECLARED package precision is what makes
// the exact-boundary spend reject at ms scale too.
#[test]
fn half_open_pool_window_until_edge_millis() {
    let text = r##"{
      format: 1
      name: half-open-pool-window-until-edge-millis
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.1"]
      package: {
        $liasse: 1
        $app: "t.msedge@1.0.0"
        $semantics: { timestamp_precision: "ms" }
        $model: {
          users: {
            $key: "id", id: "text"
            topups: {
              $key: "id"
              $bucket: { $until: ".expires_at" }
              id: "text"
              amount: "decimal"
              expires_at: "timestamp? = none"
            }
            spends: {
              $key: "id"
              $consumes: "credits"
              id: "uuid = uuid()"
              amount: "decimal"
              occurred_at: "timestamp = now()"
            }
            $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" }, $order: ["$until"] } }
            $mut: {
              consume_at: [
                "spend = .spends + { amount: @amount, occurred_at: @at }"
                "return spend { id }"
              ]
            }
          }
          $public: { wallet: { $mut: { consume_at: ".users[@user].consume_at" } } }
        }
        $data: { users: { u1: { topups: { t1: { amount: "100", expires_at: "1767830400000" } } } } }
      }
      steps: [
        // one ms before $until -> active -> ok.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830399999" },
          expect: { outcome: ok, value: { id: "$any:uuid" } } }
        // exactly AT $until (ms count) -> inactive -> rejected.
        { call: "public.wallet.consume_at", args: { user: "u1", amount: "1", at: "1767830400000" },
          expect: { outcome: rejected, violates: ["#buckets", "§14.1", "#meters", "§15.2"] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "half-open-pool-window-until-edge-millis");
}
