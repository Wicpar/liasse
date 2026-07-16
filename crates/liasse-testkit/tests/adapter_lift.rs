//! The scenario adapter's §10.1/§10.2 inline-surface wiring, driven against the
//! real runtime + surface stack over an in-memory store.
//!
//! A surface may carry an expression directly on its `$view`/`$mut` rather than a
//! bare reference to a declared view or mutation (§10.1: "a value containing a
//! mutation expression or array defines an inline program for that surface";
//! §10.2 for a surface view). The model validates such a member structurally but
//! retains no runnable declaration, and the surface router binds only *named*
//! runtime views and mutations. The adapter reconstructs what a production host
//! wires by hand: it lifts each inline member into a synthetic top-level view or
//! root mutation and binds the surface to it. These tests lock that in.
//!
//! Every expected outcome is deducible from SPEC.md alone, not from observing the
//! engine:
//!
//! - §7.1: a pass-through view (no projection block) exposes every source field;
//!   a projection block limits the visible fields to those listed.
//! - §10.1/§8.4: an inline `$mut` insertion program commits and the inserted row
//!   becomes visible through the surface view.
//! - §10.1: an inline `$mut` the model cannot compile leaves that call unbound
//!   (denied) without breaking the rest of the surface — the adapter degrades
//!   rather than failing the whole load.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// Build and run a scenario case from its Hjson text against the real adapter.
fn run(text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new("<adapter-lift>"), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("adapter-lift"), SuiteKind::Common, &case)
}

/// Assert step `index` ran, its expectation held, and it observed `expected`, so
/// a wrong outcome and a skipped step (a load/transport fault) both fail loudly.
fn assert_step(result: &CaseResult, index: usize, expected: Outcome) {
    let step = result.steps.get(index).unwrap_or_else(|| panic!("no step {index}: {:?}", result.steps));
    assert!(step.result.is_pass(), "step {index} did not pass: {:?}", result.steps);
    assert_eq!(step.observed, Some(expected), "step {index} observed the wrong outcome");
}

#[test]
fn inline_pass_through_view_exposes_every_source_field() {
    // §7.1: `$view: ".projects"` selects its source with no projection, so both
    // id and name are visible, in key order (§B.5).
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-passthrough
          suite: scenario
          spec: ["#views"]
          package: {
            $liasse: 1
            $app: "t.lift_view@1.0.0"
            $model: {
              projects: { $key: "id", id: "text", name: "text" }
              $public: { index: { $view: ".projects" } }
            }
            $data: { projects: { p1: { name: "Alpha" }, p2: { name: "Beta" } } }
          }
          steps: [
            { watch: "public.index", id: "w1",
              expect_init: { value: [ { id: "p1", name: "Alpha" }, { id: "p2", name: "Beta" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn inline_projection_view_limits_visible_fields() {
    // §7.1: a projection block shapes the result; only listed fields are outputs,
    // so `name` must be absent (FORMAT.md: expected objects match exactly).
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-projection
          suite: scenario
          spec: ["#views"]
          package: {
            $liasse: 1
            $app: "t.lift_proj@1.0.0"
            $model: {
              projects: { $key: "id", id: "text", name: "text" }
              $public: { ids: { $view: ".projects { id }" } }
            }
            $data: { projects: { p1: { name: "Alpha" } } }
          }
          steps: [
            { watch: "public.ids", id: "w1", expect_init: { value: [ { id: "p1" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn inline_mutation_program_commits_and_is_visible() {
    // §10.1/§8.4: an inline `$mut` insertion program admits, commits the
    // constructed row, and the row becomes visible through the surface view.
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-inline-mut
          suite: scenario
          spec: ["#interfaces", "#mutations"]
          package: {
            $liasse: 1
            $app: "t.lift_mut@1.0.0"
            $model: {
              things: { $key: "id", id: "text", label: "text" }
              $public: {
                things: {
                  $view: ".things { id, label }"
                  $mut: { add: ".things + { id: @id, label: @label }" }
                }
              }
            }
          }
          steps: [
            { call: "public.things.add", args: { id: "t1", label: "x" }, expect: { outcome: ok } }
            { watch: "public.things", id: "w1", expect_init: { value: [ { id: "t1", label: "x" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn inline_mutation_array_program_commits() {
    // §10.1: an array `$mut` value is an inline atomic program; it lifts to a
    // synthetic root mutation and commits, returning its projected row (§8.4).
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-inline-array
          suite: scenario
          spec: ["#interfaces", "#mutations"]
          package: {
            $liasse: 1
            $app: "t.lift_arr@1.0.0"
            $model: {
              things: { $key: "id", id: "text", label: "text" }
              $public: {
                things: {
                  $view: ".things { id, label }"
                  $mut: { add: [ "t = .things + { id: @id, label: @label }", "return t { id, label }" ] }
                }
              }
            }
          }
          steps: [
            { call: "public.things.add", args: { id: "t1", label: "x" },
              expect: { outcome: ok, value: { id: "t1", label: "x" } } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn row_mutation_reference_binds_selector_param_and_key_type() {
    // §10.1: a declared row-mutation reference `.tasks[@id].complete()` combines
    // the selector parameter (`@id`, named `id` on the surface) with the
    // mutation's parameters. §6.3: the receiver is selected by key — here a
    // `uuid` key, so the surface argument must decode as a uuid to match the
    // generated row, not as a bare string. A working commit proves both: the
    // selector arg reached the receiver and matched the uuid-keyed row.
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-receiver
          suite: scenario
          spec: ["#interfaces", "#expressions"]
          package: {
            $liasse: 1
            $app: "t.lift_recv@1.0.0"
            $model: {
              tasks: {
                $key: "id"
                id: "uuid = uuid()"
                title: "text"
                done: "bool = false"
                $mut: { finish: [ ".done = true", "return . { id, title, done }" ] }
              }
              $mut: { add_task: [ "t = .tasks + { title: @title }", "return t { id }" ] }
              $public: {
                tasks: {
                  $view: ".tasks { id, title, done }"
                  $mut: {
                    add: ".add_task"
                    complete: ".tasks[@id].finish()"
                  }
                }
              }
            }
          }
          steps: [
            { call: "public.tasks.add", args: { title: "one" },
              expect: { outcome: ok, value: { id: "$bind:t" } } }
            { call: "public.tasks.complete", args: { id: "$ref:t" },
              expect: { outcome: ok, value: { id: "$ref:t", title: "one", done: true } } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
    assert_step(&result, 1, Outcome::Ok);
}

#[test]
fn inline_view_sort_descending_combinator_lifts_and_orders() {
    // §7.3: "A leading - reverses one key." A `$sort: [-n]` projection combinator
    // is a structural directive, not a request-scoped `$name`, so the inline view
    // lifts and returns rows in descending n (§B.1 int order): 30, 20, 10.
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-sort-desc
          suite: scenario
          spec: ["#views"]
          package: {
            $liasse: 1
            $app: "t.lift_sort@1.0.0"
            $model: {
              items: { $key: "id", id: "text", n: "int" }
              $public: { byn: { $view: ".items { id, n, $sort: [-n] }" } }
            }
            $data: { items: { a: { n: "10" }, b: { n: "30" }, c: { n: "20" } } }
          }
          steps: [
            { watch: "public.byn", id: "w1",
              expect_init: { value: [ { id: "b", n: "30" }, { id: "c", n: "20" }, { id: "a", n: "10" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn inline_view_skip_and_limit_combinators_lift_and_window() {
    // §7.3: bounds apply after sorting; `$skip` drops the first rows, `$limit`
    // keeps at most the next. Sorted ascending 10,20,30,40; skip 1, limit 2 -> 20,30.
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-window
          suite: scenario
          spec: ["#views"]
          package: {
            $liasse: 1
            $app: "t.lift_window@1.0.0"
            $model: {
              items: { $key: "id", id: "text", n: "int" }
              $public: { page: { $view: ".items { id, n, $sort: [n], $skip: 1, $limit: 2 }" } }
            }
            $data: { items: { a: { n: "10" }, b: { n: "20" }, c: { n: "30" }, d: { n: "40" } } }
          }
          steps: [
            { watch: "public.page", id: "w1",
              expect_init: { value: [ { id: "b", n: "20" }, { id: "c", n: "30" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn inline_view_synthetic_key_combinator_lifts_and_groups() {
    // §7.2: a projection MAY declare a synthetic `$key`; rows sharing it form one
    // group and a non-key output aggregates over `group`. `$key` is a structural
    // directive, not a request variable, so the inline view lifts (§7.5 sum).
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-synthkey
          suite: scenario
          spec: ["#views"]
          package: {
            $liasse: 1
            $app: "t.lift_synthkey@1.0.0"
            $model: {
              lines: { $key: "id", id: "text", account: "text", debit: "int" }
              $public: { totals: { $view: ".lines { $key: account, account, total: sum(group.debit) }" } }
            }
            $data: { lines: {
              l1: { account: "a", debit: "10" }
              l2: { account: "a", debit: "5" }
              l3: { account: "b", debit: "3" }
            } }
          }
          steps: [
            { watch: "public.totals", id: "w1",
              expect_init: { value: [ { account: "a", total: "15" }, { account: "b", total: "3" } ] } }
          ]
        }"##,
    );
    assert_step(&result, 0, Outcome::Ok);
}

#[test]
fn request_scope_view_is_not_lifted_but_combinator_view_is() {
    // The lift guard (§10.2, SPEC-ISSUES item 10) separates a plain view's own
    // evaluation from the request scope. A `$view` reading a request-scoped
    // variable (`$actor`) has no binding a top-level named view can supply, so it
    // must stay unlifted (surface unbound -> its watch resolves `denied`, never
    // fabricated); a projection combinator (`$sort`) is a structural directive,
    // not a request variable, so its view lifts. This is analyzed directly on the
    // package JSON, independent of whether the package would load.
    let package = serde_json::json!({
        "$model": {
            "items": { "$key": "id", "id": "text", "owner": "text" },
            "$public": {
                "mine": { "$view": ".items[:i | i.owner == $actor] { id }" },
                "byn":  { "$view": ".items { id, $sort: [owner] }" }
            }
        }
    });
    let lift = liasse_testkit::adapter::lift::SurfaceLift::derive(&package);
    assert!(lift.view_name("public.mine").is_none(), "a $actor view must not lift");
    assert!(lift.view_name("public.byn").is_some(), "a $sort combinator view must lift");
}

#[test]
fn uncompilable_inline_mut_degrades_without_breaking_the_surface() {
    // The adapter tries the richest wiring first, then falls back: an inline
    // `$mut` the model cannot compile (a parameter used only in `return`, which
    // §8.3 leaves uninferrable) is dropped so the package still loads. That call
    // then resolves `denied` (unbound), while a sibling inline `$view` on the
    // same package keeps working — degradation, never a whole-package break.
    let result = run(
        r##"{
          format: 1
          name: adapter-lift-degrade
          suite: scenario
          spec: ["#interfaces"]
          package: {
            $liasse: 1
            $app: "t.lift_degrade@1.0.0"
            $model: {
              things: { $key: "id", id: "text", label: "text" }
              $public: {
                items: {
                  $view: ".things { id, label }"
                  $mut: { echo: "return @value" }
                }
              }
            }
          }
          steps: [
            { watch: "public.items", id: "w1", expect_init: { value: [] } }
            { call: "public.items.echo", args: { value: "x" },
              expect: { outcome: denied, violates: ["#interfaces"] } }
          ]
        }"##,
    );
    // The view still works (views-only fallback kept it bound)...
    assert_step(&result, 0, Outcome::Ok);
    // ...and the uncompilable inline mutation is unbound, so the call is denied.
    assert_step(&result, 1, Outcome::Denied);
}
