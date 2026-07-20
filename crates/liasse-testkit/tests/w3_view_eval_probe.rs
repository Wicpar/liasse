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
    if !ok { for step in &result.steps { println!("  {name} step {} [{}] -> {:?} observed={:?}", step.index, step.action, step.result, step.observed); } }
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation");
}

// Load a package JSON directly, returning the invalid-load diagnostic messages
// (`Err`) or `Ok(())` on a successful load. Used to assert the *cause* of a
// static rejection rather than merely that one occurred.
fn load_diagnostics(name: &str, package: &serde_json::Value) -> Result<(), Vec<String>> {
    use liasse_runtime::{Engine, EngineError, FixedGenerators, Precision};
    use liasse_store::MemoryStore;
    let definition = serde_json::to_string(package).expect("serialize");
    let store = MemoryStore::new(liasse_ident::InstanceId::new(name.to_owned()));
    let mut generator = FixedGenerators::new(0, Precision::Micros);
    match Engine::load(store, &definition, &mut generator) {
        Ok(_) => Ok(()),
        Err(EngineError::Invalid(diags)) => Err(diags.iter().map(|d| d.message().to_owned()).collect()),
        Err(other) => Err(vec![format!("non-invalid engine error: {other}")]),
    }
}

// DEBUG-ONLY: load a package JSON directly and print load diagnostics.
fn debug_load(name: &str, package: serde_json::Value) {
    match load_diagnostics(name, &package) {
        Ok(()) => println!("[{name}] LOADED OK"),
        Err(msgs) => {
            println!("[{name}] INVALID: {} message(s)", msgs.len());
            for m in &msgs { println!("    - {m}"); }
        }
    }
}

// DIAGNOSTIC (ignored): prints load outcomes for the view-over-view and
// combinator shapes. Records the checker gap noted in the report: a root-declared
// `$view`'s projected fields cannot be referenced by name from another expression
// (`unknown name account`) — a CLEAN load rejection, not a crash, so out of scope
// for a hard finding, but documented here. Kept ignored (no assertions).
#[test]
#[ignore = "diagnostic scaffolding (prints load outcomes)"]
fn debug_loads() {
    use serde_json::json;
    // a1: view over grouped view, project again
    debug_load("a1", json!({
        "$liasse":1, "$app":"t.a1@1.0.0",
        "$model": {
            "lines": { "$key":"id", "id":"text", "account":"text", "debit":"int" },
            "byacct": { "$view": ".lines { $key: account, account, total: sum(group.debit) }" },
            "$public": { "rollup": { "$view": ".byacct { account, total }" } }
        }
    }));
    // b1: union grouped + plain
    debug_load("b1", json!({
        "$liasse":1, "$app":"t.b1@1.0.0",
        "$model": {
            "tags": { "$key":"id", "id":"text" },
            "lines": { "$key":"id", "id":"text", "account":"text", "debit":"int" },
            "byacct": { "$view": ".lines { $key: account, account, total: sum(group.debit) }" },
            "$public": { "u": { "$view": ".tags { id } | .byacct { id: account }" } }
        }
    }));
    // b1b: union two plain collections with SAME key type but different relations
    debug_load("b1b", json!({
        "$liasse":1, "$app":"t.b1b@1.0.0",
        "$model": {
            "tags": { "$key":"id", "id":"text" },
            "users": { "$key":"id", "id":"text" },
            "$public": { "u": { "$view": ".tags { id } | .users { id }" } }
        }
    }));
    // b3: intersect two grouped views
    debug_load("b3", json!({
        "$liasse":1, "$app":"t.b3@1.0.0",
        "$model": {
            "lines": { "$key":"id", "id":"text", "account":"text", "side":"text", "debit":"int" },
            "byleft": { "$view": ".lines[:l | l.side == \"L\"] { $key: account, account, total: sum(group.debit) }" },
            "byright": { "$view": ".lines[:r | r.side == \"R\"] { $key: account, account, total: sum(group.debit) }" },
            "$public": { "both": { "$view": ".byleft { account } & .byright { account }" } }
        }
    }));
    // simplest view-over-view: plain passthrough of a root grouped view
    debug_load("vov-plain", json!({
        "$liasse":1, "$app":"t.vov@1.0.0",
        "$model": {
            "lines": { "$key":"id", "id":"text", "account":"text", "debit":"int" },
            "byacct": { "$view": ".lines { $key: account, account, total: sum(group.debit) }" },
            "$public": { "pass": { "$view": ".byacct" } }
        }
    }));
    // root grouped view alone, exposed directly as public
    debug_load("root-grouped-public", json!({
        "$liasse":1, "$app":"t.rg@1.0.0",
        "$model": {
            "lines": { "$key":"id", "id":"text", "account":"text", "debit":"int" },
            "$public": { "byacct": { "$view": ".lines { $key: account, account, total: sum(group.debit) }" } }
        }
    }));
}

fn deep_view_case(name: &str, n: usize) -> String {
    // A root projection `. { d: !(!(...(false)...)) }` nested `n` deep.
    // NB: `$app` package-name components accept only [a-z0-9_], so the app id is a
    // FIXED valid token — never the (hyphenated) case `name`.
    let expr = format!("{}false{}", "!(".repeat(n), ")".repeat(n));
    format!(
        r##"{{
      format: 1
      name: {name}
      suite: scenario
      spec: ["#views", "§7.1", "#expressions", "§6.1"]
      package: {{
        $liasse: 1
        $app: "t.deepcase@1.0.0"
        $model: {{
          items: {{ $key: "id", id: "text" }}
          $public: {{ deep: {{ $view: ". {{ d: {expr} }}" }} }}
        }}
      }}
      steps: [
        {{ watch: "public.deep", id: "w1", expect_init: {{ value: {{ d: false }} }} }}
      ]
    }}"##
    )
}

// ============ EXPLORATORY EXPERIMENTS ============

// EXP VOV: $view over $view passthrough — declared grouped view read by a public
// view with NO field references. §7.1 internal reuse; the runtime folds the
// grouped rows and the passthrough must deliver them unchanged.
#[test]
fn exp_vov_passthrough() {
    let text = r##"{
      format: 1
      name: exp-vov
      suite: scenario
      spec: ["#views", "§7.1", "§7.2"]
      package: {
        $liasse: 1
        $app: "t.expvov@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text", debit: "int" }
          byacct: { $view: ".lines { $key: account, account, total: sum(group.debit) }" }
          $public: { pass: { $view: ".byacct" } }
        }
        $data: { lines: {
          l1: { account: "a", debit: "10" }
          l2: { account: "a", debit: "5" }
          l3: { account: "b", debit: "3" }
        } }
      }
      steps: [
        { watch: "public.pass", id: "w1",
          expect_init: { value: [ { account: "a", total: "15" }, { account: "b", total: "3" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-vov");
}

// EXP INT: intersect two DECLARED grouped views on synthetic key (§7.4).
// byleft groups L-side, byright groups R-side; `&` keeps rows in BOTH by identity,
// with the LEFT projection and order. Only account "a" is in both.
#[test]
fn exp_int_two_grouped() {
    let text = r##"{
      format: 1
      name: exp-int
      suite: scenario
      spec: ["#views", "§7.4"]
      package: {
        $liasse: 1
        $app: "t.expint@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text", side: "text", debit: "int" }
          byleft: { $view: ".lines[:l | l.side == \"L\"] { $key: account, account, total: sum(group.debit) }" }
          byright: { $view: ".lines[:r | r.side == \"R\"] { $key: account, account, total: sum(group.debit) }" }
          $public: { both: { $view: ".byleft & .byright" } }
        }
        $data: { lines: {
          l1: { account: "a", side: "L", debit: "1" }
          l2: { account: "a", side: "R", debit: "2" }
          l3: { account: "b", side: "L", debit: "3" }
        } }
      }
      steps: [
        { watch: "public.both", id: "w1",
          expect_init: { value: [ { account: "a", total: "1" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-int");
}

// EXP UNI: union two DECLARED grouped views (§7.4): left order, then right
// identities not already present. account "a" is in both -> right "a" dropped.
#[test]
fn exp_uni_two_grouped() {
    let text = r##"{
      format: 1
      name: exp-uni
      suite: scenario
      spec: ["#views", "§7.4"]
      package: {
        $liasse: 1
        $app: "t.expuni@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text", side: "text", debit: "int" }
          byleft: { $view: ".lines[:l | l.side == \"L\"] { $key: account, account, total: sum(group.debit) }" }
          byright: { $view: ".lines[:r | r.side == \"R\"] { $key: account, account, total: sum(group.debit) }" }
          $public: { u: { $view: ".byleft | .byright" } }
        }
        $data: { lines: {
          l1: { account: "a", side: "L", debit: "1" }
          l2: { account: "a", side: "R", debit: "2" }
          l3: { account: "c", side: "R", debit: "7" }
          l4: { account: "b", side: "L", debit: "3" }
        } }
      }
      steps: [
        { watch: "public.u", id: "w1",
          expect_init: { value: [
            { account: "a", total: "1" },
            { account: "b", total: "3" },
            { account: "c", total: "7" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-uni");
}

// EXP DIF: difference of two DECLARED grouped views (§7.4): left rows whose
// identity is not in the right, left projection and order. "a" removed.
#[test]
fn exp_dif_two_grouped() {
    let text = r##"{
      format: 1
      name: exp-dif
      suite: scenario
      spec: ["#views", "§7.4"]
      package: {
        $liasse: 1
        $app: "t.expdif@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text", side: "text", debit: "int" }
          byleft: { $view: ".lines[:l | l.side == \"L\"] { $key: account, account, total: sum(group.debit) }" }
          byright: { $view: ".lines[:r | r.side == \"R\"] { $key: account, account, total: sum(group.debit) }" }
          $public: { d: { $view: ".byleft - .byright" } }
        }
        $data: { lines: {
          l1: { account: "a", side: "L", debit: "1" }
          l2: { account: "a", side: "R", debit: "2" }
          l3: { account: "b", side: "L", debit: "3" }
        } }
      }
      steps: [
        { watch: "public.d", id: "w1",
          expect_init: { value: [ { account: "b", total: "3" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-dif");
}

// ── FINDING F1 (HIGH) — FIXED ─────────────────────────────────────────────────
// §6.1/§2.4 + Annex C + AGENTS.md "Code must never panic": a parseable-but-deep
// expression MUST NOT abort the process. Before the fix, the nesting cap
// (`liasse-syntax/src/scan.rs::MAX_NESTING_DEPTH`) was 512, calibrated only
// against `pest`'s parse recursion, while the recursive load pipeline (pest parse
// + the liasse-expr checker `check`/`check_unary`, and the evaluator `eval_not`)
// walks the same AST depth with fatter frames and overflowed the stack far below
// 512 (SIGABRT: ~70 on a 2 MiB libtest thread, ~300–400 on an 8 MiB main thread).
// A depth-500 expression therefore ADMITTED past the cap and aborted during load.
//
// The fix lowers the cap to a single effective bound (32) that the pest scan AND
// the recursive check/eval all respect — measured ~2× under the smallest overflow
// while clearing the deepest real corpus document (12 brackets) by a wide margin.
// A too-deep expression is now a CLEAN load rejection here (before any recursion),
// never a downstream abort — which is exactly what this test now asserts.
#[test]
fn exp_deep_under_cap() {
    // Depth 500 used to SIGABRT during load; it must now be a clean `invalid`
    // rejection whose diagnostic names the nesting-depth cause, not a crash.
    let expr = format!("{}false{}", "!(".repeat(500), ")".repeat(500));
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.deepunder@1.0.0",
        "$model": {
            "items": { "$key": "id", "id": "text" },
            "$public": { "deep": { "$view": format!(". {{ d: {expr} }}") } }
        }
    });
    match load_diagnostics("deep-under", &package) {
        Ok(()) => panic!("exp-deep-under: a depth-500 expression must be rejected, but it loaded"),
        Err(msgs) => assert!(
            msgs.iter().any(|m| m.contains("nests brackets more than")),
            "exp-deep-under: expected a clean nesting-depth rejection (no crash), got {msgs:?}"
        ),
    }
}

// CONTROL F1b: the SAME `!(...)`-nested projection at a SHALLOW depth (n=8) loads
// and evaluates cleanly to `{ d: false }` (8 `!` of `false` = false). Isolates the
// defect to DEPTH — the construct itself is valid and correctly evaluated, and 8
// is comfortably under the effective cap.
#[test]
fn control_deep_shallow_ok() {
    let text = deep_view_case("ctl-deep-shallow", 8);
    assert_all_pass(&run_case_text(&text), "ctl-deep-shallow");
}

// CONTROL F1a: n=1000 far EXCEEDS the cap -> the nesting scan rejects it BEFORE
// pest runs, so the load fails as a clean static rejection (never a panic/abort),
// and the diagnostic names the nesting-depth cause specifically.
#[test]
fn exp_deep_over_cap() {
    let expr = format!("{}false{}", "!(".repeat(1000), ")".repeat(1000));
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.deepover@1.0.0",
        "$model": {
            "items": { "$key": "id", "id": "text" },
            "$public": { "deep": { "$view": format!(". {{ d: {expr} }}") } }
        }
    });
    match load_diagnostics("deep-over", &package) {
        Ok(()) => panic!("exp-deep-over: over-cap nesting must be rejected, but it loaded"),
        Err(msgs) => assert!(
            msgs.iter().any(|m| m.contains("nests brackets more than")),
            "exp-deep-over: expected a nesting-depth rejection, got {msgs:?}"
        ),
    }
}

// EXP C-SUM: sum over a field absent for some rows mixed with present (§7.5 skip).
#[test]
fn exp_c_sum_mixed_absent() {
    let text = r##"{
      format: 1
      name: exp-csum
      suite: scenario
      spec: ["#views", "§7.5"]
      package: {
        $liasse: 1
        $app: "t.expcsum@1.0.0"
        $model: {
          items: { $key: "id", id: "text", amount: "int?" }
          s: "= sum(.items.amount)"
          mx: "= max(.items.amount)"
          av: "= avg(.items.amount)"
          $public: { stat: { $view: ". { s, mx, av }" } }
        }
        $data: { items: {
          a: { amount: "5" }
          b: {}
          c: { amount: "3" }
        } }
      }
      steps: [
        { watch: "public.stat", id: "w1",
          expect_init: { value: { s: "8", mx: "5", av: "4" } } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-csum");
}

// EXP C1: sort a grouped view by an aggregate min over a field entirely absent in a group.
#[test]
fn exp_c1_sort_by_min_over_absent_group() {
    let text = r##"{
      format: 1
      name: exp-c1
      suite: scenario
      spec: ["#views", "§7.5", "§7.3"]
      package: {
        $liasse: 1
        $app: "t.expc1@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text", amount: "int?" }
          $public: { byacct: { $view: ".lines { $key: account, account, lo: min(group.amount), $sort: [lo, account] }" } }
        }
        $data: { lines: {
          l1: { account: "a", amount: "5" }
          l2: { account: "b" }
          l3: { account: "c", amount: "2" }
        } }
      }
      steps: [
        { watch: "public.byacct", id: "w1",
          expect_init: { value: [
            { account: "c", lo: "2" },
            { account: "a", lo: "5" },
            { account: "b", lo: "$absent" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-c1");
}

// EXP D1: grouped $key component evaluates to none for some rows (optional key field).
#[test]
fn exp_d1_grouped_key_none() {
    let text = r##"{
      format: 1
      name: exp-d1
      suite: scenario
      spec: ["#views", "§7.2", "§7.3", "§A.1"]
      package: {
        $liasse: 1
        $app: "t.expd1@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", account: "text?", debit: "int" }
          $public: { byacct: { $view: ".lines { $key: account, account, total: sum(group.debit) }" } }
        }
        $data: { lines: {
          l1: { account: "a", debit: "10" }
          l2: { debit: "5" }
          l3: { account: "a", debit: "1" }
        } }
      }
      steps: [
        { watch: "public.byacct", id: "w1",
          expect_init: { value: [
            { account: "a", total: "11" },
            { account: "$absent", total: "5" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-d1");
}

// EXP D2: composite grouped $key where one component evaluates to none.
#[test]
fn exp_d2_composite_key_none_component() {
    let text = r##"{
      format: 1
      name: exp-d2
      suite: scenario
      spec: ["#views", "§7.2", "§B.4"]
      package: {
        $liasse: 1
        $app: "t.expd2@1.0.0"
        $model: {
          lines: { $key: "id", id: "text", region: "text", account: "text?", debit: "int" }
          $public: { totals: { $view: ".lines { $key: [region, account], region, account, total: sum(group.debit) }" } }
        }
        $data: { lines: {
          l1: { region: "eu", account: "a", debit: "1" }
          l2: { region: "eu", debit: "2" }
          l3: { region: "eu", account: "a", debit: "3" }
        } }
      }
      steps: [
        { watch: "public.totals", id: "w1",
          expect_init: { value: [
            { region: "eu", account: "a", total: "4" },
            { region: "eu", account: "$absent", total: "2" }
          ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "exp-d2");
}

// ── FINDING F2 (MED) ──────────────────────────────────────────────────────────
// §7.4 "Operands share row shape and identity domain." + §6.3 "Values belonging
// to different target relations are statically incomparable." A `|`/`&`/`-`
// combinator between views over DIFFERENT relations that merely share a scalar key
// TYPE (both `text`) MUST be statically rejected — this is exactly the corpus red
// case `07-views/red/combinator-mismatched-identity-domain-invalid` (`.tasks |
// .users`, outcome: invalid). The implementation ACCEPTS it: the checker's
// identity-domain test (`check_combination`, check/ops.rs:412) compares only
// `RowType::key()` (the scalar key TYPE), never the relation, so any two text-keyed
// collections pass. (This escapes the corpus gate because static `invalid` cases
// are only parsed, never run against the loader — corpus_loads.rs.)
#[test]
fn finding_cross_domain_combinator_must_reject() {
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.xdomrej@1.0.0",
        "$model": {
            "tasks": { "$key": "id", "id": "text" },
            "users": { "$key": "id", "id": "text" },
            "$public": { "mixed": { "$view": ".tasks | .users" } }
        }
    });
    // §7.4/§6.3: distinct target relations do not share an identity domain -> the
    // union is statically invalid. Expect a load rejection.
    assert!(
        load_diagnostics("xdom-reject", &package).is_err(),
        "F2: `.tasks | .users` (distinct relations) must be statically rejected (§7.4/§6.3), but it loaded"
    );
}

// CONTROL F2a: a combinator whose operands DO share an identity domain — two views
// over the SAME `tasks` relation — loads cleanly. Isolates the defect to the
// cross-relation case, not to combinators in general.
#[test]
fn control_same_domain_combinator_loads() {
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.samedom@1.0.0",
        "$model": {
            "tasks": { "$key": "id", "id": "text", "grp": "text" },
            "$public": { "u": { "$view": ".tasks { id } | .tasks[:t | t.grp == \"a\"] { id }" } }
        }
    });
    assert!(
        load_diagnostics("same-domain", &package).is_ok(),
        "F2a: a same-relation combinator must load (§7.4 shared identity domain)"
    );
}

// FINDING F2b (eval-side consequence, now FORECLOSED at load): before the fix,
// admitting the cross-domain union let `eval_combine` (eval/views.rs) identify
// rows by key-text `RowId` alone, so a `tasks` row and a `users` row sharing a key
// STRING collapsed into one — a §6.3 cross-relation identity confusion that
// silently DROPPED a distinct row. The checker now REJECTS `.tasks | .users` at
// LOAD (§7.4/§6.3: distinct target relations do not share an identity domain), so
// the eval path can never see cross-relation operands and the merge cannot occur.
// This test now pins that: the confusion-prone package fails static validation
// rather than producing a (mis)merged view.
#[test]
fn finding_cross_domain_union_merges_distinct_rows() {
    let package = serde_json::json!({
        "$liasse": 1, "$app": "t.xdommerge@1.0.0",
        "$model": {
            "tasks": { "$key": "id", "id": "text" },
            "users": { "$key": "id", "id": "text" },
            "$public": { "merged": { "$view": ".tasks | .users" } }
        }
    });
    // The load rejection is what forecloses the eval-side cross-relation merge.
    assert!(
        load_diagnostics("xdom-merge", &package).is_err(),
        "F2b: `.tasks | .users` must be rejected at load so eval never merges cross-relation rows"
    );
}

// CONTROL F2c: a union WITHIN one relation deduplicates by shared identity exactly
// as §7.4 requires (the same row appearing on both sides collapses to one),
// confirming eval_combine's identity handling is correct for same-domain operands.
#[test]
fn control_same_domain_union_dedups() {
    let text = r##"{
      format: 1
      name: samedom-union
      suite: scenario
      spec: ["#views", "§7.4"]
      package: {
        $liasse: 1
        $app: "t.samedomu@1.0.0"
        $model: {
          tasks: { $key: "id", id: "text", grp: "text" }
          $public: { u: { $view: ".tasks[:a | a.grp == \"x\"] { id } | .tasks[:b | b.grp == \"x\"] { id }" } }
        }
        $data: { tasks: { t1: { grp: "x" }, t2: { grp: "x" }, t3: { grp: "y" } } }
      }
      steps: [
        // Both operands select the same two grp==x rows; §7.4 union keeps each
        // identity once -> [t1, t2] (canonical key order), t3 excluded.
        { watch: "public.u", id: "w1",
          expect_init: { value: [ { id: "t1" }, { id: "t2" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "samedom-union");
}
