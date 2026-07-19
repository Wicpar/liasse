//! Strict-JSON translation at the adapter boundary (Annex A).
//!
//! Two directions cross here. Inbound, a case's `args` object is raw wire JSON;
//! each argument is decoded against its parameter's declared [`Type`] into a
//! canonical [`Value`] the surface layer accepts, so `"20"` becomes an `int` and
//! `"  x  "` a `text` exactly as the wire form pins (FORMAT.md "int wire values
//! are JSON strings"). Outbound, a mutation `return` ([`ResponseValue`]) and a
//! view result ([`ViewResult`]) are rendered back to the same canonical JSON the
//! corpus matchers expect. When a parameter's type is unknown (a receiver key of
//! a row mutation, whose type this phase does not resolve), the value's own JSON
//! shape picks the nearest scalar type — a best effort the triage loop tightens.

use std::collections::BTreeMap;

use liasse_runtime::{ResponseValue, ViewResult, ViewRow};
use liasse_value::{Json, Text, Type, Value};
use serde_json::Value as J;

/// Decode one `args` object into typed [`Value`]s, using `types[name]` for each
/// argument's declared type and falling back to a shape inference otherwise.
#[must_use]
pub fn decode_args(args: &J, types: &BTreeMap<String, Type>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(map) = args.as_object() {
        for (name, wire) in map {
            let value = match types.get(name) {
                Some(ty) => ty.decode(wire).unwrap_or_else(|_| infer(wire)),
                None => infer(wire),
            };
            out.insert(name.clone(), value);
        }
    }
    out
}

/// Decode a single wire value against an optional declared [`Type`] — the §10.5
/// scope-row key a scoped subscription is addressed under. When the scope
/// collection's key type is known the value is decoded against it (so a
/// `uuid`/`int` scope matches by value); otherwise its JSON shape picks the
/// nearest scalar.
#[must_use]
pub fn decode_value(wire: &J, ty: Option<&Type>) -> Value {
    match ty {
        Some(ty) => ty.decode(wire).unwrap_or_else(|_| infer(wire)),
        None => infer(wire),
    }
}

/// The nearest canonical value for a wire JSON with no declared type: a string is
/// text, a bool is bool, and any composite or number is carried verbatim as
/// `json` so nothing is silently coerced.
fn infer(wire: &J) -> Value {
    match wire {
        J::String(text) => Value::Text(Text::new(text.clone())),
        J::Bool(flag) => Value::Bool(*flag),
        other => Json::from_wire(other).map_or_else(|_| Value::Text(Text::new(other.to_string())), Value::Json),
    }
}

/// Render a mutation response to its canonical strict-JSON projection (Annex A).
#[must_use]
pub fn response_to_json(response: &ResponseValue) -> J {
    response.to_wire()
}

/// Render a view result per its delivered shape (§12.2): a singular view (a
/// root/struct projection or an aggregate) is one JSON object, a collection view
/// a JSON array. A singular view materializes to exactly one row, whose fields
/// are that object; anything else falls back to the array form.
#[must_use]
pub fn view_to_json_shaped(result: &ViewResult, singular: bool) -> J {
    if let Some(value) = result.scalar() {
        return value.to_wire();
    }
    match (singular, result.rows()) {
        (true, [row]) => row_to_json(row),
        _ => rows_to_json(result.rows()),
    }
}

/// Render one view row to a strict-JSON object of its output fields.
fn row_to_json(row: &ViewRow) -> J {
    let mut object = serde_json::Map::new();
    for (name, value) in row.fields() {
        object.insert(name.clone(), value.to_wire());
    }
    J::Object(object)
}

/// Render a slice of view rows to the same strict-JSON array shape (a windowed
/// subscription delivers rows directly rather than a full [`ViewResult`]).
#[must_use]
pub fn rows_to_json(rows: &[ViewRow]) -> J {
    J::Array(rows.iter().map(row_to_json).collect())
}
