//! The COSE token namespace (§16, §17.7/§17.8): the pinned token format and
//! claims codec a `cose.sign` / `cose.verify` call site uses.
//!
//! §17.8 splits the responsibility three ways: the package controls the claims,
//! the registered namespace controls the *pinned token format and cryptographic
//! encoding*, and the provider controls the private operation. This module owns
//! the middle piece — the [`CoseClaims`] payload and the [`CoseToken`] wire — as
//! typed values, plus the §16.2 [`cose_descriptor`] a package's `$requires`
//! resolves. The private sign and the acceptance-set verify are performed by the
//! composed keyring, which owns the provider handles; this module supplies the
//! bytes it signs ([`CoseClaims::signing_bytes`]) and the structural token it
//! packages the signature into.
//!
//! The claims carry the authenticator identity (§11.4): a conforming token binds
//! `auth` into the signed payload, so a proof minted for one authenticator
//! cannot be replayed against another.

use std::collections::BTreeMap;

use liasse_value::{Bytes, Integer, Struct, Text, Type, Value};

use crate::descriptor::{
    EffectClass, FunctionDescriptor, InterfaceHash, NamespaceDescriptor, NamespaceType, OpSignature,
};
use crate::version::{ContractName, Version};

/// The structural member the token carries the signing ring under.
const RING: &str = "$ring";
/// The structural member the token carries the signing version ordinal under.
const VERSION: &str = "$version";
/// The structural member the token carries its claims under.
const CLAIMS: &str = "$claims";
/// The structural member the token carries its signature bytes under.
const SIG: &str = "$sig";
/// The claim binding a token to the authenticator that issued it (§11.4).
const AUTH: &str = "auth";

/// The claims a `cose.sign` call packages into a token (§17.8). An ordered map of
/// claim name to typed value; the `auth` claim binds the token to its issuing
/// authenticator (§11.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoseClaims {
    claims: BTreeMap<Text, Value>,
}

impl CoseClaims {
    /// Assemble claims from name/value pairs.
    #[must_use]
    pub fn new(claims: impl IntoIterator<Item = (Text, Value)>) -> Self {
        Self { claims: claims.into_iter().collect() }
    }

    /// The authenticator identity the token is bound to (§11.4), if the `auth`
    /// claim is present and textual.
    #[must_use]
    pub fn auth(&self) -> Option<&str> {
        match self.claims.get(&Text::new(AUTH))? {
            Value::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// A claim by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.claims.get(&Text::new(name))
    }

    /// The claims in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&Text, &Value)> {
        self.claims.iter()
    }

    /// The canonical bytes the provider signs (Annex A canonical JSON of the
    /// claim struct). Deterministic over the claim set, so the same claims always
    /// sign the same bytes and a verifier can re-derive them.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.as_struct().to_canonical_json_string().into_bytes()
    }

    /// The claims as a `Value::Struct`.
    #[must_use]
    pub fn as_struct(&self) -> Value {
        Value::Struct(Struct::new(self.claims.clone()))
    }

    /// Recover claims from a `Value::Struct` (the [`Self::as_struct`] inverse).
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Struct(fields) => Some(Self {
                claims: fields.fields().map(|(name, value)| (name.clone(), value.clone())).collect(),
            }),
            _ => None,
        }
    }
}

/// A signed COSE token (§17.7/§17.8): the claims, the ring and version that
/// signed them, and the signature. Verification consults the ring's acceptance
/// set by `version`, so a token from a revoked or foreign version fails even
/// though its bytes are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoseToken {
    ring: String,
    version: u64,
    claims: CoseClaims,
    signature: Bytes,
}

impl CoseToken {
    /// Package a signature over `claims` by ring version `version`.
    #[must_use]
    pub fn new(ring: impl Into<String>, version: u64, claims: CoseClaims, signature: Vec<u8>) -> Self {
        Self { ring: ring.into(), version, claims, signature: Bytes::new(signature) }
    }

    /// The signing ring name.
    #[must_use]
    pub fn ring(&self) -> &str {
        &self.ring
    }

    /// The signing version ordinal (§17.7: the version identity the accepting
    /// namespace reads).
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// The claims the token carries.
    #[must_use]
    pub fn claims(&self) -> &CoseClaims {
        &self.claims
    }

    /// The signature bytes.
    #[must_use]
    pub fn signature(&self) -> &[u8] {
        self.signature.as_slice()
    }

    /// The token as a typed value, so a driver carries it as a credential between
    /// steps and a verifier reconstructs it (§17.8 pinned format).
    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Struct(Struct::new([
            (Text::new(RING), Value::Text(Text::new(self.ring.clone()))),
            (Text::new(VERSION), Value::Int(Integer::from(i64::try_from(self.version).unwrap_or(i64::MAX)))),
            (Text::new(CLAIMS), self.claims.as_struct()),
            (Text::new(SIG), Value::Bytes(self.signature.clone())),
        ]))
    }

    /// Reconstruct a token from a [`Self::to_value`] structure, or `None` when the
    /// value is not a well-formed token.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let Value::Struct(fields) = value else { return None };
        let Value::Text(ring) = fields.get(RING)? else { return None };
        let Value::Int(version) = fields.get(VERSION)? else { return None };
        let claims = CoseClaims::from_value(fields.get(CLAIMS)?)?;
        let Value::Bytes(signature) = fields.get(SIG)? else { return None };
        Some(Self {
            ring: ring.as_str().to_owned(),
            version: version.to_canonical_text().parse::<u64>().ok()?,
            claims,
            signature: signature.clone(),
        })
    }
}

/// The §16.2 load-time descriptor for `liasse.cose@1`: the `token` named type and
/// the `sign` (generated, §16.3) and `verify` (verifier) functions a package's
/// `$requires: { cose: "liasse.cose@1" }` resolves and pins.
#[must_use]
pub fn cose_descriptor() -> NamespaceDescriptor {
    let id = ContractName::parse("liasse.cose").unwrap_or_else(|_| unreachable!("static contract name"));
    NamespaceDescriptor::new(
        id,
        Version::new(1, 0, 0),
        InterfaceHash::new("liasse.cose@1"),
        [("token".to_owned(), NamespaceType::new("cose-token", false))],
        [
            // `cose.sign(ring, claims) -> token`: the ring is addressed by path,
            // not passed as a value, so the value-typed signature is `(json) ->
            // bytes`. Signing is a generated, write-time operation (§16.3).
            (
                "sign".to_owned(),
                FunctionDescriptor::new(OpSignature::new([Type::Json], Type::Bytes), EffectClass::Generated),
            ),
            // `cose.verify(ring, token) -> claims`: a verifier over untrusted
            // input (§16.3), returning the verified claims as `json`.
            (
                "verify".to_owned(),
                FunctionDescriptor::new(OpSignature::new([Type::Bytes], Type::Json), EffectClass::Verifier),
            ),
        ],
    )
}
