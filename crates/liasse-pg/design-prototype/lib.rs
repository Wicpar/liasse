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

/// Stand-in for `liasse.eval`'s boolean face: `pred` is `{"field": f, "ne": s}`
/// over the row's tagged wire form `{"st": [[name, tagged], ...]}`. Verdict is
/// the strict `Bool(true)` truthiness contract; an absent field or a present
/// `{"none":true}` reads as `none`, and `none != text` is TRUE under
/// `Value::cmp` (none is its own rank, unequal to every present value).
#[pg_extern(immutable, parallel_safe, strict)]
fn liasse_eval_demo(pred: &[u8], row: pgrx::JsonB) -> bool {
    let pred: DemoPred = match serde_json::from_slice(pred) {
        Ok(pred) => pred,
        Err(error) => pgrx::error!("liasse_eval_demo: malformed predicate: {error}"),
    };
    let field_text = tagged_field(&row.0, &pred.field)
        .and_then(|tagged| tagged.get("s").and_then(|s| s.as_str().map(str::to_owned)));
    match field_text {
        Some(text) => text != pred.ne,
        None => true, // none != text is TRUE
    }
}

/// The tagged wire value of one stored field, or `None` when absent.
fn tagged_field(row: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
    row.get("st")?.as_array()?.iter().find_map(|pair| {
        (pair.get(0)?.as_str()? == name).then(|| pair.get(1).cloned())?
    })
}

fn as_int(tagged: &serde_json::Value) -> Option<i64> {
    tagged.get("i")?.as_i64()
}

// ---- the GENERAL evaluator faces (stand-ins) ------------------------------
//
// The real `liasse.eval` deserializes a postcard `TypedExpr` + env and runs the
// linked interpreter, returning the result Cell's tagged wire form as jsonb.
// These stand-ins prove the three call-site faces the read-side pushdown needs:
// a jsonb-returning projection evaluation (SELECT position), an
// order-preserving bytea sort-tuple encoding (ORDER BY position), and the
// boolean face above (WHERE position) — all per-row, all index-compatible.

#[derive(serde::Deserialize)]
struct ProjSpec {
    outputs: Vec<ProjOut>,
}

/// One projected output: a stored-field passthrough (`field`) or a
/// computed-style arithmetic output over two stored int fields (`sub`) — the
/// stand-in for an arbitrary interpreter expression over the candidate row.
#[derive(serde::Deserialize)]
struct ProjOut {
    name: String,
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    sub: Option<(String, String)>,
}

/// Stand-in for `liasse.eval` in SELECT position: evaluate the projection over
/// the row's tagged wire form and return the projected row's cells as jsonb
/// (tagged wire per output; an absent/faulted output is `{"none":true}`).
#[pg_extern(immutable, parallel_safe, strict)]
fn liasse_eval_demo_project(spec: &[u8], row: pgrx::JsonB) -> pgrx::JsonB {
    let spec: ProjSpec = match serde_json::from_slice(spec) {
        Ok(spec) => spec,
        Err(error) => pgrx::error!("liasse_eval_demo_project: malformed spec: {error}"),
    };
    let none = serde_json::json!({"none": true});
    let mut out = serde_json::Map::new();
    for output in spec.outputs {
        let cell = match (&output.field, &output.sub) {
            (Some(name), _) => tagged_field(&row.0, name).unwrap_or_else(|| none.clone()),
            (None, Some((a, b))) => {
                let lhs = tagged_field(&row.0, a).as_ref().and_then(as_int);
                let rhs = tagged_field(&row.0, b).as_ref().and_then(as_int);
                match (lhs, rhs) {
                    (Some(x), Some(y)) => serde_json::json!({"i": x - y}),
                    _ => none.clone(),
                }
            }
            _ => none.clone(),
        };
        out.insert(output.name, cell);
    }
    pgrx::JsonB(serde_json::Value::Object(out))
}

#[derive(serde::Deserialize)]
struct SortSpec {
    keys: Vec<SortKeySpec>,
}

#[derive(serde::Deserialize)]
struct SortKeySpec {
    field: String,
    #[serde(default)]
    desc: bool,
}

/// Stand-in for `liasse.eval_sort` in ORDER BY position: evaluate each sort key
/// over the row and encode the tuple order-preservingly (memcmp on the bytea
/// equals §7.3 order). Per key: ascending emits present values under rank 0x01
/// (big-endian, sign-flipped ints; NUL-terminated text) then `none` under rank
/// 0x02 — §7.3 "present values, then none"; descending emits `none` under rank
/// 0x00 then present values with all value bytes inverted — §7.3 "none, then
/// present values in reverse order". The real encoding reuses `key_enc`'s
/// NUL-escaping machinery; the terminator here is the stand-in shortcut.
#[pg_extern(immutable, parallel_safe, strict)]
fn liasse_eval_demo_ord(spec: &[u8], row: pgrx::JsonB) -> Vec<u8> {
    let spec: SortSpec = match serde_json::from_slice(spec) {
        Ok(spec) => spec,
        Err(error) => pgrx::error!("liasse_eval_demo_ord: malformed spec: {error}"),
    };
    let mut enc = Vec::new();
    for key in &spec.keys {
        let bytes = tagged_field(&row.0, &key.field).as_ref().and_then(value_ord_bytes);
        match (key.desc, bytes) {
            (false, Some(bytes)) => {
                enc.push(0x01);
                enc.extend(bytes);
            }
            (false, None) => enc.push(0x02),
            (true, Some(bytes)) => {
                enc.push(0x01);
                enc.extend(bytes.iter().map(|byte| !byte));
            }
            (true, None) => enc.push(0x00),
        }
    }
    enc
}

/// The ascending order-preserving byte form of one scalar sort value.
fn value_ord_bytes(tagged: &serde_json::Value) -> Option<Vec<u8>> {
    if let Some(int) = as_int(tagged) {
        return Some(((int as u64) ^ 0x8000_0000_0000_0000).to_be_bytes().to_vec());
    }
    if let Some(text) = tagged.get("s").and_then(|s| s.as_str()) {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        return Some(bytes);
    }
    None
}
