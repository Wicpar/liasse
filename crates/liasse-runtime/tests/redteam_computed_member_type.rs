#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! RED TEAM (§5.1 / §5.2 / §5.3): a computed value's member type is lost.
//!
//! ROOT CAUSE: `crates/liasse-model/src/build/fields.rs:28` builds every `"= expr"`
//! computed scalar field with `ty: Type::Json`. The static checker therefore
//! resolves a reference to a computed member (`.tax`, `.total`) as `json`, and any
//! *typed* operator applied to it — arithmetic in a default/computed/view, a
//! comparison in a `$check` — has "no type for operands `json` and <T>" and the
//! package is REJECTED at load. The runtime itself evaluates the same dependency
//! chain correctly (see the passing control), so the defect is purely the lost
//! static type: a computed member never carries the type its expression produces.
//!
//! SPEC (the oracle):
//!   §5.1 (SPEC.md:388): "Defaults and computed insertion values form one
//!     dependency graph. The model is valid when that graph is acyclic, and the
//!     implementation MAY evaluate it in any topological order."
//!   §5.2 (SPEC.md:404): a computed value "participates in views, checks, sorting,
//!     and projections like any other value."
//!   §5.3 (SPEC.md:423): "Their dependency relationships determine evaluation where
//!     expressions refer to one another."
//!
//! Each acyclic model below is therefore VALID and MUST load; the impl rejects it.
//! Expecteds are hand-derived from the arithmetic, not read off the impl.

mod support;

use liasse_runtime::{CallRequest, Engine, EngineError, FixedGenerators, Precision, Value};
use liasse_store::MemoryStore;
use liasse_ident::InstanceId;
use liasse_value::{Integer, Text};

fn int(v: i64) -> Value {
    Value::Int(Integer::from(v))
}
fn text(v: &str) -> Value {
    Value::Text(Text::new(v))
}

/// Load `def`, rendering the accumulated static diagnostics into the error string
/// so a false rejection reports *why* the checker refused a spec-valid model.
fn load_pkg(name: &str, def: &str) -> Result<Engine<MemoryStore>, String> {
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    Engine::load(MemoryStore::new(InstanceId::new(name)), def, &mut g).map_err(|error| match error {
        EngineError::Invalid(diags) => {
            diags.iter().map(|d| d.message().to_owned()).collect::<Vec<_>>().join("; ")
        }
        other => format!("{other}"),
    })
}

// A chained computed value: `tax` is computed from a writable field, and `total`
// is computed from a writable field PLUS the computed `tax`. The dependency graph
// subtotal -> tax -> total is acyclic, so §5.1 makes the model valid.
const CHAINED_COMPUTED: &str = r#"{
  "$liasse": 1, "$app": "t.chain@1.0.0",
  "$model": {
    "invoices": {
      "$key": "id", "id": "text",
      "subtotal": "int",
      "tax":   "= .subtotal * 2",
      "total": "= .subtotal + .tax"
    },
    "v": { "$view": ".invoices { id, tax, total }" },
    "$mut": { "add": ".invoices + { id: @id, subtotal: @subtotal }" }
  }
}"#;

#[test]
fn chained_computed_value_is_valid_and_computes() {
    // §5.1: subtotal -> tax -> total is acyclic, so the model MUST load.
    let mut engine = match load_pkg("chain", CHAINED_COMPUTED) {
        Ok(engine) => engine,
        Err(diag) => panic!(
            "§5.1: an acyclic computed-on-computed dependency graph is a VALID model, \
             but load rejected it: {diag}"
        ),
    };
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    engine
        .call(&CallRequest::new("add").arg("id", text("i1")).arg("subtotal", int(100)), &mut g)
        .expect("call")
        .committed_at()
        .expect("commits");
    let view = engine.view_at_head("v").expect("v").expect("declared");
    let row = &view.rows()[0];
    // subtotal = 100 -> tax = 200 -> total = subtotal + tax = 300.
    assert_eq!(row.field("tax"), Some(&int(200)), "tax = subtotal*2");
    assert_eq!(row.field("total"), Some(&int(300)), "total = subtotal + tax (computed-on-computed)");
}

// §5.2: a computed value "participates in ... checks ... like any other value".
// A row `$check` compares the computed `total` — this MUST be a legal state
// constraint. The impl rejects the load with "cannot compare `json` with `int`".
const CHECK_READS_COMPUTED: &str = r#"{
  "$liasse": 1, "$app": "t.chk@1.0.0",
  "$model": {
    "invoices": {
      "$key": "id", "id": "text",
      "subtotal": "int",
      "total": "= .subtotal * 2",
      "$check": [".total >= 0", "the total must be non-negative"]
    },
    "$mut": { "add": ".invoices + { id: @id, subtotal: @subtotal }" }
  }
}"#;

#[test]
fn row_check_may_read_a_computed_value() {
    // §5.2: a computed value participates in checks like any other value, so a
    // `$check` comparing `.total` MUST be accepted at load.
    let mut engine = match load_pkg("chk", CHECK_READS_COMPUTED) {
        Ok(engine) => engine,
        Err(diag) => panic!(
            "§5.2: a computed value participates in checks 'like any other value', but a \
             `$check` reading a computed member was rejected at load: {diag}"
        ),
    };
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    let outcome = engine
        .call(&CallRequest::new("add").arg("id", text("i1")).arg("subtotal", int(5)), &mut g)
        .expect("call");
    // total = 10 >= 0, so the check passes and the row commits.
    assert!(
        outcome.committed_at().is_some(),
        "check `.total >= 0` should pass for subtotal=5 (total=10); got {:?}",
        outcome.rejection()
    );
}

// §5.1: "Defaults and computed insertion values form one dependency graph." A
// default may therefore be derived from a computed value. The acyclic graph
// subtotal -> tax(computed) -> booked(default) is valid; the impl rejects it with
// "operator has no type for operands `json` and `int`".
const DEFAULT_FROM_COMPUTED: &str = r#"{
  "$liasse": 1, "$app": "t.dfc@1.0.0",
  "$model": {
    "invoices": {
      "$key": "id", "id": "text",
      "subtotal": "int",
      "tax":    "= .subtotal * 2",
      "booked": "int = .tax + 1"
    },
    "v": { "$view": ".invoices { id, booked }" },
    "$mut": { "add": ".invoices + { id: @id, subtotal: @subtotal }" }
  }
}"#;

#[test]
fn default_may_be_derived_from_a_computed_value() {
    // §5.1: defaults and computed values are one dependency graph, so a default
    // reading a computed member is valid when the graph is acyclic.
    let mut engine = match load_pkg("dfc", DEFAULT_FROM_COMPUTED) {
        Ok(engine) => engine,
        Err(diag) => panic!(
            "§5.1: defaults and computed values form one dependency graph, but a default \
             derived from a computed member was rejected at load: {diag}"
        ),
    };
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    engine
        .call(&CallRequest::new("add").arg("id", text("i1")).arg("subtotal", int(10)), &mut g)
        .expect("call")
        .committed_at()
        .expect("commits");
    let view = engine.view_at_head("v").expect("v").expect("declared");
    // subtotal = 10 -> tax = 20 -> booked default = tax + 1 = 21.
    assert_eq!(view.rows()[0].field("booked"), Some(&int(21)), "booked default = tax + 1");
}

// PASSING CONTROL: proves the defect is *only* the lost static type.
//  (a) The §5.2-shaped example with a computed over WRITABLE fields loads and
//      computes — so the computed machinery is otherwise sound.
//  (b) A pure ALIAS of a computed member (`echo = .tax`, no typed operator) loads
//      and the RUNTIME evaluates the computed-on-computed chain correctly — so the
//      dependency ordering and evaluation are correct; only the static type of a
//      computed member (json) is wrong, and it only bites when a typed operator
//      touches the member.
const CONTROL: &str = r#"{
  "$liasse": 1, "$app": "t.ctrl@1.0.0",
  "$model": {
    "invoices": {
      "$key": "id", "id": "text",
      "subtotal": "int", "shipping": "int",
      "total": "= .subtotal + .shipping",
      "echo":  "= .total"
    },
    "v": { "$view": ".invoices { id, total, echo }" },
    "$mut": { "add({ id: text, subtotal: int, shipping: int })": ".invoices + { id: @id, subtotal: @subtotal, shipping: @shipping }" }
  }
}"#;

#[test]
fn control_writable_computed_and_alias_are_sound() {
    let mut engine = load_pkg("ctrl", CONTROL).expect("control model loads");
    let mut g = FixedGenerators::new(1_700_000_000_000_000, Precision::Micros);
    engine
        .call(
            &CallRequest::new("add").arg("id", text("i1")).arg("subtotal", int(30)).arg("shipping", int(5)),
            &mut g,
        )
        .expect("call")
        .committed_at()
        .expect("commits");
    let view = engine.view_at_head("v").expect("v").expect("declared");
    let row = &view.rows()[0];
    // total = 30 + 5 = 35 (computed over writable fields); echo aliases total = 35
    // (computed-on-computed, evaluated correctly by the runtime).
    assert_eq!(row.field("total"), Some(&int(35)), "computed over writable fields");
    assert_eq!(row.field("echo"), Some(&int(35)), "runtime evaluates computed-on-computed alias");
}
