//! RED-TEAM finding (§7.2 grouped-projection source-value constraint): a grouped
//! `$view` output that reads a NON-KEY per-row source field through a ROW BINDING
//! (`it.amount`, `employees.eid`) — instead of the equivalent bare field name
//! (`amount`) — SLIPS PAST the §7.2 aggregate/key-derived guard and loads,
//! silently exposing the first group member's value, where the SPEC mandates a
//! load-time rejection.
//!
//! # What the SPEC pins
//!
//! §7.2 (line 846): "Rows sharing the synthetic key form one group. `group` is the
//! source-row view for that output row. **Every non-key source value MUST be
//! aggregated or derived solely from key values.**"
//! §7.2 (line 848): "**Validity is a property of the declaration, not of the
//! current data, so a plain non-key source value is rejected** even when the
//! synthetic key is unique per row and no rows actually share it. To carry a
//! source field that is determined by the key, wrap it in an aggregate over
//! `group` — for example `min(group.f)` …"
//!
//! In `.items[:it] { $key: k, k: cat, leaked: it.amount }` the output `leaked`
//! reads `amount`, a non-key source field, through the `[:it]` row binding — which
//! §6.4 binds to the current source row, so `it.amount` IS a plain non-key source
//! value, exactly as bare `amount` is. §7.2 therefore REJECTS this declaration at
//! load, independent of data.
//!
//! # The divergence (root cause, hand-traced)
//!
//! The only §7.2 grouped-source guard is `references_nonkey_field`
//! (`crates/liasse-expr/src/check/walk.rs:108-126`), gated in
//! `crates/liasse-expr/src/check/project.rs:155-168`. It flags an output only when
//! it finds a BARE `ExprKind::Name(n)` with `source.field(n).is_some()` outside the
//! key set (walk.rs:119-124). A binding-qualified access `it.amount` parses as
//! `Field { receiver: Name("it"), member: "amount" }`; the walk recurses into the
//! receiver `Name("it")`, and `source.field("it")` is `None` (the source row has
//! no field named `it` — `it` is a binding), so the reference is never flagged.
//! The per-row field `amount` reached through the binding is therefore admitted
//! into the group, and at read time `project_row` evaluates it against the FIRST
//! group member (`views.rs`), leaking a non-aggregated per-row value.
//!
//! The BARE control (`leaked: amount`) IS caught (Name("amount") resolves to a
//! source field), so the guard exists and works — the gap is that it does not see
//! through a row binding to the same field.
//!
//! Every expectation follows from SPEC.md §7.2 text alone, never from observed
//! engine behaviour. Modeled after `crates/liasse-model/tests/corpus_static.rs`'s
//! `build_package`, the real static-load path a `suite: static` corpus case takes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use liasse_diag::SourceMap;
use liasse_model::Model;
use liasse_syntax::parse_document;

/// The model builder's binary accept/reject decision for a package.
enum Decision {
    Loaded,
    Rejected(String),
}

/// Serialize the raw package JSON, parse it, and build the model — the exact path
/// `corpus_static.rs::build_package` uses to judge a `suite: static` case.
fn build_package(package: &serde_json::Value) -> Decision {
    let text = serde_json::to_string_pretty(package).expect("package serializes");
    let mut sources = SourceMap::new();
    let id = sources.add_file("package.liasse", &text);
    match parse_document(id, &text) {
        Err(diags) => Decision::Rejected(diags.render(&sources)),
        Ok(document) => match Model::build(&mut sources, id, &document) {
            Ok(_) => Decision::Loaded,
            Err(diags) => Decision::Rejected(diags.render(&sources)),
        },
    }
}

fn package(view: &str) -> serde_json::Value {
    serde_json::json!({
        "$liasse": 1,
        "$app": "t.groupleak@1.0.0",
        "$model": {
            "items": { "$key": "id", "id": "text", "cat": "text", "amount": "int" },
            "v": { "$view": view }
        }
    })
}

// ── THE FINDING ─────────────────────────────────────────────────────────────
// §7.2: `leaked: it.amount` is a plain non-key source value reached through the
// `[:it]` binding and MUST be rejected at load. FAILS today: the model builds.
#[test]
fn grouped_binding_qualified_nonkey_source_is_rejected() {
    let decision = build_package(&package(".items[:it] { $key: k, k: cat, leaked: it.amount }"));
    match decision {
        Decision::Rejected(_) => {}
        Decision::Loaded => panic!(
            "§7.2 violated: the model BUILT a grouped view whose output `leaked: it.amount` is a \
             plain non-key source value reached through the `[:it]` row binding; §7.2 requires \
             every non-key source value be aggregated or key-derived, so this MUST be rejected \
             at load"
        ),
    }
}

// ── CONTROL: the BARE form of the same field IS rejected (guard exists) ───────
// `leaked: amount` resolves to a bare source-field name, which the guard catches.
#[test]
fn control_grouped_bare_nonkey_source_is_rejected() {
    let decision = build_package(&package(".items[:it] { $key: k, k: cat, leaked: amount }"));
    assert!(
        matches!(decision, Decision::Rejected(_)),
        "§7.2: a bare non-key source value in a grouped view must be rejected at load"
    );
}

// ── CONTROL: the aggregated form of the same field LOADS (well-formed) ────────
// `leaked: max(group.amount)` is aggregated over `group`, so §7.2 admits it.
#[test]
fn control_grouped_aggregated_source_loads() {
    let decision =
        build_package(&package(".items[:it] { $key: k, k: cat, leaked: max(group.amount) }"));
    assert!(
        matches!(decision, Decision::Loaded),
        "§7.2: an aggregate over `group` is a well-formed grouped output and must load"
    );
}
