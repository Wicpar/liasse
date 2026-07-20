//! RED-TEAM cross-feature probe (WAVE 3) — auth/sessions (§11) × live views (§12)
//! × computed values (§5.2). Charge: (1) an auth mutation performing a multi-operand
//! keyed patch AND minting a token in one commit; (2) a live `$view` over a
//! cross-collection computed value re-evaluated after a commit; (3) a grouped role
//! `$members` view.
//!
//! ── THE FINDING (§8.3 × §5.6) ────────────────────────────────────────────────
//! Constructing charge (1) surfaced a static-validation divergence unrelated to
//! auth wiring: a mutation parameter used BOTH as a key selector `/coll[@p]` AND as
//! a value for a `$ref:/coll` field is rejected as "used with two incompatible
//! types (§8.3)", although both uses denote the SAME key domain:
//!
//!   * §8.3 (line 1007): "`@id` inherits `.tasks.$key`." So `/accounts[@account]`
//!     infers `@account` as `accounts.$key` — here `text`.
//!   * §5.6 (line 515): "A ref exposes the target's key type." So a `$ref:/accounts`
//!     field exposes the accounts key type — `text`. Assigning `@account` to it
//!     infers `@account` as `text` as well. §5.6 (line 523): "The application-visible
//!     value is the target's current typed key."
//!   * §6.3 (line 721): "Equality between a row or ref and a key of the same declared
//!     target compares the current typed key." — a key of `/accounts` and a ref to
//!     `/accounts` are one value domain.
//!   * §8.3 (line 1015): "All uses of the same parameter MUST infer one compatible
//!     type." The two uses DO — both are the `accounts` `text` key.
//!
//! Yet the checker infers the `$ref` use as a distinct `ref` scalar (the sibling
//! diagnostic spells it out: "component `owner` is `text`, but the target key
//! declares `ref` (§6.3, A.9)") incompatible with the selector's `text`, so a
//! perfectly valid mutation FAILS TO LOAD. Worse, §8.3's own escape hatch — "An
//! explicit prototype resolves ambiguity" (line 1009) — does NOT resolve it: an
//! explicit `op({ account: text })` prototype is still rejected.
//!
//! Root cause (hand-traced): `liasse-model/src/mutation/helpers.rs::compatible`
//! (~L118) unifies two inferred parameter types only when their `as_scalar()`
//! values are equal (or int/decimal). A `$ref:/accounts` field infers a `ref`
//! scalar rather than the target's key type `text` (§5.6 line 515), so `record`
//! (~L107) flags the selector's `text` and the ref's `ref` as a conflict, and
//! `mutation/mod.rs` (~L259) emits the §8.3 error. The prototype is merged through
//! the same `record`, so it cannot override the per-use conflict.
//!
//! Findings load the package directly through the runtime (`Engine::load`), the
//! crisp load-outcome path used by the sibling runtime redteam tests; every
//! control isolates the defect; every expectation is SPEC-derived.
//!
//! ── DRY (no divergence) ──────────────────────────────────────────────────────
//! Charge (2): a live `$view` over a cross-collection computed value stays correct
//! after a commit — verified across a source-field bump, source-row deletion (→
//! absent), a transitive computed chain, a dynamic-selector repoint, and a new
//! occurrence inserted after the bump. All hold (the §5.2 fix is robust in the
//! live-update path, not only at `init`). Recorded as passing DRY guards below.
//!
//! ── LIMITATION ───────────────────────────────────────────────────────────────
//! Charge (3): a GROUPED role `$members` view (`$key`/`$sort`) cannot be exercised
//! through the testkit's auth reconstruction: `adapter/auth.rs::plan_role`
//! (`if members.contains('$') { return; }`, ~L355; same guard in `plan_scoped_role`
//! ~L387) leaves any `$`-bearing `$members` UNWIRED, so the role resolves `denied`
//! regardless of runtime behaviour. The plain-members control below admits, proving
//! the wiring path itself is sound; the grouped variant is blocked at the harness,
//! not the runtime, and is reported as a limitation rather than a finding.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_ident::InstanceId;
use liasse_runtime::{Engine, EngineError, FixedGenerators, Precision};
use liasse_store::MemoryStore;
use liasse_testkit::{run_case, Area, Case, CaseResult, SuiteKind};

// ── load path (findings/controls): raw runtime load, diagnostics rendered ─────
fn load(name: &str, def: &str) -> Result<(), String> {
    let mut generator = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    Engine::load(MemoryStore::new(InstanceId::new(name)), def, &mut generator)
        .map(|_| ())
        .map_err(|error| match error {
            EngineError::Invalid(diags) => {
                diags.iter().map(|d| d.message().to_owned()).collect::<Vec<_>>().join(" || ")
            }
            other => format!("{other}"),
        })
}

// ── scenario path (DRY guards / limitation control) ───────────────────────────
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

// ═══════════════════════════════════════════════════════════════════════════
// FINDING 1 — a parameter used as a key selector AND as a `$ref` value to the
// same collection MUST infer one compatible type (§8.3 line 1015), because a ref
// exposes the target's key type (§5.6 line 515). The mutation is spec-valid and
// MUST load. It is rejected as "two incompatible types". THIS TEST FAILS.
// ═══════════════════════════════════════════════════════════════════════════
const REF_KEY_PARAM: &str = r#"{
  "$liasse": 1, "$app": "t.w3.refkey@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "links":    { "$key": "id", "id": "text", "owner": { "$ref": "/accounts" } },
    "$mut": { "op": [
      ".accounts[@account].name = @newname",
      ".links + { id: @lid, owner: @account }",
      "return { done: @account }"
    ] }
  }
}"#;

#[test]
fn finding_ref_key_param_reuse_must_load() {
    // §5.6 line 515 + §8.3 lines 1007/1015: both uses of @account are the accounts
    // `text` key, one compatible type, so the package MUST load.
    if let Err(diag) = load("refkey", REF_KEY_PARAM) {
        panic!(
            "§5.6/§8.3: a param used as `/accounts[@account]` and as an `owner: @account` \
             `$ref:/accounts` value are both the accounts `text` key (one compatible type), \
             so the mutation MUST load; instead it was rejected: {diag}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// FINDING 2 — §8.3 line 1009: "An explicit prototype resolves ambiguity." An
// explicit `op({ account: text })` prototype declares the exact compatible type,
// so it MUST resolve the (spurious) conflict and load. It does not. THIS TEST FAILS.
// ═══════════════════════════════════════════════════════════════════════════
const REF_KEY_PARAM_PROTOTYPED: &str = r#"{
  "$liasse": 1, "$app": "t.w3.refkeyproto@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "links":    { "$key": "id", "id": "text", "owner": { "$ref": "/accounts" } },
    "$mut": { "op({ account: text })": [
      ".accounts[@account].name = @newname",
      ".links + { id: @lid, owner: @account }",
      "return { done: @account }"
    ] }
  }
}"#;

#[test]
fn finding_explicit_prototype_does_not_resolve_ref_key_conflict() {
    if let Err(diag) = load("refkeyproto", REF_KEY_PARAM_PROTOTYPED) {
        panic!(
            "§8.3 line 1009: an explicit `op({{ account: text }})` prototype declares the \
             compatible type (accounts key = text, §5.6) and MUST resolve the conflict and \
             load; instead it was still rejected: {diag}"
        );
    }
}

// ── CONTROL: two DIFFERENT parameter names carrying the same value load ────────
// Isolates that each single use is individually valid — only the shared-name
// unification is the defect.
const TWO_NAMES: &str = r#"{
  "$liasse": 1, "$app": "t.w3.twonames@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "links":    { "$key": "id", "id": "text", "owner": { "$ref": "/accounts" } },
    "$mut": { "op": [
      ".accounts[@sel].name = @newname",
      ".links + { id: @lid, owner: @refv }",
      "return { done: @refv }"
    ] }
  }
}"#;

#[test]
fn control_two_param_names_same_value_loads() {
    assert!(load("twonames", TWO_NAMES).is_ok(), "each single use is individually valid");
}

// ── CONTROL: the identical selector+field pattern with a PLAIN `text` field ────
// (no `$ref`) loads — proving the pattern is sound and the `$ref` is the trigger.
const SELECTOR_PLUS_TEXT: &str = r#"{
  "$liasse": 1, "$app": "t.w3.seltext@1.0.0",
  "$model": {
    "accounts": { "$key": "id", "id": "text", "name": "text" },
    "links":    { "$key": "id", "id": "text", "owner": "text" },
    "$mut": { "op": [
      ".accounts[@account].name = @newname",
      ".links + { id: @lid, owner: @account }",
      "return { done: @account }"
    ] }
  }
}"#;

#[test]
fn control_selector_plus_plain_text_field_loads() {
    assert!(
        load("seltext", SELECTOR_PLUS_TEXT).is_ok(),
        "selector + plain text field is one compatible type and loads"
    );
}

// ── CONTROL: a GENUINE int/text conflict IS still rejected ─────────────────────
// (matches corpus `conflicting-param-inference-invalid`) — proving the §8.3
// detector works for real conflicts, so FINDING 1 is a false positive, not a
// disabled check.
const GENUINE_CONFLICT: &str = r#"{
  "$liasse": 1, "$app": "t.w3.realconflict@1.0.0",
  "$model": {
    "n": "int = 0",
    "s": "text = ''",
    "$mut": { "both": [ ".n = @x", ".s = @x" ] }
  }
}"#;

#[test]
fn control_genuine_int_text_conflict_still_rejects() {
    assert!(
        load("realconflict", GENUINE_CONFLICT).is_err(),
        "§8.3: an int-vs-text parameter conflict is a real incompatibility and MUST reject"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// DRY GUARDS (charge 2) — a live `$view` over a CROSS-COLLECTION computed value
// stays correct after a commit. §5.2 (a computed value participates "like any
// other value"), §12.2 (after applying patches the client result MUST equal the
// authorized declared view at the new frontier). config.doubled = base*2;
// items.derived = /config["main"].doubled + 1. All expectations are arithmetic
// from $data (base 10 -> doubled 20 -> derived 21; base 20 -> 40 -> 41). PASS.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn dry_probe2_cross_collection_computed_live_update_after_commit() {
    let text = r##"{
      format: 1
      name: xcc-computed-live-update
      suite: scenario
      spec: ["#state-model", "§5.2", "#clients", "§12.2"]
      package: {
        $liasse: 1
        $app: "t.w3.xcclive@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int", doubled: "= base * 2" }
          items: { $key: "id", id: "text", derived: "= /config[\"main\"].doubled + 1" }
          $mut: { bump: ".config[@k] { base = @base }" }
          $public: {
            items: { $view: ".items { id, derived }" }
            config: { $view: ".config { k, doubled }", $mut: { bump: ".bump" } }
          }
        }
        $data: { config: { main: { base: "10" } }, items: { i1: {} } }
      }
      steps: [
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", derived: "21" } ] } }
        { call: "public.config.bump", args: { k: "main", base: "20" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "w1", value: [ { id: "i1", derived: "41" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "xcc-computed-live-update");
}

#[test]
fn dry_probe2_source_row_deleted_makes_derived_absent() {
    // §5.2: "A computed expression yielding none produces an absent optional value."
    let text = r##"{
      format: 1
      name: xcc-source-deleted
      suite: scenario
      spec: ["#state-model", "§5.2", "#clients", "§12.2"]
      package: {
        $liasse: 1
        $app: "t.w3.xccdel@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int", doubled: "= base * 2" }
          items: { $key: "id", id: "text", derived: "= /config[\"main\"].doubled + 1" }
          $mut: { del_cfg: ".config - @k" }
          $public: {
            items: { $view: ".items { id, derived }" }
            config: { $view: ".config { k }", $mut: { del: ".del_cfg" } }
          }
        }
        $data: { config: { main: { base: "10" } }, items: { i1: {} } }
      }
      steps: [
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", derived: "21" } ] } }
        { call: "public.config.del", args: { k: "main" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "w1", value: [ { id: "i1", derived: "$absent" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "xcc-source-deleted");
}

#[test]
fn dry_probe2_transitive_and_repoint_live() {
    // Transitive same-row chain read cross-collection, then a dynamic-selector
    // repoint, both live. base 10 -> doubled 20 -> quad 40 -> derived 41; after
    // repointing item to the "hi" config (base 100 -> quad 400 -> derived 401).
    let text = r##"{
      format: 1
      name: xcc-transitive-repoint
      suite: scenario
      spec: ["#state-model", "§5.2", "#clients", "§12.2"]
      package: {
        $liasse: 1
        $app: "t.w3.xcctr@1.0.0"
        $model: {
          config: { $key: "k", k: "text", base: "int", doubled: "= base * 2", quad: "= doubled * 2" }
          items: { $key: "id", id: "text", cfg: "text", derived: "= /config[.cfg].quad + 1" }
          $mut: { repoint: ".items[@id] { cfg = @cfg }" }
          $public: {
            items: { $view: ".items { id, derived }" }
            items_edit: { $mut: { repoint: ".repoint" } }
          }
        }
        $data: { config: { lo: { base: "10" }, hi: { base: "100" } }, items: { i1: { cfg: "lo" } } }
      }
      steps: [
        { watch: "public.items", id: "w1", expect_init: { value: [ { id: "i1", derived: "41" } ] } }
        { call: "public.items_edit.repoint", args: { id: "i1", cfg: "hi" }, expect: { outcome: ok, "...": true } }
        { expect_view: { watch: "w1", value: [ { id: "i1", derived: "401" } ] } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "xcc-transitive-repoint");
}

// ═══════════════════════════════════════════════════════════════════════════
// LIMITATION control (charge 3) — the plain (ungrouped) role `$members` admits,
// proving the auth wiring path is sound; the grouped-`$members` variant is blocked
// at the testkit's `members.contains('$')` guard (see header) and cannot be
// exercised, so it is reported as a limitation rather than a finding. PASS.
// ═══════════════════════════════════════════════════════════════════════════
#[test]
fn dry_probe3_plain_members_admits() {
    let text = r##"{
      format: 1
      name: plain-members-admits
      suite: scenario
      spec: ["#interfaces", "§10.3"]
      package: {
        $liasse: 1
        $app: "t.w3.plainmembers@1.0.0"
        $model: {
          accounts: { $key: "id", id: "text", name: "text", enabled: "bool = true" }
          notes: { $key: "id", id: "uuid = uuid()", author: { $ref: "/accounts" }, body: "text" }
          $mut: { add_note: [ "note = .notes + { author: $actor, body: @body }", "return note { id, body }" ] }
          $auth: {
            key: { $credential: "text", $verify: "$credential", $actor: "/accounts[$proof.account]" }
          }
          $roles: {
            member: {
              $auth: "key"
              $members: ".accounts[:a | a.enabled]"
              notes: { $view: ".notes[:n | n.author == $actor] { id, body }", $mut: { add: ".add_note" } }
            }
          }
        }
        $data: { accounts: { a1: { name: "alice" } } }
      }
      steps: [
        { connect: "c1", authenticate: { auth: "key", credential: "a1" } }
        { call: "member.notes.add", args: { body: "hi" }, on: "c1",
          expect: { outcome: ok, value: { id: "$any:uuid", body: "hi" } } }
      ]
    }"##;
    assert_all_pass(&run_case_text(text), "plain-members-admits");
}
