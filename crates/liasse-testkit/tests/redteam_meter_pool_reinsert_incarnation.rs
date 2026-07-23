#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe — Annex D.1 / §5.5 / §15.2-§15.6: a reinserted pool source is a
//! NEW incarnation, and the frozen funding of a DELETED pool's incarnation must not
//! bind to it.
//!
//! Annex D.1 (~line 5180): "Historical actor, session, meter-pool, deletion, and
//! module coordinates include incarnation identity wherever key reuse could
//! otherwise conflate occurrences." §5.6/§21.3: "Deleting and reinserting the same
//! key creates a new incarnation and does not transfer existing refs." So the
//! frozen funding of a deleted pool's incarnation must NOT charge against a
//! freshly-reinserted pool at the same app-chosen key.
//!
//! ── FINDING ──────────────────────────────────────────────────────────────────
//! The meter-pool funding coordinate is `(source-label, pool-application-key)` with
//! NO incarnation (`resolve.rs` / `admit.rs`). Spend 40 from pool `t1`, delete `t1`,
//! reinsert `t1` with quantity 100 → balance wrongly shows 60 (old 40 charged
//! against the NEW pool) and a fresh 100-spend is wrongly REJECTED (the new
//! incarnation has full 100). UUID-keyed pools are immune (a reinsert lands a new
//! key), so only reused app-chosen pool keys conflate.

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
            eprintln!("  {name} step {} [{}] -> {:?}", step.index, step.action, step.result);
        }
    }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

/// A wallet whose nested `topups` pool has an APP-CHOSEN `text` key (so a reinsert
/// reuses the key), consumed by a `credits` meter. `add_topup`/`remove_topup`
/// insert/delete a pool row by key; `consume` spends against the meter.
const PACKAGE: &str = r##"
    $liasse: 1
    $app: "t.meters.reinsert@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      users: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "text", amount: "decimal" }
        spends: {
          $key: "id"
          $consumes: "credits"
          id: "uuid = uuid()"
          amount: "decimal"
          occurred_at: "timestamp = now()"
        }
        $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
        $mut: {
          consume: [
            "spend = .spends + { amount: @amount }"
            "return spend { id }"
          ]
          add_topup: ".topups + { id: @id, amount: @amount }"
          remove_topup: ".topups - @topup"
        }
      }
      $public: {
        wallet: {
          $view: ".users { id, balance: .credits.balance }"
          $mut: {
            consume: ".users[@user].consume"
            add_topup: ".users[@user].add_topup"
            remove_topup: ".users[@user].remove_topup"
          }
        }
      }
    }
    $data: { users: { u1: { topups: { t1: { amount: "100" } } } } }
"##;

// ── CORE REPRO — reinsert at the SAME key must not inherit deleted funding ─────
// Spend 40 from pool t1 (100) → balance 60. Delete t1 (funding stays frozen on the
// spend, §22.1). Reinsert t1 = 100 → a NEW incarnation (§5.6/§21.3, Annex D.1). The
// old spend's 40 is charged against the DELETED incarnation, NOT the fresh one, so
// balance is the full 100 and a fresh 100-spend admits.
#[test]
fn reinsert_pool_same_key_does_not_inherit_deleted_funding() {
    let text = format!(
        r##"{{
      format: 1
      name: reinsert-pool-same-key
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "60" }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ user: "u1", topup: "t1" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
        {{ call: "public.wallet.add_topup", args: {{ user: "u1", id: "t1", amount: "100" }}, expect: {{ outcome: ok }} }}
        // the reinserted t1 is a fresh incarnation with the full 100 — the deleted
        // incarnation's 40 does NOT bind to it
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
        // a fresh 100-spend must admit against the new incarnation's full capacity
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "reinsert-pool-same-key");
}

// ── SELF-RED-TEAM A — reinsert with the SAME quantity (payload identical) ──────
// Payload cannot distinguish incarnations, so a reinsert with the same amount is the
// sharpest case: balance must still restore to the full quantity, proving the
// coordinate carries incarnation, not payload.
#[test]
fn reinsert_pool_same_quantity_still_fresh() {
    let text = format!(
        r##"{{
      format: 1
      name: reinsert-pool-same-quantity
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "60" }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ user: "u1", topup: "t1" }}, expect: {{ outcome: ok }} }}
        {{ call: "public.wallet.add_topup", args: {{ user: "u1", id: "t1", amount: "100" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "reinsert-pool-same-quantity");
}

// ── SELF-RED-TEAM B — delete WITHOUT reinsert keeps funding frozen (§22.1) ─────
// The invariant that must NOT regress the other way: after deleting t1 with no
// reinsert, the spend's 40 stays frozen and future availability is zero — the fix
// must not release the deleted incarnation's allocation.
#[test]
fn delete_without_reinsert_keeps_frozen_funding() {
    let text = format!(
        r##"{{
      format: 1
      name: delete-without-reinsert
      suite: scenario
      spec: ["#meters", "§15.2", "§22.1"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.remove_topup", args: {{ user: "u1", topup: "t1" }}, expect: {{ outcome: ok }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "0" }} ] }} }}
        // future availability is zero; a fresh spend rejects
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "1" }},
          expect: {{ outcome: rejected, violates: ["#meters", "§15.2"] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "delete-without-reinsert");
}

// ── SELF-RED-TEAM D — several spends span the delete, then reinsert is fresh ───
// Two spends (40 + 20) hold 60 against t1=100 (balance 40). Delete t1, reinsert
// t1=100. BOTH held allocations are against the DELETED incarnation, so the fresh
// t1 shows the full 100 and a fresh 100-spend admits.
#[test]
fn multiple_spends_across_delete_then_fresh_reinsert() {
    let text = format!(
        r##"{{
      format: 1
      name: multiple-spends-across-delete
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "20" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s2" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "40" }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ user: "u1", topup: "t1" }}, expect: {{ outcome: ok }} }}
        {{ call: "public.wallet.add_topup", args: {{ user: "u1", id: "t1", amount: "100" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "multiple-spends-across-delete");
}

/// A wallet whose pool has a UUID `$key` (`uuid()` default) — the immune shape: an
/// app cannot reuse a generated key, so the delete/reinsert conflation cannot
/// arise. `add_topup` mints a fresh uuid per insert.
const PACKAGE_UUID_POOL: &str = r##"
    $liasse: 1
    $app: "t.meters.reinsert.uuid@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      users: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "uuid = uuid()", amount: "decimal" }
        spends: {
          $key: "id"
          $consumes: "credits"
          id: "uuid = uuid()"
          amount: "decimal"
          occurred_at: "timestamp = now()"
        }
        $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
        $mut: {
          consume: [ "spend = .spends + { amount: @amount }", "return spend { id }" ]
          add_topup: [ "topup = .topups + { amount: @amount }", "return topup { id }" ]
          remove_topup: ".topups - @topup"
        }
      }
      $public: {
        wallet: {
          $view: ".users { id, balance: .credits.balance }"
          $mut: {
            consume: ".users[@user].consume"
            add_topup: ".users[@user].add_topup"
            remove_topup: ".users[@user].remove_topup"
          }
        }
      }
    }
    $data: { users: { u1: {} } }
"##;

// ── SELF-RED-TEAM E — UUID-keyed pool is immune and unregressed ────────────────
// A generated-key pool cannot have its key reused, so the fix must be transparent
// to it: add a 100 pool, spend 40 (balance 60), delete it (balance 0, funding stays
// frozen per §22.1), add a DIFFERENT 100 pool (a fresh uuid, fresh incarnation) →
// balance 100, and a 100-spend admits against it.
#[test]
fn uuid_keyed_pool_immune_and_unregressed() {
    let text = format!(
        r##"{{
      format: 1
      name: uuid-keyed-pool-immune
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.2", "§22.1"]
      package: {{ {PACKAGE_UUID_POOL} }}
      steps: [
        {{ call: "public.wallet.add_topup", args: {{ user: "u1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:p1" }} }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "60" }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ user: "u1", topup: "$ref:p1" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
        {{ call: "public.wallet.add_topup", args: {{ user: "u1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "uuid-keyed-pool-immune");
}

// ── SELF-RED-TEAM C — a normal pool (no key reuse) is unaffected ───────────────
// Baseline: without any delete/reinsert, multiple spends draw down one pool exactly
// as before — the incarnation coordinate must be transparent to the ordinary path.
#[test]
fn normal_pool_no_reuse_unaffected() {
    let text = format!(
        r##"{{
      format: 1
      name: normal-pool-no-reuse
      suite: scenario
      spec: ["#meters", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "30" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s2" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "30" }} ] }} }}
        // overspend rejects; exact remainder admits
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "31" }},
          expect: {{ outcome: rejected, violates: ["#meters", "§15.2"] }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "30" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "normal-pool-no-reuse");
}
