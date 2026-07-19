//! RED-TEAM probe of the §15.6 observable funding-view SHAPE.
//!
//! §15.6: "The observable `spend.funding` view has exactly the members `source`
//! (text), `pool` (opaque pool identity), and `amount` (decimal). Its shape is
//! fixed and independent of the meter's source projection. Source-projected
//! metadata (for example `price`) … [is] not [a] member of the returned funding
//! view."
//!
//! Every existing corpus funding case (`w3-overlapping-heterogeneous-credits`,
//! `plan-downgrade-preserves-recorded-funding`, …) asserts funding rows with
//! `"...": true` — extra-members-allowed — so NONE of them pins the *exact* shape.
//! An implementation that (a) leaked the source-projected `price` into the funding
//! row, or (b) omitted the opaque `pool` identity, would pass every existing case
//! yet violate §15.6. This probe closes that gap: the source projects an extra
//! `price` member (legitimately, so it can drive `$order`), and the funding row is
//! matched EXACTLY — no `"...": true` — against `{ source, pool, amount }`.
//!
//! Externally deducible: §15.6 fixes the three members verbatim; the source name is
//! `topup`; the single-pool spend of 40 allocates 40; the pool identity is opaque
//! (`$any`). A pass confirms the fixed shape; a fail's observed row names exactly
//! which member the implementation added or dropped.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

// A single top-level meter whose source projects BOTH `$quantity` and an extra
// `price` member (used by `$order`). `return spend { id, funding }` surfaces the
// funding view for exact-shape assertion.
const APP: &str = r##"{
  format: 1
  name: funding-view-exact-shape-probe
  suite: scenario
  spec: ["#meters", "§15.6", "§15.3"]
  package: {
    $liasse: 1
    $app: "t.meters.fundshape@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      users: {
        $key: "id"
        id: "text"
        topups: {
          $key: "id"
          id: "text"
          amount: "decimal"
          price: "decimal"
        }
        spends: {
          $key: "id"
          $consumes: "credits"
          id: "uuid = uuid()"
          amount: "decimal"
          occurred_at: "timestamp = now()"
        }
        $limits: {
          credits: {
            $sources: { topup: ".topups { $quantity: .amount, price }" }
            $order: ["price"]
          }
        }
        $mut: {
          consume: [
            "spend = .spends + { amount: @amount }"
            "return spend { id, funding }"
          ]
        }
      }
      $public: {
        wallet: {
          $view: ".users { id, balance: .credits.balance }"
          $mut: { consume: ".users[@user].consume" }
        }
      }
    }
    $data: { users: { u1: { topups: { t1: { amount: "100", price: "9" } } } } }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<funding-view-exact-shape-probe>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("15-meters"), SuiteKind::Red, &case)
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

/// PASSING CONTROL: the spend admits and the balance reflects the allocation,
/// establishing that the meter funds normally (independent of funding-view shape).
#[test]
fn spend_admits_and_balance_reflects_allocation() {
    let result = run(
        r##"[
          { call: "public.wallet.consume", args: { user: "u1", amount: "40" },
            expect: { outcome: ok, value: { id: "$any:uuid", "...": true } } }
          { watch: "public.wallet", id: "w1", expect_init: { value: [ { id: "u1", balance: "60" } ] } }
        ]"##,
    );
    assert_all_pass(&result);
}

/// THE PROBE: the funding row must have EXACTLY `{ source, pool, amount }` — the
/// source-projected `price` must NOT appear, and the opaque `pool` identity MUST.
/// No `"...": true`, so any extra or missing member fails and the observed row
/// names the offending member.
#[test]
fn funding_row_has_exactly_source_pool_amount() {
    let result = run(
        r##"[
          { call: "public.wallet.consume", args: { user: "u1", amount: "40" },
            expect: { outcome: ok, value: {
              id: "$any:uuid"
              funding: [ { source: "topup", pool: "$any", amount: "40" } ]
            } } }
        ]"##,
    );
    assert_all_pass(&result);
}
