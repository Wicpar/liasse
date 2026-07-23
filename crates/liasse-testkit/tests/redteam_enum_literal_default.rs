//! RED-TEAM probe: a bare-literal `$default` on an enum-typed field.
//!
//! §4.2 + Annex C.4 make an expanded `$default` a *literal-or-expression* position:
//! a string NOT beginning with `=` is a LITERAL, so `$default: "open"` on an enum
//! field (§5.9) is the declared label "open" and §5.1 supplies it when the field is
//! omitted. The defect this pins: the model routed EVERY `$default` string through
//! the expression parser, so a bare literal `open` compiled to a bare identifier
//! (rejected as "unknown name") — while the inline `{ $enum: [...], $default: ... }`
//! form dropped `$default` on the floor (the enum-node path never read it), so the
//! defaulted field projected ABSENT. Neither the expanded nor the inline form
//! applied on the insert OR the seed path.
//!
//! The four spellings below of one enum default — expanded `{ $type, $default }`,
//! inline `{ $enum, $default }`, the `'`-escaped literal, and the seed path — must
//! all supply "open"; a supplied value must take precedence; and an out-of-domain
//! literal label must reject at LOAD (§5.9 closed set), never at first insertion.
//! Every expectation is deducible from SPEC.md text alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use liasse_testkit::{run_case, Area, Case, CaseResult, ScenarioAdapter, SuiteKind};

fn run(name: &str, text: &str) -> CaseResult {
    let case = Case::from_hjson(text, Path::new(name), &BTreeSet::new()).expect("case parses");
    let mut adapter = ScenarioAdapter::build(&case);
    run_case(&mut adapter, &Area::new(name), SuiteKind::Red, &case)
}

fn assert_pass(name: &str, text: &str) {
    let result = run(name, text);
    assert!(result.verdict.is_pass(), "[{name}] verdict: {:?}", result.verdict);
    for (i, step) in result.steps.iter().enumerate() {
        assert!(step.result.is_pass(), "[{name}] step {i} ({}) failed: {:?}", step.action, step.result);
    }
}

// `FIELD` = the status field spelling; `STATUS_VIEW` selects a view/mut set.
// The `add` mutation omits `status` (so the default applies); `add_explicit`
// supplies it.
fn insert_app(app: &str, field: &str) -> String {
    format!(
        r##"{{
  format: 1
  name: enum-literal-default-insert
  suite: scenario
  spec: ["#state-model", "§5.1", "§5.9"]
  package: {{
    $liasse: 1
    $app: "{app}"
    $types: {{ State: {{ $enum: ["open", "closed"] }} }}
    $model: {{
      things: {{
        $key: "id"
        id: "text"
        status: {field}
      }}
      $public: {{
        things: {{
          $view: ".things {{ id, status }}"
          $mut: {{
            add: ".things + {{ id: @id }}"
            add_explicit: ".things + {{ id: @id, status: @status }}"
          }}
        }}
      }}
    }}
  }}
  steps: [
    {{ call: "public.things.add", args: {{ id: "t1" }}, expect: {{ outcome: ok }} }}
    {{ call: "public.things.add_explicit", args: {{ id: "t2", status: "closed" }}, expect: {{ outcome: ok }} }}
    {{ watch: "public.things", id: "w1",
      expect_init: {{ value: [ {{ id: "t1", status: "open" }}, {{ id: "t2", status: "closed" }} ] }} }}
  ]
}}"##
    )
}

fn seed_app(app: &str, field: &str) -> String {
    format!(
        r##"{{
  format: 1
  name: enum-literal-default-seed
  suite: scenario
  spec: ["#state-model", "§5.1", "§5.9", "§9.1"]
  package: {{
    $liasse: 1
    $app: "{app}"
    $types: {{ State: {{ $enum: ["open", "closed"] }} }}
    $model: {{
      things: {{
        $key: "id"
        id: "text"
        status: {field}
      }}
      $public: {{ things: {{ $view: ".things {{ id, status }}" }} }}
    }}
    $data: {{ things: {{ "t1": {{ id: "t1" }}, "t2": {{ id: "t2", status: "closed" }} }} }}
  }}
  steps: [
    {{ watch: "public.things", id: "w1",
      expect_init: {{ value: [ {{ id: "t1", status: "open" }}, {{ id: "t2", status: "closed" }} ] }} }}
  ]
}}"##
    )
}

const EXPANDED: &str = r##"{ $type: "State", $default: "open" }"##;
const INLINE: &str = r##"{ $enum: ["open", "closed"], $default: "open" }"##;
const INLINE_ESCAPED: &str = r##"{ $enum: ["open", "closed"], $default: "'open" }"##;

#[test]
fn expanded_enum_literal_default_insert() {
    assert_pass("expanded-insert", &insert_app("t.exi@1.0.0", EXPANDED));
}

#[test]
fn expanded_enum_literal_default_seed() {
    assert_pass("expanded-seed", &seed_app("t.exs@1.0.0", EXPANDED));
}

#[test]
fn inline_enum_literal_default_insert() {
    assert_pass("inline-insert", &insert_app("t.ini@1.0.0", INLINE));
}

#[test]
fn inline_enum_literal_default_seed() {
    assert_pass("inline-seed", &seed_app("t.ins@1.0.0", INLINE));
}

#[test]
fn escaped_enum_literal_default_insert() {
    // §C.4: `'open` is the literal "open" with one leading `'` removed.
    assert_pass("escaped-insert", &insert_app("t.esci@1.0.0", INLINE_ESCAPED));
}

/// CONTROL: a non-enum bare-literal default (`text`) is unchanged by the fix.
#[test]
fn text_literal_default_control() {
    let app = r##"{
  format: 1
  name: text-literal-default-insert
  suite: scenario
  spec: ["#state-model", "§5.1"]
  package: {
    $liasse: 1
    $app: "t.textdef@1.0.0"
    $model: {
      things: { $key: "id", id: "text", label: { $type: "text", $default: "anon" } }
      $public: { things: {
        $view: ".things { id, label }"
        $mut: { add: ".things + { id: @id }" }
      } }
    }
  }
  steps: [
    { call: "public.things.add", args: { id: "t1" }, expect: { outcome: ok } }
    { watch: "public.things", id: "w1", expect_init: { value: [ { id: "t1", label: "anon" } ] } }
  ]
}"##;
    assert_pass("text-control", app);
}
