#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]
//! RED-TEAM probe — cross-feature: temporal bucket (§14) -> meter (§15) -> `$check`/
//! aggregate (§8/§7.5).
//!
//! ── FINDING ──────────────────────────────────────────────────────────────────
//! A bare read of a **nested** bucketed collection is NOT filtered to its active
//! rows (§14.1/§14.2). After a nested bucketed row's interval has ended, an
//! ordinary `.topups` read (a `$view`, a projection member, an aggregate, a `$mut`
//! `assert`/`$check`) still exposes the expired row — i.e. it behaves like
//! `.$all` — so `count(.topups)`/`sum(.topups.amount)` and any check that gates on
//! them observe stale rows and admit or deny WRONGLY.
//!
//!   §14.1: "For a bucketed collection: `.sessions` rows active at the evaluation
//!           time ... `.sessions.$all` all extant rows independently of current
//!           activity."
//!   §14.2: "Bucket expiration changes active views but does not delete the row.
//!           `.$all` continues to expose it until an explicit deletion."
//!   §15.1's canonical "Simple credits" nests a bucketed `topups` under `users`,
//!           so §14.1 applies to a nested bucketed collection exactly as to a
//!           top-level one; the meter reads only the pool rows active at spend time.
//!
//! Root cause: `liasse-runtime/src/materialize.rs::build_row` materializes every
//! nested keyed collection with `filter_active = false` (materialize.rs:387; see
//! the comment at materialize.rs:255-256 "nested collections are read in full"),
//! so the `Temporal::keep` activity predicate is applied ONLY to top-level
//! collections. The nested bucket IS registered (`compiled.rs::compile_buckets_at`
//! recurses into nested collections) and each expired nested row still carries its
//! `$from`/`$until` interval cells (materialize.rs:282) — which is exactly why the
//! §15 meter pool resolution (`meter/resolve.rs::active_at`) filters the same
//! nested bucket correctly. The runtime therefore KNOWS the row is inactive and
//! honours that in the meter path, yet a bare read of the same collection ignores
//! it, in violation of §14.1/§14.2.
//!
//! Two FINDING tests FAIL, each on the divergence itself (both packages load):
//!   * `nested_bucket_bare_read_ignores_expiry` — read path: the SPEC-correct
//!     post-expiry `count(.topups)` is 0, the runtime yields 1 (the extant `.$all`
//!     set). In the same view `count(.topups.$all)` correctly stays 1, so the
//!     runtime DOES distinguish active from all here — it just applies the wrong one
//!     to the bare read.
//!   * `nested_bucket_check_reads_expired_row` — `$check`/assert path (charge 3):
//!     after rollover `assert(count(.topups) == 0)` MUST admit, but the runtime
//!     counts the expired row (== 1) and REJECTS — a WRONG DENY on stale state (the
//!     symmetric WRONG ADMIT gates a privilege that should have expired).
//! Paired CONTROLs PASS and isolate the defect:
//!   * `control_toplevel_bucket_read_filters_on_expiry` — the IDENTICAL rule on a
//!     TOP-LEVEL bucket filters to 0 (only the nesting differs).
//!   * `control_nested_meter_filters_while_bare_read_does_not` — same nested
//!     bucketed collection, same clock: the meter reports balance 0 (filtered) while
//!     the bare `count(.topups)` reports 1 (unfiltered), pinning the inconsistency.
//!
//! The other three charges hold cleanly and are recorded DRY (see the `dry_*`
//! tests): the full source-bucket -> meter -> assert-guard chain enforces
//! correctly; a spend against an empty pool is a clean rejection (not an
//! `EngineError::Internal`/panic); and a meter over a nested bucket after rollover
//! rejects cleanly. Every expectation is derived from SPEC.md text alone.

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
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation (see dump)");
}

// ── FINDING 1 (read path) ────────────────────────────────────────────────────
// §14.1/§14.2: a nested bucketed `topups` row whose interval has ended MUST leave
// the active `.topups` view (still visible only through `.$all`). A bare
// `count(.topups)` in a `$view` MUST read 0 after rollover. The runtime keeps
// returning 1 (the expired row), so the post-expiry view mismatches.
#[test]
fn nested_bucket_bare_read_ignores_expiry() {
    // topup t1 expires at 2026-01-08T00:00:00Z (1767830400 s). Genesis is
    // 2026-01-01; advancing P8D reaches 2026-01-09, strictly past the half-open
    // upper bound, so t1 is inactive (§14.1 `[from, until)`).
    let text = r##"{
      format: 1
      name: nested-bucket-bare-read-ignores-expiry
      suite: scenario
      spec: ["#buckets", "§14.1", "§14.2", "#aggregates", "§7.5"]
      package: {
        $liasse: 1
        $app: "t.nbre@1.0.0"
        $semantics: { timestamp_precision: "s" }
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
          }
          $public: {
            uview: { $view: ".users { id, active: count(.topups), extant: count(.topups.$all) }" }
          }
        }
        $data: {
          users: { u1: { topups: { t1: { amount: "100", expires_at: "1767830400" } } } }
        }
      }
      steps: [
        // before expiry: the single topup is active AND extant.
        { watch: "public.uview", id: "wv", expect_init: { value: [ { id: "u1", active: "1", extant: "1" } ] } }
        { advance_time: "P8D" }
        // §14.1: t1's interval has ended -> active view empty -> active == 0;
        // §14.2: `.$all` still exposes it -> extant == 1.
        { expect_view: { watch: "wv", value: [ { id: "u1", active: "0", extant: "1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "nested-bucket-bare-read-ignores-expiry");
}

// ── FINDING 2 (`$check`/assert path) ─────────────────────────────────────────
// The same defect through a §8.8 assertion: after rollover the active nested
// bucket is empty, so `assert(count(.topups) == 0, ...)` MUST hold and the call
// MUST admit. The runtime still counts the expired row (== 1), so the assertion
// fails and the call is REJECTED — a WRONG DENY driven by stale bucket state.
#[test]
fn nested_bucket_check_reads_expired_row() {
    let text = r##"{
      format: 1
      name: nested-bucket-check-reads-expired-row
      suite: scenario
      spec: ["#buckets", "§14.1", "#aggregates", "§7.5", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.nbcheck@1.0.0"
        $semantics: { timestamp_precision: "s" }
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
            log: {
              $key: "id"
              id: "uuid = uuid()"
              n: "int"
            }
            $mut: {
              // gate a write on the ACTIVE topup count (§14.1) via a §8.8 assertion.
              require_active: [
                "assert(count(.topups) == @n, 'active-topup count mismatch')"
                "row = .log + { n: @n }"
                "return row { id }"
              ]
            }
          }
          $public: {
            api: { $mut: { require_active: ".users[@user].require_active" } }
          }
        }
        $data: {
          users: { u1: { topups: { t1: { amount: "100", expires_at: "1767830400" } } } }
        }
      }
      steps: [
        // before expiry: exactly one active topup -> assertion holds -> admits.
        { call: "public.api.require_active", args: { user: "u1", n: "1" }, expect: { outcome: ok, "...": true } }
        { advance_time: "P8D" }
        // §14.1/§8.8: the active count is now 0, so this assertion must hold -> admit.
        { call: "public.api.require_active", args: { user: "u1", n: "0" }, expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "nested-bucket-check-reads-expired-row");
}

// ── CONTROL 1 ────────────────────────────────────────────────────────────────
// The IDENTICAL §14.1 rule on a TOP-LEVEL bucketed collection filters correctly:
// `count(.sessions)` drops to 0 the instant the row's interval ends. Only the
// nesting differs from the FINDING, isolating the defect to nested collections.
#[test]
fn control_toplevel_bucket_read_filters_on_expiry() {
    let text = r##"{
      format: 1
      name: control-toplevel-bucket-read-filters
      suite: scenario
      spec: ["#buckets", "§14.1", "§14.2", "#aggregates", "§7.5"]
      package: {
        $liasse: 1
        $app: "t.ctltop@1.0.0"
        $semantics: { timestamp_precision: "s" }
        $model: {
          sessions: {
            $key: "id"
            $bucket: { $until: ".expires_at" }
            id: "text"
            expires_at: "timestamp"
          }
          $public: {
            active_count: { $view: "count(.sessions)" }
            all_count: { $view: "count(.sessions.$all)" }
          }
        }
        $data: { sessions: { s1: { expires_at: "1767830400" } } }
      }
      steps: [
        { watch: "public.active_count", id: "wa", expect_init: { value: "1" } }
        { watch: "public.all_count", id: "wx", expect_init: { value: "1" } }
        { advance_time: "P8D" }
        // §14.1: the active view drops the expired row.
        { expect_view: { watch: "wa", value: "0" } }
        // §14.2: `.$all` still exposes it.
        { expect_view: { watch: "wx", value: "1" } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-toplevel-bucket-read-filters");
}

// ── CONTROL 2 (post-fix: consistency) ────────────────────────────────────────
// Same nested bucketed collection, same clock, two readers side by side: the §15
// meter pool resolution filters the expired nested pool (`.credits.balance` -> 0),
// and — once the §14.1 nested bare-read filter is honoured — the bare
// `count(.topups)` ALSO drops to 0. Pinning both observed values in one control
// now proves CONSISTENCY: the meter path and the bare-read path agree the expired
// nested bucket row is inactive. (Before the fix this control pinned `active: "1"`
// to characterise the inconsistency the FINDING asserts; that value contradicts
// §14.1 — a bare read of a bucketed collection exposes only its rows active at the
// clock — so it is corrected here to the spec-derived `0` the fix produces.)
#[test]
fn control_nested_meter_filters_while_bare_read_does_not() {
    let text = r##"{
      format: 1
      name: control-nested-meter-filters-bare-read-does-not
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.1"]
      package: {
        $liasse: 1
        $app: "t.ctlmeter@1.0.0"
        $semantics: { timestamp_precision: "s" }
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
            $limits: {
              credits: {
                $sources: { topup: ".topups { $quantity: .amount }" }
                $order: ["$until"]
              }
            }
          }
          $public: {
            // bal is the §15.6 meter accessor (filters the nested pool by activity);
            // active is a bare §14.1 read of the same nested bucketed collection.
            uview: { $view: ".users { id, bal: .credits.balance, active: count(.topups) }" }
          }
        }
        $data: { users: { u1: { topups: { t1: { amount: "100", expires_at: "1767830400" } } } } }
      }
      steps: [
        { watch: "public.uview", id: "wv", expect_init: { value: [ { id: "u1", bal: "100", active: "1" } ] } }
        { advance_time: "P8D" }
        // meter: pool inactive at the clock -> balance 0 (§15.1, filtered).
        // bare read: §14.1 nested bare-read filter -> the expired row leaves the
        // active view -> active 0 (consistent with the meter).
        { expect_view: { watch: "wv", value: [ { id: "u1", bal: "0", active: "0" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-nested-meter-filters-bare-read-does-not");
}

// ── DRY 1 (charge 1) ─────────────────────────────────────────────────────────
// The full chain source-backed temporal bucket (§14.5) -> meter (§15.1/§15.2) ->
// `$mut` `assert` guard (§8.8) enforces correctly: an in-capacity guarded spend
// admits, an over-capacity one is rejected by the assert, and after every period
// has expired the guard rejects cleanly (no `EngineError::Internal`, no panic).
#[test]
fn dry_full_chain_guarded_consume() {
    let text = r##"{
      format: 1
      name: dry-full-chain-guarded-consume
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.4", "§14.5", "#mutations", "§8.8"]
      package: {
        $liasse: 1
        $app: "t.dry.a@1.0.0"
        $semantics: { timestamp_precision: "s" }
        $model: {
          plans: { $key: "id", id: "text", credits: "decimal", period: "period?" }
          subscriptions: {
            $key: "id", id: "text"
            account: { $ref: "/accounts" }
            plan: { $ref: "/plans" }
            starts_at: "timestamp"
            ends_at: "timestamp? = none"
          }
          credit_periods: {
            $bucket: {
              $source: ".subscriptions"
              $from: "$source.starts_at"
              $until: "$source.ends_at"
              $repeat: "/plans[$source.plan].period"
            }
            credits: "= /plans[$source.plan].credits"
          }
          accounts: {
            $key: "id", id: "text"
            $limits: {
              credits: {
                $sources: { subscription: '''/credit_periods[:p | p.$source.account == .] { $quantity: .credits }''' }
                $order: ["$until"]
              }
            }
            spends: {
              $key: "id"
              $consumes: "credits"
              id: "uuid = uuid()"
              amount: "decimal"
              occurred_at: "timestamp = now()"
            }
            $mut: {
              guarded: [
                "assert(.credits.balance >= @amount, 'insufficient balance')"
                "spend = .spends + { amount: @amount }"
                "return spend { id }"
              ]
            }
          }
          $public: {
            wallet: {
              $view: ".accounts { id, balance: .credits.balance }"
              $mut: { guarded: ".accounts[@account].guarded" }
            }
          }
        }
        $data: {
          plans: { fixed: { credits: "50", period: "= none" } }
          accounts: { a1: {} }
          subscriptions: {
            sub1: { account: "a1", plan: "fixed", starts_at: "1767225600", ends_at: "1767830400" }
          }
        }
      }
      steps: [
        { call: "public.wallet.guarded", args: { account: "a1", amount: "10" },
          expect: { outcome: ok, value: { id: "$any:uuid" } } }
        { watch: "public.wallet", id: "w1", expect_init: { value: [ { id: "a1", balance: "40" } ] } }
        // guard rejects: balance 40 < 100 (§8.8 assertion).
        { call: "public.wallet.guarded", args: { account: "a1", amount: "100" },
          expect: { outcome: rejected, violates: ["#mutations", "§8.8"] } }
        { advance_time: "P8D" }
        // period expired -> balance 0 -> guard rejects cleanly.
        { call: "public.wallet.guarded", args: { account: "a1", amount: "10" },
          expect: { outcome: rejected, violates: ["#mutations", "§8.8"] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "dry-full-chain-guarded-consume");
}

// ── DRY 2 (charge 2) ─────────────────────────────────────────────────────────
// A meter with an EMPTY pool (a source-backed bucket with no source rows) rejects
// a positive spend cleanly for insufficient capacity (§15.1/§15.2) — never an
// engine-invariant fault or panic — and a zero-amount spend admits with no funding
// (§15.1 "Zero is valid and produces no funding rows").
#[test]
fn dry_empty_pool_clean_rejection() {
    let text = r##"{
      format: 1
      name: dry-empty-pool-clean-rejection
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.4"]
      package: {
        $liasse: 1
        $app: "t.dry.b@1.0.0"
        $semantics: { timestamp_precision: "s" }
        $model: {
          plans: { $key: "id", id: "text", credits: "decimal", period: "period?" }
          subscriptions: {
            $key: "id", id: "text"
            account: { $ref: "/accounts" }
            plan: { $ref: "/plans" }
            starts_at: "timestamp"
            ends_at: "timestamp? = none"
          }
          credit_periods: {
            $bucket: {
              $source: ".subscriptions"
              $from: "$source.starts_at"
              $until: "$source.ends_at"
              $repeat: "/plans[$source.plan].period"
            }
            credits: "= /plans[$source.plan].credits"
          }
          accounts: {
            $key: "id", id: "text"
            $limits: {
              credits: {
                $sources: { subscription: '''/credit_periods[:p | p.$source.account == .] { $quantity: .credits }''' }
                $order: ["$until"]
              }
            }
            spends: {
              $key: "id"
              $consumes: "credits"
              id: "uuid = uuid()"
              amount: "decimal"
              occurred_at: "timestamp = now()"
            }
            $mut: {
              consume: [
                "spend = .spends + { amount: @amount }"
                "return spend { id }"
              ]
            }
          }
          $public: {
            wallet: {
              $view: ".accounts { id, balance: .credits.balance }"
              $mut: { consume: ".accounts[@account].consume" }
            }
          }
        }
        $data: {
          plans: { fixed: { credits: "50", period: "= none" } }
          accounts: { a1: {} }
          subscriptions: {}
        }
      }
      steps: [
        // §7.5/§15.1: empty pool -> balance is a clean numeric zero.
        { watch: "public.wallet", id: "w1", expect_init: { value: [ { id: "a1", balance: "0" } ] } }
        // §15.2: positive spend against zero eligible capacity -> clean rejection.
        { call: "public.wallet.consume", args: { account: "a1", amount: "1" },
          expect: { outcome: rejected, violates: ["#meters", "§15.2"] } }
        // §15.1: zero spend produces no funding rows -> admits.
        { call: "public.wallet.consume", args: { account: "a1", amount: "0" },
          expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "dry-empty-pool-clean-rejection");
}

// ── DRY 3 (charge 4) ─────────────────────────────────────────────────────────
// A meter whose nested bucketed pool is empty at spend time after a stored-bucket
// rollover rejects the spend cleanly (§15.1 temporal pool context, §14.1), and a
// zero spend still admits. The meter path correctly re-derives nested-bucket
// activity (contrast the FINDING's bare-read path).
#[test]
fn dry_meter_over_nested_bucket_rollover() {
    let text = r##"{
      format: 1
      name: dry-meter-over-nested-bucket-rollover
      suite: scenario
      spec: ["#meters", "§15.1", "§15.2", "#buckets", "§14.1"]
      package: {
        $liasse: 1
        $app: "t.dry.d@1.0.0"
        $semantics: { timestamp_precision: "s" }
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
            $limits: {
              credits: {
                $sources: { topup: ".topups { $quantity: .amount }" }
                $order: ["$until"]
              }
            }
            $mut: {
              consume: [
                "spend = .spends + { amount: @amount }"
                "return spend { id }"
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
        $data: { users: { u1: { topups: { t1: { amount: "100", expires_at: "1767830400" } } } } }
      }
      steps: [
        { call: "public.wallet.consume", args: { user: "u1", amount: "10" }, expect: { outcome: ok, "...": true } }
        { advance_time: "P8D" }
        { watch: "public.wallet", id: "w1", expect_init: { value: [ { id: "u1", balance: "0" } ] } }
        // pool expired -> insufficient capacity -> clean rejection.
        { call: "public.wallet.consume", args: { user: "u1", amount: "10" },
          expect: { outcome: rejected, violates: ["#meters", "§15.2"] } }
        // zero spend admits (§15.1).
        { call: "public.wallet.consume", args: { user: "u1", amount: "0" }, expect: { outcome: ok, "...": true } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "dry-meter-over-nested-bucket-rollover");
}
