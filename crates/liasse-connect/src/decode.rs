//! Turning inbound wire frames into typed surface requests (§12.1) — the hostile
//! boundary.
//!
//! Every value a client supplies (`args`, `params`, a credential, a context name)
//! arrives as an opaque [`Json`]. Here it is decoded against the type the model
//! declares for it via [`Type::decode_wire`] — the strict **machine wire/request**
//! boundary (SPEC-ISSUES item 2): a scalar MUST already be canonical (Annex A.1 /
//! D.2), and a non-canonical spelling (uppercase uuid, leading-zero int, …) is
//! refused as malformed at admission rather than silently normalized. A successful
//! decode is proof the argument is well-formed and of the right shape (parse, don't
//! validate). A value that does not decode, or an argument the schema never
//! declared, is refused — it never reaches admission as an ill-typed [`Value`], and
//! a non-canonical spelling never mints a second identity. Decoding is total: no
//! input shape panics.

use std::collections::BTreeMap;

use liasse_surface::{AuthSelection, Credential};
use liasse_value::{Type, Value};
use liasse_wire::serde_json::Value as Json;

use crate::mount::Schema;

/// Why an inbound value could not be turned into a typed request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// An argument or parameter did not decode against its declared type, or the
    /// client supplied a member the schema does not declare — the request is
    /// malformed (§12.1, mirroring the runtime's `Malformed` rejection).
    #[error("{0}")]
    Malformed(String),
    /// A credential did not decode against the authenticator's declared
    /// `$credential`, or named an authenticator the schema does not carry. Reported
    /// as an authentication refusal, never echoing the credential (§11.3).
    #[error("credential could not be read")]
    Credential,
}

/// Decode a `{name: wire}` argument object against a typed contract. Each declared
/// argument present in `wire` is decoded to its canonical [`Value`]; a declared
/// argument the client omitted is left unbound (the host binds an optional
/// parameter to `none`, a required one it rejects — §8.3). A member the contract
/// does not declare is refused, so no unmodeled argument slips through.
pub fn decode_args(
    contract: &[(String, Type)],
    wire: Option<&Json>,
) -> Result<BTreeMap<String, Value>, DecodeError> {
    let object = match wire {
        None | Some(Json::Null) => return Ok(BTreeMap::new()),
        Some(Json::Object(map)) => map,
        Some(_) => return Err(DecodeError::Malformed("arguments must be a JSON object".to_owned())),
    };
    for name in object.keys() {
        if !contract.iter().any(|(declared, _)| declared == name) {
            return Err(DecodeError::Malformed(format!("unknown argument `{name}`")));
        }
    }
    let mut args = BTreeMap::new();
    for (name, ty) in contract {
        if let Some(value) = object.get(name) {
            let decoded = ty
                .decode_wire(value)
                .map_err(|error| DecodeError::Malformed(format!("argument `{name}`: {error}")))?;
            args.insert(name.clone(), decoded);
        }
    }
    Ok(args)
}

/// Decode a per-request authenticator selection `{ auth, credential }` (§11.4). The
/// credential is decoded against the named authenticator's declared `$credential`,
/// so a forged or wrong-typed token fails here rather than reaching the verifier as
/// an ill-typed value.
pub fn decode_selection(schema: &Schema, wire: &Json) -> Result<AuthSelection, DecodeError> {
    let object = wire.as_object().ok_or(DecodeError::Credential)?;
    let auth = object.get("auth").and_then(Json::as_str).ok_or(DecodeError::Credential)?;
    let credential_wire = object.get("credential").ok_or(DecodeError::Credential)?;
    let ty = schema.credential(auth).ok_or(DecodeError::Credential)?;
    // The credential is client-supplied hostile wire input, so it decodes through
    // the strict machine-wire boundary too (SPEC-ISSUES item 2): a non-canonical
    // scalar credential is refused rather than normalized.
    let value = ty.decode_wire(credential_wire).map_err(|_| DecodeError::Credential)?;
    Ok(AuthSelection::new(auth.to_owned(), Credential::new(value)))
}

/// The role, selection, and bound-context name a `hello { auth }` carries (§11.4,
/// §11.8): `{ role, auth, credential }` plus an optional context name.
pub struct HelloAuth {
    /// The role whose accepted authenticators gate the selection.
    pub role: String,
    /// The verified authenticator selection.
    pub selection: AuthSelection,
}

/// Decode a `hello`/`authenticate` selection `{ role, auth, credential }` (§11.4).
pub fn decode_hello_auth(schema: &Schema, wire: &Json) -> Result<HelloAuth, DecodeError> {
    let object = wire.as_object().ok_or(DecodeError::Credential)?;
    let role = object.get("role").and_then(Json::as_str).ok_or(DecodeError::Credential)?;
    let selection = decode_selection(schema, wire)?;
    Ok(HelloAuth { role: role.to_owned(), selection })
}

/// Decode an optional `context` value (§11.8): a JSON string naming the context, or
/// nothing.
pub fn decode_context(wire: Option<&Json>) -> Result<Option<String>, DecodeError> {
    match wire {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(name)) => Ok(Some(name.clone())),
        Some(_) => Err(DecodeError::Malformed("context must be a string".to_owned())),
    }
}
