//! Minimal feasibility stand-in for the real `liasse-pg-ext`: a `#[pg_extern]`
//! predicate evaluator called from the recursive term of the §10.5 coverage CTE.
//!
//! The real extension deserializes a hoisted `TypedExpr` + env and runs the
//! actual liasse interpreter; this demo proves the load-bearing mechanics —
//! a Rust cdylib function invocable per worktable row, strict truthiness,
//! `none != text` semantics over the tagged wire form, and an index-served
//! recursive plan — with a trivial evaluator over a JSON predicate.

use pgrx::prelude::*;

::pgrx::pg_module_magic!();

#[derive(serde::Deserialize)]
struct DemoPred {
    field: String,
    ne: String,
}

/// Stand-in for the version-locked ABI handshake the real extension exposes.
#[pg_extern(immutable, parallel_safe)]
fn liasse_demo_abi() -> String {
    format!("liasse-demo {}", env!("CARGO_PKG_VERSION"))
}

/// Stand-in for `liasse.eval`: `pred` is `{"field": f, "ne": s}` over the row's
/// tagged wire form `{"st": [[name, tagged], ...]}`. Verdict is the strict
/// `Bool(true)` truthiness contract; an absent field or a present
/// `{"none":true}` reads as `none`, and `none != text` is TRUE under
/// `Value::cmp` (none is its own rank, unequal to every present value).
#[pg_extern(immutable, parallel_safe, strict)]
fn liasse_eval_demo(pred: &[u8], row: pgrx::JsonB) -> bool {
    let pred: DemoPred = match serde_json::from_slice(pred) {
        Ok(pred) => pred,
        Err(error) => pgrx::error!("liasse_eval_demo: malformed predicate: {error}"),
    };
    let field_text = row
        .0
        .get("st")
        .and_then(|st| st.as_array())
        .and_then(|pairs| {
            pairs.iter().find_map(|pair| {
                let name = pair.get(0)?.as_str()?;
                (name == pred.field).then(|| pair.get(1).cloned())?
            })
        })
        .and_then(|tagged| tagged.get("s").and_then(|s| s.as_str().map(str::to_owned)));
    match field_text {
        Some(text) => text != pred.ne,
        None => true, // none != text is TRUE
    }
}
