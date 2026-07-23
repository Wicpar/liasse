#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED-TEAM probe — §15.2 "Deleting a spend releases its current allocation", and
//! its root cause: the `-selection` delete path could not RESOLVE (and so never
//! removed) a row addressed through `.$all` or a nested collection.
//!
//! ── FINDING ──────────────────────────────────────────────────────────────────
//! `-.spends.$all[:s | s.id == @spend]` (the §14.2 bucket-inactive delete form used
//! by metered spends) silently no-opped. Two seams stacked in
//! `interp.rs::exec_delete_selection`. First, it stripped only the OUTER
//! `[selector]`, leaving `.spends.$all` as the collection base — which
//! `collection_ref` cannot resolve (a `.$all` node is a structural `Field`, not a
//! collection) — so the whole delete returned `Ok(())` without touching a row.
//! Second, even once resolved, it addressed every row through the TOP-LEVEL cascade
//! graph by `(leaf-collection, leaf-key)`, which cannot name a NESTED row (missing
//! the parent key), so a nested `spends`/`pools` delete no-opped. With the row
//! never removed, its frozen §15.2 funding allocation stayed held by an "extant"
//! spend, so the pool balance never restored — the allocation leaked.
//!
//! Root cause / fix: `exec_delete_selection` now peels the selection down to its
//! collection (`selection_collection`, seeing through `.$all` and `[selector]`) and
//! removes a nested collection's row by its full address (`remove_subtree`),
//! mirroring the sibling keyed-delete path (`exec_delete`).
//!
//! The FINDING regression is the corpus case
//! `tests/15-meters/red/inactive-bucketed-spend-retains-allocation.hjson` (delete of
//! an already-inactive bucketed spend releases its allocation) plus the top-level
//! `tests/14-buckets/common/expiration-preserves-row-in-all.hjson`, both formerly on
//! the scenario debt ledger. These probes self-red-team the neighbouring edges the
//! two corpus cases do not pin: PARTIAL release (one of several spends on one pool),
//! delete-then-RE-SPEND at full restored capacity, and the nested non-`.$all`
//! filtered delete form.

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

/// A wallet package whose nested `spends` collection `$consumes` a `credits` meter
/// funded by a single non-bucketed 100-unit `topup` pool. `purge` deletes through
/// the §14.2 `.$all` domain; `purge_active` deletes through the ordinary
/// active-only selector (no `.$all`).
const PACKAGE: &str = r##"
    $liasse: 1
    $app: "t.meters.release@1.0.0"
    $semantics: { timestamp_precision: "s" }
    $model: {
      users: {
        $key: "id"
        id: "text"
        topups: { $key: "id", id: "text", amount: "decimal" }
        spends: {
          $key: "id"
          $consumes: "credits"
          $bucket: ".expires_at"
          id: "uuid = uuid()"
          amount: "decimal"
          occurred_at: "timestamp = now()"
          expires_at: "timestamp"
        }
        $limits: { credits: { $sources: { topup: ".topups { $quantity: .amount }" } } }
        $mut: {
          consume: [
            "spend = .spends + { amount: @amount, expires_at: @until }"
            "return spend { id }"
          ]
          purge: "-.spends.$all[:s | s.id == @spend]"
          purge_active: "-.spends[:s | s.id == @spend]"
        }
      }
      $public: {
        wallet: {
          $view: ".users { id, balance: .credits.balance }"
          $mut: {
            consume: ".users[@user].consume"
            purge: ".users[@user].purge"
            purge_active: ".users[@user].purge_active"
          }
        }
      }
    }
    $data: { users: { u1: { topups: { t1: { amount: "100" } } } } }
"##;

// ── SELF-RED-TEAM 1 — partial release, one of several spends on one pool ───────
// Pool holds 100. Two extant spends hold 60 and 20 (balance 20). Deleting the 60
// releases ONLY its allocation: §15.2 "Changing or removing a pool never rewrites
// an earlier funding allocation" — the surviving 20 stays held, so the balance
// restores to 80, not 100. Deleting the 20 then restores the full 100.
#[test]
fn partial_release_one_of_several_spends() {
    let text = format!(
        r##"{{
      format: 1
      name: partial-release-one-of-several
      suite: scenario
      spec: ["#meters", "§15.2"]
      tags: [temporal]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "60", until: "1767830400" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "20", until: "1767830400" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s2" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "20" }} ] }} }}
        {{ call: "public.wallet.purge", args: {{ user: "u1", spend: "$ref:s1" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "80" }} ] }} }}
        {{ call: "public.wallet.purge", args: {{ user: "u1", spend: "$ref:s2" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "partial-release-one-of-several");
}

// ── SELF-RED-TEAM 2 — release enables a full re-spend ─────────────────────────
// Deleting the only spend restores the pool to full 100. A fresh spend of the FULL
// 100 must then admit (§15.2 step 6 rejects only insufficient capacity). If the
// released allocation leaked, the pool would show 40 and this spend would reject —
// so an admitted 100 (balance 0 afterwards) proves the release actually happened.
#[test]
fn release_enables_full_respend() {
    let text = format!(
        r##"{{
      format: 1
      name: release-enables-full-respend
      suite: scenario
      spec: ["#meters", "§15.2"]
      tags: [temporal]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "60", until: "1767830400" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "40" }} ] }} }}
        {{ call: "public.wallet.purge", args: {{ user: "u1", spend: "$ref:s1" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "100", until: "1767830400" }},
          expect: {{ outcome: ok, value: {{ id: "$any:uuid" }} }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "0" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "release-enables-full-respend");
}

// ── SELF-RED-TEAM 3 — nested filtered delete WITHOUT `.$all` releases ──────────
// The nested-address fix must hold independently of the `.$all` selector: deleting
// an ACTIVE nested spend through the ordinary `-.spends[:s | pred]` form (no
// `.$all`) also removes the row and releases its allocation, restoring the balance.
#[test]
fn nested_filtered_delete_without_all_releases() {
    let text = format!(
        r##"{{
      format: 1
      name: nested-filtered-delete-without-all
      suite: scenario
      spec: ["#meters", "§15.2", "§14.2"]
      tags: [temporal]
      package: {{ {PACKAGE} }}
      steps: [
        {{ call: "public.wallet.consume", args: {{ user: "u1", amount: "60", until: "1767830400" }},
          expect: {{ outcome: ok, value: {{ id: "$bind:s1" }} }} }}
        {{ watch: "public.wallet", id: "w1", expect_init: {{ value: [ {{ id: "u1", balance: "40" }} ] }} }}
        {{ call: "public.wallet.purge_active", args: {{ user: "u1", spend: "$ref:s1" }}, expect: {{ outcome: ok }} }}
        {{ expect_view: {{ watch: "w1", value: [ {{ id: "u1", balance: "100" }} ] }} }}
      ]
    }}"##
    );
    assert_all_pass(&run_case_text(&text), "nested-filtered-delete-without-all");
}
