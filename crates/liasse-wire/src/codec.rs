//! The default JSON codec: render any wire type to JSON and parse any wire type
//! from JSON, rejecting malformed input.
//!
//! The frame types are codec-agnostic, so encoding is a thin, total pass over
//! serde. Decoding is where a hostile peer meets the boundary: [`decode`] parses,
//! and a well-formed value is proof the frame is well-formed (parse, don't
//! validate). Anything else — truncated JSON, a wrong type, a missing field, an
//! unknown frame tag — becomes a [`CodecError`], never a panic.
//!
//! Strictness note (S1 seam): a frame enum is internally tagged for a flat wire
//! shape, and serde does not honor `deny_unknown_fields` on an internally tagged
//! variant, so an unknown SIBLING member inside a known frame is tolerated. The
//! plain structs that are not enum variants ([`crate::WireRow`],
//! [`crate::WireWindow`]) do reject unknown members. The plan puts the hostile-input
//! strictness at the server boundary (`liasse-connect`), which additionally
//! validates; downstream frames may even relax to forward-compatible there. What
//! this codec already guarantees at both directions: an unknown tag, a missing
//! required field, a type mismatch, and malformed bytes all fail.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Why a wire value could not be rendered or parsed.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The bytes were not well-formed JSON, or did not match the target frame's
    /// schema (missing field, wrong type, unknown tag, trailing data).
    #[error("malformed wire frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// Render a wire value to a JSON string.
///
/// # Errors
/// Returns [`CodecError`] only if a contained [`serde_json::Value`] cannot be
/// serialized (e.g. a map with non-string keys), which a well-formed wire value
/// never contains.
pub fn encode<T: Serialize>(value: &T) -> Result<String, CodecError> {
    Ok(serde_json::to_string(value)?)
}

/// Parse a wire value from a JSON string, rejecting anything that does not match
/// the target type exactly.
///
/// # Errors
/// Returns [`CodecError`] for malformed JSON, a type mismatch, a missing required
/// field, an unknown frame tag, or trailing data after the value.
pub fn decode<T: DeserializeOwned>(text: &str) -> Result<T, CodecError> {
    Ok(serde_json::from_str(text)?)
}
