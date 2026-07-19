//! COMPLETENESS — §5.2/§5.3/§5.10/§8.8: the prospective row a row `$check` (and a
//! computed value) reads must expose EVERY member kind, with NO false load-reject and
//! NO admission fault. This is the DRY guard for the F-N1 + F-N2 cluster (and their
//! F1/F2/F3 ancestors): the compile-time self-ref scope and the admission prospective
//! row-cell are now COMPLETE across member kinds, so pinning one point per kind keeps
//! the whole seam closed rather than the two red-team points alone.
//!
//! One row `$check` reads, in a single expression, a PLAIN field (`.name`), a
//! STATIC-STRUCT member (`.meta.tag`, §5.3), a COMPUTED value (`.label`, §5.2), a
//! COMPUTED-of-a-computed (`.label_echo = .label`, exercising the dependency-order
//! fold), a SET field (`.tags`), a MAP field (`.attrs`), an OPTIONAL field (`.nick`),
//! and an ENUM field (`.status`). §5.2 (SPEC.md:402): a computed value "participates
//! in views, checks, sorting, and projections like any other value"; §5.10 places no
//! member-kind restriction on a row check. `x == x` reads a cell of ANY value-type and
//! is true (a value equals itself), so it proves the cell is PRESENT without leaning on
//! a type-specific op; the `size(...) > 0` conjuncts (over the field, struct member,
//! and the two computed values) make the check ENFORCE — a blank name empties them and
//! the row is rejected as a `Check`, never faulted and never silently admitted.
//!
//! Asserted on BOTH a FLAT collection and a self-referential one (§5.8): the self-ref
//! LOAD compiles the struct/computed/check at every self-ref depth (F-N1 — a false
//! load-reject would surface here), and admission runs the member-kind check both at
//! the top level and at a NESTED (deep) self-ref row (F-N2 — a fault would surface
//! there). Every expectation is deducible from SPEC.md alone.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

mod support;

use liasse_runtime::{CallOutcome, CallRequest, Engine, RejectionReason, Value};
use liasse_store::MemoryStore;
use liasse_value::Text;
use support::{generator, load};

fn text(value: &str) -> Value {
    Value::Text(Text::new(value))
}

fn is_committed(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Committed { .. })
}

fn is_check_rejection(o: &CallOutcome) -> bool {
    matches!(o, CallOutcome::Rejected(r) if r.reason() == RejectionReason::Check)
}

/// Dispatch `request` on `engine`; an admission refusal is an outcome, not an error.
fn call(engine: &mut Engine<MemoryStore>, request: CallRequest) -> CallOutcome {
    let mut generator = generator();
    engine.call(&request, &mut generator).expect("call dispatches")
}

/// A FLAT collection carrying one member of EVERY kind, and a row `$check` that reads
/// all of them. `tags`/`attrs`/`nick` take their omitted-container/optional defaults
/// (empty set, empty map, `none`), which the check reads through `== .self`; `status`
/// and `meta` are supplied by the mutation.
const FLAT: &str = r##"{
  "$liasse": 1, "$app": "t.mkflat@1.0.0",
  "$model": {
    "widgets": {
      "$key": "id",
      "id": "text",
      "name": "text",
      "tags": { "$set": "text" },
      "attrs": "map<text, text>",
      "nick": "text?",
      "status": { "$enum": ["draft", "active", "closed"] },
      "meta": { "tag": "text" },
      "label": "= .name",
      "label_echo": "= .label",
      "$check": [
        "size(.name) > 0 && size(.meta.tag) > 0 && size(.label) > 0 && size(.label_echo) > 0 && .tags == .tags && .attrs == .attrs && .nick == .nick && .status == .status",
        "a row check reads every member kind"
      ]
    },
    "widgets_view": { "$view": ".widgets { id, name, label }" },
    "$mut": { "add": ".widgets + { id: @id, name: @name, status: @status, meta: { tag: @tag } }" }
  }
}"##;

/// FLAT: the load must succeed (no false load-reject — the check type-checks reading
/// every member kind), a satisfying row must be admitted (no admission fault — the
/// prospective row-cell exposes every kind), the computed value must materialize, and
/// a blank-name row must be a `Check` rejection (the check ENFORCES, not merely
/// tolerates).
#[test]
fn flat_row_check_reads_every_member_kind() {
    let mut engine: Engine<MemoryStore> = load("mk-flat", FLAT);

    let good = call(
        &mut engine,
        CallRequest::new("add")
            .arg("id", text("w1"))
            .arg("name", text("acme"))
            .arg("status", text("active"))
            .arg("tag", text("t")),
    );
    assert!(is_committed(&good), "a row satisfying an every-member-kind check must be admitted, got {good:?}");

    let view = engine.view_at_head("widgets_view").expect("view ok").expect("view declared");
    assert_eq!(view.rows()[0].field("label"), Some(&text("acme")), "the computed value materializes through the read path");

    let bad = call(
        &mut engine,
        CallRequest::new("add")
            .arg("id", text("w2"))
            .arg("name", text(""))
            .arg("status", text("active"))
            .arg("tag", text("t")),
    );
    assert!(is_check_rejection(&bad), "a blank-name row must be a Check rejection under the every-member-kind check, got {bad:?}");
}

/// A SELF-REFERENTIAL collection (§5.8) carrying the same member-of-every-kind shape,
/// whose struct/computed/check compiles at every self-ref depth (F-N1). `add` inserts
/// a top row; `add_sub` inserts a nested (deep) self-ref row under it.
const SELFREF: &str = r##"{
  "$liasse": 1, "$app": "t.mksr@1.0.0",
  "$types": {
    "company": {
      "$key": "id",
      "id": "text",
      "name": "text",
      "tags": { "$set": "text" },
      "attrs": "map<text, text>",
      "nick": "text?",
      "status": { "$enum": ["draft", "active", "closed"] },
      "meta": { "tag": "text" },
      "label": "= .name",
      "label_echo": "= .label",
      "$check": [
        "size(.name) > 0 && size(.meta.tag) > 0 && size(.label) > 0 && size(.label_echo) > 0 && .tags == .tags && .attrs == .attrs && .nick == .nick && .status == .status",
        "a self-ref row check reads every member kind"
      ],
      "subcompanies": "company"
    }
  },
  "$model": {
    "companies": "company",
    "$mut": {
      "add": ".companies + { id: @id, name: @name, status: @status, meta: { tag: @tag } }",
      "add_sub": ".companies[@parent].subcompanies + { id: @id, name: @name, status: @status, meta: { tag: @tag } }"
    }
  }
}"##;

/// SELF-REF: the load must succeed (F-N1 — the struct/computed/check compiles at every
/// self-ref depth, not just the shallow ones), the top-level row must be admitted, a
/// NESTED (deep) self-ref row must be admitted (F-N2 — the member-kind check runs
/// without fault at a nested level), and a blank-name row must be a `Check` rejection.
#[test]
fn selfref_row_check_reads_every_member_kind_deep() {
    let mut engine: Engine<MemoryStore> = load("mk-selfref", SELFREF);

    let top = call(
        &mut engine,
        CallRequest::new("add")
            .arg("id", text("c1"))
            .arg("name", text("acme"))
            .arg("status", text("active"))
            .arg("tag", text("t")),
    );
    assert!(is_committed(&top), "a top self-ref row satisfying the every-member-kind check must be admitted, got {top:?}");

    let nested = call(
        &mut engine,
        CallRequest::new("add_sub")
            .arg("parent", text("c1"))
            .arg("id", text("c1a"))
            .arg("name", text("sub"))
            .arg("status", text("draft"))
            .arg("tag", text("s")),
    );
    assert!(is_committed(&nested), "a NESTED (deep) self-ref row satisfying the every-member-kind check must be admitted, got {nested:?}");

    let bad = call(
        &mut engine,
        CallRequest::new("add")
            .arg("id", text("c2"))
            .arg("name", text(""))
            .arg("status", text("active"))
            .arg("tag", text("t")),
    );
    assert!(is_check_rejection(&bad), "a blank-name self-ref row must be a Check rejection under the every-member-kind check, got {bad:?}");
}
