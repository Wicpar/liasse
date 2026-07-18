//! Regression (dual-backend): a §20 migration of an instance holding nested
//! keyed-collection rows (§5.4) MUST fail closed — refuse loudly — rather than
//! silently drop the nested rows and report `committed`.
//!
//! # The bug this pins (now fixed, fail-closed)
//!
//! `StateSection::capture` (crates/liasse-runtime/src/portable.rs) captures the
//! rows of every **top-level** collection and the §8.2 singleton only; it never
//! carries a nested collection forward. `Engine::update` built the migration's
//! read-only `$old` from exactly that capture and `build_migrated` staged only
//! what `$old` carried, so a package that keeps application data in a nested keyed
//! collection (§5.4 — a first-class LIVE feature: `companies/co/accounts/a1`
//! addressing, inbound refs, meters) used to lose ALL of that nested data the
//! moment it was migrated — even by a byte-identical patch bump — while still
//! reporting `committed` (§9.4). That is silent data loss, violating §20.1 ("the
//! compatible value is copied"), §22.1 (committed-state integrity), and AGENTS.md's
//! fail-closed rule.
//!
//! # The fix (asserted here)
//!
//! Faithful nested-collection carry-through is a large, separate feature and
//! remains tracked. The SAFETY fix makes the capture seam **fail-closed**:
//! `StateSection::capture` now refuses when the instance holds nested rows, so a
//! migration is **rejected** (never a silent `committed`) and the prior state is
//! left intact. These tests therefore assert the migration is REFUSED and the
//! nested data is preserved unchanged — the honest refusal that replaces the
//! silent drop. Both backends agree step-for-step (memory == pg), so the refusal
//! is a runtime decision, not a store-contract divergence.
//!
//! The `top_level_*` test is the PASSING control proving top-level migration still
//! preserves every row and commits — the fail-closed guard fires only on actual
//! nested rows, so it never spuriously rejects the top-level path. The companion
//! export refusal is pinned by the runtime test
//! `crates/liasse-runtime/tests/redteam_nested_export_dataloss.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::path::Path;

use liasse_ident::InstanceId;
use liasse_pg::{PgStore, PgStoreFactory};
use liasse_store::StoreFactory;
use liasse_testkit::{
    run_case, Area, Case, CaseResult, MemoryProvision, ScenarioAdapter, StepResult, StepTrace,
    StoreProvision, SuiteKind,
};

struct PgProvision {
    factory: PgStoreFactory,
    created: Vec<InstanceId>,
    seen: BTreeSet<String>,
}
impl PgProvision {
    fn new(factory: PgStoreFactory) -> Self {
        Self { factory, created: Vec::new(), seen: BTreeSet::new() }
    }
    fn cleanup(&self) {
        for i in &self.created {
            let _ = self.factory.drop_instance(i);
        }
    }
}
impl StoreProvision for PgProvision {
    type Store = PgStore;
    fn provision(&mut self, instance: InstanceId) -> Result<Self::Store, String> {
        if self.seen.insert(instance.as_str().to_owned()) {
            self.created.push(instance.clone());
        }
        self.factory.create(instance).map_err(|e| e.to_string())
    }
}

fn run_both(name: &str, hjson: &str) -> (CaseResult, CaseResult) {
    let case = Case::from_hjson(hjson, Path::new(name), &BTreeSet::new()).expect("parses");
    let memory = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut a = ScenarioAdapter::build_with(&mut MemoryProvision, &case);
        run_case(&mut a, &Area::new(name), SuiteKind::Red, &case)
    }))
    .expect("memory ok");
    let handle = support::acquire();
    let mut provision = PgProvision::new(handle.factory("nestmig"));
    let pg = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut a = ScenarioAdapter::build_with(&mut provision, &case);
        run_case(&mut a, &Area::new(name), SuiteKind::Red, &case)
    }));
    provision.cleanup();
    (memory, pg.expect("pg ok"))
}

/// Print each step for both backends, assert the two agree step-for-step (store
/// contract), and return whether both PASSED the first `upto` steps.
fn agree_and_report(memory: &CaseResult, pg: &CaseResult, upto: usize, ctx: &str) {
    let sig = |s: &StepTrace| match &s.result {
        StepResult::Pass => {
            format!("pass:{}", s.observed.as_ref().map_or_else(String::new, ToString::to_string))
        }
        StepResult::Fail { reason } => format!("FAIL({reason})"),
        StepResult::Skipped { reason } => format!("skip({reason})"),
        StepResult::Unspecified { .. } => "unspec".to_owned(),
    };
    println!("\n=== {ctx} ===");
    for (i, (m, p)) in memory.steps.iter().zip(&pg.steps).enumerate() {
        println!("  step {i} ({})\n     memory: {}\n     pg:     {}", m.action, sig(m), sig(p));
    }
    assert_eq!(memory.steps.len(), pg.steps.len(), "{ctx}: step count diverges");
    for (m, p) in memory.steps.iter().zip(&pg.steps) {
        let passed = |s: &StepTrace| matches!(s.result, StepResult::Pass);
        assert_eq!(
            passed(m),
            passed(p),
            "{ctx}: STORE DIVERGENCE at step {} ({})",
            m.index,
            m.action
        );
    }
    for (label, result) in [("memory", memory), ("pg", pg)] {
        for (index, step) in result.steps.iter().take(upto).enumerate() {
            assert!(
                step.result.is_pass(),
                "{ctx}[{label}]: step {index} ({}) did not pass: {:?}",
                step.action,
                step.result
            );
        }
    }
}

/// FAIL-CLOSED REGRESSION (plain nested collection, no meters). `users.notes` is
/// an ordinary nested keyed collection (§5.4). A parent computed member
/// `n = count(.notes)` reads it; before migration `n == 1`. A byte-identical
/// PATCH-bump migration cannot carry the nested `notes` row through this build's
/// portable capture, so — rather than silently drop it and commit (§20.1/§22.1) —
/// the migration is now REFUSED (`host_load` observes `rejected`). Because a
/// rejected migration is atomic and leaves the instance unchanged, the nested row
/// survives and `n` is still `1`. The prior engine committed the drop and `n`
/// became `0`; this test now pins the honest refusal. Both backends agree.
#[test]
fn nested_collection_migration_is_refused_leaving_rows_intact() {
    let model = r##"
          users: {
            $key: "id"
            id: "text"
            notes: { $key: "id", id: "text", body: "text" }
            n: "= count(.notes)"
          }
          $public: { users: { $view: ".users { id, n, $sort: [id] }" } }
    "##;
    let hjson = format!(
        r##"{{
      format: 1
      name: nested-collection-migration-is-refused-leaving-rows-intact
      suite: scenario
      spec: ["#evolution", "§20.1", "#state-model", "§22.1"]
      package: {{
        $liasse: 1
        $app: "t.nestnotes@1.0.0"
        $model: {{ {model} }}
        $data: {{ users: {{ u1: {{ notes: {{ k1: {{ body: "hello" }} }} }} }} }}
      }}
      steps: [
        {{ watch: "public.users", id: "w0",
          expect_init: {{ value: [ {{ id: "u1", n: "1" }} ] }} }}
        // §20.1/§22.1 fail-closed: the capture cannot carry the nested `notes` row,
        // so the migration is refused instead of committing with it dropped.
        {{ host_load: {{
            package: {{ $liasse: 1, $app: "t.nestnotes@1.0.1", $model: {{ {model} }} }}
          }}
          expect: {{ outcome: rejected, violates: ["#evolution", "§20.1", "#state-model", "§22.1"] }} }}
        // The refusal is atomic: the instance is unchanged, so the nested row is
        // preserved and the count is still `1` (no silent drop).
        {{ watch: "public.users", id: "w1",
          expect_init: {{ value: [ {{ id: "u1", n: "1" }} ] }} }}
      ]
    }}"##
    );
    let (memory, pg) = run_both("nested-collection-migration-is-refused-leaving-rows-intact", &hjson);
    agree_and_report(&memory, &pg, 3, "nested collection migration refused, rows intact");
}

/// FAIL-CLOSED REGRESSION (meter consequence — driving a meter concern through a
/// migration). `users.topups` is the nested pool source of the `credits` meter;
/// before migration `.credits.balance == 50`. A migration whose `$as` NEGATES the
/// pool source (`50 -> -50`) would project a negative pool `$quantity` (§15.1).
/// The prior engine dropped the nested `topups` entirely, so the pool was empty
/// (quantity 0, not negative), nothing was rejected, and the poisoning migration
/// committed `ok` with `balance == 0` — silent data loss that also defeated the
/// §15.1 guard. Now the capture refuses to carry the nested pool source, so the
/// whole migration is REFUSED (`rejected`) before any meter re-funding runs; the
/// broader fail-closed guard (§20.1/§22.1) subsumes the specific §15.1 probe. The
/// instance is left unchanged. Both backends agree.
#[test]
fn migration_negating_nested_pool_source_is_refused() {
    let src = |amount_decl: &str| {
        format!(
            r##"
          users: {{
            $key: "id"
            id: "text"
            topups: {{ $key: "id", id: "text", amount: {amount_decl} }}
            spends: {{
              $key: "id"
              $consumes: "credits"
              id: "uuid = uuid()"
              amount: "decimal"
              occurred_at: "timestamp = now()"
            }}
            $limits: {{ credits: {{ $sources: {{ topup: ".topups {{ $quantity: .amount }}" }} }} }}
          }}
          $public: {{ wallet: {{ $view: ".users {{ id, balance: .credits.balance }}" }} }}
    "##
        )
    };
    let v1 = src("\"decimal\"");
    let v2 = src("{ $type: \"decimal\", $from: \"amount\", $as: \"0 - .\" }");
    let hjson = format!(
        r##"{{
      format: 1
      name: migration-negating-nested-pool-source-is-refused
      suite: scenario
      spec: ["#meters", "§15.1", "#evolution", "§20.1", "#state-model", "§22.1"]
      package: {{
        $liasse: 1
        $app: "t.metermig@1.0.0"
        $semantics: {{ timestamp_precision: "s" }}
        $model: {{ {v1} }}
        $data: {{ users: {{ u1: {{ topups: {{ t1: {{ amount: "50" }} }} }} }} }}
      }}
      steps: [
        {{ watch: "public.wallet", id: "w0",
          expect_init: {{ value: [ {{ id: "u1", balance: "50" }} ] }} }}
        {{ host_load: {{
            package: {{
              $liasse: 1, $app: "t.metermig@2.0.0",
              $semantics: {{ timestamp_precision: "s" }},
              $model: {{ {v2} }}
            }}
          }}
          // §20.1/§22.1 fail-closed: the capture refuses to carry the nested pool
          // source, so the migration is rejected before it could either poison the
          // meter or silently drop the pool. (The prior engine dropped it and
          // committed `ok`.)
          expect: {{ outcome: rejected, violates: ["#evolution", "§20.1", "#state-model", "§22.1"] }} }}
      ]
    }}"##
    );
    let (memory, pg) = run_both("migration-negating-nested-pool-source-is-refused", &hjson);
    agree_and_report(&memory, &pg, 2, "migration negating nested pool source refused");
}

/// PASSING CONTROL: a byte-identical patch-bump migration of a package whose data
/// lives in a TOP-LEVEL collection preserves every row and commits — proving the
/// migration path and this harness are sound for the top-level case, and that the
/// fail-closed guard fires only on actual nested rows (it never spuriously rejects
/// a top-level-only migration).
#[test]
fn top_level_migration_preserves_rows_control() {
    let model = r##"
          notes: { $key: "id", id: "text", body: "text" }
          $public: { notes: { $view: ".notes { id, body, $sort: [id] }" } }
    "##;
    let hjson = format!(
        r##"{{
      format: 1
      name: top-level-migration-preserves-rows-control
      suite: scenario
      spec: ["#evolution", "§20.1"]
      package: {{
        $liasse: 1
        $app: "t.toplvl@1.0.0"
        $model: {{ {model} }}
        $data: {{ notes: {{ k1: {{ body: "hello" }} }} }}
      }}
      steps: [
        {{ watch: "public.notes", id: "w0",
          expect_init: {{ value: [ {{ id: "k1", body: "hello" }} ] }} }}
        {{ host_load: {{
            package: {{ $liasse: 1, $app: "t.toplvl@1.0.1", $model: {{ {model} }} }}
          }}
          expect: {{ outcome: ok, result: committed }} }}
        {{ watch: "public.notes", id: "w1",
          expect_init: {{ value: [ {{ id: "k1", body: "hello" }} ] }} }}
      ]
    }}"##
    );
    let (memory, pg) = run_both("top-level-migration-preserves-rows-control", &hjson);
    agree_and_report(&memory, &pg, 3, "top-level migration preserves rows (control)");
}
