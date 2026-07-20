//! RED-TEAM bug reproduction (SPEC §12.1 closed argument objects, `view` path).
//!
//! §12.1 pins the argument object of an external request as a CLOSED shape, and
//! says so for BOTH request kinds explicitly:
//!
//! > An argument object presented to a `call` or `view` request is closed: it
//! > MUST contain only names that are declared parameters of the targeted
//! > mutation or view. A member whose name is not a declared parameter —
//! > including any reserved `$`-prefixed name — makes the request malformed; the
//! > runtime rejects it during parameter parsing (step 3), before admission, with
//! > no partial effect. There is no width subtyping over external argument
//! > objects, and an undeclared member is never silently dropped.
//!
//! The `call` path honours this: [`SurfaceHost::call`] runs `build_request`,
//! which rejects an undeclared (or `$`-prefixed) member as `malformed`
//! (`crates/liasse-surface/src/host/call.rs`, `build_request`). The corpus case
//! `tests/12-clients-live-views/red/unknown-parameter-member.hjson` pins the
//! `call` half.
//!
//! The `view`/`watch` path does NOT. [`SurfaceHost::watch`]
//! (`crates/liasse-surface/src/host/call.rs:484`) forwards `watch.args()` straight
//! into `view_query(..)` and `open_subscription` → `engine.view_with(..)`
//! (`call.rs:494`, `:561-611`) with no closed-shape check against the view's
//! declared `$params`. An undeclared member is therefore never read by the
//! declared `$view` and is SILENTLY DROPPED — the subscription opens `ok` instead
//! of being rejected `malformed`. (`SurfaceHost::resume`, `call.rs:515`, has the
//! identical gap.)
//!
//! This is a §12.1 MUST violation on the `view` request: "an undeclared member is
//! never silently dropped" and the reserved-`$`-prefixed member is called out by
//! name. Every expectation here is deducible from §12.1 text alone; it mirrors the
//! `call`-path corpus case one-for-one on the sibling request kind.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, Outcome, ScenarioAdapter, SuiteKind};

/// One `text`-keyed collection behind two PUBLIC surfaces: a `tasks` surface with
/// an `add` mutation (its arg object is closed today), and a `search` surface
/// whose `$view` declares exactly one parameter `@q`. A `watch` on `search`
/// carrying any member other than `q` is, per §12.1, a malformed `view` request.
const APP: &str = r##"{
  format: 1
  name: view-argument-object-not-closed
  suite: scenario
  spec: ["#clients", "§12.1"]
  package: {
    $liasse: 1
    $app: "t.viewargs@1.0.0"
    $model: {
      tasks: { $key: "id", id: "uuid = uuid()", title: "text" }
      $mut: { add: [ "t = .tasks + { title: @title }", "return t { id, title }" ] }
      bytitle: { $view: ".tasks[:t | t.title == @q] { id, title }" }
      $public: {
        tasks: { $view: ".tasks { id, title }", $mut: { add: ".add" } }
        search: { $view: ".bytitle" }
      }
    }
    $data: { tasks: {} }
  }
  steps: STEPS
}"##;

fn run(steps: &str) -> CaseResult {
    let text = APP.replace("STEPS", steps);
    let case = Case::from_hjson(&text, Path::new("<view-argument-object-not-closed>"), &BTreeSet::new())
        .expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new("view-argument-object-not-closed"), SuiteKind::Red, &case)
}

fn observed(result: &CaseResult, index: usize) -> Option<Outcome> {
    result.steps.get(index).and_then(|s| s.observed)
}

/// CONTROL (passes today): the `call` argument object is closed — an undeclared
/// member is rejected, and so is a reserved `$`-prefixed member. This isolates the
/// defect to the `view` path: the closed-shape machinery exists and works for the
/// sibling request kind.
#[test]
fn call_argument_object_is_closed_control() {
    let plain = run(
        r##"[
          { connect: "c1" }
          { call: "public.tasks.add", args: { title: "a", extra: 1 }, on: "c1", expect: { outcome: rejected } }
        ]"##,
    );
    assert_eq!(
        observed(&plain, 1),
        Some(Outcome::Rejected),
        "control: a `call` with an undeclared member must be rejected (§12.1)"
    );

    let dollar = run(
        r##"[
          { connect: "c1" }
          { call: "public.tasks.add", args: { title: "a", "$sort": [ "title" ] }, on: "c1", expect: { outcome: rejected } }
        ]"##,
    );
    assert_eq!(
        observed(&dollar, 1),
        Some(Outcome::Rejected),
        "control: a `call` with a reserved `$`-prefixed member must be rejected (§12.1)"
    );
}

/// CONTROL (passes today): a `watch` carrying ONLY the declared parameter `q`
/// opens `ok`. The bug tests below add exactly one undeclared member to this same
/// well-formed request, so the only variable is the closed-shape rule.
#[test]
fn watch_with_only_declared_param_opens_ok_control() {
    let result = run(
        r##"[
          { connect: "c1" }
          { watch: "public.search", on: "c1", id: "w1", args: { q: "a" }, expect_init: { value: [] } }
        ]"##,
    );
    assert_eq!(
        observed(&result, 1),
        Some(Outcome::Ok),
        "control: a `watch` with only the declared `@q` must open ok"
    );
}

/// BUG (fails today): a `watch` (a `view` request, §12.1) carrying one UNDECLARED
/// member `bogus`. §12.1: an undeclared member "makes the request malformed; the
/// runtime rejects it during parameter parsing (step 3), before admission" and
/// "is never silently dropped". The runtime instead opens the subscription `ok`,
/// silently ignoring `bogus`.
#[test]
fn watch_with_undeclared_member_must_be_rejected() {
    let result = run(
        r##"[
          { connect: "c1" }
          { watch: "public.search", on: "c1", id: "w1", args: { q: "a", bogus: 1 } }
        ]"##,
    );
    let got = observed(&result, 1);
    assert_eq!(
        got,
        Some(Outcome::Rejected),
        "§12.1 violated: a `view`/`watch` request whose argument object carries an \
         undeclared member `bogus` MUST be rejected as malformed (\"an undeclared \
         member is never silently dropped\"), but the surface `watch` path served it \
         (observed {got:?}) — it forwards `watch.args()` to `engine.view_with` with \
         no closed-shape check against the view's declared `$params`, unlike the \
         `call` path's `build_request`. Root cause: \
         crates/liasse-surface/src/host/call.rs `watch`/`open_subscription`."
    );
}

/// BUG (fails today): the reserved-`$`-prefixed variant §12.1 names explicitly.
/// A `watch` carrying `$size` (a name that is a window/paging directive elsewhere)
/// must be rejected as malformed, not silently ignored — otherwise a client can
/// smuggle reserved directives into a `view` request and have them quietly
/// dropped rather than refused.
#[test]
fn watch_with_dollar_prefixed_member_must_be_rejected() {
    let result = run(
        r##"[
          { connect: "c1" }
          { watch: "public.search", on: "c1", id: "w1", args: { q: "a", "$size": 1 } }
        ]"##,
    );
    let got = observed(&result, 1);
    assert_eq!(
        got,
        Some(Outcome::Rejected),
        "§12.1 violated: a `view`/`watch` argument object carrying a reserved \
         `$`-prefixed member (`$size`) MUST make the request malformed — §12.1 calls \
         this case out by name — but the surface `watch` path served it (observed \
         {got:?}) and silently dropped `$size`. Root cause: the `watch`/`resume` \
         path in crates/liasse-surface/src/host/call.rs performs no closed-shape \
         validation of view arguments against the declared `$params`."
    );
}
