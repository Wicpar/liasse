#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe — Annex D.1 / §15.4 / §15.2-§15.6: the reinsert-conflation fix
//! (commit 2708811, W9-2) bound meter-pool funding to a pool's durable incarnation
//! — but only for TOP-LEVEL enforcing rows. A HIERARCHICAL meter (§15.4: the same
//! meter name at a NESTED lexical ancestor row) enforces over a nested enforcing
//! row, and its nested pool rows were still matched KEY-ONLY, so the same
//! delete/reinsert conflation persisted one level down.
//!
//! Annex D.1: "Historical actor, session, meter-pool, deletion, and module
//! coordinates include incarnation identity wherever key reuse could otherwise
//! conflate occurrences." §5.6/§21.3: a delete-then-reinsert at the same key is a
//! NEW incarnation. So a deleted NESTED pool's frozen funding must NOT bind to a
//! pool reinserted at the same app-chosen key under the same enforcing account.
//!
//! ── ROOT CAUSE ───────────────────────────────────────────────────────────────
//! `materialize::materialize_row` (the single-row read a meter resolves an
//! enforcing row over) built the enforcing row's `RowId` from ONLY its last address
//! step's key, dropping the ancestor prefix. The `incarnation_index` keys durable
//! incarnations by the FULL address chain (`row_id_of`). For a nested enforcing row
//! (`companies[co].accounts[a1]`) the truncated id (`a1`) never equals the indexed
//! full-chain id (`co.a1`), so every nested pool missed the index and fell back to
//! key-only matching — reviving the pre-W9-2 conflation for §15.4 hierarchies.

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

/// A two-level §15.4 hierarchy: a `credits` meter declared at BOTH the `companies`
/// row (top-level enforcing) AND the nested `companies.accounts` row (nested
/// enforcing). Each level's pool is its own nested `topups` collection with an
/// APP-CHOSEN `text` key, so a reinsert reuses the key. The account-level pool is
/// the nested enforcing target the truncated `RowId` used to miss.
const PACKAGE: &str = r##"
    $liasse: 1
    $app: "t.meters.hier.reinsert@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      companies: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "text", amount: "decimal" }
        $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
        accounts: {
          $key: "id"
          id: "text"
          topups: { $key: "id", id: "text", amount: "decimal" }
          $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
          spends: {
            $key: "id"
            $consumes: "credits"
            id: "uuid = uuid()"
            amount: "decimal"
            occurred_at: "timestamp = now()"
          }
          $mut: {
            consume: [ "spend = .spends + { amount: @amount }", "return spend { id }" ]
            add_topup: ".topups + { id: @id, amount: @amount }"
            remove_topup: ".topups - @topup"
          }
        }
      }
      $public: {
        wallet: {
          $view: '''.companies {
            id,
            balance: .credits.balance,
            accounts: .accounts { id, balance: .credits.balance }
          }'''
          $mut: {
            consume: ".companies[@company].accounts[@account].consume"
            add_topup: ".companies[@company].accounts[@account].add_topup"
            remove_topup: ".companies[@company].accounts[@account].remove_topup"
          }
        }
      }
    }
    $data: {
      companies: {
        co: {
          topups: { ct: { amount: "1000" } }
          accounts: { a1: { topups: { at1: { amount: "100" } } } }
        }
      }
    }
"##;

// ── CORE REPRO — nested-enforcing reinsert must not inherit deleted funding ────
// Spend 40 at account a1 (account pool at1=100 → 60; company pool ct=1000 → 960).
// Delete at1 (account funding stays frozen against incarnation A, §22.1 → account
// balance 0). Reinsert at1=100 → a NEW incarnation B (§5.6/§21.3, Annex D.1). The
// deleted incarnation A's 40 is charged against A, NOT B, so account balance is the
// full 100, and a fresh 100-spend admits at the account level (drawing another 100
// off the company, whose 960 becomes 860).
#[test]
fn hierarchical_reinsert_nested_pool_does_not_inherit_deleted_funding() {
    let text = format!(
        r##"{{
      format: 1
      name: hierarchical-reinsert-nested-pool
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.4", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "60" }} ] }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ company: "co", account: "a1", topup: "at1" }},
          expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "0" }} ] }} ] }} }}
        {{ call: "public.wallet.add_topup", args: {{ company: "co", account: "a1", id: "at1", amount: "100" }},
          expect: {{ outcome: ok }} }}
        // the reinserted at1 is a fresh incarnation with the full 100 — the deleted
        // incarnation's 40 does NOT bind to it (the pre-fix bug showed 60 here)
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "100" }} ] }} ] }} }}
        // a fresh 100-spend must admit against the new incarnation's full capacity
        // (account a1 → 0; company ct draws another 100 → 860)
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "860", accounts: [ {{ id: "a1", balance: "0" }} ] }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "hierarchical-reinsert-nested-pool");
}

// ── SELF-RED-TEAM A — reinsert with the SAME quantity (payload identical) ──────
// Payload cannot distinguish incarnations, so a same-amount reinsert of the nested
// pool is the sharpest case: the account balance must still restore to the full
// quantity, proving the nested coordinate carries incarnation, not payload.
#[test]
fn hierarchical_reinsert_same_quantity_still_fresh() {
    let text = format!(
        r##"{{
      format: 1
      name: hierarchical-reinsert-same-quantity
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.4", "§15.2"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "60" }} ] }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ company: "co", account: "a1", topup: "at1" }},
          expect: {{ outcome: ok }} }}
        {{ call: "public.wallet.add_topup", args: {{ company: "co", account: "a1", id: "at1", amount: "100" }},
          expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "100" }} ] }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "hierarchical-reinsert-same-quantity");
}

// ── SELF-RED-TEAM B — delete WITHOUT reinsert keeps nested funding frozen ──────
// The other-direction invariant that must NOT regress: after deleting at1 with no
// reinsert, the account spend's 40 stays frozen against the deleted incarnation and
// the account's future availability is zero — a fresh account spend rejects.
#[test]
fn hierarchical_delete_without_reinsert_keeps_frozen_funding() {
    let text = format!(
        r##"{{
      format: 1
      name: hierarchical-delete-without-reinsert
      suite: scenario
      spec: ["#meters", "§15.4", "§15.2", "§22.1"]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.remove_topup", args: {{ company: "co", account: "a1", topup: "at1" }},
          expect: {{ outcome: ok }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "0" }} ] }} ] }} }}
        // the account level has zero future availability; a fresh spend rejects there
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "1" }},
          expect: {{ outcome: rejected, violates: ["#meters", "§15.2"] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "hierarchical-delete-without-reinsert");
}

// ── SELF-RED-TEAM C — the company (top-level) level still enforces after reuse ──
// After the account pool is deleted and reinserted, the company-level meter (whose
// pool ct is untouched) must still constrain: with the company downsized to 150, a
// spend clearing the account's fresh 200 but exceeding the company's remaining 110
// rejects at the company level.
#[test]
fn hierarchical_company_level_still_enforces_after_account_reuse() {
    let package = PACKAGE.replace(r#"topups: { ct: { amount: "1000" } }"#, r#"topups: { ct: { amount: "150" } }"#);
    let text = format!(
        r##"{{
      format: 1
      name: hierarchical-company-still-enforces
      suite: scenario
      spec: ["#meters", "§15.4", "§15.2"]
      package: {{ {package} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.remove_topup", args: {{ company: "co", account: "a1", topup: "at1" }},
          expect: {{ outcome: ok }} }}
        {{ call: "public.wallet.add_topup", args: {{ company: "co", account: "a1", id: "at1", amount: "200" }},
          expect: {{ outcome: ok }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [
          {{ id: "co", balance: "110", accounts: [ {{ id: "a1", balance: "200" }} ] }} ] }} }}
        // account (200) clears 120 but company (110) does not → company-level reject
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "120" }},
          expect: {{ outcome: rejected, violates: ["#meters", "§15.4", "§15.2"] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "hierarchical-company-still-enforces");
}

/// The immune shape: the nested account pool has a UUID `$key` (`uuid()` default),
/// so an app cannot reuse a generated key — the hierarchical conflation cannot arise
/// and the fix must be transparent to it.
const PACKAGE_UUID_POOL: &str = r##"
    $liasse: 1
    $app: "t.meters.hier.reinsert.uuid@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      companies: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "text", amount: "decimal" }
        $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
        accounts: {
          $key: "id"
          id: "text"
          topups: { $key: "id", id: "uuid = uuid()", amount: "decimal" }
          $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
          spends: {
            $key: "id"
            $consumes: "credits"
            id: "uuid = uuid()"
            amount: "decimal"
            occurred_at: "timestamp = now()"
          }
          $mut: {
            consume: [ "spend = .spends + { amount: @amount }", "return spend { id }" ]
            add_topup: [ "topup = .topups + { amount: @amount }", "return topup { id }" ]
            remove_topup: ".topups - @topup"
          }
        }
      }
      $public: {
        wallet: {
          $view: '''.companies {
            id,
            balance: .credits.balance,
            accounts: .accounts { id, balance: .credits.balance }
          }'''
          $mut: {
            consume: ".companies[@company].accounts[@account].consume"
            add_topup: ".companies[@company].accounts[@account].add_topup"
            remove_topup: ".companies[@company].accounts[@account].remove_topup"
          }
        }
      }
    }
    $data: {
      companies: { co: { topups: { ct: { amount: "1000" } }, accounts: { a1: {} } } }
    }
"##;

// ── SELF-RED-TEAM D — UUID-keyed nested pool is immune and unregressed ─────────
// A generated-key nested pool cannot have its key reused, so the fix is transparent:
// add a 100 pool, spend 40 (account 60), delete it (account 0, funding frozen), add
// a DIFFERENT 100 pool (fresh uuid, fresh incarnation) → account 100, and a
// 100-spend admits against it.
#[test]
fn hierarchical_uuid_keyed_pool_immune_and_unregressed() {
    let text = format!(
        r##"{{
      format: 1
      name: hierarchical-uuid-keyed-pool-immune
      suite: scenario
      spec: ["#identity", "§D.1", "#meters", "§15.4", "§15.2", "§22.1"]
      package: {{ {PACKAGE_UUID_POOL} }}
      steps: [
        {{ call: "public.wallet.add_topup", args: {{ company: "co", account: "a1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:p1" }} }} }}
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "40" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "60" }} ] }} ] }} }}
        {{ call: "public.wallet.remove_topup", args: {{ company: "co", account: "a1", topup: "$ref:p1" }},
          expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "0" }} ] }} ] }} }}
        {{ call: "public.wallet.add_topup", args: {{ company: "co", account: "a1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "960", accounts: [ {{ id: "a1", balance: "100" }} ] }} ] }} }}
        {{ call: "public.wallet.consume", args: {{ company: "co", account: "a1", amount: "100" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [
          {{ id: "co", balance: "860", accounts: [ {{ id: "a1", balance: "0" }} ] }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "hierarchical-uuid-keyed-pool-immune");
}
