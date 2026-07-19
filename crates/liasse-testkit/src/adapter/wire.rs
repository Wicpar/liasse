//! Strict-JSON translation at the adapter boundary (Annex A).
//!
//! Two directions cross here. Inbound, a case's `args` object is raw wire JSON;
//! each argument is decoded against its parameter's declared [`Type`] into a
//! canonical [`Value`] the surface layer accepts, so `"20"` becomes an `int` and
//! `"  x  "` a `text` exactly as the wire form pins (FORMAT.md "int wire values
//! are JSON strings"). A declared argument whose wire value does NOT decode
//! against its type is a §12.1-step-3 malformed request: the boundary surfaces
//! the typed-decode failure as an [`ArgDecodeError`] the caller reports as
//! `rejected`, never coerced to a best-effort inference (mirroring the seed
//! path, which rejects the same mismatch — `liasse-runtime/src/seed.rs`).
//!
//! Two decode shapes are legitimate and preserved. A parameter whose type this
//! phase does not resolve (a receiver key of a row mutation, §8.3) is shape-
//! inferred — the legitimate inference, not a swallowed rejection. And a set
//! operand (`set_field + values` / `- values`, §8.5) accepts EITHER a set (its
//! canonical JSON-array wire form) OR a single member: a non-array wire value for
//! a `set<T>` parameter decodes against the element type `T` as a one-member
//! operand, exactly as the runtime's set add/remove takes "a member or a set"
//! and as a set ref member is named by its target's typed key (§5.5/§A.9). A
//! wrong-typed member (a scalar that does not decode against `T`) still rejects.
//!
//! Outbound, a mutation `return` ([`ResponseValue`]) and a view result
//! ([`ViewResult`]) are rendered back to the same canonical JSON the corpus
//! matchers expect.

use std::collections::BTreeMap;
use std::fmt;

use liasse_runtime::{ResponseValue, ViewResult, ViewRow};
use liasse_value::{Json, Text, Type, Value, ValueError};
use serde_json::Value as J;

/// A §12.1 typed-argument-decode rejection (Annex A.1): one declared call/watch
/// argument's wire value did not decode against its parameter's declared type,
/// so the request is malformed and rejected at the wire boundary — never coerced
/// to a best-effort inference. Carries the offending argument name and the
/// underlying decode error for a diagnostic.
#[derive(Debug, Clone)]
pub struct ArgDecodeError {
    name: String,
    source: ValueError,
}

impl fmt::Display for ArgDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "argument `{}` does not decode against its declared type: {}", self.name, self.source)
    }
}

impl std::error::Error for ArgDecodeError {}

/// Decode one `args` object into typed [`Value`]s, using `types[name]` for each
/// argument's declared type. A declared argument that fails its typed decode
/// surfaces as an [`ArgDecodeError`] (§12.1 malformed request); an argument
/// whose type is not resolved is shape-inferred (§8.3).
///
/// # Errors
/// [`ArgDecodeError`] when a declared argument's wire value does not decode
/// against its parameter's declared type.
pub fn decode_args(
    args: &J,
    types: &BTreeMap<String, Type>,
) -> Result<BTreeMap<String, Value>, ArgDecodeError> {
    let mut out = BTreeMap::new();
    if let Some(map) = args.as_object() {
        for (name, wire) in map {
            let value = match types.get(name) {
                // §12.1 step 3 / Annex A.1: a declared argument decodes against
                // its parameter type; a typed-decode FAILURE is a malformed
                // request, surfaced as a rejection rather than coerced to a
                // best-effort inference (the seed path rejects the same mismatch).
                Some(ty) => decode_typed(ty, wire)
                    .map_err(|source| ArgDecodeError { name: name.clone(), source })?,
                // §8.3: an argument whose parameter type this phase does not
                // resolve (a receiver key of a row mutation) is shape-inferred —
                // the legitimate inference, not a swallowed typed-decode rejection.
                None => infer(wire),
            };
            out.insert(name.clone(), value);
        }
    }
    Ok(out)
}

/// Decode one argument against its declared type, honoring the §8.5 set-operand
/// rule: a `set<T>` parameter accepts either a set (its canonical JSON-array wire
/// form) or a single member. A non-array wire value for a `set<T>` decodes
/// against the element type `T` as a one-member operand — the form the runtime's
/// set add/remove takes ("a member or a set"), and by which a set ref member is
/// named by its target's typed key (§5.5/§A.9). Every other type, and a set given
/// an array, decodes against the declared type directly; a wrong-typed value
/// (including a scalar that does not decode against `T`) rejects.
fn decode_typed(ty: &Type, wire: &J) -> Result<Value, ValueError> {
    match ty {
        Type::Set(element) if !wire.is_array() => element.decode(wire),
        _ => ty.decode(wire),
    }
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
