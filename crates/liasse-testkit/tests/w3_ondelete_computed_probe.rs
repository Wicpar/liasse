//! RED-TEAM (WAVE 3) — `$on_delete` patch (§21.1) × computed values (§5.2):
//! the `$on_delete: "= { … }"` patch path is BLIND to computed values.
//!
//! §21.1: "Once complete, deletion is planned from the prospective pre-delete
//! state … A patch expression binds `.` to the referencing row as it existed at
//! planning time and `$target` to the target row being removed."
//! §5.2: a computed value "participates in views, checks, sorting, and
//! projections like any other value."
//!
//! The referencing row and the target row each carry their §5.2 computed values,
//! so a patch reading `.someComputed` or `$target.someComputed` MUST observe the
//! computed value like any stored field. It does not:
//! `cascade.rs::resolve_policy` builds both the `.` cell and the `$target` cell
//! through `row_cell_at`, which returns the BARE `eval::row_cell` — the cell that
//! folds only `collection.fields` and `collection.structs`, never
//! `collection.computed`. (Contrast `eval::row_cell_of` / `materialize_row_cell`,
//! the read/`return` path, which DO fold computed values.) So in the patch scope a
//! computed member is absent: the patch either commits the WRONG value (the
//! computed reads as `none`) or the whole delete is spuriously REJECTED (reading
//! the absent member faults). Root cause: `crates/liasse-runtime/src/cascade.rs`
//! `row_cell_at` (~L242) → `eval::row_cell` (~L893).
//!
//! Each FINDING fails against the current implementation; its paired CONTROL —
//! the identical patch reading the underlying STORED field with the computed's
//! own expression inlined — PASSES, isolating the defect to computed-invisibility
//! in the patch scope (and NOT to string concat, `$target` binding, or the
//! delete itself). Expectations are hand-derived from SPEC.md, never observed.
//!
//! The file also carries three CONTROLS for the other cross-feature interactions
//! in the charge that the runtime handles CORRECTLY (they PASS): a `restrict`
//! ref whose referencing row is itself cascade-deleted does not block (§21.1),
//! and a `$set`-of-`$ref` member `cascade` drops the member while the containing
//! row's `= count(.set)` computed re-derives on read (§5.6/§5.2).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
#![allow(clippy::doc_lazy_continuation)]

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
    assert!(ok, "{name}: a step diverged from its SPEC-derived expectation");
}

// ── FINDING 1 ────────────────────────────────────────────────────────────────
// §21.1 + §5.2: an `$on_delete` patch reads a COMPUTED field of the referencing
// row (`.tag`, `= .label + '!'`). The patch MUST observe it like any stored
// field, so deleting the project patches the surviving task's `archived` to the
// task's computed `tag` value ("Apollo!"). Instead the computed is invisible in
// the patch scope.
#[test]
fn on_delete_patch_reads_dot_computed_must_see_it() {
    let text = r##"{
      format: 1
      name: on-delete-patch-reads-dot-computed
      suite: scenario
      spec: ["#deletion", "§21.1", "#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.odc.dot@1.0.0"
        $model: {
          projects: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            label: "text"
            tag: "= .label + '!'"
            archived: "text?"
            project: {
              $ref: "/projects"
              $optional: true
              $on_delete: "= { project: none, archived: .tag }"
            }
          }
          $mut: { del: ".projects - @id" }
          tasks_view: { $view: ".tasks { id, label, tag, archived, project, $sort: [id] }" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            tasks: { $view: ".tasks_view" }
          }
        }
        $data: { projects: { p1: {} }, tasks: { t1: { label: "Apollo", project: "p1" } } }
      }
      steps: [
        // §5.2 works in a plain view: the computed `tag` is visible pre-delete.
        { watch: "public.tasks", id: "w1", expect_init: { value: [
          { id: "t1", label: "Apollo", tag: "Apollo!", archived: "$absent", project: "p1" }
        ] } }
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        // §21.1: `.` is the referencing task; `.tag` = "Apollo!" -> archived = "Apollo!".
        { expect_view: { watch: "w1", value: [
          { id: "t1", label: "Apollo", tag: "Apollo!", archived: "Apollo!", project: "$absent" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "on-delete-patch-reads-dot-computed");
}

// CONTROL 1: identical patch, but reads the STORED `.label` and inlines the
// computed's own expression (`.label + '!'`). This PASSES — proving the delete,
// the patch machinery, and string concat all work; only reading the *computed*
// member `.tag` fails.
#[test]
fn control_on_delete_patch_reads_stored_field() {
    let text = r##"{
      format: 1
      name: control-on-delete-patch-reads-stored
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.odc.ctldot@1.0.0"
        $model: {
          projects: { $key: "id", id: "text" }
          tasks: {
            $key: "id"
            id: "text"
            label: "text"
            archived: "text?"
            project: {
              $ref: "/projects"
              $optional: true
              $on_delete: "= { project: none, archived: .label + '!' }"
            }
          }
          $mut: { del: ".projects - @id" }
          tasks_view: { $view: ".tasks { id, label, archived, project, $sort: [id] }" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            tasks: { $view: ".tasks_view" }
          }
        }
        $data: { projects: { p1: {} }, tasks: { t1: { label: "Apollo", project: "p1" } } }
      }
      steps: [
        { watch: "public.tasks", id: "w1", expect_init: { value: [
          { id: "t1", label: "Apollo", archived: "$absent", project: "p1" }
        ] } }
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "w1", value: [
          { id: "t1", label: "Apollo", archived: "Apollo!", project: "$absent" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-on-delete-patch-reads-stored");
}

// ── FINDING 2 ────────────────────────────────────────────────────────────────
// §21.1 + §5.2: an `$on_delete` patch reads a COMPUTED field of `$target` (the
// project's `badge`, `= .name + '*'`). §21.1 binds `$target` to the target row
// being removed; §5.2 makes its computed values participate like any field, so
// `$target.badge` = "Apollo*" -> archived = "Apollo*". Instead invisible.
#[test]
fn on_delete_patch_reads_target_computed_must_see_it() {
    let text = r##"{
      format: 1
      name: on-delete-patch-reads-target-computed
      suite: scenario
      spec: ["#deletion", "§21.1", "#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.odc.tgt@1.0.0"
        $model: {
          projects: {
            $key: "id"
            id: "text"
            name: "text"
            badge: "= .name + '*'"
          }
          tasks: {
            $key: "id"
            id: "text"
            archived: "text?"
            project: {
              $ref: "/projects"
              $optional: true
              $on_delete: "= { project: none, archived: $target.badge }"
            }
          }
          $mut: { del: ".projects - @id" }
          tasks_view: { $view: ".tasks { id, archived, project, $sort: [id] }" }
          $public: {
            projects: { $view: ".projects { id, badge }", $mut: { delete: ".del" } }
            tasks: { $view: ".tasks_view" }
          }
        }
        $data: { projects: { p1: { name: "Apollo" } }, tasks: { t1: { project: "p1" } } }
      }
      steps: [
        // §5.2 works in a plain view: $target's computed `badge` is visible pre-delete.
        { watch: "public.projects", id: "wp", expect_init: { value: [ { id: "p1", badge: "Apollo*" } ] } }
        { watch: "public.tasks", id: "w1", expect_init: { value: [
          { id: "t1", archived: "$absent", project: "p1" }
        ] } }
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        // §21.1: $target is the removed project; $target.badge = "Apollo*".
        { expect_view: { watch: "w1", value: [
          { id: "t1", archived: "Apollo*", project: "$absent" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "on-delete-patch-reads-target-computed");
}

// CONTROL 2: identical patch, but reads the STORED `$target.name` and inlines the
// computed's own expression (`$target.name + '*'`). PASSES — the existing corpus
// case `on-delete-patch-reads-target-row` already relies on `$target.<stored>`
// working, so this isolates the defect to the *computed* member `$target.badge`.
#[test]
fn control_on_delete_patch_reads_target_stored() {
    let text = r##"{
      format: 1
      name: control-on-delete-patch-reads-target-stored
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.odc.ctltgt@1.0.0"
        $model: {
          projects: { $key: "id", id: "text", name: "text" }
          tasks: {
            $key: "id"
            id: "text"
            archived: "text?"
            project: {
              $ref: "/projects"
              $optional: true
              $on_delete: "= { project: none, archived: $target.name + '*' }"
            }
          }
          $mut: { del: ".projects - @id" }
          tasks_view: { $view: ".tasks { id, archived, project, $sort: [id] }" }
          $public: {
            projects: { $view: ".projects { id }", $mut: { delete: ".del" } }
            tasks: { $view: ".tasks_view" }
          }
        }
        $data: { projects: { p1: { name: "Apollo" } }, tasks: { t1: { project: "p1" } } }
      }
      steps: [
        { watch: "public.tasks", id: "w1", expect_init: { value: [
          { id: "t1", archived: "$absent", project: "p1" }
        ] } }
        { call: "public.projects.delete", args: { id: "p1" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "w1", value: [
          { id: "t1", archived: "Apollo*", project: "$absent" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-on-delete-patch-reads-target-stored");
}

// ── CONTROL / HELD 3 ──────────────────────────────────────────────────────────
// §21.1: a `restrict` ref does NOT block when its referencing row is itself
// cascade-deleted in the same transition. Link `l1` refers to org `o1` via a
// `cascade` ref AND a `restrict` ref; deleting `o1` cascades `l1` into the delete
// set, so its restrict no longer blocks. Both removed. This HOLDS (passes).
#[test]
fn control_restrict_does_not_block_when_referencing_row_cascade_deleted() {
    let text = r##"{
      format: 1
      name: control-restrict-cascaded-referencer
      suite: scenario
      spec: ["#deletion", "§21.1"]
      package: {
        $liasse: 1
        $app: "t.odc.rc@1.0.0"
        $model: {
          orgs: { $key: "id", id: "text" }
          links: {
            $key: "id"
            id: "text"
            home: { $ref: "/orgs", $on_delete: "cascade" }
            guard: { $ref: "/orgs", $on_delete: "restrict" }
          }
          $mut: { del: ".orgs - @id" }
          orgs_view: { $view: ".orgs { id, $sort: [id] }" }
          links_view: { $view: ".links { id, $sort: [id] }" }
          $public: {
            orgs: { $view: ".orgs_view", $mut: { delete: ".del" } }
            links: { $view: ".links_view" }
          }
        }
        $data: { orgs: { o1: {} }, links: { l1: { home: "o1", guard: "o1" } } }
      }
      steps: [
        { watch: "public.orgs", id: "wo", expect_init: { value: [ { id: "o1" } ] } }
        { watch: "public.links", id: "wl", expect_init: { value: [ { id: "l1" } ] } }
        { call: "public.orgs.delete", args: { id: "o1" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "wo", value: [] } }
        { expect_view: { watch: "wl", value: [] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-restrict-cascaded-referencer");
}

// ── CONTROL / HELD 4 ──────────────────────────────────────────────────────────
// §5.6/§21.1 + §5.2: a `$set`-of-`$ref` member `cascade` drops the deleted
// target's membership from the surviving row (NOT the whole row), and the
// containing row's `= size(.refs)` computed re-derives on read. Deleting tag
// `t1` drops it from `d1.refs`, leaving `{t2}` and `n = 1`. This HOLDS (passes).
// (`size` reads a set (§6.5); `count` is view-only (§7.5), so it is used here.)
#[test]
fn control_set_member_cascade_drops_member_and_computed_rederives() {
    let text = r##"{
      format: 1
      name: control-set-member-cascade-computed
      suite: scenario
      spec: ["#deletion", "§21.1", "#refs", "§5.6", "#state-model", "§5.2"]
      package: {
        $liasse: 1
        $app: "t.odc.setc@1.0.0"
        $model: {
          tags: { $key: "id", id: "text" }
          docs: {
            $key: "id"
            id: "text"
            refs: { $set: { $ref: "/tags", $on_delete: "cascade" } }
            n: "= size(.refs)"
          }
          $mut: { del: ".tags - @id" }
          tags_view: { $view: ".tags { id, $sort: [id] }" }
          docs_view: { $view: ".docs { id, refs, n, $sort: [id] }" }
          $public: {
            tags: { $view: ".tags_view", $mut: { delete: ".del" } }
            docs: { $view: ".docs_view" }
          }
        }
        $data: { tags: { t1: {}, t2: {} }, docs: { d1: { refs: ["t1", "t2"] } } }
      }
      steps: [
        { watch: "public.docs", id: "wd", expect_init: { value: [
          { id: "d1", refs: { $unordered: ["t1", "t2"] }, n: "2" }
        ] } }
        { call: "public.tags.delete", args: { id: "t1" }, expect: { outcome: ok, "...": true } }
        // member t1 dropped from d1.refs; computed n re-derives to 1.
        { expect_view: { watch: "wd", value: [
          { id: "d1", refs: ["t2"], n: "1" }
        ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "control-set-member-cascade-computed");
}
